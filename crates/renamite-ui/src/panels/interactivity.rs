//! Interactivity panel: author state machines (inputs, states, transitions,
//! listeners) and run them live against the editor `Engine`.

use std::cell::RefCell;
use std::rc::Rc;
use web_time::Instant;

use glam::DVec2;
use renamite_animation::LoopMode;
use renamite_behavior_common::ViewTransform;
use renamite_behavior_common::machine::{
    GraphRect, GraphState, MachineSelection, TransitionSource, add_condition, add_input, add_layer,
    add_listener, add_state, add_transition, auto_layout, default_condition, hit_state,
    hit_transition, input_is_referenced, remove_condition, remove_input, remove_layer,
    remove_listener, remove_state, remove_transition, rename_input, rename_layer, rename_state,
    set_entry_state, set_input_default, set_state_kind as pure_set_state_kind, set_state_position,
    set_transition_target, transition_mut,
};
use renamite_machine::{
    BlendChild, ClipId, CmpOp, Condition, InputDef, InputKind, InputValue, Listener,
    ListenerAction, Machine, MachineId, PointerEventKind, StateKind,
};
use renamite_model::NodeId;
use repose_canvas::{Canvas, DrawScope};
use repose_core::geometry::Rect;
use repose_core::input::PointerEvent;
use repose_core::input::{PointerButton, PointerEventKind as UiPointerEventKind};
use repose_core::{
    AlignItems, Color, Modifier, PaddingValues, Vec2, View, remember_with_key, request_frame, theme,
};
use repose_material::material3::{
    Button, ButtonConfig, DropdownMenu, DropdownMenuConfig, DropdownMenuEntry, DropdownMenuItem,
    MenuState,
};
use repose_ui::overlay::OverlayHandle;
use repose_ui::scroll::{ScrollArea, remember_scroll_state};
use repose_ui::{Box, Column, Row, Text, TextStyle, ViewExt};

use crate::components::{CollapsibleSection, CompactIconAction, PanelHeader};
use crate::session::{MachineGraphGesture, SessionRef};
use crate::symbols::{AppIcon, Symbols};

fn graph_view(machine_id: MachineId) -> Rc<RefCell<ViewTransform>> {
    remember_with_key(format!("machine_graph_view_{machine_id:?}"), || {
        RefCell::new(ViewTransform::identity())
    })
}

pub fn InteractivityPanel(session: SessionRef) -> View {
    let active = session.borrow().active_machine;
    let overlay = remember_with_key("interact_overlay", OverlayHandle::new);

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
    children.push(PreviewStatusBar(session.clone()));

    match active {
        Some(machine) => {
            children.push(ScrollArea(
                Modifier::new().fill_max_size(),
                remember_scroll_state("interact_scroll"),
                MachineBody((*overlay).clone(), session.clone(), machine),
            ));
        }
        None => children.push(EmptyMachineState(session)),
    }

    let panel = Column(Modifier::new().fill_max_size()).child(children);
    overlay.host(Modifier::new().fill_max_size(), panel)
}

fn PreviewStatusBar(session: SessionRef) -> View {
    let th = theme();
    let (enabled, states, name) = {
        let s = session.borrow();
        let name = s
            .active_machine
            .and_then(|id| s.file.machines.get(id).map(|m| m.name.clone()))
            .unwrap_or_default();
        let states = s.engine.active_machine_states();
        (s.machine_preview_enabled, states, name)
    };
    if !enabled {
        return Text("Preview off — press play to drive the machine")
            .size(th.typography.label_small)
            .color(th.on_surface_variant)
            .modifier(Modifier::new().padding_values(PaddingValues {
                left: 12.0,
                right: 12.0,
                top: 2.0,
                bottom: 6.0,
            }));
    }
    let label = match states {
        Some(st) if !st.is_empty() => {
            let names = {
                let s = session.borrow();
                let Some(id) = s.active_machine else {
                    return Box(Modifier::new());
                };
                let m = &s.file.machines[id];
                st.iter()
                    .enumerate()
                    .map(|(li, si)| {
                        m.layers
                            .get(li)
                            .and_then(|l| l.states.get(*si))
                            .map(|s| s.name.as_str())
                            .unwrap_or("?")
                            .to_string()
                    })
                    .collect::<Vec<_>>()
                    .join(" · ")
            };
            format!("▶ {name}  ·  {names}")
        }
        _ => format!("▶ {name}  ·  waiting"),
    };
    Text(label)
        .size(th.typography.label_small)
        .color(th.primary)
        .modifier(Modifier::new().padding_values(PaddingValues {
            left: 12.0,
            right: 12.0,
            top: 2.0,
            bottom: 6.0,
        }))
}

