//! Interactivity panel: author state machines (inputs, states, transitions,
//! listeners) and run them live against the editor `Engine`.

use std::rc::Rc;

use glam::DVec2;
use renamite_animation::LoopMode;
use renamite_behavior_common::machine::{
    MachineSelection, TransitionSource, add_condition, add_input, add_listener, add_state,
    add_transition, auto_layout, default_condition, hit_state, hit_transition, input_is_referenced,
    remove_input, remove_state, rename_input, rename_state, set_entry_state,
};
use renamite_machine::{
    BlendChild, ClipId, Condition, InputDef, InputKind, InputValue, Listener, ListenerAction,
    Machine, MachineId, PointerEventKind, State, StateKind,
};
use renamite_model::NodeId;
use repose_canvas::{Canvas, DrawScope};
use repose_core::geometry::Rect;
use repose_core::input::PointerEvent;
use repose_core::{
    AlignItems, Color, Modifier, PaddingValues, Vec2, View, remember_with_key, request_frame, theme,
};
use repose_material::material3::{
    Button, ButtonConfig, DropdownMenu, DropdownMenuConfig, DropdownMenuEntry, DropdownMenuItem,
    MenuState, TextField, TextFieldConfig,
};
use repose_ui::overlay::OverlayHandle;
use repose_ui::scroll::{ScrollArea, remember_scroll_state};
use repose_ui::{Box, Column, Row, Text, TextStyle, ViewExt};

use crate::components::{CompactIconAction, PanelHeader};
use crate::session::{MachineGraphGesture, MachinePreviewDrag, SessionRef};
use crate::symbols::Symbols;

pub fn InteractivityPanel(session: SessionRef) -> View {
    let active = session.borrow().active_machine;

    let mut children: Vec<View> = vec![PanelHeader(
        Symbols::account_tree,
        "Interactivity",
        vec![
            CompactIconAction(Symbols::add, "New state machine", {
                let session = session.clone();
                move || session.borrow_mut().create_machine()
            }),
            CompactIconAction(
                if session.borrow().machine_preview_enabled {
                    Symbols::pause
                } else {
                    Symbols::play_arrow
                },
                "Toggle preview",
                {
                    let session = session.clone();
                    move || {
                        let mut session = session.borrow_mut();
                        session.machine_preview_enabled = !session.machine_preview_enabled;
                        session.reset_machine_preview();
                    }
                },
            ),
        ],
    )];

    children.push(MachineSelector(session.clone()));

    match active {
        Some(machine) => {
            children.push(ScrollArea(
                Modifier::new().fill_max_size(),
                remember_scroll_state("interact_scroll"),
                MachineBody(session.clone(), machine),
            ));
        }
        None => {
            children.push(EmptyMachineState(session));
        }
    }

    Column(Modifier::new().fill_max_size()).child(children)
}

fn EmptyMachineState(session: SessionRef) -> View {
    let th = theme();
    Column(Modifier::new().fill_max_size().gap(8.0)).child((
        Text("No state machines yet")
            .size(th.typography.body_medium)
            .color(th.on_surface_variant)
            .modifier(Modifier::new().padding(16.0)),
        Box(Modifier::new().padding_values(PaddingValues {
            left: 16.0,
            right: 16.0,
            top: 0.0,
            bottom: 0.0,
        }))
        .child(Button(
            Modifier::new(),
            {
                let session = session.clone();
                move || session.borrow_mut().create_machine()
            },
            ButtonConfig::default(),
            || Text("Create state machine").size(th.typography.label_large),
        )),
    ))
}

fn MachineSelector(session: SessionRef) -> View {
    let (machines, active) = {
        let s = session.borrow();
        (s.file.machine_order.clone(), s.active_machine)
    };
    if machines.is_empty() {
        return Box(Modifier::new().height(8.0));
    }
    let th = theme();

    let mut chips = Vec::new();
    for id in machines {
        let name = session
            .borrow()
            .file
            .machines
            .get(id)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| "Machine".into());
        let is_active = active == Some(id);
        chips.push(
            Text(name)
                .size(th.typography.label_medium)
                .color(if is_active {
                    th.primary
                } else {
                    th.on_surface_variant
                })
                .modifier(
                    Modifier::new()
                        .padding_values(PaddingValues {
                            left: 10.0,
                            right: 10.0,
                            top: 6.0,
                            bottom: 6.0,
                        })
                        .background(if is_active {
                            th.secondary_container
                        } else {
                            th.surface_container
                        })
                        .clip_rounded(8.0)
                        .on_pointer_down({
                            let session = session.clone();
                            move |_| {
                                session.borrow_mut().select_machine(id);
                            }
                        }),
                ),
        );
    }

    if active.is_some() {
        chips.push(CompactIconAction(Symbols::delete, "Delete machine", {
            let session = session.clone();
            move || session.borrow_mut().remove_active_machine()
        }));
    }

    Row(Modifier::new()
        .fill_max_width()
        .padding_values(PaddingValues {
            left: 8.0,
            right: 8.0,
            top: 4.0,
            bottom: 4.0,
        })
        .gap(6.0)
        .align_items(AlignItems::CENTER))
    .child(chips)
}

fn MachineBody(session: SessionRef, machine_id: MachineId) -> View {
    Column(Modifier::new().fill_max_width()).child((
        InputsSection(session.clone(), machine_id),
        MachineGraph(session.clone(), machine_id),
        SelectionInspector(session.clone(), machine_id),
        ListenersSection(session, machine_id),
    ))
}

fn InputsSection(session: SessionRef, machine_id: MachineId) -> View {
    let (inputs, preview) = {
        let session = session.borrow();
        (
            session.file.machines[machine_id].inputs.clone(),
            session.machine_preview_inputs.clone(),
        )
    };

    let mut rows = vec![
        Row(Modifier::new()
            .height(36.0)
            .fill_max_width()
            .padding_values(PaddingValues {
                left: 12.0,
                right: 8.0,
                top: 0.0,
                bottom: 0.0,
            })
            .align_items(AlignItems::CENTER))
        .child((
            Text("Inputs")
                .size(theme().typography.label_medium)
                .color(theme().on_surface_variant)
                .modifier(Modifier::new().flex_grow(1.0)),
            CompactIconAction(
                Symbols::add,
                "Add boolean input",
                add_input_action(session.clone(), machine_id, "Boolean", || InputKind::Bool {
                    default: false,
                }),
            ),
            CompactIconAction(
                Symbols::add,
                "Add number input",
                add_input_action(session.clone(), machine_id, "Number", || {
                    InputKind::Number { default: 0.0 }
                }),
            ),
            CompactIconAction(
                Symbols::add,
                "Add trigger",
                add_input_action(session.clone(), machine_id, "Trigger", || {
                    InputKind::Trigger
                }),
            ),
        )),
    ];

    for (index, input) in inputs.into_iter().enumerate() {
        rows.push(InputRow(
            session.clone(),
            machine_id,
            index,
            input,
            preview.get(index).copied(),
        ));
    }

    Column(Modifier::new().fill_max_width()).child(rows)
}

fn add_input_action(
    session: SessionRef,
    machine_id: MachineId,
    base: &'static str,
    kind: impl Fn() -> InputKind + 'static,
) -> impl Fn() + 'static {
    move || {
        let mut s = session.borrow_mut();
        let name = {
            let machine = s.file.machines.get(machine_id);
            machine
                .map(|m| unique_input_name(m, base))
                .unwrap_or_else(|| base.to_string())
        };
        let kind = kind();
        s.edit_active_machine("Add input", move |machine| {
            add_input(machine, name, kind)?;
            Ok(())
        });
    }
}

