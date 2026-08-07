use repose_core::{JustifyContent, Modifier, PaddingValues, View, remember_with_key, theme};
use repose_material::material3::{
    Button, ButtonConfig, Dialog, DialogProperties, NavItem, NavigationBar, NavigationBarConfig,
    Scaffold, ScaffoldConfig, TextButton,
};
use repose_ui::overlay::OverlayHandle;
use repose_ui::{Box, Column, Row, Spacer, Text, TextStyle, ViewExt, ZStack};
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
    let overlay = remember_with_key("shell_overlay", OverlayHandle::new);
    let class = platform_shell_class();
    let session_body = session.clone();

    let scaffold = Scaffold(
        move |_content_padding| match class {
            ShellClass::Expanded => ExpandedWorkspace(session_body.clone()),
            ShellClass::Medium => MediumWorkspace(session_body.clone()),
            ShellClass::Compact => CompactWorkspace(session_body.clone()),
        },
        ScaffoldConfig {
            top_bar: Some(crate::AppTopBar(session.clone())),
            bottom_bar: match class {
                ShellClass::Compact => Some(BottomNavigation(session.clone())),
                _ => None,
            },
            container_color: theme().surface_container_lowest,
            ..Default::default()
        },
    );

    let confirm = discard_dialog(session.clone(), (*overlay).clone());
    let picker = color_picker_overlay(session.clone());

    overlay.host(
        Modifier::new().fill_max_size(),
        ZStack(Modifier::new().fill_max_size()).child((scaffold, picker, confirm)),
    )
}

/// Transparent-to-closem modal layer containing the color picker popover,
/// anchored under the top bar when a picker is open.
fn color_picker_overlay(session: SessionRef) -> View {
    let Some(picker) = session.borrow().open_picker.clone() else {
        return ZStack(Modifier::new());
    };
    let swatches = session.borrow().swatches.colors.clone();

    let session_close = session.clone();
    let session_change = session.clone();
    let session_commit = session.clone();
    let session_add = session.clone();
    let session_done = session.clone();

    ZStack(Modifier::new().fill_max_size()).child((
        // Scrim: click anywhere outside the picker to dismiss (cancels).
        Box(Modifier::new()
            .fill_max_size()
            .background(repose_core::Color(0, 0, 0, 90))
            .on_pointer_down(move |_| {
                session_close.borrow_mut().close_color_picker();
            })),
        Box(Modifier::new()
            .absolute()
            .offset(Some(16.0), Some(56.0), None, None))
        .child(crate::color_picker::ColorPicker(
            picker.state.clone(),
            swatches,
            Rc::new(move |c| {
                session_change.borrow_mut().apply_picker_change(c);
            }),
            Rc::new(move |c| {
                session_commit.borrow_mut().commit_picker_color(c);
            }),
            Rc::new(move |c| {
                session_add.borrow_mut().add_swatch(c);
            }),
            Rc::new(move || {
                session_done.borrow_mut().finish_color_picker();
            }),
        )),
    ))
}

fn discard_dialog(session: SessionRef, overlay: OverlayHandle) -> View {
    let state = session.borrow().confirm_dialog.clone();
    let label = theme().typography.label_large;

    let content = Column(Modifier::new().padding_values(PaddingValues {
        left: 24.0,
        right: 24.0,
        top: 24.0,
        bottom: 24.0,
    }))
    .child((
        Text("Unsaved changes").size(theme().typography.title_large),
        Box(Modifier::new().height(12.0)),
        Text("Save changes to your project before continuing?")
            .size(theme().typography.body_medium)
            .color(theme().on_surface_variant),
        Spacer(),
        Row(Modifier::new()
            .gap(8.0)
            .justify_content(JustifyContent::END))
        .child((
            TextButton(
                Modifier::new(),
                {
                    let session = session.clone();
                    move || crate::file::discard_cancel(&session)
                },
                ButtonConfig::default(),
                || Text("Cancel").size(label),
            ),
            TextButton(
                Modifier::new(),
                {
                    let session = session.clone();
                    move || crate::file::discard_discard(&session)
                },
                ButtonConfig::default(),
                || Text("Discard").size(label),
            ),
            Button(
                Modifier::new(),
                {
                    let session = session.clone();
                    move || crate::file::discard_save(&session)
                },
                ButtonConfig::default(),
                || Text("Save").size(label),
            ),
        )),
    ));

    Dialog(
        state,
        overlay,
        Modifier::new(),
        DialogProperties::default(),
        content,
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
