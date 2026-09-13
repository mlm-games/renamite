//! ZigZag path modifier: sample by arc length, offset along normals.
//! Corner mode = polyline peaks; smooth mode = cubic wave approx.

use kurbo::{BezPath, ParamCurve, ParamCurveArclen, PathEl, PathSeg, Point, Vec2};

const TOL: f64 = 1e-3;

pub fn zigzag_path(path: &BezPath, amplitude: f64, ridges: f64, smooth: bool) -> BezPath {
    if !amplitude.is_finite() || amplitude.abs() < 1e-9 {
        return path.clone();
    }
    if !ridges.is_finite() {
        return path.clone();
    }
    let ridges_total = (ridges.floor().max(0.0).min(1024.0)) as usize;
    if ridges_total == 0 {
        return path.clone();
    }

    let subpaths = split_subpaths(path);
    if subpaths.is_empty() {
        return path.clone();
    }
    let grand_total: f64 = subpaths
        .iter()
        .map(|(segs, _)| segs.iter().map(|s| s.arclen(TOL)).sum::<f64>())
        .sum();
    if grand_total < 1e-9 {
        return path.clone();
    }

    let mut out = BezPath::new();
    let mut sign = 1.0_f64;

    for (segs, closed) in &subpaths {
        let lens: Vec<f64> = segs.iter().map(|s| s.arclen(TOL)).collect();
        let sub_total: f64 = lens.iter().sum();
        if sub_total < 1e-9 {
            continue;
        }
        let n_sub = ((ridges_total as f64 * sub_total / grand_total).round() as usize).max(1);
        let mut per_seg = distribute_by_length(&lens, n_sub);
        if per_seg.iter().sum::<usize>() == 0 {
            if let Some(idx) = lens
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                .map(|(i, _)| i)
            {
                per_seg[idx] = 1;
            }
        }
        let mut first_in_subpath = true;
        for (seg, n) in segs.iter().zip(per_seg.iter()) {
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
            let mut samples: Vec<(Point, Vec2)> = Vec::with_capacity(n + 1);

            for i in 0..=n {
                let t = i as f64 / n as f64;
                let seg_len = seg.arclen(TOL);
                let target = seg_len * t;
                let t_arc = arclen_to_t(seg, target, seg_len);
                let p = seg.eval(t_arc);
                let d = tangent_at(seg, t_arc);
                let dl = d.length();
                let tangent = if dl > 1e-12 {
                    Vec2::new(d.x / dl, d.y / dl)
                } else {
                    Vec2::new(1.0, 0.0)
                };
                let normal = Vec2::new(-tangent.y, tangent.x);
                samples.push((p, normal));
            }

            let (p0, _) = samples[0];
            if first_in_subpath {
                out.move_to(p0);
                first_in_subpath = false;
            } else {
                out.line_to(p0);
            }

            for i in 0..n {
                let (p_start, _) = samples[i];
                let (p_end, _) = samples[i + 1];
                let mid_t = (i as f64 + 0.5) / n as f64;
                let seg_len = seg.arclen(TOL);
                let t_arc = arclen_to_t(seg, seg_len * mid_t, seg_len);
                let p_mid = seg.eval(t_arc);
                let d = tangent_at(seg, t_arc);
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
                    let h = ((p_end.x - p_start.x).powi(2) + (p_end.y - p_start.y).powi(2)).sqrt()
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
        if *closed {
            out.close_path();
        }
    }
    if out.elements().is_empty() {
        return path.clone();
    }
    out
}

fn distribute_by_length(lens: &[f64], total: usize) -> Vec<usize> {
    if lens.is_empty() || total == 0 {
        return vec![0; lens.len()];
    }
    let sum: f64 = lens.iter().sum();
    if sum < 1e-12 {
        return vec![0; lens.len()];
    }
    let mut out: Vec<usize> = lens
        .iter()
        .map(|l| (total as f64 * l / sum).floor() as usize)
        .collect();
    let mut assigned: usize = out.iter().sum();
    let mut order: Vec<usize> = (0..lens.len()).collect();
    order.sort_by(|a, b| lens[*b].partial_cmp(&lens[*a]).unwrap());
    let mut i = 0;
    while assigned < total && !order.is_empty() {
        let mut advanced = false;
        for &idx in &order {
            if assigned >= total {
                break;
            }
            if lens[idx] < 1e-12 {
                continue;
            }
            out[idx] += 1;
            assigned += 1;
            advanced = true;
            if assigned >= total {
                break;
            }
        }
        if !advanced {
            break;
        }
        i += 1;
        if i > total + lens.len() {
            break;
        }
    }
    out
}

fn split_subpaths(path: &BezPath) -> Vec<(Vec<PathSeg>, bool)> {
    let mut out: Vec<(Vec<PathSeg>, bool)> = Vec::new();
    let mut cur: Vec<PathSeg> = Vec::new();
    let mut closed = false;
    for seg in path.segments() {
        if let Some(last) = cur.last() {
            let prev_end = last.eval(1.0);
            let cur_start = seg.eval(0.0);
            let dx = prev_end.x - cur_start.x;
            let dy = prev_end.y - cur_start.y;
            if dx.hypot(dy) > 1e-6 {
                out.push((std::mem::take(&mut cur), closed));
                closed = false;
            }
        }
        cur.push(seg);
    }
    let closes = subpath_close_flags(path);
    if !cur.is_empty() {
        out.push((cur, false));
    }
    for (i, c) in closes.into_iter().enumerate() {
        if let Some(entry) = out.get_mut(i) {
            entry.1 = c;
        }
    }
    if out.len() == 1
        && path
            .elements()
            .last()
            .is_some_and(|e| matches!(e, PathEl::ClosePath))
    {
        out[0].1 = true;
    }
    out
}

fn subpath_close_flags(path: &BezPath) -> Vec<bool> {
    let mut flags = Vec::new();
    let mut has_geom = false;
    for el in path.elements() {
        match el {
            PathEl::MoveTo(_) => {
                if has_geom {
                    flags.push(false);
                }
                has_geom = false;
            }
            PathEl::ClosePath => {
                flags.push(true);
                has_geom = false;
            }
            _ => {
                has_geom = true;
            }
        }
    }
    if has_geom {
        flags.push(false);
    }
    flags
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
