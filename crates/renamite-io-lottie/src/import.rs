use std::collections::{HashMap, HashSet};

use glam::DVec2;
use renamite_animation::{Animated, AnimatedTransform, Frame, FrameRate};
use renamite_model::{
    AnimatedDash, Asset, BlendMode, Color, CompId, Composition, Document, FillRule, Gradient,
    GradientKind, ImageAsset, ImageNode, LayerProps, MaskProps, ModifierKind, Node, NodeId,
    NodeKind, Parent, ShapeKind, StarKind, StrokeCap, StrokeJoin, StyleKind, StylePaint, TimeMap,
    TrimMode,
};
use serde_json::Value;

use crate::property::{
    import_angle, import_color, import_gradient, import_path, import_scalar, import_vec2,
};
use crate::{LottieError, LottieReport, LottieWarning};

pub fn import_with_report(root: &Value) -> Result<LottieReport<Document>, LottieError> {
    let width = required_u32(root, "w")?;
    let height = required_u32(root, "h")?;
    let frame_rate = root
        .get("fr")
        .and_then(Value::as_f64)
        .ok_or(LottieError::Missing("fr"))?;
    let in_frame = root.get("ip").and_then(Value::as_f64).unwrap_or(0.0);
    let out_frame = root
        .get("op")
        .and_then(Value::as_f64)
        .ok_or(LottieError::Missing("op"))?;
    let mut document = Document::empty();
    let main = document.main;
    document.compositions[main] = Composition {
        name: root
            .get("nm")
            .and_then(Value::as_str)
            .unwrap_or("Imported")
            .to_owned(),
        size: (width, height),
        rate: rational_frame_rate(frame_rate),
        range: (
            Frame(in_frame.round() as i64),
            Frame(out_frame.round() as i64),
        ),
        children: Vec::new(),
    };
    let assets = root
        .get("assets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|asset| Some((asset.get("id")?.as_str()?.to_owned(), asset.clone())))
        .collect::<HashMap<_, _>>();
    let mut importer = Importer {
        document,
        assets,
        imported_assets: HashMap::new(),
        imported_images: HashMap::new(),
        building_assets: HashSet::new(),
        warnings: Vec::new(),
    };
    let layers = root
        .get("layers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    importer.import_layers(main, &layers, "layers")?;
    Ok(LottieReport {
        value: importer.document,
        warnings: importer.warnings,
    })
}

#[derive(Clone)]
struct ImportTree {
    node: Node,
    children: Vec<ImportTree>,
}

impl ImportTree {
    fn leaf(node: Node) -> Self {
        Self {
            node,
            children: Vec::new(),
        }
    }
}

struct Importer {
    document: Document,
    assets: HashMap<String, Value>,
    imported_assets: HashMap<String, CompId>,
    imported_images: HashMap<String, renamite_model::AssetId>,
    building_assets: HashSet<String>,
    warnings: Vec<LottieWarning>,
}

impl Importer {
    fn import_layers(
        &mut self,
        comp: CompId,
        layers: &[Value],
        path: &str,
    ) -> Result<(), LottieError> {
        for (index, layer) in layers.iter().enumerate() {
            let layer_path = format!("{path}/{index}");
            let layer_type = layer.get("ty").and_then(Value::as_u64).unwrap_or(u64::MAX);
            let tree = match layer_type {
                4 => self.import_shape_layer(layer, &layer_path),
                0 => Some(self.import_precomp_layer(layer, &layer_path)?),
                2 => Some(self.import_image_layer(layer, &layer_path)?),
                unsupported => {
                    self.warnings.push(LottieWarning::new(
                        layer_path,
                        format!("unsupported Lottie layer type `{unsupported}` was skipped"),
                    ));
                    None
                }
            };
            if let Some(tree) = tree {
                self.attach_tree(tree, Parent::Comp(comp));
            }
        }
        Ok(())
    }

    fn import_shape_layer(&mut self, layer: &Value, path: &str) -> Option<ImportTree> {
        let mut node = Node::new(
            layer
                .get("nm")
                .and_then(Value::as_str)
                .unwrap_or("Shape Layer"),
            NodeKind::Layer(LayerProps {
                in_frame: Frame(
                    layer
                        .get("ip")
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0)
                        .round() as i64,
                ),
                out_frame: Frame(
                    layer
                        .get("op")
                        .and_then(Value::as_f64)
                        .unwrap_or(i64::MAX as f64 / 4.0)
                        .round() as i64,
                ),
                time_stretch: layer.get("sr").and_then(Value::as_f64).unwrap_or(1.0),
                blend: blend_from_lottie(layer.get("bm").and_then(Value::as_u64).unwrap_or(0)),
            }),
        );
        node.visible = !layer.get("hd").and_then(Value::as_bool).unwrap_or(false);
        if let Some(transform) = layer.get("ks") {
            node.transform = import_transform(transform);
            node.opacity =
                import_scalar(transform.get("o").unwrap_or(&Value::Null), 1.0 / 100.0, 1.0);
        }
        let mut children: Vec<ImportTree> = Vec::new();
        // Lottie masks apply to the layer's own shapes, so prepend them as
        // sibling masks in document order (they clip the content after them).
        if let Some(masks) = layer.get("masksProperties").and_then(Value::as_array) {
            for (index, mask) in masks.iter().enumerate() {
                if let Some(tree) = self.import_mask(mask, &format!("{path}/masks/{index}")) {
                    children.push(tree);
                }
            }
        }
        children.extend(
            layer
                .get("shapes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
                .filter_map(|(index, item)| {
                    self.import_shape_item(item, &format!("{path}/shapes/{index}"))
                }),
        );
        Some(ImportTree { node, children })
    }

    fn import_image_layer(&mut self, layer: &Value, path: &str) -> Result<ImportTree, LottieError> {
        let reference = layer
            .get("refId")
            .and_then(Value::as_str)
            .ok_or(LottieError::Missing("refId"))?;

        let asset = self.ensure_image_asset(reference)?;

        let mut node = Node::new(
            layer.get("nm").and_then(Value::as_str).unwrap_or("Image"),
            NodeKind::Image(ImageNode::new(asset)),
        );

        node.visible = !layer.get("hd").and_then(Value::as_bool).unwrap_or(false);

        if let Some(transform) = layer.get("ks") {
            node.transform = import_transform(transform);
            node.opacity = import_scalar(transform.get("o").unwrap_or(&Value::Null), 0.01, 1.0);
        }

        let mut children = Vec::new();
        if let Some(masks) = layer.get("masksProperties").and_then(Value::as_array) {
            for (index, mask) in masks.iter().enumerate() {
                if let Some(tree) = self.import_mask(mask, &format!("{path}/masks/{index}")) {
                    children.push(tree);
                }
            }
        }
        if children.is_empty() {
            Ok(ImportTree::leaf(node))
        } else {
            Ok(ImportTree { node, children })
        }
    }

    fn import_mask(&mut self, mask: &Value, path: &str) -> Option<ImportTree> {
        let mode = mask.get("mode").and_then(Value::as_str);
        if let Some(mode) = mode
            && mode != "a"
            && mode != "s"
        {
            self.warnings.push(LottieWarning::new(
                path.to_owned(),
                format!("mask mode `{mode}` not supported; importing as add/invert best-effort"),
            ));
        }

        let pt = mask.get("pt").unwrap_or(&Value::Null);
        let shape = ShapeKind::Path(import_path(pt));

        let inverted =
            mask.get("inv").and_then(Value::as_bool).unwrap_or(false) || mode == Some("s");

        let tree = ImportTree::leaf(Node::new(
            mask.get("nm").and_then(Value::as_str).unwrap_or("Mask"),
            NodeKind::Mask(MaskProps { inverted, shape }),
        ));
        Some(tree)
    }

    /// Find or create the model `ImageAsset` for a Lottie `assets` entry,
    /// decoding its data-URI payload. External (non-data-URI) images are
    /// reported as warnings and skipped.
    fn ensure_image_asset(
        &mut self,
        asset_id: &str,
    ) -> Result<renamite_model::AssetId, LottieError> {
        if let Some(id) = self.imported_images.get(asset_id) {
            return Ok(*id);
        }

        let asset = self
            .assets
            .get(asset_id)
            .ok_or_else(|| LottieError::MissingAsset(asset_id.into()))?;

        let path = asset
            .get("p")
            .and_then(Value::as_str)
            .ok_or_else(|| LottieError::MissingAsset(asset_id.into()))?;

        let Some((mime, data)) = decode_data_uri(path) else {
            self.warnings.push(LottieWarning::new(
                format!("assets/{asset_id}"),
                "external Lottie image cannot be imported without a base directory",
            ));

            return Err(LottieError::MissingAsset(asset_id.into()));
        };

        let width = asset.get("w").and_then(Value::as_u64).unwrap_or(1) as u32;
        let height = asset.get("h").and_then(Value::as_u64).unwrap_or(1) as u32;

        let id = self.document.assets.insert(Asset::Image(ImageAsset {
            name: asset_id.into(),
            mime,
            bytes: data,
            width,
            height,
            srgb: true,
        }));

        self.document.asset_order.push(id);
        self.imported_images.insert(asset_id.into(), id);

        Ok(id)
    }

    fn import_precomp_layer(
        &mut self,
        layer: &Value,
        path: &str,
    ) -> Result<ImportTree, LottieError> {
        let asset_id = layer
            .get("refId")
            .and_then(Value::as_str)
            .ok_or(LottieError::Missing("refId"))?;
        let comp = self.ensure_asset_composition(asset_id)?;
        let mut node = Node::new(
            layer
                .get("nm")
                .and_then(Value::as_str)
                .unwrap_or("Precomposition"),
            NodeKind::Precomp {
                comp,
                time_map: TimeMap {
                    offset: Frame(
                        layer
                            .get("st")
                            .and_then(Value::as_f64)
                            .unwrap_or(0.0)
                            .round() as i64,
                    ),
                    stretch: layer.get("sr").and_then(Value::as_f64).unwrap_or(1.0),
                },
            },
        );
        node.visible = !layer.get("hd").and_then(Value::as_bool).unwrap_or(false);
        if let Some(transform) = layer.get("ks") {
            node.transform = import_transform(transform);
            node.opacity =
                import_scalar(transform.get("o").unwrap_or(&Value::Null), 1.0 / 100.0, 1.0);
        }
        let _ = path;
        Ok(ImportTree::leaf(node))
    }

    fn ensure_asset_composition(&mut self, asset_id: &str) -> Result<CompId, LottieError> {
        if let Some(comp) = self.imported_assets.get(asset_id) {
            return Ok(*comp);
        }
        if !self.building_assets.insert(asset_id.to_owned()) {
            return Err(LottieError::InvalidPrecomposition(asset_id.to_owned()));
        }
        let asset = self
            .assets
            .get(asset_id)
            .cloned()
            .ok_or_else(|| LottieError::MissingAsset(asset_id.to_owned()))?;
        let layers = asset
            .get("layers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let default_range = self.document.compositions[self.document.main].range;
        let min_frame = layers
            .iter()
            .filter_map(|layer| layer.get("ip").and_then(Value::as_f64))
            .min_by(f64::total_cmp)
            .unwrap_or(default_range.0.0 as f64);
        let max_frame = layers
            .iter()
            .filter_map(|layer| layer.get("op").and_then(Value::as_f64))
            .max_by(f64::total_cmp)
            .unwrap_or(default_range.1.0 as f64);
        let comp = self.document.compositions.insert(Composition {
            name: asset_id.to_owned(),
            size: (
                asset.get("w").and_then(Value::as_u64).unwrap_or(512) as u32,
                asset.get("h").and_then(Value::as_u64).unwrap_or(512) as u32,
            ),
            rate: asset
                .get("fr")
                .and_then(Value::as_f64)
                .map(rational_frame_rate)
                .unwrap_or(self.document.compositions[self.document.main].rate),
            range: (
                Frame(min_frame.round() as i64),
                Frame(max_frame.round() as i64),
            ),
            children: Vec::new(),
        });
        self.imported_assets.insert(asset_id.to_owned(), comp);
        self.import_layers(comp, &layers, &format!("assets/{asset_id}/layers"))?;
        self.building_assets.remove(asset_id);
        Ok(comp)
    }

    fn import_shape_item(&mut self, item: &Value, path: &str) -> Option<ImportTree> {
        if item.get("hd").and_then(Value::as_bool).unwrap_or(false) {
            return None;
        }
        let kind = item.get("ty").and_then(Value::as_str)?;
        match kind {
            "gr" => {
                let mut node = Node::new(
                    item.get("nm").and_then(Value::as_str).unwrap_or("Group"),
                    NodeKind::Group,
                );
                let mut children = Vec::new();
                for (index, child) in item
                    .get("it")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    if child.get("ty").and_then(Value::as_str) == Some("tr") {
                        node.transform = import_transform(child);
                        node.opacity =
                            import_scalar(child.get("o").unwrap_or(&Value::Null), 1.0 / 100.0, 1.0);
                    } else if let Some(tree) =
                        self.import_shape_item(child, &format!("{path}/it/{index}"))
                    {
                        children.push(tree);
                    }
                }
                Some(ImportTree { node, children })
            }
            "rc" => Some(ImportTree::leaf(Node::new(
                item_name(item, "Rectangle"),
                NodeKind::Shape(ShapeKind::Rect {
                    pos: import_vec2(item.get("p").unwrap_or(&Value::Null), DVec2::ZERO),
                    size: import_vec2(item.get("s").unwrap_or(&Value::Null), DVec2::ZERO),
                    rounded: import_scalar(item.get("r").unwrap_or(&Value::Null), 1.0, 0.0),
                }),
            ))),
            "el" => Some(ImportTree::leaf(Node::new(
                item_name(item, "Ellipse"),
                NodeKind::Shape(ShapeKind::Ellipse {
                    pos: import_vec2(item.get("p").unwrap_or(&Value::Null), DVec2::ZERO),
                    size: import_vec2(item.get("s").unwrap_or(&Value::Null), DVec2::ZERO),
                }),
            ))),
            "sh" => Some(ImportTree::leaf(Node::new(
                item_name(item, "Path"),
                NodeKind::Shape(ShapeKind::Path(import_path(
                    item.get("ks").unwrap_or(&Value::Null),
                ))),
            ))),
            "sr" => {
                let star_type = item.get("sy").and_then(Value::as_u64).unwrap_or(1);
                let rotation = import_angle(item.get("r").unwrap_or(&Value::Null), 0.0);
                let mut node = if star_type == 2 {
                    Node::new(
                        item_name(item, "Polygon"),
                        NodeKind::Shape(ShapeKind::Polygon {
                            pos: import_vec2(item.get("p").unwrap_or(&Value::Null), DVec2::ZERO),
                            points: import_scalar(item.get("pt").unwrap_or(&Value::Null), 1.0, 5.0),
                            outer_r: import_scalar(
                                item.get("or").unwrap_or(&Value::Null),
                                1.0,
                                100.0,
                            ),
                            roundness: import_scalar(
                                item.get("os").unwrap_or(&Value::Null),
                                1.0,
                                0.0,
                            ),
                        }),
                    )
                } else {
                    Node::new(
                        item_name(item, "Star"),
                        NodeKind::Shape(ShapeKind::Star {
                            pos: import_vec2(item.get("p").unwrap_or(&Value::Null), DVec2::ZERO),
                            points: import_scalar(item.get("pt").unwrap_or(&Value::Null), 1.0, 5.0),
                            inner_r: import_scalar(
                                item.get("ir").unwrap_or(&Value::Null),
                                1.0,
                                50.0,
                            ),
                            outer_r: import_scalar(
                                item.get("or").unwrap_or(&Value::Null),
                                1.0,
                                100.0,
                            ),
                            roundness: import_scalar(
                                item.get("os").unwrap_or(&Value::Null),
                                1.0,
                                0.0,
                            ),
                            kind: if item.get("renamiteStarKind").and_then(Value::as_str)
                                == Some("burst")
                            {
                                StarKind::Burst
                            } else {
                                StarKind::Star
                            },
                        }),
                    )
                };
                node.transform.rotation = rotation;
                Some(ImportTree::leaf(node))
            }
            "fl" => {
                let mut node = Node::new(
                    item_name(item, "Fill"),
                    NodeKind::Style(StyleKind::Fill {
                        paint: StylePaint::Solid {
                            color: import_color(
                                item.get("c").unwrap_or(&Value::Null),
                                Color::BLACK,
                            ),
                        },
                        rule: fill_rule_from_lottie(
                            item.get("r").and_then(Value::as_u64).unwrap_or(1),
                        ),
                    }),
                );
                node.opacity =
                    import_scalar(item.get("o").unwrap_or(&Value::Null), 1.0 / 100.0, 1.0);
                Some(ImportTree::leaf(node))
            }
            "st" => {
                let mut node = Node::new(
                    item_name(item, "Stroke"),
                    NodeKind::Style(StyleKind::Stroke {
                        paint: StylePaint::Solid {
                            color: import_color(
                                item.get("c").unwrap_or(&Value::Null),
                                Color::BLACK,
                            ),
                        },
                        width: import_scalar(item.get("w").unwrap_or(&Value::Null), 1.0, 1.0),
                        cap: stroke_cap_from_lottie(
                            item.get("lc").and_then(Value::as_u64).unwrap_or(1),
                        ),
                        join: stroke_join_from_lottie(
                            item.get("lj").and_then(Value::as_u64).unwrap_or(1),
                        ),
                        dash: import_dash(item.get("d")),
                        miter_limit: import_scalar(
                            item.get("ml").unwrap_or(&Value::Null),
                            1.0,
                            4.0,
                        ),
                    }),
                );
                node.opacity =
                    import_scalar(item.get("o").unwrap_or(&Value::Null), 1.0 / 100.0, 1.0);
                Some(ImportTree::leaf(node))
            }
            "gf" | "gs" => {
                let gradient = Gradient {
                    kind: gradient_kind_from_lottie(
                        item.get("t").and_then(Value::as_u64).unwrap_or(1),
                    ),
                    start: import_vec2(item.get("s").unwrap_or(&Value::Null), DVec2::ZERO),
                    end: import_vec2(
                        item.get("e").unwrap_or(&Value::Null),
                        DVec2::new(100.0, 0.0),
                    ),
                    stops: import_gradient(item.get("g").unwrap_or(&Value::Null)),
                };
                let mut node = if kind == "gf" {
                    Node::new(
                        item_name(item, "Gradient Fill"),
                        NodeKind::Style(StyleKind::Fill {
                            paint: StylePaint::Gradient(gradient),
                            rule: fill_rule_from_lottie(
                                item.get("r").and_then(Value::as_u64).unwrap_or(1),
                            ),
                        }),
                    )
                } else {
                    Node::new(
                        item_name(item, "Gradient Stroke"),
                        NodeKind::Style(StyleKind::Stroke {
                            paint: StylePaint::Gradient(gradient),
                            width: import_scalar(item.get("w").unwrap_or(&Value::Null), 1.0, 1.0),
                            cap: stroke_cap_from_lottie(
                                item.get("lc").and_then(Value::as_u64).unwrap_or(1),
                            ),
                            join: stroke_join_from_lottie(
                                item.get("lj").and_then(Value::as_u64).unwrap_or(1),
                            ),
                            dash: import_dash(item.get("d")),
                            miter_limit: import_scalar(
                                item.get("ml").unwrap_or(&Value::Null),
                                1.0,
                                4.0,
                            ),
                        }),
                    )
                };
                node.opacity =
                    import_scalar(item.get("o").unwrap_or(&Value::Null), 1.0 / 100.0, 1.0);
                Some(ImportTree::leaf(node))
            }
            "tm" => Some(ImportTree::leaf(Node::new(
                item_name(item, "Trim Path"),
                NodeKind::Modifier(ModifierKind::TrimPath {
                    start: import_scalar(item.get("s").unwrap_or(&Value::Null), 1.0 / 100.0, 0.0),
                    end: import_scalar(item.get("e").unwrap_or(&Value::Null), 1.0 / 100.0, 1.0),
                    offset: import_scalar(item.get("o").unwrap_or(&Value::Null), 1.0 / 360.0, 0.0),
                    mode: match item.get("m").and_then(Value::as_u64).unwrap_or(1) {
                        2 => TrimMode::Simultaneously,
                        _ => TrimMode::Individually,
                    },
                }),
            ))),
            "rd" => Some(ImportTree::leaf(Node::new(
                item_name(item, "Round Corners"),
                NodeKind::Modifier(ModifierKind::RoundCorners {
                    radius: import_scalar(item.get("r").unwrap_or(&Value::Null), 1.0, 0.0),
                }),
            ))),
            "op" => Some(ImportTree::leaf(Node::new(
                item_name(item, "Offset Path"),
                NodeKind::Modifier(ModifierKind::OffsetPath {
                    amount: import_scalar(item.get("a").unwrap_or(&Value::Null), 1.0, 0.0),
                }),
            ))),
            "zz" => Some(ImportTree::leaf(Node::new(
                item_name(item, "Zig Zag"),
                NodeKind::Modifier(ModifierKind::ZigZag {
                    amplitude: import_scalar(item.get("a").unwrap_or(&Value::Null), 1.0, 0.0),
                    frequency: import_scalar(item.get("f").unwrap_or(&Value::Null), 1.0, 0.0),
                    smooth: item.get("s").and_then(Value::as_u64).unwrap_or(0) != 0,
                }),
            ))),
            "pb" => Some(ImportTree::leaf(Node::new(
                item_name(item, "Pucker & Bloat"),
                NodeKind::Modifier(ModifierKind::PuckerBloat {
                    amount: import_scalar(item.get("a").unwrap_or(&Value::Null), 1.0, 0.0),
                }),
            ))),
            "rp" => Some(ImportTree::leaf(Node::new(
                item_name(item, "Repeater"),
                NodeKind::Modifier(ModifierKind::Repeater {
                    copies: import_scalar(item.get("c").unwrap_or(&Value::Null), 1.0, 1.0),
                    offset: import_scalar(item.get("o").unwrap_or(&Value::Null), 1.0, 0.0),
                    transform: Box::new(import_repeater_transform(item.get("tr").unwrap_or(&Value::Null))),
                    start_opacity: import_scalar(
                        item.pointer("/tr/so").unwrap_or(&Value::Null),
                        1.0 / 100.0,
                        1.0,
                    ),
                    end_opacity: import_scalar(
                        item.pointer("/tr/eo").unwrap_or(&Value::Null),
                        1.0 / 100.0,
                        1.0,
                    ),
                }),
            ))),
            unsupported => {
                self.warnings.push(LottieWarning::new(
                    path,
                    format!("unsupported Lottie shape item `{unsupported}` was skipped"),
                ));
                None
            }
        }
    }

    fn attach_tree(&mut self, tree: ImportTree, parent: Parent) -> NodeId {
        let id = self.document.create_node(tree.node);
        self.document
            .attach(id, parent, usize::MAX)
            .expect("fresh imported node attaches");
        for child in tree.children {
            self.attach_tree(child, Parent::Node(id));
        }
        id
    }
}

/// Decode a `data:<mime>;base64,<payload>` URI into (mime, bytes).
fn decode_data_uri(value: &str) -> Option<(String, Vec<u8>)> {
    use base64::Engine as _;

    let rest = value.strip_prefix("data:")?;
    let (metadata, encoded) = rest.split_once(',')?;

    if !metadata.ends_with(";base64") {
        return None;
    }

    let mime = metadata.trim_end_matches(";base64").to_owned();

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;

    Some((mime, bytes))
}

fn import_transform(value: &Value) -> AnimatedTransform {
    if value.is_null() {
        return AnimatedTransform::identity();
    }
    AnimatedTransform {
        anchor: import_vec2(value.get("a").unwrap_or(&Value::Null), DVec2::ZERO),
        position: import_vec2(value.get("p").unwrap_or(&Value::Null), DVec2::ZERO),
        scale: import_vec2(value.get("s").unwrap_or(&Value::Null), DVec2::splat(100.0)),
        rotation: import_angle(
            value
                .get("r")
                .or_else(|| value.get("rz"))
                .unwrap_or(&Value::Null),
            0.0,
        ),
        skew: import_scalar(value.get("sk").unwrap_or(&Value::Null), 1.0, 0.0),
        skew_axis: import_scalar(value.get("sa").unwrap_or(&Value::Null), 1.0, 0.0),
    }
}

fn import_repeater_transform(value: &Value) -> AnimatedTransform {
    AnimatedTransform {
        anchor: import_vec2(value.get("a").unwrap_or(&Value::Null), DVec2::ZERO),
        position: import_vec2(value.get("p").unwrap_or(&Value::Null), DVec2::ZERO),
        scale: import_vec2(value.get("s").unwrap_or(&Value::Null), DVec2::splat(100.0)),
        rotation: import_angle(value.get("r").unwrap_or(&Value::Null), 0.0),
        skew: import_scalar(value.get("sk").unwrap_or(&Value::Null), 1.0, 0.0),
        skew_axis: import_scalar(value.get("sa").unwrap_or(&Value::Null), 1.0, 0.0),
    }
}

fn import_dash(value: Option<&Value>) -> Option<AnimatedDash> {
    let entries = value?.as_array()?;
    let mut dashes = Vec::new();
    let mut offset = Animated::new(0.0);
    for entry in entries {
        match entry.get("n").and_then(Value::as_str) {
            Some("d") | Some("g") => {
                dashes.push(import_scalar(
                    entry.get("v").unwrap_or(&Value::Null),
                    1.0,
                    0.0,
                ));
            }
            Some("o") => {
                offset = import_scalar(entry.get("v").unwrap_or(&Value::Null), 1.0, 0.0);
            }
            _ => {}
        }
    }
    if dashes.is_empty() {
        None
    } else {
        Some(AnimatedDash { dashes, offset })
    }
}

fn rational_frame_rate(rate: f64) -> FrameRate {
    const COMMON: &[(f64, u32, u32)] = &[
        (23.976, 24_000, 1_001),
        (29.970, 30_000, 1_001),
        (59.940, 60_000, 1_001),
    ];
    for &(candidate, numerator, denominator) in COMMON {
        if (rate - candidate).abs() < 0.002 {
            return FrameRate {
                num: numerator,
                den: denominator,
            };
        }
    }
    if (rate - rate.round()).abs() < 1e-6 {
        return FrameRate {
            num: rate.round().max(1.0) as u32,
            den: 1,
        };
    }
    let denominator = 1_000_u32;
    let numerator = (rate * denominator as f64).round().max(1.0) as u32;
    let divisor = gcd(numerator, denominator);
    FrameRate {
        num: numerator / divisor,
        den: denominator / divisor,
    }
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let remainder = a % b;
        a = b;
        b = remainder;
    }
    a.max(1)
}

fn required_u32(value: &Value, field: &'static str) -> Result<u32, LottieError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .map(|value| value as u32)
        .ok_or(LottieError::Missing(field))
}

fn item_name<'a>(value: &'a Value, fallback: &'a str) -> &'a str {
    value.get("nm").and_then(Value::as_str).unwrap_or(fallback)
}

fn fill_rule_from_lottie(value: u64) -> FillRule {
    match value {
        2 => FillRule::EvenOdd,
        _ => FillRule::NonZero,
    }
}

fn gradient_kind_from_lottie(value: u64) -> GradientKind {
    match value {
        2 => GradientKind::Radial,
        _ => GradientKind::Linear,
    }
}

fn stroke_cap_from_lottie(value: u64) -> StrokeCap {
    match value {
        2 => StrokeCap::Round,
        3 => StrokeCap::Square,
        _ => StrokeCap::Butt,
    }
}

fn stroke_join_from_lottie(value: u64) -> StrokeJoin {
    match value {
        2 => StrokeJoin::Round,
        3 => StrokeJoin::Bevel,
        _ => StrokeJoin::Miter,
    }
}

fn blend_from_lottie(value: u64) -> BlendMode {
    match value {
        1 => BlendMode::Multiply,
        2 => BlendMode::Screen,
        3 => BlendMode::Overlay,
        4 => BlendMode::Darken,
        5 => BlendMode::Lighten,
        6 => BlendMode::ColorDodge,
        7 => BlendMode::ColorBurn,
        8 => BlendMode::HardLight,
        9 => BlendMode::SoftLight,
        10 => BlendMode::Difference,
        11 => BlendMode::Exclusion,
        12 => BlendMode::Hue,
        13 => BlendMode::Saturation,
        14 => BlendMode::Color,
        15 => BlendMode::Luminosity,
        _ => BlendMode::Normal,
    }
}