fn unique_input_name(machine: &Machine, base: &str) -> String {
    let mut name = base.to_string();
    let mut i = 2;
    while machine.inputs.iter().any(|input| input.name == name) {
        name = format!("{base} {i}");
        i += 1;
    }
    name
}

fn InputRow(
    session: SessionRef,
    machine_id: MachineId,
    index: usize,
    input: InputDef,
    preview: Option<InputValue>,
) -> View {
    let th = theme();
    let name = input.name.clone();
    let removable = {
        let s = session.borrow();
        !input_is_referenced(&s.file.machines[machine_id], index)
    };

    let mut controls: Vec<View> = Vec::new();

    match (input.kind, preview) {
        (InputKind::Bool { .. }, Some(InputValue::Bool(value))) => {
            controls.push(chip("On", value, {
                let session = session.clone();
                move || session.borrow_mut().set_preview_bool(index, true)
            }));
            controls.push(chip("Off", !value, {
                let session = session.clone();
                move || session.borrow_mut().set_preview_bool(index, false)
            }));
        }
        (InputKind::Number { .. }, Some(InputValue::Number(value))) => {
            controls.push(machine_scrub_number(
                session.clone(),
                value,
                0.01,
                Rc::new(move |_machine, _v| {}),
            ));
        }
        (InputKind::Trigger, _) => {
            controls.push(Button(
                Modifier::new(),
                {
                    let session = session.clone();
                    move || session.borrow_mut().fire_preview_trigger(index)
                },
                ButtonConfig::default(),
                || Text("Fire").size(th.typography.label_medium),
            ));
        }
        _ => {}
    }

    controls.push(Box(Modifier::new().flex_grow(1.0)).child(TextField(
        Modifier::new().width(120.0),
        name,
        {
            let session = session.clone();
            move |text: String| {
                let mut s = session.borrow_mut();
                s.edit_active_machine("Rename input", move |machine| {
                    rename_input(machine, index, text)?;
                    Ok(())
                });
            }
        },
        TextFieldConfig::default(),
    )));

    if removable {
        controls.push(CompactIconAction(Symbols::delete, "Remove input", {
            let session = session.clone();
            move || {
                let mut s = session.borrow_mut();
                let ok = s.edit_active_machine("Remove input", move |machine| {
                    remove_input(machine, index)?;
                    Ok(())
                });
                if ok {
                    s.machine_selection = MachineSelection::None;
                }
            }
        }));
    } else {
        controls.push(Box(Modifier::new().width(40.0)));
    }

    Row(Modifier::new()
        .height(40.0)
        .fill_max_width()
        .padding_values(PaddingValues {
            left: 12.0,
            right: 8.0,
            top: 0.0,
            bottom: 0.0,
        })
        .gap(6.0)
        .align_items(AlignItems::CENTER))
    .child(controls)
}

