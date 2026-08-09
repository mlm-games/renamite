mod common;

use renamite_animation::{Animated, Frame};
use renamite_geometry::KurboShape;
use renamite_io_svg::{export_frame, export_with_report, import};
use renamite_model::{
    Color, Document, FillRule, Node, NodeId, NodeKind, Parent, StyleKind, StylePaint,
};
use renamite_model::{ShapeKind, evaluate};

fn usvg_options() -> usvg::Options<'static> {
    let mut options = usvg::Options::default();
    options
        .fontdb_mut()
        .load_font_data(renamite_text::default_font_bytes().to_vec());
    options
}

#[test]
fn export_reparses_with_usvg() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
        <rect x="10" y="10" width="50" height="30" fill="#ff0000"/>
    </svg>"##;
    let doc = import(svg.as_bytes()).unwrap();
    let out = export_frame(&doc, doc.main, 0.0).unwrap();
    assert!(out.contains("<svg"));
    let reparsed = usvg::Tree::from_data(out.as_bytes(), &usvg_options()).unwrap();
    assert_eq!(reparsed.size().width(), 100.0);
    assert_eq!(reparsed.size().height(), 100.0);
}

#[test]
fn export_roundtrip_preserves_item_count() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
        <rect x="0" y="0" width="50" height="50" fill="#ff0000"/>
        <circle cx="75" cy="75" r="20" fill="#00ff00"/>
        <path d="M0 90h100" fill="none" stroke="#0000ff" stroke-width="2"/>
    </svg>"##;
    let doc = import(svg.as_bytes()).unwrap();
    let before = evaluate(&doc, doc.main, 0.0);
    let out = export_frame(&doc, doc.main, 0.0).unwrap();
    let doc2 = import(out.as_bytes()).unwrap();
    let after = evaluate(&doc2, doc2.main, 0.0);
    assert_eq!(after.items.len(), before.items.len());
}

#[test]
fn export_emits_gradient_defs() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
        <defs>
            <linearGradient id="g" x1="0" y1="0" x2="100" y2="0">
                <stop offset="0" stop-color="#ff0000"/>
                <stop offset="1" stop-color="#0000ff"/>
            </linearGradient>
        </defs>
        <rect x="0" y="0" width="100" height="50" fill="url(#g)"/>
    </svg>"##;
    let doc = import(svg.as_bytes()).unwrap();
    let out = export_frame(&doc, doc.main, 0.0).unwrap();
    assert!(out.contains("<linearGradient"));
    assert!(out.contains("gradientUnits=\"userSpaceOnUse\""));
    assert!(out.contains("fill=\"url(#grad0)\""));
    assert!(out.contains("<stop offset=\"0\" stop-color=\"#FF0000\""));
    let _ = usvg::Tree::from_data(out.as_bytes(), &usvg_options()).unwrap();
}

#[test]
fn export_emits_stroke_dash_attributes() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
        <rect x="10" y="10" width="80" height="40" fill="none" stroke="#000000"
              stroke-width="4" stroke-linecap="round" stroke-linejoin="bevel"
              stroke-dasharray="8 4" stroke-dashoffset="2"/>
    </svg>"##;
    let doc = import(svg.as_bytes()).unwrap();
    let out = export_frame(&doc, doc.main, 0.0).unwrap();
    assert!(out.contains("fill=\"none\""));
    assert!(out.contains("stroke=\"#000000\""));
    assert!(out.contains("stroke-width=\"4\""));
    assert!(out.contains("stroke-linecap=\"round\""));
    assert!(out.contains("stroke-linejoin=\"bevel\""));
    assert!(out.contains("stroke-dasharray=\"8 4\""));
    assert!(out.contains("stroke-dashoffset=\"2\""));
}

#[test]
fn export_emits_clip_defs() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
        <defs>
            <clipPath id="c"><rect x="0" y="0" width="50" height="50"/></clipPath>
        </defs>
        <g clip-path="url(#c)">
            <rect x="0" y="0" width="100" height="100" fill="#000000"/>
        </g>
    </svg>"##;
    let doc = import(svg.as_bytes()).unwrap();
    let out = export_frame(&doc, doc.main, 0.0).unwrap();
    assert!(out.contains("<clipPath id=\"clip0\">"));
    assert!(out.contains("clip-path=\"url(#clip0)\""));
    let _ = usvg::Tree::from_data(out.as_bytes(), &usvg_options()).unwrap();
}

