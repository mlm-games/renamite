//! Properties inspector: sections, scrubbable numbers, color swatch, keyframe diamonds.
//!
//! Pure data (rows, diamond state, edit commands) comes from
//! `renamite_behavior_common::inspect`; this panel only renders rows and routes
//! pointer drag / click into those helpers. Every edit goes through the shared
//! `resolve_property_edit` authoring loop (static vs key-at-playhead + record).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use kurbo::{Point as KurboPoint, Shape as _};
use renamite_animation::{Angle, Frame};
use renamite_behavior_common::inspect::{
    DiamondState, PropKind, PropRow, apply_value_to_each, cmd_toggle_key, props_for_node,
    props_for_selection,
};
use renamite_behavior_common::modifiers::{
    cmd_add_offset_path_after, cmd_add_pucker_bloat_after, cmd_add_repeater_after,
    cmd_add_round_corners_after, cmd_add_trim_path_after, cmd_add_zigzag_after,
};
use renamite_behavior_common::stroke::{
    cmd_add_stroke_dash_pair, cmd_disable_stroke_dash, cmd_enable_stroke_dash,
    cmd_remove_stroke_dash_pair,
};
use renamite_history::{EditorCommand, ToolOutput, resolve_property_edit};
use renamite_model::{
    Color, GradientKind, GradientStop, GradientStops, NodeId, NodeKind, PropPath, ShapeKind,
    StyleKind, StylePaint, Value, node_affine,
};
use repose_core::input::PointerEvent;
use repose_core::{
    AlignItems, FocusRequester, ImeAction, KeyboardCapitalization, KeyboardType, Modifier,
    PaddingValues, TextFieldLineLimits, TextInputConfig, View, ViewKind, remember_with_key,
    request_frame, theme,
};
use repose_material::material3::{
    IconButton, IconButtonConfig, TooltipBox, TooltipConfig, TooltipState,
};
use repose_ui::scroll::{ScrollArea, remember_scroll_state};
use repose_ui::{Box, Column, FlowRow, Row, Text, TextStyle, ViewExt};
use smallvec::smallvec;

use crate::components::{CompactIconAction, PanelHeader};
use crate::session::{
    EditorMode, InspectorDrag, PickerTarget, Session, SessionRef, overlay_anchor,
};
use crate::symbols::{AppIcon, Symbols};

