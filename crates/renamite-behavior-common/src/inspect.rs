//! Property inspector: descriptors, diamond state, edit commands.
//!
//! Pure, headless-testable helpers the Properties panel renders. Descriptors
//! are a fixed table per `NodeKind` (plus always-on transform/opacity) mapped
//! to `PropPath`s that match `Document::prop_mut`.

use renamite_animation::Frame;
use renamite_history::{EditorCommand, resolve_property_edit};
use renamite_model::{
    BlendMode, Document, ModifierKind, NodeId, NodeKind, PropPath, ShapeKind, StyleKind, Value,
    node_supports_opacity, node_supports_prop, node_supports_transform,
};

/// Shared blend-mode table: single source of truth for index ↔ mode ↔ label.
/// Indices 0..15 match AE/Friction ordering used in `layer.blend`.
pub const BLEND_MODES: &[(BlendMode, &str)] = &[
    (BlendMode::Normal, "Normal"),
    (BlendMode::Multiply, "Multiply"),
    (BlendMode::Screen, "Screen"),
    (BlendMode::Overlay, "Overlay"),
    (BlendMode::Darken, "Darken"),
    (BlendMode::Lighten, "Lighten"),
    (BlendMode::ColorDodge, "ColorDodge"),
    (BlendMode::ColorBurn, "ColorBurn"),
    (BlendMode::HardLight, "HardLight"),
    (BlendMode::SoftLight, "SoftLight"),
    (BlendMode::Difference, "Difference"),
    (BlendMode::Exclusion, "Exclusion"),
    (BlendMode::Hue, "Hue"),
    (BlendMode::Saturation, "Saturation"),
    (BlendMode::Color, "Color"),
    (BlendMode::Luminosity, "Luminosity"),
];

pub fn blend_to_index(b: BlendMode) -> usize {
    BLEND_MODES.iter().position(|(m, _)| *m == b).unwrap_or(0)
}

pub fn blend_from_index(i: i64) -> BlendMode {
    BLEND_MODES
        .get(i as usize)
        .map(|(m, _)| *m)
        .unwrap_or(BlendMode::Normal)
}

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
    pub mixed: bool,
}

/// Properties shown for a single selected node (empty if missing).
pub fn props_for_node(doc: &Document, id: NodeId, playhead: Frame) -> Vec<PropRow> {
    let Some(node) = doc.nodes.get(id) else {
        return vec![];
    };
    descriptors_for(&node.kind)
        .into_iter()
        .filter(|desc| {
            // Drop anything the node kind cannot honor at render; the model
            // is the contract, descriptors only add finer per-value gating
            // (Burst inner radius, non-miter limit) below. Discrete enum/bool
            // rows resolve via `cmd_set_discrete`, not `prop_mut`.
            if !node_supports_prop(&node.kind, desc.path.as_str())
                && !supports_discrete(&node.kind, desc.path.as_str())
            {
                return false;
            }
            // Hide inner radius when Burst (no-op).
            if desc.path.as_str() == "shape.inner_r" {
                match &node.kind {
                    NodeKind::Shape(ShapeKind::Star { kind, .. })
                        if *kind == renamite_model::StarKind::Burst =>
                    {
                        return false;
                    }
                    NodeKind::Mask(m) => match &m.shape {
                        ShapeKind::Star { kind, .. }
                            if *kind == renamite_model::StarKind::Burst =>
                        {
                            return false;
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
            if desc.path.as_str() == "stroke.miter_limit" {
                match &node.kind {
                    NodeKind::Style(StyleKind::Stroke { join, .. })
                        if *join != renamite_model::StrokeJoin::Miter =>
                    {
                        return false;
                    }
                    _ => {}
                }
            }
            true
        })
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
                    mixed: false,
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
                    mixed: false,
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
                    mixed: false,
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
                    mixed: false,
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
                    mixed: false,
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
                    mixed: false,
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
                    mixed: false,
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
                    mixed: false,
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
                mixed: false,
            })
        })
        .collect()
}

