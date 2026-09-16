use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::mem;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use web_workers::sync::Mutex;

use glam::DVec2;
use kurbo::Point;
use renamite_animation::{Frame, LoopMode, PlayState, Playback};
use renamite_behavior_canvas::{CanvasEvent, PointerButton, ToolSet};
use renamite_behavior_common::machine::{MachineSelection, remove_state, remove_transition};
use renamite_behavior_common::{
    FitState, Modifiers, Selection, SnapConfig, ToolContext, ViewTransform,
};
use renamite_behavior_timeline::{
    TimelineCtx, TimelineEvent, TimelineKeyframeBehavior, TimelineLayout, TimelineRow,
    TimelineScrubBehavior, TimelineTarget,
};
use renamite_history::{EditorCommand, History, OutputVec, ProjectMut, ToolId, ToolOutput};
use renamite_io_ren::RenFile;
use renamite_machine::{InputKind, InputValue, Machine, MachineId, State, StateKind};
use renamite_model::{PropPath, Value, node_transform_context, selection_bounds};
use renamite_player::Engine;
use renamite_render_bridge::SceneRenderer;
use repose_core::geometry::Rect;
use repose_core::input::{PointerEvent, PointerEventKind};
use repose_core::{animation_driver, remember_state_with_key, remember_with_key, request_frame};
use repose_material::material3::DialogState;
use smallvec::{SmallVec, smallvec};
use web_time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum PanelPage {
    Canvas = 0,
    Layers = 1,
    Timeline = 2,
    Inspect = 3,
    Assets = 4,
    Interact = 5,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorMode {
    Design,
    Animate,
    Interact,
}

pub type SessionRef = Rc<RefCell<Session>>;

#[derive(Clone)]
pub struct ClipboardItem {
    pub tree: renamite_history::NodeTree,
    pub source_parent: renamite_model::Parent,
    pub source_index: usize,
}

#[derive(Clone)]
pub struct ClipboardPayload {
    pub items: Vec<ClipboardItem>,
    pub style_nodes: Vec<renamite_model::NodeKind>,
}

#[derive(Clone, Copy, Debug)]
pub enum SelectionBoolean {
    Union,
    Difference,
    Intersection,
    Xor,
}

pub struct Session {
    pub file: RenFile,
    pub current_path: Option<PathBuf>,
    pub dirty: bool,
    pub history: History,
    pub engine: Engine,
    pub selection: Selection,
    pub viewport: ViewportState,
    pub active_tool: ToolId,
    pub active_page: PanelPage,
    pub mode: EditorMode,
    pub playback: Playback,
    pub playing: bool,
    pub tool: ToolSet,
    pub keys: TimelineKeyframeBehavior,
    pub scrub: TimelineScrubBehavior,
    pub renderer: SceneRenderer,
    pub render_context: repose_core::RenderContext,
    pub last_tick: Instant,
    pub revision: u64,
    pub expanded_layers: HashSet<renamite_model::NodeId>,
    pub layer_drag: Option<LayerDragState>,
    pub renaming: Option<(renamite_model::NodeId, String)>,
    pub record: bool,
    pub inspector_drag: Option<InspectorDrag>,
    pub status: Option<String>,
    pub file_ops: Arc<Mutex<VecDeque<PendingFileOp>>>,
    pub pending_intent: Option<PendingIntent>,
    pub confirm_dialog: Rc<DialogState>,
    pub current_paint: renamite_model::StylePaint,
    pub swatches: renamite_behavior_common::color::SwatchHistory,
    pub open_picker: Option<OpenPicker>,
    pub context_menu: Option<ContextMenuState>,
    pub exporting_png: bool,
    pub welcome: bool,
    pub clipboard: Option<ClipboardPayload>,
    pub active_machine: Option<MachineId>,
    pub machine_selection: MachineSelection,
    pub active_machine_layer: usize,
    pub machine_preview_inputs: Vec<InputValue>,
    pub machine_preview_enabled: bool,
    pub machine_drag: Option<MachineDrag>,
    pub machine_graph_gesture: Option<MachineGraphGesture>,
    pub listener_draft: ListenerDraft,
    pub timeline_zoom: f64,
    /// Horizontal scroll offset (px) for the timeline key area.
    /// `origin_x = -timeline_offset_x`; zoom keeps the left edge stable.
    pub timeline_offset_x: f64,
}

#[derive(Clone, Copy, Debug)]
pub enum MachineDrag {
    PreviewNumber {
        input: usize,
        origin: f64,
        press_x: f32,
    },
    MachineField {
        origin: f64,
        press_x: f32,
        txn: bool,
    },
}

#[derive(Clone, Debug)]
pub enum MachineGraphGesture {
    WireTransition {
        layer: usize,
        from_state: Option<usize>,
        current: DVec2,
    },
    Pan {
        last: DVec2,
    },
    DragState {
        layer: usize,
        state: usize,
        offset: DVec2,
        current: DVec2,
    },
}

#[derive(Clone, Debug, Default)]
pub struct ListenerDraft {
    pub event: Option<renamite_machine::PointerEventKind>,
    pub input: Option<usize>,
    pub toggle: bool,
    pub bool_value: bool,
    pub number_value: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingIntent {
    New,
    Open,
    ImportLottie,
    ImportSvg,
}

pub enum PendingFileOp {
    OpenDone {
        file: Box<RenFile>,
        path: Option<PathBuf>,
        message: String,
        /// Import compatibility warnings to surface in the status line
        /// (previously eprintln-only and discarded).
        #[allow(dead_code)]
        warnings: Vec<String>,
    },
    SaveOutcome {
        ok: bool,
        path: Option<PathBuf>,
    },
    Exported,
    ExportFinished {
        message: String,
    },
    ExportPngReady {
        bytes: Vec<u8>,
        suggested_name: String,
    },
    ImportFontDone {
        name: String,
        bytes: Vec<u8>,
    },
    ImportImageDone {
        asset: renamite_model::ImageAsset,
    },
    Failed {
        message: String,
    },
}

#[derive(Clone, Debug)]
pub struct InspectorDrag {
    pub path: PropPath,
    pub channel: usize,
    pub ids: Vec<renamite_model::NodeId>,
    pub origins: Vec<Value>,
    pub press_x: f32,
    pub txn: bool,
}

#[derive(Clone, Debug)]
pub struct LayerDragState {
    pub id: renamite_model::NodeId,
    pub hover_row: usize,
    pub before: bool,
    pub as_child: bool,
    /// Window Y (px) at press, for index math that survives scrolling.
    pub press_window_y: f32,
    /// Index of the dragged row at press.
    pub press_index: usize,
    /// Y within the pressed row at press (px, row-local).
    pub grab_offset_y: f32,
}

#[derive(Clone)]
pub struct OpenPicker {
    pub target: PickerTarget,
    pub state: Rc<RefCell<crate::color_picker::PickerState>>,

    pub anchor: DVec2,

    pub transaction_open: bool,

    pub cancel_current_paint: Option<renamite_model::StylePaint>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PickerTarget {
    CurrentPaint,
    StyleColor {
        style_id: renamite_model::NodeId,
    },
    GradientStop {
        style_id: renamite_model::NodeId,
        index: usize,
    },
    Prop {
        id: renamite_model::NodeId,
        path: renamite_model::PropPath,
    },
}

#[derive(Clone, Debug)]
pub struct ContextMenuState {
    pub screen_pos: DVec2,
    pub entries: Vec<renamite_behavior_common::context_menu::MenuEntry>,
    pub source: ContextMenuSource,
}

#[derive(Clone, Copy, Debug)]
pub enum ContextMenuSource {
    Layers { row: renamite_model::NodeId },
    Canvas { world: DVec2 },
}

impl Session {
    pub fn new(file: RenFile) -> Self {
        Self::with_render_context(file, repose_core::RenderContext::new())
    }

    pub fn with_render_context(file: RenFile, render_context: repose_core::RenderContext) -> Self {
        let engine = Engine::new(&file).unwrap_or_else(|e| {
            log::warn!(
                "Engine::new failed for project '{}': {e}; creating fallback engine",
                file.meta.name
            );
            let mut fallback =
                RenFile::new(renamite_model::Document::empty(), file.meta.name.clone());
            Engine::new(&fallback).unwrap_or_else(|_| {
                fallback.document = renamite_model::Document::empty();
                Engine::new(&fallback).expect("empty document must produce a valid engine")
            })
        });
        let range = file.document.compositions[file.document.main].range;
        let active_machine = file
            .start_machine
            .or_else(|| file.machine_order.first().copied());
        Self {
            file,
            current_path: None,
            dirty: false,
            history: History::new(),
            engine,
            selection: Selection::default(),
            viewport: ViewportState::default(),
            active_tool: ToolId::Select,
            active_page: PanelPage::Canvas,
            mode: EditorMode::Design,
            playback: Playback {
                state: PlayState::Stopped,
                head: range.0.0 as f64,
                loop_mode: LoopMode::Loop,
                range,
                dir: 1.0,
            },
            playing: false,
            tool: ToolSet::default(),
            keys: TimelineKeyframeBehavior::default(),
            scrub: TimelineScrubBehavior::default(),
            renderer: SceneRenderer::new(),
            render_context,
            last_tick: Instant::now(),
            revision: 0,
            expanded_layers: HashSet::new(),
            layer_drag: None,
            renaming: None,
            record: false,
            inspector_drag: None,
            status: None,
            file_ops: Arc::new(Mutex::new(VecDeque::new())),
            pending_intent: None,
            confirm_dialog: Rc::new(DialogState::new()),
            current_paint: renamite_model::StylePaint::solid(renamite_model::Color::rgba(
                0.96, 0.42, 0.18, 1.0,
            )),
            swatches: renamite_behavior_common::color::SwatchHistory::new(12),
            open_picker: None,
            context_menu: None,
            exporting_png: false,
            welcome: true,
            clipboard: None,
            active_machine,
            machine_selection: MachineSelection::None,
            active_machine_layer: 0,
            machine_preview_inputs: Vec::new(),
            machine_preview_enabled: false,
            machine_drag: None,
            machine_graph_gesture: None,
            listener_draft: ListenerDraft::default(),
            timeline_zoom: 6.0,
            timeline_offset_x: 0.0,
        }
    }

    pub fn apply_outputs(&mut self, outputs: OutputVec) {
        let mut document_changed = false;
        let mut needs_evaluation = false;
        let mut needs_repaint = false;
        let mut in_transaction = false;
        let mut txn_mutated = false;
        let mut mutated_outside_transaction = false;

        for out in outputs {
            match out {
                ToolOutput::BeginTransaction(l) => {
                    self.history.begin(l);
                    in_transaction = true;
                    txn_mutated = false;
                }
                ToolOutput::CommitTransaction => {
                    if in_transaction {
                        self.history.commit();
                        if txn_mutated {
                            self.dirty = true;
                        }
                        in_transaction = false;
                    } else if self.history.transaction_open() {
                        self.history.commit();
                        self.dirty = true;
                    }
                    if txn_mutated || self.dirty {
                        document_changed = true;
                    }
                }
                ToolOutput::CancelTransaction => {
                    apply_cmd(&mut self.history, &mut self.file, None);
                    document_changed = true;
                    needs_evaluation = true;
                    in_transaction = false;
                }
                ToolOutput::Commands(cmds) => {
                    for c in cmds {
                        if let Some(id) = self.history_apply(c) {
                            self.selection.nodes = vec![id];
                        }
                    }
                    self.ensure_selection_visible();
                    document_changed = true;
                    needs_evaluation = true;
                    if in_transaction || self.history.transaction_open() {
                        txn_mutated = true;
                    } else {
                        mutated_outside_transaction = true;
                    }
                }
                ToolOutput::SetPlayhead(f) => {
                    if self.machine_preview_enabled {
                    } else {
                        self.playback.head = f;
                        self.engine.scrub(&self.file, f);
                    }
                    needs_repaint = true;
                }
                ToolOutput::SwitchTool(t) => {
                    self.active_tool = t;
                    needs_repaint = true;
                }
                ToolOutput::Invalidate => {
                    needs_repaint = true;
                }
                ToolOutput::SetCurrentPaint(paint) => {
                    self.current_paint = paint;
                    needs_repaint = true;
                }
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
                    needs_repaint = true;
                }
                _ => {}
            }
        }

        if mutated_outside_transaction {
            self.dirty = true;
        }

        if document_changed {
            let rows = timeline_rows(self);
            let range = self.file.document.compositions[self.file.document.main].range;
            let ctx = timeline_ctx(
                &self.file.document,
                &self.file.clips,
                &rows,
                range,
                self.playback.head,
                self.timeline_zoom,
            );
            self.keys.retain_valid(&ctx);
            sync_playback_range(self);
        }

        if needs_evaluation {
            self.engine.reevaluate(&self.file);
        }
        if document_changed || needs_evaluation || needs_repaint {
            self.revision = self.revision.wrapping_add(1);
            request_frame();
        }
    }

    pub fn open_context_menu(&mut self, menu: ContextMenuState) {
        self.cancel_open_picker_state();
        self.open_picker = None;
        self.context_menu = Some(menu);
        self.repaint();
    }

    pub fn close_context_menu(&mut self) {
        if self.context_menu.is_none() {
            return;
        }
        self.context_menu = None;
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    pub fn run_menu_action(&mut self, action: renamite_behavior_common::context_menu::MenuAction) {
        use renamite_behavior_common::context_menu::{
            MenuAction, MenuContext, dispatch_menu_action,
        };

        match &action {
            MenuAction::Rename => {
                if let Some(ContextMenuState {
                    source: ContextMenuSource::Layers { row },
                    ..
                }) = &self.context_menu
                {
                    let name = self
                        .file
                        .document
                        .nodes
                        .get(*row)
                        .map(|n| n.name.clone())
                        .unwrap_or_default();
                    self.renaming = Some((*row, name));
                }
                self.close_context_menu();
                return;
            }
            MenuAction::Copy => {
                self.copy_selection();
                self.close_context_menu();
                return;
            }
            MenuAction::Cut => {
                self.cut_selection();
                self.close_context_menu();
                return;
            }
            MenuAction::Paste => {
                self.paste_selection();
                self.close_context_menu();
                return;
            }
            MenuAction::Duplicate => {
                self.duplicate_selection();
                self.close_context_menu();
                return;
            }
            MenuAction::CenterPivot => {
                self.center_pivot();
                self.close_context_menu();
                return;
            }
            _ => {}
        }

        let world = self.context_menu.as_ref().and_then(|m| match m.source {
            ContextMenuSource::Canvas { world } => Some(world),
            ContextMenuSource::Layers { .. } => None,
        });
        let paint = self.current_paint.clone();
        let outs = {
            let ctx = MenuContext {
                doc: &self.file.document,
                selection: &self.selection.nodes,
                comp: self.file.document.main,
                world_pos: world,
                has_clipboard: self.clipboard.is_some(),
                current_paint: &paint,
            };
            dispatch_menu_action(&ctx, &action)
        };
        self.close_context_menu();
        self.apply_outputs(outs.into());
    }

    fn center_pivot(&mut self) {
        let [node] = self.selection.nodes.as_slice() else {
            return;
        };

        let scene = self.engine.scene();
        let Some((min, max)) = selection_bounds(&self.file.document, scene, &self.selection.nodes)
        else {
            return;
        };

        let world_center = (min + max) * 0.5;

        let Some(transform) =
            node_transform_context(&self.file.document, *node, self.playback.head)
        else {
            return;
        };

        let parent_point =
            transform.parent_world.inverse() * Point::new(world_center.x, world_center.y);

        let new_position = DVec2::new(parent_point.x, parent_point.y);

        let delta_position = new_position - transform.position;

        let delta_anchor = affine_vector(transform.linear.inverse(), delta_position);

        let new_anchor = transform.anchor + delta_anchor;

        let outs: OutputVec = smallvec![
            ToolOutput::BeginTransaction("Center pivot".into()),
            ToolOutput::Commands(smallvec![
                EditorCommand::SetStatic {
                    id: *node,
                    prop: PropPath::new("transform.anchor"),
                    value: Value::DVec2(new_anchor),
                },
                EditorCommand::SetStatic {
                    id: *node,
                    prop: PropPath::new("transform.position"),
                    value: Value::DVec2(new_position),
                },
            ]),
            ToolOutput::CommitTransaction,
        ];

        self.apply_outputs(outs);
    }

    pub(crate) fn selected_roots(&self) -> Vec<renamite_model::NodeId> {
        let sel = &self.selection.nodes;
        sel.iter()
            .copied()
            .filter(|&id| {
                !sel.iter().any(|&anc| {
                    let mut p = self.file.document.nodes.get(id).and_then(|n| n.parent);
                    while let Some(par) = p {
                        if par == anc {
                            return true;
                        }
                        p = self.file.document.nodes.get(par).and_then(|n| n.parent);
                    }
                    false
                })
            })
            .collect()
    }

    fn tree_of(&self, id: renamite_model::NodeId) -> renamite_history::NodeTree {
        let children: Vec<renamite_model::NodeId> = self
            .file
            .document
            .nodes
            .get(id)
            .map(|n| n.children.clone())
            .unwrap_or_default();
        let mut node = self.file.document.nodes.get(id).cloned().unwrap();
        node.parent = None;
        node.children.clear();
        renamite_history::NodeTree {
            node,
            id: None,
            children: children.iter().map(|&c| self.tree_of(c)).collect(),
        }
    }

    fn finalize_open_edit(&mut self) {
        if let Some(open) = self.open_picker.clone()
            && open.transaction_open
        {
            let color = open.state.borrow().color();
            self.commit_picker_color(color);
        }

        if self.history.transaction_open() {
            self.history.commit();
            self.dirty = true;
        }

        self.inspector_drag = None;
        self.layer_drag = None;
        self.machine_drag = None;
        self.tool = renamite_behavior_canvas::ToolSet::default();
    }

    fn clipboard_from_selection(&mut self, cut: bool) {
        self.finalize_open_edit();
        let roots = self.selected_roots();
        let mut items = Vec::with_capacity(roots.len());
        let mut copied_ids: Vec<renamite_model::NodeId> = Vec::with_capacity(roots.len());
        for &id in &roots {
            let Some((parent, index)) = self.file.document.locate(id) else {
                continue;
            };
            if self
                .file
                .document
                .nodes
                .get(id)
                .map(|n| n.locked)
                .unwrap_or(true)
            {
                continue;
            }
            let tree = self.tree_of(id);
            items.push(ClipboardItem {
                tree,
                source_parent: parent,
                source_index: index,
            });
            copied_ids.push(id);
        }
        if items.is_empty() {
            self.status = Some("Nothing to copy: selection is empty or locked".into());
            self.repaint();
            return;
        }
        let style_nodes = copied_ids
            .first()
            .map(|&id| self.immediate_style_kinds(id))
            .unwrap_or_default();
        self.clipboard = Some(ClipboardPayload { items, style_nodes });
        if cut {
            let cmds: SmallVec<[renamite_history::EditorCommand; 4]> = copied_ids
                .iter()
                .map(|&id| renamite_history::EditorCommand::RemoveNode { id })
                .collect();
            self.apply_outputs(smallvec![
                ToolOutput::BeginTransaction("Cut".into()),
                ToolOutput::Commands(cmds),
                ToolOutput::CommitTransaction,
                ToolOutput::RequestSelection(renamite_history::SelectionChange::Set(vec![])),
            ]);
        } else {
            self.repaint();
        }
    }

    fn immediate_style_kinds(&self, id: renamite_model::NodeId) -> Vec<renamite_model::NodeKind> {
        use renamite_model::{NodeKind, StyleKind};

        let doc = &self.file.document;
        let mut fill: Option<NodeKind> = None;
        let mut stroke: Option<NodeKind> = None;

        fn walk(
            doc: &renamite_model::Document,
            id: renamite_model::NodeId,
            fill: &mut Option<NodeKind>,
            stroke: &mut Option<NodeKind>,
        ) {
            let Some(node) = doc.nodes.get(id) else {
                return;
            };
            match &node.kind {
                NodeKind::Style(st @ StyleKind::Fill { .. }) => {
                    *fill = Some(NodeKind::Style(st.clone()));
                }
                NodeKind::Style(st @ StyleKind::Stroke { .. }) => {
                    *stroke = Some(NodeKind::Style(st.clone()));
                }
                _ => {}
            }
            for &child in &node.children {
                walk(doc, child, fill, stroke);
            }
        }
        walk(doc, id, &mut fill, &mut stroke);

        let mut scope = doc.locate(id).map(|(p, _)| p);
        if fill.is_some() && stroke.is_some() {
            scope = None;
        }
        while let Some(current) = scope {
            let Ok(children) = (match current {
                renamite_model::Parent::Comp(c) => doc
                    .compositions
                    .get(c)
                    .map(|comp| comp.children.clone())
                    .ok_or(()),
                renamite_model::Parent::Node(p) => {
                    doc.nodes.get(p).map(|n| n.children.clone()).ok_or(())
                }
            }) else {
                break;
            };
            for &child in children.iter().rev() {
                let Some(node) = doc.nodes.get(child) else {
                    continue;
                };
                match &node.kind {
                    NodeKind::Style(st @ StyleKind::Fill { .. }) if fill.is_none() => {
                        fill = Some(NodeKind::Style(st.clone()));
                    }
                    NodeKind::Style(st @ StyleKind::Stroke { .. }) if stroke.is_none() => {
                        stroke = Some(NodeKind::Style(st.clone()));
                    }
                    _ => {}
                }
            }
            if fill.is_some() && stroke.is_some() {
                break;
            }
            scope = match current {
                renamite_model::Parent::Comp(_) => None,
                renamite_model::Parent::Node(p) => doc.locate(p).map(|(parent, _)| parent),
            };
        }

        [fill, stroke].into_iter().flatten().collect()
    }

    fn parent_is_attached(&self, parent: renamite_model::Parent) -> bool {
        let doc = &self.file.document;
        match parent {
            renamite_model::Parent::Comp(c) => doc.compositions.contains_key(c),
            renamite_model::Parent::Node(p) => {
                doc.nodes.contains_key(p)
                    && (doc.nodes.get(p).and_then(|n| n.parent).is_some()
                        || doc
                            .compositions
                            .values()
                            .any(|comp| comp.children.contains(&p)))
            }
        }
    }

    fn insert_trees(
        &mut self,
        items: Vec<ClipboardItem>,
        offset: DVec2,
        label: &str,
    ) -> Vec<renamite_model::NodeId> {
        self.finalize_open_edit();
        let mut created = Vec::new();
        self.history.begin(label.to_owned());
        for mut item in items {
            nudge_tree(&mut item.tree, offset);
            if let Some(id) = self.history_apply(renamite_history::EditorCommand::InsertNode {
                parent: renamite_model::Parent::Comp(self.file.document.main),
                index: 0,
                tree: item.tree,
            }) {
                created.push(id);
            }
        }
        self.history.commit();
        self.dirty = true;
        created
    }

    pub fn paste_clipboard(&mut self) {
        let Some(items) = self.clipboard.clone().map(|payload| payload.items) else {
            self.status = Some("Clipboard empty".into());
            self.repaint();
            return;
        };
        if items.is_empty() {
            self.status = Some("Clipboard empty".into());
            self.repaint();
            return;
        }
        let created = self.insert_trees(items, DVec2::new(20.0, 20.0), "Paste");
        if !created.is_empty() {
            self.selection.nodes = created;
            self.ensure_selection_visible();
            self.bump();
        }
    }

    pub fn paste_clipboard_in_place(&mut self) {
        let Some(payload) = self.clipboard.clone() else {
            self.status = Some("Clipboard empty".into());
            self.repaint();
            return;
        };
        if payload.items.is_empty() {
            self.status = Some("Clipboard empty".into());
            self.repaint();
            return;
        }

        self.finalize_open_edit();
        self.history.begin("Paste in place");

        let mut items = payload.items;
        use slotmap::Key as _;
        items.sort_by_key(|item| match item.source_parent {
            renamite_model::Parent::Comp(c) => (0u8, c.data().as_ffi(), item.source_index),
            renamite_model::Parent::Node(n) => (1u8, n.data().as_ffi(), item.source_index),
        });

        let mut created = Vec::new();

        for item in items {
            let parent = if self.parent_is_attached(item.source_parent) {
                item.source_parent
            } else {
                renamite_model::Parent::Comp(self.file.document.main)
            };

            if let Some(id) = self.history_apply(EditorCommand::InsertNode {
                parent,
                index: item.source_index,
                tree: item.tree,
            }) {
                created.push(id);
            }
        }

        self.history.commit();
        self.dirty = true;
        self.selection.nodes = created;
        self.ensure_selection_visible();
        self.bump();
    }

    pub fn paste_style(&mut self) {
        use renamite_model::Node;

        let Some(payload) = self.clipboard.clone() else {
            self.status = Some("Clipboard empty".into());
            self.repaint();
            return;
        };
        let src_fill = payload.style_nodes.iter().find_map(|k| match k {
            renamite_model::NodeKind::Style(st @ renamite_model::StyleKind::Fill { .. }) => {
                Some(st.clone())
            }
            _ => None,
        });
        let src_stroke = payload.style_nodes.iter().find_map(|k| match k {
            renamite_model::NodeKind::Style(st @ renamite_model::StyleKind::Stroke { .. }) => {
                Some(st.clone())
            }
            _ => None,
        });
        if src_fill.is_none() && src_stroke.is_none() {
            self.status = Some("No style on the clipboard".into());
            self.repaint();
            return;
        }

        let targets = self.selection.nodes.clone();
        if targets.is_empty() {
            self.status = Some("Select objects to paste the style onto".into());
            self.repaint();
            return;
        }

        struct PastePlan {
            target: renamite_model::NodeId,
            direct: Vec<EditorCommand>,
            local: Vec<renamite_model::StyleKind>,
        }

        let mut plans: Vec<PastePlan> = Vec::new();
        {
            let doc = &self.file.document;
            for target in targets {
                if !doc.nodes.contains_key(target) {
                    continue;
                }
                let mut plan = PastePlan {
                    target,
                    direct: Vec::new(),
                    local: Vec::new(),
                };
                let Some((scope, _)) = doc.locate(target) else {
                    continue;
                };
                let scope_paints_others = match scope {
                    renamite_model::Parent::Comp(c) => doc
                        .compositions
                        .get(c)
                        .map(|comp| {
                            comp.children
                                .iter()
                                .filter(|&&cid| {
                                    matches!(
                                        doc.nodes.get(cid).map(|n| &n.kind),
                                        Some(
                                            renamite_model::NodeKind::Shape(_)
                                                | renamite_model::NodeKind::Text(_)
                                        )
                                    )
                                })
                                .count()
                                > 1
                                || comp.children.iter().any(|&cid| {
                                    cid != target
                                        && matches!(
                                            doc.nodes.get(cid).map(|n| &n.kind),
                                            Some(
                                                renamite_model::NodeKind::Group
                                                    | renamite_model::NodeKind::Layer(_)
                                            )
                                        )
                                })
                        })
                        .unwrap_or(false),
                    renamite_model::Parent::Node(p) => doc
                        .nodes
                        .get(p)
                        .map(|n| {
                            n.children
                                .iter()
                                .filter(|&&cid| {
                                    matches!(
                                        doc.nodes.get(cid).map(|x| &x.kind),
                                        Some(
                                            renamite_model::NodeKind::Shape(_)
                                                | renamite_model::NodeKind::Text(_)
                                        )
                                    )
                                })
                                .count()
                                > 1
                        })
                        .unwrap_or(false),
                };

                for (source, want_stroke) in [(&src_fill, false), (&src_stroke, true)] {
                    let Some(source_kind) = source else { continue };
                    let shared_or_absent =
                        nearest_style_node(doc, target, want_stroke).is_none_or(|style_id| {
                            style_scope_info(doc, style_id)
                                .map(|(_, shared)| shared)
                                .unwrap_or(true)
                        }) || scope_paints_others;
                    if shared_or_absent {
                        plan.local.push(source_kind.clone());
                    } else if let Some(style_id) = nearest_style_node(doc, target, want_stroke) {
                        plan.direct.push(EditorCommand::SetNodeKind {
                            id: style_id,
                            kind: renamite_model::NodeKind::Style(source_kind.clone()),
                        });
                    }
                }
                plans.push(plan);
            }
        }

        let needs_work = plans
            .iter()
            .any(|p| !p.direct.is_empty() || !p.local.is_empty());
        if !needs_work {
            self.status = Some("Nothing to paste".into());
            self.repaint();
            return;
        }

        self.finalize_open_edit();
        self.history.begin("Paste style");
        let mut failed = false;

        for plan in plans {
            for cmd in plan.direct {
                if self.history_apply_full(cmd).is_none() {
                    failed = true;
                    break;
                }
            }
            if failed || plan.local.is_empty() {
                continue;
            }

            let Some((parent, index)) = self.file.document.locate(plan.target) else {
                failed = true;
                break;
            };
            let Some(group) = self.history_apply(EditorCommand::InsertNode {
                parent,
                index,
                tree: renamite_history::NodeTree::leaf(Node::new(
                    "Styled",
                    renamite_model::NodeKind::Group,
                )),
            }) else {
                failed = true;
                break;
            };
            if self
                .history_apply_full(EditorCommand::MoveNode {
                    id: plan.target,
                    new_parent: renamite_model::Parent::Node(group),
                    index: 0,
                })
                .is_none()
            {
                failed = true;
                break;
            }
            for kind in plan.local {
                let name = match kind {
                    renamite_model::StyleKind::Fill { .. } => "Fill",
                    renamite_model::StyleKind::Stroke { .. } => "Stroke",
                };
                if self
                    .history_apply_full(EditorCommand::InsertNode {
                        parent: renamite_model::Parent::Node(group),
                        index: usize::MAX,
                        tree: renamite_history::NodeTree::leaf(Node::new(
                            name,
                            renamite_model::NodeKind::Style(kind),
                        )),
                    })
                    .is_none()
                {
                    failed = true;
                    break;
                }
            }
        }

        self.history.commit();
        self.dirty = true;
        if failed {
            self.status = Some("Paste style failed on part of the selection".into());
        }
        self.bump();
    }

    pub fn duplicate_selection(&mut self) {
        self.finalize_open_edit();
        let items: Vec<(ClipboardItem, renamite_model::NodeId)> = self
            .selected_roots()
            .into_iter()
            .filter_map(|id| {
                if self
                    .file
                    .document
                    .nodes
                    .get(id)
                    .map(|n| n.locked)
                    .unwrap_or(true)
                {
                    return None;
                }
                Some((
                    ClipboardItem {
                        tree: self.tree_of(id),
                        source_parent: renamite_model::Parent::Comp(self.file.document.main),
                        source_index: 0,
                    },
                    id,
                ))
            })
            .collect();
        if items.is_empty() {
            self.status = Some("Nothing to duplicate: selection is empty or locked".into());
            self.repaint();
            return;
        }
        let trees: Vec<ClipboardItem> = items.into_iter().map(|(item, _)| item).collect();
        let created = self.insert_trees(trees, DVec2::new(20.0, 20.0), "Duplicate");
        if !created.is_empty() {
            self.selection.nodes = created;
            self.ensure_selection_visible();
            self.bump();
        }
    }

    pub fn cut_selection(&mut self) {
        self.clipboard_from_selection(true);
    }

    pub fn copy_selection(&mut self) {
        self.clipboard_from_selection(false);
    }

    pub fn paste_selection(&mut self) {
        self.paste_clipboard();
    }

    pub fn reverse_selected_paths(&mut self) {
        use renamite_model::{NodeKind, ShapeKind};

        let mut paths: Vec<renamite_model::NodeId> = Vec::new();
        for selected in self.selection.nodes.iter().copied() {
            let Some(node) = self.file.document.nodes.get(selected) else {
                continue;
            };
            match &node.kind {
                NodeKind::Shape(ShapeKind::Path(_)) => {
                    if !paths.contains(&selected) {
                        paths.push(selected);
                    }
                }
                NodeKind::Group | NodeKind::Layer(_) => {
                    for child in node.children.iter().copied() {
                        if matches!(
                            self.file.document.nodes.get(child).map(|n| &n.kind),
                            Some(NodeKind::Shape(ShapeKind::Path(_)))
                        ) && !paths.contains(&child)
                        {
                            paths.push(child);
                        }
                    }
                }
                _ => {}
            }
        }

        if paths.is_empty() {
            self.status = Some("Select a path to reverse".into());
            self.repaint();
            return;
        }

        let commands: SmallVec<[EditorCommand; 4]> = paths
            .into_iter()
            .map(|id| EditorCommand::ReversePath { id })
            .collect();

        self.apply_outputs(smallvec![
            ToolOutput::BeginTransaction("Reverse path".into()),
            ToolOutput::Commands(commands),
            ToolOutput::CommitTransaction,
        ]);
    }

    pub fn convert_selection_to_path(&mut self) {
        use renamite_model::{NodeKind, Overrides, ShapeKind};

        let frame = self.playback.head;
        let mut commands: SmallVec<[EditorCommand; 4]> = SmallVec::new();

        for id in self.selection.nodes.iter().copied() {
            let Some(shape) = self
                .file
                .document
                .nodes
                .get(id)
                .and_then(|node| match &node.kind {
                    NodeKind::Shape(
                        shape @ (ShapeKind::Rect { .. }
                        | ShapeKind::Ellipse { .. }
                        | ShapeKind::Star { .. }
                        | ShapeKind::Polygon { .. }),
                    ) => Some(shape.clone()),
                    _ => None,
                })
            else {
                continue;
            };

            let bezier = renamite_model::shape_path(&shape, id, frame, &Overrides::default());
            let path = renamite_geometry::VectorPath::from_bez_path(&bezier);
            commands.push(EditorCommand::SetNodeKind {
                id,
                kind: NodeKind::Shape(ShapeKind::Path(renamite_animation::Animated::new(path))),
            });
        }

        if commands.is_empty() {
            self.status = Some("Select a primitive shape to convert".into());
            self.repaint();
            return;
        }

        self.apply_outputs(smallvec![
            ToolOutput::BeginTransaction("Object to path".into()),
            ToolOutput::Commands(commands),
            ToolOutput::CommitTransaction,
        ]);
    }

    fn selected_shape_roots_in_z_order(&self) -> Vec<renamite_model::NodeId> {
        use renamite_model::{NodeKind, ShapeKind};

        let doc = &self.file.document;
        let mut out = Vec::new();
        fn visit(
            doc: &renamite_model::Document,
            children: &[renamite_model::NodeId],
            sel: &[renamite_model::NodeId],
            inherited_selected: bool,
            out: &mut Vec<renamite_model::NodeId>,
        ) {
            for &id in children.iter().rev() {
                let Some(node) = doc.nodes.get(id) else {
                    continue;
                };
                let selected = inherited_selected || sel.contains(&id);

                match &node.kind {
                    NodeKind::Shape(
                        ShapeKind::Path(_)
                        | ShapeKind::Rect { .. }
                        | ShapeKind::Ellipse { .. }
                        | ShapeKind::Star { .. }
                        | ShapeKind::Polygon { .. }
                        | ShapeKind::CompoundPath(_),
                    ) => {
                        if selected {
                            out.push(id);
                        }
                    }
                    NodeKind::Group | NodeKind::Layer(_) => {
                        visit(doc, &node.children, sel, selected, out);
                    }
                    _ => {}
                }
            }
        }
        let comp = doc.main;
        let Some(c) = doc.compositions.get(comp) else {
            return out;
        };
        visit(doc, &c.children, &self.selection.nodes, false, &mut out);

        let mut seen = HashSet::new();
        out.retain(|id| seen.insert(*id));
        out
    }

    fn contours_in_subject_space(
        &self,
        id: renamite_model::NodeId,
        subject: renamite_model::NodeId,
    ) -> Result<Vec<renamite_geometry::VectorPath>, String> {
        use renamite_model::{NodeKind, Overrides, ShapeKind};

        let frame = self.playback.head;
        let doc = &self.file.document;
        let Some(node) = doc.nodes.get(id) else {
            return Err("Selected node no longer exists".into());
        };
        let local: kurbo::BezPath = match &node.kind {
            NodeKind::Shape(ShapeKind::CompoundPath(compound)) => compound.to_bez_path(frame),
            NodeKind::Shape(shape) => {
                renamite_model::shape_path(shape, id, frame, &Overrides::default())
            }
            _ => return Err("Selection contains non-shape nodes".into()),
        };

        let Some(from) = renamite_model::node_transform_context(doc, id, frame) else {
            return Err("Cannot resolve transforms for the selection".into());
        };
        let Some(to) = renamite_model::node_transform_context(doc, subject, frame) else {
            return Err("Cannot resolve transforms for the selection".into());
        };
        let to_subject_space = to.world.inverse() * from.world;

        let mapped = to_subject_space * local;
        Ok(renamite_geometry::split_bez_subpaths(&mapped))
    }
    pub fn boolean_selection(&mut self, operation: SelectionBoolean) {
        let label = format!("{operation:?}");
        let ids = self.selected_shape_roots_in_z_order();

        if ids.len() < 2 {
            self.status = Some("Select at least two closed shapes".into());
            self.repaint();
            return;
        }

        let subject = ids[0];
        let mut accumulated: Vec<renamite_geometry::VectorPath> =
            match self.contours_in_subject_space(subject, subject) {
                Ok(paths) => paths,
                Err(message) => {
                    self.status = Some(message);
                    self.repaint();
                    return;
                }
            };
        if accumulated.is_empty() {
            self.status = Some("The subject has no geometry".into());
            self.repaint();
            return;
        }

        for cutter in ids.iter().copied().skip(1) {
            let rhs = match self.contours_in_subject_space(cutter, subject) {
                Ok(paths) => paths,
                Err(message) => {
                    self.status = Some(message);
                    self.repaint();
                    return;
                }
            };
            let rhs_bez = renamite_geometry::contours_to_bez(&rhs);

            let op = match operation {
                SelectionBoolean::Union => renamite_geometry::BooleanOp::Union,
                SelectionBoolean::Difference => renamite_geometry::BooleanOp::Difference,
                SelectionBoolean::Intersection => renamite_geometry::BooleanOp::Intersection,
                SelectionBoolean::Xor => renamite_geometry::BooleanOp::Xor,
            };

            let result = renamite_geometry::boolean_bez(
                &renamite_geometry::contours_to_bez(&accumulated),
                &rhs_bez,
                op,
            );

            match result {
                Ok(contours) => accumulated = contours,
                Err(error) => {
                    self.status = Some(error.to_string());
                    self.repaint();
                    return;
                }
            }
        }

        if accumulated.is_empty() {
            let commands: SmallVec<[EditorCommand; 4]> = ids
                .iter()
                .copied()
                .map(|id| EditorCommand::RemoveNode { id })
                .collect();

            self.apply_outputs(smallvec![
                ToolOutput::BeginTransaction(label),
                ToolOutput::Commands(commands),
                ToolOutput::CommitTransaction,
                ToolOutput::RequestSelection(renamite_history::SelectionChange::Set(Vec::new())),
            ]);
            return;
        }

        let replacement = renamite_model::NodeKind::Shape(renamite_model::ShapeKind::CompoundPath(
            renamite_model::CompoundPath {
                contours: accumulated
                    .into_iter()
                    .map(renamite_animation::Animated::new)
                    .collect(),
            },
        ));

        let mut commands: SmallVec<[EditorCommand; 4]> = SmallVec::new();
        commands.push(EditorCommand::SetNodeKind {
            id: subject,
            kind: replacement,
        });

        for id in ids.iter().copied().skip(1) {
            commands.push(EditorCommand::RemoveNode { id });
        }

        self.apply_outputs(smallvec![
            ToolOutput::BeginTransaction(label),
            ToolOutput::Commands(commands),
            ToolOutput::CommitTransaction,
            ToolOutput::RequestSelection(renamite_history::SelectionChange::Set(vec![subject])),
        ]);
    }

    pub fn divide_selection(&mut self) {
        use renamite_history::NodeTree;
        use renamite_model::{Node, NodeKind, Parent, ShapeKind};

        let ids = self.selected_shape_roots_in_z_order();
        if ids.len() < 2 {
            self.status = Some("Select at least two closed shapes to divide".into());
            self.repaint();
            return;
        }
        let subject = ids[0];

        {
            let doc = &self.file.document;
            let frame = self.playback.head;
            for &id in &ids {
                let closed = match doc.nodes.get(id).map(|n| &n.kind) {
                    Some(NodeKind::Shape(ShapeKind::Path(p))) => p.value_at(frame).closed,
                    Some(NodeKind::Shape(ShapeKind::CompoundPath(c))) => {
                        c.contours.iter().all(|p| p.value_at(frame).closed)
                    }
                    Some(NodeKind::Shape(_)) => true,
                    _ => false,
                };
                if !closed {
                    self.status =
                        Some("Division requires closed shapes (open cutting not supported)".into());
                    self.repaint();
                    return;
                }
            }
        }

        let mut pieces: Vec<Vec<renamite_geometry::VectorPath>> = Vec::new();
        let mut remaining: Vec<renamite_geometry::VectorPath> =
            match self.contours_in_subject_space(subject, subject) {
                Ok(paths) => paths,
                Err(message) => {
                    self.status = Some(message);
                    self.repaint();
                    return;
                }
            };

        for cutter in ids.iter().copied().skip(1) {
            let rhs = match self.contours_in_subject_space(cutter, subject) {
                Ok(paths) => paths,
                Err(message) => {
                    self.status = Some(message);
                    self.repaint();
                    return;
                }
            };
            let rhs_bez = renamite_geometry::contours_to_bez(&rhs);

            let current = mem::take(&mut remaining);
            if current.is_empty() {
                break;
            }
            let cur_bez = renamite_geometry::contours_to_bez(&current);

            match (
                renamite_geometry::boolean_bez(
                    &cur_bez,
                    &rhs_bez,
                    renamite_geometry::BooleanOp::Intersection,
                ),
                renamite_geometry::boolean_bez(
                    &cur_bez,
                    &rhs_bez,
                    renamite_geometry::BooleanOp::Difference,
                ),
            ) {
                (Ok(inside), Ok(outside)) => {
                    if !inside.is_empty() {
                        pieces.push(inside);
                    }
                    remaining = outside;

                    if remaining.is_empty() {
                        break;
                    }
                }
                (Err(e), _) | (_, Err(e)) => {
                    self.status = Some(e.to_string());
                    self.repaint();
                    return;
                }
            }
        }
        if !remaining.is_empty() {
            pieces.push(remaining);
        }
        let output_kinds: Vec<renamite_model::ShapeKind> = pieces
            .into_iter()
            .filter_map(shape_kind_from_contours)
            .collect();
        if output_kinds.is_empty() {
            self.status = Some("Division produced no geometry".into());
            self.repaint();
            return;
        }

        let mut commands: SmallVec<[EditorCommand; 4]> = SmallVec::new();
        commands.push(EditorCommand::SetNodeKind {
            id: subject,
            kind: renamite_model::NodeKind::Shape(output_kinds[0].clone()),
        });

        let (parent, index) = match self.file.document.locate(subject) {
            Some((Parent::Node(p), i)) => (Parent::Node(p), i),
            Some((Parent::Comp(c), i)) => (Parent::Comp(c), i),
            None => {
                self.status = Some("Subject is not attached".into());
                self.repaint();
                return;
            }
        };
        let template_name = self
            .file
            .document
            .nodes
            .get(subject)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| "Piece".into());

        for (k, kind) in output_kinds.iter().enumerate().skip(1) {
            let mut node = self
                .file
                .document
                .nodes
                .get(subject)
                .cloned()
                .unwrap_or_else(|| Node::new(template_name.as_str(), NodeKind::Group));
            node.name = format!("{template_name} {}", k + 1);
            node.parent = None;
            node.children.clear();
            node.kind = renamite_model::NodeKind::Shape(kind.clone());
            commands.push(EditorCommand::InsertNode {
                parent,
                index: index + k,
                tree: NodeTree::leaf(node),
            });
        }

        for id in ids.iter().copied().skip(1) {
            commands.push(EditorCommand::RemoveNode { id });
        }

        self.apply_outputs(smallvec![
            ToolOutput::BeginTransaction("Division".into()),
            ToolOutput::Commands(commands),
            ToolOutput::CommitTransaction,
            ToolOutput::RequestSelection(renamite_history::SelectionChange::Set(vec![subject])),
        ]);
    }

    pub fn combine_selection(&mut self) {
        let ids = self.selected_shape_roots_in_z_order();
        if ids.len() < 2 {
            self.status = Some("Select at least two objects to combine".into());
            self.repaint();
            return;
        }
        let bottom = ids[0];

        let mut contours: Vec<renamite_geometry::VectorPath> = Vec::new();
        for id in &ids {
            match self.contours_in_subject_space(*id, bottom) {
                Ok(paths) => contours.extend(paths),
                Err(message) => {
                    self.status = Some(message);
                    self.repaint();
                    return;
                }
            }
        }
        if contours.is_empty() {
            self.status = Some("Nothing to combine".into());
            self.repaint();
            return;
        }

        let mut commands: SmallVec<[EditorCommand; 4]> = SmallVec::new();
        commands.push(EditorCommand::SetNodeKind {
            id: bottom,
            kind: renamite_model::NodeKind::Shape(renamite_model::ShapeKind::CompoundPath(
                renamite_model::CompoundPath {
                    contours: contours
                        .into_iter()
                        .map(renamite_animation::Animated::new)
                        .collect(),
                },
            )),
        });
        for id in ids.iter().copied().skip(1) {
            commands.push(EditorCommand::RemoveNode { id });
        }

        self.apply_outputs(smallvec![
            ToolOutput::BeginTransaction("Combine".into()),
            ToolOutput::Commands(commands),
            ToolOutput::CommitTransaction,
            ToolOutput::RequestSelection(renamite_history::SelectionChange::Set(vec![bottom])),
        ]);
    }

    pub fn break_apart_selection(&mut self) {
        use renamite_history::NodeTree;
        use renamite_model::{Node, Parent};

        if self.selection.nodes.len() != 1 {
            self.status = Some("Select one combined path to break apart".into());
            self.repaint();
            return;
        }
        let id = self.selection.nodes[0];
        let Some(contours) = (match self.file.document.nodes.get(id).map(|n| &n.kind) {
            Some(renamite_model::NodeKind::Shape(renamite_model::ShapeKind::CompoundPath(
                compound,
            ))) => Some(
                compound
                    .contours
                    .iter()
                    .map(|c| c.value_at(self.playback.head))
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        }) else {
            self.status = Some("Break apart requires a compound path".into());
            self.repaint();
            return;
        };
        if contours.len() < 2 {
            self.status = Some("The compound path has a single contour".into());
            self.repaint();
            return;
        }

        let (parent, index) = match self.file.document.locate(id) {
            Some((Parent::Node(p), i)) => (Parent::Node(p), i),
            Some((Parent::Comp(c), i)) => (Parent::Comp(c), i),
            None => {
                self.status = Some("Node is not attached".into());
                self.repaint();
                return;
            }
        };
        let name = self
            .file
            .document
            .nodes
            .get(id)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| "Path".into());

        let mut commands: SmallVec<[EditorCommand; 4]> = SmallVec::new();
        commands.push(EditorCommand::SetNodeKind {
            id,
            kind: renamite_model::NodeKind::Shape(renamite_model::ShapeKind::Path(
                renamite_animation::Animated::new(contours[0].clone()),
            )),
        });
        for (k, contour) in contours.iter().enumerate().skip(1) {
            let mut node = self
                .file
                .document
                .nodes
                .get(id)
                .cloned()
                .unwrap_or_else(|| Node::new(name.as_str(), renamite_model::NodeKind::Group));
            node.name = format!("{name} {}", k + 1);
            node.parent = None;
            node.children.clear();
            node.kind = renamite_model::NodeKind::Shape(renamite_model::ShapeKind::Path(
                renamite_animation::Animated::new(contour.clone()),
            ));
            commands.push(EditorCommand::InsertNode {
                parent,
                index: index + k,
                tree: NodeTree::leaf(node),
            });
        }

        self.apply_outputs(smallvec![
            ToolOutput::BeginTransaction("Break apart".into()),
            ToolOutput::Commands(commands),
            ToolOutput::CommitTransaction,
            ToolOutput::RequestSelection(renamite_history::SelectionChange::Set(vec![id])),
        ]);
    }

    pub fn align_selection(
        &mut self,
        op: renamite_behavior_common::align::AlignOp,
        anchor: renamite_behavior_common::align::AlignAnchor,
    ) {
        use renamite_behavior_common::align;
        let roots = self.selected_roots();
        if roots.is_empty() {
            return;
        }
        let bounds = align::root_bounds(&self.file.document, &self.engine.scene().items, &roots);
        if bounds.is_empty() {
            self.status = Some("Nothing to align".into());
            self.repaint();
            return;
        }
        let target = match anchor {
            align::AlignAnchor::Selection => match align::union_bounds(&bounds) {
                Some(b) => b,
                None => return,
            },
            align::AlignAnchor::Page => {
                align::page_bounds(self.file.document.compositions[self.file.document.main].size)
            }
        };
        let frame = self.playback.head;
        let record = self.record_for_writes();
        let prop = PropPath::new("transform.position");
        let mut cmds: SmallVec<[EditorCommand; 4]> = SmallVec::new();
        for (id, delta) in align::align_deltas(&bounds, target, op) {
            if self
                .file
                .document
                .nodes
                .get(id)
                .map(|n| n.locked)
                .unwrap_or(true)
            {
                continue;
            }
            if !delta.is_finite() || delta.length_squared() < 1e-24 {
                continue;
            }
            let Ok(Value::DVec2(current)) = self.file.document.value_at(id, &prop, frame) else {
                continue;
            };
            let local =
                renamite_model::world_delta_to_parent(&self.file.document, id, frame, delta)
                    .unwrap_or(delta);
            if !local.is_finite() {
                continue;
            }
            cmds.push(renamite_history::resolve_property_edit(
                &self.file.document,
                id,
                &prop,
                Value::DVec2(current + local),
                Frame(frame.round() as i64),
                record,
            ));
        }
        if cmds.is_empty() {
            self.status = Some("Nothing to align".into());
            self.repaint();
            return;
        }
        self.apply_outputs(smallvec![
            ToolOutput::BeginTransaction("Align".into()),
            ToolOutput::Commands(cmds),
            ToolOutput::CommitTransaction,
        ]);
    }

    pub fn distribute_selection(&mut self, horizontal: bool) {
        use renamite_behavior_common::align;
        let roots = self.selected_roots();
        if roots.len() < 3 {
            self.status = Some("Select 3+ objects to distribute".into());
            self.repaint();
            return;
        }
        let bounds = align::root_bounds(&self.file.document, &self.engine.scene().items, &roots);
        if bounds.len() < 3 {
            self.status = Some("Select 3+ visible objects to distribute".into());
            self.repaint();
            return;
        }
        let Some(deltas) = align::distribute_deltas(&bounds, horizontal) else {
            self.status = Some("Nothing to distribute".into());
            self.repaint();
            return;
        };
        let frame = self.playback.head;
        let record = self.record_for_writes();
        let prop = PropPath::new("transform.position");
        let mut cmds: SmallVec<[EditorCommand; 4]> = SmallVec::new();
        for (id, delta) in deltas {
            if self
                .file
                .document
                .nodes
                .get(id)
                .map(|n| n.locked)
                .unwrap_or(true)
            {
                continue;
            }
            if !delta.is_finite() || delta.length_squared() < 1e-24 {
                continue;
            }
            let Ok(Value::DVec2(current)) = self.file.document.value_at(id, &prop, frame) else {
                continue;
            };
            let local =
                renamite_model::world_delta_to_parent(&self.file.document, id, frame, delta)
                    .unwrap_or(delta);
            if !local.is_finite() {
                continue;
            }
            cmds.push(renamite_history::resolve_property_edit(
                &self.file.document,
                id,
                &prop,
                Value::DVec2(current + local),
                Frame(frame.round() as i64),
                record,
            ));
        }
        if cmds.is_empty() {
            self.status = Some("Nothing to distribute".into());
            self.repaint();
            return;
        }
        self.apply_outputs(smallvec![
            ToolOutput::BeginTransaction("Distribute".into()),
            ToolOutput::Commands(cmds),
            ToolOutput::CommitTransaction,
        ]);
    }

    pub fn flip_selection(&mut self, horizontal: bool) {
        let roots = self.selected_roots();
        if roots.is_empty() {
            return;
        }
        let frame = self.playback.head;
        let record = self.record_for_writes();
        let prop = PropPath::new("transform.scale");
        let mut cmds: SmallVec<[EditorCommand; 4]> = SmallVec::new();
        for id in roots {
            let Some(node) = self.file.document.nodes.get(id) else {
                continue;
            };
            if node.locked {
                continue;
            }
            let Ok(Value::DVec2(current)) = self.file.document.value_at(id, &prop, frame) else {
                continue;
            };
            let next = if horizontal {
                DVec2::new(-current.x, current.y)
            } else {
                DVec2::new(current.x, -current.y)
            };
            if !next.is_finite() {
                continue;
            }
            cmds.push(renamite_history::resolve_property_edit(
                &self.file.document,
                id,
                &prop,
                Value::DVec2(next),
                Frame(frame.round() as i64),
                record,
            ));
        }
        if cmds.is_empty() {
            self.status = Some("Flip does not apply to the selection".into());
            self.repaint();
            return;
        }
        self.apply_outputs(smallvec![
            ToolOutput::BeginTransaction("Flip".into()),
            ToolOutput::Commands(cmds),
            ToolOutput::CommitTransaction,
        ]);
    }

    pub fn nudge_selection(&mut self, dir: DVec2, mods: Modifiers) {
        if self.selection.nodes.is_empty() || self.tool.is_dragging(self.active_tool) {
            return;
        }
        let scale = self.viewport.view.scale.max(1e-6);
        let step = if mods.alt {
            1.0
        } else if mods.shift {
            20.0
        } else {
            2.0
        } / scale;
        let delta = dir * step;
        if !delta.is_finite() {
            return;
        }
        let frame = self.playback.head;
        let record = self.record_for_writes();
        let prop = PropPath::new("transform.position");
        let roots = self.selected_roots();
        let cmds: SmallVec<[EditorCommand; 4]> = roots
            .into_iter()
            .filter_map(|id| {
                let node = self.file.document.nodes.get(id)?;
                if node.locked {
                    return None;
                }
                let Ok(Value::DVec2(current)) = self.file.document.value_at(id, &prop, frame)
                else {
                    return None;
                };
                let local =
                    renamite_model::world_delta_to_parent(&self.file.document, id, frame, delta)
                        .unwrap_or(delta);
                if !local.is_finite() {
                    return None;
                }
                Some(renamite_history::resolve_property_edit(
                    &self.file.document,
                    id,
                    &prop,
                    Value::DVec2(current + local),
                    Frame(frame.round() as i64),
                    record,
                ))
            })
            .collect();
        if cmds.is_empty() {
            return;
        }
        self.apply_outputs(smallvec![
            ToolOutput::BeginTransaction("Nudge".into()),
            ToolOutput::Commands(cmds),
            ToolOutput::CommitTransaction,
        ]);
    }

    pub fn delete_selection_nodes(&mut self) {
        if self.selection.nodes.is_empty() || self.tool.is_dragging(self.active_tool) {
            return;
        }
        let cmds: SmallVec<[EditorCommand; 4]> = self
            .selected_roots()
            .into_iter()
            .filter(|id| {
                self.file
                    .document
                    .nodes
                    .get(*id)
                    .map(|n| !n.locked)
                    .unwrap_or(false)
            })
            .map(|id| EditorCommand::RemoveNode { id })
            .collect();
        if cmds.is_empty() {
            self.status = Some("Locked layers cannot be deleted".into());
            self.repaint();
            return;
        }
        self.apply_outputs(smallvec![
            ToolOutput::BeginTransaction("Delete".into()),
            ToolOutput::Commands(cmds),
            ToolOutput::CommitTransaction,
            ToolOutput::RequestSelection(renamite_history::SelectionChange::Set(vec![])),
        ]);
    }

    pub fn deselect_when_idle(&mut self) -> bool {
        if self.tool.is_dragging(self.active_tool) || self.selection.nodes.is_empty() {
            return false;
        }
        self.selection.nodes.clear();
        self.repaint();
        true
    }

    pub fn cycle_selection(&mut self, reverse: bool) {
        let comp = self.file.document.main;
        let children = self.file.document.compositions[comp].children.clone();
        if children.is_empty() {
            return;
        }
        let next = match self
            .selection
            .nodes
            .first()
            .copied()
            .and_then(|cur| children.iter().position(|&c| c == cur))
        {
            Some(i) => {
                children[(i + if reverse { children.len() - 1 } else { 1 }) % children.len()]
            }
            None => children[0],
        };
        self.selection.nodes = vec![next];
        self.ensure_selection_visible();
        self.repaint();
    }

    pub fn select_all_top_level(&mut self) {
        let comp = self.file.document.main;
        self.selection.nodes = self.file.document.compositions[comp].children.clone();
        self.ensure_selection_visible();
        self.repaint();
    }

    pub fn simplify_selection(&mut self) {
        use renamite_model::{NodeKind, ShapeKind};

        let tolerance = 0.75 / self.viewport.view.scale.max(1e-6);
        let targets = self.selection.nodes.clone();
        if targets.is_empty() {
            self.status = Some("Select a path to simplify".into());
            self.repaint();
            return;
        }

        let mut commands: SmallVec<[EditorCommand; 4]> = SmallVec::new();
        {
            let doc = &self.file.document;
            let frame = self.playback.head;
            for id in targets {
                let Some(node) = doc.nodes.get(id) else {
                    continue;
                };
                let simplified = match &node.kind {
                    NodeKind::Shape(ShapeKind::Path(path)) => {
                        renamite_geometry::simplify_path(&path.value_at(frame), tolerance)
                    }
                    NodeKind::Shape(ShapeKind::CompoundPath(compound)) => {
                        let contours: Vec<_> = compound
                            .contours
                            .iter()
                            .map(|c| {
                                renamite_geometry::simplify_path(&c.value_at(frame), tolerance)
                            })
                            .collect();
                        if contours.is_empty() {
                            continue;
                        }
                        commands.push(EditorCommand::SetNodeKind {
                            id,
                            kind: NodeKind::Shape(ShapeKind::CompoundPath(
                                renamite_model::CompoundPath {
                                    contours: contours
                                        .into_iter()
                                        .map(renamite_animation::Animated::new)
                                        .collect(),
                                },
                            )),
                        });
                        continue;
                    }
                    _ => continue,
                };
                if simplified.anchors.is_empty() {
                    continue;
                }
                commands.push(EditorCommand::SetNodeKind {
                    id,
                    kind: NodeKind::Shape(ShapeKind::Path(renamite_animation::Animated::new(
                        simplified,
                    ))),
                });
            }
        }

        if commands.is_empty() {
            self.status = Some("Select vector paths to simplify".into());
            self.repaint();
            return;
        }

        self.apply_outputs(smallvec![
            ToolOutput::BeginTransaction("Simplify".into()),
            ToolOutput::Commands(commands),
            ToolOutput::CommitTransaction,
        ]);
    }

    pub fn stroke_selection_to_path(&mut self) {
        use renamite_model::{FillRule, NodeKind, Overrides, ShapeKind, StyleKind};

        let frame = self.playback.head;
        let targets = self.selection.nodes.clone();
        if targets.is_empty() {
            self.status = Some("Select stroked shapes to convert".into());
            self.repaint();
            return;
        }

        struct Conversion {
            shape: renamite_model::NodeId,
            compound: renamite_model::CompoundPath,
            stroke_id: Option<renamite_model::NodeId>,
            stroke_shared: bool,
            paint: renamite_model::StylePaint,
        }

        let mut conversions: Vec<Conversion> = Vec::new();
        {
            let doc = &self.file.document;
            for id in targets {
                let Some(node) = doc.nodes.get(id) else {
                    continue;
                };
                let geometry: Vec<kurbo::BezPath> = match &node.kind {
                    NodeKind::Shape(ShapeKind::CompoundPath(c)) => c
                        .contours
                        .iter()
                        .map(|p| p.value_at(frame).to_bez_path())
                        .collect(),
                    NodeKind::Shape(shape) => vec![renamite_model::shape_path(
                        shape,
                        id,
                        frame,
                        &Overrides::default(),
                    )],
                    _ => continue,
                };

                let Some(stroke_id) = nearest_style_node(doc, id, true) else {
                    continue;
                };
                let Some(stroke) = doc.nodes.get(stroke_id).map(|n| match &n.kind {
                    NodeKind::Style(st) => st.clone(),
                    _ => unreachable!("nearest_style_node returns style nodes"),
                }) else {
                    continue;
                };
                let StyleKind::Stroke {
                    paint,
                    width,
                    cap,
                    join,
                    dash,
                    miter_limit,
                } = stroke
                else {
                    continue;
                };

                let width_value = width.value_at(frame);
                let miter_value = miter_limit.value_at(frame).clamp(1.0, 10.0);
                let dash_sample: Option<(Vec<f64>, f64)> = dash.as_ref().map(|d| {
                    (
                        d.dashes.iter().map(|x| x.value_at(frame)).collect(),
                        d.offset.value_at(frame),
                    )
                });

                let mut outlines: Vec<renamite_geometry::VectorPath> = Vec::new();
                for bez in &geometry {
                    for path in renamite_geometry::split_bez_subpaths(bez) {
                        if let Ok(mut pieces) = renamite_geometry::stroke_to_paths(
                            &path,
                            width_value,
                            kurbo_cap(cap),
                            kurbo_join(join),
                            miter_value,
                            dash_sample
                                .as_ref()
                                .map(|(pattern, offset)| (pattern.as_slice(), *offset)),
                            0.1,
                        ) {
                            outlines.append(&mut pieces);
                        }
                    }
                }
                if outlines.is_empty() {
                    continue;
                }

                let Some((_, shared)) = style_scope_info(doc, stroke_id) else {
                    continue;
                };

                conversions.push(Conversion {
                    shape: id,
                    compound: renamite_model::CompoundPath {
                        contours: outlines
                            .into_iter()
                            .map(renamite_animation::Animated::new)
                            .collect(),
                    },
                    stroke_id: Some(stroke_id),
                    stroke_shared: shared,
                    paint,
                });
            }
        }

        if conversions.is_empty() {
            self.status = Some("No stroked shapes found".into());
            self.repaint();
            return;
        }

        self.finalize_open_edit();
        self.history.begin("Stroke to path");
        let mut failed = false;

        for conv in &conversions {
            if self
                .history_apply_full(EditorCommand::SetNodeKind {
                    id: conv.shape,
                    kind: NodeKind::Shape(ShapeKind::CompoundPath(conv.compound.clone())),
                })
                .is_none()
            {
                failed = true;
                break;
            }

            if !conv.stroke_shared {
                if let Some(style_id) = conv.stroke_id
                    && self
                        .history_apply_full(EditorCommand::SetNodeKind {
                            id: style_id,
                            kind: NodeKind::Style(StyleKind::Fill {
                                paint: conv.paint.clone(),
                                rule: FillRule::NonZero,
                            }),
                        })
                        .is_none()
                {
                    failed = true;
                }
                continue;
            }

            let Some((shape_parent, shape_index)) = self.file.document.locate(conv.shape) else {
                failed = true;
                break;
            };
            let Some(group) = self.history_apply(EditorCommand::InsertNode {
                parent: shape_parent,
                index: shape_index,
                tree: renamite_history::NodeTree::leaf(renamite_model::Node::new(
                    "Stroked Path",
                    NodeKind::Group,
                )),
            }) else {
                failed = true;
                break;
            };
            let moved = self.history_apply_full(EditorCommand::MoveNode {
                id: conv.shape,
                new_parent: renamite_model::Parent::Node(group),
                index: 0,
            });
            let filled = self.history_apply_full(EditorCommand::InsertNode {
                parent: renamite_model::Parent::Node(group),
                index: usize::MAX,
                tree: renamite_history::NodeTree::leaf(renamite_model::Node::new(
                    "Fill",
                    NodeKind::Style(StyleKind::Fill {
                        paint: conv.paint.clone(),
                        rule: FillRule::NonZero,
                    }),
                )),
            });
            if moved.is_none() || filled.is_none() {
                failed = true;
                break;
            }
        }

        self.history.commit();
        self.dirty = true;
        if failed {
            self.status = Some("Stroke to path failed on part of the selection".into());
        }
        self.bump();
    }

    pub fn add_ellipse_layer(&mut self) {
        use renamite_history::NodeTree;
        use renamite_model::{FillRule, Node, NodeKind, Parent, ShapeKind, StyleKind};

        let comp = self.file.document.main;
        let (w, h) = self.file.document.compositions[comp].size;
        let center = DVec2::new(w as f64 * 0.5, h as f64 * 0.5);
        let shape = Node::new(
            "Ellipse",
            NodeKind::Shape(ShapeKind::Ellipse {
                pos: renamite_animation::Animated::new(center),
                size: renamite_animation::Animated::new(DVec2::new(120.0, 120.0)),
            }),
        );
        let fill = Node::new(
            "Fill",
            NodeKind::Style(StyleKind::Fill {
                paint: self.current_paint.clone(),
                rule: FillRule::NonZero,
            }),
        );
        let tree = NodeTree::with_children(shape, vec![NodeTree::leaf(fill)]);

        self.history.begin("Add layer".to_owned());
        if let Some(id) = self.history_apply(EditorCommand::InsertNode {
            parent: Parent::Comp(comp),
            index: 0,
            tree,
        }) {
            self.selection.nodes = vec![id];
            self.ensure_selection_visible();
        }
        self.history.commit();
        self.dirty = true;
        self.bump();
    }

    pub fn set_all_expanded(&mut self, expanded: bool) {
        let doc = &self.file.document;
        let mut all = HashSet::new();
        fn collect(
            node: renamite_model::NodeId,
            doc: &renamite_model::Document,
            out: &mut HashSet<renamite_model::NodeId>,
        ) {
            if let Some(n) = doc.nodes.get(node) {
                if !n.children.is_empty() {
                    out.insert(node);
                }
                for &child in &n.children {
                    collect(child, doc, out);
                }
            }
        }
        let main = doc.main;
        if let Some(comp) = doc.compositions.get(main) {
            for child in &comp.children {
                collect(*child, doc, &mut all);
            }
        }
        if expanded {
            self.expanded_layers = all;
        } else {
            self.expanded_layers.clear();
        }
        self.repaint();
    }

    pub fn finish_layer_drag(&mut self) {
        let Some(drag) = self.layer_drag.take() else {
            return;
        };
        let rows = renamite_behavior_common::layers::flatten_layers(
            &self.file.document,
            self.file.document.main,
            &self.expanded_layers,
        );
        let Some(target) = rows.get(drag.hover_row) else {
            self.repaint();
            return;
        };
        if drag.id == target.id {
            self.repaint();
            return;
        }
        if renamite_behavior_common::layers::is_ancestor(&self.file.document, drag.id, target.id) {
            self.repaint();
            return;
        }
        if let renamite_model::Parent::Node(p) = target.parent
            && (p == drag.id
                || renamite_behavior_common::layers::is_ancestor(&self.file.document, drag.id, p))
        {
            self.repaint();
            return;
        }

        let Some(cmd) = renamite_behavior_common::layers::drop_command(
            drag.id,
            target,
            drag.before,
            drag.as_child,
        ) else {
            self.repaint();
            return;
        };
        if renamite_behavior_common::layers::move_is_noop(&self.file.document, &cmd) {
            self.repaint();
            return;
        }
        if drag.as_child {
            self.expanded_layers.insert(target.id);
        }
        self.apply_outputs(smallvec![
            ToolOutput::BeginTransaction("Reorder layer".into()),
            ToolOutput::Commands(smallvec![cmd]),
            ToolOutput::CommitTransaction,
        ]);
    }

    pub fn bump(&mut self) {
        self.engine.reevaluate(&self.file);
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    pub fn repaint(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    pub fn set_active_page(&mut self, page: PanelPage) {
        self.active_page = page;
        self.repaint();
    }

    pub fn set_mode(&mut self, mode: EditorMode) {
        if self.mode == mode {
            return;
        }
        let previous = self.mode;
        self.mode = mode;
        self.record = Self::record_for_mode(mode);
        match mode {
            EditorMode::Design => {
                self.stop_timeline_playback();
                self.disable_machine_preview();
                if matches!(self.active_page, PanelPage::Timeline | PanelPage::Interact) {
                    self.active_page = PanelPage::Canvas;
                }
            }
            EditorMode::Animate => {
                self.disable_machine_preview();
                self.active_page = PanelPage::Timeline;
            }
            EditorMode::Interact => {
                self.stop_timeline_playback();
                self.active_page = PanelPage::Interact;
                if self.active_machine.is_some() {
                    self.machine_preview_enabled = true;
                    self.reset_machine_preview();
                }
            }
        }
        if previous == EditorMode::Interact && mode != EditorMode::Interact {
            self.machine_graph_gesture = None;
        }
        self.repaint();
    }

    fn stop_timeline_playback(&mut self) {
        if self.playing {
            self.playing = false;
            self.playback.state = PlayState::Stopped;
        }
    }

    /// Record flag implied by the mode. Single source of truth so Design can
    /// never keyframe even if `record` was stuck from Animate.
    fn record_for_mode(mode: EditorMode) -> bool {
        matches!(mode, EditorMode::Animate)
    }

    /// Effective record flag for property writes: Design (and Interact) always
    /// produce static edits.
    pub fn record_for_writes(&self) -> bool {
        self.record && Self::record_for_mode(self.mode)
    }

    pub fn disable_machine_preview(&mut self) {
        if !self.machine_preview_enabled && self.machine_preview_inputs.is_empty() {
            return;
        }
        self.machine_preview_enabled = false;
        if matches!(self.machine_drag, Some(MachineDrag::PreviewNumber { .. })) {
            self.machine_drag = None;
        }
        self.engine.play_timeline(&self.file, LoopMode::Loop);
        self.engine.reevaluate(&self.file);
    }

    pub fn zoom_timeline(&mut self, factor: f64) {
        let old = self.timeline_zoom.max(0.5);
        self.timeline_zoom = (old * factor).clamp(0.5, 48.0);
        self.clamp_timeline_offset();
        self.repaint();
    }

    pub fn pan_timeline(&mut self, delta_px: f64) {
        self.timeline_offset_x += delta_px;
        self.clamp_timeline_offset();
        self.repaint();
    }

    fn clamp_timeline_offset(&mut self) {
        let range = self.file.document.compositions[self.file.document.main].range;
        let frames = (range.1.0 - range.0.0).max(1) as f64;
        let content_w = frames * self.timeline_zoom.max(0.5);
        self.timeline_offset_x = self
            .timeline_offset_x
            .clamp(0.0, (content_w + 48.0).max(0.0));
    }

    pub fn set_composition_range(&mut self, start: Option<Frame>, end: Option<Frame>) {
        let comp = self.file.document.main;
        self.apply_outputs(smallvec![
            ToolOutput::BeginTransaction("Duration".into()),
            ToolOutput::Commands(smallvec![EditorCommand::SetCompositionRange {
                comp,
                start,
                end
            }]),
            ToolOutput::CommitTransaction,
        ]);
        sync_playback_range(self);
    }

    pub fn step_frames(&mut self, delta: f64) {
        let range = self.file.document.compositions[self.file.document.main].range;
        self.playback.head = (self.playback.head + delta).clamp(range.0.0 as f64, range.1.0 as f64);
        let crate::session::Session {
            file,
            engine,
            playback,
            ..
        } = self;
        let head = playback.head;
        engine.scrub(file, head);
        self.bump();
    }

    pub fn cycle_loop_mode(&mut self) {
        self.playback.loop_mode = match self.playback.loop_mode {
            LoopMode::Once => LoopMode::Loop,
            LoopMode::Loop => LoopMode::PingPong,
            LoopMode::PingPong => LoopMode::Once,
        };
        let pb = self.playback;
        self.engine.set_timeline_playback(pb);
        self.repaint();
    }

    pub fn step_to_keyframe(&mut self, direction: i64) {
        let frame = renamite_animation::Frame(self.playback.head.round() as i64);
        let doc = &self.file.document;
        let rows = crate::session::timeline_rows(self);

        let selected = &self.selection.nodes;

        let mut ordered: Vec<&TimelineRow> = rows
            .iter()
            .filter(|row| selected.contains(&row.node))
            .collect();

        ordered.extend(rows.iter().filter(|row| !selected.contains(&row.node)));

        let mut candidate: Option<i64> = None;

        for row in ordered {
            for f in doc.key_frames(row.node, &row.prop).iter().map(|f| f.0) {
                let better = if direction < 0 {
                    f < frame.0 && candidate.map(|c| f > c).unwrap_or(true)
                } else {
                    f > frame.0 && candidate.map(|c| f < c).unwrap_or(true)
                };

                if better {
                    candidate = Some(f);
                }
            }
        }

        let target = candidate.unwrap_or_else(|| {
            let range = doc.compositions[doc.main].range;
            if direction < 0 { range.0.0 } else { range.1.0 }
        });
        self.playback.head = target as f64;
        let crate::session::Session {
            file,
            engine,
            playback,
            ..
        } = self;
        let head = playback.head;
        engine.scrub(file, head);
        self.bump();
    }

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

    pub fn commit_rename(&mut self) {
        let Some((id, draft)) = self.renaming.take() else {
            return;
        };
        let name = draft.trim().to_string();
        let current = self
            .file
            .document
            .nodes
            .get(id)
            .map(|n| n.name.clone())
            .unwrap_or_default();
        self.renaming = None;
        if name.is_empty() || name == current {
            self.repaint();
            return;
        }
        self.apply_outputs(smallvec![
            ToolOutput::BeginTransaction("Rename".into()),
            ToolOutput::Commands(smallvec![renamite_behavior_common::layers::cmd_rename(
                id, name
            )]),
            ToolOutput::CommitTransaction,
        ]);
    }

    pub fn cancel_rename(&mut self) {
        if self.renaming.is_none() {
            return;
        }
        self.renaming = None;
        self.repaint();
    }

    pub fn history_apply(
        &mut self,
        cmd: renamite_history::EditorCommand,
    ) -> Option<renamite_model::NodeId> {
        self.history_apply_full(cmd)?.created
    }

    pub fn history_apply_full(
        &mut self,
        command: renamite_history::EditorCommand,
    ) -> Option<renamite_history::Applied> {
        let history = &mut self.history;
        let file = &mut self.file;
        let mut project = pm_from(file);

        match history.apply(&mut project, command) {
            Ok(applied) => {
                self.dirty = true;
                Some(applied)
            }
            Err(error) => {
                self.status = Some(format!("Edit failed: {error}"));
                None
            }
        }
    }

    pub fn sync_image_assets(&mut self) {
        self.renderer
            .sync_document_images(&self.file.document, &self.render_context);
    }

    pub fn import_font(&mut self, name: String, bytes: Vec<u8>) {
        use renamite_model::{Asset, FontAsset};
        let family = renamite_text::font_family_name(&bytes).unwrap_or_else(|| {
            Path::new(&name)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "ImportedFont".into())
        });
        let asset = Asset::Font(FontAsset {
            name: name.clone(),
            family: family.clone(),
            bytes,
        });
        if self
            .history_apply(renamite_history::EditorCommand::AddAsset {
                index: usize::MAX,
                asset,
                id: None,
            })
            .is_none()
        {
            self.status = Some("Font import failed".into());
            self.bump();
            return;
        }
        self.status = Some(format!("Imported font: {family}"));
        self.bump();
    }

    pub fn import_image(&mut self, asset: renamite_model::ImageAsset) {
        use renamite_model::Asset;

        let name = asset.name.clone();
        let applied = self.history_apply_full(renamite_history::EditorCommand::AddAsset {
            index: usize::MAX,
            asset: Asset::Image(asset),
            id: None,
        });

        if applied.is_none() {
            self.status = Some("Image import failed".into());
            self.bump();
            return;
        }

        self.status = Some(format!("Imported image: {name}"));
        self.sync_image_assets();
        self.bump();
    }

    pub fn save_snapshot(&self) -> anyhow::Result<Vec<u8>> {
        let mut file = self.file.clone();
        file.normalize();
        file.garbage_collect();
        Ok(renamite_io_ren::save(&file)?.into_bytes())
    }

    pub fn pack_snapshot(&self) -> anyhow::Result<Vec<u8>> {
        let mut file = self.file.clone();
        file.normalize();
        file.garbage_collect();
        Ok(renamite_io_ren::save_binary(&file)?)
    }

    pub fn replace_file(&mut self, file: RenFile) {
        self.file = file;
        self.welcome = false;
        self.history = History::new();
        self.engine = Engine::new(&self.file).unwrap_or_else(|e| {
            log::warn!("replace_file: Engine::new failed: {e}; using fallback");
            Engine::new(&RenFile::new(
                renamite_model::Document::empty(),
                self.file.meta.name.clone(),
            ))
            .expect("empty document must produce a valid engine")
        });
        self.selection.nodes.clear();
        self.open_picker = None;
        self.context_menu = None;
        self.clipboard = None;
        self.keys = Default::default();
        self.scrub = Default::default();
        self.record = false;
        self.expanded_layers.clear();
        self.layer_drag = None;
        self.renaming = None;
        self.inspector_drag = None;
        self.active_machine = self
            .file
            .start_machine
            .or_else(|| self.file.machine_order.first().copied());
        self.machine_selection = MachineSelection::None;
        self.machine_preview_inputs = Vec::new();
        self.machine_preview_enabled = false;
        self.machine_drag = None;
        self.machine_graph_gesture = None;
        self.listener_draft = ListenerDraft::default();
        self.viewport.request_fit();
        self.viewport.pan_last = None;
        self.viewport.last_pointer = DVec2::ZERO;
        self.dirty = false;
        self.exporting_png = false;
        self.sync_image_assets();
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    pub fn mark_saved(&mut self, path: Option<PathBuf>) {
        self.current_path = path;
        self.dirty = false;
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    pub fn drain_file_ops(&mut self) -> bool {
        let ops = mem::take(&mut *self.file_ops.lock_sync());
        let mut run_intent = false;
        for op in ops {
            match op {
                PendingFileOp::OpenDone {
                    file,
                    path,
                    message,
                    warnings,
                } => {
                    self.replace_file(*file);
                    self.welcome = false;
                    self.current_path = path;
                    if warnings.is_empty() {
                        self.status = Some(message);
                    } else {
                        let shown: Vec<&str> =
                            warnings.iter().take(3).map(|s| s.as_str()).collect();
                        self.status = Some(format!(
                            "{} ({} warning{}: {})",
                            message,
                            warnings.len(),
                            if warnings.len() == 1 { "" } else { "s" },
                            shown.join("; ")
                        ));
                    }
                }
                PendingFileOp::SaveOutcome { ok: true, path } => {
                    self.mark_saved(path);
                    self.status = Some("Saved".to_string());
                    if self.pending_intent.is_some() {
                        run_intent = true;
                    }
                }
                PendingFileOp::SaveOutcome { ok: false, .. } => {
                    self.clear_pending_intent();
                    self.status = Some("Save canceled".to_string());
                    self.bump();
                }
                PendingFileOp::Exported => {
                    self.status = Some("Exported".to_string());
                    self.revision = self.revision.wrapping_add(1);
                    request_frame();
                }
                PendingFileOp::ExportFinished { message } => {
                    self.exporting_png = false;
                    self.status = Some(message);
                    self.revision = self.revision.wrapping_add(1);
                    request_frame();
                }
                PendingFileOp::ExportPngReady {
                    bytes,
                    suggested_name,
                } => {
                    self.exporting_png = false;
                    let ops = self.file_ops.clone();
                    renamite_platform::dialogs::save_bytes(
                        "Export PNG",
                        suggested_name,
                        &["png"],
                        bytes,
                        Box::new(move |outcome| {
                            if outcome.ok {
                                ops.lock_sync().push_back(PendingFileOp::ExportFinished {
                                    message: "Exported PNG".to_string(),
                                });
                            } else {
                                ops.lock_sync().push_back(PendingFileOp::ExportFinished {
                                    message: "PNG export canceled".to_string(),
                                });
                            }
                            #[cfg(target_arch = "wasm32")]
                            repose_core::request_frame();
                            #[cfg(not(target_arch = "wasm32"))]
                            repose_platform::wake_event_loop();
                        }),
                    );
                    self.revision = self.revision.wrapping_add(1);
                    request_frame();
                }
                PendingFileOp::ImportFontDone { name, bytes } => {
                    self.import_font(name, bytes);
                }
                PendingFileOp::ImportImageDone { asset } => {
                    self.import_image(asset);
                }
                PendingFileOp::Failed { message } => {
                    self.clear_pending_intent();
                    self.exporting_png = false;
                    self.status = Some(format!("Error: {message}"));
                    self.revision = self.revision.wrapping_add(1);
                    request_frame();
                }
            }
        }
        run_intent
    }

    pub fn request_discard(&mut self, intent: PendingIntent) {
        self.pending_intent = Some(intent);
        self.confirm_dialog.show();
        self.repaint();
    }

    pub fn take_pending_intent(&mut self) -> Option<PendingIntent> {
        self.pending_intent.take()
    }

    pub fn clear_pending_intent(&mut self) {
        if self.pending_intent.take().is_some() {
            self.repaint();
        }
    }

    pub fn tick_playback(&mut self) -> bool {
        if !self.playing && !self.machine_preview_enabled {
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

impl Session {
    fn cancel_open_picker_state(&mut self) {
        let Some(open) = self.open_picker.take() else {
            return;
        };

        if open.transaction_open {
            let history = &mut self.history;
            let file = &mut self.file;
            let mut project = pm_from(file);
            let _ = history.cancel(&mut project);
            self.engine.reevaluate(&self.file);
        }

        if let Some(paint) = open.cancel_current_paint {
            self.current_paint = paint;
        }
    }

    pub fn open_color_picker(
        &mut self,
        target: PickerTarget,
        initial: renamite_model::Color,
        anchor: DVec2,
    ) {
        self.close_context_menu();
        self.cancel_open_picker_state();

        let cancel_current_paint =
            (target == PickerTarget::CurrentPaint).then(|| self.current_paint.clone());

        self.open_picker = Some(OpenPicker {
            target,
            state: Rc::new(RefCell::new(crate::color_picker::PickerState::from_color(
                initial,
            ))),
            anchor,
            transaction_open: false,
            cancel_current_paint,
        });

        self.repaint();
    }

    pub fn close_color_picker(&mut self) {
        self.cancel_open_picker_state();
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    pub fn apply_picker_change(&mut self, color: renamite_model::Color) {
        let Some(target) = self.open_picker.as_ref().map(|open| open.target.clone()) else {
            return;
        };

        match target {
            PickerTarget::CurrentPaint => {
                self.current_paint.set_base_color(color);
            }

            PickerTarget::StyleColor { style_id } => {
                if !self.ensure_picker_transaction() {
                    return;
                }
                self.write_style_color(style_id, color);
                self.engine.reevaluate(&self.file);
            }

            PickerTarget::GradientStop { style_id, index } => {
                if !self.ensure_picker_transaction() {
                    return;
                }
                self.write_gradient_stop_color(style_id, index, color);
                self.engine.reevaluate(&self.file);
            }

            PickerTarget::Prop { id, path } => {
                if !self.ensure_picker_transaction() {
                    return;
                }
                self.write_prop_color(id, path, color);
                self.engine.reevaluate(&self.file);
            }
        }

        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    fn ensure_picker_transaction(&mut self) -> bool {
        let picker_owns_transaction = self
            .open_picker
            .as_ref()
            .is_some_and(|open| open.transaction_open);

        if picker_owns_transaction {
            return true;
        }

        if self.history.transaction_open() {
            self.status = Some("Finish the active edit before changing a color".into());
            return false;
        }

        self.history.begin("Edit color");

        if let Some(open) = self.open_picker.as_mut() {
            open.transaction_open = true;
        }

        true
    }

    fn write_style_color(
        &mut self,
        style_id: renamite_model::NodeId,
        color: renamite_model::Color,
    ) {
        use renamite_model::{NodeKind, PropPath, StyleKind, Value};
        let path = match self.file.document.nodes.get(style_id).map(|n| &n.kind) {
            Some(NodeKind::Style(StyleKind::Stroke { .. })) => PropPath::new("stroke.color"),
            _ => PropPath::new("fill.color"),
        };
        let frame = renamite_animation::Frame(self.playback.head.round() as i64);
        let cmd = renamite_history::resolve_property_edit(
            &self.file.document,
            style_id,
            &path,
            Value::Color(color),
            frame,
            self.record_for_writes(),
        );
        self.history_apply(cmd);
    }

    fn write_gradient_stop_color(
        &mut self,
        style_id: renamite_model::NodeId,
        index: usize,
        color: renamite_model::Color,
    ) {
        use renamite_model::{NodeKind, PropPath, StylePaint, Value};
        let Some(node) = self.file.document.nodes.get(style_id) else {
            return;
        };
        let NodeKind::Style(st) = &node.kind else {
            return;
        };
        let StylePaint::Gradient(g) = st.paint() else {
            return;
        };
        let mut stops = g.stops.value_at(self.playback.head);
        if let Some(stop) = stops.0.get_mut(index) {
            stop.color = color;
        }
        let frame = renamite_animation::Frame(self.playback.head.round() as i64);
        let cmd = renamite_history::resolve_property_edit(
            &self.file.document,
            style_id,
            &PropPath::new("grad.stops"),
            Value::Stops(stops),
            frame,
            self.record_for_writes(),
        );
        self.history_apply(cmd);
    }

    fn write_prop_color(
        &mut self,
        id: renamite_model::NodeId,
        path: renamite_model::PropPath,
        color: renamite_model::Color,
    ) {
        use renamite_model::Value;
        let frame = renamite_animation::Frame(self.playback.head.round() as i64);
        let cmd = renamite_history::resolve_property_edit(
            &self.file.document,
            id,
            &path,
            Value::Color(color),
            frame,
            self.record_for_writes(),
        );
        self.history_apply(cmd);
    }

    pub fn commit_picker_color(&mut self, color: renamite_model::Color) {
        let Some(target) = self.open_picker.as_ref().map(|open| open.target.clone()) else {
            return;
        };

        match target {
            PickerTarget::CurrentPaint => {
                let committed = self.current_paint.clone();
                if let Some(open) = self.open_picker.as_mut() {
                    open.cancel_current_paint = Some(committed);
                }
            }
            PickerTarget::StyleColor { .. }
            | PickerTarget::GradientStop { .. }
            | PickerTarget::Prop { .. } => {
                let owns_transaction = self
                    .open_picker
                    .as_ref()
                    .is_some_and(|open| open.transaction_open);

                if owns_transaction {
                    self.history.commit();
                    if let Some(open) = self.open_picker.as_mut() {
                        open.transaction_open = false;
                    }
                }
            }
        }

        self.swatches.push(color);
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    pub fn add_swatch(&mut self, color: renamite_model::Color) {
        self.swatches.push(color);
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    pub fn finish_color_picker(&mut self) {
        if let Some(open) = self.open_picker.clone()
            && open.transaction_open
        {
            let color = open.state.borrow().color();
            self.commit_picker_color(color);
        }

        self.open_picker = None;
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }
}

impl Session {
    pub fn edit_active_machine(
        &mut self,
        label: impl Into<String>,
        edit: impl FnOnce(&mut Machine) -> renamite_behavior_common::machine::Result<()>,
    ) -> bool {
        let Some(machine_id) = self.active_machine else {
            return false;
        };

        let Some(mut machine) = self.file.machines.get(machine_id).cloned() else {
            return false;
        };

        if let Err(error) = edit(&mut machine) {
            self.status = Some(format!("Machine edit failed: {error}"));
            self.revision = self.revision.wrapping_add(1);
            request_frame();
            return false;
        }

        self.apply_outputs(smallvec![
            ToolOutput::BeginTransaction(label.into()),
            ToolOutput::Commands(smallvec![EditorCommand::ReplaceMachine {
                id: machine_id,
                machine,
            }]),
            ToolOutput::CommitTransaction,
        ]);

        if self.machine_preview_enabled {
            self.resync_machine_preview_after_edit();
        }

        true
    }

    /// Keep live input values + running instance where possible after a structural edit.
    pub fn resync_machine_preview_after_edit(&mut self) {
        let Some(machine_id) = self.active_machine else {
            self.machine_preview_inputs.clear();
            return;
        };
        let Some(machine) = self.file.machines.get(machine_id).cloned() else {
            self.machine_preview_inputs.clear();
            return;
        };

        let prev = mem::take(&mut self.machine_preview_inputs);
        self.machine_preview_inputs = machine
            .inputs
            .iter()
            .enumerate()
            .map(|(i, def)| {
                let default = match def.kind {
                    InputKind::Bool { default } => InputValue::Bool(default),
                    InputKind::Number { default } => InputValue::Number(default),
                    InputKind::Trigger => InputValue::Trigger { fired: false },
                };
                match (def.kind, prev.get(i)) {
                    (InputKind::Bool { .. }, Some(InputValue::Bool(b))) => InputValue::Bool(*b),
                    (InputKind::Number { .. }, Some(InputValue::Number(n))) => {
                        InputValue::Number(*n)
                    }
                    (InputKind::Trigger, Some(InputValue::Trigger { .. })) => {
                        InputValue::Trigger { fired: false }
                    }
                    _ => default,
                }
            })
            .collect();

        let already = self.engine.playing_machine_id() == Some(machine_id);
        if !already {
            self.engine.play_machine(&self.file, machine_id);
        }
        for (i, v) in self.machine_preview_inputs.iter().enumerate() {
            match *v {
                InputValue::Bool(b) => {
                    let _ = self.engine.set_bool(&self.file, &machine.inputs[i].name, b);
                }
                InputValue::Number(n) => {
                    let _ = self
                        .engine
                        .set_number(&self.file, &machine.inputs[i].name, n);
                }
                InputValue::Trigger { .. } => {}
            }
        }
        self.engine.reevaluate(&self.file);
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    pub fn select_machine(&mut self, machine: MachineId) {
        if !self.file.machines.contains_key(machine) || !self.file.machine_order.contains(&machine)
        {
            return;
        }

        self.active_machine = Some(machine);
        self.machine_selection = MachineSelection::None;
        self.active_machine_layer = 0;
        self.machine_graph_gesture = None;

        self.reset_machine_preview();
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    pub fn create_machine(&mut self) {
        let machine = Machine {
            name: format!("State Machine {}", self.file.machine_order.len() + 1,),
            inputs: Vec::new(),
            layers: vec![renamite_machine::MachineLayer {
                name: "Layer 1".into(),
                entry: 0,
                any_transitions: Vec::new(),
                states: vec![State {
                    name: "Idle".into(),
                    kind: StateKind::Empty,
                    transitions: Vec::new(),
                    graph_pos: None,
                }],
            }],
            listeners: Vec::new(),
        };

        self.history.begin("Create machine".to_string());
        let applied = self.history_apply_full(EditorCommand::CreateMachine {
            index: usize::MAX,
            machine,
            id: None,
        });
        self.history.commit();

        if let Some(machine) = applied.and_then(|value| value.created_machine) {
            self.active_machine = Some(machine);
            self.machine_selection = MachineSelection::None;
            self.active_machine_layer = 0;
            if self.file.start_machine.is_none() {
                let _ = self.history_apply(EditorCommand::SetStartMachine {
                    start: Some(machine),
                });
            }
            if self.mode == EditorMode::Interact {
                self.machine_preview_enabled = true;
            }
            self.reset_machine_preview();
        }

        self.bump();
    }

    pub fn remove_active_machine(&mut self) {
        let Some(machine) = self.active_machine else {
            return;
        };
        if !self.file.machine_order.contains(&machine) {
            return;
        }

        self.apply_outputs(smallvec![
            ToolOutput::BeginTransaction("Remove machine".into()),
            ToolOutput::Commands(smallvec![EditorCommand::DetachMachine { id: machine }]),
            ToolOutput::CommitTransaction,
        ]);

        self.active_machine = self
            .file
            .start_machine
            .or_else(|| self.file.machine_order.first().copied());
        if self.file.start_machine.is_none()
            && let Some(id) = self.file.machine_order.first().copied()
        {
            self.apply_outputs(smallvec![
                ToolOutput::BeginTransaction("Set start machine".into()),
                ToolOutput::Commands(smallvec![EditorCommand::SetStartMachine {
                    start: Some(id)
                }]),
                ToolOutput::CommitTransaction,
            ]);
            self.active_machine = Some(id);
        }
        self.machine_selection = MachineSelection::None;
        self.active_machine_layer = 0;
        self.reset_machine_preview();
    }

    pub fn reset_machine_preview(&mut self) {
        let Some(machine_id) = self.active_machine else {
            self.machine_preview_inputs.clear();
            return;
        };

        let Some(machine) = self.file.machines.get(machine_id) else {
            self.machine_preview_inputs.clear();
            return;
        };

        // Rebuild defaults, then restore previous values where kinds still match.
        let mut next: Vec<InputValue> = machine
            .inputs
            .iter()
            .map(|input| match input.kind {
                InputKind::Bool { default } => InputValue::Bool(default),
                InputKind::Number { default } => InputValue::Number(default),
                InputKind::Trigger => InputValue::Trigger { fired: false },
            })
            .collect();

        for (i, dst) in next.iter_mut().enumerate() {
            if let Some(prev) = self.machine_preview_inputs.get(i) {
                match (dst, prev) {
                    (InputValue::Bool(a), InputValue::Bool(b)) => *a = *b,
                    (InputValue::Number(a), InputValue::Number(b)) => *a = *b,
                    // triggers: don't keep "fired" across rebuilds
                    (InputValue::Trigger { .. }, InputValue::Trigger { .. }) => {}
                    _ => {}
                }
            }
        }
        self.machine_preview_inputs = next;

        if self.machine_preview_enabled {
            self.engine.play_machine(&self.file, machine_id);
            self.engine
                .apply_machine_inputs(&self.machine_preview_inputs);
        } else {
            self.engine.play_timeline(&self.file, LoopMode::Loop);
        }

        self.engine.reevaluate(&self.file);
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    fn sync_preview_inputs_from_engine(&mut self) {
        if let Some(inputs) = self.engine.machine_inputs()
            && self.machine_preview_inputs.len() == inputs.len()
        {
            self.machine_preview_inputs = inputs.to_vec();
        }
    }

    fn ensure_machine_preview_running(&mut self) {
        if !self.machine_preview_enabled {
            self.machine_preview_enabled = true;
            self.reset_machine_preview();
            return;
        }
        if self.engine.playing_machine_id() != self.active_machine
            && let Some(id) = self.active_machine
        {
            self.engine.play_machine(&self.file, id);
            self.engine
                .apply_machine_inputs(&self.machine_preview_inputs);
            self.engine.reevaluate(&self.file);
        }
    }

    pub fn set_preview_bool(&mut self, input: usize, value: bool) {
        self.ensure_machine_preview_running();
        let Some(machine) = self.active_machine else {
            return;
        };

        if let Some(InputValue::Bool(current)) = self.machine_preview_inputs.get_mut(input) {
            *current = value;
            self.engine.set_bool(
                &self.file,
                &self.file.machines[machine].inputs[input].name,
                value,
            );
            request_frame();
        }
    }

    pub fn set_preview_number(&mut self, input: usize, value: f64) {
        self.ensure_machine_preview_running();
        let Some(machine) = self.active_machine else {
            return;
        };

        if let Some(InputValue::Number(current)) = self.machine_preview_inputs.get_mut(input) {
            *current = value;
            self.engine.set_number(
                &self.file,
                &self.file.machines[machine].inputs[input].name,
                value,
            );
            request_frame();
        }
    }

    pub fn fire_preview_trigger(&mut self, input: usize) {
        self.ensure_machine_preview_running();
        let Some(machine) = self.active_machine else {
            return;
        };

        let Some(input_def) = self.file.machines[machine].inputs.get(input) else {
            return;
        };

        self.engine.fire(&self.file, &input_def.name);

        request_frame();
    }

    pub fn engine_pointer_down(&mut self, world: DVec2) {
        self.engine.pointer_down(&self.file, world);
        let _ = self.engine.tick(&self.file, 0.0);
        self.sync_preview_inputs_from_engine();
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    pub fn engine_pointer_move(&mut self, world: DVec2) {
        self.engine.pointer_move(&self.file, world);
        let _ = self.engine.tick(&self.file, 0.0);
        self.sync_preview_inputs_from_engine();
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    pub fn engine_pointer_up(&mut self, world: DVec2) {
        self.engine.pointer_up(&self.file, world);
        let _ = self.engine.tick(&self.file, 0.0);
        self.sync_preview_inputs_from_engine();
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    pub fn engine_pointer_leave(&mut self) {
        self.engine.pointer_leave(&self.file);
        let _ = self.engine.tick(&self.file, 0.0);
        self.sync_preview_inputs_from_engine();
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    pub fn rename_active_machine(&mut self, name: impl Into<String>) {
        let name = name.into();
        let name = name.trim().to_owned();
        if name.is_empty() {
            return;
        }
        self.edit_active_machine("Rename machine", move |machine| {
            machine.name = name;
            Ok(())
        });
    }

    pub fn delete_machine_selection(&mut self) {
        let selection = self.machine_selection.clone();
        match selection {
            MachineSelection::State { layer, state } => {
                let ok = self.edit_active_machine("Delete state", move |machine| {
                    remove_state(machine, layer, state)?;
                    Ok(())
                });
                if ok {
                    self.machine_selection = MachineSelection::None;
                }
            }
            MachineSelection::Transition {
                layer,
                source,
                transition,
            } => {
                let ok = self.edit_active_machine("Remove transition", move |machine| {
                    remove_transition(machine, layer, source, transition)?;
                    Ok(())
                });
                if ok {
                    self.machine_selection = MachineSelection::None;
                }
            }
            _ => {}
        }
    }

    pub fn set_active_as_start_machine(&mut self) {
        let Some(id) = self.active_machine else {
            return;
        };
        if self.file.start_machine == Some(id) {
            return;
        }
        self.apply_outputs(smallvec![
            ToolOutput::BeginTransaction("Set start machine".into()),
            ToolOutput::Commands(smallvec![EditorCommand::SetStartMachine {
                start: Some(id)
            }]),
            ToolOutput::CommitTransaction,
        ]);
    }

    pub fn begin_machine_field_scrub(&mut self, origin: f64, press_x: f32) {
        self.machine_drag = Some(MachineDrag::MachineField {
            origin,
            press_x,
            txn: false,
        });
    }

    pub fn scrub_machine_field(
        &mut self,
        press_x_now: f32,
        step: f64,
        shift: bool,
        edit: impl FnOnce(&mut Machine, f64),
    ) {
        let Some(MachineDrag::MachineField {
            origin,
            press_x,
            txn,
        }) = self.machine_drag
        else {
            return;
        };
        let dx = (press_x_now - press_x) as f64;
        let mult = if shift { 0.1 } else { 1.0 };
        let new_value = origin + dx * step * mult;

        let Some(machine_id) = self.active_machine else {
            return;
        };
        let Some(mut machine) = self.file.machines.get(machine_id).cloned() else {
            return;
        };
        edit(&mut machine, new_value);

        if !txn {
            self.history.begin("Edit machine".to_owned());
            if let Some(MachineDrag::MachineField { txn, .. }) = self.machine_drag.as_mut() {
                *txn = true;
            }
        }
        let _ = self.history_apply(EditorCommand::ReplaceMachine {
            id: machine_id,
            machine,
        });
        if self.machine_preview_enabled {
            self.engine.reevaluate(&self.file);
        }
        self.dirty = true;
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    pub fn end_machine_field_scrub(&mut self) {
        if let Some(MachineDrag::MachineField { txn: true, .. }) = self.machine_drag.take() {
            self.history.commit();
        } else {
            self.machine_drag = None;
        }
        request_frame();
    }

    pub fn begin_preview_number_scrub(&mut self, input: usize, origin: f64, press_x: f32) {
        self.machine_drag = Some(MachineDrag::PreviewNumber {
            input,
            origin,
            press_x,
        });
    }

    pub fn scrub_preview_number(&mut self, press_x_now: f32, step: f64, shift: bool) {
        let Some(MachineDrag::PreviewNumber {
            input,
            origin,
            press_x,
        }) = self.machine_drag
        else {
            return;
        };
        let dx = (press_x_now - press_x) as f64;
        let mult = if shift { 0.1 } else { 1.0 };
        self.set_preview_number(input, origin + dx * step * mult);
    }

    pub fn end_preview_number_scrub(&mut self) {
        if matches!(self.machine_drag, Some(MachineDrag::PreviewNumber { .. })) {
            self.machine_drag = None;
        }
    }

    pub fn create_clip(&mut self, name: impl Into<String>) -> Option<renamite_machine::ClipId> {
        use renamite_animation::Frame;
        use renamite_machine::Clip;
        let name = name.into();
        let clip = Clip {
            name,
            range: (Frame(0), Frame(60)),
            tracks: Vec::new(),
            events: Vec::new(),
        };
        self.history.begin("Create clip".to_owned());
        let _ = self.history_apply_full(EditorCommand::CreateClip {
            index: usize::MAX,
            clip,
            id: None,
        });
        self.history.commit();
        self.dirty = true;
        let created = self.file.clip_order.last().copied();
        self.bump();
        created
    }

    pub fn node_name(&self, id: renamite_model::NodeId) -> String {
        self.file
            .document
            .nodes
            .get(id)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| format!("{id:?}"))
    }
}

#[derive(Clone, Debug)]
pub struct ViewportState {
    pub view: ViewTransform,
    fit: FitState,
    pub pan_last: Option<DVec2>,
    pub space_held: bool,
    pub pointer_down: bool,
    /// Latched at pointer-down while Interact/machine-preview is on:
    /// `Some(true)` = route to engine, `Some(false)` = route to canvas tools.
    /// Prevents mid-drag Alt toggling from tearing the gesture.
    pub pointer_route: Option<bool>,
    pub last_pointer: DVec2,
    /// Whether `last_pointer` has been set by a real pointer event yet.
    /// Guards wheel-zoom anchoring before the first pointer move.
    pub has_pointer: bool,
    pub screen_rect: Option<Rect>,
    /// Machine graph canvas rect, in main-viewport-local coordinates.
    /// Lets two-finger gestures route to the graph instead of the canvas.
    pub graph_rect: Option<Rect>,

    pub show_grid: bool,
    pub snapping_enabled: bool,
    pub show_guides: bool,
    pub grid_spacing: DVec2,
    pub guides: Vec<Guide>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Guide {
    pub axis: GuideAxis,
    pub position: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuideAxis {
    Horizontal,
    Vertical,
}

impl Default for ViewportState {
    fn default() -> Self {
        Self {
            view: ViewTransform::identity(),
            fit: FitState::new(),
            pan_last: None,
            space_held: false,
            pointer_down: false,
            pointer_route: None,
            last_pointer: DVec2::ZERO,
            has_pointer: false,
            screen_rect: None,
            graph_rect: None,
            show_grid: false,
            snapping_enabled: true,
            show_guides: true,
            grid_spacing: DVec2::splat(10.0),
            guides: Vec::new(),
        }
    }
}

impl ViewportState {
    /// Last fitted surface size (zero before first layout).
    pub fn surface_size(&self) -> DVec2 {
        self.fit.surface_size()
    }

    /// Request a margin refit on the next [`ViewportState::ensure_fit`].
    pub fn request_fit(&mut self) {
        self.fit.request();
    }

    pub fn ensure_fit(&mut self, surface: DVec2, artboard: DVec2) {
        if self.pan_last.is_some() {
            return;
        }

        // Editor policy: keep the user's zoom on window resize; refit on
        // first layout, artboard change, or explicit `request_fit`.
        self.fit.ensure(&mut self.view, surface, artboard, false);
    }

    pub fn fit(&mut self, artboard: DVec2) {
        let surface = self.fit.surface_size();
        self.fit.request();
        if surface.x > 1.0 && surface.y > 1.0 {
            self.fit.ensure(&mut self.view, surface, artboard, false);
        }
    }

    pub fn zoom_centered(&mut self, factor: f64) {
        self.fit.zoom_centered(&mut self.view, factor);
    }

    pub fn zoom_at(&mut self, screen_pos: DVec2, factor: f64) {
        self.fit.zoom_at(&mut self.view, screen_pos, factor);
    }

    pub fn begin_pan(&mut self, position: DVec2) {
        self.pan_last = Some(position);
    }

    pub fn update_pan(&mut self, position: DVec2) -> bool {
        let Some(previous) = self.pan_last else {
            return false;
        };

        self.pan_last = Some(position);
        self.view.pan_by(position - previous);
        true
    }

    pub fn end_pan(&mut self) {
        self.pan_last = None;
    }

    /// Record the graph canvas rect (main-viewport-local) for gesture routing.
    pub fn set_graph_rect(&mut self, rect: Rect) {
        self.graph_rect = Some(rect);
    }

    /// True when a viewport-local point falls inside the graph canvas.
    pub fn graph_rect_at(&self, local: DVec2) -> bool {
        self.graph_rect.is_some_and(|r| {
            let p = repose_core::Vec2 {
                x: local.x as f32,
                y: local.y as f32,
            };
            r.contains(p)
        })
    }
}

pub fn timeline_ctx<'a>(
    doc: &'a renamite_model::Document,
    clips: &'a renamite_machine::ClipMap,
    rows: &'a [TimelineRow],
    range: (Frame, Frame),
    playhead: f64,
    px_per_frame: f64,
) -> TimelineCtx<'a> {
    timeline_ctx_with_offset(doc, clips, rows, range, playhead, px_per_frame, 0.0)
}

pub fn timeline_ctx_with_offset<'a>(
    doc: &'a renamite_model::Document,
    clips: &'a renamite_machine::ClipMap,
    rows: &'a [TimelineRow],
    range: (Frame, Frame),
    playhead: f64,
    px_per_frame: f64,
    offset_x: f64,
) -> TimelineCtx<'a> {
    TimelineCtx {
        doc,
        clips,
        target: TimelineTarget::Doc,
        rows,
        layout: TimelineLayout {
            origin_x: -offset_x.max(0.0),
            px_per_frame: px_per_frame.clamp(0.5, 48.0),
            row_top: 24.0,
            row_height: 22.0,
            key_tolerance_px: 6.0,
        },
        range,
        playhead,
    }
}

pub fn dispatch_timeline(s: &mut Session, ev: TimelineEvent) {
    if s.machine_preview_enabled {
        return;
    }
    let rows = timeline_rows(s);
    let (head, comp) = (s.playback.head, s.file.document.main);
    let range = s.file.document.compositions[comp].range;
    let zoom = s.timeline_zoom;
    let offset_x = s.timeline_offset_x;
    let ctx = timeline_ctx_with_offset(
        &s.file.document,
        &s.file.clips,
        &rows,
        range,
        head,
        zoom,
        offset_x,
    );

    s.keys.retain_valid(&ctx);

    let scrub = match &ev {
        TimelineEvent::Press { pos, .. }
        | TimelineEvent::Move { pos, .. }
        | TimelineEvent::Release { pos, .. }
        | TimelineEvent::DoubleClick { pos, .. } => {
            if s.scrub.is_dragging() {
                true
            } else if s.keys.is_active() {
                false
            } else {
                pos.y < ctx.layout.row_top
            }
        }
        _ => false,
    };
    let outs = if scrub {
        s.scrub.handle(&ctx, ev)
    } else {
        s.keys.handle(&ctx, ev)
    };
    s.apply_outputs(outs);
    s.revision = s.revision.wrapping_add(1);
    request_frame();
}

pub fn timeline_rows(s: &Session) -> Vec<TimelineRow> {
    let comp = &s.file.document.compositions[s.file.document.main];
    let mut out = Vec::new();
    for &id in &comp.children {
        append_timeline_rows_for_node(s, id, &mut out);
    }
    out
}

fn append_timeline_rows_for_node(
    s: &Session,
    id: renamite_model::NodeId,
    out: &mut Vec<TimelineRow>,
) {
    let doc = &s.file.document;

    for row in renamite_behavior_common::inspect::props_for_node(doc, id, Frame(0)) {
        if doc.property_is_animated(id, &row.desc.path) {
            out.push(TimelineRow {
                node: id,
                prop: row.desc.path,
            });
        }
    }

    if s.expanded_layers.contains(&id)
        && let Some(node) = doc.nodes.get(id)
    {
        for &child in &node.children {
            append_timeline_rows_for_node(s, child, out);
        }
    }
}

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

fn validate_machine_selection(s: &mut Session) {
    let Some(mid) = s.active_machine else {
        s.machine_selection = MachineSelection::None;
        s.active_machine_layer = 0;
        return;
    };
    let Some(m) = s.file.machines.get(mid) else {
        s.machine_selection = MachineSelection::None;
        s.active_machine_layer = 0;
        return;
    };
    // Clamp active layer
    if s.active_machine_layer >= m.layers.len() {
        s.active_machine_layer = m.layers.len().saturating_sub(1);
    }
    let valid = match &s.machine_selection {
        MachineSelection::None
        | MachineSelection::Input { .. }
        | MachineSelection::Listener { .. } => true,
        MachineSelection::Layer { layer } => *layer < m.layers.len(),
        MachineSelection::State { layer, state } => m
            .layers
            .get(*layer)
            .is_some_and(|l| *state < l.states.len()),
        MachineSelection::Transition {
            layer,
            source,
            transition,
        } => {
            let Some(l) = m.layers.get(*layer) else {
                return;
            };
            let len = match source {
                renamite_behavior_common::machine::TransitionSource::Any => l.any_transitions.len(),
                renamite_behavior_common::machine::TransitionSource::State(si) => l
                    .states
                    .get(*si)
                    .map(|st| st.transitions.len())
                    .unwrap_or(0),
            };
            *transition < len
        }
    };
    if !valid {
        s.machine_selection = MachineSelection::None;
    }
}

pub fn undo_cmd(s: &mut Session) {
    let his = &mut s.history;
    let file = &mut s.file;
    let mut pm = pm_from(file);
    if his.undo(&mut pm).is_ok() {
        s.dirty = true;
        s.selection
            .nodes
            .retain(|&id| node_is_attached(&s.file.document, id));
        validate_machine_selection(s);
        retain_valid_keys(s);
    }
    sync_playback_range(s);
}

pub fn redo_cmd(s: &mut Session) {
    let his = &mut s.history;
    let file = &mut s.file;
    let mut pm = pm_from(file);
    if his.redo(&mut pm).is_ok() {
        s.dirty = true;
        s.selection
            .nodes
            .retain(|&id| node_is_attached(&s.file.document, id));
        validate_machine_selection(s);
        retain_valid_keys(s);
    }
    sync_playback_range(s);
}

/// Drop key selections whose keyframes no longer exist (undo/redo can delete
/// keys or whole nodes). The behavior documents this as the host's job.
fn retain_valid_keys(s: &mut Session) {
    let rows = timeline_rows(s);
    let comp = s.file.document.main;
    let range = s.file.document.compositions[comp].range;
    let ctx = timeline_ctx_with_offset(
        &s.file.document,
        &s.file.clips,
        &rows,
        range,
        s.playback.head,
        s.timeline_zoom,
        s.timeline_offset_x,
    );
    s.keys.retain_valid(&ctx);
}

/// Abort any in-progress timeline gesture, committing nothing. Used for
/// pointer-cancel/leave so scrub/key-drag state can never stick.
pub fn cancel_timeline(s: &mut Session) {
    let key_outs = s.keys.cancel();
    s.scrub.cancel();
    if !key_outs.is_empty() {
        s.apply_outputs(key_outs);
    }
    s.repaint();
}

/// Attached = node exists and is reachable: has a parent node, or is a
/// direct child of ANY composition (attach() sets parent=None for
/// Parent::Comp, so checking only `main` dropped valid non-main selections).
fn node_is_attached(doc: &renamite_model::Document, id: renamite_model::NodeId) -> bool {
    let Some(n) = doc.nodes.get(id) else {
        return false;
    };
    if n.parent.is_some() {
        return true;
    }
    doc.compositions.values().any(|c| c.children.contains(&id))
}

fn sync_playback_range(s: &mut Session) {
    let range = s.file.document.compositions[s.file.document.main].range;
    s.playback.range = range;
    let head = s.playback.head.clamp(range.0.0 as f64, range.1.0 as f64);
    if head != s.playback.head {
        s.playback.head = head;
    }
    let pb = s.playback;
    s.engine.set_timeline_playback(pb);
    s.engine.reevaluate(&s.file);
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

pub fn overlay_anchor(pe: &PointerEvent) -> DVec2 {
    let p = pe.position_in_window();
    DVec2::new(
        repose_core::px_to_dp(repose_core::Px(p.x)).0 as f64,
        repose_core::px_to_dp(repose_core::Px(p.y)).0 as f64,
    )
}

pub fn dispatch_canvas(s: &mut Session, ev: CanvasEvent, m: Modifiers) {
    let outs = {
        let Session {
            file,
            engine,
            selection,
            playback,
            viewport,
            tool,
            active_tool,
            record,
            current_paint,
            mode,
            ..
        } = s;
        let snap_grid = if viewport.show_grid && viewport.snapping_enabled {
            Some(viewport.grid_spacing.x.max(1e-6))
        } else {
            None
        };
        let ctx = ToolContext {
            doc: &file.document,
            scene: engine.scene(),
            comp: file.document.main,
            selection,
            playhead: Frame(playback.head.round() as i64),
            record: *record && matches!(*mode, EditorMode::Animate),
            view: viewport.view,
            snap: SnapConfig {
                grid: snap_grid,
                anchor: viewport.snapping_enabled,
                guide: viewport.show_guides && viewport.snapping_enabled,
            },
            modifiers: m,
            current_paint,
        };
        tool.handle(*active_tool, &ctx, ev)
    };
    s.apply_outputs(outs);
}

pub fn pe_pos(pe: &PointerEvent) -> DVec2 {
    pe_to_dvec(pe)
}

fn nudge_tree(tree: &mut renamite_history::NodeTree, d: DVec2) {
    tree.node.transform.position.base += d;
}

fn style_scope_info(
    doc: &renamite_model::Document,
    style_id: renamite_model::NodeId,
) -> Option<(renamite_model::Parent, bool)> {
    use renamite_model::{NodeKind, Parent};

    let (parent, _) = doc.locate(style_id)?;
    let children: Vec<renamite_model::NodeId> = match parent {
        Parent::Comp(c) => doc.compositions.get(c)?.children.clone(),
        Parent::Node(p) => doc.nodes.get(p)?.children.clone(),
    };
    let painted = children
        .iter()
        .filter(|&&id| {
            matches!(
                doc.nodes.get(id).map(|n| &n.kind),
                Some(NodeKind::Shape(_)) | Some(NodeKind::Text(_))
            )
        })
        .count();
    Some((parent, painted > 1))
}

fn kurbo_cap(cap: renamite_model::StrokeCap) -> kurbo::Cap {
    match cap {
        renamite_model::StrokeCap::Butt => kurbo::Cap::Butt,
        renamite_model::StrokeCap::Round => kurbo::Cap::Round,
        renamite_model::StrokeCap::Square => kurbo::Cap::Square,
    }
}

fn shape_kind_from_contours(
    contours: Vec<renamite_geometry::VectorPath>,
) -> Option<renamite_model::ShapeKind> {
    match contours.len() {
        0 => None,
        1 => Some(renamite_model::ShapeKind::Path(
            renamite_animation::Animated::new(contours.into_iter().next().unwrap()),
        )),
        _ => Some(renamite_model::ShapeKind::CompoundPath(
            renamite_model::CompoundPath {
                contours: contours
                    .into_iter()
                    .map(renamite_animation::Animated::new)
                    .collect(),
            },
        )),
    }
}

fn kurbo_join(join: renamite_model::StrokeJoin) -> kurbo::Join {
    match join {
        renamite_model::StrokeJoin::Miter => kurbo::Join::Miter,
        renamite_model::StrokeJoin::Round => kurbo::Join::Round,
        renamite_model::StrokeJoin::Bevel => kurbo::Join::Bevel,
    }
}

fn nearest_style_node(
    doc: &renamite_model::Document,
    shape: renamite_model::NodeId,
    want_stroke: bool,
) -> Option<renamite_model::NodeId> {
    use renamite_model::{NodeKind, Parent, StyleKind};

    let mut scope = doc.locate(shape).map(|(p, _)| p)?;
    loop {
        let children: Vec<renamite_model::NodeId> = match scope {
            Parent::Comp(c) => doc.compositions.get(c)?.children.clone(),
            Parent::Node(p) => doc.nodes.get(p)?.children.clone(),
        };
        let found = children.iter().rev().copied().find(|id| {
            doc.nodes.get(*id).is_some_and(|n| {
                matches!(
                    &n.kind,
                    NodeKind::Style(s)
                    if if want_stroke {
                        matches!(s, StyleKind::Stroke { .. })
                    } else {
                        matches!(s, StyleKind::Fill { .. })
                    }
                )
            })
        });
        if let Some(found) = found {
            return Some(found);
        }
        match scope {
            Parent::Comp(_) => return None,
            Parent::Node(p) => scope = doc.locate(p).map(|(parent, _)| parent)?,
        }
    }
}

fn affine_vector(affine: kurbo::Affine, value: DVec2) -> DVec2 {
    let [a, b, c, d, _, _] = affine.as_coeffs();

    DVec2::new(a * value.x + c * value.y, b * value.x + d * value.y)
}

pub fn blank_file() -> RenFile {
    RenFile::new(renamite_model::Document::empty(), "Untitled")
}

pub fn seeded_demo_file() -> RenFile {
    use renamite_animation::Animated;
    use renamite_model::{
        Color, FillRule, Node, NodeKind, Parent, ShapeKind, StyleKind, StylePaint,
    };

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
            paint: StylePaint::solid(Color::rgba(0.96, 0.42, 0.18, 1.0)),
            rule: FillRule::NonZero,
        }),
    ));

    doc.attach(shape, Parent::Comp(comp), 0).unwrap();
    doc.attach(fill, Parent::Comp(comp), 1).unwrap();

    RenFile::new(doc, "Untitled")
}

pub fn init_session(render_context: &repose_core::RenderContext) -> Rc<RefCell<Session>> {
    let rc = render_context.clone();
    let session = remember_with_key("session", || {
        RefCell::new(Session::with_render_context(blank_file(), rc))
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