pub fn PropertiesPanel(session: SessionRef) -> View {
    let th = theme();
    let (rows, playhead, record, ids, inspect_ids, diamond_quiet) = {
        let s = session.borrow();
        let ids = s.selection.nodes.clone();
        let playhead = Frame(s.playback.head.round() as i64);
        let inspect_ids: Vec<NodeId> = if ids.len() == 1 {
            vec![effective_inspect_id(&s.file.document, ids[0])]
        } else {
            ids.clone()
        };
        let rows = props_for_selection(&s.file.document, &inspect_ids, playhead);
        (
            rows,
            playhead,
            s.record,
            ids,
            inspect_ids,
            s.mode == EditorMode::Design,
        )
    };

    if ids.is_empty() {
        let (comp_id, comp_name, size, rate, range) = {
            let s = session.borrow();
            let comp_id = s.file.document.main;
            let comp = &s.file.document.compositions[comp_id];
            (comp_id, comp.name.clone(), comp.size, comp.rate, comp.range)
        };
        let comp_section = {
            let th = theme();
            let duration = (range.1.0 - range.0.0).max(0);
            crate::components::CollapsibleSection(
                "composition_props",
                format!("Composition · {comp_name}"),
                vec![],
                Column(Modifier::new().fill_max_width()).child((
                    // Name row
                    Row(Modifier::new()
                        .fill_max_width()
                        .padding_values(PaddingValues {
                            left: 12.0,
                            right: 8.0,
                            top: 6.0,
                            bottom: 2.0,
                        })
                        .gap(8.0)
                        .align_items(AlignItems::CENTER))
                    .child((
                        Text("Name")
                            .size(th.typography.body_medium)
                            .color(th.on_surface)
                            .modifier(Modifier::new().width(96.0)),
                        Box(Modifier::new().width(176.0)).child(crate::components::AppTextField(
                            format!("comp_name_{comp_id:?}"),
                            comp_name.clone(),
                            "Name",
                            false,
                            32.0,
                            {
                                let session = session.clone();
                                move |text: String| {
                                    let name = text.trim().to_string();
                                    if name.is_empty() {
                                        return;
                                    }
                                    let mut s = session.borrow_mut();
                                    s.apply_outputs(smallvec![
                                        ToolOutput::BeginTransaction("Set composition name".into()),
                                        ToolOutput::Commands(smallvec![
                                            EditorCommand::SetCompositionName {
                                                comp: comp_id,
                                                name: name.clone(),
                                            }
                                        ]),
                                        ToolOutput::CommitTransaction,
                                    ]);
                                }
                            },
                        )),
                    )),
                    // Size row: editable W x H
                    Row(Modifier::new()
                        .fill_max_width()
                        .padding_values(PaddingValues {
                            left: 12.0,
                            right: 8.0,
                            top: 6.0,
                            bottom: 6.0,
                        })
                        .gap(8.0)
                        .align_items(AlignItems::CENTER))
                    .child((
                        Text("Size")
                            .size(th.typography.body_medium)
                            .color(th.on_surface)
                            .modifier(Modifier::new().width(96.0)),
                        Box(Modifier::new().width(84.0)).child(crate::components::AppTextField(
                            format!("comp_w_{comp_id:?}"),
                            size.0.to_string(),
                            "W",
                            true,
                            32.0,
                            {
                                let session = session.clone();
                                move |text: String| {
                                    if let Ok(w) = text.trim().parse::<u32>() {
                                        let mut s = session.borrow_mut();
                                        let h = s.file.document.compositions[comp_id].size.1;
                                        s.apply_outputs(smallvec![
                                            ToolOutput::BeginTransaction(
                                                "Set composition size".into()
                                            ),
                                            ToolOutput::Commands(smallvec![
                                                EditorCommand::SetCompositionSize {
                                                    comp: comp_id,
                                                    size: (w.max(1), h.max(1)),
                                                }
                                            ]),
                                            ToolOutput::CommitTransaction,
                                        ]);
                                    }
                                }
                            },
                        )),
                        Text("×")
                            .size(th.typography.body_medium)
                            .color(th.on_surface_variant),
                        Box(Modifier::new().width(84.0)).child(crate::components::AppTextField(
                            format!("comp_h_{comp_id:?}"),
                            size.1.to_string(),
                            "H",
                            true,
                            32.0,
                            {
                                let session = session.clone();
                                move |text: String| {
                                    if let Ok(h) = text.trim().parse::<u32>() {
                                        let mut s = session.borrow_mut();
                                        let w = s.file.document.compositions[comp_id].size.0;
                                        s.apply_outputs(smallvec![
                                            ToolOutput::BeginTransaction(
                                                "Set composition size".into()
                                            ),
                                            ToolOutput::Commands(smallvec![
                                                EditorCommand::SetCompositionSize {
                                                    comp: comp_id,
                                                    size: (w.max(1), h.max(1)),
                                                }
                                            ]),
                                            ToolOutput::CommitTransaction,
                                        ]);
                                    }
                                }
                            },
                        )),
                        Text("px")
                            .size(th.typography.label_medium)
                            .color(th.on_surface_variant),
                    )),
                    // Frame rate: editable fps (num, den=1)
                    Row(Modifier::new()
                        .fill_max_width()
                        .padding_values(PaddingValues {
                            left: 12.0,
                            right: 8.0,
                            top: 2.0,
                            bottom: 6.0,
                        })
                        .gap(8.0)
                        .align_items(AlignItems::CENTER))
                    .child((
                        Text("Frame rate")
                            .size(th.typography.body_medium)
                            .color(th.on_surface)
                            .modifier(Modifier::new().width(96.0)),
                        Box(Modifier::new().width(84.0)).child(crate::components::AppTextField(
                            format!("comp_fps_{comp_id:?}"),
                            rate.num.to_string(),
                            "fps",
                            true,
                            32.0,
                            {
                                let session = session.clone();
                                move |text: String| {
                                    if let Ok(num) = text.trim().parse::<u32>() {
                                        let mut s = session.borrow_mut();
                                        s.apply_outputs(smallvec![
                                            ToolOutput::BeginTransaction("Set frame rate".into()),
                                            ToolOutput::Commands(smallvec![
                                                EditorCommand::SetCompositionRate {
                                                    comp: comp_id,
                                                    rate: renamite_animation::FrameRate {
                                                        num: num.max(1),
                                                        den: 1,
                                                    },
                                                }
                                            ]),
                                            ToolOutput::CommitTransaction,
                                        ]);
                                    }
                                }
                            },
                        )),
                        Text(format!("/{} fps", rate.den))
                            .size(th.typography.label_medium)
                            .color(th.on_surface_variant),
                    )),
                    // Range: editable In / Out
                    Row(Modifier::new()
                        .fill_max_width()
                        .padding_values(PaddingValues {
                            left: 12.0,
                            right: 8.0,
                            top: 4.0,
                            bottom: 4.0,
                        })
                        .gap(8.0)
                        .align_items(AlignItems::CENTER))
                    .child((
                        Text("Range")
                            .size(th.typography.body_medium)
                            .color(th.on_surface)
                            .modifier(Modifier::new().width(96.0)),
                        Box(Modifier::new().width(84.0)).child(crate::components::AppTextField(
                            format!("comp_in_{comp_id:?}"),
                            range.0.0.to_string(),
                            "In",
                            true,
                            32.0,
                            {
                                let session = session.clone();
                                move |text: String| {
                                    if let Ok(v) = text.trim().parse::<i64>() {
                                        let mut s = session.borrow_mut();
                                        s.apply_outputs(smallvec![
                                            ToolOutput::BeginTransaction(
                                                "Set composition range".into()
                                            ),
                                            ToolOutput::Commands(smallvec![
                                                EditorCommand::SetCompositionRange {
                                                    comp: comp_id,
                                                    start: Some(Frame(v)),
                                                    end: None,
                                                }
                                            ]),
                                            ToolOutput::CommitTransaction,
                                        ]);
                                    }
                                }
                            },
                        )),
                        Text("–")
                            .size(th.typography.body_medium)
                            .color(th.on_surface_variant),
                        Box(Modifier::new().width(84.0)).child(crate::components::AppTextField(
                            format!("comp_out_{comp_id:?}"),
                            range.1.0.to_string(),
                            "Out",
                            true,
                            32.0,
                            {
                                let session = session.clone();
                                move |text: String| {
                                    if let Ok(v) = text.trim().parse::<i64>() {
                                        let mut s = session.borrow_mut();
                                        s.apply_outputs(smallvec![
                                            ToolOutput::BeginTransaction(
                                                "Set composition range".into()
                                            ),
                                            ToolOutput::Commands(smallvec![
                                                EditorCommand::SetCompositionRange {
                                                    comp: comp_id,
                                                    start: None,
                                                    end: Some(Frame(v)),
                                                }
                                            ]),
                                            ToolOutput::CommitTransaction,
                                        ]);
                                    }
                                }
                            },
                        )),
                    )),
                    Row(Modifier::new()
                        .fill_max_width()
                        .padding_values(PaddingValues {
                            left: 12.0,
                            right: 12.0,
                            top: 2.0,
                            bottom: 8.0,
                        })
                        .gap(8.0))
                    .child((
                        Box(Modifier::new().width(96.0)),
                        Text(format!("{duration} frames"))
                            .size(th.typography.label_medium)
                            .color(th.on_surface_variant),
                    )),
                )),
            )
        };
        return Column(Modifier::new().fill_max_size()).child((
            PanelHeader(Symbols::settings, "Properties", vec![]),
            Text("No selection")
                .size(th.typography.body_medium)
                .color(th.on_surface_variant)
                .modifier(Modifier::new().padding(16.0)),
            Text("Select a layer on the canvas or in Layers to edit its properties.")
                .size(th.typography.body_small)
                .color(th.on_surface_variant)
                .modifier(Modifier::new().padding_values(repose_core::PaddingValues {
                    left: 16.0,
                    right: 16.0,
                    top: 0.0,
                    bottom: 12.0,
                })),
            comp_section,
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

    let mut header_actions: Vec<View> = vec![crate::CompactSwatchButton(session.clone())];
    if !diamond_quiet {
        header_actions.push(CompactIconAction(
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
        ));
    }
    let mut children: Vec<View> = vec![PanelHeader(Symbols::settings, title, header_actions)];
    if ids.len() == 1 {
        if let Some(v) = identity_section(session.clone(), ids[0]) {
            children.push(v);
        }
    }
    // Use effective inspect id for shape/text appearance and modifier routing
    let inspect_id_opt = if ids.len() == 1 {
        Some(inspect_ids[0])
    } else {
        None
    };
    if let Some(inspect_id) = inspect_id_opt {
        let app_opt = {
            let s = session.borrow();
            appearance_for(&s, inspect_id)
        };
        if let Some(app) = app_opt {
            let selected_is_fill = app.fill == Some(inspect_id);
            let selected_is_stroke = app.stroke == Some(inspect_id);
            let is_shape_like = !selected_is_fill && !selected_is_stroke;
            if let Some(fill_id) = app.fill {
                if let Some(v) = paint_section_for_style(
                    session.clone(),
                    app.shape_for_axis,
                    fill_id,
                    playhead,
                    record,
                ) {
                    children.push(v);
                }
                // Always show fill structural props (rule), whether shape or Fill node is selected.
                if let Some(v) = style_prop_rows(
                    session.clone(),
                    fill_id,
                    playhead,
                    record,
                    diamond_quiet,
                    "Fill",
                ) {
                    children.push(v);
                }
            }
            if let Some(stroke_id) = app.stroke {
                if let Some(v) = paint_section_for_style(
                    session.clone(),
                    app.shape_for_axis,
                    stroke_id,
                    playhead,
                    record,
                ) {
                    children.push(v);
                }
                // Always show stroke structural props (width/cap/join).
                if let Some(v) = style_prop_rows(
                    session.clone(),
                    stroke_id,
                    playhead,
                    record,
                    diamond_quiet,
                    "Stroke",
                ) {
                    children.push(v);
                }
                if let Some(v) =
                    stroke_dash_section(session.clone(), stroke_id, playhead, record, diamond_quiet)
                {
                    children.push(v);
                }
            }
            if is_shape_like {
                let mut chips: Vec<View> = Vec::new();
                match app.fill {
                    None => chips.push(style_action_chip(
                        session.clone(),
                        inspect_id,
                        StyleAction::Add(StyleAdd::Fill),
                    )),
                    Some(_) => chips.push(style_action_chip(
                        session.clone(),
                        inspect_id,
                        StyleAction::Remove(StyleAdd::Fill),
                    )),
                }
                match app.stroke {
                    None => chips.push(style_action_chip(
                        session.clone(),
                        inspect_id,
                        StyleAction::Add(StyleAdd::Stroke),
                    )),
                    Some(_) => chips.push(style_action_chip(
                        session.clone(),
                        inspect_id,
                        StyleAction::Remove(StyleAdd::Stroke),
                    )),
                }
                children.push(crate::components::CollapsibleSection(
                    "add_style_section",
                    "Appearance",
                    vec![],
                    FlowRow(
                        Modifier::new()
                            .fill_max_width()
                            .padding_values(PaddingValues {
                                left: 12.0,
                                right: 12.0,
                                top: 8.0,
                                bottom: 8.0,
                            })
                            .gap(8.0),
                    )
                    .child(chips),
                ));
            }
            // When selection itself is a style node and appearance returned only one side,
            // the other side's dash is handled above. For shape-like selection where dash
            // is missing (no stroke), nothing to show.
        } else if let Some(v) = paint_section(session.clone(), &[inspect_id], playhead, record) {
            children.push(v);
            if let Some(section) =
                stroke_dash_section(session.clone(), inspect_id, playhead, record, diamond_quiet)
            {
                children.push(section);
            }
        } else if let Some(section) =
            stroke_dash_section(session.clone(), inspect_id, playhead, record, diamond_quiet)
        {
            children.push(section);
        }
    }

    let showed_text_section = if let Some(inspect_id) = inspect_id_opt
        && let Some(section) = text_section(session.clone(), inspect_id)
    {
        children.push(section);
        true
    } else {
        false
    };

    // Single selected image: informational metadata (name, dimensions, MIME).
    if let Some(inspect_id) = inspect_id_opt
        && let Some(section) = image_meta_section(session.clone(), inspect_id)
    {
        children.push(section);
    }

    if let Some(inspect_id) = inspect_id_opt {
        if let Some(v) = layer_section(session.clone(), inspect_id) {
            children.push(v);
        }
        if let Some(v) = precomp_section(session.clone(), inspect_id) {
            children.push(v);
        }
        // Also show layer/precomp for outer group id if inspect unwrapped
        if inspect_id != ids[0] {
            if let Some(v) = layer_section(session.clone(), ids[0]) {
                children.push(v);
            }
            if let Some(v) = precomp_section(session.clone(), ids[0]) {
                children.push(v);
            }
        }
    }

    for (section, props) in sections {
        // Skip duplicate Fill/Stroke generic sections when paint already shown via style_prop_rows,
        // and skip Text when the dedicated text_section already covers size/align.
        let skip_duplicate = if let Some(id) = inspect_id_opt {
            match section {
                "Fill" | "Stroke" => {
                    let s = session.borrow();
                    appearance_for(&s, id).is_some()
                }
                "Text" => showed_text_section,
                _ => false,
            }
        } else {
            false
        };
        if skip_duplicate {
            continue;
        }
        let target_ids = if ids.len() == 1 {
            inspect_ids.clone()
        } else {
            ids.clone()
        };
        children.push(crate::components::CollapsibleSection(
            format!("props_section_{section}"),
            section,
            vec![],
            Column(Modifier::new().fill_max_width()).child(
                props
                    .iter()
                    .map(|prop| {
                        PropRowView(
                            session.clone(),
                            target_ids.clone(),
                            prop.clone(),
                            playhead,
                            record,
                            diamond_quiet,
                        )
                    })
                    .collect::<Vec<_>>(),
            ),
        ));
    }

    if let Some(inspect_id) = inspect_id_opt
        && let Some(v) = add_modifier_row(session.clone(), inspect_id)
    {
        children.push(v);
    }

    let header = children.remove(0);
    Column(Modifier::new().fill_max_size()).child((
        header,
        ScrollArea(
            Modifier::new().fill_max_size(),
            remember_scroll_state("properties_scroll"),
            Column(Modifier::new().fill_max_width()).child(children),
        ),
    ))
}

fn PropRowView(
    session: SessionRef,
    ids: Vec<NodeId>,
    row: PropRow,
    playhead: Frame,
    record: bool,
    diamond_quiet: bool,
) -> View {
    let th = theme();
    let path = row.desc.path.clone();

    // Enum fields aren't Animated<T>: render a dedicated toggle row
    // instead of the generic scrub + diamond layout.
    if let PropKind::Enum2 { a_label, b_label } = &row.desc.kind
        && let Value::I64(v) = &row.value
    {
        return enum2_row(
            session,
            ids,
            path.clone(),
            row.desc.label,
            *v as usize,
            a_label,
            b_label,
        );
    }
    if let PropKind::Enum3 { labels } = &row.desc.kind
        && let Value::I64(v) = &row.value
    {
        return enum3_row(
            session,
            ids,
            path.clone(),
            row.desc.label,
            *v as usize,
            *labels,
        );
    }

    // Bool fields aren't Animated<T> either (e.g. `mask.inverted`).
    if let PropKind::Bool = &row.desc.kind
        && let Value::Bool(v) = &row.value
    {
        return bool_toggle_row(session, ids, path.clone(), row.desc.label, *v);
    }

    Row(Modifier::new()
        .min_height(36.0)
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
            diamond_quiet,
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
///
/// In Design mode the diamonds go quiet: props without keys render a spacer
/// (no affordance) and already-keyed props render at 40% opacity so authoring
/// static art doesn't look like keyframing.
fn diamond_button(
    session: SessionRef,
    ids: Vec<NodeId>,
    path: PropPath,
    state: DiamondState,
    playhead: Frame,
    diamond_quiet: bool,
) -> View {
    if diamond_quiet && state == DiamondState::Empty {
        return Box(Modifier::new().width(32.0));
    }

    let (sym, tip) = match state {
        DiamondState::Empty => (Symbols::radio_button_unchecked, "Add keyframe"),
        DiamondState::HasKeys => (Symbols::radio_button_unchecked, "Add keyframe at playhead"),
        DiamondState::AtPlayhead => (Symbols::stop_circle, "Remove keyframe"),
    };
    let key = format!("diamond_{}", path.as_str());
    let tooltip_state = remember_with_key(key, TooltipState::new);
    let alpha = if diamond_quiet { 128 } else { 255 };
    let color = if state == DiamondState::AtPlayhead {
        theme().primary.with_alpha(alpha)
    } else {
        theme().on_surface_variant.with_alpha(alpha)
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

    // Click-to-type editing: a tap on the value opens a single-line input that
    // commits on Enter and reverts on Escape/focus-loss. Dragging still scrubs.
    let key = format!("numedit_{}_{}", path.as_str(), channel);
    let editing: Rc<RefCell<bool>> = remember_with_key(format!("{key}_ed"), || RefCell::new(false));
    let draft: Rc<RefCell<String>> =
        remember_with_key(format!("{key}_dr"), || RefCell::new(label.clone()));
    let focused_once: Rc<RefCell<bool>> =
        remember_with_key(format!("{key}_fo"), || RefCell::new(false));
    let focus: Rc<Cell<bool>> = remember_with_key(format!("{key}_fs"), || Cell::new(false));
    let focus_requester: Rc<FocusRequester> =
        remember_with_key(format!("{key}_fr"), FocusRequester::new);

    let commit = {
        let session = session.clone();
        let ids = ids.clone();
        let path = path.clone();
        let editing = editing.clone();
        move |text: String| {
            let parsed = text.trim().parse::<f64>().ok();
            let mut s = session.borrow_mut();
            if let Some(v) = parsed {
                let origin = current_value(&s, &ids, &path, channel);
                let new_v = apply_channel(&origin, channel, |_| {
                    let mut vv = v;
                    if let Some(lo) = min {
                        vv = vv.max(lo);
                    }
                    if let Some(hi) = max {
                        vv = vv.min(hi);
                    }
                    vv
                });
                let cmds =
                    apply_value_to_each(&s.file.document, &ids, &path, new_v, playhead, record);
                s.apply_outputs(smallvec![
                    ToolOutput::BeginTransaction("Edit property".into()),
                    ToolOutput::Commands(cmds.into()),
                    ToolOutput::CommitTransaction,
                ]);
            }
            drop(s);
            *editing.borrow_mut() = false;
            request_frame();
        }
    };

    if *editing.borrow() {
        if focus.get() {
            *focused_once.borrow_mut() = true;
        }
        // Focus was granted and then lost without a submit: revert to the
        // pre-edit value and close the editor.
        if *focused_once.borrow() && !focus.get() {
            *editing.borrow_mut() = false;
            request_frame();
            return Box(Modifier::new().min_width(min_width)).child(
                Text(label)
                    .size(th.typography.body_medium)
                    .color(th.primary),
            );
        }
        // Keep requesting focus until the runtime actually grants it (the
        // requester's target id is only set once this field has been laid out).
        if !focus.get() {
            focus_requester.request_focus();
        }
        return Box(Modifier::new().width(min_width)).child(
            View::new(0, ViewKind::Box).modifier(
                Modifier::new()
                    .focus_requester(focus_requester.as_ref().clone())
                    .text_input(TextInputConfig {
                        hint: String::new(),
                        multiline: false,
                        on_change: Some({
                            let draft = draft.clone();
                            Rc::new(move |text: String| {
                                *draft.borrow_mut() = text;
                            }) as Rc<dyn Fn(String)>
                        }),
                        on_submit: Some(Rc::new(commit) as Rc<dyn Fn(String)>),
                        focus_tracker: Some(focus.clone()),
                        value: draft.borrow().clone(),
                        visual_transformation: None,
                        keyboard_type: KeyboardType::Decimal,
                        capitalization: KeyboardCapitalization::None,
                        ime_action: ImeAction::Done,
                        auto_correct_enabled: Some(false),
                        enabled: true,
                        read_only: false,
                        max_lines: Some(1),
                        min_lines: 1,
                        cursor_color: Some(th.primary),
                        on_text_layout: None,
                        text_style: Some(repose_core::TextStyle {
                            font_size: 14.0,
                            color: Some(th.primary),
                            ..Default::default()
                        }),
                        keyboard_actions: None,
                        interaction_source: None,
                        line_limits: Some(TextFieldLineLimits::SingleLine),
                    }),
            ),
        );
    }

    Text(label.clone())
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
                    let editing = editing.clone();
                    let draft = draft.clone();
                    let focused_once = focused_once.clone();
                    let label = label.clone();
                    move |_pe: PointerEvent| {
                        let mut s = session.borrow_mut();
                        let was_drag = s.inspector_drag.take().map(|d| d.txn).unwrap_or(false);
                        if was_drag {
                            s.apply_outputs(smallvec![ToolOutput::CommitTransaction]);
                        }
                        drop(s);
                        // A tap (no scrub) opens the exact-value editor.
                        if !was_drag {
                            *focused_once.borrow_mut() = false;
                            *draft.borrow_mut() = label.clone();
                            *editing.borrow_mut() = true;
                            request_frame();
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
    let th = theme();
    // The swatch is the primary affordance; RGBA channel scrubbing is tucked
    // behind a disclosure so the row reads "Color [swatch] #FFCC00 100%".
    let open: Rc<RefCell<bool>> = remember_with_key(format!("colorrow_{}", path.as_str()), || {
        RefCell::new(false)
    });
    let hex = format!(
        "#{:02X}{:02X}{:02X}",
        (c.r * 255.0) as u8,
        (c.g * 255.0) as u8,
        (c.b * 255.0) as u8
    );
    let alpha_pct = (c.a * 100.0).round() as i64;

    // When a single style node is selected, the swatch opens the color picker.
    let picker_target = if ids.len() == 1 {
        match &session
            .borrow()
            .file
            .document
            .nodes
            .get(ids[0])
            .map(|n| &n.kind)
        {
            Some(NodeKind::Style(StyleKind::Fill { .. }))
            | Some(NodeKind::Style(StyleKind::Stroke { .. })) => {
                Some(PickerTarget::StyleColor { style_id: ids[0] })
            }
            _ => None,
        }
    } else {
        None
    };

    let swatch = Box(Modifier::new()
        .width(20.0)
        .height(20.0)
        .clip_rounded(4.0)
        .border(1.0, th.outline_variant, 4.0)
        .background(repose_core::Color(
            (c.r * 255.0) as u8,
            (c.g * 255.0) as u8,
            (c.b * 255.0) as u8,
            (c.a * 255.0) as u8,
        ))
        .on_pointer_down({
            let session = session.clone();
            let color = c;
            move |pe: PointerEvent| {
                if let Some(target) = picker_target {
                    let anchor = overlay_anchor(&pe);
                    session
                        .borrow_mut()
                        .open_color_picker(target, color, anchor);
                }
            }
        }));

    let is_open = *open.borrow();
    let summary = Row(Modifier::new().gap(6.0).align_items(AlignItems::CENTER)).child((
        swatch,
        Text(hex)
            .size(th.typography.body_medium)
            .color(th.on_surface),
        Text(format!("{alpha_pct}%"))
            .size(th.typography.body_medium)
            .color(th.on_surface_variant),
        Box(Modifier::new().flex_grow(1.0)),
        Text("RGBA")
            .size(th.typography.label_medium)
            .color(if is_open {
                th.primary
            } else {
                th.on_surface_variant
            })
            .modifier(
                Modifier::new()
                    .padding_values(PaddingValues {
                        left: 8.0,
                        right: 8.0,
                        top: 3.0,
                        bottom: 3.0,
                    })
                    .background(if is_open {
                        th.secondary_container
                    } else {
                        th.surface
                    })
                    .clip_rounded(999.0)
                    .on_pointer_down({
                        let open = open.clone();
                        move |_pe: PointerEvent| {
                            let next = !*open.borrow();
                            *open.borrow_mut() = next;
                            request_frame();
                        }
                    }),
            ),
    ));

    let mut children: Vec<View> = vec![summary];
    if is_open {
        children.push(
            Row(Modifier::new().gap(4.0).align_items(AlignItems::CENTER)).child((
                Text("R")
                    .size(th.typography.label_medium)
                    .color(th.on_surface_variant),
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
                    44.0,
                ),
                Text("G")
                    .size(th.typography.label_medium)
                    .color(th.on_surface_variant),
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
                    44.0,
                ),
                Text("B")
                    .size(th.typography.label_medium)
                    .color(th.on_surface_variant),
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
                    44.0,
                ),
                Text("A")
                    .size(th.typography.label_medium)
                    .color(th.on_surface_variant),
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
                    44.0,
                ),
            )),
        );
    }
    Column(Modifier::new().fill_max_width()).child(children)
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
    let shape_id = {
        let s = session.borrow();
        let doc = &s.file.document;
        match &doc.nodes.get(id)?.kind {
            NodeKind::Shape(_) => id,
            NodeKind::Group | NodeKind::Layer(_) => {
                let c = primary_content_in_group(doc, id)?;
                match doc.nodes.get(c).map(|n| &n.kind) {
                    Some(NodeKind::Shape(_)) => c,
                    _ => return None,
                }
            }
            _ => return None,
        }
    };
    let can_round = {
        let s = session.borrow();
        matches!(
            &s.file.document.nodes.get(shape_id)?.kind,
            NodeKind::Shape(ShapeKind::Rect { .. } | ShapeKind::Path(_))
        )
    };
    let id = shape_id;

    let mut buttons: Vec<View> = Vec::new();
    buttons.push(modifier_chip("Trim Path", {
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
    }));
    buttons.push(modifier_chip("Offset Path", {
        let session = session.clone();
        move || {
            let mut s = session.borrow_mut();
            if let Some(cmd) = cmd_add_offset_path_after(&s.file.document, id, 10.0) {
                s.apply_outputs(smallvec![
                    ToolOutput::BeginTransaction("Add Offset Path".into()),
                    ToolOutput::Commands(smallvec![cmd]),
                    ToolOutput::CommitTransaction,
                ]);
            }
        }
    }));
    if can_round {
        buttons.push(modifier_chip("Round Corners", {
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
        }));
    }
    buttons.push(modifier_chip("Repeater", {
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
    }));
    buttons.push(modifier_chip("Zig Zag", {
        let session = session.clone();
        move || {
            let mut s = session.borrow_mut();
            if let Some(cmd) = cmd_add_zigzag_after(&s.file.document, id) {
                s.apply_outputs(smallvec![
                    ToolOutput::BeginTransaction("Add Zig Zag".into()),
                    ToolOutput::Commands(smallvec![cmd]),
                    ToolOutput::CommitTransaction,
                ]);
            }
        }
    }));
    buttons.push(modifier_chip("Pucker & Bloat", {
        let session = session.clone();
        move || {
            let mut s = session.borrow_mut();
            if let Some(cmd) = cmd_add_pucker_bloat_after(&s.file.document, id, 50.0) {
                s.apply_outputs(smallvec![
                    ToolOutput::BeginTransaction("Add Pucker & Bloat".into()),
                    ToolOutput::Commands(smallvec![cmd]),
                    ToolOutput::CommitTransaction,
                ]);
            }
        }
    }));

    Some(crate::components::CollapsibleSection(
        "add_modifier_section",
        "Modifiers",
        vec![],
        FlowRow(
            Modifier::new()
                .fill_max_width()
                .padding_values(PaddingValues {
                    left: 12.0,
                    right: 12.0,
                    top: 8.0,
                    bottom: 8.0,
                })
                .gap(8.0),
        )
        .child(buttons),
    ))
}

fn modifier_chip(label: &'static str, on_click: impl Fn() + 'static) -> View {
    let th = theme();
    Box(Modifier::new()
        .padding_values(PaddingValues {
            left: 10.0,
            right: 10.0,
            top: 6.0,
            bottom: 6.0,
        })
        .background(th.surface_container_high)
        .clip_rounded(999.0)
        .border(1.0, th.outline_variant.with_alpha(140), 999.0)
        .on_pointer_down(move |_| on_click()))
    .child(
        Row(Modifier::new().gap(6.0).align_items(AlignItems::CENTER)).child((
            AppIcon(Symbols::add, 18.0),
            Text(label).size(th.typography.label_medium),
        )),
    )
}

enum StyleAdd {
    Fill,
    Stroke,
}

enum StyleAction {
    Add(StyleAdd),
    Remove(StyleAdd),
}

fn style_action_chip(session: SessionRef, shape_id: NodeId, action: StyleAction) -> View {
    let (label, icon) = match action {
        StyleAction::Add(StyleAdd::Fill) => ("Add fill", Symbols::add),
        StyleAction::Add(StyleAdd::Stroke) => ("Add stroke", Symbols::add),
        StyleAction::Remove(StyleAdd::Fill) => ("Remove fill", Symbols::delete),
        StyleAction::Remove(StyleAdd::Stroke) => ("Remove stroke", Symbols::delete),
    };
    let th = theme();
    Box(Modifier::new()
        .padding_values(PaddingValues {
            left: 10.0,
            right: 10.0,
            top: 6.0,
            bottom: 6.0,
        })
        .background(th.surface_container_high)
        .clip_rounded(999.0)
        .border(1.0, th.outline_variant.with_alpha(140), 999.0)
        .on_pointer_down({
            let session = session.clone();
            move |_| {
                let mut s = session.borrow_mut();
                let doc = &s.file.document;
                let (cmd, tx_label) = match action {
                    StyleAction::Add(StyleAdd::Fill) => {
                        let paint = s.current_paint.snapshot(s.playback.head);
                        (
                            renamite_behavior_common::fill::cmd_add_fill_after(
                                doc, shape_id, paint,
                            ),
                            "Add fill",
                        )
                    }
                    StyleAction::Add(StyleAdd::Stroke) => (
                        renamite_behavior_common::stroke::cmd_add_stroke_after(
                            doc,
                            shape_id,
                            s.current_paint.snapshot(s.playback.head),
                            4.0,
                        ),
                        "Add stroke",
                    ),
                    StyleAction::Remove(StyleAdd::Fill) => (
                        renamite_behavior_common::fill::cmd_remove_fill_for_shape(doc, shape_id),
                        "Remove fill",
                    ),
                    StyleAction::Remove(StyleAdd::Stroke) => (
                        renamite_behavior_common::stroke::cmd_remove_stroke_for_shape(
                            doc, shape_id,
                        ),
                        "Remove stroke",
                    ),
                };
                if let Some(cmd) = cmd {
                    s.apply_outputs(smallvec![
                        ToolOutput::BeginTransaction(tx_label.into()),
                        ToolOutput::Commands(smallvec![cmd]),
                        ToolOutput::CommitTransaction,
                    ]);
                }
            }
        }))
    .child(
        Row(Modifier::new().gap(6.0).align_items(AlignItems::CENTER)).child((
            AppIcon(icon, 18.0),
            Text(label).size(th.typography.label_medium),
        )),
    )
}

/// Two-segment toggle for `PropKind::Enum2` (diamond-less, non-animatable).
fn enum2_row(
    session: SessionRef,
    ids: Vec<NodeId>,
    path: PropPath,
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
        enum2_segment(
            session.clone(),
            ids.clone(),
            path.clone(),
            current,
            0,
            a_label,
        ),
        enum2_segment(session, ids, path, current, 1, b_label),
    ))
}

fn enum3_row(
    session: SessionRef,
    ids: Vec<NodeId>,
    path: PropPath,
    label: &'static str,
    current: usize,
    labels: [&'static str; 3],
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
        enum3_segment(
            session.clone(),
            ids.clone(),
            path.clone(),
            current,
            0,
            labels[0],
        ),
        enum3_segment(
            session.clone(),
            ids.clone(),
            path.clone(),
            current,
            1,
            labels[1],
        ),
        enum3_segment(session, ids, path, current, 2, labels[2]),
    ))
}

fn bool_toggle_row(
    session: SessionRef,
    ids: Vec<NodeId>,
    path: PropPath,
    label: &'static str,
    value: bool,
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
        bool_toggle_segment(
            session.clone(),
            ids.clone(),
            path.clone(),
            value,
            false,
            label,
        ),
        bool_toggle_segment(session, ids, path, value, true, label),
    ))
}

