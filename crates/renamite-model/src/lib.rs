//! Serializable document model + pure evaluator.
//!
//! Group evaluation: pass 1 accumulates shape paths and applies modifiers in
//! document order; pass 2 recurses/emits styles bottom-first so `Scene.items`
//! is in painter's order. Nodes live in a slotmap arena; tree membership is
//! attach/detach so undo/redo never changes a NodeId.

use kurbo::{Affine, BezPath, Point, Shape as KurboShape};
use renamite_animation::{
    Angle, Animated, AnimatedTransform, EasingHandle, Frame, Interpolation, Tween,
};
use renamite_geometry::VectorPath;
use serde::{Deserialize, Serialize};
use slotmap::{new_key_type, SlotMap};

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
            name: name.into(), parent: None, children: Vec::new(),
            visible: true, locked: false,
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
        Self { in_frame: Frame(0), out_frame: Frame(i64::MAX / 2), time_stretch: 1.0, blend: BlendMode::Normal }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ShapeKind {
    Path(Animated<VectorPath>),
    Rect { pos: Animated<glam::DVec2>, size: Animated<glam::DVec2>, rounded: Animated<f64> },
    Ellipse { pos: Animated<glam::DVec2>, size: Animated<glam::DVec2> },
    Star {
        pos: Animated<glam::DVec2>, points: Animated<f64>,
        inner_r: Animated<f64>, outer_r: Animated<f64>,
        roundness: Animated<f64>, kind: StarKind,
    },
    Polygon {
        pos: Animated<glam::DVec2>, points: Animated<f64>,
        outer_r: Animated<f64>, roundness: Animated<f64>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StyleKind {
    Fill { color: Animated<Color>, rule: FillRule },
    Stroke {
        color: Animated<Color>, width: Animated<f64>,
        cap: StrokeCap, join: StrokeJoin, dash: Option<AnimatedDash>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ModifierKind {
    TrimPath { start: Animated<f64>, end: Animated<f64>, offset: Animated<f64> },
    Repeater { copies: Animated<f64>, offset: Animated<f64>, transform: AnimatedTransform },
    RoundCorners { radius: Animated<f64> },
    OffsetPath { amount: Animated<f64> },
    ZigZag { amplitude: Animated<f64>, frequency: Animated<f64>, points: Animated<f64> },
    InflateDeflate { amount: Animated<f64> },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TimeMap { pub offset: Frame, pub stretch: f64 }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TextNode { pub text: String }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MaskProps { pub inverted: bool }

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Color { pub r: f64, pub g: f64, pub b: f64, pub a: f64 }

impl Color {
    pub const BLACK: Self = Self { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };
    pub fn rgba(r: f64, g: f64, b: f64, a: f64) -> Self { Self { r, g, b, a } }
}

impl Tween for Color {
    fn tween(a: &Self, b: &Self, t: f64) -> Self {
        Self {
            r: a.r + (b.r - a.r) * t, g: a.g + (b.g - a.g) * t,
            b: a.b + (b.b - a.b) * t, a: a.a + (b.a - a.a) * t,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FillRule { NonZero, EvenOdd }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StrokeCap { Butt, Round, Square }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StrokeJoin { Miter, Round, Bevel }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnimatedDash { pub dashes: Vec<Animated<f64>>, pub offset: Animated<f64> }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StarKind { Star, Burst }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlendMode { Normal, Multiply, Screen }
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Asset { Image }


#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Scene { pub items: Vec<SceneItem>, pub clips: Vec<ClipPath> }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SceneItem {
    /// World space, transforms folded in.
    pub path: BezPath,
    /// The *shape* node that produced this geometry (used for picking).
    pub node: NodeId,
    pub paint: Paint,
    pub kind: PaintKind,
    pub opacity: f64,
    pub clip: Option<u32>,
    pub blend: BlendMode,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PaintKind { Fill(FillRule), Stroke(StrokeSample) }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrokeSample {
    pub width: f64, pub cap: StrokeCap, pub join: StrokeJoin,
    pub dash: Option<DashSample>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DashSample { pub dashes: Vec<f64>, pub offset: f64 }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Paint { pub color: Color }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClipPath { pub path: BezPath }


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
    pub fn is_empty(&self) -> bool { self.values.is_empty() }
    pub fn clear(&mut self) { self.values.clear(); }
}

fn ov_f64(ov: &Overrides, id: NodeId, prop: &str, dflt: f64) -> f64 {
    match ov.get(id, prop) { Some(Value::F64(x)) => *x, _ => dflt }
}
fn ov_vec2(ov: &Overrides, id: NodeId, prop: &str, dflt: glam::DVec2) -> glam::DVec2 {
    match ov.get(id, prop) { Some(Value::DVec2(x)) => *x, _ => dflt }
}
fn ov_angle(ov: &Overrides, id: NodeId, prop: &str, dflt: f64) -> f64 {
    match ov.get(id, prop) {
        Some(Value::Angle(a)) => a.0,
        Some(Value::F64(x)) => *x,
        _ => dflt,
    }
}
fn ov_color(ov: &Overrides, id: NodeId, prop: &str, dflt: Color) -> Color {
    match ov.get(id, prop) { Some(Value::Color(c)) => *c, _ => dflt }
}

fn sample_transform(
    n: &Node, id: NodeId, frame: f64, ov: &Overrides,
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


fn affine_of(ts: &renamite_animation::TransformSample) -> Affine {
    let ax = ts.skew_axis.to_radians();
    let skew = Affine::rotate(ax)
        * Affine::skew(ts.skew.to_radians().tan(), 0.0)
        * Affine::rotate(-ax);
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
            kurbo::Ellipse::new((c.x, c.y), (s.x.abs() / 2.0, s.y.abs() / 2.0), 0.0).to_path(SHAPE_TOL)
        }
        ShapeKind::Star { pos, points, inner_r, outer_r, .. } => {
            star_path(ov_vec2(ov, id, "shape.pos", pos.value_at(frame)),
                      ov_f64(ov, id, "shape.points", points.value_at(frame)).round().max(3.0) as usize,
                      Some(ov_f64(ov, id, "shape.inner_r", inner_r.value_at(frame))),
                      ov_f64(ov, id, "shape.outer_r", outer_r.value_at(frame)))
        }
        ShapeKind::Polygon { pos, points, outer_r, .. } => {
            star_path(ov_vec2(ov, id, "shape.pos", pos.value_at(frame)),
                      ov_f64(ov, id, "shape.points", points.value_at(frame)).round().max(3.0) as usize,
                      None, ov_f64(ov, id, "shape.outer_r", outer_r.value_at(frame)))
        }
    }
}

/// Straight-edged star/polygon. Roundness: TODO (v0.4, with RoundCorners).
fn star_path(center: glam::DVec2, points: usize, inner: Option<f64>, outer: f64) -> BezPath {
    let mut p = BezPath::new();
    let n = if inner.is_some() { points * 2 } else { points };
    for k in 0..n {
        let ang = -std::f64::consts::FRAC_PI_2 + std::f64::consts::TAU * k as f64 / n as f64;
        let r = match inner { Some(ir) if k % 2 == 1 => ir, _ => outer };
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
        eval_group(doc, &c.children, frame, Affine::IDENTITY, 1.0, &mut scene, 0, ov);
    }
    scene
}

const MAX_DEPTH: u32 = 32; // precomp cycle guard

fn eval_group(
    doc: &Document, children: &[NodeId], frame: f64,
    tf: Affine, opacity: f64, scene: &mut Scene, depth: u32, ov: &Overrides,
) {
    if depth > MAX_DEPTH { return; }

    // Pass 1: accumulate shape paths + modifiers, in document order.
    let mut paths: Vec<(NodeId, BezPath)> = Vec::new();
    for &id in children {
        let Some(n) = doc.nodes.get(id) else { continue };
        if !n.visible { continue; }
        match &n.kind {
            NodeKind::Shape(s) => {
                let ntf = tf * affine_of(&sample_transform(n, id, frame, ov));
                paths.push((id, ntf * shape_path(s, id, frame, ov)));
            }
            NodeKind::Modifier(m) => apply_modifier(m, id, frame, ov, &mut paths),
            _ => {}
        }
    }

    // Pass 2: bottom-first recursion + style emission (painter's order).
    for &id in children.iter().rev() {
        let Some(n) = doc.nodes.get(id) else { continue };
        if !n.visible { continue; }
        let node_op = opacity * ov_f64(ov, id, "opacity", n.opacity.value_at(frame)).clamp(0.0, 1.0);
        match &n.kind {
            NodeKind::Group => {
                let ntf = tf * affine_of(&sample_transform(n, id, frame, ov));
                eval_group(doc, &n.children, frame, ntf, node_op, scene, depth + 1, ov);
            }
            NodeKind::Layer(lp) => {
                if frame < lp.in_frame.0 as f64 || frame > lp.out_frame.0 as f64 { continue; }
                let lf = (frame - lp.in_frame.0 as f64) / lp.time_stretch.max(1e-9) + lp.in_frame.0 as f64;
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
    m: &ModifierKind, id: NodeId, frame: f64, ov: &Overrides, paths: &mut Vec<(NodeId, BezPath)>,
) {
    match m {
        ModifierKind::Repeater { copies, offset, transform } => {
            let count = ov_f64(ov, id, "repeater.copies", copies.value_at(frame)).round().max(0.0) as usize;
            let off = ov_f64(ov, id, "repeater.offset", offset.value_at(frame));
            let step = affine_of(&transform.sample(frame));
            let original = std::mem::take(paths);
            for i in 0..count.max(1) {
                let mut a = Affine::IDENTITY;
                let reps = (i as f64 + off).max(0.0) as usize;
                for _ in 0..reps { a *= step; }
                for (id, p) in &original { paths.push((*id, a * p.clone())); }
            }
        }
        // v0.4: TrimPath (arclen param), RoundCorners, OffsetPath, ZigZag,
        // InflateDeflate - passthrough until then.
        _ => {}
    }
}

fn emit_style(
    st: &StyleKind, style_id: NodeId, frame: f64, ov: &Overrides,
    paths: &[(NodeId, BezPath)], opacity: f64, scene: &mut Scene,
) {
    for (node, path) in paths {
        let item = match st {
            StyleKind::Fill { color, rule } => SceneItem {
                path: path.clone(), node: *node,
                paint: Paint { color: ov_color(ov, style_id, "fill.color", color.value_at(frame)) },
                kind: PaintKind::Fill(*rule),
                opacity, clip: None, blend: BlendMode::Normal,
            },
            StyleKind::Stroke { color, width, cap, join, dash } => SceneItem {
                path: path.clone(), node: *node,
                paint: Paint { color: ov_color(ov, style_id, "stroke.color", color.value_at(frame)) },
                kind: PaintKind::Stroke(StrokeSample {
                    width: ov_f64(ov, style_id, "stroke.width", width.value_at(frame)).max(0.0),
                    cap: *cap, join: *join,
                    dash: dash.as_ref().map(|d| DashSample {
                        dashes: d.dashes.iter().map(|x| x.value_at(frame)).collect(),
                        offset: d.offset.value_at(frame),
                    }),
                }),
                opacity, clip: None, blend: BlendMode::Normal,
            },
        };
        scene.items.push(item);
    }
}


#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Parent { Node(NodeId), Comp(CompId) }

#[derive(Clone, Debug, thiserror::Error)]
pub enum ModelError {
    #[error("node not found")] MissingNode,
    #[error("composition not found")] MissingComp,
    #[error("no property at path {0}")] MissingProp(String),
    #[error("value type mismatch for {0}")] TypeMismatch(String),
    #[error("no keyframe at frame {0}")] NoKeyframe(i64),
    #[error("keyframe already exists at frame {0}")] KeyframeExists(i64),
    #[error("node is not attached")] NotAttached,
}

impl Document {
    pub fn empty() -> Self {
        let mut compositions = CompMap::default();
        let main = compositions.insert(Composition {
            name: "Main".into(), size: (512, 512),
            rate: renamite_animation::FrameRate { num: 60, den: 1 },
            range: (Frame(0), Frame(180)),
            children: Vec::new(),
        });
        Self { format_version: 1, compositions, nodes: NodeMap::default(), assets: AssetMap::default(), main }
    }

    pub fn create_node(&mut self, node: Node) -> NodeId { self.nodes.insert(node) }

    pub fn attach(&mut self, id: NodeId, parent: Parent, index: usize) -> Result<(), ModelError> {
        if !self.nodes.contains_key(id) { return Err(ModelError::MissingNode); }
        match parent {
            Parent::Node(p) => {
                let pn = self.nodes.get_mut(p).ok_or(ModelError::MissingNode)?;
                let i = index.min(pn.children.len());
                pn.children.insert(i, id);
                self.nodes[id].parent = Some(p);
            }
            Parent::Comp(c) => {
                let comp = self.compositions.get_mut(c).ok_or(ModelError::MissingComp)?;
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
            Parent::Node(p) => { self.nodes[p].children.remove(index); }
            Parent::Comp(c) => { self.compositions[c].children.remove(index); }
        }
        if let Some(n) = self.nodes.get_mut(id) { n.parent = None; }
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
            if !live.insert(id) { return; }
            if let Some(n) = doc.nodes.get(id) {
                for &c in &n.children { mark(doc, c, live); }
            }
        }
        let roots: Vec<NodeId> = self.compositions.values().flat_map(|c| c.children.clone()).collect();
        for r in roots { mark(self, r, &mut live); }
        self.nodes.retain(|id, _| live.contains(&id));
    }
}


#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PropPath(pub String);

impl PropPath {
    pub fn new(s: impl Into<String>) -> Self { Self(s.into()) }
    pub fn as_str(&self) -> &str { &self.0 }
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
    fn into_value(self) -> Value { Value::F64(self) }
    fn from_value(v: &Value) -> Option<Self> { if let Value::F64(x) = v { Some(*x) } else { None } }
}
impl PropValue for glam::DVec2 {
    fn into_value(self) -> Value { Value::DVec2(self) }
    fn from_value(v: &Value) -> Option<Self> { if let Value::DVec2(x) = v { Some(*x) } else { None } }
}
impl PropValue for Angle {
    fn into_value(self) -> Value { Value::Angle(self) }
    fn from_value(v: &Value) -> Option<Self> {
        match v { Value::Angle(a) => Some(*a), Value::F64(x) => Some(Angle(*x)), _ => None }
    }
}
impl PropValue for Color {
    fn into_value(self) -> Value { Value::Color(self) }
    fn from_value(v: &Value) -> Option<Self> { if let Value::Color(c) = v { Some(*c) } else { None } }
}
impl PropValue for VectorPath {
    fn into_value(self) -> Value { Value::Path(self) }
    fn from_value(v: &Value) -> Option<Self> { if let Value::Path(p) = v { Some(p.clone()) } else { None } }
}

pub enum PropMut<'a> {
    F64(&'a mut Animated<f64>),
    Vec2(&'a mut Animated<glam::DVec2>),
    Angle(&'a mut Animated<Angle>),
    Color(&'a mut Animated<Color>),
    Path(&'a mut Animated<VectorPath>),
}
pub enum PropRef<'a> {
    F64(&'a Animated<f64>),
    Vec2(&'a Animated<glam::DVec2>),
    Angle(&'a Animated<Angle>),
    Color(&'a Animated<Color>),
    Path(&'a Animated<VectorPath>),
}

pub trait PropVisitor { type Out; fn visit<T: PropValue>(self, a: &mut Animated<T>) -> Self::Out; }
pub trait PropReader { type Out; fn read<T: PropValue>(self, a: &Animated<T>) -> Self::Out; }

pub fn visit_prop<V: PropVisitor>(p: PropMut<'_>, v: V) -> V::Out {
    match p {
        PropMut::F64(a) => v.visit(a), PropMut::Vec2(a) => v.visit(a),
        PropMut::Angle(a) => v.visit(a), PropMut::Color(a) => v.visit(a),
        PropMut::Path(a) => v.visit(a),
    }
}
pub fn read_prop<V: PropReader>(p: PropRef<'_>, v: V) -> V::Out {
    match p {
        PropRef::F64(a) => v.read(a), PropRef::Vec2(a) => v.read(a),
        PropRef::Angle(a) => v.read(a), PropRef::Color(a) => v.read(a),
        PropRef::Path(a) => v.read(a),
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
            ("shape.rounded", NodeKind::Shape(ShapeKind::Rect { rounded, .. })) => Some(F64(rounded)),
            ("shape.points", NodeKind::Shape(ShapeKind::Star { points, .. }))
            | ("shape.points", NodeKind::Shape(ShapeKind::Polygon { points, .. })) => Some(F64(points)),
            ("shape.inner_r", NodeKind::Shape(ShapeKind::Star { inner_r, .. })) => Some(F64(inner_r)),
            ("shape.outer_r", NodeKind::Shape(ShapeKind::Star { outer_r, .. }))
            | ("shape.outer_r", NodeKind::Shape(ShapeKind::Polygon { outer_r, .. })) => Some(F64(outer_r)),
            ("shape.roundness", NodeKind::Shape(ShapeKind::Star { roundness, .. }))
            | ("shape.roundness", NodeKind::Shape(ShapeKind::Polygon { roundness, .. })) => Some(F64(roundness)),
            ("fill.color", NodeKind::Style(StyleKind::Fill { color, .. })) => Some(Color(color)),
            ("stroke.color", NodeKind::Style(StyleKind::Stroke { color, .. })) => Some(Color(color)),
            ("stroke.width", NodeKind::Style(StyleKind::Stroke { width, .. })) => Some(F64(width)),
            ("trim.start", NodeKind::Modifier(ModifierKind::TrimPath { start, .. })) => Some(F64(start)),
            ("trim.end", NodeKind::Modifier(ModifierKind::TrimPath { end, .. })) => Some(F64(end)),
            ("trim.offset", NodeKind::Modifier(ModifierKind::TrimPath { offset, .. })) => Some(F64(offset)),
            ("repeater.copies", NodeKind::Modifier(ModifierKind::Repeater { copies, .. })) => Some(F64(copies)),
            ("repeater.offset", NodeKind::Modifier(ModifierKind::Repeater { offset, .. })) => Some(F64(offset)),
            ("round.radius", NodeKind::Modifier(ModifierKind::RoundCorners { radius })) => Some(F64(radius)),
            ("offset.amount", NodeKind::Modifier(ModifierKind::OffsetPath { amount })) => Some(F64(amount)),
            ("inflate.amount", NodeKind::Modifier(ModifierKind::InflateDeflate { amount })) => Some(F64(amount)),
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
            ("shape.rounded", NodeKind::Shape(ShapeKind::Rect { rounded, .. })) => Some(F64(rounded)),
            ("shape.points", NodeKind::Shape(ShapeKind::Star { points, .. }))
            | ("shape.points", NodeKind::Shape(ShapeKind::Polygon { points, .. })) => Some(F64(points)),
            ("shape.inner_r", NodeKind::Shape(ShapeKind::Star { inner_r, .. })) => Some(F64(inner_r)),
            ("shape.outer_r", NodeKind::Shape(ShapeKind::Star { outer_r, .. }))
            | ("shape.outer_r", NodeKind::Shape(ShapeKind::Polygon { outer_r, .. })) => Some(F64(outer_r)),
            ("shape.roundness", NodeKind::Shape(ShapeKind::Star { roundness, .. }))
            | ("shape.roundness", NodeKind::Shape(ShapeKind::Polygon { roundness, .. })) => Some(F64(roundness)),
            ("fill.color", NodeKind::Style(StyleKind::Fill { color, .. })) => Some(Color(color)),
            ("stroke.color", NodeKind::Style(StyleKind::Stroke { color, .. })) => Some(Color(color)),
            ("stroke.width", NodeKind::Style(StyleKind::Stroke { width, .. })) => Some(F64(width)),
            ("trim.start", NodeKind::Modifier(ModifierKind::TrimPath { start, .. })) => Some(F64(start)),
            ("trim.end", NodeKind::Modifier(ModifierKind::TrimPath { end, .. })) => Some(F64(end)),
            ("trim.offset", NodeKind::Modifier(ModifierKind::TrimPath { offset, .. })) => Some(F64(offset)),
            ("repeater.copies", NodeKind::Modifier(ModifierKind::Repeater { copies, .. })) => Some(F64(copies)),
            ("repeater.offset", NodeKind::Modifier(ModifierKind::Repeater { offset, .. })) => Some(F64(offset)),
            ("round.radius", NodeKind::Modifier(ModifierKind::RoundCorners { radius })) => Some(F64(radius)),
            ("offset.amount", NodeKind::Modifier(ModifierKind::OffsetPath { amount })) => Some(F64(amount)),
            ("inflate.amount", NodeKind::Modifier(ModifierKind::InflateDeflate { amount })) => Some(F64(amount)),
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

struct AddKeyOp<'a> { frame: Frame, value: &'a Value, prop: &'a str }
impl PropVisitor for AddKeyOp<'_> {
    type Out = Result<Option<KeyframeData>, ModelError>;
    fn visit<T: PropValue>(self, a: &mut Animated<T>) -> Self::Out {
        let new = T::from_value(self.value).ok_or_else(|| ModelError::TypeMismatch(self.prop.into()))?;
        let old = a.key_at(self.frame).map(|k| KeyframeData {
            frame: k.frame, value: k.value.clone().into_value(),
            interpolation: k.interpolation, ease_out: k.ease_out, ease_in: k.ease_in,
        });
        a.set_key(self.frame, new);
        Ok(old)
    }
}

struct RemoveKeyOp(Frame);
impl PropVisitor for RemoveKeyOp {
    type Out = Result<KeyframeData, ModelError>;
    fn visit<T: PropValue>(self, a: &mut Animated<T>) -> Self::Out {
        let k = a.remove_key(self.0).ok_or(ModelError::NoKeyframe(self.0.0))?;
        Ok(KeyframeData {
            frame: k.frame, value: k.value.into_value(),
            interpolation: k.interpolation, ease_out: k.ease_out, ease_in: k.ease_in,
        })
    }
}

struct RestoreKeyOp<'a>(&'a KeyframeData, &'a str);
impl PropVisitor for RestoreKeyOp<'_> {
    type Out = Result<(), ModelError>;
    fn visit<T: PropValue>(self, a: &mut Animated<T>) -> Self::Out {
        let v = T::from_value(&self.0.value).ok_or_else(|| ModelError::TypeMismatch(self.1.into()))?;
        a.set_key(self.0.frame, v);
        a.set_easing(self.0.frame, self.0.interpolation, self.0.ease_out, self.0.ease_in);
        Ok(())
    }
}

struct MoveKeyOp { from: Frame, to: Frame }
impl PropVisitor for MoveKeyOp {
    type Out = Result<(), ModelError>;
    fn visit<T: PropValue>(self, a: &mut Animated<T>) -> Self::Out {
        if self.from == self.to { return Ok(()); }
        if a.key_at(self.to).is_some() { return Err(ModelError::KeyframeExists(self.to.0)); }
        if a.move_key(self.from, self.to) { Ok(()) } else { Err(ModelError::NoKeyframe(self.from.0)) }
    }
}

struct SetEasingOp { frame: Frame, i: Interpolation, o: EasingHandle, e: EasingHandle }
impl PropVisitor for SetEasingOp {
    type Out = Result<(Interpolation, EasingHandle, EasingHandle), ModelError>;
    fn visit<T: PropValue>(self, a: &mut Animated<T>) -> Self::Out {
        a.set_easing(self.frame, self.i, self.o, self.e).ok_or(ModelError::NoKeyframe(self.frame.0))
    }
}

struct IsAnimatedOp;
impl PropReader for IsAnimatedOp {
    type Out = bool;
    fn read<T: PropValue>(self, a: &Animated<T>) -> bool { a.has_keys() }
}

struct ValueAtOp(f64);
impl PropReader for ValueAtOp {
    type Out = Value;
    fn read<T: PropValue>(self, a: &Animated<T>) -> Value { a.value_at(self.0).into_value() }
}

struct GetStaticOp;
impl PropReader for GetStaticOp {
    type Out = Value;
    fn read<T: PropValue>(self, a: &Animated<T>) -> Value { a.base.clone().into_value() }
}

struct GetKeyOp(Frame);
impl PropReader for GetKeyOp {
    type Out = Option<KeyframeData>;
    fn read<T: PropValue>(self, a: &Animated<T>) -> Option<KeyframeData> {
        a.key_at(self.0).map(|k| KeyframeData {
            frame: k.frame, value: k.value.clone().into_value(),
            interpolation: k.interpolation, ease_out: k.ease_out, ease_in: k.ease_in,
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
        self.nodes.get_mut(id).ok_or(ModelError::MissingNode)?
            .prop_mut(prop).ok_or_else(|| ModelError::MissingProp(prop.0.clone()))
    }
    fn pr<'a>(&'a self, id: NodeId, prop: &PropPath) -> Result<PropRef<'a>, ModelError> {
        self.nodes.get(id).ok_or(ModelError::MissingNode)?
            .prop_ref(prop).ok_or_else(|| ModelError::MissingProp(prop.0.clone()))
    }

    /// Set the base value; returns previous base (for undo).
    pub fn set_static(&mut self, id: NodeId, prop: &PropPath, v: &Value) -> Result<Value, ModelError> {
        let name = prop.0.clone();
        visit_prop(self.pm(id, prop)?, SetStaticOp(v, &name))
    }
    /// Insert/update key at frame; returns replaced key if any (for undo).
    pub fn add_keyframe(&mut self, id: NodeId, prop: &PropPath, frame: Frame, v: &Value)
        -> Result<Option<KeyframeData>, ModelError> {
        let name = prop.0.clone();
        visit_prop(self.pm(id, prop)?, AddKeyOp { frame, value: v, prop: &name })
    }
    pub fn remove_keyframe(&mut self, id: NodeId, prop: &PropPath, frame: Frame)
        -> Result<KeyframeData, ModelError> {
        visit_prop(self.pm(id, prop)?, RemoveKeyOp(frame))
    }
    pub fn restore_keyframe(&mut self, id: NodeId, prop: &PropPath, key: &KeyframeData)
        -> Result<(), ModelError> {
        let name = prop.0.clone();
        visit_prop(self.pm(id, prop)?, RestoreKeyOp(key, &name))
    }
    pub fn move_keyframe(&mut self, id: NodeId, prop: &PropPath, from: Frame, to: Frame)
        -> Result<(), ModelError> {
        visit_prop(self.pm(id, prop)?, MoveKeyOp { from, to })
    }
    /// Returns previous easing (for undo).
    pub fn set_easing(
        &mut self, id: NodeId, prop: &PropPath, frame: Frame,
        i: Interpolation, o: EasingHandle, e: EasingHandle,
    ) -> Result<(Interpolation, EasingHandle, EasingHandle), ModelError> {
        visit_prop(self.pm(id, prop)?, SetEasingOp { frame, i, o, e })
    }

    pub fn property_is_animated(&self, id: NodeId, prop: &PropPath) -> bool {
        self.pr(id, prop).map(|p| read_prop(p, IsAnimatedOp)).unwrap_or(false)
    }
    pub fn value_at(&self, id: NodeId, prop: &PropPath, frame: f64) -> Result<Value, ModelError> {
        Ok(read_prop(self.pr(id, prop)?, ValueAtOp(frame)))
    }
    pub fn get_static(&self, id: NodeId, prop: &PropPath) -> Result<Value, ModelError> {
        Ok(read_prop(self.pr(id, prop)?, GetStaticOp))
    }
    pub fn keyframe_data(&self, id: NodeId, prop: &PropPath, frame: Frame) -> Option<KeyframeData> {
        self.pr(id, prop).ok().and_then(|p| read_prop(p, GetKeyOp(frame)))
    }
    /// All keyframe frames on a property, sorted (empty if missing).
    pub fn key_frames(&self, id: NodeId, prop: &PropPath) -> Vec<Frame> {
        self.pr(id, prop).map(|p| read_prop(p, KeyFramesOp)).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::DVec2;

    fn doc_with_ellipse_and_fill() -> (Document, NodeId) {
        let mut doc = Document::empty();
        let shape = doc.create_node(Node::new("e", NodeKind::Shape(ShapeKind::Ellipse {
            pos: Animated::new(DVec2::new(0.0, 0.0)),
            size: Animated::new(DVec2::new(100.0, 100.0)),
        })));
        let fill = doc.create_node(Node::new("f", NodeKind::Style(StyleKind::Fill {
            color: Animated::new(Color::BLACK), rule: FillRule::NonZero,
        })));
        doc.attach(shape, Parent::Comp(doc.main), 0).unwrap();
        doc.attach(fill, Parent::Comp(doc.main), 1).unwrap();
        (doc, shape)
    }

    #[test]
    fn override_beats_keyframes() {
        let (doc, shape_id) = doc_with_ellipse_and_fill();
        let mut ov = Overrides::default();
        ov.set(shape_id, PropPath::new("shape.pos"), Value::DVec2(DVec2::new(99.0, 0.0)));
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
}