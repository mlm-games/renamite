//! Tessellate `renamite_model::Scene` into reusable vector meshes.
//!
//! `SceneRenderer` owns the lyon tessellators and a mesh cache keyed by
//! geometry so sub-pixel-stable triangles are reused across frames under zoom.
//!
//! Tessellation is prepared once ([`SceneRenderer::prepare`]) and then
//! consumed by either the editor's Repose `DrawScope`
//! ([`SceneRenderer::paint_prepared`]) or a headless export `repose_core::Scene`
//! ([`SceneRenderer::append_repose_scene`]) - one `PreparedScene`, two sinks.

use kurbo::PathEl;
use lyon_path::{Path as LyonPath, geom::point};
use lyon_tessellation::{
    BuffersBuilder, FillOptions, FillTessellator, FillVertexConstructor, StrokeOptions,
    StrokeTessellator, StrokeVertexConstructor, VertexBuffers,
};
use renamite_behavior_common::ViewTransform;
use renamite_geometry::dash_bez_path;
use renamite_model::{
    BlendMode as ModelBlendMode, FillRule, GradientStops, PaintKind, Scene, SceneItem, ScenePaint,
};
use repose_canvas::{DrawCommand, DrawScope};
use repose_core::{
    BlendMode, PaintDesc, Scene as ReposeScene, SceneNode, VectorMeshData, VectorVertex,
};
use rustc_hash::FxHashMap;
use std::sync::Arc;

struct SolidVertexCtor {
    color: [f32; 4],
}

impl FillVertexConstructor<VectorVertex> for SolidVertexCtor {
    fn new_vertex(&mut self, vertex: lyon_tessellation::FillVertex) -> VectorVertex {
        let p = vertex.position();
        VectorVertex {
            pos: [p.x, p.y],
            color: self.color,
            uv: [0.0, 0.0],
        }
    }
}

impl StrokeVertexConstructor<VectorVertex> for SolidVertexCtor {
    fn new_vertex(&mut self, vertex: lyon_tessellation::StrokeVertex) -> VectorVertex {
        let p = vertex.position();
        VectorVertex {
            pos: [p.x, p.y],
            color: self.color,
            uv: [0.0, 0.0],
        }
    }
}

/// One prepared draw. `mesh` lives in world space; `transform` is the
/// world -> screen affine applied in the vertex shader.
pub struct PreparedDraw {
    pub mesh: Arc<VectorMeshData>,
    pub transform: [f32; 6],
    pub paint: PaintDesc,
    pub clip: Option<u32>,
    pub blend: BlendMode,
}

/// One prepared clip mask. Vertices are already mapped to screen space
/// because Repose renders clip masks with an identity transform.
pub struct PreparedClip {
    pub mesh: Arc<VectorMeshData>,
}

/// Tessellated frame: item draws plus the clip masks they reference.
pub struct PreparedScene {
    pub draws: Vec<PreparedDraw>,
    pub clips: Vec<PreparedClip>,
}

pub struct SceneRenderer {
    cache: FxHashMap<u64, Arc<VectorMeshData>>,
    fill_tess: FillTessellator,
    stroke_tess: StrokeTessellator,
}

impl Default for SceneRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl SceneRenderer {
    pub fn new() -> Self {
        Self {
            cache: FxHashMap::default(),
            fill_tess: FillTessellator::new(),
            stroke_tess: StrokeTessellator::new(),
        }
    }

    /// Tessellate `scene` once under `view` into a reusable `PreparedScene`.
    pub fn prepare(&mut self, scene: &Scene, view: &ViewTransform) -> PreparedScene {
        let tol = (0.25 / view.scale.max(0.01)) as f32;
        // world -> screen affine: out = M * p + t, [m00, m01, m10, m11, tx, ty].
        let t = [
            view.scale as f32,
            0.0,
            0.0,
            view.scale as f32,
            view.offset.x as f32,
            view.offset.y as f32,
        ];

        let mut clips = Vec::with_capacity(scene.clips.len());
        for clip in &scene.clips {
            let mesh = self
                .clip_mesh(&clip.path, tol)
                .map(|m| transform_mesh(&m, t))
                .unwrap_or_else(|| Arc::new(VectorMeshData::default()));
            clips.push(PreparedClip { mesh });
        }

        let mut draws = Vec::with_capacity(scene.items.len());
        for item in &scene.items {
            if let Some(mesh) = self.mesh_for(item, tol) {
                draws.push(PreparedDraw {
                    mesh,
                    transform: t,
                    paint: PaintDesc::Solid,
                    clip: item.clip,
                    blend: map_blend(item.blend),
                });
            }
        }

        PreparedScene { draws, clips }
    }

