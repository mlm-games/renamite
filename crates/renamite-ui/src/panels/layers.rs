use repose_core::{Modifier, View, theme};
use repose_ui::{Column, Row, Text, TextStyle, ViewExt};

use crate::components::PanelHeader;
use crate::session::SessionRef;
use crate::symbols::Symbols;

pub fn LayersPanel(session: SessionRef) -> View {
    let s = session.borrow();
    let comp = &s.file.document.compositions[s.file.document.main];

    let mut children = Vec::new();
    for &id in &comp.children {
        if let Some(n) = s.file.document.nodes.get(id) {
            children.push(layer_row(n.name.clone()));
        }
    }
    if children.is_empty() {
        children.push(
            Text("No layers")
                .size(theme().typography.label_medium)
                .color(theme().on_surface_variant)
                .modifier(Modifier::new().padding(12.0)),
        );
    }
    drop(s);

    let header = PanelHeader(Symbols::layers, "Layers", vec![]);
    let list = Column(Modifier::new().fill_max_size().padding(4.0).gap(2.0)).child(children);

    Column(Modifier::new().fill_max_size()).child((header, list))
}

fn layer_row(name: String) -> View {
    Row(
        Modifier::new()
            .fill_max_width()
            .height(32.0)
            .padding_values(repose_core::PaddingValues {
                left: 12.0,
                right: 12.0,
                top: 0.0,
                bottom: 0.0,
            }),
    )
    .child(
        Text(name.clone())
            .size(theme().typography.body_medium)
            .color(theme().on_surface),
    )
}