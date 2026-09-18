//! Central canvas keyboard dispatcher (Inkscape-style keymap).
//!
//! Every viewport key event flows through [`handle_viewport_key`]. The canvas
//! tools only ever see semantic [`CanvasKey`]s.

use renamite_behavior_canvas::{CanvasEvent, Key as CanvasKey};
use renamite_behavior_common::Modifiers;
use renamite_behavior_common::context_menu::MenuAction;
use renamite_history::ToolId;
use repose_core::input::{Key, KeyEvent, KeyEventType};
use repose_core::request_frame;

use crate::session::{
    EditorMode, PanelPage, SelectionBoolean, SessionRef, dispatch_canvas, redo_cmd, undo_cmd,
};

use std::cell::Cell;

thread_local! {
    static TEXT_FOCUS_COUNT: Cell<u32> = const { Cell::new(0) };
}

/// Record text-input focus changes (wired via `on_focus_changed` on every
/// text field). Global shortcuts yield while a text field is focused so
/// typing, Delete/Backspace and arrows edit text instead of the canvas.
pub fn note_text_focus(gained: bool) {
    TEXT_FOCUS_COUNT.with(|c| {
        if gained {
            c.set(c.get().saturating_add(1));
        } else {
            c.set(c.get().saturating_sub(1));
        }
    });
}

pub fn text_input_focused() -> bool {
    TEXT_FOCUS_COUNT.with(|c| c.get() > 0)
}

pub fn handle_global_action(session: &SessionRef, action: repose_core::shortcuts::Action) -> bool {
    use repose_core::shortcuts::Action;
    if text_input_focused() || session.borrow().renaming.is_some() {
        return false;
    }
    let mut s = session.borrow_mut();
    match action {
        Action::Undo => {
            undo_cmd(&mut s);
            s.bump();
            true
        }
        Action::Redo => {
            redo_cmd(&mut s);
            s.bump();
            true
        }
        Action::Copy => {
            s.copy_selection();
            true
        }
        Action::Cut => {
            s.cut_selection();
            true
        }
        Action::Paste => {
            s.paste_clipboard();
            true
        }
        Action::SelectAll => {
            s.select_all_top_level();
            true
        }
        Action::Save => {
            crate::file::save_document(session);
            true
        }
        Action::Custom(name) => match name.as_ref() {
            "renamite.redo" => {
                redo_cmd(&mut s);
                s.bump();
                true
            }
            "renamite.paste-style" => {
                s.paste_style();
                true
            }
            "renamite.paste-in-place" => {
                s.paste_clipboard_in_place();
                true
            }
            "renamite.duplicate" => {
                s.duplicate_selection();
                true
            }
            "renamite.group" => {
                s.run_menu_action(MenuAction::Group);
                true
            }
            "renamite.ungroup" => {
                s.run_menu_action(MenuAction::Ungroup);
                true
            }
            "renamite.stroke-to-path" => {
                s.stroke_selection_to_path();
                true
            }
            "renamite.convert-to-path" => {
                s.convert_selection_to_path();
                true
            }
            "renamite.combine" => {
                s.combine_selection();
                true
            }
            "renamite.break-apart" => {
                s.break_apart_selection();
                true
            }
            "renamite.simplify" => {
                s.simplify_selection();
                true
            }
            "renamite.flip-h" => {
                s.flip_selection(true);
                true
            }
            "renamite.delete" => {
                if s.mode == EditorMode::Interact {
                    s.delete_machine_selection();
                    return true;
                }
                if !matches!(
                    s.active_tool,
                    ToolId::Select | ToolId::Transform | ToolId::PathEdit
                ) && !s.selection.nodes.is_empty()
                    && !s.tool.is_dragging(s.active_tool)
                {
                    s.delete_selection_nodes();
                    return true;
                }
                false
            }
            _ => false,
        },
        _ => false,
    }
}

