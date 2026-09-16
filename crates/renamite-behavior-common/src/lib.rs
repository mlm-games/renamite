//! Shared context for tool behaviors: selection, view transform, snapping.

pub mod align;
pub mod assets;
pub mod color;
pub mod context_menu;
pub mod fill;
pub mod inspect;
pub mod layers;
pub mod machine;
pub mod modifiers;
pub mod path;
pub mod stroke;

use glam::DVec2;
use renamite_animation::Frame;
use renamite_model::{CompId, Document, NodeId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Selection {
    pub nodes: Vec<NodeId>,
    /// Optional focus target when editing a group/precomp's contents.
    pub comp: Option<CompId>,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn contains(&self, id: NodeId) -> bool {
        self.nodes.contains(&id)
    }
}

/// Screen ↔ world mapping. Px tolerance in world units.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewTransform {
    pub scale: f64,
    pub offset: DVec2,
}

impl ViewTransform {
    pub fn identity() -> Self {
        Self {
            scale: 1.0,
            offset: DVec2::ZERO,
        }
    }

    pub fn screen_to_world(&self, p: DVec2) -> DVec2 {
        (p - self.offset) / self.scale
    }

    pub fn world_to_screen(&self, p: DVec2) -> DVec2 {
        p * self.scale + self.offset
    }

    /// Tolerance in world units for a sub-pixel screen tolerance (0.25px).
    pub fn world_tolerance(&self, px: f64) -> f64 {
        px / self.scale
    }

    /// Zoom about `screen_pos` by `factor`, clamped to `[min, max]`.
    /// Shared by canvas viewport and machine graph to stay DRY.
    pub fn zoom_at(&mut self, screen_pos: DVec2, factor: f64, min: f64, max: f64) {
        let world = self.screen_to_world(screen_pos);
        self.scale = (self.scale * factor).clamp(min, max);
        self.offset = screen_pos - world * self.scale;
    }

    pub fn pan_by(&mut self, delta: DVec2) {
        self.offset += delta;
    }

    /// Fit `artboard` inside `surface` with a margin, centering it.
    /// Degenerate inputs are no-ops so an empty surface or composition
    /// can never collapse the zoom.
    pub fn fit(&mut self, surface: DVec2, artboard: DVec2) {
        fit_view(self, surface, artboard);
    }
}

/// Margin-fit shared by the editor viewport and the player embed:
/// `artboard` inside `surface` with a 56 px margin, centered.
/// No-op on degenerate inputs.
pub fn fit_view(view: &mut ViewTransform, surface: DVec2, artboard: DVec2) {
    if surface.x <= 1.0 || surface.y <= 1.0 || artboard.x <= 0.0 || artboard.y <= 0.0 {
        return;
    }
    let margin = 56.0;
    let available = (surface - DVec2::splat(margin * 2.0)).max(DVec2::splat(1.0));
    let scale = (available.x / artboard.x)
        .min(available.y / artboard.y)
        .clamp(0.05, 32.0);
    view.scale = scale;
    view.offset = (surface - artboard * scale) * 0.5;
}

/// Exact-fit shared by presentational embeds: `artboard` fills `surface`
/// with no margin, letterboxing inside the surface when aspects differ.
/// No-op on degenerate inputs.
pub fn fit_exact_view(view: &mut ViewTransform, surface: DVec2, artboard: DVec2) {
    if surface.x <= 1.0 || surface.y <= 1.0 || artboard.x <= 0.0 || artboard.y <= 0.0 {
        return;
    }
    let scale = (surface.x / artboard.x)
        .min(surface.y / artboard.y)
        .clamp(0.05, 64.0);
    view.scale = scale;
    view.offset = (surface - artboard * scale) * 0.5;
}

/// Fit-state tracker shared by the editor viewport and the player embed:
/// remembers the last fitted surface (+ artboard) and only refits on
/// resize or explicit invalidation, so interactive zoom survives redraws.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FitState {
    surface: DVec2,
    artboard: DVec2,
    pub pending: bool,
}

impl FitState {
    pub fn new() -> Self {
        Self {
            surface: DVec2::ZERO,
            artboard: DVec2::ZERO,
            pending: true,
        }
    }

    /// Pre-seed the surface record (e.g. after an explicit `fit` call) so
    /// zoom anchors work even when the artboard was degenerate.
    pub fn with_surface(surface: DVec2) -> Self {
        Self {
            surface,
            artboard: DVec2::ZERO,
            pending: false,
        }
    }

    /// Refit `view` when the surface/artboard changed beyond 0.5 px or a
    /// fit was requested via [`FitState::request`]. Returns true when a
    /// refit ran. Pan gestures opt out by skipping this call.
    ///
    /// `refit_on_resize`: the player embed refits on window resize
    /// (`true`); the editor viewport keeps the user's zoom on resize
    /// (`false`) and only refits on first layout, artboard change, or
    /// explicit request.
    pub fn ensure(
        &mut self,
        view: &mut ViewTransform,
        surface: DVec2,
        artboard: DVec2,
        refit_on_resize: bool,
    ) -> bool {
        let resized = (surface - self.surface).abs().max_element() > 0.5;
        let art_changed = (artboard - self.artboard).abs().max_element() > 0.5;
        let first_layout = self.surface == DVec2::ZERO && surface != DVec2::ZERO;
        self.surface = surface;
        if self.pending || first_layout || art_changed || (refit_on_resize && resized) {
            view.fit(surface, artboard);
            self.artboard = artboard;
            self.pending = false;
            true
        } else {
            false
        }
    }

    /// Request a refit on the next [`FitState::ensure`] call
    /// (e.g. after a zoom-to-fit shortcut).
    pub fn request(&mut self) {
        self.pending = true;
    }

    pub fn surface_size(&self) -> DVec2 {
        self.surface
    }

    /// Zoom about the surface center; no-op before the first layout.
    pub fn zoom_centered(&self, view: &mut ViewTransform, factor: f64) {
        if self.surface == DVec2::ZERO {
            return;
        }
        self.zoom_at(view, self.surface * 0.5, factor);
    }

    /// Zoom about `screen_pos`; no-op before the first layout.
    pub fn zoom_at(&self, view: &mut ViewTransform, screen_pos: DVec2, factor: f64) {
        if self.surface == DVec2::ZERO {
            return;
        }
        view.zoom_at(screen_pos, factor, 0.05, 64.0);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Modifiers {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl Modifiers {
    pub fn none() -> Self {
        Self {
            shift: false,
            alt: false,
            ctrl: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SnapConfig {
    pub grid: Option<f64>,
    pub anchor: bool,
    pub guide: bool,
}

pub struct ToolContext<'a> {
    pub doc: &'a Document,
    /// Evaluated frame currently on screen - the hit-test surface.
    pub scene: &'a renamite_model::Scene,
    pub comp: CompId,
    pub selection: &'a Selection,
    pub playhead: Frame,
    pub record: bool,
    pub view: ViewTransform,
    pub snap: SnapConfig,
    pub modifiers: Modifiers,
    /// Current paint used by the Fill tool (set from Properties or a future picker).
    pub current_paint: &'a renamite_model::StylePaint,
}
