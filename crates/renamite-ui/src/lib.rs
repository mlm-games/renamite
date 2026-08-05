//! Editor chrome.
//!
//! Root function matches Repose's `fn app(&mut Scheduler, &RenderContext) -> View`
//! pattern. `Session` holds the whole editor state behind `Rc<RefCell<_>>` so
//! Repose's `Fn` canvas/pointer callbacks can reach it via interior mutability.

#![allow(non_snake_case)]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use glam::DVec2;
use renamite_animation::{Frame, LoopMode, PlayState, Playback};
use renamite_behavior_common::{Modifiers, Selection, SnapConfig, ToolContext, ViewTransform};
use renamite_behavior_canvas::{CanvasEvent, PointerButton, ToolBehavior};
use renamite_behavior_timeline::{
    TimelineCtx, TimelineEvent, TimelineKeyframeBehavior, TimelineLayout, TimelineRow,
    TimelineScrubBehavior, TimelineTarget,
};
use renamite_history::{History, OutputVec, ProjectMut, ToolOutput};
use renamite_io_ren::RenFile;
use renamite_model::PropPath;
use renamite_player::Engine;
use renamite_render_bridge::SceneRenderer;
use repose_canvas::Canvas;
use repose_core::input::{PointerEvent, PointerEventKind};
use repose_core::{
    animation_driver, remember_state_with_key, remember_with_key, request_frame, theme, Modifier,
    Scheduler, View,
};
use repose_docking::{DockArea, DockCallbacks, DockPanel, DockState};
use repose_material::material3::{Button, ButtonConfig};
use repose_platform::{AppConfig, RenderContext, run_desktop_app_with_config};
use repose_ui::{Box, Column, Row, Text, TextStyle, ViewExt};

/// Shared editor session (single-threaded UI).
pub struct Session {
    pub file: RenFile,
    pub history: History,
    pub engine: Engine,
    pub selection: Selection,
    pub view: ViewTransform,
    pub playback: Playback,
    pub playing: bool,
    pub tool: ToolBehavior,
    pub keys: TimelineKeyframeBehavior,
    pub scrub: TimelineScrubBehavior,
    pub renderer: SceneRenderer,
    pub last_tick: Instant,
    pub revision: u64,
}

impl Session {
    pub fn new(file: RenFile) -> Self {
        let engine = Engine::new(&file).expect("project");
        let range = file.document.compositions[file.document.main].range;
        Self {
            file,
            history: History::new(),
            engine,
            selection: Selection::default(),
            view: ViewTransform {
                scale: 1.0,
                offset: DVec2::new(40.0, 40.0),
            },
            playback: Playback {
                state: PlayState::Stopped,
                head: range.0 .0 as f64,
                loop_mode: LoopMode::Loop,
                range,
                dir: 1.0,
            },
            playing: false,
            tool: ToolBehavior::default(),
            keys: TimelineKeyframeBehavior::default(),
            scrub: TimelineScrubBehavior::default(),
            renderer: SceneRenderer::new(),
            last_tick: Instant::now(),
            revision: 0,
        }
    }

    pub fn apply_outputs(&mut self, outputs: OutputVec) {
        for out in outputs {
            match out {
                ToolOutput::BeginTransaction(l) => self.history.begin(l),
                ToolOutput::CommitTransaction => {
                    self.history.commit();
                    self.bump();
                }
                ToolOutput::CancelTransaction => {
                    apply_cmd(&mut self.history, &mut self.file, None);
                    self.bump();
                }
                ToolOutput::Commands(cmds) => {
                    for c in cmds {
                        apply_cmd(&mut self.history, &mut self.file, Some(c));
                    }
                    self.bump();
                }
                ToolOutput::SetPlayhead(f) => {
                    self.playback.head = f;
                    self.engine.scrub(&self.file, f);
                    self.bump();
                }
                ToolOutput::RequestSelection(ch) => match ch {
                    renamite_history::SelectionChange::Set(ids) => self.selection.nodes = ids,
                    renamite_history::SelectionChange::Toggle(id) => {
                        if let Some(i) = self.selection.nodes.iter().position(|&x| x == id) {
                            self.selection.nodes.remove(i);
                        } else {
                            self.selection.nodes.push(id);
                        }
                    }
                },
                _ => {}
            }
        }
    }

