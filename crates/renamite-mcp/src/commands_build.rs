//! Build [`renamite_history::EditorCommand`]s from the flat JSON that the
//! `run_command` tool accepts.
//!
//! The catalogue in `commands.rs` is deliberately flat: ids are hex strings
//! from `inspect_document`, parents are `{"node": "<id>"}` objects, values are
//! plain numbers and arrays. This module is the only place that translates all
//! that into slotmap keys and `Value` payloads, so the two representations
//! cannot drift.

use renamite_animation::{Animated, Frame};
use renamite_history::{EditorCommand, NodeTree, SelectionChange};
use renamite_model::{Angle, CompId, Node, NodeId, Parent, PropPath, Value};

/// Every command a `run_command` call produced, plus what to select after.
pub struct Built {
    pub commands: Vec<EditorCommand>,
    pub selection: Option<SelectionChange>,
}

/// A build failure, naming the input that caused it.
pub struct BuildError(pub String);

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

type BuildOut = Result<Built, BuildError>;

fn field<'a>(o: &'a serde_json::Map<String, serde_json::Value>, k: &str) -> Option<&'a serde_json::Value> {
    o.get(k)
}

fn text<'a>(o: &'a serde_json::Map<String, serde_json::Value>, k: &str) -> Result<&'a str, BuildError> {
    field(o, k)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| BuildError(format!("`{k}` is required and must be a string")))
}

fn number(o: &serde_json::Map<String, serde_json::Value>, k: &str, default: f64) -> Result<f64, BuildError> {
    match field(o, k) {
        None | Some(serde_json::Value::Null) => Ok(default),
        Some(v) => v
            .as_f64()
            .ok_or_else(|| BuildError(format!("`{k}` must be a number"))),
    }
}

fn index(o: &serde_json::Map<String, serde_json::Value>, k: &str, default: usize) -> Result<usize, BuildError> {
    let n = number(o, k, default as f64)?;
    if n < 0.0 || n.fract() != 0.0 {
        return Err(BuildError(format!("`{k}` must be a whole number")));
    }
    usize::try_from(n as i64).map_err(|_| BuildError(format!("`{k}` is out of range")))
}

fn flag(o: &serde_json::Map<String, serde_json::Value>, k: &str) -> Option<bool> {
    field(o, k).and_then(serde_json::Value::as_bool)
}

/// Recover the slotmap key behind the hex string `inspect_document` handed out.
///
/// `id_to_string` writes `KeyData` as `format!("{id:?}")` minus the type name,
/// which is the ffi value in hex. Parsing the same keeps ids stable across a
/// list and a run of the same project.
fn node_id(text: &str) -> Result<NodeId, BuildError> {
    from_hex(text, "id")
}

fn comp_id(text: &str) -> Result<CompId, BuildError> {
    from_hex(text, "composition id")
}

fn from_hex<T: slotmap::Key>(text: &str, what: &str) -> Result<T, BuildError> {
    let bits = u64::from_str_radix(text, 16)
        .map_err(|_| BuildError(format!("`{what}` must be a hex id from inspect_document, not `{text}`")))?;
    let data = slotmap::KeyData::from_ffi(bits);
    Ok(T::from_ffi(data))
}

/// The parent a node should be inserted under.
fn parent(o: &serde_json::Map<String, serde_json::Value>) -> Result<Parent, BuildError> {
    match field(o, "parent") {
        Some(serde_json::Value::Object(p)) => {
            if let Some(node) = p.get("node") {
                return Ok(Parent::Node(node_id(node.as_str().ok_or_else(|| {
                    BuildError("`parent.node` must be an id string".into())
                })?)?));
            }
            if let Some(comp) = p.get("comp") {
                return Ok(Parent::Comp(comp_id(comp.as_str().ok_or_else(|| {
                    BuildError("`parent.comp` must be an id string".into())
                })?)?));
            }
            Err(BuildError("`parent` needs a `node` or a `comp` key".into()))
        }
        _ => Err(BuildError("`parent` must be {\"node\": id} or {\"comp\": id}".into())),
    }
}

/// The main composition as a parent, for commands that add top-level nodes.
fn main_parent(file: &renamite_io_ren::RenFile) -> Parent {
    Parent::Comp(file.document.main)
}

