//! Editor chrome.
//!
//! Root function matches Repose's `fn app(&mut Scheduler, &RenderContext) -> View`
//! pattern. `Session` holds the whole editor state behind `Rc<RefCell<_>>` so
//! Repose's `Fn` canvas/pointer callbacks can reach it via interior mutability.

#![allow(non_snake_case)]

pub mod color_picker;
pub mod components;
pub mod file;
pub mod panels;
pub mod session;
pub mod shell;
pub mod symbols;

use renamite_animation::PlayState;
use renamite_history::ToolId;
use repose_core::{Color, Modifier, Scheduler, View, remember_with_key, request_frame, theme};
use repose_material::material3::{
    DropdownMenu, DropdownMenuConfig, DropdownMenuEntry, DropdownMenuItem, MenuState, TopAppBar,
    TopAppBarConfig,
};
use repose_platform::RenderContext;
use repose_ui::overlay::OverlayHandle;
use repose_ui::{Box, Column, Row, Text, TextStyle, ViewExt};
use std::rc::Rc;
use web_time::Instant;

use components::{CompactIconAction, ToolAction};
use session::{EditorMode, PickerTarget, SessionRef, init_session, redo_cmd, undo_cmd};
use shell::EditorShell;
use symbols::Symbols;

pub fn app(_s: &mut Scheduler, _rc: &RenderContext) -> View {
    let session = init_session(_rc);
    if session.borrow_mut().drain_file_ops() {
        file::run_pending_intent(&session);
    }

    Box(Modifier::new().fill_max_size()).child(EditorShell(session))
}

/// Initialise WASM worker threads.
#[cfg(target_arch = "wasm32")]
pub fn init_wasm() {
    console_error_panic_hook::set_once();
    if web_workers::web::has_spawn_support() {
        let _ = web_sys::console::log_1(&"wasm worker threads: available".into());
    } else {
        let _ = web_sys::console::warn_1(&"wasm worker threads: unavailable (COOP/COEP?)".into());
    }
}

fn EditorModeSwitch(session: SessionRef) -> View {
    let mode = session.borrow().mode;
    Row(Modifier::new().gap(6.0)).child((
        crate::components::PillButton("Design", mode == EditorMode::Design, {
            let session = session.clone();
            move || session.borrow_mut().set_mode(EditorMode::Design)
        }),
        crate::components::PillButton("Animate", mode == EditorMode::Animate, {
            let session = session.clone();
            move || session.borrow_mut().set_mode(EditorMode::Animate)
        }),
        crate::components::PillButton("Interact", mode == EditorMode::Interact, {
            let session = session.clone();
            move || session.borrow_mut().set_mode(EditorMode::Interact)
        }),
    ))
}

pub fn AppTopBar(session: SessionRef, overlay: OverlayHandle) -> View {
    let (name, dirty, status, path) = {
        let s = session.borrow();
        let name = s
            .current_path
            .as_ref()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| s.file.meta.name.clone());
        let path = s
            .current_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "Unsaved".to_string());
        (name, s.dirty, s.status.clone(), path)
    };
    let title = if dirty { format!("{name} *") } else { name };

    let mut actions: Vec<View> = Vec::new();
    if shell::platform_shell_class() == shell::ShellClass::Expanded {
        actions.push(UndoButton(session.clone()));
        actions.push(RedoButton(session.clone()));
    }

    TopAppBar(
        Text(title).size(theme().typography.title_large),
        Some(
            Row(Modifier::new()
                .gap(10.0)
                .align_items(repose_core::AlignItems::CENTER))
            .child((
                EditorModeSwitch(session.clone()),
                Text(status.unwrap_or(path))
                    .size(theme().typography.label_small)
                    .color(theme().on_surface_variant),
            )),
        ),
        Some(FileMenu(session.clone(), overlay)),
        actions,
        TopAppBarConfig::default(),
    )
}

pub fn FileMenu(session: SessionRef, overlay: OverlayHandle) -> View {
    let state = remember_with_key("renamite_file_menu", MenuState::new);

    let trigger = CompactIconAction(Symbols::menu, "File", {
        let state = state.clone();
        move || state.open()
    });

    let item = |text: &'static str, action: Rc<dyn Fn()>| {
        DropdownMenuEntry::Item(DropdownMenuItem {
            text: text.into(),
            leading_icon: None,
            trailing_icon: None,
            on_click: action,
            enabled: true,
        })
    };

    let items = vec![
        item("New", {
            let session = session.clone();
            Rc::new(move || file::new_document(&session))
        }),
        item("Open…", {
            let session = session.clone();
            Rc::new(move || file::open_document(&session))
        }),
        item("Save", {
            let session = session.clone();
            Rc::new(move || {
                file::save_document(&session);
            })
        }),
        item("Save As…", {
            let session = session.clone();
            Rc::new(move || {
                file::save_document_as(&session);
            })
        }),
        DropdownMenuEntry::Divider,
        item("Undo", {
            let session = session.clone();
            Rc::new(move || {
                let mut s = session.borrow_mut();
                undo_cmd(&mut s);
                s.bump();
            })
        }),
        item("Redo", {
            let session = session.clone();
            Rc::new(move || {
                let mut s = session.borrow_mut();
                redo_cmd(&mut s);
                s.bump();
            })
        }),
        DropdownMenuEntry::Divider,
        item("Import Lottie…", {
            let session = session.clone();
            Rc::new(move || file::import_lottie(&session))
        }),
        item("Import SVG…", {
            let session = session.clone();
            Rc::new(move || file::import_svg(&session))
        }),
        item("Import Font…", {
            let session = session.clone();
            Rc::new(move || file::import_font(&session))
        }),
        DropdownMenuEntry::Divider,
        item("Export Lottie…", {
            let session = session.clone();
            Rc::new(move || file::export_lottie(&session))
        }),
        item("Export PNG…", {
            let session = session.clone();
            Rc::new(move || file::export_png(&session))
        }),
        item("Export SVG…", {
            let session = session.clone();
            Rc::new(move || file::export_svg(&session))
        }),
    ];

    DropdownMenu(
        state,
        overlay,
        Modifier::new(),
        trigger,
        items,
        DropdownMenuConfig {
            min_width: 220.0,
            max_width: 280.0,
            ..Default::default()
        },
    )
}