fn EmptyMachineState(session: SessionRef) -> View {
    let th = theme();
    Column(Modifier::new().fill_max_size().gap(8.0)).child((
        Text("No state machines yet")
            .size(th.typography.body_medium)
            .color(th.on_surface_variant)
            .modifier(Modifier::new().padding(16.0)),
        Text("State machines drive clips from inputs and pointer listeners — like Rive Interact.")
            .size(th.typography.body_small)
            .color(th.on_surface_variant)
            .modifier(Modifier::new().padding_values(PaddingValues {
                left: 16.0,
                right: 16.0,
                top: 0.0,
                bottom: 8.0,
            })),
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
    let (machines, active, start) = {
        let s = session.borrow();
        (
            s.file.machine_order.clone(),
            s.active_machine,
            s.file.start_machine,
        )
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
        let is_start = start == Some(id);
        let label = if is_start {
            format!("★ {name}")
        } else {
            name
        };
        chips.push(
            Text(label)
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
        chips.push(CompactIconAction(Symbols::star, "Set as start machine", {
            let session = session.clone();
            move || session.borrow_mut().set_active_as_start_machine()
        }));
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

fn MachineBody(overlay: OverlayHandle, session: SessionRef, machine_id: MachineId) -> View {
    let name = session.borrow().file.machines[machine_id].name.clone();

    Column(Modifier::new().fill_max_width().gap(8.0)).child((
        // Rename
        Row(Modifier::new()
            .fill_max_width()
            .padding_values(PaddingValues {
                left: 12.0,
                right: 8.0,
                top: 4.0,
                bottom: 0.0,
            })
            .gap(6.0)
            .align_items(AlignItems::CENTER))
        .child(crate::components::name_field(
            format!("machine_name_{machine_id:?}"),
            name,
            "Machine name",
            36.0,
            {
                let session = session.clone();
                move |text: String| {
                    session.borrow_mut().rename_active_machine(text);
                }
            },
        )),
        CollapsibleSection(
            "sm_inputs",
            "Inputs",
            vec![],
            InputsSection(session.clone(), machine_id),
        ),
        CollapsibleSection(
            "sm_graph",
            "Graph",
            vec![
                CompactIconAction(Symbols::add, "Add state", {
                    let session = session.clone();
                    move || {
                        let layer = target_layer(&session);
                        add_state_ui(&session, machine_id, layer)
                    }
                }),
                CompactIconAction(Symbols::layers, "Add layer", {
                    let session = session.clone();
                    move || {
                        let mut s = session.borrow_mut();
                        let n = s
                            .file
                            .machines
                            .get(machine_id)
                            .map(|m| m.layers.len() + 1)
                            .unwrap_or(1);
                        s.edit_active_machine("Add layer", move |m| {
                            add_layer(m, format!("Layer {n}"))?;
                            Ok(())
                        });
                    }
                }),
            ],
            MachineGraph(session.clone(), machine_id),
        ),
        CollapsibleSection(
            "sm_inspector",
            "Selection",
            vec![],
            SelectionInspector(overlay.clone(), session.clone(), machine_id),
        ),
        CollapsibleSection(
            "sm_listeners",
            "Listeners",
            vec![],
            ListenersSection(overlay, session, machine_id),
        ),
    ))
}

fn target_layer(session: &SessionRef) -> usize {
    let s = session.borrow();
    match s.machine_selection {
        MachineSelection::State { layer, .. }
        | MachineSelection::Transition { layer, .. }
        | MachineSelection::Layer { layer } => layer,
        _ => s.active_machine_layer,
    }
}

fn add_state_ui(session: &SessionRef, machine_id: MachineId, layer: usize) {
    let mut s = session.borrow_mut();
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
                s.machine_selection = MachineSelection::State {
                    layer,
                    state: l.states.len().saturating_sub(1),
                };
                s.active_machine_layer = layer;
            }
        }
    }
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
            .align_items(AlignItems::CENTER)
            .gap(4.0))
        .child((
            Text("Add")
                .size(theme().typography.label_small)
                .color(theme().on_surface_variant),
            chip("Bool", false, {
                let session = session.clone();
                move || {
                    add_input_named(
                        &session,
                        machine_id,
                        "Boolean",
                        InputKind::Bool { default: false },
                    )
                }
            }),
            chip("Number", false, {
                let session = session.clone();
                move || {
                    add_input_named(
                        &session,
                        machine_id,
                        "Number",
                        InputKind::Number { default: 0.0 },
                    )
                }
            }),
            chip("Trigger", false, {
                let session = session.clone();
                move || add_input_named(&session, machine_id, "Trigger", InputKind::Trigger)
            }),
        )),
    ];

    if inputs.is_empty() {
        rows.push(
            Text("No inputs — add Bool / Number / Trigger to drive transitions")
                .size(theme().typography.label_small)
                .color(theme().on_surface_variant)
                .modifier(Modifier::new().padding(12.0)),
        );
    }

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

fn add_input_named(session: &SessionRef, machine_id: MachineId, base: &str, kind: InputKind) {
    let mut s = session.borrow_mut();
    let name = s
        .file
        .machines
        .get(machine_id)
        .map(|m| unique_input_name(m, base))
        .unwrap_or_else(|| base.to_string());
    s.edit_active_machine("Add input", move |machine| {
        add_input(machine, name, kind)?;
        Ok(())
    });
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

fn input_kind_badge(kind: InputKind) -> &'static str {
    match kind {
        InputKind::Bool { .. } => "B",
        InputKind::Number { .. } => "N",
        InputKind::Trigger => "T",
    }
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

    let mut controls: Vec<View> = vec![
        Text(input_kind_badge(input.kind))
            .size(th.typography.label_small)
            .color(th.on_secondary_container)
            .modifier(
                Modifier::new()
                    .padding_values(PaddingValues {
                        left: 6.0,
                        right: 6.0,
                        top: 2.0,
                        bottom: 2.0,
                    })
                    .background(th.secondary_container)
                    .clip_rounded(4.0),
            ),
    ];

    match input.kind {
        InputKind::Bool { default } => {
            let label: &'static str = if default { "def:on" } else { "def:off" };
            controls.push(chip(label, default, {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    s.edit_active_machine("Input default", move |machine| {
                        let cur = match machine.inputs.get(index).map(|d| d.kind) {
                            Some(InputKind::Bool { default }) => default,
                            _ => default,
                        };
                        set_input_default(machine, index, InputKind::Bool { default: !cur })?;
                        Ok(())
                    });
                }
            }));
        }
        InputKind::Number { default } => {
            let def = default;
            controls.push(machine_scrub_number(
                session.clone(),
                def,
                0.01,
                Rc::new(move |machine, value| {
                    let _ = set_input_default(machine, index, InputKind::Number { default: value });
                }),
            ));
        }
        InputKind::Trigger => {}
    }

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
            controls.push(preview_scrub_number(session.clone(), index, value, 0.01));
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
        _ => {
            controls.push(
                Text("enable preview")
                    .size(th.typography.label_small)
                    .color(th.on_surface_variant),
            );
        }
    }

    controls.push(
        Box(Modifier::new().flex_grow(1.0)).child(crate::components::name_field(
            format!("machine_input_name_{index}"),
            name,
            "Name",
            32.0,
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
        )),
    );

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
    let last_click: Rc<RefCell<Option<(DVec2, Instant)>>> =
        remember_with_key("machine_graph_last_click", || {
            RefCell::new(None::<(DVec2, Instant)>)
        });
    let view = graph_view(machine_id);
    let last_pointer: Rc<RefCell<DVec2>> =
        remember_with_key(format!("machine_graph_last_ptr_{machine_id:?}"), || {
            RefCell::new(DVec2::ZERO)
        });

    Column(Modifier::new().fill_max_width()).child((
        Text("Shift+drag state or Any → wire · drag state → move · middle/space-drag → pan · wheel → zoom · double-click empty → add state")
            .size(theme().typography.label_small)
            .color(theme().on_surface_variant)
            .modifier(Modifier::new().padding_values(PaddingValues {
                left: 12.0,
                right: 8.0,
                top: 4.0,
                bottom: 4.0,
            })),
        Canvas(
            Modifier::new()
                .fill_max_width()
                .height(320.0)
                .background(theme().surface_container_lowest)
                .on_scroll({
                    let view = view.clone();
                    let last_pointer = last_pointer.clone();
                    move |delta: Vec2| {
                        let mut v = view.borrow_mut();
                        if delta.x.abs() > delta.y.abs() && delta.x.abs() > 0.1 {
                            v.pan_by(DVec2::new(delta.x as f64, 0.0));
                        } else if delta.y.abs() > 0.1 {
                            let anchor = *last_pointer.borrow();
                            let anchor = if anchor == DVec2::ZERO {
                                DVec2::new(160.0, 160.0)
                            } else {
                                anchor
                            };
                            let factor = (1.0 + (-delta.y as f64) * 0.002).clamp(0.5, 2.0);
                            v.zoom_at(anchor, factor, 0.5, 2.0);
                        }
                        request_frame();
                        Vec2::ZERO
                    }
                })
                .on_pointer_down({
                    let session = session.clone();
                    let last_click = last_click.clone();
                    let view = view.clone();
                    let last_pointer = last_pointer.clone();
                    move |event: PointerEvent| {
                        let pos = DVec2::new(event.position.x as f64, event.position.y as f64);
                        *last_pointer.borrow_mut() = pos;
                        handle_graph_down(&session, machine_id, &view, &event, &last_click);
                    }
                })
                .on_pointer_move({
                    let session = session.clone();
                    let view = view.clone();
                    let last_pointer = last_pointer.clone();
                    move |event: PointerEvent| {
                        let position =
                            DVec2::new(event.position.x as f64, event.position.y as f64);
                        *last_pointer.borrow_mut() = position;
                        let mut s = session.borrow_mut();
                        let mut view_mut = view.borrow_mut();
                        match s.machine_graph_gesture.clone() {
                            Some(MachineGraphGesture::WireTransition {
                                layer,
                                from_state,
                                ..
                            }) => {
                                s.machine_graph_gesture =
                                    Some(MachineGraphGesture::WireTransition {
                                        layer,
                                        from_state,
                                        current: position,
                                    });
                                request_frame();
                            }
                            Some(MachineGraphGesture::Pan { last }) => {
                                let delta = position - last;
                                view_mut.pan_by(delta);
                                s.machine_graph_gesture =
                                    Some(MachineGraphGesture::Pan { last: position });
                                request_frame();
                            }
                            Some(MachineGraphGesture::DragState {
                                layer,
                                state,
                                offset,
                                ..
                            }) => {
                                s.machine_graph_gesture =
                                    Some(MachineGraphGesture::DragState {
                                        layer,
                                        state,
                                        offset,
                                        current: position,
                                    });
                                request_frame();
                            }
                            None => {}
                        }
                    }
                })
                .on_pointer_up({
                    let session = session.clone();
                    let view = view.clone();
                    move |event: PointerEvent| {
                        handle_graph_up(&session, machine_id, &view, &event);
                    }
                })
                .on_pointer_cancel({
                    let session = session.clone();
                    move |_| {
                        session.borrow_mut().machine_graph_gesture = None;
                        request_frame();
                    }
                }),
            move |scope| {
                let session = draw_session.borrow();
                let machine = &session.file.machines[machine_id];
                let view_val = *view.borrow();
                let mut layout = auto_layout(machine);
                if let Some(MachineGraphGesture::DragState {
                    layer,
                    state,
                    offset,
                    current,
                }) = &session.machine_graph_gesture
                {
                    let world = view_val.screen_to_world(*current) - *offset;
                    if let Some(gs) = layout
                        .iter_mut()
                        .find(|g| g.layer == *layer && g.state == *state)
                    {
                        gs.rect.x = world.x;
                        gs.rect.y = world.y;
                    }
                }
                let active = session.engine.active_machine_states().clone();
                let screen_layout: Vec<GraphState> = layout
                    .iter()
                    .map(|g| GraphState {
                        layer: g.layer,
                        state: g.state,
                        rect: GraphRect {
                            x: g.rect.x * view_val.scale + view_val.offset.x,
                            y: g.rect.y * view_val.scale + view_val.offset.y,
                            width: g.rect.width * view_val.scale,
                            height: g.rect.height * view_val.scale,
                        },
                        entry: g.entry,
                    })
                    .collect();
                let screen_any = |machine: &Machine, layout: &[GraphState], layer: usize| -> DVec2 {
                    let wc = any_node_center(machine, layout, layer);
                    view_val.world_to_screen(wc)
                };
                let screen_state_center =
                    |machine: &Machine, layout: &[GraphState], layer: usize, state: usize| -> DVec2 {
                        let wc = layer_state_center(machine, layout, layer, state);
                        view_val.world_to_screen(wc)
                    };
                draw_machine_edges_with_view(
                    scope,
                    machine,
                    &layout,
                    &screen_layout,
                    &view_val,
                    &session.machine_selection,
                );
                if let Some(MachineGraphGesture::WireTransition {
                    layer,
                    from_state,
                    current,
                }) = &session.machine_graph_gesture
                {
                    let from = match from_state {
                        Some(si) => screen_state_center(machine, &layout, *layer, *si),
                        None => screen_any(machine, &layout, *layer),
                    };
                    draw_edge(scope, from, *current, theme().primary.with_alpha(220));
                }
                draw_any_nodes_with_view(
                    scope,
                    machine,
                    &layout,
                    &screen_layout,
                    &view_val,
                    &session.machine_selection,
                );
                draw_machine_states_with_view(
                    scope,
                    machine,
                    &layout,
                    &screen_layout,
                    &view_val,
                    &session.machine_selection,
                    active.as_deref(),
                );
            },
        ),
    ))
}

