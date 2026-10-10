//! The headless document session: one `RenFile` in memory plus the tool
//! operations an agent drives it with.

use std::path::{Path, PathBuf};

use renamite_animation::Animated;
use renamite_io_ren::RenFile;
use renamite_model::{
    Color, CompId, Document, FillRule, Node, NodeId, NodeKind, Parent, ShapeKind, StrokeCap,
    StrokeJoin, StyleKind, StylePaint,
};
use serde_json::{Value, json};

/// Hard cap on file reads, matching the CLI.
const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;

pub struct Session {
    file: RenFile,
    path: Option<PathBuf>,
    dirty: bool,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub fn new() -> Self {
        Self {
            file: RenFile::new(Document::empty(), "untitled"),
            path: None,
            dirty: false,
        }
    }

    pub fn document(&self) -> &Document {
        &self.file.document
    }

    pub fn main_comp(&self) -> Option<CompId> {
        Some(self.file.document.main)
    }

    pub fn composition_size(&self) -> (u32, u32) {
        self.file
            .document
            .compositions
            .get(self.file.document.main)
            .map(|comp| comp.size)
            .unwrap_or((0, 0))
    }

    pub fn mark_clean(&mut self) {
        self.dirty = false;
    }

    // -- ids ---------------------------------------------------------------

    /// Node ids are 0-based indices into the active composition's subtree in
    /// document order. Recomputed per call, so read ids back after edits.
    pub fn node_ids(&self) -> Vec<NodeId> {
        let mut ids = Vec::new();
        let Some(comp) = self.file.document.compositions.get(self.file.document.main) else {
            return ids;
        };
        let mut pending: Vec<NodeId> = comp.children.iter().rev().copied().collect();
        while let Some(id) = pending.pop() {
            let Some(node) = self.file.document.nodes.get(id) else {
                continue;
            };
            for child in node.children.iter().rev() {
                pending.push(*child);
            }
            ids.push(id);
        }
        ids
    }

