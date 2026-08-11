use repose_core::{JustifyContent, Modifier, PaddingValues, View, remember_with_key, theme};
use repose_material::material3::{
    Button, ButtonConfig, Dialog, DialogProperties, NavItem, NavigationBar, NavigationBarConfig,
    Scaffold, ScaffoldConfig, Snackbar, SnackbarConfig, Surface, SurfaceConfig, TextButton,
};
use repose_ui::overlay::{OverlayHandle, SnackbarController, SnackbarRequest};
use repose_ui::{Box, Column, Row, Spacer, Text, TextStyle, ViewExt, ZStack};
use std::cell::RefCell;
use std::rc::Rc;

use repose_docking::{
    DockArea, DockCallbacks, DockKind, DockNode, DockPanel, DockState, PanelId, SplitDir,
};

use crate::components::{PanelSurface, PillButton, ToolAction};
use crate::panels::{
    AssetsPanel, InteractivityPanel, LayersPanel, PropertiesPanel, TimelinePanel, ViewportPanel,
};
use crate::session::{EditorMode, PanelPage, SessionRef};
use crate::symbols::{AppIcon, Symbols};
use renamite_behavior_common::context_menu::MenuEntry;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellClass {
    Expanded,
    Medium,
    Compact,
}

/// Choose the shell layout from the current window size class (Material 3
/// adaptive thresholds: <600 dp compact, <840 dp medium, else expanded).
pub fn platform_shell_class() -> ShellClass {
    match repose_core::window_size_class().width {
        repose_core::WidthClass::Compact => ShellClass::Compact,
        repose_core::WidthClass::Medium => ShellClass::Medium,
        repose_core::WidthClass::Expanded => ShellClass::Expanded,
    }
}

pub fn EditorShell(session: SessionRef) -> View {
    let overlay = remember_with_key("shell_overlay", OverlayHandle::new);
    let class = platform_shell_class();
    let session_body = session.clone();

    let snackbar = remember_with_key("shell_snackbar", {
        let overlay = (*overlay).clone();
        move || SnackbarController::new(overlay)
    });
    {
        let mut s = session.borrow_mut();
        if s.status.is_some() {
            let message = s.status.take().unwrap_or_default();
            (*snackbar).show(SnackbarRequest {
                message: message.clone(),
                action: None,
                duration_ms: 3500,
                builder: Rc::new(move || {
                    Snackbar(
                        message.clone(),
                        None,
                        Modifier::new(),
                        SnackbarConfig::default(),
                    )
                }),
            });
        }
    }

    let scaffold = Scaffold(
        move |_content_padding| match class {
            ShellClass::Expanded => ExpandedWorkspace(session_body.clone()),
            ShellClass::Medium => MediumWorkspace(session_body.clone()),
            ShellClass::Compact => CompactWorkspace(session_body.clone()),
        },
        ScaffoldConfig {
            top_bar: Some(crate::AppTopBar(session.clone(), (*overlay).clone())),
            bottom_bar: match class {
                ShellClass::Compact => Some(BottomNavigation(session.clone())),
                // status (file actions, errors) is surfaced as a snackbar.
                _ => None,
            },
            container_color: theme().surface_container_lowest,
            ..Default::default()
        },
    );

    let confirm = discard_dialog(session.clone(), (*overlay).clone());
    let picker = color_picker_overlay(session.clone());
    let menu = context_menu_overlay(session.clone());

    overlay.host(
        Modifier::new().fill_max_size(),
        ZStack(Modifier::new().fill_max_size()).child((scaffold, picker, menu, confirm)),
    )
}

