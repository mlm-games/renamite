//! Vector path geometry. Document stores editable anchors; `kurbo::BezPath` is
//! the render/hit-test/export form only.

pub mod pucker_bloat;
pub mod zigzag;
pub use pucker_bloat::{pucker_bloat_path, pucker_bloat_vector_path};
pub use zigzag::zigzag_path;

use kurbo::ParamCurveNearest;
pub use kurbo::{Affine, BezPath, CubicBez, PathEl, Point, Rect, Shape as KurboShape, Vec2};

use glam::DVec2;

/// Validate a dash pattern before passing it to Kurbo.
///
/// Returns `None` for:
/// - empty patterns,
/// - negative/non-finite entries,
/// - all-zero patterns.
///
/// Mixed zero/nonzero patterns are retained. Kurbo handles odd-length
/// patterns according to SVG semantics.
pub fn normalize_dash_pattern(pattern: &[f64]) -> Option<Vec<f64>> {
    if pattern.is_empty()
        || pattern.iter().any(|x| !x.is_finite() || *x < 0.0)
        || pattern.iter().sum::<f64>() <= 1e-9
    {
        return None;
    }

    Some(pattern.to_vec())
}

/// Apply a stroke dash pattern to `path`.
///
/// The returned path consists of open subpaths representing visible dashes.
/// Each subpath is subsequently capped by the stroke tessellator.
///
/// `None` means the dash settings are invalid or effectively disabled, so the
/// caller should render the original solid path.
pub fn dash_bez_path(path: &BezPath, pattern: &[f64], offset: f64) -> Option<BezPath> {
    let pattern = normalize_dash_pattern(pattern)?;

    if !offset.is_finite() {
        return None;
    }

    let elements = kurbo::dash(path.elements().iter().copied(), offset, &pattern).collect();

    Some(BezPath::from_vec(elements))
}

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

    /// De Casteljau split of segment `seg` at parameter `t`. New anchor is Smooth.
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