fn MachineGraph(session: SessionRef, machine_id: MachineId) -> View {
    let draw_session = session.clone();
    let last_click =
        std::rc::Rc::new(std::cell::RefCell::new(None::<(DVec2, web_time::Instant)>));

    Column(Modifier::new().fill_max_width()).child((
        Row(Modifier::new()
            .fill_max_width()
            .padding_values(PaddingValues {
                left: 12.0,
                right: 8.0,
                top: 4.0,
                bottom: 4.0,
            })
            .gap(6.0)
            .align_items(AlignItems::CENTER))
        .child((
            Text("Graph")
                .size(theme().typography.label_medium)
                .color(theme().on_surface_variant)
                .modifier(Modifier::new().flex_grow(1.0)),
            CompactIconAction(Symbols::add, "Add state", {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    let layer = match s.machine_selection {
                        MachineSelection::State { layer, .. }
                        | MachineSelection::Transition { layer, .. }
                        | MachineSelection::Layer { layer } => layer,
                        _ => 0,
                    };
                    let name = {
                        let m = &s.file.machines[machine_id];
                        let n = m.layers.get(layer).map(|l| l.states.len()).unwrap_or(0) + 1;
                        format!("State {n}")
                    };
                    let ok = s.edit_active_machine("Add state", move |machine| {
                        add_state(machine, layer, name, StateKind::Empty)?;
                        Ok(())
                    });
                    if ok {
                        if let Some(m) = s.file.machines.get(machine_id) {
                            if let Some(l) = m.layers.get(layer) {
                                let state = l.states.len().saturating_sub(1);
                                s.machine_selection = MachineSelection::State { layer, state };
                            }
                        }
                    }
                }
            }),
            Text("Shift+drag state → wire transition · click edge to select")
                .size(theme().typography.label_small)
                .color(theme().on_surface_variant),
        )),
        Canvas(
            Modifier::new()
                .fill_max_width()
                .height(280.0)
                .background(theme().surface_container_lowest)
                .on_pointer_down({
                    let session = session.clone();
                    let last_click = last_click.clone();
                    move |event: PointerEvent| {
                        let position =
                            DVec2::new(event.position.x as f64, event.position.y as f64);
                        let shift = event.modifiers.shift;
                        let now = web_time::Instant::now();
                        let is_double = {
                            let mut lc = last_click.borrow_mut();
                            let dbl = lc
                                .map(|(p, t)| {
                                    (now - t).as_millis() < 350 && (p - position).length() < 8.0
                                })
                                .unwrap_or(false);
                            *lc = Some((position, now));
                            dbl
                        };

                        let machine = session.borrow().file.machines[machine_id].clone();
                        let layout = auto_layout(&machine);

                        let mut s = session.borrow_mut();

                        if is_double {
                            if hit_state(&layout, position).is_none() {
                                // Double-click empty → add state on layer 0.
                                let name = format!(
                                    "State {}",
                                    machine.layers.first().map(|l| l.states.len()).unwrap_or(0) + 1
                                );
                                let ok = s.edit_active_machine("Add state", move |m| {
                                    add_state(m, 0, name, StateKind::Empty)?;
                                    Ok(())
                                });
                                if ok {
                                    if let Some(m) = s.file.machines.get(machine_id) {
                                        if let Some(l) = m.layers.first() {
                                            s.machine_selection = MachineSelection::State {
                                                layer: 0,
                                                state: l.states.len().saturating_sub(1),
                                            };
                                        }
                                    }
                                }
                            }
                            request_frame();
                            return;
                        }

                        if let Some((layer, state)) = hit_state(&layout, position) {
                            s.machine_selection = MachineSelection::State { layer, state };
                            if shift {
                                s.machine_graph_gesture = Some(MachineGraphGesture::WireTransition {
                                    layer,
                                    from_state: state,
                                    current: position,
                                });
                            } else {
                                s.machine_graph_gesture = None;
                            }
                        } else if let Some((layer, source, transition)) =
                            hit_transition(&machine, &layout, position, 8.0)
                        {
                            s.machine_selection = MachineSelection::Transition {
                                layer,
                                source,
                                transition,
                            };
                            s.machine_graph_gesture = None;
                        } else {
                            s.machine_selection = MachineSelection::None;
                            s.machine_graph_gesture = None;
                        }
                        request_frame();
                    }
                })
                .on_pointer_move({
                    let session = session.clone();
                    move |event: PointerEvent| {
                        let position =
                            DVec2::new(event.position.x as f64, event.position.y as f64);
                        let mut s = session.borrow_mut();
                        if let Some(MachineGraphGesture::WireTransition { current, .. }) =
                            s.machine_graph_gesture.as_mut()
                        {
                            *current = position;
                            request_frame();
                        }
                    }
                })
                .on_pointer_up({
                    let session = session.clone();
                    move |event: PointerEvent| {
                        let position =
                            DVec2::new(event.position.x as f64, event.position.y as f64);
                        let mut s = session.borrow_mut();
                        let gesture = s.machine_graph_gesture.take();
                        if let Some(MachineGraphGesture::WireTransition {
                            layer,
                            from_state,
                            ..
                        }) = gesture
                        {
                            let machine = s.file.machines[machine_id].clone();
                            let layout = auto_layout(&machine);
                            if let Some((to_layer, to_state)) = hit_state(&layout, position) {
                                if to_layer == layer && to_state != from_state {
                                    let ok = s.edit_active_machine("Add transition", move |m| {
                                        add_transition(
                                            m,
                                            layer,
                                            TransitionSource::State(from_state),
                                            to_state,
                                        )?;
                                        Ok(())
                                    });
                                    if ok {
                                        if let Some(m) = s.file.machines.get(machine_id) {
                                            if let Some(st) = m
                                                .layers
                                                .get(layer)
                                                .and_then(|l| l.states.get(from_state))
                                            {
                                                let transition =
                                                    st.transitions.len().saturating_sub(1);
                                                s.machine_selection = MachineSelection::Transition {
                                                    layer,
                                                    source: TransitionSource::State(from_state),
                                                    transition,
                                                };
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        request_frame();
                    }
                }),
            move |scope| {
                let session = draw_session.borrow();
                let machine = &session.file.machines[machine_id];
                let layout = auto_layout(machine);
                let active = session.engine.active_machine_states();

                draw_machine_edges(scope, machine, &layout, &session.machine_selection);
                if let Some(MachineGraphGesture::WireTransition {
                    layer,
                    from_state,
                    current,
                }) = &session.machine_graph_gesture
                {
                    let from = layer_state_center(machine, &layout, *layer, *from_state);
                    draw_edge(scope, from, *current, theme().primary.with_alpha(220));
                }
                draw_machine_states(
                    scope,
                    machine,
                    &layout,
                    &session.machine_selection,
                    active.as_deref(),
                );
            },
        ),
    ))
}

fn layer_state_center(
    machine: &Machine,
    layout: &[GraphState],
    layer: usize,
    state: usize,
) -> DVec2 {
    let mut offset = 0usize;
    for (li, l) in machine.layers.iter().enumerate() {
        if li == layer {
            return layout[offset + state].rect.center();
        }
        offset += l.states.len();
    }
    DVec2::ZERO
}

use renamite_behavior_common::machine::GraphState;

fn draw_machine_edges(
    scope: &mut DrawScope,
    machine: &Machine,
    layout: &[GraphState],
    selection: &MachineSelection,
) {
    let th = theme();
    let normal = th.outline.with_alpha(160);
    let selected_color = th.primary;

    for (li, layer) in machine.layers.iter().enumerate() {
        for (si, state) in layer.states.iter().enumerate() {
            let from = layer_state_center(machine, layout, li, si);
            for (ti, tr) in state.transitions.iter().enumerate() {
                let to = layer_state_center(machine, layout, li, tr.to);
                let is_sel = matches!(
                    selection,
                    MachineSelection::Transition {
                        layer,
                        source: TransitionSource::State(src),
                        transition,
                    } if *layer == li && *src == si && *transition == ti
                );
                draw_edge(scope, from, to, if is_sel { selected_color } else { normal });
                draw_arrow_head(scope, from, to, if is_sel { selected_color } else { normal });
            }
        }

        if !layer.any_transitions.is_empty() && !layer.states.is_empty() {
            let first_center = layer_state_center(machine, layout, li, 0);
            let from = DVec2::new(first_center.x - 48.0, first_center.y);
            scope.draw_rect(
                Rect {
                    x: (from.x - 18.0) as f32,
                    y: (from.y - 12.0) as f32,
                    w: 36.0,
                    h: 24.0,
                },
                th.tertiary_container,
                6.0,
            );
            scope.draw_text(
                "Any",
                Vec2 {
                    x: (from.x - 12.0) as f32,
                    y: (from.y - 6.0) as f32,
                },
                th.on_tertiary_container,
                10.0,
            );
            for (ti, tr) in layer.any_transitions.iter().enumerate() {
                let to = layer_state_center(machine, layout, li, tr.to);
                let is_sel = matches!(
                    selection,
                    MachineSelection::Transition {
                        layer,
                        source: TransitionSource::Any,
                        transition,
                    } if *layer == li && *transition == ti
                );
                draw_edge(scope, from, to, if is_sel { selected_color } else { normal });
                draw_arrow_head(scope, from, to, if is_sel { selected_color } else { normal });
            }
        }
    }
}

fn draw_arrow_head(scope: &mut DrawScope, from: DVec2, to: DVec2, color: Color) {
    let dir = to - from;
    let len = dir.length();
    if len < 1.0 {
        return;
    }
    let n = dir / len;
    let tip = to - n * 28.0; // stop short of node
    let perp = DVec2::new(-n.y, n.x);
    let left = tip - n * 10.0 + perp * 5.0;
    let right = tip - n * 10.0 - perp * 5.0;
    draw_polyline_overlay(scope, &[left, tip, right], color);
}

fn draw_edge(scope: &mut DrawScope, from: DVec2, to: DVec2, color: Color) {
    let mut pts = vec![from];
    if from != to {
        let mid = DVec2::new((from.x + to.x) * 0.5, (from.y + to.y) * 0.5);
        pts.push(mid);
        pts.push(to);
    }
    draw_polyline_overlay(scope, &pts, color);
}

fn draw_machine_states(
    scope: &mut DrawScope,
    machine: &Machine,
    layout: &[GraphState],
    selection: &MachineSelection,
    active: Option<&[usize]>,
) {
    let th = theme();

    for gs in layout {
        let state = &machine.layers[gs.layer].states[gs.state];
        let rect = Rect {
            x: gs.rect.x as f32,
            y: gs.rect.y as f32,
            w: gs.rect.width as f32,
            h: gs.rect.height as f32,
        };
        let is_selected = matches!(
            selection,
            MachineSelection::State { layer, state }
                if *layer == gs.layer && *state == gs.state
        );
        let is_active = active
            .and_then(|a| a.get(gs.layer))
            .is_some_and(|current| *current == gs.state);

        let fill = if is_active {
            th.tertiary_container
        } else if gs.entry {
            th.primary_container
        } else {
            th.surface_container_high
        };

        if is_selected {
            scope.draw_rect_stroke(rect, th.primary, 6.0, 2.0);
        }
        scope.draw_rect(rect, fill, 6.0);
        scope.draw_text(
            &state.name,
            Vec2 {
                x: rect.x + 8.0,
                y: rect.y + rect.h * 0.5 - 7.0,
            },
            th.on_surface,
            12.0,
        );
        scope.draw_text(
            kind_label(&state.kind),
            Vec2 {
                x: rect.x + 8.0,
                y: rect.y + rect.h - 15.0,
            },
            th.on_surface_variant,
            9.0,
        );
    }
}

fn kind_label(kind: &StateKind) -> String {
    match kind {
        StateKind::Empty => "empty".into(),
        StateKind::Clip { .. } => "clip".into(),
        StateKind::Blend1D { .. } => "blend".into(),
    }
}

fn SelectionInspector(session: SessionRef, machine_id: MachineId) -> View {
    let selection = session.borrow().machine_selection.clone();

    match selection {
        MachineSelection::State { layer, state } => {
            StateInspector(session, machine_id, layer, state)
        }
        MachineSelection::Transition {
            layer,
            source,
            transition,
        } => TransitionInspector(session, machine_id, layer, source, transition),
        MachineSelection::Input { input } => {
            let _ = input;
            Box(Modifier::new().height(4.0))
        }
        _ => Box(Modifier::new().height(4.0)),
    }
}

fn StateInspector(session: SessionRef, machine_id: MachineId, layer: usize, state: usize) -> View {
    let th = theme();
    let (name, kind, states, is_entry, inputs) = {
        let s = session.borrow();
        let machine = &s.file.machines[machine_id];
        let Some(l) = machine.layers.get(layer) else {
            return Box(Modifier::new().height(4.0));
        };
        let Some(st) = l.states.get(state) else {
            return Box(Modifier::new().height(4.0));
        };
        (
            st.name.clone(),
            st.kind.clone(),
            l.states.len(),
            l.entry == state,
            machine.inputs.clone(),
        )
    };

    let mut rows: Vec<View> = vec![
        Text("State")
            .size(th.typography.label_medium)
            .color(th.on_surface_variant)
            .modifier(Modifier::new().padding_values(PaddingValues {
                left: 12.0,
                right: 12.0,
                top: 12.0,
                bottom: 2.0,
            })),
        Row(Modifier::new()
            .fill_max_width()
            .padding_values(PaddingValues {
                left: 12.0,
                right: 8.0,
                top: 0.0,
                bottom: 2.0,
            })
            .gap(6.0)
            .align_items(AlignItems::CENTER))
        .child(TextField(
            Modifier::new().fill_max_width(),
            name.clone(),
            {
                let session = session.clone();
                move |text: String| {
                    let mut s = session.borrow_mut();
                    s.edit_active_machine("Rename state", move |machine| {
                        rename_state(machine, layer, state, text)?;
                        Ok(())
                    });
                }
            },
            TextFieldConfig::default(),
        )),
        Row(Modifier::new()
            .fill_max_width()
            .padding_values(PaddingValues {
                left: 12.0,
                right: 8.0,
                top: 2.0,
                bottom: 2.0,
            })
            .gap(6.0)
            .align_items(AlignItems::CENTER))
        .child((
            Text("Kind")
                .size(th.typography.body_medium)
                .color(th.on_surface)
                .modifier(Modifier::new().width(48.0)),
            chip(
                "Empty",
                matches!(kind, StateKind::Empty),
                set_kind_action(session.clone(), layer, state, StateKind::Empty),
            ),
            chip("Clip", matches!(kind, StateKind::Clip { .. }), {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    let Some(clip) = s.file.clip_order.first().copied() else {
                        s.status = Some("Add a clip in the Assets panel first".into());
                        return;
                    };
                    s.edit_active_machine("Change state", move |machine| {
                        machine.layers[layer].states[state].kind = StateKind::Clip {
                            clip,
                            speed: 1.0,
                            loop_mode: LoopMode::Once,
                        };
                        Ok(())
                    });
                }
            }),
            chip(
                "Blend1D",
                matches!(kind, StateKind::Blend1D { .. }),
                set_kind_action(
                    session.clone(),
                    layer,
                    state,
                    StateKind::Blend1D {
                        input: number_input_index(&inputs).unwrap_or(0),
                        children: Vec::new(),
                    },
                ),
            ),
        )),
    ];

    match &kind {
        StateKind::Clip {
            clip,
            speed,
            loop_mode,
        } => {
            rows.push(
                Row(Modifier::new()
                    .fill_max_width()
                    .padding_values(PaddingValues {
                        left: 12.0,
                        right: 8.0,
                        top: 2.0,
                        bottom: 2.0,
                    })
                    .gap(6.0)
                    .align_items(AlignItems::CENTER))
                .child((
                    Text("Clip")
                        .size(th.typography.body_medium)
                        .color(th.on_surface)
                        .modifier(Modifier::new().width(48.0)),
                    Box(Modifier::new().flex_grow(1.0)).child(clip_dropdown(
                        session.clone(),
                        machine_id,
                        layer,
                        state,
                        *clip,
                    )),
                )),
            );
            let speed = *speed;
            rows.push(
                Row(Modifier::new()
                    .fill_max_width()
                    .padding_values(PaddingValues {
                        left: 12.0,
                        right: 8.0,
                        top: 2.0,
                        bottom: 2.0,
                    })
                    .gap(6.0)
                    .align_items(AlignItems::CENTER))
                .child((
                    Text("Speed")
                        .size(th.typography.body_medium)
                        .color(th.on_surface)
                        .modifier(Modifier::new().width(48.0)),
                    machine_scrub_number(
                        session.clone(),
                        speed,
                        0.1,
                        Rc::new(move |machine, value| {
                            if let StateKind::Clip { speed, .. } =
                                &mut machine.layers[layer].states[state].kind
                            {
                                *speed = value.max(0.0);
                            }
                        }),
                    ),
                )),
            );
            let loop_mode = *loop_mode;
            rows.push(
                Row(Modifier::new()
                    .fill_max_width()
                    .padding_values(PaddingValues {
                        left: 12.0,
                        right: 8.0,
                        top: 2.0,
                        bottom: 2.0,
                    })
                    .gap(6.0)
                    .align_items(AlignItems::CENTER))
                .child((
                    Text("Loop")
                        .size(th.typography.body_medium)
                        .color(th.on_surface)
                        .modifier(Modifier::new().width(48.0)),
                    chip("Once", loop_mode == LoopMode::Once, {
                        let session = session.clone();
                        move || {
                            set_state_kind(session.clone(), layer, state, |k| match k {
                                StateKind::Clip { clip, speed, .. } => StateKind::Clip {
                                    clip,
                                    speed,
                                    loop_mode: LoopMode::Once,
                                },
                                other => other,
                            })
                        }
                    }),
                    chip("Loop", loop_mode == LoopMode::Loop, {
                        let session = session.clone();
                        move || {
                            set_state_kind(session.clone(), layer, state, |k| match k {
                                StateKind::Clip { clip, speed, .. } => StateKind::Clip {
                                    clip,
                                    speed,
                                    loop_mode: LoopMode::Loop,
                                },
                                other => other,
                            })
                        }
                    }),
                )),
            );
        }
        StateKind::Blend1D { input, .. } => {
            rows.push(
                Row(Modifier::new()
                    .fill_max_width()
                    .padding_values(PaddingValues {
                        left: 12.0,
                        right: 8.0,
                        top: 2.0,
                        bottom: 2.0,
                    })
                    .gap(6.0)
                    .align_items(AlignItems::CENTER))
                .child((
                    Text("Input")
                        .size(th.typography.body_medium)
                        .color(th.on_surface)
                        .modifier(Modifier::new().width(48.0)),
                    Box(Modifier::new().flex_grow(1.0)).child(blend_input_dropdown(
                        session.clone(),
                        machine_id,
                        layer,
                        state,
                        *input,
                    )),
                )),
            );
        }
        StateKind::Empty => {}
    }

    rows.push(
        Row(Modifier::new()
            .fill_max_width()
            .padding_values(PaddingValues {
                left: 12.0,
                right: 8.0,
                top: 2.0,
                bottom: 2.0,
            })
            .gap(6.0)
            .align_items(AlignItems::CENTER))
        .child((
            chip("Mark entry", is_entry, {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    s.edit_active_machine("Set entry", move |machine| {
                        set_entry_state(machine, layer, state)?;
                        Ok(())
                    });
                }
            }),
            CompactIconAction(Symbols::delete, "Delete state", {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    let ok = s.edit_active_machine("Delete state", move |machine| {
                        remove_state(machine, layer, state)?;
                        Ok(())
                    });
                    if ok {
                        s.machine_selection = MachineSelection::None;
                    }
                }
            }),
        )),
    );

    // Outgoing transitions.
    rows.push(
        Text("Transitions")
            .size(th.typography.label_medium)
            .color(th.on_surface_variant)
            .modifier(Modifier::new().padding_values(PaddingValues {
                left: 12.0,
                right: 12.0,
                top: 10.0,
                bottom: 2.0,
            })),
    );

    let (transitions, any_transitions) = {
        let s = session.borrow();
        let machine = &s.file.machines[machine_id];
        let l = &machine.layers[layer];
        let st = &l.states[state];
        (st.transitions.clone(), l.any_transitions.clone())
    };

    for (index, tr) in transitions.iter().enumerate() {
        rows.push(transition_row(
            session.clone(),
            machine_id,
            layer,
            TransitionSource::State(state),
            index,
            tr.to,
            &tr.conditions,
        ));
    }

    for (index, tr) in any_transitions.iter().enumerate() {
        rows.push(transition_row(
            session.clone(),
            machine_id,
            layer,
            TransitionSource::Any,
            index,
            tr.to,
            &tr.conditions,
        ));
    }

    rows.push(
        Row(Modifier::new()
            .fill_max_width()
            .padding_values(PaddingValues {
                left: 12.0,
                right: 8.0,
                top: 2.0,
                bottom: 2.0,
            })
            .gap(6.0)
            .align_items(AlignItems::CENTER))
        .child((
            Text("Add transition")
                .size(th.typography.body_medium)
                .color(th.on_surface_variant)
                .modifier(Modifier::new().flex_grow(1.0)),
            Box(Modifier::new().flex_grow(0.0)).child(transition_target_dropdown(
                session.clone(),
                machine_id,
                layer,
                state,
                TransitionSource::State(state),
                states,
            )),
        )),
    );

    Column(Modifier::new().fill_max_width()).child(rows)
}

fn set_kind_action(
    session: SessionRef,
    layer: usize,
    state: usize,
    kind: StateKind,
) -> impl Fn() + 'static {
    move || {
        let mut s = session.borrow_mut();
        s.edit_active_machine("Change state", |machine| {
            machine.layers[layer].states[state].kind = kind.clone();
            Ok(())
        });
    }
}

fn set_state_kind(
    session: SessionRef,
    layer: usize,
    state: usize,
    map: impl Fn(StateKind) -> StateKind,
) {
    let mut s = session.borrow_mut();
    s.edit_active_machine("Change state", move |machine| {
        let kind = machine.layers[layer].states[state].kind.clone();
        machine.layers[layer].states[state].kind = map(kind);
        Ok(())
    });
}

fn number_input_index(inputs: &[InputDef]) -> Option<usize> {
    inputs
        .iter()
        .position(|input| matches!(input.kind, InputKind::Number { .. }))
}

fn clip_dropdown(
    session: SessionRef,
    machine_id: MachineId,
    layer: usize,
    state: usize,
    current: ClipId,
) -> View {
    let clips = {
        let s = session.borrow();
        s.file.clip_order.clone()
    };
    let names: Vec<(ClipId, String)> = {
        let s = session.borrow();
        clips
            .iter()
            .map(|&id| {
                let name = s
                    .file
                    .clips
                    .get(id)
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| "—".into());
                (id, name)
            })
            .collect()
    };
    let current_name = names
        .iter()
        .find(|(id, _)| *id == current)
        .map(|(_, n)| n.clone())
        .unwrap_or_else(|| "No clip".into());

    let items = names
        .into_iter()
        .map(|(id, name)| {
            DropdownMenuEntry::Item(DropdownMenuItem::new(name, {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    s.edit_active_machine("Change state", move |machine| {
                        let st = &mut machine.layers[layer].states[state];
                        st.kind = StateKind::Clip {
                            clip: id,
                            speed: 1.0,
                            loop_mode: LoopMode::Once,
                        };
                        Ok(())
                    });
                }
            }))
        })
        .collect();

    dropdown(
        format!("clip_{machine_id:?}_{layer}_{state}"),
        format!("{current_name} ▾"),
        items,
    )
}

