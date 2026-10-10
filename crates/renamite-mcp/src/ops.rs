//! Document operations, shared by the headless MCP session and the editor's
//! control channel, so both drive one implementation.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use renamite_animation::{Angle, Animated, EasingHandle, Frame, Interpolation, LoopMode};
use renamite_io_ren::RenFile;
use renamite_machine::{
    Clip, ClipId, CmpOp, Condition, InputDef, InputKind, Machine, MachineId, MachineLayer, State,
    StateKind, Track, Transition,
};
use renamite_model::{
    Color, Document, FillRule, KeyframeData, Node, NodeId, NodeKind, Parent, PropPath, ShapeKind,
    StrokeCap, StrokeJoin, StyleKind, StylePaint, Value as ModelValue,
};
use serde_json::{Value, json};

pub const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;

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

pub fn coerce_value(
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

#[derive(Default)]
pub struct HandleTable {
    next: usize,
    nodes: HashMap<NodeId, String>,
    order: Vec<NodeId>,
}

impl HandleTable {
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

    pub fn live(&self) -> Vec<(Value, NodeId)> {
        self.order
            .iter()
            .filter_map(|id| self.nodes.get(id).map(|handle| (json!(handle), *id)))
            .collect()
    }
}

pub fn sync_handles(file: &RenFile, handles: &mut HandleTable) {
    let live = node_ids(file);
    for id in &live {
        if !handles.nodes.contains_key(id) {
            let handle = format!("n{}", handles.next);
            handles.next += 1;
            handles.nodes.insert(*id, handle);
            handles.order.push(*id);
        }
    }
    let keep: HashSet<NodeId> = live.into_iter().collect();
    handles.order.retain(|id| keep.contains(id));
}

pub fn handle_of(file: &RenFile, handles: &mut HandleTable, id: NodeId) -> Value {
    sync_handles(file, handles);
    match handles.nodes.get(&id) {
        Some(handle) => json!(handle),
        None => json!(null),
    }
}

pub fn node_ids(file: &RenFile) -> Vec<NodeId> {
    let mut ids = Vec::new();
    let Some(comp) = file.document.compositions.get(file.document.main) else {
        return ids;
    };
    let mut pending: Vec<NodeId> = comp.children.iter().rev().copied().collect();
    while let Some(id) = pending.pop() {
        let Some(node) = file.document.nodes.get(id) else {
            continue;
        };
        for child in node.children.iter().rev() {
            pending.push(*child);
        }
        ids.push(id);
    }
    ids
}

pub fn ids_of(
    file: &RenFile,
    handles: &mut HandleTable,
    params: &Value,
) -> Result<Vec<NodeId>, String> {
    sync_handles(file, handles);
    match params.get("ids") {
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                handles
                    .resolve(item)
                    .ok_or_else(|| "unknown node handle".to_string())
            })
            .collect(),
        _ => Ok(handles.live().into_iter().map(|(_, id)| id).collect()),
    }
}

pub fn node_of(
    file: &RenFile,
    handles: &mut HandleTable,
    params: &Value,
) -> Result<NodeId, String> {
    sync_handles(file, handles);
    let handle = params.get("id").ok_or("needs a node id")?;
    handles
        .resolve(handle)
        .ok_or_else(|| "unknown node handle".to_string())
}

pub fn clip_of(file: &RenFile, params: &Value) -> Result<ClipId, String> {
    let index = params
        .get("clip")
        .and_then(Value::as_u64)
        .ok_or("needs a clip id")? as usize;
    clip_by_index(file, index)
}

pub fn clip_by_index(file: &RenFile, index: usize) -> Result<ClipId, String> {
    file.clip_order
        .get(index)
        .copied()
        .ok_or_else(|| format!("no clip id {index}"))
}

pub fn clip_index(file: &RenFile, id: ClipId) -> usize {
    file.clip_order
        .iter()
        .position(|candidate| *candidate == id)
        .unwrap_or(usize::MAX)
}

pub fn machine_of(file: &RenFile, params: &Value) -> Result<MachineId, String> {
    let index = params
        .get("machine")
        .and_then(Value::as_u64)
        .ok_or("needs a machine id")? as usize;
    machine_by_index(file, index)
}

pub fn machine_by_index(file: &RenFile, index: usize) -> Result<MachineId, String> {
    file.machine_order
        .get(index)
        .copied()
        .ok_or_else(|| format!("no machine id {index}"))
}

pub fn machine_index(file: &RenFile, id: MachineId) -> usize {
    file.machine_order
        .iter()
        .position(|candidate| *candidate == id)
        .unwrap_or(usize::MAX)
}

