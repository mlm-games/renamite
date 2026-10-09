//! Variable-width strokes: the width curve, and the contour subdivision that
//! lets a per-vertex stroke tessellator honour it.

use kurbo::{ParamCurveArclen, PathEl};

use crate::{DVec2, pt};

/// Maximum width scale a profile point may hold, so a stored profile cannot
/// describe an absurdly thick stroke.
pub const MAX_WIDTH_SCALE: f64 = 64.0;

/// Width multiplier along a contour, sampled by normalized arclength.
///
/// Positions are sorted, clamped to `0..=1` and deduplicated, with synthetic
/// `0` and `1` ends so a curve always spans the whole contour.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WidthCurve {
    points: Vec<(f64, f64)>,
}

/// Sanitize `(position, scale)` pairs into a [`WidthCurve`].
///
/// `None` when the pairs cannot describe a range: nothing usable survives, or
/// every usable position is the same.
pub fn width_curve(raw: &[(f64, f64)]) -> Option<WidthCurve> {
    let mut points: Vec<(f64, f64)> = Vec::with_capacity(raw.len() + 2);
    for &(at, scale) in raw {
        if !at.is_finite() || !scale.is_finite() {
            continue;
        }
        points.push((at.clamp(0.0, 1.0), scale.clamp(0.0, MAX_WIDTH_SCALE)));
    }
    if points.is_empty() {
        return None;
    }
    points.sort_by(|a, b| a.0.total_cmp(&b.0));
    // dedup_by keeps the last of equal neighbours: two points dragged onto the
    // same position behave like the live editor's last write.
    points.dedup_by(|later, earlier| {
        if (later.0 - earlier.0).abs() <= 1e-9 {
            earlier.1 = later.1;
            true
        } else {
            false
        }
    });
    if points.len() < 2 {
        return None;
    }
    if points[0].0 > 0.0 {
        points.insert(0, (0.0, points[0].1));
    }
    if points[points.len() - 1].0 < 1.0 {
        let end = points[points.len() - 1].1;
        points.push((1.0, end));
    }
    Some(WidthCurve { points })
}

impl WidthCurve {
    /// Scale at contour parameter `t`, linearly interpolated between points.
    pub fn scale_at(&self, t: f64) -> f64 {
        let t = if t.is_finite() {
            t.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (mut low, mut high) = match (self.points.first(), self.points.last()) {
            (Some(first), Some(last)) => (*first, *last),
            _ => return 1.0,
        };
        for window in self.points.windows(2) {
            if t <= window[1].0 {
                low = window[0];
                high = window[1];
                break;
            }
        }
        if (high.0 - low.0).abs() <= 1e-12 {
            return low.1;
        }
        let f = (t - low.0) / (high.0 - low.0);
        low.1 + (high.1 - low.1) * f
    }

    /// Largest scale in the curve: the width bound a flat stroke would need to
    /// cover the profile.
    pub fn max_scale(&self) -> f64 {
        self.points
            .iter()
            .map(|(_, scale)| *scale)
            .fold(0.0, f64::max)
            .clamp(0.0, MAX_WIDTH_SCALE)
    }

    /// A curve that never varies the stroke width, so callers can stay on the
    /// cheaper constant-width path.
    pub fn is_flat(&self) -> bool {
        let first = self.points.first().map(|p| p.1).unwrap_or(1.0);
        self.points
            .iter()
            .all(|(_, scale)| (scale - first).abs() <= 1e-9)
    }

    pub fn points(&self) -> &[(f64, f64)] {
        &self.points
    }
}

/// Segment shape between two endpoints of a [`WidthContour`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WidthSegment {
    Line,
    Quad(DVec2),
    Cubic(DVec2, DVec2),
}

/// One contour, cut at the width curve's breakpoints.
///
/// `ends` holds one entry per endpoint with the width scale there, so
/// `ends.len() == segments.len() + 1` and `segments[i]` runs from `ends[i]` to
/// `ends[i + 1]`.
#[derive(Clone, Debug)]
pub struct WidthContour {
    pub closed: bool,
    pub ends: Vec<(DVec2, f64)>,
    pub segments: Vec<WidthSegment>,
}