fn blend_input_dropdown(
    session: SessionRef,
    machine_id: MachineId,
    layer: usize,
    state: usize,
    current: usize,
) -> View {
    let inputs = {
        let s = session.borrow();
        s.file.machines[machine_id].inputs.clone()
    };
    let current_name = inputs
        .get(current)
        .map(|i| i.name.clone())
        .unwrap_or_else(|| "—".into());

    let items = inputs
        .into_iter()
        .filter(|i| matches!(i.kind, InputKind::Number { .. }))
        .map(|i| {
            let idx = {
                let s = session.borrow();
                s.file.machines[machine_id]
                    .inputs
                    .iter()
                    .position(|x| x.name == i.name)
                    .unwrap_or(0)
            };
            DropdownMenuEntry::Item(DropdownMenuItem::new(i.name, {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    s.edit_active_machine("Change state", move |machine| {
                        let st = &mut machine.layers[layer].states[state];
                        let children = blend_children(st);
                        st.kind = StateKind::Blend1D {
                            input: idx,
                            children,
                        };
                        Ok(())
                    });
                }
            }))
        })
        .collect();

    dropdown(
        format!("blend_{machine_id:?}_{layer}_{state}"),
        format!("{current_name} ▾"),
        items,
    )
}

fn blend_children(state: &State) -> Vec<BlendChild> {
    match &state.kind {
        StateKind::Blend1D { children, .. } => children.clone(),
        _ => Vec::new(),
    }
}

