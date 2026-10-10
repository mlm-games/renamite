//! Tool definitions and dispatch.

use serde_json::{Value, json};

use crate::ops::HandleTable;
use crate::session::Session;
use renamite_io_ren::RenFile;

pub enum ToolResult {
    Ok(Value),
    Err(String),
}

/// Color parameters accept `#rrggbb`, `#rrggbbaa`, or `none`.
const COLOR_DOC: &str = "#rrggbb or #rrggbbaa, or none to drop the paint";

pub fn tool_definitions() -> Vec<Value> {
    let ids = || json!({"type": "array", "items": {"type": "string"}, "description": "node handles, e.g. [n0, n3]"});
    vec![
        tool(
            "project_new",
            "Start a new document with one composition. Replaces the current document.",
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "width": {"type": "number"},
                    "height": {"type": "number"},
                },
                "required": ["width", "height"],
            }),
        ),
        tool(
            "project_open",
            "Open a .ren text document from disk, replacing the current one.",
            json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
            }),
        ),
        tool(
            "project_save",
            "Write the document as .ren, or .renb when the path ends in .renb. Defaults to the last opened or saved path.",
            json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
            }),
        ),
        tool(
            "project_info",
            "Summarize the active composition: size and the node tree with ids, names, kinds, transforms and paints.",
            json!({"type": "object", "properties": {}}),
        ),
        tool(
            "draw_shape",
            "Add an ellipse or rect at x,y (centre) with the given size, optionally under fill and stroke paint children.",
            json!({
                "type": "object",
                "properties": {
                    "shape": {"type": "string", "enum": ["ellipse", "rect"]},
                    "x": {"type": "number"},
                    "y": {"type": "number"},
                    "width": {"type": "number"},
                    "height": {"type": "number"},
                    "radius": {"type": "number", "description": "rect corner radius"},
                    "fill": {"type": "string", "description": COLOR_DOC},
                    "stroke": {"type": "string", "description": COLOR_DOC},
                    "strokeWidth": {"type": "number"},
                    "name": {"type": "string"},
                },
                "required": ["shape", "x", "y", "width", "height"],
            }),
        ),
        tool(
            "set_paint",
            "Replace fill or stroke colors, or stroke widths, on the given shapes. Omitted keys are left alone.",
            json!({
                "type": "object",
                "properties": {
                    "ids": ids(),
                    "fill": {"type": "string", "description": COLOR_DOC},
                    "stroke": {"type": "string", "description": COLOR_DOC},
                    "strokeWidth": {"type": "number"},
                },
            }),
        ),
        tool(
            "transform",
            "Move, resize, rotate or scale the given shapes. Omitted keys are left alone.",
            json!({
                "type": "object",
                "properties": {
                    "ids": ids(),
                    "x": {"type": "number"},
                    "y": {"type": "number"},
                    "width": {"type": "number"},
                    "height": {"type": "number"},
                    "rotation": {"type": "number", "description": "degrees"},
                    "scaleX": {"type": "number"},
                    "scaleY": {"type": "number"},
                },
            }),
        ),
        tool(
            "delete",
            "Detach the given nodes and their subtrees.",
            json!({
                "type": "object",
                "properties": {"ids": ids()},
                "required": ["ids"],
            }),
        ),
        tool(
            "render_png",
            "Rasterize the composition offscreen and return the PNG bytes, or write them to path. Use it to look at what you drew.",
            json!({
                "type": "object",
                "properties": {
                    "scale": {"type": "number", "description": "pixels per design unit (default 1)"},
                    "frame": {"type": "number", "description": "frame index to render (default 0)"},
                    "path": {"type": "string", "description": "write the PNG here instead of returning base64"},
                },
            }),
        ),
        tool(
            "validate",
            "Run the structural validator over the document and report diagnostics.",
            json!({"type": "object", "properties": {}}),
        ),
        tool(
            "import_svg",
            "Replace the document with one imported from an SVG file. Useful for art authored in a vector tool.",
            json!({
                "type": "object",
                "properties": {"input": {"type": "string"}},
                "required": ["input"],
            }),
        ),
        tool(
            "export_svg",
            "Export the active composition at a frame to an SVG file.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "frame": {"type": "number", "description": "frame index (default 0)"},
                },
                "required": ["path"],
            }),
        ),
        tool(
            "timeline_set",
            "Keyframe a node property on the composition timeline, or set its static value when frame is omitted. Properties: shape.pos, shape.size, shape.rounded, transform.position, transform.rotation, transform.scale, transform.anchor, transform.skew, opacity, stroke.width. Values are numbers, [x, y], degrees for rotation, or #rrggbb.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "node handle, e.g. n0 (legacy enumeration integers still work)"},
                    "property": {"type": "string"},
                    "frame": {"type": "number", "description": "omit to set the static value"},
                    "value": {"description": "number, [x, y], bool, degrees, or #rrggbb"},
                },
                "required": ["id", "property", "value"],
            }),
        ),
        tool(
            "timeline_remove",
            "Remove one keyframe from a node property.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "node handle, e.g. n0 (legacy enumeration integers still work)"},
                    "property": {"type": "string"},
                    "frame": {"type": "number"},
                },
                "required": ["id", "property", "frame"],
            }),
        ),
        tool(
            "timeline_info",
            "List the animated properties of a node with their keyframes and values.",
            json!({
                "type": "object",
                "properties": {"id": {"type": "string", "description": "node handle, e.g. n0"}},
                "required": ["id"],
            }),
        ),
        tool(
            "clip_new",
            "Create a clip: a reusable track bundle that machine states play. Returns its id.",
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "frames": {"type": "number", "description": "clip length in frames"},
                },
            }),
        ),
        tool(
            "clip_track_set",
            "Keyframe one node property inside a clip, creating the track on first use.",
            json!({
                "type": "object",
                "properties": {
                    "clip": {"type": "integer"},
                    "id": {"type": "string", "description": "node handle, e.g. n0 (legacy enumeration integers still work)"},
                    "property": {"type": "string"},
                    "frame": {"type": "number"},
                    "value": {"description": "number, [x, y], bool, degrees, or #rrggbb"},
                },
                "required": ["clip", "id", "property", "value"],
            }),
        ),
        tool(
            "machine_new",
            "Create a state machine and, by default, make it the one the runtime starts.",
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "start": {"type": "boolean", "description": "set as start_machine (default true)"},
                },
            }),
        ),
        tool(
            "machine_input",
            "Declare an input on a machine: bool, number or trigger. Transitions gate on these.",
            json!({
                "type": "object",
                "properties": {
                    "machine": {"type": "integer"},
                    "name": {"type": "string"},
                    "kind": {"type": "string", "enum": ["bool", "number", "trigger"]},
                    "default": {"description": "default value for bool or number"},
                },
                "required": ["machine", "name"],
            }),
        ),
        tool(
            "machine_state",
            "Add a state to a machine. With a clip it plays that clip; without one it rests on the document values. The first state is the entry state.",
            json!({
                "type": "object",
                "properties": {
                    "machine": {"type": "integer"},
                    "name": {"type": "string"},
                    "clip": {"type": "integer"},
                    "speed": {"type": "number"},
                    "loop": {"type": "string", "enum": ["loop", "once", "pingpong"]},
                },
                "required": ["machine", "name"],
            }),
        ),
        tool(
            "machine_transition",
            "Add a transition from one state to another, gated on one input: is <bool>, value <number> with op eq/ne/lt/le/gt/ge, or triggered for a trigger input. No input means an unconditional transition.",
            json!({
                "type": "object",
                "properties": {
                    "machine": {"type": "integer"},
                    "from": {"type": "integer"},
                    "to": {"type": "integer"},
                    "input": {"type": "string"},
                    "is": {"type": "boolean"},
                    "value": {"type": "number"},
                    "op": {"type": "string", "enum": ["eq", "ne", "lt", "le", "gt", "ge"]},
                    "triggered": {"type": "boolean"},
                    "duration": {"type": "number", "description": "crossfade in frames (default 0)"},
                    "exitTime": {"type": "number"},
                },
                "required": ["machine", "from", "to"],
            }),
        ),
        tool(
            "playback",
            "Play (the start machine, or the timeline), pause, or scrub to a frame. Playback state lives in the session, so scrub then render_png at that frame.",
            json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["play", "pause", "scrub"]},
                    "frame": {"type": "number", "description": "for scrub"},
                    "loop": {"type": "boolean", "description": "loop timeline playback (default true)"},
                },
            }),
        ),
        tool(
            "input_set",
            "Set a machine input value. The machine reacts on the next tick, which render_png performs.",
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "bool": {"type": "boolean"},
                    "number": {"type": "number"},
                },
                "required": ["name"],
            }),
        ),
        tool(
            "input_fire",
            "Fire a trigger input on the start machine.",
            json!({
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"],
            }),
        ),
    ]
}

