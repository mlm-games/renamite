//! Clips (named timelines) + state machines. Pure, deterministic, and WASM-safe.
//!
//! A `Clip` is a bag of (NodeId, PropPath) -> keyframe tracks. They are the same
//! `KeyframeData` the history system already uses, so clip authoring reuses
//! `EditorCommand` semantics later. A `Machine` turns inputs into an
//! `Overrides` patch per tick. The host feeds that to `evaluate_with`.
//!
//! State-machine semantics (exit time, any-state, trigger consumption,
//! crossfade) are implemented here from first principles / public
//! documentation of how such runtimes behave generally (though no one asked).

use renamite_animation::{Frame, LoopMode, Tween, ease_progress};
use renamite_geometry::VectorPath;
use renamite_model::{KeyframeData, NodeId, Overrides, PropPath, Value};
use serde::{Deserialize, Serialize};
use slotmap::SlotMap;
use std::collections::HashMap;

slotmap::new_key_type! { pub struct ClipId; pub struct MachineId; }
pub type ClipMap = SlotMap<ClipId, Clip>;
pub type MachineMap = SlotMap<MachineId, Machine>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub name: String,
    pub range: (Frame, Frame),
    pub tracks: Vec<Track>,
    /// Named events fired when the playhead crosses `frame`.
    pub events: Vec<EventKey>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub node: NodeId,
    pub prop: PropPath,
    /// Invariant: sorted by frame, unique frames.
    pub keys: Vec<KeyframeData>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventKey {
    pub frame: Frame,
    pub name: String,
}

/// Tween two Values of the same variant; Hold otherwise (mirrors path rules).
pub fn value_tween(a: &Value, b: &Value, t: f64) -> Value {
    use Value::*;
    let t = if t.is_finite() { t } else { 0.0 };
    match (a, b) {
        (F64(x), F64(y)) => F64(f64::tween(x, y, t)),
        (DVec2(x), DVec2(y)) => DVec2(glam::DVec2::tween(x, y, t)),
        (Angle(x), Angle(y)) => Angle(renamite_animation::Angle::tween(x, y, t)),
        (Color(x), Color(y)) => Color(renamite_model::Color::tween(x, y, t)),
        (Path(x), Path(y)) => Path(VectorPath::tween(x, y, t)),
        _ => {
            if t < 1.0 {
                a.clone()
            } else {
                b.clone()
            }
        }
    }
}

/// Scale a numeric Value by `s`; `None` for variants with no scale.
pub fn value_scale(v: &Value, s: f64) -> Option<Value> {
    use Value::*;
    let s = if s.is_finite() { s } else { 0.0 };
    Some(match v {
        F64(x) => F64(x * s),
        DVec2(x) => DVec2(*x * s),
        Angle(a) => Angle(renamite_animation::Angle(a.0 * s)),
        I64(i) => I64((*i as f64 * s).round() as i64),
        Color(c) => Color(renamite_model::Color {
            r: c.r * s,
            g: c.g * s,
            b: c.b * s,
            a: c.a * s,
        }),
        _ => return None,
    })
}

/// Component-add two Values; `None` when their variants differ or lack an add.
pub fn value_add(a: &Value, b: &Value) -> Option<Value> {
    use Value::*;
    Some(match (a, b) {
        (F64(x), F64(y)) => F64(x + y),
        (DVec2(x), DVec2(y)) => DVec2(*x + *y),
        (Angle(x), Angle(y)) => Angle(renamite_animation::Angle(x.0 + y.0)),
        (I64(x), I64(y)) => I64(x + y),
        (Color(x), Color(y)) => Color(renamite_model::Color {
            r: x.r + y.r,
            g: x.g + y.g,
            b: x.b + y.b,
            a: x.a + y.a,
        }),
        _ => return None,
    })
}

impl Track {
    pub fn value_at(&self, frame: f64) -> Option<Value> {
        let ks = &self.keys;
        if ks.is_empty() {
            return None;
        }
        if !frame.is_finite() {
            return Some(ks[0].value.clone());
        }
        if frame <= ks[0].frame.0 as f64 {
            return Some(ks[0].value.clone());
        }
        let last = ks.len() - 1;
        if frame >= ks[last].frame.0 as f64 {
            return Some(ks[last].value.clone());
        }
        let i = ks.partition_point(|k| (k.frame.0 as f64) <= frame) - 1;
        let (a, b) = (&ks[i], &ks[i + 1]);
        let span = (b.frame.0 as f64) - (a.frame.0 as f64);
        if !span.is_finite() || span <= 0.0 {
            return Some(a.value.clone());
        }
        let u = ((frame - a.frame.0 as f64) / span).clamp(0.0, 1.0);
        let y = ease_progress(a.interpolation, a.ease_out, a.ease_in, u);
        Some(value_tween(&a.value, &b.value, y))
    }
}

impl Clip {
    pub fn len_frames(&self) -> f64 {
        self.range.1.0.saturating_sub(self.range.0.0).max(1) as f64
    }

    /// Map layer-local time to a clip frame; returns (frame, normalized 0..1).
    pub fn local(&self, time: f64, loop_mode: LoopMode) -> (f64, f64) {
        let (s, len) = (self.range.0.0 as f64, self.len_frames());
        if !time.is_finite() {
            return (s, 0.0);
        }
        let t = match loop_mode {
            LoopMode::Once => time.clamp(0.0, len),
            LoopMode::Loop => time.rem_euclid(len),
            LoopMode::PingPong => {
                let c = time.rem_euclid(2.0 * len);
                if c > len { 2.0 * len - c } else { c }
            }
        };
        (s + t, t / len)
    }

    pub fn sample_into(&self, frame: f64, out: &mut HashMap<(NodeId, PropPath), Value>) {
        for tr in &self.tracks {
            if let Some(v) = tr.value_at(frame) {
                out.insert((tr.node, tr.prop.clone()), v);
            }
        }
    }

    /// Root/hips delta for `node` between two layer-local times, using the
    /// node's `transform.position` / `transform.rotation` tracks and the same
    /// `speed` / `loop_mode` mapping as playback. For a looping clip the delta
    /// is the plain difference between the wrapped samples.
    pub fn root_motion_delta(
        &self,
        node: NodeId,
        prev_time: f64,
        time: f64,
        speed: f64,
        loop_mode: LoopMode,
    ) -> RootMotion {
        let speed = finite_nonnegative(speed);
        let prev = if prev_time.is_finite() {
            prev_time * speed
        } else {
            0.0
        };
        let cur = if time.is_finite() { time * speed } else { prev };
        let (prev_frame, _) = self.local(prev, loop_mode);
        let (cur_frame, _) = self.local(cur, loop_mode);
        let pos = |frame: f64| match self.track_value(node, "transform.position", frame) {
            Some(Value::DVec2(v)) => v,
            _ => glam::DVec2::ZERO,
        };
        let rot = |frame: f64| match self.track_value(node, "transform.rotation", frame) {
            Some(Value::Angle(a)) => a.0,
            _ => 0.0,
        };
        RootMotion {
            position: pos(cur_frame) - pos(prev_frame),
            rotation_deg: rot(cur_frame) - rot(prev_frame),
        }
    }

