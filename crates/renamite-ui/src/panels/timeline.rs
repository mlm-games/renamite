use renamite_behavior_timeline::{TimelineEvent, TimelineLayout, TimelineRow};
use repose_canvas::Canvas;
use repose_core::geometry::Rect;
use repose_core::input::PointerEvent;
use repose_core::{AlignItems, Modifier, View, theme};
use repose_ui::scroll::{ScrollArea, remember_scroll_state};
use repose_ui::{Box, Column, Row, Text, TextStyle, ViewExt};

use crate::components::{CompactIconAction, PanelHeader, StatusChip};
use crate::session::{EditorMode, SessionRef, dispatch_timeline, map_modifiers, pe_pos};
use crate::symbols::Symbols;

pub fn TimelinePanel(session: SessionRef) -> View {
    let (rows, head, range, playing, record) = {
        let s = session.borrow();
        (
            crate::session::timeline_rows(&s),
            s.playback.head,
            s.file.document.compositions[s.file.document.main].range,
            s.playing,
            s.record,
        )
    };

    let header = PanelHeader(
        Symbols::play_arrow,
        "Timeline",
        vec![
            CompactIconAction(Symbols::skip_previous, "Jump to start", {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    let range = s.file.document.compositions[s.file.document.main].range;
                    s.playback.head = range.0.0 as f64;
                    let crate::session::Session {
                        file,
                        engine,
                        playback,
                        ..
                    } = &mut *s;
                    let head = playback.head;
                    engine.scrub(file, head);
                    s.bump();
                }
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
            CompactIconAction(
                if record {
                    Symbols::fiber_manual_record
                } else {
                    Symbols::radio_button_unchecked
                },
                "Animate mode",
                {
                    let session = session.clone();
                    move || session.borrow_mut().set_mode(EditorMode::Animate)
                },
            ),
        ],
    );

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
            Text("One transform property per layer")
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
                px_per_frame: 6.0,
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
