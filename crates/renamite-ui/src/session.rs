use std::cell::RefCell;
use std::rc::Rc;

use glam::DVec2;
use kurbo::Point;
use renamite_animation::{Frame, LoopMode, PlayState, Playback};
use renamite_behavior_canvas::{CanvasEvent, PointerButton, ToolSet};
use renamite_behavior_common::machine::MachineSelection;
use renamite_behavior_common::{Modifiers, Selection, SnapConfig, ToolContext, ViewTransform};
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

/// Explicit editor mode (Jitter/Linearity-style Design vs Animate, plus the
/// Rive-style Interact state-machine mental model). Drives the top-bar switch
/// and the record/playhead semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorMode {
    Design,
    Animate,
    Interact,
}

pub type SessionRef = Rc<RefCell<Session>>;

/// Shared editor session (single-threaded UI).
pub struct Session {
    pub file: RenFile,
    /// Where the current document was last saved to (None = never / "Untitled").
    pub current_path: Option<std::path::PathBuf>,
    /// True when edits since the last save exist. Undo/redo to a saved state
    /// does NOT clear this automatically (no saved snapshot tracking yet).
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
    /// Live Repose render context used to upload image assets for the viewport.
    pub render_context: repose_core::RenderContext,
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
    /// Transient status message (last file action result / error).
    pub status: Option<String>,
    /// Results of async platform dialogs, drained on the UI thread each frame.
    /// Populated from worker threads, so it is `Send + Sync`.
    pub file_ops: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<PendingFileOp>>>,
    /// Deferred destructive action awaiting the unsaved-changes dialog.
    pub pending_intent: Option<PendingIntent>,
    /// Unsaved-changes confirmation dialog (in-app, so it works on every target).
    pub confirm_dialog: Rc<DialogState>,
    /// Current paint used by the Fill tool (and, later, newly created shapes).
    pub current_paint: renamite_model::StylePaint,
    /// Recent colors for the picker's swatch strip (most-recent first).
    pub swatches: renamite_behavior_common::color::SwatchHistory,
    /// The currently open color picker popover, if any.
    pub open_picker: Option<OpenPicker>,
    /// The currently open context menu popover, if any.
    pub context_menu: Option<ContextMenuState>,
    /// True while the async PNG export render is in flight (blocks duplicates).
    pub exporting_png: bool,
    /// True while the empty-document launcher should be shown. Only a fresh
    /// blank project keeps it; opening/importing, picking a template, or
    /// dismissing to a blank canvas clears it so an emptied composition doesn't
    /// trap the user back on the launcher.
    pub welcome: bool,
    /// Serialized selection for copy/paste/duplicate (Vec<NodeTree>).
    pub clipboard: Option<Vec<renamite_history::NodeTree>>,
    /// Machine edited by the Interactivity panel.
    pub active_machine: Option<MachineId>,
    /// Graph selection within the active machine (view state).
    pub machine_selection: MachineSelection,
    /// Preview values mirrored in the player engine.
    pub machine_preview_inputs: Vec<InputValue>,
    /// Live preview drives the engine in machine mode.
    pub machine_preview_enabled: bool,
    /// Active scrubbable-drag on a machine preview Number input.
    pub machine_preview_drag: Option<MachinePreviewDrag>,
    /// Active rubber-band gesture on the state-machine graph (view state).
    pub machine_graph_gesture: Option<MachineGraphGesture>,
    /// Draft of the listener being authored (view state).
    pub listener_draft: ListenerDraft,
    /// Timeline zoom: pixels per frame in the ruler/canvas (view state).
    pub timeline_zoom: f64,
}

/// A scrubbable-drag on a machine preview Number input (view state).
#[derive(Clone, Copy, Debug)]
pub struct MachinePreviewDrag {
    pub input: usize,
    pub origin: f64,
    pub press_x: f32,
}

/// Live gesture on the state-machine graph (view state).
#[derive(Clone, Debug)]
pub enum MachineGraphGesture {
    /// Rubber-band a new transition from `from_state` on `layer`.
    WireTransition {
        layer: usize,
        from_state: usize,
        current: DVec2,
    },
}

/// Draft fields for the "add listener" row in the Interactivity panel (view
/// state, not undoable until the listener is actually added).
#[derive(Clone, Debug, Default)]
pub struct ListenerDraft {
    pub event: Option<renamite_machine::PointerEventKind>,
    pub input: Option<usize>,
    pub toggle: bool,
    pub bool_value: bool,
    pub number_value: f64,
}

/// A destructive action deferred behind the unsaved-changes guard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingIntent {
    New,
    Open,
    ImportLottie,
    ImportSvg,
}

