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
    BlendMode, ClipOp, ImageAlignment, ImageFilter, PaintDesc, Scene as ReposeScene, SceneNode,
    VectorMeshData, VectorVertex,
};
use rustc_hash::FxHashMap;
use slotmap::Key as _;
use std::collections::VecDeque;
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

/// One prepared draw. `mesh` lives in world space. `transform` is the
/// world -> screen affine applied in the vertex shader.
pub enum PreparedDraw {
    Vector {
        mesh: Arc<VectorMeshData>,
        transform: [f32; 6],
        paint: PaintDesc,
        clips: Vec<u32>,
        blend: BlendMode,
    },

    Image {
        handle: repose_core::ImageHandle,
        rect: repose_core::Rect,
        transform: repose_core::Transform,
        tint: repose_core::Color,
        fit: repose_core::ImageFit,
        clips: Vec<u32>,
        /// Model blend, applied where the backend supports it. The canvas
        /// `DrawCommand::Image` / `SceneNode::Image` carry no blend field,
        /// so non-normal modes on images render as normal through these
        /// sinks until repose gains image blend support.
        blend: BlendMode,
    },
}

/// High-bit namespace prevents collisions with normal Repose UI allocations.
const RENAMITE_IMAGE_NAMESPACE: u64 = 0xA000_0000_0000_0000;

