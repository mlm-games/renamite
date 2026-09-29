//! Layers panel: M3 list with visibility, lock, expand, select, reorder, rename.

use renamite_behavior_common::layers::{
    LayerKind, LayerRow, cmd_toggle_locked, cmd_toggle_visible, flatten_layers, select_only,
    toggle_in_selection,
};
use renamite_history::ToolOutput;
use repose_core::dnd::{DragDropModifierExt, drag_preview_label, provide_drag_preview};
use repose_core::geometry::Rect;
use repose_core::input::{Key, KeyEvent, PointerButton, PointerEvent, PointerEventKind};
use repose_core::{
    AlignItems, Dp, Modifier, PaddingValues, Px, View, dp_to_px, keyed, px_to_dp,
    remember_with_key, request_frame, theme,
};
use repose_ui::scroll::{ScrollArea, remember_scroll_state};
use repose_ui::textfield::{BasicTextField, TextFieldConfig, TextFieldState};
use repose_ui::{Box, Column, Row, Text, TextStyle, ViewExt};
use smallvec::smallvec;
use std::cell::RefCell;
use std::rc::Rc;

use crate::components::{CompactIconAction, PanelHeader};
use crate::session::{
    ContextMenuSource, ContextMenuState, LayerDropHover, SessionRef, overlay_anchor,
};
use crate::symbols::{AppIcon, Symbols};
use renamite_behavior_common::context_menu::{MenuContext, layers_menu};

const ROW_HEIGHT: f32 = 40.0;
const ROW_GAP: f32 = 2.0;

/// Payload moved while a layer row is dragged to a new slot.
pub struct LayerDragPayload {
    pub id: renamite_model::NodeId,
}

/// Where inside a row a drop lands.
///
/// The vertical band picks before/after (or child, in the middle third); the
/// horizontal position only matters for the child case, and only past the row's
/// own indent so a drop near the label does not silently reparent.
fn layer_drop_slot(
    row_rect: &std::cell::Cell<Rect>,
    row: &LayerRow,
    position_px: repose_core::Vec2,
) -> (bool, bool) {
    let r = row_rect.get();
    let local_x = px_to_dp(Px(position_px.x - dp_to_px(Dp(r.x)).0)).0;
    let local_y = px_to_dp(Px(position_px.y - dp_to_px(Dp(r.y)).0)).0;
    let y_in_row = local_y.clamp(0.0, ROW_HEIGHT);
    let indent = 8.0 + row.depth as f32 * 16.0;
    let can_nest = row.kind == LayerKind::Group || row.kind == LayerKind::Shape;
    let middle = (ROW_HEIGHT * 0.30..=ROW_HEIGHT * 0.70).contains(&y_in_row);
    let as_child = middle && can_nest && local_x > indent + 20.0;
    let before = if as_child {
        false
    } else {
        y_in_row < ROW_HEIGHT * 0.5
    };
    (before, as_child)
}

/// Bind the drag handlers for a row. Returns a modifier that is both the drag
/// source (the row) and a drop target that reorders within it.
fn layer_drag_handlers(
    m: Modifier,
    session: SessionRef,
    row: LayerRow,
    row_rect: Rc<std::cell::Cell<Rect>>,
) -> Modifier {
    let accent = theme().primary;
    let drag_id = row.id;
    let drag_label = row.name.clone();
    let over_row = row.clone();
    let over_rect = row_rect.clone();
    let over_session = session.clone();
    let leave_row = row.clone();
    let leave_session = session.clone();
    let drop_row = row;
    let drop_session = session.clone();
    let drop_rect = row_rect.clone();
    let end_session = session;

    m.cursor(repose_core::CursorIcon::Grab)
        .drag_source(move |_| {
            provide_drag_preview(drag_preview_label(drag_label.clone(), accent));
            Some(LayerDragPayload { id: drag_id })
        })
        .on_drag_end(move |_| end_session.borrow_mut().clear_layer_drop())
        .on_globally_positioned(move |r| row_rect.set(r))
        .on_drag_over_typed(move |ev, p| {
            let (before, as_child) = layer_drop_slot(&over_rect, &over_row, ev.position);
            over_session
                .borrow_mut()
                .hover_layer_drop(p.id, over_row.id, before, as_child);
        })
        .on_drag_leave_typed(move |_, p| {
            let mut s = leave_session.borrow_mut();
            if s.layer_drop_hover
                .is_some_and(|h| h.dragged == p.id && h.target == leave_row.id)
            {
                s.clear_layer_drop();
            }
        })
        .on_drop_typed(move |ev, p| {
            let (before, as_child) = layer_drop_slot(&drop_rect, &drop_row, ev.position);
            drop_session
                .borrow_mut()
                .apply_layer_drop(p.id, &drop_row, before, as_child);
            true
        })
}

