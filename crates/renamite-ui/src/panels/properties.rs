//! Properties inspector: sections, scrubbable numbers, color swatch, keyframe diamonds.
//!
//! Pure data (rows, diamond state, edit commands) comes from
//! `renamite_behavior_common::inspect`; this panel only renders rows and routes
//! pointer drag / click into those helpers. Every edit goes through the shared
//! `resolve_property_edit` authoring loop (static vs key-at-playhead + record).

use kurbo::{Point as KurboPoint, Shape as _};
use renamite_animation::{Angle, Frame};
use renamite_behavior_common::inspect::{
    DiamondState, PropKind, PropRow, apply_value_to_each, cmd_toggle_key, props_for_selection,
};
use renamite_behavior_common::modifiers::{
    cmd_add_repeater_after, cmd_add_round_corners_after, cmd_add_trim_path_after, cmd_set_trim_mode,
};
use renamite_behavior_common::stroke::{
    cmd_add_stroke_dash_pair, cmd_disable_stroke_dash, cmd_enable_stroke_dash,
    cmd_remove_stroke_dash_pair,
};
use renamite_history::{EditorCommand, ToolOutput, resolve_property_edit};
use renamite_model::{
    Color, GradientKind, GradientStop, GradientStops, NodeId, NodeKind, PropPath, ShapeKind,
    StyleKind, StylePaint, TrimMode, Value, node_affine,
};
use repose_core::input::PointerEvent;
use repose_core::{
    AlignItems, Modifier, PaddingValues, View, remember_with_key, request_frame, theme,
};
use repose_material::material3::{
    IconButton, IconButtonConfig, TextField, TextFieldConfig, TooltipBox, TooltipConfig,
    TooltipState,
};
use repose_ui::{Box, Column, Row, Text, TextStyle, ViewExt};
use smallvec::smallvec;

use crate::components::{CompactIconAction, PanelHeader};
use crate::session::{InspectorDrag, PickerTarget, Session, SessionRef};
use crate::symbols::{AppIcon, Symbols};

pub fn PropertiesPanel(session: SessionRef) -> View {
    let th = theme();
    let (rows, playhead, record, ids) = {
        let s = session.borrow();
        let ids = s.selection.nodes.clone();
        let playhead = Frame(s.playback.head.round() as i64);
        let rows = props_for_selection(&s.file.document, &ids, playhead);
        (rows, playhead, s.record, ids)
    };

    if ids.is_empty() {
        return Column(Modifier::new().fill_max_size()).child((
            PanelHeader(Symbols::settings, "Properties", vec![]),
            Text("No selection")
                .size(th.typography.body_medium)
                .color(th.on_surface_variant)
                .modifier(Modifier::new().padding(16.0)),
        ));
    }

    let title = if ids.len() == 1 {
        session
            .borrow()
            .file
            .document
            .nodes
            .get(ids[0])
            .map(|n| n.name.clone())
            .unwrap_or_else(|| "Properties".into())
    } else {
        format!("{} layers", ids.len())
    };

    let mut sections: Vec<(&str, Vec<PropRow>)> = Vec::new();
    for row in rows {
        if let Some(last) = sections.last_mut()
            && last.0 == row.desc.section
        {
            last.1.push(row);
            continue;
        }
        sections.push((row.desc.section, vec![row]));
    }

    let mut children: Vec<View> = vec![PanelHeader(
        Symbols::settings,
        title,
        vec![CompactIconAction(
            if record {
                Symbols::stop_circle
            } else {
                Symbols::radio_button_unchecked
            },
            if record {
                "Stop recording keys"
            } else {
                "Record keys on edit"
            },
            {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    s.record = !s.record;
                    s.revision = s.revision.wrapping_add(1);
                    request_frame();
                }
            },
        )],
    )];

    // Single selected node: paint section (solid / linear / radial + axis +
    // stops) driving the fill style that paints it.
    if let Some(v) = paint_section(session.clone(), &ids, playhead, record) {
        children.push(v);
    }

    // Single selected text: multiline field editing its content via
    // SetTextContent. One begin/commit transaction per keystroke (coalescing
    // would fold repeats within a single open transaction, but TextField here
    // exposes no focus callbacks to bracket them).
    if ids.len() == 1
        && let Some(section) = text_section(session.clone(), ids[0])
    {
        children.push(section);
    }

    // Single selected stroke: dash section (offset, dash/gap values, enable /
    // disable / add-pair / remove-pair controls).
    if ids.len() == 1
        && let Some(section) = stroke_dash_section(session.clone(), ids[0], playhead, record)
    {
        children.push(section);
    }

    // Single selected shape: offer to add a modifier as its sibling.
    if ids.len() == 1
        && let Some(v) = add_modifier_row(session.clone(), ids[0])
    {
        children.push(v);
    }

    for (section, props) in sections {
        children.push(
            Text(section)
                .size(theme().typography.label_medium)
                .color(th.on_surface_variant)
                .modifier(Modifier::new().padding_values(PaddingValues {
                    left: 12.0,
                    right: 12.0,
                    top: 12.0,
                    bottom: 4.0,
                })),
        );
        for prop in props {
            children.push(PropRowView(
                session.clone(),
                ids.clone(),
                prop,
                playhead,
                record,
            ));
        }
    }

    Column(Modifier::new().fill_max_size()).child(children)
}

