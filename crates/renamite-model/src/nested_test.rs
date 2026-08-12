#[test]
fn nested_fill_under_shape_renders() {
    use renamite_animation::Animated;
    use renamite_model::*;
    let mut doc = Document::empty();
    let shape = doc.create_node(Node::new(
        "e",
        NodeKind::Shape(ShapeKind::Ellipse {
            pos: Animated::new(glam::DVec2::ZERO),
            size: Animated::new(glam::DVec2::splat(100.0)),
        }),
    ));
    let fill = doc.create_node(Node::new(
        "f",
        NodeKind::Style(StyleKind::Fill {
            paint: StylePaint::solid(Color::BLACK),
            rule: FillRule::NonZero,
        }),
    ));
    doc.attach(fill, Parent::Node(shape), 0).unwrap();
    doc.attach(shape, Parent::Comp(doc.main), 0).unwrap();
    let scene = evaluate(&doc, doc.main, 0.0);
    println!("items={}", scene.items.len());
    assert_eq!(scene.items.len(), 1);
}
