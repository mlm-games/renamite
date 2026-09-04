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
    EditorMode, SelectionBoolean, SessionRef, dispatch_canvas, redo_cmd, undo_cmd,
};

/// Handle one viewport key event. Returns true when the event was consumed.
pub fn handle_viewport_key(session: &SessionRef, event: KeyEvent) -> bool {
    // Space tracks the temporary-pan gesture on both edges.
    if matches!(event.key, Key::Space) {
        let mut s = session.borrow_mut();
        s.viewport.space_held = event.event_type == KeyEventType::Down;
        return true;
    }

    if event.event_type != KeyEventType::Down {
        return false;
    }

    let mut s = session.borrow_mut();

    // Interact mode owns input for runtime listeners / state machines.
    if s.mode == EditorMode::Interact {
        if matches!(event.key, Key::Delete | Key::Backspace) {
            s.delete_machine_selection();
            return true;
        }
        return false;
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

    match (command, shift, alt, &key) {
        // Undo / redo
        (true, false, false, Key::Character('z')) => {
            undo_cmd(&mut s);
            s.bump();
            return true;
        }
        (true, true, false, Key::Character('z')) | (true, false, false, Key::Character('y')) => {
            redo_cmd(&mut s);
            s.bump();
            return true;
        }

        _ => {}
    }

    // Punctuation shortcuts use the produced logical character. Ignore Shift as a
    // semantic modifier because '+', '*', '^', '#', '%' and '|' require Shift on
    // many layouts. Placed before the clipboard arms so Ctrl+Alt+V and
    // Ctrl+Shift+V win over the broad Ctrl+V paste binding.
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
        // Clipboard / duplicate
        (true, false, false, Key::Character('c')) => {
            s.copy_selection();
            return true;
        }
        (true, false, false, Key::Character('x')) => {
            s.cut_selection();
            return true;
        }
        (true, false, false, Key::Character('v')) => {
            s.paste_clipboard();
            return true;
        }
        (true, false, false, Key::Character('d')) => {
            s.duplicate_selection();
            return true;
        }

        // Structure
        (true, false, false, Key::Character('g')) => {
            s.run_menu_action(MenuAction::Group);
            return true;
        }
        (true, true, false, Key::Character('g')) | (true, false, false, Key::Character('u')) => {
            s.run_menu_action(MenuAction::Ungroup);
            return true;
        }

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

    // Ctrl/Cmd+A: select all top-level objects in the current composition.
    if command && !shift && matches!(key, Key::Character('a')) {
        let comp = s.file.document.main;
        s.selection.nodes = s.file.document.compositions[comp].children.clone();
        s.ensure_selection_visible();
        s.repaint();
        return true;
    }

    if matches!(key, Key::Escape) {
        if s.context_menu.is_some() {
            s.close_context_menu();
            return true;
        }
        if s.open_picker.is_some() {
            s.close_color_picker();
            return true;
        }
    }

    match key {
        Key::Escape => {
            dispatch_canvas(&mut s, CanvasEvent::KeyDown(CanvasKey::Escape), mods);
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

    if s.active_tool == ToolId::PathEdit && !command {
        // Tangent-mode / segment-conversion / node-op chords. Plain letters
        // are left to the tool-selection map below, so only Shift (+ not Alt,
        // which stays free for 1px arrow nudging) reaches them here.
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
                s.viewport.fit_pending = true;
                request_frame();
                return true;
            }
            _ => {}
        }
    }

    if !command && !alt && !shift {
        let tool = match key {
            Key::Character('s' | 'v') => Some(ToolId::Select),
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
        };
        if let Some(tool) = tool {
            s.active_tool = tool;
            s.repaint();
            return true;
        }
    }

    false
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