    fn ids_of(&self, params: &Value) -> Result<Vec<NodeId>, String> {
        let all = self.node_ids();
        let requested: Vec<usize> = match params.get("ids") {
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| {
                    item.as_u64()
                        .map(|value| value as usize)
                        .ok_or_else(|| "ids must be integers".to_string())
                })
                .collect::<Result<_, _>>()?,
            _ => (0..all.len()).collect(),
        };
        requested
            .into_iter()
            .map(|index| {
                all.get(index)
                    .copied()
                    .ok_or_else(|| format!("no node id {index} in the active composition"))
            })
            .collect()
    }

    // -- project lifecycle -------------------------------------------------

    pub fn project_new(&mut self, name: Option<&str>, width: u32, height: u32) -> Result<Value, String> {
        let mut document = Document::empty();
        let comp = document
            .compositions
            .get_mut(document.main)
            .ok_or("a new document has no composition")?;
        comp.name = name.unwrap_or("Comp 1").to_string();
        comp.size = (width.max(1), height.max(1));
        self.file = RenFile::new(document, name.unwrap_or("untitled"));
        self.path = None;
        self.dirty = true;
        Ok(json!({"ok": true, "size": [width.max(1), height.max(1)]}))
    }

    pub fn project_open(&mut self, path: &Path) -> Result<Value, String> {
        let bytes = read_limited(path)?;
        let source = String::from_utf8(bytes)
            .map_err(|_| format!("{} is not a .ren text document", path.display()))?;
        self.file = renamite_io_ren::open(&source)
            .map_err(|error| format!("{} did not parse: {error}", path.display()))?;
        self.path = Some(path.to_path_buf());
        self.dirty = false;
        Ok(json!({"ok": true, "path": path.display().to_string()}))
    }

    pub fn project_save(&mut self, path: Option<&Path>) -> Result<Value, String> {
        let target = match path {
            Some(path) => path.to_path_buf(),
            None => self
                .path
                .clone()
                .ok_or("no path given and the document was never saved")?,
        };
        let binary = target
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("renb"));
        let bytes = if binary {
            renamite_io_ren::save_binary(&self.file).map_err(|error| error.to_string())?
        } else {
            renamite_io_ren::save(&self.file)
                .map_err(|error| error.to_string())?
                .into_bytes()
        };
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        std::fs::write(&target, bytes)
            .map_err(|error| format!("{}: {error}", target.display()))?;
        self.path = Some(target.clone());
        self.dirty = false;
        Ok(json!({"ok": true, "path": target.display().to_string(), "binary": binary}))
    }

    pub fn project_info(&self) -> Value {
        let comp = self
            .file
            .document
            .compositions
            .get(self.file.document.main);
        let nodes: Vec<Value> = self
            .node_ids()
            .iter()
            .enumerate()
            .filter_map(|(index, id)| {
                let node = self.file.document.nodes.get(*id)?;
                let parent = node
                    .parent
                    .and_then(|parent| self.node_ids().iter().position(|id| *id == parent));
                let (kind, detail) = match &node.kind {
                    NodeKind::Shape(shape) => {
                        let (name, pos, size) = match shape {
                            ShapeKind::Ellipse { pos, size } | ShapeKind::Rect { pos, size, .. } => (
                                if matches!(shape, ShapeKind::Ellipse { .. }) {
                                    "ellipse"
                                } else {
                                    "rect"
                                },
                                pos.value_at(0.0),
                                size.value_at(0.0),
                            ),
                            _ => ("shape", glam::DVec2::ZERO, glam::DVec2::ZERO),
                        };
                        let _ = rounded_of(shape);
                        (
                            name.to_string(),
                            json!({"pos": [pos.x, pos.y], "size": [size.x, size.y]}),
                        )
                    }
                    NodeKind::Style(style) => {
                        let (paint, width) = match style {
                            StyleKind::Fill { paint, .. } => (paint, None),
                            StyleKind::Stroke { paint, width, .. } => (paint, Some(width.value_at(0.0))),
                        };
                        let color = match paint {
                            StylePaint::Solid { color } => Some(color.value_at(0.0)),
                            StylePaint::Gradient(_) => None,
                        };
                        (
                            "style".to_string(),
                            json!({
                                "role": match style { StyleKind::Fill { .. } => "fill", _ => "stroke" },
                                "color": color.map(hex_color),
                                "width": width,
                            }),
                        )
                    }
                    NodeKind::Group => ("group".to_string(), json!({})),
                    NodeKind::Layer(_) => ("layer".to_string(), json!({})),
                    NodeKind::Modifier(_) => ("modifier".to_string(), json!({})),
                    NodeKind::Text(_) => ("text".to_string(), json!({})),
                    NodeKind::Image(_) => ("image".to_string(), json!({})),
                    NodeKind::Precomp { .. } => ("precomp".to_string(), json!({})),
                    NodeKind::Use { .. } => ("use".to_string(), json!({})),
                    NodeKind::Mask(_) => ("mask".to_string(), json!({})),
                };
                Some(json!({
                    "id": index,
                    "name": node.name,
                    "kind": kind,
                    "visible": node.visible,
                    "parent": parent,
                    "detail": detail,
                }))
            })
            .collect();
        json!({
            "name": self.file.document.compositions.get(self.file.document.main).map(|c| c.name.clone()).unwrap_or_default(),
            "size": comp.map(|c| c.size).unwrap_or((0, 0)),
            "dirty": self.dirty,
            "path": self.path.as_ref().map(|p| p.display().to_string()),
            "nodes": nodes,
        })
    }

    // -- drawing -----------------------------------------------------------

    pub fn draw_shape(&mut self, params: &Value) -> Result<Value, String> {
        let shape = params
            .get("shape")
            .and_then(Value::as_str)
            .unwrap_or("ellipse");
        let x = number(params, "x")?;
        let y = number(params, "y")?;
        let width = number(params, "width")?.max(0.0);
        let height = number(params, "height")?.max(0.0);
        let kind = match shape {
            "ellipse" => ShapeKind::Ellipse {
                pos: Animated::new(glam::DVec2::new(x, y)),
                size: Animated::new(glam::DVec2::new(width, height)),
            },
            "rect" => ShapeKind::Rect {
                pos: Animated::new(glam::DVec2::new(x, y)),
                size: Animated::new(glam::DVec2::new(width, height)),
                rounded: Animated::new(number(params, "radius").unwrap_or(0.0).max(0.0)),
            },
            other => return Err(format!("unknown shape {other}: ellipse or rect")),
        };
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(shape)
            .to_string();
        let shape_id = self.push_node(Node::new(name, NodeKind::Shape(kind)))?;
        let fill = parse_color(params.get("fill"));
        let stroke = parse_color(params.get("stroke"));
        let stroke_width = number(params, "strokeWidth").unwrap_or(0.0).max(0.0);
        if let Some(color) = fill {
            let style = fill_node(color, params.get("fillRule").and_then(Value::as_str));
            self.push_child(shape_id, style)?;
        }
        if let Some(color) = stroke {
            let style = stroke_node(color, if stroke_width > 0.0 { stroke_width } else { 1.0 });
            self.push_child(shape_id, style)?;
        }
        self.dirty = true;
        Ok(json!({"ok": true, "id": self.index_of(shape_id), "name": shape}))
    }

    pub fn set_paint(&mut self, params: &Value) -> Result<Value, String> {
        let ids = self.ids_of(params)?;
        let fill = parse_color(params.get("fill"));
        let stroke = parse_color(params.get("stroke"));
        let stroke_width = number(params, "strokeWidth");
        let mut touched = Vec::new();
        for id in ids {
            let children: Vec<NodeId> = self
                .file
                .document
                .nodes
                .get(id)
                .map(|node| node.children.clone())
                .unwrap_or_default();
            for child in children {
                let Some(node) = self.file.document.nodes.get_mut(child) else {
                    continue;
                };
                match (&mut node.kind, &fill, &stroke, stroke_width.as_ref().ok().copied()) {
                    (NodeKind::Style(StyleKind::Fill { .. }), Some(color), _, _) => {
                        node.kind = NodeKind::Style(StyleKind::Fill {
                            paint: StylePaint::solid(*color),
                            rule: FillRule::NonZero,
                        });
                        touched.push(self.index_of(child));
                    }
                    (NodeKind::Style(StyleKind::Stroke { .. }), _, Some(color), width) => {
                        node.kind = NodeKind::Style(StyleKind::Stroke {
                            paint: StylePaint::solid(*color),
                            width: Animated::new(width.unwrap_or(1.0).max(0.1)),
                            cap: StrokeCap::Round,
                            join: StrokeJoin::Round,
                            dash: None,
                            miter_limit: Animated::new(4.0),
                            profile: None,
                        });
                        touched.push(self.index_of(child));
                    }
                    (NodeKind::Style(StyleKind::Stroke { width, .. }), _, None, Some(next)) => {
                        *width = Animated::new(next.max(0.1));
                        touched.push(self.index_of(child));
                    }
                    _ => {}
                }
            }
        }
        self.dirty = true;
        Ok(json!({"ok": true, "touched": touched}))
    }

    pub fn transform(&mut self, params: &Value) -> Result<Value, String> {
        let ids = self.ids_of(params)?;
        let x = number(params, "x").ok();
        let y = number(params, "y").ok();
        let width = number(params, "width").ok();
        let height = number(params, "height").ok();
        let rotation = number(params, "rotation").ok();
        let scale_x = number(params, "scaleX").ok();
        let scale_y = number(params, "scaleY").ok();
        let mut touched = Vec::new();
        for id in ids {
            let Some(node) = self.file.document.nodes.get_mut(id) else {
                continue;
            };
            match &mut node.kind {
                NodeKind::Shape(shape) => {
                    let (pos, size) = match shape {
                        ShapeKind::Ellipse { pos, size }
                        | ShapeKind::Rect { pos, size, .. } => (pos, size),
                        _ => continue,
                    };
                    let current_pos = pos.value_at(0.0);
                    let current_size = size.value_at(0.0);
                    let next_size = glam::DVec2::new(
                        width.unwrap_or(current_size.x).max(0.0),
                        height.unwrap_or(current_size.y).max(0.0),
                    );
                    let next_pos = glam::DVec2::new(
                        x.unwrap_or(current_pos.x),
                        y.unwrap_or(current_pos.y),
                    );
                    pos.base = next_pos;
                    size.base = next_size;
                    if let Some(degrees) = rotation {
                        node.transform.rotation.base =
                            renamite_animation::Angle(degrees.to_radians());
                    }
                    if let (Some(sx), Some(sy)) = (scale_x, scale_y) {
                        node.transform.scale.base = glam::DVec2::new(sx, sy);
                    } else if let Some(scale) = scale_x.or(scale_y) {
                        node.transform.scale.base = glam::DVec2::splat(scale);
                    }
                    touched.push(self.index_of(id));
                }
                _ => continue,
            }
        }
        self.dirty = true;
        Ok(json!({"ok": true, "touched": touched}))
    }

    pub fn delete(&mut self, params: &Value) -> Result<Value, String> {
        let ids = self.ids_of(params)?;
        let deleted = ids.len();
        for id in ids {
            let _ = self.file.document.detach(id);
        }
        self.dirty = true;
        Ok(json!({"ok": true, "deleted": deleted}))
    }

    // -- output ------------------------------------------------------------

    pub fn render_png(&mut self, params: &Value) -> Result<Value, String> {
        let comp = self.composition_size();
        let scale = number(params, "scale").unwrap_or(1.0).max(0.01);
        let width = (comp.0 as f64 * scale).round().max(1.0) as u32;
        let height = (comp.1 as f64 * scale).round().max(1.0) as u32;
        let source = renamite_io_ren::save(&self.file).map_err(|error| error.to_string())?;
        let mut player = renamite_player::Player::new(renamite_io_ren::open(&source).map_err(|e| e.to_string())?)
            .map_err(|error| format!("failed to open player: {error}"))?;
        player.scrub(0.0);
        let view = renamite_behavior_common::ViewTransform {
            scale,
            offset: glam::DVec2::new(
                (width as f64 - comp.0 as f64 * scale) * 0.5,
                (height as f64 - comp.1 as f64 * scale) * 0.5,
            ),
        };
        let mut bridge = renamite_render_bridge::SceneRenderer::new();
        let mut gpu = pollster::block_on(renamite_render_offscreen::OffscreenRenderer::new(
            width, height, 4,
        ))
        .map_err(|error| format!("offscreen renderer: {error}"))?;
        gpu.sync_document_images(&player.project.document)
            .map_err(|error| format!("image upload: {error}"))?;
        let prepared = bridge.prepare(player.scene(), &view);
        let mut repose = repose_core::Scene::default();
        bridge.append_repose_scene(&prepared, &mut repose);
        let png = gpu
            .render_png(&repose, None)
            .map_err(|error| format!("render: {error}"))?;
        match params.get("path").and_then(Value::as_str) {
            Some(path) => {
                std::fs::write(path, &png)
                    .map_err(|error| format!("{path}: {error}"))?;
                Ok(json!({"ok": true, "path": path, "width": width, "height": height}))
            }
            None => {
                Ok(json!({
                    "ok": true,
                    "width": width,
                    "height": height,
                    "png_base64": base64_encode(&png),
                }))
            }
        }
    }

    pub fn validate(&self) -> Result<Value, String> {
        let report = renamite_validate::validate(&self.file);
        let messages: Vec<Value> = report
            .diagnostics
            .iter()
            .map(|diagnostic| {
                json!({
                    "severity": format!("{:?}", diagnostic.severity).to_lowercase(),
                    "path": diagnostic.path,
                    "message": diagnostic.message,
                })
            })
            .collect();
        Ok(json!({
            "ok": !report.has_errors(),
            "errors": report.error_count(),
            "warnings": report.warning_count(),
            "diagnostics": messages,
        }))
    }

    pub fn import_svg(&mut self, params: &Value) -> Result<Value, String> {
        let input = Path::new(
            params
                .get("input")
                .and_then(Value::as_str)
                .ok_or("import_svg needs an input path")?,
        );
        let bytes = read_limited(input)?;
        let report = renamite_io_svg::import_with_report(&bytes)
            .map_err(|error| format!("{}: {error}", input.display()))?;
        let document = report.value;
        let node_count = document.nodes.len();
        self.file = RenFile::new(document, "imported svg");
        self.path = None;
        self.dirty = true;
        Ok(json!({
            "ok": true,
            "nodes": node_count,
            "warnings": report.warnings.iter().map(|w| json!({"path": w.path, "message": w.message})).collect::<Vec<_>>(),
        }))
    }

    pub fn export_svg(&mut self, params: &Value) -> Result<Value, String> {
        let path = params
            .get("path")
            .and_then(Value::as_str)
            .ok_or("export_svg needs an output path")?;
        let frame = number(params, "frame").unwrap_or(0.0);
        let report = renamite_io_svg::export_with_report(
            &self.file.document,
            self.file.document.main,
            frame,
        )
        .map_err(|error| error.to_string())?;
        std::fs::write(path, report.value.as_bytes())
            .map_err(|error| format!("{path}: {error}"))?;
        Ok(json!({"ok": true, "path": path}))
    }

    // -- helpers -----------------------------------------------------------

    /// Shapes stack index 0 on top, so a new shape attaches at the front: the
    /// draw order then reads the same way it does in SVG, where later paints
    /// over earlier.
    fn push_node(&mut self, node: Node) -> Result<NodeId, String> {
        let id = self.file.document.create_node(node);
        let main = self.file.document.main;
        self.file
            .document
            .attach(id, Parent::Comp(main), 0)
            .map_err(|error| format!("node did not attach: {error}"))?;
        Ok(id)
    }

    fn push_child(&mut self, parent: NodeId, node: Node) -> Result<NodeId, String> {
        let id = self.file.document.create_node(node);
        self.file
            .document
            .attach(id, Parent::Node(parent), usize::MAX)
            .map_err(|error| format!("style did not attach: {error}"))?;
        Ok(id)
    }

    fn index_of(&self, id: NodeId) -> Option<usize> {
        self.node_ids().iter().position(|candidate| *candidate == id)
    }
}

