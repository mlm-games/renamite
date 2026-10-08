//! Topology matching: makes two contours of differing anchor counts
//! correspond anchor by anchor so they can be tweened instead of snapped.

use std::cmp::Ordering;

use kurbo::{CubicBez, ParamCurve, ParamCurveArclen};

use crate::{Anchor, VectorPath, detect_mode, pt};

/// A grown path stays inside the anchor budget `renamite-validate` enforces on
/// stored documents (`MAX_PATH_ANCHORS`).
const MAX_MORPH_ANCHORS: usize = 100_000;

/// Anchor count up to which every start-anchor rotation is scored. The search
/// runs on every frame of a scrub, so past this only the direction is chosen.
const ROTATION_SEARCH_MAX: usize = 512;

/// `a` and `b` prepared for a per-anchor tween: the same anchor count, and `b`
/// turned to start where it pairs closest to `a`.
///
/// `None` means no sane morph exists, so the caller should hold instead: the
/// paths disagree on `closed`, are too small to have a segment to split, carry
/// non-finite coordinates, or would have to grow past the anchor budget.
pub fn match_topology(a: &VectorPath, b: &VectorPath) -> Option<(VectorPath, VectorPath)> {
    if a.closed != b.closed || !morphable(a) || !morphable(b) {
        return None;
    }
    let target = a.anchors.len().max(b.anchors.len());
    if target > MAX_MORPH_ANCHORS {
        return None;
    }
    // Matching counts still need a correspondence: the same contour can start
    // at a different anchor, and pairing it index-wise would twist the morph
    // through a shape neither key ever had.
    if a.anchors.len() == b.anchors.len() {
        return Some((a.clone(), align(a, b)));
    }
    let grown = resample(a, target)?;
    let paired = align(&grown, &resample(b, target)?);
    Some((grown, paired))
}

fn morphable(path: &VectorPath) -> bool {
    let n = path.anchors.len();
    if n < 2 || (path.closed && n < 3) {
        return false;
    }
    path.anchors
        .iter()
        .all(|a| a.pos.is_finite() && a.tan_in.is_finite() && a.tan_out.is_finite())
}

fn dvec(p: kurbo::Point) -> glam::DVec2 {
    glam::DVec2::new(p.x, p.y)
}

fn segment_curve(path: &VectorPath, seg: usize) -> CubicBez {
    let n = path.anchors.len();
    let (a, b) = (path.anchors[seg], path.anchors[(seg + 1) % n]);
    CubicBez::new(
        pt(a.pos),
        pt(a.pos + a.tan_out),
        pt(b.pos + b.tan_in),
        pt(b.pos),
    )
}

/// Arclength error budget proportional to the path's own size, so the
/// subdivision depth behind it cannot depend on the document's units.
fn arclen_tolerance(path: &VectorPath) -> f64 {
    let scale = path.anchors.iter().fold(0.0f64, |scale, a| {
        scale.max(a.pos.x.abs()).max(a.pos.y.abs())
    });
    (scale * 1e-6).max(1e-9)
}

/// Splits per segment, shared out by arclength so the new anchors spread along
/// the contour, summing to exactly `extra`. `None` when nothing has length.
fn split_quota(arclens: &[f64], extra: usize) -> Option<Vec<usize>> {
    let total: f64 = arclens.iter().sum();
    if arclens.is_empty() || !total.is_finite() || total <= 0.0 {
        return None;
    }
    let mut quota = vec![0usize; arclens.len()];
    let mut fraction = vec![0f64; arclens.len()];
    let mut assigned = 0usize;
    for (i, &len) in arclens.iter().enumerate() {
        let share = len / total * extra as f64;
        let whole = share.floor();
        if !whole.is_finite() {
            return None;
        }
        quota[i] = whole as usize;
        fraction[i] = share - whole;
        assigned += quota[i];
    }
    // Largest remainder first, ties by lowest segment, so the result does not
    // depend on iteration order.
    let mut order: Vec<usize> = (0..arclens.len()).collect();
    order.sort_by(|&x, &y| fraction[y].total_cmp(&fraction[x]).then_with(|| x.cmp(&y)));
    let mut leftover = extra.saturating_sub(assigned);
    let mut cursor = 0usize;
    while leftover > 0 {
        quota[order[cursor % order.len()]] += 1;
        leftover -= 1;
        cursor += 1;
    }
    Some(quota)
}

/// Parameter of the `j`-th of `parts` equal-arclength cuts along `curve`.
fn split_param(curve: &CubicBez, len: f64, j: usize, parts: usize, tolerance: f64) -> f64 {
    let t = curve.inv_arclen(len * j as f64 / parts as f64, tolerance);
    if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        j as f64 / parts as f64
    }
}