fn tool(name: &str, description: &str, schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": schema,
    })
}

pub fn call_tool(
    session: &mut Session,
    name: &str,
    arguments: &Value,
) -> Result<ToolResult, String> {
    let outcome = match name {
        "render_png" => session.render_png(arguments),
        "playback" => session.playback(arguments),
        "input_set" => session.input_set(arguments),
        "input_fire" => session.input_fire(arguments),
        "project_new" => {
            let width = arguments.get("width").and_then(Value::as_u64).unwrap_or(64) as u32;
            let height = arguments
                .get("height")
                .and_then(Value::as_u64)
                .unwrap_or(64) as u32;
            let title = arguments.get("name").and_then(Value::as_str);
            session.project_new(title, width, height)
        }
        "project_open" => {
            let path = arguments
                .get("path")
                .and_then(Value::as_str)
                .ok_or("project_open needs a path")?;
            let value = session.project_open(std::path::Path::new(path))?;
            session.sync_handles();
            Ok(value)
        }
        "project_save" => {
            let path = arguments
                .get("path")
                .and_then(Value::as_str)
                .map(std::path::Path::new);
            session.project_save(path)
        }
        document => dispatch_document(&mut session.file, &mut session.handles, document, arguments),
    };
    Ok(match outcome {
        Ok(value) => ToolResult::Ok(value),
        Err(message) => ToolResult::Err(message),
    })
}

