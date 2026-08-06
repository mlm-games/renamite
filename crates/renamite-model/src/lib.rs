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
use renamite_geometry::VectorPath;
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
                                "dash" => content.dash = Some(map.next_value()?),
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
                            content.dash = Some(
                                seq.next_element()?
                                    .ok_or_else(|| A::Error::invalid_length(4, &"dash"))?,
                            );
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
        points: Animated<f64>,
    },
    InflateDeflate {
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TextNode {
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MaskProps {
    pub inverted: bool,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FillRule {
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
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Asset {
    Image,
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
    pub clip: Option<u32>,
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
}

impl ScenePaint {
    /// Color at world-space position `p` (used by vertex baking).
    pub fn color_at(&self, p: glam::DVec2) -> Color {
        match self {
            ScenePaint::Solid(c) => *c,
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

fn affine_of(ts: &renamite_animation::TransformSample) -> Affine {
    let ax = ts.skew_axis.to_radians();
    let skew =
        Affine::rotate(ax) * Affine::skew(ts.skew.to_radians().tan(), 0.0) * Affine::rotate(-ax);
    Affine::translate((ts.position.x, ts.position.y))
        * Affine::rotate(ts.rotation_deg.to_radians())
        * skew
        * Affine::scale_non_uniform(ts.scale.x / 100.0, ts.scale.y / 100.0)
        * Affine::translate((-ts.anchor.x, -ts.anchor.y))
}

const SHAPE_TOL: f64 = 0.1;

fn shape_path(kind: &ShapeKind, id: NodeId, frame: f64, ov: &Overrides) -> BezPath {
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

pub fn evaluate_with(doc: &Document, comp: CompId, frame: f64, ov: &Overrides) -> Scene {
    let mut scene = Scene::default();
    if let Some(c) = doc.compositions.get(comp) {
        eval_group(
            doc,
            &c.children,
            frame,
            Affine::IDENTITY,
            1.0,
            &mut scene,
            0,
            ov,
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
) {
    if depth > MAX_DEPTH {
        return;
    }

    // Pass 1: accumulate shape paths + modifiers, in document order.
    let mut paths: Vec<(NodeId, Affine, BezPath)> = Vec::new();
    for &id in children {
        let Some(n) = doc.nodes.get(id) else { continue };
        if !n.visible {
            continue;
        }
        match &n.kind {
            NodeKind::Shape(s) => {
                let ntf = affine_of(&sample_transform(n, id, frame, ov));
                paths.push((id, ntf, tf * ntf * shape_path(s, id, frame, ov)));
            }
            NodeKind::Modifier(m) => apply_modifier(m, id, frame, ov, &mut paths),
            _ => {}
        }
    }

    // Pass 2: bottom-first recursion + style emission (painter's order).
    for &id in children.iter().rev() {
        let Some(n) = doc.nodes.get(id) else { continue };
        if !n.visible {
            continue;
        }
        let node_op =
            opacity * ov_f64(ov, id, "opacity", n.opacity.value_at(frame)).clamp(0.0, 1.0);
        match &n.kind {
            NodeKind::Group => {
                let ntf = tf * affine_of(&sample_transform(n, id, frame, ov));
                eval_group(doc, &n.children, frame, ntf, node_op, scene, depth + 1, ov);
            }
            NodeKind::Layer(lp) => {
                if frame < lp.in_frame.0 as f64 || frame > lp.out_frame.0 as f64 {
                    continue;
                }
                let lf = (frame - lp.in_frame.0 as f64) / lp.time_stretch.max(1e-9)
                    + lp.in_frame.0 as f64;
                let ntf = tf * affine_of(&sample_transform(n, id, lf, ov));
                eval_group(doc, &n.children, lf, ntf, node_op, scene, depth + 1, ov);
            }
            NodeKind::Precomp { comp, time_map } => {
                let ntf = tf * affine_of(&sample_transform(n, id, frame, ov));
                let cf = (frame - time_map.offset.0 as f64) / time_map.stretch.max(1e-9);
                if let Some(c) = doc.compositions.get(*comp) {
                    eval_group(doc, &c.children, cf, ntf, node_op, scene, depth + 1, ov);
                }
            }
            NodeKind::Style(st) => emit_style(st, id, frame, ov, &paths, node_op, scene),
            _ => {}
        }
    }
}

fn apply_modifier(
    m: &ModifierKind,
    id: NodeId,
    frame: f64,
    ov: &Overrides,
    paths: &mut Vec<(NodeId, Affine, BezPath)>,
) {
    match m {
        ModifierKind::Repeater {
            copies,
            offset,
            transform,
        } => {
            let count = ov_f64(ov, id, "repeater.copies", copies.value_at(frame))
                .round()
                .max(0.0) as usize;
            let off = ov_f64(ov, id, "repeater.offset", offset.value_at(frame));
            let step = affine_of(&transform.sample(frame));
            let original = std::mem::take(paths);
            for i in 0..count.max(1) {
                let mut a = Affine::IDENTITY;
                let reps = (i as f64 + off).max(0.0) as usize;
                for _ in 0..reps {
                    a *= step;
                }
                for (id, affine, p) in &original {
                    paths.push((*id, *affine, a * p.clone()));
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
                    for (id, affine, path) in originals {
                        if let Some(trimmed) = trim_path(&path, s, e, o) {
                            paths.push((id, affine, trimmed));
                        }
                    }
                }
                TrimMode::Simultaneously => {
                    let lengths: Vec<f64> = originals
                        .iter()
                        .map(|(_, _, p)| p.perimeter(1e-3))
                        .collect();
                    let total: f64 = lengths.iter().sum();
                    if total <= 1e-9 {
                        return;
                    }
                    let mut cursor = 0.0;
                    for ((id, affine, path), len) in originals.into_iter().zip(lengths) {
                        let frac = len / total;
                        if frac > 1e-12 {
                            let ps = ((s - cursor) / frac).clamp(0.0, 1.0);
                            let pe = ((e - cursor) / frac).clamp(0.0, 1.0);
                            if pe > ps + 1e-9 {
                                if let Some(t) = trim_path(&path, ps, pe, o) {
                                    paths.push((id, affine, t));
                                }
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
                for (_, _, path) in paths.iter_mut() {
                    let vp = renamite_geometry::VectorPath::from_bez_path(path);
                    *path = vp.round_corners(r).to_bez_path();
                }
            }
        }
        // v0.4: OffsetPath, ZigZag, InflateDeflate - passthrough until then.
        _ => {}
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
    paths: &[(NodeId, Affine, BezPath)],
    opacity: f64,
    scene: &mut Scene,
) {
    for (node, affine, path) in paths {
        let (paint, kind) = match st {
            StyleKind::Fill { paint, rule } => (paint, PaintKind::Fill(*rule)),
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
            ),
        };

        let paint = sample_paint_world(paint, frame, affine, ov, style_id);

        scene.items.push(SceneItem {
            path: path.clone(),
            node: *node,
            style: style_id,
            paint,
            kind,
            opacity,
            clip: None,
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
) -> ScenePaint {
    match paint {
        StylePaint::Solid { color } => {
            ScenePaint::Solid(ov_color(ov, style_id, "fill.color", color.value_at(frame)))
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
        let pad = match &item.kind {
            PaintKind::Stroke(s) => (s.width * 0.5).max(1.0),
            PaintKind::Fill(_) => 0.0,
        };
        if !item.path.bounding_box().inflate(pad, pad).contains(q) {
            continue;
        }
        if let Some(ci) = item.clip {
            match scene.clips.get(ci as usize) {
                Some(c) if c.path.contains(q) => {}
                _ => continue, // clipped away (or dangling index): not pickable
            }
        }
        let hit = match &item.kind {
            PaintKind::Fill(rule) => match rule {
                FillRule::NonZero => item.path.winding(q) != 0,
                FillRule::EvenOdd => item.path.winding(q) % 2 != 0,
            },
            PaintKind::Stroke(_) => nearest_dist(&item.path, q) <= pad,
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

impl Node {
    pub fn prop_mut(&mut self, prop: &PropPath) -> Option<PropMut<'_>> {
        use PropMut::*;
        match (prop.as_str(), &mut self.kind) {
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
            ("round.radius", NodeKind::Modifier(ModifierKind::RoundCorners { radius })) => {
                Some(F64(radius))
            }
            ("offset.amount", NodeKind::Modifier(ModifierKind::OffsetPath { amount })) => {
                Some(F64(amount))
            }
            ("inflate.amount", NodeKind::Modifier(ModifierKind::InflateDeflate { amount })) => {
                Some(F64(amount))
            }
            _ => None,
        }
    }

    pub fn prop_ref(&self, prop: &PropPath) -> Option<PropRef<'_>> {
        use PropRef::*;
        match (prop.as_str(), &self.kind) {
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
            ("round.radius", NodeKind::Modifier(ModifierKind::RoundCorners { radius })) => {
                Some(F64(radius))
            }
            ("offset.amount", NodeKind::Modifier(ModifierKind::OffsetPath { amount })) => {
                Some(F64(amount))
            }
            ("inflate.amount", NodeKind::Modifier(ModifierKind::InflateDeflate { amount })) => {
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
