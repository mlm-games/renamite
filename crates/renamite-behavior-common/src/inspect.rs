//! Property inspector: descriptors, diamond state, edit commands.
//!
//! Pure, headless-testable helpers the Properties panel renders. Descriptors
//! are a fixed table per `NodeKind` (plus always-on transform/opacity) mapped
//! to `PropPath`s that match `Document::prop_mut`.

use renamite_animation::Frame;
use renamite_history::{EditorCommand, resolve_property_edit};
use renamite_model::{
    Document, ModifierKind, NodeId, NodeKind, PropPath, ShapeKind, StyleKind, Value,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PropKind {
    F64 {
        min: Option<f64>,
        max: Option<f64>,
        step: f64,
    },
    DVec2,
    Angle, // degrees
    Color,
    Bool,
    /// Two-state toggle (e.g. TrimMode). Value is serialized as `Value::I64`
    /// 0|1; the Properties panel routes the click to the right field.
    Enum2 {
        a_label: &'static str,
        b_label: &'static str,
    },
    Enum3 {
        labels: [&'static str; 3],
    },
}

#[derive(Clone, Debug)]
pub struct PropDescriptor {
    pub path: PropPath,
    pub label: &'static str,
    pub kind: PropKind,
    /// Section header grouping ("Transform", "Shape", …).
    pub section: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiamondState {
    /// No keys on this prop.
    Empty,
    /// Keys exist, none at playhead.
    HasKeys,
    /// Key exactly at playhead.
    AtPlayhead,
}

#[derive(Clone, Debug)]
pub struct PropRow {
    pub desc: PropDescriptor,
    pub value: Value,
    pub diamond: DiamondState,
    pub animated: bool,
}

/// Properties shown for a single selected node (empty if missing).
pub fn props_for_node(doc: &Document, id: NodeId, playhead: Frame) -> Vec<PropRow> {
    let Some(node) = doc.nodes.get(id) else {
        return vec![];
    };
    descriptors_for(&node.kind)
        .into_iter()
        .filter_map(|desc| {
            // `trim.mode` is a plain enum field, not an `Animated<T>` - the
            // generic value_at path can't resolve it. Synthesize the row from
            // the node directly (value encoded as `Value::I64` 0|1).
            if desc.path.as_str() == "trim.mode" {
                let mode = match &node.kind {
                    NodeKind::Modifier(ModifierKind::TrimPath { mode, .. }) => *mode,
                    _ => return None,
                };
                return Some(PropRow {
                    desc,
                    value: Value::I64(mode as i64),
                    diamond: DiamondState::Empty,
                    animated: false,
                });
            }
            if desc.path.as_str() == "mask.inverted" {
                let inverted = match &node.kind {
                    NodeKind::Mask(m) => m.inverted,
                    _ => return None,
                };
                return Some(PropRow {
                    desc,
                    value: Value::Bool(inverted),
                    diamond: DiamondState::Empty,
                    animated: false,
                });
            }
            if desc.path.as_str() == "zigzag.smooth" {
                let smooth = match &node.kind {
                    NodeKind::Modifier(ModifierKind::ZigZag { smooth, .. }) => *smooth,
                    _ => return None,
                };
                return Some(PropRow {
                    desc,
                    value: Value::Bool(smooth),
                    diamond: DiamondState::Empty,
                    animated: false,
                });
            }
            if desc.path.as_str() == "star.kind" {
                let kind = match &node.kind {
                    NodeKind::Shape(ShapeKind::Star { kind, .. }) => *kind,
                    NodeKind::Mask(m) => match &m.shape {
                        ShapeKind::Star { kind, .. } => *kind,
                        _ => return None,
                    },
                    _ => return None,
                };
                return Some(PropRow {
                    desc,
                    value: Value::I64(match kind {
                        renamite_model::StarKind::Star => 0,
                        renamite_model::StarKind::Burst => 1,
                    }),
                    diamond: DiamondState::Empty,
                    animated: false,
                });
            }
            if desc.path.as_str() == "fill.rule" {
                let rule = match &node.kind {
                    NodeKind::Style(StyleKind::Fill { rule, .. }) => *rule,
                    _ => return None,
                };
                return Some(PropRow {
                    desc,
                    value: Value::I64(match rule {
                        renamite_model::FillRule::NonZero => 0,
                        renamite_model::FillRule::EvenOdd => 1,
                    }),
                    diamond: DiamondState::Empty,
                    animated: false,
                });
            }
            if desc.path.as_str() == "stroke.cap" {
                let cap = match &node.kind {
                    NodeKind::Style(StyleKind::Stroke { cap, .. }) => *cap,
                    _ => return None,
                };
                return Some(PropRow {
                    desc,
                    value: Value::I64(match cap {
                        renamite_model::StrokeCap::Butt => 0,
                        renamite_model::StrokeCap::Round => 1,
                        renamite_model::StrokeCap::Square => 2,
                    }),
                    diamond: DiamondState::Empty,
                    animated: false,
                });
            }
            if desc.path.as_str() == "stroke.join" {
                let join = match &node.kind {
                    NodeKind::Style(StyleKind::Stroke { join, .. }) => *join,
                    _ => return None,
                };
                return Some(PropRow {
                    desc,
                    value: Value::I64(match join {
                        renamite_model::StrokeJoin::Miter => 0,
                        renamite_model::StrokeJoin::Round => 1,
                        renamite_model::StrokeJoin::Bevel => 2,
                    }),
                    diamond: DiamondState::Empty,
                    animated: false,
                });
            }
            if desc.path.as_str() == "text.align" {
                let align = match &node.kind {
                    NodeKind::Text(t) => t.align,
                    _ => return None,
                };
                return Some(PropRow {
                    desc,
                    value: Value::I64(match align {
                        renamite_model::TextAlign::Left => 0,
                        renamite_model::TextAlign::Center => 1,
                        renamite_model::TextAlign::Right => 2,
                    }),
                    diamond: DiamondState::Empty,
                    animated: false,
                });
            }
            let value = doc.value_at(id, &desc.path, playhead.0 as f64).ok()?;
            let animated = doc.property_is_animated(id, &desc.path);
            let diamond = diamond_state(doc, id, &desc.path, playhead, animated);
            Some(PropRow {
                desc,
                value,
                diamond,
                animated,
            })
        })
        .collect()
}

fn diamond_state(
    doc: &Document,
    id: NodeId,
    path: &PropPath,
    playhead: Frame,
    animated: bool,
) -> DiamondState {
    if !animated {
        return DiamondState::Empty;
    }
    if doc.keyframe_data(id, path, playhead).is_some() {
        DiamondState::AtPlayhead
    } else {
        DiamondState::HasKeys
    }
}

fn descriptors_for(kind: &NodeKind) -> Vec<PropDescriptor> {
    let mut d = vec![
        pd(
            "Transform",
            "Position",
            "transform.position",
            PropKind::DVec2,
        ),
        pd("Transform", "Scale", "transform.scale", PropKind::DVec2),
        pd(
            "Transform",
            "Rotation",
            "transform.rotation",
            PropKind::Angle,
        ),
        pd("Transform", "Opacity", "opacity", f04()),
        pd(
            "Transform",
            "Pivot / Anchor",
            "transform.anchor",
            PropKind::DVec2,
        ),
        pd(
            "Transform",
            "Skew",
            "transform.skew",
            PropKind::F64 {
                min: None,
                max: None,
                step: 0.5,
            },
        ),
        pd(
            "Transform",
            "Skew axis",
            "transform.skew_axis",
            PropKind::Angle,
        ),
    ];
    match kind {
        NodeKind::Shape(s) => match s {
            ShapeKind::Path(_) => {}
            ShapeKind::CompoundPath(_) => {}
            ShapeKind::Rect { .. } => {
                d.push(pd("Shape", "Size", "shape.size", PropKind::DVec2));
                d.push(pd("Shape", "Position", "shape.pos", PropKind::DVec2));
                d.push(pd(
                    "Shape",
                    "Corner radius",
                    "shape.rounded",
                    PropKind::F64 {
                        min: Some(0.0),
                        max: None,
                        step: 1.0,
                    },
                ));
            }
            ShapeKind::Ellipse { .. } => {
                d.push(pd("Shape", "Size", "shape.size", PropKind::DVec2));
                d.push(pd("Shape", "Position", "shape.pos", PropKind::DVec2));
            }
            ShapeKind::Star { .. } => {
                d.push(pd("Shape", "Position", "shape.pos", PropKind::DVec2));
                d.push(pd(
                    "Shape",
                    "Points",
                    "shape.points",
                    PropKind::F64 {
                        min: Some(3.0),
                        max: Some(64.0),
                        step: 1.0,
                    },
                ));
                d.push(pd(
                    "Shape",
                    "Outer radius",
                    "shape.outer_r",
                    PropKind::F64 {
                        min: Some(0.0),
                        max: None,
                        step: 1.0,
                    },
                ));
                d.push(pd(
                    "Shape",
                    "Inner radius",
                    "shape.inner_r",
                    PropKind::F64 {
                        min: Some(0.0),
                        max: None,
                        step: 1.0,
                    },
                ));
                d.push(pd(
                    "Shape",
                    "Roundness",
                    "shape.roundness",
                    PropKind::F64 {
                        min: Some(0.0),
                        max: None,
                        step: 0.5,
                    },
                ));
                d.push(pd(
                    "Shape",
                    "Kind",
                    "star.kind",
                    PropKind::Enum2 {
                        a_label: "Star",
                        b_label: "Burst",
                    },
                ));
            }
            ShapeKind::Polygon { .. } => {
                d.push(pd("Shape", "Position", "shape.pos", PropKind::DVec2));
                d.push(pd(
                    "Shape",
                    "Points",
                    "shape.points",
                    PropKind::F64 {
                        min: Some(3.0),
                        max: Some(64.0),
                        step: 1.0,
                    },
                ));
                d.push(pd(
                    "Shape",
                    "Outer radius",
                    "shape.outer_r",
                    PropKind::F64 {
                        min: Some(0.0),
                        max: None,
                        step: 1.0,
                    },
                ));
                d.push(pd(
                    "Shape",
                    "Roundness",
                    "shape.roundness",
                    PropKind::F64 {
                        min: Some(0.0),
                        max: None,
                        step: 0.5,
                    },
                ));
            }
        },
        NodeKind::Style(StyleKind::Fill { .. }) => {
            // Color handled by paint_section, not generic rows
            d.push(pd(
                "Fill",
                "Rule",
                "fill.rule",
                PropKind::Enum2 {
                    a_label: "NonZero",
                    b_label: "EvenOdd",
                },
            ));
        }
        NodeKind::Text(_) => {
            d.push(pd(
                "Text",
                "Size",
                "text.size",
                PropKind::F64 {
                    min: Some(1.0),
                    max: None,
                    step: 1.0,
                },
            ));
            d.push(pd(
                "Text",
                "Align",
                "text.align",
                PropKind::Enum3 {
                    labels: ["Left", "Center", "Right"],
                },
            ));
        }
        NodeKind::Style(StyleKind::Stroke { .. }) => {
            // Color handled by paint_section
            d.push(pd(
                "Stroke",
                "Width",
                "stroke.width",
                PropKind::F64 {
                    min: Some(0.0),
                    max: None,
                    step: 0.5,
                },
            ));
            d.push(pd(
                "Stroke",
                "Cap",
                "stroke.cap",
                PropKind::Enum3 {
                    labels: ["Butt", "Round", "Square"],
                },
            ));
            d.push(pd(
                "Stroke",
                "Join",
                "stroke.join",
                PropKind::Enum3 {
                    labels: ["Miter", "Round", "Bevel"],
                },
            ));
        }
        NodeKind::Modifier(m) => match m {
            ModifierKind::TrimPath { .. } => {
                d.push(pd("Trim", "Start", "trim.start", f01()));
                d.push(pd("Trim", "End", "trim.end", f01()));
                d.push(pd(
                    "Trim",
                    "Offset",
                    "trim.offset",
                    PropKind::F64 {
                        min: None,
                        max: None,
                        step: 0.01,
                    },
                ));
                d.push(pd(
                    "Trim",
                    "Mode",
                    "trim.mode",
                    PropKind::Enum2 {
                        a_label: "Individually",
                        b_label: "Simultaneously",
                    },
                ));
            }
            ModifierKind::RoundCorners { .. } => {
                d.push(pd(
                    "Round Corners",
                    "Radius",
                    "round.radius",
                    PropKind::F64 {
                        min: Some(0.0),
                        max: None,
                        step: 1.0,
                    },
                ));
            }
            ModifierKind::Repeater { .. } => {
                d.push(pd(
                    "Repeater",
                    "Copies",
                    "repeater.copies",
                    PropKind::F64 {
                        min: Some(0.0),
                        max: Some(100.0),
                        step: 1.0,
                    },
                ));
                d.push(pd(
                    "Repeater",
                    "Offset",
                    "repeater.offset",
                    PropKind::F64 {
                        min: None,
                        max: None,
                        step: 0.1,
                    },
                ));
                d.push(pd(
                    "Repeater",
                    "Start opacity",
                    "repeater.start_opacity",
                    f01(),
                ));
                d.push(pd("Repeater", "End opacity", "repeater.end_opacity", f01()));
                d.push(pd(
                    "Repeater",
                    "Position",
                    "repeater.transform.position",
                    PropKind::DVec2,
                ));
                d.push(pd(
                    "Repeater",
                    "Scale",
                    "repeater.transform.scale",
                    PropKind::DVec2,
                ));
                d.push(pd(
                    "Repeater",
                    "Rotation",
                    "repeater.transform.rotation",
                    PropKind::Angle,
                ));
                d.push(pd(
                    "Repeater",
                    "Anchor",
                    "repeater.transform.anchor",
                    PropKind::DVec2,
                ));
                d.push(pd(
                    "Repeater",
                    "Skew",
                    "repeater.transform.skew",
                    PropKind::F64 {
                        min: None,
                        max: None,
                        step: 0.5,
                    },
                ));
                d.push(pd(
                    "Repeater",
                    "Skew axis",
                    "repeater.transform.skew_axis",
                    PropKind::Angle,
                ));
            }
            ModifierKind::OffsetPath { .. } => {
                d.push(pd(
                    "Offset Path",
                    "Amount",
                    "offset.amount",
                    PropKind::F64 {
                        min: None,
                        max: None,
                        step: 1.0,
                    },
                ));
            }
            ModifierKind::ZigZag { .. } => {
                d.push(pd(
                    "Zig Zag",
                    "Amplitude",
                    "zigzag.amplitude",
                    PropKind::F64 {
                        min: None,
                        max: None,
                        step: 1.0,
                    },
                ));
                d.push(pd(
                    "Zig Zag",
                    "Frequency",
                    "zigzag.frequency",
                    PropKind::F64 {
                        min: None,
                        max: None,
                        step: 1.0,
                    },
                ));
                d.push(pd("Zig Zag", "Smooth", "zigzag.smooth", PropKind::Bool));
            }
            ModifierKind::PuckerBloat { .. } => {
                d.push(pd(
                    "Pucker & Bloat",
                    "Amount",
                    "pucker.amount",
                    PropKind::F64 {
                        min: None,
                        max: None,
                        step: 1.0,
                    },
                ));
            }
        },
        NodeKind::Mask(mask) => {
            d.push(pd("Mask", "Inverted", "mask.inverted", PropKind::Bool));
            match &mask.shape {
                ShapeKind::Path(_) | ShapeKind::CompoundPath(_) => {}
                ShapeKind::Rect { .. } => {
                    d.push(pd("Mask", "Size", "shape.size", PropKind::DVec2));
                    d.push(pd("Mask", "Position", "shape.pos", PropKind::DVec2));
                    d.push(pd(
                        "Mask",
                        "Corner radius",
                        "shape.rounded",
                        PropKind::F64 {
                            min: Some(0.0),
                            max: None,
                            step: 1.0,
                        },
                    ));
                }
                ShapeKind::Ellipse { .. } => {
                    d.push(pd("Mask", "Size", "shape.size", PropKind::DVec2));
                    d.push(pd("Mask", "Position", "shape.pos", PropKind::DVec2));
                }
                ShapeKind::Star { .. } => {
                    d.push(pd("Mask", "Position", "shape.pos", PropKind::DVec2));
                    d.push(pd(
                        "Mask",
                        "Points",
                        "shape.points",
                        PropKind::F64 {
                            min: Some(3.0),
                            max: Some(64.0),
                            step: 1.0,
                        },
                    ));
                    d.push(pd(
                        "Mask",
                        "Outer radius",
                        "shape.outer_r",
                        PropKind::F64 {
                            min: Some(0.0),
                            max: None,
                            step: 1.0,
                        },
                    ));
                    d.push(pd(
                        "Mask",
                        "Inner radius",
                        "shape.inner_r",
                        PropKind::F64 {
                            min: Some(0.0),
                            max: None,
                            step: 1.0,
                        },
                    ));
                    d.push(pd(
                        "Mask",
                        "Roundness",
                        "shape.roundness",
                        PropKind::F64 {
                            min: Some(0.0),
                            max: None,
                            step: 0.5,
                        },
                    ));
                    d.push(pd(
                        "Mask",
                        "Kind",
                        "star.kind",
                        PropKind::Enum2 {
                            a_label: "Star",
                            b_label: "Burst",
                        },
                    ));
                }
                ShapeKind::Polygon { .. } => {
                    d.push(pd("Mask", "Position", "shape.pos", PropKind::DVec2));
                    d.push(pd(
                        "Mask",
                        "Points",
                        "shape.points",
                        PropKind::F64 {
                            min: Some(3.0),
                            max: Some(64.0),
                            step: 1.0,
                        },
                    ));
                    d.push(pd(
                        "Mask",
                        "Outer radius",
                        "shape.outer_r",
                        PropKind::F64 {
                            min: Some(0.0),
                            max: None,
                            step: 1.0,
                        },
                    ));
                    d.push(pd(
                        "Mask",
                        "Roundness",
                        "shape.roundness",
                        PropKind::F64 {
                            min: Some(0.0),
                            max: None,
                            step: 0.5,
                        },
                    ));
                }
            }
        }
        _ => {}
    }
    d
}

fn f01() -> PropKind {
    PropKind::F64 {
        min: Some(0.0),
        max: Some(1.0),
        step: 0.01,
    }
}

fn f04() -> PropKind {
    PropKind::F64 {
        min: Some(0.0),
        max: Some(1.0),
        step: 0.01,
    }
}

fn pd(section: &'static str, label: &'static str, path: &str, kind: PropKind) -> PropDescriptor {
    PropDescriptor {
        path: PropPath::new(path),
        label,
        kind,
        section,
    }
}

/// Drag/type a new value (static or key at playhead via record rule).
pub fn cmd_set_value(
    doc: &Document,
    id: NodeId,
    path: &PropPath,
    value: Value,
    playhead: Frame,
    record: bool,
) -> EditorCommand {
    resolve_property_edit(doc, id, path, value, playhead, record)
}

/// Toggle keyframe diamond at playhead.
pub fn cmd_toggle_key(
    doc: &Document,
    id: NodeId,
    path: &PropPath,
    playhead: Frame,
) -> Option<EditorCommand> {
    if doc.keyframe_data(id, path, playhead).is_some() {
        return Some(EditorCommand::RemoveKeyframe {
            id,
            prop: path.clone(),
            frame: playhead,
        });
    }
    let value = doc.value_at(id, path, playhead.0 as f64).ok()?;
    Some(EditorCommand::AddKeyframe {
        id,
        prop: path.clone(),
        frame: playhead,
        value,
    })
}

/// Multi-selection: only show props common to all ids (same path set intersection).
pub fn props_for_selection(doc: &Document, ids: &[NodeId], playhead: Frame) -> Vec<PropRow> {
    match ids {
        [] => vec![],
        [id] => props_for_node(doc, *id, playhead),
        ids => {
            let mut iter = ids.iter().copied();
            let mut common = props_for_node(doc, iter.next().unwrap(), playhead);
            for id in iter {
                let paths: std::collections::HashSet<_> = props_for_node(doc, id, playhead)
                    .into_iter()
                    .map(|r| r.desc.path.as_str().to_string())
                    .collect();
                common.retain(|r| paths.contains(r.desc.path.as_str()));
            }
            common
        }
    }
}

/// One edit command per node that actually carries `path` (silently skips the
/// rest). All nodes get the SAME absolute value - v1 multi-edit semantics.
pub fn apply_value_to_each(
    doc: &Document,
    ids: &[NodeId],
    path: &PropPath,
    value: Value,
    playhead: Frame,
    record: bool,
) -> Vec<EditorCommand> {
    ids.iter()
        .filter(|id| doc.nodes.get(**id).and_then(|n| n.prop_ref(path)).is_some())
        .map(|id| cmd_set_value(doc, *id, path, value.clone(), playhead, record))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use renamite_animation::Animated;
    use renamite_history::EditorCommand;
    use renamite_model::{Color, FillRule, Node, Parent, StylePaint};

    fn doc_with_ellipse_and_rect() -> (Document, NodeId, NodeId) {
        let mut doc = Document::empty();
        let ellipse = doc.create_node(Node::new(
            "e",
            NodeKind::Shape(ShapeKind::Ellipse {
                pos: Animated::new(glam::DVec2::ZERO),
                size: Animated::new(glam::DVec2::splat(100.0)),
            }),
        ));
        let rect = doc.create_node(Node::new(
            "r",
            NodeKind::Shape(ShapeKind::Rect {
                pos: Animated::new(glam::DVec2::ZERO),
                size: Animated::new(glam::DVec2::splat(50.0)),
                rounded: Animated::new(0.0),
            }),
        ));
        let fill = doc.create_node(Node::new(
            "f",
            NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::solid(Color::BLACK),
                rule: FillRule::NonZero,
            }),
        ));
        doc.attach(ellipse, Parent::Comp(doc.main), 0).unwrap();
        doc.attach(rect, Parent::Comp(doc.main), 1).unwrap();
        doc.attach(fill, Parent::Comp(doc.main), 2).unwrap();
        (doc, ellipse, rect)
    }

    #[test]
    fn ellipse_lists_transform_and_shape_size() {
        let (doc, ellipse, _) = doc_with_ellipse_and_rect();
        let rows = props_for_node(&doc, ellipse, Frame(0));
        let paths: Vec<&str> = rows.iter().map(|r| r.desc.path.as_str()).collect();
        assert!(paths.contains(&"transform.position"));
        assert!(paths.contains(&"transform.scale"));
        assert!(paths.contains(&"opacity"));
        assert!(paths.contains(&"shape.size"));
        assert!(paths.contains(&"shape.pos"));
        assert!(
            !paths.contains(&"shape.rounded"),
            "ellipse has no corner radius"
        );
    }

    #[test]
    fn fill_lists_color() {
        let (doc, _, _) = doc_with_ellipse_and_rect();
        let fill_id = doc.nodes.iter().find(|(_, n)| n.name == "f").unwrap().0;
        let rows = props_for_node(&doc, fill_id, Frame(0));
        let paths: Vec<&str> = rows.iter().map(|r| r.desc.path.as_str()).collect();
        assert!(paths.contains(&"fill.rule"));
        assert!(!paths.contains(&"fill.color"), "paint section owns color");
    }

    #[test]
    fn diamond_empty_has_at_playhead() {
        let (mut doc, ellipse, _) = doc_with_ellipse_and_rect();
        let prop = PropPath::new("shape.size");
        let find = |doc: &Document, frame: i64| {
            props_for_node(doc, ellipse, Frame(frame))
                .into_iter()
                .find(|r| r.desc.path.as_str() == "shape.size")
                .unwrap()
                .diamond
        };
        assert_eq!(find(&doc, 0), DiamondState::Empty);

        doc.add_keyframe(
            ellipse,
            &prop,
            Frame(10),
            &Value::DVec2(glam::DVec2::splat(200.0)),
        )
        .unwrap();
        assert_eq!(find(&doc, 10), DiamondState::AtPlayhead);
        assert_eq!(find(&doc, 0), DiamondState::HasKeys);
    }

    #[test]
    fn toggle_key_adds_then_removes() {
        let (mut doc, ellipse, _) = doc_with_ellipse_and_rect();
        let prop = PropPath::new("shape.size");

        let add = cmd_toggle_key(&doc, ellipse, &prop, Frame(10)).unwrap();
        assert!(matches!(add, EditorCommand::AddKeyframe { .. }));

        doc.add_keyframe(ellipse, &prop, Frame(10), &Value::DVec2(glam::DVec2::ZERO))
            .unwrap();
        let remove = cmd_toggle_key(&doc, ellipse, &prop, Frame(10)).unwrap();
        assert!(matches!(remove, EditorCommand::RemoveKeyframe { .. }));
    }

    #[test]
    fn multi_select_intersects_paths() {
        let (doc, ellipse, rect) = doc_with_ellipse_and_rect();
        let rows = props_for_selection(&doc, &[ellipse, rect], Frame(0));
        let paths: Vec<&str> = rows.iter().map(|r| r.desc.path.as_str()).collect();
        assert!(paths.contains(&"shape.pos"));
        assert!(paths.contains(&"shape.size"));
        assert!(paths.contains(&"transform.position"));
        assert!(
            !paths.contains(&"shape.rounded"),
            "rect-only prop must be filtered"
        );
    }

    #[test]
    fn apply_value_to_each_skips_missing_props() {
        let (doc, ellipse, _) = doc_with_ellipse_and_rect();
        let fill_id = doc.nodes.iter().find(|(_, n)| n.name == "f").unwrap().0;
        let cmds = apply_value_to_each(
            &doc,
            &[ellipse, fill_id],
            &PropPath::new("shape.size"),
            Value::DVec2(glam::DVec2::splat(80.0)),
            Frame(0),
            false,
        );
        assert_eq!(cmds.len(), 1, "fill has no shape.size");
    }

    #[test]
    fn trim_path_lists_mode_enum_row() {
        use renamite_animation::Animated;
        use renamite_model::TrimMode;
        let mut doc = Document::empty();
        let trim = doc.create_node(Node::new(
            "t",
            NodeKind::Modifier(ModifierKind::TrimPath {
                start: Animated::new(0.0),
                end: Animated::new(1.0),
                offset: Animated::new(0.0),
                mode: TrimMode::Individually,
            }),
        ));
        doc.attach(trim, Parent::Comp(doc.main), 0).unwrap();
        let rows = props_for_node(&doc, trim, Frame(0));
        let mode = rows
            .iter()
            .find(|r| r.desc.path.as_str() == "trim.mode")
            .expect("mode row");
        assert!(matches!(mode.desc.kind, PropKind::Enum2 { .. }));
        assert_eq!(mode.value, Value::I64(0));
    }

    #[test]
    fn round_corners_lists_radius() {
        use renamite_animation::Animated;
        let mut doc = Document::empty();
        let rc = doc.create_node(Node::new(
            "rc",
            NodeKind::Modifier(ModifierKind::RoundCorners {
                radius: Animated::new(28.0),
            }),
        ));
        doc.attach(rc, Parent::Comp(doc.main), 0).unwrap();
        let rows = props_for_node(&doc, rc, Frame(0));
        let radius = rows
            .iter()
            .find(|r| r.desc.path.as_str() == "round.radius")
            .expect("radius row");
        assert_eq!(radius.value, Value::F64(28.0));
    }

    #[test]
    fn text_lists_size_prop() {
        use renamite_animation::Animated;
        use renamite_model::{TextAlign, TextNode};
        let mut doc = Document::empty();
        let t = doc.create_node(Node::new(
            "t",
            NodeKind::Text(TextNode {
                text: "Hi".into(),
                size: Animated::new(64.0),
                align: TextAlign::Left,
                font: None,
            }),
        ));
        doc.attach(t, Parent::Comp(doc.main), 0).unwrap();
        let rows = props_for_node(&doc, t, Frame(0));
        let size = rows
            .iter()
            .find(|r| r.desc.path.as_str() == "text.size")
            .expect("size row");
        assert_eq!(size.value, Value::F64(64.0));
        assert!(matches!(size.desc.kind, PropKind::F64 { .. }));
    }
}
