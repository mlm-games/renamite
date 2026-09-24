//! Commands, transactions, undo/redo. Inverses are captured at apply time from
//! prior document state - never full document clones. RemoveNode is
//! detach-only (arena-stable NodeIds).

use renamite_animation::{Animated, EasingHandle, Frame, Interpolation};
use renamite_geometry::{AnchorEdit, VectorPath};
use renamite_machine::{Clip, ClipId, ClipMap, EventKey, Machine, MachineId, MachineMap, Track};
use renamite_model::{
    Asset, AssetId, CompId, Document, GradientKind, GradientStop, GradientStops, KeyframeData,
    ModelError, ModifierKind, Node, NodeId, NodeKind, Parent, PropMut, PropPath, StyleKind,
    StylePaint, Value,
};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

/// Borrowed view over everything History may mutate. The editor field-splits
/// its `RenFile` into this per call. Document-only hosts pass scratch stores.
pub struct ProjectMut<'a> {
    pub document: &'a mut Document,
    pub clips: &'a mut ClipMap,
    pub clip_order: &'a mut Vec<ClipId>,
    pub machines: &'a mut MachineMap,
    pub machine_order: &'a mut Vec<MachineId>,
    pub start_machine: &'a mut Option<MachineId>,
}

#[derive(Clone)]
struct ProjectState {
    document: Document,
    clips: ClipMap,
    clip_order: Vec<ClipId>,
    machines: MachineMap,
    machine_order: Vec<MachineId>,
    start_machine: Option<MachineId>,
}

impl ProjectState {
    fn capture(project: &ProjectMut<'_>) -> Self {
        Self {
            document: project.document.clone(),
            clips: project.clips.clone(),
            clip_order: project.clip_order.clone(),
            machines: project.machines.clone(),
            machine_order: project.machine_order.clone(),
            start_machine: *project.start_machine,
        }
    }

    fn restore(self, project: &mut ProjectMut<'_>) {
        *project.document = self.document;
        *project.clips = self.clips;
        *project.clip_order = self.clip_order;
        *project.machines = self.machines;
        *project.machine_order = self.machine_order;
        *project.start_machine = self.start_machine;
    }
}

/// Node payload for creation. `id` is None until first apply, then filled so
/// redo re-attaches the SAME arena nodes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeTree {
    pub node: Node,
    pub id: Option<NodeId>,
    pub children: Vec<NodeTree>,
}