/// Stable Repose image handle for a model `AssetId`. High bits are reserved so
/// this never collides with handles allocated by the UI's `RenderContext`.
pub fn image_handle(asset: renamite_model::AssetId) -> u64 {
    RENAMITE_IMAGE_NAMESPACE | (asset.data().as_ffi() & 0x0FFF_FFFF_FFFF_FFFF)
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

const MESH_CACHE_CAPACITY: usize = 512;
type MeshCacheKey = Vec<u8>;

pub struct SceneRenderer {
    cache: FxHashMap<MeshCacheKey, Arc<VectorMeshData>>,
    cache_lru: VecDeque<MeshCacheKey>,
    fill_tess: FillTessellator,
    stroke_tess: StrokeTessellator,
    image_hashes: FxHashMap<renamite_model::AssetId, u64>,
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
            cache_lru: VecDeque::new(),
            fill_tess: FillTessellator::new(),
            stroke_tess: StrokeTessellator::new(),
            image_hashes: FxHashMap::default(),
        }
    }

    /// Upload or refresh the encoded bytes of every attached image asset, and
    /// evict handles whose assets were detached or garbage collected.
    pub fn sync_document_images(
        &mut self,
        document: &renamite_model::Document,
        render: &repose_core::RenderContext,
    ) {
        use std::hash::{Hash, Hasher};

        let mut live = std::collections::HashSet::new();

        for &id in &document.asset_order {
            let Some(image) = document.image_asset(id) else {
                continue;
            };

            live.insert(id);

            let mut hasher = rustc_hash::FxHasher::default();
            image.bytes.hash(&mut hasher);
            image.srgb.hash(&mut hasher);
            let hash = hasher.finish();

            if self.image_hashes.get(&id) == Some(&hash) {
                continue;
            }

            render.set_image_encoded(image_handle(id), image.bytes.clone(), image.srgb);
            self.image_hashes.insert(id, hash);
        }

        let stale: Vec<_> = self
            .image_hashes
            .keys()
            .copied()
            .filter(|id| !live.contains(id))
            .collect();

        for id in stale {
            render.remove_image(image_handle(id));
            self.image_hashes.remove(&id);
        }
    }

    /// Tessellate `scene` once under `view` into a reusable `PreparedScene`.
    pub fn prepare(&mut self, scene: &Scene, view: &ViewTransform) -> PreparedScene {
        let scale = if view.scale.is_finite() && view.scale > 1e-6 {
            view.scale
        } else {
            1.0
        };
        let offset = if view.offset.is_finite() {
            view.offset
        } else {
            glam::DVec2::ZERO
        };
        let safe_view = ViewTransform { scale, offset };
        let tol = quantized_tolerance((0.25 / scale) as f32);
        // world -> screen affine: out = M * p + t, [m00, m01, m10, m11, tx, ty].
        let t = [
            scale as f32,
            0.0,
            0.0,
            scale as f32,
            offset.x as f32,
            offset.y as f32,
        ];

        let mut clips = Vec::with_capacity(scene.clips.len());
        for clip in &scene.clips {
            let mesh = self
                .clip_mesh(&clip.path, clip.rule, tol)
                .map(|m| transform_mesh(&m, t))
                .unwrap_or_else(|| Arc::new(VectorMeshData::default()));
            clips.push(PreparedClip { mesh });
        }

        let mut draws = Vec::with_capacity(scene.items.len());
        for item in &scene.items {
            match &item.paint {
                ScenePaint::Image {
                    asset,
                    width,
                    height,
                    affine,
                    tint,
                } => {
                    draws.push(PreparedDraw::Image {
                        handle: image_handle(*asset),
                        rect: repose_core::Rect {
                            x: 0.0,
                            y: 0.0,
                            w: *width as f32,
                            h: *height as f32,
                        },
                        transform: Self::affine_to_repose(Self::compose_view_affine(
                            *affine, &safe_view,
                        )),
                        tint: Self::model_color_to_repose(*tint, item.opacity),
                        fit: repose_core::ImageFit::Contain,
                        clips: item.clips.clone(),
                        blend: map_blend(item.blend),
                    });
                }

                _ => {
                    if let Some(mesh) = self.mesh_for(item, tol) {
                        draws.push(PreparedDraw::Vector {
                            mesh,
                            transform: t,
                            paint: PaintDesc::Solid,
                            clips: item.clips.clone(),
                            blend: map_blend(item.blend),
                        });
                    }
                }
            }
        }

        PreparedScene { draws, clips }
    }

    /// Compose a model local→world affine with the view transform.
    fn compose_view_affine(model: [f64; 6], view: &ViewTransform) -> [f64; 6] {
        [
            model[0] * view.scale,
            model[1] * view.scale,
            model[2] * view.scale,
            model[3] * view.scale,
            model[4] * view.scale + view.offset.x,
            model[5] * view.scale + view.offset.y,
        ]
    }

    /// Convert a full affine to Repose's scale/rotate/shear/translate `Transform`.
    fn affine_to_repose(affine: [f64; 6]) -> repose_core::Transform {
        let m = [affine[0], affine[2], affine[1], affine[3]];
        let (scale_x, scale_y, rotate, shear_x, shear_y) =
            Self::decompose_linear(m).unwrap_or_else(|| Self::fallback_decompose_linear(m));

        repose_core::Transform {
            translate_x: affine[4] as f32,
            translate_y: affine[5] as f32,
            scale_x: scale_x as f32,
            scale_y: scale_y as f32,
            rotate: rotate as f32,
            shear_x: shear_x as f32,
            shear_y: shear_y as f32,
            origin_x: 0.0,
            origin_y: 0.0,
            perspective: [0.0, 0.0, 1.0],
        }
    }

    /// Split a row-major 2x2 `[m00, m01, m10, m11]` into
    /// `(scale_x, scale_y, rotate, shear_x, shear_y)` such that
    /// `M = R(rotate) * H(shear) * S(scale)`, or `None` when degenerate.
    fn decompose_linear(m: [f64; 4]) -> Option<(f64, f64, f64, f64, f64)> {
        let (mut a, mut b, c, d) = (m[0], m[1], m[2], m[3]);
        let mut angle_sign = 1.0;
        if a * d - b * c < 0.0 {
            a = -a;
            b = -b;
            angle_sign = -1.0;
        }
        let e = a * a + c * c;
        let f = a * b + c * d;
        let g = b * b + d * d;
        let det_p = (e * g - f * f).max(0.0);
        let s = (e + g + 2.0 * det_p.sqrt()).sqrt();
        if !s.is_finite() || s <= 1e-12 {
            return None;
        }
        let root_det = det_p.sqrt();
        let k00 = (e + root_det) / s;
        let k01 = f / s;
        let k10 = f / s;
        let k11 = (g + root_det) / s;
        let det_k = (k00 * k11 - k01 * k10).max(1e-24);
        let r00 = (a * k11 - b * k10) / det_k;
        let r10 = (c * k11 - d * k10) / det_k;
        let rotate = r10.atan2(r00);
        if k00.abs() < 1e-12 || k11.abs() < 1e-12 {
            return None;
        }
        if angle_sign < 0.0 {
            return Some((-k00, k11, -rotate, -(k01 / k11), -(k10 / k00)));
        }
        Some((k00, k11, rotate, k01 / k11, k10 / k00))
    }

    fn fallback_decompose_linear(m: [f64; 4]) -> (f64, f64, f64, f64, f64) {
        let [a, b, c, d] = m;
        if ![a, b, c, d].iter().all(|value| value.is_finite()) {
            return (1.0, 1.0, 0.0, 0.0, 0.0);
        }
        let first_length = (a * a + c * c).sqrt();
        if first_length > 1e-12 {
            let ux = a / first_length;
            let uy = c / first_length;
            let projected = ux * b + uy * d;
            let perpendicular = (-uy * b + ux * d).abs();
            if perpendicular > 1e-12 {
                return (
                    first_length,
                    perpendicular,
                    uy.atan2(ux),
                    projected / perpendicular,
                    0.0,
                );
            }
            return (first_length, 0.0, uy.atan2(ux), 0.0, 0.0);
        }
        let second_length = (b * b + d * d).sqrt();
        if second_length > 1e-12 {
            return (0.0, second_length, (-b).atan2(d), 0.0, 0.0);
        }
        (1.0, 1.0, 0.0, 0.0, 0.0)
    }

    fn model_color_to_repose(color: renamite_model::Color, opacity: f64) -> repose_core::Color {
        repose_core::Color(
            (color.r.clamp(0.0, 1.0) * 255.0).round() as u8,
            (color.g.clamp(0.0, 1.0) * 255.0).round() as u8,
            (color.b.clamp(0.0, 1.0) * 255.0).round() as u8,
            ((color.a * opacity).clamp(0.0, 1.0) * 255.0).round() as u8,
        )
    }

    /// Paint a prepared scene into a Repose `DrawScope` (editor canvas).
    /// Clips become real `PushVectorClip`/`PopVectorClip` nesting.
    pub fn paint_prepared(&self, prepared: &PreparedScene, scope: &mut DrawScope) {
        for draw in &prepared.draws {
            match draw {
                PreparedDraw::Vector {
                    mesh,
                    transform,
                    paint,
                    clips,
                    blend,
                } => {
                    let mut pushed_clips = 0usize;
                    for &ci in clips {
                        if let Some(clip) = prepared.clips.get(ci as usize) {
                            scope.commands.push(DrawCommand::PushVectorClip {
                                mesh: clip.mesh.clone(),
                                op: ClipOp::Intersect,
                            });
                            pushed_clips += 1;
                        }
                    }
                    scope.commands.push(DrawCommand::VectorMesh {
                        mesh: mesh.clone(),
                        transform: *transform,
                        paint: *paint,
                        clip: None,
                        blend: *blend,
                    });
                    for _ in 0..pushed_clips {
                        scope.commands.push(DrawCommand::PopVectorClip);
                    }
                }

                PreparedDraw::Image {
                    handle,
                    rect,
                    transform,
                    tint,
                    fit,
                    clips,
                    blend: _,
                } => {
                    let mut pushed_clips = 0usize;
                    for &ci in clips {
                        if let Some(clip) = prepared.clips.get(ci as usize) {
                            scope.commands.push(DrawCommand::PushVectorClip {
                                mesh: clip.mesh.clone(),
                                op: ClipOp::Intersect,
                            });
                            pushed_clips += 1;
                        }
                    }

                    scope.commands.push(DrawCommand::PushTransform {
                        transform: *transform,
                    });

                    scope.commands.push(DrawCommand::Image {
                        rect: *rect,
                        handle: *handle,
                        tint: *tint,
                        fit: *fit,
                        filter: ImageFilter::Linear,
                        source_rect: None,
                    });

                    scope.commands.push(DrawCommand::PopTransform);

                    for _ in 0..pushed_clips {
                        scope.commands.push(DrawCommand::PopVectorClip);
                    }
                }
            }
        }
    }

    /// Append a prepared scene to a headless `repose_core::Scene` (export).
    pub fn append_repose_scene(&self, prepared: &PreparedScene, out: &mut ReposeScene) {
        for draw in &prepared.draws {
            match draw {
                PreparedDraw::Vector {
                    mesh,
                    transform,
                    paint,
                    clips,
                    blend,
                } => {
                    let mut pushed_clips = 0usize;
                    for &ci in clips {
                        if let Some(clip) = prepared.clips.get(ci as usize) {
                            out.nodes.push(SceneNode::PushVectorClip {
                                mesh: clip.mesh.clone(),
                                op: ClipOp::Intersect,
                            });
                            pushed_clips += 1;
                        }
                    }
                    out.nodes.push(SceneNode::VectorMesh {
                        mesh: mesh.clone(),
                        transform: *transform,
                        paint: *paint,
                        clip: None,
                        blend: *blend,
                    });
                    for _ in 0..pushed_clips {
                        out.nodes.push(SceneNode::PopVectorClip);
                    }
                }

                PreparedDraw::Image {
                    handle,
                    rect,
                    transform,
                    tint,
                    fit,
                    clips,
                    blend: _,
                } => {
                    let mut pushed_clips = 0usize;
                    for &ci in clips {
                        if let Some(clip) = prepared.clips.get(ci as usize) {
                            out.nodes.push(SceneNode::PushVectorClip {
                                mesh: clip.mesh.clone(),
                                op: ClipOp::Intersect,
                            });
                            pushed_clips += 1;
                        }
                    }

                    out.nodes.push(SceneNode::PushTransform {
                        transform: *transform,
                    });

                    out.nodes.push(SceneNode::Image {
                        rect: *rect,
                        handle: *handle,
                        tint: *tint,
                        fit: *fit,
                        filter: ImageFilter::Linear,
                        source_rect: None,
                        alignment: ImageAlignment::Center,
                    });

                    out.nodes.push(SceneNode::PopTransform);

                    for _ in 0..pushed_clips {
                        out.nodes.push(SceneNode::PopVectorClip);
                    }
                }
            }
        }
    }

    /// Convenience wrapper: prepare + paint in one step.
    pub fn paint(&mut self, scene: &Scene, view: &ViewTransform, scope: &mut DrawScope) {
        let prepared = self.prepare(scene, view);
        self.paint_prepared(&prepared, scope);
    }

    /// Checkerboard backplate + border behind `artboard` (editor chrome).
    /// Reads as transparency upstream of the rig paint; uses the active
    /// theme so it follows light/dark. Shared by the editor viewport and
    /// the player embed so both paint the same chrome.
    pub fn paint_artboard_chrome(
        scope: &mut DrawScope,
        artboard: glam::DVec2,
        view: &ViewTransform,
    ) {
        use repose_core::geometry::Rect;
        use repose_core::{Color, Px, theme};

        let th = theme();
        let origin = view.world_to_screen(glam::DVec2::ZERO);
        let width = artboard.x * view.scale;
        let height = artboard.y * view.scale;

        scope.draw_rect(
            Rect {
                x: origin.x as f32 - 4.0,
                y: origin.y as f32 - 4.0,
                w: width as f32 + 8.0,
                h: height as f32 + 8.0,
            },
            Color(0, 0, 0, 48),
            Px(3.0),
        );

        let tile_world = 32.0;
        let cols = (artboard.x / tile_world).ceil() as usize;
        let rows = (artboard.y / tile_world).ceil() as usize;
        for y in 0..rows {
            for x in 0..cols {
                let p = view.world_to_screen(glam::DVec2::new(
                    x as f64 * tile_world,
                    y as f64 * tile_world,
                ));
                let color = if (x + y) % 2 == 0 {
                    th.surface
                } else {
                    th.surface_container_high
                };
                scope.draw_rect(
                    Rect {
                        x: p.x as f32,
                        y: p.y as f32,
                        w: (tile_world * view.scale).ceil() as f32,
                        h: (tile_world * view.scale).ceil() as f32,
                    },
                    color,
                    Px(0.0),
                );
            }
        }

        let border = th.outline_variant;
        let (x, y, w, h) = (
            origin.x as f32,
            origin.y as f32,
            width as f32,
            height as f32,
        );
        scope.draw_rect(Rect { x, y, w, h: 1.0 }, border, Px(0.0));
        scope.draw_rect(
            Rect {
                x,
                y: y + h - 1.0,
                w,
                h: 1.0,
            },
            border,
            Px(0.0),
        );
        scope.draw_rect(Rect { x, y, w: 1.0, h }, border, Px(0.0));
        scope.draw_rect(
            Rect {
                x: x + w - 1.0,
                y,
                w: 1.0,
                h,
            },
            border,
            Px(0.0),
        );
    }

    fn cache_get(&mut self, key: &MeshCacheKey) -> Option<Arc<VectorMeshData>> {
        let mesh = self.cache.get(key).cloned()?;
        self.cache_lru.retain(|entry| entry != key);
        self.cache_lru.push_back(key.clone());
        Some(mesh)
    }

    fn cache_insert(&mut self, key: MeshCacheKey, mesh: Arc<VectorMeshData>) {
        self.cache.insert(key.clone(), mesh);
        self.cache_lru.retain(|entry| entry != &key);
        self.cache_lru.push_back(key);
        while self.cache_lru.len() > MESH_CACHE_CAPACITY {
            if let Some(oldest) = self.cache_lru.pop_front()
                && !self.cache_lru.contains(&oldest)
            {
                self.cache.remove(&oldest);
            }
        }
    }

    fn mesh_for(&mut self, item: &SceneItem, tol: f32) -> Option<Arc<VectorMeshData>> {
        let key = mesh_key(item, tol);
        if let Some(m) = self.cache_get(&key) {
            return Some(m);
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
        self.cache_insert(key, mesh.clone());
        Some(mesh)
    }

    fn clip_mesh(
        &mut self,
        path: &kurbo::BezPath,
        rule: FillRule,
        tol: f32,
    ) -> Option<Arc<VectorMeshData>> {
        let key = clip_key(path, rule, tol);
        if let Some(m) = self.cache_get(&key) {
            return Some(m);
        }
        let path = bez_to_lyon(path);
        let mesh = self.tessellate(&path, &PaintKind::Fill(rule), [1.0; 4], tol)?;
        let mesh = Arc::new(mesh);
        self.cache_insert(key, mesh.clone());
        Some(mesh)
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
                    .with_line_join(map_join(s.join))
                    .with_miter_limit(s.miter_limit.max(1.0) as f32);
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

fn linear_vertex_color(r: f64, g: f64, b: f64, a: f64) -> [f32; 4] {
    let c = repose_core::Color(
        (r.clamp(0.0, 1.0) * 255.0).round() as u8,
        (g.clamp(0.0, 1.0) * 255.0).round() as u8,
        (b.clamp(0.0, 1.0) * 255.0).round() as u8,
        (a.clamp(0.0, 1.0) * 255.0).round() as u8,
    );
    c.to_linear()
}

fn colorize_mesh(mesh: VectorMeshData, paint: &ScenePaint, opacity: f64) -> Arc<VectorMeshData> {
    if let ScenePaint::Solid(c) = paint {
        let rgba = linear_vertex_color(c.r, c.g, c.b, c.a * opacity);
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
                color: linear_vertex_color(c.r, c.g, c.b, c.a * opacity),
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
    let mut contours: Vec<Vec<[f32; 2]>> = Vec::new();
    let mut cur: Vec<[f32; 2]> = Vec::new();
    kurbo::flatten(path.elements().iter().copied(), tol as f64, |el| match el {
        kurbo::PathEl::MoveTo(p) => {
            if !cur.is_empty() {
                contours.push(std::mem::take(&mut cur));
            }
            cur.push([p.x as f32, p.y as f32]);
        }
        kurbo::PathEl::LineTo(p) => {
            cur.push([p.x as f32, p.y as f32]);
        }
        kurbo::PathEl::ClosePath if !cur.is_empty() => {
            contours.push(std::mem::take(&mut cur));
        }
        _ => {}
    });
    if !cur.is_empty() {
        contours.push(cur);
    }
    if contours.len() != 1 {
        return None;
    }
    let outline = &contours[0];
    if outline.len() < 3 || !point_in_polygon(center, outline) {
        return None;
    }
    if !is_convex(outline) {
        return None;
    }

    let n = outline.len();
    let mut offsets: Vec<f64> = stops.0.iter().map(|s| s.offset.clamp(0.0, 1.0)).collect();
    offsets.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    offsets.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    if offsets.is_empty() {
        return None;
    }
    let max_spoke = outline
        .iter()
        .map(|o| (glam::DVec2::new(o[0] as f64, o[1] as f64) - center).length())
        .fold(0.0f64, f64::max)
        .max(1e-12);
    let grad_radius = (end - center).length().max(1e-12);
    let span = (max_spoke / grad_radius).max(1e-6);

    let push_center = |vertices: &mut Vec<VectorVertex>| {
        let c = stops.sample(0.0);
        vertices.push(VectorVertex {
            pos: [center.x as f32, center.y as f32],
            color: linear_vertex_color(c.r, c.g, c.b, c.a * opacity),
            uv: [0.0, 0.0],
        });
    };

    let ring_point = |t: f64, i: usize| -> glam::DVec2 {
        let o = outline[i % n];
        let p = glam::DVec2::new(o[0] as f64, o[1] as f64);
        center + (p - center) * t
    };
    let ring_color = |t: f64| -> [f32; 4] {
        let c = stops.sample((t * span).clamp(0.0, 1.0));
        linear_vertex_color(c.r, c.g, c.b, c.a * opacity)
    };

    let first = offsets[0];
    let mut vertices: Vec<VectorVertex> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let mut bounds: Vec<f64> = offsets.clone();
    if bounds.last().copied().unwrap_or(1.0) < 1.0 - 1e-9 {
        bounds.push(1.0);
    }
    if first <= 1e-9 {
        push_center(&mut vertices);
        let mut prev: Vec<u32> = Vec::with_capacity(n);
        let t1 = bounds.get(1).copied().unwrap_or(1.0);
        for i in 0..n {
            let p = ring_point(t1, i);
            vertices.push(VectorVertex {
                pos: [p.x as f32, p.y as f32],
                color: ring_color(t1),
                uv: [0.0, 0.0],
            });
            prev.push((i as u32) + 1);
        }
        for i in 0..n {
            indices.extend_from_slice(&[0, prev[i], prev[(i + 1) % n]]);
        }
        if bounds.len() == 1 {
            return Some(VectorMeshData {
                vertices: vertices.into(),
                indices: indices.into(),
            });
        }
        for &t in &bounds[2..] {
            let base = vertices.len() as u32;
            for i in 0..n {
                let p = ring_point(t, i);
                vertices.push(VectorVertex {
                    pos: [p.x as f32, p.y as f32],
                    color: ring_color(t),
                    uv: [0.0, 0.0],
                });
            }
            for i in 0..n {
                let a0 = prev[i];
                let a1 = prev[(i + 1) % n];
                let b0 = base + i as u32;
                let b1 = base + ((i + 1) % n) as u32;
                indices.extend_from_slice(&[a0, b0, b1, a0, b1, a1]);
            }
            prev = (base..base + n as u32).collect();
        }
        let base = vertices.len() as u32;
        let edge = ring_color(1.0);
        for (i, pt) in outline.iter().enumerate() {
            vertices.push(VectorVertex {
                pos: *pt,
                color: edge,
                uv: [0.0, 0.0],
            });
            let _ = i;
        }
        for i in 0..n {
            let a0 = prev[i];
            let a1 = prev[(i + 1) % n];
            let b0 = base + i as u32;
            let b1 = base + ((i + 1) % n) as u32;
            indices.extend_from_slice(&[a0, b0, b1, a0, b1, a1]);
        }
        return Some(VectorMeshData {
            vertices: vertices.into(),
            indices: indices.into(),
        });
    }

    push_center(&mut vertices);
    let mut prev: Vec<u32> = vec![0u32];
    let mut prev_is_point = true;
    for &t in &bounds {
        let base = vertices.len() as u32;
        for i in 0..n {
            let p = ring_point(t, i);
            vertices.push(VectorVertex {
                pos: [p.x as f32, p.y as f32],
                color: ring_color(t),
                uv: [0.0, 0.0],
            });
        }
        if prev_is_point {
            for i in 0..n {
                indices.extend_from_slice(&[0, base + i as u32, base + ((i + 1) % n) as u32]);
            }
        } else {
            for i in 0..n {
                let a0 = prev[i];
                let a1 = prev[(i + 1) % n];
                let b0 = base + i as u32;
                let b1 = base + ((i + 1) % n) as u32;
                indices.extend_from_slice(&[a0, b0, b1, a0, b1, a1]);
            }
        }
        prev = (base..base + n as u32).collect();
        prev_is_point = false;
    }
    let base = vertices.len() as u32;
    let edge = ring_color(1.0);
    for pt in outline.iter() {
        vertices.push(VectorVertex {
            pos: *pt,
            color: edge,
            uv: [0.0, 0.0],
        });
    }
    for i in 0..n {
        let a0 = prev[i];
        let a1 = prev[(i + 1) % n];
        let b0 = base + i as u32;
        let b1 = base + ((i + 1) % n) as u32;
        indices.extend_from_slice(&[a0, b0, b1, a0, b1, a1]);
    }

    Some(VectorMeshData {
        vertices: vertices.into(),
        indices: indices.into(),
    })
}

/// Ray-casting point-in-polygon test over an x-y ring of outline points.
fn is_convex(outline: &[[f32; 2]]) -> bool {
    let n = outline.len();
    if n < 3 {
        return false;
    }
    let mut sign = 0.0f64;
    for i in 0..n {
        let a = outline[i];
        let b = outline[(i + 1) % n];
        let c = outline[(i + 2) % n];
        let cross = ((b[0] - a[0]) as f64) * ((c[1] - b[1]) as f64)
            - ((b[1] - a[1]) as f64) * ((c[0] - b[0]) as f64);
        if cross.abs() < 1e-9 {
            continue;
        }
        if sign == 0.0 {
            sign = cross;
        } else if sign * cross < 0.0 {
            return false;
        }
    }
    true
}

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
        ModelBlendMode::Screen => BlendMode::Screen,
        ModelBlendMode::Overlay => BlendMode::Overlay,
        ModelBlendMode::Darken => BlendMode::Darken,
        ModelBlendMode::Lighten => BlendMode::Lighten,
        ModelBlendMode::ColorDodge => BlendMode::ColorDodge,
        ModelBlendMode::ColorBurn => BlendMode::ColorBurn,
        ModelBlendMode::HardLight => BlendMode::HardLight,
        ModelBlendMode::SoftLight => BlendMode::SoftLight,
        ModelBlendMode::Difference => BlendMode::Difference,
        ModelBlendMode::Exclusion => BlendMode::Exclusion,
        ModelBlendMode::Hue => BlendMode::Hue,
        ModelBlendMode::Saturation => BlendMode::Saturation,
        ModelBlendMode::Color => BlendMode::Color,
        ModelBlendMode::Luminosity => BlendMode::Luminosity,
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

fn quantized_tolerance(tolerance: f32) -> f32 {
    if !tolerance.is_finite() || tolerance <= 0.0 {
        return 0.25;
    }
    let scaled = tolerance * 1024.0;
    if !scaled.is_finite() {
        return 64.0;
    }
    let quantized = (scaled.round() / 1024.0).clamp(f32::MIN_POSITIVE, 64.0);
    if quantized > 0.0 {
        quantized
    } else {
        f32::MIN_POSITIVE
    }
}

fn push_u64(key: &mut MeshCacheKey, value: u64) {
    key.extend_from_slice(&value.to_le_bytes());
}

fn push_f64(key: &mut MeshCacheKey, value: f64) {
    push_u64(key, value.to_bits());
}

fn push_usize(key: &mut MeshCacheKey, value: usize) {
    push_u64(key, value as u64);
}

fn push_color(key: &mut MeshCacheKey, color: renamite_model::Color) {
    push_f64(key, color.r);
    push_f64(key, color.g);
    push_f64(key, color.b);
    push_f64(key, color.a);
}

fn push_point(key: &mut MeshCacheKey, point: kurbo::Point) {
    push_f64(key, point.x);
    push_f64(key, point.y);
}

fn push_path(key: &mut MeshCacheKey, path: &kurbo::BezPath) {
    push_usize(key, path.elements().len());
    for element in path.elements() {
        match *element {
            kurbo::PathEl::MoveTo(point) => {
                key.push(0);
                push_point(key, point);
            }
            kurbo::PathEl::LineTo(point) => {
                key.push(1);
                push_point(key, point);
            }
            kurbo::PathEl::QuadTo(a, b) => {
                key.push(2);
                push_point(key, a);
                push_point(key, b);
            }
            kurbo::PathEl::CurveTo(a, b, c) => {
                key.push(3);
                push_point(key, a);
                push_point(key, b);
                push_point(key, c);
            }
            kurbo::PathEl::ClosePath => key.push(4),
        }
    }
}

fn push_stops(key: &mut MeshCacheKey, stops: &GradientStops) {
    push_usize(key, stops.0.len());
    for stop in &stops.0 {
        push_f64(key, stop.offset);
        push_color(key, stop.color);
    }
}

fn push_paint(key: &mut MeshCacheKey, paint: &ScenePaint) {
    match paint {
        ScenePaint::Solid(color) => {
            key.push(0);
            push_color(key, *color);
        }
        ScenePaint::LinearGradient { start, end, stops } => {
            key.push(1);
            push_f64(key, start.x);
            push_f64(key, start.y);
            push_f64(key, end.x);
            push_f64(key, end.y);
            push_stops(key, stops);
        }
        ScenePaint::RadialGradient { center, end, stops } => {
            key.push(2);
            push_f64(key, center.x);
            push_f64(key, center.y);
            push_f64(key, end.x);
            push_f64(key, end.y);
            push_stops(key, stops);
        }
        ScenePaint::Image {
            asset,
            width,
            height,
            affine,
            tint,
        } => {
            key.push(3);
            push_u64(key, asset.data().as_ffi());
            push_usize(key, *width as usize);
            push_usize(key, *height as usize);
            for value in affine {
                push_f64(key, *value);
            }
            push_color(key, *tint);
        }
    }
}

fn push_kind(key: &mut MeshCacheKey, kind: &PaintKind) {
    match kind {
        PaintKind::Fill(rule) => {
            key.push(0);
            key.push(match rule {
                FillRule::NonZero => 0,
                FillRule::EvenOdd => 1,
            });
        }
        PaintKind::Stroke(stroke) => {
            key.push(1);
            push_f64(key, stroke.width);
            key.push(match stroke.cap {
                renamite_model::StrokeCap::Butt => 0,
                renamite_model::StrokeCap::Round => 1,
                renamite_model::StrokeCap::Square => 2,
            });
            key.push(match stroke.join {
                renamite_model::StrokeJoin::Miter => 0,
                renamite_model::StrokeJoin::Round => 1,
                renamite_model::StrokeJoin::Bevel => 2,
            });
            push_f64(key, stroke.miter_limit);
            match &stroke.dash {
                None => key.push(0),
                Some(dash) => {
                    key.push(1);
                    push_f64(key, dash.offset);
                    push_usize(key, dash.dashes.len());
                    for value in &dash.dashes {
                        push_f64(key, *value);
                    }
                }
            }
        }
    }
}

fn mesh_key(item: &SceneItem, tolerance: f32) -> MeshCacheKey {
    let mut key = Vec::new();
    key.push(0);
    push_f64(&mut key, tolerance as f64);
    push_f64(&mut key, item.opacity);
    push_paint(&mut key, &item.paint);
    push_path(&mut key, &item.path);
    push_kind(&mut key, &item.kind);
    key
}

fn clip_key(path: &kurbo::BezPath, rule: FillRule, tolerance: f32) -> MeshCacheKey {
    let mut key = Vec::new();
    key.push(1);
    push_f64(&mut key, tolerance as f64);
    push_path(&mut key, path);
    key.push(match rule {
        FillRule::NonZero => 0,
        FillRule::EvenOdd => 1,
    });
    key
}
