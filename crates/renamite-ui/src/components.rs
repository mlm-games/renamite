use repose_core::{
    AlignItems, Modifier, PaddingValues, TextFieldLineLimits, View, remember_state_with_key,
    remember_with_key, request_frame, theme,
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
            shape_radius: 14.0,
            border: Some((1.0, theme().outline_variant.with_alpha(140))),
            ..Default::default()
        },
        move || content,
    )
}

pub fn PanelHeader(symbol: Symbol, title: impl Into<String>, actions: Vec<View>) -> View {
    let title = title.into();

    Row(Modifier::new()
        .height(48.0)
        .fill_max_width()
        .padding_values(repose_core::PaddingValues {
            left: 12.0,
            right: 8.0,
            top: 0.0,
            bottom: 0.0,
        })
        .align_items(AlignItems::CENTER))
    .child((
        AppIcon(symbol, 20.0),
        Text(title)
            .size(theme().typography.title_small)
            .modifier(Modifier::new().padding(8.0)),
        Box(Modifier::new().flex_grow(1.0)),
        Row(Modifier::new().align_items(AlignItems::CENTER)).child(actions),
    ))
}

/// A Material-style collapsible card: a tappable section header with a chevron
/// that expands/collapses the body underneath. Collapse state is remembered
/// per `key` so it survives recomposition (but resets across sessions).
pub fn CollapsibleSection(
    key: impl Into<String>,
    title: impl Into<String>,
    actions: Vec<View>,
    body: View,
) -> View {
    let title = title.into();
    let open = remember_state_with_key(key, || true);
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

    Surface(
        SurfaceConfig {
            modifier: Modifier::new().fill_max_width(),
            color: th.surface_container_low,
            content_color: th.on_surface,
            shape_radius: 12.0,
            border: Some((1.0, th.outline_variant.with_alpha(140))),
            ..Default::default()
        },
        move || {
            Column(Modifier::new().fill_max_width()).child((
                Row(Modifier::new()
                    .height(40.0)
                    .fill_max_width()
                    .padding_values(PaddingValues {
                        left: 12.0,
                        right: 8.0,
                        top: 0.0,
                        bottom: 0.0,
                    })
                    .align_items(AlignItems::CENTER)
                    .gap(4.0)
                    .clickable()
                    .cursor(repose_core::CursorIcon::Pointer)
                    .on_pointer_down(toggle_open))
                .child((
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
                            left: 0.0,
                            right: 0.0,
                            top: 0.0,
                            bottom: 4.0,
                        }))
                    .child(body)
                } else {
                    Box(Modifier::new())
                },
            ))
        },
    )
}

pub fn CompactIconAction(
    symbol: Symbol,
    tooltip: &'static str,
    on_click: impl Fn() + 'static,
) -> View {
    let tooltip_state = remember_with_key(
        format!("tooltip_{}_{}", symbol.name, tooltip),
        TooltipState::new,
    );

    TooltipBox(
        tooltip,
        (*tooltip_state).clone(),
        IconButton(
            AppIcon(symbol, 22.0),
            on_click,
            IconButtonConfig {
                container_size: Some(40.0),
                ..Default::default()
            },
        ),
        TooltipConfig::default(),
    )
}

pub fn ToolAction(
    symbol: Symbol,
    label: &'static str,
    selected: bool,
    on_click: impl Fn() + 'static,
) -> View {
    let tooltip_state = remember_with_key(format!("tool_tip_{label}"), TooltipState::new);

    let config = IconButtonConfig {
        container_size: Some(48.0),
        shape_radius: Some(16.0),
        ..Default::default()
    };

    let button = if selected {
        FilledTonalIconButton(AppIcon(symbol, 24.0), on_click, config)
    } else {
        IconButton(AppIcon(symbol, 24.0), on_click, config)
    };

    TooltipBox(
        label,
        (*tooltip_state).clone(),
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
                    left: 8.0,
                    right: 8.0,
                    top: 4.0,
                    bottom: 4.0,
                })
                .background(bg)
                .clip_rounded(999.0),
        )
}

/// Segmented-mode / tab pill that reports its selected state visually.
pub fn PillButton(label: &'static str, selected: bool, on_click: impl Fn() + 'static) -> View {
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
            left: 10.0,
            right: 10.0,
            top: 6.0,
            bottom: 6.0,
        })
        .background(bg)
        .clip_rounded(999.0)
        .on_pointer_down(move |_| on_click()))
    .child(Text(label).size(th.typography.label_medium).color(fg))
}