fn bool_toggle_segment(
    session: SessionRef,
    ids: Vec<NodeId>,
    path: PropPath,
    current: bool,
    value: bool,
    row_label: &'static str,
) -> View {
    let th = theme();
    let active = current == value;
    // Use specific labels for known bools
    let label = match path.as_str() {
        "mask.inverted" => {
            if value {
                "Inverted"
            } else {
                "Normal"
            }
        }
        "zigzag.smooth" => {
            if value {
                "Smooth"
            } else {
                "Corner"
            }
        }
        _ => {
            if value {
                "On"
            } else {
                "Off"
            }
        }
    };
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
                    let path = path.clone();
                    move |_pe: PointerEvent| {
                        let mut s = session.borrow_mut();
                        let cmds: Vec<_> = ids
                            .iter()
                            .copied()
                            .filter_map(|id| {
                                renamite_behavior_common::inspect::cmd_set_discrete(
                                    &s.file.document,
                                    id,
                                    &path,
                                    if value { 1 } else { 0 },
                                )
                            })
                            .collect();
                        if cmds.is_empty() {
                            return;
                        }
                        let tx = format!("Toggle {row_label}");
                        s.apply_outputs(smallvec![
                            ToolOutput::BeginTransaction(tx),
                            ToolOutput::Commands(cmds.into()),
                            ToolOutput::CommitTransaction,
                        ]);
                    }
                }),
        )
}