fn context_menu_overlay(session: SessionRef) -> View {
    let Some(menu) = session.borrow().context_menu.clone() else {
        return ZStack(Modifier::new());
    };
    let th = theme();
    let entries = menu.entries.clone();
    let session_close = session.clone();
    let session_entries = session.clone();

    let (x, y) = menu_placement(menu.screen_pos, &entries);

    ZStack(Modifier::new().fill_max_size()).child((
        // Transparent scrim: any click outside closes the menu.
        Box(Modifier::new().fill_max_size().on_pointer_down(move |_| {
            session_close.borrow_mut().close_context_menu();
        })),
        Box(Modifier::new()
            .absolute()
            .offset(Some(x), Some(y), None, None))
        .child(Surface(
            SurfaceConfig {
                modifier: Modifier::new().width(220.0).padding(4.0),
                color: th.surface_container_high,
                content_color: th.on_surface,
                shape_radius: 8.0,
                border: Some((1.0, th.outline_variant)),
                ..Default::default()
            },
            move || {
                Column(Modifier::new().fill_max_width().gap(2.0))
                    .child(render_menu_entries(session_entries.clone(), &entries))
            },
        )),
    ))
}

/// Clamp a context menu near its anchor, then inside the window edges (mirrors
/// the color picker placement so right/bottom-edge opens don't run off-screen).
fn menu_placement(anchor: glam::DVec2, entries: &[MenuEntry]) -> (f32, f32) {
    const W: f32 = 220.0;
    const M: f32 = 8.0;
    let height = menu_entries_height(entries)
        .min(repose_core::get_window_container_height() - M * 2.0)
        .max(8.0);
    let vw = repose_core::get_window_container_width().max(1.0);
    let vh = repose_core::get_window_container_height().max(1.0);
    let mut x = anchor.x as f32;
    let mut y = anchor.y as f32;
    // Prefer opening below the cursor; flip above when there's no room.
    if y + height > vh - M {
        y = (anchor.y as f32 - height).max(M);
    }
    x = x.clamp(M, (vw - W - M).max(M));
    y = y.clamp(M, (vh - height - M).max(M));
    (x, y)
}

fn menu_entries_height(entries: &[MenuEntry]) -> f32 {
    let mut h = 0.0;
    for e in entries {
        match e {
            MenuEntry::Separator => h += 5.0,
            MenuEntry::Action { .. } => h += 36.0 + 2.0,
            MenuEntry::Submenu { children, .. } => {
                h += 24.0 + menu_entries_height(children);
            }
        }
    }
    h + 8.0 // Surface padding (4 + 4)
}

fn render_menu_entries(session: SessionRef, entries: &[MenuEntry]) -> Vec<View> {
    let th = theme();
    entries
        .iter()
        .flat_map(|e| match e {
            MenuEntry::Separator => {
                vec![Box(Modifier::new()
                    .height(1.0)
                    .fill_max_width()
                    .background(th.outline_variant))]
            }
            MenuEntry::Action {
                id, label, enabled, ..
            } => {
                let action = id.clone();
                let en = *enabled;
                vec![
                    Box(Modifier::new()
                        .height(36.0)
                        .fill_max_width()
                        .padding_values(PaddingValues {
                            left: 12.0,
                            right: 12.0,
                            top: 0.0,
                            bottom: 0.0,
                        })
                        .align_items(repose_core::AlignItems::CENTER)
                        .on_pointer_down({
                            let session = session.clone();
                            move |_| {
                                if en {
                                    session.borrow_mut().run_menu_action(action.clone());
                                }
                            }
                        }))
                    .child(
                        Text(*label).size(th.typography.body_medium).color(if en {
                            th.on_surface
                        } else {
                            th.on_surface_variant
                        }),
                    ),
                ]
            }
            MenuEntry::Submenu { label, children } => {
                let mut views = vec![
                    Text(*label)
                        .size(th.typography.label_small)
                        .color(th.on_surface_variant)
                        .modifier(Modifier::new().padding_values(PaddingValues {
                            left: 12.0,
                            right: 8.0,
                            top: 8.0,
                            bottom: 2.0,
                        })),
                ];
                views.extend(render_menu_entries(session.clone(), children));
                views
            }
        })
        .collect()
}