/// The document tools, which run against a bare `RenFile` plus its handle
/// table. The MCP session and the editor's control channel both go through
/// here, so one implementation serves both.
pub fn dispatch_document(
    file: &mut RenFile,
    handles: &mut HandleTable,
    name: &str,
    arguments: &Value,
) -> Result<Value, String> {
    use crate::ops;
    match name {
        "project_info" => Ok(ops::project_info(file, handles)),
        "project_save" => {
            let path = arguments
                .get("path")
                .and_then(Value::as_str)
                .ok_or("project_save needs a path")?;
            ops::project_save(file, std::path::Path::new(path))
        }
        "project_new" => {
            let width = arguments.get("width").and_then(Value::as_u64).unwrap_or(64) as u32;
            let height = arguments
                .get("height")
                .and_then(Value::as_u64)
                .unwrap_or(64) as u32;
            let title = arguments.get("name").and_then(Value::as_str);
            ops::project_new(file, handles, title, width, height)
        }
        "project_open" => {
            let path = arguments
                .get("path")
                .and_then(Value::as_str)
                .ok_or("project_open needs a path")?;
            ops::project_open(file, std::path::Path::new(path))
        }
        "draw_shape" => ops::draw_shape(file, handles, arguments),
        "set_paint" => ops::set_paint(file, handles, arguments),
        "transform" => ops::transform(file, handles, arguments),
        "delete" => ops::delete(file, handles, arguments),
        "timeline_set" => ops::timeline_set(file, handles, arguments),
        "timeline_remove" => ops::timeline_remove(file, handles, arguments),
        "timeline_info" => ops::timeline_info(file, handles, arguments),
        "clip_new" => ops::clip_new(file, arguments),
        "clip_track_set" => ops::clip_track_set(file, handles, arguments),
        "machine_new" => ops::machine_new(file, arguments),
        "machine_input" => ops::machine_input(file, arguments),
        "machine_state" => ops::machine_state(file, arguments),
        "machine_transition" => ops::machine_transition(file, arguments),
        "validate" => ops::validate(file),
        "import_svg" => ops::import_svg(file, arguments),
        "export_svg" => ops::export_svg(file, arguments),
        other => Err(format!("unknown tool {other}")),
    }
}
