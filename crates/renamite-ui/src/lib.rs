//! Editor chrome.
//!
//! Root function matches Repose's `fn app(&mut Scheduler, &RenderContext) -> View`
//! pattern. `Session` holds the whole editor state behind `Rc<RefCell<_>>` so
//! Repose's `Fn` canvas/pointer callbacks can reach it via interior mutability.

#![allow(non_snake_case)]

pub mod components;
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

    Box(Modifier::new().fill_max_size()).child(EditorShell(session))
}

pub fn AppTopBar(session: SessionRef) -> View {
    let is_playing = session.borrow().playing;
    let name = session.borrow().file.meta.name.clone();

    TopAppBar(
        Text("renamite").size(theme().typography.title_large),
        Some(
            Text(name)
                .size(theme().typography.label_medium)
                .color(theme().on_surface_variant),
        ),
        Some(CompactIconAction(Symbols::menu, "Main menu", || {})),
        vec![
            CompactIconAction(Symbols::folder_open, "Open", || {}),
            CompactIconAction(Symbols::save, "Save", || {}),
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
        tool(session.clone(), ToolId::Select, Symbols::arrow_selector_tool, "Select", selected),
        tool(session.clone(), ToolId::PathEdit, Symbols::edit, "Edit path", selected),
        tool(session.clone(), ToolId::Rect, Symbols::rectangle, "Rectangle", selected),
        tool(session.clone(), ToolId::Ellipse, Symbols::circle, "Ellipse", selected),
        tool(session.clone(), ToolId::Star, Symbols::star, "Star", selected),
        tool(session.clone(), ToolId::Gradient, Symbols::gradient, "Gradient", selected),
        tool(session, ToolId::Fill, Symbols::format_color_fill, "Fill", selected),
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