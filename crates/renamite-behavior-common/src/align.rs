//! Pure alignment / distribution math. UI-agnostic: world-space bounds in,
//! world-space deltas out. Callers convert deltas to parent space and author
//! them via `resolve_property_edit` so Design stays static and Animate keys.

use glam::DVec2;
use kurbo::Shape as _;
use renamite_model::{Document, NodeId, SceneItem, node_is_ancestor};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignAnchor {
    Selection,
    Page,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignOp {
    Left,
    HCenter,
    Right,
    Top,
    VCenter,
    Bottom,
    Center,
}

/// Union world bounds per selected root, gathering descendant scene items so
/// groups align by their rendered contents rather than their own origin.
pub fn root_bounds(
    doc: &Document,
    items: &[SceneItem],
    roots: &[NodeId],
) -> Vec<(NodeId, DVec2, DVec2)> {
    let mut slots: Vec<(NodeId, Option<(DVec2, DVec2)>)> =
        roots.iter().map(|&id| (id, None)).collect();
    for item in items {
        let b = item.path.bounding_box();
        let (mn, mx) = (DVec2::new(b.x0, b.y0), DVec2::new(b.x1, b.y1));
        if !mn.is_finite() || !mx.is_finite() {
            continue;
        }
        for (id, slot) in slots.iter_mut() {
            if item.node == *id || node_is_ancestor(doc, *id, item.node) {
                *slot = Some(match *slot {
                    None => (mn, mx),
                    Some((a, c)) => (a.min(mn), c.max(mx)),
                });
            }
        }
    }
    slots
        .into_iter()
        .filter_map(|(id, b)| b.map(|(a, c)| (id, a, c)))
        .collect()
}

pub fn union_bounds(bounds: &[(NodeId, DVec2, DVec2)]) -> Option<(DVec2, DVec2)> {
    let mut mn = bounds.first()?.1;
    let mut mx = bounds.first()?.2;
    for (_, a, c) in &bounds[1..] {
        mn = mn.min(*a);
        mx = mx.max(*c);
    }
    Some((mn, mx))
}

pub fn page_bounds(size: (u32, u32)) -> (DVec2, DVec2) {
    (DVec2::ZERO, DVec2::new(size.0 as f64, size.1 as f64))
}

/// World-space delta per root to satisfy `op` against `target`.
pub fn align_deltas(
    bounds: &[(NodeId, DVec2, DVec2)],
    target: (DVec2, DVec2),
    op: AlignOp,
) -> Vec<(NodeId, DVec2)> {
    let (tmin, tmax) = target;
    bounds
        .iter()
        .map(|(id, mn, mx)| {
            let (dx, dy) = match op {
                AlignOp::Left => (tmin.x - mn.x, 0.0),
                AlignOp::HCenter => ((tmin.x + tmax.x) * 0.5 - (mn.x + mx.x) * 0.5, 0.0),
                AlignOp::Right => (tmax.x - mx.x, 0.0),
                AlignOp::Top => (0.0, tmin.y - mn.y),
                AlignOp::VCenter => (0.0, (tmin.y + tmax.y) * 0.5 - (mn.y + mx.y) * 0.5),
                AlignOp::Bottom => (0.0, tmax.y - mx.y),
                AlignOp::Center => (
                    (tmin.x + tmax.x) * 0.5 - (mn.x + mx.x) * 0.5,
                    (tmin.y + tmax.y) * 0.5 - (mn.y + mx.y) * 0.5,
                ),
            };
            (*id, DVec2::new(dx, dy))
        })
        .collect()
}

/// Evenly space 3+ roots between the first and last center along one axis.
/// Returns None when there is nothing to do.
pub fn distribute_deltas(
    bounds: &[(NodeId, DVec2, DVec2)],
    horizontal: bool,
) -> Option<Vec<(NodeId, DVec2)>> {
    if bounds.len() < 3 {
        return None;
    }
    let mut sorted = bounds.to_vec();
    if horizontal {
        sorted.sort_by(|a, b| {
            ((a.1.x + a.2.x) * 0.5)
                .partial_cmp(&((b.1.x + b.2.x) * 0.5))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    } else {
        sorted.sort_by(|a, b| {
            ((a.1.y + a.2.y) * 0.5)
                .partial_cmp(&((b.1.y + b.2.y) * 0.5))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    let center = |t: &(NodeId, DVec2, DVec2)| {
        if horizontal {
            (t.1.x + t.2.x) * 0.5
        } else {
            (t.1.y + t.2.y) * 0.5
        }
    };
    let c0 = center(&sorted[0]);
    let c1 = center(&sorted[sorted.len() - 1]);
    if !c0.is_finite() || !c1.is_finite() {
        return None;
    }
    let step = (c1 - c0) / (sorted.len() as f64 - 1.0);
    Some(
        sorted
            .iter()
            .enumerate()
            .map(|(i, (id, mn, mx))| {
                let cur = if horizontal {
                    (mn.x + mx.x) * 0.5
                } else {
                    (mn.y + mx.y) * 0.5
                };
                let want = c0 + step * i as f64;
                let d = if horizontal {
                    DVec2::new(want - cur, 0.0)
                } else {
                    DVec2::new(0.0, want - cur)
                };
                (*id, d)
            })
            .collect(),
    )
}
