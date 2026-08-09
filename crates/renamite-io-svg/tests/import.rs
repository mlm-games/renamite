mod common;

use renamite_animation::Frame;
use renamite_geometry::KurboShape;
use renamite_io_svg::{import, import_with_report};
use renamite_model::{
    FillRule, GradientKind, NodeKind, ShapeKind, StrokeCap, StrokeJoin, StyleKind, StylePaint,
};

use common::{fills, find_all, paths};

fn group_children(
    doc: &renamite_model::Document,
    id: renamite_model::NodeId,
) -> Vec<renamite_model::NodeId> {
    doc.nodes[id].children.clone()
}

#[test]
fn rect_becomes_world_space_path() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
        <rect x="10" y="10" width="50" height="30" fill="#ff0000"/>
    </svg>"##;
    let doc = import(svg.as_bytes()).unwrap();
    let comp = &doc.compositions[doc.main];
    assert_eq!(comp.size, (100, 100));
    assert_eq!(comp.range, (Frame(0), Frame(1)));

    let roots = &comp.children;
    assert_eq!(roots.len(), 1);
    let group = &doc.nodes[roots[0]];
    assert!(matches!(group.kind, NodeKind::Group));
    let kids = group_children(&doc, roots[0]);
    assert_eq!(kids.len(), 2, "shape + fill");

    let shape = &doc.nodes[kids[0]];
    let NodeKind::Shape(ShapeKind::Path(p)) = &shape.kind else {
        panic!("rect must import as a path shape");
    };
    let bez = p.base.to_bez_path();
    let bounds = bez.bounding_box();
    assert!((bounds.min_x() - 10.0).abs() < 0.01);
    assert!((bounds.min_y() - 10.0).abs() < 0.01);
    assert!((bounds.max_x() - 60.0).abs() < 0.01);
    assert!((bounds.max_y() - 40.0).abs() < 0.01);

    let fill = &doc.nodes[kids[1]];
    match &fill.kind {
        NodeKind::Style(StyleKind::Fill { paint, rule }) => {
            assert_eq!(*rule, FillRule::NonZero);
            let StylePaint::Solid { color } = paint else {
                panic!("expected solid paint");
            };
            assert!((color.base.r - 1.0).abs() < 1e-6);
            assert!(color.base.g.abs() < 1e-6);
            assert!(color.base.b.abs() < 1e-6);
        }
        other => panic!("expected fill style, got {other:?}"),
    }
}

#[test]
fn nested_group_transforms_are_baked_into_geometry() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
        <g transform="translate(10,20)">
            <g transform="scale(2)">
                <rect x="5" y="5" width="10" height="10"/>
            </g>
        </g>
    </svg>"##;
    let doc = import(svg.as_bytes()).unwrap();
    let shapes = paths(&doc);
    assert_eq!(shapes.len(), 1);
    let NodeKind::Shape(ShapeKind::Path(p)) = &doc.nodes[shapes[0]].kind else {
        panic!("expected path shape");
    };
    let bounds = p.base.to_bez_path().bounding_box();
    assert!((bounds.min_x() - 20.0).abs() < 0.01);
    assert!((bounds.min_y() - 30.0).abs() < 0.01);
    assert!((bounds.max_x() - 40.0).abs() < 0.01);
    assert!((bounds.max_y() - 50.0).abs() < 0.01);
}