/// A file-lifecycle result produced off-thread by an async platform dialog
/// (`renamite_platform::dialogs`), applied to the session during the next
/// frame by [`Session::drain_file_ops`].
pub enum PendingFileOp {
    /// Install a freshly read project (Open or Import Lottie). `path` is
    /// `Some` for a real filesystem open (desktop), `None` for name+bytes
    /// (WASM/Android) or imported documents (always unsaved).
    OpenDone {
        file: Box<RenFile>,
        path: Option<std::path::PathBuf>,
        message: &'static str,
    },
    /// An async save completed. `path` is `Some` on desktop.
    SaveOutcome {
        ok: bool,
        path: Option<std::path::PathBuf>,
    },
    /// An exported frame reached its destination (WASM/Android, no path).
    Exported,
    /// An async PNG export finished; show the message in the status area.
    ExportFinished { message: String },
    /// Rendered PNG bytes ready to hand to the OS save picker (WASM/Android).
    ExportPngReady {
        bytes: Vec<u8>,
        suggested_name: String,
    },
    /// Raw font bytes (`.ttf`/`.otf`) read by the Import Font picker.
    ImportFontDone { name: String, bytes: Vec<u8> },
    /// A decoded image asset read by the Import Image picker.
    ImportImageDone { asset: renamite_model::ImageAsset },
    /// An async file op failed; surface the message.
    Failed { message: String },
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

/// A live color-picker transaction: which target it writes back to, plus the
/// picker's own view/editing state.
#[derive(Clone)]
pub struct OpenPicker {
    pub target: PickerTarget,
    pub state: Rc<RefCell<crate::color_picker::PickerState>>,

    /// Screen-space anchor (from the opening pointer event) for the popover.
    pub anchor: DVec2,

    /// Only picker-owned transactions may be committed/cancelled by the picker.
    pub transaction_open: bool,

    /// Cancellation baseline for non-document current-paint edits.
    pub cancel_current_paint: Option<renamite_model::StylePaint>,
}

/// What a live-editing picker session writes back to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerTarget {
    /// The global current-paint swatch (top bar / Fill tool).
    CurrentPaint,
    /// A specific fill/stroke style node's solid color.
    StyleColor { style_id: renamite_model::NodeId },
    /// One stop in a gradient (fill or stroke).
    GradientStop {
        style_id: renamite_model::NodeId,
        index: usize,
    },
}

/// An open context-menu popover: its screen anchor, entries, and source.
#[derive(Clone, Debug)]
pub struct ContextMenuState {
    /// Screen-space anchor for the popover (clamped by the overlay).
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

    /// Full constructor; the caller supplies the live `RenderContext` used to
    /// upload image assets for editor viewport rendering.
    pub fn with_render_context(file: RenFile, render_context: repose_core::RenderContext) -> Self {
        let engine = Engine::new(&file).expect("project");
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
            expanded_layers: std::collections::HashSet::new(),
            layer_drag: None,
            renaming: None,
            record: false,
            inspector_drag: None,
            status: None,
            file_ops: std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
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
            machine_preview_inputs: Vec::new(),
            machine_preview_enabled: false,
            machine_preview_drag: None,
            machine_graph_gesture: None,
            listener_draft: ListenerDraft::default(),
            timeline_zoom: 6.0,
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
                        // A transaction opened in a previous batch (defensive).
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
                            // Select what shape tools create.
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
                    self.playback.head = f;
                    self.engine.scrub(&self.file, f);
                    needs_repaint = true;
                }
                ToolOutput::SwitchTool(t) => {
                    self.active_tool = t;
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

        // Defensive: any document mutation that never went through a
        // Begin/Commit pair still marks the project dirty.
        if mutated_outside_transaction {
            self.dirty = true;
        }

        // Drop dangling timeline keyframe selection after undo/doc edits.
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
        }

        // Batch invalidation: at most one re-evaluation + one repaint per
        // `apply_outputs` call (a Begin/Commands/Commit edit used to bump twice).
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