fn PropRowView(
    session: SessionRef,
    ids: Vec<NodeId>,
    row: PropRow,
    playhead: Frame,
    record: bool,
) -> View {
    let th = theme();
    let path = row.desc.path.clone();

    // Enum fields (TrimMode) aren't Animated<T>: render a dedicated toggle row
    // instead of the generic scrub + diamond layout.
    if let PropKind::Enum2 { a_label, b_label } = &row.desc.kind
        && let Value::I64(v) = &row.value
    {
        return enum2_row(session, ids, row.desc.label, *v as usize, a_label, b_label);
    }

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
        .gap(8.0))
    .child((
        diamond_button(
            session.clone(),
            ids.clone(),
            path.clone(),
            row.diamond,
            playhead,
        ),
        Text(row.desc.label)
            .size(th.typography.body_medium)
            .color(th.on_surface)
            .modifier(Modifier::new().width(96.0)),
        Box(Modifier::new().flex_grow(1.0)).child(match (&row.desc.kind, &row.value) {
            (PropKind::F64 { step, min, max }, Value::F64(v)) => scrub_f64_w(
                session, ids, path, *v, *step, *min, *max, playhead, record, 0, 64.0,
            ),
            (PropKind::Angle, Value::Angle(a)) => scrub_f64_w(
                session, ids, path, a.0, 1.0, None, None, playhead, record, 0, 64.0,
            ),
            (PropKind::Angle, Value::F64(v)) => scrub_f64_w(
                session, ids, path, *v, 1.0, None, None, playhead, record, 0, 64.0,
            ),
            (PropKind::DVec2, Value::DVec2(v)) => {
                dvec2_editor(session, ids, path, *v, playhead, record)
            }
            (PropKind::Color, Value::Color(c)) => {
                color_row(session, ids, path, *c, playhead, record)
            }
            _ => Text("-")
                .size(th.typography.body_medium)
                .color(th.on_surface_variant),
        }),
    ))
}

/// Keyframe diamond: filled at playhead (remove on click), outline otherwise
/// (add at playhead). Always routes through `resolve_property_edit` semantics.
fn diamond_button(
    session: SessionRef,
    ids: Vec<NodeId>,
    path: PropPath,
    state: DiamondState,
    playhead: Frame,
) -> View {
    let (sym, tip) = match state {
        DiamondState::Empty => (Symbols::radio_button_unchecked, "Add keyframe"),
        DiamondState::HasKeys => (Symbols::radio_button_unchecked, "Add keyframe at playhead"),
        DiamondState::AtPlayhead => (Symbols::stop_circle, "Remove keyframe"),
    };
    let key = format!("diamond_{}", path.as_str());
    let tooltip_state = remember_with_key(key, TooltipState::new);
    let color = if state == DiamondState::AtPlayhead {
        theme().primary
    } else {
        theme().on_surface_variant
    };

    TooltipBox(
        tip,
        (*tooltip_state).clone(),
        IconButton(
            AppIcon(sym, 20.0).color(color),
            {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    let mut cmds = Vec::new();
                    for &id in &ids {
                        if let Some(c) = cmd_toggle_key(&s.file.document, id, &path, playhead) {
                            cmds.push(c);
                        }
                    }
                    if cmds.is_empty() {
                        return;
                    }
                    s.apply_outputs(smallvec![
                        ToolOutput::BeginTransaction("Keyframe".into()),
                        ToolOutput::Commands(cmds.into()),
                        ToolOutput::CommitTransaction,
                    ]);
                }
            },
            IconButtonConfig {
                container_size: Some(32.0),
                ..Default::default()
            },
        ),
        TooltipConfig::default(),
    )
}

#[allow(clippy::too_many_arguments)]
fn scrub_f64_w(
    session: SessionRef,
    ids: Vec<NodeId>,
    path: PropPath,
    value: f64,
    step: f64,
    min: Option<f64>,
    max: Option<f64>,
    playhead: Frame,
    record: bool,
    channel: usize,
    min_width: f32,
) -> View {
    let th = theme();
    let label = format!("{value:.3}");

    Text(label)
        .size(th.typography.body_medium)
        .color(th.primary)
        .modifier(
            Modifier::new()
                .min_width(min_width)
                .on_pointer_down({
                    let session = session.clone();
                    let ids = ids.clone();
                    let path = path.clone();
                    move |pe: PointerEvent| {
                        let mut s = session.borrow_mut();
                        let origin = current_value(&s, &ids, &path, channel);
                        s.inspector_drag = Some(InspectorDrag {
                            path: path.clone(),
                            channel,
                            origin_value: origin,
                            press_x: pe.position.x,
                            txn: false,
                        });
                    }
                })
                .on_pointer_move({
                    let session = session.clone();
                    let ids = ids.clone();
                    let path = path.clone();
                    move |pe: PointerEvent| {
                        let mut s = session.borrow_mut();
                        let Some(drag) = s.inspector_drag.clone() else {
                            return;
                        };
                        if drag.path != path || drag.channel != channel {
                            return;
                        }
                        let dx = (pe.position.x - drag.press_x) as f64;
                        let mult = if pe.modifiers.shift { 0.1 } else { 1.0 };
                        let delta = dx * step * mult;
                        let new_v = apply_channel(&drag.origin_value, channel, |o| {
                            let mut v = o + delta;
                            if let Some(lo) = min {
                                v = v.max(lo);
                            }
                            if let Some(hi) = max {
                                v = v.min(hi);
                            }
                            v
                        });
                        let mut outs = smallvec![];
                        if !drag.txn {
                            outs.push(ToolOutput::BeginTransaction("Edit property".into()));
                            if let Some(d) = s.inspector_drag.as_mut() {
                                d.txn = true;
                            }
                        }
                        let cmds = apply_value_to_each(
                            &s.file.document,
                            &ids,
                            &path,
                            new_v,
                            playhead,
                            record,
                        );
                        outs.push(ToolOutput::Commands(cmds.into()));
                        s.apply_outputs(outs);
                    }
                })
                .on_pointer_up({
                    let session = session.clone();
                    move |_pe: PointerEvent| {
                        let mut s = session.borrow_mut();
                        if let Some(d) = s.inspector_drag.take() {
                            if d.txn {
                                s.apply_outputs(smallvec![ToolOutput::CommitTransaction]);
                            } else {
                                request_frame();
                            }
                        }
                    }
                }),
        )
}

