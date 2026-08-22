//! Project validation and diagnostics for renamite.
//!
//! Deterministic checks over a [`RenFile`]: document tree integrity,
//! asset references, animation keyframe hygiene, clip/machine sanity, and
//! export-readiness warnings. Use [`validate`] to produce a
//! [`ValidationReport`]; [`ValidationReport::has_errors`] tells you whether the
//! project is safe to save/render/export.

use glam::DVec2;
use renamite_animation::{Angle, Animated, AnimatedTransform, Frame};
use renamite_geometry::VectorPath;
use renamite_io_ren::RenFile;
use renamite_machine::{
    Condition, InputKind, ListenerAction, Machine, MachineId, StateKind, Transition,
};
use renamite_model::{
    Asset, Color, CompId, Document, GradientStops, ModifierKind, Node, NodeId, NodeKind, PropRef,
    ShapeKind, StyleKind, StylePaint, Value,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

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
    };
    v.run();
    v.report
}

struct Validator<'a> {
    file: &'a RenFile,
    report: ValidationReport,
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
        self.report.push(Diagnostic::error(path, message));
    }

    fn warn(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.report.push(Diagnostic::warning(path, message));
    }

    fn validate_compositions(&mut self) {
        let doc = &self.file.document;

        if !doc.compositions.contains_key(doc.main) {
            self.err("document.main", "main composition does not exist");
        }

        for (id, comp) in &doc.compositions {
            if comp.rate.num == 0 || comp.rate.den == 0 {
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
            }
            for (index, child) in comp.children.iter().enumerate() {
                if !doc.nodes.contains_key(*child) {
                    self.err(
                        format!("composition/{id:?}/children/{index}"),
                        "child node does not exist",
                    );
                }
            }
        }
    }

    fn validate_document_tree(&mut self) {
        let doc = &self.file.document;
        let mut seen = HashSet::new();

        for (comp_id, comp) in &doc.compositions {
            for &root in &comp.children {
                self.walk_node_tree(
                    root,
                    format!("composition/{comp_id:?}"),
                    &mut seen,
                    Vec::new(),
                );
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
    }

    fn walk_node_tree(
        &mut self,
        id: NodeId,
        path: String,
        seen: &mut HashSet<NodeId>,
        mut stack: Vec<NodeId>,
    ) {
        if stack.contains(&id) {
            self.err(format!("{path}/node/{id:?}"), "cycle in node tree");
            return;
        }

        let Some(node) = self.file.document.nodes.get(id) else {
            self.err(path, format!("node {id:?} does not exist"));
            return;
        };

        seen.insert(id);
        stack.push(id);

        for (index, &child) in node.children.iter().enumerate() {
            match self.file.document.nodes.get(child) {
                Some(child_node) => {
                    if child_node.parent != Some(id) {
                        self.err(
                            format!("node/{id:?}/children/{index}"),
                            "child parent pointer does not point back to this node",
                        );
                    }
                    self.walk_node_tree(
                        child,
                        format!("node/{id:?}/children/{index}"),
                        seen,
                        stack.clone(),
                    );
                }
                None => {
                    self.err(
                        format!("node/{id:?}/children/{index}"),
                        "child node does not exist",
                    );
                }
            }
        }
    }

    fn validate_assets(&mut self) {
        let doc = &self.file.document;

        let mut seen = HashSet::new();
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

        for (id, node) in &doc.nodes {
            match &node.kind {
                NodeKind::Image(asset) => match doc.assets.get(*asset) {
                    Some(Asset::Image(img)) => {
                        if img.width == 0 || img.height == 0 {
                            self.err(
                                format!("node/{id:?}/image"),
                                "image dimensions must be nonzero",
                            );
                        }
                        if img.bytes.is_empty() {
                            self.err(format!("node/{id:?}/image"), "image asset has no bytes");
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

        for (id, asset) in &doc.assets {
            match asset {
                Asset::Image(img) => {
                    if img.width == 0 || img.height == 0 {
                        self.err(format!("asset/{id:?}"), "image dimensions must be nonzero");
                    }
                    if img.bytes.is_empty() {
                        self.warn(format!("asset/{id:?}"), "image asset has empty bytes");
                    }
                }
                Asset::Font(font) => {
                    if font.bytes.is_empty() {
                        self.err(format!("asset/{id:?}"), "font has no bytes");
                    }
                    if font.family.trim().is_empty() {
                        self.err(format!("asset/{id:?}"), "font family is empty");
                    }
                }
            }
        }

        self.validate_asset_usage(doc);
    }

    fn validate_asset_usage(&mut self, doc: &Document) {
        for (id, asset) in &doc.assets {
            match asset {
                Asset::Image(_) => {
                    let used = doc
                        .nodes
                        .values()
                        .any(|n| matches!(n.kind, NodeKind::Image(a) if a == id));
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
        for (id, node) in &doc.nodes {
            self.validate_node_animations(id, node);
        }
    }

    fn validate_node_animations(&mut self, id: NodeId, node: &Node) {
        let base = format!("node/{id:?}");
        self.check_transform(&format!("{base}/transform"), &node.transform);
        self.check_animated(&format!("{base}/opacity"), &node.opacity, finite_f64);

        if node.transform.scale.base == DVec2::ZERO {
            self.warn(format!("{base}/transform/scale"), "transform scale is zero");
        }

        match &node.kind {
            NodeKind::Shape(shape) => self.validate_shape_animations(id, shape),
            NodeKind::Style(style) => self.validate_style_animations(id, style),
            NodeKind::Modifier(modifier) => self.validate_modifier_animations(id, modifier),
            NodeKind::Text(text) => {
                self.check_animated(&format!("{base}/text/size"), &text.size, finite_f64);
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
            NodeKind::Group | NodeKind::Image(_) | NodeKind::Precomp { .. } => {}
        }
    }

    fn validate_shape_animations(&mut self, id: NodeId, shape: &ShapeKind) {
        let base = format!("node/{id:?}/shape");
        match shape {
            ShapeKind::Path(path) => {
                self.check_animated(&format!("{base}/path"), path, finite_path);
            }
            ShapeKind::Rect { pos, size, rounded } => {
                self.check_animated(&format!("{base}/pos"), pos, finite_vec2);
                self.check_animated(&format!("{base}/size"), size, finite_vec2);
                self.check_animated(&format!("{base}/rounded"), rounded, finite_f64);
            }
            ShapeKind::Ellipse { pos, size } => {
                self.check_animated(&format!("{base}/pos"), pos, finite_vec2);
                self.check_animated(&format!("{base}/size"), size, finite_vec2);
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
            }
            ShapeKind::CompoundPath(compound) => {
                for (i, contour) in compound.contours.iter().enumerate() {
                    self.check_animated(&format!("{base}/contour/{i}"), contour, finite_path);
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
                paint, width, dash, ..
            } => {
                self.validate_paint(&format!("{base}/paint"), paint);
                self.check_animated(&format!("{base}/width"), width, finite_f64);
                if let Some(dash) = dash {
                    for (i, d) in dash.dashes.iter().enumerate() {
                        self.check_animated(&format!("{base}/dash/{i}"), d, finite_f64);
                    }
                    self.check_animated(&format!("{base}/dash/offset"), &dash.offset, finite_f64);
                }
            }
        }
    }

    fn validate_paint(&mut self, path: &str, paint: &StylePaint) {
        match paint {
            StylePaint::Solid { color } => self.check_animated(path, color, finite_color),
            StylePaint::Gradient(gradient) => {
                self.check_animated(&format!("{path}/start"), &gradient.start, finite_vec2);
                self.check_animated(&format!("{path}/end"), &gradient.end, finite_vec2);
                self.check_animated(&format!("{path}/stops"), &gradient.stops, finite_stops);
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
                self.check_transform(&format!("{base}/transform"), transform);
            }
            ModifierKind::RoundCorners { radius } => {
                self.check_animated(&format!("{base}/radius"), radius, finite_f64);
            }
            ModifierKind::OffsetPath { amount } => {
                self.check_animated(&format!("{base}/amount"), amount, finite_f64);
            }
            ModifierKind::ZigZag {
                amplitude,
                frequency,
                ..
            } => {
                self.check_animated(&format!("{base}/amplitude"), amplitude, finite_f64);
                self.check_animated(&format!("{base}/frequency"), frequency, finite_f64);
            }
            ModifierKind::PuckerBloat { amount } => {
                self.check_animated(&format!("{base}/amount"), amount, finite_f64);
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
        if !check_value(&animated.base) {
            self.err(format!("{path}/base"), "value is not finite");
        }
        let mut prev: Option<Frame> = None;
        for (i, key) in animated.keyframes.iter().enumerate() {
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
        for (comp_id, comp) in &doc.compositions {
            self.scope_group(
                comp.children.to_vec(),
                format!("composition/{comp_id:?}"),
                &mut visited,
            );
        }
    }

    fn scope_group(&mut self, children: Vec<NodeId>, path: String, visited: &mut HashSet<NodeId>) {
        let doc = &self.file.document;
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
                self.scope_group(
                    node.children.clone(),
                    format!("{path}/node/{id:?}"),
                    visited,
                );
            }
        }
    }

    fn validate_precomps(&mut self) {
        let doc = &self.file.document;

        for (id, node) in &doc.nodes {
            if let NodeKind::Precomp { comp, time_map } = &node.kind {
                if !doc.compositions.contains_key(*comp) {
                    self.err(
                        format!("node/{id:?}/precomp"),
                        "referenced composition does not exist",
                    );
                }
                if !time_map.stretch.is_finite() || time_map.stretch.abs() < 1e-9 {
                    self.err(
                        format!("node/{id:?}/precomp/stretch"),
                        "invalid time stretch",
                    );
                }
            }
        }

        let mut on_stack = HashSet::new();
        let mut visited = HashSet::new();
        for comp in doc.compositions.keys() {
            self.walk_precomp(comp, &mut on_stack, &mut visited);
        }
    }

    fn walk_precomp(
        &mut self,
        comp: CompId,
        on_stack: &mut HashSet<CompId>,
        visited: &mut HashSet<CompId>,
    ) {
        if on_stack.contains(&comp) {
            self.err(
                format!("precomp/{comp:?}"),
                "composition is reachable from itself through precomps (cycle)",
            );
            return;
        }
        if !visited.insert(comp) {
            return;
        }
        on_stack.insert(comp);
        if let Some(c) = self.file.document.compositions.get(comp) {
            for &child in &c.children {
                if let Some(node) = self.file.document.nodes.get(child)
                    && let NodeKind::Precomp { comp: target, .. } = &node.kind
                {
                    self.walk_precomp(*target, on_stack, visited);
                }
            }
        }
        on_stack.remove(&comp);
    }

    fn validate_clips(&mut self) {
        let doc = &self.file.document;

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

        for (clip_id, clip) in &self.file.clips {
            if clip.range.1 <= clip.range.0 {
                self.err(format!("clip/{clip_id:?}/range"), "invalid clip range");
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

                let mut prev: Option<Frame> = None;
                for (key_index, key) in track.keys.iter().enumerate() {
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
                    }
                    prev = Some(key.frame);
                }
            }
        }
    }

    fn validate_machines(&mut self) {
        let doc = &self.file.document;

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

        for (machine_id, machine) in &self.file.machines {
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
                            let mut prev: Option<f64> = None;
                            for (child_index, child) in children.iter().enumerate() {
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

        for (transition_index, transition) in transitions.iter().enumerate() {
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

            for (condition_index, condition) in transition.conditions.iter().enumerate() {
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

        // Nodes attached directly to a composition are Lottie layers; anything
        // nested inside a group/layer is a shape item and only some kinds make
        // it through that path.
        let direct: HashSet<NodeId> = doc
            .compositions
            .values()
            .flat_map(|c| c.children.iter().copied())
            .collect();

        for (id, node) in &doc.nodes {
            match &node.kind {
                NodeKind::Text(_) => {
                    self.warn(
                        format!("node/{id:?}/text"),
                        "Lottie export bakes text to vector outlines",
                    );
                }
                NodeKind::Mask(_) => {
                    self.warn(
                        format!("node/{id:?}/mask"),
                        "Lottie mask export is best-effort and may differ from Renamite clip-stack semantics",
                    );
                }
                NodeKind::Image(asset) => {
                    if doc.image_asset(*asset).is_none() {
                        self.err(
                            format!("node/{id:?}/image"),
                            "image layer references missing image asset",
                        );
                    }
                    if !direct.contains(&id) {
                        self.warn(
                            format!("node/{id:?}/image"),
                            "nested image layer is skipped by Lottie export",
                        );
                    }
                }
                NodeKind::Precomp { .. } if !direct.contains(&id) => {
                    self.warn(
                        format!("node/{id:?}/precomp"),
                        "nested precomp is skipped by Lottie export",
                    );
                }
                _ => {}
            }
        }
    }
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
    value.is_finite()
}

fn finite_vec2(value: &DVec2) -> bool {
    value.is_finite()
}

fn finite_angle(value: &Angle) -> bool {
    value.0.is_finite()
}

fn finite_color(color: &Color) -> bool {
    color.r.is_finite() && color.g.is_finite() && color.b.is_finite() && color.a.is_finite()
}

fn finite_stops(stops: &GradientStops) -> bool {
    stops
        .0
        .iter()
        .all(|s| s.offset.is_finite() && finite_color(&s.color))
}

fn finite_path(path: &VectorPath) -> bool {
    path.anchors
        .iter()
        .all(|a| a.pos.is_finite() && a.tan_in.is_finite() && a.tan_out.is_finite())
}

/// Base-value heuristic for "this shape has no geometry": empty path, zero-size
/// rect/ellipse, or non-positive star/polygon radius.
fn shape_kind_is_empty(shape: &ShapeKind) -> bool {
    match shape {
        ShapeKind::Path(path) => path.base.anchors.is_empty(),
        ShapeKind::CompoundPath(compound) => compound.contours.is_empty(),
        ShapeKind::Rect { size, .. } | ShapeKind::Ellipse { size, .. } => {
            size.base.x == 0.0 && size.base.y == 0.0
        }
        ShapeKind::Star { outer_r, .. } | ShapeKind::Polygon { outer_r, .. } => outer_r.base <= 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use renamite_animation::{EasingHandle, Interpolation, Keyframe};
    use renamite_model::{AssetId, Parent, TextAlign, TextNode};

    fn file(name: &str) -> RenFile {
        RenFile::new(Document::empty(), name)
    }

    #[test]
    fn empty_project_is_valid() {
        let report = validate(&file("empty"));
        assert_eq!(report.error_count(), 0);
        assert_eq!(report.warning_count(), 0);
    }

    #[test]
    fn missing_image_asset_is_error() {
        let mut file = file("bad");
        let fake = AssetId::from(slotmap::KeyData::from_ffi(42));
        let node = file
            .document
            .create_node(Node::new("img", NodeKind::Image(fake)));
        file.document
            .attach(node, Parent::Comp(file.document.main), 0)
            .unwrap();

        let report = validate(&file);
        assert!(report.has_errors());
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.message.contains("image asset"))
        );
    }

    #[test]
    fn missing_font_family_is_warning() {
        let mut file = file("font");
        let node = file.document.create_node(Node::new(
            "t",
            NodeKind::Text(TextNode {
                text: String::new(),
                size: Animated::new(48.0),
                align: TextAlign::Left,
                font: Some("Missing".into()),
            }),
        ));
        file.document
            .attach(node, Parent::Comp(file.document.main), 0)
            .unwrap();

        let report = validate(&file);
        assert_eq!(report.error_count(), 0);
        assert!(report.warning_count() > 0);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.message.contains("font family"))
        );
    }

    #[test]
    fn duplicate_animated_keyframes_are_error() {
        use renamite_model::NodeKind;
        let mut file = file("keys");
        let mut node = Node::new("n", NodeKind::Group);
        node.opacity = Animated {
            base: 1.0,
            keyframes: vec![
                Keyframe {
                    frame: Frame(0),
                    value: 1.0,
                    interpolation: Interpolation::Linear,
                    ease_out: EasingHandle::LINEAR_OUT,
                    ease_in: EasingHandle::LINEAR_IN,
                },
                Keyframe {
                    frame: Frame(0),
                    value: 0.5,
                    interpolation: Interpolation::Linear,
                    ease_out: EasingHandle::LINEAR_OUT,
                    ease_in: EasingHandle::LINEAR_IN,
                },
            ],
        };
        let id = file.document.create_node(node);
        file.document
            .attach(id, Parent::Comp(file.document.main), 0)
            .unwrap();

        let report = validate(&file);
        assert!(report.has_errors());
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.message.contains("strictly increasing"))
        );
    }

    #[test]
    fn machine_bad_transition_is_error() {
        use renamite_machine::{Machine, MachineLayer, State};
        let mut file = file("machine");
        file.machines.insert(Machine {
            name: "m".into(),
            inputs: vec![],
            layers: vec![MachineLayer {
                name: "base".into(),
                entry: 0,
                any_transitions: vec![],
                states: vec![State {
                    name: "s".into(),
                    kind: StateKind::Empty,
                    transitions: vec![Transition {
                        to: 99,
                        duration: 0.0,
                        exit_time: None,
                        conditions: vec![],
                    }],
                }],
            }],
            listeners: vec![],
        });

        let report = validate(&file);
        assert!(report.has_errors());
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.path.contains("transition"))
        );
    }

    #[test]
    fn empty_blend1d_children_are_error() {
        use renamite_machine::{Machine, MachineLayer, State};
        let mut file = file("blend");
        file.machines.insert(Machine {
            name: "m".into(),
            inputs: vec![renamite_machine::InputDef {
                name: "n".into(),
                kind: InputKind::Number { default: 0.0 },
            }],
            layers: vec![MachineLayer {
                name: "base".into(),
                entry: 0,
                any_transitions: vec![],
                states: vec![State {
                    name: "s".into(),
                    kind: StateKind::Blend1D {
                        input: 0,
                        children: vec![],
                    },
                    transitions: vec![],
                }],
            }],
            listeners: vec![],
        });

        let report = validate(&file);
        assert!(report.has_errors());
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.message.contains("Blend1D has no children"))
        );
    }

    #[test]
    fn style_without_shape_in_scope_warns() {
        use renamite_model::{FillRule, StylePaint};
        let mut file = file("style");
        let fill = file.document.create_node(Node::new(
            "Fill",
            NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::solid(Color::WHITE),
                rule: FillRule::NonZero,
            }),
        ));
        file.document
            .attach(fill, Parent::Comp(file.document.main), 0)
            .unwrap();

        let report = validate(&file);
        assert_eq!(report.error_count(), 0);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.message.contains("not paired with any shape"))
        );
    }

    #[test]
    fn precomp_cycle_is_error() {
        let mut file = file("cycle");
        let comp_a = file.document.main;
        let comp_b = file
            .document
            .compositions
            .insert(renamite_model::Composition {
                name: "B".into(),
                size: (512, 512),
                rate: renamite_animation::FrameRate { num: 60, den: 1 },
                range: (Frame(0), Frame(60)),
                children: vec![],
            });
        let node_a = file.document.create_node(Node::new(
            "to B",
            NodeKind::Precomp {
                comp: comp_b,
                time_map: renamite_model::TimeMap {
                    offset: Frame(0),
                    stretch: 1.0,
                },
            },
        ));
        file.document
            .attach(node_a, Parent::Comp(comp_a), 0)
            .unwrap();
        let node_b = file.document.create_node(Node::new(
            "to A",
            NodeKind::Precomp {
                comp: comp_a,
                time_map: renamite_model::TimeMap {
                    offset: Frame(0),
                    stretch: 1.0,
                },
            },
        ));
        file.document
            .attach(node_b, Parent::Comp(comp_b), 0)
            .unwrap();

        let report = validate(&file);
        assert!(report.has_errors());
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.message.contains("cycle"))
        );
    }

    #[test]
    fn clip_track_missing_node_is_error() {
        use renamite_machine::Clip;
        let mut file = file("clip");
        let missing = NodeId::from(slotmap::KeyData::from_ffi(7));
        file.clips.insert(Clip {
            name: "c".into(),
            range: (Frame(0), Frame(10)),
            tracks: vec![renamite_machine::Track {
                node: missing,
                prop: renamite_model::PropPath::new("opacity"),
                keys: vec![],
            }],
            events: vec![],
        });

        let report = validate(&file);
        assert!(report.has_errors());
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.message.contains("missing node"))
        );
    }
}