    fn bump(&mut self) {
        self.engine.reevaluate(&self.file);
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    /// Called by the `animation_driver` tick each frame. Returns true while playing.
    pub fn tick_playback(&mut self) -> bool {
        if !self.playing {
            return false;
        }
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64().min(0.05);
        self.last_tick = now;
        let _ = self.engine.tick(&self.file, dt);
        self.playback.head = self.engine.head();
        self.revision = self.revision.wrapping_add(1);
        request_frame();
        true
    }
}

fn timeline_ctx<'a>(
    doc: &'a renamite_model::Document,
    clips: &'a renamite_machine::ClipMap,
    rows: &'a [TimelineRow],
    range: (Frame, Frame),
    playhead: f64,
) -> TimelineCtx<'a> {
    TimelineCtx {
        doc,
        clips,
        target: TimelineTarget::Doc,
        rows,
        layout: TimelineLayout {
            origin_x: 80.0,
            px_per_frame: 6.0,
            row_top: 28.0,
            row_height: 22.0,
            key_tolerance_px: 6.0,
        },
        range,
        playhead,
    }
}

fn dispatch_timeline(s: &mut Session, ev: TimelineEvent) {
    let rows = timeline_rows(&s);
    let (head, comp) = (s.playback.head, s.file.document.main);
    let range = s.file.document.compositions[comp].range;
    let ctx = timeline_ctx(
        &s.file.document,
        &s.file.clips,
        &rows,
        range,
        head,
    );
    let outs = s.keys.handle(&ctx, ev);
    s.apply_outputs(outs);
}

/// Build a `ProjectMut` from a `&mut RenFile` (no overlapping `&mut self`).
fn pm_from(file: &mut RenFile) -> ProjectMut<'_> {
    ProjectMut {
        document: &mut file.document,
        clips: &mut file.clips,
        clip_order: &mut file.clip_order,
        machines: &mut file.machines,
        machine_order: &mut file.machine_order,
        start_machine: &mut file.start_machine,
    }
}

/// Apply a command (or cancel an open transaction) given disjoint field borrows.
fn apply_cmd(history: &mut History, file: &mut RenFile, cmd: Option<renamite_history::EditorCommand>) {
    let mut pm = pm_from(file);
    match cmd {
        Some(c) => { let _ = history.apply(&mut pm, c); }
        None => { let _ = history.cancel(&mut pm); }
    }
}

fn undo_cmd(s: &mut Session) {
    let his = &mut s.history;
    let file = &mut s.file;
    let mut pm = pm_from(file);
    let _ = his.undo(&mut pm);
}

fn redo_cmd(s: &mut Session) {
    let his = &mut s.history;
    let file = &mut s.file;
    let mut pm = pm_from(file);
    let _ = his.redo(&mut pm);
}

fn pe_to_dvec(pe: &PointerEvent) -> DVec2 {
    DVec2::new(pe.position.x as f64, pe.position.y as f64)
}

fn map_button(pe: &PointerEvent) -> PointerButton {
    match pe.event {
        PointerEventKind::Down(b) | PointerEventKind::Up(b) => match b {
            repose_core::input::PointerButton::Primary => PointerButton::Primary,
            repose_core::input::PointerButton::Secondary => PointerButton::Secondary,
            repose_core::input::PointerButton::Tertiary => PointerButton::Middle,
        },
        _ => PointerButton::Primary,
    }
}

fn toolbar_toolctx<'a>(
    doc: &'a renamite_model::Document,
    comp: renamite_model::CompId,
    selection: &'a Selection,
    playback: &'a Playback,
    view: ViewTransform,
) -> ToolContext<'a> {
    ToolContext {
        doc,
        comp,
        selection,
        playhead: Frame(playback.head as i64),
        record: false,
        view,
        snap: SnapConfig { grid: None, anchor: false, guide: false },
        modifiers: Modifiers::none(),
    }
}

fn dispatch_canvas(s: &mut Session, ev: CanvasEvent) {
    let ctx = toolbar_toolctx(
        &s.file.document,
        s.file.document.main,
        &s.selection,
        &s.playback,
        s.view,
    );
    let outs = s.tool.handle(&ctx, ev);
    s.apply_outputs(outs);
}

/// Default empty document with a seeded ellipse so the artboard isn't blank.
pub fn default_file() -> RenFile {
    use renamite_animation::Animated;
    use renamite_model::{Color, Node, NodeKind, Parent, ShapeKind, StyleKind};
    let mut doc = renamite_model::Document::empty();
    let shape = doc.create_node(Node::new("Ellipse", NodeKind::Shape(ShapeKind::Ellipse {
        pos: Animated::new(DVec2::new(0.0, 0.0)),
        size: Animated::new(DVec2::new(200.0, 200.0)),
    })));
    let fill = doc.create_node(Node::new("Fill", NodeKind::Style(StyleKind::Fill {
        color: Animated::new(Color::rgba(1.0, 0.4, 0.1, 1.0)),
        rule: renamite_model::FillRule::NonZero,
    })));
    let _ = doc.attach(shape, Parent::Comp(doc.main), 0);
    let _ = doc.attach(fill, Parent::Comp(doc.main), 1);
    RenFile::new(doc, "Untitled")
}


