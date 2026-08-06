//! Command builders for adding shape modifiers to a group.

use renamite_animation::Animated;
use renamite_history::{EditorCommand, NodeTree};
use renamite_model::{ModifierKind, Node, NodeId, NodeKind, Parent, TrimMode};

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