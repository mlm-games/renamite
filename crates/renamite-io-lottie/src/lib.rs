//! Lottie JSON import and export.
//!
//! Supported Renamite/Lottie intersection:
//!
//! - Shape layers and nested shape groups
//! - Precompositions
//! - Layer/group transforms
//! - Rectangle, ellipse, path, star, and polygon shapes
//! - Solid and gradient fills/strokes
//! - Stroke dashes
//! - Trim Path, Round Corners, Offset Path, and Repeater
//! - Static and keyframed scalar/vector/color/path/gradient properties
//! - Hold, linear, and cubic-bezier interpolation
//!
//! Unsupported Lottie objects are skipped in best-effort mode and returned as
//! warnings by [`import_with_report`] / [`export_with_report`].

mod export;
mod import;
mod property;

use renamite_animation::FrameRate;
use renamite_model::Document;
use serde_json::Value;

pub use export::export_with_report;
pub use import::import_with_report;

/// Compatibility/version marker for callers that need to label the target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LottieVersion(pub u32);

/// One non-fatal compatibility warning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LottieWarning {
    pub path: String,
    pub message: String,
}

impl LottieWarning {
    pub(crate) fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

/// Successful conversion plus non-fatal compatibility warnings.
#[derive(Clone, Debug)]
pub struct LottieReport<T> {
    pub value: T,
    pub warnings: Vec<LottieWarning>,
}

#[derive(Debug, thiserror::Error)]
pub enum LottieError {
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("missing required Lottie field `{0}`")]
    Missing(&'static str),
    #[error("invalid Lottie field `{0}`")]
    Invalid(&'static str),
    #[error("main composition is missing")]
    MissingMainComposition,
    #[error("Lottie asset `{0}` is missing")]
    MissingAsset(String),
    #[error("cyclic or invalid precomposition `{0}`")]
    InvalidPrecomposition(String),
}

/// Import Lottie JSON, discarding non-fatal warnings.
pub fn import(json: &Value) -> Result<Document, LottieError> {
    Ok(import_with_report(json)?.value)
}

/// Export a Renamite document to Lottie JSON, discarding non-fatal warnings.
pub fn export(doc: &Document) -> Result<Value, LottieError> {
    Ok(export_with_report(doc)?.value)
}

/// Export as compact JSON text.
pub fn export_to_string(doc: &Document) -> Result<String, LottieError> {
    Ok(serde_json::to_string(&export(doc)?)?)
}

/// Export as pretty JSON text.
pub fn export_to_string_pretty(doc: &Document) -> Result<String, LottieError> {
    Ok(serde_json::to_string_pretty(&export(doc)?)?)
}

pub fn default_rate() -> FrameRate {
    FrameRate { num: 60, den: 1 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::DVec2;
    use renamite_animation::{Animated, EasingHandle, Frame, Interpolation};
    use renamite_model::{
        Color, FillRule, GradientStop, GradientStops, MaskProps, ModifierKind, Node, NodeKind,
        Parent, PropPath, ShapeKind, StarKind, StyleKind, StylePaint, TextAlign, TextNode,
        TrimMode, Value,
    };

    fn visible_shape_doc() -> Document {
        let mut doc = Document::empty();
        let comp = doc.main;
        let group = doc.create_node(Node::new("Ellipse Group", NodeKind::Group));
        let ellipse = doc.create_node(Node::new(
            "Ellipse",
            NodeKind::Shape(ShapeKind::Ellipse {
                pos: Animated::new(DVec2::new(256.0, 256.0)),
                size: Animated::new(DVec2::new(200.0, 160.0)),
            }),
        ));
        let fill = doc.create_node(Node::new(
            "Fill",
            NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::solid(Color::rgba(1.0, 0.4, 0.1, 1.0)),
                rule: FillRule::NonZero,
            }),
        ));
        doc.attach(ellipse, Parent::Node(group), 0).unwrap();
        doc.attach(fill, Parent::Node(group), 1).unwrap();
        doc.attach(group, Parent::Comp(comp), 0).unwrap();
        doc
    }

    #[test]
    fn shape_and_fill_export_in_same_layer() {
        let doc = visible_shape_doc();
        let value = export(&doc).unwrap();
        let layers = value["layers"].as_array().unwrap();
        assert_eq!(layers.len(), 1);
        let shapes = layers[0]["shapes"].as_array().unwrap();
        let serialized = serde_json::to_string(shapes).unwrap();
        assert!(serialized.contains("\"ty\":\"el\""));
        assert!(serialized.contains("\"ty\":\"fl\""));
    }

    #[test]
    fn export_root_metadata() {
        let doc = visible_shape_doc();
        let value = export(&doc).unwrap();
        assert_eq!(value["v"], "5.5.9");
        assert_eq!(value["w"], 512);
        assert_eq!(value["h"], 512);
        assert_eq!(value["fr"], 60.0);
        assert_eq!(value["ip"], 0.0);
        assert_eq!(value["op"], 180.0);
    }

    #[test]
    fn animated_position_round_trips() {
        let mut doc = visible_shape_doc();
        let group = doc.compositions[doc.main].children[0];
        let prop = PropPath::new("transform.position");
        doc.add_keyframe(group, &prop, Frame(0), &Value::DVec2(DVec2::new(0.0, 0.0)))
            .unwrap();
        doc.add_keyframe(
            group,
            &prop,
            Frame(60),
            &Value::DVec2(DVec2::new(120.0, 20.0)),
        )
        .unwrap();
        doc.set_easing(
            group,
            &prop,
            Frame(0),
            Interpolation::CubicBezier,
            EasingHandle { x: 0.42, y: 0.0 },
            EasingHandle { x: 0.58, y: 1.0 },
        )
        .unwrap();

        let exported = export(&doc).unwrap();
        let imported = import(&exported).unwrap();
        let layer = imported.compositions[imported.main].children[0];
        assert!(imported.property_is_animated(layer, &PropPath::new("transform.position")));
        let value = imported
            .value_at(layer, &PropPath::new("transform.position"), 60.0)
            .unwrap();
        assert_eq!(value, Value::DVec2(DVec2::new(120.0, 20.0)));
    }

    #[test]
    fn gradient_fill_round_trips() {
        let mut doc = Document::empty();
        let comp = doc.main;
        let group = doc.create_node(Node::new("Gradient Group", NodeKind::Group));
        let rect = doc.create_node(Node::new(
            "Rect",
            NodeKind::Shape(ShapeKind::Rect {
                pos: Animated::new(DVec2::new(256.0, 256.0)),
                size: Animated::new(DVec2::new(240.0, 180.0)),
                rounded: Animated::new(0.0),
            }),
        ));
        let fill = doc.create_node(Node::new(
            "Gradient Fill",
            NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::linear(
                    DVec2::new(136.0, 256.0),
                    DVec2::new(376.0, 256.0),
                    GradientStops(vec![
                        GradientStop {
                            offset: 0.0,
                            color: Color::rgba(1.0, 0.0, 0.0, 1.0),
                        },
                        GradientStop {
                            offset: 1.0,
                            color: Color::rgba(0.0, 0.0, 1.0, 0.5),
                        },
                    ]),
                ),
                rule: FillRule::NonZero,
            }),
        ));
        doc.attach(rect, Parent::Node(group), 0).unwrap();
        doc.attach(fill, Parent::Node(group), 1).unwrap();
        doc.attach(group, Parent::Comp(comp), 0).unwrap();

