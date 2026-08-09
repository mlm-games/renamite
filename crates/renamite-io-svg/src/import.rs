//! SVG import: `usvg` tree -> Renamite `Document`.
//!
//! `usvg` preprocesses CSS, resolves references/`<use>`, converts primitives
//! to absolute paths, resolves images, and flattens text. Everything is baked
//! into world space: geometry carries the absolute transform and node
//! transforms are left at identity (except images, whose affine is decomposed
//! back into a node transform).

use image::GenericImageView;
use kurbo::Affine;
use renamite_animation::{Animated, Frame};
use renamite_model::{
    Asset, BlendMode, Document, ImageAsset, LayerProps, MaskProps, Node, NodeKind, Parent,
    ShapeKind,
};
use usvg::ImageKind;
use usvg::Node as SvgNode;

use crate::paint::{import_fill, import_stroke};
use crate::path::{affine_to_animated_transform, tiny_path_to_kurbo, usvg_transform_to_kurbo};
use crate::{SvgError, SvgReport, SvgWarning};

pub fn import_with_report(bytes: &[u8]) -> Result<SvgReport<Document>, SvgError> {
    let mut options = usvg::Options::default();
    options
        .fontdb_mut()
        .load_font_data(renamite_text::default_font_bytes().to_vec());
    // Resolve text with the same deterministic default face the editor uses.
    if let Some(family) = renamite_text::font_family_name(renamite_text::default_font_bytes()) {
        options.font_family = family;
    }
    let tree = usvg::Tree::from_data(bytes, &options)?;

    let mut importer = Importer {
        document: Document::empty(),
        warnings: Vec::new(),
    };
    importer.import_tree(&tree);

    Ok(SvgReport {
        value: importer.document,
        warnings: importer.warnings,
    })
}

/// Per-element context needed while importing a paint: the element's id (for
/// node names), the warning path, and its absolute transform (gradients live
/// in the element's local space and must be folded into world space).
pub struct PaintContext {
    pub path: String,
    pub id: String,
    pub element_affine: Affine,
}

impl PaintContext {
    pub fn name(&self, fallback: &str) -> String {
        nonempty_name(&self.id, fallback)
    }

    /// Compose the element's absolute transform with a gradient's own
    /// transform, producing the affine that maps gradient-local coordinates
    /// into world space.
    pub fn paint_affine(&self, gradient_transform: usvg::Transform) -> Affine {
        self.element_affine * usvg_transform_to_kurbo(gradient_transform)
    }
}

struct ImportTree {
    node: Node,
    children: Vec<ImportTree>,
}

impl ImportTree {
    fn leaf(node: Node) -> Self {
        Self {
            node,
            children: Vec::new(),
        }
    }

    fn with_children(node: Node, children: Vec<ImportTree>) -> Self {
        Self { node, children }
    }
}

struct Importer {
    document: Document,
    warnings: Vec<SvgWarning>,
}

impl Importer {
    fn import_tree(&mut self, tree: &usvg::Tree) {
        let size = tree.size();
        let main = self.document.main;
        let composition = &mut self.document.compositions[main];
        composition.name = "Imported SVG".into();
        composition.size = (
            size.width().round().max(1.0) as u32,
            size.height().round().max(1.0) as u32,
        );
        composition.range = (Frame(0), Frame(1));

        let root = tree.root();
        let needs_wrapper = root.opacity().get() < 1.0
            || root.clip_path().is_some()
            || root.mask().is_some()
            || root.blend_mode() != usvg::BlendMode::Normal;

        let trees = if needs_wrapper {
            self.import_group(root, "svg")
                .map(|tree| vec![tree])
                .unwrap_or_default()
        } else {
            self.import_children(root.children(), "svg")
        };
        for tree in trees {
            self.attach_tree(tree, Parent::Comp(main));
        }
    }

    fn attach_tree(&mut self, tree: ImportTree, parent: Parent) {
        let id = self.document.create_node(tree.node);
        self.document
            .attach(id, parent, usize::MAX)
            .expect("fresh imported node attaches");
        for child in tree.children {
            self.attach_tree(child, Parent::Node(id));
        }
    }

    /// Import sibling nodes. SVG paints in document order (later = on top),
    /// while Renamite stacks index 0 on top, so the list is reversed before
    /// it is attached (appended).
    fn import_children(&mut self, nodes: &[SvgNode], base_path: &str) -> Vec<ImportTree> {
        let mut trees = Vec::new();
        for (index, node) in nodes.iter().enumerate() {
            let path = format!("{base_path}/{index}");
            if let Some(tree) = self.import_node(node, &path) {
                trees.push(tree);
            }
        }
        trees.reverse();
        trees
    }

