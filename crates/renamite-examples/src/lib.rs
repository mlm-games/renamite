//! Built-in Renamite example projects and templates.
//!
//! Templates are constructed in code (rather than shipped as `.ren` fixtures)
//! so they never drift out of sync with the model format. The same builders
//! back the CLI `new --template`, the editor empty state, and the example
//! smoke tests.

use glam::DVec2;
use renamite_animation::{Animated, AnimatedTransform, EasingHandle, Frame, Interpolation};
use renamite_io_ren::RenFile;
use renamite_model::{
    Color, Document, FillRule, GradientStop, GradientStops, MaskProps, ModifierKind, Node,
    NodeKind, Parent, ShapeKind, StarKind, StrokeCap, StrokeJoin, StyleKind, StylePaint, TextAlign,
    TextNode, TrimMode,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemplateId {
    Blank,
    BouncingBall,
    LoaderTrimPath,
    MaskedText,
    PhotoCard,
    RepeaterBurst,
    GradientPoster,
}

pub struct TemplateInfo {
    pub id: TemplateId,
    pub name: &'static str,
    pub description: &'static str,
}

pub fn templates() -> &'static [TemplateInfo] {
    &[
        TemplateInfo {
            id: TemplateId::Blank,
            name: "Blank",
            description: "Empty 512×512 composition.",
        },
        TemplateInfo {
            id: TemplateId::BouncingBall,
            name: "Bouncing Ball",
            description: "Position keyframes with eased motion.",
        },
        TemplateInfo {
            id: TemplateId::LoaderTrimPath,
            name: "Trim Path Loader",
            description: "Animated trim path on a circular stroke.",
        },
        TemplateInfo {
            id: TemplateId::MaskedText,
            name: "Masked Text",
            description: "Text clipped by a moving vector mask.",
        },
        TemplateInfo {
            id: TemplateId::PhotoCard,
            name: "Photo Card",
            description: "Image placeholder with rounded clipping mask.",
        },
        TemplateInfo {
            id: TemplateId::RepeaterBurst,
            name: "Repeater Burst",
            description: "Repeater with opacity falloff.",
        },
        TemplateInfo {
            id: TemplateId::GradientPoster,
            name: "Gradient Poster",
            description: "Gradient fill, text, and vector shapes.",
        },
    ]
}

pub fn build_template(id: TemplateId) -> RenFile {
    match id {
        TemplateId::Blank => RenFile::new(Document::empty(), "Blank"),
        TemplateId::BouncingBall => bouncing_ball(),
        TemplateId::LoaderTrimPath => loader_trim_path(),
        TemplateId::MaskedText => masked_text(),
        TemplateId::PhotoCard => photo_card(),
        TemplateId::RepeaterBurst => repeater_burst(),
        TemplateId::GradientPoster => gradient_poster(),
    }
}

impl TemplateId {
    /// All template ids in [`templates`] order.
    pub fn all() -> &'static [TemplateId] {
        &[
            TemplateId::Blank,
            TemplateId::BouncingBall,
            TemplateId::LoaderTrimPath,
            TemplateId::MaskedText,
            TemplateId::PhotoCard,
            TemplateId::RepeaterBurst,
            TemplateId::GradientPoster,
        ]
    }

    /// Kebab-case CLI value name, e.g. `bouncing-ball`.
    pub fn slug(&self) -> &'static str {
        match self {
            TemplateId::Blank => "blank",
            TemplateId::BouncingBall => "bouncing-ball",
            TemplateId::LoaderTrimPath => "loader-trim-path",
            TemplateId::MaskedText => "masked-text",
            TemplateId::PhotoCard => "photo-card",
            TemplateId::RepeaterBurst => "repeater-burst",
            TemplateId::GradientPoster => "gradient-poster",
        }
    }

    /// Human-readable display name, e.g. `Bouncing Ball`.
    pub fn display_name(&self) -> &'static str {
        templates()
            .iter()
            .find(|t| t.id == *self)
            .map(|t| t.name)
            .unwrap_or(self.slug())
    }
}

