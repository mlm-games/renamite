//! Repose Material shell + panels for renamite.
//!
//! App shell wires reactive state into a docked editor layout:
//! TopAppBar / ToolRail / DockHost (Viewport, Layers, Properties, Timeline,
//! Curves, Assets) / StatusBar.

use renamite_behavior_common::{Selection, ViewTransform};
use renamite_history::History;
use renamite_model::Document;
use renamite_platform::Platform;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    /// Revision bumps on `History::commit`.
    pub doc: Arc<Document>,
    pub history: Arc<History>,
    pub selection: Arc<Selection>,
    pub playhead: Arc<renamite_animation::Playback>,
    pub active_tool: Arc<u8>,
    pub record: Arc<bool>,
    pub view: Arc<ViewTransform>,
    pub comp_stack: Arc<Vec<renamite_model::CompId>>,
}

impl AppState {
    pub fn new(doc: Document) -> Self {
        Self {
            doc: Arc::new(doc),
            history: Arc::new(History::new()),
            selection: Arc::new(Selection::default()),
            playhead: Arc::new(renamite_animation::Playback {
                state: renamite_animation::PlayState::Stopped,
                head: 0.0,
                loop_mode: renamite_animation::LoopMode::Once,
                range: (
                    renamite_animation::Frame(0),
                    renamite_animation::Frame(60),
                ),
                dir: 1.0,
            }),
            active_tool: Arc::new(0),
            record: Arc::new(false),
            view: Arc::new(ViewTransform::identity()),
            comp_stack: Arc::new(Vec::new()),
        }
    }
}

pub struct EditorApp<P: Platform> {
    pub state: AppState,
    pub platform: P,
}

/// Entry point called by the runner. Builds and runs the editor UI loop.
pub fn run(_state: AppState) {
    // TODO: compose Material shell, ToolRail, DockHost, and panels.
}