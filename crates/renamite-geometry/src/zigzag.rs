//! ZigZag path modifier: sample by arc length, offset along normals.
//! Corner mode = polyline peaks; smooth mode = cubic wave approx.

use kurbo::{BezPath, ParamCurve, ParamCurveArclen, PathEl, PathSeg, Point, Vec2};

const TOL: f64 = 1e-3;

pub fn zigzag_path(path: &BezPath, amplitude: f64, ridges: f64, smooth: bool) -> BezPath {
    if !amplitude.is_finite() || amplitude.abs() < 1e-9 || !ridges.is_finite() || !path.is_finite()
    {
        return path.clone();
    }
    let ridges_total = ridges.round().clamp(0.0, 1024.0) as usize;
    if ridges_total == 0 {
        return path.clone();
    }

    let subpaths = split_subpaths(path);
    if subpaths.is_empty() {
        return path.clone();
    }
    let lengths = subpaths
        .iter()
        .map(|(segments, _)| {
            segments
                .iter()
                .map(|segment| segment.arclen(TOL))
                .sum::<f64>()
        })
        .collect::<Vec<_>>();
    let grand_total: f64 = lengths.iter().sum();
    if !grand_total.is_finite() || grand_total < 1e-9 {
        return path.clone();
    }

    let allocations = allocate_ridges(&lengths, ridges_total);
    let mut out = BezPath::new();
    for (((segments, closed), sub_total), n_sub) in
        subpaths.iter().zip(lengths.iter()).zip(allocations.iter())
    {
        if !sub_total.is_finite() || *sub_total < 1e-9 {
            continue;
        }
        let n_sub = *n_sub;
        if n_sub == 0 {
            append_subpath(&mut out, segments, *closed);
            continue;
        }
        let lens = segments
            .iter()
            .map(|segment| segment.arclen(TOL))
            .collect::<Vec<_>>();
        let per_seg = distribute_by_length(&lens, n_sub);
        let mut sign = 1.0_f64;
        let mut first_in_subpath = true;
        for (seg, n) in segments.iter().zip(per_seg.iter()) {
            let n = *n;
            if n == 0 {
                let p = seg.eval(1.0);
                if first_in_subpath {
                    out.move_to(p);
                    first_in_subpath = false;
                } else {
                    out.line_to(p);
                }
                continue;
            }
            let mut samples = Vec::with_capacity(n + 1);
            let seg_len = seg.arclen(TOL);
            let mut carry = Vec2::new(1.0, 0.0);
            for i in 0..=n {
                let t_arc = arclen_to_t(seg, seg_len * i as f64 / n as f64, seg_len);
                let tangent = unit_tangent(seg, t_arc, carry);
                carry = tangent;
                samples.push((seg.eval(t_arc), tangent));
            }

            let (p0, _) = samples[0];
            if first_in_subpath {
                out.move_to(p0);
                first_in_subpath = false;
            } else {
                out.line_to(p0);
            }

            for i in 0..n {
                let (p_start, start_tangent) = samples[i];
                let (p_end, end_tangent) = samples[i + 1];
                let t_arc = arclen_to_t(seg, seg_len * (i as f64 + 0.5) / n as f64, seg_len);
                let p_mid = seg.eval(t_arc);
                let tangent = unit_tangent(seg, t_arc, start_tangent);
                let normal = Vec2::new(-tangent.y, tangent.x);
                let peak = Point::new(
                    p_mid.x + normal.x * amplitude * sign,
                    p_mid.y + normal.y * amplitude * sign,
                );
                sign = -sign;

                if smooth {
                    let h = ((p_end.x - p_start.x).powi(2) + (p_end.y - p_start.y).powi(2)).sqrt()
                        * 0.25;
                    let c1 = Point::new(
                        p_start.x + start_tangent.x * h,
                        p_start.y + start_tangent.y * h,
                    );
                    let c2 = Point::new(peak.x - tangent.x * h, peak.y - tangent.y * h);
                    out.curve_to(c1, c2, peak);
                    let c3 = Point::new(peak.x + tangent.x * h, peak.y + tangent.y * h);
                    let c4 = Point::new(p_end.x - end_tangent.x * h, p_end.y - end_tangent.y * h);
                    out.curve_to(c3, c4, p_end);
                } else {
                    out.line_to(peak);
                    out.line_to(p_end);
                }
            }
        }
        if *closed {
            out.close_path();
        }
    }
    if out.elements().is_empty() {
        return path.clone();
    }
    out
}

