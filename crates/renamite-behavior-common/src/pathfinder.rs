//! Pure Pathfinder authoring: boolean, divide, combine and break-apart.
//!
//! UI-agnostic by design. Each entry point reads a document, a selection and a
//! frame, and answers the `EditorCommand`s that perform the operation plus the
//! selection to leave behind, with no reference to a `Session`. The editor
//! shell and the MCP server therefore run the same code and cannot diverge.
//!
//! Every operation folds its commands into one undo step: callers wrap them in
//! `ToolOutput::BeginTransaction` / `CommitTransaction`, or `History::begin` /
//! `History::commit` around a `History::apply` of each.

use std::collections::HashSet;

use glam::DVec2;

use renamite_animation::Animated;
use renamite_geometry::{BooleanOp, VectorPath, boolean_bez, contours_to_bez, split_bez_subpaths};
use renamite_history::{EditorCommand, NodeTree, SelectionChange};
use renamite_model::{
    CompId, Document, Node, NodeId, NodeKind, Overrides, Parent, ShapeKind,
};

/// Which Pathfinder operation to run over the selected shape roots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeBoolean {
    Union,
    Difference,
    Intersection,
    Xor,
}

/// Z-order holes: a shape root is a shape node whose ancestors include a group
/// or layer, with no group/layer/comp descendant between the selection and it.
pub fn shape_roots_in_z_order(doc: &Document, comp: CompId, ids: &[NodeId]) -> Vec<NodeId> {
    fn visit(
        doc: &Document,
        children: &[NodeId],
        sel: &[NodeId],
        inherited_selected: bool,
        out: &mut Vec<NodeId>,
    ) {
        for &id in children.iter().rev() {
            let Some(node) = doc.nodes.get(id) else {
                continue;
            };
            let selected = inherited_selected || sel.contains(&id);

            match &node.kind {
                NodeKind::Shape(_) => {
                    if selected {
                        out.push(id);
                    }
                }
                NodeKind::Group | NodeKind::Layer(_) => {
                    visit(doc, &node.children, sel, selected, out);
                }
                _ => {}
            }
        }
    }

    let mut out = Vec::new();
    if let Some(composition) = doc.compositions.get(comp) {
        visit(doc, &composition.children, ids, false, &mut out);
    }
    out
}

/// Z-order holes: any node id (checked against the mask) is a painting target.
pub fn selection_roots(doc: &Document, ids: &[NodeId]) -> Vec<NodeId> {
    let attached: Vec<NodeId> = ids
        .iter()
        .copied()
        .filter(|&id| node_is_attached(doc, id))
        .collect();
    let selected: HashSet<NodeId> = attached.iter().copied().collect();
    attached
        .into_iter()
        .filter(|&id| {
            let mut current = id;
            let mut seen = HashSet::new();
            while let Some(node) = doc.nodes.get(current) {
                if !seen.insert(current) {
                    return false;
                }
                let Some(parent) = node.parent else {
                    break;
                };
                if selected.contains(&parent) {
                    return false;
                }
                current = parent;
            }
            true
        })
        .collect()
}

/// A shape's contours expressed in the subject's space, at `frame`.
///
/// The subject is the first selected root and stays put; every other shape is
/// transformed into its coordinate space so booleans compose without nesting.
pub fn contours_in_subject_space(
    doc: &Document,
    id: NodeId,
    subject: NodeId,
    frame: f64,
) -> Result<Vec<VectorPath>, String> {
    let Some(node) = doc.nodes.get(id) else {
        return Err("Selected node no longer exists".into());
    };
    let local: kurbo::BezPath = match &node.kind {
        NodeKind::Shape(ShapeKind::CompoundPath(compound)) => compound.to_bez_path(frame),
        NodeKind::Shape(shape) => renamite_model::shape_path(shape, id, frame, &Overrides::default()),
        _ => return Err("Selection contains non-shape nodes".into()),
    };

    let Some(from) = renamite_model::node_transform_context(doc, id, frame) else {
        return Err("Cannot resolve transforms for the selection".into());
    };
    let Some(to) = renamite_model::node_transform_context(doc, subject, frame) else {
        return Err("Cannot resolve transforms for the selection".into());
    };
    let to_subject_space = to.world.inverse() * from.world;

    let mapped = to_subject_space * local;
    Ok(split_bez_subpaths(&mapped))
}