fn handle_graph_down(
    session: &SessionRef,
    machine_id: MachineId,
    view: &Rc<RefCell<ViewTransform>>,
    event: &PointerEvent,
    last_click: &std::rc::Rc<std::cell::RefCell<Option<(DVec2, web_time::Instant)>>>,
) {
    let position = DVec2::new(event.position.x as f64, event.position.y as f64);
    let shift = event.modifiers.shift;
    let now = web_time::Instant::now();
    let is_double = {
        let mut lc = last_click.borrow_mut();
        let dbl = lc
            .map(|(p, t)| (now - t).as_millis() < 350 && (p - position).length() < 8.0)
            .unwrap_or(false);
        *lc = Some((position, now));
        dbl
    };

    let view_val = *view.borrow();
    let world = view_val.screen_to_world(position);
    let machine = session.borrow().file.machines[machine_id].clone();
    let layout = auto_layout(&machine);
    let mut s = session.borrow_mut();

    let is_middle = matches!(
        event.event,
        UiPointerEventKind::Down(PointerButton::Tertiary)
    );
    let is_space_pan = matches!(
        event.event,
        UiPointerEventKind::Down(PointerButton::Primary)
    ) && s.viewport.space_held;
    if is_middle || is_space_pan {
        s.machine_graph_gesture = Some(MachineGraphGesture::Pan { last: position });
        request_frame();
        return;
    }

    if is_double {
        if hit_state(&layout, world).is_none() && hit_any(&machine, &layout, world).is_none() {
            let layer = match s.machine_selection {
                MachineSelection::State { layer, .. }
                | MachineSelection::Transition { layer, .. }
                | MachineSelection::Layer { layer } => layer,
                _ => s.active_machine_layer,
            };
            let name = format!(
                "State {}",
                machine
                    .layers
                    .get(layer)
                    .map(|l| l.states.len())
                    .unwrap_or(0)
                    + 1
            );
            let world_pos = world;
            let ok = s.edit_active_machine("Add state", move |m| {
                let idx = add_state(m, layer, name, StateKind::Empty)?;
                let _ = set_state_position(m, layer, idx, Some((world_pos.x, world_pos.y)));
                Ok(())
            });
            if ok {
                if let Some(m) = s.file.machines.get(machine_id) {
                    if let Some(l) = m.layers.get(layer) {
                        s.machine_selection = MachineSelection::State {
                            layer,
                            state: l.states.len().saturating_sub(1),
                        };
                        s.active_machine_layer = layer;
                    }
                }
            }
        }
        request_frame();
        return;
    }

    let hit_any_layer = hit_any(&machine, &layout, world);
    let hit_state_pair = hit_state(&layout, world);
    let hit_trans = {
        let tol_world = 8.0 / view_val.scale.max(0.1);
        hit_transition(&machine, &layout, world, tol_world)
    };

    if let Some(layer) = hit_any_layer {
        s.machine_selection = MachineSelection::Layer { layer };
        s.active_machine_layer = layer;
        if shift {
            s.machine_graph_gesture = Some(MachineGraphGesture::WireTransition {
                layer,
                from_state: None,
                current: position,
            });
        } else {
            s.machine_graph_gesture = None;
        }
    } else if let Some((layer, state)) = hit_state_pair {
        s.machine_selection = MachineSelection::State { layer, state };
        s.active_machine_layer = layer;
        if shift {
            s.machine_graph_gesture = Some(MachineGraphGesture::WireTransition {
                layer,
                from_state: Some(state),
                current: position,
            });
        } else if matches!(
            event.event,
            UiPointerEventKind::Down(PointerButton::Primary)
        ) {
            let rect = layout
                .iter()
                .find(|g| g.layer == layer && g.state == state)
                .map(|g| g.rect)
                .unwrap_or(GraphRect {
                    x: 0.0,
                    y: 0.0,
                    width: 0.0,
                    height: 0.0,
                });
            let offset = world - DVec2::new(rect.x, rect.y);
            s.machine_graph_gesture = Some(MachineGraphGesture::DragState {
                layer,
                state,
                offset,
                current: position,
            });
        } else {
            s.machine_graph_gesture = None;
        }
    } else if let Some((layer, source, transition)) = hit_trans {
        s.machine_selection = MachineSelection::Transition {
            layer,
            source,
            transition,
        };
        s.active_machine_layer = layer;
        s.machine_graph_gesture = None;
    } else {
        s.machine_selection = MachineSelection::None;
        s.machine_graph_gesture = None;
    }
    request_frame();
}

fn handle_graph_up(
    session: &SessionRef,
    machine_id: MachineId,
    view: &Rc<RefCell<ViewTransform>>,
    event: &PointerEvent,
) {
    let position = DVec2::new(event.position.x as f64, event.position.y as f64);
    let view_val = *view.borrow();
    let world = view_val.screen_to_world(position);
    let mut s = session.borrow_mut();
    let gesture = s.machine_graph_gesture.take();
    match gesture {
        Some(MachineGraphGesture::WireTransition {
            layer, from_state, ..
        }) => {
            let machine = s.file.machines[machine_id].clone();
            let layout = auto_layout(&machine);
            if let Some((to_layer, to_state)) = hit_state(&layout, world) {
                if to_layer == layer {
                    let source = match from_state {
                        Some(from) if from != to_state => TransitionSource::State(from),
                        None => TransitionSource::Any,
                        _ => {
                            request_frame();
                            return;
                        }
                    };
                    let ok = s.edit_active_machine("Add transition", move |m| {
                        add_transition(m, layer, source, to_state)?;
                        Ok(())
                    });
                    if ok {
                        if let Some(m) = s.file.machines.get(machine_id) {
                            let count = match source {
                                TransitionSource::Any => m.layers[layer].any_transitions.len(),
                                TransitionSource::State(si) => {
                                    m.layers[layer].states[si].transitions.len()
                                }
                            };
                            s.machine_selection = MachineSelection::Transition {
                                layer,
                                source,
                                transition: count.saturating_sub(1),
                            };
                            s.active_machine_layer = layer;
                        }
                    }
                }
            }
        }
        Some(MachineGraphGesture::DragState {
            layer,
            state,
            offset,
            current,
        }) => {
            let end_screen = current;
            let end_world = view_val.screen_to_world(end_screen) - offset;
            let _ = s.edit_active_machine("Move state", move |m| {
                set_state_position(m, layer, state, Some((end_world.x, end_world.y)))?;
                Ok(())
            });
            s.machine_selection = MachineSelection::State { layer, state };
            s.active_machine_layer = layer;
        }
        Some(MachineGraphGesture::Pan { .. }) => {
            // Pan already applied incrementally; nothing to commit.
        }
        None => {}
    }
    request_frame();
}

