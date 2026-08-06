//! Vector path geometry. Document stores editable anchors; `kurbo::BezPath` is
//! the render/hit-test/export form only.

use kurbo::ParamCurveNearest;
pub use kurbo::{Affine, BezPath, CubicBez, PathEl, Point, Rect, Shape as KurboShape, Vec2};

use glam::DVec2;

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VectorPath {
    pub anchors: Vec<Anchor>,
    pub closed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Anchor {
    pub pos: DVec2,
    pub tan_in: DVec2,  // relative to pos
    pub tan_out: DVec2, // relative to pos
    pub mode: TangentMode,
}

impl Anchor {
    pub fn corner(pos: DVec2) -> Self {
        Self {
            pos,
            tan_in: DVec2::ZERO,
            tan_out: DVec2::ZERO,
            mode: TangentMode::Corner,
        }
    }
    pub fn symmetric(pos: DVec2, tan_out: DVec2) -> Self {
        Self {
            pos,
            tan_in: -tan_out,
            tan_out,
            mode: TangentMode::Symmetric,
        }
    }
}

/// Glaxnimate 0.6: Alt+click cycles modes; Corner->Smooth synthesizes tangents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TangentMode {
    Corner,
    Smooth,
    Symmetric,
}

impl TangentMode {
    pub fn cycled(self) -> Self {
        match self {
            TangentMode::Corner => TangentMode::Smooth,
            TangentMode::Smooth => TangentMode::Symmetric,
            TangentMode::Symmetric => TangentMode::Corner,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BooleanOp {
    Union,
    Intersection,
    Difference,
    Xor,
}

/// One anchor edit. `Insert` exists so `Delete` has an exact inverse.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AnchorEdit {
    SetPos { index: usize, pos: DVec2 },
    SetTanIn { index: usize, tan: DVec2 },
    SetTanOut { index: usize, tan: DVec2 },
    SetMode { index: usize, mode: TangentMode },
    Delete { index: usize },
    Insert { index: usize, anchor: Anchor },
    SetClosed { closed: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathHit {
    OnPath,
    Inside,
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
pub enum GeometryError {
    #[error("segment index {0} out of range")]
    SegmentOutOfRange(usize),
    #[error("anchor index {0} out of range")]
    AnchorOutOfRange(usize),
}

fn pt(v: DVec2) -> Point {
    Point::new(v.x, v.y)
}

impl VectorPath {
    pub fn segment_count(&self) -> usize {
        let n = self.anchors.len();
        if self.closed { n } else { n.saturating_sub(1) }
    }

    pub fn to_bez_path(&self) -> BezPath {
        let mut p = BezPath::new();
        let n = self.anchors.len();
        if n == 0 {
            return p;
        }
        p.move_to(pt(self.anchors[0].pos));
        for i in 0..self.segment_count() {
            let a = &self.anchors[i];
            let b = &self.anchors[(i + 1) % n];
            p.curve_to(pt(a.pos + a.tan_out), pt(b.pos + b.tan_in), pt(b.pos));
        }
        if self.closed {
            p.close_path();
        }
        p
    }

    pub fn from_bez_path(path: &BezPath) -> Self {
        let mut out = VectorPath::default();
        let mut start = DVec2::ZERO;
        for el in path.elements() {
            match *el {
                PathEl::MoveTo(p) => {
                    let v = DVec2::new(p.x, p.y);
                    start = v;
                    out.anchors.push(Anchor::corner(v));
                }
                PathEl::LineTo(p) => out.anchors.push(Anchor::corner(DVec2::new(p.x, p.y))),
                PathEl::QuadTo(q1, q2) => {
                    // elevate quad to cubic
                    let prev = out.anchors.last().map(|a| a.pos).unwrap_or_default();
                    let q1 = DVec2::new(q1.x, q1.y);
                    let end = DVec2::new(q2.x, q2.y);
                    let c1 = prev + (q1 - prev) * (2.0 / 3.0);
                    let c2 = end + (q1 - end) * (2.0 / 3.0);
                    if let Some(last) = out.anchors.last_mut() {
                        last.tan_out = c1 - last.pos;
                    }
                    let mut a = Anchor::corner(end);
                    a.tan_in = c2 - end;
                    out.anchors.push(a);
                }
                PathEl::CurveTo(c1, c2, p) => {
                    let (c1, c2, end) = (
                        DVec2::new(c1.x, c1.y),
                        DVec2::new(c2.x, c2.y),
                        DVec2::new(p.x, p.y),
                    );
                    if let Some(last) = out.anchors.last_mut() {
                        last.tan_out = c1 - last.pos;
                    }
                    let mut a = Anchor::corner(end);
                    a.tan_in = c2 - end;
                    out.anchors.push(a);
                }
                PathEl::ClosePath => {
                    out.closed = true;
                    // merge duplicated endpoint back into the first anchor
                    if out.anchors.len() >= 2 {
                        let last = *out.anchors.last().unwrap();
                        if (last.pos - start).length_squared() < 1e-12 {
                            out.anchors[0].tan_in = last.tan_in;
                            out.anchors.pop();
                        }
                    }
                }
            }
        }
        for a in &mut out.anchors {
            a.mode = detect_mode(a.tan_in, a.tan_out);
        }
        out
    }

    /// Return `(segment_index, t_param, distance)` for the nearest cubic segment.
    pub fn nearest_segment(&self, point: DVec2) -> Option<(usize, f64, f64)> {
        if self.anchors.len() < 2 {
            return None;
        }

        let q = pt(point);
        let n = self.anchors.len();
        let seg_count = self.segment_count();

        let mut best_seg = 0usize;
        let mut best_t = 0.0;
        let mut best_dist = f64::MAX;

        for i in 0..seg_count {
            let a = &self.anchors[i];
            let b = &self.anchors[(i + 1) % n];
            let cubic = CubicBez::new(
                pt(a.pos),
                pt(a.pos + a.tan_out),
                pt(b.pos + b.tan_in),
                pt(b.pos),
            );
            let hit = cubic.nearest(q, 1e-6);
            let dist = hit.distance_sq.sqrt();
            if dist < best_dist {
                best_seg = i;
                best_t = hit.t;
                best_dist = dist;
            }
        }

        Some((best_seg, best_t, best_dist))
    }

    pub fn hit_test(&self, p: DVec2, tol: f64) -> Option<PathHit> {
        let path = self.to_bez_path();
        let q = pt(p);
        let mut best_sq = f64::MAX;
        for seg in path.segments() {
            best_sq = best_sq.min(seg.nearest(q, 1e-6).distance_sq);
        }
        if best_sq.sqrt() <= tol {
            return Some(PathHit::OnPath);
        }
        if self.closed && path.contains(q) {
            return Some(PathHit::Inside);
        }
        None
    }

    /// De Casteljau split of segment `seg` at parameter `t`; new anchor is Smooth.
    pub fn insert_anchor_at(&mut self, seg: usize, t: f64) -> Result<(), GeometryError> {
        if seg >= self.segment_count() {
            return Err(GeometryError::SegmentOutOfRange(seg));
        }
        let n = self.anchors.len();
        let (i, j) = (seg, (seg + 1) % n);
        let a = self.anchors[i];
        let b = self.anchors[j];
        let (p0, p1, p2, p3) = (a.pos, a.pos + a.tan_out, b.pos + b.tan_in, b.pos);
        let q0 = p0.lerp(p1, t);
        let q1 = p1.lerp(p2, t);
        let q2 = p2.lerp(p3, t);
        let r0 = q0.lerp(q1, t);
        let r1 = q1.lerp(q2, t);
        let s = r0.lerp(r1, t);
        self.anchors[i].tan_out = q0 - p0;
        self.anchors[j].tan_in = q2 - p3;
        self.anchors.insert(
            i + 1,
            Anchor {
                pos: s,
                tan_in: r0 - s,
                tan_out: r1 - s,
                mode: TangentMode::Smooth,
            },
        );
        Ok(())
    }

    /// Round every Corner anchor by pulling back `radius` along both adjacent
    /// edges and joining with a smooth curve (quarter-circle-ish cubic
    /// approximation, k=0.5523 scaled by the pullback distance).
    pub fn round_corners(&self, radius: f64) -> VectorPath {
        if radius <= 1e-9 || self.anchors.len() < 3 {
            return self.clone();
        }
        let n = self.anchors.len();
        let seg_count = if self.closed { n } else { n.saturating_sub(1) };
        if seg_count < 2 {
            return self.clone();
        }

        let mut out = Vec::with_capacity(n * 2);
        for i in 0..n {
            let a = self.anchors[i];
            if a.mode != TangentMode::Corner {
                out.push(a);
                continue;
            }
            // Skip endpoints of an open path - nothing to round into.
            let has_prev = self.closed || i > 0;
            let has_next = self.closed || i + 1 < n;
            if !has_prev || !has_next {
                out.push(a);
                continue;
            }
            let prev = self.anchors[(i + n - 1) % n];
            let next = self.anchors[(i + 1) % n];

            let to_prev = prev.pos - a.pos;
            let to_next = next.pos - a.pos;
            let (len_prev, len_next) = (to_prev.length(), to_next.length());
            if len_prev < 1e-9 || len_next < 1e-9 {
                out.push(a);
                continue;
            }
            // Cap pullback at 45% of the shorter adjacent edge so two rounded
            // corners on a short edge can't cross each other.
            let r = radius.min(len_prev * 0.45).min(len_next * 0.45);
            let dir_prev = to_prev / len_prev;
            let dir_next = to_next / len_next;

            let p_in = a.pos + dir_prev * r; // pullback toward prev
            let p_out = a.pos + dir_next * r; // pullback toward next

            // Cubic handle length for a circular-ish arc (standard
            // 4/3*tan(θ/4) ≈ 0.5523 for a quarter turn).
            const K: f64 = 0.5523;
            out.push(Anchor {
                pos: p_in,
                tan_in: DVec2::ZERO, // outer side of the corner stays sharp
                tan_out: -dir_prev * (r * K),
                mode: TangentMode::Smooth,
            });
            out.push(Anchor {
                pos: p_out,
                tan_in: -dir_next * (r * K),
                tan_out: DVec2::ZERO,
                mode: TangentMode::Smooth,
            });
        }

        VectorPath {
            anchors: out,
            closed: self.closed,
        }
    }

    /// Reverse direction (Trim Path needs this). Swaps in/out tangents.
    pub fn reverse(&mut self) {
        self.anchors.reverse();
        for a in &mut self.anchors {
            std::mem::swap(&mut a.tan_in, &mut a.tan_out);
        }
    }

    /// Apply an edit and return its exact inverse (None if out of range).
    pub fn apply_edit(&mut self, edit: &AnchorEdit) -> Option<AnchorEdit> {
        use AnchorEdit::*;
        match edit {
            SetPos { index, pos } => {
                let a = self.anchors.get_mut(*index)?;
                let inv = SetPos {
                    index: *index,
                    pos: a.pos,
                };
                a.pos = *pos;
                Some(inv)
            }
            SetTanIn { index, tan } => {
                let a = self.anchors.get_mut(*index)?;
                let inv = SetTanIn {
                    index: *index,
                    tan: a.tan_in,
                };
                a.tan_in = *tan;
                if a.mode == TangentMode::Symmetric {
                    a.tan_out = -*tan;
                }
                Some(inv)
            }
            SetTanOut { index, tan } => {
                let a = self.anchors.get_mut(*index)?;
                let inv = SetTanOut {
                    index: *index,
                    tan: a.tan_out,
                };
                a.tan_out = *tan;
                if a.mode == TangentMode::Symmetric {
                    a.tan_in = -*tan;
                }
                Some(inv)
            }
            SetMode { index, mode } => {
                let a = self.anchors.get_mut(*index)?;
                let inv = SetMode {
                    index: *index,
                    mode: a.mode,
                };
                a.mode = *mode;
                // Corner->Smooth synthesizes tangents if zero (Glaxnimate 0.6).
                if *mode != TangentMode::Corner
                    && a.tan_in.length_squared() < 1e-12
                    && a.tan_out.length_squared() < 1e-12
                {
                    a.tan_out = DVec2::new(10.0, 0.0);
                    a.tan_in = -a.tan_out;
                }
                Some(inv)
            }
            Delete { index } => {
                if *index >= self.anchors.len() {
                    return None;
                }
                let a = self.anchors.remove(*index);
                Some(Insert {
                    index: *index,
                    anchor: a,
                })
            }
            Insert { index, anchor } => {
                if *index > self.anchors.len() {
                    return None;
                }
                self.anchors.insert(*index, *anchor);
                Some(Delete { index: *index })
            }
            SetClosed { closed } => {
                let inv = SetClosed {
                    closed: self.closed,
                };
                self.closed = *closed;
                Some(inv)
            }
        }
    }
}

fn detect_mode(tin: DVec2, tout: DVec2) -> TangentMode {
    let (li, lo) = (tin.length(), tout.length());
    if li < 1e-9 || lo < 1e-9 {
        return TangentMode::Corner;
    }
    let cross = tin.x * tout.y - tin.y * tout.x;
    let colinear_opposed = cross.abs() <= 1e-6 * li * lo && tin.dot(tout) < 0.0;
    if !colinear_opposed {
        TangentMode::Corner
    } else if (li - lo).abs() < 1e-6 {
        TangentMode::Symmetric
    } else {
        TangentMode::Smooth
    }
}

/// Façade for Graphite/linesweeper boolean ops (feature-gated until a Graphite
/// commit is pinned; the public signature is stable either way).
#[cfg(feature = "graphite-bool")]
pub fn boolean_op(_a: &VectorPath, _b: &VectorPath, _op: BooleanOp) -> Vec<VectorPath> {
    // TODO: route through linesweeper sweep-line output -> from_bez_path.
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square() -> VectorPath {
        VectorPath {
            closed: true,
            anchors: vec![
                Anchor::corner(DVec2::new(0.0, 0.0)),
                Anchor::corner(DVec2::new(10.0, 0.0)),
                Anchor::corner(DVec2::new(10.0, 10.0)),
                Anchor::corner(DVec2::new(0.0, 10.0)),
            ],
        }
    }

    #[test]
    fn roundtrip_bez() {
        let s = square();
        let back = VectorPath::from_bez_path(&s.to_bez_path());
        assert_eq!(back.anchors.len(), 4);
        assert!(back.closed);
    }

    #[test]
    fn hit_inside_and_edge() {
        let s = square();
        assert_eq!(s.hit_test(DVec2::new(5.0, 5.0), 0.5), Some(PathHit::Inside));
        assert_eq!(
            s.hit_test(DVec2::new(10.0, 5.0), 0.5),
            Some(PathHit::OnPath)
        );
        assert_eq!(s.hit_test(DVec2::new(20.0, 20.0), 0.5), None);
    }

    #[test]
    fn edit_inverse_roundtrip() {
        let mut s = square();
        let orig = s.clone();
        let inv1 = s
            .apply_edit(&AnchorEdit::SetPos {
                index: 0,
                pos: DVec2::new(-5.0, -5.0),
            })
            .unwrap();
        let inv2 = s.apply_edit(&AnchorEdit::Delete { index: 2 }).unwrap();
        s.apply_edit(&inv2).unwrap();
        s.apply_edit(&inv1).unwrap();
        assert_eq!(s, orig);
    }

    #[test]
    fn insert_anchor_preserves_shape_endpoints() {
        let mut s = square();
        s.insert_anchor_at(0, 0.5).unwrap();
        assert_eq!(s.anchors.len(), 5);
        assert!((s.anchors[1].pos - DVec2::new(5.0, 0.0)).length() < 1e-9);
    }

    #[test]
    fn nearest_segment_finds_closest_cubic() {
        let s = square();
        let (seg, t, dist) = s.nearest_segment(DVec2::new(5.0, -5.0)).unwrap();
        assert_eq!(seg, 0); // top edge (y = 0)
        assert!((dist - 5.0).abs() < 1e-6);
        assert!(t > 0.3 && t < 0.7);
    }

    #[test]
    fn nearest_segment_requires_two_anchors() {
        let mut s = VectorPath::default();
        assert!(s.nearest_segment(DVec2::ZERO).is_none());
        s.anchors.push(Anchor::corner(DVec2::ZERO));
        assert!(s.nearest_segment(DVec2::ZERO).is_none());
    }
}

#[cfg(test)]
mod round_corner_tests {
    use super::*;

    fn square() -> VectorPath {
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
    fn zero_radius_is_identity() {
        let s = square();
        assert_eq!(s.round_corners(0.0), s);
    }

    #[test]
    fn rounding_doubles_anchor_count_on_all_corners() {
        let s = square();
        let r = s.round_corners(10.0);
        assert_eq!(r.anchors.len(), 8);
        assert!(r.closed);
    }

    #[test]
    fn pullback_points_lie_on_original_edges() {
        let s = square();
        let r = s.round_corners(10.0);
        for p in r.anchors.iter().map(|a| a.pos) {
            let on_edge = (p.x - 0.0).abs() < 1e-6
                || (p.x - 100.0).abs() < 1e-6
                || (p.y - 0.0).abs() < 1e-6
                || (p.y - 100.0).abs() < 1e-6;
            assert!(on_edge, "point {p:?} must lie on an original edge");
        }
    }

    #[test]
    fn radius_clamped_on_tiny_shape() {
        let mut tiny = square();
        for a in &mut tiny.anchors {
            a.pos *= 0.1; // 10x10 square
        }
        let r = tiny.round_corners(100.0); // absurdly large radius
        for a in &r.anchors {
            assert!(a.pos.x >= -0.01 && a.pos.x <= 10.01);
            assert!(a.pos.y >= -0.01 && a.pos.y <= 10.01);
        }
    }

    #[test]
    fn smooth_anchors_pass_through_unrounded() {
        let mut s = square();
        s.anchors[0].mode = TangentMode::Smooth;
        s.anchors[0].tan_in = DVec2::new(-5.0, 0.0);
        s.anchors[0].tan_out = DVec2::new(5.0, 0.0);
        let r = s.round_corners(10.0);
        // 1 unrounded (kept as-is) + 3 corners x2 = 7 anchors total.
        assert_eq!(r.anchors.len(), 7);
    }

    #[test]
    fn open_path_does_not_round_endpoints() {
        let open = VectorPath {
            closed: false,
            anchors: vec![
                Anchor::corner(DVec2::new(0.0, 0.0)),
                Anchor::corner(DVec2::new(50.0, 0.0)),
                Anchor::corner(DVec2::new(50.0, 50.0)),
            ],
        };
        let r = open.round_corners(5.0);
        // Endpoint 0 and endpoint 2 (last) untouched; only middle corner rounds.
        assert_eq!(r.anchors.len(), 4); // 1 + 2 + 1
        assert_eq!(r.anchors[0].pos, DVec2::new(0.0, 0.0));
        assert_eq!(r.anchors.last().unwrap().pos, DVec2::new(50.0, 50.0));
    }
}
