use renamite_model::{
    Asset, AssetId, Document, NodeId, NodeKind,
};

#[derive(Clone, Debug)]
pub struct FontAssetRow {
    pub id: Option<AssetId>,
    pub family: String,
    pub name: String,
    pub usage_count: usize,
    pub bundled: bool,
}

pub fn font_rows(doc: &Document) -> Vec<FontAssetRow> {
    let mut rows = vec![FontAssetRow {
        id: None,
        family: "default".into(),
        name: "Bundled Default".into(),
        usage_count: font_usage_count(doc, "default"),
        bundled: true,
    }];

    rows.extend(doc.assets.iter().filter_map(|(id, asset)| {
        let Asset::Font(font) = asset else {
            return None;
        };

        Some(FontAssetRow {
            id: Some(id),
            family: font.family.clone(),
            name: font.name.clone(),
            usage_count: font_usage_count(doc, &font.family),
            bundled: false,
        })
    }));

    rows.sort_by(|a, b| {
        b.bundled
            .cmp(&a.bundled)
            .then_with(|| a.family.cmp(&b.family))
    });

    rows
}

pub fn font_usage_count(doc: &Document, family: &str) -> usize {
    doc.nodes
        .values()
        .filter(|node| {
            matches!(
                &node.kind,
                NodeKind::Text(text)
                    if text.font.as_deref().unwrap_or("default") == family
            )
        })
        .count()
}

/// Resolve either a selected Text node or a group containing one Text child.
pub fn selected_text_node(
    doc: &Document,
    selection: &[NodeId],
) -> Option<NodeId> {
    let [selected] = selection else {
        return None;
    };

    let node = doc.nodes.get(*selected)?;

    if matches!(node.kind, NodeKind::Text(_)) {
        return Some(*selected);
    }

    let mut matches = node.children.iter().copied().filter(|id| {
        matches!(
            doc.nodes.get(*id).map(|node| &node.kind),
            Some(NodeKind::Text(_))
        )
    });

    let result = matches.next()?;
    matches.next().is_none().then_some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use renamite_animation::Animated;
    use renamite_model::{Node, Parent, TextAlign, TextNode};

    fn text_node(font: Option<String>) -> Node {
        Node::new(
            "t",
            NodeKind::Text(TextNode {
                text: String::new(),
                size: Animated::new(48.0),
                align: TextAlign::Left,
                font,
            }),
        )
    }

    #[test]
    fn font_rows_include_bundled_default_first() {
        let doc = Document::empty();
        let rows = font_rows(&doc);

        assert!(rows[0].bundled);
        assert_eq!(rows[0].family, "default");
    }

    #[test]
    fn font_usage_counts_nested_text() {
        let mut doc = Document::empty();
        let group = doc.create_node(Node::new("g", NodeKind::Group));
        let text = doc.create_node(text_node(Some("Inter".into())));

        doc.attach(text, Parent::Node(group), 0).unwrap();
        doc.attach(group, Parent::Comp(doc.main), 0).unwrap();

        assert_eq!(font_usage_count(&doc, "Inter"), 1);
    }

    #[test]
    fn selected_group_resolves_single_text_child() {
        let mut doc = Document::empty();
        let group = doc.create_node(Node::new("g", NodeKind::Group));
        let text = doc.create_node(text_node(None));

        doc.attach(text, Parent::Node(group), 0).unwrap();
        doc.attach(group, Parent::Comp(doc.main), 0).unwrap();

        assert_eq!(
            selected_text_node(&doc, &[group]),
            Some(text)
        );
    }
}