/// Split every contour of `path` at the curve's breakpoints, reporting the
/// width scale at each resulting endpoint.
///
/// Splitting is what makes a width profile drawable: a per-vertex width is
/// interpolated along the segment, so vertices have to exist where the profile
/// bends. Contour order follows `path`; contours with no length are dropped.
pub fn width_contours(
    path: &kurbo::BezPath,
    curve: &WidthCurve,
    tolerance: f64,
) -> Vec<WidthContour> {
    let tolerance = if tolerance.is_finite() && tolerance > 0.0 {
        tolerance
    } else {
        0.01
    };

    let mut raw = Vec::new();
    let mut open: Option<RawContour> = None;
    let mut cursor = DVec2::ZERO;
    let mut start = DVec2::ZERO;

    for element in path.elements() {
        match *element {
            PathEl::MoveTo(p) => {
                if let Some(contour) = open.take() {
                    raw.push(contour);
                }
                start = DVec2::new(p.x, p.y);
                cursor = start;
                open = Some(RawContour {
                    start,
                    segments: Vec::new(),
                    closed: false,
                    length: 0.0,
                });
            }
            PathEl::LineTo(p) => {
                let Some(contour) = open.as_mut() else {
                    continue;
                };
                let to = DVec2::new(p.x, p.y);
                let s0 = contour.length;
                let s1 = s0 + (to - cursor).length();
                contour.push(RawSegment {
                    shape: WidthSegment::Line,
                    from: cursor,
                    to,
                    s0,
                    s1,
                });
                cursor = to;
            }
            PathEl::QuadTo(c, p) => {
                let Some(contour) = open.as_mut() else {
                    continue;
                };
                let to = DVec2::new(p.x, p.y);
                let ctrl = DVec2::new(c.x, c.y);
                let s0 = contour.length;
                let seg = kurbo::QuadBez::new(pt(cursor), pt(ctrl), pt(to));
                let s1 = s0 + seg.arclen(tolerance);
                contour.push(RawSegment {
                    shape: WidthSegment::Quad(ctrl),
                    from: cursor,
                    to,
                    s0,
                    s1,
                });
                cursor = to;
            }
            PathEl::CurveTo(c1, c2, p) => {
                let Some(contour) = open.as_mut() else {
                    continue;
                };
                let to = DVec2::new(p.x, p.y);
                let a = DVec2::new(c1.x, c1.y);
                let b = DVec2::new(c2.x, c2.y);
                let s0 = contour.length;
                let seg = kurbo::CubicBez::new(pt(cursor), pt(a), pt(b), pt(to));
                let s1 = s0 + seg.arclen(tolerance);
                contour.push(RawSegment {
                    shape: WidthSegment::Cubic(a, b),
                    from: cursor,
                    to,
                    s0,
                    s1,
                });
                cursor = to;
            }
            PathEl::ClosePath => {
                let Some(contour) = open.as_mut() else {
                    continue;
                };
                contour.closed = true;
                if (cursor - start).length() > 1e-12 {
                    let s0 = contour.length;
                    let s1 = s0 + (cursor - start).length();
                    contour.push(RawSegment {
                        shape: WidthSegment::Line,
                        from: cursor,
                        to: start,
                        s0,
                        s1,
                    });
                    cursor = start;
                }
                if let Some(contour) = open.take() {
                    raw.push(contour);
                }
            }
        }
    }
    if let Some(contour) = open.take() {
        raw.push(contour);
    }

    raw.iter().filter_map(|c| split_contour(c, curve)).collect()
}

/// A contour as the path carries it, before the profile splits it.
struct RawContour {
    start: DVec2,
    segments: Vec<RawSegment>,
    closed: bool,
    length: f64,
}

impl RawContour {
    fn push(&mut self, segment: RawSegment) {
        self.length = segment.s1;
        self.segments.push(segment);
    }
}

#[derive(Clone, Copy)]
struct RawSegment {
    shape: WidthSegment,
    from: DVec2,
    to: DVec2,
    s0: f64,
    s1: f64,
}