fn dvec2_editor(
    session: SessionRef,
    ids: Vec<NodeId>,
    path: PropPath,
    v: glam::DVec2,
    playhead: Frame,
    record: bool,
) -> View {
    let th = theme();
    Row(Modifier::new().gap(6.0).align_items(AlignItems::CENTER)).child((
        Text("X")
            .size(th.typography.label_medium)
            .color(th.on_surface_variant),
        scrub_f64_w(
            session.clone(),
            ids.clone(),
            path.clone(),
            v.x,
            1.0,
            None,
            None,
            playhead,
            record,
            0,
            56.0,
        ),
        Text("Y")
            .size(th.typography.label_medium)
            .color(th.on_surface_variant),
        scrub_f64_w(
            session, ids, path, v.y, 1.0, None, None, playhead, record, 1, 56.0,
        ),
    ))
}

fn color_row(
    session: SessionRef,
    ids: Vec<NodeId>,
    path: PropPath,
    c: renamite_model::Color,
    playhead: Frame,
    record: bool,
) -> View {
    let swatch = Box(Modifier::new()
        .width(20.0)
        .height(20.0)
        .background(repose_core::Color(
            (c.r * 255.0) as u8,
            (c.g * 255.0) as u8,
            (c.b * 255.0) as u8,
            (c.a * 255.0) as u8,
        )));
    Row(Modifier::new().gap(4.0).align_items(AlignItems::CENTER)).child((
        swatch,
        scrub_f64_w(
            session.clone(),
            ids.clone(),
            path.clone(),
            c.r,
            0.01,
            Some(0.0),
            Some(1.0),
            playhead,
            record,
            0,
            40.0,
        ),
        scrub_f64_w(
            session.clone(),
            ids.clone(),
            path.clone(),
            c.g,
            0.01,
            Some(0.0),
            Some(1.0),
            playhead,
            record,
            1,
            40.0,
        ),
        scrub_f64_w(
            session.clone(),
            ids.clone(),
            path.clone(),
            c.b,
            0.01,
            Some(0.0),
            Some(1.0),
            playhead,
            record,
            2,
            40.0,
        ),
        scrub_f64_w(
            session,
            ids,
            path,
            c.a,
            0.01,
            Some(0.0),
            Some(1.0),
            playhead,
            record,
            3,
            40.0,
        ),
    ))
}

fn current_value(s: &Session, ids: &[NodeId], path: &PropPath, _ch: usize) -> Value {
    s.file
        .document
        .value_at(ids[0], path, s.playback.head)
        .unwrap_or(Value::F64(0.0))
}

fn apply_channel(origin: &Value, channel: usize, f: impl FnOnce(f64) -> f64) -> Value {
    match origin {
        Value::F64(v) => Value::F64(f(*v)),
        Value::Angle(a) => Value::Angle(Angle(f(a.0))),
        Value::DVec2(v) => {
            let mut n = *v;
            if channel == 0 {
                n.x = f(v.x);
            } else {
                n.y = f(v.y);
            }
            Value::DVec2(n)
        }
        Value::Color(c) => {
            let mut c = *c;
            match channel {
                0 => c.r = f(c.r),
                1 => c.g = f(c.g),
                2 => c.b = f(c.b),
                _ => c.a = f(c.a),
            }
            Value::Color(c)
        }
        Value::Stops(s) => {
            // Channel encodes (stop_index * 5 + component) where component
            // 0 = offset, 1..=4 = r,g,b,a.
            let mut s = s.clone();
            let stop = channel / 5;
            let comp = channel % 5;
            if let Some(st) = s.0.get_mut(stop) {
                match comp {
                    0 => st.offset = f(st.offset).clamp(0.0, 1.0),
                    1 => st.color.r = f(st.color.r).clamp(0.0, 1.0),
                    2 => st.color.g = f(st.color.g).clamp(0.0, 1.0),
                    3 => st.color.b = f(st.color.b).clamp(0.0, 1.0),
                    _ => st.color.a = f(st.color.a).clamp(0.0, 1.0),
                }
            }
            Value::Stops(s)
        }
        other => other.clone(),
    }
}