fn UndoButton(session: SessionRef) -> View {
    CompactIconAction(Symbols::undo, "Undo", {
        let session = session.clone();
        move || {
            let mut s = session.borrow_mut();
            undo_cmd(&mut s);
            s.bump();
        }
    })
}

fn RedoButton(session: SessionRef) -> View {
    CompactIconAction(Symbols::redo, "Redo", {
        let session = session.clone();
        move || {
            let mut s = session.borrow_mut();
            redo_cmd(&mut s);
            s.bump();
        }
    })
}

pub(crate) fn toggle_playback(session: &SessionRef) {
    let mut s = session.borrow_mut();
    s.playing = !s.playing;
    s.last_tick = Instant::now();
    s.playback.state = if s.playing {
        PlayState::Playing
    } else {
        PlayState::Stopped
    };
    request_frame();
}

pub fn ToolRail(session: SessionRef) -> View {
    let selected = session.borrow().active_tool;

    Column(
        Modifier::new()
            .width(72.0)
            .fill_max_height()
            .padding_values(repose_core::PaddingValues {
                left: 12.0,
                right: 12.0,
                top: 12.0,
                bottom: 12.0,
            })
            .gap(6.0)
            .background(theme().surface_container),
    )
    .child(vec![
        tool(
            session.clone(),
            ToolId::Select,
            Symbols::arrow_selector_tool,
            "Select",
            selected,
        ),
        tool(
            session.clone(),
            ToolId::Transform,
            Symbols::transform,
            "Transform / pivot",
            selected,
        ),
        tool(session.clone(), ToolId::Pen, Symbols::draw, "Pen", selected),
        tool(
            session.clone(),
            ToolId::PathEdit,
            Symbols::edit,
            "Edit path",
            selected,
        ),
        tool(
            session.clone(),
            ToolId::Rect,
            Symbols::rectangle,
            "Rectangle",
            selected,
        ),
        tool(
            session.clone(),
            ToolId::Ellipse,
            Symbols::circle,
            "Ellipse",
            selected,
        ),
        tool(
            session.clone(),
            ToolId::Star,
            Symbols::star,
            "Star",
            selected,
        ),
        tool(
            session.clone(),
            ToolId::Text,
            Symbols::text_fields,
            "Text",
            selected,
        ),
        tool(
            session.clone(),
            ToolId::Fill,
            Symbols::format_color_fill,
            "Fill",
            selected,
        ),
        tool(
            session,
            ToolId::Gradient,
            Symbols::gradient,
            "Gradient",
            selected,
        ),
    ])
}

fn tool(
    session: SessionRef,
    id: ToolId,
    symbol: repose_material::Symbol,
    label: &'static str,
    selected: ToolId,
) -> View {
    ToolAction(symbol, label, selected == id, move || {
        let mut s = session.borrow_mut();
        s.active_tool = id;
        s.revision = s.revision.wrapping_add(1);
        request_frame();
    })
}

/// Paint swatch showing the current fill paint; click opens the color picker
/// popover targeting the current paint.
pub(crate) fn CompactSwatchButton(session: SessionRef) -> View {
    let th = theme();
    let (color, is_open) = {
        let s = session.borrow();
        let color = s.current_paint.base_color();
        (color, s.open_picker.is_some())
    };

    Box(Modifier::new()
        .width(32.0)
        .height(32.0)
        .clip_rounded(8.0)
        .border(
            2.0,
            if is_open {
                th.primary
            } else {
                th.outline_variant
            },
            8.0,
        )
        .background(Color::from_rgba(
            (color.r * 255.0) as u8,
            (color.g * 255.0) as u8,
            (color.b * 255.0) as u8,
            (color.a * 255.0) as u8,
        ))
        .on_pointer_down({
            let session = session.clone();
            move |pe: repose_core::input::PointerEvent| {
                let mut s = session.borrow_mut();
                if s.open_picker.is_some() {
                    s.close_color_picker();
                } else {
                    let c = s.current_paint.base_color();
                    let anchor = glam::DVec2::new(pe.position.x as f64, pe.position.y as f64);
                    s.open_color_picker(PickerTarget::CurrentPaint, c, anchor);
                }
            }
        }))
}