/// Case- and separator-insensitive template lookup. Accepts slugs
/// (`bouncing-ball`) and display names (`Bouncing Ball`).
pub fn parse_template(input: &str) -> Option<TemplateId> {
    let normalize = |s: &str| s.to_ascii_lowercase().replace(['-', ' '], "");
    let input = normalize(input);
    templates()
        .iter()
        .find(|t| normalize(t.id.slug()) == input || normalize(t.name) == input)
        .map(|t| t.id)
}

fn doc_named(name: &str) -> Document {
    let mut doc = Document::empty();
    doc.compositions[doc.main].name = name.into();
    doc
}

fn attach_group_with(
    doc: &mut Document,
    name: &str,
    children: Vec<renamite_model::NodeId>,
) -> renamite_model::NodeId {
    let group = doc.create_node(Node::new(name, NodeKind::Group));
    for child in children {
        doc.attach(child, Parent::Node(group), usize::MAX).unwrap();
    }
    doc.attach(group, Parent::Comp(doc.main), usize::MAX)
        .unwrap();
    group
}

fn solid_fill(doc: &mut Document, color: Color) -> renamite_model::NodeId {
    doc.create_node(Node::new(
        "Fill",
        NodeKind::Style(StyleKind::Fill {
            paint: StylePaint::solid(color),
            rule: FillRule::NonZero,
        }),
    ))
}

fn solid_stroke(doc: &mut Document, color: Color, width: f64) -> renamite_model::NodeId {
    doc.create_node(Node::new(
        "Stroke",
        NodeKind::Style(StyleKind::Stroke {
            paint: StylePaint::solid(color),
            width: Animated::new(width),
            cap: StrokeCap::Round,
            join: StrokeJoin::Round,
            dash: None,
        }),
    ))
}

fn key_vec2(frame: i64, value: DVec2) -> renamite_animation::Keyframe<DVec2> {
    renamite_animation::Keyframe {
        frame: Frame(frame),
        value,
        interpolation: Interpolation::CubicBezier,
        ease_out: EasingHandle { x: 0.42, y: 0.0 },
        ease_in: EasingHandle { x: 0.58, y: 1.0 },
    }
}

fn key_f64(frame: i64, value: f64) -> renamite_animation::Keyframe<f64> {
    renamite_animation::Keyframe {
        frame: Frame(frame),
        value,
        interpolation: Interpolation::CubicBezier,
        ease_out: EasingHandle { x: 0.42, y: 0.0 },
        ease_in: EasingHandle { x: 0.58, y: 1.0 },
    }
}

fn bouncing_ball() -> RenFile {
    let mut doc = doc_named("Bouncing Ball");

    let ball = doc.create_node(Node::new(
        "Ball",
        NodeKind::Shape(ShapeKind::Ellipse {
            pos: Animated::new(DVec2::ZERO),
            size: Animated::new(DVec2::new(96.0, 96.0)),
        }),
    ));

    let fill = solid_fill(&mut doc, Color::rgba(0.96, 0.42, 0.18, 1.0));
    let group = attach_group_with(&mut doc, "Bouncing Ball", vec![ball, fill]);

    doc.nodes[group].transform.position = Animated {
        base: DVec2::new(256.0, 160.0),
        keyframes: vec![
            key_vec2(0, DVec2::new(256.0, 160.0)),
            key_vec2(30, DVec2::new(256.0, 380.0)),
            key_vec2(60, DVec2::new(256.0, 160.0)),
        ],
    };

    doc.nodes[group].transform.scale = Animated {
        base: DVec2::splat(100.0),
        keyframes: vec![
            key_vec2(0, DVec2::splat(100.0)),
            key_vec2(28, DVec2::new(120.0, 80.0)),
            key_vec2(34, DVec2::splat(100.0)),
        ],
    };

    RenFile::new(doc, "Bouncing Ball")
}

