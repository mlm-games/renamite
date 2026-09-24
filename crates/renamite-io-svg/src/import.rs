//! SVG import: `usvg` tree -> Renamite `Document`.
//!
//! `usvg` preprocesses CSS, resolves references/`<use>`, converts primitives
//! to absolute paths, resolves images, and flattens text. Everything is baked
//! into world space: geometry carries the absolute transform and node
//! transforms are left at identity (except images, whose affine is decomposed
//! back into a node transform).

use std::collections::HashMap;
use std::io::Cursor;

use image::GenericImageView;
use kurbo::Affine;
use renamite_animation::{Animated, Frame};
use renamite_model::{
    Asset, BlendMode, CompoundPath, Document, ImageAsset, ImageNode, LayerProps, MaskProps, Node,
    NodeKind, Parent, ShapeKind,
};
use usvg::ImageKind;
use usvg::Node as SvgNode;

use crate::paint::{import_fill, import_stroke};
use crate::path::{affine_to_animated_transform, tiny_path_to_kurbo, usvg_transform_to_kurbo};
use crate::{MAX_INPUT_BYTES, SvgError, SvgReport, SvgWarning};

const MAX_SVG_BYTES: usize = MAX_INPUT_BYTES;
const MAX_SVG_ELEMENTS: usize = 100_000;
const MAX_SVG_NODES: usize = 200_000;
const MAX_SVG_DEPTH: usize = 256;
const MAX_SVG_USES: usize = 4_096;
const MAX_SVG_REFERENCE_DEPTH: usize = 256;
const MAX_SVG_REFERENCE_EDGES: usize = 200_000;
const MAX_SVG_ATTRIBUTES: usize = 256;
const MAX_SVG_ATTRIBUTE_BYTES: usize = 256 * 1024;
const MAX_SVG_TOTAL_ATTRIBUTE_BYTES: usize = 64 * 1024 * 1024;
const MAX_SVG_TAG_BYTES: usize = MAX_SVG_BYTES;
const MAX_SVG_PATH_BYTES: usize = 4 * 1024 * 1024;
const MAX_SVG_PATH_POINTS: usize = 1_000_000;
const MAX_SVG_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_SVG_TOTAL_IMAGE_BYTES: usize = 128 * 1024 * 1024;
const MAX_SVG_IMAGE_DIMENSION: u32 = 16_384;
const MAX_SVG_IMAGE_ALLOC: u64 = 128 * 1024 * 1024;

pub fn import_with_report(bytes: &[u8]) -> Result<SvgReport<Document>, SvgError> {
    preflight_svg(bytes)?;
    let mut options = usvg::Options::default();
    options.image_href_resolver.resolve_string = Box::new(|_, _| None);
    options.image_href_resolver.resolve_data = Box::new(|mime, data, _| {
        if data.len() as u64 > MAX_SVG_IMAGE_ALLOC {
            return None;
        }
        match mime {
            "image/png" => Some(ImageKind::PNG(data)),
            "image/jpeg" | "image/jpg" => Some(ImageKind::JPEG(data)),
            "image/gif" => Some(ImageKind::GIF(data)),
            "image/webp" => Some(ImageKind::WEBP(data)),
            _ => None,
        }
    });
    options
        .fontdb_mut()
        .load_font_data(renamite_text::default_font_bytes().to_vec());
    // Resolve text with the same deterministic default face the editor uses.
    if let Some(family) = renamite_text::font_family_name(renamite_text::default_font_bytes()) {
        options.font_family = family;
    }
    let tree = usvg::Tree::from_data(bytes, &options)?;
    validate_usvg_tree(&tree)?;

    let mut importer = Importer {
        document: Document::empty(),
        warnings: Vec::new(),
        image_bytes: 0,
    };
    importer.import_tree(&tree)?;

    Ok(SvgReport {
        value: importer.document,
        warnings: importer.warnings,
    })
}

