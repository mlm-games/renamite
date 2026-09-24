use glam::DVec2;
use kurbo::PathEl;
use renamite_animation::{Animated, AnimatedTransform};
use renamite_model::{
    Color, Document, FillRule, Node, NodeKind, Parent, ShapeKind, StyleKind, StylePaint,
};

#[test]
fn use_of_group_applies_source_transform_and_opacity() {
    let mut document = Document::empty();
    let source = document.create_node(Node::new("source", NodeKind::Group));
    document.nodes.get_mut(source).unwrap().transform = AnimatedTransform {
        position: Animated::new(DVec2::new(100.0, 0.0)),
        ..AnimatedTransform::identity()
    };
    document.nodes.get_mut(source).unwrap().opacity = Animated::new(0.5);
    let shape = document.create_node(Node::new(
        "shape",
        NodeKind::Shape(ShapeKind::Rect {
            pos: Animated::new(DVec2::ZERO),
            size: Animated::new(DVec2::splat(10.0)),
            rounded: Animated::new(0.0),
        }),
    ));
    let style = document.create_node(Node::new(
        "fill",
        NodeKind::Style(StyleKind::Fill {
            paint: StylePaint::solid(Color::BLACK),
            rule: FillRule::NonZero,
        }),
    ));
    document.attach(shape, Parent::Node(source), 0).unwrap();
    document.attach(style, Parent::Node(source), 1).unwrap();
    document
        .attach(source, Parent::Comp(document.main), 0)
        .unwrap();

    let use_node = document.create_node(Node::new("use", NodeKind::Use { target: source }));
    document.nodes.get_mut(use_node).unwrap().transform = AnimatedTransform {
        position: Animated::new(DVec2::new(200.0, 0.0)),
        ..AnimatedTransform::identity()
    };
    document
        .attach(use_node, Parent::Comp(document.main), 1)
        .unwrap();

    let scene = renamite_model::evaluate(&document, document.main, 0.0);
    let mut positions = scene
        .items
        .iter()
        .filter(|item| item.node == shape)
        .filter_map(|item| {
            item.path
                .elements()
                .iter()
                .find_map(|element| match element {
                    PathEl::MoveTo(point) => Some(point.x),
                    _ => None,
                })
        })
        .collect::<Vec<_>>();
    positions.sort_by(f64::total_cmp);
    assert_eq!(positions, vec![95.0, 295.0]);
    assert!(
        scene
            .items
            .iter()
            .filter(|item| item.node == shape)
            .all(|item| (item.opacity - 0.5).abs() < 1e-9)
    );
}
