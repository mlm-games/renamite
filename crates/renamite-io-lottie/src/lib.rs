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
//! - Trim Path, Round Corners, Offset Path, Repeater, Zig Zag, and Pucker & Bloat
//! - Static and keyframed scalar/vector/color/path/gradient properties
//! - Hold, linear, and cubic-bezier interpolation
//!
//! Unsupported Lottie objects are skipped in best-effort mode and returned as
//! warnings by [`import_with_report`] / [`export_with_report`].
//! Project-level state with no Lottie representation (named clips, state
//! machines) is reported by [`export_project_with_report`] and fails under
//! the CLI `--strict` flag.

mod export;
mod import;
mod property;

use renamite_animation::FrameRate;
use renamite_model::Document;
use serde_json::Value;

pub use export::export_with_report;
pub use import::{MAX_LOTTIE_BYTES, import_with_report};

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
    #[error("Lottie input limit exceeded: {0}")]
    InputLimit(&'static str),
}

/// Import Lottie JSON, discarding non-fatal warnings.
pub fn import(json: &Value) -> Result<Document, LottieError> {
    Ok(import_with_report(json)?.value)
}

pub fn import_bytes(bytes: &[u8]) -> Result<LottieReport<Document>, LottieError> {
    import::preflight_bytes(bytes)?;
    let value: Value = serde_json::from_slice(bytes)?;
    import_with_report(&value)
}

/// Export a Renamite document to Lottie JSON, discarding non-fatal warnings.
pub fn export(doc: &Document) -> Result<Value, LottieError> {
    Ok(export_with_report(doc)?.value)
}