/// Transparent-to-closem modal layer containing the color picker popover,
/// anchored to the swatch that opened it (clamped to the window edges).
fn color_picker_overlay(session: SessionRef) -> View {
    let Some(picker) = session.borrow().open_picker.clone() else {
        return ZStack(Modifier::new());
    };
    let swatches = session.borrow().swatches.colors.clone();
    let (x, y) = picker_placement(picker.anchor);

    let session_close = session.clone();
    let session_change = session.clone();
    let session_commit = session.clone();
    let session_add = session.clone();
    let session_done = session.clone();

    ZStack(Modifier::new().fill_max_size()).child((
        // Transparent close layer: click anywhere outside the picker to
        // dismiss (cancels). Not a modal scrim - the editor stays visible.
        Box(Modifier::new().fill_max_size().on_pointer_down(move |_| {
            session_close.borrow_mut().close_color_picker();
        })),
        Box(Modifier::new()
            .absolute()
            .offset(Some(x), Some(y), None, None))
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

/// Clamp a picker popover near its anchor: prefer below the swatch, flip above
/// when there is no room, then clamp inside the window with a small margin.
fn picker_placement(anchor: glam::DVec2) -> (f32, f32) {
    const W: f32 = 224.0;
    const H: f32 = 416.0;
    const M: f32 = 8.0;
    let vw = repose_core::get_window_container_width().max(1.0);
    let vh = repose_core::get_window_container_height().max(1.0);
    let mut x = anchor.x as f32 + 8.0;
    let mut y = anchor.y as f32 + 8.0;
    if y + H > vh - M {
        y = anchor.y as f32 - H - 8.0;
    }
    x = x.clamp(M, (vw - W - M).max(M));
    y = y.clamp(M, (vh - H - M).max(M));
    (x, y)
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
        Box(Modifier::new().weight(1.0).fill_max_height())
            .child(DockWorkspace(session.clone(), session.borrow().mode)),
    ))
}

/// Panel ids for the dockable workspace; stable so per-mode default layouts
/// reference the same panels across sessions.
const PANEL_VIEWPORT: PanelId = 1;
const PANEL_LAYERS: PanelId = 2;
const PANEL_PROPERTIES: PanelId = 3;
const PANEL_ASSETS: PanelId = 4;
const PANEL_TIMELINE: PanelId = 5;
const PANEL_INTERACT: PanelId = 6;

/// Default dock tree per editor mode: a left rail grouping Layers + Assets for
/// design (Timeline for animate), a big center canvas, and Properties on the
/// right — matching the prior fixed Expanded layout, but now resizable.
fn default_dock_state(mode: EditorMode) -> DockState {
    let root = match mode {
        EditorMode::Design => DockNode {
            id: 1,
            kind: DockKind::Split {
                dir: SplitDir::Horizontal,
                ratio: 0.22,
                a: Box::new(DockNode {
                    id: 11,
                    kind: DockKind::Split {
                        dir: SplitDir::Vertical,
                        ratio: 0.72,
                        a: Box::new(tabs_node(101, PANEL_LAYERS)),
                        b: Box::new(tabs_node(102, PANEL_ASSETS)),
                    },
                }),
                b: Box::new(DockNode {
                    id: 12,
                    kind: DockKind::Split {
                        dir: SplitDir::Horizontal,
                        ratio: 0.78,
                        a: Box::new(tabs_node(121, PANEL_VIEWPORT)),
                        b: Box::new(tabs_node(122, PANEL_PROPERTIES)),
                    },
                }),
            },
        },
        EditorMode::Animate => DockNode {
            id: 2,
            kind: DockKind::Split {
                dir: SplitDir::Horizontal,
                ratio: 0.2,
                a: Box::new(tabs_node(21, PANEL_LAYERS)),
                b: Box::new(DockNode {
                    id: 22,
                    kind: DockKind::Split {
                        dir: SplitDir::Horizontal,
                        ratio: 0.8,
                        a: Box::new(DockNode {
                            id: 23,
                            kind: DockKind::Split {
                                dir: SplitDir::Vertical,
                                ratio: 0.72,
                                a: Box::new(tabs_node(231, PANEL_VIEWPORT)),
                                b: Box::new(tabs_node(232, PANEL_TIMELINE)),
                            },
                        }),
                        b: Box::new(tabs_node(24, PANEL_PROPERTIES)),
                    },
                }),
            },
        },
        EditorMode::Interact => DockNode {
            id: 3,
            kind: DockKind::Split {
                dir: SplitDir::Horizontal,
                ratio: 0.22,
                a: Box::new(tabs_node(31, PANEL_INTERACT)),
                b: Box::new(DockNode {
                    id: 32,
                    kind: DockKind::Split {
                        dir: SplitDir::Horizontal,
                        ratio: 0.78,
                        a: Box::new(tabs_node(321, PANEL_VIEWPORT)),
                        b: Box::new(tabs_node(322, PANEL_PROPERTIES)),
                    },
                }),
            },
        },
    };
    DockState::from_root(root, 999)
}

fn tabs_node(id: u64, panel: PanelId) -> DockNode {
    DockNode {
        id,
        kind: DockKind::Tabs {
            tabs: vec![panel],
            active: Some(panel),
        },
    }
}

/// Build the dock panels available for a mode. Panels carry their own header
/// chrome (titles/toolbars), so the dock registry only tags them by id/title.
fn dock_panels(mode: EditorMode, session: SessionRef) -> Vec<DockPanel> {
    let mut panels = vec![
        DockPanel {
            id: PANEL_VIEWPORT,
            title: "Canvas".into(),
            content: Rc::new({
                let session = session.clone();
                move || ViewportPanel(session.clone())
            }),
        },
        DockPanel {
            id: PANEL_LAYERS,
            title: "Layers".into(),
            content: Rc::new({
                let session = session.clone();
                move || PanelSurface(LayersPanel(session.clone()))
            }),
        },
        DockPanel {
            id: PANEL_PROPERTIES,
            title: "Properties".into(),
            content: Rc::new({
                let session = session.clone();
                move || PanelSurface(PropertiesPanel(session.clone()))
            }),
        },
    ];
    match mode {
        EditorMode::Design => panels.push(DockPanel {
            id: PANEL_ASSETS,
            title: "Assets".into(),
            content: Rc::new({
                let session = session.clone();
                move || PanelSurface(AssetsPanel(session.clone()))
            }),
        }),
        EditorMode::Animate => panels.push(DockPanel {
            id: PANEL_TIMELINE,
            title: "Timeline".into(),
            content: Rc::new({
                let session = session.clone();
                move || PanelSurface(TimelinePanel(session.clone()))
            }),
        }),
        EditorMode::Interact => panels.push(DockPanel {
            id: PANEL_INTERACT,
            title: "Interactivity".into(),
            content: Rc::new({
                let session = session.clone();
                move || PanelSurface(InteractivityPanel(session.clone()))
            }),
        }),
    }
    panels
}

fn dock_key(mode: EditorMode) -> &'static str {
    match mode {
        EditorMode::Design => "dock_design",
        EditorMode::Animate => "dock_animate",
        EditorMode::Interact => "dock_interact",
    }
}