fn rounded_of(shape: &ShapeKind) -> f64 {
    match shape {
        ShapeKind::Rect { rounded, .. } => rounded.value_at(0.0),
        _ => 0.0,
    }
}

fn fill_node(color: Color, rule: Option<&str>) -> Node {
    Node::new(
        "Fill",
        NodeKind::Style(StyleKind::Fill {
            paint: StylePaint::solid(color),
            rule: match rule {
                Some("evenOdd") => FillRule::EvenOdd,
                _ => FillRule::NonZero,
            },
        }),
    )
}

fn stroke_node(color: Color, width: f64) -> Node {
    Node::new(
        "Stroke",
        NodeKind::Style(StyleKind::Stroke {
            paint: StylePaint::solid(color),
            width: Animated::new(width),
            cap: StrokeCap::Round,
            join: StrokeJoin::Round,
            dash: None,
            miter_limit: Animated::new(4.0),
            profile: None,
        }),
    )
}

fn number(params: &Value, key: &str) -> Result<f64, String> {
    params
        .get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("missing number parameter {key}"))
}

fn parse_color(value: Option<&Value>) -> Option<Color> {
    let value = value?;
    let text = value.as_str()?;
    if text == "none" {
        return None;
    }
    let hex = text.trim_start_matches('#');
    if hex.len() != 6 && hex.len() != 8 {
        return None;
    }
    if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |index: usize| {
        u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16)
            .map(|value| f64::from(value) / 255.0)
            .unwrap_or(0.0)
    };
    let alpha = if hex.len() == 8 { channel(3) } else { 1.0 };
    Some(Color::rgba(channel(0), channel(1), channel(2), alpha))
}

fn hex_color(color: Color) -> String {
    let channel = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        channel(color.r),
        channel(color.g),
        channel(color.b)
    )
}

fn read_limited(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_INPUT_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(bytes)
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(b2 & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}