/// Row of "Add <modifier>" buttons shown when exactly one shape is selected.
/// Trim Path applies to any shape; Round Corners is gated to rect/path (the
/// corner-bearing shapes - ellipse/star/polygon have their own roundness).
fn add_modifier_row(session: SessionRef, id: NodeId) -> Option<View> {
    let th = theme();
    let can_round = {
        let s = session.borrow();
        match &s.file.document.nodes.get(id)?.kind {
            NodeKind::Shape(ShapeKind::Rect { .. }) | NodeKind::Shape(ShapeKind::Path(_)) => true,
            NodeKind::Shape(_) => false,
            _ => return None,
        }
    };
    let add_trim = Row(Modifier::new().gap(4.0).align_items(AlignItems::CENTER)).child((
        CompactIconAction(Symbols::add, "Add Trim Path", {
            let session = session.clone();
            move || {
                let mut s = session.borrow_mut();
                if let Some(cmd) = cmd_add_trim_path_after(&s.file.document, id) {
                    s.apply_outputs(smallvec![
                        ToolOutput::BeginTransaction("Add Trim Path".into()),
                        ToolOutput::Commands(smallvec![cmd]),
                        ToolOutput::CommitTransaction,
                    ]);
                }
            }
        }),
        Text("Trim Path")
            .size(th.typography.body_medium)
            .color(th.on_surface_variant),
    ));
    let mut buttons = vec![add_trim];
    if can_round {
        buttons.push(
            Row(Modifier::new().gap(4.0).align_items(AlignItems::CENTER)).child((
                CompactIconAction(Symbols::add, "Add Round Corners", {
                    let session = session.clone();
                    move || {
                        let mut s = session.borrow_mut();
                        if let Some(cmd) = cmd_add_round_corners_after(&s.file.document, id, 10.0) {
                            s.apply_outputs(smallvec![
                                ToolOutput::BeginTransaction("Add Round Corners".into()),
                                ToolOutput::Commands(smallvec![cmd]),
                                ToolOutput::CommitTransaction,
                            ]);
                        }
                    }
                }),
                Text("Round Corners")
                    .size(th.typography.body_medium)
                    .color(th.on_surface_variant),
            )),
        );
    }
    buttons.push(
        Row(Modifier::new().gap(4.0).align_items(AlignItems::CENTER)).child((
            CompactIconAction(Symbols::add, "Add Repeater", {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    if let Some(cmd) = cmd_add_repeater_after(&s.file.document, id) {
                        s.apply_outputs(smallvec![
                            ToolOutput::BeginTransaction("Add Repeater".into()),
                            ToolOutput::Commands(smallvec![cmd]),
                            ToolOutput::CommitTransaction,
                        ]);
                    }
                }
            }),
            Text("Repeater")
                .size(th.typography.body_medium)
                .color(th.on_surface_variant),
        )),
    );
    Some(
        Row(Modifier::new()
            .height(40.0)
            .fill_max_width()
            .padding_values(PaddingValues {
                left: 12.0,
                right: 8.0,
                top: 0.0,
                bottom: 0.0,
            })
            .align_items(AlignItems::CENTER)
            .gap(12.0))
        .child((
            Text("Add modifier")
                .size(th.typography.label_medium)
                .color(th.on_surface_variant),
            Box(Modifier::new().flex_grow(1.0))
                .child(Row(Modifier::new().gap(12.0)).child(buttons)),
        )),
    )
}

/// Two-segment toggle for `PropKind::Enum2` (diamond-less, non-animatable).
fn enum2_row(
    session: SessionRef,
    ids: Vec<NodeId>,
    label: &'static str,
    current: usize,
    a_label: &'static str,
    b_label: &'static str,
) -> View {
    let th = theme();
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
        .gap(8.0))
    .child((
        Box(Modifier::new().width(32.0)), // diamond spacer
        Text(label)
            .size(th.typography.body_medium)
            .color(th.on_surface)
            .modifier(Modifier::new().width(96.0)),
        enum2_segment(session.clone(), ids.clone(), current, 0, a_label),
        enum2_segment(session, ids, current, 1, b_label),
    ))
}

fn enum2_segment(
    session: SessionRef,
    ids: Vec<NodeId>,
    current: usize,
    index: usize,
    label: &'static str,
) -> View {
    let th = theme();
    let active = current == index;
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
                .on_pointer_down({
                    let session = session.clone();
                    let ids = ids.clone();
                    move |_pe: PointerEvent| {
                        let mut s = session.borrow_mut();
                        // Enum2 is only wired to TrimMode today: 0 -> Individually,
                        // 1 -> Simultaneously.
                        let mode = if index == 1 {
                            TrimMode::Simultaneously
                        } else {
                            TrimMode::Individually
                        };
                        let cmds: Vec<_> = ids
                            .iter()
                            .filter_map(|&id| cmd_set_trim_mode(&s.file.document, id, mode))
                            .collect();
                        if cmds.is_empty() {
                            return;
                        }
                        s.apply_outputs(smallvec![
                            ToolOutput::BeginTransaction("Trim mode".into()),
                            ToolOutput::Commands(cmds.into()),
                            ToolOutput::CommitTransaction,
                        ]);
                    }
                }),
        )
}