    /// Paint a prepared scene into a Repose `DrawScope` (editor canvas).
    /// Clips become real `PushVectorClip`/`PopVectorClip` nesting.
    pub fn paint_prepared(&self, prepared: &PreparedScene, scope: &mut DrawScope) {
        for draw in &prepared.draws {
            if let Some(ci) = draw.clip
                && let Some(clip) = prepared.clips.get(ci as usize)
            {
                scope.commands.push(DrawCommand::PushVectorClip {
                    mesh: clip.mesh.clone(),
                });
            }
            scope.commands.push(DrawCommand::VectorMesh {
                mesh: draw.mesh.clone(),
                transform: draw.transform,
                paint: draw.paint,
                clip: draw.clip,
                blend: draw.blend,
            });
            if draw.clip.is_some() {
                scope.commands.push(DrawCommand::PopVectorClip);
            }
        }
    }

    /// Append a prepared scene to a headless `repose_core::Scene` (export).
    pub fn append_repose_scene(&self, prepared: &PreparedScene, out: &mut ReposeScene) {
        for draw in &prepared.draws {
            if let Some(ci) = draw.clip
                && let Some(clip) = prepared.clips.get(ci as usize)
            {
                out.nodes.push(SceneNode::PushVectorClip {
                    mesh: clip.mesh.clone(),
                });
            }
            out.nodes.push(SceneNode::VectorMesh {
                mesh: draw.mesh.clone(),
                transform: draw.transform,
                paint: draw.paint,
                clip: draw.clip,
                blend: draw.blend,
            });
            if draw.clip.is_some() {
                out.nodes.push(SceneNode::PopVectorClip);
            }
        }
    }

    /// Convenience wrapper: prepare + paint in one step.
    pub fn paint(&mut self, scene: &Scene, view: &ViewTransform, scope: &mut DrawScope) {
        let prepared = self.prepare(scene, view);
        self.paint_prepared(&prepared, scope);
    }

    fn mesh_for(&mut self, item: &SceneItem, tol: f32) -> Option<Arc<VectorMeshData>> {
        let key = mesh_key(item, tol);
        if let Some(m) = self.cache.get(&key) {
            return Some(m.clone());
        }

        // Convert a dashed stroke into visible open subpaths before Lyon
        // tessellation. Invalid/disabled patterns fall back to the solid path.
        let dashed_path = match &item.kind {
            PaintKind::Stroke(stroke) => stroke
                .dash
                .as_ref()
                .and_then(|dash| dash_bez_path(&item.path, &dash.dashes, dash.offset)),
            PaintKind::Fill(_) => None,
        };

        let source_path = dashed_path.as_ref().unwrap_or(&item.path);
        let lyon_path = bez_to_lyon(source_path);

        let mesh = match (&item.paint, &item.kind) {
            (ScenePaint::RadialGradient { center, end, stops }, PaintKind::Fill(_)) => {
                match radial_fan_mesh(source_path, *center, *end, stops, item.opacity, tol) {
                    Some(m) => Arc::new(m),
                    None => {
                        let m = self.tessellate(&lyon_path, &item.kind, [1.0; 4], tol)?;
                        colorize_mesh(m, &item.paint, item.opacity)
                    }
                }
            }
            _ => {
                let m = self.tessellate(&lyon_path, &item.kind, [1.0; 4], tol)?;
                colorize_mesh(m, &item.paint, item.opacity)
            }
        };
        self.cache.insert(key, mesh.clone());
        Some(mesh)
    }

    fn clip_mesh(&mut self, path: &kurbo::BezPath, tol: f32) -> Option<Arc<VectorMeshData>> {
        let key = clip_key(path, tol);
        if let Some(m) = self.cache.get(&key) {
            return Some(m.clone());
        }
        let path = bez_to_lyon(path);
        let mesh = self.tessellate(&path, &PaintKind::Fill(FillRule::NonZero), [1.0; 4], tol)?;
        self.cache.insert(key, mesh.clone().into());
        Some(mesh.into())
    }