fn preflight_svg(bytes: &[u8]) -> Result<(), SvgError> {
    if bytes.len() > MAX_SVG_BYTES {
        return Err(SvgError::InputLimit("input is too large"));
    }

    let mut cursor = 0usize;
    let mut depth = 0usize;
    let mut elements = 0usize;
    let mut uses = 0usize;
    let mut references = HashMap::<String, String>::new();
    let mut use_keys = Vec::new();
    let mut nested_references = HashMap::<String, Vec<String>>::new();
    let mut nested_reference_edges = 0usize;
    let mut attribute_bytes = 0usize;
    let mut path_bytes = 0usize;
    let mut image_bytes = 0usize;
    let mut open_ids: Vec<Option<String>> = Vec::new();

    while let Some(relative) = bytes[cursor..].iter().position(|&byte| byte == b'<') {
        let start = cursor + relative;
        if bytes[start..].starts_with(b"<!--") {
            let Some(end) = find_bytes(bytes, start + 4, b"-->") else {
                break;
            };
            cursor = end + 3;
            continue;
        }
        if bytes[start..].starts_with(b"<![CDATA[") {
            let Some(end) = find_bytes(bytes, start + 9, b"]]>") else {
                break;
            };
            cursor = end + 3;
            continue;
        }
        if bytes[start..].starts_with(b"<?") {
            let Some(end) = find_bytes(bytes, start + 2, b"?>") else {
                break;
            };
            cursor = end + 2;
            continue;
        }
        if bytes[start..].starts_with(b"<!") {
            let Some(end) = find_declaration_end(bytes, start + 2) else {
                break;
            };
            cursor = end;
            continue;
        }

        let Some(end) = find_tag_end(bytes, start + 1) else {
            break;
        };
        if end.saturating_sub(start) > MAX_SVG_TAG_BYTES {
            return Err(SvgError::InputLimit("SVG tag is too large"));
        }
        let tag = &bytes[start + 1..end];
        cursor = end + 1;
        let trimmed = trim_ascii(tag);
        if trimmed.is_empty() {
            continue;
        }
        let closing = trimmed[0] == b'/';
        let content = if closing { &trimmed[1..] } else { trimmed };
        let name_end = content
            .iter()
            .position(|byte| byte.is_ascii_whitespace() || *byte == b'/')
            .unwrap_or(content.len());
        let name = &content[..name_end];
        if name.is_empty() {
            continue;
        }
        if !closing {
            elements += 1;
            if elements > MAX_SVG_ELEMENTS {
                return Err(SvgError::InputLimit("too many SVG elements"));
            }
            if local_name(name) == b"use" {
                uses += 1;
                if uses > MAX_SVG_USES {
                    return Err(SvgError::InputLimit("too many SVG use elements"));
                }
            }
            let self_closing = trimmed.last() == Some(&b'/');
            if !self_closing {
                depth += 1;
                if depth > MAX_SVG_DEPTH {
                    return Err(SvgError::InputLimit("SVG nesting is too deep"));
                }
            }
            let attrs = parse_tag_attributes(content);
            if attrs.count > MAX_SVG_ATTRIBUTES
                || (attrs.image_bytes.is_none() && attrs.total_bytes > MAX_SVG_ATTRIBUTE_BYTES)
            {
                return Err(SvgError::InputLimit("SVG element has too many attributes"));
            }
            attribute_bytes = attribute_bytes.saturating_add(attrs.total_bytes);
            if attribute_bytes > MAX_SVG_TOTAL_ATTRIBUTE_BYTES {
                return Err(SvgError::InputLimit("SVG attributes are too large"));
            }
            path_bytes = path_bytes.saturating_add(attrs.path_bytes);
            if path_bytes > MAX_SVG_PATH_BYTES {
                return Err(SvgError::InputLimit("SVG path data is too large"));
            }
            if let Some(data_bytes) = attrs.image_bytes {
                if data_bytes > MAX_SVG_IMAGE_BYTES {
                    return Err(SvgError::InputLimit("embedded SVG image is too large"));
                }
                image_bytes = image_bytes.saturating_add(data_bytes);
                if image_bytes > MAX_SVG_TOTAL_IMAGE_BYTES {
                    return Err(SvgError::InputLimit("embedded SVG images are too large"));
                }
            }
            let id = attrs.id;
            let reference = attrs.reference;
            let reference_key = if local_name(name) == b"use" {
                Some(id.clone().unwrap_or_else(|| format!("@use:{}", uses - 1)))
            } else {
                None
            };
            if local_name(name) == b"use"
                && let Some(key) = reference_key.clone()
            {
                use_keys.push(key.clone());
                if key.len() <= 256
                    && let Some(reference) = reference.clone()
                    && reference.len() <= 256
                {
                    references.insert(key, reference);
                }
            }
            if local_name(name) == b"use"
                && let Some(key) = reference_key
            {
                for ancestor in open_ids.iter().flatten() {
                    nested_reference_edges += 1;
                    if nested_reference_edges > MAX_SVG_REFERENCE_EDGES {
                        return Err(SvgError::InputLimit("SVG reference nesting is too large"));
                    }
                    let descendants = nested_references.entry(ancestor.clone()).or_default();
                    descendants.push(key.clone());
                }
            }
            if !self_closing {
                open_ids.push(id);
            }
        } else {
            depth = depth.saturating_sub(1);
            open_ids.pop();
        }
    }

    let starts = references
        .keys()
        .chain(nested_references.keys())
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    for start in starts {
        let mut pending = vec![(start, 0usize)];
        let mut visited = std::collections::HashSet::new();
        while let Some((current, depth)) = pending.pop() {
            if depth > MAX_SVG_REFERENCE_DEPTH {
                return Err(SvgError::InputLimit("SVG reference nesting is too deep"));
            }
            if !visited.insert(current.clone()) {
                continue;
            }
            if let Some(next) = references.get(&current) {
                pending.push((next.clone(), depth + 1));
            }
            if let Some(children) = nested_references.get(&current) {
                pending.extend(children.iter().cloned().map(|child| (child, depth + 1)));
            }
        }
    }

    let mut expanded = elements;
    let mut memo = HashMap::<String, usize>::new();
    let mut visiting = std::collections::HashSet::new();
    for key in use_keys {
        let estimate = estimate_reference(
            &key,
            &references,
            &nested_references,
            &mut memo,
            &mut visiting,
        )?;
        expanded = expanded.saturating_add(estimate.saturating_sub(1));
        if expanded > MAX_SVG_NODES {
            return Err(SvgError::InputLimit("expanded SVG tree is too large"));
        }
    }

    Ok(())
}

