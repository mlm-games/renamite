use std::cell::RefCell;
use std::rc::Rc;

use glam::DVec2;
use renamite_animation::{Frame, LoopMode, PlayState, Playback};
use renamite_behavior_common::{Modifiers, Selection, SnapConfig, ToolContext, ViewTransform};
use renamite_behavior_canvas::{CanvasEvent, PointerButton, ToolSet};
use renamite_behavior_timeline::{
    TimelineCtx, TimelineEvent, TimelineKeyframeBehavior, TimelineLayout, TimelineRow,
    TimelineScrubBehavior, TimelineTarget,
};
use renamite_history::{History, OutputVec, ProjectMut, ToolId, ToolOutput};
use renamite_io_ren::RenFile;
use renamite_model::{PropPath, Value};
use renamite_player::Engine;
use renamite_render_bridge::SceneRenderer;
use repose_core::input::{PointerEvent, PointerEventKind};
use repose_core::{
    animation_driver, request_frame, remember_state_with_key, remember_with_key,
};
use web_time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum PanelPage {
    Canvas = 0,
    Layers = 1,
    Timeline = 2,
    Inspect = 3,
}

pub type SessionRef = Rc<RefCell<Session>>;

/// Shared editor session (single-threaded UI).
pub struct Session {
    pub file: RenFile,
    pub history: History,
    pub engine: Engine,
    pub selection: Selection,
    pub viewport: ViewportState,
    pub active_tool: ToolId,
    pub active_page: PanelPage,
    pub playback: Playback,
    pub playing: bool,
    pub tool: ToolSet,
    pub keys: TimelineKeyframeBehavior,
    pub scrub: TimelineScrubBehavior,
    pub renderer: SceneRenderer,
    pub last_tick: Instant,
    pub revision: u64,
    /// Layers panel: groups whose children are shown (view state, not undoable).
    pub expanded_layers: std::collections::HashSet<renamite_model::NodeId>,
    /// Active drag-reorder in the layers panel (view state).
    pub layer_drag: Option<LayerDragState>,
    /// Rename-in-progress: node id + draft text (view state).
    pub renaming: Option<(renamite_model::NodeId, String)>,
    /// Properties → write keys at playhead even when the prop isn't animated.
    pub record: bool,
    /// Active pointer-drag on a Properties number field (view state).
    pub inspector_drag: Option<InspectorDrag>,
}

#[derive(Clone, Debug)]
pub struct InspectorDrag {
    pub path: PropPath,
    /// DVec2: 0 = x, 1 = y; F64/Angle: 0; Color: 0..3 = r/g/b/a.
    pub channel: usize,
    pub origin_value: Value,
    pub press_x: f32,
    pub txn: bool,
}

