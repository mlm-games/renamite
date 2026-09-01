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
            && coalesce(last, &cmd)
        {
            let (created, _) = apply_command(p, &mut cmd)?;
            *last = cmd;
            return Ok(Applied {
                created: created.node,
                created_asset: created.asset,
                created_machine: created.machine,
            });
        }
        let (created, inverse) = apply_command(p, &mut cmd)?;
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
            prev.forward.extend(t.forward);
            prev.inverse.extend(t.inverse);
        } else {
            self.undo.push(t);
        }
        self.redo.clear();
    }

    /// Discard the open transaction, applying its inverses.
    pub fn cancel(&mut self, p: &mut ProjectMut<'_>) -> Result<(), EditError> {
        if let Some(t) = self.open.take() {
            undo_transaction(p, &t)?;
        }
        Ok(())
    }

    pub fn undo(&mut self, p: &mut ProjectMut<'_>) -> Result<(), EditError> {
        if let Some(t) = self.undo.pop() {
            undo_transaction(p, &t)?;
            self.redo.push(t);
        }
        Ok(())
    }

    pub fn redo(&mut self, p: &mut ProjectMut<'_>) -> Result<(), EditError> {
        if let Some(t) = self.redo.pop() {
            redo_transaction(p, &t)?;
            self.undo.push(t);
        }
        Ok(())
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
        | SetCompositionSize { .. }
        | SetCompositionRate { .. }
        | SetLayerProps { .. }
        | SetPrecompTimeMap { .. } => {
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
            use std::collections::{HashMap, HashSet};
            // Phase 0: validate against the batch's FINAL frame-set per track.
            let mut sets: HashMap<(ClipId, NodeId, PropPath), HashSet<Frame>> = HashMap::new();
            for m in moves.iter() {
                let k = (m.clip, m.node, m.prop.clone());
                if !sets.contains_key(&k) {
                    let c = p.clips.get(m.clip).ok_or(EditError::MissingClip)?;
                    let t = c
                        .tracks
                        .iter()
                        .find(|t| t.node == m.node && t.prop == m.prop)
                        .ok_or(EditError::MissingTrack)?;
                    sets.insert(k.clone(), t.keys.iter().map(|x| x.frame).collect());
                }
            }
            for m in moves.iter() {
                let s = sets.get_mut(&(m.clip, m.node, m.prop.clone())).unwrap();
                if !s.remove(&m.from) {
                    return Err(EditError::NoClipKey(m.from.0));
                }
            }
            for m in moves.iter() {
                let s = sets.get_mut(&(m.clip, m.node, m.prop.clone())).unwrap();
                if !s.insert(m.to) {
                    return Err(EditError::ClipKeyExists(m.to.0));
                }
            }
            // Phase 1: remove all sources. Phase 2: insert all at destinations.
            let mut captured = Vec::with_capacity(moves.len());
            for m in moves.iter() {
                let c = p.clips.get_mut(m.clip).expect("validated");
                let t = clip_track_mut(c, m.node, &m.prop).expect("validated");
                let i = t
                    .keys
                    .binary_search_by_key(&m.from, |k| k.frame)
                    .expect("validated");
                captured.push(t.keys.remove(i));
            }
            for (m, mut key) in moves.iter().zip(captured) {
                key.frame = m.to;
                let c = p.clips.get_mut(m.clip).expect("validated");
                let t = clip_track_mut(c, m.node, &m.prop).expect("validated");
                let i = t.keys.partition_point(|k| k.frame < m.to);
                t.keys.insert(i, key);
            }
            let inv = moves
                .iter()
                .map(|m| ClipKeyMove {
                    clip: m.clip,
                    node: m.node,
                    prop: m.prop.clone(),
                    from: m.to,
                    to: m.from,
                })
                .collect();
            Ok((Created::default(), vec![MoveClipKeys { moves: inv }]))
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
                vec![CreateClipTrack { clip: *clip, track }],
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
            let root = ensure_tree(doc, tree)?;
            doc.attach(root, *parent, *index)?;
            Ok((Some(root), vec![RemoveNode { id: root }]))
        }
        AttachNode { id, parent, index } => {
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
            doc.detach(*id)?;
            doc.attach(*id, *new_parent, *index)?;
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
            let originals: Vec<(NodeId, Parent, usize)> = ids
                .iter()
                .filter_map(|&id| doc.locate(id).map(|(p, i)| (id, p, i)))
                .collect();
            for &id in ids.iter() {
                doc.detach(id)?;
                doc.attach(id, Parent::Node(*group), usize::MAX)?;
            }
            let inverse = originals
                .into_iter()
                .map(|(id, parent, index)| MoveNode {
                    id,
                    new_parent: parent,
                    index,
                })
                .collect();
            Ok((None, inverse))
        }
        GroupSelection {
            ids,
            parent,
            index,
            group,
        } => {
            let originals: Vec<(NodeId, Parent, usize)> = ids
                .iter()
                .filter_map(|&id| doc.locate(id).map(|(p, i)| (id, p, i)))
                .collect();
            if originals.len() != ids.len() {
                return Err(ModelError::NotAttached.into());
            }
            let gid = match *group {
                Some(g) => g,
                None => {
                    let g = doc.create_node(Node::new("Group", NodeKind::Group));
                    *group = Some(g);
                    g
                }
            };
            if !doc.nodes.contains_key(gid) {
                return Err(ModelError::MissingNode.into());
            }
            if doc.locate(gid).is_none() {
                doc.attach(gid, *parent, *index)?;
            }
            for &id in ids.iter() {
                doc.detach(id)?;
                doc.attach(id, Parent::Node(gid), usize::MAX)?;
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
            doc.restore_keyframe(*id, prop, key)?;
            Ok((
                None,
                vec![RemoveKeyframe {
                    id: *id,
                    prop: prop.clone(),
                    frame: key.frame,
                }],
            ))
        }
        MoveKeyframes { moves } => {
            let inv = moves
                .iter()
                .map(|m| {
                    doc.move_keyframe(m.id, &m.prop, m.from, m.to)
                        .expect("move validated");
                    KeyframeMove {
                        id: m.id,
                        prop: m.prop.clone(),
                        from: m.to,
                        to: m.from,
                    }
                })
                .collect();
            Ok((None, vec![MoveKeyframes { moves: inv }]))
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
            let old_start = start.is_some().then_some(c.range.0);
            let old_end = end.is_some().then_some(c.range.1);
            if let Some(s) = start {
                c.range.0 = *s;
            }
            if let Some(e) = end {
                c.range.1 = *e;
            }
            Ok((
                None,
                vec![SetCompositionRange {
                    comp: *comp,
                    start: old_start,
                    end: old_end,
                }],
            ))
        }
        SetCompositionSize { comp, size } => {
            let c = doc
                .compositions
                .get_mut(*comp)
                .ok_or(ModelError::MissingComp)?;
            let old = c.size;
            c.size = *size;
            Ok((None, vec![SetCompositionSize { comp: *comp, size: old }]))
        }
        SetCompositionRate { comp, rate } => {
            let c = doc
                .compositions
                .get_mut(*comp)
                .ok_or(ModelError::MissingComp)?;
            let old = c.rate;
            c.rate = *rate;
            Ok((None, vec![SetCompositionRate { comp: *comp, rate: old }]))
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
                lp.time_stretch = (*v).max(1e-6);
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
        SetPrecompTimeMap { id, offset, stretch } => {
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
                time_map.stretch = (*v).max(1e-6);
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
        _ => unreachable!("clip/machine commands handled in `apply_command`"),
    }
}

/// True if `new` continues the same logical edit as `last` (live drag). The
/// merged command replaces `last` in the transaction. Its first inverse entry
/// (pre-drag state) is preserved.
fn coalesce(last: &mut EditorCommand, new: &EditorCommand) -> bool {
    use EditorCommand::*;
    match (last, new) {
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
        (MoveKeyframes { moves }, MoveKeyframes { moves: nmoves }) => {
            moves.len() == nmoves.len()
                && moves
                    .iter()
                    .zip(nmoves.iter())
                    .all(|(a, b)| a.id == b.id && a.prop == b.prop && a.from == b.from)
        }
        (SetNodeFlags { id, .. }, SetNodeFlags { id: nid, .. }) => *id == *nid,
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
        (SetClipMeta { id, .. }, SetClipMeta { id: nid, .. }) => *id == *nid,
        (
            SetCompositionRange { comp, start, .. },
            SetCompositionRange {
                comp: ncomp,
                start: nstart,
                ..
            },
        ) => *comp == *ncomp && *start == *nstart,
        (SetCompositionSize { comp, .. }, SetCompositionSize { comp: c2, .. }) => *comp == *c2,
        (SetCompositionRate { comp, .. }, SetCompositionRate { comp: c2, .. }) => *comp == *c2,
        (SetLayerProps { id, .. }, SetLayerProps { id: id2, .. }) => *id == *id2,
        (SetPrecompTimeMap { id, .. }, SetPrecompTimeMap { id: id2, .. }) => *id == *id2,
        (ReplaceMachine { id, .. }, ReplaceMachine { id: nid, .. }) => *id == *nid,
        (MoveClipKeys { moves }, MoveClipKeys { moves: nmoves }) => {
            moves.len() == nmoves.len()
                && moves.iter().zip(nmoves.iter()).all(|(a, b)| {
                    a.clip == b.clip && a.node == b.node && a.prop == b.prop && a.from == b.from
                })
        }
        _ => false,
    }
}

fn undo_transaction(p: &mut ProjectMut<'_>, t: &AppliedTransaction) -> Result<(), EditError> {
    for group in t.inverse.iter().rev() {
        for cmd in group.iter().rev() {
            let mut c = cmd.clone();
            apply_command(p, &mut c)?;
        }
    }
    Ok(())
}

fn redo_transaction(p: &mut ProjectMut<'_>, t: &AppliedTransaction) -> Result<(), EditError> {
    for cmd in &t.forward {
        let mut c = cmd.clone();
        apply_command(p, &mut c)?;
    }
    Ok(())
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
    if let Some(id) = tree.id {
        return Ok(id);
    }
    let mut child_ids = Vec::with_capacity(tree.children.len());
    for child in &mut tree.children {
        child_ids.push(ensure_tree(doc, child)?);
    }
    // Fresh arena payload: strip any stale topology from the snapshot.
    let mut node = tree.node.clone();
    node.parent = None;
    node.children.clear();
    let id = doc.create_node(node);
    tree.node.parent = None;
    tree.node.children.clear();
    for cid in child_ids {
        doc.attach(cid, Parent::Node(id), usize::MAX)?;
    }
    tree.id = Some(id);
    Ok(id)
}

/// Resolve the current path (keyed value at `frame`, else base) and apply each
/// edit, returning the inverse edits in undo order.
fn apply_edits_to(
    a: &mut Animated<VectorPath>,
    frame: Option<Frame>,
    edits: &[AnchorEdit],
) -> Result<Vec<AnchorEdit>, EditError> {
    let mut inv = Vec::with_capacity(edits.len());
    for e in edits {
        let path = match frame {
            Some(f) => {
                let i = a
                    .keyframes
                    .binary_search_by_key(&f, |k| k.frame)
                    .map_err(|_| EditError::Model(ModelError::NoKeyframe(f.0)))?;
                &mut a.keyframes[i].value
            }
            None => &mut a.base,
        };
        inv.push(path.apply_edit(e).ok_or(EditError::NotAPath)?);
    }
    inv.reverse();
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

#[cfg(test)]
mod tests {
    use super::*;
    use renamite_model::{
        AnimatedDash, Asset, Color, FontAsset, ImageAsset, NodeKind, StrokeCap, StrokeJoin,
        StyleKind, StylePaint, TextNode,
    };

    fn f64_key(f: i64, v: f64) -> KeyframeData {
        KeyframeData {
            frame: Frame(f),
            value: Value::F64(v),
            interpolation: Interpolation::Linear,
            ease_out: EasingHandle::LINEAR_OUT,
            ease_in: EasingHandle::LINEAR_IN,
        }
    }

    fn empty_clip() -> Clip {
        Clip {
            name: "c".into(),
            range: (Frame(0), Frame(60)),
            tracks: vec![],
            events: vec![],
        }
    }

    fn empty_machine() -> Machine {
        Machine {
            name: "m".into(),
            inputs: vec![],
            layers: vec![],
            listeners: vec![],
        }
    }

    struct World {
        doc: Document,
        clips: ClipMap,
        clip_order: Vec<ClipId>,
        machines: MachineMap,
        machine_order: Vec<MachineId>,
        start: Option<MachineId>,
    }

    impl World {
        fn new() -> Self {
            Self {
                doc: Document::empty(),
                clips: ClipMap::default(),
                clip_order: vec![],
                machines: MachineMap::default(),
                machine_order: vec![],
                start: None,
            }
        }
        fn pm(&mut self) -> ProjectMut<'_> {
            ProjectMut {
                document: &mut self.doc,
                clips: &mut self.clips,
                clip_order: &mut self.clip_order,
                machines: &mut self.machines,
                machine_order: &mut self.machine_order,
                start_machine: &mut self.start,
            }
        }
        fn snapshot(&self) -> serde_json::Value {
            serde_json::to_value((
                &self.doc,
                &self.clips,
                &self.clip_order,
                &self.machines,
                &self.machine_order,
                &self.start,
            ))
            .unwrap()
        }
        fn node(&mut self) -> NodeId {
            let id = self.doc.create_node(Node::new("n", NodeKind::Group));
            self.doc.attach(id, Parent::Comp(self.doc.main), 0).unwrap();
            id
        }
    }

    #[test]
    fn insert_undo_redo_is_arena_stable() {
        let mut w = World::new();
        let mut h = History::new();
        let parent = Parent::Comp(w.doc.main);
        let created = h
            .apply(
                &mut w.pm(),
                EditorCommand::InsertNode {
                    parent,
                    index: 0,
                    tree: NodeTree::leaf(Node::new("rect", NodeKind::Group)),
                },
            )
            .unwrap()
            .created
            .unwrap();
        h.commit();
        assert!(w.doc.nodes.contains_key(created));
        assert!(w.doc.locate(created).is_some());
        h.undo(&mut w.pm()).unwrap();
        assert!(w.doc.nodes.contains_key(created)); // still in arena
        assert!(w.doc.locate(created).is_none()); // just detached
        h.redo(&mut w.pm()).unwrap();
        assert!(w.doc.locate(created).is_some());
        assert!(w.doc.nodes.contains_key(created));
    }

    #[test]
    fn remove_undo_reattaches() {
        let mut w = World::new();
        let id = w.node();
        let mut h = History::new();
        h.apply(&mut w.pm(), EditorCommand::RemoveNode { id })
            .unwrap();
        h.commit();
        assert!(w.doc.locate(id).is_none());
        assert!(w.doc.nodes.contains_key(id));
        h.undo(&mut w.pm()).unwrap();
        assert_eq!(w.doc.locate(id), Some((Parent::Comp(w.doc.main), 0)));
        h.redo(&mut w.pm()).unwrap();
        assert!(w.doc.locate(id).is_none());
    }

    #[test]
    fn set_node_kind_swaps_and_round_trips() {
        let mut w = World::new();
        let id = w.node();
        let mut h = History::new();

        let path_kind =
            NodeKind::Shape(renamite_model::ShapeKind::Path(Animated::new(VectorPath {
                anchors: vec![],
                closed: false,
            })));
        h.apply(
            &mut w.pm(),
            EditorCommand::SetNodeKind {
                id,
                kind: path_kind,
            },
        )
        .unwrap();
        h.commit();
        assert!(matches!(w.doc.nodes[id].kind, NodeKind::Shape(_)));

        h.undo(&mut w.pm()).unwrap();
        assert!(matches!(w.doc.nodes[id].kind, NodeKind::Group));

        h.redo(&mut w.pm()).unwrap();
        assert!(matches!(w.doc.nodes[id].kind, NodeKind::Shape(_)));
    }

    #[test]
    fn group_selection_undo_redo() {
        let mut w = World::new();
        let comp = Parent::Comp(w.doc.main);
        let a = w.node();
        let b = w.node();
        let mut h = History::new();
        let created = h
            .apply(
                &mut w.pm(),
                EditorCommand::GroupSelection {
                    ids: vec![a, b],
                    parent: comp,
                    index: 0,
                    group: None,
                },
            )
            .unwrap()
            .created
            .unwrap();
        h.commit();
        assert!(w.doc.locate(created).is_some(), "group attached");
        assert_eq!(w.doc.nodes[created].children.len(), 2);
        assert_eq!(w.doc.locate(a), Some((Parent::Node(created), 0)));
        assert_eq!(w.doc.locate(b), Some((Parent::Node(created), 1)));
        h.undo(&mut w.pm()).unwrap();
        assert!(w.doc.locate(created).is_none(), "group detached on undo");
        assert_eq!(w.doc.locate(a).map(|(p, _)| p), Some(comp));
        assert_eq!(w.doc.locate(b).map(|(p, _)| p), Some(comp));
        h.redo(&mut w.pm()).unwrap();
        assert_eq!(w.doc.locate(a), Some((Parent::Node(created), 0)));
        assert_eq!(w.doc.locate(b), Some((Parent::Node(created), 1)));
    }

    #[test]
    fn set_static_undo_redo_restores() {
        let mut w = World::new();
        let id = w.node();
        let mut h = History::new();
        let prop = PropPath::new("transform.position");
        h.apply(
            &mut w.pm(),
            EditorCommand::SetStatic {
                id,
                prop: prop.clone(),
                value: Value::DVec2(glam::DVec2::new(10.0, 20.0)),
            },
        )
        .unwrap();
        h.commit();
        assert_eq!(
            w.doc.get_static(id, &prop).unwrap(),
            Value::DVec2(glam::DVec2::new(10.0, 20.0))
        );
        h.undo(&mut w.pm()).unwrap();
        assert_eq!(
            w.doc.get_static(id, &prop).unwrap(),
            Value::DVec2(glam::DVec2::ZERO)
        );
        h.redo(&mut w.pm()).unwrap();
        assert_eq!(
            w.doc.get_static(id, &prop).unwrap(),
            Value::DVec2(glam::DVec2::new(10.0, 20.0))
        );
    }

    #[test]
    fn set_text_font_undo_redo_round_trips() {
        let mut world = World::new();
        let id = world.node();
        world.doc.nodes[id].kind = NodeKind::Text(TextNode {
            text: "Hi".into(),
            size: Animated::new(48.0),
            align: renamite_model::TextAlign::Left,
            font: None,
        });

        let mut history = History::new();
        history
            .apply(
                &mut world.pm(),
                EditorCommand::SetTextFont {
                    id,
                    font: Some("Inter".into()),
                },
            )
            .unwrap();
        history.commit();

        let NodeKind::Text(text) = &world.doc.nodes[id].kind else {
            panic!("not text");
        };
        assert_eq!(text.font.as_deref(), Some("Inter"));

        history.undo(&mut world.pm()).unwrap();
        let NodeKind::Text(text) = &world.doc.nodes[id].kind else {
            panic!("not text");
        };
        assert_eq!(text.font, None);

        history.redo(&mut world.pm()).unwrap();
        let NodeKind::Text(text) = &world.doc.nodes[id].kind else {
            panic!("not text");
        };
        assert_eq!(text.font.as_deref(), Some("Inter"));
    }

    #[test]
    fn add_remove_asset_undo_redo_round_trips() {
        let mut world = World::new();
        let mut history = History::new();
        let asset = Asset::Font(FontAsset {
            name: "Inter-Regular.ttf".into(),
            family: "Inter Regular".into(),
            bytes: vec![1, 2, 3, 4],
        });

        // Apply AddAsset; the asset lands in the arena immediately.
        history
            .apply(
                &mut world.pm(),
                EditorCommand::AddAsset {
                    index: 0,
                    asset: asset.clone(),
                    id: None,
                },
            )
            .unwrap();
        assert_eq!(world.doc.assets.len(), 1);
        assert_eq!(world.doc.asset_order.len(), 1);
        assert_eq!(world.doc.font_families(), vec!["Inter Regular"]);
        let id = world.doc.asset_order[0];
        assert!(world.doc.assets.contains_key(id));
        history.commit();

        history.undo(&mut world.pm()).unwrap();
        // Undo detaches from `asset_order`; the arena entry stays (for redo).
        assert_eq!(world.doc.assets.len(), 1);
        assert!(world.doc.asset_order.is_empty());

        history.redo(&mut world.pm()).unwrap();
        // Redo re-attaches the SAME arena id (stable under undo/redo).
        assert_eq!(world.doc.asset_order, vec![id]);
        assert_eq!(world.doc.font_families(), vec!["Inter Regular"]);
    }

    #[test]
    fn detach_asset_forbids_in_use_images() {
        use renamite_model::ImageAsset;
        let mut world = World::new();
        let mut history = History::new();

        let asset = Asset::Image(ImageAsset {
            name: "a.png".into(),
            mime: "image/png".into(),
            bytes: vec![],
            width: 2,
            height: 2,
            srgb: true,
        });

        let id = history
            .apply(
                &mut world.pm(),
                EditorCommand::AddAsset {
                    index: 0,
                    asset,
                    id: None,
                },
            )
            .unwrap()
            .created_asset
            .unwrap();
        history.commit();

        // Image layer referencing the asset -> cannot detach.
        let image = world.doc.create_node(Node::new("img", NodeKind::Image(id)));
        world
            .doc
            .attach(image, Parent::Comp(world.doc.main), 0)
            .unwrap();

        assert!(matches!(
            history.apply(&mut world.pm(), EditorCommand::DetachAsset { id }),
            Err(EditError::AssetInUse)
        ));
    }

    #[test]
    fn convert_to_gradient_undo_redo_restores_paint() {
        use renamite_model::{Color, StylePaint};
        let mut w = World::new();
        let id = w.node();
        w.doc.nodes[id].kind = NodeKind::Style(StyleKind::Fill {
            paint: StylePaint::solid(Color::rgba(0.1, 0.5, 0.9, 1.0)),
            rule: renamite_model::FillRule::NonZero,
        });
        let mut h = History::new();
        h.apply(
            &mut w.pm(),
            EditorCommand::ConvertToGradient {
                id,
                kind: GradientKind::Linear,
                start: glam::DVec2::new(0.0, 0.0),
                end: glam::DVec2::new(100.0, 0.0),
            },
        )
        .unwrap();
        h.commit();
        let NodeKind::Style(st) = &w.doc.nodes[id].kind else {
            panic!("not a style");
        };
        assert!(
            matches!(st.paint(), StylePaint::Gradient(g) if g.kind == GradientKind::Linear),
            "should be a linear gradient"
        );
        assert_eq!(st.paint().base_color(), Color::rgba(0.1, 0.5, 0.9, 1.0));

        h.undo(&mut w.pm()).unwrap();
        let NodeKind::Style(st) = &w.doc.nodes[id].kind else {
            panic!("not a style");
        };
        assert_eq!(st.paint().base_color(), Color::rgba(0.1, 0.5, 0.9, 1.0));
        assert!(
            matches!(st.paint(), StylePaint::Solid { .. }),
            "undo restores the solid"
        );

        h.redo(&mut w.pm()).unwrap();
        let NodeKind::Style(st) = &w.doc.nodes[id].kind else {
            panic!("not a style");
        };
        assert!(
            matches!(st.paint(), StylePaint::Gradient(g) if g.kind == GradientKind::Linear),
            "redo restores the gradient"
        );
    }

    #[test]
    fn set_name_undo_redo_restores() {
        let mut w = World::new();
        let id = w.node();
        assert_eq!(w.doc.nodes[id].name, "n");
        let mut h = History::new();
        h.apply(
            &mut w.pm(),
            EditorCommand::SetNodeName {
                id,
                name: "renamed".into(),
            },
        )
        .unwrap();
        h.commit();
        assert_eq!(w.doc.nodes[id].name, "renamed");
        h.undo(&mut w.pm()).unwrap();
        assert_eq!(w.doc.nodes[id].name, "n");
        h.redo(&mut w.pm()).unwrap();
        assert_eq!(w.doc.nodes[id].name, "renamed");
    }

    #[test]
    fn set_text_content_undo_redo_and_coalesces_per_node() {
        let mut w = World::new();
        let id = w.node();
        w.doc.nodes[id].kind = NodeKind::Text(renamite_model::TextNode {
            text: "Text".into(),
            size: Animated::new(48.0),
            align: Default::default(),
            font: None,
        });
        let mut h = History::new();
        h.begin("Edit text");
        h.apply(
            &mut w.pm(),
            EditorCommand::SetTextContent {
                id,
                text: "Hello".into(),
            },
        )
        .unwrap();
        h.apply(
            &mut w.pm(),
            EditorCommand::SetTextContent {
                id,
                text: "Hello!".into(),
            },
        )
        .unwrap();
        h.commit();
        let NodeKind::Text(t) = &w.doc.nodes[id].kind else {
            panic!("expected text");
        };
        assert_eq!(t.text, "Hello!");
        h.undo(&mut w.pm()).unwrap();
        let NodeKind::Text(t) = &w.doc.nodes[id].kind else {
            panic!("expected text");
        };
        assert_eq!(t.text, "Text");
        h.redo(&mut w.pm()).unwrap();
        let NodeKind::Text(t) = &w.doc.nodes[id].kind else {
            panic!("expected text");
        };
        assert_eq!(t.text, "Hello!");
    }

    #[test]
    fn consecutive_edit_text_transactions_share_one_undo_step() {
        let mut w = World::new();
        let id = w.node();
        w.doc.nodes[id].kind = NodeKind::Text(renamite_model::TextNode {
            text: "Text".into(),
            size: Animated::new(48.0),
            align: Default::default(),
            font: None,
        });
        let mut h = History::new();
        for text in ["a", "ab", "abc"] {
            h.begin("Edit text");
            h.apply(
                &mut w.pm(),
                EditorCommand::SetTextContent {
                    id,
                    text: text.into(),
                },
            )
            .unwrap();
            h.commit();
        }
        let NodeKind::Text(t) = &w.doc.nodes[id].kind else {
            panic!("expected text");
        };
        assert_eq!(t.text, "abc");
        // One undo unwinds all three keystrokes back to the pre-edit value.
        h.undo(&mut w.pm()).unwrap();
        let NodeKind::Text(t) = &w.doc.nodes[id].kind else {
            panic!("expected text");
        };
        assert_eq!(t.text, "Text");
        h.redo(&mut w.pm()).unwrap();
        let NodeKind::Text(t) = &w.doc.nodes[id].kind else {
            panic!("expected text");
        };
        assert_eq!(t.text, "abc");
    }

    #[test]
    fn set_text_content_wrong_kind_is_error() {
        let mut w = World::new();
        let id = w.node(); // group node, not text
        let mut h = History::new();
        let err = h
            .apply(
                &mut w.pm(),
                EditorCommand::SetTextContent {
                    id,
                    text: "x".into(),
                },
            )
            .err();
        assert!(matches!(
            err,
            Some(EditError::Model(ModelError::WrongNodeKind("Text")))
        ));
    }

    #[test]
    fn set_trim_mode_undo_redo_round_trips() {
        let mut w = World::new();
        let id = w.node();
        w.doc.nodes[id].kind = NodeKind::Modifier(ModifierKind::TrimPath {
            start: Animated::new(0.0),
            end: Animated::new(1.0),
            offset: Animated::new(0.0),
            mode: renamite_model::TrimMode::Individually,
        });
        let mut h = History::new();
        let cmd = EditorCommand::SetTrimMode {
            id,
            mode: renamite_model::TrimMode::Simultaneously,
        };
        h.apply(&mut w.pm(), cmd).unwrap();
        h.commit();
        let NodeKind::Modifier(ModifierKind::TrimPath { mode, .. }) = &w.doc.nodes[id].kind else {
            panic!("not a trim path");
        };
        assert_eq!(*mode, renamite_model::TrimMode::Simultaneously);
        h.undo(&mut w.pm()).unwrap();
        let NodeKind::Modifier(ModifierKind::TrimPath { mode, .. }) = &w.doc.nodes[id].kind else {
            panic!("not a trim path");
        };
        assert_eq!(*mode, renamite_model::TrimMode::Individually);
        h.redo(&mut w.pm()).unwrap();
        let NodeKind::Modifier(ModifierKind::TrimPath { mode, .. }) = &w.doc.nodes[id].kind else {
            panic!("not a trim path");
        };
        assert_eq!(*mode, renamite_model::TrimMode::Simultaneously);
    }

    #[test]
    fn set_zigzag_smooth_undo_redo_round_trips() {
        let mut w = World::new();
        let id = w.node();
        w.doc.nodes[id].kind = NodeKind::Modifier(ModifierKind::ZigZag {
            amplitude: Animated::new(10.0),
            frequency: Animated::new(4.0),
            smooth: false,
        });
        let mut h = History::new();
        let cmd = EditorCommand::SetZigZagSmooth { id, smooth: true };
        h.apply(&mut w.pm(), cmd).unwrap();
        h.commit();
        let NodeKind::Modifier(ModifierKind::ZigZag { smooth, .. }) = &w.doc.nodes[id].kind else {
            panic!("not a zigzag");
        };
        assert!(*smooth);
        h.undo(&mut w.pm()).unwrap();
        let NodeKind::Modifier(ModifierKind::ZigZag { smooth, .. }) = &w.doc.nodes[id].kind else {
            panic!("not a zigzag");
        };
        assert!(!*smooth);
        h.redo(&mut w.pm()).unwrap();
        let NodeKind::Modifier(ModifierKind::ZigZag { smooth, .. }) = &w.doc.nodes[id].kind else {
            panic!("not a zigzag");
        };
        assert!(*smooth);
    }

    #[test]
    fn set_stroke_dash_undo_redo_roundtrips() {
        let mut world = World::new();
        let id = world.node();

        world.doc.nodes[id].kind = NodeKind::Style(StyleKind::Stroke {
            paint: StylePaint::solid(Color::BLACK),
            width: Animated::new(4.0),
            cap: StrokeCap::Round,
            join: StrokeJoin::Round,
            dash: None,
        });

        let dash = AnimatedDash {
            dashes: vec![Animated::new(12.0), Animated::new(8.0)],
            offset: Animated::new(0.0),
        };

        let mut history = History::new();

        history
            .apply(
                &mut world.pm(),
                EditorCommand::SetStrokeDash {
                    id,
                    dash: Some(dash.clone()),
                },
            )
            .unwrap();

        history.commit();

        let NodeKind::Style(StyleKind::Stroke {
            dash: Some(current),
            ..
        }) = &world.doc.nodes[id].kind
        else {
            panic!("expected dashed stroke");
        };

        assert_eq!(current, &dash);

        history.undo(&mut world.pm()).unwrap();

        let NodeKind::Style(StyleKind::Stroke {
            dash: dash_field, ..
        }) = &world.doc.nodes[id].kind
        else {
            panic!("expected stroke");
        };

        assert!(dash_field.is_none());

        history.redo(&mut world.pm()).unwrap();

        let NodeKind::Style(StyleKind::Stroke {
            dash: Some(current),
            ..
        }) = &world.doc.nodes[id].kind
        else {
            panic!("expected dashed stroke");
        };

        assert_eq!(current, &dash);
    }

    #[test]
    fn drag_coalesces_to_one_undo_step() {
        let mut w = World::new();
        let id = w.node();
        let mut h = History::new();
        let prop = PropPath::new("transform.position");
        h.begin("drag");
        for p in [
            glam::DVec2::new(1.0, 0.0),
            glam::DVec2::new(2.0, 0.0),
            glam::DVec2::new(3.0, 0.0),
        ] {
            h.apply(
                &mut w.pm(),
                EditorCommand::SetStatic {
                    id,
                    prop: prop.clone(),
                    value: Value::DVec2(p),
                },
            )
            .unwrap();
        }
        h.commit();
        assert!(h.can_undo());
        h.undo(&mut w.pm()).unwrap();
        assert!(!h.can_undo()); // one drag = one undo
        assert_eq!(
            w.doc.get_static(id, &prop).unwrap(),
            Value::DVec2(glam::DVec2::ZERO)
        );
        h.redo(&mut w.pm()).unwrap();
        assert_eq!(
            w.doc.get_static(id, &prop).unwrap(),
            Value::DVec2(glam::DVec2::new(3.0, 0.0))
        );
    }

    #[test]
    fn add_remove_keyframe_undo() {
        let mut w = World::new();
        let id = w.node();
        let mut h = History::new();
        let prop = PropPath::new("transform.position");
        let v = Value::DVec2(glam::DVec2::new(5.0, 5.0));
        h.apply(
            &mut w.pm(),
            EditorCommand::AddKeyframe {
                id,
                prop: prop.clone(),
                frame: Frame(10),
                value: v,
            },
        )
        .unwrap();
        h.apply(
            &mut w.pm(),
            EditorCommand::RemoveKeyframe {
                id,
                prop: prop.clone(),
                frame: Frame(10),
            },
        )
        .unwrap();
        h.commit();
        assert!(w.doc.keyframe_data(id, &prop, Frame(10)).is_none());
        h.undo(&mut w.pm()).unwrap();
        assert!(w.doc.keyframe_data(id, &prop, Frame(10)).is_some());
        h.undo(&mut w.pm()).unwrap();
        assert!(w.doc.keyframe_data(id, &prop, Frame(10)).is_none());
    }

    #[test]
    fn create_clip_undo_redo_is_arena_stable() {
        let mut w = World::new();
        let mut h = History::new();
        h.apply(
            &mut w.pm(),
            EditorCommand::CreateClip {
                index: 0,
                clip: empty_clip(),
                id: None,
            },
        )
        .unwrap();
        let cid = w.clip_order[0];
        h.undo(&mut w.pm()).unwrap();
        assert!(w.clips.contains_key(cid)); // still in arena
        assert!(w.clip_order.is_empty()); // just detached
        h.redo(&mut w.pm()).unwrap();
        assert_eq!(w.clip_order, vec![cid]); // SAME id re-attached
    }

    #[test]
    fn add_clip_key_track_creation_has_exact_inverse() {
        let mut w = World::new();
        let node = w.node();
        let cid = w.clips.insert(empty_clip());
        w.clip_order.push(cid);
        let s0 = w.snapshot();

        let mut h = History::new();
        h.apply(
            &mut w.pm(),
            EditorCommand::AddClipKey {
                clip: cid,
                node,
                prop: PropPath::new("opacity"),
                key: f64_key(0, 0.5),
            },
        )
        .unwrap();
        assert_eq!(w.clips[cid].tracks.len(), 1);
        h.undo(&mut w.pm()).unwrap();
        assert_eq!(w.snapshot(), s0); // no empty track left behind
    }

    #[test]
    fn move_clip_keys_is_atomic() {
        let mut w = World::new();
        let node = w.node();
        let prop = PropPath::new("opacity");
        let cid = w.clips.insert(Clip {
            tracks: vec![Track {
                node,
                prop: prop.clone(),
                keys: vec![f64_key(0, 0.0), f64_key(5, 1.0), f64_key(9, 2.0)],
            }],
            ..empty_clip()
        });
        w.clip_order.push(cid);
        let s0 = w.snapshot();
        let mut h = History::new();

        // Intra-batch shuffle 0->5, 5->9 collides with the STATIONARY key at 9.
        let bad = EditorCommand::MoveClipKeys {
            moves: vec![
                ClipKeyMove {
                    clip: cid,
                    node,
                    prop: prop.clone(),
                    from: Frame(0),
                    to: Frame(5),
                },
                ClipKeyMove {
                    clip: cid,
                    node,
                    prop: prop.clone(),
                    from: Frame(5),
                    to: Frame(9),
                },
            ],
        };
        assert!(h.apply(&mut w.pm(), bad).is_err());
        assert_eq!(w.snapshot(), s0); // untouched

        let good = EditorCommand::MoveClipKeys {
            moves: vec![
                ClipKeyMove {
                    clip: cid,
                    node,
                    prop: prop.clone(),
                    from: Frame(0),
                    to: Frame(3),
                },
                ClipKeyMove {
                    clip: cid,
                    node,
                    prop: prop.clone(),
                    from: Frame(5),
                    to: Frame(0),
                },
            ],
        };
        h.apply(&mut w.pm(), good).unwrap();
        h.undo(&mut w.pm()).unwrap();
        assert_eq!(w.snapshot(), s0);
    }

    #[test]
    fn detach_machine_clears_and_restores_start() {
        let mut w = World::new();
        let mid = w.machines.insert(empty_machine());
        w.machine_order.push(mid);
        w.start = Some(mid);
        let mut h = History::new();
        h.apply(&mut w.pm(), EditorCommand::DetachMachine { id: mid })
            .unwrap();
        assert_eq!(w.start, None); // invariant held
        h.undo(&mut w.pm()).unwrap();
        assert_eq!(w.start, Some(mid)); // restored, attach-then-set order
        assert_eq!(w.machine_order, vec![mid]);
    }

    #[test]
    fn replace_machine_drag_coalesces() {
        let mut w = World::new();
        let mid = w.machines.insert(empty_machine());
        w.machine_order.push(mid);
        let mut h = History::new();
        h.begin("edit graph");
        for name in ["b", "c", "d"] {
            let m = Machine {
                name: name.into(),
                ..empty_machine()
            };
            h.apply(
                &mut w.pm(),
                EditorCommand::ReplaceMachine {
                    id: mid,
                    machine: m,
                },
            )
            .unwrap();
        }
        h.commit();
        h.undo(&mut w.pm()).unwrap();
        assert_eq!(w.machines[mid].name, "m"); // one drag = one undo
        assert!(!h.can_undo());
        h.redo(&mut w.pm()).unwrap();
        assert_eq!(w.machines[mid].name, "d");
    }

    mod prop_tests {
        use super::*;
        use proptest::prelude::*;

        #[derive(Clone, Debug)]
        enum GenCmd {
            AddKey { frame: i64, v: f64 },
            RemoveKey { frame: i64 },
            MoveKey { from: i64, to: i64 },
            SetName(String),
            Detach,
            Attach,
            DocStatic { x: f64 },
            ReplaceMachineName(String),
            SetStart(bool),
        }

        fn arb_cmd() -> impl Strategy<Value = GenCmd> {
            prop_oneof![
                (0i64..8, -5.0f64..5.0).prop_map(|(frame, v)| GenCmd::AddKey { frame, v }),
                (0i64..8).prop_map(|frame| GenCmd::RemoveKey { frame }),
                (0i64..8, 0i64..8).prop_map(|(from, to)| GenCmd::MoveKey { from, to }),
                "[a-z]{1,6}".prop_map(GenCmd::SetName),
                Just(GenCmd::Detach),
                Just(GenCmd::Attach),
                (-10.0f64..10.0).prop_map(|x| GenCmd::DocStatic { x }),
                "[a-z]{1,6}".prop_map(GenCmd::ReplaceMachineName),
                any::<bool>().prop_map(GenCmd::SetStart),
            ]
        }

        proptest! {
            #[test]
            fn interleaved_undo_redo_identity(cmds in proptest::collection::vec(arb_cmd(), 1..24)) {
                let mut w = World::new();
                let node = w.node();
                let cid = w.clips.insert(empty_clip());
                w.clip_order.push(cid);
                let mid = w.machines.insert(empty_machine());
                w.machine_order.push(mid);
                let prop = PropPath::new("opacity");

                let s0 = w.snapshot();
                let mut h = History::new();
                let mut applied = 0usize;
                for g in cmds {
                    let cmd = match g {
                        GenCmd::AddKey { frame, v } => EditorCommand::AddClipKey {
                            clip: cid, node, prop: prop.clone(), key: f64_key(frame, v) },
                        GenCmd::RemoveKey { frame } => EditorCommand::RemoveClipKey {
                            clip: cid, node, prop: prop.clone(), frame: Frame(frame) },
                        GenCmd::MoveKey { from, to } => EditorCommand::MoveClipKeys { moves: vec![
                            ClipKeyMove { clip: cid, node, prop: prop.clone(),
                                          from: Frame(from), to: Frame(to) }] },
                        GenCmd::SetName(n) => EditorCommand::SetClipMeta {
                            id: cid, name: Some(n), range: None },
                        GenCmd::Detach => EditorCommand::DetachClip { id: cid },
                        GenCmd::Attach => EditorCommand::AttachClip { id: cid, index: 0 },
                        GenCmd::DocStatic { x } => EditorCommand::SetStatic {
                            id: node, prop: PropPath::new("transform.position"),
                            value: Value::DVec2(glam::DVec2::new(x, 0.0)) },
                        GenCmd::ReplaceMachineName(n) => EditorCommand::ReplaceMachine {
                            id: mid, machine: Machine { name: n, ..empty_machine() } },
                        GenCmd::SetStart(on) => EditorCommand::SetStartMachine {
                            start: on.then_some(mid) },
                    };
                    if h.apply(&mut w.pm(), cmd).is_ok() { applied += 1; }
                }
                let s_final = w.snapshot();
                for _ in 0..applied { h.undo(&mut w.pm()).unwrap(); }
                prop_assert_eq!(w.snapshot(), s0);
                for _ in 0..applied { h.redo(&mut w.pm()).unwrap(); }
                prop_assert_eq!(w.snapshot(), s_final);
            }
        }
    }

    #[test]
    fn convert_shape_to_mask_undo_redo_roundtrips() {
        let mut world = World::new();
        let id = world.node();

        world.doc.nodes[id].kind = NodeKind::Shape(renamite_model::ShapeKind::Ellipse {
            pos: Animated::new(glam::DVec2::ZERO),
            size: Animated::new(glam::DVec2::splat(10.0)),
        });

        let mut history = History::new();

        history
            .apply(&mut world.pm(), EditorCommand::ConvertToMask { id })
            .unwrap();
        history.commit();

        assert!(matches!(world.doc.nodes[id].kind, NodeKind::Mask(_)));

        history.undo(&mut world.pm()).unwrap();
        assert!(matches!(world.doc.nodes[id].kind, NodeKind::Shape(_)));

        history.redo(&mut world.pm()).unwrap();
        assert!(matches!(world.doc.nodes[id].kind, NodeKind::Mask(_)));
    }

    #[test]
    fn release_mask_and_restore_roundtrip() {
        let mut world = World::new();
        let id = world.node();

        world.doc.nodes[id].kind = NodeKind::Mask(renamite_model::MaskProps {
            inverted: true,
            shape: renamite_model::ShapeKind::Rect {
                pos: Animated::new(glam::DVec2::ZERO),
                size: Animated::new(glam::DVec2::splat(20.0)),
                rounded: Animated::new(0.0),
            },
        });

        let mut history = History::new();
        history
            .apply(&mut world.pm(), EditorCommand::ReleaseMask { id })
            .unwrap();
        history.commit();

        let NodeKind::Shape(renamite_model::ShapeKind::Rect { size, .. }) =
            &world.doc.nodes[id].kind
        else {
            panic!("expected rect shape");
        };
        assert_eq!(size.base, glam::DVec2::splat(20.0));

        history.undo(&mut world.pm()).unwrap();
        let NodeKind::Mask(mask) = &world.doc.nodes[id].kind else {
            panic!("expected mask");
        };
        assert!(mask.inverted);
        assert!(matches!(mask.shape, renamite_model::ShapeKind::Rect { .. }));

        history.redo(&mut world.pm()).unwrap();
        assert!(matches!(world.doc.nodes[id].kind, NodeKind::Shape(_)));
    }

    #[test]
    fn set_mask_inverted_undo_redo_roundtrips() {
        let mut world = World::new();
        let id = world.node();

        world.doc.nodes[id].kind = NodeKind::Mask(renamite_model::MaskProps {
            inverted: false,
            shape: renamite_model::ShapeKind::Path(Animated::new(
                renamite_geometry::VectorPath::default(),
            )),
        });

        let mut history = History::new();
        history
            .apply(
                &mut world.pm(),
                EditorCommand::SetMaskInverted { id, inverted: true },
            )
            .unwrap();
        history.commit();

        let NodeKind::Mask(mask) = &world.doc.nodes[id].kind else {
            panic!("expected mask");
        };
        assert!(mask.inverted);

        history.undo(&mut world.pm()).unwrap();
        let NodeKind::Mask(mask) = &world.doc.nodes[id].kind else {
            panic!("expected mask");
        };
        assert!(!mask.inverted);

        history.redo(&mut world.pm()).unwrap();
        let NodeKind::Mask(mask) = &world.doc.nodes[id].kind else {
            panic!("expected mask");
        };
        assert!(mask.inverted);
    }

    #[test]
    fn composition_range_edit_round_trips_and_coalesces() {
        let mut w = World::new();
        let mut h = History::new();
        let comp = w.doc.main;

        h.begin("Duration");
        h.apply(
            &mut w.pm(),
            EditorCommand::SetCompositionRange {
                comp,
                start: None,
                end: Some(Frame(240)),
            },
        )
        .unwrap();
        h.commit();
        assert_eq!(w.doc.compositions[comp].range, (Frame(0), Frame(240)));

        h.begin("Duration");
        h.apply(
            &mut w.pm(),
            EditorCommand::SetCompositionRange {
                comp,
                start: Some(Frame(5)),
                end: None,
            },
        )
        .unwrap();
        h.commit();
        assert_eq!(w.doc.compositions[comp].range, (Frame(5), Frame(240)));

        h.undo(&mut w.pm()).unwrap();
        assert_eq!(w.doc.compositions[comp].range, (Frame(0), Frame(240)));
        h.undo(&mut w.pm()).unwrap();
        assert_eq!(w.doc.compositions[comp].range, (Frame(0), Frame(180)));

        // Redo restores the edited range.
        h.redo(&mut w.pm()).unwrap();
        assert_eq!(w.doc.compositions[comp].range, (Frame(0), Frame(240)));

        let mut w2 = World::new();
        let comp2 = w2.doc.main;
        let mut h2 = History::new();
        for end in [Frame(200), Frame(240), Frame(300)] {
            h2.begin("Duration");
            h2.apply(
                &mut w2.pm(),
                EditorCommand::SetCompositionRange {
                    comp: comp2,
                    start: None,
                    end: Some(end),
                },
            )
            .unwrap();
            h2.commit();
        }
        assert_eq!(w2.doc.compositions[comp2].range, (Frame(0), Frame(300)));
        h2.undo(&mut w2.pm()).unwrap();
        assert_eq!(
            w2.doc.compositions[comp2].range,
            (Frame(0), Frame(180)),
            "live range scrub is a single undo step"
        );
    }

    #[test]
    fn insert_from_live_snapshot_does_not_import_stale_children() {
        // Simulates cut/copy: NodeTree built from a live node that still has
        // parent/children filled in the payload.
        let mut w = World::new();
        let parent = Parent::Comp(w.doc.main);

        let fill = w.doc.create_node(Node::new(
            "Fill",
            NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::solid(Color::BLACK),
                rule: renamite_model::FillRule::NonZero,
            }),
        ));
        let shape = w.doc.create_node(Node::new(
            "Shape",
            NodeKind::Shape(renamite_model::ShapeKind::Ellipse {
                pos: Animated::new(glam::DVec2::ZERO),
                size: Animated::new(glam::DVec2::new(10.0, 10.0)),
            }),
        ));
        let group = w.doc.create_node(Node::new("Group", NodeKind::Group));
        w.doc.attach(group, parent, 0).unwrap();
        w.doc.attach(shape, Parent::Node(group), 0).unwrap();
        w.doc.attach(fill, Parent::Node(group), 1).unwrap();

        // Snapshot like the UI clipboard: clone nodes WITH live topology.
        let snap_fill = w.doc.nodes.get(fill).unwrap().clone();
        let snap_shape = w.doc.nodes.get(shape).unwrap().clone();
        let snap_group = w.doc.nodes.get(group).unwrap().clone();
        assert_eq!(snap_group.children, vec![shape, fill]);

        let tree = NodeTree {
            node: snap_group,
            id: None,
            children: vec![
                NodeTree {
                    node: snap_shape,
                    id: None,
                    children: vec![],
                },
                NodeTree {
                    node: snap_fill,
                    id: None,
                    children: vec![],
                },
            ],
        };

        let mut h = History::new();
        let created = h
            .apply(
                &mut w.pm(),
                EditorCommand::InsertNode {
                    parent,
                    index: 0,
                    tree,
                },
            )
            .unwrap()
            .created
            .unwrap();
        h.commit();

        let kids = w.doc.nodes.get(created).unwrap().children.clone();
        assert_eq!(
            kids.len(),
            2,
            "pasted group must have exactly the new children, not old+new"
        );
        assert!(!kids.contains(&shape) && !kids.contains(&fill));
        assert_eq!(w.doc.nodes.get(kids[0]).unwrap().parent, Some(created));
        assert_eq!(w.doc.nodes.get(kids[1]).unwrap().parent, Some(created));
        assert!(w.doc.nodes.get(created).unwrap().parent.is_none());
    }

    #[test]
    fn begin_commits_previous_open_transaction() {
        let mut w = World::new();
        let id = w.node();
        let mut h = History::new();
        h.begin("First");
        h.apply(
            &mut w.pm(),
            EditorCommand::SetNodeName {
                id,
                name: "A".into(),
            },
        )
        .unwrap();
        // Second begin must not drop the first edit.
        h.begin("Second");
        h.apply(
            &mut w.pm(),
            EditorCommand::SetNodeName {
                id,
                name: "B".into(),
            },
        )
        .unwrap();
        h.commit();
        assert_eq!(w.doc.nodes[id].name, "B");
        h.undo(&mut w.pm()).unwrap();
        assert_eq!(w.doc.nodes[id].name, "A");
        h.undo(&mut w.pm()).unwrap();
        assert_eq!(w.doc.nodes[id].name, "n");
    }
}
