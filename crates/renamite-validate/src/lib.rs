//! Project validation and diagnostics for renamite.
//!
//! Deterministic checks over a [`RenFile`]: document tree integrity,
//! asset references, animation keyframe hygiene, clip/machine sanity, and
//! export-readiness warnings. Use [`validate`] to produce a
//! [`ValidationReport`]; [`ValidationReport::has_errors`] tells you whether the
//! project is safe to save/render/export.

use glam::DVec2;
use image::GenericImageView;
use renamite_animation::{Angle, Animated, AnimatedTransform, Frame};
use renamite_geometry::VectorPath;
use renamite_io_ren::RenFile;
use renamite_machine::{
    Condition, InputKind, ListenerAction, Machine, MachineId, StateKind, Transition,
};
use renamite_model::{
    Asset, Color, CompId, Document, GradientStops, MAX_REPEATER_COPIES, MAX_REPEATER_OFFSET,
    ModifierKind, Node, NodeId, NodeKind, PropRef, ShapeKind, StyleKind, StylePaint, Value,
    node_supports_opacity, node_supports_transform,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Cursor;

const MAX_TREE_DEPTH: usize = 256;
const MAX_PRECOMP_DEPTH: usize = 256;
const MAX_TREE_NODES: usize = 100_000;
const MAX_CHILDREN_PER_NODE: usize = 100_000;
const MAX_KEYFRAMES: usize = 100_000;
const MAX_PATH_ANCHORS: usize = 100_000;
const MAX_GRADIENT_STOPS: usize = 4096;
const MAX_COMPOUND_CONTOURS: usize = 4096;
const MAX_TOTAL_KEYFRAMES: usize = 1_000_000;
const MAX_TOTAL_PATH_ANCHORS: usize = 2_000_000;
const MAX_TOTAL_GRADIENT_STOPS: usize = 100_000;
const MAX_TOTAL_IMAGE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_TOTAL_ASSETS: usize = 10_000;
const MAX_TOTAL_CLIP_TRACKS: usize = 100_000;
const MAX_TOTAL_CLIP_KEYS: usize = 1_000_000;
const MAX_TOTAL_MACHINE_ELEMENTS: usize = 100_000;
const MAX_DIAGNOSTICS: usize = 100_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub path: String,
    pub message: String,
}

impl Diagnostic {
    pub fn error(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            path: path.into(),
            message: message.into(),
        }
    }

    pub fn warning(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            path: path.into(),
            message: message.into(),
        }
    }

    pub fn info(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Info,
            path: path.into(),
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ValidationReport {
    pub diagnostics: Vec<Diagnostic>,
}

impl ValidationReport {
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }

    pub fn error_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count()
    }

    pub fn warning_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .count()
    }

    pub fn push(&mut self, d: Diagnostic) {
        self.diagnostics.push(d);
    }
}

pub fn validate(file: &RenFile) -> ValidationReport {
    let mut v = Validator {
        file,
        report: ValidationReport::default(),
        total_keyframes: 0,
        total_path_anchors: 0,
        total_gradient_stops: 0,
        total_image_bytes: 0,
        total_clip_tracks: 0,
        total_clip_keys: 0,
        total_machine_elements: 0,
    };
    v.run();
    v.report
}

enum CompEvent {
    Enter(CompId, usize),
    Leave(CompId),
}

enum TreeEvent {
    Enter(NodeId, String, usize),
    Leave(NodeId),
}

struct Validator<'a> {
    file: &'a RenFile,
    report: ValidationReport,
    total_keyframes: usize,
    total_path_anchors: usize,
    total_gradient_stops: usize,
    total_image_bytes: u64,
    total_clip_tracks: usize,
    total_clip_keys: usize,
    total_machine_elements: usize,
}

impl<'a> Validator<'a> {
    fn run(&mut self) {
        self.validate_compositions();
        self.validate_document_tree();
        self.validate_assets();
        self.validate_animations();
        self.validate_scope();
        self.validate_precomps();
        self.validate_clips();
        self.validate_machines();
        self.validate_export_readiness();
    }

    fn err(&mut self, path: impl Into<String>, message: impl Into<String>) {
        if self.report.diagnostics.len() < MAX_DIAGNOSTICS {
            self.report.push(Diagnostic::error(path, message));
        } else if let Some(last) = self.report.diagnostics.last_mut()
            && last.severity == Severity::Warning
        {
            *last = Diagnostic::error(path, message);
        }
    }

    fn warn(&mut self, path: impl Into<String>, message: impl Into<String>) {
        if self.report.diagnostics.len() < MAX_DIAGNOSTICS {
            self.report.push(Diagnostic::warning(path, message));
        }
    }

    fn validate_compositions(&mut self) {
        let doc = &self.file.document;

        if !doc.compositions.contains_key(doc.main) {
            self.err("document.main", "main composition does not exist");
        }

        let mut roots = HashSet::new();
        for (id, comp) in &doc.compositions {
            if comp.rate.num == 0 || comp.rate.den == 0 || comp.rate.num > 1_000_000 {
                self.err(format!("composition/{id:?}/rate"), "invalid frame rate");
            }
            if comp.range.1 <= comp.range.0 {
                self.err(
                    format!("composition/{id:?}/range"),
                    "out frame must be after in frame",
                );
            }
            if comp.size.0 == 0 || comp.size.1 == 0 {
                self.warn(
                    format!("composition/{id:?}/size"),
                    "composition size is zero",
                );
            } else if comp.size.0 > 16_384 || comp.size.1 > 16_384 {
                self.err(
                    format!("composition/{id:?}/size"),
                    "composition size is too large",
                );
            }
            if comp.children.len() > MAX_CHILDREN_PER_NODE {
                self.err(
                    format!("composition/{id:?}/children"),
                    "composition has too many children",
                );
            }
            for (index, child) in comp.children.iter().enumerate() {
                if !roots.insert(*child) {
                    self.err(
                        format!("composition/{id:?}/children/{index}"),
                        "duplicate composition root",
                    );
                }
                if !doc.nodes.contains_key(*child) {
                    self.err(
                        format!("composition/{id:?}/children/{index}"),
                        "child node does not exist",
                    );
                } else if doc.nodes.get(*child).and_then(|node| node.parent).is_some() {
                    self.err(
                        format!("composition/{id:?}/children/{index}"),
                        "composition root has a parent pointer",
                    );
                }
            }
        }
    }

    fn validate_document_tree(&mut self) {
        let doc = &self.file.document;
        let mut seen = HashSet::new();
        let mut active = HashSet::new();
        let mut visits = 0usize;
        let mut limited = false;

        for (comp_id, comp) in &doc.compositions {
            let mut pending = comp
                .children
                .iter()
                .rev()
                .map(|root| TreeEvent::Enter(*root, format!("composition/{comp_id:?}"), 0))
                .collect::<Vec<_>>();
            while let Some(event) = pending.pop() {
                match event {
                    TreeEvent::Leave(id) => {
                        active.remove(&id);
                    }
                    TreeEvent::Enter(id, path, depth) => {
                        visits += 1;
                        if visits > MAX_TREE_NODES {
                            if !limited {
                                self.err("document.tree", "node tree is too large");
                                limited = true;
                            }
                            break;
                        }
                        if depth > MAX_TREE_DEPTH {
                            self.err(&path, "node tree nesting is too deep");
                            break;
                        }
                        if !active.insert(id) {
                            self.err(format!("{path}/node/{id:?}"), "cycle in node tree");
                            continue;
                        }
                        let Some(node) = doc.nodes.get(id) else {
                            self.err(path, format!("node {id:?} does not exist"));
                            active.remove(&id);
                            continue;
                        };
                        seen.insert(id);
                        if node.children.len() > MAX_CHILDREN_PER_NODE {
                            self.err(
                                format!("node/{id:?}/children"),
                                "node has too many children",
                            );
                        }
                        pending.push(TreeEvent::Leave(id));
                        let mut child_ids = HashSet::new();
                        for (index, &child) in node.children.iter().enumerate().rev() {
                            let child_path = format!("node/{id:?}/children/{index}");
                            if !child_ids.insert(child) {
                                self.err(
                                    format!("node/{id:?}/children/{index}"),
                                    "duplicate child node",
                                );
                            }
                            if let Some(child_node) = doc.nodes.get(child) {
                                if child_node.parent != Some(id) {
                                    self.err(
                                        format!("node/{id:?}/children/{index}"),
                                        "child parent pointer does not point back to this node",
                                    );
                                }
                            } else {
                                self.err(
                                    format!("node/{id:?}/children/{index}"),
                                    "child node does not exist",
                                );
                                continue;
                            }
                            pending.push(TreeEvent::Enter(child, child_path, depth + 1));
                        }
                    }
                }
            }
            active.clear();
            if limited {
                break;
            }
        }

        for id in doc.nodes.keys() {
            if !seen.contains(&id) {
                self.warn(
                    format!("node/{id:?}"),
                    "detached arena node will be pruned on save",
                );
            }
        }
        self.validate_parent_chains();
    }

