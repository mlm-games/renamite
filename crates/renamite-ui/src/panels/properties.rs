//! Properties inspector: sections, scrubbable numbers, color swatch, keyframe diamonds.
//!
//! Pure data (rows, diamond state, edit commands) comes from
//! `renamite_behavior_common::inspect`; this panel only renders rows and routes
//! pointer drag / click into those helpers. Every edit goes through the shared
//! `resolve_property_edit` authoring loop (static vs key-at-playhead + record).

use renamite_animation::{Angle, Frame};
use renamite_behavior_common::inspect::{
    DiamondState, PropKind, PropRow, apply_value_to_each, cmd_toggle_key, props_for_selection,
};
use renamite_history::ToolOutput;
use renamite_model::{NodeId, PropPath, Value};
use repose_core::input::PointerEvent;
use repose_core::{
    AlignItems, Modifier, PaddingValues, View, remember_with_key, request_frame, theme,
};
use repose_material::material3::{
    IconButton, IconButtonConfig, TooltipBox, TooltipConfig, TooltipState,
};
use repose_ui::{Box, Column, Row, Text, TextStyle, ViewExt};
use smallvec::smallvec;

use crate::components::{CompactIconAction, PanelHeader};
use crate::session::{InspectorDrag, Session, SessionRef};
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
        other => other.clone(),
    }
}