fn any_node_center(machine: &Machine, layout: &[GraphState], layer: usize) -> DVec2 {
    let first = layer_state_center(machine, layout, layer, 0);
    DVec2::new(first.x - 48.0, first.y)
}

fn hit_any(machine: &Machine, layout: &[GraphState], position: DVec2) -> Option<usize> {
    for (li, layer) in machine.layers.iter().enumerate() {
        if layer.states.is_empty() {
            continue;
        }
        let c = any_node_center(machine, layout, li);
        let rect = GraphRect {
            x: c.x - 18.0,
            y: c.y - 12.0,
            width: 36.0,
            height: 24.0,
        };
        if rect.contains(position) {
            return Some(li);
        }
    }
    None
}

fn layer_state_center(
    _machine: &Machine,
    layout: &[GraphState],
    layer: usize,
    state: usize,
) -> DVec2 {
    layout
        .iter()
        .find(|g| g.layer == layer && g.state == state)
        .map(|g| g.rect.center())
        .unwrap_or(DVec2::ZERO)
}

#[allow(dead_code)]
fn draw_any_nodes(
    scope: &mut DrawScope,
    machine: &Machine,
    layout: &[GraphState],
    selection: &MachineSelection,
) {
    let th = theme();
    for (li, layer) in machine.layers.iter().enumerate() {
        if layer.states.is_empty() {
            continue;
        }
        let from = any_node_center(machine, layout, li);
        let selected = matches!(selection, MachineSelection::Layer { layer } if *layer == li);
        scope.draw_rect(
            Rect {
                x: (from.x - 18.0) as f32,
                y: (from.y - 12.0) as f32,
                w: 36.0,
                h: 24.0,
            },
            if selected {
                th.primary_container
            } else {
                th.tertiary_container
            },
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
        // layer label
        scope.draw_text(
            &layer.name,
            Vec2 {
                x: (from.x - 18.0) as f32,
                y: (from.y - 28.0) as f32,
            },
            th.on_surface_variant,
            9.0,
        );
    }
}

#[allow(dead_code)]
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
                let c = if is_sel { selected_color } else { normal };
                draw_edge(scope, from, to, c);
                draw_arrow_head(scope, from, to, c);
            }
        }
        if !layer.states.is_empty() {
            let from = any_node_center(machine, layout, li);
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
                let c = if is_sel { selected_color } else { normal };
                draw_edge(scope, from, to, c);
                draw_arrow_head(scope, from, to, c);
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
    let tip = to - n * 28.0;
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

#[allow(dead_code)]
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
            &kind_label(&state.kind),
            Vec2 {
                x: rect.x + 8.0,
                y: rect.y + rect.h - 15.0,
            },
            th.on_surface_variant,
            9.0,
        );
    }
}

fn draw_any_nodes_with_view(
    scope: &mut DrawScope,
    machine: &Machine,
    layout: &[GraphState],
    _screen_layout: &[GraphState],
    view: &ViewTransform,
    selection: &MachineSelection,
) {
    let th = theme();
    for (li, layer) in machine.layers.iter().enumerate() {
        if layer.states.is_empty() {
            continue;
        }
        let world_c = any_node_center(machine, layout, li);
        let c = view.world_to_screen(world_c);
        let selected = matches!(selection, MachineSelection::Layer { layer } if *layer == li);
        let s = view.scale as f32;
        scope.draw_rect(
            Rect {
                x: (c.x - 18.0 * view.scale) as f32,
                y: (c.y - 12.0 * view.scale) as f32,
                w: 36.0 * s,
                h: 24.0 * s,
            },
            if selected {
                th.primary_container
            } else {
                th.tertiary_container
            },
            6.0 * s,
        );
        scope.draw_text(
            "Any",
            Vec2 {
                x: (c.x - 12.0 * view.scale) as f32,
                y: (c.y - 6.0 * view.scale) as f32,
            },
            th.on_tertiary_container,
            (10.0 * view.scale as f32).clamp(8.0, 14.0),
        );
        scope.draw_text(
            &layer.name,
            Vec2 {
                x: (c.x - 18.0 * view.scale) as f32,
                y: (c.y - 28.0 * view.scale) as f32,
            },
            th.on_surface_variant,
            (9.0 * view.scale as f32).clamp(7.0, 12.0),
        );
    }
}

fn draw_machine_edges_with_view(
    scope: &mut DrawScope,
    machine: &Machine,
    layout: &[GraphState],
    _screen_layout: &[GraphState],
    view: &ViewTransform,
    selection: &MachineSelection,
) {
    let th = theme();
    let normal = th.outline.with_alpha(160);
    let selected_color = th.primary;
    for (li, layer) in machine.layers.iter().enumerate() {
        for (si, state) in layer.states.iter().enumerate() {
            let from = view.world_to_screen(layer_state_center(machine, layout, li, si));
            for (ti, tr) in state.transitions.iter().enumerate() {
                let to = view.world_to_screen(layer_state_center(machine, layout, li, tr.to));
                let is_sel = matches!(
                    selection,
                    MachineSelection::Transition {
                        layer,
                        source: TransitionSource::State(src),
                        transition,
                    } if *layer == li && *src == si && *transition == ti
                );
                let c = if is_sel { selected_color } else { normal };
                draw_edge(scope, from, to, c);
                draw_arrow_head(scope, from, to, c);
            }
        }
        if !layer.states.is_empty() {
            let from = view.world_to_screen(any_node_center(machine, layout, li));
            for (ti, tr) in layer.any_transitions.iter().enumerate() {
                let to = view.world_to_screen(layer_state_center(machine, layout, li, tr.to));
                let is_sel = matches!(
                    selection,
                    MachineSelection::Transition {
                        layer,
                        source: TransitionSource::Any,
                        transition,
                    } if *layer == li && *transition == ti
                );
                let c = if is_sel { selected_color } else { normal };
                draw_edge(scope, from, to, c);
                draw_arrow_head(scope, from, to, c);
            }
        }
    }
}

fn draw_machine_states_with_view(
    scope: &mut DrawScope,
    machine: &Machine,
    _layout: &[GraphState],
    screen_layout: &[GraphState],
    _view: &ViewTransform,
    selection: &MachineSelection,
    active: Option<&[usize]>,
) {
    let th = theme();
    for gs in screen_layout {
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
            &kind_label(&state.kind),
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
        StateKind::Blend1D { children, .. } => format!("blend · {}", children.len()),
    }
}

fn SelectionInspector(overlay: OverlayHandle, session: SessionRef, machine_id: MachineId) -> View {
    let selection = session.borrow().machine_selection.clone();
    match selection {
        MachineSelection::State { layer, state } => {
            StateInspector(overlay, session, machine_id, layer, state)
        }
        MachineSelection::Transition {
            layer,
            source,
            transition,
        } => TransitionInspector(overlay, session, machine_id, layer, source, transition),
        MachineSelection::Layer { layer } => LayerInspector(overlay, session, machine_id, layer),
        _ => Text("Select a state, edge, or Any node")
            .size(theme().typography.label_small)
            .color(theme().on_surface_variant)
            .modifier(Modifier::new().padding(12.0)),
    }
}