    fn tessellate(
        &mut self,
        path: &LyonPath,
        kind: &PaintKind,
        rgba: [f32; 4],
        tol: f32,
    ) -> Option<VectorMeshData> {
        let mut buffers: VertexBuffers<VectorVertex, u32> = VertexBuffers::new();
        let ctor = SolidVertexCtor { color: rgba };

        match kind {
            PaintKind::Fill(rule) => {
                let fr = match rule {
                    FillRule::NonZero => lyon_tessellation::FillRule::NonZero,
                    FillRule::EvenOdd => lyon_tessellation::FillRule::EvenOdd,
                };
                let opts = FillOptions::tolerance(tol).with_fill_rule(fr);
                let mut b = BuffersBuilder::new(&mut buffers, ctor);
                self.fill_tess.tessellate_path(path, &opts, &mut b).ok()?;
            }
            PaintKind::Stroke(s) => {
                let opts = StrokeOptions::tolerance(tol)
                    .with_line_width(s.width as f32)
                    .with_line_cap(map_cap(s.cap))
                    .with_line_join(map_join(s.join));
                let mut b = BuffersBuilder::new(&mut buffers, ctor);
                self.stroke_tess.tessellate_path(path, &opts, &mut b).ok()?;
            }
        }

        if buffers.indices.is_empty() {
            return None;
        }
        Some(VectorMeshData {
            vertices: buffers.vertices.into(),
            indices: buffers.indices.into(),
        })
    }
}

fn colorize_mesh(mesh: VectorMeshData, paint: &ScenePaint, opacity: f64) -> Arc<VectorMeshData> {
    if let ScenePaint::Solid(c) = paint {
        let rgba = [c.r as f32, c.g as f32, c.b as f32, (c.a * opacity) as f32];
        let vertices: Arc<[VectorVertex]> = mesh
            .vertices
            .iter()
            .map(|v| VectorVertex { color: rgba, ..*v })
            .collect();
        return Arc::new(VectorMeshData {
            vertices,
            indices: mesh.indices,
        });
    }

    let vertices: Arc<[VectorVertex]> = mesh
        .vertices
        .iter()
        .map(|v| {
            let p = glam::DVec2::new(v.pos[0] as f64, v.pos[1] as f64);
            let c = paint.color_at(p);
            VectorVertex {
                pos: v.pos,
                color: [c.r as f32, c.g as f32, c.b as f32, (c.a * opacity) as f32],
                uv: v.uv,
            }
        })
        .collect();

    Arc::new(VectorMeshData {
        vertices,
        indices: mesh.indices,
    })
}

fn radial_fan_mesh(
    path: &kurbo::BezPath,
    center: glam::DVec2,
    end: glam::DVec2,
    stops: &GradientStops,
    opacity: f64,
    tol: f32,
) -> Option<VectorMeshData> {
    let mut outline: Vec<[f32; 2]> = Vec::new();
    kurbo::flatten(path.elements().iter().copied(), tol as f64, |el| {
        if let kurbo::PathEl::MoveTo(p) | kurbo::PathEl::LineTo(p) = el {
            outline.push([p.x as f32, p.y as f32]);
        }
    });
    if outline.len() < 3 || !point_in_polygon(center, &outline) {
        return None;
    }

    let radius = (end - center).length().max(1e-12);
    let n = outline.len();
    let mut vertices: Vec<VectorVertex> = Vec::with_capacity(n + 1);
    let mut indices: Vec<u32> = Vec::with_capacity(n * 3);

    let push = |vertices: &mut Vec<VectorVertex>, p: glam::DVec2| {
        let c = stops.sample(((p - center).length() / radius).clamp(0.0, 1.0));
        vertices.push(VectorVertex {
            pos: [p.x as f32, p.y as f32],
            color: [c.r as f32, c.g as f32, c.b as f32, (c.a * opacity) as f32],
            uv: [0.0, 0.0],
        });
    };

    push(&mut vertices, center);
    let center_idx = 0u32;
    for (i, pt) in outline.iter().enumerate() {
        push(&mut vertices, glam::DVec2::new(pt[0] as f64, pt[1] as f64));
        let i0 = (i as u32) + 1;
        let i1 = ((i + 1) % n) as u32 + 1;
        indices.extend_from_slice(&[center_idx, i0, i1]);
    }

    Some(VectorMeshData {
        vertices: vertices.into(),
        indices: indices.into(),
    })
}

