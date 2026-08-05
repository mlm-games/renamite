//! Layers panel: M3 list with visibility, lock, expand, select, reorder, rename.

use renamite_behavior_common::layers::{
    LayerKind, LayerRow, cmd_rename, cmd_toggle_locked, cmd_toggle_visible, drop_command,
    flatten_layers, is_ancestor, move_is_noop, select_only, toggle_in_selection,
};
use renamite_history::ToolOutput;
use repose_core::input::{PointerButton, PointerEvent, PointerEventKind};
use repose_core::{AlignItems, Modifier, PaddingValues, View, request_frame, theme};
use repose_ui::{Box, Column, Row, Text, TextStyle, ViewExt};
use smallvec::smallvec;

use crate::components::{CompactIconAction, PanelHeader};
use crate::session::SessionRef;
use crate::symbols::{AppIcon, Symbols};

const ROW_HEIGHT: f32 = 40.0;
const ROW_GAP: f32 = 2.0;

pub fn LayersPanel(session: SessionRef) -> View {
    let (rows, selected, expanded, drag, renaming) = {
        let s = session.borrow();
        let rows = flatten_layers(
            &s.file.document,
            s.file.document.main,
            &s.expanded_layers,
        );
        (
            rows,
            s.selection.nodes.clone(),
            s.expanded_layers.clone(),
            s.layer_drag.clone(),
            s.renaming.clone(),
        )
    };

    let list = Column(
        Modifier::new()
            .fill_max_size()
            .padding_values(PaddingValues {
                left: 4.0,
                right: 4.0,
                top: 0.0,
                bottom: 8.0,
            })
            .gap(ROW_GAP),
    )
    .child(
        rows.iter()
            .enumerate()
            .map(|(i, row)| {
                LayerRowView(
                    session.clone(),
                    row.clone(),
                    LayerRowState {
                        index: i,
                        is_selected: selected.contains(&row.id),
                        is_expanded: expanded.contains(&row.id),
                        is_drop_target: drag
                            .as_ref()
                            .map(|d| d.hover_row == i && d.id != row.id)
                            .unwrap_or(false),
                        rename_draft: renaming
                            .as_ref()
                            .filter(|(id, _)| *id == row.id)
                            .map(|(_, t)| t.clone()),
                    },
                )
            })
            .collect::<Vec<_>>(),
    );

    Column(Modifier::new().fill_max_size()).child((
        PanelHeader(Symbols::layers, "Layers", vec![]),
        list,
    ))
}

struct LayerRowState {
    index: usize,
    is_selected: bool,
    is_expanded: bool,
    is_drop_target: bool,
    rename_draft: Option<String>,
}

