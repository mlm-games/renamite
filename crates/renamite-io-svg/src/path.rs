//! Path and transform adapters between the SVG tree and the Renamite model.
//!
//! `usvg` resolves every shape to an absolute [`kurbo::BezPath`] plus an
//! absolute [`usvg::tiny_skia_path::Transform`]. This module converts those
//! into the model's [`renamite_geometry::VectorPath`] / `AnimatedTransform`
//! representations and back to SVG path data for export.

use kurbo::{Affine, BezPath, PathEl};
use renamite_animation::AnimatedTransform;
use usvg::tiny_skia_path::{Path, PathSegment};

/// Convert a tiny-skia path (all segments in absolute coordinates) into a
/// `kurbo::BezPath`.
pub fn tiny_path_to_kurbo(path: &Path) -> BezPath {
    let mut output = BezPath::new();
    for segment in path.segments() {
        match segment {
            PathSegment::MoveTo(point) => {
                output.move_to((point.x as f64, point.y as f64));
            }
            PathSegment::LineTo(point) => {
                output.line_to((point.x as f64, point.y as f64));
            }
            PathSegment::QuadTo(control, point) => {
                output.quad_to(
                    (control.x as f64, control.y as f64),
                    (point.x as f64, point.y as f64),
                );
            }
            PathSegment::CubicTo(c1, c2, point) => {
                output.curve_to(
                    (c1.x as f64, c1.y as f64),
                    (c2.x as f64, c2.y as f64),
                    (point.x as f64, point.y as f64),
                );
            }
            PathSegment::Close => {
                output.close_path();
            }
        }
    }
    output
}

/// Convert a `usvg`/tiny-skia transform into a `kurbo::Affine`.
///
/// tiny-skia uses column-major column-vector notation (`Transform {
/// sx, kx, ky, sy, tx, ty }`), i.e. `x' = sx*x + kx*y + tx`. `kurbo::Affine`
/// stores `[a, b, c, d, e, f]` with `x' = a*x + c*y + e`. So the mapping is
/// `a=sx, b=ky, c=kx, d=sy, e=tx, f=ty`.
pub fn usvg_transform_to_kurbo(transform: usvg::Transform) -> Affine {
    Affine::new([
        transform.sx as f64,
        transform.ky as f64,
        transform.kx as f64,
        transform.sy as f64,
        transform.tx as f64,
        transform.ty as f64,
    ])
}

/// Decompose an affine into an `AnimatedTransform` (identity anchor).
///
/// Renamite's `linear_affine_of` reproduces the linear part as
/// `R(rotation) * skew(tan(skew_deg)) * S(scale/100)`. Inverting that for a
/// target linear matrix `L = [[a, c], [b, d]]`:
///
/// ```text
/// sx    = hypot(a, b)
/// θ     = atan2(b, a)                 (rotation)
/// sy    = cosθ*d - sinθ*c
/// tanφ  = (cosθ*c + sinθ*d) / sy      (skew)
/// ```
///
/// Used when importing image nodes, whose affine cannot be represented
/// exactly by Renamite's layered transform properties any other way. Returns
/// `None` when the matrix cannot be decomposed (e.g. a reflection).
pub fn affine_to_animated_transform(affine: Affine) -> Option<AnimatedTransform> {
    let [a, b, c, d, e, f] = affine.as_coeffs();

    let sx = (a * a + b * b).sqrt();
    if sx < 1e-9 {
        return None;
    }
    let theta = b.atan2(a);
    let (sin_t, cos_t) = theta.sin_cos();
    let mut sy = cos_t * d - sin_t * c;
    if sy.abs() < 1e-9 {
        return None;
    }
    // Negative scale.y is folded into a 180° rotation so both scale factors
    // stay positive (Reflections are still rejected below).
    let mut rotation_deg = theta.to_degrees();
    if sy < 0.0 {
        sy = -sy;
        rotation_deg += 180.0;
    }
    let tan_phi = (cos_t * c + sin_t * d) / sy;

    // A reflection (negative determinant) cannot be expressed with positive
    // scales. Detect it by reconstructing and comparing.
    let rotation = Affine::rotate(theta);
    let skew = Affine::skew(tan_phi, 0.0);
    let scale = Affine::scale_non_uniform(sx, sy);
    let linear = rotation * skew * scale;
    let [la, lb, lc, ld, ..] = linear.as_coeffs();
    if (la - a).abs() > 1e-3
        || (lb - b).abs() > 1e-3
        || (lc - c).abs() > 1e-3
        || (ld - d).abs() > 1e-3
    {
        return None;
    }

    let mut transform = AnimatedTransform::identity();
    transform.position = renamite_animation::Animated::new(glam::DVec2::new(e, f));
    transform.scale = renamite_animation::Animated::new(glam::DVec2::new(sx * 100.0, sy * 100.0));
    transform.rotation =
        renamite_animation::Animated::new(renamite_animation::Angle(rotation_deg.to_radians()));
    transform.skew = renamite_animation::Animated::new(tan_phi.atan().to_degrees());
    Some(transform)
}

/// Serialize a `BezPath` into an SVG `d` attribute string.
pub fn path_data(path: &BezPath) -> String {
    use std::fmt::Write;

    let mut output = String::new();
    for element in path.elements() {
        match *element {
            PathEl::MoveTo(p) => {
                let _ = write!(output, "M{} {}", fmt(p.x), fmt(p.y));
            }
            PathEl::LineTo(p) => {
                let _ = write!(output, "L{} {}", fmt(p.x), fmt(p.y));
            }
            PathEl::QuadTo(control, p) => {
                let _ = write!(
                    output,
                    "Q{} {} {} {}",
                    fmt(control.x),
                    fmt(control.y),
                    fmt(p.x),
                    fmt(p.y),
                );
            }
            PathEl::CurveTo(c1, c2, p) => {
                let _ = write!(
                    output,
                    "C{} {} {} {} {} {}",
                    fmt(c1.x),
                    fmt(c1.y),
                    fmt(c2.x),
                    fmt(c2.y),
                    fmt(p.x),
                    fmt(p.y),
                );
            }
            PathEl::ClosePath => {
                output.push('Z');
            }
        }
    }
    output
}

/// Format a coordinate for SVG output: 4 decimals, trailing zeros trimmed,
/// `-0` normalized to `0`.
pub(crate) fn fmt(value: f64) -> String {
    let mut value = format!("{value:.4}");
    while value.contains('.') && value.ends_with('0') {
        value.pop();
    }
    if value.ends_with('.') {
        value.pop();
    }
    if value == "-0" {
        value = "0".into();
    }
    value
}

/// Serialize an affine into an SVG `transform="matrix(a b c d e f)"` value.
pub fn matrix_attr(affine: Affine) -> String {
    use std::fmt::Write;
    let [a, b, c, d, e, f] = affine.as_coeffs();
    let mut out = String::new();
    let _ = write!(
        out,
        "matrix({} {} {} {} {} {})",
        fmt(a),
        fmt(b),
        fmt(c),
        fmt(d),
        fmt(e),
        fmt(f),
    );
    out
}