/// Discrete enum/bool rows bypass `prop_mut` via `cmd_set_discrete`.
/// Mirror its path → kind table so the support filter keeps them.
fn supports_discrete(kind: &NodeKind, path: &str) -> bool {
    match path {
        "trim.mode" => matches!(kind, NodeKind::Modifier(ModifierKind::TrimPath { .. })),
        "fill.rule" => matches!(kind, NodeKind::Style(StyleKind::Fill { .. })),
        "star.kind" => {
            matches!(kind, NodeKind::Shape(ShapeKind::Star { .. }))
                || matches!(kind, NodeKind::Mask(m) if matches!(&m.shape, ShapeKind::Star { .. }))
        }
        "stroke.cap" | "stroke.join" => {
            matches!(kind, NodeKind::Style(StyleKind::Stroke { .. }))
        }
        "text.align" => matches!(kind, NodeKind::Text(_)),
        "mask.inverted" => matches!(kind, NodeKind::Mask(_)),
        "zigzag.smooth" => matches!(kind, NodeKind::Modifier(ModifierKind::ZigZag { .. })),
        "layer.blend" => matches!(kind, NodeKind::Layer(_)),
        _ => false,
    }
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
    let mut d = Vec::new();
    if node_supports_transform(kind) {
        d.extend(transform_descriptors());
    }
    if node_supports_opacity(kind) {
        d.push(pd("Transform", "Opacity", "opacity", f04()));
    }
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
            d.push(pd(
                "Text",
                "Tracking",
                "text.tracking",
                PropKind::F64 {
                    min: None,
                    max: None,
                    step: 1.0,
                },
            ));
            d.push(pd(
                "Text",
                "Leading",
                "text.leading",
                PropKind::F64 {
                    min: None,
                    max: None,
                    step: 1.0,
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
            d.push(pd(
                "Stroke",
                "Miter limit",
                "stroke.miter_limit",
                PropKind::F64 {
                    min: Some(1.0),
                    max: Some(10.0),
                    step: 0.1,
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
                    "Scale %",
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
                    "Amount %",
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
        NodeKind::Layer(_) => {
            // rendered by layer_section in properties.rs.
        }
        NodeKind::Precomp { .. } => {
            // precomp_section in properties.rs.
        }
        NodeKind::Image(_) => {
            d.push(pd("Image", "Tint", "image.tint", PropKind::Color));
        }
        NodeKind::Use { .. } => {}
        NodeKind::Group => {}
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

fn transform_descriptors() -> Vec<PropDescriptor> {
    vec![
        pd(
            "Transform",
            "Position",
            "transform.position",
            PropKind::DVec2,
        ),
        pd("Transform", "Scale %", "transform.scale", PropKind::DVec2),
        pd(
            "Transform",
            "Rotation",
            "transform.rotation",
            PropKind::Angle,
        ),
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
    ]
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

/// Structural (non-Animated) inspector edits. Single place for path → command.
/// `supports_discrete` above mirrors this table; update both together.
pub fn cmd_set_discrete(
    doc: &Document,
    id: NodeId,
    path: &PropPath,
    index_or_bool: i64,
) -> Option<EditorCommand> {
    use renamite_model::*;
    let node = doc.nodes.get(id)?;
    match path.as_str() {
        "trim.mode" => {
            if !matches!(
                &node.kind,
                NodeKind::Modifier(ModifierKind::TrimPath { .. })
            ) {
                return None;
            }
            let mode = if index_or_bool == 1 {
                TrimMode::Simultaneously
            } else {
                TrimMode::Individually
            };
            Some(EditorCommand::SetTrimMode { id, mode })
        }
        "fill.rule" => {
            if !matches!(&node.kind, NodeKind::Style(StyleKind::Fill { .. })) {
                return None;
            }
            Some(EditorCommand::SetFillRule {
                id,
                rule: if index_or_bool == 1 {
                    FillRule::EvenOdd
                } else {
                    FillRule::NonZero
                },
            })
        }
        "star.kind" => {
            let is_star = matches!(&node.kind, NodeKind::Shape(ShapeKind::Star { .. }))
                || matches!(&node.kind, NodeKind::Mask(m) if matches!(&m.shape, ShapeKind::Star { .. }));
            if !is_star {
                return None;
            }
            Some(EditorCommand::SetStarKind {
                id,
                kind: if index_or_bool == 1 {
                    StarKind::Burst
                } else {
                    StarKind::Star
                },
            })
        }
        "stroke.cap" => {
            if !matches!(&node.kind, NodeKind::Style(StyleKind::Stroke { .. })) {
                return None;
            }
            Some(EditorCommand::SetStrokeCap {
                id,
                cap: match index_or_bool {
                    1 => StrokeCap::Round,
                    2 => StrokeCap::Square,
                    _ => StrokeCap::Butt,
                },
            })
        }
        "stroke.join" => {
            if !matches!(&node.kind, NodeKind::Style(StyleKind::Stroke { .. })) {
                return None;
            }
            Some(EditorCommand::SetStrokeJoin {
                id,
                join: match index_or_bool {
                    1 => StrokeJoin::Round,
                    2 => StrokeJoin::Bevel,
                    _ => StrokeJoin::Miter,
                },
            })
        }
        "text.align" => {
            if !matches!(&node.kind, NodeKind::Text(_)) {
                return None;
            }
            Some(EditorCommand::SetTextAlign {
                id,
                align: match index_or_bool {
                    1 => TextAlign::Center,
                    2 => TextAlign::Right,
                    _ => TextAlign::Left,
                },
            })
        }
        "mask.inverted" => {
            if !matches!(&node.kind, NodeKind::Mask(_)) {
                return None;
            }
            Some(EditorCommand::SetMaskInverted {
                id,
                inverted: index_or_bool != 0,
            })
        }
        "zigzag.smooth" => {
            if !matches!(&node.kind, NodeKind::Modifier(ModifierKind::ZigZag { .. })) {
                return None;
            }
            Some(EditorCommand::SetZigZagSmooth {
                id,
                smooth: index_or_bool != 0,
            })
        }
        "layer.blend" => {
            if !matches!(&node.kind, NodeKind::Layer(_)) {
                return None;
            }
            Some(EditorCommand::SetLayerProps {
                id,
                in_frame: None,
                out_frame: None,
                time_stretch: None,
                blend: Some(blend_from_index(index_or_bool)),
            })
        }
        _ => None,
    }
}

/// Multi-selection: only show props common to all ids (same path set intersection).
/// Values come from the first node; `mixed` is set when other nodes differ.
pub fn props_for_selection(doc: &Document, ids: &[NodeId], playhead: Frame) -> Vec<PropRow> {
    match ids {
        [] => vec![],
        [id] => props_for_node(doc, *id, playhead),
        ids => {
            let mut iter = ids.iter().copied();
            let first = iter.next().unwrap();
            let mut common = props_for_node(doc, first, playhead);
            for id in iter {
                let paths: std::collections::HashSet<_> = props_for_node(doc, id, playhead)
                    .into_iter()
                    .map(|r| r.desc.path.as_str().to_string())
                    .collect();
                common.retain(|r| paths.contains(r.desc.path.as_str()));
            }
            for row in &mut common {
                let v0 = row.value.clone();
                let mut mixed = false;
                for &id in &ids[1..] {
                    // Discrete enum/bool rows are synthesized per-node; compare
                    // via a fresh single-node lookup for correctness.
                    let other = props_for_node(doc, id, playhead)
                        .into_iter()
                        .find(|r| r.desc.path.as_str() == row.desc.path.as_str())
                        .map(|r| r.value);
                    if other.as_ref() != Some(&v0) {
                        mixed = true;
                        break;
                    }
                }
                if mixed {
                    row.mixed = true;
                    row.diamond = DiamondState::Empty;
                }
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
