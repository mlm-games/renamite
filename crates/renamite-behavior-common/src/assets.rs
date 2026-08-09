use renamite_model::{Asset, AssetId, Document, Node, NodeId, NodeKind, Parent};

#[derive(Clone, Debug)]
pub struct FontAssetRow {
    pub id: Option<AssetId>,
    pub family: String,
    pub name: String,
    pub usage_count: usize,
    pub bundled: bool,
}

#[derive(Clone, Debug)]
pub struct ImageAssetRow {
    pub id: AssetId,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub mime: String,
    pub usage_count: usize,
}

pub fn image_rows(doc: &Document) -> Vec<ImageAssetRow> {
    doc.asset_order
        .iter()
        .filter_map(|id| {
            let image = doc.image_asset(*id)?;

            Some(ImageAssetRow {
                id: *id,
                name: image.name.clone(),
                width: image.width,
                height: image.height,
                mime: image.mime.clone(),
                usage_count: doc.image_usage_count(*id),
            })
        })
        .collect()
}

/// Build an `InsertNode` command that places an image layer centered on
/// `position` in composition space, with the anchor at the image center so
/// scaling/rotation feel natural.
pub fn cmd_place_image(
    doc: &Document,
    asset: AssetId,
    parent: Parent,
    index: usize,
    position: glam::DVec2,
) -> Option<renamite_history::EditorCommand> {
    let image = doc.image_asset(asset)?;

    let mut node = Node::new(image.name.clone(), NodeKind::Image(asset));

    node.transform.anchor = renamite_animation::Animated::new(glam::DVec2::new(
        image.width as f64 * 0.5,
        image.height as f64 * 0.5,
    ));

    node.transform.position = renamite_animation::Animated::new(position);

    Some(renamite_history::EditorCommand::InsertNode {
        parent,
        index,
        tree: renamite_history::NodeTree::leaf(node),
    })
}

pub fn font_rows(doc: &Document) -> Vec<FontAssetRow> {
    let mut rows = vec![FontAssetRow {
        id: None,
        family: "default".into(),
        name: "Bundled Default".into(),
        usage_count: font_usage_count(doc, "default"),
        bundled: true,
    }];

    rows.extend(doc.asset_order.iter().filter_map(|id| {
        let Asset::Font(font) = doc.assets.get(*id)? else {
            return None;
        };

        Some(FontAssetRow {
            id: Some(*id),
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
pub fn selected_text_node(doc: &Document, selection: &[NodeId]) -> Option<NodeId> {
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

        assert_eq!(selected_text_node(&doc, &[group]), Some(text));
    }
}
