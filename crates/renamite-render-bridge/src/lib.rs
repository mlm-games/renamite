//! Scene -> Repose render bridge.
//!
//! Tessellates `renamite_model::Scene` items into mesh `DrawCommand`s via lyon,
//! with a mesh cache keyed by item + view scale so sub-pixel-stable geometry is
//! reused across frames. This crate is one of the two allowed to touch
//! `repose-render-wgpu`.

use renamite_behavior_common::ViewTransform;
use renamite_model::{Scene, SceneItem};
use rustc_hash::FxHashMap;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct MeshKey(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CachedMesh;

pub struct SceneRenderer {
    mesh_cache: FxHashMap<MeshKey, CachedMesh>,
    fill_tess: lyon_tessellation::FillTessellator,
    stroke_tess: lyon_tessellation::StrokeTessellator,
}

impl Default for SceneRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl SceneRenderer {
    pub fn new() -> Self {
        Self {
            mesh_cache: FxHashMap::default(),
            fill_tess: lyon_tessellation::FillTessellator::new(),
            stroke_tess: lyon_tessellation::StrokeTessellator::new(),
        }
    }

    /// Tolerance in world units for sub-pixel stability under zoom.
    pub fn world_tolerance(view: &ViewTransform) -> f64 {
        0.25 / view.scale
    }

    /// Append tessellated draw commands for `scene` into `out`.
    pub fn draw(
        &mut self,
        _scene: &Scene,
        _view: &ViewTransform,
        _out: &mut Vec<repose_canvas::DrawCommand>,
    ) {
        // TODO: BezPath -> lyon path -> tessellate fill/stroke -> mesh cache -> command.
    }

    fn key_for(item: &SceneItem, view: &ViewTransform) -> MeshKey {
        let _ = (item, view);
        MeshKey(0)
    }
}