fn text_section(session: SessionRef, id: NodeId) -> Option<View> {
    let content = {
        let session = session.borrow();
        match &session.file.document.nodes.get(id)?.kind {
            NodeKind::Text(t) => t.text.clone(),
            _ => return None,
        }
    };
    let th = theme();
    Some(
        Column(Modifier::new().fill_max_width())
            .child((
                Text("Text")
                    .size(th.typography.label_medium)
                    .color(th.on_surface_variant)
                    .modifier(Modifier::new().padding_values(PaddingValues {
                        left: 12.0,
                        right: 12.0,
                        top: 12.0,
                        bottom: 4.0,
                    })),
                Row(Modifier::new()
                    .fill_max_width()
                    .padding_values(PaddingValues {
                        left: 12.0,
                        right: 8.0,
                        top: 0.0,
                        bottom: 4.0,
                    }))
                .child(TextField(
                    Modifier::new().fill_max_width(),
                    content,
                    {
                        let session = session.clone();
                        move |text: String| {
                            let mut s = session.borrow_mut();
                            s.apply_outputs(smallvec![
                                ToolOutput::BeginTransaction("Edit text".into()),
                                ToolOutput::Commands(smallvec![
                                    EditorCommand::SetTextContent { id, text }
                                ]),
                                ToolOutput::CommitTransaction,
                            ]);
                        }
                    },
                    TextFieldConfig {
                        single_line: false,
                        ..Default::default()
                    },
                )),
            )),
    )
}

fn paint_style_id(session: &Session, selected: NodeId) -> Option<NodeId> {
    match &session.file.document.nodes.get(selected)?.kind {
        NodeKind::Style(StyleKind::Fill { .. }) | NodeKind::Style(StyleKind::Stroke { .. }) => {
            Some(selected)
        }

        NodeKind::Shape(_) => session
            .engine
            .scene()
            .items
            .iter()
            .rev()
            .find(|item| item.node == selected)
            .map(|item| item.style),

        NodeKind::Text(_) => session
            .engine
            .scene()
            .items
            .iter()
            .rev()
            .find(|item| item.node == selected)
            .map(|item| item.style),

        _ => None,
    }
}

fn default_axis(s: &Session, shape: NodeId, frame: f64) -> Option<(glam::DVec2, glam::DVec2)> {
    let r = s
        .engine
        .scene()
        .items
        .iter()
        .find(|it| it.node == shape)?
        .path
        .bounding_box();
    let w2l = node_affine(&s.file.document, shape, frame).inverse();
    let a = w2l * KurboPoint::new(r.x0, r.y0);
    let b = w2l * KurboPoint::new(r.x1, r.y0);
    Some((glam::DVec2::new(a.x, a.y), glam::DVec2::new(b.x, b.y)))
}