fn loader_trim_path() -> RenFile {
    let mut doc = doc_named("Trim Path Loader");

    let circle = doc.create_node(Node::new(
        "Circle",
        NodeKind::Shape(ShapeKind::Ellipse {
            pos: Animated::new(DVec2::new(256.0, 256.0)),
            size: Animated::new(DVec2::new(260.0, 260.0)),
        }),
    ));

    let trim = doc.create_node(Node::new(
        "Trim Path",
        NodeKind::Modifier(ModifierKind::TrimPath {
            start: Animated::new(0.0),
            end: Animated {
                base: 0.2,
                keyframes: vec![key_f64(0, 0.15), key_f64(45, 0.75), key_f64(90, 0.15)],
            },
            offset: Animated {
                base: 0.0,
                keyframes: vec![key_f64(0, 0.0), key_f64(90, 1.0)],
            },
            mode: TrimMode::Individually,
        }),
    ));

    let stroke = solid_stroke(&mut doc, Color::rgba(0.1, 0.4, 0.9, 1.0), 22.0);

    attach_group_with(&mut doc, "Loader", vec![circle, trim, stroke]);

    RenFile::new(doc, "Trim Path Loader")
}

fn masked_text() -> RenFile {
    let mut doc = doc_named("Masked Text");

    let mask = doc.create_node(Node::new(
        "Ellipse Mask",
        NodeKind::Mask(MaskProps {
            inverted: false,
            shape: ShapeKind::Ellipse {
                pos: Animated::new(DVec2::new(256.0, 210.0)),
                size: Animated::new(DVec2::new(360.0, 150.0)),
            },
        }),
    ));

    let mut text = Node::new(
        "Text",
        NodeKind::Text(TextNode {
            text: "RENAMITE".into(),
            size: Animated::new(72.0),
            align: TextAlign::Center,
            font: None,
        }),
    );
    text.transform.position = Animated::new(DVec2::new(256.0, 280.0));

    let text = doc.create_node(text);
    let fill = solid_fill(&mut doc, Color::rgba(0.96, 0.42, 0.18, 1.0));

    attach_group_with(&mut doc, "Masked Text", vec![mask, text, fill]);

    RenFile::new(doc, "Masked Text")
}