fn DockWorkspace(session: SessionRef, mode: EditorMode) -> View {
    let key = dock_key(mode);
    let state = remember_with_key(key, || Rc::new(RefCell::new(default_dock_state(mode))));
    let panels = dock_panels(mode, session.clone());

    DockArea(
        key,
        Modifier::new().fill_max_size(),
        (*state).clone(),
        panels,
        DockCallbacks {
            on_popout: None,
            on_close: None,
        },
    )
}

/// The side panel shown while on the Canvas page (Layers, so a single-panel
/// medium layout still has an obvious left nav surface).
fn effective_side_page(page: PanelPage) -> PanelPage {
    match page {
        PanelPage::Canvas => PanelPage::Layers,
        other => other,
    }
}

fn MediumSideTabs(session: SessionRef) -> View {
    let (mode, current) = {
        let s = session.borrow();
        (s.mode, effective_side_page(s.active_page))
    };
    let tabs = medium_nav_pages(mode);
    let current_id = tabs
        .iter()
        .position(|&p| p == current)
        .map(|i| tabs[i])
        .unwrap_or(tabs[0]);

    Row(Modifier::new().fill_max_width().padding(8.0).gap(6.0)).child(
        tabs.iter()
            .enumerate()
            .map(|(i, &page)| {
                let label = page_label(page);
                PillButton(label, tabs[i] == current_id, {
                    let session = session.clone();
                    move || session.borrow_mut().set_active_page(page)
                })
            })
            .collect::<Vec<_>>(),
    )
}