/// Cut one raw contour at the curve's breakpoints.
///
/// Continuity of the polyline the cut creates: the segment the previous piece
/// ended at becomes the start of the next one.
fn split_contour(raw: &RawContour, curve: &WidthCurve) -> Option<WidthContour> {
    let total = raw.segments.last().map(|s| s.s1).unwrap_or(0.0);
    if raw.segments.is_empty() || !total.is_finite() || total <= 1e-12 {
        return None;
    }
    let mut cuts: Vec<f64> = curve
        .points()
        .iter()
        .map(|(at, _)| at * total)
        .filter(|s| *s > 1e-9 && *s < total - 1e-9)
        .collect();
    cuts.sort_by(|a, b| a.total_cmp(b));

    let mut ends = vec![(raw.start, curve.scale_at(0.0))];
    let mut segments: Vec<WidthSegment> = Vec::with_capacity(raw.segments.len() + cuts.len());

    for segment in &raw.segments {
        let mut piece = *segment;
        let mut lo = segment.s0;
        for &at in cuts
            .iter()
            .filter(|&&at| at > segment.s0 && at < segment.s1)
        {
            let f = ((at - lo) / (piece.s1 - lo)).clamp(0.0, 1.0);
            let (left, right) = split_segment(&piece, f);
            segments.push(left.shape);
            ends.push((left.to, curve.scale_at(at / total)));
            piece = right;
            lo = at;
        }
        segments.push(piece.shape);
        ends.push((segment.to, curve.scale_at(segment.s1 / total)));
    }

    Some(WidthContour {
        closed: raw.closed,
        ends,
        segments,
    })
}

/// Split a segment at parameter `f` into two that span the same curve.
fn split_segment(segment: &RawSegment, f: f64) -> (RawSegment, RawSegment) {
    let span = |from: DVec2, to: DVec2| RawSegment {
        from,
        to,
        ..*segment
    };
    match segment.shape {
        WidthSegment::Line => {
            let to = segment.from + (segment.to - segment.from) * f;
            (span(segment.from, to), span(to, segment.to))
        }
        WidthSegment::Quad(ctrl) => {
            let (p0, p3) = (pt(segment.from), pt(segment.to));
            let (m0, m1) = (p0.lerp(pt(ctrl), f), pt(ctrl).lerp(p3, f));
            let mid = m0.lerp(m1, f);
            (
                span(segment.from, dvec(mid)).with_quad(dvec(m0)),
                span(dvec(mid), segment.to).with_quad(dvec(m1)),
            )
        }
        WidthSegment::Cubic(c1, c2) => {
            let (p0, p3) = (pt(segment.from), pt(segment.to));
            let (a, b, c) = (
                p0.lerp(pt(c1), f),
                pt(c1).lerp(pt(c2), f),
                pt(c2).lerp(p3, f),
            );
            let (d, e) = (a.lerp(b, f), b.lerp(c, f));
            let mid = d.lerp(e, f);
            (
                span(segment.from, dvec(mid)).with_cubic(dvec(a), dvec(d)),
                span(dvec(mid), segment.to).with_cubic(dvec(e), dvec(c)),
            )
        }
    }
}

impl RawSegment {
    fn with_quad(mut self, ctrl: DVec2) -> Self {
        self.shape = WidthSegment::Quad(ctrl);
        self
    }

    fn with_cubic(mut self, c1: DVec2, c2: DVec2) -> Self {
        self.shape = WidthSegment::Cubic(c1, c2);
        self
    }
}

fn dvec(p: kurbo::Point) -> DVec2 {
    DVec2::new(p.x, p.y)
}

/// How the outline rounds off: the caps, the joins and the miter limit of the
/// stroke it stands for.
#[derive(Clone, Copy, Debug)]
pub struct OutlineStyle {
    pub cap: kurbo::Cap,
    pub join: kurbo::Join,
    pub miter_limit: f64,
}

