use repose_core::{Modifier, View, theme};
use repose_ui::{Column, Text, TextStyle, ViewExt};

use crate::components::PanelHeader;
use crate::session::SessionRef;
use crate::symbols::Symbols;

pub fn PropertiesPanel(session: SessionRef) -> View {
    let s = session.borrow();
    let label = if s.selection.nodes.is_empty() {
        "No selection".to_string()
    } else {
        format!("{} selected", s.selection.nodes.len())
    };

    let header = PanelHeader(Symbols::settings, "Inspect", vec![]);
    let body = Text(label)
        .size(theme().typography.body_medium)
        .color(theme().on_surface_variant)
        .modifier(Modifier::new().padding(12.0));

    Column(Modifier::new().fill_max_size()).child((header, body))
}