/// A deterministic 2×2 RGBA PNG: red / green / blue / yellow.
fn tiny_png() -> Vec<u8> {
    let mut image = image::RgbaImage::new(2, 2);
    image.put_pixel(0, 0, image::Rgba([255, 80, 80, 255]));
    image.put_pixel(1, 0, image::Rgba([80, 255, 120, 255]));
    image.put_pixel(0, 1, image::Rgba([80, 120, 255, 255]));
    image.put_pixel(1, 1, image::Rgba([255, 230, 80, 255]));

    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

fn photo_card() -> RenFile {
    let mut doc = doc_named("Photo Card");

    let asset = doc
        .assets
        .insert(renamite_model::Asset::Image(renamite_model::ImageAsset {
            name: "placeholder.png".into(),
            mime: "image/png".into(),
            bytes: tiny_png(),
            width: 2,
            height: 2,
            srgb: true,
        }));
    doc.asset_order.push(asset);

    let mut image = Node::new("Image", NodeKind::Image(asset));
    image.transform.anchor = Animated::new(DVec2::new(1.0, 1.0));
    image.transform.position = Animated::new(DVec2::new(256.0, 256.0));
    image.transform.scale = Animated::new(DVec2::splat(10_000.0));

    let image = doc.create_node(image);

    let mask = doc.create_node(Node::new(
        "Rounded Mask",
        NodeKind::Mask(MaskProps {
            inverted: false,
            shape: ShapeKind::Rect {
                pos: Animated::new(DVec2::new(256.0, 256.0)),
                size: Animated::new(DVec2::new(300.0, 220.0)),
                rounded: Animated::new(24.0),
            },
        }),
    ));

    attach_group_with(&mut doc, "Photo Card", vec![mask, image]);

    RenFile::new(doc, "Photo Card")
}

fn repeater_burst() -> RenFile {
    let mut doc = doc_named("Repeater Burst");

    let star = doc.create_node(Node::new(
        "Spark",
        NodeKind::Shape(ShapeKind::Star {
            pos: Animated::new(DVec2::new(256.0, 140.0)),
            points: Animated::new(5.0),
            inner_r: Animated::new(16.0),
            outer_r: Animated::new(42.0),
            roundness: Animated::new(0.0),
            kind: StarKind::Star,
        }),
    ));

    let mut step = AnimatedTransform::identity();
    step.rotation = Animated::new(renamite_animation::Angle(36.0));
    step.position = Animated::new(DVec2::new(0.0, 0.0));
    step.scale = Animated::new(DVec2::splat(96.0));

    let repeater = doc.create_node(Node::new(
        "Repeater",
        NodeKind::Modifier(ModifierKind::Repeater {
            copies: Animated::new(10.0),
            offset: Animated::new(0.0),
            transform: Box::new(step),
            start_opacity: Animated::new(1.0),
            end_opacity: Animated::new(0.15),
        }),
    ));

    let fill = solid_fill(&mut doc, Color::rgba(1.0, 0.85, 0.1, 1.0));

    attach_group_with(&mut doc, "Repeater Burst", vec![star, repeater, fill]);

    RenFile::new(doc, "Repeater Burst")
}

fn gradient_poster() -> RenFile {
    let mut doc = doc_named("Gradient Poster");

    let rect = doc.create_node(Node::new(
        "Background",
        NodeKind::Shape(ShapeKind::Rect {
            pos: Animated::new(DVec2::new(256.0, 256.0)),
            size: Animated::new(DVec2::new(512.0, 512.0)),
            rounded: Animated::new(0.0),
        }),
    ));

    let gradient = doc.create_node(Node::new(
        "Gradient Fill",
        NodeKind::Style(StyleKind::Fill {
            paint: StylePaint::linear(
                DVec2::new(0.0, 0.0),
                DVec2::new(512.0, 512.0),
                GradientStops(vec![
                    GradientStop {
                        offset: 0.0,
                        color: Color::rgba(0.1, 0.2, 0.9, 1.0),
                    },
                    GradientStop {
                        offset: 1.0,
                        color: Color::rgba(1.0, 0.3, 0.6, 1.0),
                    },
                ]),
            ),
            rule: FillRule::NonZero,
        }),
    ));

    let mut text = Node::new(
        "Title",
        NodeKind::Text(TextNode {
            text: "MOTION".into(),
            size: Animated::new(92.0),
            align: TextAlign::Center,
            font: None,
        }),
    );
    text.transform.position = Animated::new(DVec2::new(256.0, 280.0));
    let text = doc.create_node(text);

    let text_fill = solid_fill(&mut doc, Color::WHITE);

    // Each style scopes to its own shape, so keep the background and the title
    // in separate groups (a style paints every shape path in its group).
    attach_group_with(&mut doc, "Background", vec![rect, gradient]);
    attach_group_with(&mut doc, "Title", vec![text, text_fill]);

    RenFile::new(doc, "Gradient Poster")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_template_has_main_comp_and_renders_nonempty_or_blank() {
        for t in templates() {
            let f = build_template(t.id);
            assert!(f.document.compositions.contains_key(f.document.main));

            if t.id != TemplateId::Blank {
                let scene = renamite_model::evaluate(&f.document, f.document.main, 0.0);
                assert!(!scene.items.is_empty(), "{} should render items", t.name);
            }
        }
    }

    #[test]
    fn templates_serialize_to_ren_and_back() {
        for t in templates() {
            let file = build_template(t.id);
            let bytes = renamite_io_ren::save(&file).unwrap();
            let back = renamite_io_ren::open(&bytes).unwrap();
            assert_eq!(
                back.document.compositions[back.document.main].name,
                file.document.compositions[file.document.main].name
            );
        }
    }

    #[test]
    fn templates_pack_and_unpack_binary() {
        for t in templates() {
            let file = build_template(t.id);
            let packed = renamite_io_ren::save_binary(&file).unwrap();
            let back = renamite_io_ren::open_binary(&packed).unwrap();
            assert_eq!(back.document.nodes.len(), file.document.nodes.len());
        }
    }
}