fn estimate_reference(
    key: &str,
    references: &HashMap<String, String>,
    nested_references: &HashMap<String, Vec<String>>,
    memo: &mut HashMap<String, usize>,
    visiting: &mut std::collections::HashSet<String>,
) -> Result<usize, SvgError> {
    if let Some(value) = memo.get(key) {
        return Ok(*value);
    }
    if !visiting.insert(key.to_owned()) {
        return Err(SvgError::InputLimit("SVG reference cycle"));
    }
    let value = if let Some(target) = references.get(key) {
        1usize.saturating_add(estimate_reference(
            target,
            references,
            nested_references,
            memo,
            visiting,
        )?)
    } else {
        let mut total = 1usize;
        if let Some(children) = nested_references.get(key) {
            for child in children {
                total = total.saturating_add(estimate_reference(
                    child,
                    references,
                    nested_references,
                    memo,
                    visiting,
                )?);
                if total > MAX_SVG_NODES {
                    visiting.remove(key);
                    return Err(SvgError::InputLimit("expanded SVG tree is too large"));
                }
            }
        }
        total
    };
    visiting.remove(key);
    memo.insert(key.to_owned(), value);
    Ok(value)
}

fn validate_usvg_tree(tree: &usvg::Tree) -> Result<(), SvgError> {
    let mut pending = tree
        .root()
        .children()
        .iter()
        .rev()
        .map(|node| (node, 0usize))
        .collect::<Vec<_>>();
    let mut count = 0usize;
    let mut path_points = 0usize;
    while let Some((node, depth)) = pending.pop() {
        count += 1;
        if count > MAX_SVG_NODES {
            return Err(SvgError::InputLimit("expanded SVG tree is too large"));
        }
        if depth > MAX_SVG_DEPTH {
            return Err(SvgError::InputLimit("expanded SVG nesting is too deep"));
        }
        match node {
            usvg::Node::Group(group) => {
                for child in group.children().iter().rev() {
                    pending.push((child, depth + 1));
                }
                if let Some(clip) = group.clip_path() {
                    for child in clip.root().children().iter().rev() {
                        pending.push((child, depth + 1));
                    }
                }
                if let Some(mask) = group.mask() {
                    for child in mask.root().children().iter().rev() {
                        pending.push((child, depth + 1));
                    }
                }
            }
            usvg::Node::Text(text) => {
                for child in text.flattened().children().iter().rev() {
                    pending.push((child, depth + 1));
                }
            }
            usvg::Node::Path(path) => {
                path_points = path_points.saturating_add(path.data().segments().count());
                if path_points > MAX_SVG_PATH_POINTS {
                    return Err(SvgError::InputLimit("expanded SVG path data is too large"));
                }
            }
            usvg::Node::Image(_) => {}
        }
    }
    Ok(())
}