fn transition_row(
    session: SessionRef,
    machine_id: MachineId,
    layer: usize,
    source: TransitionSource,
    index: usize,
    target: usize,
    conditions: &[Condition],
) -> View {
    let th = theme();
    let target_name = {
        let s = session.borrow();
        s.file.machines[machine_id].layers[layer]
            .states
            .get(target)
            .map(|st| st.name.clone())
            .unwrap_or_else(|| "—".into())
    };
    let conds = conditions.len();
    let cond_label = if conds == 0 {
        "no conditions".to_string()
    } else {
        format!("{conds} condition{}", if conds == 1 { "" } else { "s" })
    };

    let source_label = match source {
        TransitionSource::Any => "Any".to_string(),
        TransitionSource::State(_) => "→".to_string(),
    };

    Row(Modifier::new()
        .fill_max_width()
        .padding_values(PaddingValues {
            left: 12.0,
            right: 8.0,
            top: 2.0,
            bottom: 2.0,
        })
        .gap(6.0)
        .align_items(AlignItems::CENTER)
        .background(th.surface_container)
        .clip_rounded(6.0)
        .on_pointer_down({
            let session = session.clone();
            move |_| {
                let mut s = session.borrow_mut();
                s.machine_selection = MachineSelection::Transition {
                    layer,
                    source,
                    transition: index,
                };
                request_frame();
            }
        }))
    .child((
        Text(format!("{source_label} {target_name}"))
            .size(th.typography.body_medium)
            .color(th.on_surface)
            .modifier(Modifier::new().flex_grow(1.0)),
        Text(cond_label)
            .size(th.typography.label_small)
            .color(th.on_surface_variant),
        CompactIconAction(Symbols::delete, "Remove transition", {
            let session = session.clone();
            move || {
                let mut s = session.borrow_mut();
                s.edit_active_machine("Remove transition", move |machine| {
                    let transitions = match source {
                        TransitionSource::Any => &mut machine.layers[layer].any_transitions,
                        TransitionSource::State(si) => {
                            &mut machine.layers[layer].states[si].transitions
                        }
                    };
                    if index < transitions.len() {
                        transitions.remove(index);
                    }
                    Ok(())
                });
            }
        }),
    ))
}

