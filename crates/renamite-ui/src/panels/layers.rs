//! Layers panel: M3 list with visibility, lock, expand, select, reorder, rename.

use renamite_behavior_common::layers::{
    LayerKind, LayerRow, cmd_toggle_locked, cmd_toggle_visible, flatten_layers, select_only,
    toggle_in_selection,
};
use renamite_history::ToolOutput;
use repose_core::input::{Key, KeyEvent, PointerButton, PointerEvent, PointerEventKind};
use repose_core::{
    AlignItems, Modifier, PaddingValues, View, remember_with_key, request_frame, theme,
};
use repose_ui::scroll::{ScrollArea, remember_scroll_state};
use repose_ui::textfield::{BasicTextField, TextFieldConfig, TextFieldState};
use repose_ui::{Box, Column, Row, Text, TextStyle, ViewExt};
use smallvec::smallvec;
use std::cell::RefCell;
use std::rc::Rc;

use crate::components::{CompactIconAction, CompactIconActionWithKey, PanelHeader};
use crate::session::{
    ContextMenuSource, ContextMenuState, LayerDragState, SessionRef, overlay_anchor,
};
use crate::symbols::{AppIcon, Symbols};
use renamite_behavior_common::context_menu::{MenuContext, layers_menu};

const ROW_HEIGHT: f32 = 40.0;
const ROW_GAP: f32 = 2.0;

pub fn LayersPanel(session: SessionRef) -> View {
    let (rows, selected, expanded, drag, renaming) = {
        let s = session.borrow();
        let rows = flatten_layers(&s.file.document, s.file.document.main, &s.expanded_layers);
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
            .fill_max_width()
            .padding_values(PaddingValues {
                left: 4.0,
                right: 4.0,
                top: 0.0,
                bottom: 8.0,
            })
            .gap(ROW_GAP)
            // Safety net: a drag can end even if the pointer was released
            // outside any row.
            .on_pointer_up({
                let session = session.clone();
                move |pe: PointerEvent| {
                    if session.borrow().layer_drag.is_some() {
                        pe.consume();
                        session.borrow_mut().finish_layer_drag();
                    }
                }
            })
            .on_pointer_cancel({
                let session = session.clone();
                move |_pe: PointerEvent| {
                    let mut s = session.borrow_mut();
                    if s.layer_drag.take().is_some() {
                        s.repaint();
                    }
                }
            }),
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
                        drop_as_child: drag
                            .as_ref()
                            .map(|d| {
                                d.as_child
                                    && (row.kind == LayerKind::Group
                                        || row.kind == LayerKind::Shape)
                            })
                            .unwrap_or(false),
                        drop_before: drag.as_ref().map(|d| d.before).unwrap_or(true),
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
        PanelHeader(
            Symbols::layers,
            "Layers",
            vec![
                CompactIconAction(Symbols::add, "Add ellipse layer", {
                    let session = session.clone();
                    move || session.borrow_mut().add_ellipse_layer()
                }),
                CompactIconAction(Symbols::unfold_more, "Expand all layers", {
                    let session = session.clone();
                    move || session.borrow_mut().set_all_expanded(true)
                }),
                CompactIconAction(Symbols::unfold_less, "Collapse all layers", {
                    let session = session.clone();
                    move || session.borrow_mut().set_all_expanded(false)
                }),
            ],
        ),
        ScrollArea(
            Modifier::new().fill_max_size(),
            remember_scroll_state("layers_scroll"),
            list,
        ),
    ))
}

struct LayerRowState {
    index: usize,
    is_selected: bool,
    is_expanded: bool,
    is_drop_target: bool,
    drop_as_child: bool,
    drop_before: bool,
    rename_draft: Option<String>,
}

