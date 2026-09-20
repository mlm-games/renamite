use glam::DVec2;
use renamite_model::{Document, NodeId, SceneItem, node_is_ancestor};

use crate::SnapConfig;

pub const SNAP_TOLERANCE_PX: f64 = 8.0;

pub struct SnapInput<'a> {
    pub doc: &'a Document,
    pub items: &'a [SceneItem],
    pub selected: &'a [NodeId],
}

pub fn snap_point(
    config: &SnapConfig,
    input: &SnapInput<'_>,
    guides: &[(bool, f64)],
    raw: DVec2,
    tolerance_world: f64,
    exclude_anchor_of: Option<NodeId>,
) -> DVec2 {
    let mut best = raw;
    let mut best_dist = tolerance_world;
    if let Some(step) = config.grid
        && step > 1e-9
    {
        let gx = (raw.x / step).round() * step;
        let gy = (raw.y / step).round() * step;
        let d = (DVec2::new(gx, gy) - raw).length();
        if d <= best_dist {
            best_dist = d;
            best = DVec2::new(gx, gy);
        }
    }
    if config.guide {
        for &(horizontal, position) in guides {
            if !position.is_finite() {
                continue;
            }
            let candidate = if horizontal {
                DVec2::new(raw.x, position)
            } else {
                DVec2::new(position, raw.y)
            };
            let axis_dist = (candidate - raw).length();
            if axis_dist <= best_dist {
                best_dist = axis_dist;
                best = if best == raw {
                    candidate
                } else if horizontal {
                    DVec2::new(best.x, position)
                } else {
                    DVec2::new(position, best.y)
                };
            }
        }
    }
    if config.anchor {
        for point in anchor_points(input, exclude_anchor_of) {
            let d = (point - raw).length();
            if d <= best_dist {
                best_dist = d;
                best = point;
            }
        }
    }
    best
}

pub fn snap_delta(
    config: &SnapConfig,
    input: &SnapInput<'_>,
    guides: &[(bool, f64)],
    bounds: Option<(DVec2, DVec2)>,
    delta: DVec2,
    tolerance_world: f64,
    exclude_anchor_of: Option<NodeId>,
) -> DVec2 {
    let Some((min, max)) = bounds else {
        return snap_point(
            config,
            input,
            guides,
            delta,
            tolerance_world,
            exclude_anchor_of,
        ) - delta;
    };
    let corners = [
        min,
        DVec2::new(max.x, min.y),
        DVec2::new(min.x, max.y),
        max,
        (min + max) * 0.5,
    ];
    let mut best_delta = delta;
    let mut best_dist = tolerance_world;
    for corner in corners {
        let snapped = snap_point(
            config,
            input,
            guides,
            corner + delta,
            tolerance_world,
            exclude_anchor_of,
        );
        let candidate = snapped - corner;
        let target = corner + delta;
        let achieved = corner + candidate;
        let dist = (snapped - target).length();
        let dominated = (achieved - target).length() > dist + 1e-9;
        if !dominated && dist <= best_dist {
            best_dist = dist;
            best_delta = candidate;
        }
    }
    best_delta
}

fn anchor_points(input: &SnapInput<'_>, exclude: Option<NodeId>) -> Vec<DVec2> {
    let mut out = Vec::new();
    for item in input.items {
        if is_excluded(input.doc, input.selected, exclude, item.node) {
            continue;
        }
        let bb = item.path.bounding_box();
        let candidates = [
            DVec2::new(bb.x0, bb.y0),
            DVec2::new(bb.x1, bb.y0),
            DVec2::new(bb.x0, bb.y1),
            DVec2::new(bb.x1, bb.y1),
            DVec2::new((bb.x0 + bb.x1) * 0.5, (bb.y0 + bb.y1) * 0.5),
        ];
        for point in candidates {
            if point.is_finite() && !out.contains(&point) {
                out.push(point);
            }
        }
    }
    out
}

fn is_excluded(
    doc: &Document,
    selected: &[NodeId],
    exclude: Option<NodeId>,
    node: NodeId,
) -> bool {
    if selected.contains(&node) {
        return true;
    }
    if selected.iter().any(|id| node_is_ancestor(doc, *id, node)) {
        return true;
    }
    if let Some(root) = exclude
        && (node == root || node_is_ancestor(doc, root, node))
    {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> SnapConfig {
        SnapConfig {
            grid: None,
            anchor: false,
            guide: false,
        }
    }

    fn input<'a>(doc: &'a Document, items: &'a [SceneItem], selected: &'a [NodeId]) -> SnapInput<'a> {
        SnapInput {
            doc,
            items,
            selected,
        }
    }

    #[test]
    fn grid_snap_rounds_to_step() {
        let doc = Document::empty();
        let items: Vec<SceneItem> = Vec::new();
        let selected: Vec<NodeId> = Vec::new();
        let cfg = SnapConfig {
            grid: Some(10.0),
            ..config()
        };
        let got = snap_point(
            &cfg,
            &input(&doc, &items, &selected),
            &[],
            DVec2::new(12.0, 17.0),
            8.0,
            None,
        );
        assert_eq!(got, DVec2::new(10.0, 20.0));
    }

    #[test]
    fn grid_snap_misses_outside_tolerance() {
        let doc = Document::empty();
        let items: Vec<SceneItem> = Vec::new();
        let selected: Vec<NodeId> = Vec::new();
        let cfg = SnapConfig {
            grid: Some(10.0),
            ..config()
        };
        let raw = DVec2::new(14.0, 14.0);
        let got = snap_point(&cfg, &input(&doc, &items, &selected), &[], raw, 1.0, None);
        assert_eq!(got, raw);
    }

    #[test]
    fn guide_snap_merges_axes() {
        let doc = Document::empty();
        let items: Vec<SceneItem> = Vec::new();
        let selected: Vec<NodeId> = Vec::new();
        let cfg = SnapConfig {
            guide: true,
            ..config()
        };
        let got = snap_point(
            &cfg,
            &input(&doc, &items, &selected),
            &[(true, 100.0), (false, 50.0)],
            DVec2::new(52.0, 103.0),
            8.0,
            None,
        );
        assert_eq!(got, DVec2::new(50.0, 100.0));
    }

    #[test]
    fn non_finite_guides_ignored() {
        let doc = Document::empty();
        let items: Vec<SceneItem> = Vec::new();
        let selected: Vec<NodeId> = Vec::new();
        let cfg = SnapConfig {
            guide: true,
            ..config()
        };
        let raw = DVec2::new(10.0, 10.0);
        let got = snap_point(
            &cfg,
            &input(&doc, &items, &selected),
            &[(true, f64::NAN)],
            raw,
            8.0,
            None,
        );
        assert_eq!(got, raw);
    }

    #[test]
    fn snap_delta_keeps_best_corner() {
        let doc = Document::empty();
        let items: Vec<SceneItem> = Vec::new();
        let selected: Vec<NodeId> = Vec::new();
        let cfg = SnapConfig {
            grid: Some(10.0),
            ..config()
        };
        let bounds = Some((DVec2::new(1.0, 1.0), DVec2::new(11.0, 11.0)));
        let got = snap_delta(
            &cfg,
            &input(&doc, &items, &selected),
            &[],
            bounds,
            DVec2::new(1.5, 1.5),
            8.0,
            None,
        );
        assert_eq!(got, DVec2::new(-1.0, -1.0));
    }
}
