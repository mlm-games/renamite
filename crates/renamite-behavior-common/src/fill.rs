//! Helpers for finding and editing the style node that paints a shape.

use renamite_history::{EditorCommand, NodeTree};
use renamite_model::{Document, FillRule, Node, NodeId, NodeKind, Parent, StyleKind, StylePaint};

/// Find the Fill style that paints `shape_id`.
pub fn fill_style_for_shape(doc: &Document, shape_id: NodeId) -> Option<NodeId> {
    let (parent, shape_index) = doc.locate(shape_id)?;
    let siblings: Vec<NodeId> = match parent {
        Parent::Comp(c) => doc.compositions.get(c)?.children.clone(),
        Parent::Node(n) => doc.nodes.get(n)?.children.clone(),
    };

    for &id in siblings.iter().skip(shape_index + 1) {
        if is_fill(doc, id) {
            return Some(id);
        }
        if is_style_stack_boundary(doc, id) {
            break;
        }
    }
    None
}

fn is_style_stack_boundary(doc: &Document, id: NodeId) -> bool {
    matches!(
        doc.nodes.get(id).map(|n| &n.kind),
        Some(
            NodeKind::Shape(_)
                | NodeKind::Text(_)
                | NodeKind::Image(_)
                | NodeKind::Group
                | NodeKind::Layer(_)
                | NodeKind::Precomp { .. }
                | NodeKind::Mask(_)
        )
    )
}

fn is_fill(doc: &Document, id: NodeId) -> bool {
    matches!(
        doc.nodes.get(id).map(|n| &n.kind),
        Some(NodeKind::Style(StyleKind::Fill { .. }))
    )
}

/// Command to replace a fill node's paint with `paint`.
pub fn cmd_set_fill_paint(
    doc: &Document,
    fill_id: NodeId,
    paint: StylePaint,
) -> Option<EditorCommand> {
    if !is_fill(doc, fill_id) {
        return None;
    }
    Some(EditorCommand::SetPaint { id: fill_id, paint })
}

/// Command to set the fill style that paints `shape_id`.
pub fn cmd_fill_shape(
    doc: &Document,
    shape_id: NodeId,
    paint: StylePaint,
) -> Option<EditorCommand> {
    let fill = fill_style_for_shape(doc, shape_id)?;
    cmd_set_fill_paint(doc, fill, paint)
}

fn sibling_insert_index(doc: &Document, shape_id: NodeId) -> Option<(Parent, usize)> {
    let (parent, shape_index) = doc.locate(shape_id)?;
    Some((parent, shape_index + 1))
}

/// Insert a Fill after the shape (or after existing styles). None if a fill already paints it.
pub fn cmd_add_fill_after(
    doc: &Document,
    shape_id: NodeId,
    paint: StylePaint,
) -> Option<EditorCommand> {
    if fill_style_for_shape(doc, shape_id).is_some() {
        return None;
    }
    let (parent, index) = sibling_insert_index(doc, shape_id)?;
    Some(EditorCommand::InsertNode {
        parent,
        index,
        tree: NodeTree::leaf(Node::new(
            "Fill",
            NodeKind::Style(StyleKind::Fill {
                paint,
                rule: FillRule::NonZero,
            }),
        )),
    })
}

/// Detach the fill that paints `shape_id` (if any). Inverse is AttachNode via history.
pub fn cmd_remove_fill_for_shape(doc: &Document, shape_id: NodeId) -> Option<EditorCommand> {
    let fill = fill_style_for_shape(doc, shape_id)?;
    let (parent, shape_index) = doc.locate(shape_id)?;
    let (f_parent, f_idx) = doc.locate(fill)?;
    if f_parent != parent || f_idx <= shape_index {
        return None;
    }
    Some(EditorCommand::RemoveNode { id: fill })
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::DVec2;
    use renamite_animation::Animated;
    use renamite_model::{Color, Document, FillRule, Node, ShapeKind};

    fn doc_shape_fill() -> (Document, NodeId, NodeId) {
        let mut doc = Document::empty();
        let comp = doc.main;
        let shape = doc.create_node(Node::new(
            "shape",
            NodeKind::Shape(ShapeKind::Ellipse {
                pos: Animated::new(DVec2::ZERO),
                size: Animated::new(DVec2::splat(100.0)),
            }),
        ));
        let fill = doc.create_node(Node::new(
            "fill",
            NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::solid(Color::BLACK),
                rule: FillRule::NonZero,
            }),
        ));
        doc.attach(shape, Parent::Comp(comp), 0).unwrap();
        doc.attach(fill, Parent::Comp(comp), 1).unwrap();
        (doc, shape, fill)
    }

    #[test]
    fn finds_following_fill_sibling() {
        let (doc, shape, fill) = doc_shape_fill();
        assert_eq!(fill_style_for_shape(&doc, shape), Some(fill));
    }

    #[test]
    fn command_rejects_non_fill() {
        let (doc, shape, _) = doc_shape_fill();
        assert!(cmd_set_fill_paint(&doc, shape, StylePaint::solid(Color::WHITE)).is_none());
    }

    #[test]
    fn command_sets_shape_fill() {
        let (doc, shape, fill) = doc_shape_fill();
        let cmd = cmd_fill_shape(&doc, shape, StylePaint::solid(Color::WHITE)).unwrap();
        assert!(matches!(cmd, EditorCommand::SetPaint { id, .. } if id == fill));
    }
}
