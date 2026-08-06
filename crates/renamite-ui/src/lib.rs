//! Editor chrome.
//!
//! Root function matches Repose's `fn app(&mut Scheduler, &RenderContext) -> View`
//! pattern. `Session` holds the whole editor state behind `Rc<RefCell<_>>` so
//! Repose's `Fn` canvas/pointer callbacks can reach it via interior mutability.

#![allow(non_snake_case)]

pub mod components;
pub mod file;
pub mod panels;
pub mod session;
pub mod shell;
pub mod symbols;

use renamite_animation::PlayState;
use renamite_history::ToolId;
use repose_core::{Modifier, Scheduler, View, request_frame, theme};
use repose_material::material3::{TopAppBar, TopAppBarConfig};
use repose_platform::RenderContext;
use repose_ui::{Box, Column, Text, TextStyle, ViewExt};
use web_time::Instant;

use components::{CompactIconAction, ToolAction};
use session::{SessionRef, init_session, redo_cmd, undo_cmd};
use shell::EditorShell;
use symbols::Symbols;

pub fn app(_s: &mut Scheduler, _rc: &RenderContext) -> View {
    let session = init_session();
    if session.borrow_mut().drain_file_ops() {
        file::run_pending_intent(&session);
    }

    Box(Modifier::new().fill_max_size()).child(EditorShell(session))
}

/// Initialise WASM worker threads.
#[cfg(target_arch = "wasm32")]
pub fn init_wasm() {
    if web_workers::web::has_spawn_support() {
        let _ = web_workers::scope(|scope| {
            let _ = scope.spawn(|| {}).join();
        });
    }
}

pub fn AppTopBar(session: SessionRef) -> View {
    let is_playing = session.borrow().playing;
    let recording = session.borrow().record;
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
    let title = if dirty { format!("● {name}") } else { name };
    let subtitle = status.unwrap_or(path);

    TopAppBar(
        Text("renamite").size(theme().typography.title_large),
        Some(
            Text(format!("{title} — {subtitle}"))
                .size(theme().typography.label_medium)
                .color(theme().on_surface_variant),
        ),
        Some(CompactIconAction(Symbols::menu, "Main menu", || {})),
        vec![
            CompactIconAction(Symbols::add, "New", {
                let session = session.clone();
                move || file::new_document(&session)
            }),
            CompactIconAction(Symbols::folder_open, "Open", {
                let session = session.clone();
                move || file::open_document(&session)
            }),
            CompactIconAction(Symbols::save, "Save", {
                let session = session.clone();
                move || {
                    file::save_document(&session);
                }
            }),
            CompactIconAction(Symbols::save_as, "Save As", {
                let session = session.clone();
                move || {
                    file::save_document_as(&session);
                }
            }),
            CompactIconAction(Symbols::file_upload, "Import Lottie", {
                let session = session.clone();
                move || file::import_lottie(&session)
            }),
            CompactIconAction(Symbols::image, "Export PNG", {
                let session = session.clone();
                move || file::export_png(&session)
            }),
            CompactIconAction(Symbols::undo, "Undo", {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    undo_cmd(&mut s);
                    s.bump();
                }
            }),
            CompactIconAction(Symbols::redo, "Redo", {
                let session = session.clone();
                move || {
                    let mut s = session.borrow_mut();
                    redo_cmd(&mut s);
                    s.bump();
                }
            }),
            if recording {
                CompactIconAction(Symbols::fiber_manual_record, "Stop recording keys", {
                    let session = session.clone();
                    move || toggle_record(&session)
                })
            } else {
                CompactIconAction(Symbols::radio_button_unchecked, "Record keys on edit", {
                    let session = session.clone();
                    move || toggle_record(&session)
                })
            },
            if is_playing {
                CompactIconAction(Symbols::pause, "Pause", {
                    let session = session.clone();
                    move || toggle_playback(&session)
                })
            } else {
                CompactIconAction(Symbols::play_arrow, "Play", {
                    let session = session.clone();
                    move || toggle_playback(&session)
                })
            },
            CompactIconAction(Symbols::more_vert, "More", || {}),
        ],
        TopAppBarConfig {
            modifier: Modifier::new(),
            ..Default::default()
        },
    )
}

fn toggle_playback(session: &SessionRef) {
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

fn toggle_record(session: &SessionRef) {
    let mut s = session.borrow_mut();
    s.record = !s.record;
    s.revision = s.revision.wrapping_add(1);
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
    .child((
        tool(
            session.clone(),
            ToolId::Select,
            Symbols::arrow_selector_tool,
            "Select",
            selected,
        ),
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
            ToolId::Gradient,
            Symbols::gradient,
            "Gradient",
            selected,
        ),
        tool(
            session,
            ToolId::Fill,
            Symbols::format_color_fill,
            "Fill",
            selected,
        ),
    ))
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