#[derive(Clone, Debug)]
pub struct LayerDragState {
    pub id: renamite_model::NodeId,
    pub hover_row: usize,
    pub before: bool,
    pub as_child: bool,
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
            viewport: ViewportState::default(),
            active_tool: ToolId::Select,
            active_page: PanelPage::Canvas,
            playback: Playback {
                state: PlayState::Stopped,
                head: range.0 .0 as f64,
                loop_mode: LoopMode::Loop,
                range,
                dir: 1.0,
            },
            playing: false,
            tool: ToolSet::default(),
            keys: TimelineKeyframeBehavior::default(),
            scrub: TimelineScrubBehavior::default(),
            renderer: SceneRenderer::new(),
            last_tick: Instant::now(),
            revision: 0,
            expanded_layers: std::collections::HashSet::new(),
            layer_drag: None,
            renaming: None,
            record: false,
            inspector_drag: None,
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
                        if let Some(id) = self.history_apply(c) {
                            // Select what shape tools create.
                            self.selection.nodes = vec![id];
                        }
                    }
                    self.ensure_selection_visible();
                    self.bump();
                }
                ToolOutput::SetPlayhead(f) => {
                    self.playback.head = f;
                    self.engine.scrub(&self.file, f);
                    self.bump();
                }
                ToolOutput::SwitchTool(t) => self.active_tool = t,
                ToolOutput::RequestSelection(ch) => {
                    match ch {
                        renamite_history::SelectionChange::Set(ids) => self.selection.nodes = ids,
                        renamite_history::SelectionChange::Toggle(id) => {
                            if let Some(i) = self.selection.nodes.iter().position(|&x| x == id) {
                                self.selection.nodes.remove(i);
                            } else {
                                self.selection.nodes.push(id);
                            }
                        }
                    }
                    self.ensure_selection_visible();
                }
                _ => {}
            }
        }
    }

    pub fn bump(&mut self) {
        self.engine.reevaluate(&self.file);
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    /// Auto-expand ancestor groups so a selected (possibly nested) node's
    /// layers row becomes visible. View state only - not undoable.
    pub fn ensure_selection_visible(&mut self) {
        for &id in &self.selection.nodes {
            let mut walk = id;
            while let Some(n) = self.file.document.nodes.get(walk) {
                if let Some(p) = n.parent {
                    self.expanded_layers.insert(p);
                    walk = p;
                } else {
                    break;
                }
            }
        }
    }

    /// Apply one command; returns the created node id, if any.
    pub fn history_apply(&mut self, cmd: renamite_history::EditorCommand) -> Option<renamite_model::NodeId> {
        let his = &mut self.history;
        let file = &mut self.file;
        let mut pm = pm_from(file);
        his.apply(&mut pm, cmd).ok().and_then(|a| a.created)
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

#[derive(Clone, Debug)]
pub struct ViewportState {
    pub view: ViewTransform,
    pub surface_size: DVec2,
    pub fit_pending: bool,
    pub pan_last: Option<DVec2>,
}

impl Default for ViewportState {
    fn default() -> Self {
        Self {
            view: ViewTransform::identity(),
            surface_size: DVec2::ZERO,
            fit_pending: true,
            pan_last: None,
        }
    }
}

impl ViewportState {
    pub fn ensure_fit(&mut self, surface: DVec2, artboard: DVec2) {
        let resized = (surface - self.surface_size).abs().max_element() > 0.5;
        self.surface_size = surface;

        if self.fit_pending || resized {
            self.fit(artboard);
        }
    }

    pub fn fit(&mut self, artboard: DVec2) {
        if self.surface_size.x <= 1.0
            || self.surface_size.y <= 1.0
            || artboard.x <= 0.0
            || artboard.y <= 0.0
        {
            return;
        }

        let margin = 56.0;
        let available = (self.surface_size - DVec2::splat(margin * 2.0)).max(DVec2::splat(1.0));

        let scale = (available.x / artboard.x)
            .min(available.y / artboard.y)
            .clamp(0.05, 32.0);

        self.view.scale = scale;
        self.view.offset = (self.surface_size - artboard * scale) * 0.5;
        self.fit_pending = false;
    }

    pub fn zoom_centered(&mut self, factor: f64) {
        if self.surface_size == DVec2::ZERO {
            return;
        }

        let screen_center = self.surface_size * 0.5;
        let world_center = self.view.screen_to_world(screen_center);

        self.view.scale = (self.view.scale * factor).clamp(0.05, 64.0);
        self.view.offset = screen_center - world_center * self.view.scale;
    }

    pub fn begin_pan(&mut self, position: DVec2) {
        self.pan_last = Some(position);
    }

    pub fn update_pan(&mut self, position: DVec2) -> bool {
        let Some(previous) = self.pan_last.replace(position) else {
            return false;
        };

        self.view.offset += position - previous;
        true
    }

    pub fn end_pan(&mut self) {
        self.pan_last = None;
    }
}

pub fn timeline_ctx<'a>(
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

pub fn dispatch_timeline(s: &mut Session, ev: TimelineEvent) {
    let rows = timeline_rows(s);
    let (head, comp) = (s.playback.head, s.file.document.main);
    let range = s.file.document.compositions[comp].range;
    let ctx = timeline_ctx(&s.file.document, &s.file.clips, &rows, range, head);
    let outs = s.keys.handle(&ctx, ev);
    s.apply_outputs(outs);
}

pub fn timeline_rows(s: &Session) -> Vec<TimelineRow> {
    let comp = &s.file.document.compositions[s.file.document.main];
    comp.children
        .iter()
        .take(24)
        .map(|&id| TimelineRow {
            node: id,
            prop: PropPath::new("opacity"),
        })
        .collect()
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
fn apply_cmd(
    history: &mut History,
    file: &mut RenFile,
    cmd: Option<renamite_history::EditorCommand>,
) {
    let mut pm = pm_from(file);
    match cmd {
        Some(c) => {
            let _ = history.apply(&mut pm, c);
        }
        None => {
            let _ = history.cancel(&mut pm);
        }
    }
}

pub fn undo_cmd(s: &mut Session) {
    let his = &mut s.history;
    let file = &mut s.file;
    let mut pm = pm_from(file);
    let _ = his.undo(&mut pm);
}

pub fn redo_cmd(s: &mut Session) {
    let his = &mut s.history;
    let file = &mut s.file;
    let mut pm = pm_from(file);
    let _ = his.redo(&mut pm);
}

fn pe_to_dvec(pe: &PointerEvent) -> DVec2 {
    DVec2::new(pe.position.x as f64, pe.position.y as f64)
}

pub fn map_button(pe: &PointerEvent) -> PointerButton {
    match pe.event {
        PointerEventKind::Down(b) | PointerEventKind::Up(b) => match b {
            repose_core::input::PointerButton::Primary => PointerButton::Primary,
            repose_core::input::PointerButton::Secondary => PointerButton::Secondary,
            repose_core::input::PointerButton::Tertiary => PointerButton::Middle,
        },
        _ => PointerButton::Primary,
    }
}

pub fn map_modifiers(pe: &PointerEvent) -> Modifiers {
    Modifiers {
        shift: pe.modifiers.shift,
        ctrl: pe.modifiers.ctrl,
        alt: pe.modifiers.alt,
    }
}

pub fn dispatch_canvas(s: &mut Session, ev: CanvasEvent, m: Modifiers) {
    let outs = {
        let Session { file, engine, selection, playback, viewport, tool, active_tool, record, .. } = s;
        let ctx = ToolContext {
            doc: &file.document,
            scene: engine.scene(),
            comp: file.document.main,
            selection,
            playhead: Frame(playback.head as i64),
            record: *record,
            view: viewport.view,
            snap: SnapConfig { grid: None, anchor: false, guide: false },
            modifiers: m,
        };
        tool.handle(*active_tool, &ctx, ev)
    };
    s.apply_outputs(outs);
}

pub fn pe_pos(pe: &PointerEvent) -> DVec2 {
    pe_to_dvec(pe)
}

/// Default empty document with a seeded ellipse so the artboard isn't blank.
pub fn default_file() -> RenFile {
    use renamite_animation::Animated;
    use renamite_model::{Color, FillRule, Node, NodeKind, Parent, ShapeKind, StyleKind};

    let mut doc = renamite_model::Document::empty();
    let comp = doc.main;
    let (w, h) = doc.compositions[comp].size;
    let center = DVec2::new(w as f64 * 0.5, h as f64 * 0.5);

    let shape = doc.create_node(Node::new(
        "Ellipse",
        NodeKind::Shape(ShapeKind::Ellipse {
            pos: Animated::new(center),
            size: Animated::new(DVec2::new(180.0, 180.0)),
        }),
    ));

    let fill = doc.create_node(Node::new(
        "Fill",
        NodeKind::Style(StyleKind::Fill {
            color: Animated::new(Color::rgba(0.96, 0.42, 0.18, 1.0)),
            rule: FillRule::NonZero,
        }),
    ));

    doc.attach(shape, Parent::Comp(comp), 0).unwrap();
    doc.attach(fill, Parent::Comp(comp), 1).unwrap();

    RenFile::new(doc, "Untitled")
}

/// Register the playback driver once and return the shared session.
pub fn init_session() -> Rc<RefCell<Session>> {
    let session = remember_with_key("session", || {
        RefCell::new(Session::new(default_file()))
    });

    let registered = remember_state_with_key("pb_reg", || false);
    if !*registered.borrow() {
        let sess = session.clone();
        animation_driver::register(
            "renamite_playback".into(),
            Rc::new(RefCell::new(move || sess.borrow_mut().tick_playback())),
        );
        *registered.borrow_mut() = true;
    }

    animation_driver::touch("renamite_playback");

    let _rev = session.borrow().revision;
    session
}