fn enum2_segment(
    session: SessionRef,
    ids: Vec<NodeId>,
    path: PropPath,
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
                    let path = path.clone();
                    move |_pe: PointerEvent| {
                        let mut s = session.borrow_mut();
                        let cmds: Vec<EditorCommand> = ids
                            .iter()
                            .copied()
                            .filter_map(|id| {
                                renamite_behavior_common::inspect::cmd_set_discrete(
                                    &s.file.document,
                                    id,
                                    &path,
                                    index as i64,
                                )
                            })
                            .collect();
                        if cmds.is_empty() {
                            return;
                        }
                        let tx = format!("Set {}", path.as_str());
                        s.apply_outputs(smallvec![
                            ToolOutput::BeginTransaction(tx),
                            ToolOutput::Commands(cmds.into()),
                            ToolOutput::CommitTransaction,
                        ]);
                    }
                }),
        )
}

fn enum3_segment(
    session: SessionRef,
    ids: Vec<NodeId>,
    path: PropPath,
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
                    let path = path.clone();
                    move |_pe: PointerEvent| {
                        let mut s = session.borrow_mut();
                        let cmds: Vec<EditorCommand> = ids
                            .iter()
                            .copied()
                            .filter_map(|id| {
                                renamite_behavior_common::inspect::cmd_set_discrete(
                                    &s.file.document,
                                    id,
                                    &path,
                                    index as i64,
                                )
                            })
                            .collect();
                        if cmds.is_empty() {
                            return;
                        }
                        let tx = format!("Set {}", path.as_str());
                        s.apply_outputs(smallvec![
                            ToolOutput::BeginTransaction(tx),
                            ToolOutput::Commands(cmds.into()),
                            ToolOutput::CommitTransaction,
                        ]);
                    }
                }),
        )
}

