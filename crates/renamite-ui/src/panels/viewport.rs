use glam::DVec2;
use renamite_behavior_canvas::{CanvasEvent, PointerButton};
use renamite_behavior_common::ViewTransform;
use renamite_model::Composition;
use repose_canvas::{Canvas, DrawScope};
use repose_core::input::{PointerEvent, PointerEventKind};
use repose_core::{Color, Modifier, View, request_frame, theme};
use repose_core::geometry::Rect;
use repose_ui::{Box, Column, Row, Text, TextStyle, ViewExt};

use crate::components::{CompactIconAction, PanelSurface};
use crate::session::{dispatch_canvas, pe_pos, SessionRef};
use crate::symbols::Symbols;

pub fn ViewportPanel(session: SessionRef) -> View {
    let draw_session = session.clone();

    Column(Modifier::new().fill_max_size()).child((
        Canvas(
            Modifier::new()
                .fill_max_size()
                .background(theme().surface_container_lowest)
                .on_pointer_down({
                    let session = session.clone();
                    move |pe: PointerEvent| {
                        let mut s = session.borrow_mut();
                        let pos = pe_pos(&pe);

                        if map_button(&pe) == PointerButton::Middle {
                            s.viewport.begin_pan(pos);
                            request_frame();
                            return;
                        }

                        let world = s.viewport.view.screen_to_world(pos);
                        dispatch_canvas(
                            &mut s,
                            CanvasEvent::PointerDown {
                                pos: world,
                                button: map_button(&pe),
                            },
                        );
                    }
                })
                .on_pointer_move({
                    let session = session.clone();
                    move |pe: PointerEvent| {
                        let mut s = session.borrow_mut();
                        let pos = pe_pos(&pe);

                        if s.viewport.update_pan(pos) {
                            s.revision = s.revision.wrapping_add(1);
                            request_frame();
                            return;
                        }

                        let world = s.viewport.view.screen_to_world(pos);
                        dispatch_canvas(&mut s, CanvasEvent::PointerMove { pos: world });
                    }
                })
                .on_pointer_up({
                    let session = session.clone();
                    move |pe: PointerEvent| {
                        let mut s = session.borrow_mut();

                        if s.viewport.pan_last.is_some() {
                            s.viewport.end_pan();
                            request_frame();
                            return;
                        }

                        let world = s.viewport.view.screen_to_world(pe_pos(&pe));
                        dispatch_canvas(
                            &mut s,
                            CanvasEvent::PointerUp {
                                pos: world,
                                button: map_button(&pe),
                            },
                        );
                    }
                }),
            move |scope| {
                let mut s = draw_session.borrow_mut();
                let comp_id = s.file.document.main;
                let (cw, ch) = {
                    let comp = &s.file.document.compositions[comp_id];
                    (comp.size.0, comp.size.1)
                };
                let artboard = DVec2::new(cw as f64, ch as f64);
                let surface = DVec2::new(scope.size.width as f64, scope.size.height as f64);

                s.viewport.ensure_fit(surface, artboard);

                let comp = &s.file.document.compositions[comp_id];
                paint_artboard(scope, comp, &s.viewport.view);

                let scene = s.engine.scene().clone();
                let view = s.viewport.view;
                let prepared = s.renderer.prepare(&scene, &view);
                s.renderer.paint_prepared(&prepared, scope);
            },
        ),
        ViewportControls(session),
    ))
}

fn paint_artboard(
    scope: &mut DrawScope,
    comp: &Composition,
    view: &ViewTransform,
) {
    let th = theme();
    let origin = view.world_to_screen(DVec2::ZERO);
    let width = comp.size.0 as f64 * view.scale;
    let height = comp.size.1 as f64 * view.scale;

    // Shadow/backplate.
    scope.draw_rect(
        Rect {
            x: origin.x as f32 - 4.0,
            y: origin.y as f32 - 4.0,
            w: width as f32 + 8.0,
            h: height as f32 + 8.0,
        },
        Color(0, 0, 0, 48),
        3.0,
    );

    // Checkerboard.
    let tile_world = 32.0;
    let cols = (comp.size.0 as f64 / tile_world).ceil() as usize;
    let rows = (comp.size.1 as f64 / tile_world).ceil() as usize;

    for y in 0..rows {
        for x in 0..cols {
            let p = view.world_to_screen(DVec2::new(
                x as f64 * tile_world,
                y as f64 * tile_world,
            ));

            let color = if (x + y) % 2 == 0 {
                th.surface
            } else {
                th.surface_container_high
            };

            scope.draw_rect(
                Rect {
                    x: p.x as f32,
                    y: p.y as f32,
                    w: (tile_world * view.scale).ceil() as f32,
                    h: (tile_world * view.scale).ceil() as f32,
                },
                color,
                0.0,
            );
        }
    }

    // One-pixel artboard border.
    let border = th.outline_variant;
    let x = origin.x as f32;
    let y = origin.y as f32;
    let w = width as f32;
    let h = height as f32;

    scope.draw_rect(Rect { x, y, w, h: 1.0 }, border, 0.0);
    scope.draw_rect(Rect { x, y: y + h - 1.0, w, h: 1.0 }, border, 0.0);
    scope.draw_rect(Rect { x, y, w: 1.0, h }, border, 0.0);
    scope.draw_rect(Rect { x: x + w - 1.0, y, w: 1.0, h }, border, 0.0);
}

fn ViewportControls(session: SessionRef) -> View {
    let zoom = session.borrow().viewport.view.scale * 100.0;

    Box(
        Modifier::new()
            .absolute()
            .offset(None, None, Some(16.0), Some(16.0)),
    )
    .child(
        PanelSurface(
            Row(
                Modifier::new()
                    .align_items(repose_core::AlignItems::CENTER)
                    .gap(2.0)
                    .padding(4.0),
            )
            .child((
                CompactIconAction(Symbols::zoom_out, "Zoom out", {
                    let session = session.clone();
                    move || {
                        session.borrow_mut().viewport.zoom_centered(1.0 / 1.2);
                        request_frame();
                    }
                }),
                Text(format!("{zoom:.0}%"))
                    .size(theme().typography.label_medium)
                    .color(theme().on_surface_variant)
                    .modifier(Modifier::new().min_width(52.0)),
                CompactIconAction(Symbols::zoom_in, "Zoom in", {
                    let session = session.clone();
                    move || {
                        session.borrow_mut().viewport.zoom_centered(1.2);
                        request_frame();
                    }
                }),
                CompactIconAction(Symbols::fit_screen, "Fit artboard", {
                    let session = session.clone();
                    move || {
                        let mut s = session.borrow_mut();
                        s.viewport.fit_pending = true;
                        request_frame();
                    }
                }),
            )),
        ),
    )
}

fn map_button(pe: &PointerEvent) -> PointerButton {
    match pe.event {
        PointerEventKind::Down(button) | PointerEventKind::Up(button) => match button {
            repose_core::input::PointerButton::Primary => PointerButton::Primary,
            repose_core::input::PointerButton::Secondary => PointerButton::Secondary,
            repose_core::input::PointerButton::Tertiary => PointerButton::Middle,
        },
        _ => PointerButton::Primary,
    }
}