use std::collections::HashMap;

use renamite_animation::{Animated, AnimatedTransform};
use renamite_model::{
    BlendMode, CompId, Composition, Document, FillRule, GradientKind, LayerProps, ModifierKind,
    Node, NodeId, NodeKind, ShapeKind, StarKind, StrokeCap, StrokeJoin, StyleKind, StylePaint,
    TimeMap, TrimMode,
};
use serde_json::{Value, json};

use crate::property::{
    export_angle, export_color, export_gradient, export_path, export_scalar, export_vec2,
};
use crate::{LottieError, LottieReport, LottieWarning};

pub fn export_with_report(document: &Document) -> Result<LottieReport<Value>, LottieError> {
    let main = document
        .compositions
        .get(document.main)
        .ok_or(LottieError::MissingMainComposition)?
        .clone();
    let mut exporter = Exporter {
        document,
        assets: Vec::new(),
        asset_ids: HashMap::new(),
        warnings: Vec::new(),
        next_asset: 1,
    };
    let layers = exporter.export_composition_layers(document.main)?;
    let root = json!({
        "v": "5.5.9",
        "fr": main.rate.fps(),
        "ip": main.range.0.0,
        "op": main.range.1.0,
        "w": main.size.0,
        "h": main.size.1,
        "nm": main.name,
        "ddd": 0,
        "assets": exporter.assets,
        "layers": layers,
        "meta": {
            "g": "renamite"
        }
    });
    Ok(LottieReport {
        value: root,
        warnings: exporter.warnings,
    })
}

struct Exporter<'a> {
    document: &'a Document,
    assets: Vec<Value>,
    asset_ids: HashMap<CompId, String>,
    warnings: Vec<LottieWarning>,
    next_asset: usize,
}