fn text_section(session: SessionRef, id: NodeId) -> Option<View> {
    let (text_id, content, font, fonts) = {
        let session = session.borrow();
        let doc = &session.file.document;
        match &doc.nodes.get(id)?.kind {
            NodeKind::Text(t) => (id, t.text.clone(), t.font.clone(), doc.font_families()),
            // The text tool wraps Text + Fill in a group, and the session
            // auto-selects the group it created; editing a group that holds
            // exactly one Text child edits that text.
            NodeKind::Group => {
                let children = doc.nodes.get(id)?.children.clone();
                let mut text_ids = children.into_iter().filter(|cid| {
                    matches!(
                        doc.nodes.get(*cid).map(|n| &n.kind),
                        Some(NodeKind::Text(_))
                    )
                });
                let text_id = text_ids.next()?;
                if text_ids.next().is_some() {
                    return None;
                }
                let NodeKind::Text(t) = &doc.nodes.get(text_id)?.kind else {
                    return None;
                };
                (text_id, t.text.clone(), t.font.clone(), doc.font_families())
            }
            _ => return None,
        }
    };
    let th = theme();

    // Font-family chips: "Default" (None) plus every embedded project font.
    let mut chips: Vec<View> = vec![font_chip(
        session.clone(),
        text_id,
        "Default".to_string(),
        None,
        font.is_none(),
    )];
    for family in fonts {
        let active = font.as_deref() == Some(family.as_str());
        chips.push(font_chip(
            session.clone(),
            text_id,
            family.clone(),
            Some(family),
            active,
        ));
    }

    // Size / Align rows are folded into the Text section so the generic
    // `props_for_selection` "Text" section can be skipped.
    let (size_align_rows, playhead, record, diamond_quiet) = {
        let s = session.borrow();
        let ph = Frame(s.playback.head.round() as i64);
        let rec = s.record;
        let quiet = s.mode == EditorMode::Design;
        let rows = props_for_node(&s.file.document, text_id, ph)
            .into_iter()
            .filter(|r| r.desc.section == "Text")
            .collect::<Vec<_>>();
        (rows, ph, rec, quiet)
    };

    Some(crate::components::CollapsibleSection(
        "text_section",
        "Text",
        vec![],
        Column(Modifier::new().fill_max_width()).child((
            Row(Modifier::new()
                .fill_max_width()
                .padding_values(PaddingValues {
                    left: 12.0,
                    right: 8.0,
                    top: 0.0,
                    bottom: 4.0,
                }))
            .child(crate::components::AppTextField(
                format!("text_content_{text_id:?}"),
                content,
                "Text content",
                false,
                96.0,
                {
                    let session = session.clone();
                    move |text: String| {
                        let mut s = session.borrow_mut();
                        s.apply_outputs(smallvec![
                            ToolOutput::BeginTransaction("Edit text".into()),
                            ToolOutput::Commands(smallvec![EditorCommand::SetTextContent {
                                id: text_id,
                                text
                            }]),
                            ToolOutput::CommitTransaction,
                        ]);
                    }
                },
            )),
            Text("Font")
                .size(th.typography.body_medium)
                .color(th.on_surface)
                .modifier(Modifier::new().padding_values(PaddingValues {
                    left: 12.0,
                    right: 8.0,
                    top: 8.0,
                    bottom: 4.0,
                })),
            Row(Modifier::new()
                .fill_max_width()
                .padding_values(PaddingValues {
                    left: 12.0,
                    right: 8.0,
                    top: 0.0,
                    bottom: 4.0,
                })
                .gap(6.0))
            .child(chips),
            Column(Modifier::new().fill_max_width()).child(
                size_align_rows
                    .iter()
                    .map(|prop| {
                        PropRowView(
                            session.clone(),
                            vec![text_id],
                            prop.clone(),
                            playhead,
                            record,
                            diamond_quiet,
                        )
                    })
                    .collect::<Vec<_>>(),
            ),
        )),
    ))
}

fn primary_content_in_group(doc: &renamite_model::Document, id: NodeId) -> Option<NodeId> {
    let node = doc.nodes.get(id)?;
    if !matches!(node.kind, NodeKind::Group | NodeKind::Layer(_)) {
        return None;
    }
    let mut content = node.children.iter().copied().filter(|&cid| {
        matches!(
            doc.nodes.get(cid).map(|n| &n.kind),
            Some(NodeKind::Shape(_) | NodeKind::Text(_))
        )
    });
    let first = content.next()?;
    if content.next().is_some() {
        return None;
    }
    Some(first)
}

fn effective_inspect_id(doc: &renamite_model::Document, id: NodeId) -> NodeId {
    primary_content_in_group(doc, id).unwrap_or(id)
}

fn identity_section(session: SessionRef, id: NodeId) -> Option<View> {
    let name = {
        let s = session.borrow();
        s.file.document.nodes.get(id)?.name.clone()
    };
    let th = theme();
    Some(crate::components::CollapsibleSection(
        format!("identity_{id:?}"),
        "Layer",
        vec![],
        Column(Modifier::new().fill_max_width()).child(
            Row(Modifier::new()
                .fill_max_width()
                .padding_values(PaddingValues {
                    left: 12.0,
                    right: 8.0,
                    top: 6.0,
                    bottom: 6.0,
                })
                .gap(8.0)
                .align_items(AlignItems::CENTER))
            .child((
                Text("Name")
                    .size(th.typography.body_medium)
                    .color(th.on_surface)
                    .modifier(Modifier::new().width(96.0)),
                Box(Modifier::new().width(176.0)).child(crate::components::AppTextField(
                    format!("node_name_{id:?}"),
                    name,
                    "Name",
                    false,
                    32.0,
                    {
                        let session = session.clone();
                        move |text: String| {
                            let name = text.trim().to_string();
                            if name.is_empty() {
                                return;
                            }
                            let mut s = session.borrow_mut();
                            if s.file
                                .document
                                .nodes
                                .get(id)
                                .map(|n| n.locked)
                                .unwrap_or(true)
                            {
                                return;
                            }
                            s.apply_outputs(smallvec![
                                ToolOutput::BeginTransaction("Rename".into()),
                                ToolOutput::Commands(smallvec![EditorCommand::SetNodeName {
                                    id,
                                    name,
                                }]),
                                ToolOutput::CommitTransaction,
                            ]);
                        }
                    },
                )),
            )),
        ),
    ))
}

