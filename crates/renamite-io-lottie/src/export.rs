use std::collections::HashMap;

use renamite_animation::{Animated, AnimatedTransform};
use renamite_model::{
    BlendMode, CompId, Composition, Document, FillRule, GradientKind, LayerProps, MaskProps,
    ModifierKind, Node, NodeId, NodeKind, Overrides, ShapeKind, StarKind, StrokeCap, StrokeJoin,
    StyleKind, StylePaint, TimeMap, TrimMode,
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
        image_asset_ids: HashMap::new(),
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
    image_asset_ids: HashMap<renamite_model::AssetId, String>,
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
                NodeKind::Layer(_)
                | NodeKind::Group
                | NodeKind::Precomp { .. }
                | NodeKind::Use { .. } => {
                    self.flush_bare_run(&composition, &mut output, &mut bare_run)?;
                    match &node.kind {
                        NodeKind::Layer(props) => {
                            let base = output.len() as u32 + 1;
                            output.extend(self.export_layer_node(
                                node_id,
                                &node,
                                props,
                                &composition,
                                base,
                            ));
                        }
                        NodeKind::Group => {
                            let base = output.len() as u32 + 1;
                            output.extend(self.export_group_layer(
                                node_id,
                                &node,
                                &composition,
                                base,
                            ));
                        }
                        NodeKind::Use { target } => {
                            let base = output.len() as u32 + 1;
                            output.extend(self.export_use_layer(
                                node_id,
                                &node,
                                *target,
                                &composition,
                                base,
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
                NodeKind::Image(_) => {
                    self.flush_bare_run(&composition, &mut output, &mut bare_run)?;
                    output.push(self.export_image_layer(
                        node_id,
                        &node,
                        &composition,
                        output.len() as u32 + 1,
                    )?);
                }
                NodeKind::Shape(_)
                | NodeKind::Style(_)
                | NodeKind::Modifier(_)
                | NodeKind::Text(_) => {
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
        let shape_ids: Vec<NodeId> =
            ids.iter()
                .copied()
                .filter(|id| {
                    self.document.nodes.get(*id).is_some_and(|node| {
                        matches!(node.kind, NodeKind::Shape(_) | NodeKind::Text(_))
                    })
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
    ) -> Vec<Value> {
        let (shapes, images, masks) = self.split_layer_children(node);
        if let Some(image_layer) = self.masked_image_layer(node, &images, &shapes, masks.clone()) {
            let mut layer = image_layer;
            layer["ip"] = json!(props.in_frame.0 as f64);
            layer["op"] = json!(props.out_frame.0 as f64);
            layer["st"] = json!(props.in_frame.0 as f64);
            layer["bm"] = json!(blend_to_lottie(props.blend));
            layer["ind"] = json!(index);
            let _ = id;
            return vec![layer];
        }
        let mut out = Vec::new();
        if !images.is_empty() {
            self.warnings.push(LottieWarning::new(
                node.name.clone(),
                format!(
                    "layer contains {} shape item(s) and {} image(s); exporting as separate Lottie layers with duplicated masks",
                    shapes.len(),
                    images.len()
                ),
            ));
            let mut offset = 1u32;
            for image_id in &images {
                if let Some(mut img_layer) = self.standalone_image_layer(
                    *image_id,
                    masks.clone(),
                    props.in_frame.0 as f64,
                    props.out_frame.0 as f64,
                    props.in_frame.0 as f64,
                    blend_to_lottie(props.blend),
                    node.visible,
                ) {
                    img_layer["ind"] = json!(index + offset);
                    offset += 1;
                    out.push(img_layer);
                }
            }
        }
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
        if !masks.is_empty() {
            layer["masksProperties"] = Value::Array(masks);
        }
        layer["ind"] = json!(index);
        layer["hd"] = json!(!node.visible);
        let _ = id;
        let shapes_empty = layer
            .get("shapes")
            .and_then(|s| s.as_array())
            .is_some_and(|s| s.is_empty());
        let no_images = out.is_empty();
        if !shapes_empty || no_images {
            out.insert(0, layer);
        }
        out
    }

    fn export_use_layer(
        &mut self,
        _id: NodeId,
        node: &Node,
        target: NodeId,
        composition: &Composition,
        index: u32,
    ) -> Vec<Value> {
        let Some(source) = self.document.nodes.get(target).cloned() else {
            self.warnings.push(LottieWarning::new(
                format!("node/{_id:?}"),
                "clone source is missing; exporting empty layer",
            ));
            return Vec::new();
        };
        self.warnings.push(LottieWarning::new(
            format!("node/{_id:?}"),
            "clone bakes to a copy on Lottie export",
        ));
        let kids: Vec<NodeId> = match &source.kind {
            NodeKind::Group | NodeKind::Layer(_) => source.children.clone(),
            _ => vec![target],
        };
        let mut shapes = Vec::new();
        for child in kids {
            shapes.extend(self.export_node_item(child, None));
        }
        if shapes.is_empty() {
            return Vec::new();
        }
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
        vec![layer]
    }

    fn export_group_layer(
        &mut self,
        _id: NodeId,
        node: &Node,
        composition: &Composition,
        index: u32,
    ) -> Vec<Value> {
        let (shapes, images, masks) = self.split_layer_children(node);
        if let Some(mut image_layer) =
            self.masked_image_layer(node, &images, &shapes, masks.clone())
        {
            image_layer["ind"] = json!(index);
            image_layer["ip"] = json!(composition.range.0.0 as f64);
            image_layer["op"] = json!(composition.range.1.0 as f64);
            return vec![image_layer];
        }
        let mut out = Vec::new();
        if !images.is_empty() {
            self.warnings.push(LottieWarning::new(
                node.name.clone(),
                format!(
                    "group contains {} shape item(s) and {} image(s); exporting as separate Lottie layers with duplicated masks",
                    shapes.len(),
                    images.len()
                ),
            ));
            let mut offset = 1u32;
            for image_id in &images {
                if let Some(mut img_layer) = self.standalone_image_layer(
                    *image_id,
                    masks.clone(),
                    composition.range.0.0 as f64,
                    composition.range.1.0 as f64,
                    0.0,
                    0,
                    node.visible,
                ) {
                    img_layer["ind"] = json!(index + offset);
                    offset += 1;
                    out.push(img_layer);
                }
            }
        }
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
        if !masks.is_empty() {
            layer["masksProperties"] = Value::Array(masks);
        }
        layer["ind"] = json!(index);
        layer["hd"] = json!(!node.visible);
        let shapes_empty = layer
            .get("shapes")
            .and_then(|s| s.as_array())
            .is_some_and(|s| s.is_empty());
        let no_images = out.is_empty();
        if !shapes_empty || no_images {
            out.insert(0, layer);
        }
        out
    }

    fn split_layer_children(&mut self, node: &Node) -> (Vec<Value>, Vec<NodeId>, Vec<Value>) {
        let mut shapes = Vec::new();
        let mut images = Vec::new();
        let mut masks = Vec::new();
        for &child in &node.children {
            if let Some(child_node) = self.document.nodes.get(child) {
                match &child_node.kind {
                    NodeKind::Mask(mask) => {
                        masks.push(self.export_mask(child, child_node, mask));
                    }
                    NodeKind::Image(_) => {
                        images.push(child);
                    }
                    _ => {
                        shapes.extend(self.export_node_item(child, None));
                    }
                }
            }
        }
        (shapes, images, masks)
    }

    /// Single image + masks inside a Group/Layer exports as an image layer
    /// (`ty: 2`) with `masksProperties`, so e.g. Photo Card keeps both.
    /// Returns `None` when this fast path does not apply.
    fn masked_image_layer(
        &mut self,
        host: &Node,
        images: &[NodeId],
        shapes: &[Value],
        masks: Vec<Value>,
    ) -> Option<Value> {
        if images.len() != 1 || !shapes.is_empty() {
            return None;
        }
        let image_id = images[0];
        let image_node = self.document.nodes.get(image_id)?.clone();
        let NodeKind::Image(ref img) = image_node.kind else {
            return None;
        };
        let reference = match self.ensure_image_asset(img.asset()) {
            Ok(r) => r,
            Err(e) => {
                self.warnings.push(LottieWarning::new(
                    format!("node/{image_id:?}"),
                    format!("image asset could not be exported: {e}"),
                ));
                return None;
            }
        };
        if !transform_is_identity(&host.transform, &host.opacity) {
            self.warnings.push(LottieWarning::new(
                host.name.clone(),
                "group/layer transform/opacity is dropped on masked image export; bake it into the image node",
            ));
        }
        self.warn_tint_if_needed(image_id, img);
        let mut layer = json!({
            "ddd": 0,
            "ind": 0,
            "ty": 2,
            "nm": image_node.name,
            "refId": reference,
            "sr": 1,
            "ks": transform_json(&image_node.transform, &image_node.opacity),
            "ao": 0,
            "ip": 0,
            "op": 0,
            "st": 0,
            "bm": 0,
            "hd": !(host.visible && image_node.visible),
            "renamiteNode": format!("{image_id:?}")
        });
        if !masks.is_empty() {
            layer["masksProperties"] = Value::Array(masks);
        }
        Some(layer)
    }

    /// One `ty: 2` layer for a single image child when the masked-image fast
    /// path does not apply (e.g. shapes + images coexist). Shares the host
    /// timing/blend/visibility and duplicates masks so clipping is preserved.
    #[allow(clippy::too_many_arguments)]
    fn standalone_image_layer(
        &mut self,
        image_id: NodeId,
        masks: Vec<Value>,
        ip: f64,
        op: f64,
        st: f64,
        bm: u8,
        host_visible: bool,
    ) -> Option<Value> {
        let image_node = self.document.nodes.get(image_id)?.clone();
        let NodeKind::Image(ref img) = image_node.kind else {
            return None;
        };
        let reference = match self.ensure_image_asset(img.asset()) {
            Ok(r) => r,
            Err(e) => {
                self.warnings.push(LottieWarning::new(
                    format!("node/{image_id:?}"),
                    format!("image asset could not be exported: {e}"),
                ));
                return None;
            }
        };
        let mut layer = json!({
            "ddd": 0,
            "ind": 0,
            "ty": 2,
            "nm": image_node.name,
            "refId": reference,
            "sr": 1,
            "ks": transform_json(&image_node.transform, &image_node.opacity),
            "ao": 0,
            "ip": ip,
            "op": op,
            "st": st,
            "bm": bm,
            "hd": !(host_visible && image_node.visible),
            "renamiteNode": format!("{image_id:?}")
        });
        if !masks.is_empty() {
            layer["masksProperties"] = Value::Array(masks);
        }
        self.warn_tint_if_needed(image_id, img);
        Some(layer)
    }

    /// Lottie `ty: 2` layers carry no per-image tint: warn instead of
    /// silently baking to white (SVG already warns on the same drop).
    fn warn_tint_if_needed(&mut self, image_id: NodeId, img: &renamite_model::ImageNode) {
        if img.tint().base != renamite_model::Color::WHITE || !img.tint().keyframes.is_empty() {
            self.warnings.push(LottieWarning::new(
                format!("node/{image_id:?}"),
                "image tint is not representable in Lottie and was dropped",
            ));
        }
    }

    fn export_mask(&mut self, id: NodeId, node: &Node, mask: &MaskProps) -> Value {
        let path_property = match &mask.shape {
            ShapeKind::Path(path) => export_path(path),
            other => {
                let baked = renamite_model::shape_path(
                    other,
                    NodeId::default(),
                    0.0,
                    &Overrides::default(),
                );
                self.warnings.push(LottieWarning::new(
                    format!("node/{id:?}"),
                    "mask shape baked to static path for Lottie export",
                ));
                export_path(&Animated::new(
                    renamite_geometry::VectorPath::from_bez_path(&baked),
                ))
            }
        };

        json!({
            "nm": node.name,
            "mode": if mask.inverted { "s" } else { "a" },
            "inv": mask.inverted,
            "o": { "a": 0, "k": 100 },
            "x": { "a": 0, "k": 0 },
            "pt": path_property,
        })
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

    fn export_image_layer(
        &mut self,
        id: NodeId,
        node: &Node,
        parent: &Composition,
        index: u32,
    ) -> Result<Value, LottieError> {
        let NodeKind::Image(ref img) = node.kind else {
            return Err(LottieError::MissingAsset(format!("{id:?}")));
        };

        let reference = self.ensure_image_asset(img.asset())?;

        self.warn_tint_if_needed(id, img);

        Ok(json!({
            "ddd": 0,
            "ind": index,
            "ty": 2,
            "nm": node.name,
            "refId": reference,
            "sr": 1,
            "ks": transform_json(&node.transform, &node.opacity),
            "ao": 0,
            "ip": parent.range.0.0,
            "op": parent.range.1.0,
            "st": 0,
            "bm": 0,
            "hd": !node.visible,
            "renamiteNode": format!("{id:?}")
        }))
    }

    /// Find or create the Lottie `assets` entry for an image, embedding the
    /// original encoded bytes as a base64 data URI.
    fn ensure_image_asset(
        &mut self,
        asset_id: renamite_model::AssetId,
    ) -> Result<String, LottieError> {
        use base64::Engine as _;

        if let Some(existing) = self.image_asset_ids.get(&asset_id) {
            return Ok(existing.clone());
        }

        let image = self
            .document
            .image_asset(asset_id)
            .ok_or_else(|| LottieError::MissingAsset(format!("{asset_id:?}")))?;

        let id = format!("image_{}", self.next_asset);
        self.next_asset += 1;

        let encoded = base64::engine::general_purpose::STANDARD.encode(&image.bytes);

        self.assets.push(json!({
            "id": id,
            "w": image.width,
            "h": image.height,
            "e": 1,
            "p": format!("data:{};base64,{}", image.mime, encoded),
        }));

        self.image_asset_ids.insert(asset_id, id.clone());
        Ok(id)
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
                let has_effect = node.children.iter().any(|child| {
                    self.document.nodes.get(*child).is_some_and(|n| {
                        matches!(n.kind, NodeKind::Modifier(_) | NodeKind::Style(_))
                    })
                });
                if has_effect {
                    for child in &node.children {
                        if let Some(child_node) = self.document.nodes.get(*child) {
                            let is_shape_text =
                                matches!(child_node.kind, NodeKind::Shape(_) | NodeKind::Text(_));
                            if is_shape_text
                                && !transform_is_identity(
                                    &child_node.transform,
                                    &child_node.opacity,
                                )
                            {
                                self.warnings.push(LottieWarning::new(
                                    format!("node/{child:?}"),
                                    "transformed shape/text wraps in its own group and may escape sibling modifiers/styles on Lottie export; bake the transform or hoist the effect",
                                ));
                            }
                        }
                    }
                }
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
                let items = match shape {
                    ShapeKind::CompoundPath(compound) => {
                        self.compound_shape_items(&node.name, compound)
                    }
                    _ => vec![shape_json(&node.name, shape)],
                };
                if transform_owner == Some(id)
                    || transform_is_identity(&node.transform, &node.opacity)
                {
                    items
                } else {
                    let mut it = items;
                    it.push(group_transform_json(&node.transform, &node.opacity));
                    vec![json!({
                        "ty": "gr",
                        "nm": node.name,
                        "it": it
                    })]
                }
            }
            NodeKind::Style(style) => {
                if style_paint_is_empty(style) {
                    self.warnings.push(LottieWarning::new(
                        node.name.clone(),
                        "gradient has no stops; Lottie export uses a deterministic zero-stop fallback",
                    ));
                }
                vec![style_json(&node.name, style, &node.opacity)]
            }
            NodeKind::Modifier(modifier) => {
                modifier_json(&node.name, modifier).into_iter().collect()
            }
            NodeKind::Text(text) => {
                self.warnings.push(LottieWarning::new(
                    format!("node/{id:?}"),
                    "text baked to path outlines (Lottie text layers not yet emitted)",
                ));
                if !text.size.keyframes.is_empty() {
                    self.warnings.push(LottieWarning::new(
                        format!("node/{id:?}"),
                        "animated `text.size` bakes to its base value on export",
                    ));
                }
                if !text.tracking.keyframes.is_empty() || !text.leading.keyframes.is_empty() {
                    self.warnings.push(LottieWarning::new(
                        format!("node/{id:?}"),
                        "animated `text.tracking`/`text.leading` bake to base on export",
                    ));
                }
                let tracking = text.tracking.base;
                let leading = text.leading.base;
                let outline = if let Some((_, font)) = text
                    .font
                    .as_deref()
                    .and_then(|f| self.document.font_asset_for_family(f))
                {
                    renamite_text::shape_text_from_bytes(
                        &font.bytes,
                        &text.text,
                        text.size.base.max(0.1),
                        text.align,
                        tracking,
                        leading,
                    )
                    .unwrap_or_else(|_| {
                        renamite_text::shape_text_default(
                            &text.text,
                            text.size.base.max(0.1),
                            text.align,
                            tracking,
                            leading,
                        )
                    })
                } else {
                    renamite_text::shape_text_default(
                        &text.text,
                        text.size.base.max(0.1),
                        text.align,
                        tracking,
                        leading,
                    )
                };
                let items = bezpath_to_lottie_shapes(&node.name, &outline);
                if transform_owner == Some(id)
                    || transform_is_identity(&node.transform, &node.opacity)
                {
                    items
                } else {
                    let mut it = items;
                    it.push(group_transform_json(&node.transform, &node.opacity));
                    vec![json!({
                        "ty": "gr",
                        "nm": node.name,
                        "it": it
                    })]
                }
            }
            other => {
                let hint = match other {
                    NodeKind::Image(_) => {
                        " (hoist the image to a top-level Layer/Group child to export it)"
                    }
                    NodeKind::Mask(_) => {
                        " (hoist the mask to a top-level Layer/Group child to export it)"
                    }
                    NodeKind::Precomp { .. } => {
                        " (hoist the precomposition to a top-level child to export it)"
                    }
                    _ => "",
                };
                self.warnings.push(LottieWarning::new(
                    format!("node/{id:?}"),
                    format!(
                        "nested node kind `{}` was skipped{hint}",
                        node_kind_name(other)
                    ),
                ));
                Vec::new()
            }
        }
    }

    /// One Lottie `sh` item per compound-path contour. Contours bake to
    /// their base value (frame 0) and surface a lossy-export warning: a
    /// single `sh` track cannot carry differing per-contour topologies, and a
    /// re-import reads the items back as separate `Path` siblings rather than
    /// one `CompoundPath`.
    fn compound_shape_items(
        &mut self,
        name: &str,
        compound: &renamite_model::CompoundPath,
    ) -> Vec<Value> {
        if compound.contours.len() > 1 {
            self.warnings.push(LottieWarning::new(
                name.to_string(),
                "compound path bakes to one `sh` item per contour on Lottie export (re-imports as separate paths)",
            ));
        }
        let mut out = Vec::new();
        for (index, contour) in compound.contours.iter().enumerate() {
            if !contour.keyframes.is_empty() {
                self.warnings.push(LottieWarning::new(
                    format!("compound contour {index}"),
                    "animated contour bakes to its base value on Lottie export",
                ));
            }
            out.extend(bezpath_to_lottie_shapes(name, &contour.base.to_bez_path()));
        }
        out
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

/// Single-item Lottie shape for primitive kinds. Compound paths never reach
/// this function: they are expanded into one `sh` item per contour by
/// [`Exporter::compound_shape_items`] (a Lottie `sh` holds exactly one
/// contour).
fn shape_json(name: &str, shape: &ShapeKind) -> Value {
    match shape {
        ShapeKind::CompoundPath(_) => json!({
            "ty": "sh",
            "nm": name,
            "d": 1,
            "ks": { "a": 0, "k": [] }
        }),
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

fn style_paint_is_empty(style: &StyleKind) -> bool {
    let paint = match style {
        StyleKind::Fill { paint, .. } | StyleKind::Stroke { paint, .. } => paint,
    };
    match paint {
        StylePaint::Gradient(gradient) => {
            gradient.stops.base.0.is_empty()
                && gradient
                    .stops
                    .keyframes
                    .iter()
                    .all(|key| key.value.0.is_empty())
        }
        StylePaint::Solid { .. } => false,
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
            miter_limit,
        } => {
            let mut value = match paint {
                StylePaint::Solid { color } => json!({
                    "ty": "st",
                    "nm": name,
                    "c": export_color(color),
                    "o": export_scalar(opacity, 100.0),
                    "w": export_scalar(width, 1.0),
                    "lc": stroke_cap_to_lottie(*cap),
                    "lj": stroke_join_to_lottie(*join),
                    "ml": export_scalar(miter_limit, 1.0)
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
                        "lj": stroke_join_to_lottie(*join),
                        "ml": export_scalar(miter_limit, 1.0)
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
            start_opacity,
            end_opacity,
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
                "so": export_scalar(start_opacity, 100.0),
                "eo": export_scalar(end_opacity, 100.0),
                "sk": export_scalar(&transform.skew, 1.0),
                "sa": export_scalar(&transform.skew_axis, 1.0)
            }
        })),
        ModifierKind::OffsetPath { amount } => Some(json!({
            "ty": "op",
            "nm": name,
            "a": export_scalar(amount, 1.0),
            "ml": 4
        })),
        ModifierKind::ZigZag {
            amplitude,
            frequency,
            smooth,
        } => Some(json!({
            "ty": "zz",
            "nm": name,
            "a": export_scalar(amplitude, 1.0),
            "f": export_scalar(frequency, 1.0),
            "s": *smooth as u8
        })),
        ModifierKind::PuckerBloat { amount } => Some(json!({
            "ty": "pb",
            "nm": name,
            "a": export_scalar(amount, 1.0)
        })),
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
        BlendMode::Overlay => 3,
        BlendMode::Darken => 4,
        BlendMode::Lighten => 5,
        BlendMode::ColorDodge => 6,
        BlendMode::ColorBurn => 7,
        BlendMode::HardLight => 8,
        BlendMode::SoftLight => 9,
        BlendMode::Difference => 10,
        BlendMode::Exclusion => 11,
        BlendMode::Hue => 12,
        BlendMode::Saturation => 13,
        BlendMode::Color => 14,
        BlendMode::Luminosity => 15,
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
        NodeKind::Use { .. } => "use",
        NodeKind::Mask(_) => "mask",
    }
}

/// Baked text as standard single-contour Lottie `sh` items (one per contour).
///
/// `ShapeKind::Path` exports through [`export_path`], whose `ks.k` is the
/// flat single-contour form (`"c": bool`, `"v": [...]`) that `import_path`
/// parses back. Text used to emit a non-standard nested multi-contour `sh`
/// (`"c": [...]`, `"v": [[...], ...]`), whose re-import kept only the first
/// contour and silently dropped holes. Emitting one flat `sh` per contour
/// keeps text round-tripping through the same code path as plain paths.
fn bezpath_to_lottie_shapes(name: &str, path: &kurbo::BezPath) -> Vec<Value> {
    fn flat_item(
        name: &str,
        closed: bool,
        v: Vec<[f64; 2]>,
        i: Vec<[f64; 2]>,
        o: Vec<[f64; 2]>,
    ) -> Value {
        json!({
            "ty": "sh",
            "nm": name,
            "d": 1,
            "ks": { "a": 0, "k": { "c": closed, "v": v, "i": i, "o": o } }
        })
    }

    let mut items = Vec::new();
    let mut v: Vec<[f64; 2]> = Vec::new();
    let mut i: Vec<[f64; 2]> = Vec::new();
    let mut o: Vec<[f64; 2]> = Vec::new();
    let mut prev = [0.0, 0.0];
    let mut contour_closed = false;
    let mut in_contour = false;
    for el in path.elements() {
        match el {
            kurbo::PathEl::MoveTo(p) => {
                if in_contour {
                    items.push(flat_item(
                        name,
                        contour_closed,
                        std::mem::take(&mut v),
                        std::mem::take(&mut i),
                        std::mem::take(&mut o),
                    ));
                }
                v.push([p.x, p.y]);
                i.push([0.0, 0.0]);
                o.push([0.0, 0.0]);
                prev = [p.x, p.y];
                contour_closed = false;
                in_contour = true;
            }
            kurbo::PathEl::LineTo(p) => {
                if !in_contour {
                    in_contour = true;
                    contour_closed = false;
                }
                v.push([p.x, p.y]);
                i.push([0.0, 0.0]);
                o.push([0.0, 0.0]);
                prev = [p.x, p.y];
            }
            kurbo::PathEl::QuadTo(c, p) => {
                if !in_contour {
                    in_contour = true;
                    contour_closed = false;
                }
                let c1 = [
                    prev[0] + 2.0 / 3.0 * (c.x - prev[0]),
                    prev[1] + 2.0 / 3.0 * (c.y - prev[1]),
                ];
                let c2 = [p.x + 2.0 / 3.0 * (c.x - p.x), p.y + 2.0 / 3.0 * (c.y - p.y)];
                push_curve(&mut v, &mut i, &mut o, &mut prev, c1, c2, [p.x, p.y]);
            }
            kurbo::PathEl::CurveTo(c1, c2, p) => {
                if !in_contour {
                    in_contour = true;
                    contour_closed = false;
                }
                push_curve(
                    &mut v,
                    &mut i,
                    &mut o,
                    &mut prev,
                    [c1.x, c1.y],
                    [c2.x, c2.y],
                    [p.x, p.y],
                );
            }
            kurbo::PathEl::ClosePath => {
                contour_closed = true;
            }
        }
    }
    if in_contour {
        items.push(flat_item(
            name,
            contour_closed,
            std::mem::take(&mut v),
            std::mem::take(&mut i),
            std::mem::take(&mut o),
        ));
    }
    if items.is_empty() {
        items.push(flat_item(name, false, Vec::new(), Vec::new(), Vec::new()));
    }
    items
}

fn push_curve(
    v: &mut Vec<[f64; 2]>,
    i: &mut Vec<[f64; 2]>,
    o: &mut Vec<[f64; 2]>,
    prev: &mut [f64; 2],
    c1: [f64; 2],
    c2: [f64; 2],
    p: [f64; 2],
) {
    if let Some(out) = o.last_mut() {
        *out = [c1[0] - prev[0], c1[1] - prev[1]];
    }
    v.push(p);
    i.push([c2[0] - p[0], c2[1] - p[1]]);
    o.push([0.0, 0.0]);
    *prev = p;
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::BezPath;
    use renamite_model::{ImageNode, Parent};

    fn pts<'a>(v: &'a Value, key: &str) -> &'a Vec<Value> {
        v.get(key).unwrap().as_array().unwrap()
    }

    fn approx_pt(a: &Value, x: f64, y: f64) {
        let a = a.as_array().unwrap();
        assert!(
            (a[0].as_f64().unwrap() - x).abs() < 1e-9 && (a[1].as_f64().unwrap() - y).abs() < 1e-9,
            "expected [{x}, {y}], got {a:?}"
        );
    }

    #[test]
    fn curve_tangents_are_relative_to_their_vertex() {
        let mut path = BezPath::new();
        path.move_to((0.0, 0.0));
        path.curve_to((10.0, 5.0), (20.0, 20.0), (30.0, 0.0));
        path.line_to((30.0, 40.0));
        path.close_path();
        let shapes = bezpath_to_lottie_shapes("TEXT", &path);
        assert_eq!(shapes.len(), 1);
        let v = &shapes[0]["ks"]["k"];

        assert_eq!(v["c"], json!(true));
        assert_eq!(v["v"], json!([[0.0, 0.0], [30.0, 0.0], [30.0, 40.0]]));
        assert_eq!(pts(v, "o")[0], json!([10.0, 5.0]));
        assert_eq!(pts(v, "i")[1], json!([-10.0, 20.0]));
        assert_eq!(pts(v, "i")[0], json!([0.0, 0.0]));
        assert_eq!(pts(v, "o")[1], json!([0.0, 0.0]));
        assert_eq!(pts(v, "i")[2], json!([0.0, 0.0]));
        assert_eq!(pts(v, "o")[2], json!([0.0, 0.0]));
    }

    #[test]
    fn multiple_contours_emit_one_shape_per_contour() {
        let mut path = BezPath::new();
        path.move_to((0.0, 0.0));
        path.line_to((10.0, 0.0));
        path.move_to((20.0, 0.0));
        path.quad_to((25.0, 10.0), (30.0, 0.0));
        let shapes = bezpath_to_lottie_shapes("TEXT", &path);
        assert_eq!(shapes.len(), 2);

        for shape in &shapes {
            assert_eq!(shape["ty"], json!("sh"));
            assert!(shape["ks"]["k"]["c"].is_boolean());
        }
        assert_eq!(shapes[0]["ks"]["k"]["c"], json!(false));
        assert_eq!(shapes[0]["ks"]["k"]["v"], json!([[0.0, 0.0], [10.0, 0.0]]));
        assert_eq!(shapes[1]["ks"]["k"]["c"], json!(false));
        assert_eq!(shapes[1]["ks"]["k"]["v"], json!([[20.0, 0.0], [30.0, 0.0]]));
        // Elevated quadratic: c1 = prev + 2/3*(c - prev) = (20+10/3, 0+20/3);
        // tangents are the offsets, so o = 2/3*(c - prev), i = 2/3*(c - p).
        let second = &shapes[1]["ks"]["k"];
        approx_pt(&pts(second, "o")[0], 2.0 / 3.0 * 5.0, 2.0 / 3.0 * 10.0);
        approx_pt(&pts(second, "i")[1], 2.0 / 3.0 * -5.0, 2.0 / 3.0 * 10.0);
    }

    #[test]
    fn image_layer_round_trips_through_import() {
        let mut doc = Document::empty();
        let comp = doc.main;
        let bytes = vec![0x89, 0x50, 0x4e, 0x47, 9, 9, 9, 9];
        let asset = doc
            .assets
            .insert(renamite_model::Asset::Image(renamite_model::ImageAsset {
                name: "px.png".into(),
                mime: "image/png".into(),
                bytes: bytes.clone(),
                width: 7,
                height: 5,
                srgb: true,
            }));
        doc.asset_order.push(asset);
        let image = doc.create_node(Node::new("Pic", NodeKind::Image(ImageNode::new(asset))));
        doc.attach(image, Parent::Comp(comp), 0).unwrap();

        let json = crate::export(&doc).unwrap();
        let imported = crate::import(&json).unwrap();

        let imported_image = imported
            .asset_order
            .iter()
            .find_map(|id| imported.image_asset(*id));
        assert!(imported_image.is_some());
        let imported_image = imported_image.unwrap();
        assert_eq!(imported_image.width, 7);
        assert_eq!(imported_image.height, 5);
        assert_eq!(imported_image.mime, "image/png");
        assert_eq!(imported_image.bytes, bytes);

        assert!(
            imported
                .nodes
                .values()
                .any(|node| { matches!(node.kind, NodeKind::Image(_)) })
        );
    }
}
