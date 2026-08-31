//! Serializable document model + pure evaluator.
//!
//! Group evaluation: pass 1 accumulates shape paths and applies modifiers in
//! document order; pass 2 recurses/emits styles bottom-first so `Scene.items`
//! is in painter's order. Nodes live in a slotmap arena; tree membership is
//! attach/detach so undo/redo never changes a NodeId.

use kurbo::{Affine, BezPath, ParamCurveNearest, Point, Shape as KurboShape};
use renamite_animation::{
    Angle, Animated, AnimatedTransform, EasingHandle, Frame, Interpolation, Tween,
};
use renamite_geometry::{VectorPath, dash_bez_path, offset_bez_path};
pub use renamite_text::TextAlign;
use serde::de::{Deserializer, Error as DeError, Visitor};
use serde::{Deserialize, Serialize};
use slotmap::{SlotMap, new_key_type};

new_key_type! {
    pub struct NodeId;
    pub struct CompId;
    pub struct AssetId;
}

pub type NodeMap = SlotMap<NodeId, Node>;
pub type CompMap = SlotMap<CompId, Composition>;
pub type AssetMap = SlotMap<AssetId, Asset>;

#[derive(Clone, Serialize, Deserialize)]
pub struct Document {
    pub format_version: u32,
    pub compositions: CompMap,
    pub nodes: NodeMap,
    pub assets: AssetMap,

    /// Live/attached assets in UI order.
    #[serde(default)]
    pub asset_order: Vec<AssetId>,

