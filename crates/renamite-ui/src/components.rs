use repose_core::dnd::{DragDropModifierExt, drag_preview_chip, provide_drag_preview};
use repose_core::input::{Key, KeyEvent};
use repose_core::{
    AlignItems, AlignSelf, Brush, Dp, FocusRequester, KeyboardOptions, Modifier,
    MutableInteractionSource, PaddingValues, Rect, TextFieldLineLimits, UnitExt, View, dp_to_px,
    remember_auto, remember_state_with_key, remember_with_key, request_frame, theme,
};
use repose_material::Symbol;
use repose_material::material3::{
    FilledTonalIconButton, IconButton, IconButtonConfig, Surface, SurfaceConfig, TooltipBox,
    TooltipConfig, TooltipState,
};
use repose_ui::textfield::{BasicTextField, TextFieldConfig, TextFieldState};
use repose_ui::{Box, Column, Row, Text, TextStyle, ViewExt};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::symbols::AppIcon;
use crate::symbols::Symbols;

pub fn PanelSurface(content: View) -> View {
    Surface(
        SurfaceConfig {
            modifier: Modifier::new().fill_max_size(),
            color: theme().surface_container_low,
            content_color: theme().on_surface,
            shape_radius: 14.0.dp(),
            border: Some((1.0.dp(), theme().outline_variant.with_alpha(140))),
            ..Default::default()
        },
        move || content,
    )
}

pub fn PanelHeader(symbol: Symbol, title: impl Into<String>, actions: Vec<View>) -> View {
    let title = title.into();

    Row(Modifier::new()
        .height(Dp(48.0))
        .fill_max_width()
        .padding_values(repose_core::PaddingValues {
            left: Dp(12.0),
            right: Dp(8.0),
            top: Dp(0.0),
            bottom: Dp(0.0),
        })
        .align_items(AlignItems::CENTER))
    .child((
        AppIcon(symbol, 20.0),
        Text(title)
            .size(theme().typography.title_small)
            .modifier(Modifier::new().padding(Dp(8.0))),
        Box(Modifier::new().flex_grow(1.0)),
        Row(Modifier::new().align_items(AlignItems::CENTER)).child(actions),
    ))
}

/// Payload moved while a section card is dragged to a new slot.
pub struct SectionDragPayload {
    pub id: String,
}

/// Opt-in drag-to-reorder wiring for a [`CollapsibleSection`].
///
/// Repose arbitrates click vs drag itself: `on_drag_start` only fires past the
/// pointer slop, and a drag that started suppresses the trailing click, so the
/// header stays a plain collapse toggle until the pointer actually moves.
///
/// `on_drop` receives the dragged id and whether the drop landed in the lower
/// half of the target card (`true` = insert after, `false` = insert before).
#[derive(Clone)]
pub struct SectionDrag {
    pub id: String,
    pub label: String,
    pub on_drop: Rc<dyn Fn(&str, bool)>,
}

