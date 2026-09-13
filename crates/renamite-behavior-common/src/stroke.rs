//! Pure command builders for stroke dash structure.

use renamite_animation::Animated;
use renamite_history::{EditorCommand, NodeTree};
use renamite_model::{
    AnimatedDash, Document, Node, NodeId, NodeKind, Parent, StrokeCap, StrokeJoin, StyleKind,
    StylePaint,
};

/// Prefer a stroke *after* the shape in sibling order, until the next
/// geometry-bearing sibling (same boundary as fill).
pub fn stroke_style_for_shape(doc: &Document, shape_id: NodeId) -> Option<NodeId> {
    let (parent, shape_index) = doc.locate(shape_id)?;
    let siblings: Vec<NodeId> = match parent {
        Parent::Comp(c) => doc.compositions.get(c)?.children.clone(),
        Parent::Node(n) => doc.nodes.get(n)?.children.clone(),
    };
    for &id in siblings.iter().skip(shape_index + 1) {
        if is_stroke(doc, id) {
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

fn is_stroke(doc: &Document, id: NodeId) -> bool {
    matches!(
        doc.nodes.get(id).map(|n| &n.kind),
        Some(NodeKind::Style(StyleKind::Stroke { .. }))
    )
}

pub fn stroke_dash(doc: &Document, id: NodeId) -> Option<&AnimatedDash> {
    match &doc.nodes.get(id)?.kind {
        NodeKind::Style(StyleKind::Stroke {
            dash: Some(dash), ..
        }) => Some(dash),

        _ => None,
    }
}

pub fn cmd_enable_stroke_dash(doc: &Document, id: NodeId) -> Option<EditorCommand> {
    let NodeKind::Style(StyleKind::Stroke { dash, .. }) = &doc.nodes.get(id)?.kind else {
        return None;
    };

    if dash.is_some() {
        return None;
    }

    Some(EditorCommand::SetStrokeDash {
        id,
        dash: Some(AnimatedDash {
            dashes: vec![Animated::new(12.0), Animated::new(8.0)],
            offset: Animated::new(0.0),
        }),
    })
}

pub fn cmd_disable_stroke_dash(doc: &Document, id: NodeId) -> Option<EditorCommand> {
    stroke_dash(doc, id)?;

    Some(EditorCommand::SetStrokeDash { id, dash: None })
}

pub fn cmd_add_stroke_dash_pair(doc: &Document, id: NodeId) -> Option<EditorCommand> {
    let mut dash = stroke_dash(doc, id)?.clone();

    dash.dashes.push(Animated::new(8.0));
    dash.dashes.push(Animated::new(4.0));

    Some(EditorCommand::SetStrokeDash {
        id,
        dash: Some(dash),
    })
}

pub fn cmd_remove_stroke_dash_pair(doc: &Document, id: NodeId) -> Option<EditorCommand> {
    let mut dash = stroke_dash(doc, id)?.clone();

    if dash.dashes.len() <= 2 {
        return None;
    }

    let new_len = dash.dashes.len().saturating_sub(2).max(2);
    dash.dashes.truncate(new_len);

    Some(EditorCommand::SetStrokeDash {
        id,
        dash: Some(dash),
    })
}

pub fn cmd_add_stroke_after(
    doc: &Document,
    shape_id: NodeId,
    paint: StylePaint,
    width: f64,
) -> Option<EditorCommand> {
    if stroke_style_for_shape(doc, shape_id).is_some() {
        return None;
    }
    let (parent, shape_index) = doc.locate(shape_id)?;
    let mut index = shape_index + 1;
    if let Some(fill) = super::fill::fill_style_for_shape(doc, shape_id)
        && let Some((f_parent, f_idx)) = doc.locate(fill)
        && f_parent == parent
        && f_idx >= shape_index
        && f_idx + 1 > index
    {
        index = f_idx + 1;
    }
    Some(EditorCommand::InsertNode {
        parent,
        index,
        tree: NodeTree::leaf(Node::new(
            "Stroke",
            NodeKind::Style(StyleKind::Stroke {
                paint,
                width: Animated::new(width.max(0.0)),
                cap: StrokeCap::Round,
                join: StrokeJoin::Round,
                miter_limit: Animated::new(4.0),
                dash: None,
            }),
        )),
    })
}

pub fn cmd_remove_stroke_for_shape(doc: &Document, shape_id: NodeId) -> Option<EditorCommand> {
    let stroke = stroke_style_for_shape(doc, shape_id)?;
    let (parent, shape_index) = doc.locate(shape_id)?;
    let (s_parent, s_idx) = doc.locate(stroke)?;
    if s_parent != parent || s_idx <= shape_index {
        return None;
    }
    Some(EditorCommand::RemoveNode { id: stroke })
}