fn LayerInspector(
    overlay: OverlayHandle,
    session: SessionRef,
    machine_id: MachineId,
    layer: usize,
) -> View {
    let th = theme();
    let (name, any_count, state_count) = {
        let s = session.borrow();
        let Some(l) = s.file.machines[machine_id].layers.get(layer) else {
            return Box(Modifier::new());
        };
        (l.name.clone(), l.any_transitions.len(), l.states.len())
    };
    Column(Modifier::new().fill_max_width().gap(4.0)).child((
        Row(Modifier::new()
            .fill_max_width()
            .padding_values(PaddingValues {
                left: 12.0,
                right: 8.0,
                top: 8.0,
                bottom: 2.0,
            })
            .gap(6.0)
            .align_items(AlignItems::CENTER))
        .child((
            Box(Modifier::new().flex_grow(1.0)).child(crate::components::name_field(
                format!("machine_layer_name_{layer}"),
                name,
                "Layer name",
                36.0,
                {
                    let session = session.clone();
                    move |text: String| {
                        let mut s = session.borrow_mut();
                        s.edit_active_machine("Rename layer", move |machine| {
                            rename_layer(machine, layer, text)?;
                            Ok(())
                        });
                    }
                },
            )),
            chip("Add state", false, {
                let session = session.clone();
                move || add_state_ui(&session, machine_id, layer)
            }),
            CompactIconAction(Symbols::delete, "Delete layer", {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    let ok = s.edit_active_machine("Delete layer", move |machine| {
                        remove_layer(machine, layer)?;
                        Ok(())
                    });
                    if ok {
                        s.machine_selection = MachineSelection::None;
                        s.active_machine_layer = 0;
                    }
                }
            }),
        )),
        Text(format!(
            "{state_count} states · {any_count} any-transitions"
        ))
        .size(th.typography.body_small)
        .color(th.on_surface_variant)
        .modifier(Modifier::new().padding_values(PaddingValues {
            left: 12.0,
            right: 12.0,
            top: 0.0,
            bottom: 4.0,
        })),
        Text("Shift+drag from Any to wire a global transition")
            .size(th.typography.label_small)
            .color(th.on_surface_variant)
            .modifier(Modifier::new().padding_values(PaddingValues {
                left: 12.0,
                right: 12.0,
                top: 0.0,
                bottom: 8.0,
            })),
        Row(Modifier::new()
            .padding_values(PaddingValues {
                left: 12.0,
                right: 8.0,
                top: 0.0,
                bottom: 8.0,
            })
            .gap(6.0))
        .child((
            Text("Add any →")
                .size(th.typography.body_medium)
                .color(th.on_surface_variant),
            Box(Modifier::new()).child(transition_target_dropdown(
                overlay,
                session,
                machine_id,
                layer,
                0,
                TransitionSource::Any,
                state_count,
                true,
            )),
        )),
    ))
}