    fn track_value(&self, node: NodeId, prop: &str, frame: f64) -> Option<Value> {
        self.tracks
            .iter()
            .find(|t| t.node == node && t.prop.as_str() == prop)
            .and_then(|t| t.value_at(frame))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Machine {
    pub name: String,
    pub inputs: Vec<InputDef>,
    pub layers: Vec<MachineLayer>,
    /// Pointer interactions on scene nodes → input actions. Picking is free:
    /// `SceneItem` already carries the shape `NodeId`.
    pub listeners: Vec<Listener>,
    /// Root/hips node whose transform is reported as root motion and removed
    /// from the pose. `None` keeps every track in the pose.
    #[serde(default)]
    pub root_motion: Option<NodeId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputDef {
    pub name: String,
    pub kind: InputKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum InputKind {
    Bool { default: bool },
    Number { default: f64 },
    Trigger,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MachineLayer {
    pub name: String,
    pub states: Vec<State>,
    pub entry: usize,
    /// Checked before per-state transitions, from any state (Rive-style Any).
    pub any_transitions: Vec<Transition>,
    /// How this layer combines with the layers below it.
    #[serde(default)]
    pub blend: LayerBlend,
}

/// Layer composition mode. `Override` replaces lower layers; `Additive`
/// adds its state's values (scaled by a Number input) onto them.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
pub enum LayerBlend {
    #[default]
    Override,
    Additive {
        /// Number input used as the additive weight.
        weight: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub name: String,
    pub kind: StateKind,
    pub transitions: Vec<Transition>,
    #[serde(default)]
    pub graph_pos: Option<(f64, f64)>,
    /// States sharing a non-empty group keep a shared normalized phase across
    /// a transition (walk/run style).
    #[serde(default)]
    pub sync_group: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum StateKind {
    /// Play one clip.
    Clip {
        clip: ClipId,
        speed: f64,
        loop_mode: LoopMode,
    },
    /// 1D blend across clips by a Number input (walk/run style).
    Blend1D {
        input: usize,
        children: Vec<BlendChild>,
    },
    /// 2D blend across clips positioned in the input plane. Weights come from
    /// a Delaunay triangulation of the child positions: barycentric inside a
    /// triangle, segment projection on the nearest edge otherwise.
    Blend2D {
        input_x: usize,
        input_y: usize,
        children: Vec<BlendPoint2D>,
    },
    /// No animation (rest pose = document values).
    Empty,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlendChild {
    pub threshold: f64,
    pub clip: ClipId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlendPoint2D {
    pub pos: (f64, f64),
    pub clip: ClipId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub to: usize,
    /// Crossfade length in frames (0 = hard cut).
    pub duration: f64,
    /// Require normalized state time >= this before firing (None = anytime).
    pub exit_time: Option<f64>,
    /// AND-combined. Empty + exit_time = "when finished".
    pub conditions: Vec<Condition>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Condition {
    BoolIs { input: usize, value: bool },
    NumberCmp { input: usize, op: CmpOp, value: f64 },
    Triggered { input: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Listener {
    pub node: NodeId,
    pub event: PointerEventKind,
    pub action: ListenerAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PointerEventKind {
    Down,
    Up,
    Click,
    Enter,
    Exit,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ListenerAction {
    SetBool { input: usize, value: bool },
    ToggleBool { input: usize },
    SetNumber { input: usize, value: f64 },
    FireTrigger { input: usize },
}

fn finite_nonnegative(value: f64) -> f64 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn additive_weight(inputs: &[InputValue], weight: usize) -> f64 {
    match inputs.get(weight) {
        Some(InputValue::Number(n)) if n.is_finite() => *n,
        _ => 0.0,
    }
}

/// Compose one additive-layer value onto `out`, scaling by `weight`.
/// Non-scalable or type-mismatched values fall back to an override.
fn apply_additive(out: &mut Overrides, node: NodeId, prop: PropPath, value: Value, weight: f64) {
    let Some(delta) = value_scale(&value, weight) else {
        out.set(node, prop, value);
        return;
    };
    let merged = match out.get(node, prop.as_str()).cloned() {
        Some(base) => value_add(&base, &delta).unwrap_or(delta),
        None => delta,
    };
    out.set(node, prop, merged);
}

/// Normalized phase (0..1) of a state that carries a clip chain.
fn state_phase(state: &State, clips: &ClipMap, time: f64) -> Option<f64> {
    let (clip_id, speed, loop_mode) = match &state.kind {
        StateKind::Clip {
            clip,
            speed,
            loop_mode,
        } => (*clip, *speed, *loop_mode),
        StateKind::Blend1D { children, .. } => (children.first()?.clip, 1.0, LoopMode::Loop),
        StateKind::Blend2D { children, .. } => (children.first()?.clip, 1.0, LoopMode::Loop),
        StateKind::Empty => return None,
    };
    let clip = clips.get(clip_id)?;
    let speed = finite_nonnegative(speed);
    let time = if time.is_finite() { time * speed } else { 0.0 };
    Some(clip_phase(clip, time, loop_mode))
}

/// Layer-local time that puts `state` at normalized `phase`.
fn state_time_for_phase(state: &State, clips: &ClipMap, phase: f64) -> Option<f64> {
    let (clip_id, speed, _) = match &state.kind {
        StateKind::Clip {
            clip,
            speed,
            loop_mode,
        } => (*clip, *speed, *loop_mode),
        StateKind::Blend1D { children, .. } => (children.first()?.clip, 1.0, LoopMode::Loop),
        StateKind::Blend2D { children, .. } => (children.first()?.clip, 1.0, LoopMode::Loop),
        StateKind::Empty => return None,
    };
    let clip = clips.get(clip_id)?;
    let speed = finite_nonnegative(speed);
    if speed <= 0.0 {
        return Some(0.0);
    }
    let phase = if phase.is_finite() {
        phase.clamp(0.0, 1.0)
    } else {
        0.0
    };
    Some(phase * clip.len_frames() / speed)
}

/// Layer-local time for `to` that matches `from`'s phase, when both share a
/// non-empty sync group.
fn sync_aligned_time(
    from: &State,
    to: &State,
    clips: &ClipMap,
    from_time: f64,
) -> Option<f64> {
    let from_group = from.sync_group.as_deref()?;
    let to_group = to.sync_group.as_deref()?;
    if from_group.is_empty() || from_group != to_group {
        return None;
    }
    let phase = state_phase(from, clips, from_time)?;
    state_time_for_phase(to, clips, phase)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InputValue {
    Bool(bool),
    Number(f64),
    Trigger { fired: bool },
}

#[derive(Clone, Debug)]
struct LayerRt {
    current: usize,
    state_names: Vec<String>,
    state_kinds: Vec<StateKind>,
    /// Frames spent in current state.
    time: f64,
    fade: Option<Fade>,
    entered: bool,
}

#[derive(Clone, Debug)]
struct Fade {
    from: usize,
    from_time: f64,
    t: f64,
    duration: f64,
}

#[derive(Clone, Debug)]
pub struct MachineInstance {
    pub inputs: Vec<InputValue>,
    layers: Vec<LayerRt>,
    layer_names: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct TickOutput {
    pub events: Vec<String>,
    /// Root/hips delta for this tick when the machine declares one.
    pub root_motion: RootMotion,
}

/// Delta of a root/hips node's transform over one tick. The caller applies it
/// to a world transform instead of using the node's own pose.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RootMotion {
    /// Position delta in the node's transform space.
    pub position: glam::DVec2,
    /// Rotation delta in degrees.
    pub rotation_deg: f64,
}

impl RootMotion {
    pub fn is_zero(&self) -> bool {
        self.position == glam::DVec2::ZERO && self.rotation_deg == 0.0
    }
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum MachineError {
    #[error("unknown input `{0}`")]
    UnknownInput(String),
    #[error("input type mismatch for `{0}`")]
    InputType(String),
}

impl MachineInstance {
    pub fn new(m: &Machine) -> Self {
        let mut instance = Self {
            inputs: Vec::new(),
            layers: Vec::new(),
            layer_names: Vec::new(),
        };
        instance.sync_runtime(m);
        instance
    }

    fn sync_runtime(&mut self, m: &Machine) {
        self.inputs.resize(m.inputs.len(), InputValue::Bool(false));
        for (index, definition) in m.inputs.iter().enumerate() {
            let valid = matches!(
                (&self.inputs[index], definition.kind),
                (InputValue::Bool(_), InputKind::Bool { .. })
                    | (InputValue::Number(_), InputKind::Number { .. })
                    | (InputValue::Trigger { .. }, InputKind::Trigger)
            );
            if !valid {
                self.inputs[index] = default_input(definition);
            }
            if let InputValue::Number(value) = &mut self.inputs[index]
                && !value.is_finite()
            {
                *value = 0.0;
            }
        }

        let old_layers = std::mem::take(&mut self.layers);
        let old_names = std::mem::take(&mut self.layer_names);
        let mut used = vec![false; old_layers.len()];
        let mut layers = Vec::with_capacity(m.layers.len());
        for (index, layer) in m.layers.iter().enumerate() {
            let old_index = old_names
                .iter()
                .enumerate()
                .filter(|(old, name)| !used[*old] && *name == &layer.name)
                .map(|(old, _)| old)
                .next()
                .or_else(|| {
                    old_layers
                        .iter()
                        .enumerate()
                        .find(|(old, _)| !used[*old] && *old == index)
                        .map(|(old, _)| old)
                });
            let mut runtime = old_index
                .and_then(|old| {
                    used[old] = true;
                    old_layers.get(old).cloned()
                })
                .unwrap_or_else(|| new_layer_runtime(layer));
            let old_state_names = runtime.state_names.clone();
            let old_state_kinds = runtime.state_kinds.clone();
            let current =
                remap_state_index(&old_state_names, &old_state_kinds, runtime.current, layer);
            let exact_current =
                state_index_matches(&old_state_names, &old_state_kinds, runtime.current, layer);
            if runtime.current >= layer.states.len()
                || !runtime.time.is_finite()
                || runtime.time < 0.0
                || !exact_current
            {
                runtime.current = current;
                runtime.time = 0.0;
                runtime.fade = None;
                runtime.entered = false;
            } else {
                runtime.current = current;
            }
            if let Some(fade) = &mut runtime.fade {
                let old_from = fade.from;
                let mapped = remap_state_index(&old_state_names, &old_state_kinds, old_from, layer);
                if state_index_matches(&old_state_names, &old_state_kinds, old_from, layer) {
                    fade.from = mapped;
                } else {
                    runtime.fade = None;
                }
            }
            runtime.state_names = layer
                .states
                .iter()
                .map(|state| state.name.clone())
                .collect();
            runtime.state_kinds = layer
                .states
                .iter()
                .map(|state| state.kind.clone())
                .collect();
            layers.push(runtime);
        }
        self.layers = layers;
        self.layer_names = m.layers.iter().map(|layer| layer.name.clone()).collect();
    }

    pub fn input_index(m: &Machine, name: &str) -> Option<usize> {
        m.inputs.iter().position(|i| i.name == name)
    }
    /// Current active state per layer, in layer order.
    pub fn layer_states(&self) -> impl Iterator<Item = usize> + '_ {
        self.layers.iter().map(|layer| layer.current)
    }
    pub fn set_bool(&mut self, idx: usize, v: bool) {
        if let Some(InputValue::Bool(b)) = self.inputs.get_mut(idx) {
            *b = v;
        }
    }
    pub fn set_number(&mut self, idx: usize, v: f64) {
        if let Some(InputValue::Number(n)) = self.inputs.get_mut(idx) {
            *n = if v.is_finite() { v } else { 0.0 };
        }
    }
    pub fn fire(&mut self, idx: usize) {
        if let Some(InputValue::Trigger { fired }) = self.inputs.get_mut(idx) {
            *fired = true;
        }
    }

    /// Route a pointer event on `node` (from Scene picking) through listeners.
    pub fn pointer_event(&mut self, m: &Machine, node: NodeId, kind: PointerEventKind) {
        for l in m
            .listeners
            .iter()
            .filter(|l| l.node == node && l.event == kind)
        {
            match l.action {
                ListenerAction::SetBool { input, value } => self.set_bool(input, value),
                ListenerAction::ToggleBool { input } => {
                    if let Some(InputValue::Bool(b)) = self.inputs.get_mut(input) {
                        *b = !*b;
                    }
                }
                ListenerAction::SetNumber { input, value } => self.set_number(input, value),
                ListenerAction::FireTrigger { input } => self.fire(input),
            }
        }
    }

    /// Advance all layers by dt (frames), writing the merged Overrides patch.
    /// Triggers are frame-scoped: consumed by firing transitions, cleared at end.
    pub fn tick(
        &mut self,
        m: &Machine,
        clips: &ClipMap,
        dt_frames: f64,
        out: &mut Overrides,
    ) -> TickOutput {
        let mut output = TickOutput::default();
        if !dt_frames.is_finite() || dt_frames < 0.0 {
            return output;
        }
        self.sync_runtime(m);
        for (li, layer) in m.layers.iter().enumerate() {
            if layer.states.is_empty() {
                continue;
            }
            let Some(rt) = self.layers.get_mut(li) else {
                continue;
            };
            let current = rt.current.min(layer.states.len() - 1);
            rt.current = current;
            let prev_time = rt.time;
            let advanced_time = prev_time + dt_frames;
            if !advanced_time.is_finite() {
                continue;
            }
            rt.time = advanced_time;
            if let Some(f) = &mut rt.fade {
                f.from_time += dt_frames;
                f.t = if f.duration <= 0.0 {
                    1.0
                } else {
                    (f.t + dt_frames / f.duration).min(1.0)
                };
                if f.t >= 1.0 || !f.t.is_finite() || !f.from_time.is_finite() {
                    rt.fade = None;
                }
            }

            // transitions: Any first, then current state's, first match wins
            let state = &layer.states[current];
            let progress = state_progress(state, clips, prev_time, advanced_time);
            let fired = layer
                .any_transitions
                .iter()
                .chain(state.transitions.iter())
                .find(|tr| {
                    transition_target_valid(tr, layer.states.len())
                        && transition_ready(tr, &self.inputs, progress)
                        && (tr.to != current
                            || tr
                                .conditions
                                .iter()
                                .any(|c| matches!(c, Condition::Triggered { .. })))
                })
                .cloned();
            let old_entered = rt.entered;
            let mut transitioned_from: Option<(usize, f64, f64)> = None;
            if let Some(tr) = fired {
                transitioned_from = Some((current, prev_time, advanced_time));
                let duration = finite_nonnegative(tr.duration);
                rt.fade = (duration > 0.0).then_some(Fade {
                    from: current,
                    from_time: prev_time,
                    t: 0.0,
                    duration,
                });
                rt.current = tr.to;
                rt.time = 0.0;
                rt.entered = false;
                if let Some(aligned) = sync_aligned_time(
                    &layer.states[current],
                    &layer.states[tr.to],
                    clips,
                    prev_time,
                ) {
                    rt.time = aligned;
                }
            }
            // Keep a fade's target in phase with its source until it ends.
            if let Some(f) = rt.fade.clone()
                && let Some(aligned) = sync_aligned_time(
                    &layer.states[f.from],
                    &layer.states[rt.current],
                    clips,
                    f.from_time,
                )
            {
                rt.time = aligned;
            }

            // sample
            let mut b = HashMap::new();
            let mut evs = Vec::new();
            if let Some((from_idx, from_prev, from_cur)) = transitioned_from {
                let mut old_values = HashMap::new();
                sample_state(
                    &layer.states[from_idx],
                    clips,
                    &self.inputs,
                    from_prev,
                    from_cur,
                    &mut old_values,
                    &mut evs,
                    old_entered,
                );
            }
            let sample_prev = if transitioned_from.is_some() {
                rt.time
            } else {
                prev_if_same(prev_time, rt.time)
            };
            if li == 0
                && let Some(root) = m.root_motion
                && let StateKind::Clip {
                    clip,
                    speed,
                    loop_mode,
                } = &layer.states[rt.current].kind
                && let Some(c) = clips.get(*clip)
            {
                output.root_motion =
                    c.root_motion_delta(root, sample_prev, rt.time, *speed, *loop_mode);
            }
            sample_state(
                &layer.states[rt.current],
                clips,
                &self.inputs,
                sample_prev,
                rt.time,
                &mut b,
                &mut evs,
                !rt.entered,
            );
            rt.entered = true;
            if let Some(f) = rt.fade.clone() {
                let mut a = HashMap::new();
                let mut fade_evs = Vec::new();
                let fade_prev = (f.from_time - dt_frames).max(0.0);
                sample_state(
                    &layer.states[f.from],
                    clips,
                    &self.inputs,
                    fade_prev,
                    f.from_time,
                    &mut a,
                    &mut fade_evs,
                    false,
                );
                if transitioned_from.is_none() {
                    evs.append(&mut fade_evs);
                }
                for (k, va) in a {
                    let merged = match b.get(&k) {
                        Some(vb) => value_tween(&va, vb, f.t),
                        None => va,
                    };
                    b.insert(k, merged);
                }
            }
            if let Some(root) = m.root_motion {
                b.remove(&(root, PropPath::new("transform.position")));
                b.remove(&(root, PropPath::new("transform.rotation")));
            }
            match layer.blend {
                LayerBlend::Override => {
                    for (k, v) in b {
                        out.set(k.0, k.1, v);
                    }
                }
                LayerBlend::Additive { weight } => {
                    let weight = additive_weight(&self.inputs, weight);
                    for (k, v) in b {
                        apply_additive(out, k.0, k.1, v, weight);
                    }
                }
            }
            output.events.append(&mut evs);
        }
        // frame-scoped triggers
        for i in &mut self.inputs {
            if let InputValue::Trigger { fired } = i {
                *fired = false;
            }
        }
        output
    }

    pub fn reevaluate(&mut self, m: &Machine, clips: &ClipMap, out: &mut Overrides) -> TickOutput {
        let mut output = TickOutput::default();
        self.sync_runtime(m);
        for (li, layer) in m.layers.iter().enumerate() {
            if layer.states.is_empty() {
                continue;
            }
            let Some(rt) = self.layers.get(li) else {
                continue;
            };
            let mut b = HashMap::new();
            sample_state(
                &layer.states[rt.current],
                clips,
                &self.inputs,
                rt.time,
                rt.time,
                &mut b,
                &mut output.events,
                false,
            );
            if let Some(f) = rt.fade.clone() {
                let mut a = HashMap::new();
                let mut ignored_events = Vec::new();
                sample_state(
                    &layer.states[f.from],
                    clips,
                    &self.inputs,
                    f.from_time,
                    f.from_time,
                    &mut a,
                    &mut ignored_events,
                    false,
                );
                for (k, va) in a {
                    let merged = match b.get(&k) {
                        Some(vb) => value_tween(&va, vb, f.t),
                        None => va,
                    };
                    b.insert(k, merged);
                }
            }
            if let Some(root) = m.root_motion {
                b.remove(&(root, PropPath::new("transform.position")));
                b.remove(&(root, PropPath::new("transform.rotation")));
            }
            match layer.blend {
                LayerBlend::Override => {
                    for (k, v) in b {
                        out.set(k.0, k.1, v);
                    }
                }
                LayerBlend::Additive { weight } => {
                    let weight = additive_weight(&self.inputs, weight);
                    for (k, v) in b {
                        apply_additive(out, k.0, k.1, v, weight);
                    }
                }
            }
        }
        output
    }
}

fn new_layer_runtime(layer: &MachineLayer) -> LayerRt {
    LayerRt {
        current: valid_state_index(layer),
        state_names: layer
            .states
            .iter()
            .map(|state| state.name.clone())
            .collect(),
        state_kinds: layer
            .states
            .iter()
            .map(|state| state.kind.clone())
            .collect(),
        time: 0.0,
        fade: None,
        entered: false,
    }
}

fn unique_index<T: PartialEq>(values: &[T], value: &T) -> Option<usize> {
    let mut found = None;
    for (index, candidate) in values.iter().enumerate() {
        if candidate == value {
            if found.is_some() {
                return None;
            }
            found = Some(index);
        }
    }
    found
}

fn state_index_matches(
    old_names: &[String],
    old_kinds: &[StateKind],
    old_index: usize,
    layer: &MachineLayer,
) -> bool {
    if old_index < old_names.len()
        && old_names.len() == layer.states.len()
        && old_kinds.len() == layer.states.len()
        && old_names
            .iter()
            .zip(&layer.states)
            .all(|(name, state)| name == &state.name)
        && old_kinds
            .iter()
            .zip(&layer.states)
            .all(|(kind, state)| kind == &state.kind)
    {
        return true;
    }
    let Some(old_name) = old_names.get(old_index) else {
        return false;
    };
    if unique_index(old_names, old_name).is_some()
        && layer
            .states
            .iter()
            .filter(|state| &state.name == old_name)
            .count()
            == 1
    {
        return true;
    }
    let Some(old_kind) = old_kinds.get(old_index) else {
        return false;
    };
    unique_index(old_kinds, old_kind).is_some()
        && layer
            .states
            .iter()
            .filter(|state| &state.kind == old_kind)
            .count()
            == 1
}

fn remap_state_index(
    old_names: &[String],
    old_kinds: &[StateKind],
    old_index: usize,
    layer: &MachineLayer,
) -> usize {
    if layer.states.is_empty() {
        return 0;
    }
    if old_index < old_names.len()
        && old_names.len() == layer.states.len()
        && old_kinds.len() == layer.states.len()
        && old_names
            .iter()
            .zip(&layer.states)
            .all(|(name, state)| name == &state.name)
        && old_kinds
            .iter()
            .zip(&layer.states)
            .all(|(kind, state)| kind == &state.kind)
    {
        return old_index;
    }
    if let Some(name) = old_names.get(old_index)
        && unique_index(old_names, name).is_some()
        && let Some(index) = layer
            .states
            .iter()
            .position(|state| &state.name == name)
            .filter(|_| {
                layer
                    .states
                    .iter()
                    .filter(|state| &state.name == name)
                    .count()
                    == 1
            })
    {
        return index;
    }
    if let Some(kind) = old_kinds.get(old_index)
        && unique_index(old_kinds, kind).is_some()
        && let Some(index) = layer
            .states
            .iter()
            .position(|state| &state.kind == kind)
            .filter(|_| {
                layer
                    .states
                    .iter()
                    .filter(|state| &state.kind == kind)
                    .count()
                    == 1
            })
    {
        return index;
    }
    old_index.min(layer.states.len() - 1)
}

fn default_input(definition: &InputDef) -> InputValue {
    match definition.kind {
        InputKind::Bool { default } => InputValue::Bool(default),
        InputKind::Number { default } => {
            InputValue::Number(if default.is_finite() { default } else { 0.0 })
        }
        InputKind::Trigger => InputValue::Trigger { fired: false },
    }
}

fn valid_state_index(layer: &MachineLayer) -> usize {
    layer.entry.min(layer.states.len().saturating_sub(1))
}

fn transition_target_valid(transition: &Transition, state_count: usize) -> bool {
    transition.to < state_count
}

fn prev_if_same(prev: f64, cur: f64) -> f64 {
    if cur < prev { 0.0 } else { prev }
}

#[derive(Clone, Copy, Debug)]
struct StateProgress {
    previous: f64,
    current: f64,
    reached_one: bool,
}

fn state_progress(state: &State, clips: &ClipMap, previous: f64, current: f64) -> StateProgress {
    match &state.kind {
        StateKind::Clip {
            clip,
            speed,
            loop_mode,
        } => clips
            .get(*clip)
            .map(|clip| clip_progress(clip, previous, current, *loop_mode, *speed))
            .unwrap_or(StateProgress {
                previous: 0.0,
                current: 0.0,
                reached_one: false,
            }),
        StateKind::Blend1D { children, .. } => children
            .iter()
            .find_map(|child| clips.get(child.clip))
            .map(|clip| clip_progress(clip, previous, current, LoopMode::Loop, 1.0))
            .unwrap_or(StateProgress {
                previous: 0.0,
                current: 0.0,
                reached_one: false,
            }),
        StateKind::Blend2D { children, .. } => children
            .iter()
            .find_map(|child| clips.get(child.clip))
            .map(|clip| clip_progress(clip, previous, current, LoopMode::Loop, 1.0))
            .unwrap_or(StateProgress {
                previous: 0.0,
                current: 0.0,
                reached_one: false,
            }),
        StateKind::Empty => StateProgress {
            previous: 1.0,
            current: 1.0,
            reached_one: true,
        },
    }
}

fn clip_progress(
    clip: &Clip,
    previous: f64,
    current: f64,
    loop_mode: LoopMode,
    speed: f64,
) -> StateProgress {
    let speed = finite_nonnegative(speed);
    let previous = if previous.is_finite() {
        previous * speed
    } else {
        0.0
    };
    let current = if current.is_finite() {
        current * speed
    } else {
        previous
    };
    let length = clip.len_frames();
    if length <= 0.0 || !length.is_finite() {
        return StateProgress {
            previous: 1.0,
            current: 1.0,
            reached_one: true,
        };
    }
    let phase = |time: f64| clip_phase(clip, time, loop_mode);
    let (previous_phase, current_phase) = (phase(previous), phase(current));
    let reached_one = match loop_mode {
        LoopMode::Once => current >= length && previous < length,
        LoopMode::Loop | LoopMode::PingPong => {
            current >= previous && (current / length).floor() > (previous / length).floor()
        }
    };
    StateProgress {
        previous: previous_phase,
        current: current_phase,
        reached_one,
    }
}

fn clip_phase(clip: &Clip, time: f64, loop_mode: LoopMode) -> f64 {
    let length = clip.len_frames();
    if !time.is_finite() || length <= 0.0 || !length.is_finite() {
        return 0.0;
    }
    let phase = match loop_mode {
        LoopMode::Once => time.clamp(0.0, length),
        LoopMode::Loop => {
            let remainder = time.rem_euclid(length);
            if time >= length && remainder == 0.0 {
                length
            } else {
                remainder
            }
        }
        LoopMode::PingPong => {
            let cycle = time.rem_euclid(2.0 * length);
            if cycle > length {
                2.0 * length - cycle
            } else {
                cycle
            }
        }
    };
    (phase / length).clamp(0.0, 1.0)
}

fn transition_ready(tr: &Transition, inputs: &[InputValue], progress: StateProgress) -> bool {
    if let Some(et) = tr.exit_time {
        if !et.is_finite() || !(0.0..=1.0).contains(&et) {
            return false;
        }
        if !progress.reached_one && progress.previous < et && progress.current < et {
            return false;
        }
    }
    if tr.conditions.is_empty() && tr.exit_time.is_none() {
        return false;
    }
    tr.conditions.iter().all(|c| match *c {
        Condition::BoolIs { input, value } => {
            matches!(inputs.get(input), Some(InputValue::Bool(b)) if *b == value)
        }
        Condition::NumberCmp { input, op, value } => match inputs.get(input) {
            Some(InputValue::Number(n)) if n.is_finite() && value.is_finite() => match op {
                CmpOp::Eq => (n - value).abs() < 1e-9,
                CmpOp::Ne => (n - value).abs() >= 1e-9,
                CmpOp::Lt => *n < value,
                CmpOp::Le => *n <= value,
                CmpOp::Gt => *n > value,
                CmpOp::Ge => *n >= value,
            },
            _ => false,
        },
        Condition::Triggered { input } => {
            matches!(inputs.get(input), Some(InputValue::Trigger { fired: true }))
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn sample_state(
    state: &State,
    clips: &ClipMap,
    inputs: &[InputValue],
    prev_time: f64,
    time: f64,
    out: &mut HashMap<(NodeId, PropPath), Value>,
    events: &mut Vec<String>,
    include_start: bool,
) {
    match &state.kind {
        StateKind::Empty => {}
        StateKind::Clip {
            clip,
            speed,
            loop_mode,
        } => {
            let Some(c) = clips.get(*clip) else { return };
            let speed = finite_nonnegative(*speed);
            let time = if time.is_finite() { time } else { 0.0 };
            let prev_time = if prev_time.is_finite() {
                prev_time
            } else {
                time
            };
            let (frame, _) = c.local(time * speed, *loop_mode);
            c.sample_into(frame, out);
            emit_events(
                c,
                prev_time * speed,
                time * speed,
                *loop_mode,
                include_start,
                events,
            );
        }
        StateKind::Blend1D { input, children } => {
            if children.is_empty() {
                return;
            }
            let x = match inputs.get(*input) {
                Some(InputValue::Number(n)) if n.is_finite() => *n,
                _ => 0.0,
            };
            let (lo, hi, t) = bracket(children, x);
            let lo_clip = clips.get(children[lo].clip);
            let hi_clip = if hi == lo {
                None
            } else {
                clips.get(children[hi].clip)
            };
            let mut a = HashMap::new();
            let mut b = HashMap::new();
            let mut low_events = Vec::new();
            let mut high_events = Vec::new();
            if let Some(c) = lo_clip {
                c.sample_into(c.local(time, LoopMode::Loop).0, &mut a);
                emit_events(
                    c,
                    prev_time,
                    time,
                    LoopMode::Loop,
                    include_start,
                    &mut low_events,
                );
            }
            if let Some(c) = hi_clip {
                c.sample_into(c.local(time, LoopMode::Loop).0, &mut b);
                emit_events(
                    c,
                    prev_time,
                    time,
                    LoopMode::Loop,
                    include_start,
                    &mut high_events,
                );
            }
            let mut event_keys = std::collections::HashSet::new();
            events.extend(
                low_events
                    .into_iter()
                    .filter(|event| event_keys.insert((children[lo].clip, event.clone()))),
            );
            if hi != lo {
                events.extend(
                    high_events
                        .into_iter()
                        .filter(|event| event_keys.insert((children[hi].clip, event.clone()))),
                );
            }
            if hi != lo && lo_clip.is_some() && hi_clip.is_some() {
                for (key, va) in a {
                    let value = match b.remove(&key) {
                        Some(vb) => value_tween(&va, &vb, t),
                        None => va,
                    };
                    out.insert(key, value);
                }
                out.extend(b);
            } else if hi != lo && lo_clip.is_none() {
                out.extend(b);
            } else {
                out.extend(a);
            }
        }
        StateKind::Blend2D {
            input_x,
            input_y,
            children,
        } => {
            if children.is_empty() {
                return;
            }
            let x = match inputs.get(*input_x) {
                Some(InputValue::Number(n)) if n.is_finite() => *n,
                _ => 0.0,
            };
            let y = match inputs.get(*input_y) {
                Some(InputValue::Number(n)) if n.is_finite() => *n,
                _ => 0.0,
            };
            let weights = blend2d_weights(children, x, y);
            let mut accumulator: HashMap<(NodeId, PropPath), Value> = HashMap::new();
            let mut weight_sum: HashMap<(NodeId, PropPath), f64> = HashMap::new();
            let mut event_keys = std::collections::HashSet::new();
            for (index, child) in children.iter().enumerate() {
                let weight = weights[index];
                if weight <= 0.0 {
                    continue;
                }
                let Some(c) = clips.get(child.clip) else {
                    continue;
                };
                let mut sampled = HashMap::new();
                let mut child_events = Vec::new();
                c.sample_into(c.local(time, LoopMode::Loop).0, &mut sampled);
                emit_events(
                    c,
                    prev_time,
                    time,
                    LoopMode::Loop,
                    include_start,
                    &mut child_events,
                );
                for event in child_events {
                    if event_keys.insert((child.clip, event.clone())) {
                        events.push(event);
                    }
                }
                for (key, value) in sampled {
                    let merged = match accumulator.get(&key) {
                        Some(current) => {
                            let previous = weight_sum.get(&key).copied().unwrap_or(0.0);
                            let total = previous + weight;
                            let t = if total > 0.0 { weight / total } else { 0.0 };
                            Some(value_tween(current, &value, t))
                        }
                        None => None,
                    };
                    match merged {
                        Some(v) => {
                            let total =
                                weight_sum.get(&key).copied().unwrap_or(0.0) + weight;
                            accumulator.insert(key.clone(), v);
                            weight_sum.insert(key, total);
                        }
                        None => {
                            weight_sum.insert(key.clone(), weight);
                            accumulator.insert(key, value);
                        }
                    }
                }
            }
            out.extend(accumulator);
        }
    }
}

/// Index pair + blend factor for x among sorted thresholds.
fn bracket(children: &[BlendChild], x: f64) -> (usize, usize, f64) {
    let mut low: Option<usize> = None;
    let mut high: Option<usize> = None;
    let mut minimum: Option<usize> = None;
    let mut maximum: Option<usize> = None;
    for (index, child) in children.iter().enumerate() {
        if !child.threshold.is_finite() {
            continue;
        }
        if minimum.is_none_or(|current| child.threshold < children[current].threshold) {
            minimum = Some(index);
        }
        if maximum.is_none_or(|current| child.threshold > children[current].threshold) {
            maximum = Some(index);
        }
        if child.threshold <= x
            && low.is_none_or(|current| child.threshold > children[current].threshold)
        {
            low = Some(index);
        }
        if child.threshold >= x
            && high.is_none_or(|current| child.threshold < children[current].threshold)
        {
            high = Some(index);
        }
    }
    let (Some(minimum), Some(maximum)) = (minimum, maximum) else {
        return (0, 0, 0.0);
    };
    let low = low.unwrap_or(minimum);
    let high = high.unwrap_or(maximum);
    if x <= children[low].threshold {
        return (low, low, 0.0);
    }
    if x >= children[high].threshold {
        return (high, high, 0.0);
    }
    let span = children[high].threshold - children[low].threshold;
    let factor = if span > 0.0 {
        (x - children[low].threshold) / span
    } else {
        0.0
    };
    (low, high, factor.clamp(0.0, 1.0))
}

const WEIGHT_EPS: f64 = 1e-9;

/// Blend weights for `children` at plane point `(x, y)`.
///
/// One child: 1. With two: projection onto the segment. Otherwise a Delaunay
/// triangulation of the child positions: barycentric weights inside a triangle,
/// projection onto the nearest edge outside, nearest child when degenerate.
fn blend2d_weights(children: &[BlendPoint2D], x: f64, y: f64) -> Vec<f64> {
    let n = children.len();
    let mut weights = vec![0.0; n];
    if n == 0 {
        return weights;
    }
    let points: Vec<(f64, f64)> = children.iter().map(|child| child.pos).collect();
    if n == 1 {
        weights[0] = 1.0;
        return weights;
    }
    let x = if x.is_finite() { x } else { 0.0 };
    let y = if y.is_finite() { y } else { 0.0 };
    if n == 2 {
        let t = closest_on_segment(points[0], points[1], x, y).0;
        weights[0] = 1.0 - t;
        weights[1] = t;
        return weights;
    }
    let triangles = delaunay_triangles(&points);
    if triangles.is_empty() {
        weights[nearest_point(&points, x, y)] = 1.0;
        return weights;
    }
    for triangle in &triangles {
        if let Some((u, v, w)) = barycentric(
            points[triangle[0]],
            points[triangle[1]],
            points[triangle[2]],
            x,
            y,
        ) && u >= -WEIGHT_EPS
            && v >= -WEIGHT_EPS
            && w >= -WEIGHT_EPS
        {
            weights[triangle[0]] = u;
            weights[triangle[1]] = v;
            weights[triangle[2]] = w;
            return weights;
        }
    }
    let mut best = f64::INFINITY;
    let mut best_edge: Option<(usize, usize, f64)> = None;
    for triangle in &triangles {
        let verts = [triangle[0], triangle[1], triangle[2]];
        for k in 0..3 {
            let ia = verts[k];
            let ib = verts[(k + 1) % 3];
            let (t, (cx, cy)) = closest_on_segment(points[ia], points[ib], x, y);
            let d2 = (cx - x) * (cx - x) + (cy - y) * (cy - y);
            if d2 < best {
                best = d2;
                best_edge = Some((ia, ib, t));
            }
        }
    }
    match best_edge {
        Some((ia, ib, t)) => {
            weights[ia] = 1.0 - t;
            weights[ib] = t;
        }
        None => weights[nearest_point(&points, x, y)] = 1.0,
    }
    weights
}

fn nearest_point(points: &[(f64, f64)], x: f64, y: f64) -> usize {
    let mut best = 0;
    let mut best_d = f64::INFINITY;
    for (index, &(px, py)) in points.iter().enumerate() {
        if !px.is_finite() || !py.is_finite() {
            continue;
        }
        let d = (px - x) * (px - x) + (py - y) * (py - y);
        if d < best_d {
            best_d = d;
            best = index;
        }
    }
    best
}

/// Closest point on segment `a..b` to `p`, as `(t, point)` with `t` in 0..=1.
fn closest_on_segment(a: (f64, f64), b: (f64, f64), x: f64, y: f64) -> (f64, (f64, f64)) {
    let (ax, ay) = a;
    let (bx, by) = b;
    let dx = bx - ax;
    let dy = by - ay;
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((x - ax) * dx + (y - ay) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (t, (ax + dx * t, ay + dy * t))
}

/// Barycentric weights of `p` in triangle `(a, b, c)`, or `None` if degenerate.
fn barycentric(
    a: (f64, f64),
    b: (f64, f64),
    c: (f64, f64),
    px: f64,
    py: f64,
) -> Option<(f64, f64, f64)> {
    let (ax, ay) = a;
    let (bx, by) = b;
    let (cx, cy) = c;
    let v0 = (bx - ax, by - ay);
    let v1 = (cx - ax, cy - ay);
    let v2 = (px - ax, py - ay);
    let d00 = v0.0 * v0.0 + v0.1 * v0.1;
    let d01 = v0.0 * v1.0 + v0.1 * v1.1;
    let d11 = v1.0 * v1.0 + v1.1 * v1.1;
    let d20 = v2.0 * v0.0 + v2.1 * v0.1;
    let d21 = v2.0 * v1.0 + v2.1 * v1.1;
    let denom = d00 * d11 - d01 * d01;
    if denom.abs() < 1e-12 {
        return None;
    }
    let v = (d11 * d20 - d01 * d21) / denom;
    let w = (d00 * d21 - d01 * d20) / denom;
    Some((1.0 - v - w, v, w))
}

/// Delaunay triangulation (Bowyer-Watson) of `points`, as index triples.
/// Deterministic for a fixed input order. Empty when fewer than three finite,
/// non-degenerate points exist.
fn delaunay_triangles(points: &[(f64, f64)]) -> Vec<[usize; 3]> {
    let n = points.len();
    if n < 3 {
        return Vec::new();
    }
    let ids: Vec<usize> = (0..n)
        .filter(|&i| points[i].0.is_finite() && points[i].1.is_finite())
        .collect();
    if ids.len() < 3 {
        return Vec::new();
    }
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for &i in &ids {
        let (x, y) = points[i];
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    let extent = (max_x - min_x).max(max_y - min_y).max(1e-6);
    let d = extent * 32.0;
    let center = (min_x + max_x) * 0.5;
    let middle = (min_y + max_y) * 0.5;
    let super_vertices = [
        (center - d, middle - d),
        (center + d, middle - d),
        (center, middle + d),
    ];
    let point_at = |i: usize| {
        if i < n {
            points[i]
        } else {
            super_vertices[i - n]
        }
    };
    let mut triangles: Vec<[usize; 3]> = vec![[n, n + 1, n + 2]];
    for &pi in &ids {
        let point = point_at(pi);
        let mut bad: Vec<usize> = Vec::new();
        for (index, triangle) in triangles.iter().enumerate() {
            if in_circumcircle(
                point,
                point_at(triangle[0]),
                point_at(triangle[1]),
                point_at(triangle[2]),
            ) {
                bad.push(index);
            }
        }
        if bad.is_empty() {
            continue;
        }
        let mut edges: Vec<(usize, usize)> = Vec::new();
        for &index in &bad {
            let triangle = triangles[index];
            for k in 0..3 {
                let edge = (triangle[k], triangle[(k + 1) % 3]);
                let shared = bad.iter().any(|&other| {
                    if other == index {
                        return false;
                    }
                    let candidate = triangles[other];
                    (0..3).any(|m| {
                        let e = (candidate[m], candidate[(m + 1) % 3]);
                        e == edge || (e.1, e.0) == edge
                    })
                });
                if !shared {
                    edges.push(edge);
                }
            }
        }
        let mut sorted = bad;
        sorted.sort_unstable();
        for &index in sorted.iter().rev() {
            triangles.remove(index);
        }
        for (a, b) in edges {
            triangles.push([a, b, pi]);
        }
    }
    triangles.retain(|triangle| triangle.iter().all(|&vertex| vertex < n));
    triangles
}

fn in_circumcircle(d: (f64, f64), a: (f64, f64), b: (f64, f64), c: (f64, f64)) -> bool {
    let (dx, dy) = d;
    let (ax, ay) = a;
    let (bx, by) = b;
    let (cx, cy) = c;
    let adx = ax - dx;
    let ady = ay - dy;
    let bdx = bx - dx;
    let bdy = by - dy;
    let cdx = cx - dx;
    let cdy = cy - dy;
    let det = (adx * adx + ady * ady) * (bdx * cdy - bdy * cdx)
        - (bdx * bdx + bdy * bdy) * (adx * cdy - ady * cdx)
        + (cdx * cdx + cdy * cdy) * (adx * bdy - ady * bdx);
    let orient = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
    if orient > 0.0 { det > 1e-12 } else { det < -1e-12 }
}

fn emit_events(
    c: &Clip,
    prev: f64,
    cur: f64,
    loop_mode: LoopMode,
    include_start: bool,
    out: &mut Vec<String>,
) {
    if !prev.is_finite() || !cur.is_finite() {
        return;
    }
    let length = c.len_frames();
    if length <= 0.0 || !length.is_finite() {
        return;
    }
    let start = c.range.0.0 as f64;
    let (from, to) = if cur >= prev {
        (prev, cur)
    } else {
        (cur, prev)
    };
    let period = match loop_mode {
        LoopMode::Once | LoopMode::Loop => length,
        LoopMode::PingPong => 2.0 * length,
    };
    let mut seen = std::collections::HashSet::new();
    for event in &c.events {
        if !seen.insert((event.frame, event.name.clone())) {
            continue;
        }
        let offset = event.frame.0 as f64 - start;
        if !offset.is_finite() || offset < 0.0 || offset > length {
            continue;
        }
        let crossed = match loop_mode {
            LoopMode::Once => {
                offset > from && offset <= to || include_start && from <= 0.0 && offset == 0.0
            }
            LoopMode::Loop => {
                let first = ((from - offset) / period).floor() + 1.0;
                let crossing = first * period + offset;
                crossing <= to || include_start && from <= 0.0 && offset == 0.0
            }
            LoopMode::PingPong => {
                let forward = ((from - offset) / period).floor() + 1.0;
                let reverse = ((from - (2.0 * length - offset)) / period).floor() + 1.0;
                let forward_crossing = forward * period + offset;
                let reverse_crossing = reverse * period + (2.0 * length - offset);
                forward_crossing <= to
                    || reverse_crossing <= to
                    || include_start && from <= 0.0 && offset == 0.0
            }
        };
        if crossed {
            out.push(event.name.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use renamite_animation::{EasingHandle, Interpolation};

    fn key(f: i64, v: f64) -> KeyframeData {
        KeyframeData {
            frame: Frame(f),
            value: Value::F64(v),
            interpolation: Interpolation::Linear,
            ease_out: EasingHandle::LINEAR_OUT,
            ease_in: EasingHandle::LINEAR_IN,
        }
    }

    fn world() -> (ClipMap, Machine, NodeId) {
        let node = {
            let mut doc = renamite_model::Document::empty();
            doc.create_node(renamite_model::Node::new(
                "n",
                renamite_model::NodeKind::Group,
            ))
        };
        let mut clips = ClipMap::default();
        let up = clips.insert(Clip {
            name: "up".into(),
            range: (Frame(0), Frame(60)),
            tracks: vec![Track {
                node,
                prop: PropPath::new("opacity"),
                keys: vec![key(0, 0.0), key(60, 1.0)],
            }],
            events: vec![EventKey {
                frame: Frame(30),
                name: "half".into(),
            }],
        });
        let down = clips.insert(Clip {
            name: "down".into(),
            range: (Frame(0), Frame(60)),
            tracks: vec![Track {
                node,
                prop: PropPath::new("opacity"),
                keys: vec![key(0, 1.0), key(60, 0.0)],
            }],
            events: vec![],
        });
        let m = Machine {
            name: "hover".into(),
            inputs: vec![InputDef {
                name: "over".into(),
                kind: InputKind::Bool { default: false },
            }],
            layers: vec![MachineLayer {
                name: "base".into(),
                entry: 0,
                any_transitions: vec![],
                blend: LayerBlend::Override,
                states: vec![
                    State {
                        name: "Down".into(),
                        kind: StateKind::Clip {
                            clip: down,
                            speed: 1.0,
                            loop_mode: LoopMode::Once,
                        },
                        transitions: vec![Transition {
                            to: 1,
                            duration: 10.0,
                            exit_time: None,
                            conditions: vec![Condition::BoolIs {
                                input: 0,
                                value: true,
                            }],
                        }],
                        graph_pos: None,
                        sync_group: None,
                    },
                    State {
                        name: "Up".into(),
                        kind: StateKind::Clip {
                            clip: up,
                            speed: 1.0,
                            loop_mode: LoopMode::Once,
                        },
                        transitions: vec![Transition {
                            to: 0,
                            duration: 10.0,
                            exit_time: None,
                            conditions: vec![Condition::BoolIs {
                                input: 0,
                                value: false,
                            }],
                        }],
                        graph_pos: None,
                        sync_group: None,
                    },
                ],
            }],
            listeners: vec![Listener {
                node,
                event: PointerEventKind::Enter,
                action: ListenerAction::SetBool {
                    input: 0,
                    value: true,
                },
            }],
            root_motion: None,
        };
        (clips, m, node)
    }

    #[test]
    fn track_lerps() {
        let (clips, _, node) = world();
        let c = clips.values().find(|c| c.name == "up").unwrap();
        let mut out = HashMap::new();
        c.sample_into(30.0, &mut out);
        assert_eq!(out[&(node, PropPath::new("opacity"))], Value::F64(0.5));
    }

    #[test]
    fn bool_input_transitions_and_listener_sets_it() {
        let (clips, m, node) = world();
        let mut inst = MachineInstance::new(&m);
        let mut ov = Overrides::default();
        inst.tick(&m, &clips, 1.0, &mut ov);
        assert_eq!(inst.layers[0].current, 0);
        inst.pointer_event(&m, node, PointerEventKind::Enter);
        inst.tick(&m, &clips, 1.0, &mut ov);
        assert_eq!(inst.layers[0].current, 1);
        assert!(inst.layers[0].fade.is_some());
    }

    #[test]
    fn trigger_consumed_once() {
        let (clips, mut m, _) = world();
        m.inputs.push(InputDef {
            name: "tap".into(),
            kind: InputKind::Trigger,
        });
        m.layers[0].states[0].transitions[0].conditions = vec![Condition::Triggered { input: 1 }];
        let mut inst = MachineInstance::new(&m);
        let mut ov = Overrides::default();
        inst.fire(1);
        inst.tick(&m, &clips, 1.0, &mut ov);
        assert_eq!(inst.layers[0].current, 1);
        m.layers[0].states[1].transitions[0].conditions = vec![Condition::Triggered { input: 1 }];
        inst.tick(&m, &clips, 1.0, &mut ov);
        assert_eq!(inst.layers[0].current, 1);
    }

    #[test]
    fn clip_event_crossing_fires_once() {
        let (clips, m, _) = world();
        let mut inst = MachineInstance::new(&m);
        inst.set_bool(0, true);
        let mut ov = Overrides::default();
        inst.tick(&m, &clips, 1.0, &mut ov);
        let mut names = Vec::new();
        for _ in 0..40 {
            let out = inst.tick(&m, &clips, 1.0, &mut ov);
            names.extend(out.events);
        }
        assert_eq!(names.iter().filter(|n| *n == "half").count(), 1);
    }

    #[test]
    fn blend_events_from_distinct_clips_are_not_collapsed() {
        let (mut clips, mut machine, _) = world();
        let down = clips
            .iter()
            .find(|(_, clip)| clip.name == "down")
            .unwrap()
            .0;
        let up = clips.iter().find(|(_, clip)| clip.name == "up").unwrap().0;
        clips.get_mut(down).unwrap().events.push(EventKey {
            frame: Frame(30),
            name: "half".into(),
        });
        machine.inputs.push(InputDef {
            name: "blend".into(),
            kind: InputKind::Number { default: 0.5 },
        });
        machine.layers[0].states[0].kind = StateKind::Blend1D {
            input: 1,
            children: vec![
                BlendChild {
                    threshold: 0.0,
                    clip: down,
                },
                BlendChild {
                    threshold: 1.0,
                    clip: up,
                },
            ],
        };
        let mut instance = MachineInstance::new(&machine);
        let mut overrides = Overrides::default();
        instance.set_number(1, 0.5);
        let events = instance.tick(&machine, &clips, 31.0, &mut overrides).events;
        assert_eq!(events.iter().filter(|event| *event == "half").count(), 2);
    }

    #[test]
    fn crossfade_tweens_overlapping_props() {
        let (clips, m, node) = world();
        let mut inst = MachineInstance::new(&m);
        let mut ov = Overrides::default();
        inst.set_bool(0, true);
        // First tick starts fade Down->Up (duration 10)
        inst.tick(&m, &clips, 1.0, &mut ov);
        assert!(inst.layers[0].fade.is_some());
        // Mid-fade: opacity must be between down and up samples, not hard-cut to `b`
        ov.clear();
        inst.tick(&m, &clips, 4.0, &mut ov);
        let Some(Value::F64(op)) = ov.get(node, "opacity").cloned() else {
            panic!("expected f64 opacity");
        };
        assert!(op > 0.0 && op < 1.0, "got {op}");
    }

    fn node_id() -> NodeId {
        let mut doc = renamite_model::Document::empty();
        doc.create_node(renamite_model::Node::new(
            "n",
            renamite_model::NodeKind::Group,
        ))
    }

    fn const_clip(node: NodeId, value: f64) -> Clip {
        Clip {
            name: format!("c{value}"),
            range: (Frame(0), Frame(60)),
            tracks: vec![Track {
                node,
                prop: PropPath::new("opacity"),
                keys: vec![key(0, value), key(60, value)],
            }],
            events: vec![],
        }
    }

    #[test]
    fn blend2d_weights_are_exact_at_sample_points() {
        let point = |x: f64, y: f64| BlendPoint2D {
            pos: (x, y),
            clip: ClipId::default(),
        };
        let children = [
            point(0.0, 0.0),
            point(1.0, 0.0),
            point(0.0, 1.0),
            point(1.0, 1.0),
        ];
        assert_eq!(blend2d_weights(&children, 1.0, 0.0), vec![0.0, 1.0, 0.0, 0.0]);
        assert_eq!(
            blend2d_weights(&children, 0.5, 0.0),
            vec![0.5, 0.5, 0.0, 0.0]
        );
        assert_eq!(
            blend2d_weights(&children, -1.0, 0.5),
            vec![0.5, 0.0, 0.5, 0.0]
        );
    }

    #[test]
    fn blend2d_output_is_the_weighted_average() {
        let node = node_id();
        let mut clips = ClipMap::default();
        let children: Vec<BlendPoint2D> = [
            (0.0, 0.0, 0.0),
            (1.0, 0.0, 10.0),
            (0.0, 1.0, 20.0),
            (1.0, 1.0, 30.0),
        ]
        .into_iter()
        .map(|(x, y, v)| BlendPoint2D {
            pos: (x, y),
            clip: clips.insert(const_clip(node, v)),
        })
        .collect();
        let machine = Machine {
            name: "m".into(),
            inputs: vec![
                InputDef {
                    name: "x".into(),
                    kind: InputKind::Number { default: 0.5 },
                },
                InputDef {
                    name: "y".into(),
                    kind: InputKind::Number { default: 0.0 },
                },
            ],
            layers: vec![MachineLayer {
                name: "base".into(),
                entry: 0,
                any_transitions: vec![],
                blend: LayerBlend::Override,
                states: vec![State {
                    name: "blend".into(),
                    kind: StateKind::Blend2D {
                        input_x: 0,
                        input_y: 1,
                        children,
                    },
                    transitions: vec![],
                    graph_pos: None,
                    sync_group: None,
                }],
            }],
            listeners: vec![],
            root_motion: None,
        };
        let mut instance = MachineInstance::new(&machine);
        let mut overrides = Overrides::default();
        instance.tick(&machine, &clips, 1.0, &mut overrides);
        assert_eq!(overrides.get(node, "opacity"), Some(&Value::F64(5.0)));
    }

    #[test]
    fn additive_layer_accumulates_over_lower_layers() {
        let node = node_id();
        let mut clips = ClipMap::default();
        let base = clips.insert(const_clip(node, 0.2));
        let add = clips.insert(const_clip(node, 0.5));
        let machine = Machine {
            name: "m".into(),
            inputs: vec![InputDef {
                name: "add".into(),
                kind: InputKind::Number { default: 0.5 },
            }],
            layers: vec![
                MachineLayer {
                    name: "base".into(),
                    entry: 0,
                    any_transitions: vec![],
                    blend: LayerBlend::Override,
                    states: vec![State {
                        name: "base".into(),
                        kind: StateKind::Clip {
                            clip: base,
                            speed: 1.0,
                            loop_mode: LoopMode::Loop,
                        },
                        transitions: vec![],
                        graph_pos: None,
                        sync_group: None,
                    }],
                },
                MachineLayer {
                    name: "add".into(),
                    entry: 0,
                    any_transitions: vec![],
                    blend: LayerBlend::Additive { weight: 0 },
                    states: vec![State {
                        name: "add".into(),
                        kind: StateKind::Clip {
                            clip: add,
                            speed: 1.0,
                            loop_mode: LoopMode::Loop,
                        },
                        transitions: vec![],
                        graph_pos: None,
                        sync_group: None,
                    }],
                },
            ],
            listeners: vec![],
            root_motion: None,
        };
        let mut instance = MachineInstance::new(&machine);
        let mut overrides = Overrides::default();
        instance.tick(&machine, &clips, 1.0, &mut overrides);
        assert_eq!(overrides.get(node, "opacity"), Some(&Value::F64(0.45)));
    }

    #[test]
    fn sync_group_keeps_phase_across_transition() {
        let node = node_id();
        let mut clips = ClipMap::default();
        let a = clips.insert(const_clip(node, 0.0));
        let b = clips.insert(Clip {
            range: (Frame(0), Frame(40)),
            ..const_clip(node, 0.0)
        });
        let state = |name: &str, clip: ClipId, transitions: Vec<Transition>| State {
            name: name.into(),
            kind: StateKind::Clip {
                clip,
                speed: 1.0,
                loop_mode: LoopMode::Loop,
            },
            transitions,
            graph_pos: None,
            sync_group: Some("gait".into()),
        };
        let machine = Machine {
            name: "m".into(),
            inputs: vec![InputDef {
                name: "run".into(),
                kind: InputKind::Bool { default: false },
            }],
            layers: vec![MachineLayer {
                name: "base".into(),
                entry: 0,
                any_transitions: vec![],
                blend: LayerBlend::Override,
                states: vec![
                    state(
                        "walk",
                        a,
                        vec![Transition {
                            to: 1,
                            duration: 10.0,
                            exit_time: None,
                            conditions: vec![Condition::BoolIs {
                                input: 0,
                                value: true,
                            }],
                        }],
                    ),
                    state("run", b, vec![]),
                ],
            }],
            listeners: vec![],
            root_motion: None,
        };
        let mut instance = MachineInstance::new(&machine);
        let mut overrides = Overrides::default();
        instance.tick(&machine, &clips, 20.0, &mut overrides);
        instance.set_bool(0, true);
        instance.tick(&machine, &clips, 1.0, &mut overrides);
        let layer = &machine.layers[0];
        let fade = instance.layers[0].fade.clone().expect("crossfade");
        let phase_a = state_phase(&layer.states[fade.from], &clips, fade.from_time).unwrap();
        let phase_b = state_phase(&layer.states[instance.layers[0].current], &clips, instance.layers[0].time).unwrap();
        assert!(
            (phase_a - phase_b).abs() < 1e-12,
            "walk phase {phase_a} vs run phase {phase_b}"
        );
    }

    #[test]
    fn root_motion_delta_is_exact_for_a_clip() {
        let node = node_id();
        let mut clips = ClipMap::default();
        let clip = clips.insert(Clip {
            name: "walk".into(),
            range: (Frame(0), Frame(10)),
            tracks: vec![
                Track {
                    node,
                    prop: PropPath::new("transform.position"),
                    keys: vec![
                        KeyframeData {
                            frame: Frame(0),
                            value: Value::DVec2(glam::DVec2::ZERO),
                            interpolation: Interpolation::Linear,
                            ease_out: EasingHandle::LINEAR_OUT,
                            ease_in: EasingHandle::LINEAR_IN,
                        },
                        KeyframeData {
                            frame: Frame(10),
                            value: Value::DVec2(glam::DVec2::new(100.0, 0.0)),
                            interpolation: Interpolation::Linear,
                            ease_out: EasingHandle::LINEAR_OUT,
                            ease_in: EasingHandle::LINEAR_IN,
                        },
                    ],
                },
                Track {
                    node,
                    prop: PropPath::new("transform.rotation"),
                    keys: vec![
                        KeyframeData {
                            frame: Frame(0),
                            value: Value::Angle(renamite_animation::Angle(0.0)),
                            interpolation: Interpolation::Linear,
                            ease_out: EasingHandle::LINEAR_OUT,
                            ease_in: EasingHandle::LINEAR_IN,
                        },
                        KeyframeData {
                            frame: Frame(10),
                            value: Value::Angle(renamite_animation::Angle(90.0)),
                            interpolation: Interpolation::Linear,
                            ease_out: EasingHandle::LINEAR_OUT,
                            ease_in: EasingHandle::LINEAR_IN,
                        },
                    ],
                },
            ],
            events: vec![],
        });
        let c = &clips[clip];
        let delta = c.root_motion_delta(node, 0.0, 5.0, 1.0, LoopMode::Once);
        assert_eq!(delta.position, glam::DVec2::new(50.0, 0.0));
        assert_eq!(delta.rotation_deg, 45.0);
    }
}