impl Exporter<'_> {
    fn export_composition_layers(&mut self, comp_id: CompId) -> Result<Vec<Value>, LottieError> {
        let composition = self
            .document
            .compositions
            .get(comp_id)
            .ok_or(LottieError::MissingMainComposition)?
            .clone();
        let mut output = Vec::new();
        let mut bare_run = Vec::new();
        for node_id in composition.children.iter().copied() {
            let Some(node) = self.document.nodes.get(node_id).cloned() else {
                continue;
            };
            match &node.kind {
                NodeKind::Layer(_) | NodeKind::Group | NodeKind::Precomp { .. } => {
                    self.flush_bare_run(&composition, &mut output, &mut bare_run)?;
                    match &node.kind {
                        NodeKind::Layer(props) => {
                            output.push(self.export_layer_node(
                                node_id,
                                &node,
                                props,
                                &composition,
                                output.len() as u32 + 1,
                            ));
                        }
                        NodeKind::Group => {
                            output.push(self.export_group_layer(
                                node_id,
                                &node,
                                &composition,
                                output.len() as u32 + 1,
                            ));
                        }
                        NodeKind::Precomp { comp, time_map } => {
                            output.push(self.export_precomp_layer(
                                node_id,
                                &node,
                                *comp,
                                time_map,
                                &composition,
                                output.len() as u32 + 1,
                            )?);
                        }
                        _ => unreachable!(),
                    }
                }
                NodeKind::Shape(_) | NodeKind::Style(_) | NodeKind::Modifier(_) => {
                    bare_run.push(node_id);
                }
                _ => {
                    self.flush_bare_run(&composition, &mut output, &mut bare_run)?;
                    self.warnings.push(LottieWarning::new(
                        format!("node/{node_id:?}"),
                        "node kind is not representable as a Lottie shape layer",
                    ));
                }
            }
        }
        self.flush_bare_run(&composition, &mut output, &mut bare_run)?;
        for (index, layer) in output.iter_mut().enumerate() {
            layer["ind"] = json!(index as u32 + 1);
        }
        Ok(output)
    }

    fn flush_bare_run(
        &mut self,
        composition: &Composition,
        output: &mut Vec<Value>,
        run: &mut Vec<NodeId>,
    ) -> Result<(), LottieError> {
        if run.is_empty() {
            return Ok(());
        }
        let ids = std::mem::take(run);
        let shape_ids: Vec<NodeId> = ids
            .iter()
            .copied()
            .filter(|id| {
                self.document
                    .nodes
                    .get(*id)
                    .is_some_and(|node| matches!(node.kind, NodeKind::Shape(_)))
            })
            .collect();
        let (transform, opacity, transform_owner) = if shape_ids.len() == 1 {
            let node = &self.document.nodes[shape_ids[0]];
            (
                node.transform.clone(),
                node.opacity.clone(),
                Some(shape_ids[0]),
            )
        } else {
            (AnimatedTransform::identity(), Animated::new(1.0), None)
        };
        let shapes = ids
            .iter()
            .flat_map(|id| self.export_node_item(*id, transform_owner))
            .collect::<Vec<_>>();
        if !shapes.is_empty() {
            output.push(self.shape_layer(
                format!("Shapes {}", output.len() + 1),
                transform_json(&transform, &opacity),
                shapes,
                composition.range.0.0 as f64,
                composition.range.1.0 as f64,
                0.0,
                1.0,
                0,
            ));
        }
        Ok(())
    }

    fn export_layer_node(
        &mut self,
        id: NodeId,
        node: &Node,
        props: &LayerProps,
        _composition: &Composition,
        index: u32,
    ) -> Value {
        let shapes = node
            .children
            .iter()
            .flat_map(|child| self.export_node_item(*child, None))
            .collect();
        let mut layer = self.shape_layer(
            node.name.clone(),
            transform_json(&node.transform, &node.opacity),
            shapes,
            props.in_frame.0 as f64,
            props.out_frame.0 as f64,
            props.in_frame.0 as f64,
            props.time_stretch,
            blend_to_lottie(props.blend),
        );
        layer["ind"] = json!(index);
        layer["hd"] = json!(!node.visible);
        let _ = id;
        layer
    }

    fn export_group_layer(
        &mut self,
        _id: NodeId,
        node: &Node,
        composition: &Composition,
        index: u32,
    ) -> Value {
        let shapes = node
            .children
            .iter()
            .flat_map(|child| self.export_node_item(*child, None))
            .collect();
        let mut layer = self.shape_layer(
            node.name.clone(),
            transform_json(&node.transform, &node.opacity),
            shapes,
            composition.range.0.0 as f64,
            composition.range.1.0 as f64,
            0.0,
            1.0,
            0,
        );
        layer["ind"] = json!(index);
        layer["hd"] = json!(!node.visible);
        layer
    }

    fn export_precomp_layer(
        &mut self,
        id: NodeId,
        node: &Node,
        comp: CompId,
        time_map: &TimeMap,
        parent: &Composition,
        index: u32,
    ) -> Result<Value, LottieError> {
        let asset_id = self.ensure_precomp_asset(comp)?;
        let source = self
            .document
            .compositions
            .get(comp)
            .ok_or(LottieError::InvalidPrecomposition(asset_id.clone()))?;
        Ok(json!({
            "ddd": 0,
            "ind": index,
            "ty": 0,
            "nm": node.name,
            "refId": asset_id,
            "w": source.size.0,
            "h": source.size.1,
            "sr": time_map.stretch,
            "ks": transform_json(&node.transform, &node.opacity),
            "ao": 0,
            "ip": parent.range.0.0,
            "op": parent.range.1.0,
            "st": time_map.offset.0,
            "bm": 0,
            "hd": !node.visible,
            "renamiteNode": format!("{id:?}")
        }))
    }

    fn ensure_precomp_asset(&mut self, comp_id: CompId) -> Result<String, LottieError> {
        if let Some(existing) = self.asset_ids.get(&comp_id) {
            return Ok(existing.clone());
        }
        let asset_id = format!("comp_{}", self.next_asset);
        self.next_asset += 1;
        // Register before recursion to break composition cycles.
        self.asset_ids.insert(comp_id, asset_id.clone());
        let composition = self
            .document
            .compositions
            .get(comp_id)
            .ok_or_else(|| LottieError::InvalidPrecomposition(asset_id.clone()))?
            .clone();
        let layers = self.export_composition_layers(comp_id)?;
        self.assets.push(json!({
            "id": asset_id,
            "w": composition.size.0,
            "h": composition.size.1,
            "fr": composition.rate.fps(),
            "layers": layers
        }));
        Ok(asset_id)
    }

    #[allow(clippy::too_many_arguments)]
    fn shape_layer(
        &self,
        name: String,
        transform: Value,
        shapes: Vec<Value>,
        ip: f64,
        op: f64,
        st: f64,
        sr: f64,
        bm: u8,
    ) -> Value {
        json!({
            "ddd": 0,
            "ind": 0,
            "ty": 4,
            "nm": name,
            "sr": sr,
            "ks": transform,
            "ao": 0,
            "shapes": shapes,
            "ip": ip,
            "op": op,
            "st": st,
            "bm": bm
        })
    }

    fn export_node_item(&mut self, id: NodeId, transform_owner: Option<NodeId>) -> Vec<Value> {
        let Some(node) = self.document.nodes.get(id).cloned() else {
            return Vec::new();
        };
        if !node.visible {
            return Vec::new();
        }
        match &node.kind {
            NodeKind::Group | NodeKind::Layer(_) => {
                let mut items = node
                    .children
                    .iter()
                    .flat_map(|child| self.export_node_item(*child, None))
                    .collect::<Vec<_>>();
                items.push(group_transform_json(&node.transform, &node.opacity));
                vec![json!({
                    "ty": "gr",
                    "nm": node.name,
                    "hd": !node.visible,
                    "it": items
                })]
            }
            NodeKind::Shape(shape) => {
                let item = shape_json(&node.name, shape);
                if transform_owner == Some(id)
                    || transform_is_identity(&node.transform, &node.opacity)
                {
                    vec![item]
                } else {
                    vec![json!({
                        "ty": "gr",
                        "nm": node.name,
                        "it": [
                            item,
                            group_transform_json(
                                &node.transform,
                                &node.opacity
                            )
                        ]
                    })]
                }
            }
            NodeKind::Style(style) => {
                vec![style_json(&node.name, style, &node.opacity)]
            }
            NodeKind::Modifier(modifier) => {
                modifier_json(&node.name, modifier).into_iter().collect()
            }
            other => {
                self.warnings.push(LottieWarning::new(
                    format!("node/{id:?}"),
                    format!("nested node kind `{}` was skipped", node_kind_name(other)),
                ));
                Vec::new()
            }
        }
    }
}

