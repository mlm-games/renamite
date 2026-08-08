//! ZigZag path modifier: sample by arc length, offset along normals.
//! Corner mode = polyline peaks; smooth mode = cubic wave approx.

use kurbo::{BezPath, ParamCurve, ParamCurveArclen, PathEl, PathSeg, Point, Vec2};

const TOL: f64 = 1e-3;

pub fn zigzag_path(path: &BezPath, amplitude: f64, ridges: f64, smooth: bool) -> BezPath {
    if !amplitude.is_finite() || amplitude.abs() < 1e-9 {
        return path.clone();
    }
    let ridges = ridges.floor().max(0.0) as usize;
    if ridges == 0 {
        return path.clone();
    }

    let mut out = BezPath::new();
    let mut sign = 1.0_f64;

    for seg in path.segments() {
        let len = seg.arclen(TOL);
        if len < 1e-9 {
            continue;
        }

        // Total samples along this segment: ridges "teeth" → ridges+1 endpoints.
        // Each ridge places one peak between endpoints.
        let n = ridges;
        let mut samples: Vec<(Point, Vec2)> = Vec::with_capacity(n + 1);

        for i in 0..=n {
            let t = i as f64 / n as f64;
            // Arc-length parameterization via binary search on arclen.
            let target = len * t;
            let t_arc = arclen_to_t(&seg, target, len);
            let p = seg.eval(t_arc);
            let d = tangent_at(&seg, t_arc);
            let dl = d.length();
            let tangent = if dl > 1e-12 {
                Vec2::new(d.x / dl, d.y / dl)
            } else {
                Vec2::new(1.0, 0.0)
            };
            let normal = Vec2::new(-tangent.y, tangent.x);
            samples.push((p, normal));
        }

        // First point of segment
        let (p0, _) = samples[0];
        if out.elements().is_empty() {
            out.move_to(p0);
        } else {
            // Continue from previous subpath join
            out.line_to(p0);
        }

        for i in 0..n {
            let (p_start, _) = samples[i];
            let (p_end, _) = samples[i + 1];
            let mid_t = (i as f64 + 0.5) / n as f64;
            let t_arc = arclen_to_t(&seg, len * mid_t, len);
            let p_mid = seg.eval(t_arc);
            let d = tangent_at(&seg, t_arc);
            let dl = d.length();
            let tangent = if dl > 1e-12 {
                Vec2::new(d.x / dl, d.y / dl)
            } else {
                Vec2::new(1.0, 0.0)
            };
            let normal = Vec2::new(-tangent.y, tangent.x);
            let peak = Point::new(
                p_mid.x + normal.x * amplitude * sign,
                p_mid.y + normal.y * amplitude * sign,
            );
            sign = -sign;

            if smooth {
                // Two cubics: start→peak, peak→end with handles along tangent.
                let h = ((p_end.x - p_start.x).powi(2) + (p_end.y - p_start.y).powi(2))
                    .sqrt()
                    * 0.25;
                let c1 = Point::new(p_start.x + tangent.x * h, p_start.y + tangent.y * h);
                let c2 = Point::new(peak.x - tangent.x * h, peak.y - tangent.y * h);
                out.curve_to(c1, c2, peak);
                let c3 = Point::new(peak.x + tangent.x * h, peak.y + tangent.y * h);
                let c4 = Point::new(p_end.x - tangent.x * h, p_end.y - tangent.y * h);
                out.curve_to(c3, c4, p_end);
            } else {
                out.line_to(peak);
                out.line_to(p_end);
            }
        }
    }

    // Preserve close if original closed.
    if path
        .elements()
        .last()
        .is_some_and(|e| matches!(e, PathEl::ClosePath))
    {
        out.close_path();
    }
    out
}

fn tangent_at(seg: &PathSeg, t: f64) -> Vec2 {
    let eps = 1e-4;
    let t0 = (t - eps).max(0.0);
    let t1 = (t + eps).min(1.0);
    let p0 = seg.eval(t0);
    let p1 = seg.eval(t1);
    Vec2::new(p1.x - p0.x, p1.y - p0.y)
}

fn arclen_to_t(seg: &PathSeg, target: f64, total: f64) -> f64 {
    if total < 1e-12 {
        return 0.0;
    }
    let mut lo = 0.0;
    let mut hi = 1.0;
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if seg.subsegment(0.0..mid).arclen(TOL) < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::Shape as _;

    fn line() -> BezPath {
        let mut p = BezPath::new();
        p.move_to((0.0, 0.0));
        p.line_to((100.0, 0.0));
        p
    }

    #[test]
    fn zero_amp_identity() {
        let p = line();
        assert_eq!(zigzag_path(&p, 0.0, 4.0, false).elements(), p.elements());
    }

    #[test]
    fn zero_ridges_identity() {
        let p = line();
        assert_eq!(zigzag_path(&p, 10.0, 0.0, false).elements(), p.elements());
    }

    #[test]
    fn corner_mode_has_no_curves() {
        let out = zigzag_path(&line(), 10.0, 2.0, false);
        assert!(
            out.elements().iter().all(|e| !matches!(e, PathEl::CurveTo(..))),
            "corner zig should be polyline only"
        );
    }

    #[test]
    fn smooth_mode_emits_curves() {
        let out = zigzag_path(&line(), 10.0, 2.0, true);
        assert!(
            out.elements().iter().any(|e| matches!(e, PathEl::CurveTo(..))),
            "smooth zig should use cubics"
        );
    }

    #[test]
    fn closed_rect_stays_closed() {
        let r = kurbo::Rect::new(0.0, 0.0, 100.0, 100.0).to_path(0.1);
        let out = zigzag_path(&r, 5.0, 4.0, false);
        assert!(matches!(out.elements().last(), Some(PathEl::ClosePath)));
    }
}