    fn validate_parent_chains(&mut self) {
        let doc = &self.file.document;
        for start in doc.nodes.keys() {
            let mut current = start;
            let mut seen = HashSet::new();
            for depth in 0..=MAX_TREE_DEPTH {
                if !seen.insert(current) {
                    self.err(
                        format!("node/{start:?}/parent"),
                        "parent pointer chain contains a cycle",
                    );
                    break;
                }
                let Some(node) = doc.nodes.get(current) else {
                    self.err(
                        format!("node/{start:?}/parent"),
                        "parent pointer references a missing node",
                    );
                    break;
                };
                let Some(parent) = node.parent else {
                    break;
                };
                if !doc.nodes.get(parent).is_some_and(|parent_node| {
                    parent_node
                        .children
                        .iter()
                        .filter(|&&child| child == current)
                        .count()
                        == 1
                }) {
                    self.err(
                        format!("node/{current:?}/parent"),
                        "parent pointer is not backed by the parent child list",
                    );
                    break;
                }
                current = parent;
                if depth == MAX_TREE_DEPTH {
                    self.err(
                        format!("node/{start:?}/parent"),
                        "parent pointer chain is too deep",
                    );
                }
            }
        }
    }

    fn validate_assets(&mut self) {
        let doc = &self.file.document;

        if doc.assets.len() > MAX_TOTAL_ASSETS {
            self.err("assets", "project has too many assets");
        }
        let mut seen = HashSet::new();
        if doc.asset_order.len() > MAX_TREE_NODES {
            self.err("assets/order", "asset order is too large");
        }
        for (i, &id) in doc.asset_order.iter().enumerate() {
            if !doc.assets.contains_key(id) {
                self.err(format!("assets/order/{i}"), "asset id does not exist");
            }
            if !seen.insert(id) {
                self.err(
                    format!("assets/order/{i}"),
                    "duplicate asset id in asset_order",
                );
            }
        }

        for id in doc.assets.keys() {
            if !seen.contains(&id) {
                self.warn(
                    format!("asset/{id:?}"),
                    "asset exists but is not attached in asset_order",
                );
            }
        }

        for (id, node) in doc.nodes.iter().take(MAX_TREE_NODES) {
            match &node.kind {
                NodeKind::Image(img) => match doc.assets.get(img.asset()) {
                    Some(Asset::Image(img)) => {
                        if img.width == 0 || img.height == 0 {
                            self.err(
                                format!("node/{id:?}/image"),
                                "image dimensions must be nonzero",
                            );
                        } else if img.width > 16_384 || img.height > 16_384 {
                            self.err(
                                format!("node/{id:?}/image"),
                                "image dimensions are too large",
                            );
                        }
                        if img.bytes.is_empty() {
                            self.err(format!("node/{id:?}/image"), "image asset has no bytes");
                        } else if img.bytes.len() > 128 * 1024 * 1024 {
                            self.err(format!("node/{id:?}/image"), "image asset is too large");
                        }
                    }
                    Some(_) => self.err(
                        format!("node/{id:?}/image"),
                        "referenced asset is not an image",
                    ),
                    None => self.err(format!("node/{id:?}/image"), "image asset is missing"),
                },
                NodeKind::Text(text) => {
                    if let Some(family) = &text.font
                        && family != "default"
                        && doc.font_asset_for_family(family).is_none()
                    {
                        self.warn(
                            format!("node/{id:?}/text/font"),
                            format!(
                                "font family `{family}` not found; bundled default will be used"
                            ),
                        );
                    }
                }
                _ => {}
            }
        }

        for (id, asset) in doc.assets.iter().take(MAX_TOTAL_ASSETS) {
            match asset {
                Asset::Image(img) => {
                    self.total_image_bytes = self
                        .total_image_bytes
                        .saturating_add(img.bytes.len() as u64);
                    if self.total_image_bytes > MAX_TOTAL_IMAGE_BYTES {
                        self.err("assets", "project has too many image bytes");
                    }
                    if img.width == 0 || img.height == 0 {
                        self.err(format!("asset/{id:?}"), "image dimensions must be nonzero");
                    } else if img.width > 16_384 || img.height > 16_384 {
                        self.err(format!("asset/{id:?}"), "image dimensions are too large");
                    }
                    if img.bytes.is_empty() {
                        self.warn(format!("asset/{id:?}"), "image asset has empty bytes");
                    } else if img.bytes.len() > 128 * 1024 * 1024 {
                        self.err(format!("asset/{id:?}"), "image asset is too large");
                    } else {
                        self.validate_image_bytes(&format!("asset/{id:?}"), img);
                    }
                }
                Asset::Font(font) => {
                    if font.bytes.is_empty() {
                        self.err(format!("asset/{id:?}"), "font has no bytes");
                    } else if font.bytes.len() > 64 * 1024 * 1024 {
                        self.err(format!("asset/{id:?}"), "font asset is too large");
                    }
                    if font.family.trim().is_empty() {
                        self.err(format!("asset/{id:?}"), "font family is empty");
                    }
                }
            }
        }

        self.validate_asset_usage(doc);
    }

    fn validate_image_bytes(&mut self, path: &str, image: &renamite_model::ImageAsset) {
        let mut reader =
            match image::ImageReader::new(Cursor::new(&image.bytes)).with_guessed_format() {
                Ok(reader) => reader,
                Err(error) => {
                    self.warn(
                        path,
                        format!("image format could not be identified: {error}"),
                    );
                    return;
                }
            };
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(16_384);
        limits.max_image_height = Some(16_384);
        limits.max_alloc = Some(128 * 1024 * 1024);
        reader.limits(limits);
        match reader.decode() {
            Ok(decoded) => {
                let (width, height) = decoded.dimensions();
                if width == 0 || height == 0 || width > 16_384 || height > 16_384 {
                    self.err(path, "decoded image dimensions are too large");
                }
                if width != image.width || height != image.height {
                    self.warn(path, "stored image dimensions do not match encoded data");
                }
            }
            Err(image::ImageError::Limits(_)) => {
                self.err(path, "encoded image exceeds decode limits");
            }
            Err(error) => {
                self.warn(path, format!("image could not be decoded: {error}"));
            }
        }
    }

    fn validate_asset_usage(&mut self, doc: &Document) {
        for (id, asset) in doc.assets.iter().take(MAX_TOTAL_ASSETS) {
            match asset {
                Asset::Image(_) => {
                    let used = doc
                        .nodes
                        .values()
                        .any(|n| matches!(&n.kind, NodeKind::Image(img) if img.asset() == id));
                    if !used {
                        self.warn(
                            format!("asset/{id:?}"),
                            "image asset is not used by any image layer",
                        );
                    }
                }
                Asset::Font(font) => {
                    let used = doc.nodes.values().any(|n| {
                        matches!(&n.kind, NodeKind::Text(t) if t.font.as_deref() == Some(font.family.as_str()))
                    });
                    if !used {
                        self.warn(
                            format!("asset/{id:?}"),
                            "font asset is not used by any text node",
                        );
                    }
                }
            }
        }
    }

    fn validate_animations(&mut self) {
        let doc = &self.file.document;
        if doc.nodes.len() > MAX_TREE_NODES {
            self.err("document.nodes", "project has too many nodes");
        }
        for (id, node) in doc.nodes.iter().take(MAX_TREE_NODES) {
            self.validate_node_animations(id, node);
        }
    }

