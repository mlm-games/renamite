//! The editor's control channel: a loopback JSON-lines listener that drives
//! the running editor, so an agent can edit the live document and watch it
//! change. Mirrors the craft apps' `--control` port.
//!
//! The listener thread owns the socket only; document edits are applied on
//! the UI thread by draining the queue, because the editor's session is
//! `!Send`. Edits go straight to the model and bypass the editor's undo
//! history, which is a control-channel property, not a bug.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::ops::HandleTable;
use crate::tools;

/// One request waiting for the UI thread, with the channel its reply rides.
pub struct ControlCall {
    pub id: Value,
    pub method: String,
    pub params: Value,
    pub reply: Sender<String>,
}

/// The queue shared between the listener thread and the UI thread.
#[derive(Clone, Default)]
pub struct ControlChannel {
    queue: Arc<Mutex<VecDeque<ControlCall>>>,
}

impl ControlChannel {
    /// Listen on `addr` (`127.0.0.1:7979`) until the process exits. Requests
    /// are queued for the UI thread; the reply is written from there.
    pub fn spawn(addr: &str) -> std::io::Result<Self> {
        let channel = Self::default();
        let listener = TcpListener::bind(addr)?;
        let queue = channel.queue.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let queue = queue.clone();
                std::thread::spawn(move || serve_connection(stream, queue));
            }
        });
        Ok(channel)
    }

    /// Take everything queued since the last call. The UI thread drains this
    /// once per frame.
    pub fn take(&self) -> Vec<ControlCall> {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        let mut calls: Vec<ControlCall> = queue.drain(..).collect();
        calls.reverse();
        calls
    }
}

fn serve_connection(stream: std::net::TcpStream, queue: Arc<Mutex<VecDeque<ControlCall>>>) {
    let peer = stream.peer_addr().ok();
    let mut writer = stream;
    let reader = match writer.try_clone() {
        Ok(clone) => clone,
        Err(_) => return,
    };
    log::info!("renamite control: client {peer:?} connected");
    for line in BufReader::new(reader).lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        let call = match serde_json::from_str::<Value>(&line) {
            Ok(message) => ControlCall {
                id: message.get("id").cloned().unwrap_or(Value::Null),
                method: message
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                params: message.get("params").cloned().unwrap_or(json!({})),
                reply: reply_tx,
            },
            Err(error) => {
                let _ = writeln!(
                    writer,
                    "{}",
                    error_json(Value::Null, -32700, &format!("parse error: {error}"))
                );
                continue;
            }
        };
        let method = call.method.clone();
        queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(call);
        // The reply rides back from the UI thread through the channel.
        match reply_rx.recv() {
            Ok(reply) => {
                if writeln!(writer, "{reply}").is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
        if method == "quit" {
            break;
        }
    }
    log::info!("renamite control: client {peer:?} left");
}

fn error_json(id: Value, code: i64, message: &str) -> String {
    json!({"ok": false, "id": id, "error": {"code": code, "message": message}}).to_string()
}

/// Apply one control call to the live document. Tool names are the MCP tool
/// names, minus the player-backed ones the editor drives itself.
pub fn control_apply(
    file: &mut renamite_io_ren::RenFile,
    handles: &mut HandleTable,
    method: &str,
    params: &Value,
) -> String {
    let outcome = tools::dispatch_document(file, handles, method, params);
    match outcome {
        Ok(value) => json!({"ok": true, "result": value}).to_string(),
        Err(message) => error_json(json!({"method": method}), -32603, &message),
    }
}