fn transform_is_identity(transform: &AnimatedTransform, opacity: &Animated<f64>) -> bool {
    transform.anchor.keyframes.is_empty()
        && transform.anchor.base == glam::DVec2::ZERO
        && transform.position.keyframes.is_empty()
        && transform.position.base == glam::DVec2::ZERO
        && transform.scale.keyframes.is_empty()
        && transform.scale.base == glam::DVec2::splat(100.0)
        && transform.rotation.keyframes.is_empty()
        && transform.rotation.base.0 == 0.0
        && transform.skew.keyframes.is_empty()
        && transform.skew.base == 0.0
        && transform.skew_axis.keyframes.is_empty()
        && transform.skew_axis.base == 0.0
        && opacity.keyframes.is_empty()
        && opacity.base == 1.0
}

fn transform_json(transform: &AnimatedTransform, opacity: &Animated<f64>) -> Value {
    json!({
        "a": export_vec2(&transform.anchor),
        "p": export_vec2(&transform.position),
        "s": export_vec2(&transform.scale),
        "r": export_angle(&transform.rotation),
        "o": export_scalar(opacity, 100.0),
        "sk": export_scalar(&transform.skew, 1.0),
        "sa": export_scalar(&transform.skew_axis, 1.0)
    })
}

fn group_transform_json(transform: &AnimatedTransform, opacity: &Animated<f64>) -> Value {
    let mut value = transform_json(transform, opacity);
    value["ty"] = json!("tr");
    value
}

