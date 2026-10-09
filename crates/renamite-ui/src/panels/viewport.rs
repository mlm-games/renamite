use glam::DVec2;
use renamite_behavior_canvas::{CanvasEvent, PointerButton, ShapePreviewKind, ToolOverlay};
use renamite_behavior_common::{Modifiers, SnapConfig, ToolContext, ViewTransform};
use renamite_model::Composition;
use repose_canvas::{Canvas, DrawScope};
use repose_core::geometry::Rect;
use repose_core::input::{KeyEvent, PointerEvent, PointerEventKind, PointerKind};
use repose_core::{
    AlignItems, Color, CursorIcon, Dp, FocusRequester, JustifyContent, Modifier, Overflow, Px,
    View, remember_auto, remember_with_key, request_frame, theme,
};
use repose_ui::scroll::{
    HorizontalScrollArea, ScrollArea, remember_horizontal_scroll_state, remember_scroll_state,
};
use repose_ui::{Box, Column, Row, Text, TextStyle, ViewExt, ZStack};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use crate::components::CompactIconAction;
use crate::session::{
    ContextMenuSource, ContextMenuState, SessionRef, dispatch_canvas, map_modifiers,
    overlay_anchor, pe_pos,
};
use crate::symbols::Symbols;
use renamite_behavior_common::context_menu::{MenuContext, canvas_menu};

/// How long a finger must rest on the stage before it counts as a right
/// click. Matches the framework's own drag-and-drop long-press threshold.
const LONG_PRESS_MS: u64 = 400;
/// Movement past this (dp) during the wait means the user is panning or
/// dragging, not asking for a menu.
const LONG_PRESS_SLOP: f64 = 10.0;

/// A touch press waiting to become a context menu, or one already turned into
/// one. `fired` is shared with the timer closure, which sets it before opening
/// the menu so the pointer-up that follows cannot also act on the canvas.
struct LongPress {
    pos: DVec2,
    fired: Rc<Cell<bool>>,
    timer: repose_core::timer::TimerHandle,
}

type LongPressState = Rc<RefCell<Option<LongPress>>>;

fn cancel_long_press(state: &LongPressState) {
    if let Some(press) = state.borrow_mut().take() {
        press.timer.cancel();
    }
}

/// Arm the context-menu long press. Only touch arms it: a mouse already has a
/// right button, and a second finger cancels the pending press so a two-finger
/// pan/zoom is never mistaken for a long press.
fn arm_long_press(state: &LongPressState, session: &SessionRef, pe: &PointerEvent) {
    if pe.kind != PointerKind::Touch {
        cancel_long_press(state);
        return;
    }
    // A second finger down means the first one's press was the opening move
    // of a two-finger pan/zoom, not a request for a menu. Drop it rather
    // than re-arming the timer under the new finger.
    if state.borrow().is_some() {
        cancel_long_press(state);
        return;
    }
    let pos = pe_pos(pe);
    let screen = overlay_anchor(pe);
    let fired = Rc::new(Cell::new(false));
    let timer = {
        let session = session.clone();
        let fired = fired.clone();
        repose_core::timer::delay(Duration::from_millis(LONG_PRESS_MS), move || {
            fired.set(true);
            let mut s = session.borrow_mut();
            let world = s.viewport.view.screen_to_world(pos);
            open_canvas_context_menu(&mut s, world, screen, false);
        })
    };
    *state.borrow_mut() = Some(LongPress { pos, fired, timer });
}

/// Drop the pending press once the finger travels far enough to be a pan.
fn track_long_press_move(state: &LongPressState, pe: &PointerEvent) {
    let beyond_slop = state
        .borrow()
        .as_ref()
        .is_some_and(|press| (pe_pos(pe) - press.pos).length() > LONG_PRESS_SLOP);
    if beyond_slop {
        cancel_long_press(state);
    }
}

/// Returns true when the press already opened the menu, in which case the
/// pointer-up must not reach the canvas tools.
fn finish_long_press(state: &LongPressState) -> bool {
    let fired = state
        .borrow()
        .as_ref()
        .is_some_and(|press| press.fired.get());
    cancel_long_press(state);
    fired
}

