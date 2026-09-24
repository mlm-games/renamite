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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Machine {
    pub name: String,
    pub inputs: Vec<InputDef>,
    pub layers: Vec<MachineLayer>,
    /// Pointer interactions on scene nodes → input actions. Picking is free:
    /// `SceneItem` already carries the shape `NodeId`.
    pub listeners: Vec<Listener>,
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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub name: String,
    pub kind: StateKind,
    pub transitions: Vec<Transition>,
    #[serde(default)]
    pub graph_pos: Option<(f64, f64)>,
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
    /// No animation (rest pose = document values).
    Empty,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlendChild {
    pub threshold: f64,
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
            for (k, v) in b {
                out.set(k.0, k.1, v);
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
            for (k, v) in b {
                out.set(k.0, k.1, v);
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
}
