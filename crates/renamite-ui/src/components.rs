use repose_core::{AlignItems, Modifier, View, remember_with_key, theme};
use repose_material::Symbol;
use repose_material::material3::{
    FilledTonalIconButton, IconButton, IconButtonConfig, Surface, SurfaceConfig, TooltipBox,
    TooltipConfig, TooltipState,
};
use repose_ui::{Box, Row, Text, TextStyle, ViewExt};

use crate::symbols::AppIcon;

pub fn PanelSurface(content: View) -> View {
    Surface(
        SurfaceConfig {
            modifier: Modifier::new().fill_max_size(),
            color: theme().surface_container,
            content_color: theme().on_surface,
            shape_radius: 12.0,
            border: Some((1.0, theme().outline_variant)),
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