    pub main: CompId,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Composition {
    pub name: String,
    pub size: (u32, u32),
    pub rate: renamite_animation::FrameRate,
    pub range: (Frame, Frame),
    /// z-order: index 0 = top of stack.
    pub children: Vec<NodeId>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub name: String,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub visible: bool,
    pub locked: bool,
    pub transform: AnimatedTransform,
    pub opacity: Animated<f64>,
    pub kind: NodeKind,
}

impl Node {
    pub fn new(name: impl Into<String>, kind: NodeKind) -> Self {
        Self {
            name: name.into(),
            parent: None,
            children: Vec::new(),
            visible: true,
            locked: false,
            transform: AnimatedTransform::identity(),
            opacity: Animated::new(1.0),
            kind,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum NodeKind {
    Group,
    Layer(LayerProps),
    Shape(ShapeKind),
    Style(StyleKind),
    Modifier(ModifierKind),
    Text(TextNode),
    Image(AssetId),
    Precomp { comp: CompId, time_map: TimeMap },
    Mask(MaskProps),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LayerProps {
    pub in_frame: Frame,
    pub out_frame: Frame,
    pub time_stretch: f64,
    pub blend: BlendMode,
}

impl Default for LayerProps {
    fn default() -> Self {
        Self {
            in_frame: Frame(0),
            out_frame: Frame(i64::MAX / 2),
            time_stretch: 1.0,
            blend: BlendMode::Normal,
        }
    }
}

/// Multi-contour geometry: boolean results and stroke expansions produce
/// outer contours plus holes, which one `VectorPath` cannot represent.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CompoundPath {
    /// Every entry is one contour. Linesweeper's orientation guarantees that
    /// holes work under both NonZero and EvenOdd filling.
    pub contours: Vec<Animated<VectorPath>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ShapeKind {
    Path(Animated<VectorPath>),
    Rect {
        pos: Animated<glam::DVec2>,
        size: Animated<glam::DVec2>,
        rounded: Animated<f64>,
    },
    Ellipse {
        pos: Animated<glam::DVec2>,
        size: Animated<glam::DVec2>,
    },
    Star {
        pos: Animated<glam::DVec2>,
        points: Animated<f64>,
        inner_r: Animated<f64>,
        outer_r: Animated<f64>,
        roundness: Animated<f64>,
        kind: StarKind,
    },
    Polygon {
        pos: Animated<glam::DVec2>,
        points: Animated<f64>,
        outer_r: Animated<f64>,
        roundness: Animated<f64>,
    },

    // Appended last to keep postcard enum indices of existing variants stable.
    CompoundPath(CompoundPath),
}

impl CompoundPath {
    /// All contours flattened into one multi-subpath `BezPath` at `frame`.
    pub fn to_bez_path(&self, frame: f64) -> BezPath {
        let mut result = BezPath::new();
        for contour in &self.contours {
            result.extend(
                contour
                    .value_at(frame)
                    .to_bez_path()
                    .elements()
                    .iter()
                    .copied(),
            );
        }
        result
    }
}

/// One color anchor along a gradient axis. `offset` is in 0..=1.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GradientStop {
    pub offset: f64,
    pub color: Color,
}

/// Ordered gradient stops, sampled by `sample`. Kept small (usually 2-4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GradientStops(pub Vec<GradientStop>);

impl Default for GradientStops {
    fn default() -> Self {
        Self(vec![
            GradientStop {
                offset: 0.0,
                color: Color::rgba(1.0, 1.0, 1.0, 1.0),
            },
            GradientStop {
                offset: 1.0,
                color: Color::rgba(0.0, 0.0, 0.0, 1.0),
            },
        ])
    }
}

impl GradientStops {
    /// Sample the gradient at normalized position `t` (clamped to 0..=1).
    pub fn sample(&self, t: f64) -> Color {
        let t = t.clamp(0.0, 1.0);
        let stops = &self.0;
        if stops.is_empty() {
            return Color::BLACK;
        }
        if t <= stops[0].offset {
            return stops[0].color;
        }
        for w in stops.windows(2) {
            let (a, b) = (&w[0], &w[1]);
            if t <= b.offset {
                let span = (b.offset - a.offset).max(1e-9);
                let u = ((t - a.offset) / span).clamp(0.0, 1.0);
                return Color::rgba(
                    a.color.r + (b.color.r - a.color.r) * u,
                    a.color.g + (b.color.g - a.color.g) * u,
                    a.color.b + (b.color.b - a.color.b) * u,
                    a.color.a + (b.color.a - a.color.a) * u,
                );
            }
        }
        stops.last().unwrap().color
    }
}

impl Tween for GradientStops {
    fn tween(a: &Self, b: &Self, t: f64) -> Self {
        if a.0.len() != b.0.len() {
            return if t < 1.0 { a.clone() } else { b.clone() };
        }
        Self(
            a.0.iter()
                .zip(&b.0)
                .map(|(x, y)| GradientStop {
                    offset: x.offset + (y.offset - x.offset) * t,
                    color: Color::rgba(
                        x.color.r + (y.color.r - x.color.r) * t,
                        x.color.g + (y.color.g - x.color.g) * t,
                        x.color.b + (y.color.b - x.color.b) * t,
                        x.color.a + (y.color.a - x.color.a) * t,
                    ),
                })
                .collect(),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GradientKind {
    Linear,
    Radial,
}

/// A gradient in the *node's* local space. The evaluator folds the owning
/// shape's world transform into `start`/`end` before baking vertex colors.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Gradient {
    pub kind: GradientKind,
    /// Linear: start point. Radial: center.
    pub start: Animated<glam::DVec2>,
    /// Linear: end point. Radial: circumference point (radius = |end-start|).
    pub end: Animated<glam::DVec2>,
    pub stops: Animated<GradientStops>,
}

/// Paint on a style node: solid color or gradient. Animated so whole-list
/// keyframes (e.g. stop-color morphs) work through the existing machinery.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum StylePaint {
    Solid { color: Animated<Color> },
    Gradient(Gradient),
}

impl StylePaint {
    pub fn solid(color: Color) -> Self {
        Self::Solid {
            color: Animated::new(color),
        }
    }

    pub fn linear(start: glam::DVec2, end: glam::DVec2, stops: GradientStops) -> Self {
        Self::Gradient(Gradient {
            kind: GradientKind::Linear,
            start: Animated::new(start),
            end: Animated::new(end),
            stops: Animated::new(stops),
        })
    }

    pub fn radial(center: glam::DVec2, end: glam::DVec2, stops: GradientStops) -> Self {
        Self::Gradient(Gradient {
            kind: GradientKind::Radial,
            start: Animated::new(center),
            end: Animated::new(end),
            stops: Animated::new(stops),
        })
    }

    /// Sample the paint into a `ScenePaint` at `frame`.
    pub fn sample(&self, frame: f64) -> ScenePaint {
        match self {
            StylePaint::Solid { color } => ScenePaint::Solid(color.value_at(frame)),
            StylePaint::Gradient(g) => {
                let start = g.start.value_at(frame);
                let end = g.end.value_at(frame);
                let stops = g.stops.value_at(frame);
                match g.kind {
                    GradientKind::Linear => ScenePaint::LinearGradient { start, end, stops },
                    GradientKind::Radial => ScenePaint::RadialGradient {
                        center: start,
                        end,
                        stops,
                    },
                }
            }
        }
    }

    /// Produce a static paint snapshot at `frame`.
    ///
    /// Current paint is tool state, not an animation track, so copying paint
    /// from a document should sample it rather than copying its keyframes.
    pub fn snapshot(&self, frame: f64) -> Self {
        match self {
            StylePaint::Solid { color } => StylePaint::solid(color.value_at(frame)),
            StylePaint::Gradient(gradient) => StylePaint::Gradient(Gradient {
                kind: gradient.kind,
                start: Animated::new(gradient.start.value_at(frame)),
                end: Animated::new(gradient.end.value_at(frame)),
                stops: Animated::new(gradient.stops.value_at(frame)),
            }),
        }
    }

    /// Change the representative color while preserving paint type.
    ///
    /// For a gradient, this updates its first stop rather than destroying the
    /// gradient and converting it to a solid.
    pub fn set_base_color(&mut self, color: Color) {
        match self {
            StylePaint::Solid { color: animated } => {
                animated.base = color;
                animated.keyframes.clear();
            }
            StylePaint::Gradient(gradient) => {
                gradient.start.keyframes.clear();
                gradient.end.keyframes.clear();
                gradient.stops.keyframes.clear();

                if let Some(first) = gradient.stops.base.0.first_mut() {
                    first.color = color;
                } else {
                    gradient
                        .stops
                        .base
                        .0
                        .push(GradientStop { offset: 0.0, color });
                }
            }
        }
    }
}

impl Tween for StylePaint {
    fn tween(a: &Self, b: &Self, t: f64) -> Self {
        match (a, b) {
            (StylePaint::Solid { color: ca }, StylePaint::Solid { color: cb }) => {
                StylePaint::Solid {
                    color: Animated::new(Tween::tween(&ca.base, &cb.base, t)),
                }
            }
            (StylePaint::Gradient(ga), StylePaint::Gradient(gb)) => {
                StylePaint::Gradient(Gradient {
                    kind: ga.kind,
                    start: Animated::new(Tween::tween(&ga.start.base, &gb.start.base, t)),
                    end: Animated::new(Tween::tween(&ga.end.base, &gb.end.base, t)),
                    stops: Animated::new(Tween::tween(&ga.stops.base, &gb.stops.base, t)),
                })
            }
            _ => {
                if t < 1.0 {
                    a.clone()
                } else {
                    b.clone()
                }
            }
        }
    }
}

impl StyleKind {
    /// Replace the paint (fill or stroke), returning the previous one (undo).
    pub fn swap_paint(&mut self, paint: StylePaint) -> StylePaint {
        match self {
            StyleKind::Fill { paint: p, .. } | StyleKind::Stroke { paint: p, .. } => {
                std::mem::replace(p, paint)
            }
        }
    }

    pub fn paint(&self) -> &StylePaint {
        match self {
            StyleKind::Fill { paint, .. } | StyleKind::Stroke { paint, .. } => paint,
        }
    }
}

impl StylePaint {
    /// For a solid paint: the (possibly keyed) base color. For a gradient:
    /// the first stop's color (stable handle for conversions).
    pub fn base_color(&self) -> Color {
        match self {
            StylePaint::Solid { color } => color.base,
            StylePaint::Gradient(g) => g
                .stops
                .base
                .0
                .first()
                .map(|s| s.color)
                .unwrap_or(Color::BLACK),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum StyleKind {
    Fill {
        paint: StylePaint,
        rule: FillRule,
    },
    Stroke {
        paint: StylePaint,
        width: Animated<f64>,
        cap: StrokeCap,
        join: StrokeJoin,
        dash: Option<AnimatedDash>,
    },
}

#[derive(Default)]
struct StyleCompatContent {
    paint: Option<StylePaint>,
    color: Option<Animated<Color>>,
    width: Option<Animated<f64>>,
    cap: Option<StrokeCap>,
    join: Option<StrokeJoin>,
    dash: Option<AnimatedDash>,
    rule: Option<FillRule>,
}

impl<'de> Deserialize<'de> for StyleKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        enum StyleTag {
            Fill,
            Stroke,
        }

        struct StyleKindVisitor;
        impl<'de> Visitor<'de> for StyleKindVisitor {
            type Value = StyleKind;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a Fill or Stroke style")
            }

            fn visit_enum<A>(self, data: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::EnumAccess<'de>,
            {
                use serde::de::VariantAccess as _;
                struct ContentVisitor {
                    fill: bool,
                }
                impl<'de> Visitor<'de> for ContentVisitor {
                    type Value = StyleCompatContent;
                    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                        f.write_str("style variant fields")
                    }
                    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
                    where
                        A: serde::de::MapAccess<'de>,
                    {
                        use serde::de::Error as _;
                        let mut content = StyleCompatContent::default();
                        while let Some(key) = map.next_key::<String>()? {
                            match key.as_str() {
                                "paint" => content.paint = Some(map.next_value()?),
                                "color" => content.color = Some(map.next_value()?),
                                "width" => content.width = Some(map.next_value()?),
                                "cap" => content.cap = Some(map.next_value()?),
                                "join" => content.join = Some(map.next_value()?),
                                "dash" => {
                                    content.dash = map.next_value::<Option<AnimatedDash>>()?
                                }
                                "rule" => content.rule = Some(map.next_value()?),
                                other => {
                                    return Err(A::Error::unknown_field(
                                        other,
                                        &["paint", "color", "width", "cap", "join", "dash", "rule"],
                                    ));
                                }
                            }
                        }
                        Ok(content)
                    }
                    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
                    where
                        A: serde::de::SeqAccess<'de>,
                    {
                        use serde::de::Error as _;
                        // Postcard encodes struct-variant content positionally
                        // (no keys), in derived declaration order.
                        let mut content = StyleCompatContent {
                            paint: Some(
                                seq.next_element()?
                                    .ok_or_else(|| A::Error::invalid_length(0, &"paint"))?,
                            ),
                            ..Default::default()
                        };
                        if self.fill {
                            content.rule = Some(
                                seq.next_element()?
                                    .ok_or_else(|| A::Error::invalid_length(1, &"rule"))?,
                            );
                        } else {
                            content.width = Some(
                                seq.next_element()?
                                    .ok_or_else(|| A::Error::invalid_length(1, &"width"))?,
                            );
                            content.cap = Some(
                                seq.next_element()?
                                    .ok_or_else(|| A::Error::invalid_length(2, &"cap"))?,
                            );
                            content.join = Some(
                                seq.next_element()?
                                    .ok_or_else(|| A::Error::invalid_length(3, &"join"))?,
                            );
                            content.dash = seq
                                .next_element::<Option<AnimatedDash>>()?
                                .ok_or_else(|| A::Error::invalid_length(4, &"dash"))?;
                        }
                        Ok(content)
                    }
                }
                let (tag, content) = data.variant::<StyleTag>()?;
                let content = match tag {
                    StyleTag::Fill => {
                        content.struct_variant(&["paint", "rule"], ContentVisitor { fill: true })?
                    }
                    StyleTag::Stroke => content.struct_variant(
                        &["paint", "width", "cap", "join", "dash"],
                        ContentVisitor { fill: false },
                    )?,
                };
                let paint = match content.paint {
                    Some(p) => p,
                    None => match content.color {
                        Some(color) => StylePaint::Solid { color },
                        None => return Err(A::Error::missing_field("paint")),
                    },
                };
                match tag {
                    StyleTag::Fill => Ok(StyleKind::Fill {
                        paint,
                        rule: content
                            .rule
                            .ok_or_else(|| A::Error::missing_field("rule"))?,
                    }),
                    StyleTag::Stroke => Ok(StyleKind::Stroke {
                        paint,
                        width: content
                            .width
                            .ok_or_else(|| A::Error::missing_field("width"))?,
                        cap: content.cap.ok_or_else(|| A::Error::missing_field("cap"))?,
                        join: content
                            .join
                            .ok_or_else(|| A::Error::missing_field("join"))?,
                        dash: content.dash,
                    }),
                }
            }
        }

        deserializer.deserialize_enum("StyleKind", &["Fill", "Stroke"], StyleKindVisitor)
    }
}

/// Serde default for a scalar animation pinned to a constant `1.0`.
fn animated_one() -> Animated<f64> {
    Animated::new(1.0)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ModifierKind {
    TrimPath {
        start: Animated<f64>,
        end: Animated<f64>,
        offset: Animated<f64>,
        #[serde(default)]
        mode: TrimMode,
    },
    Repeater {
        copies: Animated<f64>,
        offset: Animated<f64>,
        transform: AnimatedTransform,
        /// Opacity of the first copy (0..=1). Lottie `so` / 100.
        #[serde(default = "animated_one")]
        start_opacity: Animated<f64>,
        /// Opacity of the last copy (0..=1). Lottie `eo` / 100.
        #[serde(default = "animated_one")]
        end_opacity: Animated<f64>,
    },
    RoundCorners {
        radius: Animated<f64>,
    },
    OffsetPath {
        amount: Animated<f64>,
    },
    ZigZag {
        amplitude: Animated<f64>,
        frequency: Animated<f64>,
        /// false = corner zig-zag; true = smooth wave (cubic).
        #[serde(default)]
        smooth: bool,
    },
    PuckerBloat {
        /// Percent; positive = bloat, negative = pucker.
        amount: Animated<f64>,
    },
}

/// How Trim distributes [start, end] across multiple accumulated paths.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrimMode {
    /// Each path is trimmed to [start, end] of its own perimeter.
    #[default]
    Individually,
    /// The concatenation of all paths is treated as one arc-length domain.
    Simultaneously,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TimeMap {
    pub offset: Frame,
    pub stretch: f64,
}

fn default_text_size() -> Animated<f64> {
    Animated::new(48.0)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TextNode {
    pub text: String,
    /// Em size in document units. The one animatable text property (strings
    /// aren't tweenable, so content/align/font are whole-field structural).
    #[serde(default = "default_text_size")]
    pub size: Animated<f64>,
    #[serde(default)]
    pub align: TextAlign,
    /// Reserved for document-embedded fonts; `None` = bundled default.
    #[serde(default)]
    pub font: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MaskProps {
    pub inverted: bool,

    /// The actual vector geometry of the mask.
    ///
    /// Defaults to an empty path so legacy documents deserialize safely.
    #[serde(default)]
    pub shape: ShapeKind,
}

impl Default for ShapeKind {
    fn default() -> Self {
        ShapeKind::Path(Animated::new(renamite_geometry::VectorPath::default()))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Color {
    pub const BLACK: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };
    pub const WHITE: Self = Self {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    };
    pub fn rgba(r: f64, g: f64, b: f64, a: f64) -> Self {
        Self { r, g, b, a }
    }
}

impl Tween for Color {
    fn tween(a: &Self, b: &Self, t: f64) -> Self {
        Self {
            r: a.r + (b.r - a.r) * t,
            g: a.g + (b.g - a.g) * t,
            b: a.b + (b.b - a.b) * t,
            a: a.a + (b.a - a.a) * t,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FillRule {
    #[default]
    NonZero,
    EvenOdd,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StrokeCap {
    Butt,
    Round,
    Square,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StrokeJoin {
    Miter,
    Round,
    Bevel,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnimatedDash {
    pub dashes: Vec<Animated<f64>>,
    pub offset: Animated<f64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StarKind {
    Star,
    Burst,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlendMode {
    Normal,
    Multiply,
    Screen,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Asset {
    Image(ImageAsset),
    Font(FontAsset),
}

fn default_true() -> bool {
    true
}

/// An embedded image asset. Stores the original encoded PNG/JPEG/WebP bytes
/// and the decoded pixel dimensions established at import time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImageAsset {
    pub name: String,
    pub mime: String,

    /// Original encoded PNG/JPEG/WebP bytes.
    pub bytes: Vec<u8>,

    /// Decoded pixel dimensions, established during import.
    pub width: u32,
    pub height: u32,

    /// Decode/upload using an sRGB texture.
    #[serde(default = "default_true")]
    pub srgb: bool,
}

/// A project font: the user-visible name, the logical family key text nodes
/// reference, and the raw TTF/OTF bytes (saved/loaded inside the project).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FontAsset {
    /// Display name in the UI (e.g. "Inter-Regular.ttf").
    pub name: String,
    /// Logical family key that `TextNode.font` references.
    pub family: String,
    /// Raw TTF/OTF bytes.
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    pub items: Vec<SceneItem>,
    pub clips: Vec<ClipPath>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SceneItem {
    /// World space, transforms folded in.
    pub path: BezPath,
    /// The *shape* node that produced this geometry (used for picking).
    pub node: NodeId,
    /// The style node (Fill/Stroke) whose paint produced this item. Lets the
    /// gradient tool / inspector target the exact style to edit.
    pub style: NodeId,
    /// Resolved paint. Gradient coordinates are in world space (the owning
    /// shape's local transform is folded in during evaluation), matching the
    /// vertex positions the renderer bakes colors from.
    pub paint: ScenePaint,
    pub kind: PaintKind,
    pub opacity: f64,

    /// Clip stack applied to this item, outermost → innermost.
    #[serde(default)]
    pub clips: Vec<u32>,

    pub blend: BlendMode,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PaintKind {
    Fill(FillRule),
    Stroke(StrokeSample),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrokeSample {
    pub width: f64,
    pub cap: StrokeCap,
    pub join: StrokeJoin,
    pub dash: Option<DashSample>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DashSample {
    pub dashes: Vec<f64>,
    pub offset: f64,
}

/// Resolved, per-frame paint attached to a scene item. The renderer bakes
/// this into mesh vertex colors at tessellation time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ScenePaint {
    Solid(Color),
    LinearGradient {
        start: glam::DVec2,
        end: glam::DVec2,
        stops: GradientStops,
    },
    RadialGradient {
        center: glam::DVec2,
        end: glam::DVec2,
        stops: GradientStops,
    },
    Image {
        asset: AssetId,

        /// Local image rectangle dimensions.
        width: u32,
        height: u32,

        /// Full local-image → world affine.
        affine: [f64; 6],

        /// Multiplicative tint. WHITE means unchanged.
        tint: Color,
    },
}

impl ScenePaint {
    /// Color at world-space position `p` (used by vertex baking).
    pub fn color_at(&self, p: glam::DVec2) -> Color {
        match self {
            ScenePaint::Solid(c) => *c,
            ScenePaint::Image { tint, .. } => *tint,
            ScenePaint::LinearGradient { start, end, stops } => {
                let d = *end - *start;
                let len2 = d.length_squared().max(1e-12);
                let t = ((p - *start).dot(d) / len2).clamp(0.0, 1.0);
                stops.sample(t)
            }
            ScenePaint::RadialGradient { center, end, stops } => {
                let r = (*end - *center).length().max(1e-12);
                let t = ((p - *center).length() / r).clamp(0.0, 1.0);
                stops.sample(t)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClipPath {
    pub path: BezPath,
    #[serde(default)]
    pub rule: FillRule,
}

/// Per-frame property patch. Produced by clip/state-machine playback,
/// consumed by `evaluate_with`. Never touches the document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Overrides {
    pub values: std::collections::HashMap<(NodeId, PropPath), Value>,
}

impl Overrides {
    pub fn set(&mut self, id: NodeId, prop: PropPath, v: Value) {
        self.values.insert((id, prop), v);
    }
    /// TODO(perf): intern PropPath (u16 ids) to kill this per-lookup alloc.
    pub fn get(&self, id: NodeId, prop: &str) -> Option<&Value> {
        self.values.get(&(id, PropPath::new(prop)))
    }
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
    pub fn clear(&mut self) {
        self.values.clear();
    }
}

fn ov_f64(ov: &Overrides, id: NodeId, prop: &str, dflt: f64) -> f64 {
    match ov.get(id, prop) {
        Some(Value::F64(x)) => *x,
        _ => dflt,
    }
}
fn ov_vec2(ov: &Overrides, id: NodeId, prop: &str, dflt: glam::DVec2) -> glam::DVec2 {
    match ov.get(id, prop) {
        Some(Value::DVec2(x)) => *x,
        _ => dflt,
    }
}
fn ov_angle(ov: &Overrides, id: NodeId, prop: &str, dflt: f64) -> f64 {
    match ov.get(id, prop) {
        Some(Value::Angle(a)) => a.0,
        Some(Value::F64(x)) => *x,
        _ => dflt,
    }
}
fn ov_color(ov: &Overrides, id: NodeId, prop: &str, dflt: Color) -> Color {
    match ov.get(id, prop) {
        Some(Value::Color(c)) => *c,
        _ => dflt,
    }
}

fn sample_transform(
    n: &Node,
    id: NodeId,
    frame: f64,
    ov: &Overrides,
) -> renamite_animation::TransformSample {
    let mut ts = n.transform.sample(frame);
    if !ov.is_empty() {
        ts.anchor = ov_vec2(ov, id, "transform.anchor", ts.anchor);
        ts.position = ov_vec2(ov, id, "transform.position", ts.position);
        ts.scale = ov_vec2(ov, id, "transform.scale", ts.scale);
        ts.rotation_deg = ov_angle(ov, id, "transform.rotation", ts.rotation_deg);
        ts.skew = ov_f64(ov, id, "transform.skew", ts.skew);
        ts.skew_axis = ov_f64(ov, id, "transform.skew_axis", ts.skew_axis);
    }
    ts
}

/// The topmost fill style that paints `shape`: the last Fill style sibling
/// in the closest ancestor scope that has one. Used by the inspector to edit
/// a shape's fill (the tool instead tracks the exact style via `SceneItem`).
pub fn fill_style_for(doc: &Document, shape: NodeId) -> Option<NodeId> {
    let mut scope = doc.locate(shape).map(|(p, _)| p)?;
    loop {
        let children: Vec<NodeId> = match scope {
            Parent::Comp(c) => doc.compositions.get(c)?.children.clone(),
            Parent::Node(p) => doc.nodes.get(p)?.children.clone(),
        };
        if let Some(fill) = children.iter().rev().find(|id| {
            matches!(
                doc.nodes.get(**id).map(|n| &n.kind),
                Some(NodeKind::Style(StyleKind::Fill { .. }))
            )
        }) {
            return Some(*fill);
        }
        match scope {
            Parent::Comp(_) => return None,
            Parent::Node(p) => scope = doc.locate(p).map(|(parent, _)| parent)?,
        }
    }
}

/// The node's own transform as an affine, ignoring group/accumulated
/// transforms. Gradient handles are authored in this space and folded into
/// world space with this same affine during evaluation, so the inverse maps
/// world gradient handles back to local coordinates for editing.
pub fn node_affine(doc: &Document, id: NodeId, frame: f64) -> Affine {
    let Some(n) = doc.nodes.get(id) else {
        return Affine::IDENTITY;
    };
    affine_of(&sample_transform(n, id, frame, &Overrides::default()))
}

fn linear_affine_of(sample: &renamite_animation::TransformSample) -> Affine {
    let axis = sample.skew_axis.to_radians();
    let skew = Affine::rotate(axis)
        * Affine::skew(sample.skew.to_radians().tan(), 0.0)
        * Affine::rotate(-axis);
    Affine::rotate(sample.rotation_deg.to_radians())
        * skew
        * Affine::scale_non_uniform(sample.scale.x / 100.0, sample.scale.y / 100.0)
}

fn affine_of(sample: &renamite_animation::TransformSample) -> Affine {
    Affine::translate((sample.position.x, sample.position.y))
        * linear_affine_of(sample)
        * Affine::translate((-sample.anchor.x, -sample.anchor.y))
}

/// Resolved transform information for one node at an editor frame.
#[derive(Clone, Copy, Debug)]
pub struct NodeTransformContext {
    /// Transform from the node's parent coordinate space into world space.
    pub parent_world: Affine,

    /// Linear part of this node's transform: rotation * skew * scale.
    /// Does not contain position or anchor translations.
    pub linear: Affine,

    /// Full node-local to parent transform.
    pub local: Affine,

    /// Full node-local to world transform.
    pub world: Affine,

    /// Effective frame after ancestor layer time-stretch mappings.
    pub frame: f64,

    /// Node position in parent coordinates.
    pub position: glam::DVec2,

    /// Node anchor/pivot in local coordinates.
    pub anchor: glam::DVec2,

    /// Pivot location in world coordinates.
    pub pivot_world: glam::DVec2,
}

fn node_effective_frame(node: &Node, incoming_frame: f64) -> f64 {
    match &node.kind {
        NodeKind::Layer(layer) => {
            (incoming_frame - layer.in_frame.0 as f64) / layer.time_stretch.max(1e-9)
                + layer.in_frame.0 as f64
        }

        _ => incoming_frame,
    }
}

/// Resolve one attached node's parent/world transforms.
///
/// This follows the node's actual parent chain and applies the same Layer
/// time-stretch convention used by the evaluator. It intentionally does not
/// traverse through `Precomp` references because precomposition contents live
/// in a separate composition tree.
pub fn node_transform_context(
    doc: &Document,
    id: NodeId,
    root_frame: f64,
) -> Option<NodeTransformContext> {
    let mut chain = Vec::new();
    let mut current = id;

    loop {
        chain.push(current);

        let node = doc.nodes.get(current)?;

        let Some(parent) = node.parent else {
            break;
        };

        current = parent;
    }

    chain.reverse();

    let mut parent_world = Affine::IDENTITY;
    let mut frame = root_frame;

    for current in chain {
        let node = doc.nodes.get(current)?;
        let effective = node_effective_frame(node, frame);
        let sample = node.transform.sample(effective);
        let linear = linear_affine_of(&sample);
        let local = affine_of(&sample);

        if current == id {
            let pivot = parent_world * Point::new(sample.position.x, sample.position.y);

            return Some(NodeTransformContext {
                parent_world,
                linear,
                local,
                world: parent_world * local,
                frame: effective,
                position: sample.position,
                anchor: sample.anchor,
                pivot_world: glam::DVec2::new(pivot.x, pivot.y),
            });
        }

        parent_world *= local;
        frame = effective;
    }

    None
}

const SHAPE_TOL: f64 = 0.1;

/// The vector outline of a shape (or mask shape) node's geometry in its own
/// local coordinate space at `frame`, honoring any `shape.*` overrides.
pub fn shape_path(kind: &ShapeKind, id: NodeId, frame: f64, ov: &Overrides) -> BezPath {
    match kind {
        ShapeKind::Path(p) => {
            if let Some(Value::Path(p)) = ov.get(id, "shape.path") {
                return p.to_bez_path();
            }
            p.value_at(frame).to_bez_path()
        }
        ShapeKind::Rect { pos, size, rounded } => {
            let c = ov_vec2(ov, id, "shape.pos", pos.value_at(frame));
            let s = ov_vec2(ov, id, "shape.size", size.value_at(frame));
            let r = kurbo::Rect::from_center_size((c.x, c.y), (s.x.abs(), s.y.abs()));
            let radius = ov_f64(ov, id, "shape.rounded", rounded.value_at(frame));
            if radius > 1e-9 {
                kurbo::RoundedRect::from_rect(r, radius).to_path(SHAPE_TOL)
            } else {
                r.to_path(SHAPE_TOL)
            }
        }
        ShapeKind::Ellipse { pos, size } => {
            let c = ov_vec2(ov, id, "shape.pos", pos.value_at(frame));
            let s = ov_vec2(ov, id, "shape.size", size.value_at(frame));
            kurbo::Ellipse::new((c.x, c.y), (s.x.abs() / 2.0, s.y.abs() / 2.0), 0.0)
                .to_path(SHAPE_TOL)
        }
        ShapeKind::Star {
            pos,
            points,
            inner_r,
            outer_r,
            ..
        } => star_path(
            ov_vec2(ov, id, "shape.pos", pos.value_at(frame)),
            ov_f64(ov, id, "shape.points", points.value_at(frame))
                .round()
                .max(3.0) as usize,
            Some(ov_f64(ov, id, "shape.inner_r", inner_r.value_at(frame))),
            ov_f64(ov, id, "shape.outer_r", outer_r.value_at(frame)),
        ),
        ShapeKind::Polygon {
            pos,
            points,
            outer_r,
            ..
        } => star_path(
            ov_vec2(ov, id, "shape.pos", pos.value_at(frame)),
            ov_f64(ov, id, "shape.points", points.value_at(frame))
                .round()
                .max(3.0) as usize,
            None,
            ov_f64(ov, id, "shape.outer_r", outer_r.value_at(frame)),
        ),
        ShapeKind::CompoundPath(compound) => compound.to_bez_path(frame),
    }
}

/// Straight-edged star/polygon. Roundness: TODO (v0.4, with RoundCorners).
fn star_path(center: glam::DVec2, points: usize, inner: Option<f64>, outer: f64) -> BezPath {
    let mut p = BezPath::new();
    let n = if inner.is_some() { points * 2 } else { points };
    for k in 0..n {
        let ang = -std::f64::consts::FRAC_PI_2 + std::f64::consts::TAU * k as f64 / n as f64;
        let r = match inner {
            Some(ir) if k % 2 == 1 => ir,
            _ => outer,
        };
        let v = Point::new(center.x + r * ang.cos(), center.y + r * ang.sin());
        if k == 0 { p.move_to(v) } else { p.line_to(v) }
    }
    p.close_path();
    p
}

pub fn evaluate(doc: &Document, comp: CompId, frame: f64) -> Scene {
    evaluate_with(doc, comp, frame, &Overrides::default())
}

fn mask_shape_path(shape: &ShapeKind, id: NodeId, frame: f64, ov: &Overrides) -> BezPath {
    shape_path(shape, id, frame, ov)
}

fn inverted_clip_path(scope_world: &BezPath, mask_world: &BezPath) -> ClipPath {
    let mut path = scope_world.clone();
    path.extend(mask_world.clone());
    ClipPath {
        path,
        rule: FillRule::EvenOdd,
    }
}

pub fn evaluate_with(doc: &Document, comp: CompId, frame: f64, ov: &Overrides) -> Scene {
    let mut scene = Scene::default();
    if let Some(c) = doc.compositions.get(comp) {
        let scope = kurbo::Rect::new(0.0, 0.0, c.size.0 as f64, c.size.1 as f64);
        eval_group(
            doc,
            &c.children,
            frame,
            Affine::IDENTITY,
            1.0,
            &mut scene,
            0,
            ov,
            scope,
            &[],
            &[],
        );
    }
    scene
}

const MAX_DEPTH: u32 = 32; // precomp cycle guard

fn eval_group(
    doc: &Document,
    children: &[NodeId],
    frame: f64,
    tf: Affine,
    opacity: f64,
    scene: &mut Scene,
    depth: u32,
    ov: &Overrides,
    scope_rect: kurbo::Rect,
    inherited_clips: &[u32],
    seed_paths: &[ShapeEntry],
) {
    if depth > MAX_DEPTH {
        return;
    }

    // Pass 1: accumulate shape paths + modifiers, in document order.
    let mut paths: Vec<ShapeEntry> = seed_paths.to_vec();
    for &id in children {
        let Some(n) = doc.nodes.get(id) else { continue };
        if !n.visible {
            continue;
        }
        match &n.kind {
            NodeKind::Shape(s) => {
                let ntf = affine_of(&sample_transform(n, id, frame, ov));
                paths.push(ShapeEntry {
                    node: id,
                    affine: ntf,
                    opacity: 1.0,
                    path: tf * ntf * shape_path(s, id, frame, ov),
                });
            }
            NodeKind::Text(t) => {
                let ntf = affine_of(&sample_transform(n, id, frame, ov));
                let size = ov_f64(ov, id, "text.size", t.size.value_at(frame)).max(0.1);
                // Prefer an embedded project font by family.
                let outline = if let Some((_, font)) =
                    t.font.as_deref().and_then(|f| doc.font_asset_for_family(f))
                {
                    renamite_text::shape_text_from_bytes(&font.bytes, &t.text, size, t.align)
                        .unwrap_or_else(|_| {
                            renamite_text::shape_text_default(&t.text, size, t.align)
                        })
                } else {
                    renamite_text::shape_text_default(&t.text, size, t.align)
                };
                paths.push(ShapeEntry {
                    node: id,
                    affine: ntf,
                    opacity: 1.0,
                    path: tf * ntf * outline,
                });
            }
            NodeKind::Modifier(m) => apply_modifier(m, id, frame, ov, &mut paths),
            NodeKind::Mask(_) => {}
            _ => {}
        }
    }

    // Pass 2: resolve the clip stack for every sibling.
    let mut active: Vec<Vec<u32>> = Vec::with_capacity(children.len());
    let mut acc = inherited_clips.to_vec();
    for &id in children {
        active.push(acc.clone());
        let Some(n) = doc.nodes.get(id) else { continue };
        if !n.visible {
            continue;
        }
        if let NodeKind::Mask(mask) = &n.kind {
            let local = affine_of(&sample_transform(n, id, frame, ov));
            let world_mask = tf * local * mask_shape_path(&mask.shape, id, frame, ov);
            let clip = if mask.inverted {
                inverted_clip_path(&(tf * scope_rect.to_path(0.1)), &world_mask)
            } else {
                ClipPath {
                    path: world_mask,
                    rule: FillRule::NonZero,
                }
            };
            scene.clips.push(clip);
            acc.push((scene.clips.len() - 1) as u32);
        }
    }

    // Pass 3: emit bottom-first (painter's order), carrying each child's
    // active clip stack down into recursion and style emission.
    for (i, &id) in children.iter().enumerate().rev() {
        let Some(n) = doc.nodes.get(id) else { continue };
        if !n.visible {
            continue;
        }
        let node_op =
            opacity * ov_f64(ov, id, "opacity", n.opacity.value_at(frame)).clamp(0.0, 1.0);
        let clips = &active[i];
        match &n.kind {
            NodeKind::Mask(_) => {}
            NodeKind::Group => {
                let ntf = tf * affine_of(&sample_transform(n, id, frame, ov));
                eval_group(
                    doc,
                    &n.children,
                    frame,
                    ntf,
                    node_op,
                    scene,
                    depth + 1,
                    ov,
                    scope_rect,
                    clips,
                    &[],
                );
            }
            NodeKind::Layer(lp) => {
                if frame < lp.in_frame.0 as f64 || frame > lp.out_frame.0 as f64 {
                    continue;
                }
                let lf = (frame - lp.in_frame.0 as f64) / lp.time_stretch.max(1e-9)
                    + lp.in_frame.0 as f64;
                let ntf = tf * affine_of(&sample_transform(n, id, lf, ov));
                eval_group(
                    doc,
                    &n.children,
                    lf,
                    ntf,
                    node_op,
                    scene,
                    depth + 1,
                    ov,
                    scope_rect,
                    clips,
                    &[],
                );
            }
            NodeKind::Image(asset_id) => {
                let Some(asset) = doc.image_asset(*asset_id) else {
                    continue;
                };

                let node_transform = affine_of(&sample_transform(n, id, frame, ov));
                let full_transform = tf * node_transform;

                let local_rect =
                    kurbo::Rect::new(0.0, 0.0, asset.width as f64, asset.height as f64);

                let world_path = full_transform * local_rect.to_path(0.1);

                scene.items.push(SceneItem {
                    path: world_path,
                    node: id,
                    style: id,
                    paint: ScenePaint::Image {
                        asset: *asset_id,
                        width: asset.width,
                        height: asset.height,
                        affine: full_transform.as_coeffs(),
                        tint: Color::WHITE,
                    },
                    kind: PaintKind::Fill(FillRule::NonZero),
                    opacity: node_op,
                    clips: clips.to_vec(),
                    blend: BlendMode::Normal,
                });
            }
            NodeKind::Precomp { comp, time_map } => {
                let ntf = tf * affine_of(&sample_transform(n, id, frame, ov));
                let cf = (frame - time_map.offset.0 as f64) / time_map.stretch.max(1e-9);
                if let Some(c) = doc.compositions.get(*comp) {
                    let pre_scope = kurbo::Rect::new(0.0, 0.0, c.size.0 as f64, c.size.1 as f64);
                    eval_group(
                        doc,
                        &c.children,
                        cf,
                        ntf,
                        node_op,
                        scene,
                        depth + 1,
                        ov,
                        pre_scope,
                        clips,
                        &[],
                    );
                }
            }
            NodeKind::Style(st) => emit_style(st, id, frame, ov, &paths, node_op, clips, scene),
            NodeKind::Shape(_) | NodeKind::Text(_) if !n.children.is_empty() => {
                let seeds: Vec<ShapeEntry> =
                    paths.iter().filter(|e| e.node == id).cloned().collect();
                eval_group(
                    doc,
                    &n.children,
                    frame,
                    tf,
                    node_op,
                    scene,
                    depth + 1,
                    ov,
                    scope_rect,
                    clips,
                    &seeds,
                );
            }
            _ => {}
        }
    }
}

/// One accumulated shape path in pass 1 of group evaluation, carrying the
/// shape's local affine (gradient folding) and a per-copy opacity factor
/// (repeater falloff) that rides along until style emission.
#[derive(Clone)]
struct ShapeEntry {
    node: NodeId,
    affine: Affine,
    opacity: f64,
    path: BezPath,
}

fn apply_modifier(
    m: &ModifierKind,
    id: NodeId,
    frame: f64,
    ov: &Overrides,
    paths: &mut Vec<ShapeEntry>,
) {
    match m {
        ModifierKind::Repeater {
            copies,
            offset,
            transform,
            start_opacity,
            end_opacity,
        } => {
            let count = ov_f64(ov, id, "repeater.copies", copies.value_at(frame))
                .round()
                .max(0.0) as usize;
            let off = ov_f64(ov, id, "repeater.offset", offset.value_at(frame));
            let so = ov_f64(
                ov,
                id,
                "repeater.start_opacity",
                start_opacity.value_at(frame),
            )
            .clamp(0.0, 1.0);
            let eo =
                ov_f64(ov, id, "repeater.end_opacity", end_opacity.value_at(frame)).clamp(0.0, 1.0);
            let mut ts = transform.sample(frame);
            ts.position = ov_vec2(ov, id, "repeater.transform.position", ts.position);
            ts.scale = ov_vec2(ov, id, "repeater.transform.scale", ts.scale);
            ts.rotation_deg = ov_angle(ov, id, "repeater.transform.rotation", ts.rotation_deg);
            ts.anchor = ov_vec2(ov, id, "repeater.transform.anchor", ts.anchor);
            ts.skew = ov_f64(ov, id, "repeater.transform.skew", ts.skew);
            ts.skew_axis = ov_f64(ov, id, "repeater.transform.skew_axis", ts.skew_axis);
            let step = affine_of(&ts);
            let original = std::mem::take(paths);
            let n = count.max(1);
            for i in 0..n {
                // Linear falloff: first copy = so, last copy = eo.
                let t = if n <= 1 {
                    0.0
                } else {
                    i as f64 / (n - 1) as f64
                };
                let copy_opacity = so + (eo - so) * t;

                let mut a = Affine::IDENTITY;
                let reps = (i as f64 + off).max(0.0) as usize;
                for _ in 0..reps {
                    a *= step;
                }
                for e in &original {
                    paths.push(ShapeEntry {
                        node: e.node,
                        affine: e.affine,
                        opacity: e.opacity * copy_opacity,
                        path: a * e.path.clone(),
                    });
                }
            }
        }
        ModifierKind::TrimPath {
            start,
            end,
            offset,
            mode,
        } => {
            let mut s = ov_f64(ov, id, "trim.start", start.value_at(frame)).clamp(0.0, 1.0);
            let mut e = ov_f64(ov, id, "trim.end", end.value_at(frame)).clamp(0.0, 1.0);
            if s > e {
                std::mem::swap(&mut s, &mut e);
            }
            let o = ov_f64(ov, id, "trim.offset", offset.value_at(frame)).rem_euclid(1.0);

            if (e - s).abs() < 1e-9 {
                paths.clear();
                return;
            }

            let originals = std::mem::take(paths);
            match mode {
                TrimMode::Individually => {
                    for entry in originals {
                        if let Some(trimmed) = trim_path(&entry.path, s, e, o) {
                            paths.push(ShapeEntry {
                                path: trimmed,
                                ..entry
                            });
                        }
                    }
                }
                TrimMode::Simultaneously => {
                    let lengths: Vec<f64> = originals
                        .iter()
                        .map(|entry| entry.path.perimeter(1e-3))
                        .collect();
                    let total: f64 = lengths.iter().sum();
                    if total <= 1e-9 {
                        return;
                    }
                    let mut cursor = 0.0;
                    for (entry, len) in originals.into_iter().zip(lengths) {
                        let frac = len / total;
                        if frac > 1e-12 {
                            let ps = ((s - cursor) / frac).clamp(0.0, 1.0);
                            let pe = ((e - cursor) / frac).clamp(0.0, 1.0);
                            if pe > ps + 1e-9
                                && let Some(t) = trim_path(&entry.path, ps, pe, o)
                            {
                                paths.push(ShapeEntry { path: t, ..entry });
                            }
                        }
                        cursor += frac;
                    }
                }
            }
        }
        ModifierKind::RoundCorners { radius } => {
            let r = ov_f64(ov, id, "round.radius", radius.value_at(frame)).max(0.0);
            if r > 1e-9 {
                // RoundCorners needs anchor-level data, so round-trip each
                // flattened path back through `VectorPath` before rounding.
                // `from_bez_path` re-detects tangent modes from the flattened
                // geometry: already-curved (Smooth) paths pass through untouched,
                // while hard cuts (e.g. from a preceding Trim) detect as Corner
                // and get rounded - Lottie modifier-order semantics.
                for entry in paths.iter_mut() {
                    let vp = renamite_geometry::VectorPath::from_bez_path(&entry.path);
                    entry.path = vp.round_corners(r).to_bez_path();
                }
            }
        }
        ModifierKind::OffsetPath { amount } => {
            let amount = ov_f64(ov, id, "offset.amount", amount.value_at(frame));
            if amount.abs() > 1e-9 {
                for entry in paths.iter_mut() {
                    if let Some(offset) = offset_bez_path(&entry.path, amount, SHAPE_TOL) {
                        entry.path = offset;
                    }
                }
            }
        }
        ModifierKind::ZigZag {
            amplitude,
            frequency,
            smooth,
        } => {
            let amp = ov_f64(ov, id, "zigzag.amplitude", amplitude.value_at(frame));
            let freq = ov_f64(ov, id, "zigzag.frequency", frequency.value_at(frame));
            if amp.abs() > 1e-9 && freq.abs() > 1e-9 {
                for entry in paths.iter_mut() {
                    entry.path = renamite_geometry::zigzag_path(&entry.path, amp, freq, *smooth);
                }
            }
        }
        ModifierKind::PuckerBloat { amount } => {
            let amt = ov_f64(ov, id, "pucker.amount", amount.value_at(frame));
            if amt.abs() > 1e-9 {
                for entry in paths.iter_mut() {
                    let vp = renamite_geometry::VectorPath::from_bez_path(&entry.path);
                    entry.path =
                        renamite_geometry::pucker_bloat_vector_path(&vp, amt).to_bez_path();
                }
            }
        }
    }
}

fn trim_path(path: &BezPath, s: f64, e: f64, offset: f64) -> Option<BezPath> {
    use kurbo::ParamCurveArclen;

    if (e - s).abs() < 1e-9 {
        return None;
    }

    let segments: Vec<kurbo::PathSeg> = path.segments().collect();
    if segments.is_empty() {
        return None;
    }
    let lengths: Vec<f64> = segments.iter().map(|seg| seg.arclen(1e-3)).collect();
    let total: f64 = lengths.iter().sum();
    if total <= 1e-9 {
        return None;
    }

    let s_offset = s + offset;
    let e_offset = e + offset;
    // Wrap decision in the PRE-modulo domain: the interval touches/crosses 1.0.
    // (Comparing the rem_euclid'd `a` vs `b` is unreliable near the seam where
    // FP noise flips `<` and silently empties the span.)
    let wraps = s_offset < 1.0 && e_offset >= 1.0;
    let a = s_offset.rem_euclid(1.0);
    let b = e_offset.rem_euclid(1.0);

    let mut out = BezPath::new();
    let mut last_end: Option<Point> = None;
    if wraps {
        // Wraps: [a, 1] then [0, b]. (a == b arises when e - s covers the
        // whole domain after offset; both halves together emit the full path.)
        emit_range(&segments, &lengths, total, a, 1.0, &mut out, &mut last_end);
        emit_range(&segments, &lengths, total, 0.0, b, &mut out, &mut last_end);
    } else {
        emit_range(&segments, &lengths, total, a, b, &mut out, &mut last_end);
    }

    if out.elements().is_empty() {
        None
    } else {
        Some(out)
    }
}

fn emit_range(
    segments: &[kurbo::PathSeg],
    lengths: &[f64],
    total: f64,
    a: f64,
    b: f64,
    out: &mut BezPath,
    last_end: &mut Option<Point>,
) {
    use kurbo::ParamCurve;

    let a_len = a * total;
    let b_len = b * total;
    let mut cursor = 0.0;

    for (seg, &len) in segments.iter().zip(lengths) {
        let seg_start = cursor;
        let seg_end = cursor + len;
        cursor = seg_end;

        if seg_end <= a_len {
            continue;
        }
        if seg_start >= b_len {
            break;
        }

        let t0 = if seg_start < a_len {
            arclen_to_t(seg, a_len - seg_start)
        } else {
            0.0
        };
        let t1 = if seg_end > b_len {
            arclen_to_t(seg, b_len - seg_start)
        } else {
            1.0
        };
        if t1 <= t0 + 1e-9 {
            continue;
        }

        let sub = seg.subsegment(t0..t1);
        let start_pt = sub.start();
        // Continuity check: new subpath (MoveTo break / wrap seam) → move_to.
        let connected = last_end
            .map(|p| (p - start_pt).hypot() < 1e-6)
            .unwrap_or(false);
        if !connected {
            out.move_to(start_pt);
        }
        append_seg(out, &sub);
        *last_end = Some(sub.end());
    }
}

fn arclen_to_t(seg: &kurbo::PathSeg, target: f64) -> f64 {
    use kurbo::{ParamCurve, ParamCurveArclen};
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    for _ in 0..24 {
        let mid = 0.5 * (lo + hi);
        if seg.subsegment(0.0..mid).arclen(1e-3) < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

fn append_seg(out: &mut BezPath, seg: &kurbo::PathSeg) {
    match seg {
        kurbo::PathSeg::Line(l) => out.line_to(l.p1),
        kurbo::PathSeg::Quad(q) => out.quad_to(q.p1, q.p2),
        kurbo::PathSeg::Cubic(c) => out.curve_to(c.p1, c.p2, c.p3),
    }
}

fn fold_gradient_point(affine: &Affine, local: glam::DVec2) -> glam::DVec2 {
    let p = *affine * Point::new(local.x, local.y);
    glam::DVec2::new(p.x, p.y)
}

fn emit_style(
    st: &StyleKind,
    style_id: NodeId,
    frame: f64,
    ov: &Overrides,
    paths: &[ShapeEntry],
    opacity: f64,
    active_clips: &[u32],
    scene: &mut Scene,
) {
    for e in paths {
        let (paint, kind, is_stroke) = match st {
            StyleKind::Fill { paint, rule } => (paint, PaintKind::Fill(*rule), false),
            StyleKind::Stroke {
                paint,
                width,
                cap,
                join,
                dash,
            } => (
                paint,
                PaintKind::Stroke(StrokeSample {
                    width: ov_f64(ov, style_id, "stroke.width", width.value_at(frame)).max(0.0),
                    cap: *cap,
                    join: *join,
                    dash: dash.as_ref().map(|d| DashSample {
                        dashes: d.dashes.iter().map(|x| x.value_at(frame)).collect(),
                        offset: d.offset.value_at(frame),
                    }),
                }),
                true,
            ),
        };

        let paint = sample_paint_world(paint, frame, &e.affine, ov, style_id, is_stroke);

        scene.items.push(SceneItem {
            path: e.path.clone(),
            node: e.node,
            style: style_id,
            paint,
            kind,
            opacity: opacity * e.opacity,
            clips: active_clips.to_vec(),
            blend: BlendMode::Normal,
        });
    }
}

fn sample_paint_world(
    paint: &StylePaint,
    frame: f64,
    affine: &Affine,
    ov: &Overrides,
    style_id: NodeId,
    is_stroke: bool,
) -> ScenePaint {
    match paint {
        StylePaint::Solid { color } => {
            let path = if is_stroke {
                "stroke.color"
            } else {
                "fill.color"
            };
            ScenePaint::Solid(ov_color(ov, style_id, path, color.value_at(frame)))
        }
        StylePaint::Gradient(g) => {
            let kind = g.kind;
            let start_local = ov_vec2(ov, style_id, "grad.start", g.start.value_at(frame));
            let end_local = ov_vec2(ov, style_id, "grad.end", g.end.value_at(frame));
            let stops = ov_stops(ov, style_id, "grad.stops", &g.stops.value_at(frame));
            match kind {
                GradientKind::Linear => ScenePaint::LinearGradient {
                    start: fold_gradient_point(affine, start_local),
                    end: fold_gradient_point(affine, end_local),
                    stops,
                },
                GradientKind::Radial => ScenePaint::RadialGradient {
                    center: fold_gradient_point(affine, start_local),
                    end: fold_gradient_point(affine, end_local),
                    stops,
                },
            }
        }
    }
}

fn ov_stops(ov: &Overrides, id: NodeId, prop: &str, dflt: &GradientStops) -> GradientStops {
    match ov.get(id, prop) {
        Some(Value::Stops(s)) => s.clone(),
        _ => dflt.clone(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Parent {
    Node(NodeId),
    Comp(CompId),
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum ModelError {
    #[error("node not found")]
    MissingNode,
    #[error("node kind mismatch (expected {0})")]
    WrongNodeKind(&'static str),
    #[error("composition not found")]
    MissingComp,
    #[error("no property at path {0}")]
    MissingProp(String),
    #[error("value type mismatch for {0}")]
    TypeMismatch(String),
    #[error("no keyframe at frame {0}")]
    NoKeyframe(i64),
    #[error("keyframe already exists at frame {0}")]
    KeyframeExists(i64),
    #[error("node is not attached")]
    NotAttached,
    #[error("asset not found")]
    MissingAsset,
}

impl Document {
    pub fn empty() -> Self {
        let mut compositions = CompMap::default();
        let main = compositions.insert(Composition {
            name: "Main".into(),
            size: (512, 512),
            rate: renamite_animation::FrameRate { num: 60, den: 1 },
            range: (Frame(0), Frame(180)),
            children: Vec::new(),
        });
        Self {
            format_version: 1,
            compositions,
            nodes: NodeMap::default(),
            assets: AssetMap::default(),
            asset_order: Vec::new(),
            main,
        }
    }

    pub fn create_node(&mut self, node: Node) -> NodeId {
        self.nodes.insert(node)
    }

    pub fn attach(&mut self, id: NodeId, parent: Parent, index: usize) -> Result<(), ModelError> {
        if !self.nodes.contains_key(id) {
            return Err(ModelError::MissingNode);
        }
        match parent {
            Parent::Node(p) => {
                let pn = self.nodes.get_mut(p).ok_or(ModelError::MissingNode)?;
                let i = index.min(pn.children.len());
                pn.children.insert(i, id);
                self.nodes[id].parent = Some(p);
            }
            Parent::Comp(c) => {
                let comp = self
                    .compositions
                    .get_mut(c)
                    .ok_or(ModelError::MissingComp)?;
                let i = index.min(comp.children.len());
                comp.children.insert(i, id);
                self.nodes[id].parent = None;
            }
        }
        Ok(())
    }

    pub fn detach(&mut self, id: NodeId) -> Result<(Parent, usize), ModelError> {
        let (parent, index) = self.locate(id).ok_or(ModelError::NotAttached)?;
        match parent {
            Parent::Node(p) => {
                self.nodes[p].children.remove(index);
            }
            Parent::Comp(c) => {
                self.compositions[c].children.remove(index);
            }
        }
        if let Some(n) = self.nodes.get_mut(id) {
            n.parent = None;
        }
        Ok((parent, index))
    }

    pub fn locate(&self, id: NodeId) -> Option<(Parent, usize)> {
        let n = self.nodes.get(id)?;
        if let Some(p) = n.parent {
            let i = self.nodes.get(p)?.children.iter().position(|&c| c == id)?;
            return Some((Parent::Node(p), i));
        }
        for (cid, comp) in &self.compositions {
            if let Some(i) = comp.children.iter().position(|&c| c == id) {
                return Some((Parent::Comp(cid), i));
            }
        }
        None
    }

    /// Drop arena nodes not reachable from any composition (call before save).
    pub fn garbage_collect(&mut self) {
        let mut live = std::collections::HashSet::new();
        fn mark(doc: &Document, id: NodeId, live: &mut std::collections::HashSet<NodeId>) {
            if !live.insert(id) {
                return;
            }
            if let Some(n) = doc.nodes.get(id) {
                for &c in &n.children {
                    mark(doc, c, live);
                }
            }
        }
        let roots: Vec<NodeId> = self
            .compositions
            .values()
            .flat_map(|c| c.children.clone())
            .collect();
        for r in roots {
            mark(self, r, &mut live);
        }
        self.nodes.retain(|id, _| live.contains(&id));

        // Retain attached or node-referenced assets.
        let mut live_assets: std::collections::HashSet<AssetId> =
            self.asset_order.iter().copied().collect();
        for node in self.nodes.values() {
            if let NodeKind::Image(asset) = node.kind {
                live_assets.insert(asset);
            }
        }
        self.assets.retain(|id, _| live_assets.contains(&id));
        self.asset_order.retain(|id| self.assets.contains_key(*id));
    }

    /// Rebuild `asset_order` to match the arena: every attached id must exist
    /// and be unique; any arena asset missing from the order gets appended.
    /// (Call after loading legacy projects that predate `asset_order`.)
    pub fn normalize_assets(&mut self) {
        let mut seen = std::collections::HashSet::new();

        self.asset_order
            .retain(|id| self.assets.contains_key(*id) && seen.insert(*id));

        for id in self.assets.keys() {
            if seen.insert(id) {
                self.asset_order.push(id);
            }
        }
    }

    /// The embedded image asset behind `id`, if it is an image.
    pub fn image_asset(&self, id: AssetId) -> Option<&ImageAsset> {
        match self.assets.get(id)? {
            Asset::Image(image) => Some(image),
            _ => None,
        }
    }

    /// Number of image-layer nodes referencing `asset`.
    pub fn image_usage_count(&self, asset: AssetId) -> usize {
        self.nodes
            .values()
            .filter(|node| matches!(node.kind, NodeKind::Image(id) if id == asset))
            .count()
    }

    /// The font asset whose family matches `family`, if the project has one.
    /// Surfaces the `AssetId` (for removal) alongside the asset.
    pub fn font_asset_for_family(&self, family: &str) -> Option<(AssetId, &FontAsset)> {
        self.assets.iter().find_map(|(id, asset)| match asset {
            Asset::Font(font) if font.family == family => Some((id, font)),
            _ => None,
        })
    }

    /// Sorted, deduplicated family keys of every font asset in the project.
    pub fn font_families(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .asset_order
            .iter()
            .filter_map(|id| match self.assets.get(*id) {
                Some(Asset::Font(font)) => Some(font.family.clone()),
                _ => None,
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PropPath(pub String);

impl PropPath {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Value {
    F64(f64),
    DVec2(glam::DVec2),
    Angle(Angle),
    Color(Color),
    Path(VectorPath),
    Bool(bool),
    I64(i64),
    /// Whole gradient stop list (animatable as one unit; v1).
    Stops(GradientStops),
    /// Whole style paint (structural swaps like solid<->gradient).
    Paint(StylePaint),
}

/// Topmost pickable item under `pt` (world space).
pub fn pick(scene: &Scene, pt: glam::DVec2) -> Option<NodeId> {
    let q = Point::new(pt.x, pt.y);
    for item in scene.items.iter().rev() {
        if item.opacity <= 0.0 {
            continue;
        }

        let dashed_path = match &item.kind {
            PaintKind::Stroke(stroke) => stroke
                .dash
                .as_ref()
                .and_then(|dash| dash_bez_path(&item.path, &dash.dashes, dash.offset)),
            PaintKind::Fill(_) => None,
        };

        let hit_path = dashed_path.as_ref().unwrap_or(&item.path);

        let padding = match &item.kind {
            PaintKind::Stroke(stroke) => (stroke.width * 0.5).max(1.0),
            PaintKind::Fill(_) => 0.0,
        };

        if !hit_path
            .bounding_box()
            .inflate(padding, padding)
            .contains(q)
        {
            continue;
        }
        let clips_ok = item.clips.iter().all(|&ci| {
            let Some(c) = scene.clips.get(ci as usize) else {
                return false; // dangling index: not pickable
            };
            match c.rule {
                FillRule::NonZero => c.path.winding(q) != 0,
                FillRule::EvenOdd => c.path.winding(q) % 2 != 0,
            }
        });
        if !clips_ok {
            continue;
        }
        let hit = match &item.kind {
            PaintKind::Fill(rule) => match rule {
                FillRule::NonZero => hit_path.winding(q) != 0,
                FillRule::EvenOdd => hit_path.winding(q) % 2 != 0,
            },
            PaintKind::Stroke(_) => nearest_dist(hit_path, q) <= padding,
        };
        if hit {
            return Some(item.node);
        }
    }
    None
}

fn nearest_dist(path: &BezPath, q: Point) -> f64 {
    let mut best = f64::MAX;
    for seg in path.segments() {
        best = best.min(seg.nearest(q, 1e-6).distance_sq);
    }
    best.sqrt()
}

/// Nodes whose geometry is FULLY CONTAINED in the box (rubber-band semantics).
pub fn pick_box(scene: &Scene, min: glam::DVec2, max: glam::DVec2) -> Vec<NodeId> {
    let mut out = Vec::new();
    for item in &scene.items {
        if item.opacity <= 0.0 {
            continue;
        }
        let bb = item.path.bounding_box();
        if bb.x0 >= min.x
            && bb.x1 <= max.x
            && bb.y0 >= min.y
            && bb.y1 <= max.y
            && !out.contains(&item.node)
        {
            out.push(item.node);
        }
    }
    out
}

/// Union bbox of all items belonging to `nodes` (selection bounds).
pub fn nodes_bounds(scene: &Scene, nodes: &[NodeId]) -> Option<(glam::DVec2, glam::DVec2)> {
    let mut acc: Option<kurbo::Rect> = None;
    for item in &scene.items {
        if !nodes.contains(&item.node) {
            continue;
        }
        let bb = item.path.bounding_box();
        acc = Some(acc.map_or(bb, |a| a.union(bb)));
    }
    acc.map(|r| (glam::DVec2::new(r.x0, r.y0), glam::DVec2::new(r.x1, r.y1)))
}

fn transform_vector(affine: Affine, value: glam::DVec2) -> glam::DVec2 {
    let [a, b, c, d, _, _] = affine.as_coeffs();

    glam::DVec2::new(a * value.x + c * value.y, b * value.x + d * value.y)
}

/// Convert a world-space drag delta to a node's parent coordinate system.
pub fn world_delta_to_parent(
    doc: &Document,
    id: NodeId,
    frame: f64,
    delta: glam::DVec2,
) -> Option<glam::DVec2> {
    let context = node_transform_context(doc, id, frame)?;
    let inverse = context.parent_world.inverse();

    let result = transform_vector(inverse, delta);

    result.is_finite().then_some(result)
}

pub fn node_is_ancestor(doc: &Document, ancestor: NodeId, mut node: NodeId) -> bool {
    while let Some(current) = doc.nodes.get(node) {
        let Some(parent) = current.parent else {
            return false;
        };

        if parent == ancestor {
            return true;
        }

        node = parent;
    }

    false
}

/// If `picked` belongs to an already-selected group/layer, return that selected
/// ancestor instead of replacing it with the leaf shape.
pub fn selected_ancestor_for_pick(
    doc: &Document,
    picked: NodeId,
    selection: &[NodeId],
) -> Option<NodeId> {
    selection
        .iter()
        .copied()
        .find(|selected| *selected == picked || node_is_ancestor(doc, *selected, picked))
}

/// Return the immediate child of `ancestor` that contains `descendant`.
pub fn immediate_child_below(
    doc: &Document,
    ancestor: NodeId,
    descendant: NodeId,
) -> Option<NodeId> {
    if ancestor == descendant {
        return None;
    }

    let mut current = descendant;

    loop {
        let parent = doc.nodes.get(current)?.parent?;

        if parent == ancestor {
            return Some(current);
        }

        current = parent;
    }
}

/// Union bounds of selected leaf nodes and all rendered descendants of selected
/// groups/layers.
pub fn selection_bounds(
    doc: &Document,
    scene: &Scene,
    selection: &[NodeId],
) -> Option<(glam::DVec2, glam::DVec2)> {
    let mut bounds: Option<kurbo::Rect> = None;

    for item in &scene.items {
        let included = selection
            .iter()
            .copied()
            .any(|selected| selected == item.node || node_is_ancestor(doc, selected, item.node));

        if !included {
            continue;
        }

        let item_bounds = item.path.bounding_box();

        bounds = Some(match bounds {
            Some(existing) => existing.union(item_bounds),
            None => item_bounds,
        });
    }

    bounds.map(|rect| {
        (
            glam::DVec2::new(rect.x0, rect.y0),
            glam::DVec2::new(rect.x1, rect.y1),
        )
    })
}

/// Serialized keyframe (for RestoreKeyframe / undo).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyframeData {
    pub frame: Frame,
    pub value: Value,
    pub interpolation: Interpolation,
    pub ease_out: EasingHandle,
    pub ease_in: EasingHandle,
}

pub trait PropValue: Tween + Clone {
    fn into_value(self) -> Value;
    fn from_value(v: &Value) -> Option<Self>;
}
impl PropValue for f64 {
    fn into_value(self) -> Value {
        Value::F64(self)
    }
    fn from_value(v: &Value) -> Option<Self> {
        if let Value::F64(x) = v {
            Some(*x)
        } else {
            None
        }
    }
}
impl PropValue for glam::DVec2 {
    fn into_value(self) -> Value {
        Value::DVec2(self)
    }
    fn from_value(v: &Value) -> Option<Self> {
        if let Value::DVec2(x) = v {
            Some(*x)
        } else {
            None
        }
    }
}
impl PropValue for Angle {
    fn into_value(self) -> Value {
        Value::Angle(self)
    }
    fn from_value(v: &Value) -> Option<Self> {
        match v {
            Value::Angle(a) => Some(*a),
            Value::F64(x) => Some(Angle(*x)),
            _ => None,
        }
    }
}
impl PropValue for Color {
    fn into_value(self) -> Value {
        Value::Color(self)
    }
    fn from_value(v: &Value) -> Option<Self> {
        if let Value::Color(c) = v {
            Some(*c)
        } else {
            None
        }
    }
}
impl PropValue for VectorPath {
    fn into_value(self) -> Value {
        Value::Path(self)
    }
    fn from_value(v: &Value) -> Option<Self> {
        if let Value::Path(p) = v {
            Some(p.clone())
        } else {
            None
        }
    }
}
impl PropValue for GradientStops {
    fn into_value(self) -> Value {
        Value::Stops(self)
    }
    fn from_value(v: &Value) -> Option<Self> {
        if let Value::Stops(s) = v {
            Some(s.clone())
        } else {
            None
        }
    }
}
impl PropValue for StylePaint {
    fn into_value(self) -> Value {
        Value::Paint(self)
    }
    fn from_value(v: &Value) -> Option<Self> {
        if let Value::Paint(p) = v {
            Some(p.clone())
        } else {
            None
        }
    }
}

pub enum PropMut<'a> {
    F64(&'a mut Animated<f64>),
    Vec2(&'a mut Animated<glam::DVec2>),
    Angle(&'a mut Animated<Angle>),
    Color(&'a mut Animated<Color>),
    Path(&'a mut Animated<VectorPath>),
    Stops(&'a mut Animated<GradientStops>),
}
pub enum PropRef<'a> {
    F64(&'a Animated<f64>),
    Vec2(&'a Animated<glam::DVec2>),
    Angle(&'a Animated<Angle>),
    Color(&'a Animated<Color>),
    Path(&'a Animated<VectorPath>),
    Stops(&'a Animated<GradientStops>),
}

pub trait PropVisitor {
    type Out;
    fn visit<T: PropValue>(self, a: &mut Animated<T>) -> Self::Out;
}
pub trait PropReader {
    type Out;
    fn read<T: PropValue>(self, a: &Animated<T>) -> Self::Out;
}

pub fn visit_prop<V: PropVisitor>(p: PropMut<'_>, v: V) -> V::Out {
    match p {
        PropMut::F64(a) => v.visit(a),
        PropMut::Vec2(a) => v.visit(a),
        PropMut::Angle(a) => v.visit(a),
        PropMut::Color(a) => v.visit(a),
        PropMut::Path(a) => v.visit(a),
        PropMut::Stops(a) => v.visit(a),
    }
}
pub fn read_prop<V: PropReader>(p: PropRef<'_>, v: V) -> V::Out {
    match p {
        PropRef::F64(a) => v.read(a),
        PropRef::Vec2(a) => v.read(a),
        PropRef::Angle(a) => v.read(a),
        PropRef::Color(a) => v.read(a),
        PropRef::Path(a) => v.read(a),
        PropRef::Stops(a) => v.read(a),
    }
}

fn dash_index(path: &str) -> Option<usize> {
    path.strip_prefix("stroke.dash.")?.parse().ok()
}

impl Node {
    pub fn prop_mut(&mut self, prop: &PropPath) -> Option<PropMut<'_>> {
        use PropMut::*;
        let s = prop.as_str();
        if s == "stroke.dash.offset" || dash_index(s).is_some() {
            if let NodeKind::Style(StyleKind::Stroke {
                dash: Some(dash), ..
            }) = &mut self.kind
            {
                if s == "stroke.dash.offset" {
                    return Some(F64(&mut dash.offset));
                }
                if let Some(index) = dash_index(s) {
                    return dash.dashes.get_mut(index).map(PropMut::F64);
                }
            }
            return None;
        }

        match (s, &mut self.kind) {
            ("opacity", _) => Some(F64(&mut self.opacity)),
            ("transform.anchor", _) => Some(Vec2(&mut self.transform.anchor)),
            ("transform.position", _) => Some(Vec2(&mut self.transform.position)),
            ("transform.scale", _) => Some(Vec2(&mut self.transform.scale)),
            ("transform.rotation", _) => Some(Angle(&mut self.transform.rotation)),
            ("transform.skew", _) => Some(F64(&mut self.transform.skew)),
            ("transform.skew_axis", _) => Some(F64(&mut self.transform.skew_axis)),
            ("shape.path", NodeKind::Shape(ShapeKind::Path(p))) => Some(Path(p)),
            ("shape.pos", NodeKind::Shape(ShapeKind::Rect { pos, .. }))
            | ("shape.pos", NodeKind::Shape(ShapeKind::Ellipse { pos, .. }))
            | ("shape.pos", NodeKind::Shape(ShapeKind::Star { pos, .. }))
            | ("shape.pos", NodeKind::Shape(ShapeKind::Polygon { pos, .. })) => Some(Vec2(pos)),
            ("shape.size", NodeKind::Shape(ShapeKind::Rect { size, .. }))
            | ("shape.size", NodeKind::Shape(ShapeKind::Ellipse { size, .. })) => Some(Vec2(size)),
            ("shape.rounded", NodeKind::Shape(ShapeKind::Rect { rounded, .. })) => {
                Some(F64(rounded))
            }
            ("shape.points", NodeKind::Shape(ShapeKind::Star { points, .. }))
            | ("shape.points", NodeKind::Shape(ShapeKind::Polygon { points, .. })) => {
                Some(F64(points))
            }
            ("shape.inner_r", NodeKind::Shape(ShapeKind::Star { inner_r, .. })) => {
                Some(F64(inner_r))
            }
            ("shape.outer_r", NodeKind::Shape(ShapeKind::Star { outer_r, .. }))
            | ("shape.outer_r", NodeKind::Shape(ShapeKind::Polygon { outer_r, .. })) => {
                Some(F64(outer_r))
            }
            ("shape.roundness", NodeKind::Shape(ShapeKind::Star { roundness, .. }))
            | ("shape.roundness", NodeKind::Shape(ShapeKind::Polygon { roundness, .. })) => {
                Some(F64(roundness))
            }
            ("text.size", NodeKind::Text(t)) => Some(F64(&mut t.size)),
            (
                "shape.path",
                NodeKind::Mask(MaskProps {
                    shape: ShapeKind::Path(p),
                    ..
                }),
            ) => Some(Path(p)),
            (
                "shape.pos",
                NodeKind::Mask(MaskProps {
                    shape: ShapeKind::Rect { pos, .. },
                    ..
                }),
            )
            | (
                "shape.pos",
                NodeKind::Mask(MaskProps {
                    shape: ShapeKind::Ellipse { pos, .. },
                    ..
                }),
            )
            | (
                "shape.pos",
                NodeKind::Mask(MaskProps {
                    shape: ShapeKind::Star { pos, .. },
                    ..
                }),
            )
            | (
                "shape.pos",
                NodeKind::Mask(MaskProps {
                    shape: ShapeKind::Polygon { pos, .. },
                    ..
                }),
            ) => Some(Vec2(pos)),
            (
                "shape.size",
                NodeKind::Mask(MaskProps {
                    shape: ShapeKind::Rect { size, .. },
                    ..
                }),
            )
            | (
                "shape.size",
                NodeKind::Mask(MaskProps {
                    shape: ShapeKind::Ellipse { size, .. },
                    ..
                }),
            ) => Some(Vec2(size)),
            (
                "shape.rounded",
                NodeKind::Mask(MaskProps {
                    shape: ShapeKind::Rect { rounded, .. },
                    ..
                }),
            ) => Some(F64(rounded)),
            (
                "shape.points",
                NodeKind::Mask(MaskProps {
                    shape: ShapeKind::Star { points, .. } | ShapeKind::Polygon { points, .. },
                    ..
                }),
            ) => Some(F64(points)),
            (
                "shape.inner_r",
                NodeKind::Mask(MaskProps {
                    shape: ShapeKind::Star { inner_r, .. },
                    ..
                }),
            ) => Some(F64(inner_r)),
            (
                "shape.outer_r",
                NodeKind::Mask(MaskProps {
                    shape: ShapeKind::Star { outer_r, .. } | ShapeKind::Polygon { outer_r, .. },
                    ..
                }),
            ) => Some(F64(outer_r)),
            (
                "shape.roundness",
                NodeKind::Mask(MaskProps {
                    shape: ShapeKind::Star { roundness, .. } | ShapeKind::Polygon { roundness, .. },
                    ..
                }),
            ) => Some(F64(roundness)),
            (
                "fill.color",
                NodeKind::Style(StyleKind::Fill {
                    paint: StylePaint::Solid { color },
                    ..
                }),
            ) => Some(Color(color)),
            (
                "stroke.color",
                NodeKind::Style(StyleKind::Stroke {
                    paint: StylePaint::Solid { color },
                    ..
                }),
            ) => Some(Color(color)),
            ("stroke.width", NodeKind::Style(StyleKind::Stroke { width, .. })) => Some(F64(width)),
            (
                "grad.start",
                NodeKind::Style(StyleKind::Fill {
                    paint: StylePaint::Gradient(g),
                    ..
                }),
            )
            | (
                "grad.start",
                NodeKind::Style(StyleKind::Stroke {
                    paint: StylePaint::Gradient(g),
                    ..
                }),
            ) => Some(Vec2(&mut g.start)),
            (
                "grad.end",
                NodeKind::Style(StyleKind::Fill {
                    paint: StylePaint::Gradient(g),
                    ..
                }),
            )
            | (
                "grad.end",
                NodeKind::Style(StyleKind::Stroke {
                    paint: StylePaint::Gradient(g),
                    ..
                }),
            ) => Some(Vec2(&mut g.end)),
            (
                "grad.stops",
                NodeKind::Style(StyleKind::Fill {
                    paint: StylePaint::Gradient(g),
                    ..
                }),
            )
            | (
                "grad.stops",
                NodeKind::Style(StyleKind::Stroke {
                    paint: StylePaint::Gradient(g),
                    ..
                }),
            ) => Some(Stops(&mut g.stops)),
            ("trim.start", NodeKind::Modifier(ModifierKind::TrimPath { start, .. })) => {
                Some(F64(start))
            }
            ("trim.end", NodeKind::Modifier(ModifierKind::TrimPath { end, .. })) => Some(F64(end)),
            ("trim.offset", NodeKind::Modifier(ModifierKind::TrimPath { offset, .. })) => {
                Some(F64(offset))
            }
            ("repeater.copies", NodeKind::Modifier(ModifierKind::Repeater { copies, .. })) => {
                Some(F64(copies))
            }
            ("repeater.offset", NodeKind::Modifier(ModifierKind::Repeater { offset, .. })) => {
                Some(F64(offset))
            }
            (
                "repeater.start_opacity",
                NodeKind::Modifier(ModifierKind::Repeater { start_opacity, .. }),
            ) => Some(F64(start_opacity)),
            (
                "repeater.end_opacity",
                NodeKind::Modifier(ModifierKind::Repeater { end_opacity, .. }),
            ) => Some(F64(end_opacity)),
            (
                "repeater.transform.position",
                NodeKind::Modifier(ModifierKind::Repeater { transform, .. }),
            ) => Some(Vec2(&mut transform.position)),
            (
                "repeater.transform.scale",
                NodeKind::Modifier(ModifierKind::Repeater { transform, .. }),
            ) => Some(Vec2(&mut transform.scale)),
            (
                "repeater.transform.rotation",
                NodeKind::Modifier(ModifierKind::Repeater { transform, .. }),
            ) => Some(Angle(&mut transform.rotation)),
            (
                "repeater.transform.anchor",
                NodeKind::Modifier(ModifierKind::Repeater { transform, .. }),
            ) => Some(Vec2(&mut transform.anchor)),
            (
                "repeater.transform.skew",
                NodeKind::Modifier(ModifierKind::Repeater { transform, .. }),
            ) => Some(F64(&mut transform.skew)),
            (
                "repeater.transform.skew_axis",
                NodeKind::Modifier(ModifierKind::Repeater { transform, .. }),
            ) => Some(F64(&mut transform.skew_axis)),
            ("round.radius", NodeKind::Modifier(ModifierKind::RoundCorners { radius })) => {
                Some(F64(radius))
            }
            ("offset.amount", NodeKind::Modifier(ModifierKind::OffsetPath { amount })) => {
                Some(F64(amount))
            }
            ("zigzag.amplitude", NodeKind::Modifier(ModifierKind::ZigZag { amplitude, .. })) => {
                Some(F64(amplitude))
            }
            ("zigzag.frequency", NodeKind::Modifier(ModifierKind::ZigZag { frequency, .. })) => {
                Some(F64(frequency))
            }
            ("pucker.amount", NodeKind::Modifier(ModifierKind::PuckerBloat { amount })) => {
                Some(F64(amount))
            }
            _ => None,
        }
    }

    pub fn prop_ref(&self, prop: &PropPath) -> Option<PropRef<'_>> {
        use PropRef::*;
        let s = prop.as_str();
        if s == "stroke.dash.offset" || dash_index(s).is_some() {
            if let NodeKind::Style(StyleKind::Stroke {
                dash: Some(dash), ..
            }) = &self.kind
            {
                if s == "stroke.dash.offset" {
                    return Some(F64(&dash.offset));
                }
                if let Some(index) = dash_index(s) {
                    return dash.dashes.get(index).map(PropRef::F64);
                }
            }
            return None;
        }

        match (s, &self.kind) {
            ("opacity", _) => Some(F64(&self.opacity)),
            ("transform.anchor", _) => Some(Vec2(&self.transform.anchor)),
            ("transform.position", _) => Some(Vec2(&self.transform.position)),
            ("transform.scale", _) => Some(Vec2(&self.transform.scale)),
            ("transform.rotation", _) => Some(Angle(&self.transform.rotation)),
            ("transform.skew", _) => Some(F64(&self.transform.skew)),
            ("transform.skew_axis", _) => Some(F64(&self.transform.skew_axis)),
            ("shape.path", NodeKind::Shape(ShapeKind::Path(p))) => Some(Path(p)),
            ("shape.pos", NodeKind::Shape(ShapeKind::Rect { pos, .. }))
            | ("shape.pos", NodeKind::Shape(ShapeKind::Ellipse { pos, .. }))
            | ("shape.pos", NodeKind::Shape(ShapeKind::Star { pos, .. }))
            | ("shape.pos", NodeKind::Shape(ShapeKind::Polygon { pos, .. })) => Some(Vec2(pos)),
            ("shape.size", NodeKind::Shape(ShapeKind::Rect { size, .. }))
            | ("shape.size", NodeKind::Shape(ShapeKind::Ellipse { size, .. })) => Some(Vec2(size)),
            ("shape.rounded", NodeKind::Shape(ShapeKind::Rect { rounded, .. })) => {
                Some(F64(rounded))
            }
            ("shape.points", NodeKind::Shape(ShapeKind::Star { points, .. }))
            | ("shape.points", NodeKind::Shape(ShapeKind::Polygon { points, .. })) => {
                Some(F64(points))
            }
            ("shape.inner_r", NodeKind::Shape(ShapeKind::Star { inner_r, .. })) => {
                Some(F64(inner_r))
            }
            ("shape.outer_r", NodeKind::Shape(ShapeKind::Star { outer_r, .. }))
            | ("shape.outer_r", NodeKind::Shape(ShapeKind::Polygon { outer_r, .. })) => {
                Some(F64(outer_r))
            }
            ("shape.roundness", NodeKind::Shape(ShapeKind::Star { roundness, .. }))
            | ("shape.roundness", NodeKind::Shape(ShapeKind::Polygon { roundness, .. })) => {
                Some(F64(roundness))
            }
            ("text.size", NodeKind::Text(t)) => Some(F64(&t.size)),
            (
                "shape.path",
                NodeKind::Mask(MaskProps {
                    shape: ShapeKind::Path(p),
                    ..
                }),
            ) => Some(Path(p)),
            (
                "shape.pos",
                NodeKind::Mask(MaskProps {
                    shape: ShapeKind::Rect { pos, .. },
                    ..
                }),
            )
            | (
                "shape.pos",
                NodeKind::Mask(MaskProps {
                    shape: ShapeKind::Ellipse { pos, .. },
                    ..
                }),
            )
            | (
                "shape.pos",
                NodeKind::Mask(MaskProps {
                    shape: ShapeKind::Star { pos, .. },
                    ..
                }),
            )
            | (
                "shape.pos",
                NodeKind::Mask(MaskProps {
                    shape: ShapeKind::Polygon { pos, .. },
                    ..
                }),
            ) => Some(Vec2(pos)),
            (
                "shape.size",
                NodeKind::Mask(MaskProps {
                    shape: ShapeKind::Rect { size, .. },
                    ..
                }),
            )
            | (
                "shape.size",
                NodeKind::Mask(MaskProps {
                    shape: ShapeKind::Ellipse { size, .. },
                    ..
                }),
            ) => Some(Vec2(size)),
            (
                "shape.rounded",
                NodeKind::Mask(MaskProps {
                    shape: ShapeKind::Rect { rounded, .. },
                    ..
                }),
            ) => Some(F64(rounded)),
            (
                "shape.points",
                NodeKind::Mask(MaskProps {
                    shape: ShapeKind::Star { points, .. } | ShapeKind::Polygon { points, .. },
                    ..
                }),
            ) => Some(F64(points)),
            (
                "shape.inner_r",
                NodeKind::Mask(MaskProps {
                    shape: ShapeKind::Star { inner_r, .. },
                    ..
                }),
            ) => Some(F64(inner_r)),
            (
                "shape.outer_r",
                NodeKind::Mask(MaskProps {
                    shape: ShapeKind::Star { outer_r, .. } | ShapeKind::Polygon { outer_r, .. },
                    ..
                }),
            ) => Some(F64(outer_r)),
            (
                "shape.roundness",
                NodeKind::Mask(MaskProps {
                    shape: ShapeKind::Star { roundness, .. } | ShapeKind::Polygon { roundness, .. },
                    ..
                }),
            ) => Some(F64(roundness)),
            (
                "fill.color",
                NodeKind::Style(StyleKind::Fill {
                    paint: StylePaint::Solid { color },
                    ..
                }),
            ) => Some(Color(color)),
            (
                "stroke.color",
                NodeKind::Style(StyleKind::Stroke {
                    paint: StylePaint::Solid { color },
                    ..
                }),
            ) => Some(Color(color)),
            ("stroke.width", NodeKind::Style(StyleKind::Stroke { width, .. })) => Some(F64(width)),
            (
                "grad.start",
                NodeKind::Style(StyleKind::Fill {
                    paint: StylePaint::Gradient(g),
                    ..
                }),
            )
            | (
                "grad.start",
                NodeKind::Style(StyleKind::Stroke {
                    paint: StylePaint::Gradient(g),
                    ..
                }),
            ) => Some(Vec2(&g.start)),
            (
                "grad.end",
                NodeKind::Style(StyleKind::Fill {
                    paint: StylePaint::Gradient(g),
                    ..
                }),
            )
            | (
                "grad.end",
                NodeKind::Style(StyleKind::Stroke {
                    paint: StylePaint::Gradient(g),
                    ..
                }),
            ) => Some(Vec2(&g.end)),
            (
                "grad.stops",
                NodeKind::Style(StyleKind::Fill {
                    paint: StylePaint::Gradient(g),
                    ..
                }),
            )
            | (
                "grad.stops",
                NodeKind::Style(StyleKind::Stroke {
                    paint: StylePaint::Gradient(g),
                    ..
                }),
            ) => Some(Stops(&g.stops)),
            ("trim.start", NodeKind::Modifier(ModifierKind::TrimPath { start, .. })) => {
                Some(F64(start))
            }
            ("trim.end", NodeKind::Modifier(ModifierKind::TrimPath { end, .. })) => Some(F64(end)),
            ("trim.offset", NodeKind::Modifier(ModifierKind::TrimPath { offset, .. })) => {
                Some(F64(offset))
            }
            ("repeater.copies", NodeKind::Modifier(ModifierKind::Repeater { copies, .. })) => {
                Some(F64(copies))
            }
            ("repeater.offset", NodeKind::Modifier(ModifierKind::Repeater { offset, .. })) => {
                Some(F64(offset))
            }
            (
                "repeater.start_opacity",
                NodeKind::Modifier(ModifierKind::Repeater { start_opacity, .. }),
            ) => Some(F64(start_opacity)),
            (
                "repeater.end_opacity",
                NodeKind::Modifier(ModifierKind::Repeater { end_opacity, .. }),
            ) => Some(F64(end_opacity)),
            (
                "repeater.transform.position",
                NodeKind::Modifier(ModifierKind::Repeater { transform, .. }),
            ) => Some(Vec2(&transform.position)),
            (
                "repeater.transform.scale",
                NodeKind::Modifier(ModifierKind::Repeater { transform, .. }),
            ) => Some(Vec2(&transform.scale)),
            (
                "repeater.transform.rotation",
                NodeKind::Modifier(ModifierKind::Repeater { transform, .. }),
            ) => Some(Angle(&transform.rotation)),
            (
                "repeater.transform.anchor",
                NodeKind::Modifier(ModifierKind::Repeater { transform, .. }),
            ) => Some(Vec2(&transform.anchor)),
            (
                "repeater.transform.skew",
                NodeKind::Modifier(ModifierKind::Repeater { transform, .. }),
            ) => Some(F64(&transform.skew)),
            (
                "repeater.transform.skew_axis",
                NodeKind::Modifier(ModifierKind::Repeater { transform, .. }),
            ) => Some(F64(&transform.skew_axis)),
            ("round.radius", NodeKind::Modifier(ModifierKind::RoundCorners { radius })) => {
                Some(F64(radius))
            }
            ("offset.amount", NodeKind::Modifier(ModifierKind::OffsetPath { amount })) => {
                Some(F64(amount))
            }
            ("zigzag.amplitude", NodeKind::Modifier(ModifierKind::ZigZag { amplitude, .. })) => {
                Some(F64(amplitude))
            }
            ("zigzag.frequency", NodeKind::Modifier(ModifierKind::ZigZag { frequency, .. })) => {
                Some(F64(frequency))
            }
            ("pucker.amount", NodeKind::Modifier(ModifierKind::PuckerBloat { amount })) => {
                Some(F64(amount))
            }
            _ => None,
        }
    }
}

struct SetStaticOp<'a>(&'a Value, &'a str);
impl PropVisitor for SetStaticOp<'_> {
    type Out = Result<Value, ModelError>;
    fn visit<T: PropValue>(self, a: &mut Animated<T>) -> Self::Out {
        let new = T::from_value(self.0).ok_or_else(|| ModelError::TypeMismatch(self.1.into()))?;
        Ok(std::mem::replace(&mut a.base, new).into_value())
    }
}

struct AddKeyOp<'a> {
    frame: Frame,
    value: &'a Value,
    prop: &'a str,
}
impl PropVisitor for AddKeyOp<'_> {
    type Out = Result<Option<KeyframeData>, ModelError>;
    fn visit<T: PropValue>(self, a: &mut Animated<T>) -> Self::Out {
        let new =
            T::from_value(self.value).ok_or_else(|| ModelError::TypeMismatch(self.prop.into()))?;
        let old = a.key_at(self.frame).map(|k| KeyframeData {
            frame: k.frame,
            value: k.value.clone().into_value(),
            interpolation: k.interpolation,
            ease_out: k.ease_out,
            ease_in: k.ease_in,
        });
        a.set_key(self.frame, new);
        Ok(old)
    }
}

struct RemoveKeyOp(Frame);
impl PropVisitor for RemoveKeyOp {
    type Out = Result<KeyframeData, ModelError>;
    fn visit<T: PropValue>(self, a: &mut Animated<T>) -> Self::Out {
        let k = a
            .remove_key(self.0)
            .ok_or(ModelError::NoKeyframe(self.0.0))?;
        Ok(KeyframeData {
            frame: k.frame,
            value: k.value.into_value(),
            interpolation: k.interpolation,
            ease_out: k.ease_out,
            ease_in: k.ease_in,
        })
    }
}

struct RestoreKeyOp<'a>(&'a KeyframeData, &'a str);
impl PropVisitor for RestoreKeyOp<'_> {
    type Out = Result<(), ModelError>;
    fn visit<T: PropValue>(self, a: &mut Animated<T>) -> Self::Out {
        let v =
            T::from_value(&self.0.value).ok_or_else(|| ModelError::TypeMismatch(self.1.into()))?;
        a.set_key(self.0.frame, v);
        a.set_easing(
            self.0.frame,
            self.0.interpolation,
            self.0.ease_out,
            self.0.ease_in,
        );
        Ok(())
    }
}

struct MoveKeyOp {
    from: Frame,
    to: Frame,
}
impl PropVisitor for MoveKeyOp {
    type Out = Result<(), ModelError>;
    fn visit<T: PropValue>(self, a: &mut Animated<T>) -> Self::Out {
        if self.from == self.to {
            return Ok(());
        }
        if a.key_at(self.to).is_some() {
            return Err(ModelError::KeyframeExists(self.to.0));
        }
        if a.move_key(self.from, self.to) {
            Ok(())
        } else {
            Err(ModelError::NoKeyframe(self.from.0))
        }
    }
}

struct SetEasingOp {
    frame: Frame,
    i: Interpolation,
    o: EasingHandle,
    e: EasingHandle,
}
impl PropVisitor for SetEasingOp {
    type Out = Result<(Interpolation, EasingHandle, EasingHandle), ModelError>;
    fn visit<T: PropValue>(self, a: &mut Animated<T>) -> Self::Out {
        a.set_easing(self.frame, self.i, self.o, self.e)
            .ok_or(ModelError::NoKeyframe(self.frame.0))
    }
}

struct IsAnimatedOp;
impl PropReader for IsAnimatedOp {
    type Out = bool;
    fn read<T: PropValue>(self, a: &Animated<T>) -> bool {
        a.has_keys()
    }
}

struct ValueAtOp(f64);
impl PropReader for ValueAtOp {
    type Out = Value;
    fn read<T: PropValue>(self, a: &Animated<T>) -> Value {
        a.value_at(self.0).into_value()
    }
}

struct GetStaticOp;
impl PropReader for GetStaticOp {
    type Out = Value;
    fn read<T: PropValue>(self, a: &Animated<T>) -> Value {
        a.base.clone().into_value()
    }
}

struct GetKeyOp(Frame);
impl PropReader for GetKeyOp {
    type Out = Option<KeyframeData>;
    fn read<T: PropValue>(self, a: &Animated<T>) -> Option<KeyframeData> {
        a.key_at(self.0).map(|k| KeyframeData {
            frame: k.frame,
            value: k.value.clone().into_value(),
            interpolation: k.interpolation,
            ease_out: k.ease_out,
            ease_in: k.ease_in,
        })
    }
}

/// Enumeration of keyframe frames (sorted; the source is sorted by invariant).
struct KeyFramesOp;
impl PropReader for KeyFramesOp {
    type Out = Vec<Frame>;
    fn read<T: PropValue>(self, a: &Animated<T>) -> Vec<Frame> {
        a.keyframes.iter().map(|k| k.frame).collect()
    }
}

impl Document {
    /// All live node ids whose name equals `name`. Lets hosts (games) find
    /// nodes by name without tracking `NodeId`s across document loads.
    pub fn find_nodes_by_name<'a>(&'a self, name: &'a str) -> impl Iterator<Item = NodeId> + 'a {
        self.nodes
            .iter()
            .filter(move |(_, n)| n.name == name)
            .map(|(id, _)| id)
    }

    fn pm<'a>(&'a mut self, id: NodeId, prop: &PropPath) -> Result<PropMut<'a>, ModelError> {
        self.nodes
            .get_mut(id)
            .ok_or(ModelError::MissingNode)?
            .prop_mut(prop)
            .ok_or_else(|| ModelError::MissingProp(prop.0.clone()))
    }
    fn pr<'a>(&'a self, id: NodeId, prop: &PropPath) -> Result<PropRef<'a>, ModelError> {
        self.nodes
            .get(id)
            .ok_or(ModelError::MissingNode)?
            .prop_ref(prop)
            .ok_or_else(|| ModelError::MissingProp(prop.0.clone()))
    }

    /// Set the base value; returns previous base (for undo).
    pub fn set_static(
        &mut self,
        id: NodeId,
        prop: &PropPath,
        v: &Value,
    ) -> Result<Value, ModelError> {
        let name = prop.0.clone();
        visit_prop(self.pm(id, prop)?, SetStaticOp(v, &name))
    }
    /// Insert/update key at frame; returns replaced key if any (for undo).
    pub fn add_keyframe(
        &mut self,
        id: NodeId,
        prop: &PropPath,
        frame: Frame,
        v: &Value,
    ) -> Result<Option<KeyframeData>, ModelError> {
        let name = prop.0.clone();
        visit_prop(
            self.pm(id, prop)?,
            AddKeyOp {
                frame,
                value: v,
                prop: &name,
            },
        )
    }
    pub fn remove_keyframe(
        &mut self,
        id: NodeId,
        prop: &PropPath,
        frame: Frame,
    ) -> Result<KeyframeData, ModelError> {
        visit_prop(self.pm(id, prop)?, RemoveKeyOp(frame))
    }
    pub fn restore_keyframe(
        &mut self,
        id: NodeId,
        prop: &PropPath,
        key: &KeyframeData,
    ) -> Result<(), ModelError> {
        let name = prop.0.clone();
        visit_prop(self.pm(id, prop)?, RestoreKeyOp(key, &name))
    }
    pub fn move_keyframe(
        &mut self,
        id: NodeId,
        prop: &PropPath,
        from: Frame,
        to: Frame,
    ) -> Result<(), ModelError> {
        visit_prop(self.pm(id, prop)?, MoveKeyOp { from, to })
    }
    /// Returns previous easing (for undo).
    pub fn set_easing(
        &mut self,
        id: NodeId,
        prop: &PropPath,
        frame: Frame,
        i: Interpolation,
        o: EasingHandle,
        e: EasingHandle,
    ) -> Result<(Interpolation, EasingHandle, EasingHandle), ModelError> {
        visit_prop(self.pm(id, prop)?, SetEasingOp { frame, i, o, e })
    }

    pub fn property_is_animated(&self, id: NodeId, prop: &PropPath) -> bool {
        self.pr(id, prop)
            .map(|p| read_prop(p, IsAnimatedOp))
            .unwrap_or(false)
    }
    pub fn value_at(&self, id: NodeId, prop: &PropPath, frame: f64) -> Result<Value, ModelError> {
        Ok(read_prop(self.pr(id, prop)?, ValueAtOp(frame)))
    }
    pub fn get_static(&self, id: NodeId, prop: &PropPath) -> Result<Value, ModelError> {
        Ok(read_prop(self.pr(id, prop)?, GetStaticOp))
    }
    pub fn keyframe_data(&self, id: NodeId, prop: &PropPath, frame: Frame) -> Option<KeyframeData> {
        self.pr(id, prop)
            .ok()
            .and_then(|p| read_prop(p, GetKeyOp(frame)))
    }
    /// All keyframe frames on a property, sorted (empty if missing).
    pub fn key_frames(&self, id: NodeId, prop: &PropPath) -> Vec<Frame> {
        self.pr(id, prop)
            .map(|p| read_prop(p, KeyFramesOp))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::DVec2;

    fn doc_with_ellipse_and_fill() -> (Document, NodeId) {
        let mut doc = Document::empty();
        let shape = doc.create_node(Node::new(
            "e",
            NodeKind::Shape(ShapeKind::Ellipse {
                pos: Animated::new(DVec2::new(0.0, 0.0)),
                size: Animated::new(DVec2::new(100.0, 100.0)),
            }),
        ));
        let fill = doc.create_node(Node::new(
            "f",
            NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::solid(Color::BLACK),
                rule: FillRule::NonZero,
            }),
        ));
        doc.attach(shape, Parent::Comp(doc.main), 0).unwrap();
        doc.attach(fill, Parent::Comp(doc.main), 1).unwrap();
        (doc, shape)
    }

    #[test]
    fn override_beats_keyframes() {
        let (doc, shape_id) = doc_with_ellipse_and_fill();
        let mut ov = Overrides::default();
        ov.set(
            shape_id,
            PropPath::new("shape.pos"),
            Value::DVec2(DVec2::new(99.0, 0.0)),
        );
        let s = evaluate_with(&doc, doc.main, 0.0, &ov);
        assert!(s.items[0].path.bounding_box().center().x > 90.0);
    }

    #[test]
    fn no_overrides_matches_evaluate() {
        let (doc, _) = doc_with_ellipse_and_fill();
        let a = evaluate(&doc, doc.main, 0.0);
        let b = evaluate_with(&doc, doc.main, 0.0, &Overrides::default());
        assert_eq!(a, b);
    }

    #[test]
    fn five_point_star_is_closed_with_10_corners() {
        let p = star_path(DVec2::ZERO, 5, Some(20.0), 50.0);
        assert!(matches!(
            p.elements().last(),
            Some(kurbo::PathEl::ClosePath)
        ));
        let lines = p
            .elements()
            .iter()
            .filter(|e| matches!(e, kurbo::PathEl::LineTo(_) | kurbo::PathEl::MoveTo(_)))
            .count();
        assert_eq!(lines, 10);
    }

    #[test]
    fn polygon_six_points() {
        let p = star_path(DVec2::ZERO, 6, None, 40.0);
        let verts = p
            .elements()
            .iter()
            .filter(|e| matches!(e, kurbo::PathEl::MoveTo(_) | kurbo::PathEl::LineTo(_)))
            .count();
        assert_eq!(verts, 6);
    }

    fn find_fill(doc: &Document) -> NodeId {
        let mut found = None;
        for (id, n) in doc.nodes.iter() {
            if let NodeKind::Style(StyleKind::Fill { .. }) = n.kind {
                found = Some(id);
            }
        }
        found.unwrap()
    }

    #[test]
    fn text_node_evaluates_to_scene_items() {
        let mut doc = Document::empty();
        let comp = doc.main;
        let text = doc.create_node(Node::new(
            "t",
            NodeKind::Text(TextNode {
                text: "Hi".into(),
                size: Animated::new(64.0),
                align: TextAlign::Left,
                font: None,
            }),
        ));
        let fill = doc.create_node(Node::new(
            "f",
            NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::solid(Color::BLACK),
                rule: FillRule::NonZero,
            }),
        ));
        doc.attach(text, Parent::Comp(comp), 0).unwrap();
        doc.attach(fill, Parent::Comp(comp), 1).unwrap();
        let scene = evaluate(&doc, comp, 0.0);
        assert_eq!(scene.items.len(), 1);
        assert!(!scene.items[0].path.elements().is_empty());
        assert_eq!(scene.items[0].node, text);
    }

    #[test]
    fn font_asset_lookup_finds_family() {
        let mut doc = Document::empty();
        let id = doc.assets.insert(Asset::Font(FontAsset {
            name: "Test".into(),
            family: "test".into(),
            bytes: vec![1, 2, 3],
        }));

        let found = doc.font_asset_for_family("test").unwrap();
        assert_eq!(found.0, id);
        assert_eq!(found.1.family, "test");
        assert!(doc.font_asset_for_family("missing").is_none());
    }

    #[test]
    fn font_families_sorted_and_deduped() {
        let mut doc = Document::empty();
        let a = doc.assets.insert(Asset::Font(FontAsset {
            name: "B".into(),
            family: "zeta".into(),
            bytes: vec![],
        }));
        let b = doc.assets.insert(Asset::Font(FontAsset {
            name: "A".into(),
            family: "alpha".into(),
            bytes: vec![],
        }));
        let c = doc.assets.insert(Asset::Font(FontAsset {
            name: "dup".into(),
            family: "alpha".into(),
            bytes: vec![],
        }));
        doc.asset_order.extend([a, b, c]);
        assert_eq!(doc.font_families(), vec!["alpha", "zeta"]);
    }

    #[test]
    fn text_prefers_project_font_family() {
        let mut doc = Document::empty();
        let comp = doc.main;

        let font_bytes = include_bytes!("../../renamite-text/assets/default.ttf").to_vec();
        doc.assets.insert(Asset::Font(FontAsset {
            name: "Default Test Font".into(),
            family: "testfont".into(),
            bytes: font_bytes,
        }));

        let text = doc.create_node(Node::new(
            "t",
            NodeKind::Text(TextNode {
                text: "Hello".into(),
                size: Animated::new(48.0),
                align: TextAlign::Left,
                font: Some("testfont".into()),
            }),
        ));
        let fill = doc.create_node(Node::new(
            "f",
            NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::solid(Color::BLACK),
                rule: FillRule::NonZero,
            }),
        ));
        doc.attach(text, Parent::Comp(comp), 0).unwrap();
        doc.attach(fill, Parent::Comp(comp), 1).unwrap();

        let scene = evaluate(&doc, comp, 0.0);
        assert_eq!(scene.items.len(), 1);
        assert!(!scene.items[0].path.elements().is_empty());
    }

    #[test]
    fn text_with_unknown_family_falls_back_to_default() {
        let mut doc = Document::empty();
        let comp = doc.main;
        let text = doc.create_node(Node::new(
            "t",
            NodeKind::Text(TextNode {
                text: "Hello".into(),
                size: Animated::new(48.0),
                align: TextAlign::Left,
                font: Some("not-a-font-family".into()),
            }),
        ));
        let fill = doc.create_node(Node::new(
            "f",
            NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::solid(Color::BLACK),
                rule: FillRule::NonZero,
            }),
        ));
        doc.attach(text, Parent::Comp(comp), 0).unwrap();
        doc.attach(fill, Parent::Comp(comp), 1).unwrap();

        let scene = evaluate(&doc, comp, 0.0);
        assert_eq!(scene.items.len(), 1);
        assert!(!scene.items[0].path.elements().is_empty());
    }

    #[test]
    fn image_layer_emits_ordered_scene_item() {
        let mut doc = Document::empty();
        let comp = doc.main;

        let asset = doc.assets.insert(Asset::Image(ImageAsset {
            name: "test.png".into(),
            mime: "image/png".into(),
            bytes: vec![1, 2, 3],
            width: 64,
            height: 32,
            srgb: true,
        }));
        doc.asset_order.push(asset);

        let mut node = Node::new("Image", NodeKind::Image(asset));
        node.transform.anchor = Animated::new(glam::DVec2::new(32.0, 16.0));
        node.transform.position = Animated::new(glam::DVec2::new(100.0, 100.0));

        let image = doc.create_node(node);
        doc.attach(image, Parent::Comp(comp), 0).unwrap();

        let scene = evaluate(&doc, comp, 0.0);

        assert_eq!(scene.items.len(), 1);
        assert_eq!(scene.items[0].node, image);

        assert!(matches!(
            scene.items[0].paint,
            ScenePaint::Image { asset: id, .. } if id == asset
        ));
    }

    #[test]
    fn garbage_collect_keeps_referenced_images() {
        let mut doc = Document::empty();
        let comp = doc.main;

        let used = doc.assets.insert(Asset::Image(ImageAsset {
            name: "used.png".into(),
            mime: "image/png".into(),
            bytes: vec![],
            width: 1,
            height: 1,
            srgb: true,
        }));
        let orphan = doc.assets.insert(Asset::Image(ImageAsset {
            name: "orphan.png".into(),
            mime: "image/png".into(),
            bytes: vec![],
            width: 1,
            height: 1,
            srgb: true,
        }));
        doc.asset_order.push(used);

        let image = doc.create_node(Node::new("Image", NodeKind::Image(used)));
        doc.attach(image, Parent::Comp(comp), 0).unwrap();

        doc.garbage_collect();

        assert!(doc.assets.contains_key(used));
        assert!(!doc.assets.contains_key(orphan));
        assert_eq!(doc.asset_order, vec![used]);
    }

    #[test]
    fn normalize_assets_repairs_order() {
        let mut doc = Document::empty();
        let a = doc.assets.insert(Asset::Font(FontAsset {
            name: "A".into(),
            family: "a".into(),
            bytes: vec![],
        }));
        let b = doc.assets.insert(Asset::Font(FontAsset {
            name: "B".into(),
            family: "b".into(),
            bytes: vec![],
        }));
        // Legacy-style malformed order: missing id, duplicate, dangling id.
        let dangling = doc.assets.insert(Asset::Font(FontAsset {
            name: "d".into(),
            family: "d".into(),
            bytes: vec![],
        }));
        doc.assets.remove(dangling);
        doc.asset_order = vec![a, a, dangling];

        doc.normalize_assets();

        assert_eq!(doc.asset_order, vec![a, b]);
    }

    #[test]
    fn pick_hits_center_and_misses_outside() {
        let (doc, shape) = doc_with_ellipse_and_fill();
        let scene = evaluate(&doc, doc.main, 0.0);
        assert_eq!(pick(&scene, DVec2::ZERO), Some(shape));
        assert_eq!(pick(&scene, DVec2::new(500.0, 500.0)), None);
    }

    #[test]
    fn pick_box_contains_fully() {
        let (doc, shape) = doc_with_ellipse_and_fill();
        let scene = evaluate(&doc, doc.main, 0.0);
        let picked = pick_box(&scene, DVec2::splat(-200.0), DVec2::splat(200.0));
        assert_eq!(picked, vec![shape]);
    }

    #[test]
    fn dashed_stroke_gaps_are_not_pickable() {
        let mut path = BezPath::new();
        path.move_to((0.0, 0.0));
        path.line_to((40.0, 0.0));

        let shape = NodeId::default();

        let scene = Scene {
            clips: vec![],
            items: vec![SceneItem {
                path,
                node: shape,
                style: NodeId::default(),
                paint: ScenePaint::Solid(Color::BLACK),
                kind: PaintKind::Stroke(StrokeSample {
                    width: 4.0,
                    cap: StrokeCap::Butt,
                    join: StrokeJoin::Miter,
                    dash: Some(DashSample {
                        dashes: vec![10.0, 10.0],
                        offset: 0.0,
                    }),
                }),
                opacity: 1.0,
                clips: vec![],
                blend: BlendMode::Normal,
            }],
        };

        assert_eq!(pick(&scene, DVec2::new(5.0, 0.0)), Some(shape),);

        assert_eq!(
            pick(&scene, DVec2::new(15.0, 0.0)),
            None,
            "point lies inside an off-gap",
        );

        assert_eq!(pick(&scene, DVec2::new(25.0, 0.0)), Some(shape),);
    }

    #[test]
    fn gradient_fill_emits_linear_paint() {
        let (mut doc, _) = doc_with_ellipse_and_fill();
        let fill = find_fill(&doc);
        let NodeKind::Style(st) = &mut doc.nodes[fill].kind else {
            panic!("fill node missing");
        };
        st.swap_paint(StylePaint::linear(
            DVec2::new(0.0, 0.0),
            DVec2::new(100.0, 0.0),
            GradientStops(vec![
                GradientStop {
                    offset: 0.0,
                    color: Color::BLACK,
                },
                GradientStop {
                    offset: 1.0,
                    color: Color::WHITE,
                },
            ]),
        ));
        let scene = evaluate(&doc, doc.main, 0.0);
        let item = &scene.items[0];
        match &item.paint {
            ScenePaint::LinearGradient { start, end, .. } => {
                assert!((start.x - 0.0).abs() < 1e-9);
                assert!((end.x - 100.0).abs() < 1e-9);
            }
            other => panic!("expected linear gradient, got {other:?}"),
        }
    }

    #[test]
    fn radial_gradient_keeps_radius_for_identity_transform() {
        let (mut doc, _) = doc_with_ellipse_and_fill();
        let fill = find_fill(&doc);
        let NodeKind::Style(st) = &mut doc.nodes[fill].kind else {
            panic!("fill node missing");
        };
        st.swap_paint(StylePaint::radial(
            DVec2::new(0.0, 0.0),
            DVec2::new(120.0, 0.0),
            GradientStops(vec![GradientStop {
                offset: 0.0,
                color: Color::WHITE,
            }]),
        ));
        let scene = evaluate(&doc, doc.main, 0.0);
        let item = &scene.items[0];
        match &item.paint {
            ScenePaint::RadialGradient { center, end, .. } => {
                assert!((center.x - 0.0).abs() < 1e-9 && (center.y - 0.0).abs() < 1e-9);
                assert!((end.x - 120.0).abs() < 1e-9, "radius must not degenerate");
            }
            other => panic!("expected radial gradient, got {other:?}"),
        }
    }

    #[test]
    fn scene_items_carry_the_painting_style_and_fill_style_for_resolves() {
        let (doc, shape) = doc_with_ellipse_and_fill();
        let fill = find_fill(&doc);
        assert_eq!(fill_style_for(&doc, shape), Some(fill));
        let scene = evaluate(&doc, doc.main, 0.0);
        assert_eq!(scene.items.len(), 1);
        assert_eq!(scene.items[0].node, shape);
        assert_eq!(scene.items[0].style, fill);
    }

    #[test]
    fn nodes_bounds_unions_selected() {
        let (doc, shape) = doc_with_ellipse_and_fill();
        let scene = evaluate(&doc, doc.main, 0.0);
        let (min, max) = nodes_bounds(&scene, &[shape]).unwrap();
        assert!(
            min.x <= 0.0 && max.x >= 0.0,
            "bounds must cover the ellipse center"
        );
    }

    #[test]
    fn dash_entries_are_addressable_properties() {
        let mut doc = Document::empty();

        let stroke = doc.create_node(Node::new(
            "Stroke",
            NodeKind::Style(StyleKind::Stroke {
                paint: StylePaint::solid(Color::BLACK),
                width: Animated::new(4.0),
                cap: StrokeCap::Round,
                join: StrokeJoin::Round,
                dash: Some(AnimatedDash {
                    dashes: vec![Animated::new(12.0), Animated::new(8.0)],
                    offset: Animated::new(2.0),
                }),
            }),
        ));

        doc.attach(stroke, Parent::Comp(doc.main), 0).unwrap();

        assert_eq!(
            doc.value_at(stroke, &PropPath::new("stroke.dash.0"), 0.0,)
                .unwrap(),
            Value::F64(12.0),
        );

        assert_eq!(
            doc.value_at(stroke, &PropPath::new("stroke.dash.offset"), 0.0,)
                .unwrap(),
            Value::F64(2.0),
        );

        doc.set_static(stroke, &PropPath::new("stroke.dash.1"), &Value::F64(4.0))
            .unwrap();

        assert_eq!(
            doc.value_at(stroke, &PropPath::new("stroke.dash.1"), 0.0,)
                .unwrap(),
            Value::F64(4.0),
        );
    }

    #[test]
    fn repeater_falloff_fades_copies_linearly() {
        let mut doc = Document::empty();
        let comp = doc.main;
        let group = doc.create_node(Node::new("g", NodeKind::Group));
        let shape = doc.create_node(Node::new(
            "r",
            NodeKind::Shape(ShapeKind::Rect {
                pos: Animated::new(DVec2::new(30.0, 30.0)),
                size: Animated::new(DVec2::splat(40.0)),
                rounded: Animated::new(0.0),
            }),
        ));
        let mut step = AnimatedTransform::identity();
        step.position = Animated::new(DVec2::new(60.0, 0.0));
        let rep = doc.create_node(Node::new(
            "rp",
            NodeKind::Modifier(ModifierKind::Repeater {
                copies: Animated::new(3.0),
                offset: Animated::new(0.0),
                transform: step,
                start_opacity: Animated::new(1.0),
                end_opacity: Animated::new(0.2),
            }),
        ));
        let fill = doc.create_node(Node::new(
            "f",
            NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::solid(Color::BLACK),
                rule: FillRule::NonZero,
            }),
        ));
        doc.attach(shape, Parent::Node(group), 0).unwrap();
        doc.attach(rep, Parent::Node(group), 1).unwrap();
        doc.attach(fill, Parent::Node(group), 2).unwrap();
        doc.attach(group, Parent::Comp(comp), 0).unwrap();

        let scene = evaluate(&doc, comp, 0.0);
        assert_eq!(scene.items.len(), 3);
        let ops: Vec<f64> = scene.items.iter().map(|i| i.opacity).collect();
        assert!((ops[0] - 1.0).abs() < 1e-9);
        assert!(
            (ops[1] - 0.6).abs() < 1e-9,
            "midpoint of 1.0..0.2, got {}",
            ops[1]
        );
        assert!((ops[2] - 0.2).abs() < 1e-9);
    }

    #[test]
    fn repeater_opacity_props_are_addressable() {
        let mut doc = Document::empty();
        let id = doc.create_node(Node::new(
            "rp",
            NodeKind::Modifier(ModifierKind::Repeater {
                copies: Animated::new(2.0),
                offset: Animated::new(0.0),
                transform: AnimatedTransform::identity(),
                start_opacity: Animated::new(1.0),
                end_opacity: Animated::new(0.25),
            }),
        ));

        doc.attach(id, Parent::Comp(doc.main), 0).unwrap();

        assert_eq!(
            doc.value_at(id, &PropPath::new("repeater.start_opacity"), 0.0)
                .unwrap(),
            Value::F64(1.0),
        );

        assert_eq!(
            doc.value_at(id, &PropPath::new("repeater.end_opacity"), 0.0)
                .unwrap(),
            Value::F64(0.25),
        );

        doc.set_static(id, &PropPath::new("repeater.end_opacity"), &Value::F64(0.5))
            .unwrap();

        assert_eq!(
            doc.value_at(id, &PropPath::new("repeater.end_opacity"), 0.0)
                .unwrap(),
            Value::F64(0.5),
        );
    }

    #[test]
    fn offset_path_expands_rect_bounds() {
        let mut doc = Document::empty();
        let comp = doc.main;
        let group = doc.create_node(Node::new("g", NodeKind::Group));

        let rect = doc.create_node(Node::new(
            "r",
            NodeKind::Shape(ShapeKind::Rect {
                pos: Animated::new(DVec2::new(100.0, 100.0)),
                size: Animated::new(DVec2::new(100.0, 100.0)),
                rounded: Animated::new(0.0),
            }),
        ));

        let offset = doc.create_node(Node::new(
            "op",
            NodeKind::Modifier(ModifierKind::OffsetPath {
                amount: Animated::new(10.0),
            }),
        ));

        let fill = doc.create_node(Node::new(
            "f",
            NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::solid(Color::WHITE),
                rule: FillRule::NonZero,
            }),
        ));

        doc.attach(rect, Parent::Node(group), 0).unwrap();
        doc.attach(offset, Parent::Node(group), 1).unwrap();
        doc.attach(fill, Parent::Node(group), 2).unwrap();
        doc.attach(group, Parent::Comp(comp), 0).unwrap();

        let scene = evaluate(&doc, comp, 0.0);
        let bb = scene.items[0].path.bounding_box();

        assert!(bb.width() > 118.0, "bb = {:?}", bb);
        assert!(bb.height() > 118.0, "bb = {:?}", bb);
    }

    #[test]
    fn offset_amount_property_is_addressable() {
        let mut doc = Document::empty();

        let id = doc.create_node(Node::new(
            "op",
            NodeKind::Modifier(ModifierKind::OffsetPath {
                amount: Animated::new(5.0),
            }),
        ));

        doc.attach(id, Parent::Comp(doc.main), 0).unwrap();

        assert_eq!(
            doc.value_at(id, &PropPath::new("offset.amount"), 0.0)
                .unwrap(),
            Value::F64(5.0),
        );

        doc.set_static(id, &PropPath::new("offset.amount"), &Value::F64(12.0))
            .unwrap();

        assert_eq!(
            doc.value_at(id, &PropPath::new("offset.amount"), 0.0)
                .unwrap(),
            Value::F64(12.0),
        );
    }

    #[test]
    fn zigzag_modifier_perturbs_rect_edge() {
        let mut doc = Document::empty();
        let comp = doc.main;
        let group = doc.create_node(Node::new("g", NodeKind::Group));

        let rect = doc.create_node(Node::new(
            "r",
            NodeKind::Shape(ShapeKind::Rect {
                pos: Animated::new(DVec2::new(100.0, 100.0)),
                size: Animated::new(DVec2::new(100.0, 100.0)),
                rounded: Animated::new(0.0),
            }),
        ));

        let zz = doc.create_node(Node::new(
            "zz",
            NodeKind::Modifier(ModifierKind::ZigZag {
                amplitude: Animated::new(10.0),
                frequency: Animated::new(4.0),
                smooth: false,
            }),
        ));

        let fill = doc.create_node(Node::new(
            "f",
            NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::solid(Color::WHITE),
                rule: FillRule::NonZero,
            }),
        ));

        doc.attach(rect, Parent::Node(group), 0).unwrap();
        doc.attach(zz, Parent::Node(group), 1).unwrap();
        doc.attach(fill, Parent::Node(group), 2).unwrap();
        doc.attach(group, Parent::Comp(comp), 0).unwrap();

        let scene = evaluate(&doc, comp, 0.0);
        let path = &scene.items[0].path;
        // Zig-zag adds extra vertices along the rect edges.
        let verts = path
            .elements()
            .iter()
            .filter(|e| matches!(e, kurbo::PathEl::LineTo(_) | kurbo::PathEl::MoveTo(_)))
            .count();
        assert!(verts > 4, "got {} vertices", verts);
    }

    #[test]
    fn pucker_bloat_expands_and_contracts_bounds() {
        let mut doc = Document::empty();
        let comp = doc.main;
        let group = doc.create_node(Node::new("g", NodeKind::Group));

        let rect = doc.create_node(Node::new(
            "r",
            NodeKind::Shape(ShapeKind::Rect {
                pos: Animated::new(DVec2::new(100.0, 100.0)),
                size: Animated::new(DVec2::new(100.0, 100.0)),
                rounded: Animated::new(0.0),
            }),
        ));

        let fill = doc.create_node(Node::new(
            "f",
            NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::solid(Color::WHITE),
                rule: FillRule::NonZero,
            }),
        ));

        doc.attach(rect, Parent::Node(group), 0).unwrap();
        doc.attach(fill, Parent::Node(group), 2).unwrap();
        doc.attach(group, Parent::Comp(comp), 0).unwrap();

        let base = evaluate(&doc, comp, 0.0);
        let base_bb = base.items[0].path.bounding_box();

        let bloat = doc.create_node(Node::new(
            "pb",
            NodeKind::Modifier(ModifierKind::PuckerBloat {
                amount: Animated::new(50.0),
            }),
        ));
        doc.attach(bloat, Parent::Node(group), 1).unwrap();

        let bloated = evaluate(&doc, comp, 0.0);
        let bloat_bb = bloated.items[0].path.bounding_box();
        assert!(
            bloat_bb.width() > base_bb.width(),
            "w {} vs {}",
            bloat_bb.width(),
            base_bb.width()
        );
        assert!(bloat_bb.height() > base_bb.height());

        doc.set_static(bloat, &PropPath::new("pucker.amount"), &Value::F64(-50.0))
            .unwrap();
        let puckered = evaluate(&doc, comp, 0.0);
        let pucker_bb = puckered.items[0].path.bounding_box();
        // Vertices move toward the centroid for +amount (toward center) and
        // away for -amount, so negative amount must be strictly wider.
        assert!(
            pucker_bb.width() > bloat_bb.width(),
            "pucker {} vs bloat {}",
            pucker_bb.width(),
            bloat_bb.width()
        );
    }

    #[test]
    fn zigzag_props_are_addressable() {
        let mut doc = Document::empty();
        let id = doc.create_node(Node::new(
            "zz",
            NodeKind::Modifier(ModifierKind::ZigZag {
                amplitude: Animated::new(5.0),
                frequency: Animated::new(3.0),
                smooth: true,
            }),
        ));
        doc.attach(id, Parent::Comp(doc.main), 0).unwrap();

        assert_eq!(
            doc.value_at(id, &PropPath::new("zigzag.amplitude"), 0.0)
                .unwrap(),
            Value::F64(5.0),
        );
        assert_eq!(
            doc.value_at(id, &PropPath::new("zigzag.frequency"), 0.0)
                .unwrap(),
            Value::F64(3.0),
        );

        doc.set_static(id, &PropPath::new("zigzag.amplitude"), &Value::F64(12.0))
            .unwrap();
        assert_eq!(
            doc.value_at(id, &PropPath::new("zigzag.amplitude"), 0.0)
                .unwrap(),
            Value::F64(12.0),
        );
    }

    #[test]
    fn pucker_amount_property_is_addressable() {
        let mut doc = Document::empty();
        let id = doc.create_node(Node::new(
            "pb",
            NodeKind::Modifier(ModifierKind::PuckerBloat {
                amount: Animated::new(20.0),
            }),
        ));
        doc.attach(id, Parent::Comp(doc.main), 0).unwrap();

        assert_eq!(
            doc.value_at(id, &PropPath::new("pucker.amount"), 0.0)
                .unwrap(),
            Value::F64(20.0),
        );

        doc.set_static(id, &PropPath::new("pucker.amount"), &Value::F64(-30.0))
            .unwrap();
        assert_eq!(
            doc.value_at(id, &PropPath::new("pucker.amount"), 0.0)
                .unwrap(),
            Value::F64(-30.0),
        );
    }
}

#[cfg(test)]
mod trim_tests {
    use super::*;
    use kurbo::Shape;

    fn line() -> BezPath {
        let mut p = BezPath::new();
        p.move_to((0.0, 0.0));
        p.line_to((100.0, 0.0));
        p
    }

    #[test]
    fn trim_first_quarter_of_line() {
        let out = trim_path(&line(), 0.0, 0.25, 0.0).unwrap();
        let bb = out.bounding_box();
        assert!((bb.x1 - 25.0).abs() < 0.5, "x1={}", bb.x1);
        assert!(bb.x0.abs() < 0.5);
    }

    #[test]
    fn trim_second_half_of_line() {
        let out = trim_path(&line(), 0.5, 1.0, 0.0).unwrap();
        let bb = out.bounding_box();
        assert!((bb.x0 - 50.0).abs() < 0.5 && (bb.x1 - 100.0).abs() < 0.5);
    }

    #[test]
    fn trim_zero_length_returns_none() {
        assert!(trim_path(&line(), 0.5, 0.5, 0.0).is_none());
    }

    #[test]
    fn trim_offset_shifts_range() {
        let a = trim_path(&line(), 0.0, 0.5, 0.0).unwrap();
        let b = trim_path(&line(), 0.0, 0.5, 0.5).unwrap();
        assert!((a.bounding_box().x1 - 50.0).abs() < 0.5);
        assert!(
            (b.bounding_box().x0 - 50.0).abs() < 0.5,
            "offset must shift to second half"
        );
    }

    #[test]
    fn trim_wraps_when_offset_pushes_past_end() {
        // [0, 0.5] + offset 0.75 → [0.75, 1] ∪ [0, 0.25]: both ends, gap in middle.
        let out = trim_path(&line(), 0.0, 0.5, 0.75).unwrap();
        let bb = out.bounding_box();
        assert!(bb.x0 < 1.0 && bb.x1 > 99.0, "both ends present");
        // Two disconnected subpaths → two MoveTo elements.
        let moves = out
            .elements()
            .iter()
            .filter(|el| matches!(el, kurbo::PathEl::MoveTo(_)))
            .count();
        assert_eq!(moves, 2);
    }

    #[test]
    fn trim_quarter_of_closed_square_is_one_side() {
        let sq = kurbo::Rect::new(0.0, 0.0, 100.0, 100.0).to_path(0.1);
        let out = trim_path(&sq, 0.0, 0.25, 0.0).unwrap();
        let bb = out.bounding_box();
        // One side of the square: long in one axis, ~zero in the other.
        assert!(bb.width().min(bb.height()) < 1.0);
        assert!((bb.width().max(bb.height()) - 100.0).abs() < 1.0);
    }

    #[test]
    fn full_range_with_offset_emits_whole_path() {
        let out = trim_path(&line(), 0.0, 1.0, 0.3).unwrap();
        let bb = out.bounding_box();
        assert!(bb.x0 < 0.5 && bb.x1 > 99.5);
    }
}

#[cfg(test)]
mod style_paint_tests {
    use super::*;
    use glam::DVec2;

    #[test]
    fn paint_snapshot_samples_without_copying_keys() {
        let mut color = Animated::new(Color::BLACK);
        color.set_key(Frame(0), Color::BLACK);
        color.set_key(Frame(10), Color::WHITE);

        let sampled = StylePaint::Solid { color }.snapshot(10.0);
        let StylePaint::Solid { color } = sampled else {
            panic!("expected solid");
        };

        assert_eq!(color.base, Color::WHITE);
        assert!(color.keyframes.is_empty());
    }

    #[test]
    fn set_base_color_preserves_gradient() {
        let mut paint =
            StylePaint::linear(glam::DVec2::ZERO, glam::DVec2::X, GradientStops::default());

        let red = Color::rgba(1.0, 0.0, 0.0, 1.0);
        paint.set_base_color(red);

        let StylePaint::Gradient(gradient) = paint else {
            panic!("must remain a gradient");
        };
        assert_eq!(gradient.stops.base.0[0].color, red);
    }

    fn doc_with_square_and_fill() -> (Document, NodeId) {
        let mut doc = Document::empty();
        let rect = doc.create_node(Node::new(
            "r",
            NodeKind::Shape(ShapeKind::Rect {
                pos: Animated::new(DVec2::new(0.0, 0.0)),
                size: Animated::new(DVec2::new(200.0, 200.0)),
                rounded: Animated::new(0.0),
            }),
        ));
        let fill = doc.create_node(Node::new(
            "f",
            NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::solid(Color::WHITE),
                rule: FillRule::NonZero,
            }),
        ));
        doc.attach(rect, Parent::Comp(doc.main), 0).unwrap();
        doc.attach(fill, Parent::Comp(doc.main), 1).unwrap();
        (doc, rect)
    }

    #[test]
    fn mask_clips_subsequent_siblings() {
        let (mut doc, square) = doc_with_square_and_fill();
        let mask = doc.create_node(Node::new(
            "m",
            NodeKind::Mask(MaskProps {
                inverted: false,
                shape: ShapeKind::Ellipse {
                    pos: Animated::new(DVec2::new(100.0, 100.0)),
                    size: Animated::new(DVec2::new(50.0, 50.0)),
                },
            }),
        ));
        let comp = doc.main;
        // Detach the already-attached square, reattach it after the mask.
        let (_, _) = doc.detach(square).unwrap();
        doc.attach(mask, Parent::Comp(comp), 0).unwrap();
        doc.attach(square, Parent::Comp(comp), 1).unwrap();

        let scene = evaluate(&doc, comp, 0.0);
        assert_eq!(scene.items.len(), 1);
        let item = &scene.items[0];
        assert_eq!(item.node, square);
        assert_eq!(item.clips.len(), 1);
        assert_eq!(scene.clips.len(), 1);
        assert_eq!(scene.clips[0].rule, FillRule::NonZero);
    }

    #[test]
    fn inverted_mask_uses_evenodd_rule() {
        let (mut doc, square) = doc_with_square_and_fill();
        let mask = doc.create_node(Node::new(
            "m",
            NodeKind::Mask(MaskProps {
                inverted: true,
                shape: ShapeKind::Rect {
                    pos: Animated::new(DVec2::new(100.0, 100.0)),
                    size: Animated::new(DVec2::new(50.0, 50.0)),
                    rounded: Animated::new(0.0),
                },
            }),
        ));
        let comp = doc.main;
        let (_, _) = doc.detach(square).unwrap();
        doc.attach(mask, Parent::Comp(comp), 0).unwrap();
        doc.attach(square, Parent::Comp(comp), 1).unwrap();

        let scene = evaluate(&doc, comp, 0.0);
        assert_eq!(scene.items.len(), 1);
        assert_eq!(scene.items[0].node, square);
        assert_eq!(scene.clips.len(), 1);
        assert_eq!(scene.clips[0].rule, FillRule::EvenOdd);
    }

    #[test]
    fn mask_clips_image_siblings() {
        let mut doc = Document::empty();
        let comp = doc.main;
        let mask = doc.create_node(Node::new(
            "m",
            NodeKind::Mask(MaskProps {
                inverted: false,
                shape: ShapeKind::Rect {
                    pos: Animated::new(DVec2::new(0.0, 0.0)),
                    size: Animated::new(DVec2::new(10.0, 10.0)),
                    rounded: Animated::new(0.0),
                },
            }),
        ));
        let img_asset = doc.assets.insert(Asset::Image(ImageAsset {
            name: "img".into(),
            mime: "image/png".into(),
            bytes: Vec::new(),
            width: 64,
            height: 64,
            srgb: true,
        }));
        let img = doc.create_node(Node::new("i", NodeKind::Image(img_asset)));
        doc.attach(mask, Parent::Comp(comp), 0).unwrap();
        doc.attach(img, Parent::Comp(comp), 1).unwrap();

        let scene = evaluate(&doc, comp, 0.0);
        assert_eq!(scene.items.len(), 1);
        assert_eq!(scene.items[0].clips.len(), 1);
    }

    #[test]
    fn mask_param_paths_edit_mask_geometry() {
        let mut doc = Document::empty();
        let mask = doc.create_node(Node::new(
            "m",
            NodeKind::Mask(MaskProps {
                inverted: false,
                shape: ShapeKind::Rect {
                    pos: Animated::new(DVec2::new(0.0, 0.0)),
                    size: Animated::new(DVec2::new(10.0, 20.0)),
                    rounded: Animated::new(0.0),
                },
            }),
        ));
        let mut node = doc.nodes.get_mut(mask).unwrap();
        let Some(PropMut::Vec2(v)) = node.prop_mut(&PropPath::new("shape.pos")) else {
            panic!("mask shape.pos not addressable");
        };
        v.base = DVec2::new(5.0, 5.0);
        drop(node);
        let n = doc.nodes.get_mut(mask).unwrap();
        let Some(PropRef::Vec2(v)) = n.prop_ref(&PropPath::new("shape.pos")) else {
            panic!("mask shape.pos not readable");
        };
        assert_eq!(v.base, DVec2::new(5.0, 5.0));
    }
}

#[cfg(test)]
mod group_transform_tests {
    use super::*;

    fn grouped_rect() -> (Document, NodeId, NodeId) {
        let mut doc = Document::empty();
        let comp = doc.main;

        let group = doc.create_node(Node::new("Group", NodeKind::Group));

        let shape = doc.create_node(Node::new(
            "Rect",
            NodeKind::Shape(ShapeKind::Rect {
                pos: Animated::new(glam::DVec2::new(100.0, 80.0)),
                size: Animated::new(glam::DVec2::new(60.0, 40.0)),
                rounded: Animated::new(0.0),
            }),
        ));

        let fill = doc.create_node(Node::new(
            "Fill",
            NodeKind::Style(StyleKind::Fill {
                paint: StylePaint::solid(Color::BLACK),
                rule: FillRule::NonZero,
            }),
        ));

        doc.attach(shape, Parent::Node(group), 0).unwrap();
        doc.attach(fill, Parent::Node(group), 1).unwrap();
        doc.attach(group, Parent::Comp(comp), 0).unwrap();

        (doc, group, shape)
    }

    #[test]
    fn group_selection_bounds_include_descendants() {
        let (doc, group, _) = grouped_rect();
        let scene = evaluate(&doc, doc.main, 0.0);

        let bounds = selection_bounds(&doc, &scene, &[group]);

        assert!(bounds.is_some());

        let (min, max) = bounds.unwrap();
        assert!(max.x > min.x);
        assert!(max.y > min.y);
    }

    #[test]
    fn selected_group_is_resolved_from_child_pick() {
        let (doc, group, shape) = grouped_rect();

        assert_eq!(
            selected_ancestor_for_pick(&doc, shape, &[group]),
            Some(group),
        );
    }

    #[test]
    fn immediate_child_resolution_descends_one_level() {
        let mut doc = Document::empty();

        let outer = doc.create_node(Node::new("Outer", NodeKind::Group));
        let inner = doc.create_node(Node::new("Inner", NodeKind::Group));
        let shape = doc.create_node(Node::new(
            "Shape",
            NodeKind::Shape(ShapeKind::Ellipse {
                pos: Animated::new(glam::DVec2::ZERO),
                size: Animated::new(glam::DVec2::ONE),
            }),
        ));

        doc.attach(shape, Parent::Node(inner), 0).unwrap();
        doc.attach(inner, Parent::Node(outer), 0).unwrap();
        doc.attach(outer, Parent::Comp(doc.main), 0).unwrap();

        assert_eq!(immediate_child_below(&doc, outer, shape), Some(inner));
    }

    #[test]
    fn nested_parent_delta_conversion_respects_scale() {
        let (mut doc, group, shape) = grouped_rect();

        doc.nodes[group].transform.scale = Animated::new(glam::DVec2::splat(200.0));

        let local = world_delta_to_parent(&doc, shape, 0.0, glam::DVec2::new(20.0, 10.0)).unwrap();

        assert!((local - glam::DVec2::new(10.0, 5.0)).length() < 1e-9);
    }
}
