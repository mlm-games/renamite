//! Commands, transactions, undo/redo. Inverses are captured at apply time from
//! prior document state - never full document clones. RemoveNode is
//! detach-only (arena-stable NodeIds).

use renamite_animation::{Animated, EasingHandle, Frame, Interpolation};
use renamite_geometry::{AnchorEdit, VectorPath};
use renamite_model::{
    Document, KeyframeData, ModelError, Node, NodeId, Parent, PropMut, PropPath, Value,
};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

/// Node payload for creation. `id` is None until first apply, then filled so
/// redo re-attaches the SAME arena nodes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeTree {
    pub node: Node,
    pub id: Option<NodeId>,
    pub children: Vec<NodeTree>,
}

impl NodeTree {
    pub fn leaf(node: Node) -> Self { Self { node, id: None, children: Vec::new() } }
    pub fn with_children(node: Node, children: Vec<NodeTree>) -> Self {
        Self { node, id: None, children }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum EditorCommand {
    // structure
    InsertNode { parent: Parent, index: usize, tree: NodeTree },
    /// Undo-internal: re-attach an arena node that was detached.
    AttachNode { id: NodeId, parent: Parent, index: usize },
    /// Detach only - node stays in the arena for undo.
    RemoveNode { id: NodeId },
    MoveNode { id: NodeId, new_parent: Parent, index: usize },
    GroupNodes { ids: Vec<NodeId>, group: NodeId },
    SetNodeFlags { id: NodeId, visible: Option<bool>, locked: Option<bool> },

    // properties
    SetStatic { id: NodeId, prop: PropPath, value: Value },
    AddKeyframe { id: NodeId, prop: PropPath, frame: Frame, value: Value },
    RemoveKeyframe { id: NodeId, prop: PropPath, frame: Frame },
    RestoreKeyframe { id: NodeId, prop: PropPath, key: KeyframeData },
    MoveKeyframes { moves: Vec<KeyframeMove> },
    SetEasing {
        id: NodeId, prop: PropPath, frame: Frame,
        interpolation: Interpolation, ease_out: EasingHandle, ease_in: EasingHandle,
    },

    // path editing (applies to key at `frame` if Some, else to base)
    EditAnchors { id: NodeId, frame: Option<Frame>, edits: Vec<AnchorEdit> },
    ReversePath { id: NodeId },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyframeMove { pub id: NodeId, pub prop: PropPath, pub from: Frame, pub to: Frame }

#[derive(Clone, Debug, thiserror::Error)]
pub enum EditError {
    #[error(transparent)] Model(#[from] ModelError),
    #[error("path property missing on node")] NotAPath,
}

/// Result of a single apply (created root id surfaces for selection).
pub struct Applied { pub created: Option<NodeId> }

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AppliedTransaction {
    label: String,
    forward: Vec<EditorCommand>,
    /// inverse[i] undoes forward[i]; each may be several commands.
    inverse: Vec<Vec<EditorCommand>>,
}

#[derive(Default)]
pub struct History {
    undo: Vec<AppliedTransaction>,
    redo: Vec<AppliedTransaction>,
    open: Option<AppliedTransaction>,
}

impl History {
    pub fn new() -> Self { Self::default() }

    pub fn begin(&mut self, label: impl Into<String>) {
        self.open = Some(AppliedTransaction { label: label.into(), forward: Vec::new(), inverse: Vec::new() });
    }

    pub fn apply(&mut self, doc: &mut Document, mut cmd: EditorCommand) -> Result<Applied, EditError> {
        // Coalesce repeated live-drag edits so one drag = one inverse entry.
        if let Some(t) = &mut self.open {
            if let Some(last) = t.forward.last_mut() {
                if coalesce(last, &cmd) {
                    let created = apply_command(doc, &mut cmd)?;
                    *last = cmd;
                    return Ok(Applied { created: created.0 });
                }
            }
        }
        let (created, inverse) = apply_command(doc, &mut cmd)?;
        if let Some(t) = &mut self.open {
            t.forward.push(cmd);
            t.inverse.push(inverse);
        } else {
            self.undo.push(AppliedTransaction {
                label: String::new(), forward: vec![cmd], inverse: vec![inverse],
            });
            self.redo.clear();
        }
        Ok(Applied { created })
    }

    /// Close the open transaction and make it undoable.
    pub fn commit(&mut self) {
        if let Some(t) = self.open.take() {
            if !t.forward.is_empty() {
                self.undo.push(t);
                self.redo.clear();
            }
        }
    }

    /// Discard the open transaction, applying its inverses.
    pub fn cancel(&mut self, doc: &mut Document) -> Result<(), EditError> {
        if let Some(t) = self.open.take() {
            undo_transaction(doc, &t)?;
        }
        Ok(())
    }

    pub fn undo(&mut self, doc: &mut Document) -> Result<(), EditError> {
        if let Some(t) = self.undo.pop() {
            undo_transaction(doc, &t)?;
            self.redo.push(t);
        }
        Ok(())
    }

    pub fn redo(&mut self, doc: &mut Document) -> Result<(), EditError> {
        if let Some(t) = self.redo.pop() {
            redo_transaction(doc, &t)?;
            self.undo.push(t);
        }
        Ok(())
    }

    pub fn can_undo(&self) -> bool { !self.undo.is_empty() }
    pub fn can_redo(&self) -> bool { !self.redo.is_empty() }
}

/// Apply a command to `doc`, filling in InsertNode ids, and return the created
/// root (if any) plus the inverse commands captured from prior state.
fn apply_command(
    doc: &mut Document,
    cmd: &mut EditorCommand,
) -> Result<(Option<NodeId>, Vec<EditorCommand>), EditError> {
    use EditorCommand::*;
    match cmd {
        InsertNode { parent, index, tree } => {
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
            Ok((None, vec![AttachNode { id: *id, parent, index }]))
        }
        MoveNode { id, new_parent, index } => {
            let old = doc.locate(*id).ok_or(ModelError::NotAttached)?;
            doc.detach(*id)?;
            doc.attach(*id, *new_parent, *index)?;
            Ok((None, vec![MoveNode { id: *id, new_parent: old.0, index: old.1 }]))
        }
        GroupNodes { ids, group } => {
            if !doc.nodes.contains_key(*group) { return Err(ModelError::MissingNode.into()); }
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
                .map(|(id, parent, index)| MoveNode { id, new_parent: parent, index })
                .collect();
            Ok((None, inverse))
        }
        SetNodeFlags { id, visible, locked } => {
            let n = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            let old_visible = n.visible;
            let old_locked = n.locked;
            if let Some(v) = *visible { n.visible = v; }
            if let Some(l) = *locked { n.locked = l; }
            Ok((None, vec![SetNodeFlags {
                id: *id,
                visible: visible.is_some().then_some(old_visible),
                locked: locked.is_some().then_some(old_locked),
            }]))
        }
        SetStatic { id, prop, value } => {
            let old = doc.set_static(*id, prop, value)?;
            Ok((None, vec![SetStatic { id: *id, prop: prop.clone(), value: old }]))
        }
        AddKeyframe { id, prop, frame, value } => {
            let replaced = doc.add_keyframe(*id, prop, *frame, value)?;
            let inv = match replaced {
                Some(k) => vec![RestoreKeyframe { id: *id, prop: prop.clone(), key: k }],
                None => vec![RemoveKeyframe { id: *id, prop: prop.clone(), frame: *frame }],
            };
            Ok((None, inv))
        }
        RemoveKeyframe { id, prop, frame } => {
            let key = doc.remove_keyframe(*id, prop, *frame)?;
            Ok((None, vec![RestoreKeyframe { id: *id, prop: prop.clone(), key }]))
        }
        RestoreKeyframe { id, prop, key } => {
            doc.restore_keyframe(*id, prop, key)?;
            Ok((None, vec![RemoveKeyframe { id: *id, prop: prop.clone(), frame: key.frame }]))
        }
        MoveKeyframes { moves } => {
            let inv = moves.iter().map(|m| {
                doc.move_keyframe(m.id, &m.prop, m.from, m.to).expect("move validated");
                KeyframeMove { id: m.id, prop: m.prop.clone(), from: m.to, to: m.from }
            }).collect();
            Ok((None, vec![MoveKeyframes { moves: inv }]))
        }
        SetEasing { id, prop, frame, interpolation, ease_out, ease_in } => {
            let (oi, oo, oe) = doc.set_easing(*id, prop, *frame, *interpolation, *ease_out, *ease_in)?;
            Ok((None, vec![SetEasing {
                id: *id, prop: prop.clone(), frame: *frame,
                interpolation: oi, ease_out: oo, ease_in: oe,
            }]))
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
            Ok((None, vec![EditAnchors { id: *id, frame: *frame, edits: inv_edits }]))
        }
        ReversePath { id } => {
            let prop = PropPath::new("shape.path");
            let node = doc.nodes.get_mut(*id).ok_or(ModelError::MissingNode)?;
            match node.prop_mut(&prop) {
                Some(PropMut::Path(a)) => {
                    a.base.reverse();
                    for k in &mut a.keyframes { k.value.reverse(); }
                }
                _ => return Err(EditError::NotAPath),
            }
            Ok((None, vec![ReversePath { id: *id }]))
        }
    }
}

/// True if `new` continues the same logical edit as `last` (live drag). The
/// merged command replaces `last` in the transaction; its first inverse entry
/// (pre-drag state) is preserved.
fn coalesce(last: &mut EditorCommand, new: &EditorCommand) -> bool {
    use EditorCommand::*;
    match (last, new) {
        (SetStatic { id, prop, .. }, SetStatic { id: nid, prop: nprop, .. }) =>
            *id == *nid && *prop == *nprop,
        (AddKeyframe { id, prop, frame, .. }, AddKeyframe { id: nid, prop: nprop, frame: nframe, .. }) =>
            *id == *nid && *prop == *nprop && *frame == *nframe,
        (SetEasing { id, prop, frame, .. }, SetEasing { id: nid, prop: nprop, frame: nframe, .. }) =>
            *id == *nid && *prop == *nprop && *frame == *nframe,
        (EditAnchors { id, frame, .. }, EditAnchors { id: nid, frame: nframe, .. }) =>
            *id == *nid && *frame == *nframe,
        (MoveKeyframes { moves }, MoveKeyframes { moves: nmoves }) =>
            moves.len() == nmoves.len()
                && moves.iter().zip(nmoves.iter())
                    .all(|(a, b)| a.id == b.id && a.prop == b.prop && a.from == b.from),
        (SetNodeFlags { id, .. }, SetNodeFlags { id: nid, .. }) => *id == *nid,
        _ => false,
    }
}

fn undo_transaction(doc: &mut Document, t: &AppliedTransaction) -> Result<(), EditError> {
    for group in t.inverse.iter().rev() {
        for cmd in group.iter().rev() {
            let mut c = cmd.clone();
            apply_command(doc, &mut c)?;
        }
    }
    Ok(())
}

fn redo_transaction(doc: &mut Document, t: &AppliedTransaction) -> Result<(), EditError> {
    for cmd in &t.forward {
        let mut c = cmd.clone();
        apply_command(doc, &mut c)?;
    }
    Ok(())
}

/// Recursively create a tree's arena nodes once, filling `tree.id`; no-ops on
/// redo when ids are already filled. Children are attached to their parents.
fn ensure_tree(doc: &mut Document, tree: &mut NodeTree) -> Result<NodeId, ModelError> {
    if let Some(id) = tree.id { return Ok(id); }
    let mut child_ids = Vec::with_capacity(tree.children.len());
    for child in &mut tree.children {
        child_ids.push(ensure_tree(doc, child)?);
    }
    let id = doc.create_node(tree.node.clone());
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
                let i = a.keyframes.binary_search_by_key(&f, |k| k.frame)
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
        EditorCommand::AddKeyframe { id, prop: prop.clone(), frame: playhead, value }
    } else {
        EditorCommand::SetStatic { id, prop: prop.clone(), value }
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

#[derive(Clone, Copy, Debug)]
pub enum ToolId {
    Select,
    Transform,
    Pen,
    PathEdit,
    Rect,
    Ellipse,
    Star,
    Gradient,
    Fill,
}

#[cfg(test)]
mod tests {
    use super::*;
    use renamite_model::NodeKind;

    fn comp(doc: &Document) -> Parent { Parent::Comp(doc.main) }

    fn single(doc: &mut Document) -> NodeId {
        let id = doc.create_node(Node::new("x", NodeKind::Group));
        doc.attach(id, comp(doc), 0).unwrap();
        id
    }

    #[test]
    fn insert_undo_redo_is_arena_stable() {
        let mut doc = Document::empty();
        let mut h = History::new();
        let parent = comp(&doc);
        let created = h.apply(&mut doc, EditorCommand::InsertNode {
            parent, index: 0, tree: NodeTree::leaf(Node::new("rect", NodeKind::Group)),
        }).unwrap().created.unwrap();
        h.commit();
        assert!(doc.nodes.contains_key(created));
        assert!(doc.locate(created).is_some());
        h.undo(&mut doc).unwrap();
        assert!(doc.nodes.contains_key(created));   // still in arena
        assert!(doc.locate(created).is_none());     // just detached
        h.redo(&mut doc).unwrap();
        assert!(doc.locate(created).is_some());
        assert!(doc.nodes.contains_key(created));
    }

    #[test]
    fn remove_undo_reattaches() {
        let mut doc = Document::empty();
        let id = single(&mut doc);
        let mut h = History::new();
        h.apply(&mut doc, EditorCommand::RemoveNode { id }).unwrap();
        h.commit();
        assert!(doc.locate(id).is_none());
        assert!(doc.nodes.contains_key(id));
        h.undo(&mut doc).unwrap();
        assert_eq!(doc.locate(id), Some((comp(&doc), 0)));
        h.redo(&mut doc).unwrap();
        assert!(doc.locate(id).is_none());
    }

    #[test]
    fn set_static_undo_redo_restores() {
        let mut doc = Document::empty();
        let id = single(&mut doc);
        let mut h = History::new();
        let prop = PropPath::new("transform.position");
        h.apply(&mut doc, EditorCommand::SetStatic {
            id, prop: prop.clone(), value: Value::DVec2(glam::DVec2::new(10.0, 20.0)),
        }).unwrap();
        h.commit();
        assert_eq!(doc.get_static(id, &prop).unwrap(), Value::DVec2(glam::DVec2::new(10.0, 20.0)));
        h.undo(&mut doc).unwrap();
        assert_eq!(doc.get_static(id, &prop).unwrap(), Value::DVec2(glam::DVec2::ZERO));
        h.redo(&mut doc).unwrap();
        assert_eq!(doc.get_static(id, &prop).unwrap(), Value::DVec2(glam::DVec2::new(10.0, 20.0)));
    }

    #[test]
    fn drag_coalesces_to_one_undo_step() {
        let mut doc = Document::empty();
        let id = single(&mut doc);
        let mut h = History::new();
        let prop = PropPath::new("transform.position");
        h.begin("drag");
        for p in [glam::DVec2::new(1.0, 0.0), glam::DVec2::new(2.0, 0.0), glam::DVec2::new(3.0, 0.0)] {
            h.apply(&mut doc, EditorCommand::SetStatic {
                id, prop: prop.clone(), value: Value::DVec2(p),
            }).unwrap();
        }
        h.commit();
        assert!(h.can_undo());
        h.undo(&mut doc).unwrap();
        assert!(!h.can_undo());                    // one drag = one undo
        assert_eq!(doc.get_static(id, &prop).unwrap(), Value::DVec2(glam::DVec2::ZERO));
        h.redo(&mut doc).unwrap();
        assert_eq!(doc.get_static(id, &prop).unwrap(), Value::DVec2(glam::DVec2::new(3.0, 0.0)));
    }

    #[test]
    fn add_remove_keyframe_undo() {
        let mut doc = Document::empty();
        let id = single(&mut doc);
        let mut h = History::new();
        let prop = PropPath::new("transform.position");
        let v = Value::DVec2(glam::DVec2::new(5.0, 5.0));
        h.apply(&mut doc, EditorCommand::AddKeyframe {
            id, prop: prop.clone(), frame: Frame(10), value: v,
        }).unwrap();
        h.apply(&mut doc, EditorCommand::RemoveKeyframe {
            id, prop: prop.clone(), frame: Frame(10),
        }).unwrap();
        h.commit();
        assert!(doc.keyframe_data(id, &prop, Frame(10)).is_none());
        h.undo(&mut doc).unwrap();
        assert!(doc.keyframe_data(id, &prop, Frame(10)).is_some());
        h.undo(&mut doc).unwrap();
        assert!(doc.keyframe_data(id, &prop, Frame(10)).is_none());
    }
}