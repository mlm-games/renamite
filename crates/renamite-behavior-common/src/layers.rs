//! Pure layer-list helpers: tree flatten, drop targets, command builders.

use renamite_history::{EditorCommand, SelectionChange};
use renamite_model::{CompId, Document, NodeId, NodeKind, Parent};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerRow {
    pub id: NodeId,
    pub name: String,
    pub depth: usize,
    pub visible: bool,
    pub locked: bool,
    pub kind: LayerKind,
    pub child_count: usize,
    /// Index among siblings in parent.children / comp.children.
    pub sibling_index: usize,
    pub parent: Parent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerKind {
    Group,
    Shape,
    Style,
    Other,
}

impl LayerKind {
    pub fn from_node(k: &NodeKind) -> Self {
        match k {
            NodeKind::Group | NodeKind::Layer(_) => LayerKind::Group,
            NodeKind::Shape(_) => LayerKind::Shape,
            NodeKind::Style(_) => LayerKind::Style,
            _ => LayerKind::Other,
        }
    }
}

/// Flatten composition children depth-first. `expanded` controls whether
/// a group's children are included. z-order: index 0 = top of stack
/// (already Glaxnimate/list order in your model).
pub fn flatten_layers(
    doc: &Document,
    comp: CompId,
    expanded: &std::collections::HashSet<NodeId>,
) -> Vec<LayerRow> {
    let mut out = Vec::new();
    let Some(c) = doc.compositions.get(comp) else { return out };
    walk(doc, Parent::Comp(comp), &c.children, 0, expanded, &mut out);
    out
}

fn walk(
    doc: &Document,
    parent: Parent,
    children: &[NodeId],
    depth: usize,
    expanded: &std::collections::HashSet<NodeId>,
    out: &mut Vec<LayerRow>,
) {
    for (sibling_index, &id) in children.iter().enumerate() {
        let Some(n) = doc.nodes.get(id) else { continue };
        let kind = LayerKind::from_node(&n.kind);
        out.push(LayerRow {
            id,
            name: n.name.clone(),
            depth,
            visible: n.visible,
            locked: n.locked,
            kind,
            child_count: n.children.len(),
            sibling_index,
            parent,
        });
        if kind == LayerKind::Group && expanded.contains(&id) && !n.children.is_empty() {
            walk(doc, Parent::Node(id), &n.children, depth + 1, expanded, out);
        }
    }
}

pub fn cmd_toggle_visible(id: NodeId, currently_visible: bool) -> EditorCommand {
    EditorCommand::SetNodeFlags {
        id,
        visible: Some(!currently_visible),
        locked: None,
    }
}

pub fn cmd_toggle_locked(id: NodeId, currently_locked: bool) -> EditorCommand {
    EditorCommand::SetNodeFlags {
        id,
        visible: None,
        locked: Some(!currently_locked),
    }
}

pub fn cmd_rename(id: NodeId, name: String) -> EditorCommand {
    EditorCommand::SetNodeName { id, name }
}

/// Reorder: move `id` to `new_parent` at `index` (clamped by apply).
pub fn cmd_move(id: NodeId, new_parent: Parent, index: usize) -> EditorCommand {
    EditorCommand::MoveNode { id, new_parent, index }
}

/// Drop `dragged` onto `target` row.
/// - `as_child`: if target is a group and drop is on the right half / indent zone,
///   become last child of target.
/// - else: insert among target's siblings, before target if `before`, else after.
pub fn drop_command(
    dragged: NodeId,
    target: &LayerRow,
    before: bool,
    as_child: bool,
) -> Option<EditorCommand> {
    if dragged == target.id {
        return None;
    }
    // Prevent parenting a node under its own descendant — host should also
    // reject via is_ancestor check when as_child.
    if as_child && target.kind == LayerKind::Group {
        return Some(cmd_move(dragged, Parent::Node(target.id), usize::MAX));
    }
    let index = if before {
        target.sibling_index
    } else {
        target.sibling_index + 1
    };
    Some(cmd_move(dragged, target.parent, index))
}

pub fn is_ancestor(doc: &Document, ancestor: NodeId, mut node: NodeId) -> bool {
    while let Some(n) = doc.nodes.get(node) {
        match n.parent {
            Some(p) if p == ancestor => return true,
            Some(p) => node = p,
            None => return false,
        }
    }
    false
}

/// True if a `MoveNode` command leaves the node in its current position.
/// `MoveNode` detaches then attaches (index clamped to the post-detach
/// length), so indices above the removed slot shift down by one.
pub fn move_is_noop(doc: &Document, cmd: &EditorCommand) -> bool {
    let EditorCommand::MoveNode { id, new_parent, index } = cmd else {
        return false;
    };
    let Some((old_parent, old_index)) = doc.locate(*id) else {
        return false;
    };
    if old_parent != *new_parent {
        return false;
    }
    let adjusted = if *index > old_index { *index - 1 } else { *index };
    adjusted == old_index
}

/// Selection helpers for the panel.
pub fn select_only(id: NodeId) -> SelectionChange {
    SelectionChange::Set(vec![id])
}
pub fn toggle_in_selection(id: NodeId) -> SelectionChange {
    SelectionChange::Toggle(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use renamite_animation::Animated;
    use renamite_model::{Node, ShapeKind};
    use glam::DVec2;

    fn tree() -> (Document, NodeId, NodeId, NodeId) {
        // comp: [group, lone]
        // group children: [child]
        let mut doc = Document::empty();
        let c = doc.main;
        let group = doc.create_node(Node::new("G", NodeKind::Group));
        let child = doc.create_node(Node::new("C", NodeKind::Shape(ShapeKind::Ellipse {
            pos: Animated::new(DVec2::ZERO), size: Animated::new(DVec2::ONE),
        })));
        let lone = doc.create_node(Node::new("L", NodeKind::Group));
        doc.attach(group, Parent::Comp(c), 0).unwrap();
        doc.attach(lone, Parent::Comp(c), 1).unwrap();
        doc.attach(child, Parent::Node(group), 0).unwrap();
        (doc, group, child, lone)
    }

    #[test]
    fn flatten_hides_collapsed_children() {
        let (doc, group, _child, lone) = tree();
        let empty = std::collections::HashSet::new();
        let rows = flatten_layers(&doc, doc.main, &empty);
        assert_eq!(rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["G", "L"]);
        assert_eq!(rows[0].id, group);
        assert_eq!(rows[1].id, lone);
        assert_eq!(rows[0].child_count, 1);
    }

    #[test]
    fn flatten_expands_group() {
        let (doc, group, child, _) = tree();
        let mut exp = std::collections::HashSet::new();
        exp.insert(group);
        let rows = flatten_layers(&doc, doc.main, &exp);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].id, child);
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[1].parent, Parent::Node(group));
    }

    #[test]
    fn drop_before_sibling() {
        let (doc, group, _, lone) = tree();
        let rows = flatten_layers(&doc, doc.main, &Default::default());
        let target = &rows[0]; // G
        let cmd = drop_command(lone, target, true, false).unwrap();
        match cmd {
            EditorCommand::MoveNode { id, new_parent, index } => {
                assert_eq!(id, lone);
                assert_eq!(new_parent, Parent::Comp(doc.main));
                assert_eq!(index, 0);
            }
            _ => panic!(),
        }
        let _ = group;
    }

    #[test]
    fn drop_into_group() {
        let (doc, group, _, lone) = tree();
        let rows = flatten_layers(&doc, doc.main, &Default::default());
        let g = rows.iter().find(|r| r.id == group).unwrap();
        let cmd = drop_command(lone, g, false, true).unwrap();
        match cmd {
            EditorCommand::MoveNode { new_parent, .. } => {
                assert_eq!(new_parent, Parent::Node(group));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn is_ancestor_detects_cycle() {
        let (doc, group, child, _) = tree();
        assert!(is_ancestor(&doc, group, child));
        assert!(!is_ancestor(&doc, child, group));
    }

    #[test]
    fn move_to_same_slot_is_noop() {
        let (doc, group, child, lone) = tree();
        // Dropping a row onto itself yields no command at all.
        let rows = flatten_layers(&doc, doc.main, &Default::default());
        let lone_row = rows.iter().find(|r| r.id == lone).unwrap();
        assert!(drop_command(lone, lone_row, true, false).is_none());
        // Moving lone before G (index 0) really moves it.
        let g = rows.iter().find(|r| r.id == group).unwrap();
        let cmd = drop_command(lone, g, true, false).unwrap();
        assert!(!move_is_noop(&doc, &cmd));
        // Dragging a child within its group "after" its sibling lands it one
        // slot later — a real move.
        let mut exp = std::collections::HashSet::new();
        exp.insert(group);
        let rows = flatten_layers(&doc, doc.main, &exp);
        let g_row = rows.iter().find(|r| r.id == group).unwrap();
        let cmd = drop_command(child, g_row, true, false).unwrap();
        match cmd {
            EditorCommand::MoveNode { id, new_parent, index } => {
                assert_eq!(id, child);
                assert_eq!(new_parent, Parent::Comp(doc.main));
                assert_eq!(index, 0);
            }
            _ => panic!(),
        }
        assert!(!move_is_noop(&doc, &cmd));
    }
}