    /// Run a menu action. Host-side actions (Rename, clipboard ops, Duplicate)
    /// are handled here; everything else routes to the pure dispatcher.
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
            MenuAction::Copy | MenuAction::Cut => {
                let cut = matches!(action, MenuAction::Cut);
                self.clipboard_from_selection(cut);
                self.close_context_menu();
                return;
            }
            MenuAction::Paste => {
                self.paste_clipboard();
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

    /// Center the selected node's pivot over its rendered geometry without
    /// moving the content (compensated anchor/position edit).
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

    fn selected_roots(&self) -> Vec<renamite_model::NodeId> {
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
        let node = self.file.document.nodes.get(id).cloned().unwrap();
        renamite_history::NodeTree {
            node,
            id: None,
            children: children.iter().map(|&c| self.tree_of(c)).collect(),
        }
    }

    fn clipboard_from_selection(&mut self, cut: bool) {
        let roots = self.selected_roots();
        if roots.is_empty() {
            return;
        }
        self.clipboard = Some(roots.iter().map(|&id| self.tree_of(id)).collect());
        if cut {
            let cmds: SmallVec<[renamite_history::EditorCommand; 4]> = roots
                .iter()
                .map(|&id| renamite_history::EditorCommand::RemoveNode { id })
                .collect();
            self.apply_outputs(smallvec![
                ToolOutput::BeginTransaction("Cut".into()),
                ToolOutput::Commands(cmds),
                ToolOutput::CommitTransaction,
                ToolOutput::RequestSelection(renamite_history::SelectionChange::Set(vec![])),
            ]);
        }
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

    /// Expand (true) or collapse (false) every transform group in the Layers
    /// panel (view state only, no undo).
    pub fn set_all_expanded(&mut self, expanded: bool) {
        let doc = &self.file.document;
        let mut all = std::collections::HashSet::new();
        fn collect(
            node: renamite_model::NodeId,
            doc: &renamite_model::Document,
            out: &mut std::collections::HashSet<renamite_model::NodeId>,
        ) {
            if let Some(n) = doc.nodes.get(node) {
                match &n.kind {
                    renamite_model::NodeKind::Group { .. } | renamite_model::NodeKind::Shape(_) => {
                        out.insert(node);
                    }
                    _ => {}
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

    fn insert_trees(
        &mut self,
        trees: Vec<renamite_history::NodeTree>,
        offset: DVec2,
        label: &str,
    ) -> Vec<renamite_model::NodeId> {
        let mut created = Vec::new();
        self.history.begin(label.to_owned());
        for mut t in trees {
            nudge_tree(&mut t, offset);
            if let Some(id) = self.history_apply(renamite_history::EditorCommand::InsertNode {
                parent: renamite_model::Parent::Comp(self.file.document.main),
                index: 0,
                tree: t,
            }) {
                created.push(id);
            }
        }
        self.history.commit();
        self.dirty = true;
        created
    }

    fn paste_clipboard(&mut self) {
        let Some(trees) = self.clipboard.clone() else {
            return;
        };
        if trees.is_empty() {
            return;
        }
        let created = self.insert_trees(trees, DVec2::new(20.0, 20.0), "Paste");
        if !created.is_empty() {
            self.selection.nodes = created;
            self.ensure_selection_visible();
            self.bump();
        }
    }

    fn duplicate_selection(&mut self) {
        let roots = self.selected_roots();
        if roots.is_empty() {
            return;
        }
        let created = self.insert_trees(
            roots.iter().map(|&id| self.tree_of(id)).collect(),
            DVec2::new(20.0, 20.0),
            "Duplicate",
        );
        if !created.is_empty() {
            self.selection.nodes = created;
            self.ensure_selection_visible();
            self.bump();
        }
    }

    pub fn bump(&mut self) {
        self.engine.reevaluate(&self.file);
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    /// Request a repaint without re-evaluating the engine (pure view state).
    pub fn repaint(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    pub fn set_active_page(&mut self, page: PanelPage) {
        self.active_page = page;
        self.repaint();
    }

    /// Switch the explicit editor mode, wiring up the record flag and the
    /// active page so the shell shows the right workspace surface.
    pub fn set_mode(&mut self, mode: EditorMode) {
        self.mode = mode;
        match mode {
            EditorMode::Design => {
                self.record = false;
                if matches!(self.active_page, PanelPage::Timeline | PanelPage::Interact) {
                    self.active_page = PanelPage::Canvas;
                }
            }
            EditorMode::Animate => {
                self.record = true;
                self.active_page = PanelPage::Timeline;
            }
            EditorMode::Interact => {
                self.record = false;
                self.active_page = PanelPage::Interact;
            }
        }
        self.repaint();
    }

    /// Zoom the timeline by a multiplicative factor, clamped to sane bounds.
    pub fn zoom_timeline(&mut self, factor: f64) {
        let old = self.timeline_zoom.max(0.5);
        self.timeline_zoom = (old * factor).clamp(0.5, 48.0);
        self.repaint();
    }

    /// Move the playhead by a fixed number of frames (transport step).
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

    /// Cycle the playback loop mode (Once → Loop → PingPong).
    pub fn cycle_loop_mode(&mut self) {
        self.playback.loop_mode = match self.playback.loop_mode {
            LoopMode::Once => LoopMode::Loop,
            LoopMode::Loop => LoopMode::PingPong,
            LoopMode::PingPong => LoopMode::Once,
        };
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

    /// Commit the in-progress layer rename. Closes the editor without a
    /// history entry when the name is unchanged, empty, or whitespace-only.
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

    /// Dismiss the in-progress layer rename without committing.
    pub fn cancel_rename(&mut self) {
        if self.renaming.is_none() {
            return;
        }
        self.renaming = None;
        self.repaint();
    }

    /// Apply one command; returns the created node id, if any.
    pub fn history_apply(
        &mut self,
        cmd: renamite_history::EditorCommand,
    ) -> Option<renamite_model::NodeId> {
        self.history_apply_full(cmd)?.created
    }

    /// Apply one command, returning the full `Applied` result (created node
    /// and/or created asset). Surfaces the failure message in the status bar.
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

    /// Upload/refresh the encoded bytes of every attached image asset, and
    /// evict handles for detached assets. Call after any asset mutation.
    pub fn sync_image_assets(&mut self) {
        self.renderer
            .sync_document_images(&self.file.document, &self.render_context);
    }

    /// Import raw font bytes as a project asset (undoable). Derives the family
    /// key from the reported font name (or the file stem as a fallback) and
    /// rejects bytes that don't parse as a font.
    pub fn import_font(&mut self, name: String, bytes: Vec<u8>) {
        use renamite_model::{Asset, FontAsset};
        let family = renamite_text::font_family_name(&bytes).unwrap_or_else(|| {
            std::path::Path::new(&name)
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

    /// Attach a decoded image asset to the project (undoable) and refresh the
    /// viewport's uploaded image handles.
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

    /// Serialize a clean copy of the project as `.ren` text bytes. The live
    /// in-memory project is NOT garbage collected (undo relies on detached
    /// arena entries staying alive); only the save-time snapshot is pruned.
    pub fn save_snapshot(&self) -> anyhow::Result<Vec<u8>> {
        let mut file = self.file.clone();
        file.normalize();
        file.garbage_collect();
        Ok(renamite_io_ren::save(&file)?.into_bytes())
    }

    /// Serialize a clean copy of the project as `.renb` binary bytes.
    pub fn pack_snapshot(&self) -> anyhow::Result<Vec<u8>> {
        let mut file = self.file.clone();
        file.normalize();
        file.garbage_collect();
        Ok(renamite_io_ren::save_binary(&file)?)
    }

    /// Load a fresh project, resetting all session view state and undo history.
    /// Any real document (open/import/template) clears the welcome launcher;
    /// callers that want the launcher back set `welcome = true` afterwards.
    pub fn replace_file(&mut self, file: RenFile) {
        self.file = file;
        self.welcome = false;
        self.history = History::new();
        self.engine = Engine::new(&self.file).expect("valid project");
        self.selection.nodes.clear();
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
        self.machine_preview_drag = None;
        self.machine_graph_gesture = None;
        self.listener_draft = ListenerDraft::default();
        self.viewport.fit_pending = true;
        self.viewport.pan_last = None;
        self.dirty = false;
        self.exporting_png = false;
        self.sync_image_assets();
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    /// Record that the current document was written to `path` (or cleared).
    pub fn mark_saved(&mut self, path: Option<std::path::PathBuf>) {
        self.current_path = path;
        self.dirty = false;
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    /// Apply results queued by async platform dialogs (called once per frame
    /// from the UI). Parsing/serialization already happened off-thread, so this
    /// only installs state.
    ///
    /// Returns true when a deferred [`PendingIntent`] should now be run (a
    /// guard "Save" finished successfully on a non-desktop target).
    pub fn drain_file_ops(&mut self) -> bool {
        let ops = std::mem::take(&mut *self.file_ops.lock().unwrap());
        let mut run_intent = false;
        for op in ops {
            match op {
                PendingFileOp::OpenDone {
                    file,
                    path,
                    message,
                } => {
                    self.replace_file(*file);
                    self.welcome = false;
                    self.current_path = path;
                    self.status = Some(message.to_string());
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
                                ops.lock()
                                    .unwrap()
                                    .push_back(PendingFileOp::ExportFinished {
                                        message: "Exported PNG".to_string(),
                                    });
                            } else {
                                ops.lock()
                                    .unwrap()
                                    .push_back(PendingFileOp::ExportFinished {
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
                    self.status = Some(format!("Error: {message}"));
                    self.revision = self.revision.wrapping_add(1);
                    request_frame();
                }
            }
        }
        run_intent
    }

    /// Defer `intent` behind the unsaved-changes dialog and show it. Showing a
    /// dialog is pure UI state - repaint without re-evaluating the engine.
    pub fn request_discard(&mut self, intent: PendingIntent) {
        self.pending_intent = Some(intent);
        self.confirm_dialog.show();
        self.repaint();
    }

    /// Take the deferred intent (clearing it).
    pub fn take_pending_intent(&mut self) -> Option<PendingIntent> {
        self.pending_intent.take()
    }

    /// Drop a deferred intent (e.g. the user canceled the guard's Save).
    pub fn clear_pending_intent(&mut self) {
        if self.pending_intent.take().is_some() {
            self.repaint();
        }
    }

    /// Called by the `animation_driver` tick each frame. Returns true while playing.
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

    /// Open a color picker editing `initial`, anchored at the screen-space
    /// point the user clicked (`anchor`). The history transaction is begun
    /// lazily on the first change, so an untouched picker leaves no undo entry.
    pub fn open_color_picker(
        &mut self,
        target: PickerTarget,
        initial: renamite_model::Color,
        anchor: DVec2,
    ) {
        self.close_context_menu();
        // Cancel only the old picker's own pending work.
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

    /// Dismiss the picker without committing: an in-progress gesture's changes
    /// (applied since the last `commit_picker_color`) are reverted.
    pub fn close_color_picker(&mut self) {
        self.cancel_open_picker_state();
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    /// Live preview path: writes the current picker color to its target inside
    /// a transaction, so an entire drag coalesces into one undo step.
    pub fn apply_picker_change(&mut self, color: renamite_model::Color) {
        let Some(target) = self.open_picker.as_ref().map(|open| open.target) else {
            return;
        };

        match target {
            PickerTarget::CurrentPaint => {
                // Tool state is not part of document history.
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
        }

        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    /// Returns false if another editor gesture owns the global transaction.
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
            self.record,
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
            self.record,
        );
        self.history_apply(cmd);
    }

    /// End-of-gesture: coalesce the open transaction into a single undo step
    /// and record the color in swatch history. Does not close the picker --
    /// closing is a separate, explicit action so stop-color workflows can stay
    /// open across picks.
    pub fn commit_picker_color(&mut self, color: renamite_model::Color) {
        let Some(target) = self.open_picker.as_ref().map(|open| open.target) else {
            return;
        };

        match target {
            PickerTarget::CurrentPaint => {
                // Update cancellation baseline. Closing after a committed color
                // must preserve that color.
                let committed = self.current_paint.clone();
                if let Some(open) = self.open_picker.as_mut() {
                    open.cancel_current_paint = Some(committed);
                }
            }
            PickerTarget::StyleColor { .. } | PickerTarget::GradientStop { .. } => {
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

    /// Save the current picker color to the swatch history (no history edit).
    pub fn add_swatch(&mut self, color: renamite_model::Color) {
        self.swatches.push(color);
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    /// Dismiss the picker. Commits a still-open picker-owned transaction before
    /// discarding so the latest color is preserved, then clears the picker.
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
    /// Run one pure machine edit through the helpers and commit it as a single
    /// undoable `ReplaceMachine`. On error, surfaces the message and returns
    /// false (no history entry is written).
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

        // Existing runtime instances hold state/input arrays derived from the
        // old definition. Structural edits reset the preview deterministically.
        if self.machine_preview_enabled {
            self.reset_machine_preview();
        }

        true
    }

    /// Switch the active machine (no-op if `id` is not attached).
    pub fn select_machine(&mut self, machine: MachineId) {
        if !self.file.machines.contains_key(machine) || !self.file.machine_order.contains(&machine)
        {
            return;
        }

        self.active_machine = Some(machine);
        self.machine_selection = MachineSelection::None;
        self.machine_graph_gesture = None;

        self.reset_machine_preview();
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    /// Create a default machine (one layer, one Idle state) and select it.
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
            self.reset_machine_preview();
        }

        self.bump();
    }

    /// Delete the active machine from the project (undoable). Clears the
    /// selection if the active machine is removed.
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
        self.machine_selection = MachineSelection::None;
        self.reset_machine_preview();
    }

    /// Rebuild `machine_preview_inputs` from the machine's input defaults and
    /// restart the engine in machine (preview) or timeline mode.
    pub fn reset_machine_preview(&mut self) {
        let Some(machine_id) = self.active_machine else {
            self.machine_preview_inputs.clear();
            return;
        };

        let Some(machine) = self.file.machines.get(machine_id) else {
            self.machine_preview_inputs.clear();
            return;
        };

        self.machine_preview_inputs = machine
            .inputs
            .iter()
            .map(|input| match input.kind {
                InputKind::Bool { default } => InputValue::Bool(default),

                InputKind::Number { default } => InputValue::Number(default),

                InputKind::Trigger => InputValue::Trigger { fired: false },
            })
            .collect();

        if self.machine_preview_enabled {
            self.engine.play_machine(&self.file, machine_id);
        } else {
            self.engine.play_timeline(&self.file, LoopMode::Loop);
        }

        self.engine.reevaluate(&self.file);
        self.revision = self.revision.wrapping_add(1);
        request_frame();
    }

    pub fn set_preview_bool(&mut self, input: usize, value: bool) {
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
        let Some(machine) = self.active_machine else {
            return;
        };

        let Some(input_def) = self.file.machines[machine].inputs.get(input) else {
            return;
        };

        self.engine.fire(&self.file, &input_def.name);

        request_frame();
    }

    /// Node display name for a scene node, falling back to its id string.
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

        if self.pan_last.is_some() {
            return;
        }

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
        let Some(previous) = self.pan_last else {
            return false;
        };

        self.pan_last = Some(position);
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
    px_per_frame: f64,
) -> TimelineCtx<'a> {
    TimelineCtx {
        doc,
        clips,
        target: TimelineTarget::Doc,
        rows,
        layout: TimelineLayout {
            origin_x: 0.0,
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
    let rows = timeline_rows(s);
    let (head, comp) = (s.playback.head, s.file.document.main);
    let range = s.file.document.compositions[comp].range;
    let zoom = s.timeline_zoom;
    let ctx = timeline_ctx(&s.file.document, &s.file.clips, &rows, range, head, zoom);

    // Drop dangling selection after undo/doc edits.
    s.keys.retain_valid(&ctx);

    // Ruler band (above first row) → scrub playhead.
    // Track area → keyframe behavior (hit diamond / box-select / drag).
    let scrub = match &ev {
        TimelineEvent::Press { pos, .. }
        | TimelineEvent::Move { pos, .. }
        | TimelineEvent::Release { pos, .. }
        | TimelineEvent::DoubleClick { pos, .. } => {
            pos.y < ctx.layout.row_top || s.scrub.is_dragging()
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
    let mut added_any = false;

    for row in renamite_behavior_common::inspect::props_for_node(doc, id, Frame(0)) {
        if doc.property_is_animated(id, &row.desc.path) {
            out.push(TimelineRow {
                node: id,
                prop: row.desc.path,
            });
            added_any = true;
        }
    }

    if !added_any && s.selection.nodes.contains(&id) {
        out.push(TimelineRow {
            node: id,
            prop: timeline_row_prop(doc, id),
        });
    }

    if s.expanded_layers.contains(&id)
        && let Some(node) = doc.nodes.get(id)
    {
        for &child in &node.children {
            append_timeline_rows_for_node(s, child, out);
        }
    }
}

/// Which animatable property a layer's timeline row edits. Uses the first
/// Transform-section property from the inspector (position, scale, rotation,
/// opacity, anchor) so rows aren't all labeled "opacity"; falls back to
/// opacity when nothing resolves.
fn timeline_row_prop(doc: &renamite_model::Document, id: renamite_model::NodeId) -> PropPath {
    renamite_behavior_common::inspect::props_for_node(doc, id, Frame(0))
        .into_iter()
        .find(|row| row.desc.section == "Transform")
        .map(|row| row.desc.path)
        .unwrap_or_else(|| PropPath::new("opacity"))
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
    if his.undo(&mut pm).is_ok() {
        s.dirty = true;
    }
}

pub fn redo_cmd(s: &mut Session) {
    let his = &mut s.history;
    let file = &mut s.file;
    let mut pm = pm_from(file);
    if his.redo(&mut pm).is_ok() {
        s.dirty = true;
    }
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

/// Canonical anchor for cursor-anchored popovers (context menu / color picker).
pub fn overlay_anchor(pe: &PointerEvent) -> DVec2 {
    let p = pe.position_in_window();
    DVec2::new(p.x as f64, p.y as f64)
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
            ..
        } = s;
        let ctx = ToolContext {
            doc: &file.document,
            scene: engine.scene(),
            comp: file.document.main,
            selection,
            playhead: Frame(playback.head as i64),
            record: *record,
            view: viewport.view,
            snap: SnapConfig {
                grid: None,
                anchor: false,
                guide: false,
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

fn affine_vector(affine: kurbo::Affine, value: DVec2) -> DVec2 {
    let [a, b, c, d, _, _] = affine.as_coeffs();

    DVec2::new(a * value.x + c * value.y, b * value.x + d * value.y)
}

/// Blank document used by the launcher / "New" template flow. Empty artboard
/// so the template picker is the first thing users see on a fresh project.
pub fn blank_file() -> RenFile {
    RenFile::new(renamite_model::Document::empty(), "Untitled")
}

/// Seeded demo document: one ellipse + fill so the artboard isn't blank.
/// Only used by tests and as a debug/fallback surface - the launcher path
/// deliberately starts from [`blank_file`] instead.
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

/// Register the playback driver once and return the shared session.
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

#[cfg(test)]
mod tests {
    use super::*;
    use renamite_model::{Color, NodeKind, fill_style_for};

    fn fill_id_of(s: &Session) -> renamite_model::NodeId {
        let comp = s.file.document.main;
        let shape = s.file.document.compositions[comp].children[0];
        fill_style_for(&s.file.document, shape).expect("default file has a fill")
    }

    fn add_text_node(s: &mut Session) -> renamite_model::NodeId {
        let text = s.file.document.create_node(renamite_model::Node::new(
            "t",
            renamite_model::NodeKind::Text(renamite_model::TextNode {
                text: "Hi".into(),
                size: renamite_animation::Animated::new(48.0),
                align: renamite_model::TextAlign::Left,
                font: None,
            }),
        ));
        s.file
            .document
            .attach(text, renamite_model::Parent::Comp(s.file.document.main), 0)
            .unwrap();
        text
    }

    #[test]
    fn edit_batch_increments_revision_once() {
        let mut s = Session::new(seeded_demo_file());
        let text = add_text_node(&mut s);
        let before = s.revision;
        s.apply_outputs(smallvec![
            ToolOutput::BeginTransaction("Rename".into()),
            ToolOutput::Commands(smallvec![renamite_history::EditorCommand::SetTextFont {
                id: text,
                font: Some("Fancy".into()),
            }]),
            ToolOutput::CommitTransaction,
        ]);
        assert_eq!(
            s.revision,
            before + 1,
            "a Begin/Commands/Commit edit re-evaluates and repaints exactly once"
        );
        assert!(s.history.can_undo());
    }

    #[test]
    fn empty_transaction_does_not_mark_dirty() {
        let mut s = Session::new(seeded_demo_file());
        let before = s.dirty;
        s.apply_outputs(smallvec![
            ToolOutput::BeginTransaction("No-op".into()),
            ToolOutput::CommitTransaction,
        ]);
        assert_eq!(
            s.dirty, before,
            "empty transaction leaves clean state untouched"
        );
    }

    #[test]
    fn playhead_update_batches_invalidation() {
        let mut s = Session::new(seeded_demo_file());
        let before = s.revision;
        s.apply_outputs(smallvec![ToolOutput::SetPlayhead(12.0)]);
        assert_eq!(s.playback.head, 12.0);
        assert_eq!(s.revision, before + 1, "scrub repaints once");
    }

    #[test]
    fn selection_only_output_does_not_reevaluate_document() {
        let mut s = Session::new(seeded_demo_file());
        let comp = s.file.document.main;
        let id = s.file.document.compositions[comp].children[0];
        let before = s.revision;
        s.apply_outputs(smallvec![ToolOutput::RequestSelection(
            renamite_history::SelectionChange::Set(vec![id])
        )]);
        assert_eq!(s.selection.nodes, vec![id]);
        assert_eq!(
            s.revision,
            before + 1,
            "selection-only output repaints once"
        );
    }

    #[test]
    fn picker_change_then_commit_is_one_undo_step() {
        let mut s = Session::new(seeded_demo_file());
        let fill = fill_id_of(&s);
        s.open_color_picker(
            PickerTarget::StyleColor { style_id: fill },
            Color::BLACK,
            DVec2::ZERO,
        );
        for _ in 0..5 {
            s.apply_picker_change(Color::rgba(0.1, 0.2, 0.3, 1.0));
        }
        s.commit_picker_color(Color::rgba(0.1, 0.2, 0.3, 1.0));
        assert!(s.history.can_undo());
        undo_cmd(&mut s);
        assert!(!s.history.can_undo(), "whole picker drag = one undo step");
    }

    #[test]
    fn undoing_picker_restores_original_color() {
        let mut s = Session::new(seeded_demo_file());
        let fill = fill_id_of(&s);
        let orig = {
            let s = &s;
            let NodeKind::Style(st) = &s.file.document.nodes.get(fill).unwrap().kind else {
                panic!("not a style");
            };
            st.paint().base_color()
        };
        s.open_color_picker(
            PickerTarget::StyleColor { style_id: fill },
            orig,
            DVec2::ZERO,
        );
        s.apply_picker_change(Color::WHITE);
        s.commit_picker_color(Color::WHITE);
        undo_cmd(&mut s);
        let now = {
            let s = &s;
            let NodeKind::Style(st) = &s.file.document.nodes.get(fill).unwrap().kind else {
                panic!("not a style");
            };
            st.paint().base_color()
        };
        assert!((now.r - orig.r).abs() < 1e-6, "color restored after undo");
    }

    #[test]
    fn close_without_commit_cancels() {
        let mut s = Session::new(seeded_demo_file());
        let fill = fill_id_of(&s);
        s.open_color_picker(
            PickerTarget::StyleColor { style_id: fill },
            Color::BLACK,
            DVec2::ZERO,
        );
        s.apply_picker_change(Color::WHITE);
        s.close_color_picker();
        assert!(
            !s.history.can_undo(),
            "cancelled picker leaves no undo entry"
        );
    }

    #[test]
    fn current_paint_picker_does_not_open_history() {
        let mut session = Session::new(seeded_demo_file());

        session.open_color_picker(
            PickerTarget::CurrentPaint,
            session.current_paint.base_color(),
            DVec2::ZERO,
        );
        session.apply_picker_change(Color::WHITE);
        session.commit_picker_color(Color::WHITE);

        assert!(!session.history.can_undo());
        assert!(!session.history.transaction_open());
    }

    #[test]
    fn cancelling_current_paint_restores_original() {
        let mut session = Session::new(seeded_demo_file());
        let original = session.current_paint.clone();

        session.open_color_picker(
            PickerTarget::CurrentPaint,
            original.base_color(),
            DVec2::ZERO,
        );
        session.apply_picker_change(Color::WHITE);
        session.close_color_picker();

        assert_eq!(session.current_paint, original);
    }

    #[test]
    fn import_font_adds_font_asset() {
        let mut session = Session::new(seeded_demo_file());
        let bytes = include_bytes!("../../renamite-text/assets/default.ttf").to_vec();
        let family = renamite_text::font_family_name(&bytes).expect("bundled font has a name");

        session
            .file_ops
            .lock()
            .unwrap()
            .push_back(PendingFileOp::ImportFontDone {
                name: "Inter-Regular.ttf".into(),
                bytes,
            });
        session.drain_file_ops();

        assert!(session.file.document.font_families().contains(&family));
        assert!(session.history.can_undo(), "font import is undoable");
    }

    #[test]
    fn undoing_font_import_removes_asset() {
        let mut session = Session::new(seeded_demo_file());
        let bytes = include_bytes!("../../renamite-text/assets/default.ttf").to_vec();
        let family = renamite_text::font_family_name(&bytes).expect("bundled font has a name");

        session
            .file_ops
            .lock()
            .unwrap()
            .push_back(PendingFileOp::ImportFontDone {
                name: "Undo-Me.ttf".into(),
                bytes,
            });
        session.drain_file_ops();
        assert!(session.file.document.font_families().contains(&family));

        undo_cmd(&mut session);
        assert!(
            !session.file.document.font_families().contains(&family),
            "undo removes the imported font"
        );
    }

    #[test]
    fn set_text_font_round_trips_through_history() {
        let mut session = Session::new(seeded_demo_file());
        // A text node + sibling fill, grouped like the Text tool creates.
        let text = session.file.document.create_node(renamite_model::Node::new(
            "t",
            renamite_model::NodeKind::Text(renamite_model::TextNode {
                text: "Hi".into(),
                size: renamite_animation::Animated::new(48.0),
                align: renamite_model::TextAlign::Left,
                font: None,
            }),
        ));
        session
            .file
            .document
            .attach(
                text,
                renamite_model::Parent::Comp(session.file.document.main),
                0,
            )
            .unwrap();

        session.apply_outputs(smallvec![
            ToolOutput::BeginTransaction("Change font".into()),
            ToolOutput::Commands(smallvec![renamite_history::EditorCommand::SetTextFont {
                id: text,
                font: Some("Fancy".into()),
            },]),
            ToolOutput::CommitTransaction,
        ]);
        let NodeKind::Text(t) = &session.file.document.nodes.get(text).unwrap().kind else {
            panic!("not text");
        };
        assert_eq!(t.font.as_deref(), Some("Fancy"));

        undo_cmd(&mut session);
        let NodeKind::Text(t) = &session.file.document.nodes.get(text).unwrap().kind else {
            panic!("not text");
        };
        assert_eq!(t.font, None);
    }

    #[test]
    fn machine_edit_is_one_undo_step() {
        use renamite_behavior_common::machine::add_input;

        let mut session = Session::new(seeded_demo_file());
        session.create_machine();
        let machine = session.active_machine.expect("machine created");

        for index in 0..3 {
            let ok = session.edit_active_machine(format!("Add input {index}"), move |m| {
                add_input(
                    m,
                    format!("toggle {index}"),
                    InputKind::Bool { default: false },
                )?;
                Ok(())
            });
            assert!(ok, "input edit succeeds");
        }

        assert_eq!(session.file.machines[machine].inputs.len(), 3);

        undo_cmd(&mut session);
        assert_eq!(
            session.file.machines[machine].inputs.len(),
            2,
            "one undo = one machine edit"
        );
    }

    #[test]
    fn preview_bool_drives_transition() {
        use renamite_behavior_common::machine::{
            TransitionSource, add_input, add_state, add_transition, transition_mut,
        };
        use renamite_machine::Condition;

        let mut session = Session::new(seeded_demo_file());
        session.create_machine();
        let machine = session.active_machine.expect("machine created");

        let ok = session.edit_active_machine("Build", |m| {
            add_input(m, "hover", InputKind::Bool { default: false })?;
            add_state(m, 0, "Active", StateKind::Empty)?;
            let index = add_transition(m, 0, TransitionSource::State(0), 1)?;
            transition_mut(m, 0, TransitionSource::State(0), index)?
                .conditions
                .push(Condition::BoolIs {
                    input: 0,
                    value: true,
                });
            Ok(())
        });
        assert!(ok, "build edit succeeds");

        session.machine_preview_enabled = true;
        session.reset_machine_preview();
        session.set_preview_bool(0, true);
        session.engine.tick(&session.file, 0.1);

        let states = session
            .engine
            .active_machine_states()
            .expect("engine in machine mode");
        assert_eq!(states[0], 1, "true bool advances to the Active state");
    }
}
