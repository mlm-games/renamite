//! Tool definitions and dispatch.

use serde_json::{Value, json};

use crate::session::Session;

pub enum ToolResult {
    Ok(Value),
    Err(String),
}

/// Color parameters accept `#rrggbb`, `#rrggbbaa`, or `none`.
const COLOR_DOC: &str = "#rrggbb or #rrggbbaa, or none to drop the paint";

pub fn tool_definitions() -> Vec<Value> {
    let ids = || json!({"type": "array", "items": {"type": "integer"}});
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
    ]
}

fn tool(name: &str, description: &str, schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": schema,
    })
}

pub fn call_tool(session: &mut Session, name: &str, arguments: &Value) -> Result<ToolResult, String> {
    let outcome = match name {
        "project_new" => {
            let width = arguments.get("width").and_then(Value::as_u64).unwrap_or(64) as u32;
            let height = arguments.get("height").and_then(Value::as_u64).unwrap_or(64) as u32;
            let title = arguments.get("name").and_then(Value::as_str);
            session.project_new(title, width, height)
        }
        "project_open" => {
            let path = arguments
                .get("path")
                .and_then(Value::as_str)
                .ok_or("project_open needs a path")?;
            session.project_open(std::path::Path::new(path))
        }
        "project_save" => {
            let path = arguments.get("path").and_then(Value::as_str).map(std::path::Path::new);
            session.project_save(path)
        }
        "project_info" => Ok(session.project_info()),
        "draw_shape" => session.draw_shape(arguments),
        "set_paint" => session.set_paint(arguments),
        "transform" => session.transform(arguments),
        "delete" => session.delete(arguments),
        "render_png" => session.render_png(arguments),
        "validate" => session.validate(),
        "import_svg" => session.import_svg(arguments),
        "export_svg" => session.export_svg(arguments),
        other => Err(format!("unknown tool {other}")),
    };
    Ok(match outcome {
        Ok(value) => ToolResult::Ok(value),
        Err(message) => ToolResult::Err(message),
    })
}
