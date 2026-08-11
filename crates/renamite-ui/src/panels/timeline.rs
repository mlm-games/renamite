use glam::DVec2;
use renamite_behavior_timeline::{
    TimelineEvent, TimelineKey, TimelineLayout, TimelineOverlay, TimelineRow,
};
use repose_canvas::{Canvas, DrawScope};
use repose_core::geometry::Rect;
use repose_core::input::{Key, KeyEvent, PointerEvent};
use repose_core::{
    AlignItems, Color, JustifyContent, Modifier, TextFieldLineLimits, Vec2, View,
    remember_with_key, theme,
};
use repose_ui::scroll::{ScrollArea, remember_scroll_state};
use repose_ui::textfield::{BasicTextField, TextFieldConfig, TextFieldState};
use repose_ui::{Box, Column, Row, Text, TextStyle, ViewExt};
use std::cell::RefCell;
use std::rc::Rc;

use crate::components::{CompactIconAction, PanelHeader, StatusChip};
use crate::session::{SessionRef, dispatch_timeline, map_modifiers, pe_pos};
use crate::symbols::Symbols;

pub fn TimelinePanel(session: SessionRef) -> View {
    let (rows, head, range, playing, record, loop_mode) = {
        let s = session.borrow();
        (
            crate::session::timeline_rows(&s),
            s.playback.head,
            s.file.document.compositions[s.file.document.main].range,
            s.playing,
            s.record,
            s.playback.loop_mode,
        )
    };

    let loop_icon = match loop_mode {
        renamite_animation::LoopMode::Once => Symbols::arrow_right_alt,
        renamite_animation::LoopMode::Loop => Symbols::sync,
        renamite_animation::LoopMode::PingPong => Symbols::swap_horiz,
    };

    let header = PanelHeader(
        Symbols::play_arrow,
        "Timeline",
        vec![
            CompactIconAction(Symbols::skip_previous, "Previous keyframe / start", {
                let session = session.clone();
                move || session.borrow_mut().step_to_keyframe(-1)
            }),
            CompactIconAction(Symbols::fast_rewind, "Step back 1 frame", {
                let session = session.clone();
                move || session.borrow_mut().step_frames(-1.0)
            }),
            CompactIconAction(
                if playing {
                    Symbols::pause
                } else {
                    Symbols::play_arrow
                },
                "Play/Pause",
                {
                    let session = session.clone();
                    move || crate::toggle_playback(&session)
                },
            ),
            CompactIconAction(Symbols::fast_forward, "Step forward 1 frame", {
                let session = session.clone();
                move || session.borrow_mut().step_frames(1.0)
            }),
            CompactIconAction(Symbols::skip_next, "Next keyframe / end", {
                let session = session.clone();
                move || session.borrow_mut().step_to_keyframe(1)
            }),
            CompactIconAction(loop_icon, "Loop mode (Once / Loop / Ping-Pong)", {
                let session = session.clone();
                move || session.borrow_mut().cycle_loop_mode()
            }),
            CompactIconAction(Symbols::zoom_in, "Zoom in", {
                let session = session.clone();
                move || session.borrow_mut().zoom_timeline(1.25)
            }),
            CompactIconAction(Symbols::zoom_out, "Zoom out", {
                let session = session.clone();
                move || session.borrow_mut().zoom_timeline(0.8)
            }),
            CompactIconAction(Symbols::delete, "Delete selected keyframes", {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    dispatch_timeline(&mut s, TimelineEvent::KeyDown(TimelineKey::Delete));
                }
            }),
            CompactIconAction(Symbols::fit_screen, "Fit range", {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    let range = s.file.document.compositions[s.file.document.main].range;
                    let frames = (range.1.0 - range.0.0).max(1) as f64;
                    s.timeline_zoom = (300.0 / frames).clamp(0.5, 48.0);
                    s.repaint();
                }
            }),
        ],
    );

    if rows.is_empty() {
        let th = theme();
        return Column(Modifier::new().fill_max_size()).child((
            header,
            TimelineInfoBar(session.clone(), head, range, record),
            Box(Modifier::new()
                .fill_max_size()
                .padding_values(repose_core::PaddingValues {
                    left: 24.0,
                    right: 24.0,
                    top: 0.0,
                    bottom: 0.0,
                })
                .align_items(AlignItems::CENTER)
                .justify_content(JustifyContent::CENTER))
            .child(
                Column(Modifier::new().gap(8.0).align_items(AlignItems::CENTER)).child((
                    Text("No animated properties yet")
                        .size(th.typography.body_medium)
                        .color(th.on_surface),
                    Text(
                        "Select a layer, switch to Animate, then click a diamond in \
                        Properties, or enable Record and edit a value at the playhead.",
                    )
                    .size(th.typography.body_small)
                    .color(th.on_surface_variant),
                )),
            ),
        ));
    }

    Column(Modifier::new().fill_max_size()).child((
        header,
        TimelineInfoBar(session.clone(), head, range, record),
        Row(Modifier::new().fill_max_size()).child((
            TimelineLabels(session.clone(), &rows),
            Box(Modifier::new().weight(1.0).fill_max_height()).child(TimelineCanvas(session)),
        )),
    ))
}

