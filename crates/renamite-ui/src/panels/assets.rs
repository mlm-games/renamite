use renamite_behavior_common::assets::{
    cmd_place_image, font_rows, image_rows, selected_text_node,
};
use renamite_history::{EditorCommand, ToolOutput};
use repose_core::{AlignItems, Modifier, View, theme};
use repose_ui::scroll::{ScrollArea, remember_scroll_state};
use repose_ui::{Box, Column, ImageExt, Row, Text, TextStyle, ViewExt};
use smallvec::smallvec;

use crate::components::{CompactIconAction, PanelHeader};
use crate::session::SessionRef;
use crate::symbols::{AppIcon, Symbols};

pub fn AssetsPanel(session: SessionRef) -> View {
    let (rows, image_rows, selected_text) = {
        let session = session.borrow();

        (
            font_rows(&session.file.document),
            image_rows(&session.file.document),
            selected_text_node(&session.file.document, &session.selection.nodes),
        )
    };

    let mut children = vec![PanelHeader(
        Symbols::folder_open,
        "Assets",
        vec![
            CompactIconAction(Symbols::font_download, "Import Font", {
                let session = session.clone();
                move || crate::file::import_font(&session)
            }),
            CompactIconAction(Symbols::image, "Import Image", {
                let session = session.clone();
                move || crate::file::import_image(&session)
            }),
        ],
    )];

    if rows.is_empty() && image_rows.is_empty() {
        children.push(
            Column(Modifier::new().fill_max_width().gap(8.0).padding(16.0)).child((
                Text("No assets yet")
                    .size(theme().typography.body_medium)
                    .color(theme().on_surface),
                Text("Import an image or font to use it in this project.")
                    .size(theme().typography.body_small)
                    .color(theme().on_surface_variant),
            )),
        );
    } else {
        children.push(
            Text("FONTS")
                .size(theme().typography.label_small)
                .color(theme().on_surface_variant)
                .modifier(Modifier::new().padding(12.0)),
        );

        for row in rows {
            children.push(FontRow(session.clone(), row, selected_text));
        }

        children.push(
            Text("IMAGES")
                .size(theme().typography.label_small)
                .color(theme().on_surface_variant)
                .modifier(Modifier::new().padding(12.0)),
        );

        for row in image_rows {
            children.push(ImageRow(session.clone(), row));
        }
    }

    let header = children.remove(0);
    Column(Modifier::new().fill_max_size()).child((
        header,
        ScrollArea(
            Modifier::new().fill_max_size(),
            remember_scroll_state("assets_scroll"),
            Column(Modifier::new().fill_max_width()).child(children),
        ),
    ))
}

fn ImageRow(session: SessionRef, row: renamite_behavior_common::assets::ImageAssetRow) -> View {
    let th = theme();
    let handle = renamite_render_bridge::image_handle(row.id);

    Row(Modifier::new()
        .height(64.0)
        .fill_max_width()
        .padding_values(repose_core::PaddingValues {
            left: 8.0,
            right: 8.0,
            top: 4.0,
            bottom: 4.0,
        })
        .gap(8.0)
        .align_items(AlignItems::CENTER))
    .child((
        repose_ui::Image(
            Modifier::new()
                .width(52.0)
                .height(52.0)
                .background(th.surface_container_high)
                .clip_rounded(6.0),
            handle,
        )
        .image_fit(repose_core::ImageFit::Contain),
        Column(Modifier::new().flex_grow(1.0)).child((
            Text(row.name)
                .size(th.typography.body_medium)
                .color(th.on_surface),
            Text(format!(
                "{}×{} · {} use{}",
                row.width,
                row.height,
                row.usage_count,
                if row.usage_count == 1 { "" } else { "s" },
            ))
            .size(th.typography.label_small)
            .color(th.on_surface_variant),
        )),
        CompactIconAction(Symbols::add, "Place image", {
            let session = session.clone();

            move || {
                let mut session = session.borrow_mut();

                let comp = session.file.document.main;

                let composition = &session.file.document.compositions[comp];

                let position = glam::DVec2::new(
                    composition.size.0 as f64 * 0.5,
                    composition.size.1 as f64 * 0.5,
                );

                let Some(command) = cmd_place_image(
                    &session.file.document,
                    row.id,
                    renamite_model::Parent::Comp(comp),
                    0,
                    position,
                ) else {
                    return;
                };

                session.apply_outputs(smallvec![
                    ToolOutput::BeginTransaction("Place image".into()),
                    ToolOutput::Commands(smallvec![command]),
                    ToolOutput::CommitTransaction,
                ]);
            }
        }),
        if row.usage_count == 0 {
            CompactIconAction(Symbols::delete, "Remove image", {
                let session = session.clone();

                move || {
                    session.borrow_mut().apply_outputs(smallvec![
                        ToolOutput::BeginTransaction("Remove image".into()),
                        ToolOutput::Commands(smallvec![EditorCommand::DetachAsset { id: row.id }]),
                        ToolOutput::CommitTransaction,
                    ]);
                }
            })
        } else {
            Box(Modifier::new().width(40.0))
        },
    ))
}

fn FontRow(
    session: SessionRef,
    row: renamite_behavior_common::assets::FontAssetRow,
    selected_text: Option<renamite_model::NodeId>,
) -> View {
    let th = theme();
    let active = selected_text.is_some_and(|id| {
        let session = session.borrow();

        matches!(
            &session.file.document.nodes[id].kind,
            renamite_model::NodeKind::Text(text)
                if text.font.as_deref().unwrap_or("default") == row.family
        )
    });

    Row(Modifier::new()
        .height(44.0)
        .fill_max_width()
        .padding_values(repose_core::PaddingValues {
            left: 12.0,
            right: 8.0,
            top: 0.0,
            bottom: 0.0,
        })
        .align_items(AlignItems::CENTER)
        .gap(8.0)
        .background(if active {
            th.secondary_container
        } else {
            th.surface_container
        }))
    .child((
        AppIcon(Symbols::text_fields, 20.0),
        Column(Modifier::new().flex_grow(1.0)).child((
            Text(row.family.clone())
                .size(th.typography.body_medium)
                .color(th.on_surface),
            Text(format!(
                "{} · {} use{}",
                row.name,
                row.usage_count,
                if row.usage_count == 1 { "" } else { "s" },
            ))
            .size(th.typography.label_small)
            .color(th.on_surface_variant),
        )),
        if let Some(text_id) = selected_text {
            CompactIconAction(Symbols::check, "Apply to selected text", {
                let session = session.clone();
                let family = row.family.clone();

                move || {
                    session.borrow_mut().apply_outputs(smallvec![
                        ToolOutput::BeginTransaction("Change font".into()),
                        ToolOutput::Commands(smallvec![EditorCommand::SetTextFont {
                            id: text_id,
                            font: if family == "default" {
                                None
                            } else {
                                Some(family.clone())
                            },
                        }]),
                        ToolOutput::CommitTransaction,
                    ]);
                }
            })
        } else {
            Box(Modifier::new().width(40.0))
        },
        if !row.bundled && row.usage_count == 0 {
            CompactIconAction(Symbols::delete, "Remove font asset", {
                let session = session.clone();
                let asset_id = row.id.unwrap();

                move || {
                    session.borrow_mut().apply_outputs(smallvec![
                        ToolOutput::BeginTransaction("Remove font".into()),
                        ToolOutput::Commands(smallvec![EditorCommand::DetachAsset {
                            id: asset_id,
                        }]),
                        ToolOutput::CommitTransaction,
                    ]);
                }
            })
        } else {
            Box(Modifier::new().width(40.0))
        },
    ))
}