    fn validate_node_animations(&mut self, id: NodeId, node: &Node) {
        let base = format!("node/{id:?}");
        self.check_transform(&format!("{base}/transform"), &node.transform);
        self.check_animated(&format!("{base}/opacity"), &node.opacity, finite_f64);
        self.check_opacity_range(&format!("{base}/opacity"), &node.opacity);

        if !node_supports_transform(&node.kind) && !transform_is_default(&node.transform) {
            self.warn(
                format!("{base}/transform"),
                "transform is not honored for this node kind and has no render effect",
            );
        }
        if !node_supports_opacity(&node.kind) && !opacity_is_default(&node.opacity) {
            self.warn(
                format!("{base}/opacity"),
                "opacity is not honored for this node kind and has no render effect",
            );
        }

        match &node.kind {
            NodeKind::Shape(shape) => self.validate_shape_animations(id, shape),
            NodeKind::Style(style) => self.validate_style_animations(id, style),
            NodeKind::Modifier(modifier) => self.validate_modifier_animations(id, modifier),
            NodeKind::Text(text) => {
                self.check_animated(&format!("{base}/text/size"), &text.size, finite_f64);
                self.check_animated(&format!("{base}/text/tracking"), &text.tracking, finite_f64);
                self.check_animated(&format!("{base}/text/leading"), &text.leading, finite_f64);
                self.check_nonnegative(&format!("{base}/text/size"), &text.size);
            }
            NodeKind::Layer(props) => {
                if !props.time_stretch.is_finite() || props.time_stretch <= 0.0 {
                    self.err(
                        format!("{base}/layer/time_stretch"),
                        "time stretch must be positive and finite",
                    );
                }
                if props.out_frame <= props.in_frame {
                    self.warn(
                        format!("{base}/layer/range"),
                        "layer out frame must be after in frame",
                    );
                }
            }
            NodeKind::Mask(mask) => {
                self.validate_shape_animations(id, &mask.shape);
                if shape_kind_is_empty(&mask.shape) {
                    self.warn(format!("{base}/mask"), "mask has no geometry");
                }
            }
            NodeKind::Image(img) => {
                self.check_animated(&format!("{base}/image/tint"), img.tint(), finite_color);
                let c = img.crop();
                if !c.x.is_finite() || !c.y.is_finite() || !c.z.is_finite() || !c.w.is_finite() {
                    self.err(format!("{base}/image/crop"), "crop is not finite");
                } else {
                    if !(0.0..=1.0).contains(&c.x)
                        || !(0.0..=1.0).contains(&c.y)
                        || c.z <= 0.0
                        || c.w <= 0.0
                        || c.z > 1.0
                        || c.w > 1.0
                    {
                        self.err(
                            format!("{base}/image/crop"),
                            "crop must be x,y in [0,1] and w,h in (0,1]",
                        );
                    }
                    if c.x + c.z > 1.0 + 1e-9 || c.y + c.w > 1.0 + 1e-9 {
                        self.err(
                            format!("{base}/image/crop"),
                            "crop rect must be inside [0,1] image bounds (x+w<=1, y+h<=1)",
                        );
                    }
                }
            }
            NodeKind::Group | NodeKind::Precomp { .. } | NodeKind::Use { .. } => {}
        }
    }

    fn validate_shape_animations(&mut self, id: NodeId, shape: &ShapeKind) {
        let base = format!("node/{id:?}/shape");
        match shape {
            ShapeKind::Path(path) => {
                self.check_animated(&format!("{base}/path"), path, finite_path);
                self.check_path_complexity(&format!("{base}/path"), path);
            }
            ShapeKind::Rect { pos, size, rounded } => {
                self.check_animated(&format!("{base}/pos"), pos, finite_vec2);
                self.check_animated(&format!("{base}/size"), size, finite_vec2);
                self.check_animated(&format!("{base}/rounded"), rounded, finite_f64);
                self.check_nonnegative_size(&format!("{base}/size"), size);
                self.check_nonnegative(&format!("{base}/rounded"), rounded);
            }
            ShapeKind::Ellipse { pos, size } => {
                self.check_animated(&format!("{base}/pos"), pos, finite_vec2);
                self.check_animated(&format!("{base}/size"), size, finite_vec2);
                self.check_nonnegative_size(&format!("{base}/size"), size);
            }
            ShapeKind::Star {
                pos,
                points,
                inner_r,
                outer_r,
                roundness,
                ..
            } => {
                self.check_animated(&format!("{base}/pos"), pos, finite_vec2);
                self.check_animated(&format!("{base}/points"), points, finite_f64);
                self.check_animated(&format!("{base}/inner_r"), inner_r, finite_f64);
                self.check_animated(&format!("{base}/outer_r"), outer_r, finite_f64);
                self.check_animated(&format!("{base}/roundness"), roundness, finite_f64);
                self.check_range(&format!("{base}/points"), points, 3.0, 256.0);
                self.check_nonnegative(&format!("{base}/inner_r"), inner_r);
                self.check_nonnegative(&format!("{base}/outer_r"), outer_r);
                self.check_nonnegative(&format!("{base}/roundness"), roundness);
            }
            ShapeKind::Polygon {
                pos,
                points,
                outer_r,
                roundness,
            } => {
                self.check_animated(&format!("{base}/pos"), pos, finite_vec2);
                self.check_animated(&format!("{base}/points"), points, finite_f64);
                self.check_animated(&format!("{base}/outer_r"), outer_r, finite_f64);
                self.check_animated(&format!("{base}/roundness"), roundness, finite_f64);
                self.check_range(&format!("{base}/points"), points, 3.0, 256.0);
                self.check_nonnegative(&format!("{base}/outer_r"), outer_r);
                self.check_nonnegative(&format!("{base}/roundness"), roundness);
            }
            ShapeKind::CompoundPath(compound) => {
                if compound.contours.len() > MAX_COMPOUND_CONTOURS {
                    self.err(
                        format!("{base}/contours"),
                        "compound path has too many contours",
                    );
                }
                for (i, contour) in compound
                    .contours
                    .iter()
                    .take(MAX_COMPOUND_CONTOURS)
                    .enumerate()
                {
                    let path = format!("{base}/contour/{i}");
                    self.check_animated(&path, contour, finite_path);
                    self.check_path_complexity(&path, contour);
                }
            }
        }
    }

    fn validate_style_animations(&mut self, id: NodeId, style: &StyleKind) {
        let base = format!("node/{id:?}/style");
        match style {
            StyleKind::Fill { paint, .. } => {
                self.validate_paint(&format!("{base}/paint"), paint);
            }
            StyleKind::Stroke {
                paint,
                width,
                dash,
                miter_limit,
                ..
            } => {
                self.validate_paint(&format!("{base}/paint"), paint);
                self.check_animated(&format!("{base}/width"), width, finite_f64);
                self.check_nonnegative(&format!("{base}/width"), width);
                self.check_animated(&format!("{base}/miter_limit"), miter_limit, finite_f64);
                self.check_range(&format!("{base}/miter_limit"), miter_limit, 1.0, 10.0);
                if let Some(dash) = dash {
                    if dash.dashes.len() > 4096 {
                        self.err(format!("{base}/dash"), "dash pattern has too many entries");
                    }
                    for (i, d) in dash.dashes.iter().take(4096).enumerate() {
                        let path = format!("{base}/dash/{i}");
                        self.check_animated(&path, d, finite_f64);
                        self.check_nonnegative(&path, d);
                    }
                    self.check_animated(&format!("{base}/dash/offset"), &dash.offset, finite_f64);
                    let base_sum = dash
                        .dashes
                        .iter()
                        .map(|value| value.base)
                        .filter(|value| value.is_finite())
                        .sum::<f64>();
                    let keyed_sum = dash.dashes.iter().any(|value| {
                        value
                            .keyframes
                            .iter()
                            .any(|key| key.value.is_finite() && key.value > 0.0)
                    });
                    if dash.dashes.is_empty()
                        || !base_sum.is_finite()
                        || (base_sum <= 1e-9 && !keyed_sum)
                    {
                        self.warn(
                            format!("{base}/dash"),
                            "dash pattern has no positive length and is ignored",
                        );
                    }
                }
            }
        }
    }

    fn validate_paint(&mut self, path: &str, paint: &StylePaint) {
        match paint {
            StylePaint::Solid { color } => {
                self.check_animated(path, color, finite_color);
                self.check_color_range(path, color);
            }
            StylePaint::Gradient(gradient) => {
                self.check_animated(&format!("{path}/start"), &gradient.start, finite_vec2);
                self.check_animated(&format!("{path}/end"), &gradient.end, finite_vec2);
                self.check_animated(&format!("{path}/stops"), &gradient.stops, finite_stops);
                self.check_gradient_stops(&format!("{path}/stops"), &gradient.stops);
                let radial = matches!(gradient.kind, renamite_model::GradientKind::Radial);
                self.check_gradient_axis(
                    &format!("{path}/start"),
                    &gradient.start,
                    &gradient.end,
                    radial,
                );
            }
        }
    }