/// Ray-casting point-in-polygon test over an x-y ring of outline points.
fn point_in_polygon(p: glam::DVec2, outline: &[[f32; 2]]) -> bool {
    let n = outline.len();
    let (mut j, mut i) = (n - 1, 0usize);
    let mut inside = false;
    while i < n {
        let xi = outline[i][0] as f64;
        let yi = outline[i][1] as f64;
        let xj = outline[j][0] as f64;
        let yj = outline[j][1] as f64;
        if ((yi > p.y) != (yj > p.y)) && (p.x < (xj - xi) * (p.y - yi) / (yj - yi) + xi) {
            inside = !inside;
        }
        j = i;
        i += 1;
    }
    inside
}

/// Apply a 2x3 affine to every vertex of `mesh`.
fn transform_mesh(mesh: &VectorMeshData, a: [f32; 6]) -> Arc<VectorMeshData> {
    let vertices: Arc<[VectorVertex]> = mesh
        .vertices
        .iter()
        .map(|v| VectorVertex {
            pos: [
                a[0] * v.pos[0] + a[2] * v.pos[1] + a[4],
                a[1] * v.pos[0] + a[3] * v.pos[1] + a[5],
            ],
            color: v.color,
            uv: v.uv,
        })
        .collect();
    Arc::new(VectorMeshData {
        vertices,
        indices: mesh.indices.clone(),
    })
}

fn map_blend(b: ModelBlendMode) -> BlendMode {
    match b {
        ModelBlendMode::Normal => BlendMode::Alpha,
        ModelBlendMode::Multiply => BlendMode::Multiply,
        ModelBlendMode::Screen => BlendMode::Alpha,
    }
}

fn bez_to_lyon(path: &kurbo::BezPath) -> LyonPath {
    let mut b = LyonPath::builder();
    let mut started = false;
    for el in path.elements() {
        match *el {
            PathEl::MoveTo(p) => {
                if started {
                    b.end(false);
                }
                b.begin(point(p.x as f32, p.y as f32));
                started = true;
            }
            PathEl::LineTo(p) => {
                b.line_to(point(p.x as f32, p.y as f32));
            }
            PathEl::QuadTo(c, p) => {
                b.quadratic_bezier_to(point(c.x as f32, c.y as f32), point(p.x as f32, p.y as f32));
            }
            PathEl::CurveTo(c1, c2, p) => {
                b.cubic_bezier_to(
                    point(c1.x as f32, c1.y as f32),
                    point(c2.x as f32, c2.y as f32),
                    point(p.x as f32, p.y as f32),
                );
            }
            PathEl::ClosePath => {
                b.close();
                started = false;
            }
        }
    }
    if started {
        b.end(false);
    }
    b.build()
}

fn map_cap(c: renamite_model::StrokeCap) -> lyon_tessellation::LineCap {
    match c {
        renamite_model::StrokeCap::Butt => lyon_tessellation::LineCap::Butt,
        renamite_model::StrokeCap::Round => lyon_tessellation::LineCap::Round,
        renamite_model::StrokeCap::Square => lyon_tessellation::LineCap::Square,
    }
}

fn map_join(j: renamite_model::StrokeJoin) -> lyon_tessellation::LineJoin {
    match j {
        renamite_model::StrokeJoin::Miter => lyon_tessellation::LineJoin::Miter,
        renamite_model::StrokeJoin::Round => lyon_tessellation::LineJoin::Round,
        renamite_model::StrokeJoin::Bevel => lyon_tessellation::LineJoin::Bevel,
    }
}