fn EditorRoot(_s: &mut Scheduler, _rc: &RenderContext) -> View {
    let th = theme();

    let session = remember_with_key("session", || {
        RefCell::new(Session::new(default_file()))
    });

    // Register playback driver once; keep it alive via explicit touch each frame.
    let registered = remember_state_with_key("pb_reg", || false);
    if !*registered.borrow() {
        let sess = session.clone();
        animation_driver::register(
            "renamite_playback".into(),
            Rc::new(RefCell::new(move || sess.borrow_mut().tick_playback())),
        );
        *registered.borrow_mut() = true;
    }

    let dock = remember_with_key("dock", || {
        RefCell::new(DockState::new_with_tabs(vec![1, 2, 3, 4]))
    });

    // Keep the driver registration alive across sweeps.
    let key = "renamite_playback".to_string();
    animation_driver::touch(&key);

    // Force subscribe to revision so recompose reads the session.
    let _rev = session.borrow().revision;

    let session_v = session.clone();
    let session_t = session.clone();
    let session_p = session.clone();
    let session_l = session.clone();
    let panels = vec![
        DockPanel {
            id: 1,
            title: "Viewport".into(),
            content: Rc::new(move || ViewportPanel(session_v.clone())),
        },
        DockPanel {
            id: 2,
            title: "Timeline".into(),
            content: Rc::new(move || TimelinePanel(session_t.clone())),
        },
        DockPanel {
            id: 3,
            title: "Properties".into(),
            content: Rc::new(move || PropertiesPanel(session_p.clone())),
        },
        DockPanel {
            id: 4,
            title: "Layers".into(),
            content: Rc::new(move || LayersPanel(session_l.clone())),
        },
    ];

    Box(Modifier::new().fill_max_size().background(th.background)).child(
        Column(Modifier::new().fill_max_size()).child((
            TopBar(session.clone()),
            Row(Modifier::new().fill_max_size().weight(1.0)).child((
                ToolRail(session.clone()),
                DockArea(
                    String::from("main_dock"),
                    Modifier::new().fill_max_size().weight(1.0),
                    dock.clone(),
                    panels,
                    DockCallbacks::default(),
                ),
            )),
            StatusBar(session.clone()),
        )),
    )
}

fn TopBar(session: Rc<RefCell<Session>>) -> View {
    let th = theme();
    Row(Modifier::new()
        .height(40.0)
        .fill_max_width()
        .padding(8.0)
        .background(th.surface_container)
        .gap(8.0))
    .child((
        Text("renamite").color(th.on_surface),
        Button(
            Modifier::new(),
            {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    undo_cmd(&mut s);
                    s.bump();
                }
            },
            ButtonConfig::default(),
            || Text("Undo"),
        ),
        Button(
            Modifier::new(),
            {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    redo_cmd(&mut s);
                    s.bump();
                }
            },
            ButtonConfig::default(),
            || Text("Redo"),
        ),
        Button(
            Modifier::new(),
            {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    s.playing = !s.playing;
                    s.last_tick = Instant::now();
                    s.playback.state = if s.playing {
                        PlayState::Playing
                    } else {
                        PlayState::Stopped
                    };
                    request_frame();
                }
            },
            ButtonConfig::default(),
            || Text("Play/Pause"),
        ),
    ))
}

fn ToolRail(_session: Rc<RefCell<Session>>) -> View {
    let th = theme();
    Column(Modifier::new()
        .width(48.0)
        .fill_max_height()
        .background(th.surface_container_high)
        .padding(4.0)
        .gap(4.0))
    .child((
        Text("S").color(th.on_surface),
        Text("P").color(th.on_surface),
    ))
}

fn StatusBar(session: Rc<RefCell<Session>>) -> View {
    let th = theme();
    let head = session.borrow().playback.head;
    let rev = session.borrow().revision;
    Row(Modifier::new()
        .height(24.0)
        .fill_max_width()
        .padding_values(repose_core::modifier::PaddingValues {
            left: 8.0,
            right: 8.0,
            top: 2.0,
            bottom: 2.0,
        })
        .background(th.surface_container))
    .child(Text(format!("f={head:.1}  rev={rev}")).color(th.on_surface_variant))
}