/// Project-aware export: like [`export_with_report`], plus lossy-export
/// warnings for project-level state Lottie cannot represent (named clips,
/// state machines, auto-start). Timeline keyframes on the main composition
/// are exported; only the machine/clip layer is dropped.
pub fn export_project_with_report(
    doc: &Document,
    clip_count: usize,
    machine_count: usize,
    has_start_machine: bool,
) -> Result<LottieReport<Value>, LottieError> {
    let mut report = export_with_report(doc)?;
    if clip_count > 0 {
        report.warnings.push(LottieWarning::new(
            "clips",
            format!(
                "{clip_count} named clip(s) are not representable in Lottie and were dropped (main timeline only)"
            ),
        ));
    }
    if machine_count > 0 || has_start_machine {
        report.warnings.push(LottieWarning::new(
            "machines",
            "state machines are not representable in Lottie and were dropped (bake frames or drive via host instead)",
        ));
    }
    Ok(report)
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
        Color, FillRule, GradientStop, GradientStops, Node, NodeKind, Parent, PropPath, ShapeKind,
        StyleKind, StylePaint, TextAlign, TextNode, Value,
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

    fn gradient_doc(offsets: &[f64]) -> Document {
        let mut doc = Document::empty();
        let comp = doc.main;
        let group = doc.create_node(Node::new("Gradient Group", NodeKind::Group));
        let rect = doc.create_node(Node::new(
            "Rect",
            NodeKind::Shape(ShapeKind::Rect {
                pos: Animated::new(DVec2::new(50.0, 50.0)),
                size: Animated::new(DVec2::new(100.0, 100.0)),
                rounded: Animated::new(0.0),
            }),
        ));
        let stops = GradientStops(
            offsets
                .iter()
                .enumerate()
                .map(|(i, o)| GradientStop {
                    offset: *o,
                    color: Color::rgba(i as f64 * 0.4, 0.2, 1.0 - i as f64 * 0.4, 1.0),
                })
                .collect(),
        );
        let fill = doc.create_node(Node::new(
            "Gradient Fill",
            NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::linear(DVec2::new(0.0, 0.0), DVec2::new(100.0, 0.0), stops),
                rule: FillRule::NonZero,
            }),
        ));
        doc.attach(rect, Parent::Node(group), 0).unwrap();
        doc.attach(fill, Parent::Node(group), 1).unwrap();
        doc.attach(group, Parent::Comp(comp), 0).unwrap();
        doc
    }

    fn imported_gradient_offsets(doc: &Document) -> Vec<f64> {
        let mut found = Vec::new();
        for node in doc.nodes.values() {
            if let NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::Gradient(g),
                ..
            }) = &node.kind
            {
                found = g.stops.base.0.iter().map(|s| s.offset).collect();
            }
        }
        found
    }

    #[test]
    fn static_gradient_offsets_survive_roundtrip() {
        let doc = gradient_doc(&[0.0, 0.2, 1.0]);
        let exported = export(&doc).unwrap();
        let text = serde_json::to_string(&exported).unwrap();
        assert!(
            text.contains("0.2"),
            "non-uniform offsets must be packed verbatim, got {text}"
        );
        let imported = import(&exported).unwrap();
        let offsets = imported_gradient_offsets(&imported);
        assert_eq!(offsets, vec![0.0, 0.2, 1.0]);
    }

    fn count_shapes(value: &serde_json::Value, ty: &str) -> usize {
        let mut count = 0;
        if value.get("ty").and_then(|t| t.as_str()) == Some(ty) {
            count += 1;
        }
        if let Some(items) = value.get("it").and_then(|it| it.as_array()) {
            count += items
                .iter()
                .map(|item| count_shapes(item, ty))
                .sum::<usize>();
        }
        if let Some(shapes) = value.get("shapes").and_then(|s| s.as_array()) {
            count += shapes
                .iter()
                .map(|item| count_shapes(item, ty))
                .sum::<usize>();
        }
        if let Some(layers) = value.get("layers").and_then(|l| l.as_array()) {
            count += layers
                .iter()
                .map(|item| count_shapes(item, ty))
                .sum::<usize>();
        }
        count
    }

    fn count_flat_shapes(value: &serde_json::Value) -> usize {
        let mut count = 0;
        if value.get("ty").and_then(|t| t.as_str()) == Some("sh") {
            assert!(
                value.pointer("/ks/k/c").and_then(|c| c.as_bool()).is_some(),
                "baked `sh` must use the flat single-contour form, got {}",
                value.pointer("/ks/k/c").unwrap_or(&serde_json::Value::Null)
            );
            count += 1;
        }
        if let Some(items) = value.get("it").and_then(|it| it.as_array()) {
            count += items.iter().map(count_flat_shapes).sum::<usize>();
        }
        if let Some(shapes) = value.get("shapes").and_then(|s| s.as_array()) {
            count += shapes.iter().map(count_flat_shapes).sum::<usize>();
        }
        if let Some(layers) = value.get("layers").and_then(|l| l.as_array()) {
            count += layers.iter().map(count_flat_shapes).sum::<usize>();
        }
        count
    }

    fn count_imported_paths(doc: &Document) -> usize {
        doc.nodes
            .values()
            .filter(|node| matches!(&node.kind, NodeKind::Shape(ShapeKind::Path(_))))
            .count()
    }

    #[test]
    fn text_contours_survive_export_import() {
        let mut doc = Document::empty();
        let comp = doc.main;
        let group = doc.create_node(Node::new("Text Group", NodeKind::Group));
        let text = doc.create_node(Node::new(
            "Text",
            NodeKind::Text(TextNode {
                text: "O".into(),
                size: Animated::new(48.0),
                align: TextAlign::Left,
                font: None,
                tracking: Animated::new(0.0),
                leading: Animated::new(0.0),
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

        let exported = export(&doc).unwrap();
        let flat = count_flat_shapes(&exported);
        assert!(
            flat > 1,
            "`O` outline must split into per-contour `sh` items (outer + hole), got {flat}"
        );
        let imported = import(&exported).unwrap();
        assert_eq!(
            count_imported_paths(&imported),
            flat,
            "every baked contour must re-import (holes were dropped by the nested form)"
        );
    }

    #[test]
    fn static_compound_warns_and_splits_to_paths() {
        use kurbo::Shape as _;
        let contour = |x0: f64, y0: f64, x1: f64, y1: f64| {
            Animated::new(renamite_geometry::VectorPath::from_bez_path(
                &kurbo::Rect::new(x0, y0, x1, y1).to_path(0.1),
            ))
        };
        let mut doc = Document::empty();
        let comp = doc.main;
        let group = doc.create_node(Node::new("Compound Group", NodeKind::Group));
        let shape = doc.create_node(Node::new(
            "Compound",
            NodeKind::Shape(ShapeKind::CompoundPath(renamite_model::CompoundPath {
                contours: vec![
                    contour(0.0, 0.0, 10.0, 10.0),
                    contour(20.0, 20.0, 30.0, 30.0),
                ],
            })),
        ));
        let fill = doc.create_node(Node::new(
            "Fill",
            NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::solid(Color::BLACK),
                rule: FillRule::NonZero,
            }),
        ));
        doc.attach(shape, Parent::Node(group), 0).unwrap();
        doc.attach(fill, Parent::Node(group), 1).unwrap();
        doc.attach(group, Parent::Comp(comp), 0).unwrap();

        let report = export_with_report(&doc).unwrap();
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.message.contains("one `sh` item per contour")),
            "static compounds must warn about the lossy split, got {:?}",
            report.warnings
        );
        assert_eq!(count_shapes(&report.value, "sh"), 2);
        let imported = import(&report.value).unwrap();
        assert_eq!(count_imported_paths(&imported), 2);
    }
}
