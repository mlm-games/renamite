use repose_core::{
    AlignItems, Modifier, PaddingValues, View, remember_state_with_key, remember_with_key,
    request_frame, theme,
};
use repose_material::Symbol;
use repose_material::material3::{
    FilledTonalIconButton, IconButton, IconButtonConfig, Surface, SurfaceConfig, TooltipBox,
    TooltipConfig, TooltipState,
};
use repose_ui::{Box, Column, Row, Text, TextStyle, ViewExt};

use crate::symbols::Symbols;
use crate::symbols::AppIcon;

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
                    Box(Modifier::new().fill_max_width().padding_values(PaddingValues {
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
