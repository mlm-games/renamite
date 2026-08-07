use renamite_behavior_common::assets::{
    font_rows, selected_text_node,
};
use renamite_history::{EditorCommand, ToolOutput};
use repose_core::{AlignItems, Modifier, View, theme};
use repose_ui::{Box, Column, Row, Text, TextStyle, ViewExt};
use smallvec::smallvec;

use crate::components::{CompactIconAction, PanelHeader};
use crate::session::SessionRef;
use crate::symbols::{AppIcon, Symbols};

pub fn AssetsPanel(session: SessionRef) -> View {
    let (rows, selected_text) = {
        let session = session.borrow();

        (
            font_rows(&session.file.document),
            selected_text_node(
                &session.file.document,
                &session.selection.nodes,
            ),
        )
    };

    let mut children = vec![PanelHeader(
        Symbols::folder_open,
        "Assets",
        vec![CompactIconAction(
            Symbols::font_download,
            "Import Font",
            {
                let session = session.clone();
                move || crate::file::import_font(&session)
            },
        )],
    )];

    children.push(
        Text("FONTS")
            .size(theme().typography.label_small)
            .color(theme().on_surface_variant)
            .modifier(Modifier::new().padding(12.0)),
    );

    for row in rows {
        children.push(FontRow(
            session.clone(),
            row,
            selected_text,
        ));
    }

    Column(Modifier::new().fill_max_size()).child(children)
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

    Row(
        Modifier::new()
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
            }),
    )
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
            CompactIconAction(
                Symbols::check,
                "Apply to selected text",
                {
                    let session = session.clone();
                    let family = row.family.clone();

                    move || {
                        session.borrow_mut().apply_outputs(smallvec![
                            ToolOutput::BeginTransaction(
                                "Change font".into()
                            ),
                            ToolOutput::Commands(smallvec![
                                EditorCommand::SetTextFont {
                                    id: text_id,
                                    font: if family == "default" {
                                        None
                                    } else {
                                        Some(family.clone())
                                    },
                                }
                            ]),
                            ToolOutput::CommitTransaction,
                        ]);
                    }
                },
            )
        } else {
            Box(Modifier::new().width(40.0))
        },

        if !row.bundled && row.usage_count == 0 {
            CompactIconAction(
                Symbols::delete,
                "Remove font asset",
                {
                    let session = session.clone();
                    let asset_id = row.id.unwrap();

                    move || {
                        session.borrow_mut().apply_outputs(smallvec![
                            ToolOutput::BeginTransaction(
                                "Remove font".into()
                            ),
                            ToolOutput::Commands(smallvec![
                                EditorCommand::RemoveAsset {
                                    id: asset_id,
                                }
                            ]),
                            ToolOutput::CommitTransaction,
                        ]);
                    }
                },
            )
        } else {
            Box(Modifier::new().width(40.0))
        },
    ))
}
