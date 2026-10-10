//! Pure geometric edits: align, distribute, flip and nudge.
//!
//! UI-agnostic, like [`crate::pathfinder`]: read a document, a selection and a
//! frame; answer the `EditorCommand`s that move the selection. The scene is
//! evaluated here with [`renamite_model::evaluate`] rather than taken from a
//! live session, so the editor and the MCP server run identical code.
//!
//! Design stays static and Animate keys: every move is authored through
//! [`renamite_history::resolve_property_edit`], which picks `AddKeyframe` or
//! `SetStatic` from the property's animated state and the `record` flag.

use glam::DVec2;

use crate::align::{self, AlignAnchor, AlignOp};
use renamite_animation::Frame;
use renamite_history::{EditorCommand, resolve_property_edit};
use renamite_model::{Document, NodeId, PropPath, Value, evaluate, world_delta_to_parent};

use crate::pathfinder::{geometric_target, selection_roots};

const POSITION: &str = "transform.position";
const SCALE: &str = "transform.scale";
const EPS_SQ: f64 = 1e-24;

/// World-space deltas to add to each selected root. `None` per entry means the
/// root could not be resolved or moved.
fn resolve_targets(doc: &Document, ids: &[NodeId]) -> Vec<Option<NodeId>> {
    ids.iter().map(|&id| geometric_target(doc, id)).collect()
}

/// Author a world delta per root into commands. Locked nodes and non-finite
/// deltas are skipped; an empty answer means there was nothing to move.
pub fn move_commands(
    doc: &Document,
    ids: &[NodeId],
    frame: f64,
    record: bool,
    deltas: &[(NodeId, DVec2)],
) -> Vec<EditorCommand> {
    let targets: Vec<Option<NodeId>> = ids.iter().map(|&id| geometric_target(doc, id)).collect();
    let target_of: std::collections::HashMap<NodeId, Option<NodeId>> =
        ids.iter().copied().zip(targets).collect();
    let prop = PropPath::new(POSITION);
    let mut cmds = Vec::new();
    for (root, delta) in deltas {
        if !delta.is_finite() || delta.length_squared() < EPS_SQ {
            continue;
        }
        let Some(Some(id)) = target_of.get(root) else {
            continue;
        };
        if doc.nodes.get(*id).map(|n| n.locked).unwrap_or(true) {
            continue;
        }
        let Ok(Value::DVec2(current)) = doc.value_at(*id, &prop, frame) else {
            continue;
        };
        let local = world_delta_to_parent(doc, *id, frame, *delta).unwrap_or(*delta);
        if !local.is_finite() {
            continue;
        }
        cmds.push(resolve_property_edit(
            doc,
            *id,
            &prop,
            Value::DVec2(current + local),
            Frame(frame.round() as i64),
            record,
        ));
    }
    cmds
}

/// Align the selection to itself or to the page.
///
/// The scene is evaluated at `frame` so groups align by their rendered bounds.
pub fn align(
    doc: &Document,
    comp: renamite_model::CompId,
    selection: &[NodeId],
    frame: f64,
    record: bool,
    op: AlignOp,
    anchor: AlignAnchor,
) -> Option<Vec<EditorCommand>> {
    let roots = selection_roots(doc, selection);
    if roots.is_empty() {
        return None;
    }
    let scene = evaluate(doc, comp, frame);
    let bounds = align::root_bounds(doc, &scene.items, &roots);
    if bounds.is_empty() {
        return None;
    }
    let target = match anchor {
        AlignAnchor::Selection => align::union_bounds(&bounds)?,
        AlignAnchor::Page => {
            let size = doc
                .main_composition()
                .map(|composition| composition.size)
                .unwrap_or((512, 512));
            align::page_bounds(size)
        }
    };
    let cmds = move_commands(doc, &roots, frame, record, &align::align_deltas(&bounds, target, op));
    (!cmds.is_empty()).then_some(cmds)
}

/// Evenly space 3+ selected roots between the first and last center.
pub fn distribute(
    doc: &Document,
    comp: renamite_model::CompId,
    selection: &[NodeId],
    frame: f64,
    record: bool,
    horizontal: bool,
) -> Option<Vec<EditorCommand>> {
    let roots = selection_roots(doc, selection);
    if roots.len() < 3 {
        return None;
    }
    let scene = evaluate(doc, comp, frame);
    let bounds = align::root_bounds(doc, &scene.items, &roots);
    if bounds.len() < 3 {
        return None;
    }
    let deltas = align::distribute_deltas(&bounds, horizontal)?;
    let cmds = move_commands(doc, &roots, frame, record, &deltas);
    (!cmds.is_empty()).then_some(cmds)
}

/// Mirror the selection's scale about its own origin.
pub fn flip(
    doc: &Document,
    selection: &[NodeId],
    frame: f64,
    record: bool,
    horizontal: bool,
) -> Option<Vec<EditorCommand>> {
    let roots = selection_roots(doc, selection);
    if roots.is_empty() {
        return None;
    }
    let targets = resolve_targets(doc, &roots);
    let prop = PropPath::new(SCALE);
    let mut cmds = Vec::new();
    for target in targets {
        let Some(id) = target else {
            continue;
        };
        if doc.nodes.get(id).map(|n| n.locked).unwrap_or(true) {
            continue;
        }
        let Ok(Value::DVec2(current)) = doc.value_at(id, &prop, frame) else {
            continue;
        };
        let next = if horizontal {
            DVec2::new(-current.x, current.y)
        } else {
            DVec2::new(current.x, -current.y)
        };
        if !next.is_finite() {
            continue;
        }
        cmds.push(resolve_property_edit(
            doc,
            id,
            &prop,
            Value::DVec2(next),
            Frame(frame.round() as i64),
            record,
        ));
    }
    (!cmds.is_empty()).then_some(cmds)
}

/// Nudge the selection by a world-space delta.
///
/// The delta is converted into each target's parent space so nested layers move
/// correctly. `nil` if it moves nothing.
pub fn nudge(
    doc: &Document,
    selection: &[NodeId],
    frame: f64,
    record: bool,
    delta: DVec2,
) -> Option<Vec<EditorCommand>> {
    if !delta.is_finite() {
        return None;
    }
    let roots = selection_roots(doc, selection);
    if roots.is_empty() {
        return None;
    }
    let targets = resolve_targets(doc, &roots);
    let prop = PropPath::new(POSITION);
    let mut cmds = Vec::new();
    for target in targets {
        let Some(id) = target else {
            continue;
        };
        if doc.nodes.get(id).map(|n| n.locked).unwrap_or(true) {
            continue;
        }
        let Ok(Value::DVec2(current)) = doc.value_at(id, &prop, frame) else {
            continue;
        };
        let local = world_delta_to_parent(doc, id, frame, delta).unwrap_or(delta);
        if !local.is_finite() {
            continue;
        }
        cmds.push(resolve_property_edit(
            doc,
            id,
            &prop,
            Value::DVec2(current + local),
            Frame(frame.round() as i64),
            record,
        ));
    }
    (!cmds.is_empty()).then_some(cmds)
}
