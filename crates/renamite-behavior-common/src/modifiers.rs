//! Command builders for adding shape modifiers to a group.

use renamite_animation::Animated;
use renamite_history::{EditorCommand, NodeTree};
use renamite_model::{Document, ModifierKind, Node, NodeId, NodeKind, Parent, TrimMode};

/// Append a Trim Path modifier as the last child of `parent` (a group).
/// Placed at the end so it applies after all sibling shapes in pass 1.
pub fn cmd_add_trim_path_to(parent: NodeId) -> EditorCommand {
    EditorCommand::InsertNode {
        parent: Parent::Node(parent),
        index: usize::MAX,
        tree: NodeTree::leaf(Node::new(
            "Trim Path",
            NodeKind::Modifier(ModifierKind::TrimPath {
                start: Animated::new(0.0),
                end: Animated::new(1.0),
                offset: Animated::new(0.0),
                mode: TrimMode::Individually,
            }),
        )),
    }
}

/// Append a Trim Path modifier as a sibling immediately after `after`
/// (e.g. right after the selected shape, before its style children).
pub fn cmd_add_trim_path_after(doc: &Document, after: NodeId) -> Option<EditorCommand> {
    let (parent, index) = doc.locate(after)?;
    Some(EditorCommand::InsertNode {
        parent,
        index: index + 1,
        tree: NodeTree::leaf(Node::new(
            "Trim Path",
            NodeKind::Modifier(ModifierKind::TrimPath {
                start: Animated::new(0.0),
                end: Animated::new(1.0),
                offset: Animated::new(0.0),
                mode: TrimMode::Individually,
            }),
        )),
    })
}

/// Append a Round Corners modifier as a sibling immediately after `after`.
pub fn cmd_add_round_corners_after(
    doc: &Document,
    after: NodeId,
    radius: f64,
) -> Option<EditorCommand> {
    let (parent, index) = doc.locate(after)?;
    Some(EditorCommand::InsertNode {
        parent,
        index: index + 1,
        tree: NodeTree::leaf(Node::new(
            "Round Corners",
            NodeKind::Modifier(ModifierKind::RoundCorners {
                radius: Animated::new(radius),
            }),
        )),
    })
}

/// Current TrimMode of a Trim Path node, if it is one.
pub fn get_trim_mode(doc: &Document, id: NodeId) -> Option<TrimMode> {
    match &doc.nodes.get(id)?.kind {
        NodeKind::Modifier(ModifierKind::TrimPath { mode, .. }) => Some(*mode),
        _ => None,
    }
}

/// Set a Trim Path node's mode to `mode`; None if it is already there or the
/// node is not a Trim Path. `trim.mode` is an enum field (not `Animated<T>`),
/// so it bypasses the generic property path entirely.
pub fn cmd_set_trim_mode(doc: &Document, id: NodeId, mode: TrimMode) -> Option<EditorCommand> {
    if get_trim_mode(doc, id)? == mode {
        return None;
    }
    Some(EditorCommand::SetTrimMode { id, mode })
}

/// Flip a Trim Path node's mode (used by the toggle in the Properties panel).
pub fn cmd_toggle_trim_mode(doc: &Document, id: NodeId) -> Option<EditorCommand> {
    let cur = get_trim_mode(doc, id)?;
    let next = match cur {
        TrimMode::Individually => TrimMode::Simultaneously,
        TrimMode::Simultaneously => TrimMode::Individually,
    };
    cmd_set_trim_mode(doc, id, next)
}