    fn import_node(&mut self, node: &SvgNode, path: &str) -> Option<ImportTree> {
        match node {
            SvgNode::Group(group) => self.import_group(group, path),
            SvgNode::Path(path_node) => self.import_path_node(path_node, path),
            SvgNode::Text(text) => {
                self.warnings.push(SvgWarning {
                    path: path.into(),
                    message: "SVG text imported as editable path outlines".into(),
                });
                let content = self.import_children(text.flattened().children(), path);
                if content.is_empty() {
                    return None;
                }
                Some(ImportTree::with_children(
                    Node::new(nonempty_name(text.id(), "Text"), NodeKind::Group),
                    content,
                ))
            }
            SvgNode::Image(image) => self.import_image_node(image, path),
        }
    }

    fn import_group(&mut self, group: &usvg::Group, path: &str) -> Option<ImportTree> {
        let mut children: Vec<ImportTree> = Vec::new();

        // Masks/clip paths clip everything that follows, so they lead.
        if let Some(clip) = group.clip_path()
            && let Some(mask) = self.import_clip_path(clip, group, path)
        {
            children.push(mask);
        }
        if let Some(mask) = group.mask()
            && let Some(mask_tree) = self.import_mask(mask, group, path)
        {
            children.push(mask_tree);
        }

        for filter in group.filters() {
            self.warnings.push(SvgWarning {
                path: path.into(),
                message: format!(
                    "SVG filter `{}` is not supported in Renamite and was skipped",
                    filter.id()
                ),
            });
        }

        let content = self.import_children(group.children(), path);
        if children.is_empty() && content.is_empty() {
            return None;
        }
        children.extend(content);

        let mut node = Node::new(nonempty_name(group.id(), "Group"), NodeKind::Group);
        node.opacity = Animated::new(group.opacity().get() as f64);
        let blend = match group.blend_mode() {
            usvg::BlendMode::Normal => BlendMode::Normal,
            usvg::BlendMode::Multiply => BlendMode::Multiply,
            usvg::BlendMode::Screen => BlendMode::Screen,
            unsupported => {
                self.warnings.push(SvgWarning {
                    path: path.into(),
                    message: format!(
                        "SVG blend mode `{unsupported:?}` is not supported; using normal"
                    ),
                });
                BlendMode::Normal
            }
        };
        if blend != BlendMode::Normal {
            node.kind = NodeKind::Layer(LayerProps {
                blend,
                ..LayerProps::default()
            });
        }
        Some(ImportTree::with_children(node, children))
    }

    fn import_path_node(&mut self, path_node: &usvg::Path, path: &str) -> Option<ImportTree> {
        if !path_node.is_visible() {
            return None;
        }
        let affine = usvg_transform_to_kurbo(path_node.abs_transform());
        let geometry = affine * tiny_path_to_kurbo(path_node.data());
        let vector_path = renamite_geometry::VectorPath::from_bez_path(&geometry);

        let name = nonempty_name(path_node.id(), "Path");
        let mut children = vec![ImportTree::leaf(Node::new(
            name.clone(),
            NodeKind::Shape(ShapeKind::Path(Animated::new(vector_path))),
        ))];

        let context = PaintContext {
            path: path.into(),
            id: path_node.id().into(),
            element_affine: affine,
        };
        if let Some(fill) = path_node.fill()
            && let Some(style) = import_fill(fill, &context, &mut self.warnings)
        {
            children.push(ImportTree::leaf(style));
        }
        if let Some(stroke) = path_node.stroke()
            && let Some(style) = import_stroke(stroke, &context, &mut self.warnings)
        {
            // Stroke paints on top of the fill in SVG, so it must come
            // before the fill in the (top-first) child list.
            children.insert(1, ImportTree::leaf(style));
        }

        if children.len() == 1 {
            return Some(children.pop().unwrap());
        }
        Some(ImportTree::with_children(
            Node::new(name, NodeKind::Group),
            children,
        ))
    }

    fn import_clip_path(
        &mut self,
        clip: &usvg::ClipPath,
        owner: &usvg::Group,
        path: &str,
    ) -> Option<ImportTree> {
        let clip_affine = usvg_transform_to_kurbo(clip.transform());
        // The referencing group's absolute transform maps the clip's user
        // space into world space; the clip children are relative to the clip
        // root (identity), so compose both.
        let bias = usvg_transform_to_kurbo(owner.abs_transform()) * clip_affine;
        let shape = self.import_clip_shape(clip.root(), path, bias)?;
        let mut node = Node::new(
            nonempty_name(clip.id(), "Clip"),
            NodeKind::Mask(MaskProps {
                inverted: false,
                shape,
            }),
        );
        node.visible = true;
        Some(ImportTree::leaf(node))
    }