/// Build every command in one `run_command` call, in order.
///
/// `doc` is read, never written: values are matched against what a property
/// already holds so the same JSON means one thing per property kind. Returns
/// the commands plus, when a command names a selection, what to leave selected.
pub fn build(
    doc: &renamite_model::Document,
    file: &renamite_io_ren::RenFile,
    entries: &Value,
    frame: f64,
) -> Result<Built, BuildError> {
    let list = entries
        .as_array()
        .ok_or_else(|| BuildError("`commands` must be an array".into()))?;
    let mut out: Vec<EditorCommand> = Vec::with_capacity(list.len());
    let mut selection = None;
    for entry in list {
        let o = entry
            .as_object()
            .ok_or_else(|| BuildError("every command must be an object".into()))?;
        let id = text(o, "id")?;
        let inputs = field(o, "inputs").cloned().unwrap_or(Value::Null);
        let input_obj = inputs.as_object().cloned().unwrap_or_default();
        let built = match id {
            "node.insert" => {
                let parent = if field(&input_obj, "parent").is_some() {
                    parent(&input_obj)?
                } else {
                    main_parent(file)
                };
                let node = Node::new(
                    field(&input_obj, "name").and_then(Value::as_str).unwrap_or("Node"),
                    node_kind(&input_obj)?,
                );
                vec![EditorCommand::InsertNode {
                    parent,
                    index: index(&input_obj, "index", usize::MAX)?,
                    tree: NodeTree::leaf(node),
                }]
            }
            "node.remove" => vec![EditorCommand::RemoveNode { id: node_id(text(&input_obj, "id")?)? }],
            "node.move" => vec![EditorCommand::MoveNode {
                id: node_id(text(&input_obj, "id")?)?,
                new_parent: parent(&input_obj)?,
                index: index(&input_obj, "index", 0)?,
            }],
            "node.rename" => vec![EditorCommand::SetNodeName {
                id: node_id(text(&input_obj, "id")?)?,
                name: text(&input_obj, "name")?.to_string(),
            }],
            "node.flags" => vec![EditorCommand::SetNodeFlags {
                id: node_id(text(&input_obj, "id")?)?,
                visible: flag(&input_obj, "visible"),
                locked: flag(&input_obj, "locked"),
            }],
            "property.set" => {
                let id = node_id(text(&input_obj, "id")?)?;
                let prop = PropPath::new(text(&input_obj, "prop")?);
                let value = value_for(
                    doc,
                    id,
                    &prop,
                    field(&input_obj, "value").cloned().unwrap_or(Value::Null),
                )?;
                vec![EditorCommand::SetStatic { id, prop, value }]
            }
            "property.key" => {
                let id = node_id(text(&input_obj, "id")?)?;
                let prop = PropPath::new(text(&input_obj, "prop")?);
                let value = value_for(
                    doc,
                    id,
                    &prop,
                    field(&input_obj, "value").cloned().unwrap_or(Value::Null),
                )?;
                vec![EditorCommand::AddKeyframe {
                    id,
                    prop,
                    frame: Frame(index(&input_obj, "frame", frame as usize)? as i64),
                    value,
                }]
            }
            "property.removeKeyframe" => vec![EditorCommand::RemoveKeyframe {
                id: node_id(text(&input_obj, "id")?)?,
                prop: PropPath::new(text(&input_obj, "prop")?),
                frame: Frame(index(&input_obj, "frame", frame as usize)? as i64),
            }],
            "shape.reverse" => vec![EditorCommand::ReversePath {
                id: node_id(text(&input_obj, "id")?)?,
            }],
            "node.group" => {
                let ids = field(&input_obj, "ids")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(node_id)
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .transpose()?
                    .unwrap_or_default();
                if ids.len() < 2 {
                    return Err(BuildError("grouping needs at least two node ids".into()));
                }
                vec![EditorCommand::GroupSelection {
                    ids,
                    parent: main_parent(file),
                    index: usize::MAX,
                    group: None,
                }]
            }
            "composition.set" => vec![EditorCommand::SetCompositionName {
                id: comp_id(text(&input_obj, "id")?)?,
                name: field(&input_obj, "name").and_then(Value::as_str).map(str::to_string),
            }],
            other => return Err(BuildError(format!("unknown command `{other}`"))),
        };
        if id == "node.insert" {
            selection = Some(SelectionChange::Set(Vec::new()));
        }
        out.extend(built);
    }
    if out.is_empty() {
        return Err(BuildError("no commands given".into()));
    }
    Ok(Built { commands: out, selection })
}

/// The shape kind a `node.insert` asks for, from its flat fields.
fn shape_kind(o: &serde_json::Map<String, serde_json::Value>) -> Result<renamite_model::ShapeKind, BuildError> {
    let width = number(o, "width", 512.0)?;
    let height = number(o, "height", 512.0)?;
    let points = number(o, "points", 5.0)?;
    let size = glam::DVec2::new(width, height);
    Ok(match text(o, "kind")? {
        "rect" | "rectangle" => renamite_model::ShapeKind::Rect {
            pos: Animated::new(glam::DVec2::ZERO),
            size: Animated::new(size),
            rounded: Animated::new(number(o, "radius", 0.0)?),
        },
        "ellipse" => renamite_model::ShapeKind::Ellipse {
            pos: Animated::new(glam::DVec2::ZERO),
            size: Animated::new(size),
        },
        "star" => renamite_model::ShapeKind::Star {
            pos: Animated::new(glam::DVec2::ZERO),
            points: Animated::new(points),
            inner_r: Animated::new(number(o, "inner", width * 0.5)?),
            outer_r: Animated::new(width * 0.5),
            roundness: Animated::new(0.0),
            kind: renamite_model::StarKind::Polygon,
        },
        "polygon" => renamite_model::ShapeKind::Polygon {
            pos: Animated::new(glam::DVec2::ZERO),
            points: Animated::new(points),
            outer_r: Animated::new(width * 0.5),
            roundness: Animated::new(0.0),
        },
        "path" => renamite_model::ShapeKind::Path(Animated::new(renamite_geometry::VectorPath::default())),
        "compound" => renamite_model::ShapeKind::CompoundPath(renamite_model::CompoundPath {
            contours: Vec::new(),
        }),
        other => return Err(BuildError(format!("unknown shape kind `{other}`"))),
    })
}