fn paint_section(
    session: SessionRef,
    ids: &[NodeId],
    playhead: Frame,
    record: bool,
) -> Option<View> {
    if ids.len() != 1 {
        return None;
    }
    let id = ids[0];
    let (style_id, paint, section_label, solid_path) = {
        let session = session.borrow();
        let style_id = paint_style_id(&session, id)?;
        let node = session.file.document.nodes.get(style_id)?;

        match &node.kind {
            NodeKind::Style(StyleKind::Fill { paint, .. }) => {
                (style_id, paint.clone(), "Fill", "fill.color")
            }
            NodeKind::Style(StyleKind::Stroke { paint, .. }) => {
                (style_id, paint.clone(), "Stroke", "stroke.color")
            }
            _ => return None,
        }
    };
    let th = theme();
    let gradient = match &paint {
        StylePaint::Gradient(g) => Some(g.clone()),
        _ => None,
    };
    let active_kind = gradient.as_ref().map(|g| g.kind);

    let is_solid = gradient.is_none();
    let toggle = Row(Modifier::new()
        .height(36.0)
        .fill_max_width()
        .padding_values(PaddingValues {
            left: 12.0,
            right: 8.0,
            top: 0.0,
            bottom: 0.0,
        })
        .align_items(AlignItems::CENTER)
        .gap(8.0))
    .child((
        Box(Modifier::new().width(32.0)), // diamond spacer
        Text("Paint")
            .size(th.typography.body_medium)
            .color(th.on_surface)
            .modifier(Modifier::new().width(96.0)),
        paint_segment(
            session.clone(),
            id,
            style_id,
            "Solid",
            is_solid,
            PaintTarget::Solid,
        ),
        paint_segment(
            session.clone(),
            id,
            style_id,
            "Linear",
            active_kind == Some(GradientKind::Linear),
            PaintTarget::Gradient(GradientKind::Linear),
        ),
        paint_segment(
            session.clone(),
            id,
            style_id,
            "Radial",
            active_kind == Some(GradientKind::Radial),
            PaintTarget::Gradient(GradientKind::Radial),
        ),
    ));

    let mut children = vec![
        Text(section_label)
            .size(theme().typography.label_medium)
            .color(th.on_surface_variant)
            .modifier(Modifier::new().padding_values(PaddingValues {
                left: 12.0,
                right: 12.0,
                top: 12.0,
                bottom: 4.0,
            })),
        toggle,
    ];

    match &paint {
        StylePaint::Solid { .. } => {
            // Color row bound to the style node's solid-color property.
            let path = PropPath::new(solid_path);
            let swatch = paint_swatch_button(
                session.clone(),
                PickerTarget::StyleColor { style_id },
                paint.base_color(),
            );
            children.push(
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
                    .gap(8.0))
                .child((
                    Box(Modifier::new().width(32.0)),
                    Text("Color")
                        .size(th.typography.body_medium)
                        .color(th.on_surface)
                        .modifier(Modifier::new().width(96.0)),
                    swatch,
                    Box(Modifier::new().flex_grow(1.0)).child(color_row(
                        session.clone(),
                        vec![style_id],
                        path,
                        paint.base_color(),
                        playhead,
                        record,
                    )),
                )),
            );
        }
        StylePaint::Gradient(g) => {
            children.push(axis_row(
                session.clone(),
                style_id,
                playhead,
                record,
                "Start",
                "grad.start",
                g.start.value_at(playhead.0 as f64),
            ));
            children.push(axis_row(
                session.clone(),
                style_id,
                playhead,
                record,
                "End",
                "grad.end",
                g.end.value_at(playhead.0 as f64),
            ));
            children.extend(stop_rows(
                session.clone(),
                style_id,
                playhead,
                record,
                g.stops.value_at(playhead.0 as f64),
            ));
            // Add-stop button.
            children.push(
                Row(Modifier::new()
                    .height(32.0)
                    .padding_values(PaddingValues {
                        left: 12.0,
                        right: 8.0,
                        top: 0.0,
                        bottom: 0.0,
                    })
                    .align_items(AlignItems::CENTER)
                    .gap(4.0))
                .child((
                    CompactIconAction(Symbols::add, "Add stop", {
                        let session = session.clone();
                        move || {
                            let mut s = session.borrow_mut();
                            let frame = s.playback.head;
                            let Some(stops) = s
                                .file
                                .document
                                .value_at(style_id, &PropPath::new("grad.stops"), frame)
                                .ok()
                            else {
                                return;
                            };
                            let Value::Stops(mut stops) = stops else {
                                return;
                            };
                            let last_color =
                                stops.0.last().map(|x| x.color).unwrap_or(Color::BLACK);
                            stops.0.push(GradientStop {
                                offset: 1.0,
                                color: last_color,
                            });
                            let cmd = resolve_property_edit(
                                &s.file.document,
                                style_id,
                                &PropPath::new("grad.stops"),
                                Value::Stops(stops),
                                Frame(frame.round() as i64),
                                s.record,
                            );
                            s.apply_outputs(smallvec![
                                ToolOutput::BeginTransaction("Add stop".into()),
                                ToolOutput::Commands(smallvec![cmd]),
                                ToolOutput::CommitTransaction,
                            ]);
                        }
                    }),
                    Text("Add stop")
                        .size(th.typography.body_medium)
                        .color(th.on_surface_variant),
                )),
            );
        }
    }

    children.push(
        Row(Modifier::new()
            .height(32.0)
            .fill_max_width()
            .padding_values(PaddingValues {
                left: 12.0,
                right: 8.0,
                top: 0.0,
                bottom: 0.0,
            })
            .align_items(AlignItems::CENTER)
            .gap(8.0))
        .child((
            Box(Modifier::new().width(32.0)),
            Text("Swatch")
                .size(th.typography.body_medium)
                .color(th.on_surface)
                .modifier(Modifier::new().width(96.0)),
            CompactIconAction(Symbols::palette, "Use as current paint", {
                let session = session.clone();
                let paint = paint.clone();
                move || {
                    let mut session = session.borrow_mut();
                    session.current_paint = paint.snapshot(session.playback.head);

                    session.status = Some("Current paint updated".into());
                    session.revision = session.revision.wrapping_add(1);
                    request_frame();
                }
            }),
            Text("Use as current paint")
                .size(th.typography.body_medium)
                .color(th.on_surface_variant),
        )),
    );

    Some(Column(Modifier::new().fill_max_width()).child(children))
}

#[derive(Clone, Copy)]
enum PaintTarget {
    Solid,
    Gradient(GradientKind),
}