fn MediumWorkspace(session: SessionRef) -> View {
    Row(Modifier::new().fill_max_size().padding(8.0).gap(8.0)).child((
        crate::ToolRail(session.clone()),
        Box(Modifier::new().weight(1.0).fill_max_height())
            .child(PanelSurface(ViewportPanel(session.clone()))),
        Box(Modifier::new().width(320.0).fill_max_height()).child(PanelSurface(
            Column(Modifier::new().fill_max_size()).child((
                MediumSideTabs(session.clone()),
                Box(Modifier::new().weight(1.0).fill_max_width()).child(active_side_panel(session)),
            )),
        )),
    ))
}

fn CompactWorkspace(session: SessionRef) -> View {
    let page = session.borrow().active_page;
    match page {
        PanelPage::Canvas => CompactCanvas(session),
        PanelPage::Layers => LayersPanel(session),
        PanelPage::Timeline => TimelinePanel(session),
        PanelPage::Inspect => PropertiesPanel(session),
        PanelPage::Assets => AssetsPanel(session),
        PanelPage::Interact => InteractivityPanel(session),
    }
}

fn CompactCanvas(session: SessionRef) -> View {
    ZStack(Modifier::new().fill_max_size())
        .child((ViewportPanel(session.clone()), CompactToolPalette(session)))
}

fn compact_tool(
    session: SessionRef,
    id: renamite_history::ToolId,
    icon: repose_material::Symbol,
    label: &'static str,
) -> View {
    let selected = session.borrow().active_tool == id;
    ToolAction(icon, label, selected, move || {
        let mut s = session.borrow_mut();
        s.active_tool = id;
        s.repaint();
    })
}

/// Floating tool palette for the compact canvas (the tool rail is dropped on
/// phones, so the tools move on top of the stage instead). Wider than a rail
/// on desktop but still compact: one button per tool, same set everywhere.
fn CompactToolPalette(session: SessionRef) -> View {
    Box(Modifier::new()
        .absolute()
        .offset(Some(12.0), None, None, Some(12.0)))
    .child(
        Box(Modifier::new()
            .padding(6.0)
            .background(theme().surface_container_high)
            .clip_rounded(12.0)
            .border(1.0, theme().outline_variant, 12.0))
        .child(Column(Modifier::new().gap(4.0)).child(vec![
            compact_tool(
                session.clone(),
                renamite_history::ToolId::Select,
                Symbols::arrow_selector_tool,
                "Select",
            ),
            compact_tool(
                session.clone(),
                renamite_history::ToolId::Transform,
                Symbols::transform,
                "Transform / pivot",
            ),
            compact_tool(
                session.clone(),
                renamite_history::ToolId::Pen,
                Symbols::draw,
                "Pen",
            ),
            compact_tool(
                session.clone(),
                renamite_history::ToolId::PathEdit,
                Symbols::edit,
                "Edit path",
            ),
            compact_tool(
                session.clone(),
                renamite_history::ToolId::Rect,
                Symbols::rectangle,
                "Rectangle",
            ),
            compact_tool(
                session.clone(),
                renamite_history::ToolId::Ellipse,
                Symbols::circle,
                "Ellipse",
            ),
            compact_tool(
                session.clone(),
                renamite_history::ToolId::Star,
                Symbols::star,
                "Star",
            ),
            compact_tool(
                session.clone(),
                renamite_history::ToolId::Text,
                Symbols::text_fields,
                "Text",
            ),
            compact_tool(
                session.clone(),
                renamite_history::ToolId::Gradient,
                Symbols::gradient,
                "Gradient",
            ),
            compact_tool(
                session,
                renamite_history::ToolId::Fill,
                Symbols::format_color_fill,
                "Fill",
            ),
        ])),
    )
}