/// Handle one viewport key event. Returns true when the event was consumed.
pub fn handle_viewport_key(session: &SessionRef, event: KeyEvent) -> bool {
    // Text fields own their keys: only Escape (overlay dismissal) is allowed
    // through, everything else must reach the focused field, not the canvas.
    if text_input_focused() {
        if event.event_type == KeyEventType::Down && matches!(event.key, Key::Escape) {
            let mut s = session.borrow_mut();
            if s.context_menu.is_some() {
                s.close_context_menu();
                return true;
            }
            if s.open_picker.is_some() {
                s.close_color_picker();
                return true;
            }
        }
        return false;
    }
    let renaming = session.borrow().renaming.is_some();
    if renaming {
        if event.event_type == KeyEventType::Down && matches!(event.key, Key::Escape) {
            session.borrow_mut().cancel_rename();
            return true;
        }
        return false;
    }
    if matches!(event.key, Key::Space) {
        if event.event_type == KeyEventType::Up {
            session.borrow_mut().viewport.space_held = false;
        } else if event.event_type == KeyEventType::Down {
            session.borrow_mut().viewport.space_held = true;
        }
        request_frame();
        return true;
    }

    if event.event_type != KeyEventType::Down {
        return false;
    }

    let mut s = session.borrow_mut();

    if s.mode == EditorMode::Interact {
        if event.modifiers.command
            || !matches!(
                event.key,
                Key::Character('+' | '=' | '-' | '_' | '1' | '2' | '5' | 'f' | '#' | '%' | '|')
            )
        {
            return false;
        }
        drop(s);
        return handle_view_only_key(session, event);
    }

    let command = event.modifiers.command;
    let shift = event.modifiers.shift;
    let alt = event.modifiers.alt;
    let mods = Modifiers {
        shift,
        alt,
        ctrl: event.modifiers.ctrl,
    };
    let key = event.key.clone();

    if command {
        match key {
            Key::Character('+' | '=') => {
                s.boolean_selection(SelectionBoolean::Union);
                return true;
            }
            Key::Character('-' | '_') => {
                s.boolean_selection(SelectionBoolean::Difference);
                return true;
            }
            Key::Character('*') => {
                s.boolean_selection(SelectionBoolean::Intersection);
                return true;
            }
            Key::Character('^') => {
                s.boolean_selection(SelectionBoolean::Xor);
                return true;
            }
            Key::Character('/') => {
                s.divide_selection();
                return true;
            }
            Key::Character('k') if shift => {
                s.break_apart_selection();
                return true;
            }
            Key::Character('k') => {
                s.combine_selection();
                return true;
            }
            Key::Character('l') => {
                s.simplify_selection();
                return true;
            }
            Key::Character('c') if alt => {
                s.stroke_selection_to_path();
                return true;
            }
            Key::Character('v') if shift => {
                s.paste_style();
                return true;
            }
            Key::Character('v') if alt => {
                s.paste_clipboard_in_place();
                return true;
            }
            _ => {}
        }
    }

    match (command, shift, alt, &key) {
        // Path operations backed by real history commands
        (false, true, false, Key::Character('r')) => {
            s.reverse_selected_paths();
            return true;
        }
        (true, true, false, Key::Character('c')) => {
            s.convert_selection_to_path();
            return true;
        }

        _ => {}
    }

    if command && !alt && matches!(key, Key::Character('A')) {
        s.set_active_page(PanelPage::Inspect);
        return true;
    }

    match key {
        Key::Escape => {
            if s.context_menu.is_some() {
                s.close_context_menu();
                return true;
            }
            if s.open_picker.is_some() {
                s.close_color_picker();
                return true;
            }
            let was_dragging = s.tool.is_dragging(s.active_tool);
            dispatch_canvas(&mut s, CanvasEvent::KeyDown(CanvasKey::Escape), mods);
            if !was_dragging {
                s.deselect_when_idle();
            }
            return true;
        }
        Key::Delete | Key::Backspace => {
            let k = if matches!(key, Key::Delete) {
                CanvasKey::Delete
            } else {
                CanvasKey::Backspace
            };
            dispatch_canvas(&mut s, CanvasEvent::KeyDown(k), mods);
            return true;
        }
        Key::Home => {
            s.run_menu_action(MenuAction::BringToFront);
            return true;
        }
        Key::End => {
            s.run_menu_action(MenuAction::SendToBack);
            return true;
        }
        Key::PageUp => {
            s.run_menu_action(MenuAction::BringForward);
            return true;
        }
        Key::PageDown => {
            s.run_menu_action(MenuAction::SendBackward);
            return true;
        }
        _ => {}
    }

    if s.active_tool == ToolId::PathEdit {
        if command && !alt {
            if dispatch_path_edit_arrows(&mut s, &key, mods) {
                return true;
            }
            return false;
        }

        if shift && !alt {
            let key = match key {
                Key::Character('b') => CanvasKey::NodeBreak,
                Key::Character('j') => CanvasKey::NodeJoin,
                Key::Character('a') => CanvasKey::NodeAutoSmooth,
                Key::Character('c') => CanvasKey::NodeCorner,
                Key::Character('s') => CanvasKey::NodeSmooth,
                Key::Character('y') => CanvasKey::NodeSymmetric,
                Key::Character('l') => CanvasKey::SegmentLine,
                Key::Character('u') => CanvasKey::SegmentCurve,
                _ => {
                    if dispatch_path_edit_arrows(&mut s, &key, mods) {
                        return true;
                    }
                    // Unmapped Shift+letter: fall through to the rest of the
                    // dispatcher (e.g. Shift+R reverse paths).
                    if let Key::Character('r') = key {
                        s.reverse_selected_paths();
                        return true;
                    }
                    return false;
                }
            };
            dispatch_canvas(&mut s, CanvasEvent::KeyDown(key), mods);
            return true;
        }
        match key {
            Key::Insert => {
                dispatch_canvas(&mut s, CanvasEvent::KeyDown(CanvasKey::Insert), mods);
                return true;
            }
            Key::Tab => {
                dispatch_canvas(&mut s, CanvasEvent::KeyDown(CanvasKey::Tab), mods);
                return true;
            }
            Key::ArrowLeft => {
                dispatch_canvas(&mut s, CanvasEvent::KeyDown(CanvasKey::ArrowLeft), mods);
                return true;
            }
            Key::ArrowRight => {
                dispatch_canvas(&mut s, CanvasEvent::KeyDown(CanvasKey::ArrowRight), mods);
                return true;
            }
            Key::ArrowUp => {
                dispatch_canvas(&mut s, CanvasEvent::KeyDown(CanvasKey::ArrowUp), mods);
                return true;
            }
            Key::ArrowDown => {
                dispatch_canvas(&mut s, CanvasEvent::KeyDown(CanvasKey::ArrowDown), mods);
                return true;
            }
            _ => {}
        }
    }

    if !command && !alt && matches!(key, Key::Tab) && s.active_tool != ToolId::PathEdit {
        s.cycle_selection(shift);
        return true;
    }

    if !command && !s.selection.nodes.is_empty() {
        let dir = match key {
            Key::ArrowLeft => Some(glam::DVec2::new(-1.0, 0.0)),
            Key::ArrowRight => Some(glam::DVec2::new(1.0, 0.0)),
            Key::ArrowUp => Some(glam::DVec2::new(0.0, -1.0)),
            Key::ArrowDown => Some(glam::DVec2::new(0.0, 1.0)),
            _ => None,
        };
        if let Some(dir) = dir
            && s.active_tool != ToolId::PathEdit
        {
            s.nudge_selection(dir, mods);
            return true;
        }
    }

    if !command && !alt && shift && matches!(key, Key::Character('H')) {
        s.flip_selection(false);
        return true;
    }

    if !command && !alt && !shift {
        if matches!(key, Key::Character('h' | 'H')) {
            s.flip_selection(true);
            return true;
        }
        let tool = match tool_key(&key, event.physical.as_deref()) {
            Some(t) => Some(t),
            None => match key {
                Key::Character('s' | 'v') => Some(ToolId::Select),
                Key::Character('m') => Some(ToolId::Transform),
                Key::Character('n') => Some(ToolId::PathEdit),
                Key::Character('b' | 'p') => Some(ToolId::Pen),
                Key::Character('r') => Some(ToolId::Rect),
                Key::Character('e') => Some(ToolId::Ellipse),
                Key::Character('*') => Some(ToolId::Star),
                Key::Character('t') => Some(ToolId::Text),
                Key::Character('g') => Some(ToolId::Gradient),
                Key::Character('u') => Some(ToolId::Fill),
                Key::Character('d') | Key::F(7) => Some(ToolId::Dropper),
                _ => None,
            },
        };
        if let Some(tool) = tool {
            s.active_tool = tool;
            s.repaint();
            return true;
        }
    }

    if !command && !alt {
        // View toggles use the produced logical character: Shift is ignored as
        // a semantic modifier because '#', '%' and '|' require Shift on many
        // layouts.
        match key {
            Key::Character('#') => {
                s.viewport.show_grid = !s.viewport.show_grid;
                s.repaint();
                return true;
            }
            Key::Character('%') => {
                s.viewport.snapping_enabled = !s.viewport.snapping_enabled;
                s.repaint();
                return true;
            }
            Key::Character('|') => {
                s.viewport.show_guides = !s.viewport.show_guides;
                s.repaint();
                return true;
            }
            _ => {}
        }
    }

    if !command {
        match key {
            Key::Character('+' | '=') => {
                s.viewport.zoom_centered(1.2);
                request_frame();
                return true;
            }
            Key::Character('-' | '_') => {
                s.viewport.zoom_centered(1.0 / 1.2);
                request_frame();
                return true;
            }
            Key::Character('1') => {
                set_zoom(&mut s, 1.0);
                return true;
            }
            Key::Character('2') => {
                set_zoom(&mut s, 0.5);
                return true;
            }
            Key::Character('5' | 'f') => {
                s.viewport.request_fit();
                request_frame();
                return true;
            }
            _ => {}
        }
    }

    false
}