fn append_subpath(out: &mut BezPath, segments: &[PathSeg], closed: bool) {
    let Some(first) = segments.first() else {
        return;
    };
    out.move_to(first.start());
    for segment in segments {
        match segment {
            PathSeg::Line(line) => out.line_to(line.p1),
            PathSeg::Quad(quad) => out.quad_to(quad.p1, quad.p2),
            PathSeg::Cubic(cubic) => out.curve_to(cubic.p1, cubic.p2, cubic.p3),
        }
    }
    if closed {
        out.close_path();
    }
}

fn allocate_ridges(lengths: &[f64], total: usize) -> Vec<usize> {
    if lengths.is_empty() || total == 0 {
        return vec![0; lengths.len()];
    }
    let sum = lengths
        .iter()
        .filter(|length| length.is_finite())
        .sum::<f64>();
    if !sum.is_finite() || sum <= 0.0 {
        return vec![0; lengths.len()];
    }
    let raw = lengths
        .iter()
        .map(|length| {
            if length.is_finite() && *length > 0.0 {
                total as f64 * *length / sum
            } else {
                0.0
            }
        })
        .collect::<Vec<_>>();
    let mut output = raw
        .iter()
        .map(|value| value.floor() as usize)
        .collect::<Vec<_>>();
    let mut assigned = output.iter().sum::<usize>();
    let mut order = (0..raw.len()).collect::<Vec<_>>();
    order.sort_by(|a, b| {
        let left = raw[*a].fract();
        let right = raw[*b].fract();
        right.total_cmp(&left)
    });
    let mut cursor = 0usize;
    let mut attempts = 0usize;
    while assigned < total
        && !order.is_empty()
        && attempts
            <= total
                .saturating_mul(order.len())
                .saturating_add(order.len())
    {
        attempts += 1;
        let index = order[cursor % order.len()];
        if lengths[index].is_finite() && lengths[index] > 0.0 {
            output[index] += 1;
            assigned += 1;
        }
        cursor += 1;
    }
    output
}

fn distribute_by_length(lens: &[f64], total: usize) -> Vec<usize> {
    if lens.is_empty() || total == 0 {
        return vec![0; lens.len()];
    }
    let sum: f64 = lens.iter().filter(|length| length.is_finite()).sum();
    if !sum.is_finite() || sum < 1e-12 {
        return vec![0; lens.len()];
    }
    let mut out = lens
        .iter()
        .map(|length| {
            if length.is_finite() {
                (total as f64 * length / sum).floor() as usize
            } else {
                0
            }
        })
        .collect::<Vec<_>>();
    let mut assigned: usize = out.iter().sum();
    let mut order = (0..lens.len())
        .filter(|index| lens[*index].is_finite() && lens[*index] >= 1e-12)
        .collect::<Vec<_>>();
    order.sort_by(|a, b| lens[*b].total_cmp(&lens[*a]));
    while assigned < total {
        let mut advanced = false;
        for &index in &order {
            out[index] += 1;
            assigned += 1;
            advanced = true;
            if assigned == total {
                break;
            }
        }
        if !advanced {
            break;
        }
    }
    out
}

fn split_subpaths(path: &BezPath) -> Vec<(Vec<PathSeg>, bool)> {
    let mut out = Vec::new();
    let mut current = BezPath::new();
    let mut closed = false;
    let flush = |out: &mut Vec<(Vec<PathSeg>, bool)>, current: &mut BezPath, closed: bool| {
        if current.elements().is_empty() {
            return;
        }
        let segments = current.segments().collect::<Vec<_>>();
        if !segments.is_empty() {
            out.push((segments, closed));
        }
        *current = BezPath::new();
    };
    for element in path.elements().iter().copied() {
        match element {
            PathEl::MoveTo(_) => {
                if !current.elements().is_empty() {
                    flush(&mut out, &mut current, closed);
                }
                closed = false;
                current.push(element);
            }
            PathEl::ClosePath => {
                current.push(element);
                flush(&mut out, &mut current, true);
                closed = false;
            }
            PathEl::LineTo(_) | PathEl::QuadTo(_, _) | PathEl::CurveTo(_, _, _) => {
                current.push(element)
            }
        }
    }
    flush(&mut out, &mut current, closed);
    out
}

fn unit_tangent(seg: &PathSeg, t: f64, fallback: Vec2) -> Vec2 {
    let eps = 1e-4;
    let ranges = [((t - eps).max(0.0), t), (t, (t + eps).min(1.0)), (0.0, 1.0)];
    for (a, b) in ranges {
        let p0 = seg.eval(a);
        let p1 = seg.eval(b);
        let d = Vec2::new(p1.x - p0.x, p1.y - p0.y);
        let length = d.length();
        if length.is_finite() && length > 1e-12 {
            return Vec2::new(d.x / length, d.y / length);
        }
    }
    fallback
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