pub fn index_of(file: &RenFile, id: NodeId) -> Option<usize> {
    node_ids(file).iter().position(|candidate| *candidate == id)
}

pub fn key_frames_of(file: &RenFile, id: NodeId, prop: &PropPath) -> Vec<Frame> {
    file.document.key_frames(id, prop)
}

pub fn push_node(file: &mut RenFile, node: Node) -> Result<NodeId, String> {
    let id = file.document.create_node(node);
    let main = file.document.main;
    file.document
        .attach(id, Parent::Comp(main), 0)
        .map_err(|error| format!("node did not attach: {error}"))?;
    Ok(id)
}

pub fn push_child(file: &mut RenFile, parent: NodeId, node: Node) -> Result<NodeId, String> {
    let id = file.document.create_node(node);
    file.document
        .attach(id, Parent::Node(parent), usize::MAX)
        .map_err(|error| format!("style did not attach: {error}"))?;
    Ok(id)
}

pub fn project_new(
    file: &mut RenFile,
    handles: &mut HandleTable,
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
    *file = RenFile::new(document, name.unwrap_or("untitled"));
    *handles = HandleTable::default();
    Ok(json!({"ok": true, "size": [width.max(1), height.max(1)]}))
}

pub fn project_open(file: &mut RenFile, path: &Path) -> Result<Value, String> {
    let bytes = read_limited(path)?;
    let source = String::from_utf8(bytes)
        .map_err(|_| format!("{} is not a .ren text document", path.display()))?;
    *file = renamite_io_ren::open(&source)
        .map_err(|error| format!("{} did not parse: {error}", path.display()))?;
    Ok(json!({"ok": true, "path": path.display().to_string()}))
}

pub fn project_save(file: &mut RenFile, path: &Path) -> Result<Value, String> {
    let target = path.to_path_buf();
    let binary = target
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("renb"));
    let bytes = if binary {
        renamite_io_ren::save_binary(file).map_err(|error| error.to_string())?
    } else {
        renamite_io_ren::save(file)
            .map_err(|error| error.to_string())?
            .into_bytes()
    };
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    std::fs::write(&target, bytes).map_err(|error| format!("{}: {error}", target.display()))?;
    Ok(json!({"ok": true, "path": target.display().to_string(), "binary": binary}))
}

pub fn project_info(file: &RenFile, handles: &mut HandleTable) -> Value {
    sync_handles(file, handles);
    let ids = node_ids(file);
    let comp = file
        .document
        .compositions
        .get(file.document.main)
        .map(|c| (c.name.clone(), c.size));
    let mut nodes: Vec<Value> = Vec::new();
    for id in &ids {
        let Some(node) = file.document.nodes.get(*id) else {
            continue;
        };
        let name = node.name.clone();
        let visible = node.visible;
        let (kind, detail) = {
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
            "handle": handle_of(file, handles, *id),
            "name": name,
            "kind": kind,
            "visible": visible,
            "detail": detail,
        }));
    }
    json!({
        "name": comp.as_ref().map(|(name, _)| name.clone()).unwrap_or_default(),
        "size": comp.map(|(_, size)| size).unwrap_or((0, 0)),
        "nodes": nodes,
    })
}

pub fn draw_shape(
    file: &mut RenFile,
    handles: &mut HandleTable,
    params: &Value,
) -> Result<Value, String> {
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
    let shape_id = push_node(file, Node::new(name, NodeKind::Shape(kind)))?;
    let fill = parse_color(params.get("fill"));
    let stroke = parse_color(params.get("stroke"));
    let stroke_width = number(params, "strokeWidth").unwrap_or(0.0).max(0.0);
    if let Some(color) = fill {
        let style = fill_node(color, params.get("fillRule").and_then(Value::as_str));
        push_child(file, shape_id, style)?;
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
        push_child(file, shape_id, style)?;
    }
    Ok(json!({"ok": true, "handle": handle_of(file, handles, shape_id), "name": shape}))
}

pub fn set_paint(
    file: &mut RenFile,
    handles: &mut HandleTable,
    params: &Value,
) -> Result<Value, String> {
    let ids = ids_of(file, handles, params)?;
    let fill = parse_color(params.get("fill"));
    let stroke = parse_color(params.get("stroke"));
    let stroke_width = number(params, "strokeWidth");
    let mut touched = Vec::new();
    for id in ids {
        let children: Vec<NodeId> = file
            .document
            .nodes
            .get(id)
            .map(|node| node.children.clone())
            .unwrap_or_default();
        for child in children {
            let Some(node) = file.document.nodes.get_mut(child) else {
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
                    touched.push(index_of(file, child));
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
                    touched.push(index_of(file, child));
                }
                (NodeKind::Style(StyleKind::Stroke { width, .. }), _, None, Some(next)) => {
                    *width = Animated::new(next.max(0.1));
                    touched.push(index_of(file, child));
                }
                _ => {}
            }
        }
    }
    Ok(json!({"ok": true, "touched": touched}))
}