fn find_bytes(bytes: &[u8], start: usize, needle: &[u8]) -> Option<usize> {
    if start > bytes.len() || needle.is_empty() {
        return None;
    }
    bytes[start..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| start + offset)
}

fn find_tag_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut quote = 0u8;
    for (offset, &byte) in bytes[start..].iter().enumerate() {
        if quote != 0 {
            if byte == quote {
                quote = 0;
            }
            continue;
        }
        match byte {
            b'\'' | b'"' => quote = byte,
            b'>' => return Some(start + offset),
            _ => {}
        }
    }
    None
}

fn find_declaration_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut quote = 0u8;
    let mut brackets = 0usize;
    for (offset, &byte) in bytes[start..].iter().enumerate() {
        if quote != 0 {
            if byte == quote {
                quote = 0;
            }
            continue;
        }
        match byte {
            b'\'' | b'"' => quote = byte,
            b'[' => brackets += 1,
            b']' => brackets = brackets.saturating_sub(1),
            b'>' if brackets == 0 => return Some(start + offset),
            _ => {}
        }
    }
    None
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn local_name(name: &[u8]) -> &[u8] {
    name.rsplit(|byte| *byte == b':').next().unwrap_or(name)
}

struct TagAttributes {
    id: Option<String>,
    reference: Option<String>,
    count: usize,
    total_bytes: usize,
    path_bytes: usize,
    image_bytes: Option<usize>,
}