fn StateInspector(
    overlay: OverlayHandle,
    session: SessionRef,
    machine_id: MachineId,
    layer: usize,
    state: usize,
) -> View {
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
                top: 8.0,
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
        .child(crate::components::name_field(
            format!("machine_state_name_{layer}_{state}"),
            name.clone(),
            "State name",
            36.0,
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
                        s.status = Some("Create a clip in Assets first".into());
                        s.repaint();
                        return;
                    };
                    s.edit_active_machine("Change state", move |machine| {
                        pure_set_state_kind(
                            machine,
                            layer,
                            state,
                            StateKind::Clip {
                                clip,
                                speed: 1.0,
                                loop_mode: LoopMode::Once,
                            },
                        )?;
                        Ok(())
                    });
                }
            }),
            chip("Blend1D", matches!(kind, StateKind::Blend1D { .. }), {
                let session = session.clone();
                let inputs = inputs.clone();
                move || {
                    let Some(input) = number_input_index(&inputs) else {
                        let mut s = session.borrow_mut();
                        s.status = Some("Add a Number input before Blend1D".into());
                        s.repaint();
                        return;
                    };
                    let mut s = session.borrow_mut();
                    s.edit_active_machine("Change state", move |machine| {
                        pure_set_state_kind(
                            machine,
                            layer,
                            state,
                            StateKind::Blend1D {
                                input,
                                children: Vec::new(),
                            },
                        )?;
                        Ok(())
                    });
                }
            }),
        )),
    ];

    match &kind {
        StateKind::Clip {
            clip,
            speed,
            loop_mode,
        } => {
            rows.push(labeled_row(
                "Clip",
                clip_dropdown(
                    overlay.clone(),
                    session.clone(),
                    machine_id,
                    layer,
                    state,
                    *clip,
                ),
            ));
            let speed = *speed;
            rows.push(labeled_row(
                "Speed",
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
            ));
            let loop_mode = *loop_mode;
            rows.push(
                Row(Modifier::new()
                    .fill_max_width()
                    .padding_values(pad_row())
                    .gap(6.0)
                    .align_items(AlignItems::CENTER))
                .child((
                    Text("Loop")
                        .size(th.typography.body_medium)
                        .modifier(Modifier::new().width(48.0)),
                    chip("Once", loop_mode == LoopMode::Once, {
                        let session = session.clone();
                        move || set_clip_loop(session.clone(), layer, state, LoopMode::Once)
                    }),
                    chip("Loop", loop_mode == LoopMode::Loop, {
                        let session = session.clone();
                        move || set_clip_loop(session.clone(), layer, state, LoopMode::Loop)
                    }),
                    chip("PingPong", loop_mode == LoopMode::PingPong, {
                        let session = session.clone();
                        move || set_clip_loop(session.clone(), layer, state, LoopMode::PingPong)
                    }),
                )),
            );
        }
        StateKind::Blend1D { input, children } => {
            rows.push(labeled_row(
                "Input",
                blend_input_dropdown(
                    overlay.clone(),
                    session.clone(),
                    machine_id,
                    layer,
                    state,
                    *input,
                ),
            ));
            rows.push(
                Text("Blend children")
                    .size(th.typography.label_medium)
                    .color(th.on_surface_variant)
                    .modifier(Modifier::new().padding_values(PaddingValues {
                        left: 12.0,
                        right: 12.0,
                        top: 8.0,
                        bottom: 2.0,
                    })),
            );
            for (ci, child) in children.iter().enumerate() {
                rows.push(blend_child_row(
                    overlay.clone(),
                    session.clone(),
                    machine_id,
                    layer,
                    state,
                    ci,
                    child.clone(),
                ));
            }
            rows.push(
                Row(Modifier::new()
                    .padding_values(pad_row())
                    .gap(6.0)
                    .align_items(AlignItems::CENTER))
                .child(Button(
                    Modifier::new(),
                    {
                        let session = session.clone();
                        move || {
                            let mut s = session.borrow_mut();
                            let Some(clip) = s.file.clip_order.first().copied() else {
                                s.status = Some("Create a clip in Assets first".into());
                                s.repaint();
                                return;
                            };
                            s.edit_active_machine("Add blend child", move |machine| {
                                let st = &mut machine.layers[layer].states[state];
                                if let StateKind::Blend1D { children, .. } = &mut st.kind {
                                    let thr =
                                        children.last().map(|c| c.threshold + 0.5).unwrap_or(0.0);
                                    children.push(BlendChild {
                                        threshold: thr,
                                        clip,
                                    });
                                    renamite_behavior_common::machine::sort_blend_children(
                                        children,
                                    );
                                }
                                Ok(())
                            });
                        }
                    },
                    ButtonConfig::default(),
                    || Text("Add child").size(th.typography.label_medium),
                )),
            );
        }
        StateKind::Empty => {}
    }

    rows.push(
        Row(Modifier::new()
            .fill_max_width()
            .padding_values(pad_row())
            .gap(6.0)
            .align_items(AlignItems::CENTER))
        .child((
            if is_entry {
                // Already the entry state: static label, not an action.
                Row(Modifier::new().gap(4.0).align_items(AlignItems::CENTER)).child((
                    AppIcon(Symbols::play_arrow, 16.0),
                    Text("Entry")
                        .size(th.typography.body_medium)
                        .color(th.primary),
                ))
            } else {
                chip("Set as entry", false, {
                    let session = session.clone();
                    move || {
                        let mut s = session.borrow_mut();
                        s.edit_active_machine("Set entry", move |machine| {
                            set_entry_state(machine, layer, state)?;
                            Ok(())
                        });
                    }
                })
            },
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

    // Outgoing transitions from THIS state only (not Any).
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

    let transitions = {
        let s = session.borrow();
        s.file.machines[machine_id].layers[layer].states[state]
            .transitions
            .clone()
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

    rows.push(
        Row(Modifier::new()
            .fill_max_width()
            .padding_values(pad_row())
            .gap(6.0)
            .align_items(AlignItems::CENTER))
        .child((
            Text("Add transition")
                .size(th.typography.body_medium)
                .color(th.on_surface_variant)
                .modifier(Modifier::new().flex_grow(1.0)),
            Box(Modifier::new().flex_grow(0.0)).child(transition_target_dropdown(
                overlay,
                session,
                machine_id,
                layer,
                state,
                TransitionSource::State(state),
                states,
                true,
            )),
        )),
    );

    Column(Modifier::new().fill_max_width()).child(rows)
}

fn set_clip_loop(session: SessionRef, layer: usize, state: usize, mode: LoopMode) {
    set_state_kind(session, layer, state, |k| match k {
        StateKind::Clip { clip, speed, .. } => StateKind::Clip {
            clip,
            speed,
            loop_mode: mode,
        },
        other => other,
    });
}

fn blend_child_row(
    overlay: OverlayHandle,
    session: SessionRef,
    machine_id: MachineId,
    layer: usize,
    state: usize,
    index: usize,
    child: BlendChild,
) -> View {
    let th = theme();
    Row(Modifier::new()
        .fill_max_width()
        .padding_values(pad_row())
        .gap(6.0)
        .align_items(AlignItems::CENTER))
    .child((
        Text("thr")
            .size(th.typography.label_small)
            .color(th.on_surface_variant),
        machine_scrub_number(
            session.clone(),
            child.threshold,
            0.01,
            Rc::new(move |machine, value| {
                if let StateKind::Blend1D { children, .. } =
                    &mut machine.layers[layer].states[state].kind
                {
                    if let Some(c) = children.get_mut(index) {
                        c.threshold = value;
                    }
                    renamite_behavior_common::machine::sort_blend_children(children);
                }
            }),
        ),
        Box(Modifier::new().flex_grow(1.0)).child(clip_dropdown_for_blend(
            overlay,
            session.clone(),
            machine_id,
            layer,
            state,
            index,
            child.clip,
        )),
        CompactIconAction(Symbols::delete, "Remove child", {
            let session = session.clone();
            move || {
                let mut s = session.borrow_mut();
                s.edit_active_machine("Remove blend child", move |machine| {
                    if let StateKind::Blend1D { children, .. } =
                        &mut machine.layers[layer].states[state].kind
                    {
                        if index < children.len() {
                            children.remove(index);
                        }
                    }
                    Ok(())
                });
            }
        }),
    ))
}

fn clip_dropdown_for_blend(
    overlay: OverlayHandle,
    session: SessionRef,
    machine_id: MachineId,
    layer: usize,
    state: usize,
    child_index: usize,
    current: ClipId,
) -> View {
    let names = clip_names(&session);
    let current_name = names
        .iter()
        .find(|(id, _)| *id == current)
        .map(|(_, n)| n.clone())
        .unwrap_or_else(|| "—".into());
    let items = names
        .into_iter()
        .map(|(id, name)| {
            DropdownMenuEntry::Item(DropdownMenuItem::new(name, {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    s.edit_active_machine("Blend child clip", move |machine| {
                        if let StateKind::Blend1D { children, .. } =
                            &mut machine.layers[layer].states[state].kind
                        {
                            if let Some(c) = children.get_mut(child_index) {
                                c.clip = id;
                            }
                        }
                        Ok(())
                    });
                }
            }))
        })
        .collect();
    dropdown(
        overlay,
        format!("bclip_{machine_id:?}_{layer}_{state}_{child_index}"),
        format!("{current_name} ▾"),
        items,
    )
}

fn TransitionInspector(
    overlay: OverlayHandle,
    session: SessionRef,
    machine_id: MachineId,
    layer: usize,
    source: TransitionSource,
    transition: usize,
) -> View {
    let th = theme();
    let (state_count, duration, exit_time, conditions, inputs, target) = {
        let s = session.borrow();
        let machine = &s.file.machines[machine_id];
        let Some(l) = machine.layers.get(layer) else {
            return Box(Modifier::new().height(4.0));
        };
        let transitions = match source {
            TransitionSource::Any => &l.any_transitions,
            TransitionSource::State(si) => {
                if si >= l.states.len() {
                    return Box(Modifier::new().height(4.0));
                }
                &l.states[si].transitions
            }
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
            tr.to,
        )
    };

    let mut rows: Vec<View> = vec![
        Text(match source {
            TransitionSource::Any => "Transition · Any",
            TransitionSource::State(_) => "Transition",
        })
        .size(th.typography.label_medium)
        .color(th.on_surface_variant)
        .modifier(Modifier::new().padding_values(PaddingValues {
            left: 12.0,
            right: 12.0,
            top: 8.0,
            bottom: 2.0,
        })),
        labeled_row(
            "Target",
            retarget_dropdown(
                overlay.clone(),
                session.clone(),
                machine_id,
                layer,
                source,
                transition,
                target,
                state_count,
            ),
        ),
        labeled_row(
            "Duration",
            machine_scrub_number(
                session.clone(),
                duration,
                1.0,
                Rc::new(move |machine, value| {
                    if let Ok(tr) = transition_mut(machine, layer, source, transition) {
                        tr.duration = value.max(0.0);
                    }
                }),
            ),
        ),
        Row(Modifier::new()
            .fill_max_width()
            .padding_values(pad_row())
            .gap(6.0)
            .align_items(AlignItems::CENTER))
        .child((
            Text("Exit")
                .size(th.typography.body_medium)
                .modifier(Modifier::new().width(48.0)),
            chip("When finished", exit_time.is_some(), {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    s.edit_active_machine("Transition exit", move |machine| {
                        if let Ok(tr) = transition_mut(machine, layer, source, transition) {
                            tr.exit_time = if tr.exit_time.is_some() {
                                None
                            } else {
                                Some(1.0)
                            };
                        }
                        Ok(())
                    });
                }
            }),
        )),
    ];

    if let Some(exit) = exit_time {
        rows.push(labeled_row(
            "Value",
            machine_scrub_number(
                session.clone(),
                exit,
                0.01,
                Rc::new(move |machine, value| {
                    if let Ok(tr) = transition_mut(machine, layer, source, transition) {
                        tr.exit_time = Some(value.clamp(0.0, 1.0));
                    }
                }),
            ),
        ));
    }

    rows.push(
        Text("Conditions (AND)")
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
        rows.push(condition_editor_row(
            session.clone(),
            layer,
            source,
            transition,
            index,
            condition.clone(),
            &inputs,
        ));
    }

    rows.push(
        Row(Modifier::new()
            .fill_max_width()
            .padding_values(pad_row())
            .gap(6.0)
            .align_items(AlignItems::CENTER))
        .child((
            Text("Add condition")
                .size(th.typography.body_medium)
                .color(th.on_surface_variant)
                .modifier(Modifier::new().flex_grow(1.0)),
            Box(Modifier::new()).child(condition_dropdown(
                overlay,
                session.clone(),
                machine_id,
                layer,
                source,
                transition,
                inputs,
            )),
        )),
    );

    rows.push(
        Row(Modifier::new().padding_values(pad_row())).child(CompactIconAction(
            Symbols::delete,
            "Delete transition",
            {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    let ok = s.edit_active_machine("Remove transition", move |machine| {
                        remove_transition(machine, layer, source, transition)?;
                        Ok(())
                    });
                    if ok {
                        s.machine_selection = MachineSelection::None;
                    }
                }
            },
        )),
    );

    Column(Modifier::new().fill_max_width()).child(rows)
}

