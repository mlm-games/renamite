//! The headless document session: one `RenFile` in memory plus the tool
//! operations an agent drives it with.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use renamite_animation::{Angle, Animated, EasingHandle, Frame, Interpolation, LoopMode};
use renamite_io_ren::RenFile;
use renamite_machine::{
    Clip, ClipId, CmpOp, Condition, InputDef, InputKind, Machine, MachineId, MachineLayer, State,
    StateKind, Track, Transition,
};
use renamite_model::{
    Color, CompId, Document, FillRule, KeyframeData, Node, NodeId, NodeKind, Parent, PropPath,
    ShapeKind, StrokeCap, StrokeJoin, StyleKind, StylePaint, Value as ModelValue,
};
use renamite_player::Player;
use serde_json::{Value, json};

/// Hard cap on file reads, matching the CLI.
const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;

pub struct Session {
    file: RenFile,
    player: Player,
    /// Stable per-session handles. Enumeration indices shift the moment a
    /// node is added or removed, so every id the tools hand out is a handle
    /// assigned on first sight and kept for the session's life.
    handles: HandleTable,
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
        let file = RenFile::new(Document::empty(), "untitled");
        let player = Player::new(file.clone()).expect("an empty document opens");
        Self {
            file,
            player,
            handles: HandleTable::default(),
            path: None,
            dirty: false,
        }
    }

    /// The player owns machine state and the playhead, so every edit rebuilds
    /// it from the file. Authoring and then previewing is the loop; live
    /// tweaking of a playing rig is not a v2 goal.
    fn refresh_player(&mut self) {
        self.player = Player::new(self.file.clone()).expect("the session file opens");
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

    /// Refresh handles against the live tree: new nodes get one, removed
    /// nodes lose theirs.
    pub fn sync_handles(&mut self) {
        let live = self.node_ids();
        for id in &live {
            if !self.handles.nodes.contains_key(id) {
                let handle = format!("n{}", self.handles.next);
                self.handles.next += 1;
                self.handles.nodes.insert(*id, handle);
                self.handles.order.push(*id);
            }
        }
        let keep: HashSet<NodeId> = live.into_iter().collect();
        self.handles.order.retain(|id| keep.contains(id));
    }

    fn handle_of(&mut self, id: NodeId) -> Value {
        self.sync_handles();
        match self.handles.nodes.get(&id) {
            Some(handle) => json!(handle),
            None => json!(null),
        }
    }

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

    fn ids_of(&mut self, params: &Value) -> Result<Vec<NodeId>, String> {
        self.sync_handles();
        match params.get("ids") {
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| {
                    self.handles
                        .resolve(item)
                        .ok_or_else(|| "unknown node handle".to_string())
                })
                .collect(),
            _ => Ok(self.handles.live().into_iter().map(|(_, id)| id).collect()),
        }
    }

    // -- project lifecycle -------------------------------------------------

    pub fn project_new(
        &mut self,
        name: Option<&str>,
        width: u32,
        height: u32,
    ) -> Result<Value, String> {
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
        self.refresh_player();
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
        self.refresh_player();
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
        std::fs::write(&target, bytes).map_err(|error| format!("{}: {error}", target.display()))?;
        self.path = Some(target.clone());
        self.dirty = false;
        Ok(json!({"ok": true, "path": target.display().to_string(), "binary": binary}))
    }

    pub fn project_info(&mut self) -> Value {
        self.sync_handles();
        let ids = self.node_ids();
        let comp = self
            .file
            .document
            .compositions
            .get(self.file.document.main)
            .map(|c| (c.name.clone(), c.size));
        let mut nodes: Vec<Value> = Vec::new();
        for id in &ids {
            let Some(node) = self.file.document.nodes.get(*id) else {
                continue;
            };
            let name = node.name.clone();
            let visible = node.visible;
            let (kind, detail) = {
                let (kind, detail) = match &node.kind {
                    NodeKind::Shape(shape) => {
                        let (name, pos, size) = match shape {
                            ShapeKind::Ellipse { pos, size }
                            | ShapeKind::Rect { pos, size, .. } => (
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
                        (
                            name.to_string(),
                            json!({"pos": [pos.x, pos.y], "size": [size.x, size.y]}),
                        )
                    }
                    NodeKind::Style(style) => {
                        let (paint, width) = match style {
                            StyleKind::Fill { paint, .. } => (paint, None),
                            StyleKind::Stroke { paint, width, .. } => {
                                (paint, Some(width.value_at(0.0)))
                            }
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
                (kind, detail)
            };
            nodes.push(json!({
                "handle": self.handle_of(*id),
                "name": name,
                "kind": kind,
                "visible": visible,
                "detail": detail,
            }));
        }
        json!({
            "name": comp.as_ref().map(|(name, _)| name.clone()).unwrap_or_default(),
            "size": comp.map(|(_, size)| size).unwrap_or((0, 0)),
            "dirty": self.dirty,
            "path": self.path.as_ref().map(|p| p.display().to_string()),
            "nodes": nodes,
        })
    }

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
            let style = stroke_node(
                color,
                if stroke_width > 0.0 {
                    stroke_width
                } else {
                    1.0
                },
            );
            self.push_child(shape_id, style)?;
        }
        self.dirty = true;
        self.refresh_player();
        Ok(json!({"ok": true, "handle": self.handle_of(shape_id), "name": shape}))
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
                match (
                    &mut node.kind,
                    &fill,
                    &stroke,
                    stroke_width.as_ref().ok().copied(),
                ) {
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
        self.refresh_player();
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
                        ShapeKind::Ellipse { pos, size } | ShapeKind::Rect { pos, size, .. } => {
                            (pos, size)
                        }
                        _ => continue,
                    };
                    let current_pos = pos.value_at(0.0);
                    let current_size = size.value_at(0.0);
                    let next_size = glam::DVec2::new(
                        width.unwrap_or(current_size.x).max(0.0),
                        height.unwrap_or(current_size.y).max(0.0),
                    );
                    let next_pos =
                        glam::DVec2::new(x.unwrap_or(current_pos.x), y.unwrap_or(current_pos.y));
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
        self.refresh_player();
        Ok(json!({"ok": true, "touched": touched}))
    }

    pub fn delete(&mut self, params: &Value) -> Result<Value, String> {
        let ids = self.ids_of(params)?;
        let deleted = ids.len();
        for id in ids {
            let _ = self.file.document.detach(id);
        }
        self.dirty = true;
        self.refresh_player();
        Ok(json!({"ok": true, "deleted": deleted}))
    }

    // -- output ------------------------------------------------------------

    pub fn render_png(&mut self, params: &Value) -> Result<Value, String> {
        let comp = self.composition_size();
        let scale = number(params, "scale").unwrap_or(1.0).max(0.01);
        let width = (comp.0 as f64 * scale).round().max(1.0) as u32;
        let height = (comp.1 as f64 * scale).round().max(1.0) as u32;
        // The session player carries machine state and the playhead, so a
        // playback or input_set call before this render applies here.
        match params.get("frame").and_then(Value::as_f64) {
            Some(frame) => self.seek(frame),
            None => {
                let _ = self.player.tick(1.0 / 60.0);
            }
        }
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
        gpu.sync_document_images(&self.player.project.document)
            .map_err(|error| format!("image upload: {error}"))?;
        let prepared = bridge.prepare(self.player.scene(), &view);
        let mut repose = repose_core::Scene::default();
        bridge.append_repose_scene(&prepared, &mut repose);
        let png = gpu
            .render_png(&repose, None)
            .map_err(|error| format!("render: {error}"))?;
        match params.get("path").and_then(Value::as_str) {
            Some(path) => {
                std::fs::write(path, &png).map_err(|error| format!("{path}: {error}"))?;
                Ok(json!({"ok": true, "path": path, "width": width, "height": height}))
            }
            None => Ok(json!({
                "ok": true,
                "width": width,
                "height": height,
                "png_base64": base64_encode(&png),
            })),
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
        self.refresh_player();
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
        self.node_ids()
            .iter()
            .position(|candidate| *candidate == id)
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

impl Session {
    // Timeline, clips, machines and playback: v2 of the authoring loop.

    /// Set one keyframe on a node property, or its static value when no frame
    /// is given. `value` is a number, [x, y], "#rrggbb", a bool, or degrees for
    /// rotation properties, and must match the property's existing type.
    pub fn timeline_set(&mut self, params: &Value) -> Result<Value, String> {
        let id = self.node_of(params)?;
        let property = params
            .get("property")
            .and_then(Value::as_str)
            .ok_or("timeline_set needs a property name")?
            .to_string();
        let prop = PropPath::new(property.clone());
        let like = self.file.document.get_static(id, &prop).ok();
        let value = coerce_value(like.as_ref(), &property, params.get("value"))
            .ok_or_else(|| format!("property {property} cannot take this value"))?;
        match params.get("frame").and_then(Value::as_f64) {
            Some(frame) => {
                let frame = Frame(frame.round().max(0.0) as i64);
                self.file
                    .document
                    .add_keyframe(id, &prop, frame, &value)
                    .map_err(|error| error.to_string())?;
            }
            None => {
                self.file
                    .document
                    .set_static(id, &prop, &value)
                    .map_err(|error| error.to_string())?;
            }
        }
        self.dirty = true;
        self.refresh_player();
        Ok(json!({"ok": true, "keys": self.key_frames_of(id, &prop)}))
    }

    pub fn timeline_remove(&mut self, params: &Value) -> Result<Value, String> {
        let id = self.node_of(params)?;
        let property = params
            .get("property")
            .and_then(Value::as_str)
            .ok_or("timeline_remove needs a property name")?
            .to_string();
        let prop = PropPath::new(property.clone());
        let frame = params
            .get("frame")
            .and_then(Value::as_f64)
            .ok_or("timeline_remove needs a frame")?;
        let removed = self
            .file
            .document
            .remove_keyframe(id, &prop, Frame(frame.round().max(0.0) as i64))
            .is_ok();
        self.dirty = true;
        self.refresh_player();
        Ok(json!({"ok": true, "removed": removed, "keys": self.key_frames_of(id, &prop)}))
    }

    /// Keyframes of every animated property on the node.
    pub fn timeline_info(&mut self, params: &Value) -> Result<Value, String> {
        let id = self.node_of(params)?;
        let node = self.file.document.nodes.get(id).ok_or("no such node")?;
        let name = node.name.clone();
        let mut animated = Vec::new();
        for property in PROPERTIES {
            let prop = PropPath::new(*property);
            let frames = self.key_frames_of(id, &prop);
            if frames.is_empty() {
                continue;
            }
            let keys: Vec<Value> = frames
                .iter()
                .map(|frame| {
                    self.file
                        .document
                        .keyframe_data(id, &prop, *frame)
                        .map(|key| {
                            json!({"frame": key.frame.0, "value": model_value_json(&key.value)})
                        })
                        .unwrap_or(json!({"frame": frame.0}))
                })
                .collect();
            animated.push(json!({"property": property, "keys": keys}));
        }
        Ok(json!({"ok": true, "handle": self.handle_of(id), "name": name, "animated": animated}))
    }

    /// Move the playhead to `frame`. `Player::scrub` leaves machine mode, which
    /// would orphan the inputs a rig's transitions gate on, so a machine is
    /// re-armed on the first seek and only ticked afterwards: re-arming mid
    /// session would reset the inputs the caller just set.
    fn seek(&mut self, frame: f64) {
        let rate = self.player.rate();
        let step = 1.0 / (rate.num as f64 / rate.den as f64).max(1.0);
        if let Some(machine) = self.file.start_machine {
            if self.player.active_machine_states().is_none() {
                self.player.play_machine(machine);
            }
            let mut ticks = 0;
            while (self.player.head() < frame || ticks == 0) && ticks < 100_000 {
                self.player.tick(step);
                ticks += 1;
            }
        } else {
            self.player.scrub(frame);
        }
    }

    fn key_frames_of(&self, id: NodeId, prop: &PropPath) -> Vec<Frame> {
        self.file.document.key_frames(id, prop)
    }

    pub fn clip_new(&mut self, params: &Value) -> Result<Value, String> {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("Clip");
        let frames = params.get("frames").and_then(Value::as_f64).unwrap_or(60.0);
        let clip = Clip {
            name: name.to_string(),
            range: (Frame(0), Frame(frames.round().max(1.0) as i64)),
            tracks: Vec::new(),
            events: Vec::new(),
        };
        let id = self.file.clips.insert(clip);
        self.file.clip_order.push(id);
        self.dirty = true;
        self.refresh_player();
        Ok(json!({"ok": true, "clip": self.clip_index(id)}))
    }

    /// Set one key in a clip track, creating the track on first use.
    pub fn clip_track_set(&mut self, params: &Value) -> Result<Value, String> {
        let clip = self.clip_of(params)?;
        let id = self.node_of(params)?;
        let property = params
            .get("property")
            .and_then(Value::as_str)
            .ok_or("clip_track_set needs a property name")?
            .to_string();
        let prop = PropPath::new(property.clone());
        let frame = params.get("frame").and_then(Value::as_f64).unwrap_or(0.0);
        let frame = Frame(frame.round().max(0.0) as i64);
        let existing = self
            .file
            .clips
            .get(clip)
            .and_then(|clip| {
                clip.tracks
                    .iter()
                    .find(|track| track.node == id && track.prop == prop)
            })
            .and_then(|track| track.keys.first())
            .map(|key| key.value.clone());
        let value = coerce_value(existing.as_ref(), &property, params.get("value"))
            .ok_or_else(|| format!("property {property} cannot take this value"))?;
        let key = KeyframeData {
            frame,
            value,
            interpolation: Interpolation::Linear,
            ease_out: EasingHandle::LINEAR_OUT,
            ease_in: EasingHandle::LINEAR_IN,
        };
        let clip = self.file.clips.get_mut(clip).ok_or("clip disappeared")?;
        match clip
            .tracks
            .iter_mut()
            .find(|track| track.node == id && track.prop == prop)
        {
            Some(track) => {
                track.keys.retain(|key| key.frame != frame);
                track.keys.push(key);
                track.keys.sort_by_key(|key| key.frame.0);
            }
            None => clip.tracks.push(Track {
                node: id,
                prop: prop.clone(),
                keys: vec![key],
            }),
        }
        if frame > clip.range.1 {
            clip.range.1 = frame;
        }
        self.dirty = true;
        self.refresh_player();
        Ok(json!({"ok": true}))
    }

    pub fn machine_new(&mut self, params: &Value) -> Result<Value, String> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Machine");
        let machine = Machine {
            name: name.to_string(),
            inputs: Vec::new(),
            layers: vec![MachineLayer {
                name: "Base".to_string(),
                states: Vec::new(),
                entry: 0,
                any_transitions: Vec::new(),
            }],
            listeners: Vec::new(),
        };
        let id = self.file.machines.insert(machine);
        self.file.machine_order.push(id);
        let start = params.get("start").and_then(Value::as_bool).unwrap_or(true);
        if start {
            self.file.start_machine = Some(id);
        }
        self.dirty = true;
        self.refresh_player();
        Ok(json!({"ok": true, "machine": self.machine_index(id), "start": start}))
    }

    pub fn machine_input(&mut self, params: &Value) -> Result<Value, String> {
        let machine = self.machine_of(params)?;
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or("machine_input needs a name")?
            .to_string();
        let kind = match params.get("kind").and_then(Value::as_str).unwrap_or("bool") {
            "bool" => InputKind::Bool {
                default: params
                    .get("default")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            },
            "number" => InputKind::Number {
                default: params.get("default").and_then(Value::as_f64).unwrap_or(0.0),
            },
            "trigger" => InputKind::Trigger,
            other => {
                return Err(format!(
                    "unknown input kind {other}: bool, number or trigger"
                ));
            }
        };
        let machine = self
            .file
            .machines
            .get_mut(machine)
            .ok_or("machine disappeared")?;
        machine.inputs.push(InputDef { name, kind });
        self.dirty = true;
        Ok(json!({"ok": true, "input": machine.inputs.len() - 1}))
    }

    /// Add a state to the machine's first layer. With a clip it plays that
    /// clip; without one it rests on the document values. The first state is
    /// the entry state.
    pub fn machine_state(&mut self, params: &Value) -> Result<Value, String> {
        let machine = self.machine_of(params)?;
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or("machine_state needs a name")?
            .to_string();
        let kind = match params.get("clip").and_then(Value::as_u64) {
            Some(clip) => {
                let clip = self.clip_by_index(clip as usize)?;
                let loop_mode = match params.get("loop").and_then(Value::as_str).unwrap_or("loop") {
                    "once" => LoopMode::Once,
                    "pingpong" => LoopMode::PingPong,
                    _ => LoopMode::Loop,
                };
                StateKind::Clip {
                    clip,
                    speed: params.get("speed").and_then(Value::as_f64).unwrap_or(1.0),
                    loop_mode,
                }
            }
            None => StateKind::Empty,
        };
        let machine = self
            .file
            .machines
            .get_mut(machine)
            .ok_or("machine disappeared")?;
        let layer = machine.layers.first_mut().ok_or("machine has no layer")?;
        layer.states.push(State {
            name,
            kind,
            transitions: Vec::new(),
            graph_pos: None,
        });
        let state = layer.states.len() - 1;
        self.dirty = true;
        Ok(json!({"ok": true, "state": state}))
    }

    /// Add a transition between states, gated on one machine input. Names
    /// resolve to input indices; `op` is eq, ne, lt, le, gt or ge.
    pub fn machine_transition(&mut self, params: &Value) -> Result<Value, String> {
        let machine = self.machine_of(params)?;
        let from = params
            .get("from")
            .and_then(Value::as_u64)
            .ok_or("needs from")? as usize;
        let to = params.get("to").and_then(Value::as_u64).ok_or("needs to")? as usize;
        let duration = params
            .get("duration")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let conditions = {
            let machine = self
                .file
                .machines
                .get(machine)
                .ok_or("machine disappeared")?;
            let states = machine
                .layers
                .first()
                .map(|layer| layer.states.len())
                .unwrap_or(0);
            if from >= states || to >= states {
                return Err("no such state".to_string());
            }
            match params.get("input").and_then(Value::as_str) {
                Some(name) => {
                    let index = machine
                        .inputs
                        .iter()
                        .position(|input| input.name == name)
                        .ok_or_else(|| format!("no input named {name}"))?;
                    let condition = if params.get("triggered").and_then(Value::as_bool)
                        == Some(true)
                    {
                        Condition::Triggered { input: index }
                    } else if let Some(value) = params.get("is").and_then(Value::as_bool) {
                        Condition::BoolIs {
                            input: index,
                            value,
                        }
                    } else if let Some(value) = params.get("value").and_then(Value::as_f64) {
                        let op = match params.get("op").and_then(Value::as_str).unwrap_or("eq") {
                            "ne" => CmpOp::Ne,
                            "lt" => CmpOp::Lt,
                            "le" => CmpOp::Le,
                            "gt" => CmpOp::Gt,
                            "ge" => CmpOp::Ge,
                            _ => CmpOp::Eq,
                        };
                        Condition::NumberCmp {
                            input: index,
                            op,
                            value,
                        }
                    } else {
                        return Err("needs is, value or triggered".to_string());
                    };
                    vec![condition]
                }
                None => Vec::new(),
            }
        };
        let machine = self
            .file
            .machines
            .get_mut(machine)
            .ok_or("machine disappeared")?;
        let layer = machine.layers.first_mut().ok_or("machine has no layer")?;
        layer.states[from].transitions.push(Transition {
            to,
            duration,
            exit_time: params.get("exitTime").and_then(Value::as_f64),
            conditions,
        });
        self.dirty = true;
        Ok(json!({"ok": true}))
    }

    pub fn playback(&mut self, params: &Value) -> Result<Value, String> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("play");
        match action {
            "play" => {
                let looped = params.get("loop").and_then(Value::as_bool).unwrap_or(true);
                let mode = if looped {
                    LoopMode::Loop
                } else {
                    LoopMode::Once
                };
                match self.file.start_machine {
                    Some(machine) => self.player.play_machine(machine),
                    None => {
                        self.player.play_timeline(mode);
                        true
                    }
                };
                true
            }
            "pause" => {
                self.player.pause();
                true
            }
            "scrub" => {
                let frame = params
                    .get("frame")
                    .and_then(Value::as_f64)
                    .ok_or("scrub needs a frame")?;
                self.player.scrub(frame);
                true
            }
            other => {
                return Err(format!(
                    "unknown playback action {other}: play, pause or scrub"
                ));
            }
        };
        Ok(json!({"ok": true, "action": action}))
    }

    pub fn input_set(&mut self, params: &Value) -> Result<Value, String> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or("input_set needs a name")?;
        let ok = if let Some(value) = params.get("bool").and_then(Value::as_bool) {
            self.player.set_bool(name, value)
        } else if let Some(value) = params.get("number").and_then(Value::as_f64) {
            self.player.set_number(name, value)
        } else {
            return Err("input_set needs bool or number".to_string());
        };
        Ok(json!({"ok": ok}))
    }

    pub fn input_fire(&mut self, params: &Value) -> Result<Value, String> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or("input_fire needs a name")?;
        Ok(json!({"ok": self.player.fire(name)}))
    }

    fn node_of(&mut self, params: &Value) -> Result<NodeId, String> {
        self.sync_handles();
        let handle = params.get("id").ok_or("needs a node id")?;
        self.handles
            .resolve(handle)
            .ok_or_else(|| "unknown node handle".to_string())
    }

    fn clip_of(&self, params: &Value) -> Result<ClipId, String> {
        let index = params
            .get("clip")
            .and_then(Value::as_u64)
            .ok_or("needs a clip id")? as usize;
        self.clip_by_index(index)
    }

    fn clip_by_index(&self, index: usize) -> Result<ClipId, String> {
        self.file
            .clip_order
            .get(index)
            .copied()
            .ok_or_else(|| format!("no clip id {index}"))
    }

    fn clip_index(&self, id: ClipId) -> usize {
        self.file
            .clip_order
            .iter()
            .position(|candidate| *candidate == id)
            .unwrap_or(usize::MAX)
    }

    fn machine_of(&self, params: &Value) -> Result<MachineId, String> {
        let index = params
            .get("machine")
            .and_then(Value::as_u64)
            .ok_or("needs a machine id")? as usize;
        self.machine_by_index(index)
    }

    fn machine_by_index(&self, index: usize) -> Result<MachineId, String> {
        self.file
            .machine_order
            .get(index)
            .copied()
            .ok_or_else(|| format!("no machine id {index}"))
    }

    fn machine_index(&self, id: MachineId) -> usize {
        self.file
            .machine_order
            .iter()
            .position(|candidate| *candidate == id)
            .unwrap_or(usize::MAX)
    }
}

/// The animatable properties `timeline_set` accepts, in the model's own naming.
const PROPERTIES: &[&str] = &[
    "shape.pos",
    "shape.size",
    "shape.rounded",
    "transform.position",
    "transform.rotation",
    "transform.scale",
    "transform.anchor",
    "transform.skew",
    "opacity",
    "stroke.width",
];

fn coerce_value(
    like: Option<&ModelValue>,
    property: &str,
    input: Option<&Value>,
) -> Option<ModelValue> {
    let input = input?;
    if property.contains("rotation") {
        return input
            .as_f64()
            .map(|degrees| ModelValue::Angle(Angle(degrees.to_radians())));
    }
    if let Some(like) = like {
        return match like {
            ModelValue::DVec2(_) => input
                .as_array()
                .and_then(|pair| Some([pair.first()?.as_f64()?, pair.get(1)?.as_f64()?]))
                .map(|[x, y]| ModelValue::DVec2(glam::DVec2::new(x, y))),
            ModelValue::Color(_) => input.as_str().and_then(parse_model_color),
            ModelValue::Bool(_) => input.as_bool().map(ModelValue::Bool),
            ModelValue::Angle(_) => input
                .as_f64()
                .map(|degrees| ModelValue::Angle(Angle(degrees.to_radians()))),
            other => input.as_f64().map(|_| other.clone()),
        };
    }
    if let Some(text) = input.as_str() {
        return parse_model_color(text);
    }
    if let Some(pair) = input.as_array() {
        return Some(ModelValue::DVec2(glam::DVec2::new(
            pair.first()?.as_f64()?,
            pair.get(1)?.as_f64()?,
        )));
    }
    if let Some(flag) = input.as_bool() {
        return Some(ModelValue::Bool(flag));
    }
    input.as_f64().map(ModelValue::F64)
}

fn parse_model_color(text: &str) -> Option<ModelValue> {
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
    Some(ModelValue::Color(Color::rgba(
        channel(0),
        channel(1),
        channel(2),
        alpha,
    )))
}

fn model_value_json(value: &ModelValue) -> Value {
    match value {
        ModelValue::F64(value) => json!(value),
        ModelValue::I64(value) => json!(value),
        ModelValue::Bool(value) => json!(value),
        ModelValue::DVec2(value) => json!([value.x, value.y]),
        ModelValue::Angle(value) => json!(value.0.to_degrees()),
        ModelValue::Color(value) => json!(hex_color(*value)),
        other => json!(format!("{other:?}")),
    }
}

/// Stable per-session node handles. Enumeration indices shift the moment a
/// node is added or removed, so the tools hand these out instead. Legacy
/// integer ids still resolve against the current enumeration order.
#[derive(Default)]
pub struct HandleTable {
    next: usize,
    nodes: HashMap<NodeId, String>,
    order: Vec<NodeId>,
}

impl HandleTable {
    /// Resolve a handle string, or a legacy enumeration index, to a node.
    pub fn resolve(&self, handle: &Value) -> Option<NodeId> {
        if let Some(text) = handle.as_str() {
            return self
                .order
                .iter()
                .copied()
                .find(|id| self.nodes.get(id).map(String::as_str) == Some(text));
        }
        let index = handle.as_u64()? as usize;
        self.order.get(index).copied()
    }

    /// Live handles with their nodes, in assignment order.
    pub fn live(&self) -> Vec<(Value, NodeId)> {
        self.order
            .iter()
            .filter_map(|id| self.nodes.get(id).map(|handle| (json!(handle), *id)))
            .collect()
    }
}
