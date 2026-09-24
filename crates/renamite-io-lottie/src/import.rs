use std::collections::{HashMap, HashSet};

use image::GenericImageView;
use image::ImageReader;
use std::io::Cursor;

use glam::DVec2;
use renamite_animation::{Animated, AnimatedTransform, Frame, FrameRate};
use renamite_model::{
    AnimatedDash, Asset, BlendMode, Color, CompId, Composition, CompoundPath, Document, FillRule,
    Gradient, GradientKind, ImageAsset, ImageNode, LayerProps, MaskProps, ModifierKind, Node,
    NodeId, NodeKind, Parent, ShapeKind, StarKind, StrokeCap, StrokeJoin, StyleKind, StylePaint,
    TimeMap, TrimMode,
};
use serde_json::Value;

use crate::property::{
    import_angle, import_color, import_gradient, import_path, import_scalar, import_vec2,
    path_contour_count, path_contours,
};
use crate::{LottieError, LottieReport, LottieWarning};

pub const MAX_LOTTIE_BYTES: usize = 128 * 1024 * 1024;
const MAX_LOTTIE_DEPTH: usize = 128;
const MAX_LOTTIE_VALUES: usize = 1_000_000;
const MAX_LOTTIE_KEYFRAMES: usize = 100_000;
const MAX_LOTTIE_PATH_ANCHORS: usize = 100_000;
const MAX_LOTTIE_PATH_CONTOURS: usize = 4096;
const MAX_LOTTIE_ARRAY_ITEMS: usize = 200_000;
const MAX_LOTTIE_TOTAL_ARRAY_ITEMS: usize = 2_000_000;
const MAX_LOTTIE_ASSETS: usize = 10_000;
const MAX_LOTTIE_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_LOTTIE_TOTAL_IMAGE_BYTES: usize = 128 * 1024 * 1024;
const MAX_LOTTIE_IMAGE_DIMENSION: u32 = 16_384;
const MAX_LOTTIE_IMAGE_ALLOC: u64 = 128 * 1024 * 1024;
const MAX_LOTTIE_FALLBACK_IMAGE_BYTES: usize = 1024 * 1024;
const MAX_LOTTIE_STRING_BYTES: usize = MAX_LOTTIE_IMAGE_BYTES * 2;

pub fn import_with_report(root: &Value) -> Result<LottieReport<Document>, LottieError> {
    preflight_lottie(root)?;
    let width = required_u32(root, "w")?;
    let height = required_u32(root, "h")?;
    let frame_rate = root
        .get("fr")
        .and_then(Value::as_f64)
        .ok_or(LottieError::Missing("fr"))?;
    let frame_rate = rational_frame_rate(frame_rate)?;
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
        rate: frame_rate,
        range: (frame_number(in_frame), frame_number(out_frame)),
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
        image_bytes: 0,
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

pub(crate) fn preflight_bytes(bytes: &[u8]) -> Result<(), LottieError> {
    if bytes.len() > MAX_LOTTIE_BYTES {
        return Err(LottieError::InputLimit("JSON input is too large"));
    }
    let mut index = 0usize;
    let mut depth = 0usize;
    let mut items = 0usize;
    let mut strings = 0usize;
    let mut image_bytes = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'{' | b'[' => {
                depth += 1;
                items = items.saturating_add(1);
                if depth > MAX_LOTTIE_DEPTH {
                    return Err(LottieError::InputLimit("JSON nesting is too deep"));
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            b',' => items = items.saturating_add(1),
            b'"' => {
                strings += 1;
                let start = index + 1;
                let mut end = start;
                let mut escaped = false;
                while end < bytes.len() {
                    let byte = bytes[end];
                    if escaped {
                        escaped = false;
                    } else if byte == b'\\' {
                        escaped = true;
                    } else if byte == b'"' {
                        break;
                    }
                    end += 1;
                }
                if end >= bytes.len() {
                    return Err(LottieError::Json(serde_json::Error::io(
                        std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "unterminated JSON string",
                        ),
                    )));
                }
                let length = end - start;
                if length > MAX_LOTTIE_STRING_BYTES {
                    return Err(LottieError::InputLimit("JSON string is too large"));
                }
                if bytes[start..end].starts_with(b"data:") {
                    let payload = bytes[start..end]
                        .iter()
                        .position(|&byte| byte == b',')
                        .map(|comma| end - start - comma - 1)
                        .unwrap_or(0);
                    let decoded = payload.saturating_mul(3) / 4;
                    if decoded > MAX_LOTTIE_IMAGE_BYTES {
                        return Err(LottieError::InputLimit("embedded image is too large"));
                    }
                    image_bytes = image_bytes.saturating_add(decoded);
                    if image_bytes > MAX_LOTTIE_TOTAL_IMAGE_BYTES {
                        return Err(LottieError::InputLimit("embedded images are too large"));
                    }
                }
                index = end + 1;
                continue;
            }
            _ => {}
        }
        if items > MAX_LOTTIE_TOTAL_ARRAY_ITEMS || strings > MAX_LOTTIE_VALUES {
            return Err(LottieError::InputLimit("JSON contains too many values"));
        }
        index += 1;
    }
    Ok(())
}

