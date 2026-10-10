//! renamite's MCP server.
//!
//! [Model Context Protocol](https://modelcontext.io) over stdio:
//! newline-delimited JSON-RPC 2.0, hand-written (no async runtime). The
//! server exposes the headless document session as MCP tools so an agent can
//! author, inspect, validate and render rigs without the editor window.
//!
//! Entry points: [`serve`] (stdio loop) and [`Server::handle_line`] (one
//! message).
#![forbid(unsafe_code)]

mod server;
mod session;
mod tools;

pub use server::{PROTOCOL_VERSION, Server};
pub use session::Session;
pub use tools::{ToolResult, call_tool, tool_definitions};

/// Serve newline-delimited JSON-RPC until `input` closes. Logs go to stderr
/// only; stdout carries the protocol stream.
pub fn serve() -> std::io::Result<()> {
    use std::io::BufRead;
    let mut server = Server::new(Session::new());
    let mut out = std::io::BufWriter::new(std::io::stdout());
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        for response in server.handle_line(&line) {
            use std::io::Write;
            writeln!(out, "{response}")?;
            out.flush()?;
        }
    }
    Ok(())
}