fn handle_view_only_key(session: &SessionRef, event: KeyEvent) -> bool {
    let key = event.key.clone();
    let mut s = session.borrow_mut();
    match key {
        Key::Character('#') => {
            s.viewport.show_grid = !s.viewport.show_grid;
            s.repaint();
            true
        }
        Key::Character('%') => {
            s.viewport.snapping_enabled = !s.viewport.snapping_enabled;
            s.repaint();
            true
        }
        Key::Character('|') => {
            s.viewport.show_guides = !s.viewport.show_guides;
            s.repaint();
            true
        }
        Key::Character('+' | '=') => {
            s.viewport.zoom_centered(1.2);
            request_frame();
            true
        }
        Key::Character('-' | '_') => {
            s.viewport.zoom_centered(1.0 / 1.2);
            request_frame();
            true
        }
        Key::Character('1') => {
            set_zoom(&mut s, 1.0);
            true
        }
        Key::Character('2') => {
            set_zoom(&mut s, 0.5);
            true
        }
        Key::Character('5' | 'f') => {
            s.viewport.request_fit();
            request_frame();
            true
        }
        _ => false,
    }
}

/// Arrow forwarding inside PathEdit: arrows are delivered regardless of Alt
/// (Alt = 1 screen px step inside the tool). Returns true when consumed.
fn dispatch_path_edit_arrows(s: &mut crate::session::Session, key: &Key, mods: Modifiers) -> bool {
    let canvas_key = match key {
        Key::ArrowLeft => CanvasKey::ArrowLeft,
        Key::ArrowRight => CanvasKey::ArrowRight,
        Key::ArrowUp => CanvasKey::ArrowUp,
        Key::ArrowDown => CanvasKey::ArrowDown,
        _ => return false,
    };
    dispatch_canvas(s, CanvasEvent::KeyDown(canvas_key), mods);
    true
}