#[test]
fn linear_gradient_imports_world_space_coords() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
        <defs>
            <linearGradient id="g" x1="0" y1="0" x2="1" y2="0">
                <stop offset="0" stop-color="#ff0000"/>
                <stop offset="1" stop-color="#0000ff" stop-opacity="0.5"/>
            </linearGradient>
        </defs>
        <rect x="0" y="0" width="100" height="50" fill="url(#g)"/>
    </svg>"##;
    let doc = import(svg.as_bytes()).unwrap();
    let gradient_fills = find_all(&doc, |n| {
        matches!(
            &n.kind,
            NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::Gradient(_),
                ..
            })
        )
    });
    assert_eq!(gradient_fills.len(), 1);
    let NodeKind::Style(StyleKind::Fill { paint, .. }) = &doc.nodes[gradient_fills[0]].kind else {
        panic!("expected fill");
    };
    let StylePaint::Gradient(g) = paint else {
        panic!("expected gradient")
    };
    assert_eq!(g.kind, GradientKind::Linear);
    assert!((g.start.base.x - 0.0).abs() < 1e-3);
    assert!((g.end.base.x - 100.0).abs() < 1e-3);
    assert_eq!(g.stops.base.0.len(), 2);
    assert!((g.stops.base.0[1].color.a - 0.5).abs() < 1e-6);
}

#[test]
fn radial_gradient_imports_center_and_radius() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
        <defs>
            <radialGradient id="r" cx="0.5" cy="0.5" r="0.4">
                <stop offset="0" stop-color="#ffffff"/>
                <stop offset="1" stop-color="#000000"/>
            </radialGradient>
        </defs>
        <rect x="0" y="0" width="100" height="100" fill="url(#r)"/>
    </svg>"##;
    let doc = import(svg.as_bytes()).unwrap();
    let gradient_fills = find_all(&doc, |n| {
        matches!(
            &n.kind,
            NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::Gradient(_),
                ..
            })
        )
    });
    let NodeKind::Style(StyleKind::Fill { paint, .. }) = &doc.nodes[gradient_fills[0]].kind else {
        panic!("expected fill");
    };
    let StylePaint::Gradient(g) = paint else {
        panic!("expected gradient")
    };
    assert_eq!(g.kind, GradientKind::Radial);
    assert!((g.start.base.x - 50.0).abs() < 1e-3);
    assert!((g.start.base.y - 50.0).abs() < 1e-3);
    let r = (g.end.base - g.start.base).length();
    assert!((r - 40.0).abs() < 1e-3);
}

#[test]
fn stroke_width_cap_join_dash_import() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
        <rect x="10" y="10" width="80" height="40" fill="none" stroke="#000000"
              stroke-width="4" stroke-linecap="round" stroke-linejoin="bevel"
              stroke-dasharray="8 4" stroke-dashoffset="2"/>
    </svg>"##;
    let doc = import(svg.as_bytes()).unwrap();
    let strokes = find_all(&doc, |n| {
        matches!(n.kind, NodeKind::Style(StyleKind::Stroke { .. }))
    });
    assert_eq!(strokes.len(), 1);
    let NodeKind::Style(StyleKind::Stroke {
        width,
        cap,
        join,
        dash,
        ..
    }) = &doc.nodes[strokes[0]].kind
    else {
        panic!("expected stroke");
    };
    assert!((width.base - 4.0).abs() < 1e-6);
    assert_eq!(*cap, StrokeCap::Round);
    assert_eq!(*join, StrokeJoin::Bevel);
    let dash = dash.as_ref().unwrap();
    assert_eq!(dash.dashes.len(), 2);
    assert!((dash.dashes[0].base - 8.0).abs() < 1e-6);
    assert!((dash.dashes[1].base - 4.0).abs() < 1e-6);
    assert!((dash.offset.base - 2.0).abs() < 1e-6);
}

#[test]
fn text_imports_as_flattened_path_outlines() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100">
        <text x="10" y="30" font-size="20">Hi</text>
    </svg>"##;
    let report = import_with_report(svg.as_bytes()).unwrap();
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.message.contains("path outlines")),
        "expected a text-flattening warning"
    );
    let shapes = paths(&report.value);
    assert!(!shapes.is_empty(), "text must flatten to path shapes");
}