fn shape_json(name: &str, shape: &ShapeKind) -> Value {
    match shape {
        ShapeKind::Rect { pos, size, rounded } => json!({
            "ty": "rc",
            "nm": name,
            "d": 1,
            "p": export_vec2(pos),
            "s": export_vec2(size),
            "r": export_scalar(rounded, 1.0)
        }),
        ShapeKind::Ellipse { pos, size } => json!({
            "ty": "el",
            "nm": name,
            "d": 1,
            "p": export_vec2(pos),
            "s": export_vec2(size)
        }),
        ShapeKind::Path(path) => json!({
            "ty": "sh",
            "nm": name,
            "d": 1,
            "ks": export_path(path)
        }),
        ShapeKind::Star {
            pos,
            points,
            inner_r,
            outer_r,
            roundness,
            kind,
        } => json!({
            "ty": "sr",
            "nm": name,
            "d": 1,
            "sy": 1,
            "p": export_vec2(pos),
            "r": { "a": 0, "k": 0.0 },
            "pt": export_scalar(points, 1.0),
            "or": export_scalar(outer_r, 1.0),
            "os": export_scalar(roundness, 1.0),
            "ir": export_scalar(inner_r, 1.0),
            "is": export_scalar(roundness, 1.0),
            "renamiteStarKind": match kind {
                StarKind::Star => "star",
                StarKind::Burst => "burst"
            }
        }),
        ShapeKind::Polygon {
            pos,
            points,
            outer_r,
            roundness,
        } => json!({
            "ty": "sr",
            "nm": name,
            "d": 1,
            "sy": 2,
            "p": export_vec2(pos),
            "r": { "a": 0, "k": 0.0 },
            "pt": export_scalar(points, 1.0),
            "or": export_scalar(outer_r, 1.0),
            "os": export_scalar(roundness, 1.0)
        }),
    }
}

fn style_json(name: &str, style: &StyleKind, opacity: &Animated<f64>) -> Value {
    match style {
        StyleKind::Fill { paint, rule } => match paint {
            StylePaint::Solid { color } => json!({
                "ty": "fl",
                "nm": name,
                "c": export_color(color),
                "o": export_scalar(opacity, 100.0),
                "r": fill_rule_to_lottie(*rule)
            }),
            StylePaint::Gradient(gradient) => {
                let (count, stops) = export_gradient(&gradient.stops);
                let mut value = json!({
                    "ty": "gf",
                    "nm": name,
                    "t": gradient_kind_to_lottie(gradient.kind),
                    "s": export_vec2(&gradient.start),
                    "e": export_vec2(&gradient.end),
                    "g": {
                        "p": count,
                        "k": stops
                    },
                    "o": export_scalar(opacity, 100.0),
                    "r": fill_rule_to_lottie(*rule)
                });
                if gradient.kind == GradientKind::Radial {
                    value["h"] = json!({ "a": 0, "k": 0.0 });
                    value["a"] = json!({ "a": 0, "k": 0.0 });
                }
                value
            }
        },
        StyleKind::Stroke {
            paint,
            width,
            cap,
            join,
            dash,
        } => {
            let mut value = match paint {
                StylePaint::Solid { color } => json!({
                    "ty": "st",
                    "nm": name,
                    "c": export_color(color),
                    "o": export_scalar(opacity, 100.0),
                    "w": export_scalar(width, 1.0),
                    "lc": stroke_cap_to_lottie(*cap),
                    "lj": stroke_join_to_lottie(*join)
                }),
                StylePaint::Gradient(gradient) => {
                    let (count, stops) = export_gradient(&gradient.stops);
                    let mut value = json!({
                        "ty": "gs",
                        "nm": name,
                        "t": gradient_kind_to_lottie(gradient.kind),
                        "s": export_vec2(&gradient.start),
                        "e": export_vec2(&gradient.end),
                        "g": {
                            "p": count,
                            "k": stops
                        },
                        "o": export_scalar(opacity, 100.0),
                        "w": export_scalar(width, 1.0),
                        "lc": stroke_cap_to_lottie(*cap),
                        "lj": stroke_join_to_lottie(*join)
                    });
                    if gradient.kind == GradientKind::Radial {
                        value["h"] = json!({ "a": 0, "k": 0.0 });
                        value["a"] = json!({ "a": 0, "k": 0.0 });
                    }
                    value
                }
            };
            if let Some(dash) = dash {
                let mut entries = Vec::new();
                for (index, value) in dash.dashes.iter().enumerate() {
                    entries.push(json!({
                        "n": if index % 2 == 0 { "d" } else { "g" },
                        "v": export_scalar(value, 1.0)
                    }));
                }
                entries.push(json!({
                    "n": "o",
                    "v": export_scalar(&dash.offset, 1.0)
                }));
                value["d"] = Value::Array(entries);
            }
            value
        }
    }
}