fn path_anchor_count(root: &Value) -> usize {
    let mut pending = vec![root];
    let mut count = 0usize;
    while let Some(value) = pending.pop() {
        match value {
            Value::Object(object) => {
                if let Some(vertices) = object.get("v").and_then(Value::as_array) {
                    count = count.saturating_add(vertex_count(vertices));
                    continue;
                }
                if object.get("t").is_some() {
                    if let Some(start) = object.get("s")
                        && has_path_vertices(start)
                    {
                        pending.push(start);
                    } else if let Some(end) = object.get("e") {
                        pending.push(end);
                    }
                    continue;
                }
                pending.extend(object.values());
            }
            Value::Array(items) => pending.extend(items),
            _ => {}
        }
    }
    count
}

fn has_path_vertices(value: &Value) -> bool {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Object(object) => {
                if object
                    .get("v")
                    .and_then(Value::as_array)
                    .is_some_and(|vertices| !vertices.is_empty())
                {
                    return true;
                }
                pending.extend(object.values());
            }
            Value::Array(items) => pending.extend(items),
            _ => {}
        }
    }
    false
}

fn vertex_count(vertices: &[Value]) -> usize {
    if vertices
        .first()
        .and_then(Value::as_array)
        .is_some_and(|first| first.first().and_then(Value::as_array).is_some())
    {
        vertices
            .iter()
            .filter_map(Value::as_array)
            .map(Vec::len)
            .sum()
    } else {
        vertices.len()
    }
}

