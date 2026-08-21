//! Central canvas keyboard dispatcher (Inkscape-style keymap).
//!
//! Every viewport key event flows through [`handle_viewport_key`]; the canvas
//! tools only ever see semantic [`CanvasKey`]s.
//!
//! HACK: Chords whose engine features will be added in future, or
//! don't exist yet (booleans, stroke-to-path, simplify, combine/break-apart,
//! paste style, paste in place, dropper, grid/snap/guide toggles) are
//! left unbound.

use renamite_behavior_canvas::{CanvasEvent, Key as CanvasKey};
use renamite_behavior_common::Modifiers;
use renamite_behavior_common::context_menu::MenuAction;
use renamite_history::ToolId;
use repose_core::input::{Key, KeyEvent, KeyEventType};
use repose_core::request_frame;

use crate::session::{EditorMode, SessionRef, dispatch_canvas, redo_cmd, undo_cmd};

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
        (true, true, false, Key::Character('z' | 'y')) => {
            redo_cmd(&mut s);
            s.bump();
            return true;
        }

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

    if s.active_tool == ToolId::PathEdit && !command && !alt {
        if shift {
            let key = match key {
                Key::Character('c') => CanvasKey::NodeCorner,
                Key::Character('s' | 'a') => CanvasKey::NodeSmooth,
                Key::Character('y') => CanvasKey::NodeSymmetric,
                Key::Character('l') => CanvasKey::SegmentLine,
                Key::Character('u') => CanvasKey::SegmentCurve,
                _ => return false,
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

fn set_zoom(s: &mut crate::session::Session, target: f64) {
    let current = s.viewport.view.scale;
    if current > f64::EPSILON {
        s.viewport.zoom_centered(target / current);
        request_frame();
    }
}
