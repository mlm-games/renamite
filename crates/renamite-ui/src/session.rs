use std::cell::RefCell;
use std::rc::Rc;

use glam::DVec2;
use renamite_animation::{Frame, LoopMode, PlayState, Playback};
use renamite_behavior_canvas::{CanvasEvent, PointerButton, ToolSet};
use renamite_behavior_common::{Modifiers, Selection, SnapConfig, ToolContext, ViewTransform};
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
    /// Serialized selection for copy/paste/duplicate (Vec<NodeTree>).
    pub clipboard: Option<Vec<renamite_history::NodeTree>>,
}

/// A destructive action deferred behind the unsaved-changes guard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingIntent {
    New,
    Open,
    ImportLottie,
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
    /// Raw font bytes (`.ttf`/`.otf`) read by the Import Font picker.
    ImportFontDone { name: String, bytes: Vec<u8> },
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
        let engine = Engine::new(&file).expect("project");
        let range = file.document.compositions[file.document.main].range;
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
            clipboard: None,
        }
    }

    pub fn apply_outputs(&mut self, outputs: OutputVec) {
        for out in outputs {
            match out {
                ToolOutput::BeginTransaction(l) => self.history.begin(l),
                ToolOutput::CommitTransaction => {
                    self.history.commit();
                    self.dirty = true;
                    self.bump();
                }
                ToolOutput::CancelTransaction => {
                    apply_cmd(&mut self.history, &mut self.file, None);
                    self.dirty = true;
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

    pub fn open_context_menu(&mut self, menu: ContextMenuState) {
        self.context_menu = Some(menu);
        self.revision = self.revision.wrapping_add(1);
        request_frame();
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
    pub fn history_apply(
        &mut self,
        cmd: renamite_history::EditorCommand,
    ) -> Option<renamite_model::NodeId> {
        let his = &mut self.history;
        let file = &mut self.file;
        let mut pm = pm_from(file);
        match his.apply(&mut pm, cmd) {
            Ok(a) => {
                self.dirty = true;
                a.created
            }
            Err(_) => None,
        }
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
            .history_apply(renamite_history::EditorCommand::AddAsset { asset })
            .is_none()
        {
            self.status = Some("Font import failed".into());
            self.bump();
            return;
        }
        self.status = Some(format!("Imported font: {family}"));
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
    pub fn replace_file(&mut self, file: RenFile) {
        self.file = file;
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
        self.viewport.fit_pending = true;
        self.dirty = false;
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
                PendingFileOp::ImportFontDone { name, bytes } => {
                    self.import_font(name, bytes);
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

    /// Defer `intent` behind the unsaved-changes dialog and show it.
    pub fn request_discard(&mut self, intent: PendingIntent) {
        self.pending_intent = Some(intent);
        self.confirm_dialog.show();
        self.bump();
    }

    /// Take the deferred intent (clearing it).
    pub fn take_pending_intent(&mut self) -> Option<PendingIntent> {
        self.pending_intent.take()
    }

    /// Drop a deferred intent (e.g. the user canceled the guard's Save).
    pub fn clear_pending_intent(&mut self) {
        if self.pending_intent.take().is_some() {
            self.bump();
        }
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

    /// Open a color picker editing `initial`. The history transaction is begun
    /// lazily on the first change, so an untouched picker leaves no undo entry.
    pub fn open_color_picker(&mut self, target: PickerTarget, initial: renamite_model::Color) {
        // Cancel only the old picker's own pending work.
        self.cancel_open_picker_state();

        let cancel_current_paint =
            (target == PickerTarget::CurrentPaint).then(|| self.current_paint.clone());

        self.open_picker = Some(OpenPicker {
            target,
            state: Rc::new(RefCell::new(crate::color_picker::PickerState::from_color(
                initial,
            ))),
            transaction_open: false,
            cancel_current_paint,
        });

        self.revision = self.revision.wrapping_add(1);
        request_frame();
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

/// Default empty document with a seeded ellipse so the artboard isn't blank.
pub fn default_file() -> RenFile {
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
pub fn init_session() -> Rc<RefCell<Session>> {
    let session = remember_with_key("session", || RefCell::new(Session::new(default_file())));

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

    #[test]
    fn picker_change_then_commit_is_one_undo_step() {
        let mut s = Session::new(default_file());
        let fill = fill_id_of(&s);
        s.open_color_picker(PickerTarget::StyleColor { style_id: fill }, Color::BLACK);
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
        let mut s = Session::new(default_file());
        let fill = fill_id_of(&s);
        let orig = {
            let s = &s;
            let NodeKind::Style(st) = &s.file.document.nodes.get(fill).unwrap().kind else {
                panic!("not a style");
            };
            st.paint().base_color()
        };
        s.open_color_picker(PickerTarget::StyleColor { style_id: fill }, orig);
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
        let mut s = Session::new(default_file());
        let fill = fill_id_of(&s);
        s.open_color_picker(PickerTarget::StyleColor { style_id: fill }, Color::BLACK);
        s.apply_picker_change(Color::WHITE);
        s.close_color_picker();
        assert!(
            !s.history.can_undo(),
            "cancelled picker leaves no undo entry"
        );
    }

    #[test]
    fn current_paint_picker_does_not_open_history() {
        let mut session = Session::new(default_file());

        session.open_color_picker(
            PickerTarget::CurrentPaint,
            session.current_paint.base_color(),
        );
        session.apply_picker_change(Color::WHITE);
        session.commit_picker_color(Color::WHITE);

        assert!(!session.history.can_undo());
        assert!(!session.history.transaction_open());
    }

    #[test]
    fn cancelling_current_paint_restores_original() {
        let mut session = Session::new(default_file());
        let original = session.current_paint.clone();

        session.open_color_picker(PickerTarget::CurrentPaint, original.base_color());
        session.apply_picker_change(Color::WHITE);
        session.close_color_picker();

        assert_eq!(session.current_paint, original);
    }

    #[test]
    fn import_font_adds_font_asset() {
        let mut session = Session::new(default_file());
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
        let mut session = Session::new(default_file());
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
        let mut session = Session::new(default_file());
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
}