fn mesh_key(item: &SceneItem, tolerance: f32) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut h = rustc_hash::FxHasher::default();

    tolerance.to_bits().hash(&mut h);
    item.opacity.to_bits().hash(&mut h);

    hash_paint(&item.paint, &mut h);

    for element in item.path.elements() {
        match *element {
            kurbo::PathEl::MoveTo(p) => {
                0u8.hash(&mut h);
                hash_point(p, &mut h);
            }
            kurbo::PathEl::LineTo(p) => {
                1u8.hash(&mut h);
                hash_point(p, &mut h);
            }
            kurbo::PathEl::QuadTo(a, b) => {
                2u8.hash(&mut h);
                hash_point(a, &mut h);
                hash_point(b, &mut h);
            }
            kurbo::PathEl::CurveTo(a, b, c) => {
                3u8.hash(&mut h);
                hash_point(a, &mut h);
                hash_point(b, &mut h);
                hash_point(c, &mut h);
            }
            kurbo::PathEl::ClosePath => {
                4u8.hash(&mut h);
            }
        }
    }

    match &item.kind {
        PaintKind::Fill(rule) => {
            0u8.hash(&mut h);
            match rule {
                FillRule::NonZero => 0u8.hash(&mut h),
                FillRule::EvenOdd => 1u8.hash(&mut h),
            }
        }
        PaintKind::Stroke(stroke) => {
            1u8.hash(&mut h);
            stroke.width.to_bits().hash(&mut h);
            std::mem::discriminant(&stroke.cap).hash(&mut h);
            std::mem::discriminant(&stroke.join).hash(&mut h);

            match &stroke.dash {
                None => {
                    0u8.hash(&mut h);
                }

                Some(dash) => {
                    1u8.hash(&mut h);
                    dash.offset.to_bits().hash(&mut h);
                    dash.dashes.len().hash(&mut h);

                    for value in &dash.dashes {
                        value.to_bits().hash(&mut h);
                    }
                }
            }
        }
    }

    h.finish()
}

fn clip_key(path: &kurbo::BezPath, tolerance: f32) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut h = rustc_hash::FxHasher::default();

    tolerance.to_bits().hash(&mut h);

    for element in path.elements() {
        match *element {
            kurbo::PathEl::MoveTo(p) => {
                0u8.hash(&mut h);
                hash_point(p, &mut h);
            }
            kurbo::PathEl::LineTo(p) => {
                1u8.hash(&mut h);
                hash_point(p, &mut h);
            }
            kurbo::PathEl::QuadTo(a, b) => {
                2u8.hash(&mut h);
                hash_point(a, &mut h);
                hash_point(b, &mut h);
            }
            kurbo::PathEl::CurveTo(a, b, c) => {
                3u8.hash(&mut h);
                hash_point(a, &mut h);
                hash_point(b, &mut h);
                hash_point(c, &mut h);
            }
            kurbo::PathEl::ClosePath => {
                4u8.hash(&mut h);
            }
        }
    }

    0u8.hash(&mut h);
    (FillRule::NonZero as u8).hash(&mut h);

    h.finish()
}

fn hash_point(point: kurbo::Point, h: &mut impl std::hash::Hasher) {
    use std::hash::Hash;
    point.x.to_bits().hash(h);
    point.y.to_bits().hash(h);
}

fn hash_paint(paint: &ScenePaint, h: &mut impl std::hash::Hasher) {
    use std::hash::Hash;
    match paint {
        ScenePaint::Solid(c) => {
            0u8.hash(h);
            c.r.to_bits().hash(h);
            c.g.to_bits().hash(h);
            c.b.to_bits().hash(h);
            c.a.to_bits().hash(h);
        }
        ScenePaint::LinearGradient { start, end, stops } => {
            1u8.hash(h);
            start.x.to_bits().hash(h);
            start.y.to_bits().hash(h);
            end.x.to_bits().hash(h);
            end.y.to_bits().hash(h);
            hash_stops(stops, h);
        }
        ScenePaint::RadialGradient { center, end, stops } => {
            2u8.hash(h);
            center.x.to_bits().hash(h);
            center.y.to_bits().hash(h);
            end.x.to_bits().hash(h);
            end.y.to_bits().hash(h);
            hash_stops(stops, h);
        }
    }
}