fn condition_editor_row(
    session: SessionRef,
    layer: usize,
    source: TransitionSource,
    transition: usize,
    index: usize,
    condition: Condition,
    inputs: &[InputDef],
) -> View {
    let th = theme();
    let mut parts: Vec<View> = vec![
        Text(condition_label(inputs, &condition))
            .size(th.typography.body_medium)
            .color(th.on_surface)
            .modifier(Modifier::new().flex_grow(1.0)),
    ];

    match condition {
        Condition::BoolIs { value, .. } => {
            parts.push(chip(if value { "true" } else { "false" }, true, {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    s.edit_active_machine("Edit condition", move |machine| {
                        if let Ok(tr) = transition_mut(machine, layer, source, transition) {
                            if let Some(Condition::BoolIs { value: v, .. }) =
                                tr.conditions.get_mut(index)
                            {
                                *v = !value;
                            }
                        }
                        Ok(())
                    });
                }
            }));
        }
        Condition::NumberCmp { op, value, .. } => {
            parts.push(chip(cmp_label(op), false, {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    s.edit_active_machine("Edit condition", move |machine| {
                        if let Ok(tr) = transition_mut(machine, layer, source, transition) {
                            if let Some(Condition::NumberCmp { op: o, .. }) =
                                tr.conditions.get_mut(index)
                            {
                                *o = next_cmp(*o);
                            }
                        }
                        Ok(())
                    });
                }
            }));
            parts.push(machine_scrub_number(
                session.clone(),
                value,
                0.01,
                Rc::new(move |machine, v| {
                    if let Ok(tr) = transition_mut(machine, layer, source, transition) {
                        if let Some(Condition::NumberCmp { value, .. }) =
                            tr.conditions.get_mut(index)
                        {
                            *value = v;
                        }
                    }
                }),
            ));
        }
        Condition::Triggered { .. } => {}
    }

    parts.push(CompactIconAction(Symbols::delete, "Remove condition", {
        let session = session.clone();
        move || {
            let mut s = session.borrow_mut();
            s.edit_active_machine("Remove condition", move |machine| {
                remove_condition(machine, layer, source, transition, index)?;
                Ok(())
            });
        }
    }));

    Row(Modifier::new()
        .fill_max_width()
        .padding_values(pad_row())
        .gap(6.0)
        .align_items(AlignItems::CENTER))
    .child(parts)
}

fn cmp_label(op: CmpOp) -> &'static str {
    match op {
        CmpOp::Eq => "==",
        CmpOp::Ne => "!=",
        CmpOp::Lt => "<",
        CmpOp::Le => "≤",
        CmpOp::Gt => ">",
        CmpOp::Ge => "≥",
    }
}

fn next_cmp(op: CmpOp) -> CmpOp {
    match op {
        CmpOp::Ge => CmpOp::Gt,
        CmpOp::Gt => CmpOp::Le,
        CmpOp::Le => CmpOp::Lt,
        CmpOp::Lt => CmpOp::Eq,
        CmpOp::Eq => CmpOp::Ne,
        CmpOp::Ne => CmpOp::Ge,
    }
}

fn ListenersSection(overlay: OverlayHandle, session: SessionRef, machine_id: MachineId) -> View {
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
                            remove_listener(machine, index)?;
                            Ok(())
                        });
                    }
                }),
            )),
        );
    }

    // Add-listener row: node comes from the editor selection.
    if let Some(node) = selected_node {
        rows.push(AddListenerRow(
            overlay,
            session.clone(),
            machine_id,
            node,
            inputs,
        ));
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
    overlay: OverlayHandle,
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

    let number_editor: View = if matches!(input_kind, Some(InputKind::Number { .. })) {
        listener_draft_number_scrub(session.clone(), draft.number_value, 0.01)
    } else {
        Box(Modifier::new().width(0.0))
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
        dropdown(
            overlay.clone(),
            "lst_event",
            format!("{event_label} ▾"),
            event_items,
        ),
        dropdown(
            overlay.clone(),
            "lst_input",
            format!("{input_label} ▾"),
            input_items,
        ),
        dropdown(
            overlay.clone(),
            "lst_action",
            format!("{action_label} ▾"),
            action_items,
        ),
        number_editor,
        Button(
            Modifier::new(),
            {
                let session = session.clone();
                let inputs = inputs.clone();
                move || {
                    let mut s = session.borrow_mut();
                    let draft = s.listener_draft.clone();
                    let Some(event) = draft.event else {
                        return;
                    };
                    let Some(input) = draft.input else {
                        return;
                    };
                    let kind = inputs.get(input).map(|i| i.kind);
                    let action = match kind {
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
                    let ok = s.edit_active_machine("Add listener", move |machine| {
                        add_listener(machine, listener)?;
                        Ok(())
                    });
                    if ok {
                        s.listener_draft = crate::session::ListenerDraft::default();
                    }
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

fn set_kind_action(
    session: SessionRef,
    layer: usize,
    state: usize,
    kind: StateKind,
) -> impl Fn() + 'static {
    move || {
        let mut s = session.borrow_mut();
        s.edit_active_machine("Change state", |machine| {
            pure_set_state_kind(machine, layer, state, kind.clone())?;
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
        let new_kind = map(kind);
        pure_set_state_kind(machine, layer, state, new_kind)?;
        Ok(())
    });
}

fn number_input_index(inputs: &[InputDef]) -> Option<usize> {
    inputs
        .iter()
        .position(|input| matches!(input.kind, InputKind::Number { .. }))
}

fn clip_names(session: &SessionRef) -> Vec<(ClipId, String)> {
    let s = session.borrow();
    s.file
        .clip_order
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
}

fn clip_dropdown(
    overlay: OverlayHandle,
    session: SessionRef,
    machine_id: MachineId,
    layer: usize,
    state: usize,
    current: ClipId,
) -> View {
    let names = clip_names(&session);
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
                        let (speed, loop_mode) = match &st.kind {
                            StateKind::Clip {
                                speed, loop_mode, ..
                            } => (*speed, *loop_mode),
                            _ => (1.0, LoopMode::Once),
                        };
                        st.kind = StateKind::Clip {
                            clip: id,
                            speed,
                            loop_mode,
                        };
                        Ok(())
                    });
                }
            }))
        })
        .collect();
    dropdown(
        overlay,
        format!("clip_{machine_id:?}_{layer}_{state}"),
        format!("{current_name} ▾"),
        items,
    )
}