fn TimelineInfoBar(
    session: SessionRef,
    head: f64,
    range: (renamite_animation::Frame, renamite_animation::Frame),
    record: bool,
) -> View {
    Row(Modifier::new()
        .fill_max_width()
        .padding(8.0)
        .gap(8.0)
        .align_items(AlignItems::CENTER))
    .child((
        StatusChip(
            format!("Frame {}", head.round() as i64),
            theme().surface_container_high,
            theme().on_surface_variant,
        ),
        RangeEditor(session, range),
        if record {
            StatusChip(
                "● REC — edits add keys".to_string(),
                theme().error_container,
                theme().on_error_container,
            )
        } else {
            Text("One row per keyed property")
                .size(theme().typography.label_small)
                .color(theme().on_surface_variant)
        },
    ))
}

fn RangeEditor(
    session: SessionRef,
    range: (renamite_animation::Frame, renamite_animation::Frame),
) -> View {
    let committed = remember_with_key("timeline_range_committed", || {
        RefCell::new((range.0.0, range.1.0))
    });
    let start_state = remember_with_key("timeline_range_start", || {
        RefCell::new(TextFieldState::new())
    });
    let end_state = remember_with_key("timeline_range_end", || RefCell::new(TextFieldState::new()));
    let start_focus =
        remember_with_key("timeline_range_start_focus", || std::cell::Cell::new(false));
    let end_focus = remember_with_key("timeline_range_end_focus", || std::cell::Cell::new(false));
    let was_start = remember_with_key("timeline_range_start_was", || std::cell::Cell::new(false));
    let was_end = remember_with_key("timeline_range_end_was", || std::cell::Cell::new(false));
    let th = theme();

    {
        let mut c = committed.borrow_mut();
        if (range.0.0, range.1.0) != *c {
            if !start_focus.get() {
                start_state.borrow_mut().text = range.0.0.to_string();
            }
            if !end_focus.get() {
                end_state.borrow_mut().text = range.1.0.to_string();
            }
            *c = (range.0.0, range.1.0);
        }
    }
    // Uncommitted edits revert when a field loses focus (Properties style).
    for (was, focus, into) in [
        (&was_start, &start_focus, &start_state),
        (&was_end, &end_focus, &end_state),
    ] {
        if was.get() && !focus.get() {
            let c = committed.borrow();
            let v = if std::ptr::eq(&was_start, was) {
                c.0
            } else {
                c.1
            };
            into.borrow_mut().text = v.to_string();
        }
        was.set(focus.get());
    }

    let field = |state: Rc<RefCell<TextFieldState>>, focus: Rc<std::cell::Cell<bool>>| {
        let session = session.clone();
        let committed = committed.clone();
        let start_state = start_state.clone();
        let end_state = end_state.clone();
        BasicTextField(
            state,
            Modifier::new().width(48.0).height(28.0).on_key_event({
                let committed = committed.clone();
                let start_state = start_state.clone();
                let end_state = end_state.clone();
                move |ke: KeyEvent| {
                    if matches!(ke.key, Key::Escape) {
                        let c = committed.borrow();
                        start_state.borrow_mut().text = c.0.to_string();
                        end_state.borrow_mut().text = c.1.to_string();
                        return true;
                    }
                    false
                }
            }),
            "",
            TextFieldConfig {
                line_limits: TextFieldLineLimits::SingleLine,
                focus_tracker: Some(focus),
                on_submit: Some(Rc::new(move |_| {
                    let start = start_state
                        .borrow()
                        .text
                        .trim()
                        .parse::<i64>()
                        .ok()
                        .filter(|&s| s >= 0);
                    let end = end_state.borrow().text.trim().parse::<i64>().ok();
                    if let (Some(s), Some(e)) = (start, end)
                        && e > s
                    {
                        session.borrow_mut().set_composition_range(
                            Some(renamite_animation::Frame(s)),
                            Some(renamite_animation::Frame(e)),
                        );
                    }
                })),
                text_style: repose_core::TextStyle {
                    font_size: th.typography.body_small,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
    };

    Row(Modifier::new().gap(4.0).align_items(AlignItems::CENTER)).child((
        field(start_state.clone(), start_focus),
        Text("–")
            .size(th.typography.body_small)
            .color(th.on_surface_variant),
        field(end_state.clone(), end_focus),
    ))
}

fn prop_label(
    session: SessionRef,
    node: renamite_model::NodeId,
    path: &renamite_model::PropPath,
) -> Option<String> {
    let s = session.borrow();
    renamite_behavior_common::inspect::props_for_node(
        &s.file.document,
        node,
        renamite_animation::Frame(0),
    )
    .into_iter()
    .find(|row| &row.desc.path == path)
    .map(|row| row.desc.label.to_string())
}

fn TimelineLabels(session: SessionRef, rows: &[TimelineRow]) -> View {
    Box(Modifier::new().width(170.0).fill_max_height()).child(ScrollArea(
        Modifier::new().fill_max_size(),
        remember_scroll_state("timeline_labels_scroll"),
        Column(Modifier::new().fill_max_width()).child(
            rows.iter()
                .map(|row| {
                    let label = prop_label(session.clone(), row.node, &row.prop);
                    let name = match label {
                        Some(prop) => {
                            format!("{} · {}", session.borrow().node_name(row.node), prop)
                        }
                        None => session.borrow().node_name(row.node),
                    };
                    Box(Modifier::new()
                        .height(22.0)
                        .fill_max_width()
                        .padding_values(repose_core::PaddingValues {
                            left: 10.0,
                            right: 8.0,
                            top: 0.0,
                            bottom: 0.0,
                        })
                        .align_items(AlignItems::CENTER))
                    .child(
                        Text(name)
                            .size(theme().typography.body_small)
                            .color(theme().on_surface),
                    )
                })
                .collect::<Vec<_>>(),
        ),
    ))
}

fn TimelineCanvas(session: SessionRef) -> View {
    let sess_draw = session.clone();
    // Simple double-click detector (repose has no on_double_click on Modifier yet).
    let last_click = std::rc::Rc::new(std::cell::RefCell::new(None::<(DVec2, web_time::Instant)>));

    Canvas(
        Modifier::new()
            .fill_max_size()
            .on_scroll({
                let session = session.clone();
                move |delta: repose_core::Vec2| {
                    let mut s = session.borrow_mut();
                    let factor = (1.0 + (delta.y as f64) * 0.002).clamp(0.5, 2.0);
                    s.zoom_timeline(factor);
                    repose_core::Vec2::ZERO
                }
            })
            .on_pointer_down({
                let session = session.clone();
                let last_click = last_click.clone();
                move |pe: PointerEvent| {
                    let pos = pe_pos(&pe);
                    let mods = map_modifiers(&pe);
                    let now = web_time::Instant::now();
                    let is_double = {
                        let mut lc = last_click.borrow_mut();
                        let dbl = lc
                            .map(|(p, t)| (now - t).as_millis() < 350 && (p - pos).length() < 6.0)
                            .unwrap_or(false);
                        *lc = Some((pos, now));
                        dbl
                    };

                    let mut s = session.borrow_mut();
                    if is_double {
                        dispatch_timeline(
                            &mut s,
                            TimelineEvent::DoubleClick {
                                pos,
                                modifiers: mods,
                            },
                        );
                    } else {
                        dispatch_timeline(
                            &mut s,
                            TimelineEvent::Press {
                                pos,
                                modifiers: mods,
                            },
                        );
                    }
                }
            })
            .on_pointer_move({
                let session = session.clone();
                move |pe: PointerEvent| {
                    let mut s = session.borrow_mut();
                    dispatch_timeline(
                        &mut s,
                        TimelineEvent::Move {
                            pos: pe_pos(&pe),
                            modifiers: map_modifiers(&pe),
                        },
                    );
                }
            })
            .on_pointer_up({
                let session = session.clone();
                move |pe: PointerEvent| {
                    let mut s = session.borrow_mut();
                    dispatch_timeline(
                        &mut s,
                        TimelineEvent::Release {
                            pos: pe_pos(&pe),
                            modifiers: map_modifiers(&pe),
                        },
                    );
                }
            }),
        move |scope| {
            let s = sess_draw.borrow();
            let th = theme();
            let rows = crate::session::timeline_rows(&s);
            let layout = TimelineLayout {
                origin_x: 0.0,
                px_per_frame: s.timeline_zoom,
                row_top: 24.0,
                row_height: 22.0,
                key_tolerance_px: 6.0,
            };
            let range = s.file.document.compositions[s.file.document.main].range;
            let selected = s.keys.selected();
            let overlay = s.keys.overlay();

            // Ruler background.
            scope.draw_rect(
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w: scope.size.width,
                    h: layout.row_top as f32,
                },
                th.surface_container_highest,
                0.0,
            );

            // Zebra rows.
            for i in 0..rows.len() {
                let y = layout.row_top + i as f64 * layout.row_height;
                let bg = if i % 2 == 0 {
                    th.surface_container
                } else {
                    th.surface_container_high
                };
                scope.draw_rect(
                    Rect {
                        x: 0.0,
                        y: y as f32,
                        w: scope.size.width,
                        h: layout.row_height as f32,
                    },
                    bg,
                    0.0,
                );
            }

            // Ruler ticks + frame labels on majors.
            for frame in range.0.0..=range.1.0 {
                let x = layout.frame_to_x(frame as f64) as f32;
                let major = frame % 10 == 0;
                let tick_h = if major { layout.row_top as f32 } else { 8.0 };
                scope.draw_rect(
                    Rect {
                        x,
                        y: layout.row_top as f32 - tick_h,
                        w: if major { 1.5 } else { 1.0 },
                        h: tick_h,
                    },
                    if major {
                        th.outline
                    } else {
                        th.outline_variant
                    },
                    0.0,
                );
                if major {
                    scope.draw_text(
                        &frame.to_string(),
                        Vec2 { x: x + 3.0, y: 4.0 },
                        th.on_surface_variant,
                        10.0,
                    );
                }
            }

            // Keyframe diamonds.
            for (row_i, row) in rows.iter().enumerate() {
                let cy = layout.row_center_y(row_i) as f32;
                let frames = s.file.document.key_frames(row.node, &row.prop);
                for frame in frames {
                    let cx = layout.frame_to_x(frame.0 as f64) as f32;
                    let is_sel = selected
                        .iter()
                        .any(|k| k.node == row.node && k.prop == row.prop && k.frame == frame);
                    draw_diamond(
                        scope,
                        cx,
                        cy,
                        if is_sel { 7.0 } else { 5.5 },
                        if is_sel { th.primary } else { th.secondary },
                        if is_sel {
                            Some(th.on_primary)
                        } else {
                            Some(th.surface)
                        },
                    );
                }
            }

            // Box-select / drag-delta overlay.
            match overlay {
                TimelineOverlay::BoxSelect { min, max } => {
                    let r = Rect {
                        x: min.x.min(max.x) as f32,
                        y: min.y.min(max.y) as f32,
                        w: (max.x - min.x).abs() as f32,
                        h: (max.y - min.y).abs() as f32,
                    };
                    scope.draw_rect(r, th.primary.with_alpha(40), 0.0);
                    scope.draw_rect_stroke(r, th.primary.with_alpha(200), 0.0, 1.0);
                }
                TimelineOverlay::DragDelta { frames } if frames != 0 => {
                    scope.draw_text(
                        &format!("{frames:+}f"),
                        Vec2 {
                            x: 8.0,
                            y: scope.size.height - 18.0,
                        },
                        th.primary,
                        12.0,
                    );
                }
                _ => {}
            }

            // Playhead on top.
            let x = layout.frame_to_x(s.playback.head) as f32;
            // triangle head in ruler
            draw_diamond(
                scope,
                x,
                (layout.row_top * 0.45) as f32,
                6.0,
                th.primary,
                None,
            );
            scope.draw_rect(
                Rect {
                    x: x - 1.0,
                    y: 0.0,
                    w: 2.0,
                    h: scope.size.height,
                },
                th.primary,
                0.0,
            );
        },
    )
}

/// Axis-aligned diamond (rotated square) via two triangles in a vector overlay.
fn draw_diamond(
    scope: &mut DrawScope,
    cx: f32,
    cy: f32,
    half: f32,
    fill: Color,
    stroke_center: Option<Color>,
) {
    let c = [
        fill.0 as f32 / 255.0,
        fill.1 as f32 / 255.0,
        fill.2 as f32 / 255.0,
        fill.3 as f32 / 255.0,
    ];
    let pts = [
        [cx, cy - half],
        [cx + half, cy],
        [cx, cy + half],
        [cx - half, cy],
    ];
    let vertices: Vec<_> = pts
        .iter()
        .map(|p| repose_core::view::VectorVertex {
            pos: *p,
            color: c,
            uv: [0.0, 0.0],
        })
        .collect();
    let mesh = repose_core::view::VectorMeshData {
        vertices: std::sync::Arc::from(vertices),
        indices: std::sync::Arc::from([0u32, 1, 2, 0, 2, 3]),
    };
    scope.draw_vector_overlay(std::sync::Arc::from([mesh]));

    // Inner highlight for selected keys.
    if let Some(inner) = stroke_center {
        let ih = half * 0.45;
        let c2 = [
            inner.0 as f32 / 255.0,
            inner.1 as f32 / 255.0,
            inner.2 as f32 / 255.0,
            inner.3 as f32 / 255.0,
        ];
        let pts2 = [[cx, cy - ih], [cx + ih, cy], [cx, cy + ih], [cx - ih, cy]];
        let vertices: Vec<_> = pts2
            .iter()
            .map(|p| repose_core::view::VectorVertex {
                pos: *p,
                color: c2,
                uv: [0.0, 0.0],
            })
            .collect();
        let mesh = repose_core::view::VectorMeshData {
            vertices: std::sync::Arc::from(vertices),
            indices: std::sync::Arc::from([0u32, 1, 2, 0, 2, 3]),
        };
        scope.draw_vector_overlay(std::sync::Arc::from([mesh]));
    }
}