pub fn transform(
    file: &mut RenFile,
    handles: &mut HandleTable,
    params: &Value,
) -> Result<Value, String> {
    let ids = ids_of(file, handles, params)?;
    let x = number(params, "x").ok();
    let y = number(params, "y").ok();
    let width = number(params, "width").ok();
    let height = number(params, "height").ok();
    let rotation = number(params, "rotation").ok();
    let scale_x = number(params, "scaleX").ok();
    let scale_y = number(params, "scaleY").ok();
    let mut touched = Vec::new();
    for id in ids {
        let Some(node) = file.document.nodes.get_mut(id) else {
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
                    node.transform.rotation.base = renamite_animation::Angle(degrees.to_radians());
                }
                if let (Some(sx), Some(sy)) = (scale_x, scale_y) {
                    node.transform.scale.base = glam::DVec2::new(sx, sy);
                } else if let Some(scale) = scale_x.or(scale_y) {
                    node.transform.scale.base = glam::DVec2::splat(scale);
                }
                touched.push(index_of(file, id));
            }
            _ => continue,
        }
    }
    Ok(json!({"ok": true, "touched": touched}))
}

pub fn delete(
    file: &mut RenFile,
    handles: &mut HandleTable,
    params: &Value,
) -> Result<Value, String> {
    let ids = ids_of(file, handles, params)?;
    let deleted = ids.len();
    for id in ids {
        let _ = file.document.detach(id);
    }
    Ok(json!({"ok": true, "deleted": deleted}))
}