fn blend_input_dropdown(
    overlay: OverlayHandle,
    session: SessionRef,
    machine_id: MachineId,
    layer: usize,
    state: usize,
    current: usize,
) -> View {
    let inputs = session.borrow().file.machines[machine_id].inputs.clone();
    let current_name = inputs
        .get(current)
        .map(|i| i.name.clone())
        .unwrap_or_else(|| "—".into());
    let items = inputs
        .iter()
        .enumerate()
        .filter(|(_, i)| matches!(i.kind, InputKind::Number { .. }))
        .map(|(idx, i)| {
            DropdownMenuEntry::Item(DropdownMenuItem::new(i.name.clone(), {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    s.edit_active_machine("Change state", move |machine| {
                        let st = &mut machine.layers[layer].states[state];
                        let children = match &st.kind {
                            StateKind::Blend1D { children, .. } => children.clone(),
                            _ => Vec::new(),
                        };
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
        overlay,
        format!("blend_{machine_id:?}_{layer}_{state}"),
        format!("{current_name} ▾"),
        items,
    )
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
    let target_name = session.borrow().file.machines[machine_id].layers[layer]
        .states
        .get(target)
        .map(|st| st.name.clone())
        .unwrap_or_else(|| "—".into());
    let cond_label = if conditions.is_empty() {
        "no conditions".into()
    } else {
        format!(
            "{} condition{}",
            conditions.len(),
            if conditions.len() == 1 { "" } else { "s" }
        )
    };
    let source_label = match source {
        TransitionSource::Any => "Any →",
        TransitionSource::State(_) => "→",
    };

    Row(Modifier::new()
        .fill_max_width()
        .padding_values(pad_row())
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
            .modifier(Modifier::new().flex_grow(1.0)),
        Text(cond_label)
            .size(th.typography.label_small)
            .color(th.on_surface_variant),
        CompactIconAction(Symbols::delete, "Remove transition", {
            let session = session.clone();
            move || {
                let mut s = session.borrow_mut();
                s.edit_active_machine("Remove transition", move |machine| {
                    remove_transition(machine, layer, source, index)?;
                    Ok(())
                });
            }
        }),
    ))
}

/// `add_mode`: true = add new transition to target; false unused (use retarget_dropdown).
fn transition_target_dropdown(
    overlay: OverlayHandle,
    session: SessionRef,
    machine_id: MachineId,
    layer: usize,
    _source_state: usize,
    source: TransitionSource,
    state_count: usize,
    add_mode: bool,
) -> View {
    let items = (0..state_count)
        .filter(|&target| match source {
            TransitionSource::State(from) => from != target,
            TransitionSource::Any => true,
        })
        .map(|target| {
            let target_name = session.borrow().file.machines[machine_id].layers[layer]
                .states
                .get(target)
                .map(|st| st.name.clone())
                .unwrap_or_else(|| target.to_string());
            DropdownMenuEntry::Item(DropdownMenuItem::new(target_name, {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    if add_mode {
                        let ok = s.edit_active_machine("Add transition", move |machine| {
                            add_transition(machine, layer, source, target)?;
                            Ok(())
                        });
                        if ok {
                            if let Some(m) = s.file.machines.get(machine_id) {
                                let n = match source {
                                    TransitionSource::Any => m.layers[layer].any_transitions.len(),
                                    TransitionSource::State(si) => {
                                        m.layers[layer].states[si].transitions.len()
                                    }
                                };
                                s.machine_selection = MachineSelection::Transition {
                                    layer,
                                    source,
                                    transition: n.saturating_sub(1),
                                };
                            }
                        }
                    }
                }
            }))
        })
        .collect();
    dropdown(
        overlay,
        format!("target_add_{machine_id:?}_{layer}_{source:?}"),
        format!("{} ▾", Symbols::add.name),
        items,
    )
}

fn retarget_dropdown(
    overlay: OverlayHandle,
    session: SessionRef,
    machine_id: MachineId,
    layer: usize,
    source: TransitionSource,
    transition: usize,
    current: usize,
    state_count: usize,
) -> View {
    let current_name = session.borrow().file.machines[machine_id].layers[layer]
        .states
        .get(current)
        .map(|st| st.name.clone())
        .unwrap_or_else(|| "—".into());
    let items = (0..state_count)
        .map(|target| {
            let target_name = session.borrow().file.machines[machine_id].layers[layer]
                .states
                .get(target)
                .map(|st| st.name.clone())
                .unwrap_or_else(|| target.to_string());
            DropdownMenuEntry::Item(DropdownMenuItem::new(target_name, {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    s.edit_active_machine("Retarget transition", move |machine| {
                        set_transition_target(machine, layer, source, transition, target)?;
                        Ok(())
                    });
                }
            }))
        })
        .collect();
    dropdown(
        overlay,
        format!("retarget_{machine_id:?}_{layer}_{transition}"),
        format!("{current_name} ▾"),
        items,
    )
}

fn condition_dropdown(
    overlay: OverlayHandle,
    session: SessionRef,
    machine_id: MachineId,
    layer: usize,
    source: TransitionSource,
    transition: usize,
    inputs: Vec<InputDef>,
) -> View {
    if inputs.is_empty() {
        return Text("add inputs first")
            .size(theme().typography.label_small)
            .color(theme().on_surface_variant);
    }
    let items = inputs
        .iter()
        .enumerate()
        .map(|(idx, input)| {
            DropdownMenuEntry::Item(DropdownMenuItem::new(input.name.clone(), {
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
        overlay,
        format!("cond_{machine_id:?}_{layer}_{transition:?}"),
        format!("{} ▾", Symbols::add.name),
        items,
    )
}

fn condition_label(inputs: &[InputDef], condition: &Condition) -> String {
    match condition {
        Condition::BoolIs { input, value } => format!("{} == {value}", input_name(inputs, *input)),
        Condition::NumberCmp { input, op, value } => {
            format!(
                "{} {} {value:.2}",
                input_name(inputs, *input),
                cmp_label(*op)
            )
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

fn pad_row() -> PaddingValues {
    PaddingValues {
        left: 12.0,
        right: 8.0,
        top: 2.0,
        bottom: 2.0,
    }
}

fn labeled_row(label: &'static str, child: View) -> View {
    let th = theme();
    Row(Modifier::new()
        .fill_max_width()
        .padding_values(pad_row())
        .gap(6.0)
        .align_items(AlignItems::CENTER))
    .child((
        Text(label)
            .size(th.typography.body_medium)
            .modifier(Modifier::new().width(56.0)),
        Box(Modifier::new().flex_grow(1.0)).child(child),
    ))
}

type MachineScrub = Rc<dyn Fn(&mut Machine, f64)>;

fn machine_scrub_number(session: SessionRef, value: f64, step: f64, edit: MachineScrub) -> View {
    let th = theme();
    let label = format!("{value:.2}");
    Text(label)
        .size(th.typography.body_medium)
        .color(th.primary)
        .modifier(
            Modifier::new()
                .min_width(52.0)
                .cursor(repose_core::CursorIcon::EwResize)
                .on_pointer_down({
                    let session = session.clone();
                    move |pe: PointerEvent| {
                        session
                            .borrow_mut()
                            .begin_machine_field_scrub(value, pe.position.x);
                    }
                })
                .on_pointer_move({
                    let session = session.clone();
                    let edit = edit.clone();
                    move |pe: PointerEvent| {
                        let edit = edit.clone();
                        session.borrow_mut().scrub_machine_field(
                            pe.position.x,
                            step,
                            pe.modifiers.shift,
                            move |machine, v| edit(machine, v),
                        );
                    }
                })
                .on_pointer_up({
                    let session = session.clone();
                    move |_| {
                        session.borrow_mut().end_machine_field_scrub();
                    }
                }),
        )
}

fn preview_scrub_number(session: SessionRef, input: usize, value: f64, step: f64) -> View {
    let th = theme();
    Text(format!("{value:.2}"))
        .size(th.typography.body_medium)
        .color(th.primary)
        .modifier(
            Modifier::new()
                .min_width(52.0)
                .cursor(repose_core::CursorIcon::EwResize)
                .on_pointer_down({
                    let session = session.clone();
                    move |pe: PointerEvent| {
                        session.borrow_mut().begin_preview_number_scrub(
                            input,
                            value,
                            pe.position.x,
                        );
                    }
                })
                .on_pointer_move({
                    let session = session.clone();
                    move |pe: PointerEvent| {
                        session.borrow_mut().scrub_preview_number(
                            pe.position.x,
                            step,
                            pe.modifiers.shift,
                        );
                    }
                })
                .on_pointer_up({
                    let session = session.clone();
                    move |_| {
                        session.borrow_mut().end_preview_number_scrub();
                    }
                }),
        )
}

fn listener_draft_number_scrub(session: SessionRef, value: f64, step: f64) -> View {
    let th = theme();
    let drag = Rc::new(RefCell::new(None::<(f64, f32)>));
    Text(format!("{value:.2}"))
        .size(th.typography.body_medium)
        .color(th.primary)
        .modifier(
            Modifier::new()
                .min_width(52.0)
                .cursor(repose_core::CursorIcon::EwResize)
                .on_pointer_down({
                    let drag = drag.clone();
                    move |pe: PointerEvent| {
                        *drag.borrow_mut() = Some((value, pe.position.x));
                    }
                })
                .on_pointer_move({
                    let session = session.clone();
                    let drag = drag.clone();
                    move |pe: PointerEvent| {
                        let Some((origin, press_x)) = *drag.borrow() else {
                            return;
                        };
                        let dx = (pe.position.x - press_x) as f64;
                        let mult = if pe.modifiers.shift { 0.1 } else { 1.0 };
                        let new_value = origin + dx * step * mult;
                        session.borrow_mut().listener_draft.number_value = new_value;
                        request_frame();
                    }
                })
                .on_pointer_up({
                    let drag = drag.clone();
                    move |_| {
                        *drag.borrow_mut() = None;
                    }
                }),
        )
}

fn dropdown(
    overlay: OverlayHandle,
    key: impl Into<String>,
    label: String,
    items: Vec<DropdownMenuEntry>,
) -> View {
    let key = key.into();
    let state: Rc<MenuState> = remember_with_key(format!("{key}_state"), MenuState::new);
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
        overlay.clone(),
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
    let t = 1.0;
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
