//! Timeline behaviors: ruler scrub, multi-key drag, easing curve editing.

use renamite_animation::Frame;
use renamite_history::{OutputVec, ToolOutput};

#[derive(Clone, Debug)]
pub struct TimelineEvent {
    pub pos: f64,
    pub button: PointerButton,
    pub kind: EventKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerButton {
    Primary,
    Secondary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Press,
    Move,
    Release,
    DoubleClick,
}

/// Alt+click cycles easing presets (Glaxnimate 0.6 behavior).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EasingPreset {
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
    Anticipate,
    Overshoot,
    Hold,
}

pub struct TimelineScrubBehavior {
    pub rate: renamite_animation::FrameRate,
    pub range: (Frame, Frame),
}

pub struct TimelineKeyframeBehavior {
    pub rate: renamite_animation::FrameRate,
    pub range: (Frame, Frame),
    pub drag_start: Option<(Frame, Frame)>,
}

pub struct EasingCurveBehavior;

impl TimelineScrubBehavior {
    pub fn new(rate: renamite_animation::FrameRate) -> Self {
        Self { rate, range: (Frame(0), Frame(0)) }
    }

    pub fn handle(&mut self, ev: &TimelineEvent) -> OutputVec {
        let x = ev.pos;
        let frame = self.rate.secs_to_frames(x) as i64;
        match ev.kind {
            EventKind::Press | EventKind::Move => {
                let mut out: OutputVec = Default::default();
                out.push(ToolOutput::SetPlayhead(frame as f64));
                out
            }
            _ => Default::default(),
        }
    }
}

impl Default for TimelineKeyframeBehavior {
    fn default() -> Self {
        Self {
            rate: renamite_animation::FrameRate { num: 30, den: 1 },
            range: (Frame(0), Frame(60)),
            drag_start: None,
        }
    }
}

impl TimelineKeyframeBehavior {
    pub fn handle(&mut self, _ev: &TimelineEvent) -> OutputVec {
        // TODO: multi-key drag -> MoveKeyframes (one transaction), box-select.
        Default::default()
    }
}

impl EasingCurveBehavior {
    pub fn handle(&mut self, _ev: &TimelineEvent) -> OutputVec {
        // TODO: drag ease_in/out; x clamped [0, 1].
        Default::default()
    }
}