    fn validate_modifier_animations(&mut self, id: NodeId, modifier: &ModifierKind) {
        let base = format!("node/{id:?}/modifier");
        match modifier {
            ModifierKind::TrimPath {
                start, end, offset, ..
            } => {
                self.check_animated(&format!("{base}/start"), start, finite_f64);
                self.check_animated(&format!("{base}/end"), end, finite_f64);
                self.check_animated(&format!("{base}/offset"), offset, finite_f64);
                self.check_range(&format!("{base}/start"), start, 0.0, 1.0);
                self.check_range(&format!("{base}/end"), end, 0.0, 1.0);
                self.check_abs_max(&format!("{base}/offset"), offset, 1_000_000.0);
            }
            ModifierKind::Repeater {
                copies,
                offset,
                transform,
                start_opacity,
                end_opacity,
            } => {
                self.check_animated(&format!("{base}/copies"), copies, finite_f64);
                self.check_animated(&format!("{base}/offset"), offset, finite_f64);
                self.check_animated(&format!("{base}/start_opacity"), start_opacity, finite_f64);
                self.check_animated(&format!("{base}/end_opacity"), end_opacity, finite_f64);
                self.check_range(&format!("{base}/copies"), copies, 0.0, MAX_REPEATER_COPIES);
                self.check_abs_max(&format!("{base}/offset"), offset, MAX_REPEATER_OFFSET);
                self.check_opacity_range(&format!("{base}/start_opacity"), start_opacity);
                self.check_opacity_range(&format!("{base}/end_opacity"), end_opacity);
                self.check_transform(&format!("{base}/transform"), transform);
            }
            ModifierKind::RoundCorners { radius } => {
                self.check_animated(&format!("{base}/radius"), radius, finite_f64);
                self.check_nonnegative(&format!("{base}/radius"), radius);
            }
            ModifierKind::OffsetPath { amount } => {
                self.check_animated(&format!("{base}/amount"), amount, finite_f64);
                self.check_abs_max(&format!("{base}/amount"), amount, 1_000_000.0);
            }
            ModifierKind::ZigZag {
                amplitude,
                frequency,
                ..
            } => {
                self.check_animated(&format!("{base}/amplitude"), amplitude, finite_f64);
                self.check_animated(&format!("{base}/frequency"), frequency, finite_f64);
                self.check_abs_max(&format!("{base}/amplitude"), amplitude, 1_000_000.0);
                self.check_range(&format!("{base}/frequency"), frequency, 0.0, 1024.0);
            }
            ModifierKind::PuckerBloat { amount } => {
                self.check_animated(&format!("{base}/amount"), amount, finite_f64);
                self.check_abs_max(&format!("{base}/amount"), amount, 1_000_000.0);
            }
        }
    }

    fn check_range(&mut self, path: &str, animated: &Animated<f64>, min: f64, max: f64) {
        if animated.base.is_finite() && !(min..=max).contains(&animated.base) {
            self.err(format!("{path}/base"), "value is outside the allowed range");
        }
        for (index, key) in animated.keyframes.iter().take(MAX_KEYFRAMES).enumerate() {
            if key.value.is_finite() && !(min..=max).contains(&key.value) {
                self.err(
                    format!("{path}/key/{index}"),
                    "value is outside the allowed range",
                );
            }
        }
    }

    fn check_abs_max(&mut self, path: &str, animated: &Animated<f64>, max: f64) {
        if animated.base.is_finite() && animated.base.abs() > max {
            self.err(format!("{path}/base"), "value is outside the allowed range");
        }
        for (index, key) in animated.keyframes.iter().take(MAX_KEYFRAMES).enumerate() {
            if key.value.is_finite() && key.value.abs() > max {
                self.err(
                    format!("{path}/key/{index}"),
                    "value is outside the allowed range",
                );
            }
        }
    }

    fn check_nonnegative(&mut self, path: &str, animated: &Animated<f64>) {
        self.check_range(path, animated, 0.0, f64::MAX);
    }

    fn check_nonnegative_size(&mut self, path: &str, animated: &Animated<DVec2>) {
        if animated.base.is_finite() && (animated.base.x < 0.0 || animated.base.y < 0.0) {
            self.err(format!("{path}/base"), "size must be non-negative");
        }
        for (index, key) in animated.keyframes.iter().take(MAX_KEYFRAMES).enumerate() {
            if key.value.is_finite() && (key.value.x < 0.0 || key.value.y < 0.0) {
                self.err(format!("{path}/key/{index}"), "size must be non-negative");
            }
        }
    }

    fn check_opacity_range(&mut self, path: &str, animated: &Animated<f64>) {
        self.check_range(path, animated, 0.0, 1.0);
    }

    fn check_scale_range(&mut self, path: &str, animated: &Animated<DVec2>) {
        if animated.base.is_finite() && (animated.base.x == 0.0 || animated.base.y == 0.0) {
            self.warn(format!("{path}/base"), "transform scale is degenerate");
        }
        for (index, key) in animated.keyframes.iter().take(MAX_KEYFRAMES).enumerate() {
            if key.value.is_finite() && (key.value.x == 0.0 || key.value.y == 0.0) {
                self.warn(
                    format!("{path}/key/{index}"),
                    "transform scale is degenerate",
                );
            }
        }
    }

    fn check_color_range(&mut self, path: &str, animated: &Animated<Color>) {
        let check = |color: &Color| {
            (0.0..=1.0).contains(&color.r)
                && (0.0..=1.0).contains(&color.g)
                && (0.0..=1.0).contains(&color.b)
                && (0.0..=1.0).contains(&color.a)
        };
        if !check(&animated.base) {
            self.err(format!("{path}/base"), "color channel is outside [0, 1]");
        }
        for (index, key) in animated.keyframes.iter().take(MAX_KEYFRAMES).enumerate() {
            if !check(&key.value) {
                self.err(
                    format!("{path}/key/{index}"),
                    "color channel is outside [0, 1]",
                );
            }
        }
    }

    fn check_path_complexity(&mut self, path: &str, animated: &Animated<VectorPath>) {
        let base_count = animated.base.anchors.len();
        if base_count > MAX_PATH_ANCHORS {
            self.err(format!("{path}/base"), "path has too many anchors");
        }
        self.total_path_anchors = self.total_path_anchors.saturating_add(base_count);
        for (index, key) in animated.keyframes.iter().take(MAX_KEYFRAMES).enumerate() {
            let count = key.value.anchors.len();
            if count > MAX_PATH_ANCHORS {
                self.err(format!("{path}/key/{index}"), "path has too many anchors");
            }
            self.total_path_anchors = self.total_path_anchors.saturating_add(count);
            if self.total_path_anchors > MAX_TOTAL_PATH_ANCHORS {
                self.err("document.paths", "project has too many path anchors");
            }
        }
        if self.total_path_anchors > MAX_TOTAL_PATH_ANCHORS {
            self.err("document.paths", "project has too many path anchors");
        }
    }

    fn check_gradient_stops(&mut self, path: &str, animated: &Animated<GradientStops>) {
        let mut check = |stops: &GradientStops, suffix: &str| {
            if stops.0.len() > MAX_GRADIENT_STOPS {
                self.err(format!("{path}{suffix}"), "gradient has too many stops");
            }
            self.total_gradient_stops = self.total_gradient_stops.saturating_add(stops.0.len());
            if self.total_gradient_stops > MAX_TOTAL_GRADIENT_STOPS {
                self.err("document.gradients", "project has too many gradient stops");
            }
            if stops.0.is_empty() {
                self.warn(format!("{path}{suffix}"), "gradient has no stops");
            }
            let mut previous = 0.0;
            for (index, stop) in stops.0.iter().take(MAX_GRADIENT_STOPS + 1).enumerate() {
                if !stop.offset.is_finite() || !(0.0..=1.0).contains(&stop.offset) {
                    self.err(
                        format!("{path}{suffix}/{index}/offset"),
                        "gradient stop offset must be in [0, 1]",
                    );
                } else if index > 0 && stop.offset < previous {
                    self.err(
                        format!("{path}{suffix}/{index}/offset"),
                        "gradient stop offsets must be ordered",
                    );
                }
                if stop.offset.is_finite() {
                    previous = stop.offset;
                }
                if !finite_color(&stop.color) {
                    self.err(
                        format!("{path}{suffix}/{index}/color"),
                        "gradient stop color is not finite",
                    );
                } else if !(0.0..=1.0).contains(&stop.color.r)
                    || !(0.0..=1.0).contains(&stop.color.g)
                    || !(0.0..=1.0).contains(&stop.color.b)
                    || !(0.0..=1.0).contains(&stop.color.a)
                {
                    self.err(
                        format!("{path}{suffix}/{index}/color"),
                        "gradient stop color is outside [0, 1]",
                    );
                }
            }
        };
        check(&animated.base, "/base");
        for (index, key) in animated.keyframes.iter().take(MAX_KEYFRAMES).enumerate() {
            check(&key.value, &format!("/key/{index}"));
        }
    }

