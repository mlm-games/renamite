use renamite_behavior_timeline::{TimelineEvent, TimelineLayout, TimelineRow};
use repose_canvas::Canvas;
use repose_core::geometry::Rect;
use repose_core::input::PointerEvent;
use repose_core::{AlignItems, JustifyContent, Modifier, View, theme};
use repose_ui::scroll::{ScrollArea, remember_scroll_state};
use repose_ui::{Box, Column, Row, Text, TextStyle, ViewExt};

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
            TimelineInfoBar(head, range, record),
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
        TimelineInfoBar(head, range, record),
        Row(Modifier::new().fill_max_size()).child((
            TimelineLabels(session.clone(), &rows),
            Box(Modifier::new().weight(1.0).fill_max_height()).child(TimelineCanvas(session)),
        )),
    ))
}

fn TimelineInfoBar(
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
        StatusChip(
            format!("Range {}–{}", range.0.0, range.1.0),
            theme().surface_container_high,
            theme().on_surface_variant,
        ),
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
                move |pe: PointerEvent| {
                    let mut s = session.borrow_mut();
                    dispatch_timeline(
                        &mut s,
                        TimelineEvent::Press {
                            pos: pe_pos(&pe),
                            modifiers: map_modifiers(&pe),
                        },
                    );
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

            // Ruler ticks across the full frame range.
            let range = s.file.document.compositions[s.file.document.main].range;
            for frame in range.0.0..=range.1.0 {
                let x = layout.frame_to_x(frame as f64) as f32;
                let major = frame % 30 == 0;
                let tick_h = if major { scope.size.height } else { 12.0 };
                scope.draw_rect(
                    Rect {
                        x,
                        y: 0.0,
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
            }

            for (i, row) in rows.iter().enumerate() {
                let y = layout.row_top + i as f64 * layout.row_height + layout.row_height * 0.5;

                for frame in s.file.document.key_frames(row.node, &row.prop) {
                    let x = layout.frame_to_x(frame.0 as f64) as f32;
                    let at_playhead = (frame.0 as f64 - s.playback.head).abs() < 0.5;
                    scope.draw_rect(
                        Rect {
                            x: x - 4.0,
                            y: y as f32 - 4.0,
                            w: 8.0,
                            h: 8.0,
                        },
                        if at_playhead {
                            th.primary
                        } else {
                            th.primary_container
                        },
                        2.0,
                    );
                }
            }

            // Playhead.
            let x = layout.frame_to_x(s.playback.head) as f32;
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