/// A Material-style collapsible card: a tappable section header with a chevron
/// that expands/collapses the body underneath. Collapse state is remembered
/// per `key` so it survives recomposition (but resets across sessions).
///
/// `drag` opts the card into the owning panel's drag-to-reorder list; pass
/// `None` to leave it fixed.
pub fn CollapsibleSection(
    key: impl Into<String>,
    title: impl Into<String>,
    actions: Vec<View>,
    body: View,
    drag: Option<SectionDrag>,
) -> View {
    let key = key.into();
    let title = title.into();
    let open = remember_state_with_key(key.clone(), || true);
    let is_open = *open.borrow();
    let th = theme();
    let toggle_open = {
        let open = open.clone();
        move |_| {
            let next = !*open.borrow();
            *open.borrow_mut() = next;
            request_frame();
        }
    };

    let mut card = Modifier::new().fill_max_width();
    let mut header = Modifier::new()
        .height(Dp(40.0))
        .fill_max_width()
        .padding_values(PaddingValues {
            left: Dp(12.0),
            right: Dp(8.0),
            top: Dp(0.0),
            bottom: Dp(0.0),
        })
        .align_items(AlignItems::CENTER)
        .gap(Dp(4.0))
        .clickable()
        .cursor(repose_core::CursorIcon::Pointer)
        .on_pointer_down(toggle_open);

    if let Some(d) = drag {
        let SectionDrag {
            id: drag_id,
            label: drag_label,
            on_drop,
        } = d;
        // Keyed on the card's own collapse key, not `remember_auto`: the panel
        // emits a varying number of cards per selection, and an auto slot is
        // positional, so the rect would migrate between cards when the set
        // changes.
        let card_rect: Rc<Cell<Rect>> =
            remember_with_key(format!("card_rect:{key}"), || Cell::new(Rect::default()));
        let accent = th.primary;
        let rect_for_pos = card_rect.clone();
        card = card
            .on_globally_positioned(move |r| rect_for_pos.set(r))
            .on_drop_typed::<SectionDragPayload>(move |ev, payload| {
                let r = card_rect.get();
                // NOTE: `DropEvent.position` is already window px; only the Dp rect
                // needs converting before the two can be compared.
                let y = ev.position.y;
                let mid = dp_to_px(Dp(r.y + r.h * 0.5)).0;
                on_drop(&payload.id, y > mid);
                true
            });
        header = header
            .cursor(repose_core::CursorIcon::Grab)
            .drag_source(move |_| {
                provide_drag_preview(drag_preview_chip(drag_label.clone(), accent));
                Some(SectionDragPayload {
                    id: drag_id.clone(),
                })
            });
    }

    Surface(
        SurfaceConfig {
            modifier: card,
            color: th.surface_container_low,
            content_color: th.on_surface,
            shape_radius: 12.0.dp(),
            border: Some((1.0.dp(), th.outline_variant.with_alpha(140))),
            ..Default::default()
        },
        move || {
            Column(Modifier::new().fill_max_width()).child((
                Row(header).child((
                    Text(title.clone())
                        .size(th.typography.title_small)
                        .color(th.on_surface)
                        .modifier(Modifier::new().weight(1.0)),
                    Row(Modifier::new().align_items(AlignItems::CENTER)).child(actions),
                    AppIcon(
                        if is_open {
                            Symbols::expand_more
                        } else {
                            Symbols::chevron_right
                        },
                        20.0,
                    ),
                )),
                if is_open {
                    Box(Modifier::new()
                        .fill_max_width()
                        .padding_values(PaddingValues {
                            left: Dp(0.0),
                            right: Dp(0.0),
                            top: Dp(0.0),
                            bottom: Dp(4.0),
                        }))
                    .child(body)
                } else {
                    Box(Modifier::new())
                },
            ))
        },
    )
}

/// Outer box of a [`CompactIconAction`].
///
/// `IconButton` clamps its box up to the M3 48dp minimum touch target whatever
/// `container_size` asks for, so anything that has to line up with one of these
/// measures against this, not against the icon size.
pub const ICON_ACTION_SIZE: Dp = Dp(48.0);

#[track_caller]
pub fn CompactIconAction(
    symbol: Symbol,
    tooltip: &'static str,
    on_click: impl Fn() + 'static,
) -> View {
    compact_icon_action(symbol, tooltip, on_click)
}

#[track_caller]
pub fn CompactIconActionWithKey(
    key: impl Into<String>,
    symbol: Symbol,
    tooltip: &'static str,
    on_click: impl Fn() + 'static,
) -> View {
    let key = key.into();
    let tooltip_state = remember_with_key(format!("compact_tooltip:{key}"), TooltipState::new);
    let interaction_source = remember_with_key(
        format!("compact_interaction:{key}"),
        MutableInteractionSource::new,
    );

    TooltipBox(
        tooltip,
        tooltip_state,
        tooltip_host(),
        IconButton(
            AppIcon(symbol, 22.0),
            on_click,
            IconButtonConfig {
                container_size: Some(40.0.dp()),
                interaction_source: Some(interaction_source.as_ref().clone()),
                ..Default::default()
            },
        ),
        TooltipConfig::default(),
    )
}

#[track_caller]
fn compact_icon_action(
    symbol: Symbol,
    tooltip: &'static str,
    on_click: impl Fn() + 'static,
) -> View {
    let tooltip_state = remember_auto("tooltip", TooltipState::new);
    let interaction_source = remember_auto("interaction", MutableInteractionSource::new);

    TooltipBox(
        tooltip,
        tooltip_state,
        tooltip_host(),
        IconButton(
            AppIcon(symbol, 22.0),
            on_click,
            IconButtonConfig {
                container_size: Some(40.0.dp()),
                interaction_source: Some(interaction_source.as_ref().clone()),
                ..Default::default()
            },
        ),
        TooltipConfig::default(),
    )
}

/// `TooltipBox` defaults its host to `align-self: flex-start`, which overrides
/// the container's `align-items` and pins a button to the top of a row - 8dp low
/// once the 48dp touch box is taller than the row it sits in.
fn tooltip_host() -> Modifier {
    Modifier::new().align_self(AlignSelf::CENTER)
}

