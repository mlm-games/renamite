//! Pure state-machine authoring helpers.

use renamite_machine::{
    CmpOp, Condition, InputDef, InputKind, Listener, ListenerAction, Machine, MachineLayer, State,
    StateKind, Transition,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum MachineSelection {
    #[default]
    None,

    Input {
        input: usize,
    },

    Layer {
        layer: usize,
    },

    State {
        layer: usize,
        state: usize,
    },

    Transition {
        layer: usize,
        source: TransitionSource,
        transition: usize,
    },

    Listener {
        listener: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitionSource {
    Any,
    State(usize),
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum MachineEditError {
    #[error("index is out of range")]
    OutOfRange,

    #[error("name must not be empty")]
    EmptyName,

    #[error("name `{0}` already exists")]
    DuplicateName(String),

    #[error("input is still referenced")]
    InputInUse,

    #[error("input type is incompatible")]
    InputTypeMismatch,

    #[error("layer must retain at least one state")]
    LastState,

    #[error("transition already exists")]
    DuplicateTransition,
}

pub type Result<T> = std::result::Result<T, MachineEditError>;

pub fn add_input(machine: &mut Machine, name: impl Into<String>, kind: InputKind) -> Result<usize> {
    let name = name.into();
    let name = name.trim();

    if name.is_empty() {
        return Err(MachineEditError::EmptyName);
    }

    if machine.inputs.iter().any(|input| input.name == name) {
        return Err(MachineEditError::DuplicateName(name.to_owned()));
    }

    machine.inputs.push(InputDef {
        name: name.to_owned(),
        kind,
    });

    Ok(machine.inputs.len() - 1)
}

pub fn rename_input(machine: &mut Machine, input: usize, name: impl Into<String>) -> Result<()> {
    if input >= machine.inputs.len() {
        return Err(MachineEditError::OutOfRange);
    }

    let name = name.into();
    let name = name.trim();

    if name.is_empty() {
        return Err(MachineEditError::EmptyName);
    }

    if machine
        .inputs
        .iter()
        .enumerate()
        .any(|(index, value)| index != input && value.name == name)
    {
        return Err(MachineEditError::DuplicateName(name.to_owned()));
    }

    machine.inputs[input].name = name.to_owned();
    Ok(())
}

pub fn input_is_referenced(machine: &Machine, input: usize) -> bool {
    for layer in &machine.layers {
        for state in &layer.states {
            if matches!(
                state.kind,
                StateKind::Blend1D {
                    input: state_input,
                    ..
                } if state_input == input
            ) {
                return true;
            }

            if state.transitions.iter().any(|transition| {
                transition
                    .conditions
                    .iter()
                    .any(|condition| condition_input(condition) == input)
            }) {
                return true;
            }
        }

        if layer.any_transitions.iter().any(|transition| {
            transition
                .conditions
                .iter()
                .any(|condition| condition_input(condition) == input)
        }) {
            return true;
        }
    }

    machine
        .listeners
        .iter()
        .any(|listener| listener_action_input(&listener.action) == input)
}

pub fn remove_input(machine: &mut Machine, input: usize) -> Result<InputDef> {
    if input >= machine.inputs.len() {
        return Err(MachineEditError::OutOfRange);
    }

    if input_is_referenced(machine, input) {
        return Err(MachineEditError::InputInUse);
    }

    let removed = machine.inputs.remove(input);

    for layer in &mut machine.layers {
        for state in &mut layer.states {
            if let StateKind::Blend1D {
                input: blend_input, ..
            } = &mut state.kind
            {
                if *blend_input > input {
                    *blend_input -= 1;
                }
            }

            remap_transition_inputs(&mut state.transitions, input);
        }

        remap_transition_inputs(&mut layer.any_transitions, input);
    }

    for listener in &mut machine.listeners {
        remap_listener_input(&mut listener.action, input);
    }

    Ok(removed)
}

fn condition_input(condition: &Condition) -> usize {
    match condition {
        Condition::BoolIs { input, .. }
        | Condition::NumberCmp { input, .. }
        | Condition::Triggered { input } => *input,
    }
}

fn listener_action_input(action: &ListenerAction) -> usize {
    match action {
        ListenerAction::SetBool { input, .. }
        | ListenerAction::ToggleBool { input }
        | ListenerAction::SetNumber { input, .. }
        | ListenerAction::FireTrigger { input } => *input,
    }
}

fn remap_transition_inputs(transitions: &mut [Transition], removed: usize) {
    for transition in transitions {
        for condition in &mut transition.conditions {
            match condition {
                Condition::BoolIs { input, .. }
                | Condition::NumberCmp { input, .. }
                | Condition::Triggered { input } => {
                    if *input > removed {
                        *input -= 1;
                    }
                }
            }
        }
    }
}

fn remap_listener_input(action: &mut ListenerAction, removed: usize) {
    match action {
        ListenerAction::SetBool { input, .. }
        | ListenerAction::ToggleBool { input }
        | ListenerAction::SetNumber { input, .. }
        | ListenerAction::FireTrigger { input } => {
            if *input > removed {
                *input -= 1;
            }
        }
    }
}

pub fn add_layer(machine: &mut Machine, name: impl Into<String>) -> Result<usize> {
    let name = name.into();
    let name = name.trim();

    if name.is_empty() {
        return Err(MachineEditError::EmptyName);
    }

    machine.layers.push(MachineLayer {
        name: name.to_owned(),
        entry: 0,
        any_transitions: Vec::new(),
        states: vec![State {
            name: "Entry".into(),
            kind: StateKind::Empty,
            transitions: Vec::new(),
        }],
    });

    Ok(machine.layers.len() - 1)
}

pub fn add_state(
    machine: &mut Machine,
    layer: usize,
    name: impl Into<String>,
    kind: StateKind,
) -> Result<usize> {
    let layer = machine
        .layers
        .get_mut(layer)
        .ok_or(MachineEditError::OutOfRange)?;

    let name = name.into();
    let name = name.trim();

    if name.is_empty() {
        return Err(MachineEditError::EmptyName);
    }

    if layer.states.iter().any(|state| state.name == name) {
        return Err(MachineEditError::DuplicateName(name.to_owned()));
    }

    layer.states.push(State {
        name: name.to_owned(),
        kind,
        transitions: Vec::new(),
    });

    Ok(layer.states.len() - 1)
}

pub fn rename_state(
    machine: &mut Machine,
    layer: usize,
    state: usize,
    name: impl Into<String>,
) -> Result<()> {
    let layer = machine
        .layers
        .get_mut(layer)
        .ok_or(MachineEditError::OutOfRange)?;

    if state >= layer.states.len() {
        return Err(MachineEditError::OutOfRange);
    }

    let name = name.into();
    let name = name.trim();

    if name.is_empty() {
        return Err(MachineEditError::EmptyName);
    }

    if layer
        .states
        .iter()
        .enumerate()
        .any(|(index, value)| index != state && value.name == name)
    {
        return Err(MachineEditError::DuplicateName(name.to_owned()));
    }

    layer.states[state].name = name.to_owned();
    Ok(())
}

pub fn set_entry_state(machine: &mut Machine, layer: usize, state: usize) -> Result<()> {
    let layer = machine
        .layers
        .get_mut(layer)
        .ok_or(MachineEditError::OutOfRange)?;

    if state >= layer.states.len() {
        return Err(MachineEditError::OutOfRange);
    }

    layer.entry = state;
    Ok(())
}

pub fn remove_state(
    machine: &mut Machine,
    layer_index: usize,
    state_index: usize,
) -> Result<State> {
    let layer = machine
        .layers
        .get_mut(layer_index)
        .ok_or(MachineEditError::OutOfRange)?;

    if state_index >= layer.states.len() {
        return Err(MachineEditError::OutOfRange);
    }

    if layer.states.len() <= 1 {
        return Err(MachineEditError::LastState);
    }

    let removed = layer.states.remove(state_index);

    repair_transition_targets(&mut layer.any_transitions, state_index);

    for state in &mut layer.states {
        repair_transition_targets(&mut state.transitions, state_index);
    }

    if layer.entry == state_index {
        layer.entry = state_index.min(layer.states.len() - 1);
    } else if layer.entry > state_index {
        layer.entry -= 1;
    }

    Ok(removed)
}

fn repair_transition_targets(transitions: &mut Vec<Transition>, removed_state: usize) {
    transitions.retain(|transition| transition.to != removed_state);

    for transition in transitions {
        if transition.to > removed_state {
            transition.to -= 1;
        }
    }
}

pub fn add_transition(
    machine: &mut Machine,
    layer: usize,
    source: TransitionSource,
    target: usize,
) -> Result<usize> {
    let layer = machine
        .layers
        .get_mut(layer)
        .ok_or(MachineEditError::OutOfRange)?;

    if target >= layer.states.len() {
        return Err(MachineEditError::OutOfRange);
    }

    let transitions = match source {
        TransitionSource::Any => &mut layer.any_transitions,

        TransitionSource::State(source) => {
            if source >= layer.states.len() {
                return Err(MachineEditError::OutOfRange);
            }

            &mut layer.states[source].transitions
        }
    };

    if transitions
        .iter()
        .any(|transition| transition.to == target && transition.conditions.is_empty())
    {
        return Err(MachineEditError::DuplicateTransition);
    }

    transitions.push(Transition {
        to: target,
        duration: 0.0,
        exit_time: None,
        conditions: Vec::new(),
    });

    Ok(transitions.len() - 1)
}

pub fn transition_mut(
    machine: &mut Machine,
    layer: usize,
    source: TransitionSource,
    transition: usize,
) -> Result<&mut Transition> {
    let layer = machine
        .layers
        .get_mut(layer)
        .ok_or(MachineEditError::OutOfRange)?;

    let transitions = match source {
        TransitionSource::Any => &mut layer.any_transitions,

        TransitionSource::State(state) => {
            &mut layer
                .states
                .get_mut(state)
                .ok_or(MachineEditError::OutOfRange)?
                .transitions
        }
    };

    transitions
        .get_mut(transition)
        .ok_or(MachineEditError::OutOfRange)
}

pub fn add_condition(
    machine: &mut Machine,
    layer: usize,
    source: TransitionSource,
    transition: usize,
    condition: Condition,
) -> Result<()> {
    let input = condition_input(&condition);
    let input_kind = machine
        .inputs
        .get(input)
        .ok_or(MachineEditError::OutOfRange)?
        .kind;

    if !condition_matches_input(&condition, input_kind) {
        return Err(MachineEditError::InputTypeMismatch);
    }

    transition_mut(machine, layer, source, transition)?
        .conditions
        .push(condition);

    Ok(())
}

fn condition_matches_input(condition: &Condition, input: InputKind) -> bool {
    matches!(
        (condition, input),
        (Condition::BoolIs { .. }, InputKind::Bool { .. },)
            | (Condition::NumberCmp { .. }, InputKind::Number { .. },)
            | (Condition::Triggered { .. }, InputKind::Trigger,)
    )
}

pub fn default_condition(machine: &Machine, input: usize) -> Result<Condition> {
    let def = machine
        .inputs
        .get(input)
        .ok_or(MachineEditError::OutOfRange)?;

    Ok(match def.kind {
        InputKind::Bool { .. } => Condition::BoolIs { input, value: true },

        InputKind::Number { .. } => Condition::NumberCmp {
            input,
            op: CmpOp::Ge,
            value: 0.5,
        },

        InputKind::Trigger => Condition::Triggered { input },
    })
}

pub fn add_listener(machine: &mut Machine, listener: Listener) -> Result<usize> {
    let input = listener_action_input(&listener.action);

    let kind = machine
        .inputs
        .get(input)
        .ok_or(MachineEditError::OutOfRange)?
        .kind;

    if !listener_action_matches_input(&listener.action, kind) {
        return Err(MachineEditError::InputTypeMismatch);
    }

    machine.listeners.push(listener);
    Ok(machine.listeners.len() - 1)
}

fn listener_action_matches_input(action: &ListenerAction, input: InputKind) -> bool {
    matches!(
        (action, input),
        (
            ListenerAction::SetBool { .. } | ListenerAction::ToggleBool { .. },
            InputKind::Bool { .. },
        ) | (ListenerAction::SetNumber { .. }, InputKind::Number { .. },)
            | (ListenerAction::FireTrigger { .. }, InputKind::Trigger,)
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GraphRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl GraphRect {
    pub fn contains(self, point: glam::DVec2) -> bool {
        point.x >= self.x
            && point.y >= self.y
            && point.x <= self.x + self.width
            && point.y <= self.y + self.height
    }

    pub fn center(self) -> glam::DVec2 {
        glam::DVec2::new(self.x + self.width * 0.5, self.y + self.height * 0.5)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GraphState {
    pub layer: usize,
    pub state: usize,
    pub rect: GraphRect,
    pub entry: bool,
}

pub fn auto_layout(machine: &Machine) -> Vec<GraphState> {
    const LEFT: f64 = 32.0;
    const TOP: f64 = 32.0;
    const WIDTH: f64 = 144.0;
    const HEIGHT: f64 = 56.0;
    const COLUMN_GAP: f64 = 48.0;
    const LAYER_GAP: f64 = 120.0;

    let mut output = Vec::new();

    for (layer_index, layer) in machine.layers.iter().enumerate() {
        for state_index in 0..layer.states.len() {
            output.push(GraphState {
                layer: layer_index,
                state: state_index,
                rect: GraphRect {
                    x: LEFT + state_index as f64 * (WIDTH + COLUMN_GAP),
                    y: TOP + layer_index as f64 * (HEIGHT + LAYER_GAP),
                    width: WIDTH,
                    height: HEIGHT,
                },
                entry: layer.entry == state_index,
            });
        }
    }

    output
}

pub fn hit_state(layout: &[GraphState], position: glam::DVec2) -> Option<(usize, usize)> {
    layout
        .iter()
        .rev()
        .find(|state| state.rect.contains(position))
        .map(|state| (state.layer, state.state))
}

/// Hit a transition edge near `position`. Returns (layer, source, transition).
pub fn hit_transition(
    machine: &Machine,
    layout: &[GraphState],
    position: glam::DVec2,
    tolerance: f64,
) -> Option<(usize, TransitionSource, usize)> {
    let mut best: Option<(f64, usize, TransitionSource, usize)> = None;

    for (li, layer) in machine.layers.iter().enumerate() {
        for (si, state) in layer.states.iter().enumerate() {
            let from = state_center(layout, li, si);
            for (ti, tr) in state.transitions.iter().enumerate() {
                let to = state_center(layout, li, tr.to);
                let d = dist_point_segment(position, from, to);
                if d <= tolerance && best.as_ref().map(|(bd, ..)| d < *bd).unwrap_or(true) {
                    best = Some((d, li, TransitionSource::State(si), ti));
                }
            }
        }
        if !layer.states.is_empty() {
            let first = state_center(layout, li, 0);
            let from = glam::DVec2::new(first.x - 48.0, first.y);
            for (ti, tr) in layer.any_transitions.iter().enumerate() {
                let to = state_center(layout, li, tr.to);
                let d = dist_point_segment(position, from, to);
                if d <= tolerance && best.as_ref().map(|(bd, ..)| d < *bd).unwrap_or(true) {
                    best = Some((d, li, TransitionSource::Any, ti));
                }
            }
        }
    }
    best.map(|(_, l, s, t)| (l, s, t))
}

fn state_center(layout: &[GraphState], layer: usize, state: usize) -> glam::DVec2 {
    layout
        .iter()
        .find(|g| g.layer == layer && g.state == state)
        .map(|g| g.rect.center())
        .unwrap_or(glam::DVec2::ZERO)
}

fn dist_point_segment(p: glam::DVec2, a: glam::DVec2, b: glam::DVec2) -> f64 {
    let ab = b - a;
    let t = if ab.length_squared() < 1e-9 {
        0.0
    } else {
        ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0)
    };
    (a + ab * t - p).length()
}

#[cfg(test)]
mod tests {
    use super::*;
    use renamite_machine::Machine;

    fn sample_machine() -> Machine {
        Machine {
            name: "sample".into(),
            inputs: vec![InputDef {
                name: "hover".into(),
                kind: InputKind::Bool { default: false },
            }],
            layers: vec![MachineLayer {
                name: "base".into(),
                entry: 0,
                any_transitions: Vec::new(),
                states: vec![
                    State {
                        name: "Idle".into(),
                        kind: StateKind::Empty,
                        transitions: Vec::new(),
                    },
                    State {
                        name: "Hover".into(),
                        kind: StateKind::Empty,
                        transitions: Vec::new(),
                    },
                    State {
                        name: "Active".into(),
                        kind: StateKind::Empty,
                        transitions: Vec::new(),
                    },
                ],
            }],
            listeners: Vec::new(),
        }
    }

    #[test]
    fn removing_state_repairs_transition_targets() {
        let mut machine = sample_machine();

        add_transition(&mut machine, 0, TransitionSource::State(0), 1).unwrap();

        add_transition(&mut machine, 0, TransitionSource::State(0), 2).unwrap();

        machine.layers[0].entry = 2;

        remove_state(&mut machine, 0, 1).unwrap();

        assert_eq!(machine.layers[0].entry, 1,);

        assert_eq!(machine.layers[0].states[0].transitions.len(), 1,);

        assert_eq!(machine.layers[0].states[0].transitions[0].to, 1,);
    }

    #[test]
    fn referenced_input_cannot_be_removed() {
        let mut machine = sample_machine();

        add_transition(&mut machine, 0, TransitionSource::State(0), 1).unwrap();

        transition_mut(&mut machine, 0, TransitionSource::State(0), 0)
            .unwrap()
            .conditions
            .push(Condition::BoolIs {
                input: 0,
                value: true,
            });

        assert!(matches!(
            remove_input(&mut machine, 0),
            Err(MachineEditError::InputInUse)
        ));
    }

    #[test]
    fn unreferenced_input_remaps_later_indices() {
        let mut machine = sample_machine();
        machine.inputs.push(InputDef {
            name: "second".into(),
            kind: InputKind::Bool { default: false },
        });

        // Condition references input 1 (which shifts to 0 after removal).
        add_transition(&mut machine, 0, TransitionSource::State(0), 1).unwrap();

        transition_mut(&mut machine, 0, TransitionSource::State(0), 0)
            .unwrap()
            .conditions
            .push(Condition::BoolIs {
                input: 1,
                value: true,
            });

        remove_input(&mut machine, 0).unwrap();

        let Condition::BoolIs { input, .. } =
            machine.layers[0].states[0].transitions[0].conditions[0]
        else {
            panic!("expected BoolIs");
        };

        assert_eq!(input, 0);
    }

    #[test]
    fn auto_layout_is_deterministic() {
        let machine = sample_machine();
        let a = auto_layout(&machine);
        let b = auto_layout(&machine);
        assert_eq!(a, b);

        let hit = hit_state(&a, a[1].rect.center());
        assert_eq!(hit, Some((0, 1)));
    }

    #[test]
    fn duplicate_names_and_empty_rejected() {
        let mut machine = sample_machine();

        assert!(matches!(
            add_input(&mut machine, "hover", InputKind::Bool { default: false }),
            Err(MachineEditError::DuplicateName(_))
        ));
        assert!(matches!(
            add_input(&mut machine, "   ", InputKind::Bool { default: false }),
            Err(MachineEditError::EmptyName)
        ));
    }

    #[test]
    fn invalid_condition_input_kind_rejected() {
        let mut machine = sample_machine();

        add_transition(&mut machine, 0, TransitionSource::State(0), 1).unwrap();

        // A NumberCmp on a Bool input is a type mismatch.
        assert!(matches!(
            add_condition(
                &mut machine,
                0,
                TransitionSource::State(0),
                0,
                Condition::NumberCmp {
                    input: 0,
                    op: CmpOp::Ge,
                    value: 0.5,
                },
            ),
            Err(MachineEditError::InputTypeMismatch)
        ));

        assert!(
            add_condition(
                &mut machine,
                0,
                TransitionSource::State(0),
                0,
                Condition::BoolIs {
                    input: 0,
                    value: true,
                },
            )
            .is_ok()
        );
    }

    #[test]
    fn adding_an_unconditional_duplicate_transition_rejected() {
        let mut machine = sample_machine();

        add_transition(&mut machine, 0, TransitionSource::State(0), 1).unwrap();

        assert!(matches!(
            add_transition(&mut machine, 0, TransitionSource::State(0), 1,),
            Err(MachineEditError::DuplicateTransition)
        ));
    }
}