fn transition_target_dropdown(
    session: SessionRef,
    machine_id: MachineId,
    layer: usize,
    source_state: usize,
    source: TransitionSource,
    state_count: usize,
) -> View {
    let items = (0..state_count)
        .map(|target| {
            let target_name = {
                let s = session.borrow();
                s.file.machines[machine_id].layers[layer]
                    .states
                    .get(target)
                    .map(|st| st.name.clone())
                    .unwrap_or_else(|| target.to_string())
            };
            DropdownMenuEntry::Item(DropdownMenuItem::new(target_name, {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    let ok = s.edit_active_machine("Add transition", move |machine| {
                        add_transition(machine, layer, source, target)?;
                        Ok(())
                    });
                    if ok {
                        s.machine_selection = MachineSelection::Transition {
                            layer,
                            source,
                            transition: 0,
                        };
                    }
                }
            }))
        })
        .collect();

    dropdown(
        format!("target_{machine_id:?}_{layer}_{source_state}"),
        format!("{} ▾", Symbols::add.name),
        items,
    )
}

fn TransitionInspector(
    session: SessionRef,
    machine_id: MachineId,
    layer: usize,
    source: TransitionSource,
    transition: usize,
) -> View {
    let th = theme();
    let (state_count, duration, exit_time, conditions, inputs) = {
        let s = session.borrow();
        let machine = &s.file.machines[machine_id];
        let Some(l) = machine.layers.get(layer) else {
            return Box(Modifier::new().height(4.0));
        };
        let transitions = match source {
            TransitionSource::Any => &l.any_transitions,
            TransitionSource::State(si) => &l.states[si].transitions,
        };
        let Some(tr) = transitions.get(transition) else {
            return Box(Modifier::new().height(4.0));
        };
        (
            l.states.len(),
            tr.duration,
            tr.exit_time,
            tr.conditions.clone(),
            machine.inputs.clone(),
        )
    };

    let mut rows: Vec<View> = vec![
        Text("Transition")
            .size(th.typography.label_medium)
            .color(th.on_surface_variant)
            .modifier(Modifier::new().padding_values(PaddingValues {
                left: 12.0,
                right: 12.0,
                top: 12.0,
                bottom: 2.0,
            })),
        Row(Modifier::new()
            .fill_max_width()
            .padding_values(PaddingValues {
                left: 12.0,
                right: 8.0,
                top: 2.0,
                bottom: 2.0,
            })
            .gap(6.0)
            .align_items(AlignItems::CENTER))
        .child((
            Text("Target")
                .size(th.typography.body_medium)
                .color(th.on_surface)
                .modifier(Modifier::new().width(48.0)),
            Box(Modifier::new().flex_grow(1.0)).child(transition_target_dropdown(
                session.clone(),
                machine_id,
                layer,
                0,
                source,
                state_count,
            )),
        )),
        Row(Modifier::new()
            .fill_max_width()
            .padding_values(PaddingValues {
                left: 12.0,
                right: 8.0,
                top: 2.0,
                bottom: 2.0,
            })
            .gap(6.0)
            .align_items(AlignItems::CENTER))
        .child((
            Text("Duration")
                .size(th.typography.body_medium)
                .color(th.on_surface)
                .modifier(Modifier::new().width(48.0)),
            machine_scrub_number(
                session.clone(),
                duration,
                1.0,
                Rc::new(move |machine, value| {
                    let transitions = match source {
                        TransitionSource::Any => &mut machine.layers[layer].any_transitions,
                        TransitionSource::State(si) => {
                            &mut machine.layers[layer].states[si].transitions
                        }
                    };
                    if let Some(tr) = transitions.get_mut(transition) {
                        tr.duration = value.max(0.0);
                    }
                }),
            ),
        )),
    ];

    rows.push(
        Row(Modifier::new()
            .fill_max_width()
            .padding_values(PaddingValues {
                left: 12.0,
                right: 8.0,
                top: 2.0,
                bottom: 2.0,
            })
            .gap(6.0)
            .align_items(AlignItems::CENTER))
        .child((
            Text("Exit time")
                .size(th.typography.body_medium)
                .color(th.on_surface)
                .modifier(Modifier::new().width(48.0)),
            chip("When finished", exit_time.is_some(), {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    s.edit_active_machine("Transition", move |machine| {
                        let transitions = match source {
                            TransitionSource::Any => &mut machine.layers[layer].any_transitions,
                            TransitionSource::State(si) => {
                                &mut machine.layers[layer].states[si].transitions
                            }
                        };
                        if let Some(tr) = transitions.get_mut(transition) {
                            tr.exit_time = match tr.exit_time {
                                Some(v) => {
                                    if v >= 1.0 {
                                        None
                                    } else {
                                        Some(1.0)
                                    }
                                }
                                None => Some(1.0),
                            };
                        }
                        Ok(())
                    });
                }
            }),
        )),
    );

    if let Some(exit) = exit_time {
        rows.push(
            Row(Modifier::new()
                .fill_max_width()
                .padding_values(PaddingValues {
                    left: 12.0,
                    right: 8.0,
                    top: 2.0,
                    bottom: 2.0,
                })
                .gap(6.0)
                .align_items(AlignItems::CENTER))
            .child((
                Text("Value")
                    .size(th.typography.body_medium)
                    .color(th.on_surface)
                    .modifier(Modifier::new().width(48.0)),
                machine_scrub_number(
                    session.clone(),
                    exit,
                    0.01,
                    Rc::new(move |machine, value| {
                        let transitions = match source {
                            TransitionSource::Any => &mut machine.layers[layer].any_transitions,
                            TransitionSource::State(si) => {
                                &mut machine.layers[layer].states[si].transitions
                            }
                        };
                        if let Some(tr) = transitions.get_mut(transition) {
                            tr.exit_time = Some(value.clamp(0.0, 1.0));
                        }
                    }),
                ),
            )),
        );
    }

    rows.push(
        Text("Conditions")
            .size(th.typography.label_medium)
            .color(th.on_surface_variant)
            .modifier(Modifier::new().padding_values(PaddingValues {
                left: 12.0,
                right: 12.0,
                top: 10.0,
                bottom: 2.0,
            })),
    );

    for (index, condition) in conditions.iter().enumerate() {
        let label = condition_label(&inputs, condition);
        rows.push(
            Row(Modifier::new()
                .fill_max_width()
                .padding_values(PaddingValues {
                    left: 12.0,
                    right: 8.0,
                    top: 2.0,
                    bottom: 2.0,
                })
                .gap(6.0)
                .align_items(AlignItems::CENTER))
            .child((
                Text(label)
                    .size(th.typography.body_medium)
                    .color(th.on_surface)
                    .modifier(Modifier::new().flex_grow(1.0)),
                CompactIconAction(Symbols::delete, "Remove condition", {
                    let session = session.clone();
                    move || {
                        let mut s = session.borrow_mut();
                        s.edit_active_machine("Remove condition", move |machine| {
                            let transitions = match source {
                                TransitionSource::Any => &mut machine.layers[layer].any_transitions,
                                TransitionSource::State(si) => {
                                    &mut machine.layers[layer].states[si].transitions
                                }
                            };
                            if let Some(tr) = transitions.get_mut(transition)
                                && index < tr.conditions.len()
                            {
                                tr.conditions.remove(index);
                            }
                            Ok(())
                        });
                    }
                }),
            )),
        );
    }

    rows.push(
        Row(Modifier::new()
            .fill_max_width()
            .padding_values(PaddingValues {
                left: 12.0,
                right: 8.0,
                top: 2.0,
                bottom: 2.0,
            })
            .gap(6.0)
            .align_items(AlignItems::CENTER))
        .child((
            Text("Add condition")
                .size(th.typography.body_medium)
                .color(th.on_surface_variant)
                .modifier(Modifier::new().flex_grow(1.0)),
            Box(Modifier::new().flex_grow(0.0)).child(condition_dropdown(
                session.clone(),
                machine_id,
                layer,
                source,
                transition,
                inputs,
            )),
        )),
    );

    Column(Modifier::new().fill_max_width()).child(rows)
}