fn active_side_panel(session: SessionRef) -> View {
    let page = session.borrow().active_page;
    match page {
        PanelPage::Layers => LayersPanel(session),
        PanelPage::Timeline => TimelinePanel(session),
        PanelPage::Inspect => PropertiesPanel(session),
        PanelPage::Canvas => LayersPanel(session),
        PanelPage::Assets => AssetsPanel(session),
        PanelPage::Interact => InteractivityPanel(session),
    }
}

fn BottomNavigation(session: SessionRef) -> View {
    let (mode, active_page) = {
        let s = session.borrow();
        (s.mode, s.active_page)
    };
    let pages = compact_nav_pages(mode);
    // Selection is position-based, not `page as usize`: `Interact` no longer
    // overruns the nav item list, and the nav changes meaningfully per mode.
    let selected = pages
        .iter()
        .position(|&p| p == active_page)
        .unwrap_or(0)
        .min(pages.len().saturating_sub(1));

    NavigationBar(
        selected,
        pages
            .iter()
            .map(|&page| nav_item(session.clone(), page, page_symbol(page), page_label(page)))
            .collect(),
        NavigationBarConfig::default(),
    )
}

/// Compact bottom-nav page order per mode, so editing, animating, and logic
/// work each surface only the destinations that matter for that mode.
fn compact_nav_pages(mode: EditorMode) -> Vec<PanelPage> {
    match mode {
        EditorMode::Design => vec![
            PanelPage::Canvas,
            PanelPage::Layers,
            PanelPage::Inspect,
            PanelPage::Assets,
        ],
        EditorMode::Animate => vec![
            PanelPage::Canvas,
            PanelPage::Timeline,
            PanelPage::Inspect,
            PanelPage::Layers,
        ],
        EditorMode::Interact => vec![PanelPage::Canvas, PanelPage::Interact, PanelPage::Inspect],
    }
}

/// The tabbed side rail (medium) also list only the pages that matter per mode.
fn medium_nav_pages(mode: EditorMode) -> Vec<PanelPage> {
    match mode {
        EditorMode::Design => vec![PanelPage::Layers, PanelPage::Inspect, PanelPage::Assets],
        EditorMode::Animate => vec![PanelPage::Timeline, PanelPage::Layers, PanelPage::Inspect],
        EditorMode::Interact => vec![PanelPage::Interact, PanelPage::Inspect],
    }
}

fn page_label(page: PanelPage) -> &'static str {
    match page {
        PanelPage::Canvas => "Canvas",
        PanelPage::Layers => "Layers",
        PanelPage::Timeline => "Timeline",
        PanelPage::Inspect => "Inspect",
        PanelPage::Assets => "Assets",
        PanelPage::Interact => "Logic",
    }
}

fn page_symbol(page: PanelPage) -> repose_material::Symbol {
    match page {
        PanelPage::Canvas => Symbols::edit,
        PanelPage::Layers => Symbols::layers,
        PanelPage::Timeline => Symbols::play_arrow,
        PanelPage::Inspect => Symbols::settings,
        PanelPage::Assets => Symbols::folder_open,
        PanelPage::Interact => Symbols::account_tree,
    }
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
            session.borrow_mut().set_active_page(page);
        }),
        enabled: true,
        interaction_source: None,
    }
}