fn paint_segment(
    session: SessionRef,
    shape: NodeId,
    style_id: NodeId,
    label: &'static str,
    active: bool,
    target: PaintTarget,
) -> View {
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
                .on_pointer_down({
                    let session = session.clone();
                    move |_pe: PointerEvent| {
                        let mut s = session.borrow_mut();
                        let frame = s.playback.head;
                        let current = match &s.file.document.nodes.get(style_id).map(|n| &n.kind) {
                            Some(NodeKind::Style(st)) => st.paint().clone(),
                            _ => return,
                        };
                        // Solid: collapse to the first stop's color. Gradient:
                        // keep stops+axis when switching kind; seed a default
                        // axis from the shape bounds when converting from solid.
                        let cmd = match target {
                            PaintTarget::Solid => {
                                Some(EditorCommand::ConvertToSolid { id: style_id })
                            }
                            PaintTarget::Gradient(kind) => match (&current, kind) {
                                (StylePaint::Gradient(g), k) if g.kind == k => None,
                                (StylePaint::Gradient(g), k) => {
                                    let mut g = g.clone();
                                    g.kind = k;
                                    Some(EditorCommand::SetPaint {
                                        id: style_id,
                                        paint: StylePaint::Gradient(g),
                                    })
                                }
                                (StylePaint::Solid { .. }, k) => {
                                    let (start, end) = default_axis(&s, shape, frame)
                                        .unwrap_or((glam::DVec2::ZERO, glam::DVec2::new(1.0, 0.0)));
                                    Some(EditorCommand::ConvertToGradient {
                                        id: style_id,
                                        kind: k,
                                        start,
                                        end,
                                    })
                                }
                            },
                        };
                        if let Some(cmd) = cmd {
                            s.apply_outputs(smallvec![
                                ToolOutput::BeginTransaction("Paint".into()),
                                ToolOutput::Commands(smallvec![cmd]),
                                ToolOutput::CommitTransaction,
                            ]);
                        }
                    }
                }),
        )
}

/// A small clickable fill-color swatch that opens the picker for `target`.
fn paint_swatch_button(session: SessionRef, target: PickerTarget, color: Color) -> View {
    let th = theme();
    Box(Modifier::new()
        .width(28.0)
        .height(28.0)
        .clip_rounded(6.0)
        .border(1.0, th.outline_variant, 6.0)
        .background(repose_core::Color(
            (color.r * 255.0) as u8,
            (color.g * 255.0) as u8,
            (color.b * 255.0) as u8,
            (color.a * 255.0) as u8,
        ))
        .on_pointer_down({
            let session = session.clone();
            move |_| {
                session.borrow_mut().open_color_picker(target, color);
            }
        }))
}

fn axis_row(
    session: SessionRef,
    style_id: NodeId,
    playhead: Frame,
    record: bool,
    label: &'static str,
    path: &'static str,
    v: glam::DVec2,
) -> View {
    let th = theme();
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
        .gap(8.0))
    .child((
        Box(Modifier::new().width(32.0)),
        Text(label)
            .size(th.typography.body_medium)
            .color(th.on_surface)
            .modifier(Modifier::new().width(96.0)),
        Box(Modifier::new().flex_grow(1.0)).child(dvec2_editor(
            session,
            vec![style_id],
            PropPath::new(path),
            v,
            playhead,
            record,
        )),
    ))
}

fn stop_rows(
    session: SessionRef,
    style_id: NodeId,
    playhead: Frame,
    record: bool,
    stops: GradientStops,
) -> Vec<View> {
    stops
        .0
        .iter()
        .enumerate()
        .map(|(i, stop)| {
            let th = theme();
            let path = PropPath::new("grad.stops");
            let stop_color = stop.color;
            let swatch = Box(Modifier::new()
                .width(20.0)
                .height(20.0)
                .clip_rounded(4.0)
                .border(1.0, th.outline_variant, 4.0)
                .background(repose_core::Color(
                    (stop_color.r * 255.0) as u8,
                    (stop_color.g * 255.0) as u8,
                    (stop_color.b * 255.0) as u8,
                    (stop_color.a * 255.0) as u8,
                ))
                .on_pointer_down({
                    let session = session.clone();
                    move |_| {
                        session.borrow_mut().open_color_picker(
                            PickerTarget::GradientStop { style_id, index: i },
                            stop_color,
                        );
                    }
                }));
            let base = i * 5;
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
                Box(Modifier::new().width(24.0)).child(
                    Text(format!("{}", i + 1))
                        .size(th.typography.label_medium)
                        .color(th.on_surface_variant),
                ),
                swatch,
                scrub_f64_w(
                    session.clone(),
                    vec![style_id],
                    path.clone(),
                    stop.offset,
                    0.01,
                    Some(0.0),
                    Some(1.0),
                    playhead,
                    record,
                    base,
                    40.0,
                ),
                scrub_f64_w(
                    session.clone(),
                    vec![style_id],
                    path.clone(),
                    stop.color.r,
                    0.01,
                    Some(0.0),
                    Some(1.0),
                    playhead,
                    record,
                    base + 1,
                    40.0,
                ),
                scrub_f64_w(
                    session.clone(),
                    vec![style_id],
                    path.clone(),
                    stop.color.g,
                    0.01,
                    Some(0.0),
                    Some(1.0),
                    playhead,
                    record,
                    base + 2,
                    40.0,
                ),
                scrub_f64_w(
                    session.clone(),
                    vec![style_id],
                    path.clone(),
                    stop.color.b,
                    0.01,
                    Some(0.0),
                    Some(1.0),
                    playhead,
                    record,
                    base + 3,
                    40.0,
                ),
                scrub_f64_w(
                    session.clone(),
                    vec![style_id],
                    path,
                    stop.color.a,
                    0.01,
                    Some(0.0),
                    Some(1.0),
                    playhead,
                    record,
                    base + 4,
                    40.0,
                ),
            ))
        })
        .collect()
}

