//! JSON-RPC 2.0 framing and the MCP lifecycle / tools methods.

use serde_json::{Value, json};

use crate::session::Session;
use crate::tools::{call_tool, tool_definitions};

pub const PROTOCOL_VERSION: &str = "2025-06-18";
const SUPPORTED_VERSIONS: &[&str] = &[PROTOCOL_VERSION, "2025-03-26", "2024-11-05"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

const INSTRUCTIONS: &str = "renamite is a vector motion rig editor. Documents are .ren (RON text) or \
.renb (packed binary); a document holds compositions, and a composition holds a node tree of groups, \
shapes and paint (style) children. Coordinates are design units, y down, origin at the composition's \
top-left; a shape's pos is its centre and size its diameter. Node ids are 0-based indices into the \
active composition's subtree in document order, and every mutating tool returns the ids it touched, so \
read them back before the next edit. Draw with draw_shape, color with set_paint, move with transform, \
and look at the result with render_png. project_save writes .ren (or .renb by extension).";

pub struct Server {
    session: Session,
    initialized: bool,
}

fn response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

impl Server {
    pub fn new(session: Session) -> Self {
        Self {
            session,
            initialized: false,
        }
    }

    pub fn session(&mut self) -> &mut Session {
        &mut self.session
    }

    /// Handle one protocol message; returns zero or more replies. Notifications
    /// (no `id`) produce none, and `tools/call` results carry isError flags.
    pub fn handle_line(&mut self, line: &str) -> Vec<String> {
        let parsed: Result<Value, _> = serde_json::from_str(line);
        let Ok(message) = parsed else {
            return vec![error(Value::Null, PARSE_ERROR, "parse error").to_string()];
        };
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return vec![error(Value::Null, INVALID_REQUEST, "missing method").to_string()];
        };
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let notification = id.is_none();

        let result = match method {
            "initialize" => {
                let asked = params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or(PROTOCOL_VERSION);
                let version = if SUPPORTED_VERSIONS.contains(&asked) {
                    asked
                } else {
                    PROTOCOL_VERSION
                };
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "renamite", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS,
                }))
            }
            "notifications/initialized" | "initialized" => {
                self.initialized = true;
                return Vec::new();
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tool_definitions()})),
            "tools/call" => self.call_tool(&params),
            _ => Err((METHOD_NOT_FOUND, format!("unknown method {method}"))),
        };

        match result {
            Ok(value) => vec![response(id.unwrap_or(Value::Null), value).to_string()],
            Err((code, message)) => {
                if notification {
                    Vec::new()
                } else {
                    vec![error(id.unwrap_or(Value::Null), code, message).to_string()]
                }
            }
        }
    }

    fn call_tool(&mut self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or((INVALID_PARAMS, "tools/call needs a name".to_string()))?;
        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
        let outcome = call_tool(&mut self.session, name, &arguments)
            .map_err(|message| (INTERNAL_ERROR, message))?;
        let text = match &outcome {
            crate::tools::ToolResult::Ok(value) => value.to_string(),
            crate::tools::ToolResult::Err(message) => message.clone(),
        };
        Ok(json!({
            "content": [{"type": "text", "text": text}],
            "isError": matches!(outcome, crate::tools::ToolResult::Err(_)),
        }))
    }
}
