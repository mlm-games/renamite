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
    use renamite_animation::Animated;
    use renamite_model::{Color, FillRule, ShapeKind};

    fn rect(doc: &mut Document, name: &str) -> NodeId {
        doc.create_node(Node::new(
            name,
            NodeKind::Shape(ShapeKind::Rect {
                pos: Animated::new(glam::DVec2::ZERO),
                size: Animated::new(glam::DVec2::splat(10.0)),
                rounded: Animated::new(0.0),
            }),
        ))
    }

    fn fill(doc: &mut Document, name: &str) -> NodeId {
        doc.create_node(Node::new(
            name,
            NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::solid(Color::BLACK),
                rule: FillRule::NonZero,
            }),
        ))
    }

    #[test]
    fn style_stack_uses_first_fill() {
        let mut doc = Document::empty();
        let main = doc.main;
        let shape = rect(&mut doc, "shape");
        let fill_a = fill(&mut doc, "a");
        let fill_b = fill(&mut doc, "b");
        let other = rect(&mut doc, "other");
        doc.attach(shape, Parent::Comp(main), 0).unwrap();
        doc.attach(fill_a, Parent::Comp(main), 1).unwrap();
        doc.attach(fill_b, Parent::Comp(main), 2).unwrap();
        doc.attach(other, Parent::Comp(main), 3).unwrap();
        assert_eq!(fill_style_for_shape(&doc, shape), Some(fill_a));
    }
}