/// The filled outline of a variable-width stroke: the polygon that covers what
/// the stroke paints, for every sink that can only carry geometry (expand
/// stroke, static SVG export).
///
/// Both sides of the flattened contour are offset by half the local width and
/// joined as a flat stroker would - miter inside the limit, an arc for round,
/// two points otherwise - so joins and caps read the same at any width. A
/// direction reversal clamps its width to the neighbouring segment, which keeps
/// the two offset chains from crossing into each other.
///
/// An empty result means nothing drawable (no width, no length).
pub fn stroke_outline(
    path: &kurbo::BezPath,
    curve: &WidthCurve,
    width: f64,
    style: OutlineStyle,
    tolerance: f64,
) -> Vec<kurbo::BezPath> {
    let width = if width.is_finite() {
        width.max(0.0)
    } else {
        0.0
    };
    let tolerance = if tolerance.is_finite() && tolerance > 0.0 {
        tolerance
    } else {
        0.01
    };
    let mut out = Vec::new();
    if width <= 0.0 {
        return out;
    }
    for contour in crate::flatten_bez_path(path, tolerance) {
        if let Some(polygon) = outline_contour(&contour, curve, width, style, tolerance) {
            let polygon = simplify(polygon, tolerance);
            if polygon.segments().count() >= 2 {
                out.push(polygon);
            }
        }
    }
    out
}

/// One flattened contour to its offset polygon.
fn outline_contour(
    contour: &crate::FlatContour,
    curve: &WidthCurve,
    width: f64,
    style: OutlineStyle,
    tolerance: f64,
) -> Option<kurbo::BezPath> {
    let points = &contour.points;
    let n = points.len();
    if n < 2 {
        return None;
    }
    let closed = contour.closed && n >= 3;

    // Cumulative arclength, so the curve is sampled where it is walked.
    let mut s = Vec::with_capacity(n);
    s.push(0.0);
    for i in 1..n {
        s.push(s[i - 1] + (points[i] - points[i - 1]).length());
    }
    let total = s[n - 1];
    if !total.is_finite() || total <= 1e-12 {
        return None;
    }

    // Central-difference directions; open ends borrow their own side.
    let dir = |i: usize| -> DVec2 {
        let (a, b) = match i {
            0 if !closed => (0, 1),
            i if i + 1 == n && !closed => (n - 2, n - 1),
            i => ((i + n - 1) % n, (i + 1) % n),
        };
        let v = points[b] - points[a];
        if v.length() <= 1e-12 {
            return DVec2::X;
        }
        v.normalize()
    };
    let dirs: Vec<DVec2> = (0..n).map(dir).collect();

    let mut half: Vec<f64> = (0..n)
        .map(|i| width * curve.scale_at(s[i] / total) * 0.5)
        .collect();
    for i in 0..n {
        // A near-reversal folds the outline onto itself; cap the width at what
        // the neighbouring segments can hold.
        let (a, b) = if closed {
            ((i + n - 1) % n, (i + 1) % n)
        } else {
            (i.saturating_sub(1), (i + 1).min(n - 1))
        };
        if dirs[i].dot(dirs[(a + 1) % n.max(1)]) < 0.0 || dirs[i].dot(dirs[b]) < 0.0 {
            let limit = (points[i] - points[a])
                .length()
                .min((points[b] - points[i]).length());
            half[i] = half[i].min(limit);
        }
    }

    let left = offset_chain(points, &dirs, &half, 1.0, closed, style, tolerance);
    let right = offset_chain(points, &dirs, &half, -1.0, closed, style, tolerance);
    if left.len() < 2 || right.len() < 2 {
        return None;
    }

    let mut ring: Vec<DVec2> = Vec::with_capacity(left.len() + right.len() + 4);
    ring.extend_from_slice(&left);
    if !closed {
        // End cap, back along the other side, then the start cap.
        let (end, start) = (n - 1, 0);
        match style.cap {
            kurbo::Cap::Round => {
                let hint = points[end] + dirs[end] * half[end];
                arc_points(
                    &mut ring,
                    points[end],
                    half[end],
                    left[end],
                    right[end],
                    hint,
                    tolerance,
                );
            }
            kurbo::Cap::Square => {
                ring.push(left[end] + dirs[end] * half[end]);
                ring.push(right[end] + dirs[end] * half[end]);
            }
            kurbo::Cap::Butt => {}
        }
        ring.extend(right.iter().rev());
        match style.cap {
            kurbo::Cap::Round => {
                let hint = points[start] - dirs[start] * half[start];
                arc_points(
                    &mut ring,
                    points[start],
                    half[start],
                    right[start],
                    left[start],
                    hint,
                    tolerance,
                );
            }
            kurbo::Cap::Square => {
                ring.push(right[start] - dirs[start] * half[start]);
                ring.push(left[start] - dirs[start] * half[start]);
            }
            kurbo::Cap::Butt => {}
        }
    } else {
        ring.extend(right.iter().rev());
    }

    let mut out = kurbo::BezPath::new();
    out.move_to(pt(ring[0]));
    for p in &ring[1..] {
        out.line_to(pt(*p));
    }
    out.close_path();
    Some(out)
}