pub fn ViewportPanel(session: SessionRef) -> View {
    let draw_session = session.clone();
    let focus = remember_auto("focus", FocusRequester::new);
    let long_press = remember_auto("long_press", || Rc::new(RefCell::new(None::<LongPress>)));

    let show_template_picker = { session.borrow().welcome };

    let main_view = if show_template_picker {
        TemplatePicker(session.clone())
    } else {
        let panning = session.borrow().viewport.pan_last.is_some();
        let space_armed = session.borrow().viewport.space_held;
        Canvas(
            Modifier::new()
                .fill_max_size()
                .background(theme().surface_container_lowest)
                .overflow(Overflow::Clip)
                .focusable(true)
                .focus_requester((*focus).clone())
                .on_globally_positioned({
                    let session = session.clone();
                    move |r: Rect| session.borrow_mut().viewport.screen_rect = Some(rect_to_px(r))
                })
                .cursor(if panning {
                    CursorIcon::Grabbing
                } else if space_armed {
                    CursorIcon::Hidden
                } else {
                    CursorIcon::Default
                })
                .on_scroll({
                    let session = session.clone();
                    move |delta: repose_core::Vec2| {
                        let (is_tool_drag, touch_count, canvas_press) = {
                            let s = session.borrow();
                            (
                                s.viewport.pan_last.is_some() || s.tool.is_dragging(s.active_tool),
                                s.touch_count(),
                                s.viewport.pointer_down,
                            )
                        };
                        if is_tool_drag {
                            return repose_core::Vec2::ZERO;
                        }
                        if touch_count > 1 {
                            // Two fingers are a pinch, and the platform already
                            // turns them into a zoom or pan action. Swallowing
                            // the delta here is what keeps a pinch from moving
                            // the view while it scales it.
                            return repose_core::Vec2::ZERO;
                        }
                        if touch_count == 1 && !canvas_press {
                            // Scroll handlers chain down the hit regions under
                            // the finger, each returning what it did not use. A
                            // side rail is a vertical scroller, so it eats
                            // delta.y and hands us delta.x - which used to drag
                            // the canvas sideways while the rail scrolled.
                            // Panning belongs to a drag that began on the
                            // canvas, so ignore the axis nobody claimed.
                            return repose_core::Vec2::ZERO;
                        }
                        let mut s = session.borrow_mut();
                        if touch_count == 1 {
                            // A finger drag on the canvas is a pan on both
                            // axes, never a zoom: pinch is the two-finger
                            // gesture.
                            s.viewport.view.offset += DVec2::new(delta.x as f64, delta.y as f64);
                        } else if delta.y.abs() < delta.x.abs() {
                            // Horizontal wheel: pan the canvas along X.
                            s.viewport.view.offset += DVec2::new(delta.x as f64, 0.0);
                        } else {
                            let anchor = if s.viewport.has_pointer {
                                s.viewport.last_pointer
                            } else {
                                s.viewport.surface_size() * 0.5
                            };
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
                .on_action({
                    let session = session.clone();
                    move |action: repose_core::shortcuts::Action| {
                        handle_viewport_gesture(&session, &action)
                    }
                })
                .on_pointer_down({
                    let session = session.clone();
                    let long_press = long_press.clone();
                    move |pe: PointerEvent| {
                        arm_long_press(&long_press, &session, &pe);
                        let mut s = session.borrow_mut();
                        let pos = pe_pos(&pe);
                        s.viewport.last_pointer = pos;
                        s.viewport.has_pointer = true;
                        s.viewport.touch_drag = pe.kind == PointerKind::Touch;
                        // A fresh press means any previous gesture is over, even
                        // if its lift landed off-surface and never reached here.
                        s.viewport.gesture_anchor.end();

                        if map_button(&pe) == PointerButton::Secondary {
                            focus.request_focus();
                            let world = s.viewport.view.screen_to_world(pos);
                            let screen = overlay_anchor(&pe);
                            open_canvas_context_menu(&mut s, world, screen, pe.modifiers.shift);
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

                        if map_button(&pe) == PointerButton::Primary
                            && s.mode != crate::session::EditorMode::Interact
                            && !s.machine_preview_enabled
                        {
                            let ruler = 20.0;
                            let surface = s.viewport.surface_size();
                            let in_top_ruler = pos.y >= 0.0
                                && pos.y < ruler
                                && pos.x >= ruler
                                && pos.x < surface.x.max(ruler);
                            let in_left_ruler = pos.x >= 0.0
                                && pos.x < ruler
                                && pos.y >= ruler
                                && pos.y < surface.y.max(ruler);
                            if in_top_ruler || in_left_ruler {
                                pe.consume();
                                s.viewport.begin_guide_drag_new(if in_top_ruler {
                                    crate::session::GuideAxis::Horizontal
                                } else {
                                    crate::session::GuideAxis::Vertical
                                });
                                s.viewport.update_guide_drag(pos);
                                request_frame();
                                return;
                            }
                            if let Some(index) = s.viewport.guide_hit(pos) {
                                pe.consume();
                                s.viewport.begin_guide_drag_existing(index);
                                request_frame();
                                return;
                            }
                        }

                        focus.request_focus();
                        s.viewport.pointer_down = true;
                        if s.renaming.is_some() {
                            s.commit_rename();
                            s.renaming = None;
                        }
                        let world = s.viewport.view.screen_to_world(pos);
                        if s.mode == crate::session::EditorMode::Interact
                            || s.machine_preview_enabled
                        {
                            let to_engine = !pe.modifiers.alt;
                            s.viewport.pointer_route = Some(to_engine);
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
                        s.viewport.pointer_route = Some(false);
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
                    let long_press = long_press.clone();
                    move |pe: PointerEvent| {
                        track_long_press_move(&long_press, &pe);
                        let mut s = session.borrow_mut();
                        let pos = pe_pos(&pe);
                        s.viewport.last_pointer = pos;
                        s.viewport.has_pointer = true;

                        if s.viewport.update_pan(pos) {
                            pe.consume();
                            request_frame();
                            return;
                        }

                        if s.viewport.guide_drag.is_some() {
                            pe.consume();
                            s.viewport.update_guide_drag(pos);
                            request_frame();
                            return;
                        }

                        let world = s.viewport.view.screen_to_world(pos);
                        let to_engine = match s.viewport.pointer_route {
                            Some(v) => v,
                            None => {
                                (s.mode == crate::session::EditorMode::Interact
                                    || s.machine_preview_enabled)
                                    && !pe.modifiers.alt
                            }
                        };
                        if to_engine
                            && (s.mode == crate::session::EditorMode::Interact
                                || s.machine_preview_enabled)
                        {
                            s.engine_pointer_move(world);
                            return;
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
                    let long_press = long_press.clone();
                    move |pe: PointerEvent| {
                        if finish_long_press(&long_press) {
                            // The menu is already up; letting the lift reach the
                            // tools would retarget the selection behind it.
                            pe.consume();
                            return;
                        }
                        let mut s = session.borrow_mut();
                        s.viewport.pointer_down = false;
                        s.viewport.touch_drag = false;
                        s.viewport.gesture_anchor.end();

                        if s.viewport.guide_drag.is_some() {
                            let surface = s.viewport.surface_size();
                            let pos = pe_pos(&pe);
                            if s.viewport.end_guide_drag(pos, surface) {
                                s.repaint();
                            }
                            pe.consume();
                            request_frame();
                            s.viewport.pointer_route = None;
                            return;
                        }

                        if s.viewport.pan_last.is_some() {
                            if map_button(&pe) != PointerButton::Secondary {
                                pe.consume();
                                s.viewport.end_pan();
                                request_frame();
                            }
                            s.viewport.pointer_route = None;
                            return;
                        }

                        let world = s.viewport.view.screen_to_world(pe_pos(&pe));
                        let to_engine = match s.viewport.pointer_route.take() {
                            Some(v) => v,
                            None => {
                                (s.mode == crate::session::EditorMode::Interact
                                    || s.machine_preview_enabled)
                                    && !pe.modifiers.alt
                            }
                        };
                        if to_engine
                            && (s.mode == crate::session::EditorMode::Interact
                                || s.machine_preview_enabled)
                        {
                            s.engine_pointer_up(world);
                            return;
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
                    let long_press = long_press.clone();
                    move |pe| {
                        pe.consume();
                        cancel_long_press(&long_press);
                        let mut s = session.borrow_mut();
                        s.viewport.pointer_down = false;
                        s.viewport.pointer_route = None;
                        s.viewport.touch_drag = false;
                        s.viewport.gesture_anchor.end();
                        let tool = s.active_tool;
                        let outs = s.tool.cancel(tool);
                        s.apply_outputs(outs);
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
                paint_rulers(scope, &s.viewport.view, surface);

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
                    let guide_positions: Vec<(bool, f64)> = s
                        .viewport
                        .guides
                        .iter()
                        .map(|g| {
                            (
                                matches!(g.axis, crate::session::GuideAxis::Horizontal),
                                g.position,
                            )
                        })
                        .collect();
                    let ctx = ToolContext {
                        doc: &s.file.document,
                        scene: &scene,
                        comp: s.file.document.main,
                        selection: &s.selection,
                        playhead: renamite_animation::Frame(s.playback.head.round() as i64),
                        record: s.record_for_writes(),
                        view,
                        snap: SnapConfig {
                            grid: (s.viewport.show_grid
                                && s.viewport.snapping_enabled
                                && s.viewport.snap_to_grid)
                                .then_some(s.viewport.grid_spacing.x.max(1e-6)),
                            anchor: s.viewport.snapping_enabled && s.viewport.snap_to_objects,
                            guide: s.viewport.show_guides
                                && s.viewport.snapping_enabled
                                && s.viewport.snap_to_guides,
                        },
                        guides: &guide_positions,
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

/// Two-finger pan and pinch for the stage.
///
/// Routed through the app's single global shortcut handler (see
/// `crate::shortcuts::handle_global_action`). A second
/// `InstallShortcutHandler` installed from this panel never ran: only the most
/// recently installed global handler is consulted, and the shell's
/// keyboard/command handler is installed after the workspace composes, so it
/// shadowed this one. That is why two-finger zoom reached the machine graph -
/// it uses a per-node `on_action`, tried before any global handler - but not
/// the canvas.
pub fn handle_viewport_gesture(
    session: &SessionRef,
    action: &repose_core::shortcuts::Action,
) -> bool {
    use repose_core::shortcuts::{Action, Gesture};
    match action {
        Action::Gesture(Gesture::Pan { delta, center }) => {
            let local = viewport_local_center(session, *center);
            if !claim_canvas_for_gesture(session, Some(*center)) {
                return false;
            }
            {
                let mut s = session.borrow_mut();
                // Latch on the first frame of the gesture, whichever kind it
                // is, so a pinch that starts as a pan still anchors where the
                // fingers first landed.
                s.viewport.gesture_anchor.begin(local);
                s.viewport.view.offset += DVec2::new(delta.x as f64, delta.y as f64);
            }
            request_frame();
            true
        }
        Action::Gesture(Gesture::Pinch { delta_scale }) => {
            if !claim_canvas_for_gesture(session, None) {
                return false;
            }
            let center_in_viewport = {
                let s = session.borrow();
                s.viewport.surface_size() * 0.5
            };
            {
                let mut s = session.borrow_mut();
                s.viewport.zoom_at(center_in_viewport, *delta_scale as f64);
            }
            request_frame();
            true
        }
        Action::Gesture(Gesture::PinchWithCenter {
            delta_scale,
            center,
        }) => {
            let local = viewport_local_center(session, *center);
            if !claim_canvas_for_gesture(session, Some(*center)) {
                return false;
            }
            {
                let mut s = session.borrow_mut();
                s.viewport.gesture_anchor.begin(local);
                let anchor = s
                    .viewport
                    .gesture_anchor
                    .resolve(s.viewport.surface_size() * 0.5);
                s.viewport.zoom_at(anchor, *delta_scale as f64);
            }
            request_frame();
            true
        }
        _ => false,
    }
}

/// The gesture centre (window px) in main-viewport-local px. Falls back to the
/// surface centre before the canvas has ever been laid out.
fn viewport_local_center(session: &SessionRef, center: repose_core::Vec2) -> DVec2 {
    let s = session.borrow();
    match s.viewport.screen_rect {
        Some(rect) => DVec2::new((center.x - rect.x) as f64, (center.y - rect.y) as f64),
        None => s.viewport.surface_size() * 0.5,
    }
}

/// Scale a layout rect (dp) into the physical-px space the viewport
/// transform and the platform's gesture centres live in.
fn rect_to_px(r: Rect) -> Rect {
    let scale = repose_core::locals::effective_density_scale() as f64;
    Rect {
        x: (r.x as f64 * scale) as f32,
        y: (r.y as f64 * scale) as f32,
        w: (r.w as f64 * scale) as f32,
        h: (r.h as f64 * scale) as f32,
    }
}

/// Decide whether a two-finger gesture may drive the stage, taking the pointer
/// from whatever drag holds it.
///
/// The framework only emits these gestures for two or more simultaneous
/// contacts, and every finger dispatches its own primary press on the way
/// down. So a canvas tool is normally *already* armed by the time a gesture
/// arrives - in design mode the first finger's press alone starts a rubber
/// band or a node drag. Yielding to that guard would drop every pinch and
/// two-finger pan, leaving the stage navigable only in Interact mode, where
/// the press never reaches a tool.
///
/// On touch the collision is not intent: two fingers mean navigate. The drag
/// is cancelled - which rolls back the transaction it opened, so lifting a
/// finger cannot commit a half-finished move - and the gesture takes over.
///
/// A drag the user started with a mouse or stylus keeps priority and the
/// gesture yields, as does one centred on the machine graph, which owns its
/// own per-node handler.
fn claim_canvas_for_gesture(session: &SessionRef, center: Option<repose_core::Vec2>) -> bool {
    if center.is_some_and(|c| viewport_gesture_in_graph(&session.borrow().viewport, c)) {
        return false;
    }
    let mut s = session.borrow_mut();
    let held = s.tool.is_dragging(s.active_tool) || s.viewport.pan_last.is_some();
    if !held || !s.viewport.touch_drag {
        return !held;
    }
    let tool = s.active_tool;
    let outs = s.tool.cancel(tool);
    s.apply_outputs(outs);
    s.viewport.end_pan();
    s.viewport.pointer_down = false;
    s.viewport.pointer_route = None;
    s.viewport.gesture_anchor.end();
    true
}

/// Select whatever is under `world`, then open the canvas context menu there.
///
/// Shared by the right-click path and the touch long-press, so a long press
/// menus the same target a right click would. `extend` keeps the current
/// selection when nothing was hit (the right-click Shift behaviour).
fn open_canvas_context_menu(
    s: &mut crate::session::Session,
    world: DVec2,
    screen: DVec2,
    extend: bool,
) {
    let scene = s.engine.scene().clone();
    let comp = s.file.document.main;
    if let Some(id) = renamite_model::pick_selectable(&s.file.document, &scene, comp, world) {
        if !s.selection.nodes.contains(&id) {
            s.selection.nodes = vec![id];
        }
    } else if !extend {
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
        screen_pos: screen,
        entries,
        source: ContextMenuSource::Canvas { world },
    });
}

fn HudSurface(content: View) -> View {
    Box(Modifier::new()
        .background(theme().surface_container_high)
        .clip_rounded(Dp(12.0))
        .border(Dp(1.0), theme().outline_variant, Dp(12.0)))
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

    // On compact the tool palette floats over the stage's left edge, so the
    // readout moves right rather than being painted over.
    let anchor = if crate::shell::platform_shell_class() == crate::shell::ShellClass::Compact {
        Modifier::new()
            .absolute()
            .offset(None, Some(Dp(16.0)), Some(Dp(16.0)), None)
    } else {
        Modifier::new()
            .absolute()
            .offset(Some(Dp(16.0)), Some(Dp(16.0)), None, None)
    };

    Box(anchor).child(HudSurface(
        Row(Modifier::new()
            .padding(Dp(8.0))
            .gap(Dp(8.0))
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
                        .join(", ")
                })
                .unwrap_or_default();
            vec![
                Text("▶ Interact, Preview")
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
                Text(format!("{w}x{h}"))
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
    let shown = {
        let s = session.borrow();
        s.show_hints() && crate::shell::platform_shell_class() != crate::shell::ShellClass::Compact
    };
    if !shown {
        return ZStack(Modifier::new());
    }
    let is_interact = session.borrow().mode == crate::session::EditorMode::Interact;
    let text = if is_interact {
        "Alt+click to select, click fires"
    } else {
        "Middle or Space drag to pan, Wheel or +/- zoom, F fits, S/V select, N path edit, B/P pen, R/E shapes, * star, T text, G gradient, U fill, D pick"
    };
    Box(Modifier::new()
        .absolute()
        .offset(Some(Dp(16.0)), None, None, Some(Dp(16.0))))
    .child(HudSurface(
        Text(text)
            .size(theme().typography.label_small)
            .color(theme().on_surface_variant)
            .modifier(Modifier::new().padding(Dp(8.0))),
    ))
}

/// Empty-composition launcher: quick-start actions plus template cards.
fn TemplatePicker(session: SessionRef) -> View {
    let th = theme();
    let panel_w = remember_auto("panel_w", || Cell::new(0.0f32));

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
        .map(|chunk| Row(Modifier::new().gap(Dp(12.0))).child(chunk.to_vec()))
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
        .map(|chunk| Row(Modifier::new().gap(Dp(12.0))).child(chunk.to_vec()))
        .collect();

    Column(
        Modifier::new()
            .fill_max_size()
            .padding(Dp(24.0))
            .gap(Dp(20.0))
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
    .child(Column(Modifier::new().gap(Dp(12.0))).child(tile_rows))
    .child(ScrollArea(
        Modifier::new().fill_max_size(),
        remember_scroll_state("template_picker_scroll"),
        Column(
            Modifier::new()
                .fill_max_width()
                .gap(Dp(12.0))
                .align_items(AlignItems::CENTER),
        )
        .child(
            Text("Templates")
                .size(th.typography.title_medium)
                .color(th.on_surface),
        )
        .child(Column(Modifier::new().gap(Dp(12.0))).child(rows)),
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
        .width(Dp(width))
        .padding(Dp(14.0))
        .background(th.surface_container_high)
        .clip_rounded(Dp(12.0))
        .on_pointer_down(move |_| on_click()))
    .child(
        Column(Modifier::new().gap(Dp(6.0))).child((
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
        .width(Dp(width))
        .padding(Dp(14.0))
        .background(th.surface_container_high)
        .clip_rounded(Dp(10.0))
        .on_pointer_down({
            let session = session.clone();
            let id = template.id;
            move |_| {
                let file = renamite_examples::build_template(id);
                let mut s = session.borrow_mut();
                s.replace_file(file);
                s.welcome = false;
                s.current_path = None;
                s.mark_dirty();
                s.status = Some(format!("Created from \"{}\"", id.display_name()));
            }
        }))
    .child(
        Column(Modifier::new().gap(Dp(6.0))).child((
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
    renamite_render_bridge::SceneRenderer::paint_artboard_chrome(
        scope,
        DVec2::new(comp.size.0 as f64, comp.size.1 as f64),
        view,
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
            Px(0.0),
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
            Px(0.0),
        );
    }
}

fn paint_rulers(scope: &mut DrawScope, view: &ViewTransform, surface: DVec2) {
    const RULER: f64 = 20.0;
    if surface.x < RULER * 2.0 || surface.y < RULER * 2.0 {
        return;
    }
    let th = theme();
    let bg = th.surface_container_high;
    let tick = th.on_surface_variant.with_alpha(140);
    scope.draw_rect(
        Rect {
            x: 0.0,
            y: 0.0,
            w: surface.x as f32,
            h: RULER as f32,
        },
        bg,
        Px(0.0),
    );
    scope.draw_rect(
        Rect {
            x: 0.0,
            y: 0.0,
            w: RULER as f32,
            h: surface.y as f32,
        },
        bg,
        Px(0.0),
    );
    let step = ruler_step(view.scale);
    if step <= 0.0 {
        return;
    }
    let x0 = view.screen_to_world(DVec2::new(RULER, 0.0)).x;
    let x1 = view.screen_to_world(DVec2::new(surface.x, 0.0)).x;
    let mut x = (x0 / step).floor() * step;
    while x <= x1 {
        let p = view.world_to_screen(DVec2::new(x, 0.0));
        if p.x >= RULER {
            let major = (x / (step * 5.0)).fract().abs() < 1e-9;
            let h = if major { 10.0 } else { 5.0 };
            scope.draw_rect(
                Rect {
                    x: p.x as f32,
                    y: (RULER - h) as f32,
                    w: 1.0,
                    h: h as f32,
                },
                tick,
                Px(0.0),
            );
        }
        x += step;
    }
    let y0 = view.screen_to_world(DVec2::new(0.0, RULER)).y;
    let y1 = view.screen_to_world(DVec2::new(0.0, surface.y)).y;
    let mut y = (y0 / step).floor() * step;
    while y <= y1 {
        let p = view.world_to_screen(DVec2::new(0.0, y));
        if p.y >= RULER {
            let major = (y / (step * 5.0)).fract().abs() < 1e-9;
            let w = if major { 10.0 } else { 5.0 };
            scope.draw_rect(
                Rect {
                    x: (RULER - w) as f32,
                    y: p.y as f32,
                    w: w as f32,
                    h: 1.0,
                },
                tick,
                Px(0.0),
            );
        }
        y += step;
    }
    scope.draw_rect(
        Rect {
            x: 0.0,
            y: 0.0,
            w: RULER as f32,
            h: RULER as f32,
        },
        th.surface_container,
        Px(0.0),
    );
}

fn ruler_step(scale: f64) -> f64 {
    let target_px = 80.0;
    let raw = target_px / scale.max(1e-6);
    let mag = 10f64.powf(raw.log10().floor().clamp(-9.0, 9.0));
    for m in [1.0, 2.0, 5.0, 10.0] {
        if mag * m >= raw {
            return mag * m;
        }
    }
    mag * 10.0
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
                    Px(0.0),
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
                    Px(0.0),
                );
            }
        }
    }
}

/// Bottom-of-stage space the floating controls bar occupies, published so the
/// compact tool palette can stop above it instead of running underneath.
///
/// Seeded from the bar's own metrics (48dp button + 2x4dp padding + 16dp margin
/// + 8dp gap) so the first frame is already clear, then corrected from the
/// measured row - if the bar grows or shrinks, the palette follows.
pub fn controls_bottom_clearance() -> Rc<Cell<f32>> {
    const SEEDED: f32 = 80.0;
    remember_with_key("viewport_controls_clearance", || Cell::new(SEEDED))
}

fn ViewportControls(session: SessionRef) -> View {
    let clearance = controls_bottom_clearance();
    let (zoom, snapping, snap_grid, snap_guides, snap_objects, max_width) = {
        let s = session.borrow();
        (
            s.viewport.view.scale * 100.0,
            s.viewport.snapping_enabled,
            s.viewport.snap_to_grid,
            s.viewport.snap_to_guides,
            s.viewport.snap_to_objects,
            // The bar is right-anchored, so a row wider than the stage runs
            // off the left. Bound it to the stage and let it scroll instead.
            stage_content_width(s.viewport.screen_rect),
        )
    };

    let scroller = Modifier::new();
    let scroller = match max_width {
        Some(w) => scroller.max_width(Dp(w)),
        None => scroller,
    };

    Box(Modifier::new()
        .absolute()
        .offset(None, None, Some(Dp(16.0)), Some(Dp(16.0)))
        .on_globally_positioned({
            let clearance = clearance.clone();
            move |r: Rect| {
                // Row height + its 16dp bottom margin + an 8dp gap.
                let reserved = r.h + 24.0;
                if (reserved - clearance.get()).abs() > 0.5 {
                    clearance.set(reserved);
                    request_frame();
                }
            }
        }))
    .child(HorizontalScrollArea(
        scroller,
        remember_horizontal_scroll_state("viewport_controls_scroll"),
        HudSurface(
            Row(Modifier::new()
                .align_items(AlignItems::CENTER)
                .gap(Dp(2.0))
                .padding(Dp(4.0)))
            .child((
                crate::components::ToolAction(
                    Symbols::gps_fixed,
                    if snapping {
                        "Snapping on"
                    } else {
                        "Snapping off"
                    },
                    snapping,
                    {
                        let session = session.clone();
                        move || {
                            let mut s = session.borrow_mut();
                            s.viewport.snapping_enabled = !s.viewport.snapping_enabled;
                            s.repaint();
                        }
                    },
                ),
                crate::components::ToolAction(
                    Symbols::grid_on,
                    if snap_grid {
                        "Snap to grid on (Shift+G)"
                    } else {
                        "Snap to grid off (Shift+G)"
                    },
                    snapping && snap_grid,
                    {
                        let session = session.clone();
                        move || {
                            let mut s = session.borrow_mut();
                            s.viewport.snap_to_grid = !s.viewport.snap_to_grid;
                            s.repaint();
                        }
                    },
                ),
                crate::components::ToolAction(
                    Symbols::grid_guides,
                    if snap_guides {
                        "Snap to guides on (Shift+I)"
                    } else {
                        "Snap to guides off (Shift+I)"
                    },
                    snapping && snap_guides,
                    {
                        let session = session.clone();
                        move || {
                            let mut s = session.borrow_mut();
                            s.viewport.snap_to_guides = !s.viewport.snap_to_guides;
                            s.repaint();
                        }
                    },
                ),
                crate::components::ToolAction(
                    Symbols::transform,
                    if snap_objects {
                        "Snap to objects on (Shift+O)"
                    } else {
                        "Snap to objects off (Shift+O)"
                    },
                    snapping && snap_objects,
                    {
                        let session = session.clone();
                        move || {
                            let mut s = session.borrow_mut();
                            s.viewport.snap_to_objects = !s.viewport.snap_to_objects;
                            s.repaint();
                        }
                    },
                ),
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
                    .modifier(Modifier::new().min_width(Dp(52.0))),
                CompactIconAction(Symbols::zoom_in, "Zoom in", {
                    let session = session.clone();
                    move || {
                        session.borrow_mut().viewport.zoom_centered(1.2);
                        request_frame();
                    }
                }),
                CompactIconAction(Symbols::fit_screen, "Fit artboard (F)", {
                    let session = session.clone();
                    move || {
                        let mut s = session.borrow_mut();
                        s.viewport.request_fit();
                        request_frame();
                    }
                }),
            )),
        ),
    ))
}

/// Usable width for the right-anchored controls bar: the stage width less its
/// right margin. `None` until the stage has been laid out, so the bar is never
/// pinned to a zero width on the first frame.
fn stage_content_width(screen_rect: Option<Rect>) -> Option<f32> {
    let rect = screen_rect?;
    let scale = repose_core::locals::effective_density_scale().max(1e-6) as f64;
    let width = (rect.w as f64 / scale) - 32.0;
    (width.is_finite() && width > 0.0).then_some(width as f32)
}

fn viewport_gesture_in_graph(
    viewport: &crate::session::ViewportState,
    center: repose_core::Vec2,
) -> bool {
    let Some(rect) = viewport.screen_rect else {
        return false;
    };
    let local = DVec2::new((center.x - rect.x) as f64, (center.y - rect.y) as f64);
    viewport.graph_rect_at(local)
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

fn model_to_ui_color(c: renamite_model::Color) -> Color {
    Color::from_rgba(
        (c.r.clamp(0.0, 1.0) * 255.0).round() as u8,
        (c.g.clamp(0.0, 1.0) * 255.0).round() as u8,
        (c.b.clamp(0.0, 1.0) * 255.0).round() as u8,
        (c.a.clamp(0.0, 1.0) * 255.0).round() as u8,
    )
}

fn paint_overlay(scope: &mut DrawScope, overlay: &ToolOverlay, view: &ViewTransform) {
    let th = theme();
    let primary = th.primary;
    match overlay {
        ToolOverlay::None => {}
        ToolOverlay::RubberBand { min, max } => {
            let r = to_screen_rect(*min, *max, view);
            scope.draw_rect_stroke(r, primary.with_alpha(110), Px(0.0), Px(1.0));
        }
        ToolOverlay::Selection {
            min,
            max,
            rotate,
            scale,
            pivot,
        } => {
            let r = to_screen_rect(*min, *max, view);
            scope.draw_rect_stroke(r, primary.with_alpha(110), Px(0.0), Px(1.0));
            for point in [rotate, scale] {
                draw_selection_handle(scope, view.world_to_screen(*point), primary, th.on_primary);
            }
            if let Some(pivot) = pivot {
                draw_pivot(scope, view.world_to_screen(*pivot), th.tertiary);
            }
        }
        ToolOverlay::ShapePreview { min, max, kind } => {
            let r = to_screen_rect(*min, *max, view);
            scope.draw_rect_stroke(r, primary.with_alpha(110), Px(0.0), Px(1.0));
            let pts = star_preview_pts(*min, *max, *kind, view);
            draw_polyline_overlay(scope, &pts, primary.with_alpha(110));
        }
        ToolOverlay::PenPreview { anchors, hover, .. } => {
            if anchors.len() >= 2 {
                let path = renamite_geometry::VectorPath {
                    anchors: anchors.clone(),
                    closed: false,
                };
                draw_bezier_overlay(scope, &path.to_bez_path(), view, primary);
            }
            if let Some(h) = hover {
                if let Some(last) = anchors.last() {
                    // Rubber band: where the next anchor would land.
                    draw_polyline_overlay(scope, &[last.pos, *h], primary.with_alpha(150));
                }
                let sp = view.world_to_screen(*h);
                draw_node_marker(scope, sp, primary.with_alpha(150));
            }
            for (i, a) in anchors.iter().enumerate() {
                draw_tangents(scope, view, a, th.tertiary.with_alpha(220));
                let active = hover.is_none() && i + 1 == anchors.len();
                draw_node_marker(
                    scope,
                    view.world_to_screen(a.pos),
                    if active {
                        primary
                    } else {
                        primary.with_alpha(200)
                    },
                );
            }
        }
        ToolOverlay::PathHandles {
            path,
            extra,
            active_anchor,
        } => {
            let contours = std::iter::once(path).chain(extra.iter());
            for (ci, contour) in contours.clone().enumerate() {
                draw_bezier_overlay(
                    scope,
                    &contour.to_bez_path(),
                    view,
                    if ci == 0 {
                        primary.with_alpha(200)
                    } else {
                        primary.with_alpha(90)
                    },
                );
            }
            for (ci, contour) in contours.enumerate() {
                for (i, a) in contour.anchors.iter().enumerate() {
                    let active = ci == 0 && *active_anchor == Some(i);
                    draw_tangents(scope, view, a, th.tertiary.with_alpha(220));
                    let sp = view.world_to_screen(a.pos);
                    if active {
                        draw_filled_node_marker(scope, sp, primary, th.on_primary);
                    } else {
                        draw_node_marker(scope, sp, primary);
                    }
                }
            }
        }
        ToolOverlay::GradientLine {
            start,
            end,
            radial,
            stops,
        } => {
            let a = view.world_to_screen(*start);
            let b = view.world_to_screen(*end);
            if (a - b).length() < 1.0 {
                return;
            }
            let va = repose_core::Vec2 {
                x: a.x as f32,
                y: a.y as f32,
            };
            let vb = repose_core::Vec2 {
                x: b.x as f32,
                y: b.y as f32,
            };
            let brush = repose_core::Brush::Linear {
                start: va,
                end: vb,
                start_color: model_to_ui_color(stops.sample(0.0)),
                end_color: model_to_ui_color(stops.sample(1.0)),
            };
            scope.draw_line_brush(va, vb, brush, Px(2.5), repose_core::StrokeCap::Round);
            draw_handle_dot(scope, a, th.primary.with_alpha(240));
            let end_color = if *radial { th.tertiary } else { th.primary };
            draw_handle_dot(scope, b, end_color.with_alpha(240));
        }
    }
}

fn draw_handle_dot(scope: &mut DrawScope, tip: DVec2, color: Color) {
    let half = dp_px(TANGENT_HALF_DP);
    let rect = Rect {
        x: tip.x as f32 - half,
        y: tip.y as f32 - half,
        w: half * 2.0,
        h: half * 2.0,
    };
    // Radius equal to the full extent keeps it a round dot at any density.
    scope.draw_rect(rect, color, Px(half * 2.0));
}

fn draw_selection_handle(scope: &mut DrawScope, point: DVec2, stroke: Color, fill: Color) {
    let half = dp_px(HANDLE_HALF_DP);
    let rect = Rect {
        x: point.x as f32 - half,
        y: point.y as f32 - half,
        w: half * 2.0,
        h: half * 2.0,
    };

    scope.draw_rect(rect, fill, Px(dp_px(2.0)));
    scope.draw_rect_stroke(rect, stroke, Px(dp_px(2.0)), Px(dp_px(1.0)));
}

fn draw_pivot(scope: &mut DrawScope, point: DVec2, color: Color) {
    let arm = dp_px(PIVOT_HALF_DP);
    let thin = dp_px(1.0);
    let horizontal = Rect {
        x: point.x as f32 - arm,
        y: point.y as f32 - thin,
        w: arm * 2.0,
        h: thin * 2.0,
    };

    let vertical = Rect {
        x: point.x as f32 - thin,
        y: point.y as f32 - arm,
        w: thin * 2.0,
        h: arm * 2.0,
    };

    scope.draw_rect(horizontal, color, Px(0.0));
    scope.draw_rect(vertical, color, Px(0.0));

    scope.draw_rect_stroke(
        Rect {
            x: point.x as f32 - arm,
            y: point.y as f32 - arm,
            w: arm * 2.0,
            h: arm * 2.0,
        },
        color,
        Px(arm * 2.0),
        Px(dp_px(1.0)),
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

/// Overlay chrome is drawn in physical px, so marker sizes are declared in dp
/// and converted with [`crate::session::dp_px`] - each is picked once and
/// follows the display.
use crate::session::dp_px;

/// Half-extent of a path node marker, in dp. Inkscape-sized and legible on a
/// phone, where the old 4px square was barely two millimetres.
const NODE_HALF_DP: f32 = 6.0;
/// Half-extent of a path tangent tip, in dp.
const TANGENT_HALF_DP: f32 = 3.5;
/// Half-extent of a selection rotate/scale corner handle, in dp.
const HANDLE_HALF_DP: f32 = 5.0;
/// Half-extent of the pivot centre square, in dp.
const PIVOT_HALF_DP: f32 = 5.0;

/// Stroke a bezier by flattening it, since the canvas only strokes polylines.
fn draw_bezier_overlay(
    scope: &mut DrawScope,
    path: &renamite_geometry::BezPath,
    view: &ViewTransform,
    color: Color,
) {
    let tolerance = view.world_tolerance(0.5);
    for contour in renamite_geometry::flatten_bez_path(path, tolerance) {
        let pts: Vec<DVec2> = contour
            .points
            .iter()
            .map(|p| view.world_to_screen(*p))
            .collect();
        draw_polyline_overlay(scope, &pts, color);
    }
}

fn draw_tangents(
    scope: &mut DrawScope,
    view: &ViewTransform,
    anchor: &renamite_geometry::Anchor,
    color: Color,
) {
    for tan in [anchor.tan_in, anchor.tan_out] {
        if tan.length_squared() <= 1e-12 {
            continue;
        }
        draw_polyline_overlay(scope, &[anchor.pos, anchor.pos + tan], color);
        draw_handle_dot(scope, view.world_to_screen(anchor.pos + tan), color);
    }
}

fn draw_node_marker(scope: &mut DrawScope, at: DVec2, color: Color) {
    let half = dp_px(NODE_HALF_DP);
    let rect = Rect {
        x: at.x as f32 - half,
        y: at.y as f32 - half,
        w: half * 2.0,
        h: half * 2.0,
    };
    scope.draw_rect(rect, theme().surface, Px(0.0));
    scope.draw_rect_stroke(rect, color, Px(0.0), Px(dp_px(1.0)));
}

fn draw_filled_node_marker(scope: &mut DrawScope, at: DVec2, fill: Color, border: Color) {
    let half = dp_px(NODE_HALF_DP);
    let rect = Rect {
        x: at.x as f32 - half,
        y: at.y as f32 - half,
        w: half * 2.0,
        h: half * 2.0,
    };
    scope.draw_rect(rect, fill, Px(dp_px(1.0)));
    scope.draw_rect_stroke(rect, border, Px(0.0), Px(dp_px(1.0)));
}

fn draw_polyline_overlay(scope: &mut DrawScope, pts: &[DVec2], color: Color) {
    if pts.len() < 2 {
        return;
    }
    let points: Vec<repose_core::Vec2> = pts
        .iter()
        .map(|p| repose_core::Vec2 {
            x: p.x as f32,
            y: p.y as f32,
        })
        .collect();
    scope.draw_line_path(
        points,
        repose_core::Brush::Solid(color),
        Px(dp_px(2.0)),
        repose_core::StrokeCap::Round,
        repose_core::StrokeJoin::Round,
    );
}