/// The node kind a `node.insert` asks for, from its flat fields.
fn node_kind(o: &serde_json::Map<String, serde_json::Value>) -> Result<renamite_model::NodeKind, BuildError> {
    let kind = text(o, "kind")?;
    if kind == "group" {
        return Ok(renamite_model::NodeKind::Group);
    }
    if kind == "layer" {
        return Ok(renamite_model::NodeKind::Layer(renamite_model::LayerProps::default()));
    }
    if kind == "text" {
        let text = field(o, "text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Text")
            .to_string();
        return Ok(renamite_model::NodeKind::Text(renamite_model::TextNode {
            text,
            size: Animated::new(number(o, "size", 64.0)?),
            align: renamite_model::TextAlign::Left,
            font: None,
            tracking: Animated::new(0.0),
            leading: Animated::new(0.0),
        }));
    }
    if kind == "mask" {
        return Ok(renamite_model::NodeKind::Mask(renamite_model::MaskProps {
            inverted: flag(o, "inverted").unwrap_or(false),
            shape: shape_kind(o)?,
        }));
    }
    Ok(renamite_model::NodeKind::Shape(shape_kind(o)?))
}

/// A `Value` from the JSON a caller writes, matched against what the property
/// actually holds. This is why `property.set` reads the document: the same JSON
/// means different things to an angle, a colour and a position.
fn value_for(
    doc: &renamite_model::Document,
    id: NodeId,
    prop: &PropPath,
    json: serde_json::Value,
) -> Result<Value, BuildError> {
    let current = doc.value_at(id, prop, 0.0).ok();
    let out = match (current, &json) {
        (Some(Value::Bool(_)), serde_json::Value::Bool(b)) => Value::Bool(*b),
        (Some(Value::I64(_)), serde_json::Value::Number(n)) => Value::I64(
            n.as_i64().ok_or_else(|| BuildError("value must be a whole number".into()))?,
        ),
        (Some(Value::F64(_)), serde_json::Value::Number(n)) => {
            let v = n.as_f64().ok_or_else(|| BuildError("value must be a number".into()))?;
            if !v.is_finite() {
                return Err(BuildError("value must be finite".into()));
            }
            Value::F64(v)
        }
        (Some(Value::Angle(_)), serde_json::Value::Number(n)) => {
            let v = n.as_f64().ok_or_else(|| BuildError("value must be a number".into()))?;
            Value::Angle(Angle(v.to_radians()))
        }
        (Some(Value::DVec2(_)), serde_json::Value::Array(a)) => {
            let x = a.first().and_then(serde_json::Value::as_f64).ok_or_else(|| {
                BuildError("`value` must be [x, y] for a position or size".into())
            })?;
            let y = a.get(1).and_then(serde_json::Value::as_f64).ok_or_else(|| {
                BuildError("`value` must be [x, y] for a position or size".into())
            })?;
            if !x.is_finite() || !y.is_finite() {
                return Err(BuildError("`value` must be finite".into()));
            }
            Value::DVec2(glam::DVec2::new(x, y))
        }
        (Some(Value::Color(_)), serde_json::Value::String(s)) => {
            Value::Color(parse_color(s).ok_or_else(|| {
                BuildError("`value` must be a colour as #rrggbb, #rrggbbaa or [r, g, b, a]".into())
            })?)
        }
        _ => return Err(BuildError(
            "the property and the value do not match; read it first with property paths".into(),
        )),
    };
    Ok(out)
}

/// A colour from `#rrggbb`, `#rrggbbaa`, `#rgb`, or an `[r, g, b, a]` list of
/// 0..1 components. All other spellings are rejected rather than guessed at.
fn parse_color(text: &str) -> Option<renamite_model::Color> {
    let hex = text.strip_prefix('#')?;
    let byte = |i: usize| -> Option<f64> {
        u8::from_str_radix(hex.get(i..i + 2)?, 16)
            .ok()
            .map(f64::from)
            .map(|v| v / 255.0)
    };
    let expand = |i: usize| -> Option<f64> {
        let c = hex.get(i..=i)?;
        u8::from_str_radix(&format!("{c}{c}"), 16)
            .ok()
            .map(f64::from)
            .map(|v| v / 255.0)
    };
    let to01 = |v: f64| v.clamp(0.0, 1.0);
    match hex.len() {
        3 => Some(renamite_model::Color::rgba(to01(expand(0)?), to01(expand(1)?), to01(expand(2)?), 1.0)),
        6 => Some(renamite_model::Color::rgba(to01(byte(0)?), to01(byte(2)?), to01(byte(4)?), 1.0)),
        8 => Some(renamite_model::Color::rgba(to01(byte(0)?), to01(byte(2)?), to01(byte(4)?), to01(byte(6)?))),
        _ => None,
    }
}