    fn import_mask(
        &mut self,
        mask: &usvg::Mask,
        owner: &usvg::Group,
        path: &str,
    ) -> Option<ImportTree> {
        if mask.kind() != usvg::MaskType::Alpha {
            self.warnings.push(SvgWarning {
                path: path.into(),
                message: "SVG luminance masks are approximated as alpha masks".into(),
            });
        }
        let bias = usvg_transform_to_kurbo(owner.abs_transform());
        let shape = self.import_clip_shape(mask.root(), path, bias)?;
        Some(ImportTree::leaf(Node::new(
            nonempty_name(mask.id(), "Mask"),
            NodeKind::Mask(MaskProps {
                inverted: false,
                shape,
            }),
        )))
    }

    /// Collect every visible shape inside a clip/mask root into a single
    /// combined path, in world space.
    fn import_clip_shape(
        &mut self,
        root: &usvg::Group,
        path: &str,
        bias: Affine,
    ) -> Option<ShapeKind> {
        let mut combined = kurbo::BezPath::new();
        self.gather_clip_geometry(root, &bias, &mut combined, path);
        if combined.is_empty() {
            None
        } else {
            Some(ShapeKind::Path(Animated::new(
                renamite_geometry::VectorPath::from_bez_path(&combined),
            )))
        }
    }

    fn gather_clip_geometry(
        &mut self,
        group: &usvg::Group,
        bias: &Affine,
        combined: &mut kurbo::BezPath,
        path: &str,
    ) {
        for (index, child) in group.children().iter().enumerate() {
            match child {
                SvgNode::Path(p) => {
                    if p.is_visible() {
                        let affine = *bias * usvg_transform_to_kurbo(p.abs_transform());
                        combined.extend(affine * tiny_path_to_kurbo(p.data()));
                    }
                }
                SvgNode::Group(g) => self.gather_clip_geometry(g, bias, combined, path),
                SvgNode::Text(t) => {
                    self.warnings.push(SvgWarning {
                        path: format!("{path}/{index}"),
                        message: "SVG text inside a clip path imported as path outlines".into(),
                    });
                    self.gather_clip_geometry(t.flattened(), bias, combined, path);
                }
                SvgNode::Image(_) => {
                    self.warnings.push(SvgWarning {
                        path: format!("{path}/{index}"),
                        message: "SVG image inside a clip path was skipped".into(),
                    });
                }
            }
        }
    }

    fn import_image_node(&mut self, image: &usvg::Image, path: &str) -> Option<ImportTree> {
        if !image.is_visible() {
            return None;
        }
        let (mime, bytes) = match image.kind() {
            ImageKind::PNG(data) => ("image/png", data.as_ref()),
            ImageKind::JPEG(data) => ("image/jpeg", data.as_ref()),
            ImageKind::GIF(data) => ("image/gif", data.as_ref()),
            ImageKind::WEBP(data) => ("image/webp", data.as_ref()),
            ImageKind::SVG(_) => {
                self.warnings.push(SvgWarning {
                    path: path.into(),
                    message: "SVG-in-SVG images are not supported in v1 and were skipped".into(),
                });
                return None;
            }
        };

        let (width, height) = match image::load_from_memory(bytes) {
            Ok(decoded) => decoded.dimensions(),
            Err(error) => {
                self.warnings.push(SvgWarning {
                    path: path.into(),
                    message: format!("SVG image failed to decode and was skipped: {error}"),
                });
                return None;
            }
        };

        let name = nonempty_name(image.id(), "Image");
        let asset_id = self.document.assets.insert(Asset::Image(ImageAsset {
            name: name.clone(),
            mime: mime.into(),
            bytes: bytes.to_vec(),
            width,
            height,
            srgb: true,
        }));
        self.document.asset_order.push(asset_id);

        let affine = usvg_transform_to_kurbo(image.abs_transform());
        let transform = match affine_to_animated_transform(affine) {
            Some(transform) => transform,
            None => {
                self.warnings.push(SvgWarning {
                    path: path.into(),
                    message: "SVG image has a transform (e.g. reflection) that cannot be "
                        .to_string()
                        + "represented; skipped",
                });
                return None;
            }
        };
        let mut node = Node::new(name, NodeKind::Image(asset_id));
        node.transform = transform;
        Some(ImportTree::leaf(node))
    }
}

fn nonempty_name(id: &str, fallback: &str) -> String {
    if id.is_empty() {
        fallback.to_string()
    } else {
        id.to_string()
    }
}
