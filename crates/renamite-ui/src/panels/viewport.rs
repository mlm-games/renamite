use glam::DVec2;
use renamite_behavior_canvas::{CanvasEvent, PointerButton, ShapePreviewKind, ToolOverlay};
use renamite_behavior_common::{Modifiers, SnapConfig, ToolContext, ViewTransform};
use renamite_model::Composition;
use repose_canvas::{Canvas, DrawScope};
use repose_core::geometry::Rect;
use repose_core::input::{KeyEvent, PointerEvent, PointerEventKind};
use repose_core::{
    AlignItems, Color, CursorIcon, FocusRequester, JustifyContent, Modifier, View, remember,
    remember_with_key, request_frame, theme,
};
use repose_ui::scroll::{ScrollArea, remember_scroll_state};
use repose_ui::{Box, Column, Row, Text, TextStyle, ViewExt, ZStack};
use std::cell::Cell;

use crate::components::CompactIconAction;
use crate::session::{
    ContextMenuSource, ContextMenuState, SessionRef, dispatch_canvas, map_modifiers,
    overlay_anchor, pe_pos,
};
use crate::symbols::Symbols;
use renamite_behavior_common::context_menu::{MenuContext, canvas_menu};

pub fn ViewportPanel(session: SessionRef) -> View {
    let draw_session = session.clone();
    let focus = remember(FocusRequester::new);

    let _gesture_handler = remember_with_key("viewport_gesture_handler", || {
        let session = session.clone();
        repose_core::shortcuts::InstallShortcutHandler(std::rc::Rc::new(move |action| {
            use repose_core::shortcuts::{Action, Gesture};
            match action {
                Action::Gesture(Gesture::Pan { delta }) => {
                    let is_drag = {
                        let s = session.borrow();
                        s.tool.is_dragging(s.active_tool) || s.viewport.pan_last.is_some()
                    };
                    if is_drag {
                        return false;
                    }
                    {
                        let mut s = session.borrow_mut();
                        s.viewport.view.offset += DVec2::new(delta.x as f64, delta.y as f64);
                    }
                    request_frame();
                    true
                }
                Action::Gesture(Gesture::Pinch { delta_scale }) => {
                    let is_drag = {
                        let s = session.borrow();
                        s.tool.is_dragging(s.active_tool)
                    };
                    if is_drag {
                        return false;
                    }
                    let center_in_viewport = {
                        let s = session.borrow();
                        s.viewport.surface_size * 0.5
                    };
                    {
                        let mut s = session.borrow_mut();
                        s.viewport.zoom_at(center_in_viewport, delta_scale as f64);
                    }
                    request_frame();
                    true
                }
                Action::Gesture(Gesture::PinchWithCenter {
                    delta_scale,
                    center,
                }) => {
                    let is_drag = {
                        let s = session.borrow();
                        s.tool.is_dragging(s.active_tool)
                    };
                    if is_drag {
                        return false;
                    }
                    let center_in_viewport = {
                        let s = session.borrow();
                        if let Some(rect) = s.viewport.screen_rect {
                            DVec2::new((center.x - rect.x) as f64, (center.y - rect.y) as f64)
                        } else {
                            DVec2::new(center.x as f64, center.y as f64)
                        }
                    };
                    {
                        let mut s = session.borrow_mut();
                        s.viewport.zoom_at(center_in_viewport, delta_scale as f64);
                    }
                    request_frame();
                    true
                }
                _ => false,
            }
        }))
    });
    let _ = &_gesture_handler;

    let show_template_picker = { session.borrow().welcome };

    let main_view = if show_template_picker {
        TemplatePicker(session.clone())
    } else {
        let panning = session.borrow().viewport.pan_last.is_some();
        Canvas(
            Modifier::new()
                .fill_max_size()
                .background(theme().surface_container_lowest)
                .focusable(true)
                .focus_requester((*focus).clone())
                .cursor(if panning {
                    CursorIcon::Grabbing
                } else {
                    CursorIcon::Default
                })
                .on_scroll({
                    let session = session.clone();
                    move |delta: repose_core::Vec2| {
                        let is_tool_drag = {
                            let s = session.borrow();
                            s.viewport.pan_last.is_some() || s.tool.is_active(s.active_tool)
                        };
                        if is_tool_drag {
                            return repose_core::Vec2::ZERO;
                        }
                        let mut s = session.borrow_mut();
                        if delta.y.abs() < delta.x.abs() {
                            // Horizontal wheel: pan the canvas along X.
                            s.viewport.view.offset += DVec2::new(delta.x as f64, 0.0);
                        } else {
                            let anchor = s.viewport.last_pointer;
                            let factor = (1.0 + (-delta.y as f64) * 0.002).clamp(0.5, 2.0);
                            s.viewport.zoom_at(anchor, factor);
                        }
                        request_frame();
                        repose_core::Vec2::ZERO
                    }
                })
                .on_key_event({
                    let session = session.clone();
                    move |ke: KeyEvent| crate::shortcuts::handle_viewport_key(&session, ke)
                })
                .on_pointer_down({
                    let session = session.clone();
                    move |pe: PointerEvent| {
                        let mut s = session.borrow_mut();
                        let pos = pe_pos(&pe);
                        s.viewport.last_pointer = pos;

                        if map_button(&pe) == PointerButton::Secondary {
                            // Right-click: pick/select under cursor, then menu.
                            focus.request_focus();
                            let world = s.viewport.view.screen_to_world(pos);
                            let scene = s.engine.scene().clone();
                            if let Some(id) = renamite_model::pick(&scene, world) {
                                if !s.selection.nodes.contains(&id) {
                                    s.selection.nodes = vec![id];
                                }
                            } else if !pe.modifiers.shift {
                                s.selection.nodes.clear();
                            }
                            let paint = s.current_paint.clone();
                            let entries = {
                                let ctx = MenuContext {
                                    doc: &s.file.document,
                                    selection: &s.selection.nodes,
                                    comp: s.file.document.main,
                                    world_pos: Some(world),
                                    has_clipboard: s.clipboard.is_some(),
                                    current_paint: &paint,
                                };
                                canvas_menu(&ctx)
                            };
                            s.open_context_menu(ContextMenuState {
                                screen_pos: overlay_anchor(&pe),
                                entries,
                                source: ContextMenuSource::Canvas { world },
                            });
                            return;
                        }

                        let is_middle_pan = map_button(&pe) == PointerButton::Middle;
                        let is_space_pan =
                            map_button(&pe) == PointerButton::Primary && s.viewport.space_held;
                        if is_middle_pan || is_space_pan {
                            pe.consume();
                            s.viewport.begin_pan(pos);
                            request_frame();
                            return;
                        }

                        focus.request_focus();
                        s.viewport.pointer_down = true;
                        let world = s.viewport.view.screen_to_world(pos);
                        if s.mode == crate::session::EditorMode::Interact
                            || s.machine_preview_enabled
                        {
                            // Still allow selection with Alt for listener authoring.
                            if pe.modifiers.alt {
                                dispatch_canvas(
                                    &mut s,
                                    CanvasEvent::PointerDown {
                                        pos: world,
                                        button: map_button(&pe),
                                    },
                                    map_modifiers(&pe),
                                );
                            } else {
                                s.engine_pointer_down(world);
                            }
                            return;
                        }
                        dispatch_canvas(
                            &mut s,
                            CanvasEvent::PointerDown {
                                pos: world,
                                button: map_button(&pe),
                            },
                            map_modifiers(&pe),
                        );
                    }
                })
                .on_pointer_move({
                    let session = session.clone();
                    move |pe: PointerEvent| {
                        let mut s = session.borrow_mut();
                        let pos = pe_pos(&pe);
                        s.viewport.last_pointer = pos;

                        let window_pos = pe.position_in_window();
                        let origin = repose_core::Vec2 {
                            x: window_pos.x - pos.x as f32,
                            y: window_pos.y - pos.y as f32,
                        };
                        let size = repose_core::Vec2 {
                            x: s.viewport.surface_size.x as f32,
                            y: s.viewport.surface_size.y as f32,
                        };
                        if size.x > 0.0 && size.y > 0.0 {
                            s.viewport.screen_rect = Some(repose_core::geometry::Rect {
                                x: origin.x,
                                y: origin.y,
                                w: size.x,
                                h: size.y,
                            });
                        }

                        if s.viewport.update_pan(pos) {
                            pe.consume();
                            request_frame();
                            return;
                        }

                        let world = s.viewport.view.screen_to_world(pos);
                        if s.mode == crate::session::EditorMode::Interact
                            || s.machine_preview_enabled
                        {
                            if !pe.modifiers.alt {
                                s.engine_pointer_move(world);
                                return;
                            }
                        }
                        dispatch_canvas(
                            &mut s,
                            CanvasEvent::PointerMove { pos: world },
                            map_modifiers(&pe),
                        );
                    }
                })
                .on_pointer_up({
                    let session = session.clone();
                    move |pe: PointerEvent| {
                        let mut s = session.borrow_mut();
                        s.viewport.pointer_down = false;

                        if s.viewport.pan_last.is_some() {
                            pe.consume();
                            s.viewport.end_pan();
                            request_frame();
                            return;
                        }

                        let world = s.viewport.view.screen_to_world(pe_pos(&pe));
                        if s.mode == crate::session::EditorMode::Interact
                            || s.machine_preview_enabled
                        {
                            if !pe.modifiers.alt {
                                s.engine_pointer_up(world);
                                return;
                            }
                        }
                        dispatch_canvas(
                            &mut s,
                            CanvasEvent::PointerUp {
                                pos: world,
                                button: map_button(&pe),
                            },
                            map_modifiers(&pe),
                        );
                    }
                })
                .on_pointer_cancel({
                    let session = session.clone();
                    move |pe| {
                        pe.consume();
                        let mut s = session.borrow_mut();
                        s.viewport.pointer_down = false;
                        let tool = s.active_tool;
                        s.tool.cancel(tool);
                        if s.machine_preview_enabled
                            || s.mode == crate::session::EditorMode::Interact
                        {
                            s.engine_pointer_leave();
                        }
                        if s.viewport.pan_last.is_some() {
                            s.viewport.end_pan();
                            request_frame();
                        } else {
                            request_frame();
                        }
                    }
                })
                .on_pointer_leave({
                    let session = session.clone();
                    move |_pe: PointerEvent| {
                        let mut s = session.borrow_mut();
                        if s.machine_preview_enabled
                            || s.mode == crate::session::EditorMode::Interact
                        {
                            s.engine_pointer_leave();
                        }
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

                if s.viewport.show_grid {
                    paint_grid(scope, comp, &s.viewport);
                }
                if s.viewport.show_guides {
                    paint_guides(scope, &s.viewport.view, &s.viewport.guides);
                }

                let scene = s.engine.scene().clone();
                let view = s.viewport.view;
                let prepared = s.renderer.prepare(&scene, &view);
                s.renderer.paint_prepared(&prepared, scope);

                let overlay = {
                    let ctx = ToolContext {
                        doc: &s.file.document,
                        scene: &scene,
                        comp: s.file.document.main,
                        selection: &s.selection,
                        playhead: renamite_animation::Frame(s.playback.head as i64),
                        record: s.record,
                        view,
                        snap: SnapConfig {
                            grid: (s.viewport.show_grid && s.viewport.snapping_enabled)
                                .then_some(s.viewport.grid_spacing.x.max(1e-6)),
                            anchor: s.viewport.snapping_enabled,
                            guide: s.viewport.show_guides && s.viewport.snapping_enabled,
                        },
                        modifiers: Modifiers::none(),
                        current_paint: &s.current_paint,
                    };
                    s.tool.overlay(s.active_tool, &ctx)
                };
                paint_overlay(scope, &overlay, &view);
            },
        )
    };

    ZStack(Modifier::new().fill_max_size()).child((
        main_view,
        ViewportStageHud(session.clone()),
        ViewportHint(session.clone()),
        ViewportControls(session),
    ))
}

fn HudSurface(content: View) -> View {
    Box(Modifier::new()
        .background(theme().surface_container_high)
        .clip_rounded(12.0)
        .border(1.0, theme().outline_variant, 12.0))
    .child(content)
}

fn ViewportStageHud(session: SessionRef) -> View {
    let (w, h, frame, tool_label, sel_count, record, is_interact, states) = {
        let s = session.borrow();
        let comp = &s.file.document.compositions[s.file.document.main];
        let label = match s.active_tool {
            renamite_history::ToolId::Select => "Select",
            renamite_history::ToolId::Transform => "Transform",
            renamite_history::ToolId::Pen => "Pen",
            renamite_history::ToolId::PathEdit => "Path Edit",
            renamite_history::ToolId::Rect => "Rectangle",
            renamite_history::ToolId::Ellipse => "Ellipse",
            renamite_history::ToolId::Star => "Star",
            renamite_history::ToolId::Text => "Text",
            renamite_history::ToolId::Gradient => "Gradient",
            renamite_history::ToolId::Fill => "Fill",
            renamite_history::ToolId::Dropper => "Dropper",
        };
        (
            comp.size.0,
            comp.size.1,
            s.playback.head.round() as i64,
            label,
            s.selection.nodes.len(),
            s.record,
            s.mode == crate::session::EditorMode::Interact,
            s.engine.active_machine_states(),
        )
    };

    Box(Modifier::new()
        .absolute()
        .offset(Some(16.0), Some(16.0), None, None))
    .child(HudSurface(
        Row(Modifier::new()
            .padding(8.0)
            .gap(8.0)
            .align_items(AlignItems::CENTER))
        .child(if is_interact {
            let names = states
                .as_deref()
                .map(|st| {
                    let s = session.borrow();
                    st.iter()
                        .enumerate()
                        .filter_map(|(li, si)| {
                            s.active_machine.and_then(|id| {
                                s.file
                                    .machines
                                    .get(id)
                                    .and_then(|m| m.layers.get(li))
                                    .and_then(|l| l.states.get(*si))
                                    .map(|st| st.name.clone())
                            })
                        })
                        .collect::<Vec<_>>()
                        .join(" · ")
                })
                .unwrap_or_default();
            vec![
                Text("▶ Interact · Preview")
                    .size(theme().typography.label_medium)
                    .color(theme().primary),
                Text(if names.is_empty() {
                    "waiting".into()
                } else {
                    names
                })
                .size(theme().typography.label_small)
                .color(theme().on_surface_variant),
            ]
        } else if record {
            vec![
                Text("● REC")
                    .size(theme().typography.label_medium)
                    .color(theme().error),
                Text(format!("Frame {frame}"))
                    .size(theme().typography.label_small)
                    .color(theme().on_surface_variant),
            ]
        } else {
            vec![
                Text("Main").size(theme().typography.label_medium),
                Text(format!("{w}×{h}"))
                    .size(theme().typography.label_small)
                    .color(theme().on_surface_variant),
                Text(format!("Frame {frame}"))
                    .size(theme().typography.label_small)
                    .color(theme().on_surface_variant),
                Text(tool_label)
                    .size(theme().typography.label_small)
                    .color(theme().primary),
                Text(format!("{sel_count} selected"))
                    .size(theme().typography.label_small)
                    .color(theme().on_surface_variant),
            ]
        }),
    ))
}

fn ViewportHint(session: SessionRef) -> View {
    if crate::shell::platform_shell_class() == crate::shell::ShellClass::Compact {
        return ZStack(Modifier::new());
    }
    let is_interact = session.borrow().mode == crate::session::EditorMode::Interact;
    let text = if is_interact {
        "Click shapes to fire listeners · Alt+click to select · Preview inputs in the panel"
    } else {
        "Middle or Space drag to pan · Wheel or +/- zoom · F fits · S/V select · B/P pen · N nodes · R/E shapes · T text"
    };
    Box(Modifier::new()
        .absolute()
        .offset(Some(16.0), None, None, Some(16.0)))
    .child(HudSurface(
        Text(text)
            .size(theme().typography.label_small)
            .color(theme().on_surface_variant)
            .modifier(Modifier::new().padding(8.0)),
    ))
}

/// Empty-composition launcher: quick-start actions plus template cards.
fn TemplatePicker(session: SessionRef) -> View {
    let th = theme();
    let panel_w = remember_with_key("welcome_panel_w", || Cell::new(0.0f32));

    let available = welcome_available_width(panel_w.get());
    let content_w = (available - 48.0).max(0.0);

    let cols = launcher_cols(content_w);
    let card_w = launcher_card_width(content_w, cols);
    let tile_cols = launcher_tile_cols(content_w);
    let tile_w = launcher_tile_width(content_w, tile_cols);

    let cards: Vec<View> = renamite_examples::templates()
        .iter()
        .map(|t| TemplateCard(session.clone(), t, card_w))
        .collect();
    let rows: Vec<View> = cards
        .chunks(cols)
        .map(|chunk| Row(Modifier::new().gap(12.0)).child(chunk.to_vec()))
        .collect();

    let tiles: Vec<View> = vec![
        LauncherTile("New", "Create a fresh project", tile_w, {
            let session = session.clone();
            move || crate::file::new_document(&session)
        }),
        LauncherTile("Open", "Open .ren / .renb", tile_w, {
            let session = session.clone();
            move || crate::file::open_document(&session)
        }),
        LauncherTile("Import Lottie", "Bring in JSON animation", tile_w, {
            let session = session.clone();
            move || crate::file::import_lottie(&session)
        }),
        LauncherTile("Import SVG", "Bring in vector artwork", tile_w, {
            let session = session.clone();
            move || crate::file::import_svg(&session)
        }),
    ];
    let tile_rows: Vec<View> = tiles
        .chunks(tile_cols)
        .map(|chunk| Row(Modifier::new().gap(12.0)).child(chunk.to_vec()))
        .collect();

    Column(
        Modifier::new()
            .fill_max_size()
            .padding(24.0)
            .gap(20.0)
            .justify_content(JustifyContent::CENTER)
            .align_items(AlignItems::CENTER)
            .on_size_changed({
                let panel_w = panel_w.clone();
                move |size: repose_core::Vec2| {
                    let w = size.x;
                    if (panel_w.get() - w).abs() > 0.5 {
                        panel_w.set(w);
                        request_frame();
                    }
                }
            }),
    )
    .child(
        Text("Start a Renamite project")
            .size(th.typography.headline_small)
            .color(th.on_surface),
    )
    .child(
        Text("Open an existing file, import artwork, or start from a motion template.")
            .size(th.typography.body_medium)
            .color(th.on_surface_variant),
    )
    .child(Column(Modifier::new().gap(12.0)).child(tile_rows))
    .child(ScrollArea(
        Modifier::new().fill_max_size(),
        remember_scroll_state("template_picker_scroll"),
        Column(
            Modifier::new()
                .fill_max_width()
                .gap(12.0)
                .align_items(AlignItems::CENTER),
        )
        .child(
            Text("Templates")
                .size(th.typography.title_medium)
                .color(th.on_surface),
        )
        .child(Column(Modifier::new().gap(12.0)).child(rows)),
    ))
    .child(
        Text("Or dismiss to a blank canvas")
            .size(th.typography.label_medium)
            .color(th.primary)
            .modifier(Modifier::new().on_pointer_down({
                let session = session.clone();
                move |_| {
                    let mut s = session.borrow_mut();
                    s.welcome = false;
                    s.revision = s.revision.wrapping_add(1);
                    request_frame();
                }
            })),
    )
}

fn welcome_available_width(measured: f32) -> f64 {
    if measured > 0.0 {
        return measured as f64;
    }
    let win = repose_core::get_window_container_width() as f64;
    match crate::shell::platform_shell_class() {
        crate::shell::ShellClass::Expanded => win * 0.55,
        crate::shell::ShellClass::Medium => (win - 416.0).max(0.0),
        crate::shell::ShellClass::Compact => win,
    }
}

fn launcher_cols(content_w: f64) -> usize {
    ((content_w / 272.0).floor() as usize).clamp(1, 4)
}

fn launcher_card_width(content_w: f64, cols: usize) -> f32 {
    let w = (content_w - (cols as f64 - 1.0) * 12.0) / cols as f64;
    w.clamp(120.0, 260.0) as f32
}

fn launcher_tile_cols(content_w: f64) -> usize {
    ((content_w / 192.0).floor() as usize).clamp(1, 4)
}

fn launcher_tile_width(content_w: f64, cols: usize) -> f32 {
    let w = (content_w - (cols as f64 - 1.0) * 12.0) / cols as f64;
    w.clamp(140.0, 180.0) as f32
}

fn LauncherTile(
    title: &'static str,
    subtitle: &'static str,
    width: f32,
    on_click: impl Fn() + 'static,
) -> View {
    let th = theme();
    Box(Modifier::new()
        .width(width)
        .padding(14.0)
        .background(th.surface_container_high)
        .clip_rounded(12.0)
        .on_pointer_down(move |_| on_click()))
    .child(
        Column(Modifier::new().gap(6.0)).child((
            Text(title)
                .size(th.typography.title_small)
                .color(th.on_surface),
            Text(subtitle)
                .size(th.typography.body_small)
                .color(th.on_surface_variant),
        )),
    )
}

fn TemplateCard(
    session: SessionRef,
    template: &'static renamite_examples::TemplateInfo,
    width: f32,
) -> View {
    let th = theme();
    Box(Modifier::new()
        .width(width)
        .padding(14.0)
        .background(th.surface_container_high)
        .clip_rounded(10.0)
        .on_pointer_down({
            let session = session.clone();
            let id = template.id;
            move |_| {
                let file = renamite_examples::build_template(id);
                let mut s = session.borrow_mut();
                s.replace_file(file);
                s.welcome = false;
                s.current_path = None;
                s.dirty = true;
                s.status = Some(format!("Created from \"{}\"", id.display_name()));
            }
        }))
    .child(
        Column(Modifier::new().gap(6.0)).child((
            Text(template.name)
                .size(th.typography.title_small)
                .color(th.on_surface),
            Text(template.description)
                .size(th.typography.body_small)
                .color(th.on_surface_variant),
        )),
    )
}

fn paint_artboard(scope: &mut DrawScope, comp: &Composition, view: &ViewTransform) {
    let th = theme();
    let origin = view.world_to_screen(DVec2::ZERO);
    let width = comp.size.0 as f64 * view.scale;
    let height = comp.size.1 as f64 * view.scale;

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

    let tile_world = 32.0;
    let cols = (comp.size.0 as f64 / tile_world).ceil() as usize;
    let rows = (comp.size.1 as f64 / tile_world).ceil() as usize;

    for y in 0..rows {
        for x in 0..cols {
            let p = view.world_to_screen(DVec2::new(x as f64 * tile_world, y as f64 * tile_world));

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

    let border = th.outline_variant;
    let x = origin.x as f32;
    let y = origin.y as f32;
    let w = width as f32;
    let h = height as f32;

    scope.draw_rect(Rect { x, y, w, h: 1.0 }, border, 0.0);
    scope.draw_rect(
        Rect {
            x,
            y: y + h - 1.0,
            w,
            h: 1.0,
        },
        border,
        0.0,
    );
    scope.draw_rect(Rect { x, y, w: 1.0, h }, border, 0.0);
    scope.draw_rect(
        Rect {
            x: x + w - 1.0,
            y,
            w: 1.0,
            h,
        },
        border,
        0.0,
    );
}

fn paint_grid(scope: &mut DrawScope, comp: &Composition, viewport: &crate::session::ViewportState) {
    let th = theme();
    let view = &viewport.view;
    let base = viewport.grid_spacing.x.max(0.01);
    let mut step = base;
    while step * view.scale < 8.0 {
        step *= 2.0;
    }
    let major_every = (step / base).round().max(1.0) as usize;

    let origin = view.world_to_screen(DVec2::ZERO);
    let width = comp.size.0 as f64 * view.scale;
    let height = comp.size.1 as f64 * view.scale;

    let minor = th.outline_variant.with_alpha(70);
    let major = th.outline_variant.with_alpha(140);

    let cols = (comp.size.0 as f64 / step).floor() as usize;
    let rows = (comp.size.1 as f64 / step).floor() as usize;

    for c in 1..cols {
        let p = view.world_to_screen(DVec2::new(c as f64 * step, 0.0));
        let color = if c % major_every == 0 { major } else { minor };
        scope.draw_rect(
            Rect {
                x: p.x as f32,
                y: origin.y as f32,
                w: 1.0,
                h: height as f32,
            },
            color,
            0.0,
        );
    }
    for r in 1..rows {
        let p = view.world_to_screen(DVec2::new(0.0, r as f64 * step));
        let color = if r % major_every == 0 { major } else { minor };
        scope.draw_rect(
            Rect {
                x: origin.x as f32,
                y: p.y as f32,
                w: width as f32,
                h: 1.0,
            },
            color,
            0.0,
        );
    }
}

fn paint_guides(scope: &mut DrawScope, view: &ViewTransform, guides: &[crate::session::Guide]) {
    let color = theme().tertiary.with_alpha(180);
    for guide in guides {
        match guide.axis {
            crate::session::GuideAxis::Horizontal => {
                let p = view.world_to_screen(DVec2::new(0.0, guide.position));
                scope.draw_rect(
                    Rect {
                        x: 0.0,
                        y: p.y as f32 - 0.5,
                        w: scope.size.width,
                        h: 1.0,
                    },
                    color,
                    0.0,
                );
            }
            crate::session::GuideAxis::Vertical => {
                let p = view.world_to_screen(DVec2::new(guide.position, 0.0));
                scope.draw_rect(
                    Rect {
                        x: p.x as f32 - 0.5,
                        y: 0.0,
                        w: 1.0,
                        h: scope.size.height,
                    },
                    color,
                    0.0,
                );
            }
        }
    }
}

fn ViewportControls(session: SessionRef) -> View {
    let zoom = session.borrow().viewport.view.scale * 100.0;

    Box(Modifier::new()
        .absolute()
        .offset(None, None, Some(16.0), Some(16.0)))
    .child(HudSurface(
        Row(Modifier::new()
            .align_items(AlignItems::CENTER)
            .gap(2.0)
            .padding(4.0))
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
    ))
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

fn to_screen_rect(min: DVec2, max: DVec2, view: &ViewTransform) -> Rect {
    let a = view.world_to_screen(min);
    let b = view.world_to_screen(max);
    Rect {
        x: a.x as f32,
        y: a.y as f32,
        w: (b.x - a.x) as f32,
        h: (b.y - a.y) as f32,
    }
}

fn paint_overlay(scope: &mut DrawScope, overlay: &ToolOverlay, view: &ViewTransform) {
    let th = theme();
    let primary = th.primary;
    match overlay {
        ToolOverlay::None => {}
        ToolOverlay::RubberBand { min, max } => {
            let r = to_screen_rect(*min, *max, view);
            scope.draw_rect_stroke(r, primary.with_alpha(180), 0.0, 1.0);
        }
        ToolOverlay::Selection {
            min,
            max,
            rotate,
            scale,
            pivot,
        } => {
            let r = to_screen_rect(*min, *max, view);
            scope.draw_rect_stroke(r, primary.with_alpha(180), 0.0, 1.0);
            for point in [rotate, scale] {
                draw_selection_handle(scope, view.world_to_screen(*point), primary, th.on_primary);
            }
            if let Some(pivot) = pivot {
                draw_pivot(scope, view.world_to_screen(*pivot), th.tertiary);
            }
        }
        ToolOverlay::ShapePreview { min, max, kind } => {
            let r = to_screen_rect(*min, *max, view);
            scope.draw_rect_stroke(r, primary.with_alpha(180), 0.0, 1.0);
            let pts = star_preview_pts(*min, *max, *kind, view);
            draw_polyline_overlay(scope, &pts, primary.with_alpha(180));
        }
        ToolOverlay::PenPreview { anchors, hover, .. } => {
            for a in anchors {
                let sp = view.world_to_screen(a.pos);
                let rect = Rect {
                    x: sp.x as f32 - 4.0,
                    y: sp.y as f32 - 4.0,
                    w: 8.0,
                    h: 8.0,
                };
                scope.draw_rect(rect, th.surface, 0.0);
                scope.draw_rect_stroke(rect, primary, 0.0, 1.0);
            }
            if let Some(h) = hover {
                let sp = view.world_to_screen(*h);
                let rect = Rect {
                    x: sp.x as f32 - 3.0,
                    y: sp.y as f32 - 3.0,
                    w: 6.0,
                    h: 6.0,
                };
                scope.draw_rect(rect, primary.with_alpha(180), 3.0);
            }
        }
        ToolOverlay::PathHandles {
            path,
            extra,
            active_anchor,
        } => {
            let contours = std::iter::once(path).chain(extra.iter());
            for (ci, contour) in contours.enumerate() {
                for (i, a) in contour.anchors.iter().enumerate() {
                    let sp = view.world_to_screen(a.pos);
                    let rect = Rect {
                        x: sp.x as f32 - 4.0,
                        y: sp.y as f32 - 4.0,
                        w: 8.0,
                        h: 8.0,
                    };
                    let active = ci == 0 && *active_anchor == Some(i);
                    if active {
                        scope.draw_rect(rect, primary, 1.0);
                    } else {
                        scope.draw_rect(rect, th.surface, 0.0);
                        scope.draw_rect_stroke(rect, primary, 0.0, 1.0);
                    }

                    if a.tan_in.length_squared() > 1e-12 {
                        let tip = view.world_to_screen(a.pos + a.tan_in);
                        draw_handle_dot(scope, tip, th.tertiary.with_alpha(220));
                    }
                    if a.tan_out.length_squared() > 1e-12 {
                        let tip = view.world_to_screen(a.pos + a.tan_out);
                        draw_handle_dot(scope, tip, th.tertiary.with_alpha(220));
                    }
                }
            }
        }
        ToolOverlay::GradientLine { start, end, radial } => {
            let a = view.world_to_screen(*start);
            let b = view.world_to_screen(*end);
            if (a - b).length() < 1.0 {
                return;
            }
            let dir = b - a;
            let len = dir.length();
            let n = DVec2::new(-dir.y / len, dir.x / len);
            let t = 1.25; // half thickness (px)
            let c = [
                th.primary.0 as f32 / 255.0,
                th.primary.1 as f32 / 255.0,
                th.primary.2 as f32 / 255.0,
                1.0,
            ];
            let quad = [a + n * t, a - n * t, b - n * t, b + n * t];
            let mk = |p: DVec2| repose_core::view::VectorVertex {
                pos: [p.x as f32, p.y as f32],
                color: c,
                uv: [0.0, 0.0],
            };
            let mesh = repose_core::view::VectorMeshData {
                vertices: std::sync::Arc::from([
                    mk(quad[0]),
                    mk(quad[1]),
                    mk(quad[2]),
                    mk(quad[3]),
                ]),
                indices: std::sync::Arc::from([0u32, 1, 2, 0, 2, 3]),
            };
            scope.draw_vector_overlay(std::sync::Arc::from([mesh]));
            draw_handle_dot(scope, a, th.primary.with_alpha(240));
            let end_color = if *radial { th.tertiary } else { th.primary };
            draw_handle_dot(scope, b, end_color.with_alpha(240));
        }
    }
}

fn draw_handle_dot(scope: &mut DrawScope, tip: DVec2, color: Color) {
    let rect = Rect {
        x: tip.x as f32 - 2.5,
        y: tip.y as f32 - 2.5,
        w: 5.0,
        h: 5.0,
    };
    scope.draw_rect(rect, color, 5.0);
}

fn draw_selection_handle(scope: &mut DrawScope, point: DVec2, stroke: Color, fill: Color) {
    let rect = Rect {
        x: point.x as f32 - 4.0,
        y: point.y as f32 - 4.0,
        w: 8.0,
        h: 8.0,
    };

    scope.draw_rect(rect, fill, 2.0);
    scope.draw_rect_stroke(rect, stroke, 2.0, 1.0);
}

fn draw_pivot(scope: &mut DrawScope, point: DVec2, color: Color) {
    let horizontal = Rect {
        x: point.x as f32 - 7.0,
        y: point.y as f32 - 1.0,
        w: 14.0,
        h: 2.0,
    };

    let vertical = Rect {
        x: point.x as f32 - 1.0,
        y: point.y as f32 - 7.0,
        w: 2.0,
        h: 14.0,
    };

    scope.draw_rect(horizontal, color, 1.0);
    scope.draw_rect(vertical, color, 1.0);

    scope.draw_rect_stroke(
        Rect {
            x: point.x as f32 - 4.0,
            y: point.y as f32 - 4.0,
            w: 8.0,
            h: 8.0,
        },
        color,
        4.0,
        1.0,
    );
}

fn star_preview_pts(
    min: DVec2,
    max: DVec2,
    kind: ShapePreviewKind,
    view: &ViewTransform,
) -> Vec<DVec2> {
    let (points, star) = match kind {
        ShapePreviewKind::Star => (5.0f64, true),
        ShapePreviewKind::Polygon => (6.0f64, false),
        _ => return vec![],
    };
    let center = (min + max) * 0.5;
    let outer = (max.x - min.x).abs().min((max.y - min.y).abs()) * 0.5;
    let inner = outer * 0.4;
    let pts = points.round().max(3.0) as usize;
    let n = if star { pts * 2 } else { pts };
    let step = std::f64::consts::TAU / pts as f64;
    let base = -std::f64::consts::FRAC_PI_2;
    let mut out = Vec::with_capacity(n + 1);
    for k in 0..n {
        let ang = if star {
            step * 0.5 * k as f64
        } else {
            step * k as f64
        };
        let r = if star && k % 2 == 1 { inner } else { outer };
        out.push(view.world_to_screen(DVec2::new(
            center.x + r * (base + ang).cos(),
            center.y + r * (base + ang).sin(),
        )));
    }
    if let Some(&first) = out.first() {
        out.push(first);
    }
    out
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