/// One selectable font-family chip in the text properties section.
fn image_meta_section(session: SessionRef, id: NodeId) -> Option<View> {
    let (name, width, height, mime) = {
        let session = session.borrow();
        let doc = &session.file.document;
        let NodeKind::Image(img) = &doc.nodes.get(id)?.kind else {
            return None;
        };
        let image = doc.image_asset(img.asset())?;
        (
            image.name.clone(),
            image.width,
            image.height,
            image.mime.clone(),
        )
    };

    let crop = {
        let s = session.borrow();
        let doc = &s.file.document;
        let NodeKind::Image(img) = &doc.nodes.get(id)?.kind else {
            return None;
        };
        img.crop()
    };

    let th = theme();
    let mut children: Vec<View> = Vec::new();

    for (label, value) in [
        ("Name", name),
        ("Dimensions", format!("{width}×{height} px")),
        ("Type", mime),
    ] {
        children.push(
            Row(Modifier::new()
                .fill_max_width()
                .padding_values(PaddingValues {
                    left: 12.0,
                    right: 12.0,
                    top: 2.0,
                    bottom: 2.0,
                })
                .gap(8.0)
                .align_items(AlignItems::CENTER))
            .child((
                Text(label)
                    .size(th.typography.label_small)
                    .color(th.on_surface_variant),
                Text(value)
                    .size(th.typography.body_small)
                    .color(th.on_surface),
            )),
        );
    }

    children.push(
        Column(Modifier::new().fill_max_width()).child((
            Row(Modifier::new()
                .fill_max_width()
                .padding_values(PaddingValues {
                    left: 12.0,
                    right: 8.0,
                    top: 6.0,
                    bottom: 4.0,
                })
                .gap(6.0)
                .align_items(AlignItems::CENTER))
            .child((
                Text("Crop")
                    .size(th.typography.body_medium)
                    .color(th.on_surface)
                    .modifier(Modifier::new().width(48.0)),
                Text("X")
                    .size(th.typography.label_small)
                    .color(th.on_surface_variant),
                Box(Modifier::new().width(56.0)).child(crate::components::AppTextField(
                    format!("img_crop_x_{id:?}"),
                    format!("{:.3}", crop.x),
                    "X",
                    true,
                    32.0,
                    {
                        let session = session.clone();
                        move |text: String| {
                            if let Ok(v) = text.trim().parse::<f64>() {
                                let mut s = session.borrow_mut();
                                let Some(NodeKind::Image(img)) =
                                    s.file.document.nodes.get(id).map(|n| &n.kind)
                                else {
                                    return;
                                };
                                let mut new_crop = img.crop();
                                new_crop.x = v.clamp(0.0, 1.0);
                                s.apply_outputs(smallvec![
                                    ToolOutput::BeginTransaction("Set image crop".into()),
                                    ToolOutput::Commands(smallvec![EditorCommand::SetImageCrop {
                                        id,
                                        crop: new_crop
                                    }]),
                                    ToolOutput::CommitTransaction,
                                ]);
                            }
                        }
                    },
                )),
                Text("Y")
                    .size(th.typography.label_small)
                    .color(th.on_surface_variant),
                Box(Modifier::new().width(56.0)).child(crate::components::AppTextField(
                    format!("img_crop_y_{id:?}"),
                    format!("{:.3}", crop.y),
                    "Y",
                    true,
                    32.0,
                    {
                        let session = session.clone();
                        move |text: String| {
                            if let Ok(v) = text.trim().parse::<f64>() {
                                let mut s = session.borrow_mut();
                                let Some(NodeKind::Image(img)) =
                                    s.file.document.nodes.get(id).map(|n| &n.kind)
                                else {
                                    return;
                                };
                                let mut new_crop = img.crop();
                                new_crop.y = v.clamp(0.0, 1.0);
                                s.apply_outputs(smallvec![
                                    ToolOutput::BeginTransaction("Set image crop".into()),
                                    ToolOutput::Commands(smallvec![EditorCommand::SetImageCrop {
                                        id,
                                        crop: new_crop
                                    }]),
                                    ToolOutput::CommitTransaction,
                                ]);
                            }
                        }
                    },
                )),
            )),
            Row(Modifier::new()
                .fill_max_width()
                .padding_values(PaddingValues {
                    left: 12.0,
                    right: 8.0,
                    top: 0.0,
                    bottom: 4.0,
                })
                .gap(6.0)
                .align_items(AlignItems::CENTER))
            .child((
                Box(Modifier::new().width(48.0)),
                Text("W")
                    .size(th.typography.label_small)
                    .color(th.on_surface_variant),
                Box(Modifier::new().width(56.0)).child(crate::components::AppTextField(
                    format!("img_crop_w_{id:?}"),
                    format!("{:.3}", crop.z),
                    "W",
                    true,
                    32.0,
                    {
                        let session = session.clone();
                        move |text: String| {
                            if let Ok(v) = text.trim().parse::<f64>() {
                                let mut s = session.borrow_mut();
                                let Some(NodeKind::Image(img)) =
                                    s.file.document.nodes.get(id).map(|n| &n.kind)
                                else {
                                    return;
                                };
                                let mut new_crop = img.crop();
                                new_crop.z = v.clamp(0.05, 1.0);
                                s.apply_outputs(smallvec![
                                    ToolOutput::BeginTransaction("Set image crop".into()),
                                    ToolOutput::Commands(smallvec![EditorCommand::SetImageCrop {
                                        id,
                                        crop: new_crop
                                    }]),
                                    ToolOutput::CommitTransaction,
                                ]);
                            }
                        }
                    },
                )),
                Text("H")
                    .size(th.typography.label_small)
                    .color(th.on_surface_variant),
                Box(Modifier::new().width(56.0)).child(crate::components::AppTextField(
                    format!("img_crop_h_{id:?}"),
                    format!("{:.3}", crop.w),
                    "H",
                    true,
                    32.0,
                    {
                        let session = session.clone();
                        move |text: String| {
                            if let Ok(v) = text.trim().parse::<f64>() {
                                let mut s = session.borrow_mut();
                                let Some(NodeKind::Image(img)) =
                                    s.file.document.nodes.get(id).map(|n| &n.kind)
                                else {
                                    return;
                                };
                                let mut new_crop = img.crop();
                                new_crop.w = v.clamp(0.05, 1.0);
                                s.apply_outputs(smallvec![
                                    ToolOutput::BeginTransaction("Set image crop".into()),
                                    ToolOutput::Commands(smallvec![EditorCommand::SetImageCrop {
                                        id,
                                        crop: new_crop
                                    }]),
                                    ToolOutput::CommitTransaction,
                                ]);
                            }
                        }
                    },
                )),
            )),
        )),
    );

    Some(crate::components::CollapsibleSection(
        "image_meta_section",
        "Image Info",
        vec![],
        Column(Modifier::new().fill_max_width()).child(children),
    ))
}