fn stroke_dash_section(
    session: SessionRef,
    id: NodeId,
    playhead: Frame,
    record: bool,
) -> Option<View> {
    let (is_stroke, dash) = {
        let session = session.borrow();
        let node = session.file.document.nodes.get(id)?;

        match &node.kind {
            NodeKind::Style(StyleKind::Stroke { dash, .. }) => (true, dash.clone()),

            _ => (false, None),
        }
    };

    if !is_stroke {
        return None;
    }

    let th = theme();
    let mut children = vec![
        Text("Dash")
            .size(th.typography.label_medium)
            .color(th.on_surface_variant)
            .modifier(Modifier::new().padding_values(PaddingValues {
                left: 12.0,
                right: 12.0,
                top: 12.0,
                bottom: 4.0,
            })),
    ];

    let Some(dash) = dash else {
        children.push(
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
                .gap(8.0))
            .child((
                CompactIconAction(Symbols::add, "Enable dashes", {
                    let session = session.clone();

                    move || {
                        let mut session = session.borrow_mut();

                        let Some(command) = cmd_enable_stroke_dash(&session.file.document, id)
                        else {
                            return;
                        };

                        session.apply_outputs(smallvec![
                            ToolOutput::BeginTransaction("Enable dashes".into()),
                            ToolOutput::Commands(smallvec![command]),
                            ToolOutput::CommitTransaction,
                        ]);
                    }
                }),
                Text("Enable stroke dashes")
                    .size(th.typography.body_medium)
                    .color(th.on_surface_variant),
            )),
        );

        return Some(Column(Modifier::new().fill_max_width()).child(children));
    };

    // Offset
    children.push(dash_scalar_row(
        session.clone(),
        id,
        playhead,
        record,
        "Offset",
        PropPath::new("stroke.dash.offset"),
        dash.offset.value_at(playhead.0 as f64),
        None,
    ));

    // Alternating dash/gap values.
    for (index, value) in dash.dashes.iter().enumerate() {
        let label = if index % 2 == 0 {
            format!("Dash {}", index / 2 + 1)
        } else {
            format!("Gap {}", index / 2 + 1)
        };

        children.push(dash_scalar_row(
            session.clone(),
            id,
            playhead,
            record,
            label,
            PropPath::new(format!("stroke.dash.{index}")),
            value.value_at(playhead.0 as f64),
            Some(0.0),
        ));
    }

    // Structural controls.
    children.push(
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
            .gap(8.0))
        .child((
            CompactIconAction(Symbols::add, "Add dash/gap pair", {
                let session = session.clone();

                move || {
                    apply_dash_structure_command(&session, "Add dash pair", |doc| {
                        cmd_add_stroke_dash_pair(doc, id)
                    });
                }
            }),
            if dash.dashes.len() > 2 {
                CompactIconAction(Symbols::remove, "Remove last dash/gap pair", {
                    let session = session.clone();

                    move || {
                        apply_dash_structure_command(&session, "Remove dash pair", |doc| {
                            cmd_remove_stroke_dash_pair(doc, id)
                        });
                    }
                })
            } else {
                Box(Modifier::new().width(40.0))
            },
            CompactIconAction(Symbols::delete, "Disable dashes", {
                let session = session.clone();

                move || {
                    apply_dash_structure_command(&session, "Disable dashes", |doc| {
                        cmd_disable_stroke_dash(doc, id)
                    });
                }
            }),
        )),
    );

    Some(Column(Modifier::new().fill_max_width()).child(children))
}

#[allow(clippy::too_many_arguments)]
fn dash_scalar_row(
    session: SessionRef,
    id: NodeId,
    playhead: Frame,
    record: bool,
    label: impl Into<String>,
    path: PropPath,
    value: f64,
    min: Option<f64>,
) -> View {
    let label = label.into();

    let state = {
        let session = session.borrow();
        let animated = session.file.document.property_is_animated(id, &path);

        if session
            .file
            .document
            .keyframe_data(id, &path, playhead)
            .is_some()
        {
            DiamondState::AtPlayhead
        } else if animated {
            DiamondState::HasKeys
        } else {
            DiamondState::Empty
        }
    };

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
        .gap(8.0))
    .child((
        diamond_button(session.clone(), vec![id], path.clone(), state, playhead),
        Text(label)
            .size(theme().typography.body_medium)
            .color(theme().on_surface)
            .modifier(Modifier::new().width(96.0)),
        Box(Modifier::new().flex_grow(1.0)).child(scrub_f64_w(
            session,
            vec![id],
            path,
            value,
            0.5,
            min,
            None,
            playhead,
            record,
            0,
            64.0,
        )),
    ))
}

fn apply_dash_structure_command(
    session: &SessionRef,
    label: &'static str,
    command: impl FnOnce(&renamite_model::Document) -> Option<EditorCommand>,
) {
    let mut session = session.borrow_mut();

    let Some(command) = command(&session.file.document) else {
        return;
    };

    session.apply_outputs(smallvec![
        ToolOutput::BeginTransaction(label.into()),
        ToolOutput::Commands(smallvec![command]),
        ToolOutput::CommitTransaction,
    ]);
}