    fn check_gradient_axis(
        &mut self,
        path: &str,
        start: &Animated<DVec2>,
        end: &Animated<DVec2>,
        _radial: bool,
    ) {
        let mut frames = std::collections::BTreeSet::new();
        for key in start.keyframes.iter().chain(end.keyframes.iter()) {
            frames.insert(key.frame);
        }
        let mut samples = Vec::with_capacity(frames.len().saturating_mul(2).saturating_add(1));
        samples.push((start.base, end.base));
        let frames = frames.into_iter().collect::<Vec<_>>();
        for frame in &frames {
            let frame = frame.0 as f64;
            samples.push((start.value_at(frame), end.value_at(frame)));
        }
        for pair in frames.windows(2) {
            let first = pair[0].0 as f64;
            let second = pair[1].0 as f64;
            samples.push((
                start.value_at((first + second) * 0.5),
                end.value_at((first + second) * 0.5),
            ));
        }
        for (a, b) in samples {
            if a.is_finite() && b.is_finite() && a.distance(b) <= 1e-9 {
                self.err(path, "gradient axis has zero length");
                break;
            }
        }
    }

    fn check_transform(&mut self, path: &str, transform: &AnimatedTransform) {
        self.check_animated(&format!("{path}/anchor"), &transform.anchor, finite_vec2);
        self.check_animated(
            &format!("{path}/position"),
            &transform.position,
            finite_vec2,
        );
        self.check_animated(&format!("{path}/scale"), &transform.scale, finite_vec2);
        self.check_scale_range(&format!("{path}/scale"), &transform.scale);
        self.check_animated(
            &format!("{path}/rotation"),
            &transform.rotation,
            finite_angle,
        );
        self.check_animated(&format!("{path}/skew"), &transform.skew, finite_f64);
        self.check_animated(
            &format!("{path}/skew_axis"),
            &transform.skew_axis,
            finite_f64,
        );
    }

    fn check_animated<T>(
        &mut self,
        path: &str,
        animated: &Animated<T>,
        check_value: impl Fn(&T) -> bool,
    ) {
        let keyframe_count = animated.keyframes.len();
        if keyframe_count > MAX_KEYFRAMES {
            self.err(path, "animated property has too many keyframes");
        }
        self.total_keyframes = self.total_keyframes.saturating_add(keyframe_count);
        if self.total_keyframes > MAX_TOTAL_KEYFRAMES {
            self.err("document.animations", "project has too many keyframes");
        }
        if !check_value(&animated.base) {
            self.err(format!("{path}/base"), "value is not finite");
        }
        let mut prev: Option<Frame> = None;
        for (i, key) in animated.keyframes.iter().take(MAX_KEYFRAMES).enumerate() {
            if let Some(p) = prev
                && key.frame <= p
            {
                self.err(
                    format!("{path}/key/{i}"),
                    format!(
                        "keyframes not strictly increasing (duplicate or out of order at frame {})",
                        key.frame.0
                    ),
                );
            }
            if !check_value(&key.value) {
                self.err(format!("{path}/key/{i}"), "keyframe value is not finite");
            }
            if !key.ease_out.x.is_finite()
                || !key.ease_out.y.is_finite()
                || !key.ease_in.x.is_finite()
                || !key.ease_in.y.is_finite()
            {
                self.err(
                    format!("{path}/key/{i}/easing"),
                    "easing handle is not finite",
                );
            } else if key.ease_out.y.abs() > 1_000_000_000.0
                || key.ease_in.y.abs() > 1_000_000_000.0
            {
                self.err(
                    format!("{path}/key/{i}/easing"),
                    "easing handle y is outside the supported range",
                );
            } else if !(0.0..=1.0).contains(&key.ease_out.x)
                || !(0.0..=1.0).contains(&key.ease_in.x)
            {
                self.err(
                    format!("{path}/key/{i}/easing"),
                    "easing handle x must be in [0, 1]",
                );
            }
            prev = Some(key.frame);
        }
    }

    /// Style/modifier scoping mirrors group evaluation: a style paints every
    /// shape path accumulated in its group, and a modifier only affects shapes
    /// seen before it. Warn when either would be a no-op.
    fn validate_scope(&mut self) {
        let doc = &self.file.document;
        let mut visited = HashSet::new();
        let mut pending = doc
            .compositions
            .iter()
            .map(|(comp_id, comp)| {
                (
                    comp.children.clone(),
                    format!("composition/{comp_id:?}"),
                    0usize,
                )
            })
            .collect::<Vec<_>>();
        let mut groups = 0usize;
        while let Some((children, path, depth)) = pending.pop() {
            groups += 1;
            if groups > MAX_TREE_NODES {
                self.err("document.scope", "node scope is too large");
                break;
            }
            if depth > MAX_TREE_DEPTH {
                self.err(&path, "node scope nesting is too deep");
                continue;
            }
            let mut has_shape = false;
            for (index, &id) in children.iter().enumerate() {
                let Some(node) = doc.nodes.get(id) else {
                    continue;
                };
                match &node.kind {
                    NodeKind::Shape(_) | NodeKind::Text(_) => has_shape = true,
                    NodeKind::Modifier(_) if !has_shape => {
                        self.warn(
                            format!("{path}/children/{index}"),
                            "modifier appears before any shape in scope and will have no effect",
                        );
                    }
                    _ => {}
                }
            }

            if !has_shape {
                for (index, &id) in children.iter().enumerate() {
                    let Some(node) = doc.nodes.get(id) else {
                        continue;
                    };
                    if matches!(node.kind, NodeKind::Style(_)) {
                        self.warn(
                            format!("{path}/children/{index}"),
                            "style node is not paired with any shape in scope",
                        );
                    }
                }
            }

            for &id in &children {
                let Some(node) = doc.nodes.get(id) else {
                    continue;
                };
                if matches!(node.kind, NodeKind::Group | NodeKind::Layer(_)) && visited.insert(id) {
                    pending.push((
                        node.children.clone(),
                        format!("{path}/node/{id:?}"),
                        depth + 1,
                    ));
                }
            }
        }
    }

