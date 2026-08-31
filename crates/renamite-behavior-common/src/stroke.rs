//! Pure command builders for stroke dash structure.

use renamite_animation::Animated;
use renamite_history::EditorCommand;
use renamite_model::{AnimatedDash, Document, NodeId, NodeKind, Parent, StyleKind};

/// Prefer a stroke *after* the shape in sibling order (same as fill).
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
    }
    for &id in &siblings {
        if is_stroke(doc, id) {
            return Some(id);
        }
    }
    renamite_model::stroke_style_for(doc, shape_id)
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

#[cfg(test)]
mod tests {
    use super::*;
    use renamite_model::{Color, Node, Parent, StrokeCap, StrokeJoin, StylePaint};

    fn stroke_doc() -> (Document, NodeId) {
        let mut doc = Document::empty();

        let id = doc.create_node(Node::new(
            "Stroke",
            NodeKind::Style(StyleKind::Stroke {
                paint: StylePaint::solid(Color::BLACK),
                width: Animated::new(4.0),
                cap: StrokeCap::Round,
                join: StrokeJoin::Round,
                dash: None,
            }),
        ));

        doc.attach(id, Parent::Comp(doc.main), 0).unwrap();

        (doc, id)
    }

    #[test]
    fn enable_creates_default_pair() {
        let (doc, id) = stroke_doc();

        let command = cmd_enable_stroke_dash(&doc, id).unwrap();

        let EditorCommand::SetStrokeDash {
            dash: Some(dash), ..
        } = command
        else {
            panic!("expected dash command");
        };

        assert_eq!(dash.dashes.len(), 2);
        assert_eq!(dash.dashes[0].base, 12.0);
        assert_eq!(dash.dashes[1].base, 8.0);
    }

    #[test]
    fn add_and_remove_pairs_preserve_existing_entries() {
        let (mut doc, id) = stroke_doc();

        let NodeKind::Style(StyleKind::Stroke { dash, .. }) = &mut doc.nodes[id].kind else {
            panic!();
        };

        *dash = Some(AnimatedDash {
            dashes: vec![Animated::new(10.0), Animated::new(5.0)],
            offset: Animated::new(2.0),
        });

        let add = cmd_add_stroke_dash_pair(&doc, id).unwrap();

        let EditorCommand::SetStrokeDash {
            dash: Some(added), ..
        } = add
        else {
            panic!();
        };

        assert_eq!(added.dashes.len(), 4);
        assert_eq!(added.dashes[0].base, 10.0);
        assert_eq!(added.offset.base, 2.0);
    }

    #[test]
    fn stroke_style_for_shape_finds_following_sibling() {
        use renamite_model::{Node, ShapeKind};
        let mut doc = Document::empty();
        let shape = doc.create_node(Node::new(
            "shape",
            NodeKind::Shape(ShapeKind::Ellipse {
                pos: Animated::new(glam::DVec2::ZERO),
                size: Animated::new(glam::DVec2::splat(10.0)),
            }),
        ));
        let stroke = doc.create_node(Node::new(
            "stroke",
            NodeKind::Style(StyleKind::Stroke {
                paint: StylePaint::solid(Color::BLACK),
                width: Animated::new(2.0),
                cap: StrokeCap::Butt,
                join: StrokeJoin::Miter,
                dash: None,
            }),
        ));
        doc.attach(shape, Parent::Comp(doc.main), 0).unwrap();
        doc.attach(stroke, Parent::Comp(doc.main), 1).unwrap();
        assert_eq!(stroke_style_for_shape(&doc, shape), Some(stroke));
        assert_eq!(renamite_model::stroke_style_for(&doc, shape), Some(stroke));
    }
}
