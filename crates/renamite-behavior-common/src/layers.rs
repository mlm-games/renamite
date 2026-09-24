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
    Mask,
    Use,
    Other,
}

impl LayerKind {
    pub fn from_node(k: &NodeKind) -> Self {
        match k {
            NodeKind::Group | NodeKind::Layer(_) => LayerKind::Group,
            NodeKind::Shape(_) => LayerKind::Shape,
            NodeKind::Style(_) => LayerKind::Style,
            NodeKind::Mask(_) => LayerKind::Mask,
            NodeKind::Use { .. } => LayerKind::Use,
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
    let Some(c) = doc.compositions.get(comp) else {
        return out;
    };
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
        if is_expandable(kind, !n.children.is_empty()) && expanded.contains(&id) {
            walk(doc, Parent::Node(id), &n.children, depth + 1, expanded, out);
        }
    }
}

fn is_expandable(kind: LayerKind, has_children: bool) -> bool {
    match kind {
        LayerKind::Group => true,
        LayerKind::Shape
        | LayerKind::Style
        | LayerKind::Mask
        | LayerKind::Use
        | LayerKind::Other => has_children,
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
    EditorCommand::MoveNode {
        id,
        new_parent,
        index,
    }
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
    // Prevent parenting a node under its own descendant - host should also
    // reject via is_ancestor check when as_child.
    if as_child && target.kind == LayerKind::Group {
        return Some(cmd_move(dragged, Parent::Node(target.id), usize::MAX));
    }
    // Shape containers can also hold nested style/modifier children.
    if as_child && target.kind == LayerKind::Shape {
        return Some(cmd_move(dragged, Parent::Node(target.id), usize::MAX));
    }
    // Sibling insert: refuse if target's parent is inside dragged (would cycle).
    if let Parent::Node(p) = target.parent
        && p == dragged
    {
        return None;
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
    let EditorCommand::MoveNode {
        id,
        new_parent,
        index,
    } = cmd
    else {
        return false;
    };
    let Some((old_parent, old_index)) = doc.locate(*id) else {
        return false;
    };
    if old_parent != *new_parent {
        return false;
    }
    let adjusted = if *index > old_index {
        *index - 1
    } else {
        *index
    };
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
    use renamite_model::{Node, NodeKind};

    fn group(doc: &mut Document, name: &str) -> NodeId {
        doc.create_node(Node::new(name, NodeKind::Group))
    }

    #[test]
    fn groups_expand_even_without_children() {
        assert!(is_expandable(LayerKind::Group, false));
        assert!(is_expandable(LayerKind::Group, true));
    }

    #[test]
    fn any_node_with_children_is_expandable() {
        for kind in [
            LayerKind::Shape,
            LayerKind::Style,
            LayerKind::Mask,
            LayerKind::Use,
            LayerKind::Other,
        ] {
            assert!(!is_expandable(kind, false));
            assert!(is_expandable(kind, true));
        }
    }

    #[test]
    fn flatten_shows_nested_style_children_when_expanded() {
        let mut doc = Document::empty();
        let main = doc.main;
        let shape = group(&mut doc, "shape");
        doc.attach(shape, Parent::Comp(main), 0).unwrap();
        let style = doc.create_node(Node::new(
            "fill",
            NodeKind::Style(renamite_model::StyleKind::Fill {
                paint: renamite_model::StylePaint::solid(renamite_model::Color::BLACK),
                rule: renamite_model::FillRule::NonZero,
            }),
        ));
        doc.attach(style, Parent::Node(shape), 0).unwrap();
        let collapsed = flatten_layers(&doc, main, &std::collections::HashSet::new());
        assert_eq!(collapsed.len(), 1);
        let mut expanded = std::collections::HashSet::new();
        expanded.insert(shape);
        let rows = flatten_layers(&doc, main, &expanded);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].id, style);
    }
}