    fn validate_precomps(&mut self) {
        let doc = &self.file.document;

        for (id, node) in doc.nodes.iter().take(MAX_TREE_NODES) {
            if let NodeKind::Precomp { comp, time_map } = &node.kind {
                if !doc.compositions.contains_key(*comp) {
                    self.err(
                        format!("node/{id:?}/precomp"),
                        "referenced composition does not exist",
                    );
                }
                if !time_map.stretch.is_finite() || time_map.stretch <= 0.0 {
                    self.err(
                        format!("node/{id:?}/precomp/stretch"),
                        "invalid time stretch",
                    );
                }
            }
            if let NodeKind::Use { target } = &node.kind {
                let Some(source) = doc.nodes.get(*target) else {
                    self.err(format!("node/{id:?}/use"), "referenced node does not exist");
                    continue;
                };
                if matches!(
                    &source.kind,
                    NodeKind::Style(_) | NodeKind::Modifier(_) | NodeKind::Mask(_)
                ) {
                    self.err(
                        format!("node/{id:?}/use"),
                        "use target must be a shape, group, or layer",
                    );
                }
                if bounded_node_ancestors(doc, id).is_none()
                    || bounded_node_ancestors(doc, id).is_some_and(|chain| chain.contains(target))
                    || node_descends_from(doc, *target, id).unwrap_or(true)
                {
                    self.err(
                        format!("node/{id:?}/use"),
                        "use containment creates a cycle",
                    );
                }
                let mut current = *target;
                let mut seen = HashSet::new();
                let mut hops = 0usize;
                loop {
                    if current == id {
                        self.err(
                            format!("node/{id:?}/use"),
                            "use node reference chain contains a cycle",
                        );
                        break;
                    }
                    if !seen.insert(current) {
                        self.err(
                            format!("node/{id:?}/use"),
                            "use node reference chain contains a cycle",
                        );
                        break;
                    }
                    hops += 1;
                    if hops > MAX_TREE_DEPTH {
                        self.err(
                            format!("node/{id:?}/use"),
                            "use node reference chain is too deep",
                        );
                        break;
                    }
                    let Some(next) = self.file.document.nodes.get(current) else {
                        self.err(
                            format!("node/{id:?}/use"),
                            "use reference chain references a missing node",
                        );
                        break;
                    };
                    let NodeKind::Use {
                        target: next_target,
                    } = &next.kind
                    else {
                        if matches!(
                            &next.kind,
                            NodeKind::Style(_) | NodeKind::Modifier(_) | NodeKind::Mask(_)
                        ) {
                            self.err(
                                format!("node/{id:?}/use"),
                                "use target must resolve to renderable content",
                            );
                        }
                        break;
                    };
                    current = *next_target;
                }
            }
        }

        let mut on_stack = HashSet::new();
        let mut visited = HashSet::new();
        let mut pending = doc
            .compositions
            .keys()
            .map(|comp| CompEvent::Enter(comp, 0))
            .collect::<Vec<_>>();
        while let Some(event) = pending.pop() {
            match event {
                CompEvent::Leave(comp) => {
                    on_stack.remove(&comp);
                }
                CompEvent::Enter(comp, depth) => {
                    if depth > MAX_PRECOMP_DEPTH {
                        self.err(
                            format!("precomp/{comp:?}"),
                            "precomposition nesting is too deep",
                        );
                        continue;
                    }
                    if on_stack.contains(&comp) {
                        self.err(
                            format!("precomp/{comp:?}"),
                            "composition is reachable from itself through precomps (cycle)",
                        );
                        continue;
                    }
                    if !visited.insert(comp) {
                        continue;
                    }
                    on_stack.insert(comp);
                    pending.push(CompEvent::Leave(comp));
                    let Some(c) = doc.compositions.get(comp) else {
                        continue;
                    };
                    let mut stack = c.children.clone();
                    let mut seen_nodes = HashSet::new();
                    let mut node_count = 0usize;
                    while let Some(nid) = stack.pop() {
                        node_count += 1;
                        if node_count > MAX_TREE_NODES {
                            self.err(
                                format!("precomp/{comp:?}"),
                                "precomposition contains too many nodes",
                            );
                            break;
                        }
                        if !seen_nodes.insert(nid) {
                            continue;
                        }
                        let Some(node) = doc.nodes.get(nid) else {
                            continue;
                        };
                        if let NodeKind::Precomp { comp: target, .. } = &node.kind {
                            pending.push(CompEvent::Enter(*target, depth + 1));
                        }
                        stack.extend(node.children.iter().copied());
                    }
                }
            }
        }
    }

    fn validate_clips(&mut self) {
        let doc = &self.file.document;

        if self.file.clips.len() > MAX_TREE_NODES || self.file.clip_order.len() > MAX_TREE_NODES {
            self.err("clips", "project has too many clips");
        }
        let mut seen = HashSet::new();
        for (i, &id) in self.file.clip_order.iter().enumerate() {
            if !self.file.clips.contains_key(id) {
                self.err(format!("clips/order/{i}"), "clip id does not exist");
            }
            if !seen.insert(id) {
                self.err(
                    format!("clips/order/{i}"),
                    "duplicate clip id in clip_order",
                );
            }
        }

        for (clip_id, clip) in self.file.clips.iter().take(MAX_TREE_NODES) {
            if clip.range.1 <= clip.range.0 {
                self.err(format!("clip/{clip_id:?}/range"), "invalid clip range");
            }
            if clip.tracks.len() > MAX_TREE_NODES || clip.events.len() > MAX_TREE_NODES {
                self.err(
                    format!("clip/{clip_id:?}"),
                    "clip has too many tracks or events",
                );
            }
            self.total_clip_tracks = self.total_clip_tracks.saturating_add(clip.tracks.len());
            if self.total_clip_tracks > MAX_TOTAL_CLIP_TRACKS {
                self.err("clips", "project has too many clip tracks");
            }
            if clip.events.len() > 1 {
                let mut previous = None;
                for (index, event) in clip.events.iter().enumerate() {
                    if let Some(previous) = previous
                        && event.frame <= previous
                    {
                        self.warn(
                            format!("clip/{clip_id:?}/event/{index}"),
                            "clip events are not strictly ordered",
                        );
                    }
                    previous = Some(event.frame);
                }
            }

            for (track_index, track) in clip.tracks.iter().enumerate() {
                let track_path = format!("clip/{clip_id:?}/track/{track_index}");
                let prop = match doc.nodes.get(track.node) {
                    Some(node) => match node.prop_ref(&track.prop) {
                        Some(prop) => prop,
                        None => {
                            self.err(
                                format!("{track_path}/prop"),
                                "track references missing or incompatible property",
                            );
                            continue;
                        }
                    },
                    None => {
                        self.err(
                            format!("{track_path}/node"),
                            "track references missing node",
                        );
                        continue;
                    }
                };

                if track.keys.len() > MAX_KEYFRAMES {
                    self.err(format!("{track_path}/keys"), "clip has too many keyframes");
                }
                self.total_clip_keys = self.total_clip_keys.saturating_add(track.keys.len());
                if self.total_clip_keys > MAX_TOTAL_CLIP_KEYS {
                    self.err("clips", "project has too many clip keyframes");
                }
                let mut prev: Option<Frame> = None;
                for (key_index, key) in track.keys.iter().take(MAX_KEYFRAMES).enumerate() {
                    if let Some(p) = prev
                        && key.frame <= p
                    {
                        self.err(
                            format!("{track_path}/key/{key_index}"),
                            "clip keyframes not strictly increasing (duplicate or out of order)",
                        );
                    }
                    if !key_value_matches_prop(&key.value, &prop) {
                        self.err(
                            format!("{track_path}/key/{key_index}/value"),
                            "keyframe value type does not match property",
                        );
                    } else if !value_is_finite(&key.value) {
                        self.err(
                            format!("{track_path}/key/{key_index}/value"),
                            "keyframe value is not finite",
                        );
                    }
                    if !key.ease_out.x.is_finite()
                        || !key.ease_out.y.is_finite()
                        || !key.ease_in.x.is_finite()
                        || !key.ease_in.y.is_finite()
                        || key.ease_out.y.abs() > 1_000_000_000.0
                        || key.ease_in.y.abs() > 1_000_000_000.0
                        || !(0.0..=1.0).contains(&key.ease_out.x)
                        || !(0.0..=1.0).contains(&key.ease_in.x)
                    {
                        self.err(
                            format!("{track_path}/key/{key_index}/easing"),
                            "clip keyframe easing is invalid",
                        );
                    }
                    prev = Some(key.frame);
                }
            }
        }
    }