fn ViewportPanel(session: Rc<RefCell<Session>>) -> View {
    let th = theme();
    let sess_draw = session.clone();
    let sess_down = session.clone();
    let sess_move = session.clone();
    let sess_up = session.clone();

    Canvas(
        Modifier::new()
            .fill_max_size()
            .background(th.surface)
            .on_pointer_down(move |pe: PointerEvent| {
                let mut s = sess_down.borrow_mut();
                let world = s.view.screen_to_world(pe_to_dvec(&pe));
                dispatch_canvas(&mut s, CanvasEvent::PointerDown {
                    pos: world,
                    button: map_button(&pe),
                });
            })
            .on_pointer_move(move |pe: PointerEvent| {
                let mut s = sess_move.borrow_mut();
                let world = s.view.screen_to_world(pe_to_dvec(&pe));
                dispatch_canvas(&mut s, CanvasEvent::PointerMove { pos: world });
            })
            .on_pointer_up(move |pe: PointerEvent| {
                let mut s = sess_up.borrow_mut();
                let world = s.view.screen_to_world(pe_to_dvec(&pe));
                dispatch_canvas(&mut s, CanvasEvent::PointerUp {
                    pos: world,
                    button: map_button(&pe),
                });
            }),
        move |scope| {
            let mut s = sess_draw.borrow_mut();
            let _ = s.revision;
            let scene = s.engine.scene().clone();
            let view = s.view;
            s.renderer.paint(&scene, &view, scope);
        },
    )
}

fn timeline_rows(s: &Session) -> Vec<TimelineRow> {
    let comp = &s.file.document.compositions[s.file.document.main];
    comp.children
        .iter()
        .take(24)
        .map(|&id| TimelineRow { node: id, prop: PropPath::new("opacity") })
        .collect()
}

fn TimelinePanel(session: Rc<RefCell<Session>>) -> View {
    let th = theme();
    let sess_draw = session.clone();
    Canvas(
        Modifier::new()
            .fill_max_size()
            .background(th.surface)
            .on_pointer_down({
                let session = session.clone();
                move |pe: PointerEvent| {
                    let mut s = session.borrow_mut();
                    dispatch_timeline(&mut s, TimelineEvent::Press {
                        pos: pe_to_dvec(&pe),
                        modifiers: Modifiers::none(),
                    });
                }
            })
            .on_pointer_up({
                let session = session.clone();
                move |pe: PointerEvent| {
                    let mut s = session.borrow_mut();
                    dispatch_timeline(&mut s, TimelineEvent::Release {
                        pos: pe_to_dvec(&pe),
                        modifiers: Modifiers::none(),
                    });
                }
            }),
        move |scope| {
            let s = sess_draw.borrow();
            let th = theme();
            let layout = TimelineLayout {
                origin_x: 80.0,
                px_per_frame: 6.0,
                row_top: 28.0,
                row_height: 22.0,
                key_tolerance_px: 6.0,
            };
            let x = layout.frame_to_x(s.playback.head) as f32;
            scope.draw_rect(
                repose_core::geometry::Rect { x: x - 1.0, y: 0.0, w: 2.0, h: scope.size.height },
                th.primary,
                0.0,
            );
            let _ = s.revision;
        },
    )
}

fn PropertiesPanel(session: Rc<RefCell<Session>>) -> View {
    let th = theme();
    let s = session.borrow();
    let label = if s.selection.nodes.is_empty() {
        "No selection".to_string()
    } else {
        format!("{} selected", s.selection.nodes.len())
    };
    Column(Modifier::new().fill_max_size().padding(8.0).background(th.surface))
        .child(Text(label).color(th.on_surface))
}

fn LayersPanel(session: Rc<RefCell<Session>>) -> View {
    let th = theme();
    let s = session.borrow();
    let comp = &s.file.document.compositions[s.file.document.main];
    let mut children = Vec::new();
    for &id in &comp.children {
        if let Some(n) = s.file.document.nodes.get(id) {
            children.push(Text(n.name.clone()).color(th.on_surface));
        }
    }
    Column(Modifier::new().fill_max_size().padding(8.0).gap(4.0).background(th.surface))
        .child(children)
}

/// Public runner for the binary.
pub fn run() -> anyhow::Result<()> {
    run_desktop_app_with_config(EditorRoot, AppConfig::default())
}
