//! Clips (named timelines) + state machines. Pure, deterministic, and WASM-safe.
//!
//! A `Clip` is a bag of (NodeId, PropPath) -> keyframe tracks. They are the same
//! `KeyframeData` the history system already uses, so clip authoring reuses
//! `EditorCommand` semantics later. A `Machine` turns inputs into an
//! `Overrides` patch per tick; the host feeds that to `evaluate_with`.
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
        if frame <= ks[0].frame.0 as f64 {
            return Some(ks[0].value.clone());
        }
        let last = ks.len() - 1;
        if frame >= ks[last].frame.0 as f64 {
            return Some(ks[last].value.clone());
        }
        let i = ks.partition_point(|k| (k.frame.0 as f64) <= frame) - 1;
        let (a, b) = (&ks[i], &ks[i + 1]);
        let u = (frame - a.frame.0 as f64) / (b.frame.0 - a.frame.0) as f64;
        let y = ease_progress(a.interpolation, a.ease_out, a.ease_in, u);
        Some(value_tween(&a.value, &b.value, y))
    }
}

impl Clip {
    pub fn len_frames(&self) -> f64 {
        (self.range.1.0 - self.range.0.0).max(1) as f64
    }

    /// Map layer-local time to a clip frame; returns (frame, normalized 0..1).
    pub fn local(&self, time: f64, loop_mode: LoopMode) -> (f64, f64) {
        let (s, len) = (self.range.0.0 as f64, self.len_frames());
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InputValue {
    Bool(bool),
    Number(f64),
    Trigger { fired: bool },
}

#[derive(Clone, Debug)]
struct LayerRt {
    current: usize,
    /// Frames spent in current state.
    time: f64,
    fade: Option<Fade>,
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
        Self {
            inputs: m
                .inputs
                .iter()
                .map(|i| match i.kind {
                    InputKind::Bool { default } => InputValue::Bool(default),
                    InputKind::Number { default } => InputValue::Number(default),
                    InputKind::Trigger => InputValue::Trigger { fired: false },
                })
                .collect(),
            layers: m
                .layers
                .iter()
                .map(|l| LayerRt {
                    current: l.entry.min(l.states.len().saturating_sub(1)),
                    time: 0.0,
                    fade: None,
                })
                .collect(),
        }
    }

    pub fn input_index(m: &Machine, name: &str) -> Option<usize> {
        m.inputs.iter().position(|i| i.name == name)
    }
    /// Current active state per layer, in layer order.
    pub fn layer_states(
        &self,
    ) -> impl Iterator<Item = usize> + '_ {
        self.layers
            .iter()
            .map(|layer| layer.current)
    }
    pub fn set_bool(&mut self, idx: usize, v: bool) {
        if let Some(InputValue::Bool(b)) = self.inputs.get_mut(idx) {
            *b = v;
        }
    }
    pub fn set_number(&mut self, idx: usize, v: f64) {
        if let Some(InputValue::Number(n)) = self.inputs.get_mut(idx) {
            *n = v;
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
        for (li, layer) in m.layers.iter().enumerate() {
            let rt = &mut self.layers[li];
            let prev_time = rt.time;
            rt.time += dt_frames;
            if let Some(f) = &mut rt.fade {
                f.from_time += dt_frames;
                f.t = if f.duration <= 0.0 {
                    1.0
                } else {
                    (f.t + dt_frames / f.duration).min(1.0)
                };
                if f.t >= 1.0 {
                    rt.fade = None;
                }
            }

            // transitions: Any first, then current state's, first match wins
            let state = &layer.states[rt.current];
            let norm = normalized_time(state, clips, rt.time);
            let fired = layer
                .any_transitions
                .iter()
                .chain(state.transitions.iter())
                .find(|tr| transition_ready(tr, &self.inputs, norm));
            if let Some(tr) = fired.cloned() {
                consume_triggers(&tr, &mut self.inputs);
                rt.fade = (tr.duration > 0.0).then_some(Fade {
                    from: rt.current,
                    from_time: rt.time,
                    t: 0.0,
                    duration: tr.duration,
                });
                rt.current = tr.to.min(layer.states.len() - 1);
                rt.time = 0.0;
            }

            // sample
            let mut b = HashMap::new();
            let mut evs = Vec::new();
            sample_state(
                &layer.states[rt.current],
                clips,
                &self.inputs,
                prev_if_same(prev_time, rt.time),
                rt.time,
                &mut b,
                &mut evs,
            );
            if let Some(f) = &rt.fade {
                let mut a = HashMap::new();
                sample_state(
                    &layer.states[f.from],
                    clips,
                    &self.inputs,
                    f.from_time,
                    f.from_time,
                    &mut a,
                    &mut Vec::new(),
                );
                for (k, va) in a {
                    let merged = match b.get(&k) {
                        Some(vb) => value_tween(&va, vb, f.t),
                        None => va,
                    };
                    b.entry(k).or_insert(merged);
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
}

fn prev_if_same(prev: f64, cur: f64) -> f64 {
    if cur < prev { 0.0 } else { prev }
}

fn normalized_time(state: &State, clips: &ClipMap, time: f64) -> f64 {
    match &state.kind {
        StateKind::Clip {
            clip,
            speed,
            loop_mode,
        } => clips
            .get(*clip)
            .map(|c| c.local(time * speed.max(0.0), *loop_mode).1)
            .unwrap_or(1.0),
        StateKind::Blend1D { children, .. } => children
            .first()
            .and_then(|ch| clips.get(ch.clip))
            .map(|c| c.local(time, LoopMode::Loop).1)
            .unwrap_or(1.0),
        StateKind::Empty => 1.0,
    }
}

fn transition_ready(tr: &Transition, inputs: &[InputValue], norm: f64) -> bool {
    if let Some(et) = tr.exit_time
        && norm < et
    {
        return false;
    }
    if tr.conditions.is_empty() && tr.exit_time.is_none() {
        return false;
    }
    tr.conditions.iter().all(|c| match *c {
        Condition::BoolIs { input, value } => {
            matches!(inputs.get(input), Some(InputValue::Bool(b)) if *b == value)
        }
        Condition::NumberCmp { input, op, value } => match inputs.get(input) {
            Some(InputValue::Number(n)) => match op {
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

fn consume_triggers(tr: &Transition, inputs: &mut [InputValue]) {
    for c in &tr.conditions {
        if let Condition::Triggered { input } = c
            && let Some(InputValue::Trigger { fired }) = inputs.get_mut(*input)
        {
            *fired = false;
        }
    }
}

fn sample_state(
    state: &State,
    clips: &ClipMap,
    inputs: &[InputValue],
    prev_time: f64,
    time: f64,
    out: &mut HashMap<(NodeId, PropPath), Value>,
    events: &mut Vec<String>,
) {
    match &state.kind {
        StateKind::Empty => {}
        StateKind::Clip {
            clip,
            speed,
            loop_mode,
        } => {
            let Some(c) = clips.get(*clip) else { return };
            let (frame, _) = c.local(time * speed.max(0.0), *loop_mode);
            let (pframe, _) = c.local(prev_time * speed.max(0.0), *loop_mode);
            c.sample_into(frame, out);
            emit_events(c, pframe, frame, *loop_mode, events);
        }
        StateKind::Blend1D { input, children } => {
            let x = match inputs.get(*input) {
                Some(InputValue::Number(n)) => *n,
                _ => 0.0,
            };
            let (lo, hi, t) = bracket(children, x);
            let mut a = HashMap::new();
            if let Some(c) = clips.get(children[lo].clip) {
                c.sample_into(c.local(time, LoopMode::Loop).0, &mut a);
            }
            if hi != lo {
                let mut b = HashMap::new();
                if let Some(c) = clips.get(children[hi].clip) {
                    c.sample_into(c.local(time, LoopMode::Loop).0, &mut b);
                }
                for (k, va) in a {
                    let v = match b.remove(&k) {
                        Some(vb) => value_tween(&va, &vb, t),
                        None => va,
                    };
                    out.insert(k, v);
                }
                out.extend(b);
            } else {
                out.extend(a);
            }
        }
    }
}

/// Index pair + blend factor for x among sorted thresholds.
fn bracket(children: &[BlendChild], x: f64) -> (usize, usize, f64) {
    if children.is_empty() {
        return (0, 0, 0.0);
    }
    if x <= children[0].threshold {
        return (0, 0, 0.0);
    }
    let last = children.len() - 1;
    if x >= children[last].threshold {
        return (last, last, 0.0);
    }
    let hi = children.partition_point(|c| c.threshold <= x);
    let (lo, hi) = (hi - 1, hi);
    let span = children[hi].threshold - children[lo].threshold;
    (
        lo,
        hi,
        if span <= 0.0 {
            0.0
        } else {
            (x - children[lo].threshold) / span
        },
    )
}

fn emit_events(c: &Clip, prev: f64, cur: f64, loop_mode: LoopMode, out: &mut Vec<String>) {
    let hit = |a: f64, b: f64, out: &mut Vec<String>| {
        for e in &c.events {
            let f = e.frame.0 as f64;
            if f > a && f <= b {
                out.push(e.name.clone());
            }
        }
    };
    if cur >= prev {
        hit(prev, cur, out);
    } else if loop_mode == LoopMode::Loop {
        hit(prev, c.range.1.0 as f64, out);
        hit(c.range.0.0 as f64 - 1.0, cur, out);
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
}