    fn validate_machines(&mut self) {
        let doc = &self.file.document;

        if self.file.machines.len() > MAX_TREE_NODES
            || self.file.machine_order.len() > MAX_TREE_NODES
        {
            self.err("machines", "project has too many machines");
        }
        if let Some(start) = self.file.start_machine {
            if !self.file.machines.contains_key(start) {
                self.err("start_machine", "start machine does not exist");
            }
            if !self.file.machine_order.contains(&start) {
                self.warn(
                    "start_machine",
                    "start machine exists but is detached from machine_order",
                );
            }
        }

        let mut seen = HashSet::new();
        for (i, &id) in self.file.machine_order.iter().enumerate() {
            if !self.file.machines.contains_key(id) {
                self.err(format!("machines/order/{i}"), "machine id does not exist");
            }
            if !seen.insert(id) {
                self.err(
                    format!("machines/order/{i}"),
                    "duplicate machine id in machine_order",
                );
            }
        }

        for (machine_id, machine) in self.file.machines.iter().take(MAX_TREE_NODES) {
            let machine_elements = machine
                .inputs
                .len()
                .saturating_add(machine.listeners.len())
                .saturating_add(
                    machine
                        .layers
                        .iter()
                        .map(|layer| {
                            1usize
                                .saturating_add(layer.states.len())
                                .saturating_add(layer.any_transitions.len())
                                .saturating_add(
                                    layer
                                        .states
                                        .iter()
                                        .map(|state| {
                                            1usize
                                                .saturating_add(state.transitions.len())
                                                .saturating_add(
                                                    state
                                                        .transitions
                                                        .iter()
                                                        .map(|transition| {
                                                            transition.conditions.len()
                                                        })
                                                        .sum::<usize>(),
                                                )
                                        })
                                        .sum::<usize>(),
                                )
                        })
                        .sum::<usize>(),
                );
            self.total_machine_elements =
                self.total_machine_elements.saturating_add(machine_elements);
            if self.total_machine_elements > MAX_TOTAL_MACHINE_ELEMENTS {
                self.err("machines", "project has too many machine elements");
            }
            if machine.layers.len() > MAX_TREE_NODES
                || machine.inputs.len() > MAX_TREE_NODES
                || machine.listeners.len() > MAX_TREE_NODES
            {
                self.err(
                    format!("machine/{machine_id:?}"),
                    "machine contains too many elements",
                );
            }
            for (layer_index, layer) in machine.layers.iter().enumerate() {
                if layer.states.is_empty() {
                    self.err(
                        format!("machine/{machine_id:?}/layer/{layer_index}"),
                        "layer has no states",
                    );
                    continue;
                }

                if layer.entry >= layer.states.len() {
                    self.err(
                        format!("machine/{machine_id:?}/layer/{layer_index}/entry"),
                        "entry state index is out of range",
                    );
                }

                for (state_index, state) in layer.states.iter().enumerate() {
                    match &state.kind {
                        StateKind::Clip { clip, speed, .. } => {
                            if !self.file.clips.contains_key(*clip) {
                                self.err(
                                    format!("machine/{machine_id:?}/layer/{layer_index}/state/{state_index}/clip"),
                                    "state references missing clip",
                                );
                            }
                            if !speed.is_finite() || *speed < 0.0 {
                                self.err(
                                    format!("machine/{machine_id:?}/layer/{layer_index}/state/{state_index}/speed"),
                                    "clip state speed must be non-negative and finite",
                                );
                            }
                        }
                        StateKind::Blend1D { input, children } => {
                            let base = format!(
                                "machine/{machine_id:?}/layer/{layer_index}/state/{state_index}/blend"
                            );
                            match machine.inputs.get(*input) {
                                Some(input_def) => {
                                    if !matches!(input_def.kind, InputKind::Number { .. }) {
                                        self.err(
                                            format!("{base}/input"),
                                            "Blend1D input must be a number input",
                                        );
                                    }
                                }
                                None => self.err(
                                    format!("{base}/input"),
                                    "Blend1D input index is out of range",
                                ),
                            }
                            if children.is_empty() {
                                self.err(format!("{base}/children"), "Blend1D has no children");
                            }
                            if children.len() > MAX_TREE_NODES {
                                self.err(
                                    format!("{base}/children"),
                                    "Blend1D has too many children",
                                );
                            }
                            let mut prev: Option<f64> = None;
                            for (child_index, child) in
                                children.iter().take(MAX_TREE_NODES).enumerate()
                            {
                                if !self.file.clips.contains_key(child.clip) {
                                    self.err(
                                        format!("{base}/child/{child_index}"),
                                        "blend child references missing clip",
                                    );
                                }
                                if !child.threshold.is_finite() {
                                    self.err(
                                        format!("{base}/child/{child_index}/threshold"),
                                        "blend threshold must be finite",
                                    );
                                }
                                if let Some(p) = prev
                                    && child.threshold <= p
                                {
                                    self.warn(
                                        format!("{base}/child/{child_index}/threshold"),
                                        "blend thresholds are not strictly increasing",
                                    );
                                }
                                prev = Some(child.threshold);
                            }
                        }
                        StateKind::Empty => {}
                    }

                    self.validate_transitions(
                        machine_id,
                        machine,
                        layer_index,
                        Some(state_index),
                        &state.transitions,
                    );
                }

                self.validate_transitions(
                    machine_id,
                    machine,
                    layer_index,
                    None,
                    &layer.any_transitions,
                );
            }

            for (listener_index, listener) in machine.listeners.iter().enumerate() {
                if !doc.nodes.contains_key(listener.node) {
                    self.err(
                        format!("machine/{machine_id:?}/listener/{listener_index}/node"),
                        "listener references missing node",
                    );
                }

                let input = listener_action_input(&listener.action);
                let base = format!("machine/{machine_id:?}/listener/{listener_index}");
                match machine.inputs.get(input) {
                    Some(input_def) => {
                        if !listener_matches_input(&listener.action, input_def.kind) {
                            self.err(
                                format!("{base}/input"),
                                "listener action type does not match input type",
                            );
                        }
                    }
                    None => self.err(format!("{base}/input"), "listener references missing input"),
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_transitions(
        &mut self,
        machine_id: MachineId,
        machine: &Machine,
        layer_index: usize,
        state_index: Option<usize>,
        transitions: &[Transition],
    ) {
        let Some(layer) = machine.layers.get(layer_index) else {
            return;
        };

        if transitions.len() > MAX_TREE_NODES {
            self.err(
                format!("machine/{machine_id:?}/layer/{layer_index}"),
                "state machine has too many transitions",
            );
        }
        for (transition_index, transition) in transitions.iter().take(MAX_TREE_NODES).enumerate() {
            let base = match state_index {
                Some(s) => format!(
                    "machine/{machine_id:?}/layer/{layer_index}/state/{s}/transition/{transition_index}"
                ),
                None => format!(
                    "machine/{machine_id:?}/layer/{layer_index}/any_transition/{transition_index}"
                ),
            };

            if transition.to >= layer.states.len() {
                self.err(&base, "transition target state is out of range");
            }
            if !transition.duration.is_finite() || transition.duration < 0.0 {
                self.err(&base, "transition duration must be non-negative and finite");
            }
            if let Some(exit_time) = transition.exit_time
                && (!exit_time.is_finite() || !(0.0..=1.0).contains(&exit_time))
            {
                self.err(&base, "transition exit_time must be in [0, 1]");
            }

            if transition.conditions.len() > MAX_TREE_NODES {
                self.err(&base, "transition has too many conditions");
            }
            for (condition_index, condition) in transition
                .conditions
                .iter()
                .take(MAX_TREE_NODES)
                .enumerate()
            {
                let input = condition_input(condition);
                let condition_path = format!("{base}/condition/{condition_index}");
                match machine.inputs.get(input) {
                    Some(input_def) => {
                        if !condition_matches_input(condition, input_def.kind) {
                            self.err(&condition_path, "condition type does not match input type");
                        }
                    }
                    None => self.err(&condition_path, "condition references missing input"),
                }
            }
        }
    }

    fn validate_export_readiness(&mut self) {
        let doc = &self.file.document;

        let direct: HashSet<NodeId> = doc
            .compositions
            .values()
            .flat_map(|c| c.children.iter().copied())
            .collect();
        let mut image_exportable = direct.clone();
        for id in &direct {
            if let Some(node) = doc.nodes.get(*id)
                && matches!(node.kind, NodeKind::Group | NodeKind::Layer(_))
            {
                image_exportable.extend(node.children.iter().copied());
            }
        }

        for (id, node) in doc.nodes.iter().take(MAX_TREE_NODES) {
            match &node.kind {
                NodeKind::Text(text) => {
                    self.warn(
                        format!("node/{id:?}/text"),
                        "Lottie export bakes text to vector outlines",
                    );
                    if !text.size.keyframes.is_empty() {
                        self.warn(
                            format!("node/{id:?}/text"),
                            "animated `text.size` bakes to its base value on Lottie export",
                        );
                    }
                    if !text.tracking.keyframes.is_empty() || !text.leading.keyframes.is_empty() {
                        self.warn(
                            format!("node/{id:?}/text"),
                            "animated `text.tracking`/`text.leading` bake to base on Lottie export",
                        );
                    }
                }
                NodeKind::Mask(_) => {
                    self.warn(
                        format!("node/{id:?}/mask"),
                        "Lottie mask export is best-effort and may differ from Renamite clip-stack semantics",
                    );
                }
                NodeKind::Image(img) => {
                    if doc.image_asset(img.asset()).is_none() {
                        self.err(
                            format!("node/{id:?}/image"),
                            "image layer references missing image asset",
                        );
                    }
                    if !image_exportable.contains(&id) {
                        self.warn(
                            format!("node/{id:?}/image"),
                            "deeply nested image layer is skipped by Lottie export (hoist to a top-level Layer/Group child)",
                        );
                    }
                    if img.tint().base != Color::WHITE || !img.tint().keyframes.is_empty() {
                        self.warn(
                            format!("node/{id:?}/image"),
                            "image tint is dropped by Lottie/SVG export",
                        );
                    }
                    let crop = img.crop();
                    if crop.x.abs() > 1e-9
                        || crop.y.abs() > 1e-9
                        || (crop.z - 1.0).abs() > 1e-6
                        || (crop.w - 1.0).abs() > 1e-6
                    {
                        self.warn(
                            format!("node/{id:?}/image"),
                            "image crop is approximated by GPU/SVG/Lottie sinks (full texture fitted into cropped rect)",
                        );
                    }
                }
                NodeKind::Precomp { .. } if !direct.contains(&id) => {
                    self.warn(
                        format!("node/{id:?}/precomp"),
                        "nested precomp is skipped by Lottie export (hoist to a top-level child)",
                    );
                }
                NodeKind::Use { .. } => {
                    self.warn(
                        format!("node/{id:?}/use"),
                        "use node bakes to a copy on Lottie export",
                    );
                }
                _ => {}
            }
        }
    }
}

fn bounded_node_ancestors(doc: &Document, start: NodeId) -> Option<Vec<NodeId>> {
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let mut current = start;
    for _ in 0..=MAX_TREE_DEPTH {
        if !seen.insert(current) || !doc.nodes.contains_key(current) {
            return None;
        }
        chain.push(current);
        let Some(parent) = doc.nodes.get(current).and_then(|node| node.parent) else {
            return Some(chain);
        };
        current = parent;
    }
    None
}

fn node_descends_from(doc: &Document, ancestor: NodeId, start: NodeId) -> Option<bool> {
    let mut pending = vec![start];
    let mut seen = HashSet::new();
    for _ in 0..=MAX_TREE_DEPTH {
        let Some(current) = pending.pop() else {
            return Some(false);
        };
        if current == ancestor {
            return Some(true);
        }
        if !seen.insert(current) {
            continue;
        }
        let node = doc.nodes.get(current)?;
        pending.extend(node.children.iter().copied());
    }
    None
}

fn condition_input(condition: &Condition) -> usize {
    match condition {
        Condition::BoolIs { input, .. }
        | Condition::NumberCmp { input, .. }
        | Condition::Triggered { input } => *input,
    }
}

fn condition_matches_input(condition: &Condition, input: InputKind) -> bool {
    matches!(
        (condition, input),
        (Condition::BoolIs { .. }, InputKind::Bool { .. })
            | (Condition::NumberCmp { .. }, InputKind::Number { .. })
            | (Condition::Triggered { .. }, InputKind::Trigger)
    )
}

fn listener_action_input(action: &ListenerAction) -> usize {
    match action {
        ListenerAction::SetBool { input, .. }
        | ListenerAction::ToggleBool { input }
        | ListenerAction::SetNumber { input, .. }
        | ListenerAction::FireTrigger { input } => *input,
    }
}

fn listener_matches_input(action: &ListenerAction, input: InputKind) -> bool {
    matches!(
        (action, input),
        (ListenerAction::SetBool { .. }, InputKind::Bool { .. })
            | (ListenerAction::ToggleBool { .. }, InputKind::Bool { .. })
            | (ListenerAction::SetNumber { .. }, InputKind::Number { .. })
            | (ListenerAction::FireTrigger { .. }, InputKind::Trigger)
    )
}

fn value_is_finite(value: &Value) -> bool {
    match value {
        Value::F64(value) => finite_f64(value),
        Value::DVec2(value) => finite_vec2(value),
        Value::Angle(value) => finite_angle(value),
        Value::Color(value) => finite_color(value),
        Value::Path(value) => finite_path(value),
        Value::Stops(value) => finite_stops(value),
        Value::Paint(value) => match value {
            StylePaint::Solid { color } => {
                finite_color(&color.base)
                    && color
                        .keyframes
                        .iter()
                        .take(MAX_KEYFRAMES)
                        .all(|key| finite_color(&key.value))
            }
            StylePaint::Gradient(gradient) => {
                finite_vec2(&gradient.start.base)
                    && finite_vec2(&gradient.end.base)
                    && finite_stops(&gradient.stops.base)
                    && gradient
                        .start
                        .keyframes
                        .iter()
                        .take(MAX_KEYFRAMES)
                        .all(|key| finite_vec2(&key.value))
                    && gradient
                        .end
                        .keyframes
                        .iter()
                        .take(MAX_KEYFRAMES)
                        .all(|key| finite_vec2(&key.value))
                    && gradient
                        .stops
                        .keyframes
                        .iter()
                        .take(MAX_KEYFRAMES)
                        .all(|key| finite_stops(&key.value))
            }
        },
        Value::Bool(_) | Value::I64(_) => true,
    }
}

fn key_value_matches_prop(value: &Value, prop: &PropRef) -> bool {
    matches!(
        (value, prop),
        (Value::F64(_), PropRef::F64(_))
            | (Value::DVec2(_), PropRef::Vec2(_))
            | (Value::Angle(_), PropRef::Angle(_))
            | (Value::Color(_), PropRef::Color(_))
            | (Value::Path(_), PropRef::Path(_))
            | (Value::Stops(_), PropRef::Stops(_))
    )
}

fn finite_f64(value: &f64) -> bool {
    value.is_finite() && value.abs() <= 1_000_000_000.0
}

fn finite_vec2(value: &DVec2) -> bool {
    value.is_finite() && value.abs().max_element() <= 1_000_000_000.0
}

fn finite_angle(value: &Angle) -> bool {
    value.0.is_finite() && value.0.abs() <= 1_000_000_000.0
}

fn finite_color(color: &Color) -> bool {
    color.r.is_finite() && color.g.is_finite() && color.b.is_finite() && color.a.is_finite()
}

fn finite_stops(stops: &GradientStops) -> bool {
    if stops.0.len() > MAX_GRADIENT_STOPS {
        return false;
    }
    let mut previous = 0.0;
    stops.0.iter().all(|stop| {
        let valid = stop.offset.is_finite()
            && (0.0..=1.0).contains(&stop.offset)
            && stop.offset >= previous
            && finite_color(&stop.color);
        if valid {
            previous = stop.offset;
        }
        valid
    })
}

fn finite_path(path: &VectorPath) -> bool {
    path.anchors.len() <= MAX_PATH_ANCHORS
        && path.anchors.iter().all(|a| {
            a.pos.is_finite()
                && a.tan_in.is_finite()
                && a.tan_out.is_finite()
                && a.pos.abs().max_element() <= 1_000_000_000.0
                && a.tan_in.abs().max_element() <= 1_000_000_000.0
                && a.tan_out.abs().max_element() <= 1_000_000_000.0
        })
}

fn transform_is_default(t: &AnimatedTransform) -> bool {
    t.anchor.base == DVec2::ZERO
        && t.position.base == DVec2::ZERO
        && t.anchor.keyframes.is_empty()
        && t.position.keyframes.is_empty()
        && t.rotation.base.0 == 0.0
        && t.rotation.keyframes.is_empty()
        && t.skew.base == 0.0
        && t.skew.keyframes.is_empty()
        && t.skew_axis.base == 0.0
        && t.skew_axis.keyframes.is_empty()
        && scale_is_default(&t.scale)
}

fn scale_is_default(scale: &Animated<DVec2>) -> bool {
    scale.base == DVec2::splat(100.0) && scale.keyframes.is_empty()
}

fn opacity_is_default(opacity: &Animated<f64>) -> bool {
    opacity.base == 1.0 && opacity.keyframes.is_empty()
}

/// Base-value heuristic for "this shape has no geometry": empty path, zero-size
/// rect/ellipse, or non-positive star/polygon radius.
fn shape_kind_is_empty(shape: &ShapeKind) -> bool {
    match shape {
        ShapeKind::Path(path) => path.base.anchors.is_empty(),
        ShapeKind::CompoundPath(compound) => compound.contours.is_empty(),
        ShapeKind::Rect { size, .. } | ShapeKind::Ellipse { size, .. } => {
            size.base.x == 0.0 || size.base.y == 0.0
        }
        ShapeKind::Star { outer_r, .. } | ShapeKind::Polygon { outer_r, .. } => outer_r.base <= 0.0,
    }
}