/// Wrap contours back into the tightest shape kind: one contour is a plain
/// `Path`, several are a `CompoundPath`.
pub fn shape_kind_from_contours(contours: Vec<VectorPath>) -> Option<ShapeKind> {
    match contours.len() {
        0 => None,
        1 => contours
            .into_iter()
            .next()
            .map(|c| ShapeKind::Path(Animated::new(c))),
        _ => Some(ShapeKind::CompoundPath(renamite_model::CompoundPath {
            contours: contours.into_iter().map(Animated::new).collect(),
        })),
    }
}

/// An anchor's world bounds, for aligning to the page.
pub fn page_bounds_world(size: (u32, u32)) -> (DVec2, DVec2) {
    (DVec2::ZERO, DVec2::new(size.0 as f64, size.1 as f64))
}

/// Roots that can carry a geometric edit (align/distribute/flip/nudge): style
/// and modifier nodes have no honored transform (see
/// `renamite_model::node_supports_transform`), so resolve them to the shape they
/// paint or affect instead of writing dead values. Returns `None` when the node
/// is not a geometric carrier and no carrier exists.
pub fn geometric_target(doc: &Document, id: NodeId) -> Option<NodeId> {
    let node = doc.nodes.get(id)?;
    if renamite_model::node_supports_transform(&node.kind) {
        return Some(id);
    }
    match &node.kind {
        NodeKind::Style(_) => painted_shape_for(doc, id),
        NodeKind::Modifier(_) => {
            let parent = node.parent?;
            let parent_node = doc.nodes.get(parent)?;
            if renamite_model::node_supports_transform(&parent_node.kind) {
                Some(parent)
            } else {
                geometric_target(doc, parent)
            }
        }
        _ => None,
    }
}

/// The shape a paint node affects: the nearest preceding sibling that paints.
fn painted_shape_for(doc: &Document, id: NodeId) -> Option<NodeId> {
    let node = doc.nodes.get(id)?;
    if !matches!(node.kind, NodeKind::Style(_)) {
        return None;
    }
    let (parent, idx) = doc.locate(id)?;
    let siblings: Vec<NodeId> = match parent {
        Parent::Comp(c) => doc.compositions.get(c)?.children.clone(),
        Parent::Node(n) => doc.nodes.get(n)?.children.clone(),
    };
    for &sid in siblings[..idx].iter().rev() {
        match doc.nodes.get(sid).map(|n| &n.kind) {
            Some(NodeKind::Shape(_) | NodeKind::Text(_) | NodeKind::Image(_)) => return Some(sid),
            Some(NodeKind::Style(_) | NodeKind::Modifier(_)) => continue,
            _ => break,
        }
    }
    None
}

/// The parent and index a node occupies, for inserting siblings after it.
fn locate(doc: &Document, id: NodeId) -> Option<(Parent, usize)> {
    match doc.locate(id)? {
        (Parent::Node(p), i) => Some((Parent::Node(p), i)),
        (Parent::Comp(c), i) => Some((Parent::Comp(c), i)),
    }
}

fn node_name(doc: &Document, id: NodeId, fallback: &str) -> String {
    doc.nodes
        .get(id)
        .map(|n| n.name.clone())
        .unwrap_or_else(|| fallback.into())
}

/// One folder of Pathfinder work: a label for the undo step, the commands to
/// apply, and what should be selected once they land.
pub struct PathfinderEdit {
    pub label: String,
    pub commands: Vec<EditorCommand>,
    pub selection: SelectionChange,
}

