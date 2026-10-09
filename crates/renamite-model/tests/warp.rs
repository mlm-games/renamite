use glam::DVec2;
use renamite_animation::{Animated, AnimatedTransform};
use renamite_geometry::KurboShape;
use renamite_model::{
    Color, Document, FillRule, ModifierKind, Node, NodeKind, ShapeKind, StyleKind, StylePaint,
    WarpPin, WarpSpec, evaluate,
};

/// A group holding a rect, its fill and a warp with one pin, dragged by `drag`.
fn warped_document(drag: f64) -> Document {
    let mut document = Document::empty();
    let group = document.create_node(Node::new("group", NodeKind::Group));
    let rect = document.create_node(Node::new(
        "rect",
        NodeKind::Shape(ShapeKind::Rect {
            pos: Animated::new(DVec2::ZERO),
            size: Animated::new(DVec2::splat(120.0)),
            rounded: Animated::new(0.0),
        }),
    ));
    let fill = document.create_node(Node::new(
        "fill",
        NodeKind::Style(StyleKind::Fill {
            paint: StylePaint::solid(Color::BLACK),
            rule: FillRule::NonZero,
        }),
    ));
    let warp = document.create_node(Node::new(
        "warp",
        NodeKind::Modifier(ModifierKind::Warp(WarpSpec {
            spacing: Animated::new(30.0),
            pins: vec![WarpPin {
                rest: DVec2::splat(60.0),
                at: Animated::new(DVec2::new(60.0 + drag, 60.0)),
                angle: None,
            }],
        })),
    ));
    document.nodes.get_mut(group).unwrap().children = vec![rect, fill, warp];
    document.nodes.get_mut(rect).unwrap().transform = AnimatedTransform::identity();
    document
        .compositions
        .get_mut(document.main)
        .unwrap()
        .children = vec![group];
    document
}

/// The same group with the warp's pins taken off, so the warp has nothing to do.
fn with_pins_cleared(mut document: Document) -> Document {
    let group = document.compositions.get(document.main).unwrap().children[0];
    let children = document.nodes.get(group).unwrap().children.clone();
    let node = *children
        .iter()
        .find(|id| {
            matches!(
                document.nodes.get(**id).map(|n| &n.kind),
                Some(NodeKind::Modifier(ModifierKind::Warp(_)))
            )
        })
        .unwrap();
    if let NodeKind::Modifier(ModifierKind::Warp(spec)) =
        &mut document.nodes.get_mut(node).unwrap().kind
    {
        spec.pins.clear();
    }
    document
}

fn path_bounds(document: &Document, frame: f64) -> kurbo::Rect {
    let scene = evaluate(document, document.main, frame);
    scene
        .items
        .iter()
        .map(|item| item.path.bounding_box())
        .reduce(|a, b| a.union(b))
        .unwrap_or_default()
}

#[test]
fn a_dragged_pin_carries_the_art_with_it() {
    let still = path_bounds(&warped_document(0.0), 0.0);
    let dragged = path_bounds(&warped_document(40.0), 0.0);
    assert!(
        dragged.x1 > still.x1 + 5.0,
        "a pin dragging right should pull the art right: {still:?} -> {dragged:?}"
    );
}

#[test]
fn a_pin_that_has_not_moved_changes_nothing() {
    let document = warped_document(0.0);
    let inert = with_pins_cleared(document.clone());
    let (with_pin, without_pins) = (path_bounds(&document, 0.0), path_bounds(&inert, 0.0));
    let moved = (with_pin.x0 - without_pins.x0)
        .abs()
        .max((with_pin.x1 - without_pins.x1).abs());
    assert!(
        moved < 1e-6,
        "an unmoved pin should leave the art exactly as it was: {moved}"
    );
}

#[test]
fn the_warp_moves_per_frame() {
    let document = warped_document(40.0);
    let early = path_bounds(&document, 0.0);
    let late = path_bounds(&document, 1.0);
    let (a, b) = (early.x1, late.x1);
    assert!(
        (a - b).abs() < 1e-6,
        "the same drag at both frames: {a} vs {b}"
    );
}

#[test]
fn no_pins_leaves_the_modifier_inert() {
    let document = with_pins_cleared(warped_document(40.0));
    let (start, end) = (path_bounds(&document, 0.0), path_bounds(&document, 1.0));
    let moved = (start.x0 - end.x0).abs().max((start.x1 - end.x1).abs());
    assert!(
        moved < 1e-6 && start.width() > 0.0,
        "without pins the warp should not move anything: {moved}"
    );
}
