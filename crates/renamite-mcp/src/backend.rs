//! Where tool calls land: a headless renamite session in this process.
//!
//! Unlike the desktop craft apps, renamite has no `--control` channel yet, so
//! there is no `Remote` backend and no `--connect` flag: everything runs in
//! process. The trait stays so adding a control channel later is one impl.

use crate::session::McpSession;

/// Something that answers control-channel methods.
pub trait Backend {
    fn session(&mut self) -> &mut McpSession;
    /// True when a real UI is attached. Never here, but kept so the trait's
    /// shape matches the other craft apps.
    fn has_ui(&self) -> bool {
        false
    }
    fn describe(&self) -> String {
        "headless".into()
    }
}

/// The in-process session this server wraps.
pub struct Headless {
    pub session: McpSession,
}

impl Default for Headless {
    fn default() -> Self {
        Self::new()
    }
}

impl Headless {
    pub fn new() -> Self {
        Self { session: McpSession::new() }
    }
}

impl Backend for Headless {
    fn session(&mut self) -> &mut McpSession {
        &mut self.session
    }
}
