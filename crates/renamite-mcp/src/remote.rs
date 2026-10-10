//! Remote mode: the MCP server drives a running editor over its control
//! channel instead of hosting a headless session of its own. Same tools, same
//! replies; the document is the one on screen.

use std::io::{BufRead, BufReader, Write};

use serde_json::{Value, json};

use crate::tools::tool_definitions;

/// Serve newline-delimited JSON-RPC on stdio, forwarding `tools/call` to the
/// editor at `addr`. Lifecycle methods are answered locally.
pub fn serve(addr: &str) -> std::io::Result<()> {
    let mut editor = EditorLink::connect(addr)?;
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Some(reply) = handle(&mut editor, &line) else {
            continue;
        };
        writeln!(out, "{reply}")?;
        out.flush()?;
    }
    Ok(())
}

struct EditorLink {
    stream: std::net::TcpStream,
    next_id: u64,
}

impl EditorLink {
    fn connect(addr: &str) -> std::io::Result<Self> {
        let stream = std::net::TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;
        Ok(Self { stream, next_id: 1 })
    }

    /// One control call: a line out, a line back. The editor answers
    /// `{"ok": true, "result": ...}` or `{"ok": false, "error": {...}}`.
    fn call(&mut self, method: &str, params: &Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({"id": id, "method": method, "params": params});
        writeln!(&self.stream, "{request}").map_err(|error| error.to_string())?;
        self.stream.flush().map_err(|error| error.to_string())?;
        let mut line = String::new();
        let mut reader = BufReader::new(&self.stream);
        reader
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        let reply: Value =
            serde_json::from_str(line.trim()).map_err(|error| format!("bad reply: {error}"))?;
        match reply.get("ok").and_then(Value::as_bool) {
            Some(true) => Ok(reply.get("result").cloned().unwrap_or(Value::Null)),
            _ => Err(reply
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("the editor refused the call")
                .to_string()),
        }
    }
}

/// Handle one protocol line; `None` for notifications, which carry no reply.
fn handle(editor: &mut EditorLink, line: &str) -> Option<String> {
    let message: Value = serde_json::from_str(line).ok()?;
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    if id.is_null() {
        return None;
    }
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let reply = match method {
        "initialize" => json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "renamite (remote)", "version": env!("CARGO_PKG_VERSION")},
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tool_definitions()}),
        "tools/call" => {
            let name = message
                .get("params")
                .and_then(|params| params.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let arguments = message
                .get("params")
                .and_then(|params| params.get("arguments"))
                .cloned()
                .unwrap_or(json!({}));
            let text = match editor.call(name, &arguments) {
                Ok(value) => value.to_string(),
                Err(message) => message,
            };
            return Some(
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{"type": "text", "text": text}],
                        "isError": !text.starts_with('{') && text.contains("error"),
                    },
                })
                .to_string(),
            );
        }
        other => {
            return Some(
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("unknown method {other}")}})
                    .to_string(),
            );
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": reply}).to_string())
}
