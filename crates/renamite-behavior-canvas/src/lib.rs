//! Canvas tool behaviors - pure state machines over `ToolContext`.

use glam::DVec2;
use renamite_behavior_common::ToolContext;
use renamite_history::OutputVec;
use smallvec::smallvec;

#[derive(Clone, Debug)]
pub enum CanvasEvent {
    PointerDown { pos: DVec2, button: PointerButton },
    PointerMove { pos: DVec2 },
    PointerUp { pos: DVec2, button: PointerButton },
    KeyDown(Key),
    KeyUp(Key),
    DoubleClick { pos: DVec2 },
    Scroll { delta: DVec2, pos: DVec2 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerButton {
    Primary,
    Secondary,
    Middle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Escape,
    Shift,
    Alt,
    Control,
    Enter,
    Backspace,
    Delete,
    A,
    D,
    P,
    V,
}

pub struct ToolBehavior {
    state: SelectState,
}

/// Overlay primitives rendered in screen space by the viewport.
#[derive(Clone, Debug)]
pub enum ToolOverlay {
    None,
    RubberBand { start: DVec2, end: DVec2 },
    PreviewPath(renamite_geometry::VectorPath),
}

impl ToolBehavior {
    pub fn handle(
        &mut self,
        _ctx: &ToolContext,
        _ev: CanvasEvent,
    ) -> OutputVec {
        smallvec![]
    }

    pub fn overlay(&self, _ctx: &ToolContext) -> ToolOverlay {
        ToolOverlay::None
    }

    pub fn on_deactivate(&mut self) -> OutputVec {
        smallvec![]
    }
}

impl Default for ToolBehavior {
    fn default() -> Self {
        Self { state: SelectState::Idle }
    }
}

enum SelectState {
    Idle,
    RubberBand { start: DVec2 },
    ClickPending,
    DragMove { start: DVec2, orig: Vec<(renamite_model::NodeId, DVec2)> },
    DragRotate { pivot: DVec2, start_angle: f64, accumulated: f64 },
    DragScale,
}

/// Multi-turn rotation (Glaxnimate 0.6): unwrap delta against `accumulated`
/// by ±TAU so 3 turns = 1080°, not 0°.
#[allow(dead_code)]
fn unwrap_delta(delta: f64, accumulated: f64) -> f64 {
    let mut d = delta - accumulated;
    while d > std::f64::consts::TAU {
        d -= std::f64::consts::TAU;
    }
    while d < -std::f64::consts::TAU {
        d += std::f64::consts::TAU;
    }
    accumulated + d
}

/// Snap an angle to increments when shift is held.
#[allow(dead_code)]
fn snap_angle(angle: f64, shift: bool) -> f64 {
    if !shift {
        return angle;
    }
    let step = 15.0_f64.to_radians();
    (angle / step).round() * step
}