fn LayerRowView(session: SessionRef, row: LayerRow, st: LayerRowState) -> View {
    let th = theme();
    let bg = if st.is_selected {
        th.secondary_container
    } else if st.is_drop_target {
        th.primary_container
    } else {
        th.surface_container
    };
    let indent = 8.0 + row.depth as f32 * 16.0;
    let index = st.index;

    let id = row.id;
    let visible = row.visible;
    let locked = row.locked;
    let kind = row.kind;
    let name = row.name.clone();
    let child_count = row.child_count;

    Row(
        Modifier::new()
            .height(ROW_HEIGHT)
            .fill_max_width()
            .padding_values(PaddingValues {
                left: indent,
                right: 4.0,
                top: 0.0,
                bottom: 0.0,
            })
            .align_items(AlignItems::CENTER)
            .gap(2.0)
            .background(bg)
            .on_pointer_down({
                let session = session.clone();
                let row = row.clone();
                move |pe: PointerEvent| {
                    if matches!(pe.event, PointerEventKind::Down(PointerButton::Primary)) {
                        let mut s = session.borrow_mut();
                        s.renaming = None;
                        if pe.modifiers.shift || pe.modifiers.ctrl {
                            s.apply_outputs(smallvec![ToolOutput::RequestSelection(
                                toggle_in_selection(row.id)
                            )]);
                        } else {
                            s.apply_outputs(smallvec![ToolOutput::RequestSelection(
                                select_only(row.id)
                            )]);
                        }
                        s.revision = s.revision.wrapping_add(1);
                        request_frame();
                    }
                }
            })
            .on_pointer_move({
                let session = session.clone();
                let row = row.clone();
                move |pe: PointerEvent| {
                    let mut s = session.borrow_mut();
                    if s.layer_drag.is_none() {
                        return;
                    }
                    let d = s.layer_drag.as_mut().unwrap();
                    d.hover_row = index;
                    d.before = pe.position.y < ROW_HEIGHT * 0.5;
                    d.as_child = row.kind == LayerKind::Group
                        && pe.position.x > indent + 48.0;
                    s.revision = s.revision.wrapping_add(1);
                    request_frame();
                }
            })
            .on_pointer_up({
                let session = session.clone();
                move |_pe: PointerEvent| {
                    let mut s = session.borrow_mut();
                    let Some(drag) = s.layer_drag.take() else {
                        return;
                    };
                    let rows = flatten_layers(
                        &s.file.document,
                        s.file.document.main,
                        &s.expanded_layers,
                    );
                    let target = rows.get(drag.hover_row);
                    let committed = match target {
                        Some(target) => {
                            let cyclic =
                                drag.as_child && is_ancestor(&s.file.document, drag.id, target.id);
                            !cyclic
                                && drop_command(drag.id, target, drag.before, drag.as_child)
                                    .filter(|cmd| !move_is_noop(&s.file.document, cmd))
                                    .is_some_and(|cmd| {
                                        s.apply_outputs(smallvec![
                                            ToolOutput::BeginTransaction("Reorder layer".into()),
                                            ToolOutput::Commands(smallvec![cmd]),
                                            ToolOutput::CommitTransaction,
                                        ]);
                                        true
                                    })
                        }
                        None => false,
                    };
                    if !committed {
                        s.revision = s.revision.wrapping_add(1);
                        request_frame();
                    }
                }
            })
            .on_double_click({
                let session = session.clone();
                let name = name.clone();
                move || {
                    let mut s = session.borrow_mut();
                    if !s.file.document.nodes.get(id).map(|n| n.locked).unwrap_or(true) {
                        s.renaming = Some((id, name.clone()));
                        request_frame();
                    }
                }
            }),
    )
    .child((
        // Expand chevron (groups only)
        if kind == LayerKind::Group && child_count > 0 {
            CompactIconAction(
                if st.is_expanded {
                    Symbols::expand_more
                } else {
                    Symbols::chevron_right
                },
                if st.is_expanded { "Collapse" } else { "Expand" },
                {
                    let session = session.clone();
                    move || {
                        let mut s = session.borrow_mut();
                        if s.expanded_layers.contains(&id) {
                            s.expanded_layers.remove(&id);
                        } else {
                            s.expanded_layers.insert(id);
                        }
                        s.revision = s.revision.wrapping_add(1);
                        request_frame();
                    }
                },
            )
        } else {
            Box(Modifier::new().width(40.0)) // spacer
        },

        // Kind glyph
        AppIcon(
            match kind {
                LayerKind::Shape => Symbols::circle,
                LayerKind::Style => Symbols::format_color_fill,
                LayerKind::Group => Symbols::layers,
                LayerKind::Other => Symbols::layers,
            },
            18.0,
        ),

        // Name or rename field
        if let Some(draft) = st.rename_draft {
            rename_field(session.clone(), id, draft)
        } else {
            Text(name)
                .size(th.typography.body_medium)
                .color(if visible {
                    th.on_surface
                } else {
                    th.on_surface_variant
                })
                .modifier(Modifier::new().flex_grow(1.0))
        },

        // Visibility
        CompactIconAction(
            if visible {
                Symbols::visibility
            } else {
                Symbols::visibility_off
            },
            "Toggle visibility",
            {
                let session = session.clone();
                move || {
                    session
                        .borrow_mut()
                        .apply_outputs(smallvec![
                            ToolOutput::BeginTransaction("Visibility".into()),
                            ToolOutput::Commands(smallvec![cmd_toggle_visible(id, visible)]),
                            ToolOutput::CommitTransaction,
                        ]);
                }
            },
        ),

        // Lock
        CompactIconAction(
            if locked {
                Symbols::lock
            } else {
                Symbols::lock_open
            },
            "Toggle lock",
            {
                let session = session.clone();
                move || {
                    session
                        .borrow_mut()
                        .apply_outputs(smallvec![
                            ToolOutput::BeginTransaction("Lock".into()),
                            ToolOutput::Commands(smallvec![cmd_toggle_locked(id, locked)]),
                            ToolOutput::CommitTransaction,
                        ]);
                }
            },
        ),
    ))
}

fn rename_field(session: SessionRef, id: renamite_model::NodeId, draft: String) -> View {
    Row(
        Modifier::new()
            .flex_grow(1.0)
            .gap(2.0)
            .align_items(AlignItems::CENTER),
    )
    .child((
        Text(draft.clone())
            .size(theme().typography.body_medium)
            .modifier(Modifier::new().flex_grow(1.0)),
        CompactIconAction(Symbols::undo, "Cancel rename", {
            let session = session.clone();
            move || {
                session.borrow_mut().renaming = None;
                request_frame();
            }
        }),
        CompactIconAction(Symbols::save, "Apply name", {
            let session = session.clone();
            let draft = draft.clone();
            move || {
                let mut s = session.borrow_mut();
                let name = draft.trim().to_string();
                s.renaming = None;
                if !name.is_empty() {
                    s.apply_outputs(smallvec![
                        ToolOutput::BeginTransaction("Rename".into()),
                        ToolOutput::Commands(smallvec![cmd_rename(id, name)]),
                        ToolOutput::CommitTransaction,
                    ]);
                } else {
                    request_frame();
                }
            }
        }),
    ))
}