#[track_caller]
pub fn ToolAction(
    symbol: Symbol,
    label: &'static str,
    selected: bool,
    on_click: impl Fn() + 'static,
) -> View {
    let tooltip_state = remember_auto("tooltip", TooltipState::new);
    let interaction_source = remember_auto("interaction", MutableInteractionSource::new);

    let config = IconButtonConfig {
        container_size: Some(48.0.dp()),
        shape_radius: Some(16.0.dp()),
        interaction_source: Some(interaction_source.as_ref().clone()),
        ..Default::default()
    };

    let button = if selected {
        FilledTonalIconButton(AppIcon(symbol, 24.0), on_click, config)
    } else {
        IconButton(AppIcon(symbol, 24.0), on_click, config)
    };
    TooltipBox(
        label,
        tooltip_state.clone(),
        Modifier::new(),
        button,
        TooltipConfig::default(),
    )
}

/// Small rounded status pill (save state, record state, frame/range chips).
pub fn StatusChip(
    label: impl Into<String>,
    bg: repose_core::Color,
    fg: repose_core::Color,
) -> View {
    Text(label.into())
        .size(theme().typography.label_small)
        .color(fg)
        .modifier(
            Modifier::new()
                .padding_values(repose_core::PaddingValues {
                    left: Dp(8.0),
                    right: Dp(8.0),
                    top: Dp(4.0),
                    bottom: Dp(4.0),
                })
                .background(bg)
                .clip_rounded(Dp(999.0)),
        )
}

/// Segmented-mode / tab pill that reports its selected state visually.
pub fn PillButton(label: &'static str, selected: bool, on_click: impl Fn() + 'static) -> View {
    PillIconButton(None, label, selected, on_click)
}

/// Segmented pill with a leading Material Symbol; the label doubles as the
/// accessibility text, so icon-only consumers pass `None` as the symbol.
pub fn PillIconButton(
    symbol: Option<Symbol>,
    label: &'static str,
    selected: bool,
    on_click: impl Fn() + 'static,
) -> View {
    let th = theme();
    let bg = if selected {
        th.secondary_container
    } else {
        th.surface_container_high
    };
    let fg = if selected {
        th.on_secondary_container
    } else {
        th.on_surface_variant
    };

    Box(Modifier::new()
        .padding_values(repose_core::PaddingValues {
            left: Dp(10.0),
            right: Dp(10.0),
            top: Dp(6.0),
            bottom: Dp(6.0),
        })
        .background(bg)
        .clip_rounded(Dp(999.0))
        .on_pointer_down(move |_| on_click()))
    .child({
        let mut children: Vec<View> = Vec::new();
        if let Some(s) = symbol {
            children.push(AppIcon(s, 18.0).color(fg));
        }
        children.push(Text(label).size(th.typography.label_medium).color(fg));
        Row(Modifier::new().gap(Dp(6.0)).align_items(AlignItems::CENTER)).child(children)
    })
}

pub const FIELD_RADIUS: Dp = Dp(8.0);

pub fn field_padding() -> PaddingValues {
    PaddingValues {
        left: Dp(8.0),
        right: Dp(8.0),
        top: Dp(6.0),
        bottom: Dp(6.0),
    }
}

/// Container chrome for the editor's compact text inputs. `focused` comes from
/// the field's `focus_tracker`, which the layout pass writes, so the accent
/// lands one frame after the click - the same trade the M3 text field makes for
/// its floating label.
pub fn field_container(m: Modifier, focused: bool) -> Modifier {
    let th = theme();
    m.background(th.surface_container_highest)
        .clip_rounded(FIELD_RADIUS)
        .border(
            if focused { Dp(2.0) } else { Dp(1.0) },
            if focused {
                th.primary
            } else {
                th.outline_variant
            },
            FIELD_RADIUS,
        )
}

pub fn field_cursor_brush() -> Brush {
    Brush::Solid(theme().primary)
}

/// Capabilities beyond plain text entry, for [`AppTextFieldWith`].
#[derive(Default)]
pub struct TextFieldOpts {
    /// Commit hook: fired on Enter / the IME done action, with the current text.
    pub on_submit: Option<Rc<dyn Fn(String)>>,
    /// Platform keyboard hint (numeric keypad, IME action, autocorrect).
    pub keyboard_options: Option<KeyboardOptions>,
    /// Lets the owner pull focus into the field instead of waiting for a tap.
    pub focus_requester: Option<Rc<FocusRequester>>,
    /// Size to the text instead of filling the row, floored at this width. A
    /// `fill_max_width` field resolves against the row's width, not the box it
    /// sits in, so the inspector's number fields pass their own floor here to
    /// stay put next to their label.
    pub min_width: Option<f32>,
}

/// Compact state-backed field. Prefer this over M3 TextField (paste/recompose-safe).
///
/// The model `value` is synced into the field state on recomposition. Edits flow
/// back through `on_change`, so the field never fights the value-driven model
/// and pasted text stays visible.
pub fn AppTextField(
    key: impl Into<String>,
    value: String,
    hint: impl Into<String>,
    single_line: bool,
    min_height: f32,
    on_change: impl Fn(String) + 'static,
) -> View {
    AppTextFieldWith(
        key,
        value,
        hint,
        single_line,
        min_height,
        TextFieldOpts::default(),
        on_change,
    )
}

