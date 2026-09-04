//! Time and tweening. Integer frames internally - no FP drift on scrub/import.

use glam::DVec2;
use renamite_geometry::{Anchor, VectorPath};
use serde::{Deserialize, Serialize};

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Frame(pub i64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameRate {
    pub num: u32,
    pub den: u32,
}

impl FrameRate {
    pub fn fps(self) -> f64 {
        self.num as f64 / self.den.max(1) as f64
    }
    pub fn secs_to_frames(self, secs: f64) -> f64 {
        secs * self.fps()
    }
    pub fn frames_to_secs(self, frames: f64) -> f64 {
        frames / self.fps()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Interpolation {
    Hold,
    Linear,
    CubicBezier,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct EasingHandle {
    pub x: f64,
    pub y: f64,
}

impl EasingHandle {
    pub const LINEAR_OUT: Self = Self {
        x: 1.0 / 3.0,
        y: 1.0 / 3.0,
    };
    pub const LINEAR_IN: Self = Self {
        x: 2.0 / 3.0,
        y: 2.0 / 3.0,
    };
    pub fn clamped(self) -> Self {
        Self {
            x: self.x.clamp(0.0, 1.0),
            y: self.y,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Keyframe<T> {
    pub frame: Frame,
    pub value: T,
    /// Applies to the segment *after* this key (Lottie-compatible).
    pub interpolation: Interpolation,
    /// This key outgoing (Lottie `o`).
    pub ease_out: EasingHandle,
    /// Next key incoming (Lottie `i`) - stored on the left key of the segment.
    pub ease_in: EasingHandle,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Animated<T> {
    pub base: T,
    /// Invariant: sorted by frame, unique frames.
    pub keyframes: Vec<Keyframe<T>>,
}

/// Alt+click on a timeline key cycles these (Glaxnimate 0.6 behavior).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EasingPreset {
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
    Anticipate,
    Overshoot,
    Hold,
}

impl EasingPreset {
    /// (interpolation, ease_out, ease_in) for the segment after a key.
    pub fn segment(self) -> (Interpolation, EasingHandle, EasingHandle) {
        use EasingPreset::*;
        let h = |x, y| EasingHandle { x, y };
        match self {
            Linear => (
                Interpolation::Linear,
                EasingHandle::LINEAR_OUT,
                EasingHandle::LINEAR_IN,
            ),
            EaseIn => (Interpolation::CubicBezier, h(0.42, 0.0), h(1.0, 1.0)),
            EaseOut => (Interpolation::CubicBezier, h(0.0, 0.0), h(0.58, 1.0)),
            EaseInOut => (Interpolation::CubicBezier, h(0.42, 0.0), h(0.58, 1.0)),
            Anticipate => (Interpolation::CubicBezier, h(0.5, -0.3), h(0.8, 1.0)),
            Overshoot => (Interpolation::CubicBezier, h(0.2, 0.0), h(0.5, 1.3)),
            Hold => (
                Interpolation::Hold,
                EasingHandle::LINEAR_OUT,
                EasingHandle::LINEAR_IN,
            ),
        }
    }

    pub fn next(self) -> Self {
        use EasingPreset::*;
        match self {
            Linear => EaseIn,
            EaseIn => EaseOut,
            EaseOut => EaseInOut,
            EaseInOut => Anticipate,
            Anticipate => Overshoot,
            Overshoot => Hold,
            Hold => Linear,
        }
    }

    /// Detect which preset (if any) a segment currently matches.
    pub fn detect(i: Interpolation, o: EasingHandle, e: EasingHandle) -> Option<Self> {
        use EasingPreset::*;
        let eq =
            |a: EasingHandle, b: EasingHandle| (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6;
        for p in [
            Linear, EaseIn, EaseOut, EaseInOut, Anticipate, Overshoot, Hold,
        ] {
            let (pi, po, pe) = p.segment();
            if pi == i && (i == Interpolation::Hold || (eq(po, o) && eq(pe, e))) {
                return Some(p);
            }
        }
        None
    }
}

pub trait Tween: Clone {
    fn tween(a: &Self, b: &Self, t: f64) -> Self;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEdit {
    Added,
    Updated,
}

/// Progress through a segment: maps normalized time u∈[0,1] to eased t∈[0,1].
/// This is the single easing implementation shared by `Animated<T>` and clip tracks.
pub fn ease_progress(i: Interpolation, out: EasingHandle, inn: EasingHandle, u: f64) -> f64 {
    match i {
        Interpolation::Hold => 0.0,
        Interpolation::Linear => u,
        Interpolation::CubicBezier => {
            let t = solve_cubic_x(out.x, inn.x, u);
            cubic_at(t, out.y, inn.y)
        }
    }
}

impl<T: Tween> Animated<T> {
    pub fn new(base: T) -> Self {
        Self {
            base,
            keyframes: Vec::new(),
        }
    }

    pub fn value_at(&self, frame: f64) -> T {
        let ks = &self.keyframes;
        if ks.is_empty() {
            return self.base.clone();
        }
        if frame <= ks[0].frame.0 as f64 {
            return ks[0].value.clone();
        }
        let last = ks.len() - 1;
        if frame >= ks[last].frame.0 as f64 {
            return ks[last].value.clone();
        }
        let i = ks.partition_point(|k| (k.frame.0 as f64) <= frame) - 1;
        let (a, b) = (&ks[i], &ks[i + 1]);
        let span = (b.frame.0 - a.frame.0) as f64;
        let u = (frame - a.frame.0 as f64) / span;
        let y = ease_progress(a.interpolation, a.ease_out, a.ease_in, u);
        T::tween(&a.value, &b.value, y)
    }

    /// Insert-or-update. Updating keeps the key's existing easing.
    pub fn set_key(&mut self, frame: Frame, value: T) -> KeyEdit {
        match self.keyframes.binary_search_by_key(&frame, |k| k.frame) {
            Ok(i) => {
                self.keyframes[i].value = value;
                KeyEdit::Updated
            }
            Err(i) => {
                let (interpolation, ease_out, ease_in) = EasingPreset::Linear.segment();
                self.keyframes.insert(
                    i,
                    Keyframe {
                        frame,
                        value,
                        interpolation,
                        ease_out,
                        ease_in,
                    },
                );
                KeyEdit::Added
            }
        }
    }

    pub fn remove_key(&mut self, frame: Frame) -> Option<Keyframe<T>> {
        match self.keyframes.binary_search_by_key(&frame, |k| k.frame) {
            Ok(i) => Some(self.keyframes.remove(i)),
            Err(_) => None,
        }
    }

    /// Fails (returns false) if `to` is occupied by another key.
    pub fn move_key(&mut self, from: Frame, to: Frame) -> bool {
        if from == to {
            return true;
        }
        if self.key_at(to).is_some() {
            return false;
        }
        let Some(mut k) = self.remove_key(from) else {
            return false;
        };
        k.frame = to;
        let i = self.keyframes.partition_point(|x| x.frame < to);
        self.keyframes.insert(i, k);
        true
    }

    /// Returns the previous easing triple, if the key exists.
    pub fn set_easing(
        &mut self,
        frame: Frame,
        interpolation: Interpolation,
        ease_out: EasingHandle,
        ease_in: EasingHandle,
    ) -> Option<(Interpolation, EasingHandle, EasingHandle)> {
        let i = self
            .keyframes
            .binary_search_by_key(&frame, |k| k.frame)
            .ok()?;
        let k = &mut self.keyframes[i];
        let old = (k.interpolation, k.ease_out, k.ease_in);
        k.interpolation = interpolation;
        k.ease_out = ease_out.clamped();
        k.ease_in = ease_in.clamped();
        Some(old)
    }

    pub fn has_keys(&self) -> bool {
        !self.keyframes.is_empty()
    }
    pub fn key_at(&self, frame: Frame) -> Option<&Keyframe<T>> {
        self.keyframes
            .binary_search_by_key(&frame, |k| k.frame)
            .ok()
            .map(|i| &self.keyframes[i])
    }
}

fn cubic_at(t: f64, p1: f64, p2: f64) -> f64 {
    let mt = 1.0 - t;
    3.0 * mt * mt * t * p1 + 3.0 * mt * t * t * p2 + t * t * t
}
fn cubic_deriv(t: f64, p1: f64, p2: f64) -> f64 {
    let mt = 1.0 - t;
    3.0 * mt * mt * p1 + 6.0 * mt * t * (p2 - p1) + 3.0 * t * t * (1.0 - p2)
}
/// Newton with bisection fallback: find t with x(t) = u.
fn solve_cubic_x(x1: f64, x2: f64, u: f64) -> f64 {
    let (x1, x2) = (x1.clamp(0.0, 1.0), x2.clamp(0.0, 1.0));
    let mut t = u;
    for _ in 0..8 {
        let err = cubic_at(t, x1, x2) - u;
        if err.abs() < 1e-7 {
            return t;
        }
        let d = cubic_deriv(t, x1, x2);
        if d.abs() < 1e-7 {
            break;
        }
        t = (t - err / d).clamp(0.0, 1.0);
    }
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..32 {
        t = 0.5 * (lo + hi);
        if cubic_at(t, x1, x2) < u {
            lo = t
        } else {
            hi = t
        }
    }
    t
}

impl Tween for f64 {
    fn tween(a: &Self, b: &Self, t: f64) -> Self {
        a + (b - a) * t
    }
}
impl Tween for DVec2 {
    fn tween(a: &Self, b: &Self, t: f64) -> Self {
        *a + (*b - *a) * t
    }
}
impl Tween for glam::DVec4 {
    fn tween(a: &Self, b: &Self, t: f64) -> Self {
        *a + (*b - *a) * t
    }
}

/// Degrees; lerp WITHOUT modulo (preserves multi-turn values (like glax)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Angle(pub f64);
impl Tween for Angle {
    fn tween(a: &Self, b: &Self, t: f64) -> Self {
        Angle(a.0 + (b.0 - a.0) * t)
    }
}

/// Topology-safe path tween: equal anchor count + same closed -> per-anchor lerp;
/// otherwise Hold (Lottie behavior).
impl Tween for VectorPath {
    fn tween(a: &Self, b: &Self, t: f64) -> Self {
        if a.anchors.len() != b.anchors.len() || a.closed != b.closed {
            return if t < 1.0 { a.clone() } else { b.clone() };
        }
        VectorPath {
            closed: a.closed,
            anchors: a
                .anchors
                .iter()
                .zip(&b.anchors)
                .map(|(x, y)| Anchor {
                    pos: x.pos + (y.pos - x.pos) * t,
                    tan_in: x.tan_in + (y.tan_in - x.tan_in) * t,
                    tan_out: x.tan_out + (y.tan_out - x.tan_out) * t,
                    mode: x.mode,
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnimatedTransform {
    pub anchor: Animated<DVec2>,
    pub position: Animated<DVec2>,
    /// 100,100 = 100%.
    pub scale: Animated<DVec2>,
    pub rotation: Animated<Angle>,
    pub skew: Animated<f64>,
    pub skew_axis: Animated<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransformSample {
    pub anchor: DVec2,
    pub position: DVec2,
    pub scale: DVec2,
    pub rotation_deg: f64,
    pub skew: f64,
    pub skew_axis: f64,
}

impl AnimatedTransform {
    pub fn identity() -> Self {
        Self {
            anchor: Animated::new(DVec2::ZERO),
            position: Animated::new(DVec2::ZERO),
            scale: Animated::new(DVec2::splat(100.0)),
            rotation: Animated::new(Angle(0.0)),
            skew: Animated::new(0.0),
            skew_axis: Animated::new(0.0),
        }
    }
    pub fn sample(&self, frame: f64) -> TransformSample {
        TransformSample {
            anchor: self.anchor.value_at(frame),
            position: self.position.value_at(frame),
            scale: self.scale.value_at(frame),
            rotation_deg: self.rotation.value_at(frame).0,
            skew: self.skew.value_at(frame),
            skew_axis: self.skew_axis.value_at(frame),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayState {
    Stopped,
    Playing,
    Scrubbing,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopMode {
    Once,
    Loop,
    PingPong,
}

#[derive(Clone, Copy)]
pub struct Playback {
    pub state: PlayState,
    pub head: f64,
    pub loop_mode: LoopMode,
    pub range: (Frame, Frame),
    /// +1.0 / -1.0 for ping-pong.
    pub dir: f64,
}

impl Playback {
    pub fn stopped(range: (Frame, Frame)) -> Self {
        Self {
            state: PlayState::Stopped,
            head: range.0.0 as f64,
            loop_mode: LoopMode::Loop,
            range,
            dir: 1.0,
        }
    }

    /// Returns true if the visual head moved (redraw without doc dirty).
    pub fn advance(&mut self, dt_secs: f64, rate: FrameRate) -> bool {
        if self.state != PlayState::Playing {
            return false;
        }
        let (s, e) = (self.range.0.0 as f64, self.range.1.0 as f64);
        if e <= s {
            return false;
        }
        self.head += self.dir * dt_secs * rate.fps();
        match self.loop_mode {
            LoopMode::Once => {
                if self.head >= e {
                    self.head = e;
                    self.state = PlayState::Stopped;
                }
                if self.head < s {
                    self.head = s;
                }
            }
            LoopMode::Loop => {
                self.head = s + (self.head - s).rem_euclid(e - s);
            }
            LoopMode::PingPong => {
                let mut guard = 0;
                while (self.head > e || self.head < s) && guard < 8 {
                    if self.head > e {
                        self.head = 2.0 * e - self.head;
                        self.dir = -self.dir;
                    }
                    if self.head < s {
                        self.head = 2.0 * s - self.head;
                        self.dir = -self.dir;
                    }
                    guard += 1;
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anim() -> Animated<f64> {
        let mut a = Animated::new(0.0);
        a.set_key(Frame(0), 0.0);
        a.set_key(Frame(60), 100.0);
        a
    }

    #[test]
    fn endpoints_and_linear_midpoint() {
        let a = anim();
        assert_eq!(a.value_at(-5.0), 0.0);
        assert_eq!(a.value_at(65.0), 100.0);
        assert!((a.value_at(30.0) - 50.0).abs() < 1e-9);
    }

    #[test]
    fn hold_holds() {
        let mut a = anim();
        a.set_easing(
            Frame(0),
            Interpolation::Hold,
            EasingHandle::LINEAR_OUT,
            EasingHandle::LINEAR_IN,
        );
        assert_eq!(a.value_at(59.9), 0.0);
        assert_eq!(a.value_at(60.0), 100.0);
    }

    #[test]
    fn cubic_monotone_endpoints() {
        let mut a = anim();
        let (i, o, e) = EasingPreset::EaseInOut.segment();
        a.set_easing(Frame(0), i, o, e);
        assert!((a.value_at(0.0)).abs() < 1e-9);
        assert!((a.value_at(60.0) - 100.0).abs() < 1e-9);
        assert!(a.value_at(30.0) > 0.0 && a.value_at(30.0) < 100.0);
    }

    #[test]
    fn move_key_collision_fails() {
        let mut a = anim();
        assert!(!a.move_key(Frame(0), Frame(60)));
        assert!(a.move_key(Frame(0), Frame(10)));
        assert_eq!(a.keyframes[0].frame, Frame(10));
    }

    #[test]
    fn preset_roundtrip() {
        for p in [
            EasingPreset::Linear,
            EasingPreset::EaseInOut,
            EasingPreset::Hold,
        ] {
            let (i, o, e) = p.segment();
            assert_eq!(EasingPreset::detect(i, o, e), Some(p));
        }
    }
}