fn LayerRowView(session: SessionRef, row: LayerRow, st: LayerRowState) -> View {
    let th = theme();
    let bg = if st.is_selected {
        th.secondary_container
    } else if st.is_drop_target && st.drop_as_child {
        th.primary_container.with_alpha(180)
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
    let show_sibling_divider = st.is_drop_target && !st.drop_as_child;
    let divider = Box(Modifier::new()
        .height(2.0)
        .fill_max_width()
        .background(th.primary)
        .padding_values(PaddingValues {
            left: indent,
            right: 4.0,
            top: 0.0,
            bottom: 0.0,
        }));
    let row_view = Row(Modifier::new()
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
                let mut s = session.borrow_mut();
                if matches!(pe.event, PointerEventKind::Down(PointerButton::Secondary)) {
                    // Right-click: select the row if needed, then open the menu.
                    if !s.selection.nodes.contains(&row.id) {
                        s.selection.nodes = vec![row.id];
                    }
                    let paint = s.current_paint.clone();
                    let entries = {
                        let ctx = MenuContext {
                            doc: &s.file.document,
                            selection: &s.selection.nodes,
                            comp: s.file.document.main,
                            world_pos: None,
                            has_clipboard: s.clipboard.is_some(),
                            current_paint: &paint,
                        };
                        layers_menu(&ctx, row.id)
                    };
                    s.open_context_menu(ContextMenuState {
                        screen_pos: overlay_anchor(&pe),
                        entries,
                        source: ContextMenuSource::Layers { row: row.id },
                    });
                    return;
                }
                if matches!(pe.event, PointerEventKind::Down(PointerButton::Primary)) {
                    // Clicking outside an active rename field commits it.
                    if s.renaming.is_some() {
                        s.commit_rename();
                    }
                    s.renaming = None;
                    if pe.modifiers.shift || pe.modifiers.ctrl {
                        s.apply_outputs(smallvec![ToolOutput::RequestSelection(
                            toggle_in_selection(row.id)
                        )]);
                    } else {
                        s.apply_outputs(smallvec![ToolOutput::RequestSelection(select_only(
                            row.id
                        ))]);
                    }
                    s.layer_drag = Some(LayerDragState {
                        id: row.id,
                        hover_row: index,
                        before: true,
                        as_child: false,
                    });
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
                if !s
                    .file
                    .document
                    .nodes
                    .get(id)
                    .map(|n| n.locked)
                    .unwrap_or(true)
                {
                    s.renaming = Some((id, name.clone()));
                    request_frame();
                }
            }
        })
        .on_pointer_move({
            let session = session.clone();
            move |pe: PointerEvent| {
                let mut s = session.borrow_mut();
                if s.layer_drag.is_none() {
                    return;
                }
                let step = ROW_HEIGHT + ROW_GAP;
                let content_y = index as f32 * step + pe.position.y;
                let slot = (content_y / step).floor().max(0.0) as usize;
                let rows =
                    flatten_layers(&s.file.document, s.file.document.main, &s.expanded_layers);
                let Some(row) = rows.get(slot) else {
                    pe.consume();
                    s.repaint();
                    return;
                };
                let y_in_row = content_y - slot as f32 * step;
                let middle = y_in_row >= ROW_HEIGHT * 0.30 && y_in_row <= ROW_HEIGHT * 0.70;
                let indent = 8.0 + row.depth as f32 * 16.0;
                let wants_child = (row.kind == LayerKind::Group || row.kind == LayerKind::Shape)
                    && pe.position.x > indent + 20.0;
                let as_child = middle && wants_child;
                let before = if as_child {
                    false
                } else {
                    y_in_row < ROW_HEIGHT * 0.5
                };
                let d = s.layer_drag.as_mut().unwrap();
                d.hover_row = slot;
                d.before = before;
                d.as_child = as_child;
                pe.consume();
                s.repaint();
            }
        })
        .on_pointer_up({
            let session = session.clone();
            move |pe: PointerEvent| {
                pe.consume();
                session.borrow_mut().finish_layer_drag();
            }
        })
        .on_pointer_cancel({
            let session = session.clone();
            move |pe: PointerEvent| {
                pe.consume();
                let mut s = session.borrow_mut();
                if s.layer_drag.take().is_some() {
                    s.repaint();
                }
            }
        }))
    .child((
        // Expand chevron (container rows with children)
        if (kind == LayerKind::Group || kind == LayerKind::Shape) && child_count > 0 {
            CompactIconActionWithKey(
                format!("layers_{:?}_expand", id),
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
                LayerKind::Mask => Symbols::content_cut,
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
        CompactIconActionWithKey(
            format!("layers_{:?}_visibility", id),
            if visible {
                Symbols::visibility
            } else {
                Symbols::visibility_off
            },
            "Toggle visibility",
            {
                let session = session.clone();
                move || {
                    session.borrow_mut().apply_outputs(smallvec![
                        ToolOutput::BeginTransaction("Visibility".into()),
                        ToolOutput::Commands(smallvec![cmd_toggle_visible(id, visible)]),
                        ToolOutput::CommitTransaction,
                    ]);
                }
            },
        ),
        // Lock
        CompactIconActionWithKey(
            format!("layers_{:?}_lock", id),
            if locked {
                Symbols::lock
            } else {
                Symbols::lock_open
            },
            "Toggle lock",
            {
                let session = session.clone();
                move || {
                    session.borrow_mut().apply_outputs(smallvec![
                        ToolOutput::BeginTransaction("Lock".into()),
                        ToolOutput::Commands(smallvec![cmd_toggle_locked(id, locked)]),
                        ToolOutput::CommitTransaction,
                    ]);
                }
            },
        ),
    ));
    if show_sibling_divider {
        if st.drop_before {
            Column(Modifier::new().fill_max_width()).child((divider, row_view))
        } else {
            Column(Modifier::new().fill_max_width()).child((row_view, divider))
        }
    } else {
        row_view
    }
}

fn rename_field(session: SessionRef, _id: renamite_model::NodeId, draft: String) -> View {
    let tf_state = remember_with_key(
        "active_rename_field",
        || RefCell::new(TextFieldState::new()),
    );
    // Seed the field with the current name, selecting it so typing replaces it.
    {
        let mut st = tf_state.borrow_mut();
        if st.text != draft {
            st.text = draft.clone();
            st.select_all();
        }
    }

    Row(Modifier::new()
        .flex_grow(1.0)
        .gap(2.0)
        .align_items(AlignItems::CENTER))
    .child((
        BasicTextField(
            tf_state,
            Modifier::new().flex_grow(1.0).height(32.0).on_key_event({
                let session = session.clone();
                move |ke: KeyEvent| {
                    if matches!(ke.key, Key::Escape) {
                        session.borrow_mut().cancel_rename();
                        return true;
                    }
                    false
                }
            }),
            "",
            TextFieldConfig {
                line_limits: repose_core::TextFieldLineLimits::SingleLine,
                on_change: Some(Rc::new({
                    let session = session.clone();
                    move |text: String| {
                        let mut s = session.borrow_mut();
                        if let Some((_, draft)) = s.renaming.as_mut() {
                            *draft = text;
                        }
                        s.repaint();
                    }
                })),
                on_submit: Some(Rc::new({
                    let session = session.clone();
                    move |_| session.borrow_mut().commit_rename()
                })),
                text_style: repose_core::TextStyle {
                    font_size: theme().typography.body_medium,
                    ..Default::default()
                },
                ..Default::default()
            },
        ),
        CompactIconAction(Symbols::undo, "Cancel rename", {
            let session = session.clone();
            move || session.borrow_mut().cancel_rename()
        }),
        CompactIconAction(Symbols::save, "Apply name", {
            let session = session.clone();
            move || session.borrow_mut().commit_rename()
        }),
    ))
}