fn preflight_lottie(root: &Value) -> Result<(), LottieError> {
    if root
        .get("assets")
        .and_then(Value::as_array)
        .is_some_and(|assets| assets.len() > MAX_LOTTIE_ASSETS)
    {
        return Err(LottieError::InputLimit("Lottie has too many assets"));
    }
    let mut pending = vec![(root, 0usize)];
    let mut values = 0usize;
    let mut keyframes = 0usize;
    let mut array_items = 0usize;
    let mut image_bytes = 0usize;
    let anchors = path_anchor_count(root);
    if anchors > MAX_LOTTIE_PATH_ANCHORS {
        return Err(LottieError::InputLimit("too many path anchors"));
    }

    while let Some((value, depth)) = pending.pop() {
        values += 1;
        if values > MAX_LOTTIE_VALUES {
            return Err(LottieError::InputLimit("JSON contains too many values"));
        }
        if depth > MAX_LOTTIE_DEPTH {
            return Err(LottieError::InputLimit("JSON nesting is too deep"));
        }
        match value {
            Value::Array(items) => {
                if items.len() > MAX_LOTTIE_ARRAY_ITEMS {
                    return Err(LottieError::InputLimit("JSON array is too large"));
                }
                array_items = array_items.saturating_add(items.len());
                if array_items > MAX_LOTTIE_TOTAL_ARRAY_ITEMS {
                    return Err(LottieError::InputLimit(
                        "JSON contains too many array items",
                    ));
                }
                for item in items.iter().rev() {
                    pending.push((item, depth + 1));
                }
            }
            Value::Object(object) => {
                if object.len() > MAX_LOTTIE_ARRAY_ITEMS {
                    return Err(LottieError::InputLimit("JSON object is too large"));
                }
                let animated = object.get("a").and_then(Value::as_u64) == Some(1)
                    || object
                        .get("k")
                        .and_then(Value::as_array)
                        .and_then(|items| items.first())
                        .is_some_and(|item| item.get("t").is_some());
                if animated && let Some(items) = object.get("k").and_then(Value::as_array) {
                    keyframes = keyframes.saturating_add(items.len());
                    if keyframes > MAX_LOTTIE_KEYFRAMES {
                        return Err(LottieError::InputLimit("too many keyframes"));
                    }
                }
                if let Some(path) = object.get("p").and_then(Value::as_str)
                    && path.starts_with("data:")
                {
                    let length = path.len();
                    if length > MAX_LOTTIE_STRING_BYTES {
                        return Err(LottieError::InputLimit("embedded image is too large"));
                    }
                    let payload = path
                        .split_once(',')
                        .map(|(_, value)| value.len())
                        .unwrap_or(0);
                    let decoded = payload.saturating_mul(3) / 4;
                    if decoded > MAX_LOTTIE_IMAGE_BYTES {
                        return Err(LottieError::InputLimit("embedded image is too large"));
                    }
                    image_bytes = image_bytes.saturating_add(decoded);
                    if image_bytes > MAX_LOTTIE_TOTAL_IMAGE_BYTES {
                        return Err(LottieError::InputLimit("embedded images are too large"));
                    }
                }
                for (key, item) in object {
                    if key.len() > 1024 {
                        return Err(LottieError::InputLimit("JSON key is too long"));
                    }
                    pending.push((item, depth + 1));
                }
            }
            Value::String(string) => {
                if string.len() > MAX_LOTTIE_STRING_BYTES {
                    return Err(LottieError::InputLimit("JSON string is too large"));
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }

    preflight_asset_graph(root)
}

fn preflight_asset_graph(root: &Value) -> Result<(), LottieError> {
    let Some(assets) = root.get("assets").and_then(Value::as_array) else {
        return Ok(());
    };
    let mut definitions = HashMap::<String, &Value>::new();
    for asset in assets {
        if let Some(id) = asset.get("id").and_then(Value::as_str) {
            definitions.insert(id.to_owned(), asset);
        }
    }
    let mut pending = Vec::new();
    if let Some(layers) = root.get("layers").and_then(Value::as_array) {
        pending.extend(layers.iter().map(|layer| (layer, 0usize)));
    }
    let mut visited = HashSet::<String>::new();
    let mut count = 0usize;
    while let Some((layer, depth)) = pending.pop() {
        count += 1;
        if count > MAX_LOTTIE_VALUES {
            return Err(LottieError::InputLimit(
                "too many precomposition references",
            ));
        }
        if depth > MAX_LOTTIE_DEPTH {
            return Err(LottieError::InputLimit(
                "precomposition nesting is too deep",
            ));
        }
        if layer.get("ty").and_then(Value::as_u64) == Some(0)
            && let Some(reference) = layer.get("refId").and_then(Value::as_str)
        {
            if !visited.insert(reference.to_owned()) {
                continue;
            }
            if let Some(asset) = definitions.get(reference)
                && let Some(layers) = asset.get("layers").and_then(Value::as_array)
            {
                for child in layers.iter().rev() {
                    pending.push((child, depth + 1));
                }
            }
        }
    }
    Ok(())
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
    image_bytes: usize,
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
                4 => self.import_shape_layer(layer, &layer_path)?,
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

    fn import_shape_layer(
        &mut self,
        layer: &Value,
        path: &str,
    ) -> Result<Option<ImportTree>, LottieError> {
        let mut node = Node::new(
            layer
                .get("nm")
                .and_then(Value::as_str)
                .unwrap_or("Shape Layer"),
            NodeKind::Layer(LayerProps {
                in_frame: frame_number(layer.get("ip").and_then(Value::as_f64).unwrap_or(0.0)),
                out_frame: frame_number(
                    layer
                        .get("op")
                        .and_then(Value::as_f64)
                        .unwrap_or(i64::MAX as f64 / 4.0),
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
                if let Some(tree) = (self.import_mask(mask, &format!("{path}/masks/{index}")))? {
                    children.push(tree);
                }
            }
        }
        if let Some(shapes) = layer.get("shapes").and_then(Value::as_array) {
            for (index, item) in shapes.iter().enumerate() {
                if let Some(tree) =
                    self.import_shape_item(item, &format!("{path}/shapes/{index}"))?
                {
                    children.push(tree);
                }
            }
        }
        Ok(Some(ImportTree { node, children }))
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

        let mut masks = Vec::new();
        if let Some(masks_json) = layer.get("masksProperties").and_then(Value::as_array) {
            for (index, mask) in masks_json.iter().enumerate() {
                if let Some(tree) = (self.import_mask(mask, &format!("{path}/masks/{index}")))? {
                    masks.push(tree);
                }
            }
        }
        if masks.is_empty() {
            Ok(ImportTree::leaf(node))
        } else {
            let name = node.name.clone();
            masks.push(ImportTree::leaf(node));
            Ok(ImportTree {
                node: Node::new(name, NodeKind::Group),
                children: masks,
            })
        }
    }

    fn import_mask(&mut self, mask: &Value, path: &str) -> Result<Option<ImportTree>, LottieError> {
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
        let contour_count = path_contour_count(pt);
        if contour_count > MAX_LOTTIE_PATH_CONTOURS {
            return Err(LottieError::InputLimit("Lottie mask has too many contours"));
        }
        if contour_count == 0 {
            self.warnings.push(LottieWarning::new(
                path,
                "Lottie mask has no usable vertices and was skipped",
            ));
            return Ok(None);
        }
        let shape = if contour_count > 1 {
            let animated = pt.get("a").and_then(Value::as_u64) == Some(1)
                || pt
                    .get("k")
                    .and_then(Value::as_array)
                    .and_then(|items| items.first())
                    .is_some_and(|item| item.get("t").is_some());
            if animated {
                return Err(LottieError::Invalid("multi-contour animated mask path"));
            }
            let Some(contours) = path_contours(pt) else {
                return Ok(None);
            };
            ShapeKind::CompoundPath(CompoundPath {
                contours: contours
                    .into_iter()
                    .map(renamite_animation::Animated::new)
                    .collect(),
            })
        } else {
            ShapeKind::Path(import_path(pt))
        };

        let inverted =
            mask.get("inv").and_then(Value::as_bool).unwrap_or(false) || mode == Some("s");

        let tree = ImportTree::leaf(Node::new(
            mask.get("nm").and_then(Value::as_str).unwrap_or("Mask"),
            NodeKind::Mask(MaskProps { inverted, shape }),
        ));
        Ok(Some(tree))
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
        if path.len() > MAX_LOTTIE_STRING_BYTES {
            return Err(LottieError::InputLimit("embedded image is too large"));
        }
        if path
            .split_once(',')
            .is_some_and(|(_, encoded)| encoded.len() > MAX_LOTTIE_IMAGE_BYTES.div_ceil(3) * 4)
        {
            return Err(LottieError::InputLimit("embedded image is too large"));
        }

        let Some((declared_mime, data)) = decode_data_uri(path) else {
            self.warnings.push(LottieWarning::new(
                format!("assets/{asset_id}"),
                "external Lottie image cannot be imported without a base directory",
            ));

            return Err(LottieError::MissingAsset(asset_id.into()));
        };

        if data.len() > MAX_LOTTIE_IMAGE_BYTES
            || self.image_bytes.saturating_add(data.len()) > MAX_LOTTIE_TOTAL_IMAGE_BYTES
        {
            return Err(LottieError::InputLimit("embedded images are too large"));
        }
        let declared_dimensions = match (
            asset.get("w").and_then(Value::as_u64),
            asset.get("h").and_then(Value::as_u64),
        ) {
            (Some(width), Some(height))
                if width > 0
                    && height > 0
                    && width <= MAX_LOTTIE_IMAGE_DIMENSION as u64
                    && height <= MAX_LOTTIE_IMAGE_DIMENSION as u64 =>
            {
                Some((width as u32, height as u32))
            }
            (Some(width), Some(height))
                if width > MAX_LOTTIE_IMAGE_DIMENSION as u64
                    || height > MAX_LOTTIE_IMAGE_DIMENSION as u64 =>
            {
                return Err(LottieError::InputLimit(
                    "embedded image dimensions are too large",
                ));
            }
            _ => None,
        };
        let decoded = (|| -> Result<Option<(u32, u32, String)>, LottieError> {
            let header_dimensions = match ImageReader::new(Cursor::new(&data)).with_guessed_format()
            {
                Ok(reader) => match reader.into_dimensions() {
                    Ok(dimensions) => dimensions,
                    Err(image::ImageError::Limits(_)) => {
                        return Err(LottieError::InputLimit("embedded image is too large"));
                    }
                    Err(_) => return Ok(None),
                },
                Err(_) => return Ok(None),
            };
            if header_dimensions.0 == 0
                || header_dimensions.1 == 0
                || header_dimensions.0 > MAX_LOTTIE_IMAGE_DIMENSION
                || header_dimensions.1 > MAX_LOTTIE_IMAGE_DIMENSION
            {
                return Err(LottieError::InputLimit(
                    "embedded image dimensions are too large",
                ));
            }
            let mut reader = ImageReader::new(Cursor::new(&data))
                .with_guessed_format()
                .map_err(|_| LottieError::Invalid("embedded image format"))?;
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(MAX_LOTTIE_IMAGE_DIMENSION);
            limits.max_image_height = Some(MAX_LOTTIE_IMAGE_DIMENSION);
            limits.max_alloc = Some(MAX_LOTTIE_IMAGE_ALLOC);
            reader.limits(limits);
            let format = reader.format();
            let decoded = match reader.decode() {
                Ok(decoded) => decoded,
                Err(image::ImageError::Limits(_)) => {
                    return Err(LottieError::InputLimit("embedded image is too large"));
                }
                Err(_) => return Ok(None),
            };
            let dimensions = decoded.dimensions();
            if dimensions.0 == 0
                || dimensions.1 == 0
                || dimensions.0 > MAX_LOTTIE_IMAGE_DIMENSION
                || dimensions.1 > MAX_LOTTIE_IMAGE_DIMENSION
            {
                return Err(LottieError::InputLimit(
                    "embedded image dimensions are too large",
                ));
            }
            let mime = match format {
                Some(image::ImageFormat::Png) => "image/png",
                Some(image::ImageFormat::Jpeg) => "image/jpeg",
                Some(image::ImageFormat::WebP) => "image/webp",
                _ => declared_mime.as_str(),
            }
            .to_owned();
            Ok(Some((dimensions.0, dimensions.1, mime)))
        })();
        let (width, height, mime) = match decoded {
            Ok(Some(decoded)) => decoded,
            Ok(None) if data.len() <= MAX_LOTTIE_FALLBACK_IMAGE_BYTES => {
                let (width, height) =
                    declared_dimensions.ok_or(LottieError::Invalid("embedded image"))?;
                (width, height, declared_mime)
            }
            Ok(None) => return Err(LottieError::Invalid("embedded image")),
            Err(error) => return Err(error),
        };

        self.image_bytes += data.len();
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
                    offset: frame_number(layer.get("st").and_then(Value::as_f64).unwrap_or(0.0)),
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
        if layer.get("tm").is_some() {
            self.warnings.push(LottieWarning::new(
                path.to_owned(),
                "Lottie time remapping is not representable and was skipped",
            ));
        }
        if layer
            .get("masksProperties")
            .and_then(Value::as_array)
            .is_some_and(|masks| !masks.is_empty())
        {
            self.warnings.push(LottieWarning::new(
                path.to_owned(),
                "masks on precomposition layers are not supported and were dropped",
            ));
        }
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
                asset
                    .get("w")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or(512),
                asset
                    .get("h")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or(512),
            ),
            rate: match asset.get("fr").and_then(Value::as_f64) {
                Some(rate) => rational_frame_rate(rate)?,
                None => self.document.compositions[self.document.main].rate,
            },
            range: (frame_number(min_frame), frame_number(max_frame)),
            children: Vec::new(),
        });
        self.imported_assets.insert(asset_id.to_owned(), comp);
        self.import_layers(comp, &layers, &format!("assets/{asset_id}/layers"))?;
        self.building_assets.remove(asset_id);
        Ok(comp)
    }

    fn import_shape_item(
        &mut self,
        item: &Value,
        path: &str,
    ) -> Result<Option<ImportTree>, LottieError> {
        if item.get("hd").and_then(Value::as_bool).unwrap_or(false) {
            return Ok(None);
        }
        let Some(kind) = item.get("ty").and_then(Value::as_str) else {
            return Ok(None);
        };
        Ok(match kind {
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
                        self.import_shape_item(child, &format!("{path}/it/{index}"))?
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
            "sh" => {
                let property = item.get("ks").unwrap_or(&Value::Null);
                let contours = path_contour_count(property);
                if contours > MAX_LOTTIE_PATH_CONTOURS {
                    return Err(LottieError::InputLimit("Lottie path has too many contours"));
                }
                if contours == 0 {
                    self.warnings.push(LottieWarning::new(
                        path,
                        "Lottie path has no usable vertices and was skipped",
                    ));
                    return Ok(None);
                }
                let animated = property.get("a").and_then(Value::as_u64) == Some(1)
                    || property
                        .get("k")
                        .and_then(Value::as_array)
                        .and_then(|items| items.first())
                        .is_some_and(|item| item.get("t").is_some());
                if contours > 1 {
                    if animated {
                        return Err(LottieError::Invalid("multi-contour animated path"));
                    }
                    let Some(contours) = path_contours(property) else {
                        self.warnings.push(LottieWarning::new(
                            path,
                            "Lottie path vertices could not be parsed and were skipped",
                        ));
                        return Ok(None);
                    };
                    return Ok(Some(ImportTree::leaf(Node::new(
                        item_name(item, "Path"),
                        NodeKind::Shape(ShapeKind::CompoundPath(CompoundPath {
                            contours: contours
                                .into_iter()
                                .map(renamite_animation::Animated::new)
                                .collect(),
                        })),
                    ))));
                }
                let parsed_path = import_path(property);
                if parsed_path.base.anchors.is_empty() {
                    self.warnings.push(LottieWarning::new(
                        path,
                        "Lottie path vertices could not be parsed and were skipped",
                    ));
                    return Ok(None);
                }
                Some(ImportTree::leaf(Node::new(
                    item_name(item, "Path"),
                    NodeKind::Shape(ShapeKind::Path(parsed_path)),
                )))
            }
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
                let Some(stops) = import_gradient(item.get("g").unwrap_or(&Value::Null)) else {
                    self.warnings.push(LottieWarning::new(
                        path,
                        "Lottie gradient has no valid stops (empty or malformed) and was skipped",
                    ));
                    return Ok(None);
                };
                let gradient = Gradient {
                    kind: gradient_kind_from_lottie(
                        item.get("t").and_then(Value::as_u64).unwrap_or(1),
                    ),
                    start: import_vec2(item.get("s").unwrap_or(&Value::Null), DVec2::ZERO),
                    end: import_vec2(
                        item.get("e").unwrap_or(&Value::Null),
                        DVec2::new(100.0, 0.0),
                    ),
                    stops,
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
            "tm" => {
                let mode = item.get("m").and_then(Value::as_u64).unwrap_or(1);
                if !matches!(mode, 1 | 2) {
                    self.warnings.push(LottieWarning::new(
                        path,
                        format!("unsupported Lottie trim mode `{mode}`; using individual mode"),
                    ));
                }
                Some(ImportTree::leaf(Node::new(
                    item_name(item, "Trim Path"),
                    NodeKind::Modifier(ModifierKind::TrimPath {
                        start: import_scalar(
                            item.get("s").unwrap_or(&Value::Null),
                            1.0 / 100.0,
                            0.0,
                        ),
                        end: import_scalar(item.get("e").unwrap_or(&Value::Null), 1.0 / 100.0, 1.0),
                        offset: import_scalar(
                            item.get("o").unwrap_or(&Value::Null),
                            1.0 / 360.0,
                            0.0,
                        ),
                        mode: if mode == 2 {
                            TrimMode::Simultaneously
                        } else {
                            TrimMode::Individually
                        },
                    }),
                )))
            }
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
                    transform: Box::new(import_repeater_transform(
                        item.get("tr").unwrap_or(&Value::Null),
                    )),
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
        })
    }

    fn attach_tree(&mut self, tree: ImportTree, parent: Parent) -> NodeId {
        let root = self.document.create_node(tree.node);
        self.document
            .attach(root, parent, usize::MAX)
            .expect("fresh imported node attaches");
        let mut pending = tree
            .children
            .into_iter()
            .rev()
            .map(|child| (child, Parent::Node(root)))
            .collect::<Vec<_>>();
        while let Some((child, parent)) = pending.pop() {
            let id = self.document.create_node(child.node);
            self.document
                .attach(id, parent, usize::MAX)
                .expect("fresh imported node attaches");
            for child in child.children.into_iter().rev() {
                pending.push((child, Parent::Node(id)));
            }
        }
        root
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
    let max_encoded = MAX_LOTTIE_IMAGE_BYTES.saturating_add(2) / 3 * 4;
    if encoded.len() > max_encoded {
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
    if entries.len() > 4096 {
        return None;
    }
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

fn rational_frame_rate(rate: f64) -> Result<FrameRate, LottieError> {
    if !rate.is_finite() || rate <= 0.0 || rate > 1_000_000.0 {
        return Err(LottieError::Invalid("fr"));
    }
    const COMMON: &[(f64, u32, u32)] = &[
        (23.976, 24_000, 1_001),
        (29.970, 30_000, 1_001),
        (59.940, 60_000, 1_001),
    ];
    for &(candidate, numerator, denominator) in COMMON {
        if (rate - candidate).abs() < 0.002 {
            return Ok(FrameRate {
                num: numerator,
                den: denominator,
            });
        }
    }
    if (rate - rate.round()).abs() < 1e-6 {
        return Ok(FrameRate {
            num: rate.round().max(1.0) as u32,
            den: 1,
        });
    }
    let denominator = 1_000_u32;
    let numerator = (rate * denominator as f64).round().max(1.0) as u32;
    if numerator == 0 || numerator > 1_000_000 {
        return Err(LottieError::Invalid("fr"));
    }
    let divisor = gcd(numerator, denominator);
    Ok(FrameRate {
        num: numerator / divisor,
        den: denominator / divisor,
    })
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let remainder = a % b;
        a = b;
        b = remainder;
    }
    a.max(1)
}

fn frame_number(value: f64) -> Frame {
    if !value.is_finite() {
        return Frame(0);
    }
    let rounded = value.round();
    if rounded <= i64::MIN as f64 {
        Frame(i64::MIN)
    } else if rounded >= i64::MAX as f64 {
        Frame(i64::MAX)
    } else {
        Frame(rounded as i64)
    }
}

fn required_u32(value: &Value, field: &'static str) -> Result<u32, LottieError> {
    let value = value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or(LottieError::Missing(field))?;
    u32::try_from(value).map_err(|_| LottieError::Invalid(field))
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
