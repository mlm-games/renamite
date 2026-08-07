//! Shared context for tool behaviors: selection, view transform, snapping.

pub mod assets;
pub mod color;
pub mod context_menu;
pub mod fill;
pub mod inspect;
pub mod layers;
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

/// Screen ↔ world mapping; px tolerance in world units.
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