fn condition_dropdown(
    session: SessionRef,
    machine_id: MachineId,
    layer: usize,
    source: TransitionSource,
    transition: usize,
    inputs: Vec<InputDef>,
) -> View {
    let items = inputs
        .iter()
        .map(|input| {
            let name = input.name.clone();
            let idx = {
                let s = session.borrow();
                s.file.machines[machine_id]
                    .inputs
                    .iter()
                    .position(|x| x.name == name)
                    .unwrap_or(0)
            };
            DropdownMenuEntry::Item(DropdownMenuItem::new(name, {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    s.edit_active_machine("Add condition", move |machine| {
                        let condition = default_condition(machine, idx)?;
                        add_condition(machine, layer, source, transition, condition)?;
                        Ok(())
                    });
                }
            }))
        })
        .collect();

    dropdown(
        format!("cond_{machine_id:?}_{layer}_{transition:?}"),
        format!("{} ▾", Symbols::add.name),
        items,
    )
}

fn condition_label(inputs: &[InputDef], condition: &Condition) -> String {
    match condition {
        Condition::BoolIs { input, value } => format!("{} == {value}", input_name(inputs, *input)),
        Condition::NumberCmp { input, op, value } => {
            format!("{} {:?} {value:.2}", input_name(inputs, *input), op)
        }
        Condition::Triggered { input } => format!("{} fired", input_name(inputs, *input)),
    }
}

fn input_name(inputs: &[InputDef], index: usize) -> String {
    inputs
        .get(index)
        .map(|i| i.name.clone())
        .unwrap_or_else(|| "?".into())
}

fn ListenersSection(session: SessionRef, machine_id: MachineId) -> View {
    let th = theme();
    let (listeners, inputs, selected_node) = {
        let s = session.borrow();
        let machine = &s.file.machines[machine_id];
        (
            machine.listeners.clone(),
            machine.inputs.clone(),
            s.selection.nodes.first().copied(),
        )
    };

    let mut rows: Vec<View> = vec![
        Text("Listeners")
            .size(th.typography.label_medium)
            .color(th.on_surface_variant)
            .modifier(Modifier::new().padding_values(PaddingValues {
                left: 12.0,
                right: 12.0,
                top: 12.0,
                bottom: 2.0,
            })),
    ];

    for (index, listener) in listeners.iter().enumerate() {
        let node_name = session.borrow().node_name(listener.node);
        let action_label = listener_action_label(&inputs, &listener.action);
        rows.push(
            Row(Modifier::new()
                .fill_max_width()
                .padding_values(PaddingValues {
                    left: 12.0,
                    right: 8.0,
                    top: 2.0,
                    bottom: 2.0,
                })
                .gap(6.0)
                .align_items(AlignItems::CENTER))
            .child((
                Text(format!(
                    "{} {} {}",
                    node_name,
                    listener_event_label(listener.event),
                    action_label
                ))
                .size(th.typography.body_medium)
                .color(th.on_surface)
                .modifier(Modifier::new().flex_grow(1.0)),
                CompactIconAction(Symbols::delete, "Remove listener", {
                    let session = session.clone();
                    move || {
                        let mut s = session.borrow_mut();
                        s.edit_active_machine("Remove listener", move |machine| {
                            if index < machine.listeners.len() {
                                machine.listeners.remove(index);
                            }
                            Ok(())
                        });
                    }
                }),
            )),
        );
    }

    // Add-listener row: node comes from the editor selection.
    if let Some(node) = selected_node {
        rows.push(AddListenerRow(session.clone(), machine_id, node, inputs));
    } else {
        rows.push(
            Text("Select a shape to add a listener")
                .size(th.typography.label_small)
                .color(th.on_surface_variant)
                .modifier(Modifier::new().padding_values(PaddingValues {
                    left: 12.0,
                    right: 12.0,
                    top: 2.0,
                    bottom: 2.0,
                })),
        );
    }

    Column(Modifier::new().fill_max_width()).child(rows)
}

fn AddListenerRow(
    session: SessionRef,
    machine_id: MachineId,
    node: NodeId,
    inputs: Vec<InputDef>,
) -> View {
    let th = theme();
    let node_name = session.borrow().node_name(node);
    let draft = session.borrow().listener_draft.clone();
    let input_kind = draft
        .input
        .and_then(|i| inputs.get(i).map(|input| input.kind));

    let event_items = [
        PointerEventKind::Down,
        PointerEventKind::Up,
        PointerEventKind::Click,
        PointerEventKind::Enter,
        PointerEventKind::Exit,
    ]
    .into_iter()
    .map(|event| {
        DropdownMenuEntry::Item(DropdownMenuItem::new(listener_event_label(event), {
            let session = session.clone();
            move || {
                let mut s = session.borrow_mut();
                s.listener_draft.event = Some(event);
                request_frame();
            }
        }))
    })
    .collect();

    let input_items = inputs
        .iter()
        .map(|input| {
            let name = input.name.clone();
            let idx = {
                let s = session.borrow();
                s.file.machines[machine_id]
                    .inputs
                    .iter()
                    .position(|x| x.name == name)
                    .unwrap_or(0)
            };
            DropdownMenuEntry::Item(DropdownMenuItem::new(name, {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    s.listener_draft.input = Some(idx);
                    request_frame();
                }
            }))
        })
        .collect();

    let mut action_items: Vec<DropdownMenuEntry> = Vec::new();
    match input_kind {
        Some(InputKind::Bool { .. }) => {
            action_items.push(DropdownMenuEntry::Item(DropdownMenuItem::new(
                "Set true",
                {
                    let session = session.clone();
                    move || {
                        let mut s = session.borrow_mut();
                        s.listener_draft.toggle = false;
                        s.listener_draft.bool_value = true;
                        request_frame();
                    }
                },
            )));
            action_items.push(DropdownMenuEntry::Item(DropdownMenuItem::new(
                "Set false",
                {
                    let session = session.clone();
                    move || {
                        let mut s = session.borrow_mut();
                        s.listener_draft.toggle = false;
                        s.listener_draft.bool_value = false;
                        request_frame();
                    }
                },
            )));
            action_items.push(DropdownMenuEntry::Item(DropdownMenuItem::new("Toggle", {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    s.listener_draft.toggle = true;
                    request_frame();
                }
            })));
        }
        Some(InputKind::Number { .. }) => {
            action_items.push(DropdownMenuEntry::Item(DropdownMenuItem::new(
                "Set number",
                {
                    let session = session.clone();
                    move || {
                        let mut s = session.borrow_mut();
                        s.listener_draft.toggle = false;
                        s.listener_draft.number_value = 0.0;
                        request_frame();
                    }
                },
            )));
        }
        Some(InputKind::Trigger) => {
            action_items.push(DropdownMenuEntry::Item(DropdownMenuItem::new(
                "Fire trigger",
                {
                    let session = session.clone();
                    move || {
                        let mut s = session.borrow_mut();
                        s.listener_draft.toggle = false;
                        request_frame();
                    }
                },
            )));
        }
        None => {}
    }

    let event_label = draft
        .event
        .map(listener_event_label)
        .unwrap_or_else(|| "event".into());
    let input_label = draft
        .input
        .map(|i| input_name(&inputs, i))
        .unwrap_or_else(|| "input".into());
    let action_label: String = match (input_kind, draft.toggle, draft.bool_value) {
        (Some(InputKind::Bool { .. }), true, _) => "Toggle".into(),
        (Some(InputKind::Bool { .. }), false, v) => {
            if v {
                "Set true".into()
            } else {
                "Set false".into()
            }
        }
        (Some(InputKind::Number { .. }), _, _) => "Set number".into(),
        (Some(InputKind::Trigger), _, _) => "Fire trigger".into(),
        _ => "action".into(),
    };

    Row(Modifier::new()
        .fill_max_width()
        .padding_values(PaddingValues {
            left: 12.0,
            right: 8.0,
            top: 2.0,
            bottom: 2.0,
        })
        .gap(4.0)
        .align_items(AlignItems::CENTER))
    .child((
        Text(node_name)
            .size(th.typography.label_small)
            .color(th.on_surface_variant),
        dropdown("lst_event", format!("{event_label} ▾"), event_items),
        dropdown("lst_input", format!("{input_label} ▾"), input_items),
        dropdown("lst_action", format!("{action_label} ▾"), action_items),
        Button(
            Modifier::new(),
            {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    let draft = s.listener_draft.clone();
                    let Some(event) = draft.event else {
                        return;
                    };
                    let Some(input) = draft.input else {
                        return;
                    };
                    let action = match input_kind {
                        Some(InputKind::Bool { .. }) => {
                            if draft.toggle {
                                ListenerAction::ToggleBool { input }
                            } else {
                                ListenerAction::SetBool {
                                    input,
                                    value: draft.bool_value,
                                }
                            }
                        }
                        Some(InputKind::Number { .. }) => ListenerAction::SetNumber {
                            input,
                            value: draft.number_value,
                        },
                        Some(InputKind::Trigger) => ListenerAction::FireTrigger { input },
                        None => return,
                    };
                    let listener = Listener {
                        node,
                        event,
                        action,
                    };
                    s.edit_active_machine("Add listener", move |machine| {
                        add_listener(machine, listener)?;
                        Ok(())
                    });
                }
            },
            ButtonConfig::default(),
            || Text("Add").size(th.typography.label_medium),
        ),
    ))
}