/// Offset polyline of one side, with joins at every direction change.
fn offset_chain(
    points: &[DVec2],
    dirs: &[DVec2],
    half: &[f64],
    side: f64,
    closed: bool,
    style: OutlineStyle,
    tolerance: f64,
) -> Vec<DVec2> {
    let n = points.len();
    let offset = |i: usize| -> DVec2 {
        let d = dirs[i];
        points[i] + DVec2::new(-d.y, d.x) * side * half[i]
    };
    let mut out = Vec::with_capacity(n + 4);
    for i in 0..n {
        let here = offset(i);
        let previous = if i == 0 && closed {
            n - 1
        } else {
            i.saturating_sub(1)
        };
        if i > 0 || closed {
            let turn = dirs[previous].x * dirs[i].y - dirs[previous].y * dirs[i].x;
            let before = offset(previous);
            if turn * side > 0.0 {
                match style.join {
                    kurbo::Join::Bevel => out.push(here),
                    kurbo::Join::Miter => {
                        match line_x_line(before, dirs[previous], here, dirs[i]) {
                            Some(m)
                                if (m - points[i]).length()
                                    <= style.miter_limit * half[i].max(1e-9) =>
                            {
                                out.push(m)
                            }
                            _ => out.push(here),
                        }
                    }
                    kurbo::Join::Round => {
                        let bisector =
                            ((before - points[i]) + (here - points[i])).normalize_or_zero();
                        arc_points(
                            &mut out,
                            points[i],
                            half[i],
                            before,
                            here,
                            points[i] + bisector * half[i],
                            tolerance,
                        );
                        out.push(here);
                    }
                }
                continue;
            }
        }
        out.push(here);
    }
    out
}

/// Where two lines cross; `None` when they run parallel.
fn line_x_line(p0: DVec2, d0: DVec2, p1: DVec2, d1: DVec2) -> Option<DVec2> {
    let den = d0.x * d1.y - d0.y * d1.x;
    let delta = p1 - p0;
    if !den.is_finite() || den.abs() <= 1e-12 {
        return None;
    }
    let hit = p0 + d0 * ((delta.x * d1.y - delta.y * d1.x) / den);
    (hit.is_finite()).then_some(hit)
}

/// Arc from `from` to `to` about `center`, taking the way through `hint`.
/// Pushes the intermediate points only.
fn arc_points(
    out: &mut Vec<DVec2>,
    center: DVec2,
    radius: f64,
    from: DVec2,
    to: DVec2,
    hint: DVec2,
    tolerance: f64,
) {
    if !radius.is_finite() || radius <= 1e-9 {
        return;
    }
    let angle = |v: DVec2| (v - center).y.atan2((v - center).x);
    let (a0, a1, ah) = (angle(from), angle(to), angle(hint));
    let two_pi = std::f64::consts::TAU;
    let wrap = |a: f64| ((a % two_pi) + two_pi) % two_pi;
    let positive = wrap(a1 - a0);
    let span = if wrap(ah - a0) <= positive {
        positive
    } else {
        positive - two_pi
    };
    let steps = ((span.abs() * radius / tolerance.max(1e-6)).ceil() as usize).clamp(1, 512);
    for step in 1..steps {
        let a = a0 + span * step as f64 / steps as f64;
        let p = center + DVec2::new(a.cos(), a.sin()) * radius;
        if p.is_finite() {
            out.push(p);
        }
    }
}

/// Drop the points of an outline polygon that a straight edge already covers.
fn simplify(polygon: kurbo::BezPath, tolerance: f64) -> kurbo::BezPath {
    let tolerance = if tolerance.is_finite() && tolerance > 0.0 {
        tolerance
    } else {
        0.01
    };
    kurbo::simplify::simplify_bezpath(
        polygon.elements().iter().copied(),
        tolerance.max(1e-4),
        &kurbo::simplify::SimplifyOptions::default(),
    )
}