/// Compact state-backed field. Prefer this over M3 TextField (paste/recompose-safe).
///
/// The model `value` is synced into the field state on recomposition; edits flow
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
    let key = key.into();
    let hint = hint.into();
    let tf_state = remember_with_key(key, || RefCell::new(TextFieldState::new()));
    {
        let mut st = tf_state.borrow_mut();
        if st.text != value {
            st.text = value.clone();
            let len = st.text.len();
            st.selection = len..len;
        }
    }
    let th = theme();
    BasicTextField(
        tf_state,
        Modifier::new()
            .fill_max_width()
            .height(min_height)
            .padding_values(PaddingValues {
                left: 8.0,
                right: 8.0,
                top: 6.0,
                bottom: 6.0,
            })
            .background(th.surface_container_highest)
            .clip_rounded(8.0),
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
            on_change: Some(Rc::new(on_change)),
            text_style: repose_core::TextStyle {
                font_size: th.typography.body_medium,
                color: Some(th.on_surface),
                ..Default::default()
            },
            ..Default::default()
        },
    )
}

pub fn deferred_name_field(
    key: impl Into<String>,
    value: String,
    hint: impl Into<String>,
    min_height: f32,
    commit: impl Fn(String) -> Result<(), String> + 'static,
) -> View {
    let key = key.into();
    let hint = hint.into();
    let draft: Rc<RefCell<String>> =
        remember_with_key(format!("{key}_draft"), || RefCell::new(String::new()));
    let focused: Rc<Cell<bool>> = remember_with_key(format!("{key}_focused"), || Cell::new(false));
    let was_focused: Rc<Cell<bool>> =
        remember_with_key(format!("{key}_was_focused"), || Cell::new(false));
    let error: Rc<RefCell<Option<String>>> =
        remember_with_key(format!("{key}_error"), || RefCell::new(None));
    let tf_state = remember_with_key(key.clone(), || RefCell::new(TextFieldState::new()));

    let is_focused = focused.get();

    {
        let mut d = draft.borrow_mut();
        if !is_focused && *d != value {
            *d = value.clone();
            *error.borrow_mut() = None;
        }
        if was_focused.get() && !is_focused {
            let text = d.trim().to_string();
            match commit(text) {
                Ok(()) => {
                    *error.borrow_mut() = None;
                    *d = value.clone();
                }
                Err(message) => *error.borrow_mut() = Some(message),
            }
        }
        was_focused.set(is_focused);
    }

    let th = theme();
    let target = if is_focused {
        draft.borrow().clone()
    } else {
        value.clone()
    };
    {
        let mut st = tf_state.borrow_mut();
        if st.text != target {
            st.text = target;
            let len = st.text.len();
            st.selection = len..len;
        }
    }

    let focus = focused.clone();
    let config = TextFieldConfig {
        line_limits: TextFieldLineLimits::SingleLine,
        on_change: Some(Rc::new({
            let draft = draft.clone();
            move |text: String| *draft.borrow_mut() = text
        })),
        on_submit: Some(Rc::new({
            let draft = draft.clone();
            let error = error.clone();
            let commit = Rc::new(commit);
            move |text: String| {
                let trimmed = text.trim().to_string();
                match commit(trimmed.clone()) {
                    Ok(()) => {
                        *error.borrow_mut() = None;
                        *draft.borrow_mut() = trimmed;
                    }
                    Err(message) => *error.borrow_mut() = Some(message),
                }
            }
        })),
        focus_tracker: Some(focus),
        text_style: repose_core::TextStyle {
            font_size: th.typography.body_medium,
            color: Some(th.on_surface),
            ..Default::default()
        },
        ..Default::default()
    };
    let text_field = BasicTextField(
        tf_state,
        Modifier::new()
            .fill_max_width()
            .height(min_height)
            .padding_values(PaddingValues {
                left: 8.0,
                right: 8.0,
                top: 6.0,
                bottom: 6.0,
            })
            .background(th.surface_container_highest)
            .clip_rounded(8.0),
        hint,
        config,
    );

    match error.borrow().clone() {
        Some(message) => Column(Modifier::new().fill_max_width()).child((
            text_field,
            Text(message)
                .size(th.typography.label_small)
                .color(th.error)
                .modifier(Modifier::new().padding_values(PaddingValues {
                    left: 8.0,
                    right: 8.0,
                    top: 2.0,
                    bottom: 0.0,
                })),
        )),
        None => text_field,
    }
}