fn insert_gradient_stop(stops: &mut GradientStops) {
    if stops.0.is_empty() {
        stops.0.push(GradientStop {
            offset: 0.0,
            color: Color::BLACK,
        });
        stops.0.push(GradientStop {
            offset: 1.0,
            color: Color::WHITE,
        });
        return;
    }
    let mut order: Vec<usize> = (0..stops.0.len()).collect();
    order.sort_by(|&a, &b| {
        stops.0[a]
            .offset
            .partial_cmp(&stops.0[b].offset)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut best_i = 0usize;
    let mut best_gap = -1.0f64;
    for w in order.windows(2) {
        let a = stops.0[w[0]].offset;
        let b = stops.0[w[1]].offset;
        let gap = b - a;
        if gap > best_gap {
            best_gap = gap;
            best_i = w[0];
        }
    }
    let (i0, i1) = if order.len() < 2 {
        (order[0], order[0])
    } else {
        let idx = order.iter().position(|&i| i == best_i).unwrap();
        (order[idx], order[idx + 1])
    };
    let a = &stops.0[i0];
    let b = &stops.0[i1];
    let t = 0.5;
    let color = Color {
        r: a.color.r + (b.color.r - a.color.r) * t,
        g: a.color.g + (b.color.g - a.color.g) * t,
        b: a.color.b + (b.color.b - a.color.b) * t,
        a: a.color.a + (b.color.a - a.color.a) * t,
    };
    let offset = if order.len() < 2 {
        0.5
    } else {
        a.offset + (b.offset - a.offset) * t
    };
    stops.0.push(GradientStop { offset, color });
    stops.0.sort_by(|x, y| {
        x.offset
            .partial_cmp(&y.offset)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

fn layer_section(session: SessionRef, id: NodeId) -> Option<View> {
    let (in_f, out_f, stretch, blend) = {
        let s = session.borrow();
        let NodeKind::Layer(lp) = &s.file.document.nodes.get(id)?.kind else {
            return None;
        };
        (lp.in_frame.0, lp.out_frame.0, lp.time_stretch, lp.blend)
    };
    let th = theme();
    let blend_idx = match blend {
        renamite_model::BlendMode::Normal => 0,
        renamite_model::BlendMode::Multiply => 1,
        renamite_model::BlendMode::Screen => 2,
        renamite_model::BlendMode::Overlay => 3,
        renamite_model::BlendMode::Darken => 4,
        renamite_model::BlendMode::Lighten => 5,
        renamite_model::BlendMode::ColorDodge => 6,
        renamite_model::BlendMode::ColorBurn => 7,
        renamite_model::BlendMode::HardLight => 8,
        renamite_model::BlendMode::SoftLight => 9,
        renamite_model::BlendMode::Difference => 10,
        renamite_model::BlendMode::Exclusion => 11,
        renamite_model::BlendMode::Hue => 12,
        renamite_model::BlendMode::Saturation => 13,
        renamite_model::BlendMode::Color => 14,
        renamite_model::BlendMode::Luminosity => 15,
    };
    Some(crate::components::CollapsibleSection(
        format!("layer_props_{id:?}"),
        "Layer",
        vec![],
        Column(Modifier::new().fill_max_width()).child((
            Row(Modifier::new()
                .fill_max_width()
                .padding_values(PaddingValues {
                    left: 12.0,
                    right: 8.0,
                    top: 6.0,
                    bottom: 4.0,
                })
                .gap(8.0)
                .align_items(AlignItems::CENTER))
            .child((
                Text("In")
                    .size(th.typography.body_medium)
                    .color(th.on_surface)
                    .modifier(Modifier::new().width(96.0)),
                Box(Modifier::new().width(84.0)).child(crate::components::AppTextField(
                    format!("layer_in_{id:?}"),
                    in_f.to_string(),
                    "In",
                    true,
                    32.0,
                    {
                        let session = session.clone();
                        move |text: String| {
                            if let Ok(v) = text.trim().parse::<i64>() {
                                let mut s = session.borrow_mut();
                                s.apply_outputs(smallvec![
                                    ToolOutput::BeginTransaction("Set layer in".into()),
                                    ToolOutput::Commands(smallvec![EditorCommand::SetLayerProps {
                                        id,
                                        in_frame: Some(Frame(v)),
                                        out_frame: None,
                                        time_stretch: None,
                                        blend: None,
                                    }]),
                                    ToolOutput::CommitTransaction,
                                ]);
                            }
                        }
                    },
                )),
            )),
            Row(Modifier::new()
                .fill_max_width()
                .padding_values(PaddingValues {
                    left: 12.0,
                    right: 8.0,
                    top: 4.0,
                    bottom: 4.0,
                })
                .gap(8.0)
                .align_items(AlignItems::CENTER))
            .child((
                Text("Out")
                    .size(th.typography.body_medium)
                    .color(th.on_surface)
                    .modifier(Modifier::new().width(96.0)),
                Box(Modifier::new().width(84.0)).child(crate::components::AppTextField(
                    format!("layer_out_{id:?}"),
                    out_f.to_string(),
                    "Out",
                    true,
                    32.0,
                    {
                        let session = session.clone();
                        move |text: String| {
                            if let Ok(v) = text.trim().parse::<i64>() {
                                let mut s = session.borrow_mut();
                                s.apply_outputs(smallvec![
                                    ToolOutput::BeginTransaction("Set layer out".into()),
                                    ToolOutput::Commands(smallvec![EditorCommand::SetLayerProps {
                                        id,
                                        in_frame: None,
                                        out_frame: Some(Frame(v)),
                                        time_stretch: None,
                                        blend: None,
                                    }]),
                                    ToolOutput::CommitTransaction,
                                ]);
                            }
                        }
                    },
                )),
            )),
            Row(Modifier::new()
                .fill_max_width()
                .padding_values(PaddingValues {
                    left: 12.0,
                    right: 8.0,
                    top: 4.0,
                    bottom: 4.0,
                })
                .gap(8.0)
                .align_items(AlignItems::CENTER))
            .child((
                Text("Time stretch")
                    .size(th.typography.body_medium)
                    .color(th.on_surface)
                    .modifier(Modifier::new().width(96.0)),
                Box(Modifier::new().width(84.0)).child(crate::components::AppTextField(
                    format!("layer_stretch_{id:?}"),
                    stretch.to_string(),
                    "stretch",
                    true,
                    32.0,
                    {
                        let session = session.clone();
                        move |text: String| {
                            if let Ok(v) = text.trim().parse::<f64>() {
                                let mut s = session.borrow_mut();
                                s.apply_outputs(smallvec![
                                    ToolOutput::BeginTransaction("Set layer stretch".into()),
                                    ToolOutput::Commands(smallvec![EditorCommand::SetLayerProps {
                                        id,
                                        in_frame: None,
                                        out_frame: None,
                                        time_stretch: Some(v.max(1e-6)),
                                        blend: None,
                                    }]),
                                    ToolOutput::CommitTransaction,
                                ]);
                            }
                        }
                    },
                )),
            )),
            FlowRow(
                Modifier::new()
                    .fill_max_width()
                    .padding_values(PaddingValues {
                        left: 12.0,
                        right: 8.0,
                        top: 4.0,
                        bottom: 8.0,
                    })
                    .gap(6.0),
            )
            .child({
                let labels: [&str; 16] = [
                    "Normal",
                    "Multiply",
                    "Screen",
                    "Overlay",
                    "Darken",
                    "Lighten",
                    "ColorDodge",
                    "ColorBurn",
                    "HardLight",
                    "SoftLight",
                    "Difference",
                    "Exclusion",
                    "Hue",
                    "Saturation",
                    "Color",
                    "Luminosity",
                ];
                let mut chips: Vec<View> = Vec::with_capacity(17);
                chips.push(
                    Text("Blend")
                        .size(th.typography.body_medium)
                        .color(th.on_surface)
                        .modifier(Modifier::new().width(96.0)),
                );
                for (idx, label) in labels.iter().enumerate() {
                    chips.push(blend_segment(session.clone(), id, blend_idx, idx, label));
                }
                chips
            }),
        )),
    ))
}

fn blend_segment(
    session: SessionRef,
    id: NodeId,
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
                .on_pointer_down(move |_pe: PointerEvent| {
                    let blend = match index {
                        1 => renamite_model::BlendMode::Multiply,
                        2 => renamite_model::BlendMode::Screen,
                        3 => renamite_model::BlendMode::Overlay,
                        4 => renamite_model::BlendMode::Darken,
                        5 => renamite_model::BlendMode::Lighten,
                        6 => renamite_model::BlendMode::ColorDodge,
                        7 => renamite_model::BlendMode::ColorBurn,
                        8 => renamite_model::BlendMode::HardLight,
                        9 => renamite_model::BlendMode::SoftLight,
                        10 => renamite_model::BlendMode::Difference,
                        11 => renamite_model::BlendMode::Exclusion,
                        12 => renamite_model::BlendMode::Hue,
                        13 => renamite_model::BlendMode::Saturation,
                        14 => renamite_model::BlendMode::Color,
                        15 => renamite_model::BlendMode::Luminosity,
                        _ => renamite_model::BlendMode::Normal,
                    };
                    let mut s = session.borrow_mut();
                    s.apply_outputs(smallvec![
                        ToolOutput::BeginTransaction("Set blend".into()),
                        ToolOutput::Commands(smallvec![EditorCommand::SetLayerProps {
                            id,
                            in_frame: None,
                            out_frame: None,
                            time_stretch: None,
                            blend: Some(blend),
                        }]),
                        ToolOutput::CommitTransaction,
                    ]);
                }),
        )
}

fn precomp_section(session: SessionRef, id: NodeId) -> Option<View> {
    let (offset, stretch, current_comp, comps) = {
        let s = session.borrow();
        let NodeKind::Precomp { comp, time_map } = &s.file.document.nodes.get(id)?.kind else {
            return None;
        };
        let comps: Vec<(renamite_model::CompId, String)> = s
            .file
            .document
            .compositions
            .iter()
            .map(|(cid, comp)| (cid, comp.name.clone()))
            .collect();
        (time_map.offset.0, time_map.stretch, *comp, comps)
    };
    let th = theme();
    Some(crate::components::CollapsibleSection(
        format!("precomp_props_{id:?}"),
        "Precomp",
        vec![],
        Column(Modifier::new().fill_max_width()).child((
            Row(Modifier::new()
                .fill_max_width()
                .padding_values(PaddingValues {
                    left: 12.0,
                    right: 8.0,
                    top: 6.0,
                    bottom: 4.0,
                })
                .gap(8.0)
                .align_items(AlignItems::CENTER))
            .child((
                Text("Time offset")
                    .size(th.typography.body_medium)
                    .color(th.on_surface)
                    .modifier(Modifier::new().width(96.0)),
                Box(Modifier::new().width(84.0)).child(crate::components::AppTextField(
                    format!("precomp_off_{id:?}"),
                    offset.to_string(),
                    "offset",
                    true,
                    32.0,
                    {
                        let session = session.clone();
                        move |text: String| {
                            if let Ok(v) = text.trim().parse::<i64>() {
                                let mut s = session.borrow_mut();
                                s.apply_outputs(smallvec![
                                    ToolOutput::BeginTransaction("Set precomp offset".into()),
                                    ToolOutput::Commands(smallvec![
                                        EditorCommand::SetPrecompTimeMap {
                                            id,
                                            offset: Some(Frame(v)),
                                            stretch: None,
                                        }
                                    ]),
                                    ToolOutput::CommitTransaction,
                                ]);
                            }
                        }
                    },
                )),
            )),
            Row(Modifier::new()
                .fill_max_width()
                .padding_values(PaddingValues {
                    left: 12.0,
                    right: 8.0,
                    top: 4.0,
                    bottom: 8.0,
                })
                .gap(8.0)
                .align_items(AlignItems::CENTER))
            .child((
                Text("Time stretch")
                    .size(th.typography.body_medium)
                    .color(th.on_surface)
                    .modifier(Modifier::new().width(96.0)),
                Box(Modifier::new().width(84.0)).child(crate::components::AppTextField(
                    format!("precomp_st_{id:?}"),
                    stretch.to_string(),
                    "stretch",
                    true,
                    32.0,
                    {
                        let session = session.clone();
                        move |text: String| {
                            if let Ok(v) = text.trim().parse::<f64>() {
                                let mut s = session.borrow_mut();
                                s.apply_outputs(smallvec![
                                    ToolOutput::BeginTransaction("Set precomp stretch".into()),
                                    ToolOutput::Commands(smallvec![
                                        EditorCommand::SetPrecompTimeMap {
                                            id,
                                            offset: None,
                                            stretch: Some(v.max(1e-6)),
                                        }
                                    ]),
                                    ToolOutput::CommitTransaction,
                                ]);
                            }
                        }
                    },
                )),
            )),
            FlowRow(
                Modifier::new()
                    .fill_max_width()
                    .padding_values(PaddingValues {
                        left: 12.0,
                        right: 8.0,
                        top: 4.0,
                        bottom: 8.0,
                    })
                    .gap(6.0),
            )
            .child({
                let mut chips: Vec<View> = Vec::new();
                chips.push(
                    Text("Source")
                        .size(th.typography.body_medium)
                        .color(th.on_surface)
                        .modifier(Modifier::new().width(96.0)),
                );
                for (cid, name) in comps {
                    let active = cid == current_comp;
                    chips.push(precomp_comp_chip(
                        session.clone(),
                        id,
                        name.clone(),
                        cid,
                        active,
                    ));
                }
                chips
            }),
        )),
    ))
}

fn precomp_comp_chip(
    session: SessionRef,
    node_id: NodeId,
    label: String,
    comp: renamite_model::CompId,
    active: bool,
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
                    move |_| {
                        if active {
                            return;
                        }
                        let mut s = session.borrow_mut();
                        s.apply_outputs(smallvec![
                            ToolOutput::BeginTransaction("Set precomp source".into()),
                            ToolOutput::Commands(smallvec![EditorCommand::SetPrecompComp {
                                id: node_id,
                                comp,
                            }]),
                            ToolOutput::CommitTransaction,
                        ]);
                    }
                }),
        )
}

fn font_chip(
    session: SessionRef,
    text_id: NodeId,
    label: String,
    family: Option<String>,
    active: bool,
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
                    move |_| {
                        let mut s = session.borrow_mut();
                        s.apply_outputs(smallvec![
                            ToolOutput::BeginTransaction("Change font".into()),
                            ToolOutput::Commands(smallvec![EditorCommand::SetTextFont {
                                id: text_id,
                                font: family.clone(),
                            }]),
                            ToolOutput::CommitTransaction,
                        ]);
                    }
                }),
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