/// Rebuild `path` with `target` anchors, placed at equal arclength along the
/// curve. One pass over the segments: splitting the longest segment repeatedly
/// instead costs a vector insert per anchor, which is quadratic in the target
/// and a multi-second stall once a keyframe runs to a few thousand anchors.
fn resample(path: &VectorPath, target: usize) -> Option<VectorPath> {
    let n = path.anchors.len();
    if n == target {
        return Some(path.clone());
    }
    if target > MAX_MORPH_ANCHORS || target < n {
        return None;
    }
    let tolerance = arclen_tolerance(path);
    let arclens: Vec<f64> = (0..path.segment_count())
        .map(|seg| segment_curve(path, seg).arclen(tolerance))
        .collect();
    let quota = split_quota(&arclens, target - n)?;

    let mut chain: Vec<CubicBez> = Vec::with_capacity(target);
    for (seg, &splits) in quota.iter().enumerate() {
        let curve = segment_curve(path, seg);
        let len = arclens[seg];
        let mut t_prev = 0.0f64;
        for j in 1..=splits {
            let t = split_param(&curve, len, j, splits + 1, tolerance);
            chain.push(curve.subsegment(t_prev..t));
            t_prev = t;
        }
        chain.push(curve.subsegment(t_prev..1.0));
    }

    let closing = path.closed;
    let last = chain.len().saturating_sub(1);
    let mut anchors: Vec<Anchor> = Vec::with_capacity(target);
    anchors.push(Anchor::corner(path.anchors[0].pos));
    for (i, piece) in chain.iter().enumerate() {
        // A closed contour's final segment lands back on the first anchor, so
        // its incoming tangent belongs there rather than to a duplicate anchor.
        if closing && i == last {
            if let Some(first) = anchors.first_mut() {
                first.tan_in = dvec(piece.p2) - first.pos;
            }
            continue;
        }
        if let Some(prev) = anchors.last_mut() {
            prev.tan_out = dvec(piece.p1) - prev.pos;
        }
        let mut anchor = Anchor::corner(dvec(piece.p3));
        anchor.tan_in = dvec(piece.p2) - anchor.pos;
        anchors.push(anchor);
    }
    for anchor in &mut anchors {
        anchor.mode = detect_mode(anchor.tan_in, anchor.tan_out);
    }
    Some(VectorPath {
        anchors,
        closed: path.closed,
    })
}

/// Face `b` towards `a`: pick its direction and, when closed, its start anchor,
/// so the blend between them runs the short way round instead of twisting.
fn align(a: &VectorPath, b: &VectorPath) -> VectorPath {
    let forward = best_shift(&a.anchors, &b.anchors, b.closed);
    // Distances cannot beat zero, so an already exact pairing skips the turn.
    if forward.1 > 0.0 {
        let mut reversed = b.clone();
        reversed.reverse();
        let backward = best_shift(&a.anchors, &reversed.anchors, b.closed);
        if backward.1.total_cmp(&forward.1) == Ordering::Less {
            reversed.anchors.rotate_left(backward.0);
            return reversed;
        }
    }
    let mut out = b.clone();
    out.anchors.rotate_left(forward.0);
    out
}

/// Shift of `b`'s anchors pairing each of `a's` with its nearest neighbour,
/// plus that pairing's squared distance. Open paths have no start to choose, so
/// only the direction is free.
fn best_shift(a: &[Anchor], b: &[Anchor], closed: bool) -> (usize, f64) {
    let n = a.len();
    if !closed || n > ROTATION_SEARCH_MAX || b.is_empty() {
        return (0, squared_error(a, b, 0));
    }
    let mut best = (0usize, squared_error(a, b, 0));
    for shift in 1..n {
        best = better(shift, best, a, b);
    }
    best
}

/// Lowest shift wins ties, so the result never depends on scan order.
fn better(shift: usize, best: (usize, f64), a: &[Anchor], b: &[Anchor]) -> (usize, f64) {
    let cost = squared_error(a, b, shift);
    if cost.total_cmp(&best.1) == Ordering::Less || (cost == best.1 && shift < best.0) {
        (shift, cost)
    } else {
        best
    }
}

fn squared_error(a: &[Anchor], b: &[Anchor], shift: usize) -> f64 {
    let n = b.len();
    let mut sum = 0.0;
    for (i, x) in a.iter().enumerate() {
        sum += (x.pos - b[(i + shift) % n].pos).length_squared();
    }
    sum
}