/// [`AppTextField`] plus the extras a field needs when it isn't a plain
/// model-bound input: commit-on-submit, a keyboard hint, and focus the owner
/// pulls in itself (the inspector's number editor opens and grabs the caret).
#[track_caller]
pub fn AppTextFieldWith(
    key: impl Into<String>,
    value: String,
    hint: impl Into<String>,
    single_line: bool,
    min_height: f32,
    opts: TextFieldOpts,
    on_change: impl Fn(String) + 'static,
) -> View {
    let key = key.into();
    let hint = hint.into();
    let tf_state = remember_with_key(key.clone(), || RefCell::new(TextFieldState::new()));
    let focused: Rc<Cell<bool>> = remember_with_key(format!("{key}_focus"), || Cell::new(false));
    {
        let mut st = tf_state.borrow_mut();
        if st.text != value {
            st.text = value.clone();
            let len = st.text.len();
            st.selection = len..len;
        }
    }
    let th = theme();
    let mut layout = Modifier::new()
        .height(Dp(min_height))
        .padding_values(field_padding());
    layout = match opts.min_width {
        Some(floor) => layout.min_width(Dp(floor)),
        None => layout.fill_max_width(),
    };
    if let Some(requester) = &opts.focus_requester {
        layout = layout.focus_requester(requester.as_ref().clone());
    }
    BasicTextField(
        tf_state,
        field_container(layout, focused.get()).on_focus_changed(crate::shortcuts::note_text_focus),
        hint,
        TextFieldConfig {
            line_limits: if single_line {
                TextFieldLineLimits::SingleLine
            } else {
                TextFieldLineLimits::MultiLine {
                    min_height_in_lines: 2,
                    max_height_in_lines: 8,
                }
            },
            keyboard_options: opts.keyboard_options.unwrap_or(KeyboardOptions::DEFAULT),
            on_change: Some(Rc::new(on_change)),
            on_submit: opts.on_submit,
            focus_tracker: Some(focused),
            cursor_brush: Some(field_cursor_brush()),
            text_style: repose_core::TextStyle {
                font_size: th.typography.body_medium,
                color: Some(th.on_surface),
                ..Default::default()
            },
            ..Default::default()
        },
    )
}

/// Single-line name field that keeps a local draft and only commits on submit
/// (Enter). Mirrors the layer-rename field: typing updates the draft, Enter
/// commits it, Escape discards it. Used for machine / state / input names so
/// that validation (empty/duplicate) fires on commit, not on every keystroke.
pub fn name_field(
    key: impl Into<String>,
    value: String,
    hint: impl Into<String>,
    min_height: f32,
    commit: impl Fn(String) + 'static,
) -> View {
    let key = key.into();
    let hint = hint.into();
    let draft: Rc<RefCell<String>> =
        remember_with_key(format!("{key}_draft"), || RefCell::new(String::new()));
    let focused: Rc<Cell<bool>> = remember_with_key(format!("{key}_focused"), || Cell::new(false));

    // Sync the draft from the model value whenever the field is not focused.
    if !focused.get() && *draft.borrow() != value {
        *draft.borrow_mut() = value.clone();
    }

    let tf_state = remember_with_key(key.clone(), || RefCell::new(TextFieldState::new()));
    {
        let mut st = tf_state.borrow_mut();
        if st.text != *draft.borrow() {
            st.text = draft.borrow().clone();
            let len = st.text.len();
            st.selection = len..len;
        }
    }

    let th = theme();
    let focus = focused.clone();
    BasicTextField(
        tf_state,
        field_container(
            Modifier::new()
                .fill_max_width()
                .height(Dp(min_height))
                .padding_values(field_padding()),
            focused.get(),
        )
        .on_focus_changed(crate::shortcuts::note_text_focus)
        .on_key_event(move |ek: KeyEvent| {
            if matches!(ek.key, Key::Escape) {
                focused.set(false);
                return true;
            }
            false
        }),
        hint,
        TextFieldConfig {
            line_limits: TextFieldLineLimits::SingleLine,
            focus_tracker: Some(focus),
            on_change: Some(Rc::new({
                let draft = draft.clone();
                move |text: String| *draft.borrow_mut() = text
            })),
            on_submit: Some(Rc::new(move |_| {
                let trimmed = draft.borrow().trim().to_owned();
                commit(trimmed);
            })),
            cursor_brush: Some(field_cursor_brush()),
            text_style: repose_core::TextStyle {
                font_size: th.typography.body_medium,
                color: Some(th.on_surface),
                ..Default::default()
            },
            ..Default::default()
        },
    )
}