#[test]
fn export_image_uses_data_uri() {
    use base64::Engine as _;

    let mut png = Vec::new();
    let img = image::RgbaImage::from_fn(2, 2, |_, _| image::Rgba([255, 0, 0, 255]));
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
            <image x="0" y="0" width="2" height="2" href="data:image/png;base64,{b64}"/>
        </svg>"##
    );
    let doc = import(svg.as_bytes()).unwrap();
    let out = export_frame(&doc, doc.main, 0.0).unwrap();
    assert!(out.contains("data:image/png;base64,"));
    let _ = usvg::Tree::from_data(out.as_bytes(), &usvg_options()).unwrap();
}

#[test]
fn export_is_frame_snapshot() {
    let mut doc = Document::empty();
    let comp = doc.main;
    let group = doc.create_node(Node::new("G", NodeKind::Group));
    let mut pos = Animated::new(glam::DVec2::new(0.0, 0.0));
    pos.set_key(Frame(0), glam::DVec2::new(0.0, 0.0));
    pos.set_key(Frame(60), glam::DVec2::new(50.0, 50.0));
    let rect = doc.create_node(Node::new(
        "Rect",
        NodeKind::Shape(ShapeKind::Rect {
            pos,
            size: Animated::new(glam::DVec2::new(100.0, 100.0)),
            rounded: Animated::new(0.0),
        }),
    ));
    let fill = doc.create_node(Node::new(
        "Fill",
        NodeKind::Style(StyleKind::Fill {
            paint: StylePaint::solid(Color::rgba(1.0, 0.0, 0.0, 1.0)),
            rule: FillRule::NonZero,
        }),
    ));
    doc.attach(rect, Parent::Node(group), 0).unwrap();
    doc.attach(fill, Parent::Node(group), 1).unwrap();
    doc.attach(group, Parent::Comp(comp), 0).unwrap();

    let out = export_frame(&doc, comp, 30.0).unwrap();
    let doc2 = import(out.as_bytes()).unwrap();
    let scene = evaluate(&doc2, doc2.main, 0.0);
    assert_eq!(scene.items.len(), 1);
    let bounds = scene.items[0].path.bounding_box();
    assert!((bounds.min_x() - -25.0).abs() < 0.01);
    assert!((bounds.min_y() - -25.0).abs() < 0.01);
    assert!((bounds.max_x() - 75.0).abs() < 0.01);
    assert!((bounds.max_y() - 75.0).abs() < 0.01);
}

#[test]
fn export_missing_composition_errors() {
    let doc = Document::empty();
    let err = export_frame(&doc, renamite_model::CompId::default(), 0.0)
        .err()
        .unwrap();
    assert!(err.to_string().contains("composition"));
}

#[test]
fn export_opacity_preserved() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
        <rect x="0" y="0" width="10" height="10" fill="#ff0000" opacity="0.5"/>
    </svg>"##;
    let doc = import(svg.as_bytes()).unwrap();
    let out = export_frame(&doc, doc.main, 0.0).unwrap();
    assert!(out.contains("opacity=\"0.5\""));
}

fn item_bounds(doc: &Document, root: NodeId) -> kurbo::Rect {
    let kids = doc.nodes[root].children.clone();
    for id in kids {
        if let NodeKind::Shape(ShapeKind::Path(p)) = &doc.nodes[id].kind {
            return p.base.to_bez_path().bounding_box();
        }
    }
    panic!("no path shape found");
}

#[test]
fn export_roundtrip_geometry_identical() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
        <path d="M10 20 L90 20 Q100 50 50 60 C30 70 20 80 10 90 Z" fill="#ff0000"/>
    </svg>"##;
    let doc = import(svg.as_bytes()).unwrap();
    let out = export_frame(&doc, doc.main, 0.0).unwrap();
    let doc2 = import(out.as_bytes()).unwrap();
    let r1 = item_bounds(&doc, doc.compositions[doc.main].children[0]);
    let r2 = item_bounds(&doc2, doc2.compositions[doc2.main].children[0]);
    assert!((r1.min_x() - r2.min_x()).abs() < 0.01);
    assert!((r1.min_y() - r2.min_y()).abs() < 0.01);
    assert!((r1.max_x() - r2.max_x()).abs() < 0.01);
    assert!((r1.max_y() - r2.max_y()).abs() < 0.01);
}

#[test]
fn export_with_report_returns_warnings() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
        <rect x="0" y="0" width="10" height="10" fill="#ff0000"/>
    </svg>"##;
    let doc = import(svg.as_bytes()).unwrap();
    let report = export_with_report(&doc, doc.main, 0.0).unwrap();
    assert!(report.value.contains("<svg"));
}