fn listener_event_label(event: PointerEventKind) -> String {
    match event {
        PointerEventKind::Down => "Down".into(),
        PointerEventKind::Up => "Up".into(),
        PointerEventKind::Click => "Click".into(),
        PointerEventKind::Enter => "Enter".into(),
        PointerEventKind::Exit => "Exit".into(),
    }
}

fn listener_action_label(inputs: &[InputDef], action: &ListenerAction) -> String {
    match action {
        ListenerAction::SetBool { input, value } => {
            format!("{} = {value}", input_name(inputs, *input))
        }
        ListenerAction::ToggleBool { input } => {
            format!("toggle {}", input_name(inputs, *input))
        }
        ListenerAction::SetNumber { input, value } => {
            format!("{} = {value:.2}", input_name(inputs, *input))
        }
        ListenerAction::FireTrigger { input } => {
            format!("fire {}", input_name(inputs, *input))
        }
    }
}

fn chip(label: &'static str, active: bool, on_click: impl Fn() + 'static) -> View {
    let th = theme();
    Text(label)
        .size(th.typography.body_medium)
        .color(if active {
            th.primary
        } else {
            th.on_surface_variant
        })
        .modifier(
            Modifier::new()
                .padding_values(PaddingValues {
                    left: 8.0,
                    right: 8.0,
                    top: 4.0,
                    bottom: 4.0,
                })
                .background(if active {
                    th.secondary_container
                } else {
                    th.surface
                })
                .clip_rounded(6.0)
                .on_pointer_down(move |_| on_click()),
        )
}

/// Scrubbable number that writes back into the machine via `edit`. The drag
/// gesture is tracked in `session.machine_preview_drag`.
type MachineScrub = Rc<dyn Fn(&mut Machine, f64)>;

fn machine_scrub_number(session: SessionRef, value: f64, step: f64, edit: MachineScrub) -> View {
    let th = theme();
    let label = format!("{value:.2}");
    let key = remember_with_key(format!("scrub_{label}_{value}"), || 0u8);
    let _ = key;

    Text(label)
        .size(th.typography.body_medium)
        .color(th.primary)
        .modifier(
            Modifier::new()
                .min_width(52.0)
                .on_pointer_down({
                    let session = session.clone();
                    move |pe: PointerEvent| {
                        let mut s = session.borrow_mut();
                        s.machine_preview_drag = Some(MachinePreviewDrag {
                            input: usize::MAX,
                            origin: value,
                            press_x: pe.position.x,
                        });
                    }
                })
                .on_pointer_move({
                    let session = session.clone();
                    let edit = edit.clone();
                    move |pe: PointerEvent| {
                        let mut s = session.borrow_mut();
                        let Some(drag) = s.machine_preview_drag else {
                            return;
                        };
                        if drag.input != usize::MAX {
                            return;
                        }
                        let dx = (pe.position.x - drag.press_x) as f64;
                        let mult = if pe.modifiers.shift { 0.1 } else { 1.0 };
                        let new_value = drag.origin + dx * step * mult;
                        let edit = edit.clone();
                        s.edit_active_machine("Edit machine", move |machine| {
                            edit(machine, new_value);
                            Ok(())
                        });
                    }
                })
                .on_pointer_up({
                    let session = session.clone();
                    move |_pe: PointerEvent| {
                        let mut s = session.borrow_mut();
                        if s.machine_preview_drag
                            .is_some_and(|drag| drag.input == usize::MAX)
                        {
                            s.machine_preview_drag = None;
                        }
                        request_frame();
                    }
                }),
        )
}

fn dropdown(key: impl Into<String>, label: String, items: Vec<DropdownMenuEntry>) -> View {
    let key = key.into();
    let state: Rc<MenuState> = remember_with_key(format!("{key}_state"), MenuState::new);
    let overlay: Rc<OverlayHandle> =
        remember_with_key(format!("{key}_overlay"), OverlayHandle::new);
    let th = theme();

    let trigger = Text(label)
        .size(th.typography.body_medium)
        .color(th.primary)
        .modifier(
            Modifier::new()
                .padding_values(PaddingValues {
                    left: 8.0,
                    right: 8.0,
                    top: 4.0,
                    bottom: 4.0,
                })
                .background(th.surface_container)
                .clip_rounded(6.0)
                .on_pointer_down({
                    let state = state.clone();
                    move |_| state.open()
                }),
        );

    DropdownMenu(
        state,
        (*overlay).clone(),
        Modifier::new(),
        trigger,
        items,
        DropdownMenuConfig {
            min_width: 140.0,
            max_width: 220.0,
            ..Default::default()
        },
    )
}

fn draw_polyline_overlay(scope: &mut DrawScope, pts: &[DVec2], color: Color) {
    if pts.len() < 2 {
        return;
    }
    let t = 1.0; // half thickness (px)
    let c = [
        color.0 as f32 / 255.0,
        color.1 as f32 / 255.0,
        color.2 as f32 / 255.0,
        color.3 as f32 / 255.0,
    ];
    let mut vertices = Vec::with_capacity(pts.len() * 2);
    let mut indices = Vec::with_capacity((pts.len() - 1) * 6);
    for pair in pts.windows(2) {
        let a = pair[0];
        let b = pair[1];
        let dir = b - a;
        let len = dir.length();
        if len < 1e-6 {
            continue;
        }
        let n = DVec2::new(-dir.y / len, dir.x / len);
        let base = vertices.len() as u32;
        for p in [a + n * t, a - n * t, b - n * t, b + n * t] {
            vertices.push(repose_core::view::VectorVertex {
                pos: [p.x as f32, p.y as f32],
                color: c,
                uv: [0.0, 0.0],
            });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    if indices.is_empty() {
        return;
    }
    let mesh = repose_core::view::VectorMeshData {
        vertices: std::sync::Arc::from(vertices),
        indices: std::sync::Arc::from(indices),
    };
    scope.draw_vector_overlay(std::sync::Arc::from([mesh]));
}