fn hash_stops(stops: &GradientStops, h: &mut impl std::hash::Hasher) {
    use std::hash::Hash;
    stops.0.len().hash(h);
    for s in &stops.0 {
        s.offset.to_bits().hash(h);
        s.color.r.to_bits().hash(h);
        s.color.g.to_bits().hash(h);
        s.color.b.to_bits().hash(h);
        s.color.a.to_bits().hash(h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::{Circle, Shape};
    use renamite_model::{Color, GradientStop, GradientStops, NodeId, SceneItem};

    fn solid_item(path: kurbo::BezPath, color: Color) -> SceneItem {
        SceneItem {
            path,
            node: NodeId::default(),
            style: NodeId::default(),
            paint: ScenePaint::Solid(color),
            kind: PaintKind::Fill(renamite_model::FillRule::NonZero),
            opacity: 1.0,
            clip: None,
            blend: renamite_model::BlendMode::Normal,
        }
    }

    #[test]
    fn circle_tessellates() {
        let mut r = SceneRenderer::new();
        let item = solid_item(
            Circle::new((0.0, 0.0), 20.0).to_path(0.1),
            Color::rgba(1.0, 0.0, 0.0, 1.0),
        );
        let m = r.mesh_for(&item, 0.5).unwrap();
        assert!(m.indices.len() >= 3 && m.indices.len().is_multiple_of(3));
    }

    #[test]
    fn solid_mesh_has_uniform_vertex_color() {
        let mut r = SceneRenderer::new();
        let item = solid_item(
            Circle::new((0.0, 0.0), 20.0).to_path(0.1),
            Color::rgba(1.0, 0.5, 0.25, 0.75),
        );
        let m = r.mesh_for(&item, 0.5).unwrap();
        assert!(m.vertices.len() >= 3);
        for v in m.vertices.iter() {
            assert!((v.color[0] - 1.0).abs() < 1e-4);
            assert!((v.color[1] - 0.5).abs() < 1e-4);
            assert!((v.color[2] - 0.25).abs() < 1e-4);
            assert!((v.color[3] - 0.75).abs() < 1e-4);
        }
    }

    #[test]
    fn linear_gradient_vertex_colors_interpolate() {
        let mut r = SceneRenderer::new();
        // A circle spanning x 0..=100 on the gradient axis gives vertices all
        // the way from start (red) to end (blue).
        let item = SceneItem {
            path: Circle::new((50.0, 50.0), 50.0).to_path(0.1),
            node: NodeId::default(),
            style: NodeId::default(),
            paint: ScenePaint::LinearGradient {
                start: glam::DVec2::new(0.0, 50.0),
                end: glam::DVec2::new(100.0, 50.0),
                stops: GradientStops(vec![
                    GradientStop {
                        offset: 0.0,
                        color: Color::rgba(1.0, 0.0, 0.0, 1.0),
                    },
                    GradientStop {
                        offset: 1.0,
                        color: Color::rgba(0.0, 0.0, 1.0, 1.0),
                    },
                ]),
            },
            kind: PaintKind::Fill(renamite_model::FillRule::NonZero),
            opacity: 1.0,
            clip: None,
            blend: renamite_model::BlendMode::Normal,
        };
        let m = r.mesh_for(&item, 0.5).unwrap();
        let (mut max_r, mut max_b) = (0.0f32, 0.0f32);
        for v in m.vertices.iter() {
            max_r = max_r.max(v.color[0]);
            max_b = max_b.max(v.color[2]);
        }
        assert!(
            (max_r - 1.0).abs() < 0.15,
            "start side should be pure red, got max r={max_r}"
        );
        assert!(
            (max_b - 1.0).abs() < 0.15,
            "end side should be pure blue, got max b={max_b}"
        );
    }

    #[test]
    fn radial_gradient_vertex_colors_center_is_opaque() {
        let mut r = SceneRenderer::new();
        let item = SceneItem {
            path: Circle::new((50.0, 50.0), 50.0).to_path(0.1),
            node: NodeId::default(),
            style: NodeId::default(),
            paint: ScenePaint::RadialGradient {
                center: glam::DVec2::new(50.0, 50.0),
                end: glam::DVec2::new(100.0, 50.0),
                stops: GradientStops(vec![
                    GradientStop {
                        offset: 0.0,
                        color: Color::rgba(0.96, 0.42, 0.18, 1.0),
                    },
                    GradientStop {
                        offset: 1.0,
                        color: Color::rgba(0.96, 0.42, 0.18, 0.0),
                    },
                ]),
            },
            kind: PaintKind::Fill(renamite_model::FillRule::NonZero),
            opacity: 1.0,
            clip: None,
            blend: renamite_model::BlendMode::Normal,
        };
        let m = r.mesh_for(&item, 0.5).unwrap();
        let mut max_a = 0.0f32;
        for v in m.vertices.iter() {
            max_a = max_a.max(v.color[3]);
        }
        assert!(
            (max_a - 1.0).abs() < 0.15,
            "center vertex should be opaque, got max alpha={max_a}"
        );
    }

    #[test]
    fn cache_key_distinguishes_paints() {
        let a = solid_item(
            Circle::new((0.0, 0.0), 20.0).to_path(0.1),
            Color::rgba(1.0, 0.0, 0.0, 1.0),
        );
        let mut b = a.clone();
        b.paint = ScenePaint::Solid(Color::rgba(0.0, 0.0, 1.0, 1.0));
        assert_ne!(mesh_key(&a, 0.5), mesh_key(&b, 0.5));
    }

    #[test]
    fn dashed_stroke_tessellates() {
        let mut renderer = SceneRenderer::new();

        let mut path = kurbo::BezPath::new();
        path.move_to((0.0, 0.0));
        path.line_to((100.0, 0.0));

        let item = SceneItem {
            path,
            node: NodeId::default(),
            style: NodeId::default(),
            paint: ScenePaint::Solid(Color::BLACK),
            kind: PaintKind::Stroke(renamite_model::StrokeSample {
                width: 8.0,
                cap: renamite_model::StrokeCap::Round,
                join: renamite_model::StrokeJoin::Round,
                dash: Some(renamite_model::DashSample {
                    dashes: vec![12.0, 8.0],
                    offset: 0.0,
                }),
            }),
            opacity: 1.0,
            clip: None,
            blend: renamite_model::BlendMode::Normal,
        };

        let mesh = renderer.mesh_for(&item, 0.25).unwrap();

        assert!(!mesh.vertices.is_empty());
        assert!(!mesh.indices.is_empty());
        assert!(mesh.indices.len().is_multiple_of(3));
    }

    #[test]
    fn cache_key_includes_dash_pattern_and_offset() {
        let mut path = kurbo::BezPath::new();
        path.move_to((0.0, 0.0));
        path.line_to((100.0, 0.0));

        let make = |pattern: Vec<f64>, offset: f64| SceneItem {
            path: path.clone(),
            node: NodeId::default(),
            style: NodeId::default(),
            paint: ScenePaint::Solid(Color::BLACK),
            kind: PaintKind::Stroke(renamite_model::StrokeSample {
                width: 4.0,
                cap: renamite_model::StrokeCap::Butt,
                join: renamite_model::StrokeJoin::Miter,
                dash: Some(renamite_model::DashSample {
                    dashes: pattern,
                    offset,
                }),
            }),
            opacity: 1.0,
            clip: None,
            blend: renamite_model::BlendMode::Normal,
        };

        assert_ne!(
            mesh_key(&make(vec![10.0, 5.0], 0.0), 0.25),
            mesh_key(&make(vec![10.0, 5.0], 2.0), 0.25),
        );

        assert_ne!(
            mesh_key(&make(vec![10.0, 5.0], 0.0), 0.25),
            mesh_key(&make(vec![5.0, 10.0], 0.0), 0.25),
        );
    }

    #[test]
    fn clip_emits_push_and_pop() {
        let mut r = SceneRenderer::new();
        let scene = Scene {
            clips: vec![renamite_model::ClipPath {
                path: kurbo::Rect::new(0.0, 0.0, 10.0, 10.0).to_path(0.1),
            }],
            items: vec![SceneItem {
                path: Circle::new((5.0, 5.0), 4.0).to_path(0.1),
                node: NodeId::default(),
                style: NodeId::default(),
                paint: ScenePaint::Solid(Color::BLACK),
                kind: PaintKind::Fill(renamite_model::FillRule::NonZero),
                opacity: 1.0,
                clip: Some(0),
                blend: renamite_model::BlendMode::Normal,
            }],
        };
        let view = ViewTransform::identity();
        let prepared = r.prepare(&scene, &view);

        let mut scope = DrawScope {
            commands: Vec::new(),
            size: repose_core::Size {
                width: 100.0,
                height: 100.0,
            },
        };
        r.paint_prepared(&prepared, &mut scope);
        assert!(matches!(
            scope.commands[0],
            DrawCommand::PushVectorClip { .. }
        ));
        assert!(matches!(scope.commands[1], DrawCommand::VectorMesh { .. }));
        assert!(matches!(scope.commands[2], DrawCommand::PopVectorClip));

        let mut out = ReposeScene::default();
        r.append_repose_scene(&prepared, &mut out);
        assert!(matches!(out.nodes[0], SceneNode::PushVectorClip { .. }));
        assert!(matches!(out.nodes[1], SceneNode::VectorMesh { .. }));
        assert!(matches!(out.nodes[2], SceneNode::PopVectorClip));
    }
}
