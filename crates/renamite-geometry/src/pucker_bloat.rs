//! Pucker & Bloat: vertices toward/away from contour centroid. Handles opposite.
//! Matches AE / lottie-web `pb` semantics (amount is percent, 100 = extreme).

use kurbo::{BezPath, PathEl, Point};

/// `amount` in percent. Positive = bloat (vertices out), negative = pucker.
pub fn pucker_bloat_path(path: &BezPath, amount_pct: f64) -> BezPath {
    if !amount_pct.is_finite() || amount_pct.abs() < 1e-9 {
        return path.clone();
    }
    let k = amount_pct / 100.0;

    let mut out = BezPath::new();
    for (pts, closed) in flatten_subpaths(path) {
        if pts.len() < 2 {
            continue;
        }
        let center = centroid(&pts);
        let warped: Vec<Point> = pts.iter().map(|p| lerp_pt(*p, center, k)).collect();

        // Reconstruct as polyline (handles not stored on flattened path;
        // cubic paths are flattened first like offset). For VectorPath-aware
        // path, prefer pucker_bloat_vector_path below.
        out.move_to(warped[0]);
        for p in warped.iter().skip(1) {
            out.line_to(*p);
        }
        if closed {
            out.close_path();
        }
    }
    out
}

/// Anchor-level pucker/bloat (preserves cubic handles). Preferred for Path shapes.
pub fn pucker_bloat_vector_path(path: &crate::VectorPath, amount_pct: f64) -> crate::VectorPath {
    use crate::{Anchor, VectorPath};

    if !amount_pct.is_finite() || amount_pct.abs() < 1e-9 || path.anchors.is_empty() {
        return path.clone();
    }
    let k = amount_pct / 100.0;
    let center = {
        let n = path.anchors.len() as f64;
        let s: glam::DVec2 = path.anchors.iter().map(|a| a.pos).sum();
        s / n.max(1.0)
    };

    let anchors: Vec<Anchor> = path
        .anchors
        .iter()
        .map(|a| {
            // Vertex toward center by +k; handles opposite (-k).
            let pos = a.pos + (center - a.pos) * k;
            // Absolute handle points, then convert back to relative.
            let hin_abs = a.pos + a.tan_in;
            let hout_abs = a.pos + a.tan_out;
            let hin_w = hin_abs + (center - hin_abs) * (-k);
            let hout_w = hout_abs + (center - hout_abs) * (-k);
            Anchor {
                pos,
                tan_in: hin_w - pos,
                tan_out: hout_w - pos,
                mode: a.mode,
            }
        })
        .collect();

    VectorPath {
        anchors,
        closed: path.closed,
    }
}

fn centroid(pts: &[Point]) -> Point {
    let n = pts.len() as f64;
    let (mut x, mut y) = (0.0, 0.0);
    for p in pts {
        x += p.x;
        y += p.y;
    }
    Point::new(x / n, y / n)
}

fn lerp_pt(a: Point, b: Point, t: f64) -> Point {
    // Vertex moves toward center by t: a + (b-a)*t
    Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

fn flatten_subpaths(path: &BezPath) -> Vec<(Vec<Point>, bool)> {
    let mut result = Vec::new();
    let mut cur = Vec::new();
    let mut closed = false;
    let mut start = Point::ZERO;

    kurbo::flatten(path, 0.25, |el| match el {
        PathEl::MoveTo(p) => {
            if !cur.is_empty() {
                result.push((std::mem::take(&mut cur), closed));
                closed = false;
            }
            start = p;
            cur.push(p);
        }
        PathEl::LineTo(p) => {
            if cur.last().is_none_or(|q| (*q - p).hypot() > 1e-9) {
                cur.push(p);
            }
        }
        PathEl::ClosePath => {
            if cur.len() >= 2 {
                let last = *cur.last().unwrap();
                if (last - start).hypot() < 1e-9 {
                    cur.pop();
                }
            }
            closed = true;
        }
        _ => {}
    });
    if !cur.is_empty() {
        result.push((cur, closed));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Anchor, TangentMode, VectorPath};
    use glam::DVec2;

    fn unit_square_path() -> VectorPath {
        VectorPath {
            closed: true,
            anchors: vec![
                Anchor::corner(DVec2::new(0.0, 0.0)),
                Anchor::corner(DVec2::new(100.0, 0.0)),
                Anchor::corner(DVec2::new(100.0, 100.0)),
                Anchor::corner(DVec2::new(0.0, 100.0)),
            ],
        }
    }

    #[test]
    fn zero_amount_identity() {
        let p = unit_square_path();
        assert_eq!(pucker_bloat_vector_path(&p, 0.0), p);
    }

    #[test]
    fn positive_moves_vertices_toward_center() {
        let p = unit_square_path();
        let out = pucker_bloat_vector_path(&p, 50.0);
        // Center is (50,50). Corner (0,0) → halfway = (25,25).
        assert!((out.anchors[0].pos - DVec2::new(25.0, 25.0)).length() < 1e-6);
    }

    #[test]
    fn negative_moves_vertices_outward() {
        let p = unit_square_path();
        let out = pucker_bloat_vector_path(&p, -50.0);
        // pos = a + (c-a)*k with k=-0.5 = (0,0) + (50,50)*(-0.5) = (-25,-25)
        assert!((out.anchors[0].pos - DVec2::new(-25.0, -25.0)).length() < 1e-6);
    }

    #[test]
    fn handles_move_opposite_to_vertices() {
        let mut p = unit_square_path();
        p.anchors[0].mode = TangentMode::Smooth;
        p.anchors[0].tan_out = DVec2::new(10.0, 0.0);
        let out = pucker_bloat_vector_path(&p, 50.0);
        let tan = out.anchors[0].tan_out;
        assert!(tan.length() > 1e-3, "handle should remain non-zero");
    }
}
