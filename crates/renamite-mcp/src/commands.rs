//! The command catalogue that `list_commands` answers and `run_command` accepts.
//!
//! Schemas are written as literal JSON because `Value::to_string` is not const,
//! and a catalogue that can only be built at runtime answers `tools/list` with a
//! heap allocation every time. The catalogue is intentionally small: every entry
//! maps to a builder in [`crate::commands_build`].

use serde_json::{Value, json};

/// One command.
pub struct CommandDef {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub inputs: Value,
}

/// Every command, in `list_commands` order.
pub fn commands() -> Vec<CommandDef> {
    vec![
        CommandDef {
            id: "node.insert",
            label: "Insert node",
            description: "Create a node inside the main composition, or inside a parent node, at an index.",
            inputs: json!({
                "type": "object",
                "required": ["kind"],
                "properties": {
                    "name": {"type": "string", "description": "Node name"},
                    "kind": {"type": "string", "enum": ["rect", "ellipse", "star", "polygon", "path", "compound", "group", "layer", "text", "mask"], "description": "Node kind to create"},
                    "parent": {"type": "object", "description": "'node' or 'comp' key with an id from inspect_document; defaults to the main composition"},
                    "index": {"type": "integer", "description": "Position among the parent's children; default: last"},
                    "width": {"type": "number", "description": "Shape width, and the outer radius for star/polygon"},
                    "height": {"type": "number", "description": "Shape height"},
                    "radius": {"type": "number", "description": "Rect corner radius"},
                    "inner": {"type": "number", "description": "Star inner radius"},
                    "points": {"type": "integer", "description": "Star/polygon point count"},
                    "size": {"type": "number", "description": "Text em size"},
                    "text": {"type": "string", "description": "Text content for a text node"},
                    "inverted": {"type": "boolean", "description": "Whether a mask inverts its geometry"},
                },
            }),
        },
        CommandDef {
            id: "node.remove",
            label: "Remove node",
            description: "Detach a node from its parent.",
            inputs: json!({
                "type": "object",
                "required": ["id"],
                "properties": {"id": {"type": "string", "description": "Node id from inspect_document"}},
            }),
        },
        CommandDef {
            id: "node.move",
            label: "Move node",
            description: "Reparent a node to a new parent at a new index.",
            inputs: json!({
                "type": "object",
                "required": ["id", "parent"],
                "properties": {
                    "id": {"type": "string"},
                    "parent": {"type": "object", "description": "'node' or 'comp' key with an id from inspect_document"},
                    "index": {"type": "integer"},
                },
            }),
        },
        CommandDef {
            id: "node.rename",
            label: "Rename node",
            description: "Set a node's name.",
            inputs: json!({
                "type": "object",
                "required": ["id", "name"],
                "properties": {"id": {"type": "string"}, "name": {"type": "string"}},
            }),
        },
        CommandDef {
            id: "node.flags",
            label: "Set node flags",
            description: "Toggle a node's visibility or locked state. Omit a field to leave it alone.",
            inputs: json!({
                "type": "object",
                "required": ["id"],
                "properties": {
                    "id": {"type": "string"},
                    "visible": {"type": "boolean"},
                    "locked": {"type": "boolean"},
                },
            }),
        },
        CommandDef {
            id: "node.group",
            label: "Group nodes",
            description: "Group two or more nodes under a new group node in the main composition.",
            inputs: json!({
                "type": "object",
                "required": ["ids"],
                "properties": {"ids": {"type": "array", "items": {"type": "string"}, "minItems": 2}},
            }),
        },
        CommandDef {
            id: "property.set",
            label: "Set property",
            description: "Write a static value to a property. Numbers for scalars, [x, y] for positions and sizes, true/false for booleans, #rrggbbaa for colours.",
            inputs: json!({
                "type": "object",
                "required": ["id", "prop", "value"],
                "properties": {
                    "id": {"type": "string"},
                    "prop": {"type": "string", "description": "Dotted path, e.g. transform.position"},
                    "value": {"description": "The value, matched against the property's kind"},
                },
            }),
        },
        CommandDef {
            id: "property.key",
            label: "Add keyframe",
            description: "Key a property at a frame, for animated values.",
            inputs: json!({
                "type": "object",
                "required": ["id", "prop", "value"],
                "properties": {
                    "id": {"type": "string"},
                    "prop": {"type": "string"},
                    "frame": {"type": "integer", "description": "Frame index; default: the playhead"},
                    "value": {"description": "The value, matched against the property's kind"},
                },
            }),
        },
        CommandDef {
            id: "property.removeKeyframe",
            label: "Remove keyframe",
            description: "Drop the keyframe a property carries at a frame.",
            inputs: json!({
                "type": "object",
                "required": ["id", "prop"],
                "properties": {
                    "id": {"type": "string"},
                    "prop": {"type": "string"},
                    "frame": {"type": "integer"},
                },
            }),
        },
        CommandDef {
            id: "shape.reverse",
            label: "Reverse path",
            description: "Reverse the direction a path is drawn in.",
            inputs: json!({
                "type": "object",
                "required": ["id"],
                "properties": {"id": {"type": "string"}},
            }),
        },
        CommandDef {
            id: "composition.set",
            label: "Set composition",
            description: "Rename the main composition.",
            inputs: json!({
                "type": "object",
                "required": ["id"],
                "properties": {"id": {"type": "string"}, "name": {"type": "string"}},
            }),
        },
    ]
}