/// Union / Difference / Intersection / Xor across the selected shape roots.
///
/// The subject keeps its position and receives the result; the other shapes are
/// removed. A result with no geometry deletes every selected shape instead.
pub fn boolean(
    doc: &Document,
    comp: CompId,
    selection: &[NodeId],
    frame: f64,
    operation: ShapeBoolean,
) -> Result<PathfinderEdit, String> {
    let ids = shape_roots_in_z_order(doc, comp, selection);
    if ids.len() < 2 {
        return Err("Select at least two closed shapes".into());
    }

    let subject = ids[0];
    let mut accumulated = contours_in_subject_space(doc, subject, subject, frame)?;
    if accumulated.is_empty() {
        return Err("The subject has no geometry".into());
    }

    let op = match operation {
        ShapeBoolean::Union => BooleanOp::Union,
        ShapeBoolean::Difference => BooleanOp::Difference,
        ShapeBoolean::Intersection => BooleanOp::Intersection,
        ShapeBoolean::Xor => BooleanOp::Xor,
    };

    for cutter in ids.iter().copied().skip(1) {
        let rhs = contours_in_subject_space(doc, cutter, subject, frame)?;
        let result = boolean_bez(
            &contours_to_bez(&accumulated),
            &contours_to_bez(&rhs),
            op,
        )
        .map_err(|e| e.to_string())?;
        accumulated = result;
    }

    let label = format!("{operation:?}");

    if accumulated.is_empty() {
        return Ok(PathfinderEdit {
            label,
            commands: ids
                .iter()
                .copied()
                .map(|id| EditorCommand::RemoveNode { id })
                .collect(),
            selection: SelectionChange::Set(Vec::new()),
        });
    }

    let mut commands = vec![EditorCommand::SetNodeKind {
        id: subject,
        kind: NodeKind::Shape(
            shape_kind_from_contours(accumulated).ok_or("Boolean produced no geometry")?,
        ),
    }];
    for id in ids.iter().copied().skip(1) {
        commands.push(EditorCommand::RemoveNode { id });
    }

    Ok(PathfinderEdit {
        label,
        commands,
        selection: SelectionChange::Set(vec![subject]),
    })
}

/// Cut the subject by every other selected shape; each cut becomes a piece.
pub fn divide(
    doc: &Document,
    comp: CompId,
    selection: &[NodeId],
    frame: f64,
) -> Result<PathfinderEdit, String> {
    let ids = shape_roots_in_z_order(doc, comp, selection);
    if ids.len() < 2 {
        return Err("Select at least two closed shapes to divide".into());
    }
    let subject = ids[0];

    for &id in &ids {
        let closed = match doc.nodes.get(id).map(|n| &n.kind) {
            Some(NodeKind::Shape(ShapeKind::Path(p))) => p.value_at(frame).closed,
            Some(NodeKind::Shape(ShapeKind::CompoundPath(c))) => {
                c.contours.iter().all(|p| p.value_at(frame).closed)
            }
            Some(NodeKind::Shape(_)) => true,
            _ => false,
        };
        if !closed {
            return Err("Division requires closed shapes (open cutting not supported)".into());
        }
    }

    let mut pieces: Vec<Vec<VectorPath>> = Vec::new();
    let mut remaining = contours_in_subject_space(doc, subject, subject, frame)?;

    for cutter in ids.iter().copied().skip(1) {
        let rhs = contours_in_subject_space(doc, cutter, subject, frame)?;
        let rhs_bez = contours_to_bez(&rhs);

        let current = std::mem::take(&mut remaining);
        if current.is_empty() {
            break;
        }
        let cur_bez = contours_to_bez(&current);

        let (inside, outside) = (
            boolean_bez(&cur_bez, &rhs_bez, BooleanOp::Intersection).map_err(|e| e.to_string())?,
            boolean_bez(&cur_bez, &rhs_bez, BooleanOp::Difference).map_err(|e| e.to_string())?,
        );
        if !inside.is_empty() {
            pieces.push(inside);
        }
        remaining = outside;

        if remaining.is_empty() {
            break;
        }
    }
    if !remaining.is_empty() {
        pieces.push(remaining);
    }

    let output_kinds: Vec<ShapeKind> = pieces
        .into_iter()
        .filter_map(shape_kind_from_contours)
        .collect();
    if output_kinds.is_empty() {
        return Err("Division produced no geometry".into());
    }

    let (parent, index) =
        locate(doc, subject).ok_or_else(|| "Subject is not attached".to_string())?;
    let template_name = node_name(doc, subject, "Piece");

    let mut commands = vec![EditorCommand::SetNodeKind {
        id: subject,
        kind: NodeKind::Shape(output_kinds[0].clone()),
    }];

    for (k, kind) in output_kinds.iter().enumerate().skip(1) {
        let mut node = doc
            .nodes
            .get(subject)
            .cloned()
            .unwrap_or_else(|| Node::new(template_name.as_str(), NodeKind::Group));
        node.name = format!("{template_name} {}", k + 1);
        node.parent = None;
        node.children.clear();
        node.kind = NodeKind::Shape(kind.clone());
        commands.push(EditorCommand::InsertNode {
            parent,
            index: index + k,
            tree: NodeTree::leaf(node),
        });
    }

    for id in ids.iter().copied().skip(1) {
        commands.push(EditorCommand::RemoveNode { id });
    }

    Ok(PathfinderEdit {
        label: "Division".into(),
        commands,
        selection: SelectionChange::Set(vec![subject]),
    })
}