#[test]
fn embedded_png_imports_as_image_asset() {
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
    let images = find_all(&doc, |n| matches!(n.kind, NodeKind::Image(_)));
    assert_eq!(images.len(), 1);
    let NodeKind::Image(asset_id) = doc.nodes[images[0]].kind else {
        panic!("expected image");
    };
    let asset = doc.image_asset(asset_id).unwrap();
    assert_eq!(asset.mime, "image/png");
    assert_eq!(asset.width, 2);
    assert_eq!(asset.height, 2);
    assert_eq!(asset.bytes, png);
}

#[test]
fn clip_path_imports_as_mask_sibling() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
        <defs>
            <clipPath id="c"><rect x="0" y="0" width="50" height="50"/></clipPath>
        </defs>
        <g clip-path="url(#c)">
            <rect x="0" y="0" width="100" height="100" fill="#000000"/>
        </g>
    </svg>"##;
    let doc = import(svg.as_bytes()).unwrap();
    let masks = find_all(&doc, |n| matches!(n.kind, NodeKind::Mask(_)));
    assert_eq!(masks.len(), 1);
    let NodeKind::Mask(mask) = &doc.nodes[masks[0]].kind else {
        panic!("expected mask")
    };
    assert!(!mask.inverted);
    assert!(matches!(mask.shape, ShapeKind::Path(_)));

    let roots = &doc.compositions[doc.main].children;
    assert_eq!(roots.len(), 1);
    let kids = group_children(&doc, roots[0]);
    assert_eq!(kids.len(), 2);
    assert!(
        matches!(doc.nodes[kids[0]].kind, NodeKind::Mask(_)),
        "mask must lead the group"
    );
}

#[test]
fn unsupported_filters_produce_warnings() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
        <filter id="b"><feGaussianBlur stdDeviation="2"/></filter>
        <g filter="url(#b)"><rect x="0" y="0" width="10" height="10" fill="#000000"/></g>
    </svg>"##;
    let report = import_with_report(svg.as_bytes()).unwrap();
    assert!(
        report.warnings.iter().any(|w| w.message.contains("filter")),
        "expected a filter warning"
    );
    let shapes = paths(&report.value);
    assert_eq!(shapes.len(), 1, "content still imports");
}

#[test]
fn pattern_fill_imports_shape_without_fill() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
        <defs>
            <pattern id="p" width="10" height="10"><rect width="10" height="10" fill="#000000"/></pattern>
        </defs>
        <rect x="0" y="0" width="100" height="100" fill="url(#p)"/>
    </svg>"##;
    let report = import_with_report(svg.as_bytes()).unwrap();
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.message.contains("pattern")),
        "expected a pattern warning"
    );
    let fills = fills(&report.value);
    assert!(fills.is_empty(), "pattern fill must not import a style");
    assert_eq!(paths(&report.value).len(), 1);
}

#[test]
fn fill_rule_evenodd_imports() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
        <path d="M0 0h100v100h-100z M20 20h60v60h-60z" fill="#ff0000" fill-rule="evenodd"/>
    </svg>"##;
    let doc = import(svg.as_bytes()).unwrap();
    let style_fills = find_all(&doc, |n| {
        matches!(n.kind, NodeKind::Style(StyleKind::Fill { .. }))
    });
    assert_eq!(style_fills.len(), 1);
    let NodeKind::Style(StyleKind::Fill { rule, .. }) = &doc.nodes[style_fills[0]].kind else {
        panic!("expected fill");
    };
    assert_eq!(*rule, FillRule::EvenOdd);
}

#[test]
fn group_opacity_survives_import() {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
        <g opacity="0.5"><rect x="0" y="0" width="10" height="10" fill="#000000"/></g>
    </svg>"##;
    let doc = import(svg.as_bytes()).unwrap();
    let roots = &doc.compositions[doc.main].children;
    assert_eq!(roots.len(), 1);
    let group = &doc.nodes[roots[0]];
    assert!(matches!(group.kind, NodeKind::Group));
    assert!((group.opacity.base - 0.5).abs() < 1e-6);
}

#[test]
fn parse_errors_are_reported() {
    let err = import(b"<svg><rect").err().unwrap();
    assert!(!err.to_string().is_empty());
}