        let exported = export(&doc).unwrap();
        let text = serde_json::to_string(&exported).unwrap();
        assert!(text.contains("\"ty\":\"gf\""));

        let imported = import(&exported).unwrap();
        let layer = imported.compositions[imported.main].children[0];
        fn find_gradient(doc: &Document, id: renamite_model::NodeId) -> bool {
            let node = &doc.nodes[id];
            if matches!(
                &node.kind,
                NodeKind::Style(StyleKind::Fill {
                    paint: StylePaint::Gradient(_),
                    ..
                })
            ) {
                return true;
            }
            node.children
                .iter()
                .copied()
                .any(|child| find_gradient(doc, child))
        }
        assert!(find_gradient(&imported, layer));
    }

    #[test]
    fn star_exports_as_polystar() {
        let mut doc = Document::empty();
        let comp = doc.main;
        let group = doc.create_node(Node::new("Star Group", NodeKind::Group));
        let star = doc.create_node(Node::new(
            "Star",
            NodeKind::Shape(ShapeKind::Star {
                pos: Animated::new(DVec2::new(256.0, 256.0)),
                points: Animated::new(5.0),
                inner_r: Animated::new(50.0),
                outer_r: Animated::new(120.0),
                roundness: Animated::new(0.0),
                kind: StarKind::Star,
            }),
        ));
        let fill = doc.create_node(Node::new(
            "Fill",
            NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::solid(Color::WHITE),
                rule: FillRule::NonZero,
            }),
        ));
        doc.attach(star, Parent::Node(group), 0).unwrap();
        doc.attach(fill, Parent::Node(group), 1).unwrap();
        doc.attach(group, Parent::Comp(comp), 0).unwrap();

        let value = export(&doc).unwrap();
        let text = serde_json::to_string(&value).unwrap();
        assert!(text.contains("\"ty\":\"sr\""));
        assert!(text.contains("\"sy\":1"));
    }

    #[test]
    fn trim_and_round_corners_round_trip() {
        let mut doc = Document::empty();
        let comp = doc.main;
        let group = doc.create_node(Node::new("Modifiers", NodeKind::Group));
        let rect = doc.create_node(Node::new(
            "Rect",
            NodeKind::Shape(ShapeKind::Rect {
                pos: Animated::new(DVec2::new(256.0, 256.0)),
                size: Animated::new(DVec2::new(200.0, 200.0)),
                rounded: Animated::new(0.0),
            }),
        ));
        let trim = doc.create_node(Node::new(
            "Trim",
            NodeKind::Modifier(ModifierKind::TrimPath {
                start: Animated::new(0.0),
                end: Animated::new(0.5),
                offset: Animated::new(0.0),
                mode: TrimMode::Individually,
            }),
        ));
        let round = doc.create_node(Node::new(
            "Round",
            NodeKind::Modifier(ModifierKind::RoundCorners {
                radius: Animated::new(12.0),
            }),
        ));
        let fill = doc.create_node(Node::new(
            "Fill",
            NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::solid(Color::BLACK),
                rule: FillRule::NonZero,
            }),
        ));
        doc.attach(rect, Parent::Node(group), 0).unwrap();
        doc.attach(trim, Parent::Node(group), 1).unwrap();
        doc.attach(round, Parent::Node(group), 2).unwrap();
        doc.attach(fill, Parent::Node(group), 3).unwrap();
        doc.attach(group, Parent::Comp(comp), 0).unwrap();

        let exported = export(&doc).unwrap();
        let text = serde_json::to_string(&exported).unwrap();
        assert!(text.contains("\"ty\":\"tm\""));
        assert!(text.contains("\"ty\":\"rd\""));

        let imported = import(&exported).unwrap();
        fn count_modifiers(doc: &Document, id: renamite_model::NodeId) -> usize {
            let node = &doc.nodes[id];
            let own = usize::from(matches!(node.kind, NodeKind::Modifier(_)));
            own + node
                .children
                .iter()
                .copied()
                .map(|child| count_modifiers(doc, child))
                .sum::<usize>()
        }
        let layer = imported.compositions[imported.main].children[0];
        assert_eq!(count_modifiers(&imported, layer), 2);
    }

    #[test]
    fn unknown_shapes_are_nonfatal() {
        let value = serde_json::json!({
            "v": "5.5.9",
            "fr": 30.0,
            "ip": 0.0,
            "op": 60.0,
            "w": 100,
            "h": 100,
            "nm": "Unknown Test",
            "layers": [
                {
                    "ty": 4,
                    "ind": 1,
                    "nm": "Shape Layer",
                    "ks": {},
                    "shapes": [
                        {
                            "ty": "not-a-real-shape",
                            "nm": "Unsupported"
                        },
                        {
                            "ty": "el",
                            "nm": "Ellipse",
                            "p": { "a": 0, "k": [50, 50] },
                            "s": { "a": 0, "k": [40, 40] }
                        }
                    ]
                }
            ]
        });
        let report = import_with_report(&value).unwrap();
        assert!(!report.warnings.is_empty());
        let layer = report.value.compositions[report.value.main].children[0];
        assert_eq!(report.value.nodes[layer].children.len(), 1);
    }

    #[test]
    fn dashed_stroke_round_trips() {
        let lottie = serde_json::json!({
            "v": "5.5.9",
            "fr": 60.0,
            "ip": 0.0,
            "op": 60.0,
            "w": 100,
            "h": 100,
            "layers": [{
                "ty": 4,
                "ind": 1,
                "nm": "Dashed Stroke",
                "ks": {},
                "shapes": [
                    {
                        "ty": "sh",
                        "nm": "Line",
                        "ks": {
                            "a": 0,
                            "k": {
                                "c": false,
                                "v": [[0, 0], [100, 0]],
                                "i": [[0, 0], [0, 0]],
                                "o": [[0, 0], [0, 0]]
                            }
                        }
                    },
                    {
                        "ty": "st",
                        "nm": "Stroke",
                        "c": { "a": 0, "k": [0, 0, 0, 1] },
                        "o": { "a": 0, "k": 100 },
                        "w": { "a": 0, "k": 4 },
                        "lc": 2,
                        "lj": 2,
                        "d": [
                            {
                                "n": "d",
                                "v": { "a": 0, "k": 12 }
                            },
                            {
                                "n": "g",
                                "v": { "a": 0, "k": 8 }
                            },
                            {
                                "n": "o",
                                "v": { "a": 0, "k": 3 }
                            }
                        ]
                    }
                ]
            }]
        });

        let document = import(&lottie).unwrap();

        let exported = export(&document).unwrap();

        let serialized = serde_json::to_string(&exported).unwrap();

        assert!(serialized.contains("\"n\":\"d\""));
        assert!(serialized.contains("\"n\":\"g\""));
        assert!(serialized.contains("\"n\":\"o\""));
    }

    #[test]
    fn repeater_falloff_round_trips() {
        let lottie = serde_json::json!({
            "v": "5.5.9",
            "fr": 60.0,
            "ip": 0.0,
            "op": 60.0,
            "w": 100,
            "h": 100,
            "layers": [{ "ty": 4, "ind": 1, "nm": "L", "ks": {}, "shapes": [
                { "ty": "rp", "nm": "Rep",
                  "c": {"a": 0, "k": 4},
                  "o": {"a": 0, "k": 0},
                  "tr": {
                      "p": {"a": 0, "k": [30, 0]},
                      "so": {"a": 0, "k": 100},
                      "eo": {"a": 0, "k": 25}
                  } }
            ]}]
        });

        let report = import_with_report(&lottie).unwrap();
        assert!(
            report.warnings.is_empty(),
            "so/eo must no longer warn: {:?}",
            report.warnings
        );

        let layer = report.value.compositions[report.value.main].children[0];
        let rep = report.value.nodes[layer].children[0];
        let NodeKind::Modifier(ModifierKind::Repeater {
            start_opacity,
            end_opacity,
            ..
        }) = &report.value.nodes[rep].kind
        else {
            panic!("expected repeater")
        };
        assert!((start_opacity.base - 1.0).abs() < 1e-9);
        assert!((end_opacity.base - 0.25).abs() < 1e-9);

        let exported = export(&report.value).unwrap();
        let text = serde_json::to_string(&exported).unwrap();
        assert!(text.contains("\"eo\""));
        assert!(text.contains("\"so\""));
    }

    #[test]
    fn text_bakes_to_path_outline() {
        let mut doc = Document::empty();
        let comp = doc.main;
        let group = doc.create_node(Node::new("Text Group", NodeKind::Group));
        let text = doc.create_node(Node::new(
            "Text",
            NodeKind::Text(TextNode {
                text: "Hi".into(),
                size: Animated::new(48.0),
                align: TextAlign::Left,
                font: None,
            }),
        ));
        let fill = doc.create_node(Node::new(
            "Fill",
            NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::solid(Color::BLACK),
                rule: FillRule::NonZero,
            }),
        ));
        doc.attach(text, Parent::Node(group), 0).unwrap();
        doc.attach(fill, Parent::Node(group), 1).unwrap();
        doc.attach(group, Parent::Comp(comp), 0).unwrap();

        let report = export_with_report(&doc).unwrap();
        assert!(
            report
                .warnings
                .iter()
                .all(|w| !w.message.contains("animated `text.size`")),
            "static text must not warn about size: {:?}",
            report.warnings
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.message.contains("baked to path outlines")),
            "text bake must be surfaced: {:?}",
            report.warnings
        );
        let layers = report.value["layers"].as_array().unwrap();
        let shapes = layers[0]["shapes"].as_array().unwrap();
        let serialized = serde_json::to_string(&shapes).unwrap();
        assert!(serialized.contains("\"ty\":\"sh\""));
        assert!(serialized.contains("\"ty\":\"fl\""));
        assert!(serialized.contains("\"v\":[["));
    }

    #[test]
    fn animated_text_size_warns_and_bakes_base() {
        let mut doc = Document::empty();
        let comp = doc.main;
        let group = doc.create_node(Node::new("Text Group", NodeKind::Group));
        let text = doc.create_node(Node::new(
            "Text",
            NodeKind::Text(TextNode {
                text: "Pulse".into(),
                size: Animated::new(48.0),
                align: TextAlign::Center,
                font: None,
            }),
        ));
        doc.attach(text, Parent::Node(group), 0).unwrap();
        doc.attach(group, Parent::Comp(comp), 0).unwrap();
        doc.add_keyframe(
            text,
            &PropPath::new("text.size"),
            Frame(0),
            &Value::F64(48.0),
        )
        .unwrap();
        doc.add_keyframe(
            text,
            &PropPath::new("text.size"),
            Frame(60),
            &Value::F64(96.0),
        )
        .unwrap();

        let report = export_with_report(&doc).unwrap();
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.message.contains("text.size")),
            "animated size must warn, got {:?}",
            report.warnings
        );
    }

    #[test]
    fn export_direct_masks_to_masks_properties() {
        let mut doc = Document::empty();
        let comp = doc.main;
        let group = doc.create_node(Node::new("Masked Group", NodeKind::Group));
        let mask = doc.create_node(Node::new(
            "Mask",
            NodeKind::Mask(MaskProps {
                inverted: true,
                shape: ShapeKind::Path(Animated::new(renamite_geometry::VectorPath::default())),
            }),
        ));
        let rect = doc.create_node(Node::new(
            "Rect",
            NodeKind::Shape(ShapeKind::Rect {
                pos: Animated::new(DVec2::new(256.0, 256.0)),
                size: Animated::new(DVec2::new(200.0, 200.0)),
                rounded: Animated::new(0.0),
            }),
        ));
        let fill = doc.create_node(Node::new(
            "Fill",
            NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::solid(Color::WHITE),
                rule: FillRule::NonZero,
            }),
        ));
        doc.attach(mask, Parent::Node(group), 0).unwrap();
        doc.attach(rect, Parent::Node(group), 1).unwrap();
        doc.attach(fill, Parent::Node(group), 2).unwrap();
        doc.attach(group, Parent::Comp(comp), 0).unwrap();

        let value = export(&doc).unwrap();
        let masks = value["layers"][0]["masksProperties"].as_array().unwrap();
        assert_eq!(masks.len(), 1);
        assert_eq!(masks[0]["nm"], "Mask");
        assert_eq!(masks[0]["mode"], "s");
        assert_eq!(masks[0]["inv"], true);
        assert!(masks[0]["pt"]["a"] == 0);
    }

    #[test]
    fn offset_path_round_trips_lottie() {
        let lottie = serde_json::json!({
            "v": "5.5.9",
            "fr": 60.0,
            "ip": 0.0,
            "op": 60.0,
            "w": 100,
            "h": 100,
            "layers": [{
                "ty": 4,
                "ind": 1,
                "nm": "Offset",
                "ks": {},
                "shapes": [
                    {
                        "ty": "rc",
                        "nm": "Rect",
                        "p": { "a": 0, "k": [0, 0] },
                        "s": { "a": 0, "k": [20, 20] },
                        "r": { "a": 0, "k": 0 }
                    },
                    {
                        "ty": "op",
                        "nm": "Offset Path",
                        "a": { "a": 0, "k": 8 },
                        "ml": 4
                    },
                    {
                        "ty": "fl",
                        "nm": "Fill",
                        "c": { "a": 0, "k": [1, 1, 1, 1] },
                        "o": { "a": 0, "k": 100 }
                    }
                ]
            }]
        });

        let doc = import(&lottie).unwrap();

        assert!(doc.nodes.values().any(|node| {
            matches!(
                node.kind,
                NodeKind::Modifier(ModifierKind::OffsetPath { .. })
            )
        }));

        let exported = export(&doc).unwrap();
        let text = serde_json::to_string(&exported).unwrap();

        assert!(text.contains("\"ty\":\"op\""));
        assert!(text.contains("\"Offset Path\""));
    }

    #[test]
    fn zigzag_round_trips_lottie() {
        let lottie = serde_json::json!({
            "v": "5.5.9",
            "fr": 60.0,
            "ip": 0.0,
            "op": 60.0,
            "w": 100,
            "h": 100,
            "layers": [{
                "ty": 4,
                "ind": 1,
                "nm": "Zig",
                "ks": {},
                "shapes": [
                    {
                        "ty": "rc",
                        "nm": "Rect",
                        "p": { "a": 0, "k": [0, 0] },
                        "s": { "a": 0, "k": [20, 20] },
                        "r": { "a": 0, "k": 0 }
                    },
                    {
                        "ty": "zz",
                        "nm": "Zig Zag",
                        "a": { "a": 0, "k": 12 },
                        "f": { "a": 0, "k": 4 },
                        "s": 1
                    },
                    {
                        "ty": "fl",
                        "nm": "Fill",
                        "c": { "a": 0, "k": [1, 1, 1, 1] },
                        "o": { "a": 0, "k": 100 }
                    }
                ]
            }]
        });

        let doc = import(&lottie).unwrap();

        assert!(doc.nodes.values().any(|node| {
            matches!(
                node.kind,
                NodeKind::Modifier(ModifierKind::ZigZag { smooth: true, .. })
            )
        }));

        let exported = export(&doc).unwrap();
        let text = serde_json::to_string(&exported).unwrap();

        assert!(text.contains("\"ty\":\"zz\""));
        assert!(text.contains("\"Zig Zag\""));
    }

    #[test]
    fn pucker_bloat_round_trips_lottie() {
        let lottie = serde_json::json!({
            "v": "5.5.9",
            "fr": 60.0,
            "ip": 0.0,
            "op": 60.0,
            "w": 100,
            "h": 100,
            "layers": [{
                "ty": 4,
                "ind": 1,
                "nm": "PB",
                "ks": {},
                "shapes": [
                    {
                        "ty": "rc",
                        "nm": "Rect",
                        "p": { "a": 0, "k": [0, 0] },
                        "s": { "a": 0, "k": [20, 20] },
                        "r": { "a": 0, "k": 0 }
                    },
                    {
                        "ty": "pb",
                        "nm": "Pucker & Bloat",
                        "a": { "a": 0, "k": -40 }
                    },
                    {
                        "ty": "fl",
                        "nm": "Fill",
                        "c": { "a": 0, "k": [1, 1, 1, 1] },
                        "o": { "a": 0, "k": 100 }
                    }
                ]
            }]
        });

        let doc = import(&lottie).unwrap();

        assert!(doc.nodes.values().any(|node| {
            matches!(node.kind, NodeKind::Modifier(ModifierKind::PuckerBloat { .. }))
        }));

        let exported = export(&doc).unwrap();
        let text = serde_json::to_string(&exported).unwrap();

        assert!(text.contains("\"ty\":\"pb\""));
        assert!(text.contains("\"Pucker & Bloat\""));
    }

    #[test]
    fn import_layer_masks_properties() {
        let lottie = serde_json::json!({
            "v": "5.5.9",
            "fr": 60.0,
            "ip": 0.0,
            "op": 60.0,
            "w": 100,
            "h": 100,
            "layers": [{
                "ty": 4,
                "ind": 1,
                "nm": "Masked",
                "ks": {},
                "masksProperties": [{
                    "nm": "Mask 1",
                    "mode": "s",
                    "inv": true,
                    "o": { "a": 0, "k": 100 },
                    "x": { "a": 0, "k": 0 },
                    "pt": {
                        "a": 0,
                        "k": {
                            "c": true,
                            "v": [[0, 0], [10, 0], [10, 10], [0, 10]],
                            "i": [[0, 0], [0, 0], [0, 0], [0, 0]],
                            "o": [[0, 0], [0, 0], [0, 0], [0, 0]]
                        }
                    }
                }],
                "shapes": []
            }]
        });

        let document = import(&lottie).unwrap();
        let layer = document.compositions[document.main].children[0];
        let children = &document.nodes[layer].children;
        assert!(
            children.iter().any(|&id| {
                matches!(
                    &document.nodes[id].kind,
                    NodeKind::Mask(MaskProps { inverted: true, .. })
                )
            }),
            "inverted mask must import as Mask with inverted=true"
        );
    }
}
