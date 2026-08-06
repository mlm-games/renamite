use repose_core::{Modifier, View, theme};
use repose_material::material3::{
    NavItem, NavigationBar, NavigationBarConfig, Scaffold, ScaffoldConfig,
};
use repose_ui::{Box, Column, Row, ViewExt};
use std::rc::Rc;

use crate::components::PanelSurface;
use crate::panels::{LayersPanel, PropertiesPanel, TimelinePanel, ViewportPanel};
use crate::session::{PanelPage, SessionRef};
use crate::symbols::{AppIcon, Symbols};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellClass {
    Expanded,
    Medium,
    Compact,
}

pub fn platform_shell_class() -> ShellClass {
    #[cfg(target_os = "android")]
    {
        ShellClass::Compact
    }

    #[cfg(all(target_arch = "wasm32", not(target_os = "android")))]
    {
        ShellClass::Medium
    }

    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
    {
        ShellClass::Expanded
    }
}

pub fn EditorShell(session: SessionRef) -> View {
    let class = platform_shell_class();
    let session_body = session.clone();

    Scaffold(
        move |_content_padding| match class {
            ShellClass::Expanded => ExpandedWorkspace(session_body.clone()),
            ShellClass::Medium => MediumWorkspace(session_body.clone()),
            ShellClass::Compact => CompactWorkspace(session_body.clone()),
        },
        ScaffoldConfig {
            top_bar: Some(crate::AppTopBar(session.clone())),
            bottom_bar: match class {
                ShellClass::Compact => Some(BottomNavigation(session)),
                _ => None,
            },
            container_color: theme().surface_container_lowest,
            ..Default::default()
        },
    )
}

fn ExpandedWorkspace(session: SessionRef) -> View {
    Row(Modifier::new().fill_max_size().padding(8.0).gap(8.0)).child((
        crate::ToolRail(session.clone()),
        Box(Modifier::new().width(264.0).fill_max_height())
            .child(PanelSurface(LayersPanel(session.clone()))),
        Column(Modifier::new().fill_max_size().weight(1.0).gap(8.0)).child((
            Box(Modifier::new().weight(1.0).fill_max_width())
                .child(PanelSurface(ViewportPanel(session.clone()))),
            Box(Modifier::new().height(240.0).fill_max_width())
                .child(PanelSurface(TimelinePanel(session.clone()))),
        )),
        Box(Modifier::new().width(320.0).fill_max_height())
            .child(PanelSurface(PropertiesPanel(session))),
    ))
}

fn MediumWorkspace(session: SessionRef) -> View {
    Row(Modifier::new().fill_max_size().padding(8.0).gap(8.0)).child((
        crate::ToolRail(session.clone()),
        Box(Modifier::new().weight(1.0).fill_max_height())
            .child(PanelSurface(ViewportPanel(session.clone()))),
        Box(Modifier::new().width(288.0).fill_max_height())
            .child(PanelSurface(active_side_panel(session))),
    ))
}

fn CompactWorkspace(session: SessionRef) -> View {
    let page = session.borrow().active_page;
    match page {
        PanelPage::Canvas => ViewportPanel(session),
        PanelPage::Layers => LayersPanel(session),
        PanelPage::Timeline => TimelinePanel(session),
        PanelPage::Inspect => PropertiesPanel(session),
    }
}

fn active_side_panel(session: SessionRef) -> View {
    let page = session.borrow().active_page;
    match page {
        PanelPage::Layers => LayersPanel(session),
        PanelPage::Timeline => TimelinePanel(session),
        PanelPage::Inspect => PropertiesPanel(session),
        PanelPage::Canvas => LayersPanel(session),
    }
}

fn BottomNavigation(session: SessionRef) -> View {
    let selected = session.borrow().active_page as usize;

    NavigationBar(
        selected,
        vec![
            nav_item(session.clone(), PanelPage::Canvas, Symbols::edit, "Canvas"),
            nav_item(
                session.clone(),
                PanelPage::Layers,
                Symbols::layers,
                "Layers",
            ),
            nav_item(
                session.clone(),
                PanelPage::Timeline,
                Symbols::play_arrow,
                "Animate",
            ),
            nav_item(session, PanelPage::Inspect, Symbols::settings, "Inspect"),
        ],
        NavigationBarConfig::default(),
    )
}

fn nav_item(
    session: SessionRef,
    page: PanelPage,
    symbol: repose_material::Symbol,
    label: &'static str,
) -> NavItem {
    NavItem {
        icon: AppIcon(symbol, 24.0),
        label: label.to_owned(),
        on_click: Rc::new(move || {
            let mut s = session.borrow_mut();
            s.active_page = page;
            s.revision = s.revision.wrapping_add(1);
            repose_core::request_frame();
        }),
        enabled: true,
        interaction_source: None,
    }
}