struct AppearanceTarget {
    shape_for_axis: NodeId,
    fill: Option<NodeId>,
    stroke: Option<NodeId>,
}

fn find_shape_painted_by(session: &Session, style_id: NodeId) -> Option<NodeId> {
    session
        .engine
        .scene()
        .items
        .iter()
        .rev()
        .find(|it| it.style == style_id)
        .map(|it| it.node)
}

fn appearance_for(session: &Session, selected: NodeId) -> Option<AppearanceTarget> {
    let doc = &session.file.document;
    let node = doc.nodes.get(selected)?;
    match &node.kind {
        NodeKind::Style(StyleKind::Fill { .. }) => {
            let shape_for_axis = find_shape_painted_by(session, selected).unwrap_or(selected);
            Some(AppearanceTarget {
                shape_for_axis,
                fill: Some(selected),
                stroke: None,
            })
        }
        NodeKind::Style(StyleKind::Stroke { .. }) => {
            let shape_for_axis = find_shape_painted_by(session, selected).unwrap_or(selected);
            Some(AppearanceTarget {
                shape_for_axis,
                fill: None,
                stroke: Some(selected),
            })
        }
        NodeKind::Shape(_) | NodeKind::Text(_) => {
            let fill =
                renamite_behavior_common::fill::fill_style_for_shape(doc, selected).or_else(|| {
                    session
                        .engine
                        .scene()
                        .items
                        .iter()
                        .rev()
                        .find(|it| {
                            it.node == selected
                                && matches!(it.kind, renamite_model::PaintKind::Fill(_))
                        })
                        .map(|it| it.style)
                });
            let stroke = renamite_behavior_common::stroke::stroke_style_for_shape(doc, selected)
                .or_else(|| {
                    session
                        .engine
                        .scene()
                        .items
                        .iter()
                        .rev()
                        .find(|it| {
                            it.node == selected
                                && matches!(it.kind, renamite_model::PaintKind::Stroke(_))
                        })
                        .map(|it| it.style)
                });
            Some(AppearanceTarget {
                shape_for_axis: selected,
                fill,
                stroke,
            })
        }
        NodeKind::Group | NodeKind::Layer(_) => {
            let content = primary_content_in_group(doc, selected)?;
            appearance_for(session, content)
        }
        _ => None,
    }
}

fn style_prop_rows(
    session: SessionRef,
    style_id: NodeId,
    playhead: Frame,
    record: bool,
    diamond_quiet: bool,
    section: &'static str,
) -> Option<View> {
    let rows: Vec<PropRow> = {
        let s = session.borrow();
        props_for_node(&s.file.document, style_id, playhead)
            .into_iter()
            .filter(|r| r.desc.section == section)
            .collect()
    };
    if rows.is_empty() {
        return None;
    }
    Some(crate::components::CollapsibleSection(
        format!("style_props_{:?}_{section}", style_id),
        section,
        vec![],
        Column(Modifier::new().fill_max_width()).child(
            rows.iter()
                .map(|prop| {
                    PropRowView(
                        session.clone(),
                        vec![style_id],
                        prop.clone(),
                        playhead,
                        record,
                        diamond_quiet,
                    )
                })
                .collect::<Vec<_>>(),
        ),
    ))
}

fn paint_section_for_style(
    session: SessionRef,
    shape_for_axis: NodeId,
    style_id: NodeId,
    playhead: Frame,
    record: bool,
) -> Option<View> {
    let (paint, section_label, solid_path) = {
        let session = session.borrow();
        let node = session.file.document.nodes.get(style_id)?;
        match &node.kind {
            NodeKind::Style(StyleKind::Fill { paint, .. }) => (paint.clone(), "Fill", "fill.color"),
            NodeKind::Style(StyleKind::Stroke { paint, .. }) => {
                (paint.clone(), "Stroke", "stroke.color")
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
        Box(Modifier::new().width(32.0)),
        Text("Paint")
            .size(th.typography.body_medium)
            .color(th.on_surface)
            .modifier(Modifier::new().width(96.0)),
        paint_segment(
            session.clone(),
            shape_for_axis,
            style_id,
            "Solid",
            is_solid,
            PaintTarget::Solid,
        ),
        paint_segment(
            session.clone(),
            shape_for_axis,
            style_id,
            "Linear",
            active_kind == Some(GradientKind::Linear),
            PaintTarget::Gradient(GradientKind::Linear),
        ),
        paint_segment(
            session.clone(),
            shape_for_axis,
            style_id,
            "Radial",
            active_kind == Some(GradientKind::Radial),
            PaintTarget::Gradient(GradientKind::Radial),
        ),
    ));
    let mut children = vec![toggle];
    match &paint {
        StylePaint::Solid { .. } => {
            let path = PropPath::new(solid_path);
            children.push(
                Row(Modifier::new()
                    .min_height(36.0)
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
            let (start_label, end_label) = match g.kind {
                GradientKind::Linear => ("Start", "End"),
                GradientKind::Radial => ("Center", "Edge"),
            };
            children.push(axis_row(
                session.clone(),
                style_id,
                playhead,
                record,
                start_label,
                "grad.start",
                g.start.value_at(playhead.0 as f64),
            ));
            children.push(axis_row(
                session.clone(),
                style_id,
                playhead,
                record,
                end_label,
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
                            insert_gradient_stop(&mut stops);
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
    Some(crate::components::CollapsibleSection(
        format!("paint_section_{:?}", style_id),
        section_label,
        vec![],
        Column(Modifier::new().fill_max_width()).child(children),
    ))
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
    let style_id = {
        let s = session.borrow();
        paint_style_id(&s, id)?
    };
    paint_section_for_style(session, id, style_id, playhead, record)
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
    let total = stops.0.len();
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
                    move |pe: repose_core::input::PointerEvent| {
                        let anchor = overlay_anchor(&pe);
                        session.borrow_mut().open_color_picker(
                            PickerTarget::GradientStop { style_id, index: i },
                            stop_color,
                            anchor,
                        );
                    }
                }));
            let base = i * 5;
            let remove = if total > 2 {
                CompactIconAction(Symbols::remove, "Remove stop", {
                    let session = session.clone();
                    move || {
                        let mut s = session.borrow_mut();
                        let frame = s.playback.head;
                        let Ok(Value::Stops(mut stops)) =
                            s.file
                                .document
                                .value_at(style_id, &PropPath::new("grad.stops"), frame)
                        else {
                            return;
                        };
                        if i >= stops.0.len() || stops.0.len() <= 2 {
                            return;
                        }
                        stops.0.remove(i);
                        let cmd = resolve_property_edit(
                            &s.file.document,
                            style_id,
                            &PropPath::new("grad.stops"),
                            Value::Stops(stops),
                            Frame(frame.round() as i64),
                            s.record,
                        );
                        s.apply_outputs(smallvec![
                            ToolOutput::BeginTransaction("Remove stop".into()),
                            ToolOutput::Commands(smallvec![cmd]),
                            ToolOutput::CommitTransaction,
                        ]);
                    }
                })
            } else {
                Box(Modifier::new().width(28.0))
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
                    path.clone(),
                    stop.color.a,
                    0.01,
                    Some(0.0),
                    Some(1.0),
                    playhead,
                    record,
                    base + 4,
                    40.0,
                ),
                remove,
            ))
        })
        .collect()
}

fn stroke_dash_section(
    session: SessionRef,
    id: NodeId,
    playhead: Frame,
    record: bool,
    diamond_quiet: bool,
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
    let mut children: Vec<View> = Vec::new();

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

        return Some(crate::components::CollapsibleSection(
            "stroke_dash_section",
            "Dash",
            vec![],
            Column(Modifier::new().fill_max_width()).child(children),
        ));
    };

    // Offset
    children.push(dash_scalar_row(
        session.clone(),
        id,
        playhead,
        record,
        diamond_quiet,
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
            diamond_quiet,
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

    Some(crate::components::CollapsibleSection(
        "stroke_dash_section",
        "Dash",
        vec![],
        Column(Modifier::new().fill_max_width()).child(children),
    ))
}

#[allow(clippy::too_many_arguments)]
fn dash_scalar_row(
    session: SessionRef,
    id: NodeId,
    playhead: Frame,
    record: bool,
    diamond_quiet: bool,
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
        diamond_button(
            session.clone(),
            vec![id],
            path.clone(),
            state,
            playhead,
            diamond_quiet,
        ),
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
