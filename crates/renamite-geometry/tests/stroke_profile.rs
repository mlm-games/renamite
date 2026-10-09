use kurbo::BezPath;
use kurbo::Shape;
use renamite_geometry::{OutlineStyle, stroke_outline, width_curve};

fn line() -> BezPath {
    let mut p = BezPath::new();
    p.move_to(kurbo::Point::new(0.0, 0.0));
    p.line_to(kurbo::Point::new(100.0, 0.0));
    p
}

#[test]
fn flat_profile_matches_rectangle() {
    let curve = width_curve(&[(0.0, 1.0), (1.0, 1.0)]).unwrap();
    let out = stroke_outline(
        &line(),
        &curve,
        10.0,
        OutlineStyle {
            cap: kurbo::Cap::Butt,
            join: kurbo::Join::Miter,
            miter_limit: 4.0,
        },
        0.25,
    );
    assert_eq!(out.len(), 1);
    let b = out[0].bounding_box();
    assert!((b.x0 - 0.0).abs() < 1e-6, "x0 {}", b.x0);
    assert!((b.x1 - 100.0).abs() < 1e-6, "x1 {}", b.x1);
    assert!((b.y0 - -5.0).abs() < 1e-6, "y0 {}", b.y0);
    assert!((b.y1 - 5.0).abs() < 1e-6, "y1 {}", b.y1);
}

#[test]
fn taper_narrows_to_a_point() {
    let curve = width_curve(&[(0.0, 1.0), (1.0, 0.0)]).unwrap();
    let out = stroke_outline(
        &line(),
        &curve,
        20.0,
        OutlineStyle {
            cap: kurbo::Cap::Butt,
            join: kurbo::Join::Miter,
            miter_limit: 4.0,
        },
        0.25,
    );
    let b = out[0].bounding_box();
    assert!((b.y0 - -10.0).abs() < 1e-6, "start half width {}", -b.y0);
    assert!((b.y1 - 10.0).abs() < 1e-6, "start half width {}", b.y1);
    let tip = out[0]
        .elements()
        .iter()
        .filter_map(|el| match el {
            kurbo::PathEl::LineTo(p) => Some((p.x, p.y)),
            _ => None,
        })
        .any(|(x, y)| x > 99.9 && y.abs() < 0.5);
    assert!(tip, "the far end tapers to a point");
}

#[test]
fn round_cap_reaches_outward() {
    let curve = width_curve(&[(0.0, 1.0), (1.0, 1.0)]).unwrap();
    let out = stroke_outline(
        &line(),
        &curve,
        10.0,
        OutlineStyle {
            cap: kurbo::Cap::Round,
            join: kurbo::Join::Miter,
            miter_limit: 4.0,
        },
        0.25,
    );
    let b = out[0].bounding_box();
    assert!((b.x1 - 105.0).abs() < 0.2, "round cap {:?}", b);
}

#[test]
fn closed_rectangle_profile_stays_a_ring() {
    let mut p = BezPath::new();
    p.move_to(kurbo::Point::new(0.0, 0.0));
    p.line_to(kurbo::Point::new(100.0, 0.0));
    p.line_to(kurbo::Point::new(100.0, 100.0));
    p.line_to(kurbo::Point::new(0.0, 100.0));
    p.close_path();
    let curve = width_curve(&[(0.0, 1.0), (0.5, 2.0), (1.0, 1.0)]).unwrap();
    let out = stroke_outline(
        &p,
        &curve,
        4.0,
        OutlineStyle {
            cap: kurbo::Cap::Butt,
            join: kurbo::Join::Miter,
            miter_limit: 4.0,
        },
        0.25,
    );
    assert_eq!(out.len(), 1);
    let b = out[0].bounding_box();
    assert!(b.x0 < 0.0, "outline reaches past the contour: {b:?}");
    assert!(b.x1 > 100.0, "outline reaches past the contour: {b:?}");
}