pub fn LayersPanel(session: SessionRef) -> View {
    let (rows, selected, expanded, hover, renaming) = {
        let s = session.borrow();
        let rows = flatten_layers(&s.file.document, s.file.document.main, &s.expanded_layers);
        (
            rows,
            s.selection.nodes.clone(),
            s.expanded_layers.clone(),
            s.layer_drop_hover,
            s.renaming.clone(),
        )
    };

    let list = Column(
        Modifier::new()
            .fill_max_width()
            .padding_values(PaddingValues {
                left: Dp(4.0),
                right: Dp(4.0),
                top: Dp(0.0),
                bottom: Dp(8.0),
            })
            .gap(Dp(ROW_GAP)),
    )
    .child(
        rows.iter()
            .enumerate()
            .map(|(i, row)| {
                keyed(format!("{:?}", row.id), || {
                    LayerRowView(
                        session.clone(),
                        row.clone(),
                        LayerRowState {
                            index: i,
                            is_selected: selected.contains(&row.id),
                            is_expanded: expanded.contains(&row.id),
                            hover,
                            rename_draft: renaming
                                .as_ref()
                                .filter(|(id, _)| *id == row.id)
                                .map(|(_, t)| t.clone()),
                        },
                    )
                })
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
    hover: Option<LayerDropHover>,
    rename_draft: Option<String>,
}

fn LayerRowView(session: SessionRef, row: LayerRow, st: LayerRowState) -> View {
    let th = theme();
    let is_drop_target = st
        .hover
        .is_some_and(|h| h.target == row.id && h.dragged != row.id);
    let drop_as_child = st.hover.is_some_and(|h| h.as_child && h.target == row.id);
    let drop_before = st.hover.map(|h| h.before).unwrap_or(true);
    let bg = if st.is_selected {
        th.secondary_container
    } else if is_drop_target && drop_as_child {
        th.primary_container.with_alpha(180)
    } else {
        th.surface_container
    };
    let indent = 8.0 + row.depth as f32 * 16.0;

    let id = row.id;
    let visible = row.visible;
    let locked = row.locked;
    let kind = row.kind;
    let name = row.name.clone();
    let child_count = row.child_count;
    let row_index = st.index;
    let show_sibling_divider = is_drop_target && !drop_as_child;
    let divider = Box(Modifier::new()
        .height(Dp(2.0))
        .fill_max_width()
        .background(th.primary)
        .padding_values(PaddingValues {
            left: Dp(indent),
            right: Dp(4.0),
            top: Dp(0.0),
            bottom: Dp(0.0),
        }));

    // Keyed on the row id, not `remember_auto`: rows are a list whose membership
    // changes with expand/collapse, and an auto slot is positional.
    let row_rect: Rc<std::cell::Cell<Rect>> =
        remember_with_key(format!("layer_row_rect:{id:?}"), || {
            std::cell::Cell::new(Rect::default)
        });
    let row_view = Row(layer_drag_handlers(
        Modifier::new()
            .height(Dp(ROW_HEIGHT))
            .fill_max_width()
            .padding_values(PaddingValues {
                left: Dp(indent),
                right: Dp(4.0),
                top: Dp(0.0),
                bottom: Dp(0.0),
            })
            .align_items(AlignItems::CENTER)
            .gap(Dp(2.0))
            .background(bg),
        session.clone(),
        row.clone(),
        row_rect,
    )
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
                s.clear_layer_drop();
                if pe.modifiers.ctrl {
                    s.apply_outputs(smallvec![ToolOutput::RequestSelection(
                        toggle_in_selection(row.id)
                    )]);
                } else if pe.modifiers.shift {
                    let rows =
                        flatten_layers(&s.file.document, s.file.document.main, &s.expanded_layers);
                    let anchor = s
                        .selection
                        .nodes
                        .last()
                        .and_then(|a| rows.iter().position(|r| &r.id == a))
                        .unwrap_or(row_index);
                    let (lo, hi) = if anchor <= row_index {
                        (anchor, row_index)
                    } else {
                        (row_index, anchor)
                    };
                    let ids = rows[lo..=hi.min(rows.len().saturating_sub(1))]
                        .iter()
                        .map(|r| r.id)
                        .collect::<Vec<_>>();
                    s.apply_outputs(smallvec![ToolOutput::RequestSelection(
                        renamite_history::SelectionChange::Set(ids)
                    )]);
                } else {
                    s.apply_outputs(smallvec![ToolOutput::RequestSelection(select_only(row.id))]);
                }
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
    }))
    .child((
        // Expand chevron: any row with children (matches `is_expandable`).
        if child_count > 0 {
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
            Box(Modifier::new().width(Dp(40.0))) // spacer
        },
        AppIcon(
            match kind {
                LayerKind::Shape => Symbols::circle,
                LayerKind::Style => Symbols::format_color_fill,
                LayerKind::Mask => Symbols::content_cut,
                LayerKind::Use => Symbols::content_copy,
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
                    session.borrow_mut().apply_outputs(smallvec![
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
        if drop_before {
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

    let focused: Rc<std::cell::Cell<bool>> =
        remember_with_key("active_rename_focus", || std::cell::Cell::new(false));

    Row(Modifier::new()
        .flex_grow(1.0)
        .gap(Dp(2.0))
        .align_items(AlignItems::CENTER))
    .child((
        BasicTextField(
            tf_state,
            crate::components::field_container(
                Modifier::new()
                    .flex_grow(1.0)
                    .height(Dp(32.0))
                    .padding_values(crate::components::field_padding()),
                focused.get(),
            )
            .on_focus_changed(crate::shortcuts::note_text_focus)
            .on_key_event({
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
                focus_tracker: Some(focused),
                cursor_brush: Some(crate::components::field_cursor_brush()),
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