fn parse_tag_attributes(content: &[u8]) -> TagAttributes {
    let mut cursor = 0usize;
    let mut id = None;
    let mut reference = None;
    let mut count = 0usize;
    let mut total_bytes = 0usize;
    let mut path_bytes = 0usize;
    let mut image_bytes = None;
    while cursor < content.len() {
        while cursor < content.len()
            && (content[cursor].is_ascii_whitespace() || content[cursor] == b'/')
        {
            cursor += 1;
        }
        let name_start = cursor;
        while cursor < content.len()
            && !content[cursor].is_ascii_whitespace()
            && content[cursor] != b'='
            && content[cursor] != b'/'
        {
            cursor += 1;
        }
        if name_start == cursor {
            cursor += 1;
            continue;
        }
        let name = &content[name_start..cursor];
        while cursor < content.len() && content[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= content.len() || content[cursor] != b'=' {
            continue;
        }
        cursor += 1;
        while cursor < content.len() && content[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= content.len() || !matches!(content[cursor], b'\'' | b'"') {
            continue;
        }
        count += 1;
        total_bytes = total_bytes.saturating_add(name.len());
        let quote = content[cursor];
        cursor += 1;
        let value_start = cursor;
        while cursor < content.len() && content[cursor] != quote {
            cursor += 1;
        }
        let raw_value = &content[value_start..cursor];
        total_bytes = total_bytes.saturating_add(raw_value.len());
        if local_name(name) == b"d" {
            path_bytes = path_bytes.saturating_add(raw_value.len());
        }
        let value = String::from_utf8_lossy(raw_value);
        if local_name(name) == b"id" && value.len() <= 256 {
            id = Some(value.into_owned());
        } else if local_name(name) == b"href" {
            if let Some(start) = value.find('#') {
                let end = value[start + 1..]
                    .find(|character: char| character.is_whitespace() || character == ')')
                    .map(|offset| start + 1 + offset)
                    .unwrap_or(value.len());
                if end > start + 1 && end - start - 1 <= 256 {
                    reference = Some(value[start + 1..end].to_owned());
                }
            }
            if value.starts_with("data:") {
                let payload = value
                    .split_once(',')
                    .map(|(_, value)| value.len())
                    .unwrap_or(0);
                image_bytes = Some(payload.saturating_mul(3) / 4);
            }
        }
        if cursor < content.len() {
            cursor += 1;
        }
    }
    TagAttributes {
        id,
        reference,
        count,
        total_bytes,
        path_bytes,
        image_bytes,
    }
}

/// Per-element context needed while importing a paint: the element's id (for
/// node names), the warning path, and its absolute transform (gradients live
/// in the element's local space and must be folded into world space).
pub struct PaintContext {
    pub path: String,
    pub id: String,
    pub element_affine: Affine,
}

impl PaintContext {
    pub fn name(&self, fallback: &str) -> String {
        nonempty_name(&self.id, fallback)
    }

    /// Compose the element's absolute transform with a gradient's own
    /// transform, producing the affine that maps gradient-local coordinates
    /// into world space.
    pub fn paint_affine(&self, gradient_transform: usvg::Transform) -> Affine {
        self.element_affine * usvg_transform_to_kurbo(gradient_transform)
    }
}

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

    fn with_children(node: Node, children: Vec<ImportTree>) -> Self {
        Self { node, children }
    }
}

struct Importer {
    document: Document,
    warnings: Vec<SvgWarning>,
    image_bytes: usize,
}

impl Importer {
    fn import_tree(&mut self, tree: &usvg::Tree) -> Result<(), SvgError> {
        let size = tree.size();
        let main = self.document.main;
        let composition = &mut self.document.compositions[main];
        composition.name = "Imported SVG".into();
        composition.size = (
            size.width().round().max(1.0) as u32,
            size.height().round().max(1.0) as u32,
        );
        composition.range = (Frame(0), Frame(1));

        let root = tree.root();
        let needs_wrapper = root.opacity().get() < 1.0
            || root.clip_path().is_some()
            || root.mask().is_some()
            || root.blend_mode() != usvg::BlendMode::Normal;

        let trees = if needs_wrapper {
            self.import_group(root, "svg", 0)?
                .map(|tree| vec![tree])
                .unwrap_or_default()
        } else {
            self.import_children(root.children(), "svg", 0)?
        };
        for tree in trees {
            self.attach_tree(tree, Parent::Comp(main));
        }
        Ok(())
    }

    fn attach_tree(&mut self, tree: ImportTree, parent: Parent) {
        let mut pending = vec![(tree, parent)];
        while let Some((tree, parent)) = pending.pop() {
            let id = self.document.create_node(tree.node);
            self.document
                .attach(id, parent, usize::MAX)
                .expect("fresh imported node attaches");
            for child in tree.children.into_iter().rev() {
                pending.push((child, Parent::Node(id)));
            }
        }
    }

    /// Import sibling nodes. SVG paints in document order (later = on top),
    /// while Renamite stacks index 0 on top, so the list is reversed before
    /// it is attached (appended).
    fn import_children(
        &mut self,
        nodes: &[SvgNode],
        base_path: &str,
        depth: usize,
    ) -> Result<Vec<ImportTree>, SvgError> {
        let mut trees = Vec::new();
        for (index, node) in nodes.iter().enumerate() {
            let path = format!("{base_path}/{index}");
            if let Some(tree) = self.import_node(node, &path, depth)? {
                trees.push(tree);
            }
        }
        trees.reverse();
        Ok(trees)
    }

    fn import_node(
        &mut self,
        node: &SvgNode,
        path: &str,
        depth: usize,
    ) -> Result<Option<ImportTree>, SvgError> {
        self.check_depth(depth, path)?;
        match node {
            SvgNode::Group(group) => self.import_group(group, path, depth),
            SvgNode::Path(path_node) => self.import_path_node(path_node, path),
            SvgNode::Text(text) => {
                self.warnings.push(SvgWarning {
                    path: path.into(),
                    message: "SVG text imported as editable path outlines".into(),
                });
                let content = self.import_children(text.flattened().children(), path, depth + 1)?;
                if content.is_empty() {
                    return Ok(None);
                }
                Ok(Some(ImportTree::with_children(
                    Node::new(nonempty_name(text.id(), "Text"), NodeKind::Group),
                    content,
                )))
            }
            SvgNode::Image(image) => self.import_image_node(image, path),
        }
    }

    fn import_group(
        &mut self,
        group: &usvg::Group,
        path: &str,
        depth: usize,
    ) -> Result<Option<ImportTree>, SvgError> {
        self.check_depth(depth, path)?;
        let mut children: Vec<ImportTree> = Vec::new();

        // Masks/clip paths clip everything that follows, so they lead.
        if let Some(clip) = group.clip_path()
            && let Some(mask) = self.import_clip_path(clip, group, path, depth + 1)?
        {
            children.push(mask);
        }
        if let Some(mask) = group.mask()
            && let Some(mask_tree) = self.import_mask(mask, group, path, depth + 1)?
        {
            children.push(mask_tree);
        }

        for filter in group.filters() {
            self.warnings.push(SvgWarning {
                path: path.into(),
                message: format!(
                    "SVG filter `{}` is not supported in Renamite and was skipped",
                    filter.id()
                ),
            });
        }

        let content = self.import_children(group.children(), path, depth + 1)?;
        if children.is_empty() && content.is_empty() {
            return Ok(None);
        }
        children.extend(content);

        let mut node = Node::new(nonempty_name(group.id(), "Group"), NodeKind::Group);
        node.opacity = Animated::new(group.opacity().get() as f64);
        let blend = match group.blend_mode() {
            usvg::BlendMode::Normal => BlendMode::Normal,
            usvg::BlendMode::Multiply => BlendMode::Multiply,
            usvg::BlendMode::Screen => BlendMode::Screen,
            usvg::BlendMode::Overlay => BlendMode::Overlay,
            usvg::BlendMode::Darken => BlendMode::Darken,
            usvg::BlendMode::Lighten => BlendMode::Lighten,
            usvg::BlendMode::ColorDodge => BlendMode::ColorDodge,
            usvg::BlendMode::ColorBurn => BlendMode::ColorBurn,
            usvg::BlendMode::HardLight => BlendMode::HardLight,
            usvg::BlendMode::SoftLight => BlendMode::SoftLight,
            usvg::BlendMode::Difference => BlendMode::Difference,
            usvg::BlendMode::Exclusion => BlendMode::Exclusion,
            usvg::BlendMode::Hue => BlendMode::Hue,
            usvg::BlendMode::Saturation => BlendMode::Saturation,
            usvg::BlendMode::Color => BlendMode::Color,
            usvg::BlendMode::Luminosity => BlendMode::Luminosity,
        };
        if blend != BlendMode::Normal {
            node.kind = NodeKind::Layer(LayerProps {
                blend,
                ..LayerProps::default()
            });
        }
        Ok(Some(ImportTree::with_children(node, children)))
    }

    fn import_path_node(
        &mut self,
        path_node: &usvg::Path,
        path: &str,
    ) -> Result<Option<ImportTree>, SvgError> {
        if !path_node.is_visible() {
            return Ok(None);
        }
        if path_node.data().segments().count() > MAX_SVG_PATH_POINTS {
            return Err(SvgError::InputLimit("SVG path data is too large"));
        }
        let affine = usvg_transform_to_kurbo(path_node.abs_transform());
        let geometry = affine * tiny_path_to_kurbo(path_node.data());
        let contours = renamite_geometry::split_bez_subpaths(&geometry);
        if contours.is_empty() {
            return Ok(None);
        }
        let shape = if let [contour] = contours.as_slice() {
            ShapeKind::Path(Animated::new(contour.clone()))
        } else {
            ShapeKind::CompoundPath(CompoundPath {
                contours: contours.into_iter().map(Animated::new).collect(),
            })
        };

        let name = nonempty_name(path_node.id(), "Path");
        let mut children = vec![ImportTree::leaf(Node::new(
            name.clone(),
            NodeKind::Shape(shape),
        ))];

        let context = PaintContext {
            path: path.into(),
            id: path_node.id().into(),
            element_affine: affine,
        };
        if let Some(fill) = path_node.fill()
            && let Some(style) = import_fill(fill, &context, &mut self.warnings)
        {
            children.push(ImportTree::leaf(style));
        }
        if let Some(stroke) = path_node.stroke()
            && let Some(style) = import_stroke(stroke, &context, &mut self.warnings)
        {
            children.insert(1, ImportTree::leaf(style));
        }

        if children.len() == 1 {
            return Ok(Some(children.pop().unwrap()));
        }
        Ok(Some(ImportTree::with_children(
            Node::new(name, NodeKind::Group),
            children,
        )))
    }

    fn import_clip_path(
        &mut self,
        clip: &usvg::ClipPath,
        owner: &usvg::Group,
        path: &str,
        depth: usize,
    ) -> Result<Option<ImportTree>, SvgError> {
        let clip_affine = usvg_transform_to_kurbo(clip.transform());
        // The referencing group's absolute transform maps the clip's user
        // space into world space; the clip children are relative to the clip
        // root (identity), so compose both.
        let bias = usvg_transform_to_kurbo(owner.abs_transform()) * clip_affine;
        let Some(shape) = self.import_clip_shape(clip.root(), path, bias, depth)? else {
            return Ok(None);
        };
        let mut node = Node::new(
            nonempty_name(clip.id(), "Clip"),
            NodeKind::Mask(MaskProps {
                inverted: false,
                shape,
            }),
        );
        node.visible = true;
        Ok(Some(ImportTree::leaf(node)))
    }

    fn import_mask(
        &mut self,
        mask: &usvg::Mask,
        owner: &usvg::Group,
        path: &str,
        depth: usize,
    ) -> Result<Option<ImportTree>, SvgError> {
        if mask.kind() != usvg::MaskType::Alpha {
            self.warnings.push(SvgWarning {
                path: path.into(),
                message: "SVG luminance masks are approximated as alpha masks".into(),
            });
        }
        let bias = usvg_transform_to_kurbo(owner.abs_transform());
        let Some(shape) = self.import_clip_shape(mask.root(), path, bias, depth)? else {
            return Ok(None);
        };
        Ok(Some(ImportTree::leaf(Node::new(
            nonempty_name(mask.id(), "Mask"),
            NodeKind::Mask(MaskProps {
                inverted: false,
                shape,
            }),
        ))))
    }

    /// Collect every visible shape inside a clip/mask root into a single
    /// combined path, in world space.
    fn import_clip_shape(
        &mut self,
        root: &usvg::Group,
        path: &str,
        bias: Affine,
        depth: usize,
    ) -> Result<Option<ShapeKind>, SvgError> {
        let mut combined = kurbo::BezPath::new();
        self.gather_clip_geometry(root, &bias, &mut combined, path, depth)?;
        let contours = renamite_geometry::split_bez_subpaths(&combined);
        if contours.is_empty() {
            Ok(None)
        } else if contours.len() == 1 {
            Ok(Some(ShapeKind::Path(Animated::new(contours[0].clone()))))
        } else {
            Ok(Some(ShapeKind::CompoundPath(CompoundPath {
                contours: contours.into_iter().map(Animated::new).collect(),
            })))
        }
    }

    fn gather_clip_geometry(
        &mut self,
        group: &usvg::Group,
        bias: &Affine,
        combined: &mut kurbo::BezPath,
        path: &str,
        depth: usize,
    ) -> Result<(), SvgError> {
        self.check_depth(depth, path)?;
        let mut path_points = 0usize;
        for (index, child) in group.children().iter().enumerate() {
            match child {
                SvgNode::Path(p) => {
                    path_points = path_points.saturating_add(p.data().segments().count());
                    if path_points > MAX_SVG_PATH_POINTS {
                        return Err(SvgError::InputLimit("SVG clip path data is too large"));
                    }
                    if p.is_visible() {
                        if p.fill()
                            .is_some_and(|fill| fill.rule() == usvg::FillRule::EvenOdd)
                        {
                            self.warnings.push(SvgWarning {
                                path: format!("{path}/{index}"),
                                message:
                                    "SVG clip path with evenodd rule is approximated as nonzero"
                                        .into(),
                            });
                        }
                        let affine = *bias * usvg_transform_to_kurbo(p.abs_transform());
                        combined.extend(affine * tiny_path_to_kurbo(p.data()));
                    }
                }
                SvgNode::Group(g) => {
                    self.gather_clip_geometry(g, bias, combined, path, depth + 1)?;
                }
                SvgNode::Text(t) => {
                    self.warnings.push(SvgWarning {
                        path: format!("{path}/{index}"),
                        message: "SVG text inside a clip path imported as path outlines".into(),
                    });
                    self.gather_clip_geometry(t.flattened(), bias, combined, path, depth + 1)?;
                }
                SvgNode::Image(_) => {
                    self.warnings.push(SvgWarning {
                        path: format!("{path}/{index}"),
                        message: "SVG image inside a clip path was skipped".into(),
                    });
                }
            }
        }
        Ok(())
    }

    fn check_depth(&self, depth: usize, path: &str) -> Result<(), SvgError> {
        if depth > MAX_SVG_DEPTH {
            let _ = path;
            return Err(SvgError::InputLimit("SVG nesting is too deep"));
        }
        Ok(())
    }

    fn import_image_node(
        &mut self,
        image: &usvg::Image,
        path: &str,
    ) -> Result<Option<ImportTree>, SvgError> {
        if !image.is_visible() {
            return Ok(None);
        }
        let (mime, bytes) = match image.kind() {
            ImageKind::PNG(data) => ("image/png", data.as_ref()),
            ImageKind::JPEG(data) => ("image/jpeg", data.as_ref()),
            ImageKind::GIF(data) => ("image/gif", data.as_ref()),
            ImageKind::WEBP(data) => ("image/webp", data.as_ref()),
            ImageKind::SVG(_) => {
                self.warnings.push(SvgWarning {
                    path: path.into(),
                    message: "SVG-in-SVG images are not supported in v1 and were skipped".into(),
                });
                return Ok(None);
            }
        };
        if bytes.len() > MAX_SVG_IMAGE_BYTES
            || self.image_bytes.saturating_add(bytes.len()) > MAX_SVG_TOTAL_IMAGE_BYTES
        {
            return Err(SvgError::InputLimit("SVG images are too large"));
        }

        let (width, height) = match image::ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
        {
            Ok(mut reader) => {
                let mut limits = image::Limits::default();
                limits.max_image_width = Some(MAX_SVG_IMAGE_DIMENSION);
                limits.max_image_height = Some(MAX_SVG_IMAGE_DIMENSION);
                limits.max_alloc = Some(MAX_SVG_IMAGE_ALLOC);
                reader.limits(limits);
                match reader.decode() {
                    Ok(decoded) => decoded.dimensions(),
                    Err(error) => {
                        self.warnings.push(SvgWarning {
                            path: path.into(),
                            message: format!("SVG image failed to decode and was skipped: {error}"),
                        });
                        return Ok(None);
                    }
                }
            }
            Err(error) => {
                self.warnings.push(SvgWarning {
                    path: path.into(),
                    message: format!("SVG image failed to decode and was skipped: {error}"),
                });
                return Ok(None);
            }
        };
        if width == 0
            || height == 0
            || width > MAX_SVG_IMAGE_DIMENSION
            || height > MAX_SVG_IMAGE_DIMENSION
        {
            return Err(SvgError::InputLimit("SVG image dimensions are too large"));
        }

        let affine = usvg_transform_to_kurbo(image.abs_transform());
        let transform = match affine_to_animated_transform(affine) {
            Some(transform) => transform,
            None => {
                self.warnings.push(SvgWarning {
                    path: path.into(),
                    message: "SVG image has a transform (e.g. reflection) that cannot be represented; skipped".into(),
                });
                return Ok(None);
            }
        };
        let name = nonempty_name(image.id(), "Image");
        let asset_id = self.document.assets.insert(Asset::Image(ImageAsset {
            name: name.clone(),
            mime: mime.into(),
            bytes: bytes.to_vec(),
            width,
            height,
            srgb: true,
        }));
        self.document.asset_order.push(asset_id);
        self.image_bytes += bytes.len();
        let mut node = Node::new(name, NodeKind::Image(ImageNode::new(asset_id)));
        node.transform = transform;
        Ok(Some(ImportTree::leaf(node)))
    }
}

fn nonempty_name(id: &str, fallback: &str) -> String {
    if id.is_empty() {
        fallback.to_string()
    } else {
        id.to_string()
    }
}