/// Answer for `list_commands`, filtered by the optional substring.
pub fn list(filter: Option<&str>) -> Value {
    let filter = filter.unwrap_or("").to_ascii_lowercase();
    json!({
        "commands": commands()
            .into_iter()
            .filter(|c| filter.is_empty()
                || c.id.to_ascii_lowercase().contains(&filter)
                || c.label.to_ascii_lowercase().contains(&filter))
            .map(|c| json!({
                "id": c.id,
                "label": c.label,
                "description": c.description,
                "inputs": c.inputs,
            }))
            .collect::<Vec<_>>(),
    })
}

/// One MCP tool.
pub struct ToolDef {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub schema: Value,
    pub read_only: bool,
}

/// Every MCP tool, in `tools/list` order.
pub fn tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "list_commands",
            title: "List commands",
            description: "List the commands that edit a project: id, label, description, and the inputs each one takes. Every editing action is a command; run any of them with run_command.",
            schema: json!({"type": "object", "properties": {"filter": {"type": "string", "description": "Case-insensitive substring matched against id and label"}}}),
            read_only: true,
        },
        ToolDef {
            name: "run_command",
            title: "Run command",
            description: "Apply edits as one undo step: {label, commands: [{id, inputs}]}. Each command's inputs come from list_commands. Answers how many applied.",
            schema: json!({
                "type": "object",
                "required": ["commands"],
                "properties": {
                    "label": {"type": "string", "description": "Undo-step label"},
                    "commands": {"type": "array", "minItems": 1, "items": {
                        "type": "object",
                        "required": ["id"],
                        "properties": {"id": {"type": "string"}, "inputs": {"type": "object"}}
                    }}
                }
            }),
            read_only: false,
        },
        ToolDef {
            name: "open_project",
            title: "Open project",
            description: "Open a .ren (RON text) or .renb (postcard binary) project from disk.",
            schema: json!({"type": "object", "required": ["path"], "properties": {"path": {"type": "string"}}}),
            read_only: false,
        },
        ToolDef {
            name: "new_project",
            title: "New project",
            description: "Start a new project from a built-in template. List them with the templates tool.",
            schema: json!({"type": "object", "properties": {"template": {"type": "string", "description": "Template slug; default "ellipse""}}}),
            read_only: false,
        },
        ToolDef {
            name: "templates",
            title: "Templates",
            description: "The built-in project templates: slug, name, and description.",
            schema: json!({"type": "object", "properties": {}}),
            read_only: true,
        },
        ToolDef {
            name: "inspect_document",
            title: "Inspect document",
            description: "Summary of the open project: compositions with size, rate and frame range, node/clip/machine counts, the active machine's inputs, and the playhead. Drill into parts with the renamite:// resources.",
            schema: json!({"type": "object", "properties": {
                "include_nodes": {"type": "boolean", "description": "Include a per-node id/name/kind/bounds list"},
                "limit": {"type": "integer", "description": "Cap on nodes when include_nodes is set; 0 = all"}
            }}),
            read_only: true,
        },
        ToolDef {
            name: "save_project",
            title: "Save project",
            description: "Write the project to disk. Without path it writes where it was opened from; an error when it never was. A .renb extension writes binary.",
            schema: json!({"type": "object", "properties": {"path": {"type": "string", "description": "Destination; defaults to the opened path"}}}),
            read_only: false,
        },
        ToolDef {
            name: "render_frame",
            title: "Render frame",
            description: "Render the current frame, or one given, to a PNG file. This is how to look at the result.",
            schema: json!({"type": "object", "required": ["path"], "properties": {
                "path": {"type": "string"},
                "frame": {"type": "number", "description": "Frame to render; default: the playhead"},
                "width": {"type": "integer", "description": "Output width in px; default 512"},
                "height": {"type": "integer", "description": "Output height in px; default 512"},
                "background": {"type": "string", "description": "transparent | white | black | #rrggbbaa"}
            }}),
            read_only: false,
        },
        ToolDef {
            name: "export",
            title: "Export",
            description: "Export the project, or one frame, to SVG (vector), Lottie, or .renb. SVG and Lottie report what they could not represent.",
            schema: json!({"type": "object", "required": ["path"], "properties": {
                "path": {"type": "string"},
                "format": {"type": "string", "description": "svg | lottie | renb | ren, when the path has no useful extension"},
                "frame": {"type": "number", "description": "Frame to export for SVG; default: the playhead"},
                "strict": {"type": "boolean", "description": "Fail when the exporter emits compatibility warnings"}
            }}),
            read_only: false,
        },
        ToolDef {
            name: "import",
            title: "Import",
            description: "Import an SVG or Lottie file, replacing the open project.",
            schema: json!({"type": "object", "required": ["path"], "properties": {
                "path": {"type": "string"},
                "strict": {"type": "boolean", "description": "Fail when the importer drops something"}
            }}),
            read_only: false,
        },
        ToolDef {
            name: "validate",
            title: "Validate",
            description: "Validate the project and report every diagnostic as error, warning or info. fix normalizes in place.",
            schema: json!({"type": "object", "properties": {
                "fix": {"type": "boolean", "description": "Normalize the project in place when fixable"},
                "warnings_as_errors": {"type": "boolean", "description": "Report invalid when there are warnings"}
            }}),
            read_only: true,
        },
        ToolDef {
            name: "bake",
            title: "Bake frames",
            description: "Evaluate a frame range to scenes and answer them as JSON. Runs without a renderer.",
            schema: json!({"type": "object", "properties": {
                "frames": {"type": "integer", "description": "How many frames to bake; default 1"},
                "dt": {"type": "number", "description": "Seconds per frame; default 1 / the composition rate"},
                "from": {"type": "number", "description": "First frame; default: the playhead"}
            }}),
            read_only: false,
        },
        ToolDef {
            name: "scrub",
            title: "Scrub",
            description: "Move the playhead to an absolute frame without playing.",
            schema: json!({"type": "object", "required": ["frame"], "properties": {"frame": {"type": "number"}}}),
            read_only: false,
        },
        ToolDef {
            name: "tick",
            title: "Tick",
            description: "Advance playback by dt seconds and answer the new frame and the events the player emitted.",
            schema: json!({"type": "object", "properties": {"dt": {"type": "number", "description": "Seconds to advance; default: one frame at the composition rate"}}}),
            read_only: false,
        },
        ToolDef {
            name: "play",
            title: "Play",
            description: "Play, pause or resume timeline playback, and set whether it loops.",
            schema: json!({"type": "object", "properties": {
                "action": {"type": "string", "enum": ["play", "pause"]},
                "loop": {"type": "boolean", "description": "Set timeline looping"}
            }}),
            read_only: false,
        },
        ToolDef {
            name: "machines",
            title: "Machines",
            description: "The project's machines and the start machine's inputs, with each input's kind. Drives interactive playback.",
            schema: json!({"type": "object", "properties": {}}),
            read_only: true,
        },
        ToolDef {
            name: "machine_input",
            title: "Machine input",
            description: "Set a machine input, or fire a machine event with a null value. The player must be in machine playback.",
            schema: json!({"type": "object", "required": ["name"], "properties": {
                "name": {"type": "string"},
                "value": {"description": "Number, boolean, or null to fire an event"}
            }}),
            read_only: false,
        },
        ToolDef {
            name: "pointer",
            title: "Pointer",
            description: "Send a pointer event to the project's machine listeners, in document coordinates.",
            schema: json!({"type": "object", "required": ["stage", "x", "y"], "properties": {
                "stage": {"type": "string", "enum": ["move", "down", "up", "leave"]},
                "x": {"type": "number"},
                "y": {"type": "number"}
            }}),
            read_only: false,
        },
        ToolDef {
            name: "undo",
            title: "Undo",
            description: "Undo the last undo step.",
            schema: json!({"type": "object", "properties": {}}),
            read_only: false,
        },
        ToolDef {
            name: "redo",
            title: "Redo",
            description: "Redo the last undone step.",
            schema: json!({"type": "object", "properties": {}}),
            read_only: false,
        },
        ToolDef {
            name: "session",
            title: "Session",
            description: "State of this server: what is open, unsaved changes, undo/redo availability, and the selection.",
            schema: json!({"type": "object", "properties": {}}),
            read_only: true,
        },
    ]
}

/// The tool list as `tools/list` answers it.
pub fn tool_definitions() -> Vec<Value> {
    tools()
        .into_iter()
        .map(|t| json!({
            "name": t.name,
            "title": t.title,
            "description": t.description,
            "inputSchema": t.schema,
            "annotations": {"title": t.title, "readOnlyHint": t.read_only, "openWorldHint": false},
        }))
        .collect()
}