/// Flatten a compound path into its contours; the subject keeps the first.
pub fn break_apart(
    doc: &Document,
    selection: &[NodeId],
    frame: f64,
) -> Result<PathfinderEdit, String> {
    if selection.len() != 1 {
        return Err("Select one combined path to break apart".into());
    }
    let id = selection[0];

    let Some(contours) = (match doc.nodes.get(id).map(|n| &n.kind) {
        Some(NodeKind::Shape(ShapeKind::CompoundPath(compound))) => Some(
            compound
                .contours
                .iter()
                .map(|c| c.value_at(frame))
                .collect::<Vec<_>>(),
        ),
        _ => None,
    }) else {
        return Err("Break apart requires a compound path".into());
    };
    if contours.len() < 2 {
        return Err("The compound path has a single contour".into());
    }

    let (parent, index) = locate(doc, id).ok_or_else(|| "Node is not attached".to_string())?;
    let name = node_name(doc, id, "Path");

    let mut commands = vec![EditorCommand::SetNodeKind {
        id,
        kind: NodeKind::Shape(ShapeKind::Path(Animated::new(contours[0].clone()))),
    }];

    for (k, contour) in contours.iter().enumerate().skip(1) {
        let mut node = doc
            .nodes
            .get(id)
            .cloned()
            .unwrap_or_else(|| Node::new(name.as_str(), NodeKind::Group));
        node.name = format!("{name} {}", k + 1);
        node.parent = None;
        node.children.clear();
        node.kind = NodeKind::Shape(ShapeKind::Path(Animated::new(contour.clone())));
        commands.push(EditorCommand::InsertNode {
            parent,
            index: index + k,
            tree: NodeTree::leaf(node),
        });
    }

    Ok(PathfinderEdit {
        label: "Break apart".into(),
        commands,
        selection: SelectionChange::Set(vec![id]),
    })
}

/// Merge every selected shape into one compound path; the subject keeps it.
pub fn combine(
    doc: &Document,
    comp: CompId,
    selection: &[NodeId],
    frame: f64,
) -> Result<PathfinderEdit, String> {
    let ids = shape_roots_in_z_order(doc, comp, selection);
    if ids.len() < 2 {
        return Err("Select at least two objects to combine".into());
    }
    let bottom = ids[0];

    let mut contours: Vec<VectorPath> = Vec::new();
    for id in &ids {
        contours.extend(contours_in_subject_space(doc, *id, bottom, frame)?);
    }
    if contours.is_empty() {
        return Err("Nothing to combine".into());
    }

    let mut commands = vec![EditorCommand::SetNodeKind {
        id: bottom,
        kind: NodeKind::Shape(ShapeKind::CompoundPath(renamite_model::CompoundPath {
            contours: contours.into_iter().map(Animated::new).collect(),
        })),
    }];
    for id in ids.iter().copied().skip(1) {
        commands.push(EditorCommand::RemoveNode { id });
    }

    Ok(PathfinderEdit {
        label: "Combine".into(),
        commands,
        selection: SelectionChange::Set(vec![bottom]),
    })
}

/// Whether a node still hangs off exactly one composition. Cycles stop the
/// walk, and a node with both a parent and a composition membership is corrupt.
pub fn node_is_attached(doc: &Document, id: NodeId) -> bool {
    let mut current = id;
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(current) {
            return false;
        }
        let Some(node) = doc.nodes.get(current) else {
            return false;
        };
        let root_memberships: usize = doc
            .compositions
            .values()
            .map(|composition| {
                composition
                    .children
                    .iter()
                    .filter(|&&child| child == current)
                    .count()
            })
            .sum();
        if node.parent.is_some() && root_memberships != 0 {
            return false;
        }
        let Some(parent) = node.parent else {
            return root_memberships == 1;
        };
        let Some(parent_node) = doc.nodes.get(parent) else {
            return false;
        };
        if parent_node
            .children
            .iter()
            .filter(|&&child| child == current)
            .count()
            != 1
        {
            return false;
        }
        current = parent;
    }
}
