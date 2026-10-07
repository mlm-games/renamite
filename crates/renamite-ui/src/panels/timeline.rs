use glam::DVec2;
use renamite_behavior_timeline::{TimelineEvent, TimelineKey, TimelineOverlay, TimelineRow};
use repose_canvas::{Canvas, DrawScope};
use repose_core::geometry::Rect;
use repose_core::input::{Key, KeyEvent, KeyEventType, PointerEvent, PointerEventKind};
use repose_core::{
    AlignItems, Color, Dp, FocusRequester, JustifyContent, Modifier, Overflow, Px,
    TextFieldLineLimits, Vec2, View, remember_auto, remember_state_auto, theme,
};
use repose_ui::scroll::{ScrollArea, remember_scroll_state};
use repose_ui::textfield::{BasicTextField, TextFieldConfig, TextFieldState};
use repose_ui::{Box, Column, Row, Text, TextStyle, ViewExt};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::components::{CompactIconAction, PanelHeader, StatusChip};
use crate::request_frame;
use crate::session::overlay_anchor;
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
                move || session.borrow_mut().fit_timeline()
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
                    left: Dp(24.0),
                    right: Dp(24.0),
                    top: Dp(0.0),
                    bottom: Dp(0.0),
                })
                .align_items(AlignItems::CENTER)
                .justify_content(JustifyContent::CENTER))
            .child(
                Column(Modifier::new().gap(Dp(8.0)).align_items(AlignItems::CENTER)).child((
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
    let hints = session.borrow().show_hints();
    Row(Modifier::new()
        .fill_max_width()
        .padding(Dp(8.0))
        .gap(Dp(8.0))
        .align_items(AlignItems::CENTER))
    .child((
        StatusChip(
            format!("Frame {}", head.round() as i64),
            theme().surface_container_high,
            theme().on_surface_variant,
        ),
        RangeEditor(session.clone(), range),
        if record {
            // A toggle, not a label: it looks like a control and sits next to
            // the other transport buttons, so tapping it has to disarm
            // recording - the same switch as the Properties-panel diamond.
            crate::components::ToolAction(
                Symbols::fiber_manual_record,
                "Record keys on edit",
                true,
                {
                    let session = session.clone();
                    move || {
                        let mut s = session.borrow_mut();
                        s.record = false;
                        s.revision = s.revision.wrapping_add(1);
                        request_frame();
                    }
                },
            )
        } else if hints {
            Text("One row per keyed property")
                .size(theme().typography.label_small)
                .color(theme().on_surface_variant)
        } else {
            Box(Modifier::new())
        },
    ))
}

fn RangeEditor(
    session: SessionRef,
    range: (renamite_animation::Frame, renamite_animation::Frame),
) -> View {
    let committed = remember_state_auto("committed", || (range.0.0, range.1.0));
    let start_state = remember_state_auto("start", || {
        let mut st = TextFieldState::new();
        st.text = range.0.0.to_string();
        st
    });
    let end_state = remember_state_auto("end", || {
        let mut st = TextFieldState::new();
        st.text = range.1.0.to_string();
        st
    });
    let start_focus = remember_auto("start_focus", || std::cell::Cell::new(false));
    let end_focus = remember_auto("end_focus", || std::cell::Cell::new(false));
    let was_start = remember_auto("was_start", || std::cell::Cell::new(false));
    let was_end = remember_auto("was_end", || std::cell::Cell::new(false));
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
            crate::components::field_container(
                Modifier::new()
                    .width(Dp(48.0))
                    .height(Dp(28.0))
                    .padding_values(repose_core::PaddingValues {
                        left: Dp(6.0),
                        right: Dp(6.0),
                        top: Dp(2.0),
                        bottom: Dp(2.0),
                    }),
                focus.get(),
            )
            .on_focus_changed(crate::shortcuts::note_text_focus)
            .on_key_event({
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
                cursor_brush: Some(crate::components::field_cursor_brush()),
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

    Row(Modifier::new().gap(Dp(4.0)).align_items(AlignItems::CENTER)).child((
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
    // Read the geometry the canvas paints from, rather than restating it: the
    // canvas reserves `row_top` for the frame ruler and spaces rows by
    // `row_height`, both density-scaled. A label that guesses either number
    // lands on the wrong row - and because the canvas starts below the ruler
    // while this column used to start at the very top, every label was off by
    // one whole row.
    let layout = session.borrow().timeline_layout();
    let row_top = Dp(layout.row_top as f32);
    let row_height = Dp(layout.row_height as f32);

    Box(Modifier::new().width(Dp(170.0)).fill_max_height()).child(ScrollArea(
        Modifier::new().fill_max_size(),
        remember_scroll_state("timeline_labels_scroll"),
        Column(Modifier::new().fill_max_width()).child((
            Box(Modifier::new().height(row_top).fill_max_width()),
            rows.iter()
                .map(|row| {
                    let label = prop_label(session.clone(), row.node, &row.prop);
                    let name = match label {
                        Some(prop) => {
                            format!("{}, {}", session.borrow().node_name(row.node), prop)
                        }
                        None => session.borrow().node_name(row.node),
                    };
                    Box(Modifier::new()
                        .height(row_height)
                        .fill_max_width()
                        .padding_values(repose_core::PaddingValues {
                            left: Dp(10.0),
                            right: Dp(8.0),
                            top: Dp(0.0),
                            bottom: Dp(0.0),
                        })
                        .align_items(AlignItems::CENTER))
                    .child(
                        Text(name)
                            .size(theme().typography.body_small)
                            .color(theme().on_surface),
                    )
                })
                .collect::<Vec<_>>(),
        )),
    ))
}

/// Map one timeline key event onto a semantic [`TimelineKey`] and dispatch it.
/// Returns true when the event was consumed. `Home`/`End` move the playhead
/// rather than the selection, so they are resolved here instead of in the
/// behavior crate.
fn handle_timeline_key(session: &SessionRef, event: KeyEvent) -> bool {
    if event.event_type != KeyEventType::Down {
        return false;
    }
    let cmd = event.modifiers.command;
    let shift = event.modifiers.shift;
    let step = |frames: i64| {
        if cmd {
            None
        } else {
            Some(TimelineKey::Nudge { frames })
        }
    };
    if !cmd {
        let range = match event.key {
            Key::Home => Some(true),
            Key::End => Some(false),
            _ => None,
        };
        if let Some(to_start) = range {
            let mut s = session.borrow_mut();
            let (start, end) = s
                .file
                .document
                .main_composition()
                .map(|c| (c.range.0.0, c.range.1.0))
                .unwrap_or((0, 180));
            s.set_playhead(if to_start { start as f64 } else { end as f64 });
            return true;
        }
    }
    let key = match event.key {
        Key::Delete => Some(TimelineKey::Delete),
        Key::Backspace if !cmd => Some(TimelineKey::Delete),
        Key::Escape => Some(TimelineKey::Escape),
        Key::Character('a') | Key::Character('A') if cmd => Some(TimelineKey::SelectAll),
        Key::Character('d') | Key::Character('D') if cmd => Some(TimelineKey::Duplicate),
        Key::Character('c') | Key::Character('C') if cmd => Some(TimelineKey::Copy),
        Key::Character('x') | Key::Character('X') if cmd => Some(TimelineKey::Cut),
        Key::Character('v') | Key::Character('V') if cmd => Some(TimelineKey::Paste),
        Key::ArrowLeft => step(if shift { -10 } else { -1 }),
        Key::ArrowRight => step(if shift { 10 } else { 1 }),
        _ => None,
    };
    let Some(key) = key else {
        return false;
    };
    let mut s = session.borrow_mut();
    dispatch_timeline(&mut s, TimelineEvent::KeyDown(key));
    true
}

const LONG_PRESS_MS: u64 = 500;
const LONG_PRESS_SLOP: f64 = 10.0;

struct TimelineLongPress {
    pos: DVec2,
    fired: Rc<Cell<bool>>,
    timer: repose_core::timer::TimerHandle,
}

type LongPressState = Rc<RefCell<Option<TimelineLongPress>>>;

fn cancel_timeline_long_press(state: &LongPressState) {
    if let Some(press) = state.borrow_mut().take() {
        press.timer.cancel();
    }
}

/// Arm the touch long press. Only touch arms it: a mouse already has the
/// right-click path, and arming there would double-fire on press-and-hold.
fn arm_timeline_long_press(state: &LongPressState, session: &SessionRef, pe: &PointerEvent) {
    if pe.kind != repose_core::input::PointerKind::Touch {
        cancel_timeline_long_press(state);
        return;
    }
    // A second finger means pan/zoom: drop the first finger's pending press
    // instead of re-arming the timer under the new one.
    if state.borrow().is_some() {
        cancel_timeline_long_press(state);
        return;
    }
    let pos = pe_pos(pe);
    let screen = overlay_anchor(pe);
    let fired = Rc::new(Cell::new(false));
    let timer = {
        let session = session.clone();
        let fired = fired.clone();
        repose_core::timer::delay(std::time::Duration::from_millis(LONG_PRESS_MS), move || {
            fired.set(true);
            let mut s = session.borrow_mut();
            open_timeline_context_menu(&mut s, pos, screen);
        })
    };
    *state.borrow_mut() = Some(TimelineLongPress { pos, fired, timer });
}

/// Drop the pending press once the finger travels far enough to be a pan.
fn track_timeline_long_press_move(state: &LongPressState, pe: &PointerEvent) {
    let beyond = state
        .borrow()
        .as_ref()
        .is_some_and(|press| (pe_pos(pe) - press.pos).length() > LONG_PRESS_SLOP);
    if beyond {
        cancel_timeline_long_press(state);
    }
}

fn finish_timeline_long_press(state: &LongPressState) -> bool {
    let fired = state
        .borrow()
        .as_ref()
        .is_some_and(|press| press.fired.get());
    cancel_timeline_long_press(state);
    fired
}

/// Right-click / long-press menu for the timeline. A key under the pointer that
/// is not part of the selection narrows the selection to it first, so the menu
/// always acts on what the pointer is actually over - the same rule the canvas
/// and layers panels follow.
fn open_timeline_context_menu(session: &mut crate::session::Session, pos: DVec2, screen: DVec2) {
    use crate::session::ContextMenuSource;
    use renamite_behavior_common::context_menu::TimelineMenuContext;

    let rows = crate::session::timeline_rows(session);
    let range = session
        .file
        .document
        .compositions
        .get(session.file.document.main)
        .map(|c| c.range)
        .unwrap_or((renamite_animation::Frame(0), renamite_animation::Frame(0)));
    let layout = session.timeline_layout();
    let ctx = crate::session::timeline_ctx_with_layout(
        &session.file.document,
        &session.file.clips,
        &rows,
        range,
        session.playback.head,
        layout,
    );
    session.keys.focus_key_at(&ctx, pos);

    let row_under_pointer = layout
        .y_to_row(pos.y)
        .is_some_and(|r| pos.y >= layout.row_top && r < rows.len());
    let entries = renamite_behavior_common::context_menu::timeline_menu(&TimelineMenuContext {
        selected_keys: session.keys.selected().len(),
        row_under_pointer,
        has_clipboard: session.keys.has_clipboard(),
    });

    session.open_context_menu(crate::session::ContextMenuState {
        screen_pos: screen,
        entries,
        source: ContextMenuSource::Timeline { pos },
    });
}

/// Touch pan/pinch for the timeline. Registered as a node action so it wins
/// over the global viewport gesture handler, which would otherwise pan and zoom
/// the *canvas* when the fingers land here.
///
/// A key drag or scrub holds the pointer against a gesture, so a one-finger
/// drag that became a key move is not then panned sideways - unless a finger
/// started it, in which case the gesture takes over (see
/// [`claim_timeline_for_gesture`]).
fn handle_timeline_gesture(session: &SessionRef, action: &repose_core::shortcuts::Action) -> bool {
    use repose_core::shortcuts::{Action, Gesture};
    match action {
        Action::Gesture(Gesture::Pan { delta, center }) => {
            if !claim_timeline_for_gesture(session) {
                return false;
            }
            let mut s = session.borrow_mut();
            let local_x = timeline_local_x(&s, *center);
            s.timeline_anchor.begin(DVec2::new(local_x, 0.0));
            s.pan_timeline(delta.x as f64);
            true
        }
        Action::Gesture(Gesture::Pinch { delta_scale })
        | Action::Gesture(Gesture::PinchWithCenter { delta_scale, .. }) => {
            if !claim_timeline_for_gesture(session) {
                return false;
            }
            let mut s = session.borrow_mut();
            // Zoom about the gesture centre in canvas-local x. A plain `Pinch`
            // carries no centre, so it anchors at the left edge. The anchor
            // latched on the gesture's first frame keeps the keys under the
            // fingers from sliding sideways as one finger travels.
            let center_x = match action {
                Action::Gesture(Gesture::PinchWithCenter { center, .. }) => {
                    timeline_local_x(&s, *center)
                }
                _ => 0.0,
            };
            let pinned = DVec2::new(center_x, 0.0);
            s.timeline_anchor.begin(pinned);
            let anchor_x = s.timeline_anchor.resolve(DVec2::ZERO).x;
            s.zoom_timeline_at(*delta_scale as f64, anchor_x);
            true
        }
        _ => false,
    }
}

/// The gesture centre (window px) in timeline-canvas-local px.
///
/// The timeline works in px throughout - `zoom_timeline_at` takes an
/// `anchor_px` and mixes it with the px scroll offset - so the rect (which
/// layout reports in dp) is scaled up to px here. Scaling the centre down
/// instead would halve the anchor on a 2x display.
fn timeline_local_x(s: &crate::session::Session, center: repose_core::Vec2) -> f64 {
    let scale = crate::session::dp_px(1.0) as f64;
    match s.timeline_rect {
        Some(r) => center.x as f64 - r.x as f64 * scale,
        None => 0.0,
    }
}

/// Decide whether a two-finger gesture may drive the timeline, taking the
/// pointer from a drag that only the first finger's own press started.
///
/// Every finger dispatches its own press, so by the time a gesture arrives the
/// first finger has usually armed a scrub or a key drag. Two fingers mean
/// navigate: cancel it - rolling back the transaction a key drag opened - and
/// let the gesture through. A mouse- or stylus-started drag keeps priority.
fn claim_timeline_for_gesture(session: &SessionRef) -> bool {
    let mut s = session.borrow_mut();
    let held = s.keys.is_active() || s.scrub.is_dragging() || s.timeline_pan_last.is_some();
    if !held || !s.timeline_touch_press {
        return !held;
    }
    let outs = s.keys.cancel();
    s.apply_outputs(outs);
    let outs = s.scrub.cancel();
    s.apply_outputs(outs);
    s.timeline_pan_last = None;
    s.timeline_anchor.end();
    true
}

fn TimelineCanvas(session: SessionRef) -> View {
    let sess_draw = session.clone();
    let last_click: Rc<RefCell<Option<(DVec2, web_time::Instant)>>> = Rc::new(RefCell::new(None));
    let press_moved: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
    let down_pos: Rc<RefCell<DVec2>> = Rc::new(RefCell::new(DVec2::ZERO));
    let focus = remember_auto("timeline_canvas_focus", FocusRequester::new);
    let long_press = remember_auto("timeline_long_press", || {
        Rc::new(RefCell::new(None::<TimelineLongPress>))
    });
    let anchor = remember_auto("timeline_wheel_anchor", || Rc::new(Cell::new(0.0f64)));

    Canvas(
        Modifier::new()
            .fill_max_size()
            // Scrolled past frame 0, `frame_to_x` goes negative and the keys,
            // ticks and playhead would paint over the label column. The ruler
            // and zebra rows are already bounded to the canvas width, so this
            // only clips what can escape left.
            .overflow(Overflow::Clip)
            .focusable(true)
            .on_key_event({
                let session = session.clone();
                move |ke: KeyEvent| handle_timeline_key(&session, ke)
            })
            .on_globally_positioned({
                let session = session.clone();
                move |r: Rect| session.borrow_mut().timeline_rect = Some(r)
            })
            .on_scroll({
                let session = session.clone();
                let anchor = anchor.clone();
                move |delta: repose_core::Vec2| {
                    let mut s = session.borrow_mut();
                    // A finger drag pans; only a wheel zooms. Without this a
                    // one-finger drag that started on the bottom nav bar zooms
                    // the range as it slides over the timeline.
                    if s.touch_active
                        || delta.y.abs() < delta.x.abs()
                        || (delta.x.abs() > 0.5 && delta.y.abs() > 0.5)
                    {
                        s.pan_timeline(delta.x as f64);
                    } else {
                        // Anchor on the cursor so the frame under it stays put,
                        // matching the canvas wheel-zoom behaviour.
                        let factor = (1.0 + (delta.y as f64) * 0.002).clamp(0.5, 2.0);
                        s.zoom_timeline_at(factor, anchor.get());
                    }
                    repose_core::Vec2::ZERO
                }
            })
            .on_action({
                let session = session.clone();
                move |action: repose_core::shortcuts::Action| {
                    handle_timeline_gesture(&session, &action)
                }
            })
            .on_pointer_down({
                let session = session.clone();
                let last_click = last_click.clone();
                let press_moved = press_moved.clone();
                let down_pos = down_pos.clone();
                let focus = focus.clone();
                let long_press = long_press.clone();
                let anchor = anchor.clone();
                move |pe: PointerEvent| {
                    if !matches!(pe.event, PointerEventKind::Down(_)) {
                        return;
                    }
                    session.borrow_mut().timeline_touch_press =
                        pe.kind == repose_core::input::PointerKind::Touch;
                    // A fresh press means any previous gesture is over, even if
                    // its lift landed off-surface and never reached here.
                    session.borrow_mut().timeline_anchor.end();
                    if let PointerEventKind::Down(b) = pe.event {
                        use repose_core::input::PointerButton as RB;
                        match b {
                            // Right-click: menu the thing under the pointer.
                            RB::Secondary => {
                                cancel_timeline_long_press(&long_press);
                                let mut s = session.borrow_mut();
                                open_timeline_context_menu(
                                    &mut s,
                                    pe_pos(&pe),
                                    overlay_anchor(&pe),
                                );
                                return;
                            }
                            // Middle drag pans the range, like the canvas.
                            RB::Tertiary => {
                                cancel_timeline_long_press(&long_press);
                                session.borrow_mut().timeline_pan_last = Some(pe_pos(&pe));
                                focus.request_focus();
                                return;
                            }
                            RB::Primary => {}
                        }
                    }
                    let pos = pe_pos(&pe);
                    let mods = map_modifiers(&pe);
                    let now = web_time::Instant::now();
                    *down_pos.borrow_mut() = pos;
                    *press_moved.borrow_mut() = false;
                    anchor.set(pos.x);
                    arm_timeline_long_press(&long_press, &session, &pe);
                    focus.request_focus();
                    let gesture_active = {
                        let s = session.borrow();
                        s.scrub.is_dragging() || s.keys.is_active()
                    };
                    let is_double = if gesture_active {
                        false
                    } else {
                        let lc = last_click.borrow();
                        lc.map(|(p, t)| (now - t).as_millis() < 350 && (p - pos).length() < 6.0)
                            .unwrap_or(false)
                    };

                    let mut s = session.borrow_mut();
                    if is_double {
                        *last_click.borrow_mut() = None;
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
                let press_moved = press_moved.clone();
                let down_pos = down_pos.clone();
                let long_press = long_press.clone();
                let anchor = anchor.clone();
                move |pe: PointerEvent| {
                    let pos = pe_pos(&pe);
                    if session.borrow().timeline_pan_last.is_some() {
                        // Middle-drag pan: keep the range under the cursor put.
                        let mut s = session.borrow_mut();
                        if let Some(last) = s.timeline_pan_last {
                            s.timeline_pan_last = Some(pos);
                            s.pan_timeline(pos.x - last.x);
                        }
                        request_frame();
                        return;
                    }
                    track_timeline_long_press_move(&long_press, &pe);
                    if matches!(pe.event, PointerEventKind::Move) {
                        // Keep the wheel-zoom anchor under a moving cursor.
                        anchor.set(pos.x);
                    }
                    if (*down_pos.borrow() - pos).length() >= 3.0 {
                        *press_moved.borrow_mut() = true;
                    }
                    let mut s = session.borrow_mut();
                    dispatch_timeline(
                        &mut s,
                        TimelineEvent::Move {
                            pos,
                            modifiers: map_modifiers(&pe),
                        },
                    );
                }
            })
            .on_pointer_up({
                let session = session.clone();
                let last_click = last_click.clone();
                let press_moved = press_moved.clone();
                let long_press = long_press.clone();
                move |pe: PointerEvent| {
                    session.borrow_mut().timeline_touch_press = false;
                    session.borrow_mut().timeline_anchor.end();
                    // A middle drag or a long press owns the lift: the former
                    // ends the pan, the latter already opened the menu and must
                    // not also register as a click (which would scrub).
                    if session.borrow_mut().timeline_pan_last.take().is_some() {
                        return;
                    }
                    if finish_timeline_long_press(&long_press) {
                        *press_moved.borrow_mut() = true;
                        return;
                    }
                    if let PointerEventKind::Up(b) = pe.event {
                        use repose_core::input::PointerButton as RB;
                        if !matches!(b, RB::Primary) {
                            return;
                        }
                    }
                    if !*press_moved.borrow() {
                        *last_click.borrow_mut() = Some((pe_pos(&pe), web_time::Instant::now()));
                    } else {
                        *last_click.borrow_mut() = None;
                    }
                    *press_moved.borrow_mut() = false;
                    let mut s = session.borrow_mut();
                    dispatch_timeline(
                        &mut s,
                        TimelineEvent::Release {
                            pos: pe_pos(&pe),
                            modifiers: map_modifiers(&pe),
                        },
                    );
                }
            })
            .on_pointer_cancel({
                let session = session.clone();
                let last_click = last_click.clone();
                let press_moved = press_moved.clone();
                let long_press = long_press.clone();
                move |pe: PointerEvent| {
                    pe.consume();
                    cancel_timeline_long_press(&long_press);
                    {
                        let mut s = session.borrow_mut();
                        s.timeline_pan_last = None;
                        s.timeline_touch_press = false;
                        s.timeline_anchor.end();
                    }
                    *last_click.borrow_mut() = None;
                    *press_moved.borrow_mut() = false;
                    let mut s = session.borrow_mut();
                    crate::session::cancel_timeline(&mut s);
                }
            })
            .on_pointer_leave({
                let session = session.clone();
                move |_pe: PointerEvent| {
                    let mut s = session.borrow_mut();
                    if s.scrub.is_dragging() || s.keys.is_active() {
                        crate::session::cancel_timeline(&mut s);
                    }
                }
            }),
        move |scope| {
            let s = sess_draw.borrow();
            let th = theme();
            let rows = crate::session::timeline_rows(&s);
            // The one layout both this paint pass and every hit test read, so
            // row geometry cannot drift between drawing and clicking.
            let layout = s.timeline_layout();
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
                Px(0.0),
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
                    Px(0.0),
                );
            }

            let tick_step = if layout.px_per_frame >= 8.0 {
                1
            } else if layout.px_per_frame >= 3.0 {
                5
            } else if layout.px_per_frame >= 1.0 {
                10
            } else {
                30
            };
            let label_step = (10 / tick_step.max(1)).max(1) * tick_step;
            let vis0 = (layout.x_to_frame(0.0).floor() as i64).clamp(range.0.0, range.1.0);
            let vis1 = (layout.x_to_frame(scope.size.width as f64).ceil() as i64)
                .clamp(range.0.0, range.1.0);
            let step = tick_step.max(1) as i64;
            let px = crate::session::dp_px;
            let mut frame = range.0.0.max(vis0 - (vis0 - range.0.0).rem_euclid(step));
            while frame <= range.1.0.min(vis1) {
                let x = layout.frame_to_x(frame as f64) as f32;
                let major = frame % (label_step as i64) == 0;
                let tick_h = if major {
                    layout.row_top as f32
                } else {
                    px(8.0)
                };
                scope.draw_rect(
                    Rect {
                        x,
                        y: layout.row_top as f32 - tick_h,
                        w: px(if major { 1.5 } else { 1.0 }),
                        h: tick_h,
                    },
                    if major {
                        th.outline
                    } else {
                        th.outline_variant
                    },
                    Px(0.0),
                );
                if major {
                    scope.draw_text(
                        frame.to_string(),
                        Vec2 {
                            x: x + px(3.0),
                            y: px(4.0),
                        },
                        th.on_surface_variant,
                        Px(px(10.0)),
                    );
                }
                frame += step;
            }

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
                        px(if is_sel { 7.0 } else { 5.5 }),
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
                    scope.draw_rect(r, th.primary.with_alpha(40), Px(0.0));
                    scope.draw_rect_stroke(r, th.primary.with_alpha(200), Px(0.0), Px(px(1.0)));
                }
                TimelineOverlay::DragDelta { frames } if frames != 0 => {
                    scope.draw_text(
                        format!("{frames:+}f"),
                        Vec2 {
                            x: px(8.0),
                            y: scope.size.height - px(18.0),
                        },
                        th.primary,
                        Px(px(12.0)),
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
                px(6.0),
                th.primary,
                None,
            );
            scope.draw_rect(
                Rect {
                    x: x - px(1.0),
                    y: 0.0,
                    w: px(2.0),
                    h: scope.size.height,
                },
                th.primary,
                Px(0.0),
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