impl NodeTree {
    pub fn leaf(node: Node) -> Self {
        Self {
            node,
            id: None,
            children: Vec::new(),
        }
    }
    pub fn with_children(node: Node, children: Vec<NodeTree>) -> Self {
        Self {
            node,
            id: None,
            children,
        }
    }
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum EditorCommand {
    // structure
    InsertNode {
        parent: Parent,
        index: usize,
        tree: NodeTree,
    },
    /// Undo-internal: re-attach an arena node that was detached.
    AttachNode {
        id: NodeId,
        parent: Parent,
        index: usize,
    },
    /// Detach only - node stays in the arena for undo.
    RemoveNode {
        id: NodeId,
    },
    MoveNode {
        id: NodeId,
        new_parent: Parent,
        index: usize,
    },
    GroupNodes {
        ids: Vec<NodeId>,
        group: NodeId,
    },
    /// Atomic "group selection": create a fresh Group node at `parent` and
    /// reparent `ids` into it, all in one undo step. `group` is None until
    /// first apply, then filled so undo/redo reuse the same arena node.
    GroupSelection {
        ids: Vec<NodeId>,
        parent: Parent,
        index: usize,
        group: Option<NodeId>,
    },
    SetNodeFlags {
        id: NodeId,
        visible: Option<bool>,
        locked: Option<bool>,
    },
    SetNodeName {
        id: NodeId,
        name: String,
    },
    /// Whole-string swap (strings aren't tweenable). Coalesces per node so
    /// continuous typing = one undo step.
    SetTextContent {
        id: NodeId,
        text: String,
    },
    /// Whole-field swap of a text node's font family key (`TextNode.font`).
    /// Exact inverse: restore the previous family (or `None` = bundled
    /// default). Coalesces per node, like `SetTextContent`.
    SetTextFont {
        id: NodeId,
        font: Option<String>,
    },
    /// Insert a project asset (font/image bytes). The asset lands in the
    /// arena on first apply; redo re-attaches the same arena id. `id` is None
    /// until first apply, then filled so undo/redo keep AssetIds stable.
    AddAsset {
        index: usize,
        asset: Asset,
        id: Option<AssetId>,
    },
    /// Undo-internal: re-attach an arena asset.
    AttachAsset {
        id: AssetId,
        index: usize,
    },
    /// Detach only - the asset stays in the arena for undo/redo, but
    /// disappears from `asset_order` (and thus the Assets panel).
    DetachAsset {
        id: AssetId,
    },
    /// Swap a fill/stroke's paint for a gradient seeded from its current
    /// solid color. Exact inverse: restore the previous `StylePaint`.
    ConvertToGradient {
        id: NodeId,
        kind: GradientKind,
        start: glam::DVec2,
        end: glam::DVec2,
    },
    /// Swap a gradient fill/stroke back to a solid using the first stop's
    /// color. Exact inverse: restore the previous `StylePaint`.
    ConvertToSolid {
        id: NodeId,
    },
    /// Whole-paint swap (used as the exact inverse of the convert commands).
    /// Undo-internal surface, but also the generic path for inspector edits.
    SetPaint {
        id: NodeId,
        paint: StylePaint,
    },
    /// Enum-field write (TrimMode is not an `Animated<T>`); same pattern as
    /// `SetNodeName` - whole-field swap, exact inverse.
    SetTrimMode {
        id: NodeId,
        mode: renamite_model::TrimMode,
    },
    /// Turn a Shape into a Mask (whole-kind structural edit). Exact inverse:
    /// restore the previous shape. Use `ReleaseMask` to go back.
    ConvertToMask {
        id: NodeId,
    },
    /// Turn a Mask back into a Shape. Exact inverse: `RestoreMask`.
    ReleaseMask {
        id: NodeId,
    },
    /// Undo-internal: restore a mask node's `MaskProps` (inverse of
    /// `ReleaseMask`).
    RestoreMask {
        id: NodeId,
        mask: renamite_model::MaskProps,
    },
    /// Flip a mask's `inverted` flag. Exact inverse: same command with the old
    /// value.
    SetMaskInverted {
        id: NodeId,
        inverted: bool,
    },
    /// Enable/disable a stroke's dash pattern (whole-value structural edit).
    /// No coalescing: discrete structural edits only.
    SetStrokeDash {
        id: NodeId,
        dash: Option<renamite_model::AnimatedDash>,
    },
    /// Flip a ZigZag's `smooth` flag (corner zig vs smooth wave). Exact
    /// inverse: same command with the old value.
    SetZigZagSmooth {
        id: NodeId,
        smooth: bool,
    },
    SetStrokeCap {
        id: NodeId,
        cap: renamite_model::StrokeCap,
    },
    SetStrokeJoin {
        id: NodeId,
        join: renamite_model::StrokeJoin,
    },
    SetFillRule {
        id: NodeId,
        rule: renamite_model::FillRule,
    },
    SetTextAlign {
        id: NodeId,
        align: renamite_model::TextAlign,
    },
    SetStarKind {
        id: NodeId,
        kind: renamite_model::StarKind,
    },
    /// Swap a node's whole kind (e.g. primitive Shape -> evaluated Path).
    /// Exact inverse: restore the previous kind.
    SetNodeKind {
        id: NodeId,
        kind: NodeKind,
    },

    // properties
    SetStatic {
        id: NodeId,
        prop: PropPath,
        value: Value,
    },
    AddKeyframe {
        id: NodeId,
        prop: PropPath,
        frame: Frame,
        value: Value,
    },
    RemoveKeyframe {
        id: NodeId,
        prop: PropPath,
        frame: Frame,
    },
    RestoreKeyframe {
        id: NodeId,
        prop: PropPath,
        key: KeyframeData,
    },
    MoveKeyframes {
        moves: Vec<KeyframeMove>,
    },
    SetEasing {
        id: NodeId,
        prop: PropPath,
        frame: Frame,
        interpolation: Interpolation,
        ease_out: EasingHandle,
        ease_in: EasingHandle,
    },

    // path editing (applies to key at `frame` if Some, else to base)
    EditAnchors {
        id: NodeId,
        frame: Option<Frame>,
        edits: Vec<AnchorEdit>,
    },
    ReversePath {
        id: NodeId,
    },

    /// Extend/shrink a composition's playable frame range.
    SetCompositionRange {
        comp: CompId,
        start: Option<Frame>,
        end: Option<Frame>,
    },
    SetCompositionName {
        comp: CompId,
        name: String,
    },
    SetCompositionSize {
        comp: CompId,
        size: (u32, u32),
    },
    SetCompositionRate {
        comp: CompId,
        rate: renamite_animation::FrameRate,
    },
    SetLayerProps {
        id: NodeId,
        in_frame: Option<Frame>,
        out_frame: Option<Frame>,
        time_stretch: Option<f64>,
        blend: Option<renamite_model::BlendMode>,
    },
    SetPrecompTimeMap {
        id: NodeId,
        offset: Option<Frame>,
        stretch: Option<f64>,
    },
    SetPrecompComp {
        id: NodeId,
        comp: CompId,
    },
    SetImageCrop {
        id: NodeId,
        crop: glam::DVec4,
    },

    CreateClip {
        index: usize,
        clip: Clip,
        id: Option<ClipId>,
    },
    /// Undo-internal: re-attach an arena clip.
    AttachClip {
        id: ClipId,
        index: usize,
    },
    /// Detach only - clip stays in the arena for undo. Machines referencing a
    /// detached clip keep resolving during undo windows. Save-time GC decides.
    DetachClip {
        id: ClipId,
    },
    SetClipMeta {
        id: ClipId,
        name: Option<String>,
        range: Option<(Frame, Frame)>,
    },

    // clip tracks & keys (hot path: fine-grained, coalescable)
    /// Insert-or-replace (carries full easing, so it doubles as restore).
    /// The (node, prop) track is created if missing.
    AddClipKey {
        clip: ClipId,
        node: NodeId,
        prop: PropPath,
        key: KeyframeData,
    },
    RemoveClipKey {
        clip: ClipId,
        node: NodeId,
        prop: PropPath,
        frame: Frame,
    },
    /// Atomic multi-key drag: validated against the batch's final frame-set,
    /// then applied two-phase (remove all, insert all). All or nothing.
    MoveClipKeys {
        moves: Vec<ClipKeyMove>,
    },
    CreateClipTrack {
        clip: ClipId,
        track: Track,
    },
    RemoveClipTrack {
        clip: ClipId,
        node: NodeId,
        prop: PropPath,
    },
    AddClipEvent {
        clip: ClipId,
        event: EventKey,
    },
    RemoveClipEvent {
        clip: ClipId,
        frame: Frame,
        name: String,
    },

    // machines (cold path: coarse-grained, still exactly invertible)
    CreateMachine {
        index: usize,
        machine: Machine,
        id: Option<MachineId>,
    },
    AttachMachine {
        id: MachineId,
        index: usize,
    },
    DetachMachine {
        id: MachineId,
    },
    /// Whole-value structural edit (graph panel). Coalesces per id, so one
    /// drag = one undo step. Machines are small value types; this is cheap.
    ReplaceMachine {
        id: MachineId,
        machine: Machine,
    },
    SetStartMachine {
        start: Option<MachineId>,
    },
    AttachClipTrack {
        clip: ClipId,
        track: Track,
        index: usize,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClipKeyMove {
    pub clip: ClipId,
    pub node: NodeId,
    pub prop: PropPath,
    pub from: Frame,
    pub to: Frame,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyframeMove {
    pub id: NodeId,
    pub prop: PropPath,
    pub from: Frame,
    pub to: Frame,
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum EditError {
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error("path property missing on node")]
    NotAPath,
    #[error("clip not found")]
    MissingClip,
    #[error("clip not attached")]
    ClipNotAttached,
    #[error("clip already attached")]
    ClipAlreadyAttached,
    #[error("track missing on clip")]
    MissingTrack,
    #[error("track already exists on clip")]
    TrackExists,
    #[error("no clip key at frame {0}")]
    NoClipKey(i64),
    #[error("clip key already exists at frame {0}")]
    ClipKeyExists(i64),
    #[error("machine not found")]
    MissingMachine,
    #[error("machine not attached")]
    MachineNotAttached,
    #[error("machine already attached")]
    MachineAlreadyAttached,
    #[error("asset is already attached")]
    AssetAlreadyAttached,
    #[error("asset is not attached")]
    AssetNotAttached,
    #[error("asset is still referenced by an image layer")]
    AssetInUse,
    #[error("a transaction is already open")]
    TransactionOpen,
}

/// Result of a single apply (created ids surface for selection).
pub struct Applied {
    pub created: Option<NodeId>,
    pub created_asset: Option<AssetId>,
    pub created_machine: Option<MachineId>,
}

/// Internal creation payload returned by each apply arm.
#[derive(Default)]
struct Created {
    node: Option<NodeId>,
    asset: Option<AssetId>,
    machine: Option<MachineId>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AppliedTransaction {
    label: String,
    forward: Vec<EditorCommand>,
    /// inverse[i] undoes forward[i]. Each may be several commands.
    inverse: Vec<Vec<EditorCommand>>,
}

fn command_needs_snapshot(command: &EditorCommand) -> bool {
    matches!(
        command,
        EditorCommand::InsertNode { .. }
            | EditorCommand::AttachNode { .. }
            | EditorCommand::RemoveNode { .. }
            | EditorCommand::MoveNode { .. }
            | EditorCommand::GroupNodes { .. }
            | EditorCommand::GroupSelection { .. }
            | EditorCommand::SetNodeKind { .. }
            | EditorCommand::EditAnchors { .. }
            | EditorCommand::MoveKeyframes { .. }
            | EditorCommand::MoveClipKeys { .. }
            | EditorCommand::CreateClip { .. }
            | EditorCommand::DetachClip { .. }
            | EditorCommand::CreateMachine { .. }
            | EditorCommand::DetachMachine { .. }
            | EditorCommand::ReplaceMachine { .. }
    )
}

#[derive(Default)]
pub struct History {
    undo: Vec<AppliedTransaction>,
    redo: Vec<AppliedTransaction>,
    open: Option<AppliedTransaction>,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn begin(&mut self, label: impl Into<String>) {
        // Never drop an open transaction: commit any leftover batch so live
        // drags / property scrubs aren't lost when a new edit starts.
        if self.open.is_some() {
            self.commit();
        }
        self.open = Some(AppliedTransaction {
            label: label.into(),
            forward: Vec::new(),
            inverse: Vec::new(),
        });
    }

    pub fn apply(
        &mut self,
        p: &mut ProjectMut<'_>,
        mut cmd: EditorCommand,
    ) -> Result<Applied, EditError> {
        // Coalesce repeated live-drag edits so one drag = one inverse entry.
        if let Some(t) = &mut self.open
            && let Some(last) = t.forward.last_mut()
        {
            let old_inverse = t.inverse.last().cloned().unwrap_or_default();
            let mut replacement = last.clone();
            if coalesce(&mut replacement, &cmd) {
                let snapshot = ProjectState::capture(p);
                let (created, inverse) = match apply_command(p, &mut cmd) {
                    Ok(result) => result,
                    Err(error) => {
                        snapshot.restore(p);
                        return Err(error);
                    }
                };
                let inverse = coalesced_inverse(&replacement, &inverse, &old_inverse);
                *last = replacement;
                *t.inverse
                    .last_mut()
                    .expect("open transaction has an inverse") = inverse;
                return Ok(Applied {
                    created: created.node,
                    created_asset: created.asset,
                    created_machine: created.machine,
                });
            }
        }
        let snapshot = command_needs_snapshot(&cmd).then(|| ProjectState::capture(p));
        let (created, inverse) = match apply_command(p, &mut cmd) {
            Ok(result) => result,
            Err(error) => {
                if let Some(snapshot) = snapshot {
                    snapshot.restore(p);
                }
                return Err(error);
            }
        };
        if let Some(t) = &mut self.open {
            t.forward.push(cmd);
            t.inverse.push(inverse);
        } else {
            self.undo.push(AppliedTransaction {
                label: String::new(),
                forward: vec![cmd],
                inverse: vec![inverse],
            });
            self.redo.clear();
        }
        Ok(Applied {
            created: created.node,
            created_asset: created.asset,
            created_machine: created.machine,
        })
    }

    /// Close the open transaction and make it undoable. Consecutive
    /// transactions with the same label whose boundary commands coalesce
    /// (e.g. one `SetTextContent` per keystroke under "Edit text") fold into
    /// a single undo step.
    pub fn commit(&mut self) {
        let Some(t) = self.open.take() else {
            return;
        };
        if t.forward.is_empty() {
            return;
        }
        let merge = self.undo.last().is_some_and(|prev| {
            prev.label == t.label
                && prev
                    .forward
                    .last()
                    .zip(t.forward.first())
                    .is_some_and(|(a, b)| {
                        let mut a = a.clone();
                        coalesce(&mut a, b)
                    })
        });
        if merge {
            let prev = self.undo.last_mut().expect("merge requires a prior entry");
            let mut replacement = prev
                .forward
                .last()
                .cloned()
                .expect("merge requires a command");
            coalesce(&mut replacement, &t.forward[0]);
            let old_inverse = prev.inverse.last().cloned().unwrap_or_default();
            let inverse = coalesced_inverse(&replacement, &t.inverse[0], &old_inverse);
            prev.forward.pop();
            prev.forward.push(replacement);
            prev.inverse.pop();
            prev.inverse.push(inverse);
            prev.forward.extend(t.forward.into_iter().skip(1));
            prev.inverse.extend(t.inverse.into_iter().skip(1));
        } else {
            self.undo.push(t);
        }
        self.redo.clear();
    }

    /// Discard the open transaction, applying its inverses.
    pub fn cancel(&mut self, p: &mut ProjectMut<'_>) -> Result<(), EditError> {
        let Some(t) = self.open.take() else {
            return Ok(());
        };
        let snapshot = ProjectState::capture(p);
        if let Err(error) = undo_transaction(p, &t) {
            snapshot.restore(p);
            self.open = Some(t);
            return Err(error);
        }
        Ok(())
    }

    pub fn undo(&mut self, p: &mut ProjectMut<'_>) -> Result<(), EditError> {
        if self.open.is_some() {
            return Err(EditError::TransactionOpen);
        }
        let Some(t) = self.undo.pop() else {
            return Ok(());
        };
        let snapshot = ProjectState::capture(p);
        match undo_transaction(p, &t) {
            Ok(()) => {
                self.redo.push(t);
                Ok(())
            }
            Err(error) => {
                snapshot.restore(p);
                self.undo.push(t);
                Err(error)
            }
        }
    }

    pub fn redo(&mut self, p: &mut ProjectMut<'_>) -> Result<(), EditError> {
        if self.open.is_some() {
            return Err(EditError::TransactionOpen);
        }
        let Some(t) = self.redo.pop() else {
            return Ok(());
        };
        let snapshot = ProjectState::capture(p);
        match redo_transaction(p, &t) {
            Ok(()) => {
                self.undo.push(t);
                Ok(())
            }
            Err(error) => {
                snapshot.restore(p);
                self.redo.push(t);
                Err(error)
            }
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// True while an apply-batch transaction is open (between `begin`/`commit`).
    pub fn transaction_open(&self) -> bool {
        self.open.is_some()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

/// Apply a command to the project, filling in creation ids, and return the
/// created root (if any) plus the inverse commands captured from prior state.
/// Document commands are delegated to [`apply_document_command`].
fn apply_command(
    p: &mut ProjectMut<'_>,
    cmd: &mut EditorCommand,
) -> Result<(Created, Vec<EditorCommand>), EditError> {
    use EditorCommand::*;
    match cmd {
        InsertNode { .. }
        | AttachNode { .. }
        | RemoveNode { .. }
        | MoveNode { .. }
        | GroupNodes { .. }
        | GroupSelection { .. }
        | SetNodeFlags { .. }
        | SetNodeName { .. }
        | SetTextContent { .. }
        | SetTextFont { .. }
        | ConvertToGradient { .. }
        | ConvertToSolid { .. }
        | SetPaint { .. }
        | SetTrimMode { .. }
        | SetStrokeDash { .. }
        | SetStrokeCap { .. }
        | SetStrokeJoin { .. }
        | SetFillRule { .. }
        | SetTextAlign { .. }
        | SetStarKind { .. }
        | ConvertToMask { .. }
        | ReleaseMask { .. }
        | RestoreMask { .. }
        | SetMaskInverted { .. }
        | SetZigZagSmooth { .. }
        | SetNodeKind { .. }
        | SetStatic { .. }
        | AddKeyframe { .. }
        | RemoveKeyframe { .. }
        | RestoreKeyframe { .. }
        | MoveKeyframes { .. }
        | SetEasing { .. }
        | EditAnchors { .. }
        | ReversePath { .. }
        | SetCompositionRange { .. }
        | SetCompositionName { .. }
        | SetCompositionSize { .. }
        | SetCompositionRate { .. }
        | SetLayerProps { .. }
        | SetPrecompTimeMap { .. }
        | SetPrecompComp { .. }
        | SetImageCrop { .. } => {
            let (node, inv) = apply_document_command(p.document, cmd)?;
            Ok((
                Created {
                    node,
                    asset: None,
                    machine: None,
                },
                inv,
            ))
        }

        AddAsset { index, asset, id } => {
            let asset_id = match *id {
                Some(existing) => {
                    if !p.document.assets.contains_key(existing) {
                        return Err(ModelError::MissingAsset.into());
                    }
                    existing
                }
                None => {
                    let new_id = p.document.assets.insert(asset.clone());
                    *id = Some(new_id);
                    new_id
                }
            };

            if p.document.asset_order.contains(&asset_id) {
                return Err(EditError::AssetAlreadyAttached);
            }

            let index = (*index).min(p.document.asset_order.len());
            p.document.asset_order.insert(index, asset_id);

            Ok((
                Created {
                    node: None,
                    asset: Some(asset_id),
                    machine: None,
                },
                vec![DetachAsset { id: asset_id }],
            ))
        }
        AttachAsset { id, index } => {
            if !p.document.assets.contains_key(*id) {
                return Err(ModelError::MissingAsset.into());
            }
            if p.document.asset_order.contains(id) {
                return Err(EditError::AssetAlreadyAttached);
            }
            let index = (*index).min(p.document.asset_order.len());
            p.document.asset_order.insert(index, *id);
            Ok((Created::default(), vec![DetachAsset { id: *id }]))
        }
        DetachAsset { id } => {
            if p.document.image_usage_count(*id) > 0 {
                return Err(EditError::AssetInUse);
            }
            let index = p
                .document
                .asset_order
                .iter()
                .position(|entry| entry == id)
                .ok_or(EditError::AssetNotAttached)?;
            p.document.asset_order.remove(index);
            Ok((Created::default(), vec![AttachAsset { id: *id, index }]))
        }

        CreateClip { index, clip, id } => {
            let cid = match *id {
                Some(c) => {
                    // Redo path: arena entry must still exist (GC is save-only).
                    if !p.clips.contains_key(c) {
                        return Err(EditError::MissingClip);
                    }
                    c
                }
                None => {
                    let c = p.clips.insert(clip.clone());
                    *id = Some(c);
                    c
                }
            };
            if p.clip_order.contains(&cid) {
                return Err(EditError::ClipAlreadyAttached);
            }
            let i = (*index).min(p.clip_order.len());
            p.clip_order.insert(i, cid);
            Ok((Created::default(), vec![DetachClip { id: cid }]))
        }
        AttachClip { id, index } => {
            if !p.clips.contains_key(*id) {
                return Err(EditError::MissingClip);
            }
            if p.clip_order.contains(id) {
                return Err(EditError::ClipAlreadyAttached);
            }
            let i = (*index).min(p.clip_order.len());
            p.clip_order.insert(i, *id);
            Ok((Created::default(), vec![DetachClip { id: *id }]))
        }
        DetachClip { id } => {
            let i = p
                .clip_order
                .iter()
                .position(|c| c == id)
                .ok_or(EditError::ClipNotAttached)?;
            p.clip_order.remove(i);
            Ok((Created::default(), vec![AttachClip { id: *id, index: i }]))
        }
        SetClipMeta { id, name, range } => {
            let c = p.clips.get_mut(*id).ok_or(EditError::MissingClip)?;
            let old_name = name.is_some().then(|| c.name.clone());
            let old_range = range.is_some().then_some(c.range);
            if let Some(n) = name {
                c.name = n.clone();
            }
            if let Some(r) = range {
                c.range = *r;
            }
            Ok((
                Created::default(),
                vec![SetClipMeta {
                    id: *id,
                    name: old_name,
                    range: old_range,
                }],
            ))
        }

        AddClipKey {
            clip,
            node,
            prop,
            key,
        } => {
            let c = p.clips.get_mut(*clip).ok_or(EditError::MissingClip)?;
            match clip_track_mut(c, *node, prop) {
                Some(t) => match t.keys.binary_search_by_key(&key.frame, |k| k.frame) {
                    Ok(i) => {
                        let old = std::mem::replace(&mut t.keys[i], key.clone());
                        Ok((
                            Created::default(),
                            vec![AddClipKey {
                                clip: *clip,
                                node: *node,
                                prop: prop.clone(),
                                key: old,
                            }],
                        ))
                    }
                    Err(i) => {
                        t.keys.insert(i, key.clone());
                        Ok((
                            Created::default(),
                            vec![RemoveClipKey {
                                clip: *clip,
                                node: *node,
                                prop: prop.clone(),
                                frame: key.frame,
                            }],
                        ))
                    }
                },
                None => {
                    // Track auto-created -> the exact inverse is "no track".
                    c.tracks.push(Track {
                        node: *node,
                        prop: prop.clone(),
                        keys: vec![key.clone()],
                    });
                    Ok((
                        Created::default(),
                        vec![RemoveClipTrack {
                            clip: *clip,
                            node: *node,
                            prop: prop.clone(),
                        }],
                    ))
                }
            }
        }
        RemoveClipKey {
            clip,
            node,
            prop,
            frame,
        } => {
            let c = p.clips.get_mut(*clip).ok_or(EditError::MissingClip)?;
            let t = clip_track_mut(c, *node, prop).ok_or(EditError::MissingTrack)?;
            let i = t
                .keys
                .binary_search_by_key(frame, |k| k.frame)
                .map_err(|_| EditError::NoClipKey(frame.0))?;
            let key = t.keys.remove(i); // empty track remains: exact-inverse invariant
            Ok((
                Created::default(),
                vec![AddClipKey {
                    clip: *clip,
                    node: *node,
                    prop: prop.clone(),
                    key,
                }],
            ))
        }
        MoveClipKeys { moves } => {
            // Phase 0: validate against the batch's FINAL frame-set per track.
            let inverse = move_clip_keys(p.clips, moves)?;
            // Phase 1: remove all sources. Phase 2: insert all at destinations.
            Ok((Created::default(), vec![MoveClipKeys { moves: inverse }]))
        }
        CreateClipTrack { clip, track } => {
            let c = p.clips.get_mut(*clip).ok_or(EditError::MissingClip)?;
            if clip_track_mut(c, track.node, &track.prop).is_some() {
                return Err(EditError::TrackExists);
            }
            c.tracks.push(track.clone());
            Ok((
                Created::default(),
                vec![RemoveClipTrack {
                    clip: *clip,
                    node: track.node,
                    prop: track.prop.clone(),
                }],
            ))
        }
        AttachClipTrack { clip, track, index } => {
            let c = p.clips.get_mut(*clip).ok_or(EditError::MissingClip)?;
            if clip_track_mut(c, track.node, &track.prop).is_some() {
                return Err(EditError::TrackExists);
            }
            let index = (*index).min(c.tracks.len());
            c.tracks.insert(index, track.clone());
            Ok((
                Created::default(),
                vec![RemoveClipTrack {
                    clip: *clip,
                    node: track.node,
                    prop: track.prop.clone(),
                }],
            ))
        }
        RemoveClipTrack { clip, node, prop } => {
            let c = p.clips.get_mut(*clip).ok_or(EditError::MissingClip)?;
            let i = c
                .tracks
                .iter()
                .position(|t| t.node == *node && &t.prop == prop)
                .ok_or(EditError::MissingTrack)?;
            let track = c.tracks.remove(i);
            Ok((
                Created::default(),
                vec![AttachClipTrack {
                    clip: *clip,
                    track,
                    index: i,
                }],
            ))
        }
        AddClipEvent { clip, event } => {
            let c = p.clips.get_mut(*clip).ok_or(EditError::MissingClip)?;
            // Canonical (frame, name) order keeps undo/redo structurally exact.
            let i = c.events.partition_point(|e| {
                (e.frame, e.name.as_str()) <= (event.frame, event.name.as_str())
            });
            c.events.insert(i, event.clone());
            Ok((
                Created::default(),
                vec![RemoveClipEvent {
                    clip: *clip,
                    frame: event.frame,
                    name: event.name.clone(),
                }],
            ))
        }
        RemoveClipEvent { clip, frame, name } => {
            let c = p.clips.get_mut(*clip).ok_or(EditError::MissingClip)?;
            let i = c
                .events
                .iter()
                .position(|e| e.frame == *frame && &e.name == name)
                .ok_or(EditError::NoClipKey(frame.0))?;
            let event = c.events.remove(i);
            Ok((
                Created::default(),
                vec![AddClipEvent { clip: *clip, event }],
            ))
        }

        CreateMachine { index, machine, id } => {
            let mid = match *id {
                Some(m) => {
                    if !p.machines.contains_key(m) {
                        return Err(EditError::MissingMachine);
                    }
                    m
                }
                None => {
                    let m = p.machines.insert(machine.clone());
                    *id = Some(m);
                    m
                }
            };
            if p.machine_order.contains(&mid) {
                return Err(EditError::MachineAlreadyAttached);
            }
            let i = (*index).min(p.machine_order.len());
            p.machine_order.insert(i, mid);
            Ok((
                Created {
                    machine: Some(mid),
                    ..Default::default()
                },
                vec![DetachMachine { id: mid }],
            ))
        }
        AttachMachine { id, index } => {
            if !p.machines.contains_key(*id) {
                return Err(EditError::MissingMachine);
            }
            if p.machine_order.contains(id) {
                return Err(EditError::MachineAlreadyAttached);
            }
            let i = (*index).min(p.machine_order.len());
            p.machine_order.insert(i, *id);
            Ok((Created::default(), vec![DetachMachine { id: *id }]))
        }
        DetachMachine { id } => {
            let i = p
                .machine_order
                .iter()
                .position(|m| m == id)
                .ok_or(EditError::MachineNotAttached)?;
            p.machine_order.remove(i);
            let mut inverse = Vec::new();
            // Inverse group is applied REVERSED by undo, so list [SetStart, Attach]
            // replays as Attach-then-SetStart.
            if *p.start_machine == Some(*id) {
                *p.start_machine = None;
                inverse.push(SetStartMachine { start: Some(*id) });
            }
            inverse.push(AttachMachine { id: *id, index: i });
            Ok((Created::default(), inverse))
        }
        ReplaceMachine { id, machine } => {
            let m = p.machines.get_mut(*id).ok_or(EditError::MissingMachine)?;
            let old = std::mem::replace(m, machine.clone());
            Ok((
                Created::default(),
                vec![ReplaceMachine {
                    id: *id,
                    machine: old,
                }],
            ))
        }
        SetStartMachine { start } => {
            if let Some(s) = start
                && !p.machines.contains_key(*s)
            {
                return Err(EditError::MissingMachine);
            }
            let old = std::mem::replace(p.start_machine, *start);
            Ok((Created::default(), vec![SetStartMachine { start: old }]))
        }
    }
}

/// Document-only commands (the pre-refactor apply bodies, unchanged).
fn apply_document_command(
    doc: &mut Document,
    cmd: &mut EditorCommand,
) -> Result<(Option<NodeId>, Vec<EditorCommand>), EditError> {
    use EditorCommand::*;
    match cmd {
        InsertNode {
            parent,
            index,
            tree,
        } => {
            validate_parent(doc, *parent)?;
            let root = ensure_tree(doc, tree)?;
            if doc.locate(root).is_some() {
                return Err(ModelError::AlreadyAttached.into());
            }
            doc.attach(root, *parent, *index)?;
            Ok((Some(root), vec![RemoveNode { id: root }]))
        }
        AttachNode { id, parent, index } => {
            if !doc.nodes.contains_key(*id) {
                return Err(ModelError::MissingNode.into());
            }
            if doc.locate(*id).is_some() {
                return Err(ModelError::AlreadyAttached.into());
            }
            validate_parent(doc, *parent)?;
            doc.attach(*id, *parent, *index)?;
            Ok((None, vec![RemoveNode { id: *id }]))
        }
        RemoveNode { id } => {
            let (parent, index) = doc.detach(*id)?;
            Ok((
                None,
                vec![AttachNode {
                    id: *id,
                    parent,
                    index,
                }],
            ))
        }
        MoveNode {
            id,
            new_parent,
            index,
        } => {
            let old = doc.locate(*id).ok_or(ModelError::NotAttached)?;
            if *new_parent == Parent::Node(*id) || parent_is_descendant(doc, *new_parent, *id) {
                return Err(ModelError::AttachmentCycle.into());
            }
            validate_parent(doc, *new_parent)?;
            doc.detach(*id)?;
            if let Err(error) = doc.attach(*id, *new_parent, *index) {
                let _ = doc.attach(*id, old.0, old.1);
                return Err(error.into());
            }
            Ok((
                None,
                vec![MoveNode {
                    id: *id,
                    new_parent: old.0,
                    index: old.1,
                }],
            ))
        }
        GroupNodes { ids, group } => {
            if !doc.nodes.contains_key(*group) {
                return Err(ModelError::MissingNode.into());
            }
            if doc.locate(*group).is_none() {
                return Err(ModelError::NotAttached.into());
            }
            let mut seen = std::collections::HashSet::new();
            let mut originals = Vec::with_capacity(ids.len());
            let mut desired_children = Vec::<(Parent, Vec<NodeId>)>::new();
            for &id in ids.iter() {
                if !seen.insert(id) || id == *group {
                    return Err(ModelError::AttachmentCycle.into());
                }
                let (parent, index) = doc.locate(id).ok_or(ModelError::NotAttached)?;
                if parent_is_descendant(doc, Parent::Node(*group), id) {
                    return Err(ModelError::AttachmentCycle.into());
                }
                if !desired_children.iter().any(|(known, _)| *known == parent) {
                    let children =
                        children_for_parent(doc, parent).ok_or(ModelError::MalformedTree)?;
                    desired_children.push((parent, children));
                }
                originals.push((id, parent, index));
            }
            if !desired_children
                .iter()
                .any(|(parent, _)| *parent == Parent::Node(*group))
            {
                let children = children_for_parent(doc, Parent::Node(*group))
                    .ok_or(ModelError::MalformedTree)?;
                desired_children.push((Parent::Node(*group), children));
            }
            for (moved, &id) in ids.iter().enumerate() {
                if let Err(error) = doc.detach(id) {
                    for &(id, parent, index) in originals.iter().take(moved).rev() {
                        let _ = doc.attach(id, parent, index);
                    }
                    return Err(error.into());
                }
                if let Err(error) = doc.attach(id, Parent::Node(*group), usize::MAX) {
                    for &(id, parent, index) in originals.iter().take(moved).rev() {
                        let _ = doc.attach(id, parent, index);
                    }
                    let _ = doc.attach(id, originals[moved].1, originals[moved].2);
                    return Err(error.into());
                }
            }
            let group_parent = Parent::Node(*group);
            let mut current_children = desired_children
                .iter()
                .map(|(parent, _)| {
                    (
                        *parent,
                        children_for_parent(doc, *parent).unwrap_or_default(),
                    )
                })
                .collect::<Vec<_>>();
            let group_slot = current_children
                .iter()
                .position(|(parent, _)| *parent == group_parent)
                .expect("group parent was captured");
            let mut operations = Vec::with_capacity(originals.len());
            for (id, parent, _) in &originals {
                let target_slot = current_children
                    .iter()
                    .position(|(known, _)| known == parent)
                    .expect("original parent was captured");
                let desired_slot = desired_children
                    .iter()
                    .position(|(known, _)| known == parent)
                    .expect("original parent was captured");
                let target_position = desired_children[desired_slot]
                    .1
                    .iter()
                    .position(|candidate| candidate == id)
                    .expect("selected node was captured");
                if group_slot != target_slot {
                    current_children[group_slot]
                        .1
                        .retain(|candidate| *candidate != *id);
                } else {
                    current_children[target_slot]
                        .1
                        .retain(|candidate| *candidate != *id);
                }
                let index = current_children[target_slot]
                    .1
                    .iter()
                    .filter(|candidate| {
                        desired_children[desired_slot]
                            .1
                            .iter()
                            .position(|wanted| wanted == *candidate)
                            .is_some_and(|position| position < target_position)
                    })
                    .count();
                current_children[target_slot].1.insert(index, *id);
                operations.push(MoveNode {
                    id: *id,
                    new_parent: *parent,
                    index,
                });
            }
            let inverse = operations.into_iter().rev().collect();
            Ok((None, inverse))
        }
        GroupSelection {
            ids,
            parent,
            index,
            group,
        } => {
            validate_parent(doc, *parent)?;
            let mut seen = std::collections::HashSet::new();
            let mut originals = Vec::with_capacity(ids.len());
            for &id in ids.iter() {
                if !seen.insert(id) {
                    return Err(ModelError::AttachmentCycle.into());
                }
                let old = doc.locate(id).ok_or(ModelError::NotAttached)?;
                if parent_is_descendant(doc, *parent, id) {
                    return Err(ModelError::AttachmentCycle.into());
                }
                originals.push((id, old.0, old.1));
            }
            let existing_group = *group;
            if let Some(gid) = existing_group {
                if !doc.nodes.contains_key(gid) {
                    return Err(ModelError::MissingNode.into());
                }
                if doc.locate(gid).is_some() {
                    return Err(ModelError::AlreadyAttached.into());
                }
                if ids.contains(&gid) || node_contains(doc, gid, ids) {
                    return Err(ModelError::AttachmentCycle.into());
                }
            }
            let created_group = existing_group.is_none();
            let gid = if let Some(gid) = existing_group {
                gid
            } else {
                let gid = doc.create_node(Node::new("Group", NodeKind::Group));
                *group = Some(gid);
                gid
            };
            if doc.locate(gid).is_none()
                && let Err(error) = doc.attach(gid, *parent, *index)
            {
                if created_group {
                    doc.nodes.remove(gid);
                    *group = None;
                }
                return Err(error.into());
            }
            for (moved, &id) in ids.iter().enumerate() {
                if let Err(error) = doc.detach(id) {
                    for &(id, old_parent, old_index) in originals.iter().take(moved).rev() {
                        let _ = doc.attach(id, old_parent, old_index);
                    }
                    let _ = doc.detach(gid);
                    if created_group {
                        doc.nodes.remove(gid);
                        *group = None;
                    }
                    return Err(error.into());
                }
                if let Err(error) = doc.attach(id, Parent::Node(gid), usize::MAX) {
                    for &(id, old_parent, old_index) in originals.iter().take(moved).rev() {
                        let _ = doc.attach(id, old_parent, old_index);
                    }
                    let _ = doc.attach(id, originals[moved].1, originals[moved].2);
                    let _ = doc.detach(gid);
                    if created_group {
                        doc.nodes.remove(gid);
                        *group = None;
                    }
                    return Err(error.into());
                }
            }
            // Undo order matters: move ids back to their original parents
            // FIRST, then detach the (now empty) group. Since undo applies
            // the inverse list in reverse order, put the RemoveNode first so
            // it runs last.
            let mut inverse = vec![RemoveNode { id: gid }];
            inverse.extend(originals.into_iter().map(|(id, parent, index)| MoveNode {
                id,
                new_parent: parent,
                index,
            }));
            Ok((Some(gid), inverse))
        }
        SetNodeFlags {
            id,
            visible,
            locked,
        } => {
            let n = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            let old_visible = n.visible;
            let old_locked = n.locked;
            if let Some(v) = *visible {
                n.visible = v;
            }
            if let Some(l) = *locked {
                n.locked = l;
            }
            Ok((
                None,
                vec![SetNodeFlags {
                    id: *id,
                    visible: visible.is_some().then_some(old_visible),
                    locked: locked.is_some().then_some(old_locked),
                }],
            ))
        }
        SetNodeName { id, name } => {
            let n = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            let old = std::mem::replace(&mut n.name, name.clone());
            Ok((None, vec![SetNodeName { id: *id, name: old }]))
        }
        SetTextContent { id, text } => {
            let n = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            let NodeKind::Text(t) = &mut n.kind else {
                return Err(ModelError::WrongNodeKind("Text").into());
            };
            let old = std::mem::replace(&mut t.text, text.clone());
            Ok((None, vec![SetTextContent { id: *id, text: old }]))
        }
        SetTextFont { id, font } => {
            let n = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            let NodeKind::Text(t) = &mut n.kind else {
                return Err(ModelError::WrongNodeKind("Text").into());
            };
            let old = std::mem::replace(&mut t.font, font.clone());
            Ok((None, vec![SetTextFont { id: *id, font: old }]))
        }
        ConvertToGradient {
            id,
            kind,
            start,
            end,
        } => {
            let n = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            let NodeKind::Style(st) = &mut n.kind else {
                return Err(ModelError::WrongNodeKind("Style").into());
            };
            // Seed both stops with the current solid color so the gradient is
            // invisible until the tool drags the axis; stops then diverge.
            let base = st.paint().base_color();
            let new_paint = StylePaint::Gradient(renamite_model::Gradient {
                kind: *kind,
                start: Animated::new(*start),
                end: Animated::new(*end),
                stops: Animated::new(GradientStops(vec![
                    GradientStop {
                        offset: 0.0,
                        color: base,
                    },
                    GradientStop {
                        offset: 1.0,
                        color: base,
                    },
                ])),
            });
            let prev = st.swap_paint(new_paint);
            Ok((
                None,
                vec![SetPaint {
                    id: *id,
                    paint: prev,
                }],
            ))
        }
        ConvertToSolid { id } => {
            let n = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            let NodeKind::Style(st) = &mut n.kind else {
                return Err(ModelError::WrongNodeKind("Style").into());
            };
            let prev = st.swap_paint(StylePaint::solid(st.paint().base_color()));
            Ok((
                None,
                vec![SetPaint {
                    id: *id,
                    paint: prev,
                }],
            ))
        }
        SetPaint { id, paint } => {
            let n = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            let NodeKind::Style(st) = &mut n.kind else {
                return Err(ModelError::WrongNodeKind("Style").into());
            };
            let prev = st.swap_paint(paint.clone());
            Ok((
                None,
                vec![SetPaint {
                    id: *id,
                    paint: prev,
                }],
            ))
        }
        SetTrimMode { id, mode } => {
            let n = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            let NodeKind::Modifier(ModifierKind::TrimPath { mode: cur, .. }) = &mut n.kind else {
                return Err(ModelError::WrongNodeKind("Trim Path modifier").into());
            };
            let old = std::mem::replace(cur, *mode);
            Ok((None, vec![SetTrimMode { id: *id, mode: old }]))
        }
        SetStrokeDash { id, dash } => {
            let node = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;

            let NodeKind::Style(StyleKind::Stroke { dash: current, .. }) = &mut node.kind else {
                return Err(ModelError::WrongNodeKind("Stroke").into());
            };

            let old = std::mem::replace(current, dash.clone());

            Ok((None, vec![SetStrokeDash { id: *id, dash: old }]))
        }
        SetNodeKind { id, kind } => {
            let node = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            let old = std::mem::replace(&mut node.kind, kind.clone());
            Ok((None, vec![SetNodeKind { id: *id, kind: old }]))
        }
        ConvertToMask { id } => {
            let node = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            let kind = std::mem::replace(&mut node.kind, NodeKind::Group);
            let shape = match kind {
                NodeKind::Shape(shape) => shape,
                other => {
                    node.kind = other;
                    return Err(ModelError::WrongNodeKind("Shape").into());
                }
            };
            node.kind = NodeKind::Mask(renamite_model::MaskProps {
                inverted: false,
                shape,
            });
            Ok((None, vec![ReleaseMask { id: *id }]))
        }
        ReleaseMask { id } => {
            let node = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            let kind = std::mem::replace(&mut node.kind, NodeKind::Group);
            let mask = match kind {
                NodeKind::Mask(mask) => mask,
                other => {
                    node.kind = other;
                    return Err(ModelError::WrongNodeKind("Mask").into());
                }
            };
            node.kind = NodeKind::Shape(mask.shape.clone());
            Ok((None, vec![RestoreMask { id: *id, mask }]))
        }
        RestoreMask { id, mask } => {
            let node = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            let kind = std::mem::replace(&mut node.kind, NodeKind::Group);
            let _shape = match kind {
                NodeKind::Shape(shape) => shape,
                other => {
                    node.kind = other;
                    return Err(ModelError::WrongNodeKind("Shape").into());
                }
            };
            node.kind = NodeKind::Mask(mask.clone());
            Ok((None, vec![ReleaseMask { id: *id }]))
        }
        SetMaskInverted { id, inverted } => {
            let node = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            let NodeKind::Mask(mask) = &mut node.kind else {
                return Err(ModelError::WrongNodeKind("Mask").into());
            };
            let old = std::mem::replace(&mut mask.inverted, *inverted);
            Ok((
                None,
                vec![SetMaskInverted {
                    id: *id,
                    inverted: old,
                }],
            ))
        }
        SetZigZagSmooth { id, smooth } => {
            let node = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            let NodeKind::Modifier(ModifierKind::ZigZag {
                smooth: current, ..
            }) = &mut node.kind
            else {
                return Err(ModelError::WrongNodeKind("Modifier").into());
            };
            let old = std::mem::replace(current, *smooth);
            Ok((
                None,
                vec![SetZigZagSmooth {
                    id: *id,
                    smooth: old,
                }],
            ))
        }
        SetStrokeCap { id, cap } => {
            let node = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            let NodeKind::Style(StyleKind::Stroke { cap: cur, .. }) = &mut node.kind else {
                return Err(ModelError::WrongNodeKind("Stroke").into());
            };
            let old = std::mem::replace(cur, *cap);
            Ok((None, vec![SetStrokeCap { id: *id, cap: old }]))
        }
        SetStrokeJoin { id, join } => {
            let node = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            let NodeKind::Style(StyleKind::Stroke { join: cur, .. }) = &mut node.kind else {
                return Err(ModelError::WrongNodeKind("Stroke").into());
            };
            let old = std::mem::replace(cur, *join);
            Ok((None, vec![SetStrokeJoin { id: *id, join: old }]))
        }
        SetFillRule { id, rule } => {
            let node = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            let NodeKind::Style(StyleKind::Fill { rule: cur, .. }) = &mut node.kind else {
                return Err(ModelError::WrongNodeKind("Fill").into());
            };
            let old = std::mem::replace(cur, *rule);
            Ok((None, vec![SetFillRule { id: *id, rule: old }]))
        }
        SetTextAlign { id, align } => {
            let node = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            let NodeKind::Text(t) = &mut node.kind else {
                return Err(ModelError::WrongNodeKind("Text").into());
            };
            let old = std::mem::replace(&mut t.align, *align);
            Ok((
                None,
                vec![SetTextAlign {
                    id: *id,
                    align: old,
                }],
            ))
        }
        SetStarKind { id, kind } => {
            let node = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            match &mut node.kind {
                NodeKind::Shape(renamite_model::ShapeKind::Star { kind: cur, .. }) => {
                    let old = std::mem::replace(cur, *kind);
                    Ok((None, vec![SetStarKind { id: *id, kind: old }]))
                }
                NodeKind::Mask(renamite_model::MaskProps {
                    shape: renamite_model::ShapeKind::Star { kind: cur, .. },
                    ..
                }) => {
                    let old = std::mem::replace(cur, *kind);
                    Ok((None, vec![SetStarKind { id: *id, kind: old }]))
                }
                _ => Err(ModelError::WrongNodeKind("Star").into()),
            }
        }
        SetStatic { id, prop, value } => {
            let old = doc.set_static(*id, prop, value)?;
            Ok((
                None,
                vec![SetStatic {
                    id: *id,
                    prop: prop.clone(),
                    value: old,
                }],
            ))
        }
        AddKeyframe {
            id,
            prop,
            frame,
            value,
        } => {
            let replaced = doc.add_keyframe(*id, prop, *frame, value)?;
            let inv = match replaced {
                Some(k) => vec![RestoreKeyframe {
                    id: *id,
                    prop: prop.clone(),
                    key: k,
                }],
                None => vec![RemoveKeyframe {
                    id: *id,
                    prop: prop.clone(),
                    frame: *frame,
                }],
            };
            Ok((None, inv))
        }
        RemoveKeyframe { id, prop, frame } => {
            let key = doc.remove_keyframe(*id, prop, *frame)?;
            Ok((
                None,
                vec![RestoreKeyframe {
                    id: *id,
                    prop: prop.clone(),
                    key,
                }],
            ))
        }
        RestoreKeyframe { id, prop, key } => {
            let replaced = doc.keyframe_data(*id, prop, key.frame);
            doc.restore_keyframe(*id, prop, key)?;
            let inv = match replaced {
                Some(k) => vec![RestoreKeyframe {
                    id: *id,
                    prop: prop.clone(),
                    key: k,
                }],
                None => vec![RemoveKeyframe {
                    id: *id,
                    prop: prop.clone(),
                    frame: key.frame,
                }],
            };
            Ok((None, inv))
        }
        MoveKeyframes { moves } => {
            let inverse = move_keyframes(doc, moves)?;
            Ok((None, vec![MoveKeyframes { moves: inverse }]))
        }
        SetEasing {
            id,
            prop,
            frame,
            interpolation,
            ease_out,
            ease_in,
        } => {
            let (oi, oo, oe) =
                doc.set_easing(*id, prop, *frame, *interpolation, *ease_out, *ease_in)?;
            Ok((
                None,
                vec![SetEasing {
                    id: *id,
                    prop: prop.clone(),
                    frame: *frame,
                    interpolation: oi,
                    ease_out: oo,
                    ease_in: oe,
                }],
            ))
        }
        EditAnchors { id, frame, edits } => {
            let prop = PropPath::new("shape.path");
            let inv_edits = {
                let node = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
                match node.prop_mut(&prop) {
                    Some(PropMut::Path(a)) => apply_edits_to(a, *frame, edits)?,
                    _ => return Err(EditError::NotAPath),
                }
            };
            Ok((
                None,
                vec![EditAnchors {
                    id: *id,
                    frame: *frame,
                    edits: inv_edits,
                }],
            ))
        }
        ReversePath { id } => {
            let prop = PropPath::new("shape.path");
            let node = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            match node.prop_mut(&prop) {
                Some(PropMut::Path(a)) => {
                    a.base.reverse();
                    for k in &mut a.keyframes {
                        k.value.reverse();
                    }
                }
                _ => return Err(EditError::NotAPath),
            }
            Ok((None, vec![ReversePath { id: *id }]))
        }
        SetCompositionRange { comp, start, end } => {
            let c = doc
                .compositions
                .get_mut(*comp)
                .ok_or(ModelError::MissingComp)?;
            let old_range = c.range;
            if let Some(s) = start {
                c.range.0 = *s;
            }
            if let Some(e) = end {
                c.range.1 = *e;
            }
            if c.range.0.0 > c.range.1.0 {
                if start.is_some() && end.is_none() {
                    c.range.1 = renamite_animation::Frame(c.range.0.0);
                } else if end.is_some() && start.is_none() {
                    c.range.0 = renamite_animation::Frame(c.range.1.0);
                } else {
                    let (a, b) = (c.range.0.0.min(c.range.1.0), c.range.0.0.max(c.range.1.0));
                    c.range = (renamite_animation::Frame(a), renamite_animation::Frame(b));
                }
            }
            let inverse_start =
                (start.is_some() || c.range.0 != old_range.0).then_some(old_range.0);
            let inverse_end = (end.is_some() || c.range.1 != old_range.1).then_some(old_range.1);
            Ok((
                None,
                vec![SetCompositionRange {
                    comp: *comp,
                    start: inverse_start,
                    end: inverse_end,
                }],
            ))
        }
        SetCompositionName { comp, name } => {
            let c = doc
                .compositions
                .get_mut(*comp)
                .ok_or(ModelError::MissingComp)?;
            let old = std::mem::replace(&mut c.name, name.clone());
            Ok((
                None,
                vec![SetCompositionName {
                    comp: *comp,
                    name: old,
                }],
            ))
        }
        SetCompositionSize { comp, size } => {
            let c = doc
                .compositions
                .get_mut(*comp)
                .ok_or(ModelError::MissingComp)?;
            let old = c.size;
            let size = (size.0.max(1), size.1.max(1));
            c.size = size;
            Ok((
                None,
                vec![SetCompositionSize {
                    comp: *comp,
                    size: old,
                }],
            ))
        }
        SetCompositionRate { comp, rate } => {
            let c = doc
                .compositions
                .get_mut(*comp)
                .ok_or(ModelError::MissingComp)?;
            let old = c.rate;
            let rate = if rate.den == 0 { old } else { *rate };
            let rate = renamite_animation::FrameRate {
                num: rate.num.max(1),
                den: rate.den.max(1),
            };
            c.rate = rate;
            Ok((
                None,
                vec![SetCompositionRate {
                    comp: *comp,
                    rate: old,
                }],
            ))
        }
        SetLayerProps {
            id,
            in_frame,
            out_frame,
            time_stretch,
            blend,
        } => {
            let n = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            let NodeKind::Layer(lp) = &mut n.kind else {
                return Err(ModelError::WrongNodeKind("Layer").into());
            };
            let old_in = in_frame.is_some().then_some(lp.in_frame);
            let old_out = out_frame.is_some().then_some(lp.out_frame);
            let old_stretch = time_stretch.is_some().then_some(lp.time_stretch);
            let old_blend = blend.is_some().then_some(lp.blend);
            if let Some(v) = in_frame {
                lp.in_frame = *v;
            }
            if let Some(v) = out_frame {
                lp.out_frame = *v;
            }
            if let Some(v) = time_stretch {
                let st = if v.is_finite() { v.max(1e-6) } else { 1.0 };
                lp.time_stretch = st;
            }
            if let Some(v) = blend {
                lp.blend = *v;
            }
            Ok((
                None,
                vec![SetLayerProps {
                    id: *id,
                    in_frame: old_in,
                    out_frame: old_out,
                    time_stretch: old_stretch,
                    blend: old_blend,
                }],
            ))
        }
        SetPrecompTimeMap {
            id,
            offset,
            stretch,
        } => {
            let n = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            let NodeKind::Precomp { time_map, .. } = &mut n.kind else {
                return Err(ModelError::WrongNodeKind("Precomp").into());
            };
            let old_off = offset.is_some().then_some(time_map.offset);
            let old_st = stretch.is_some().then_some(time_map.stretch);
            if let Some(v) = offset {
                time_map.offset = *v;
            }
            if let Some(v) = stretch {
                let st = if v.is_finite() { v.max(1e-6) } else { 1.0 };
                time_map.stretch = st;
            }
            Ok((
                None,
                vec![SetPrecompTimeMap {
                    id: *id,
                    offset: old_off,
                    stretch: old_st,
                }],
            ))
        }
        SetPrecompComp { id, comp } => {
            // Validate kind and target before mutable borrow (cycle check needs &doc)
            if !matches!(
                doc.nodes.get(*id).map(|n| &n.kind),
                Some(NodeKind::Precomp { .. })
            ) {
                if doc.nodes.get(*id).is_none() {
                    return Err(ModelError::MissingNode.into());
                } else {
                    return Err(ModelError::WrongNodeKind("Precomp").into());
                }
            }
            if !doc.compositions.contains_key(*comp) {
                return Err(ModelError::MissingComp.into());
            }
            // Cycle guard: precomp must not be reachable from its target.
            if let Some(host) = find_host_comp(doc, *id)
                && (*comp == host || is_comp_reachable(doc, *comp, host))
            {
                return Err(EditError::Model(ModelError::PrecompCycle));
            }
            let n = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            let NodeKind::Precomp { comp: cur, .. } = &mut n.kind else {
                return Err(ModelError::WrongNodeKind("Precomp").into());
            };
            let old = *cur;
            *cur = *comp;
            Ok((None, vec![SetPrecompComp { id: *id, comp: old }]))
        }
        SetImageCrop { id, crop } => {
            let n = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            let NodeKind::Image(img) = &mut n.kind else {
                return Err(ModelError::WrongNodeKind("Image").into());
            };
            let old = img.crop;
            img.crop = *crop;
            Ok((None, vec![SetImageCrop { id: *id, crop: old }]))
        }
        _ => unreachable!("clip/machine commands handled in `apply_command`"),
    }
}

fn validate_parent(doc: &Document, parent: Parent) -> Result<(), ModelError> {
    match parent {
        Parent::Node(id) => {
            if doc.nodes.contains_key(id) {
                Ok(())
            } else {
                Err(ModelError::MissingNode)
            }
        }
        Parent::Comp(id) => {
            if doc.compositions.contains_key(id) {
                Ok(())
            } else {
                Err(ModelError::MissingComp)
            }
        }
    }
}

fn children_for_parent(doc: &Document, parent: Parent) -> Option<Vec<NodeId>> {
    match parent {
        Parent::Node(id) => doc.nodes.get(id).map(|node| node.children.clone()),
        Parent::Comp(id) => doc
            .compositions
            .get(id)
            .map(|composition| composition.children.clone()),
    }
}

fn parent_is_descendant(doc: &Document, parent: Parent, ancestor: NodeId) -> bool {
    let Parent::Node(mut current) = parent else {
        return false;
    };
    for _ in 0..=doc.nodes.len() {
        if current == ancestor {
            return true;
        }
        let Some(node) = doc.nodes.get(current) else {
            return false;
        };
        let Some(next) = node.parent else {
            return false;
        };
        current = next;
    }
    false
}

fn node_contains(doc: &Document, ancestor: NodeId, needles: &[NodeId]) -> bool {
    let mut pending = vec![ancestor];
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = pending.pop() {
        if needles.contains(&id) {
            return true;
        }
        if !seen.insert(id) {
            continue;
        }
        if let Some(node) = doc.nodes.get(id) {
            pending.extend(node.children.iter().copied());
        }
    }
    false
}

fn find_host_comp(doc: &Document, mut node: NodeId) -> Option<CompId> {
    loop {
        if let Some((parent, _)) = doc.locate(node) {
            match parent {
                renamite_model::Parent::Comp(c) => return Some(c),
                renamite_model::Parent::Node(p) => node = p,
            }
        } else {
            // Check if node is root child of any composition (locate failed but node.parent is None)
            for (cid, comp) in &doc.compositions {
                if comp.children.contains(&node) {
                    return Some(cid);
                }
            }
            return None;
        }
    }
}

fn is_comp_reachable(doc: &Document, from: CompId, target: CompId) -> bool {
    use std::collections::HashSet;
    let mut stack = vec![from];
    let mut visited = HashSet::new();
    let mut visited_nodes = HashSet::new();
    while let Some(cur) = stack.pop() {
        if cur == target {
            return true;
        }
        if !visited.insert(cur) {
            continue;
        }
        if let Some(comp) = doc.compositions.get(cur) {
            let mut nodes = comp.children.clone();
            while let Some(node_id) = nodes.pop() {
                if !visited_nodes.insert(node_id) {
                    continue;
                }
                let Some(node) = doc.nodes.get(node_id) else {
                    continue;
                };
                if let NodeKind::Precomp {
                    comp: child_comp, ..
                } = &node.kind
                {
                    stack.push(*child_comp);
                }
                nodes.extend(node.children.iter().copied());
            }
        }
    }
    false
}

fn coalesce_keyframe_moves(last: &mut [KeyframeMove], new: &[KeyframeMove]) -> bool {
    if last.len() != new.len() {
        return false;
    }
    if !last
        .iter()
        .zip(new)
        .all(|(a, b)| a.id == b.id && a.prop == b.prop && (a.from == b.from || a.to == b.from))
    {
        return false;
    }
    for (a, b) in last.iter_mut().zip(new) {
        a.to = b.to;
    }
    true
}

fn coalesce_clip_key_moves(last: &mut [ClipKeyMove], new: &[ClipKeyMove]) -> bool {
    if last.len() != new.len() {
        return false;
    }
    if !last.iter().zip(new).all(|(a, b)| {
        a.clip == b.clip
            && a.node == b.node
            && a.prop == b.prop
            && (a.from == b.from || a.to == b.from)
    }) {
        return false;
    }
    for (a, b) in last.iter_mut().zip(new) {
        a.to = b.to;
    }
    true
}

/// True if `new` continues the same logical edit as `last` (live drag). The
/// merged command replaces `last` in the transaction. Its first inverse entry
/// (pre-drag state) is preserved.
fn coalesce(last: &mut EditorCommand, new: &EditorCommand) -> bool {
    use EditorCommand::*;
    let same = match (&*last, new) {
        (
            SetStatic { id, prop, .. },
            SetStatic {
                id: nid,
                prop: nprop,
                ..
            },
        ) => *id == *nid && *prop == *nprop,
        (
            AddKeyframe {
                id, prop, frame, ..
            },
            AddKeyframe {
                id: nid,
                prop: nprop,
                frame: nframe,
                ..
            },
        ) => *id == *nid && *prop == *nprop && *frame == *nframe,
        (
            SetEasing {
                id, prop, frame, ..
            },
            SetEasing {
                id: nid,
                prop: nprop,
                frame: nframe,
                ..
            },
        ) => *id == *nid && *prop == *nprop && *frame == *nframe,
        (
            EditAnchors { id, frame, .. },
            EditAnchors {
                id: nid,
                frame: nframe,
                ..
            },
        ) => *id == *nid && *frame == *nframe,
        (MoveKeyframes { .. }, MoveKeyframes { .. }) => true,
        (
            SetNodeFlags {
                id,
                visible,
                locked,
            },
            SetNodeFlags {
                id: nid,
                visible: nvisible,
                locked: nlocked,
            },
        ) => {
            *id == *nid
                && visible.is_some() == nvisible.is_some()
                && locked.is_some() == nlocked.is_some()
        }
        (SetNodeName { id, .. }, SetNodeName { id: nid, .. }) => *id == *nid,
        (SetTextContent { id, .. }, SetTextContent { id: nid, .. }) => *id == *nid,
        (SetTextFont { id, .. }, SetTextFont { id: nid, .. }) => *id == *nid,
        (
            AddClipKey {
                clip,
                node,
                prop,
                key,
            },
            AddClipKey {
                clip: nc,
                node: nn,
                prop: np,
                key: nk,
            },
        ) => *clip == *nc && *node == *nn && *prop == *np && key.frame == nk.frame,
        (
            SetClipMeta { id, name, range },
            SetClipMeta {
                id: nid,
                name: nname,
                range: nrange,
            },
        ) => {
            *id == *nid && name.is_some() == nname.is_some() && range.is_some() == nrange.is_some()
        }
        (
            SetCompositionRange { comp, start, end },
            SetCompositionRange {
                comp: ncomp,
                start: nstart,
                end: nend,
            },
        ) => {
            *comp == *ncomp
                && start.is_some() == nstart.is_some()
                && end.is_some() == nend.is_some()
        }
        (SetCompositionName { comp, .. }, SetCompositionName { comp: c2, .. }) => *comp == *c2,
        (SetCompositionSize { comp, .. }, SetCompositionSize { comp: c2, .. }) => *comp == *c2,
        (SetCompositionRate { comp, .. }, SetCompositionRate { comp: c2, .. }) => *comp == *c2,
        (
            SetLayerProps {
                id,
                in_frame,
                out_frame,
                time_stretch,
                blend,
            },
            SetLayerProps {
                id: id2,
                in_frame: inf2,
                out_frame: outf2,
                time_stretch: ts2,
                blend: bl2,
            },
        ) => {
            *id == *id2
                && (in_frame.is_some() == inf2.is_some())
                && (out_frame.is_some() == outf2.is_some())
                && (time_stretch.is_some() == ts2.is_some())
                && (blend.is_some() == bl2.is_some())
        }
        (
            SetPrecompTimeMap {
                id,
                offset,
                stretch,
            },
            SetPrecompTimeMap {
                id: id2,
                offset: o2,
                stretch: s2,
            },
        ) => {
            *id == *id2 && (offset.is_some() == o2.is_some()) && (stretch.is_some() == s2.is_some())
        }
        (SetPrecompComp { id, .. }, SetPrecompComp { id: id2, .. }) => *id == *id2,
        (SetImageCrop { id, .. }, SetImageCrop { id: id2, .. }) => *id == *id2,
        (ReplaceMachine { id, .. }, ReplaceMachine { id: nid, .. }) => *id == *nid,
        (MoveClipKeys { .. }, MoveClipKeys { .. }) => true,
        _ => false,
    };
    if !same {
        return false;
    }
    match (&mut *last, new) {
        (
            EditAnchors {
                id: _,
                frame: _,
                edits,
            },
            EditAnchors {
                id: _,
                frame: _,
                edits: new_edits,
            },
        ) => edits.extend(new_edits.iter().cloned()),
        (MoveKeyframes { moves }, MoveKeyframes { moves: new_moves }) => {
            if !coalesce_keyframe_moves(moves, new_moves) {
                return false;
            }
        }
        (MoveClipKeys { moves }, MoveClipKeys { moves: new_moves }) => {
            if !coalesce_clip_key_moves(moves, new_moves) {
                return false;
            }
        }
        _ => *last = new.clone(),
    }
    true
}

fn coalesced_inverse(
    replacement: &EditorCommand,
    new_inverse: &[EditorCommand],
    old_inverse: &[EditorCommand],
) -> Vec<EditorCommand> {
    match replacement {
        EditorCommand::MoveKeyframes { moves } => {
            vec![EditorCommand::MoveKeyframes {
                moves: moves
                    .iter()
                    .filter(|m| m.from != m.to)
                    .map(|m| KeyframeMove {
                        id: m.id,
                        prop: m.prop.clone(),
                        from: m.to,
                        to: m.from,
                    })
                    .collect(),
            }]
        }
        EditorCommand::MoveClipKeys { moves } => {
            vec![EditorCommand::MoveClipKeys {
                moves: moves
                    .iter()
                    .filter(|m| m.from != m.to)
                    .map(|m| ClipKeyMove {
                        clip: m.clip,
                        node: m.node,
                        prop: m.prop.clone(),
                        from: m.to,
                        to: m.from,
                    })
                    .collect(),
            }]
        }
        EditorCommand::EditAnchors { .. } => {
            let mut inverse = old_inverse.to_vec();
            inverse.extend_from_slice(new_inverse);
            inverse
        }
        _ => old_inverse.to_vec(),
    }
}

fn rollback_applied(p: &mut ProjectMut<'_>, applied: &[Vec<EditorCommand>]) {
    for inverse in applied.iter().rev() {
        for cmd in inverse.iter().rev() {
            let mut c = cmd.clone();
            let _ = apply_command(p, &mut c);
        }
    }
}

fn undo_transaction(p: &mut ProjectMut<'_>, t: &AppliedTransaction) -> Result<(), EditError> {
    let mut applied = Vec::new();
    for group in t.inverse.iter().rev() {
        for cmd in group.iter().rev() {
            let mut c = cmd.clone();
            match apply_command(p, &mut c) {
                Ok((_, inverse)) => applied.push(inverse),
                Err(error) => {
                    rollback_applied(p, &applied);
                    return Err(error);
                }
            }
        }
    }
    Ok(())
}

fn redo_transaction(p: &mut ProjectMut<'_>, t: &AppliedTransaction) -> Result<(), EditError> {
    let mut applied = Vec::new();
    for cmd in &t.forward {
        let mut c = cmd.clone();
        match apply_command(p, &mut c) {
            Ok((_, inverse)) => applied.push(inverse),
            Err(error) => {
                rollback_applied(p, &applied);
                return Err(error);
            }
        }
    }
    Ok(())
}

fn move_keyframes(
    doc: &mut Document,
    moves: &[KeyframeMove],
) -> Result<Vec<KeyframeMove>, ModelError> {
    use std::collections::HashSet;

    struct Group {
        id: NodeId,
        prop: PropPath,
        moves: Vec<KeyframeMove>,
    }

    let mut groups: Vec<Group> = Vec::new();
    for m in moves {
        if let Some(group) = groups
            .iter_mut()
            .find(|group| group.id == m.id && group.prop == m.prop)
        {
            group.moves.push(m.clone());
        } else {
            groups.push(Group {
                id: m.id,
                prop: m.prop.clone(),
                moves: vec![m.clone()],
            });
        }
    }

    let mut snapshots: Vec<Vec<KeyframeData>> = Vec::with_capacity(groups.len());
    for group in &groups {
        let frames = doc.key_frames(group.id, &group.prop);
        let mut sources = HashSet::new();
        let mut destinations = HashSet::new();
        let mut moving_sources = HashSet::new();
        for m in &group.moves {
            if !sources.insert(m.from) {
                return Err(ModelError::KeyframeExists(m.from.0));
            }
            if !destinations.insert(m.to) {
                return Err(ModelError::KeyframeExists(m.to.0));
            }
            if m.from != m.to {
                moving_sources.insert(m.from);
            }
        }
        for m in &group.moves {
            if !frames.contains(&m.from) {
                return Err(ModelError::NoKeyframe(m.from.0));
            }
            if m.from != m.to && frames.contains(&m.to) && !moving_sources.contains(&m.to) {
                return Err(ModelError::KeyframeExists(m.to.0));
            }
        }
        let snapshot = frames
            .iter()
            .map(|frame| {
                doc.keyframe_data(group.id, &group.prop, *frame)
                    .ok_or(ModelError::NoKeyframe(frame.0))
            })
            .collect::<Result<Vec<_>, _>>()?;
        snapshots.push(snapshot);
    }

    let mut applied: Vec<usize> = Vec::new();
    for (group_index, group) in groups.iter().enumerate() {
        let result = (|| {
            for m in group.moves.iter().filter(|m| m.from != m.to) {
                doc.remove_keyframe(group.id, &group.prop, m.from)?;
            }
            for m in group.moves.iter().filter(|m| m.from != m.to) {
                let mut key = snapshots[group_index]
                    .iter()
                    .find(|key| key.frame == m.from)
                    .cloned()
                    .ok_or(ModelError::NoKeyframe(m.from.0))?;
                key.frame = m.to;
                doc.restore_keyframe(group.id, &group.prop, &key)?;
            }
            Ok::<(), ModelError>(())
        })();
        if let Err(error) = result {
            for index in applied.into_iter().rev() {
                restore_keyframe_set(
                    doc,
                    groups[index].id,
                    &groups[index].prop,
                    &snapshots[index],
                );
            }
            restore_keyframe_set(doc, group.id, &group.prop, &snapshots[group_index]);
            return Err(error);
        }
        applied.push(group_index);
    }

    Ok(moves
        .iter()
        .filter(|m| m.from != m.to)
        .map(|m| KeyframeMove {
            id: m.id,
            prop: m.prop.clone(),
            from: m.to,
            to: m.from,
        })
        .collect())
}

fn move_clip_keys(
    clips: &mut ClipMap,
    moves: &[ClipKeyMove],
) -> Result<Vec<ClipKeyMove>, EditError> {
    use std::collections::HashSet;

    struct Group {
        clip: ClipId,
        node: NodeId,
        prop: PropPath,
        moves: Vec<ClipKeyMove>,
    }

    let mut groups: Vec<Group> = Vec::new();
    for m in moves {
        if let Some(group) = groups
            .iter_mut()
            .find(|group| group.clip == m.clip && group.node == m.node && group.prop == m.prop)
        {
            group.moves.push(m.clone());
        } else {
            groups.push(Group {
                clip: m.clip,
                node: m.node,
                prop: m.prop.clone(),
                moves: vec![m.clone()],
            });
        }
    }

    let mut snapshots: Vec<Vec<KeyframeData>> = Vec::with_capacity(groups.len());
    for group in &groups {
        let clip = clips.get(group.clip).ok_or(EditError::MissingClip)?;
        let track = clip
            .tracks
            .iter()
            .find(|track| track.node == group.node && track.prop == group.prop)
            .ok_or(EditError::MissingTrack)?;
        let frames: Vec<Frame> = track.keys.iter().map(|key| key.frame).collect();
        let mut sources = HashSet::new();
        let mut destinations = HashSet::new();
        let mut moving_sources = HashSet::new();
        for m in &group.moves {
            if !sources.insert(m.from) {
                return Err(EditError::NoClipKey(m.from.0));
            }
            if !destinations.insert(m.to) {
                return Err(EditError::ClipKeyExists(m.to.0));
            }
            if m.from != m.to {
                moving_sources.insert(m.from);
            }
        }
        for m in &group.moves {
            if !frames.contains(&m.from) {
                return Err(EditError::NoClipKey(m.from.0));
            }
            if m.from != m.to && frames.contains(&m.to) && !moving_sources.contains(&m.to) {
                return Err(EditError::ClipKeyExists(m.to.0));
            }
        }
        snapshots.push(track.keys.clone());
    }

    let mut applied: Vec<usize> = Vec::new();
    for (group_index, group) in groups.iter().enumerate() {
        let result = (|| {
            let clip = clips.get_mut(group.clip).ok_or(EditError::MissingClip)?;
            let track = clip
                .tracks
                .iter_mut()
                .find(|track| track.node == group.node && track.prop == group.prop)
                .ok_or(EditError::MissingTrack)?;
            for frame in snapshots[group_index].iter().map(|key| key.frame) {
                let index = track
                    .keys
                    .binary_search_by_key(&frame, |key| key.frame)
                    .map_err(|_| EditError::NoClipKey(frame.0))?;
                track.keys.remove(index);
            }
            for m in &group.moves {
                let mut key = snapshots[group_index]
                    .iter()
                    .find(|key| key.frame == m.from)
                    .cloned()
                    .ok_or(EditError::NoClipKey(m.from.0))?;
                key.frame = m.to;
                let index = track.keys.partition_point(|key| key.frame < m.to);
                track.keys.insert(index, key);
            }
            Ok::<(), EditError>(())
        })();
        if let Err(error) = result {
            for index in applied.into_iter().rev() {
                restore_clip_track_keys(
                    clips,
                    groups[index].clip,
                    groups[index].node,
                    &groups[index].prop,
                    &snapshots[index],
                );
            }
            restore_clip_track_keys(
                clips,
                group.clip,
                group.node,
                &group.prop,
                &snapshots[group_index],
            );
            return Err(error);
        }
        applied.push(group_index);
    }

    Ok(moves
        .iter()
        .map(|m| ClipKeyMove {
            clip: m.clip,
            node: m.node,
            prop: m.prop.clone(),
            from: m.to,
            to: m.from,
        })
        .collect())
}

fn restore_clip_track_keys(
    clips: &mut ClipMap,
    clip_id: ClipId,
    node: NodeId,
    prop: &PropPath,
    keys: &[KeyframeData],
) {
    let Some(clip) = clips.get_mut(clip_id) else {
        return;
    };
    let Some(track) = clip
        .tracks
        .iter_mut()
        .find(|track| track.node == node && &track.prop == prop)
    else {
        return;
    };
    track.keys.clear();
    track.keys.extend_from_slice(keys);
}

fn restore_keyframe_set(doc: &mut Document, id: NodeId, prop: &PropPath, keys: &[KeyframeData]) {
    for frame in doc.key_frames(id, prop) {
        let _ = doc.remove_keyframe(id, prop, frame);
    }
    for key in keys {
        let _ = doc.restore_keyframe(id, prop, key);
    }
}

/// Find the (node, prop) track on a clip, if it exists.
fn clip_track_mut<'t>(c: &'t mut Clip, node: NodeId, prop: &PropPath) -> Option<&'t mut Track> {
    c.tracks
        .iter_mut()
        .find(|t| t.node == node && &t.prop == prop)
}

/// Recursively create a tree's arena nodes once, filling `tree.id`. No-ops on
/// redo when ids are already filled. Children are attached to their parents.
fn ensure_tree(doc: &mut Document, tree: &mut NodeTree) -> Result<NodeId, ModelError> {
    let mut created = Vec::new();
    let result = ensure_tree_inner(doc, tree, &mut created);
    if result.is_err() {
        let created_set: std::collections::HashSet<_> = created.iter().copied().collect();
        clear_created_tree_ids(tree, &created_set);
        for id in created.into_iter().rev() {
            doc.nodes.remove(id);
        }
    }
    result
}

fn ensure_tree_inner(
    doc: &mut Document,
    tree: &mut NodeTree,
    created: &mut Vec<NodeId>,
) -> Result<NodeId, ModelError> {
    if let Some(id) = tree.id {
        if doc.nodes.contains_key(id) {
            return Ok(id);
        }
        return Err(ModelError::MissingNode);
    }
    let mut child_ids = Vec::with_capacity(tree.children.len());
    for child in &mut tree.children {
        let id = ensure_tree_inner(doc, child, created)?;
        if doc.locate(id).is_some() {
            return Err(ModelError::WrongNodeKind("detached tree node"));
        }
        child_ids.push(id);
    }
    // Fresh arena payload: strip any stale topology from the snapshot.
    let mut node = tree.node.clone();
    node.parent = None;
    node.children.clear();
    let id = doc.create_node(node);
    created.push(id);
    tree.id = Some(id);
    tree.node.parent = None;
    tree.node.children.clear();
    for child_id in child_ids {
        doc.attach(child_id, Parent::Node(id), usize::MAX)?;
    }
    Ok(id)
}

fn clear_created_tree_ids(tree: &mut NodeTree, created: &std::collections::HashSet<NodeId>) {
    if tree.id.is_some_and(|id| created.contains(&id)) {
        tree.id = None;
    }
    for child in &mut tree.children {
        clear_created_tree_ids(child, created);
    }
}

/// Resolve the current path (keyed value at `frame`, else base) and apply each
/// edit, returning the inverse edits in undo order.
fn apply_edits_to(
    a: &mut Animated<VectorPath>,
    frame: Option<Frame>,
    edits: &[AnchorEdit],
) -> Result<Vec<AnchorEdit>, EditError> {
    let mut candidate = a.clone();
    let mut inv = Vec::with_capacity(edits.len());
    for e in edits {
        let path = match frame {
            Some(f) => {
                let i = candidate
                    .keyframes
                    .binary_search_by_key(&f, |k| k.frame)
                    .map_err(|_| EditError::Model(ModelError::NoKeyframe(f.0)))?;
                &mut candidate.keyframes[i].value
            }
            None => &mut candidate.base,
        };
        inv.push(path.apply_edit(e).ok_or(EditError::NotAPath)?);
    }
    inv.reverse();
    *a = candidate;
    Ok(inv)
}

/// Shared keyframe-recording rule so tools and Properties agree.
pub fn resolve_property_edit(
    doc: &Document,
    id: NodeId,
    prop: &PropPath,
    value: Value,
    playhead: Frame,
    record: bool,
) -> EditorCommand {
    let animated = doc.property_is_animated(id, prop);
    if record || animated {
        EditorCommand::AddKeyframe {
            id,
            prop: prop.clone(),
            frame: playhead,
            value,
        }
    } else {
        EditorCommand::SetStatic {
            id,
            prop: prop.clone(),
            value,
        }
    }
}

/// Output blocks from a `ToolBehavior` invocation.
pub type OutputVec = SmallVec<[ToolOutput; 2]>;

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum ToolOutput {
    Commands(SmallVec<[EditorCommand; 4]>),
    BeginTransaction(String),
    CommitTransaction,
    CancelTransaction,
    SetCursor(CursorIcon),
    RequestSelection(SelectionChange),
    SwitchTool(ToolId),
    /// Timeline scrub - app signal, not doc command.
    SetPlayhead(f64),
    /// Pure overlay/view change.
    Invalidate,
    /// Replace the editor's current-paint swatch (dropper tool).
    SetCurrentPaint(StylePaint),
}

#[derive(Clone, Copy, Debug)]
pub enum CursorIcon {
    Default,
    Crosshair,
    Grab,
    Move,
}

#[derive(Clone, Debug)]
pub enum SelectionChange {
    Set(Vec<NodeId>),
    Toggle(NodeId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolId {
    Select,
    Transform,
    Pen,
    PathEdit,
    Rect,
    Ellipse,
    Star,
    Text,
    Gradient,
    Fill,

    /// Appended last: sample a paint from the canvas and apply it.
    Dropper,
}
