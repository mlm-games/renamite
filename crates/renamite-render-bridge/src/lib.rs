//! Tessellate `renamite_model::Scene` into a Repose `DrawScope` (VectorMesh).
//!
//! `SceneRenderer` owns the lyon tessellators and a mesh cache keyed by
//! geometry so sub-pixel-stable triangles are reused across frames under zoom.

use kurbo::{PathEl, Shape as KurboShape};
use lyon_path::{Path as LyonPath, geom::point};
use lyon_tessellation::{
    BuffersBuilder, FillOptions, FillTessellator, FillVertexConstructor,
    StrokeOptions, StrokeTessellator, StrokeVertexConstructor, VertexBuffers,
};
use renamite_behavior_common::ViewTransform;
use renamite_model::{FillRule, PaintKind, Scene, SceneItem};
use repose_canvas::DrawScope;
use repose_core::{PaintDesc, VectorMeshData, VectorVertex};
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

    /// Paint `scene` into a Repose `DrawScope` in screen space.
    pub fn paint(&mut self, scene: &Scene, view: &ViewTransform, scope: &mut DrawScope) {
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

        for item in &scene.items {
            if let Some(mesh) = self.mesh_for(item, tol) {
                scope.draw_vector_mesh(mesh, t, PaintDesc::Solid);
            }
        }
    }

    fn mesh_for(&mut self, item: &SceneItem, tol: f32) -> Option<Arc<VectorMeshData>> {
        let key = cheap_key(item, tol);
        if let Some(m) = self.cache.get(&key) {
            return Some(m.clone());
        }
        let rgba = [
            item.paint.color.r as f32,
            item.paint.color.g as f32,
            item.paint.color.b as f32,
            (item.paint.color.a * item.opacity) as f32,
        ];
        let path = bez_to_lyon(&item.path);
        let mut buffers: VertexBuffers<VectorVertex, u32> = VertexBuffers::new();
        let ctor = SolidVertexCtor { color: rgba };

        match &item.kind {
            PaintKind::Fill(rule) => {
                let fr = match rule {
                    FillRule::NonZero => lyon_tessellation::FillRule::NonZero,
                    FillRule::EvenOdd => lyon_tessellation::FillRule::EvenOdd,
                };
                let opts = FillOptions::tolerance(tol).with_fill_rule(fr);
                let mut b = BuffersBuilder::new(&mut buffers, ctor);
                self.fill_tess.tessellate_path(&path, &opts, &mut b).ok()?;
            }
            PaintKind::Stroke(s) => {
                let opts = StrokeOptions::tolerance(tol)
                    .with_line_width(s.width as f32)
                    .with_line_cap(map_cap(s.cap))
                    .with_line_join(map_join(s.join));
                let mut b = BuffersBuilder::new(&mut buffers, ctor);
                self.stroke_tess.tessellate_path(&path, &opts, &mut b).ok()?;
            }
        }
        if buffers.indices.is_empty() {
            return None;
        }
        let mesh = Arc::new(VectorMeshData {
            vertices: buffers.vertices.into(),
            indices: buffers.indices.into(),
        });
        self.cache.insert(key, mesh.clone());
        Some(mesh)
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

fn cheap_key(item: &SceneItem, tol: f32) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = rustc_hash::FxHasher::default();
    let bb = item.path.bounding_box();
    bb.x0.to_bits().hash(&mut h);
    bb.y0.to_bits().hash(&mut h);
    bb.x1.to_bits().hash(&mut h);
    bb.y1.to_bits().hash(&mut h);
    tol.to_bits().hash(&mut h);
    item.opacity.to_bits().hash(&mut h);
    item.paint.color.r.to_bits().hash(&mut h);
    item.paint.color.g.to_bits().hash(&mut h);
    item.paint.color.b.to_bits().hash(&mut h);
    item.paint.color.a.to_bits().hash(&mut h);
    std::mem::discriminant(&item.kind).hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::{Circle, Shape};
    use renamite_model::{Color, NodeId, Paint, SceneItem};

    #[test]
    fn circle_tessellates() {
        let mut r = SceneRenderer::new();
        let item = SceneItem {
            path: Circle::new((0.0, 0.0), 20.0).to_path(0.1),
            node: NodeId::default(),
            paint: Paint {
                color: Color::rgba(1.0, 0.0, 0.0, 1.0),
            },
            kind: PaintKind::Fill(renamite_model::FillRule::NonZero),
            opacity: 1.0,
            clip: None,
            blend: renamite_model::BlendMode::Normal,
        };
        let m = r.mesh_for(&item, 0.5).unwrap();
        assert!(m.indices.len() >= 3 && m.indices.len() % 3 == 0);
    }
}