fn modifier_json(name: &str, modifier: &ModifierKind) -> Option<Value> {
    match modifier {
        ModifierKind::TrimPath {
            start,
            end,
            offset,
            mode,
        } => Some(json!({
            "ty": "tm",
            "nm": name,
            "s": export_scalar(start, 100.0),
            "e": export_scalar(end, 100.0),
            "o": export_scalar(offset, 360.0),
            "m": match mode {
                TrimMode::Individually => 1,
                TrimMode::Simultaneously => 2
            }
        })),
        ModifierKind::RoundCorners { radius } => Some(json!({
            "ty": "rd",
            "nm": name,
            "r": export_scalar(radius, 1.0)
        })),
        ModifierKind::Repeater {
            copies,
            offset,
            transform,
        } => Some(json!({
            "ty": "rp",
            "nm": name,
            "c": export_scalar(copies, 1.0),
            "o": export_scalar(offset, 1.0),
            "tr": {
                "a": export_vec2(&transform.anchor),
                "p": export_vec2(&transform.position),
                "s": export_vec2(&transform.scale),
                "r": export_angle(&transform.rotation),
                "so": { "a": 0, "k": 100.0 },
                "eo": { "a": 0, "k": 100.0 },
                "sk": export_scalar(&transform.skew, 1.0),
                "sa": export_scalar(&transform.skew_axis, 1.0)
            }
        })),
        _ => None,
    }
}

fn fill_rule_to_lottie(rule: FillRule) -> u8 {
    match rule {
        FillRule::NonZero => 1,
        FillRule::EvenOdd => 2,
    }
}

fn gradient_kind_to_lottie(kind: GradientKind) -> u8 {
    match kind {
        GradientKind::Linear => 1,
        GradientKind::Radial => 2,
    }
}

fn stroke_cap_to_lottie(cap: StrokeCap) -> u8 {
    match cap {
        StrokeCap::Butt => 1,
        StrokeCap::Round => 2,
        StrokeCap::Square => 3,
    }
}

fn stroke_join_to_lottie(join: StrokeJoin) -> u8 {
    match join {
        StrokeJoin::Miter => 1,
        StrokeJoin::Round => 2,
        StrokeJoin::Bevel => 3,
    }
}

fn blend_to_lottie(blend: BlendMode) -> u8 {
    match blend {
        BlendMode::Normal => 0,
        BlendMode::Multiply => 1,
        BlendMode::Screen => 2,
    }
}

fn node_kind_name(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Group => "group",
        NodeKind::Layer(_) => "layer",
        NodeKind::Shape(_) => "shape",
        NodeKind::Style(_) => "style",
        NodeKind::Modifier(_) => "modifier",
        NodeKind::Text(_) => "text",
        NodeKind::Image(_) => "image",
        NodeKind::Precomp { .. } => "precomposition",
        NodeKind::Mask(_) => "mask",
    }
}