pub fn timeline_set(
    file: &mut RenFile,
    handles: &mut HandleTable,
    params: &Value,
) -> Result<Value, String> {
    let id = node_of(file, handles, params)?;
    let property = params
        .get("property")
        .and_then(Value::as_str)
        .ok_or("timeline_set needs a property name")?
        .to_string();
    let prop = PropPath::new(property.clone());
    let like = file.document.get_static(id, &prop).ok();
    let value = coerce_value(like.as_ref(), &property, params.get("value"))
        .ok_or_else(|| format!("property {property} cannot take this value"))?;
    match params.get("frame").and_then(Value::as_f64) {
        Some(frame) => {
            let frame = Frame(frame.round().max(0.0) as i64);
            file.document
                .add_keyframe(id, &prop, frame, &value)
                .map_err(|error| error.to_string())?;
        }
        None => {
            file.document
                .set_static(id, &prop, &value)
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(json!({"ok": true, "keys": key_frames_of(file, id, &prop)}))
}

pub fn timeline_remove(
    file: &mut RenFile,
    handles: &mut HandleTable,
    params: &Value,
) -> Result<Value, String> {
    let id = node_of(file, handles, params)?;
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
    let removed = file
        .document
        .remove_keyframe(id, &prop, Frame(frame.round().max(0.0) as i64))
        .is_ok();
    Ok(json!({"ok": true, "removed": removed, "keys": key_frames_of(file, id, &prop)}))
}

pub fn timeline_info(
    file: &RenFile,
    handles: &mut HandleTable,
    params: &Value,
) -> Result<Value, String> {
    let id = node_of(file, handles, params)?;
    let node = file.document.nodes.get(id).ok_or("no such node")?;
    let name = node.name.clone();
    let mut animated = Vec::new();
    for property in PROPERTIES {
        let prop = PropPath::new(*property);
        let frames = key_frames_of(file, id, &prop);
        if frames.is_empty() {
            continue;
        }
        let keys: Vec<Value> = frames
            .iter()
            .map(|frame| {
                file.document
                    .keyframe_data(id, &prop, *frame)
                    .map(|key| json!({"frame": key.frame.0, "value": model_value_json(&key.value)}))
                    .unwrap_or(json!({"frame": frame.0}))
            })
            .collect();
        animated.push(json!({"property": property, "keys": keys}));
    }
    Ok(
        json!({"ok": true, "handle": handle_of(file, handles, id), "name": name, "animated": animated}),
    )
}

pub fn clip_new(file: &mut RenFile, params: &Value) -> Result<Value, String> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("Clip");
    let frames = params.get("frames").and_then(Value::as_f64).unwrap_or(60.0);
    let clip = Clip {
        name: name.to_string(),
        range: (Frame(0), Frame(frames.round().max(1.0) as i64)),
        tracks: Vec::new(),
        events: Vec::new(),
    };
    let id = file.clips.insert(clip);
    file.clip_order.push(id);
    Ok(json!({"ok": true, "clip": clip_index(file, id)}))
}

pub fn clip_track_set(
    file: &mut RenFile,
    handles: &mut HandleTable,
    params: &Value,
) -> Result<Value, String> {
    let clip = clip_of(file, params)?;
    let id = node_of(file, handles, params)?;
    let property = params
        .get("property")
        .and_then(Value::as_str)
        .ok_or("clip_track_set needs a property name")?
        .to_string();
    let prop = PropPath::new(property.clone());
    let frame = params.get("frame").and_then(Value::as_f64).unwrap_or(0.0);
    let frame = Frame(frame.round().max(0.0) as i64);
    let existing = file
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
    let clip = file.clips.get_mut(clip).ok_or("clip disappeared")?;
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
    Ok(json!({"ok": true}))
}

pub fn machine_new(file: &mut RenFile, params: &Value) -> Result<Value, String> {
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
    let id = file.machines.insert(machine);
    file.machine_order.push(id);
    let start = params.get("start").and_then(Value::as_bool).unwrap_or(true);
    if start {
        file.start_machine = Some(id);
    }
    Ok(json!({"ok": true, "machine": machine_index(file, id), "start": start}))
}

pub fn machine_input(file: &mut RenFile, params: &Value) -> Result<Value, String> {
    let machine = machine_of(file, params)?;
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
    let machine = file
        .machines
        .get_mut(machine)
        .ok_or("machine disappeared")?;
    machine.inputs.push(InputDef { name, kind });
    Ok(json!({"ok": true, "input": machine.inputs.len() - 1}))
}

pub fn machine_state(file: &mut RenFile, params: &Value) -> Result<Value, String> {
    let machine = machine_of(file, params)?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or("machine_state needs a name")?
        .to_string();
    let kind = match params.get("clip").and_then(Value::as_u64) {
        Some(clip) => {
            let clip = clip_by_index(file, clip as usize)?;
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
    let machine = file
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
    Ok(json!({"ok": true, "state": state}))
}

pub fn machine_transition(file: &mut RenFile, params: &Value) -> Result<Value, String> {
    let machine = machine_of(file, params)?;
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
        let machine = file.machines.get(machine).ok_or("machine disappeared")?;
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
                let condition = if params.get("triggered").and_then(Value::as_bool) == Some(true) {
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
    let machine = file
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
    Ok(json!({"ok": true}))
}

pub fn validate(file: &RenFile) -> Result<Value, String> {
    let report = renamite_validate::validate(file);
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

pub fn import_svg(file: &mut RenFile, params: &Value) -> Result<Value, String> {
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
    *file = RenFile::new(document, "imported svg");
    Ok(json!({
        "ok": true,
        "nodes": node_count,
        "warnings": report.warnings.iter().map(|w| json!({"path": w.path, "message": w.message})).collect::<Vec<_>>(),
    }))
}

pub fn export_svg(file: &mut RenFile, params: &Value) -> Result<Value, String> {
    let path = params
        .get("path")
        .and_then(Value::as_str)
        .ok_or("export_svg needs an output path")?;
    let frame = number(params, "frame").unwrap_or(0.0);
    let report = renamite_io_svg::export_with_report(&file.document, file.document.main, frame)
        .map_err(|error| error.to_string())?;
    std::fs::write(path, report.value.as_bytes()).map_err(|error| format!("{path}: {error}"))?;
    Ok(json!({"ok": true, "path": path}))
}

pub fn fill_node(color: Color, rule: Option<&str>) -> Node {
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

pub fn stroke_node(color: Color, width: f64) -> Node {
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

pub fn number(params: &Value, key: &str) -> Result<f64, String> {
    params
        .get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("missing number parameter {key}"))
}

pub fn parse_color(value: Option<&Value>) -> Option<Color> {
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

pub fn hex_color(color: Color) -> String {
    let channel = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        channel(color.r),
        channel(color.g),
        channel(color.b)
    )
}

pub fn read_limited(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_INPUT_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(bytes)
}

pub fn base64_encode(bytes: &[u8]) -> String {
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

pub fn parse_model_color(text: &str) -> Option<ModelValue> {
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

pub fn model_value_json(value: &ModelValue) -> Value {
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

pub fn composition_size(file: &RenFile) -> (u32, u32) {
    file.document
        .compositions
        .get(file.document.main)
        .map(|comp| comp.size)
        .unwrap_or((0, 0))
}