/// Errors from boolean operations and stroke expansion.
#[derive(Debug, thiserror::Error)]
pub enum PathOpError {
    #[error("path operation requires closed contours")]
    OpenPath,
    #[error("path operation produced no geometry")]
    Empty,
    #[error("boolean operation failed: {0}")]
    Boolean(#[from] linesweeper::Error),
}

fn map_boolean_op(op: BooleanOp) -> linesweeper::BinaryOp {
    match op {
        BooleanOp::Union => linesweeper::BinaryOp::Union,
        BooleanOp::Intersection => linesweeper::BinaryOp::Intersection,
        BooleanOp::Difference => linesweeper::BinaryOp::Difference,
        BooleanOp::Xor => linesweeper::BinaryOp::Xor,
    }
}

/// Concatenate closed `contours` into one multi-subpath `BezPath`, suitable
/// as an input to [`boolean_bez`].
pub fn contours_to_bez(contours: &[VectorPath]) -> BezPath {
    let mut out = BezPath::new();
    for contour in contours {
        out.extend(contour.to_bez_path().elements().iter().copied());
    }
    out
}

/// Linesweeper-backed boolean op on raw Bézier outlines.
///
/// Unlike [`boolean_op`], both sides may already be compound (multi-subpath)
/// outlines, so folding N selected shapes never discards intermediate holes or
/// disjoint pieces.
pub fn boolean_bez(
    a: &BezPath,
    b: &BezPath,
    op: BooleanOp,
) -> Result<Vec<VectorPath>, PathOpError> {
    let contours =
        linesweeper::binary_op(a, b, linesweeper::FillRule::NonZero, map_boolean_op(op))?;

    Ok(contours
        .contours()
        .filter_map(|contour| {
            let path = VectorPath::from_bez_path(&contour.path);
            (path.closed && path.anchors.len() >= 3).then_some(path)
        })
        .collect())
}

/// Boolean op between two single-contour paths. Multi-contour inputs should
/// use [`boolean_bez`] via [`contours_to_bez`] so holes survive the fold.
pub fn boolean_op(
    a: &VectorPath,
    b: &VectorPath,
    op: BooleanOp,
) -> Result<Vec<VectorPath>, PathOpError> {
    if !a.closed || !b.closed {
        return Err(PathOpError::OpenPath);
    }
    boolean_bez(&a.to_bez_path(), &b.to_bez_path(), op)
}

/// Split a multi-subpath `BezPath` into one [`VectorPath`] per subpath
/// (`MoveTo` .. next `MoveTo`). Subpaths with fewer than two anchors are
/// dropped; open subpaths stay open.
pub fn split_bez_subpaths(path: &BezPath) -> Vec<VectorPath> {
    let mut output = Vec::new();
    let mut current = BezPath::new();

    for element in path.elements().iter().copied() {
        if matches!(element, PathEl::MoveTo(_)) && !current.is_empty() {
            let sub = VectorPath::from_bez_path(&current);
            if sub.anchors.len() >= 2 {
                output.push(sub);
            }
            current = BezPath::new();
        }
        current.push(element);
    }

    if !current.is_empty() {
        let sub = VectorPath::from_bez_path(&current);
        if sub.anchors.len() >= 2 {
            output.push(sub);
        }
    }

    output
}

/// Expand a stroke into filled outlines (one contour per disjoint piece).
///
/// Dashes (if any) are expanded first; the resulting dash subpaths are then
/// stroked with the given cap/join configuration.
pub fn stroke_to_paths(
    path: &VectorPath,
    width: f64,
    cap: kurbo::Cap,
    join: kurbo::Join,
    miter_limit: f64,
    dash: Option<(&[f64], f64)>,
    tolerance: f64,
) -> Result<Vec<VectorPath>, PathOpError> {
    if !width.is_finite() || width <= 0.0 {
        return Err(PathOpError::Empty);
    }

    let original = path.to_bez_path();

    let source = match dash {
        Some((pattern, offset)) => dash_bez_path(&original, pattern, offset).unwrap_or(original),
        None => original,
    };

    let stroke = kurbo::Stroke::new(width)
        .with_start_cap(cap)
        .with_end_cap(cap)
        .with_join(join)
        .with_miter_limit(miter_limit);

    let outline = kurbo::stroke(
        source.elements().iter().copied(),
        &stroke,
        &kurbo::StrokeOpts::default(),
        tolerance.max(1e-4),
    );

    let result = split_bez_subpaths(&outline);

    if result.is_empty() {
        Err(PathOpError::Empty)
    } else {
        Ok(result)
    }
}

/// Fit a simpler path through the same geometry within `tolerance` document
/// units (kurbo curve fitting).
pub fn simplify_path(path: &VectorPath, tolerance: f64) -> VectorPath {
    let simplified = kurbo::simplify::simplify_bezpath(
        path.to_bez_path(),
        tolerance.max(1e-4),
        &kurbo::simplify::SimplifyOptions::default(),
    );

    VectorPath::from_bez_path(&simplified)
}

/// Offset a path by `amount` in document units.
///
/// Positive amount expands closed contours outward based on their winding.
/// Negative amount insets closed contours. Open contours are shifted to their
/// left side for positive amount.
///
/// This is a deterministic flattened-polyline offset. Exact cubic offset curves
/// are not generally cubic Béziers (v1).
pub fn offset_bez_path(path: &BezPath, amount: f64, tolerance: f64) -> Option<BezPath> {
    if !amount.is_finite() {
        return None;
    }

    if amount.abs() <= 1e-9 {
        return Some(path.clone());
    }

    let contours = flatten_to_contours(path, tolerance.max(0.01));
    if contours.is_empty() {
        return None;
    }

    let mut out = BezPath::new();

    for contour in contours {
        let offset = offset_contour(&contour.points, contour.closed, amount)?;

        if offset.len() < 2 {
            continue;
        }

        out.move_to(pt(offset[0]));

        for p in offset.iter().skip(1) {
            out.line_to(pt(*p));
        }

        if contour.closed {
            out.close_path();
        }
    }

    if out.elements().is_empty() {
        None
    } else {
        Some(out)
    }
}

#[derive(Clone, Debug)]
struct FlatContour {
    points: Vec<DVec2>,
    closed: bool,
}

fn flatten_to_contours(path: &BezPath, tolerance: f64) -> Vec<FlatContour> {
    use kurbo::{ParamCurve, ParamCurveArclen};

    let mut contours = Vec::new();
    let mut current: Vec<DVec2> = Vec::new();
    let mut cursor = DVec2::ZERO;
    let mut start = DVec2::ZERO;

    let flush = |contours: &mut Vec<FlatContour>, current: &mut Vec<DVec2>, closed: bool| {
        dedupe_points(current);

        if current.len() >= 2 {
            contours.push(FlatContour {
                points: std::mem::take(current),
                closed,
            });
        } else {
            current.clear();
        }
    };

    for element in path.elements() {
        match *element {
            PathEl::MoveTo(p) => {
                flush(&mut contours, &mut current, false);
                cursor = DVec2::new(p.x, p.y);
                start = cursor;
                current.push(cursor);
            }

            PathEl::LineTo(p) => {
                cursor = DVec2::new(p.x, p.y);
                current.push(cursor);
            }

            PathEl::QuadTo(c, p) => {
                let seg = kurbo::QuadBez::new(pt(cursor), c, p);

                let len = seg.arclen(tolerance);
                let steps = (len / tolerance).ceil().max(2.0) as usize;

                for i in 1..=steps {
                    let t = i as f64 / steps as f64;
                    let q = seg.eval(t);
                    current.push(DVec2::new(q.x, q.y));
                }

                cursor = DVec2::new(p.x, p.y);
            }

            PathEl::CurveTo(c1, c2, p) => {
                let seg = CubicBez::new(pt(cursor), c1, c2, p);

                let len = seg.arclen(tolerance);
                let steps = (len / tolerance).ceil().max(3.0) as usize;

                for i in 1..=steps {
                    let t = i as f64 / steps as f64;
                    let q = seg.eval(t);
                    current.push(DVec2::new(q.x, q.y));
                }

                cursor = DVec2::new(p.x, p.y);
            }

            PathEl::ClosePath => {
                if (cursor - start).length_squared() > 1e-12 {
                    current.push(start);
                }

                // Remove duplicated close point. We use ClosePath instead.
                if current.len() >= 2
                    && (current[0] - *current.last().unwrap()).length_squared() <= 1e-12
                {
                    current.pop();
                }

                flush(&mut contours, &mut current, true);
                cursor = start;
            }
        }
    }

    flush(&mut contours, &mut current, false);

    contours
}

fn dedupe_points(points: &mut Vec<DVec2>) {
    let mut out = Vec::with_capacity(points.len());

    for p in points.drain(..) {
        if out
            .last()
            .map(|last: &DVec2| (*last - p).length_squared() > 1e-12)
            .unwrap_or(true)
        {
            out.push(p);
        }
    }

    *points = out;
}

fn offset_contour(points: &[DVec2], closed: bool, amount: f64) -> Option<Vec<DVec2>> {
    if points.len() < 2 {
        return None;
    }

    if closed && points.len() < 3 {
        return None;
    }

    if closed {
        offset_closed_contour(points, amount)
    } else {
        offset_open_contour(points, amount)
    }
}

fn offset_open_contour(points: &[DVec2], amount: f64) -> Option<Vec<DVec2>> {
    let n = points.len();

    let mut out = Vec::with_capacity(n);

    for i in 0..n {
        if i == 0 {
            let dir = unit(points[1] - points[0])?;
            out.push(points[0] + left_normal(dir) * amount);
        } else if i == n - 1 {
            let dir = unit(points[n - 1] - points[n - 2])?;
            out.push(points[n - 1] + left_normal(dir) * amount);
        } else {
            let prev = unit(points[i] - points[i - 1])?;
            let next = unit(points[i + 1] - points[i])?;
            let n0 = left_normal(prev);
            let n1 = left_normal(next);
            out.push(join_point(points[i], prev, next, n0, n1, amount));
        }
    }

    Some(out)
}

fn offset_closed_contour(points: &[DVec2], amount: f64) -> Option<Vec<DVec2>> {
    let n = points.len();
    let area = signed_area(points);

    // For a positive shoelace winding, the contour interior is on the left
    // side of edges, so outward is the right normal. For negative winding,
    // outward is the left normal.
    let outward_right = area >= 0.0;

    let mut out = Vec::with_capacity(n);

    for i in 0..n {
        let prev_i = (i + n - 1) % n;
        let next_i = (i + 1) % n;

        let prev_dir = unit(points[i] - points[prev_i])?;
        let next_dir = unit(points[next_i] - points[i])?;

        let n0 = if outward_right {
            right_normal(prev_dir)
        } else {
            left_normal(prev_dir)
        };

        let n1 = if outward_right {
            right_normal(next_dir)
        } else {
            left_normal(next_dir)
        };

        out.push(join_point(points[i], prev_dir, next_dir, n0, n1, amount));
    }

    Some(out)
}

fn signed_area(points: &[DVec2]) -> f64 {
    let mut area = 0.0;

    for i in 0..points.len() {
        let a = points[i];
        let b = points[(i + 1) % points.len()];
        area += a.x * b.y - b.x * a.y;
    }

    area * 0.5
}

fn unit(v: DVec2) -> Option<DVec2> {
    let len = v.length();

    if len <= 1e-12 || !len.is_finite() {
        None
    } else {
        Some(v / len)
    }
}

fn left_normal(v: DVec2) -> DVec2 {
    DVec2::new(-v.y, v.x)
}

fn right_normal(v: DVec2) -> DVec2 {
    DVec2::new(v.y, -v.x)
}

fn join_point(
    p: DVec2,
    prev_dir: DVec2,
    next_dir: DVec2,
    prev_normal: DVec2,
    next_normal: DVec2,
    amount: f64,
) -> DVec2 {
    let a0 = p + prev_normal * amount;
    let a1 = p + next_normal * amount;

    match line_intersection(a0, prev_dir, a1, next_dir) {
        Some(miter) => {
            let miter_len = (miter - p).length();
            let limit = amount.abs() * 8.0 + 1e-6;

            if miter_len.is_finite() && miter_len <= limit {
                miter
            } else {
                // Bevel-ish fallback: average the two offset endpoints.
                (a0 + a1) * 0.5
            }
        }

        None => (a0 + a1) * 0.5,
    }
}

fn line_intersection(p: DVec2, r: DVec2, q: DVec2, s: DVec2) -> Option<DVec2> {
    let cross = r.x * s.y - r.y * s.x;

    if cross.abs() <= 1e-12 {
        return None;
    }

    let qp = q - p;
    let t = (qp.x * s.y - qp.y * s.x) / cross;

    Some(p + r * t)
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

#[cfg(test)]
mod dash_tests {
    use super::*;
    use kurbo::ParamCurveArclen;

    fn line(length: f64) -> BezPath {
        let mut path = BezPath::new();
        path.move_to((0.0, 0.0));
        path.line_to((length, 0.0));
        path
    }

    fn length(path: &BezPath) -> f64 {
        path.segments().map(|segment| segment.arclen(1e-6)).sum()
    }

    #[test]
    fn dash_line_produces_expected_visible_length() {
        // 40 units with [10 on, 10 off] => 20 visible units.
        let dashed = dash_bez_path(&line(40.0), &[10.0, 10.0], 0.0).unwrap();

        assert!((length(&dashed) - 20.0).abs() < 1e-5);
    }

    #[test]
    fn dash_offset_shifts_pattern() {
        let a = dash_bez_path(&line(40.0), &[10.0, 10.0], 0.0).unwrap();

        let b = dash_bez_path(&line(40.0), &[10.0, 10.0], 5.0).unwrap();

        assert_ne!(a.elements(), b.elements());
    }

    #[test]
    fn odd_pattern_matches_explicitly_doubled_pattern() {
        let path = line(100.0);

        let odd = dash_bez_path(&path, &[10.0], 0.0).unwrap();

        let doubled = dash_bez_path(&path, &[10.0, 10.0], 0.0).unwrap();

        assert_eq!(odd.elements(), doubled.elements());
    }

    #[test]
    fn negative_offset_is_supported() {
        let path = line(100.0);

        let positive = dash_bez_path(&path, &[10.0, 5.0], 30.0).unwrap();

        let negative = dash_bez_path(&path, &[10.0, 5.0], -30.0).unwrap();

        assert!(positive.is_finite());
        assert!(negative.is_finite());
    }

    #[test]
    fn rejects_invalid_patterns() {
        assert!(dash_bez_path(&line(10.0), &[], 0.0).is_none());
        assert!(dash_bez_path(&line(10.0), &[0.0, 0.0], 0.0).is_none());
        assert!(dash_bez_path(&line(10.0), &[-1.0, 2.0], 0.0).is_none());
        assert!(dash_bez_path(&line(10.0), &[f64::NAN, 2.0], 0.0).is_none());
        assert!(dash_bez_path(&line(10.0), &[1.0, 2.0], f64::NAN).is_none());
    }
}

#[cfg(test)]
mod boolean_tests {
    use super::*;

    fn rect_path(x0: f64, y0: f64, x1: f64, y1: f64) -> VectorPath {
        let mut p = BezPath::new();
        p.move_to((x0, y0));
        p.line_to((x1, y0));
        p.line_to((x1, y1));
        p.line_to((x0, y1));
        p.close_path();
        VectorPath::from_bez_path(&p)
    }

    #[test]
    fn boolean_difference_preserves_hole() {
        let outer = rect_path(0.0, 0.0, 100.0, 100.0);
        let inner = rect_path(25.0, 25.0, 75.0, 75.0);

        let result = boolean_op(&outer, &inner, BooleanOp::Difference).unwrap();

        assert_eq!(result.len(), 2); // outside boundary + hole
        assert!(result.iter().all(|p| p.closed));
    }

    #[test]
    fn boolean_union_can_return_disjoint_contours() {
        let a = rect_path(0.0, 0.0, 10.0, 10.0);
        let b = rect_path(20.0, 0.0, 30.0, 10.0);

        let result = boolean_op(&a, &b, BooleanOp::Union).unwrap();

        assert_eq!(result.len(), 2);
    }

    #[test]
    fn boolean_intersection_of_overlapping_squares_is_one_contour() {
        let a = rect_path(0.0, 0.0, 20.0, 20.0);
        let b = rect_path(10.0, 10.0, 30.0, 30.0);

        let result = boolean_op(&a, &b, BooleanOp::Intersection).unwrap();

        assert_eq!(result.len(), 1);
        let bb = contours_to_bez(&result).bounding_box();
        assert!((bb.x0 - 10.0).abs() < 1e-6 && (bb.x1 - 20.0).abs() < 1e-6);
    }

    #[test]
    fn boolean_op_rejects_open_paths() {
        let mut open = rect_path(0.0, 0.0, 10.0, 10.0);
        open.closed = false;
        let closed = rect_path(0.0, 0.0, 5.0, 5.0);

        assert!(matches!(
            boolean_op(&open, &closed, BooleanOp::Union),
            Err(PathOpError::OpenPath)
        ));
    }

    #[test]
    fn boolean_intersection_of_disjoint_shapes_is_validly_empty() {
        let a = rect_path(0.0, 0.0, 10.0, 10.0);
        let b = rect_path(20.0, 0.0, 30.0, 10.0);

        // Empty is a RESULT, not an error.
        let result = boolean_op(&a, &b, BooleanOp::Intersection).unwrap();
        assert!(result.is_empty());

        // Difference under a covering cutter also legitimately erases.
        let covered = boolean_op(&a, &b, BooleanOp::Difference).is_ok();
        assert!(covered);
        let erased = rect_path(-5.0, -5.0, 15.0, 15.0);
        assert!(
            boolean_op(&a, &erased, BooleanOp::Difference)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn boolean_bez_folds_compound_accumulator_without_losing_holes() {
        // Square with a hole (difference), unioned with an overlapping square.
        let outer = rect_path(0.0, 0.0, 100.0, 100.0);
        let inner = rect_path(25.0, 25.0, 75.0, 75.0);
        let holed = boolean_op(&outer, &inner, BooleanOp::Difference).unwrap();

        let cutter = rect_path(60.0, 0.0, 160.0, 40.0);

        // Fold via the BezPath-level wrapper: the hole must survive.
        let folded = boolean_bez(
            &contours_to_bez(&holed),
            &cutter.to_bez_path(),
            BooleanOp::Union,
        )
        .unwrap();

        let all = contours_to_bez(&folded);
        let center = Point::new(50.0, 50.0);
        assert_eq!(all.winding(center), 0, "hole must remain after the fold");
    }
}

#[cfg(test)]
mod stroke_tests {
    use super::*;
    use kurbo::ParamCurveArclen;

    #[test]
    fn stroked_line_produces_closed_outline_near_expected_width() {
        let line = VectorPath {
            anchors: vec![
                Anchor::corner(DVec2::new(0.0, 0.0)),
                Anchor::corner(DVec2::new(100.0, 0.0)),
            ],
            closed: false,
        };

        let outlines = stroke_to_paths(
            &line,
            4.0,
            kurbo::Cap::Butt,
            kurbo::Join::Miter,
            4.0,
            None,
            0.1,
        )
        .unwrap();

        assert_eq!(outlines.len(), 1);
        assert!(outlines[0].closed);
        let bez = outlines[0].to_bez_path();
        let bb = bez.bounding_box();
        assert!((bb.height() - 4.0).abs() < 0.2, "height = {}", bb.height());
        assert!((bb.width() - 100.0).abs() < 0.2, "width = {}", bb.width());
    }

    #[test]
    fn dashed_stroke_expands_each_dash() {
        let line = VectorPath {
            anchors: vec![
                Anchor::corner(DVec2::new(0.0, 0.0)),
                Anchor::corner(DVec2::new(100.0, 0.0)),
            ],
            closed: false,
        };

        let outlines = stroke_to_paths(
            &line,
            2.0,
            kurbo::Cap::Butt,
            kurbo::Join::Bevel,
            4.0,
            Some(([10.0, 10.0].as_slice(), 0.0)),
            0.1,
        )
        .unwrap();

        // 100 units of [10 on / 10 off] => 5 dashes => 5 outline pieces.
        assert_eq!(outlines.len(), 5);
        let total: f64 = outlines
            .iter()
            .map(|p| {
                p.to_bez_path()
                    .segments()
                    .map(|s| s.arclen(1e-3))
                    .sum::<f64>()
            })
            .sum();
        assert!(total > 0.0);
    }

    #[test]
    fn closed_square_stroke_is_one_ring() {
        let square = VectorPath {
            anchors: vec![
                Anchor::corner(DVec2::new(0.0, 0.0)),
                Anchor::corner(DVec2::new(10.0, 0.0)),
                Anchor::corner(DVec2::new(10.0, 10.0)),
                Anchor::corner(DVec2::new(0.0, 10.0)),
            ],
            closed: true,
        };

        let outlines = stroke_to_paths(
            &square,
            2.0,
            kurbo::Cap::Butt,
            kurbo::Join::Miter,
            4.0,
            None,
            0.1,
        )
        .unwrap();

        // A closed shape's stroke is an annulus: outer ring + hole.
        assert_eq!(outlines.len(), 2);
        assert!(outlines.iter().all(|p| p.closed));
        let all = contours_to_bez(&outlines);
        assert_eq!(all.winding(Point::new(5.0, 5.0)), 0, "center stays hollow");
    }

    #[test]
    fn invalid_width_is_an_error() {
        let line = VectorPath::default();
        assert!(
            stroke_to_paths(
                &line,
                0.0,
                kurbo::Cap::Butt,
                kurbo::Join::Miter,
                4.0,
                None,
                0.1
            )
            .is_err()
        );
        assert!(
            stroke_to_paths(
                &line,
                f64::NAN,
                kurbo::Cap::Butt,
                kurbo::Join::Miter,
                4.0,
                None,
                0.1
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod simplify_tests {
    use super::*;

    #[test]
    fn simplify_keeps_collinear_polyline_small_and_openness() {
        // A dense zig-zag-free polyline: simplify should not blow it up.
        let mut p = BezPath::new();
        p.move_to((0.0, 0.0));
        for i in 1..=20 {
            p.line_to((i as f64 * 5.0, (i % 2) as f64));
        }

        let dense = VectorPath::from_bez_path(&p);
        let simple = simplify_path(&dense, 1.0);

        assert!(!simple.closed);
        assert!(!simple.anchors.is_empty());
    }

    #[test]
    fn tolerance_floor_never_panics_on_degenerate_input() {
        let single = VectorPath {
            anchors: vec![Anchor::corner(DVec2::ZERO)],
            closed: false,
        };
        let out = simplify_path(&single, f64::NAN);
        assert!(out.anchors.len() <= 1, "degenerate input must not grow");
    }
}

#[cfg(test)]
mod offset_tests {
    use super::*;

    fn square_path() -> BezPath {
        Rect::new(0.0, 0.0, 100.0, 100.0).to_path(0.1)
    }

    fn line_path() -> BezPath {
        let mut path = BezPath::new();
        path.move_to((0.0, 0.0));
        path.line_to((100.0, 0.0));
        path
    }

    #[test]
    fn positive_offset_expands_square() {
        let out = offset_bez_path(&square_path(), 10.0, 0.5).unwrap();
        let bb = out.bounding_box();

        assert!(bb.x0 < -9.0, "x0 = {}", bb.x0);
        assert!(bb.y0 < -9.0, "y0 = {}", bb.y0);
        assert!(bb.x1 > 109.0, "x1 = {}", bb.x1);
        assert!(bb.y1 > 109.0, "y1 = {}", bb.y1);
    }

    #[test]
    fn negative_offset_insets_square() {
        let out = offset_bez_path(&square_path(), -10.0, 0.5).unwrap();
        let bb = out.bounding_box();

        assert!(bb.x0 > 9.0, "x0 = {}", bb.x0);
        assert!(bb.y0 > 9.0, "y0 = {}", bb.y0);
        assert!(bb.x1 < 91.0, "x1 = {}", bb.x1);
        assert!(bb.y1 < 91.0, "y1 = {}", bb.y1);
    }

    #[test]
    fn offset_preserves_closedness() {
        let out = offset_bez_path(&square_path(), 5.0, 0.5).unwrap();

        assert!(matches!(out.elements().last(), Some(PathEl::ClosePath)));
    }

    #[test]
    fn open_line_offsets_left_for_positive_amount() {
        let out = offset_bez_path(&line_path(), 10.0, 0.5).unwrap();
        let bb = out.bounding_box();

        assert!(bb.y0 > 9.0 && bb.y1 > 9.0, "bb = {:?}", bb);
    }

    #[test]
    fn zero_offset_is_identity() {
        let path = square_path();
        let out = offset_bez_path(&path, 0.0, 0.5).unwrap();
        assert_eq!(out.elements(), path.elements());
    }

    #[test]
    fn invalid_offset_returns_none() {
        assert!(offset_bez_path(&square_path(), f64::NAN, 0.5).is_none());
    }
}