fn set_zoom(s: &mut crate::session::Session, target: f64) {
    let current = s.viewport.view.scale;
    if current > f64::EPSILON {
        s.viewport.zoom_centered(target / current);
        request_frame();
    }
}

/// Tool shortcut by physical key position (`KeyV`, `KeyB`, ... — winit
/// `KeyCode` debug names from `KeyEvent::physical`). Letters always resolve
/// lowercase like repose's own `map_key`; Shift is not a semantic modifier
/// here, so AZERTY/QWERTZ users get the same tool on the same key position.
/// Returns `None` when `physical` is absent (synthetic events) or unmapped —
/// callers fall back to the logical-character match.
fn tool_key(key: &Key, physical: Option<&str>) -> Option<ToolId> {
    let pos = match physical {
        // Already resolved to a tool by the logical match below (e.g. '*' on
        // any layout, F7); don't override it.
        _ if matches!(key, Key::Character('*') | Key::F(_)) => return None,
        Some(p) => p,
        None => return None,
    };
    match pos {
        "KeyV" => Some(ToolId::Select),
        "KeyM" => Some(ToolId::Transform),
        "KeyN" => Some(ToolId::PathEdit),
        "KeyB" | "KeyP" => Some(ToolId::Pen),
        "KeyR" => Some(ToolId::Rect),
        "KeyE" => Some(ToolId::Ellipse),
        "KeyT" => Some(ToolId::Text),
        "KeyG" => Some(ToolId::Gradient),
        "KeyU" => Some(ToolId::Fill),
        "KeyD" => Some(ToolId::Dropper),
        _ => None,
    }
}
