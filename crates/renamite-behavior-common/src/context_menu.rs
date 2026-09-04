//! Context-menu models and command builders. Pure.

use glam::DVec2;
use renamite_animation::Animated;
use renamite_history::{EditorCommand, NodeTree, SelectionChange, ToolId, ToolOutput};
use renamite_model::{
    CompId, Document, FillRule, Node, NodeId, NodeKind, Parent, ShapeKind, StarKind, StyleKind,
    StylePaint, TextAlign, TextNode,
};
use smallvec::{SmallVec, smallvec};

/// One row (or submenu) in a context menu.
#[derive(Clone, Debug, PartialEq)]
pub enum MenuEntry {
    Action {
        id: MenuAction,
        label: &'static str,
        enabled: bool,
        /// Material symbol hint for the host (needed in newer rel.).
        icon: Option<&'static str>,
    },
    Submenu {
        label: &'static str,
        children: Vec<MenuEntry>,
    },
    Separator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuAction {
    Rename,
    Duplicate,
    Delete,
    Cut,
    Copy,
    Paste,
    Group,
    Ungroup,
    ToggleVisible,
    ToggleLocked,
    BringForward,
    SendBackward,
    BringToFront,
    SendToBack,
    AddTrimPath,
    AddOffsetPath,
    AddRoundCorners,
    AddRepeater,
    AddZigZag,
    AddPuckerBloat,
    EditPath,
    CreateRect,
    CreateEllipse,
    CreateStar,
    CreateText,
    SwitchTool(ToolId),
    UseAsClipMask,
    ReleaseMask,
    ToggleMaskInverted,
    CenterPivot,
}

#[derive(Clone)]
pub struct MenuContext<'a> {
    pub doc: &'a Document,
    pub selection: &'a [NodeId],
    pub comp: CompId,
    /// World position of the click (canvas menus); None for layers.
    pub world_pos: Option<DVec2>,
    pub has_clipboard: bool,
    pub current_paint: &'a StylePaint,
}

/// Right-click menu for a layers-panel row.
pub fn layers_menu(ctx: &MenuContext, row_id: NodeId) -> Vec<MenuEntry> {
    let n = ctx.doc.nodes.get(row_id);
    let locked = n.map(|n| n.locked).unwrap_or(true);
    let visible = n.map(|n| n.visible).unwrap_or(true);
    let is_group = n.is_some_and(|n| matches!(n.kind, NodeKind::Group | NodeKind::Layer(_)));
    let can_ungroup = is_group && n.map(|n| !n.children.is_empty()).unwrap_or(false);
    let multi = ctx.selection.len() > 1;

    let mut m = vec![
        action(MenuAction::Rename, "Rename", !locked),
        action(MenuAction::Duplicate, "Duplicate", true),
        action(MenuAction::Delete, "Delete", !locked),
        MenuEntry::Separator,
        action(MenuAction::Cut, "Cut", !locked),
        action(MenuAction::Copy, "Copy", true),
        action(MenuAction::Paste, "Paste", ctx.has_clipboard),
        MenuEntry::Separator,
        action(
            MenuAction::ToggleVisible,
            if visible { "Hide" } else { "Show" },
            true,
        ),
        action(
            MenuAction::ToggleLocked,
            if locked { "Unlock" } else { "Lock" },
            true,
        ),
        MenuEntry::Separator,
        action(MenuAction::BringToFront, "Bring to Front", !locked),
        action(MenuAction::BringForward, "Bring Forward", !locked),
        action(MenuAction::SendBackward, "Send Backward", !locked),
        action(MenuAction::SendToBack, "Send to Back", !locked),
        MenuEntry::Separator,
        action(MenuAction::Group, "Group", multi && same_parent(ctx)),
        action(MenuAction::Ungroup, "Ungroup", can_ungroup && !locked),
    ];
    if let Some(n) = ctx.doc.nodes.get(row_id) {
        m.push(MenuEntry::Separator);
        match &n.kind {
            NodeKind::Mask(_) => {
                m.push(action(
                    MenuAction::ToggleMaskInverted,
                    "Invert Mask",
                    !locked,
                ));
                m.push(action(
                    MenuAction::ReleaseMask,
                    "Release Clipping Path",
                    !locked,
                ));
            }
            NodeKind::Shape(_) => {
                m.push(action(
                    MenuAction::UseAsClipMask,
                    "Use as Clipping Path",
                    !locked,
                ));
            }
            _ => {}
        }
    }
    m.push(MenuEntry::Separator);
    m.push(MenuEntry::Submenu {
        label: "Add modifier",
        children: vec![
            action(MenuAction::AddTrimPath, "Trim Path", !locked),
            action(MenuAction::AddOffsetPath, "Offset Path", !locked),
            action(MenuAction::AddRoundCorners, "Round Corners", !locked),
            action(MenuAction::AddRepeater, "Repeater", !locked),
            action(MenuAction::AddZigZag, "Zig Zag", !locked),
            action(MenuAction::AddPuckerBloat, "Pucker & Bloat", !locked),
        ],
    });
    m
}

/// Right-click menu for the viewport: empty canvas vs. an existing selection.
pub fn canvas_menu(ctx: &MenuContext) -> Vec<MenuEntry> {
    if ctx.selection.is_empty() {
        return empty_canvas_menu(ctx);
    }
    selection_canvas_menu(ctx)
}

fn empty_canvas_menu(ctx: &MenuContext) -> Vec<MenuEntry> {
    let mut m = vec![MenuEntry::Submenu {
        label: "Create",
        children: vec![
            action(MenuAction::CreateRect, "Rectangle", true),
            action(MenuAction::CreateEllipse, "Ellipse", true),
            action(MenuAction::CreateStar, "Star", true),
            action(MenuAction::CreateText, "Text", true),
        ],
    }];
    if ctx.has_clipboard {
        m.insert(0, action(MenuAction::Paste, "Paste", true));
        m.insert(1, MenuEntry::Separator);
    }
    m
}

fn selection_canvas_menu(ctx: &MenuContext) -> Vec<MenuEntry> {
    let any_locked = ctx
        .selection
        .iter()
        .any(|id| ctx.doc.nodes.get(*id).map(|n| n.locked).unwrap_or(true));
    let multi = ctx.selection.len() > 1;
    let single = ctx.selection.first().copied();
    let is_path = single.is_some_and(|id| {
        matches!(
            ctx.doc.nodes.get(id).map(|n| &n.kind),
            Some(NodeKind::Shape(ShapeKind::Path(_)))
        ) || is_path_group(ctx.doc, id)
    });
    let can_ungroup = single.is_some_and(|id| {
        matches!(
            ctx.doc.nodes.get(id).map(|n| &n.kind),
            Some(NodeKind::Group | NodeKind::Layer(_))
        )
    });

    let mut m = vec![
        action(MenuAction::Cut, "Cut", !any_locked),
        action(MenuAction::Copy, "Copy", true),
        action(MenuAction::Paste, "Paste", ctx.has_clipboard),
        action(MenuAction::Duplicate, "Duplicate", true),
        action(MenuAction::Delete, "Delete", !any_locked),
        MenuEntry::Separator,
        action(MenuAction::Group, "Group", multi && same_parent(ctx)),
        action(MenuAction::Ungroup, "Ungroup", can_ungroup && !any_locked),
        MenuEntry::Separator,
        MenuEntry::Submenu {
            label: "Add modifier",
            children: vec![
                action(
                    MenuAction::AddTrimPath,
                    "Trim Path",
                    !any_locked && single.is_some(),
                ),
                action(
                    MenuAction::AddOffsetPath,
                    "Offset Path",
                    !any_locked && single.is_some(),
                ),
                action(
                    MenuAction::AddRoundCorners,
                    "Round Corners",
                    !any_locked && single.is_some(),
                ),
                action(
                    MenuAction::AddRepeater,
                    "Repeater",
                    !any_locked && single.is_some(),
                ),
                action(
                    MenuAction::AddZigZag,
                    "Zig Zag",
                    !any_locked && single.is_some(),
                ),
                action(
                    MenuAction::AddPuckerBloat,
                    "Pucker & Bloat",
                    !any_locked && single.is_some(),
                ),
            ],
        },
    ];
    if is_path {
        m.push(MenuEntry::Separator);
        m.push(action(MenuAction::EditPath, "Edit path", true));
    }
    m.push(action(
        MenuAction::CenterPivot,
        "Center Pivot",
        single.is_some() && !any_locked,
    ));
    let single_kind = single.and_then(|id| ctx.doc.nodes.get(id)).map(|n| &n.kind);
    if !multi {
        match single_kind {
            Some(NodeKind::Mask(_)) => {
                m.push(MenuEntry::Separator);
                m.push(action(
                    MenuAction::ToggleMaskInverted,
                    "Invert Mask",
                    !any_locked,
                ));
                m.push(action(
                    MenuAction::ReleaseMask,
                    "Release Clipping Path",
                    !any_locked,
                ));
            }
            Some(NodeKind::Shape(_)) => {
                m.push(MenuEntry::Separator);
                m.push(action(
                    MenuAction::UseAsClipMask,
                    "Use as Clipping Path",
                    !any_locked,
                ));
            }
            _ => {}
        }
    }
    m
}

fn action(id: MenuAction, label: &'static str, enabled: bool) -> MenuEntry {
    MenuEntry::Action {
        id,
        label,
        enabled,
        icon: None,
    }
}

fn same_parent(ctx: &MenuContext) -> bool {
    let mut parent = None;
    for &id in ctx.selection {
        let Some((p, _)) = ctx.doc.locate(id) else {
            return false;
        };
        if parent.is_none() {
            parent = Some(p);
        } else if parent != Some(p) {
            return false;
        }
    }
    parent.is_some()
}

fn is_path_group(doc: &Document, id: NodeId) -> bool {
    let Some(n) = doc.nodes.get(id) else {
        return false;
    };
    if !matches!(n.kind, NodeKind::Group | NodeKind::Layer(_)) {
        return false;
    }
    n.children.iter().any(|c| {
        matches!(
            doc.nodes.get(*c).map(|x| &x.kind),
            Some(NodeKind::Shape(ShapeKind::Path(_)))
        )
    })
}

#[derive(Clone, Copy)]
enum Reorder {
    Forward,
    Backward,
    Front,
    Back,
}

#[derive(Clone, Copy)]
enum ModKind {
    Trim,
    Offset,
    Round,
    Repeater,
    ZigZag,
    PuckerBloat,
}

/// Convert a menu action into outputs. Host-owned actions (`Rename`,
/// `Cut`, `Copy`, `Paste`, `Duplicate`) return nothing here - the session
/// implements them so it can update the clipboard / focus the rename field.
pub fn dispatch_menu_action(ctx: &MenuContext, action: &MenuAction) -> Vec<ToolOutput> {
    match action {
        MenuAction::Delete => {
            if ctx.selection.is_empty() {
                return vec![];
            }
            let mut cmds = SmallVec::new();
            for &id in ctx.selection {
                cmds.push(EditorCommand::RemoveNode { id });
            }
            vec![
                ToolOutput::BeginTransaction("Delete".into()),
                ToolOutput::Commands(cmds),
                ToolOutput::CommitTransaction,
                ToolOutput::RequestSelection(SelectionChange::Set(vec![])),
            ]
        }

        MenuAction::ToggleVisible => toggle_flags(ctx, true, false),
        MenuAction::ToggleLocked => toggle_flags(ctx, false, true),
        MenuAction::BringForward => reorder(ctx, Reorder::Forward),
        MenuAction::SendBackward => reorder(ctx, Reorder::Backward),
        MenuAction::BringToFront => reorder(ctx, Reorder::Front),
        MenuAction::SendToBack => reorder(ctx, Reorder::Back),

        MenuAction::Group => group_selection(ctx),
        MenuAction::Ungroup => ungroup(ctx),

        MenuAction::AddTrimPath => add_mod(ctx, ModKind::Trim),
        MenuAction::AddOffsetPath => add_mod(ctx, ModKind::Offset),
        MenuAction::AddRoundCorners => add_mod(ctx, ModKind::Round),
        MenuAction::AddRepeater => add_mod(ctx, ModKind::Repeater),
        MenuAction::AddZigZag => add_mod(ctx, ModKind::ZigZag),
        MenuAction::AddPuckerBloat => add_mod(ctx, ModKind::PuckerBloat),

        MenuAction::EditPath => vec![ToolOutput::SwitchTool(ToolId::PathEdit)],

        MenuAction::UseAsClipMask => mask_command(
            ctx,
            |id, _| EditorCommand::ConvertToMask { id },
            "Use as clipping path",
            true,
        ),
        MenuAction::ReleaseMask => mask_command(
            ctx,
            |id, _| EditorCommand::ReleaseMask { id },
            "Release clipping path",
            true,
        ),
        MenuAction::ToggleMaskInverted => mask_command(
            ctx,
            |id, inverted| EditorCommand::SetMaskInverted { id, inverted },
            "Invert mask",
            true,
        ),

        MenuAction::CreateRect => create_primitive(ctx, Prim::Rect),
        MenuAction::CreateEllipse => create_primitive(ctx, Prim::Ellipse),
        MenuAction::CreateStar => create_primitive(ctx, Prim::Star),
        MenuAction::CreateText => create_primitive(ctx, Prim::Text),

        MenuAction::SwitchTool(t) => vec![ToolOutput::SwitchTool(*t)],

        // Host-side in the session: needs scene + playhead, unavailable here.
        MenuAction::CenterPivot => vec![],

        MenuAction::Rename
        | MenuAction::Cut
        | MenuAction::Copy
        | MenuAction::Paste
        | MenuAction::Duplicate => vec![],
    }
}

fn toggle_flags(ctx: &MenuContext, vis: bool, lock: bool) -> Vec<ToolOutput> {
    let mut cmds = SmallVec::new();
    for &id in ctx.selection {
        let Some(n) = ctx.doc.nodes.get(id) else {
            continue;
        };
        cmds.push(EditorCommand::SetNodeFlags {
            id,
            visible: vis.then_some(!n.visible),
            locked: lock.then_some(!n.locked),
        });
    }
    if cmds.is_empty() {
        return vec![];
    }
    let label = if vis { "Visibility" } else { "Lock" };
    vec![
        ToolOutput::BeginTransaction(label.into()),
        ToolOutput::Commands(cmds),
        ToolOutput::CommitTransaction,
    ]
}

fn mask_command<F: Fn(NodeId, bool) -> EditorCommand>(
    ctx: &MenuContext,
    build: F,
    label: &str,
    toggle_inverted: bool,
) -> Vec<ToolOutput> {
    let Some(&id) = ctx.selection.first() else {
        return vec![];
    };
    let inverted = match ctx.doc.nodes.get(id).map(|n| &n.kind) {
        Some(NodeKind::Mask(mask)) => mask.inverted,
        _ => false,
    };
    let cmd = build(id, if toggle_inverted { !inverted } else { inverted });
    vec![
        ToolOutput::BeginTransaction(label.into()),
        ToolOutput::Commands(smallvec![cmd]),
        ToolOutput::CommitTransaction,
    ]
}

fn reorder(ctx: &MenuContext, how: Reorder) -> Vec<ToolOutput> {
    let mut cmds = SmallVec::new();
    for &id in ctx.selection {
        let Some((parent, index)) = ctx.doc.locate(id) else {
            continue;
        };
        let len = sibling_count(ctx.doc, parent);
        // index 0 = top of stack / front.
        match how {
            Reorder::Front => {
                if index != 0 {
                    cmds.push(EditorCommand::MoveNode {
                        id,
                        new_parent: parent,
                        index: 0,
                    });
                }
            }
            Reorder::Forward => {
                if index > 0 {
                    cmds.push(EditorCommand::MoveNode {
                        id,
                        new_parent: parent,
                        index: index - 1,
                    });
                }
            }
            Reorder::Backward => {
                if index + 1 < len {
                    cmds.push(EditorCommand::MoveNode {
                        id,
                        new_parent: parent,
                        index: index + 1,
                    });
                }
            }
            Reorder::Back => {
                if index + 1 < len {
                    cmds.push(EditorCommand::MoveNode {
                        id,
                        new_parent: parent,
                        index: len - 1,
                    });
                }
            }
        }
    }
    if cmds.is_empty() {
        return vec![];
    }
    vec![
        ToolOutput::BeginTransaction("Reorder".into()),
        ToolOutput::Commands(cmds),
        ToolOutput::CommitTransaction,
    ]
}

fn sibling_count(doc: &Document, parent: Parent) -> usize {
    match parent {
        Parent::Comp(c) => doc
            .compositions
            .get(c)
            .map(|c| c.children.len())
            .unwrap_or(0),
        Parent::Node(n) => doc.nodes.get(n).map(|n| n.children.len()).unwrap_or(0),
    }
}

fn add_mod(ctx: &MenuContext, kind: ModKind) -> Vec<ToolOutput> {
    let Some(&target) = ctx.selection.first() else {
        return vec![];
    };
    let cmd = match kind {
        ModKind::Trim => crate::modifiers::cmd_add_trim_path_after(ctx.doc, target),
        ModKind::Offset => crate::modifiers::cmd_add_offset_path_after(ctx.doc, target, 10.0),
        ModKind::Round => crate::modifiers::cmd_add_round_corners_after(ctx.doc, target, 10.0),
        ModKind::Repeater => crate::modifiers::cmd_add_repeater_after(ctx.doc, target),
        ModKind::ZigZag => crate::modifiers::cmd_add_zigzag_after(ctx.doc, target),
        ModKind::PuckerBloat => crate::modifiers::cmd_add_pucker_bloat_after(ctx.doc, target, 50.0),
    };
    let Some(cmd) = cmd else {
        return vec![];
    };
    vec![
        ToolOutput::BeginTransaction("Add modifier".into()),
        ToolOutput::Commands(smallvec![cmd]),
        ToolOutput::CommitTransaction,
    ]
}

fn group_selection(ctx: &MenuContext) -> Vec<ToolOutput> {
    let ids: Vec<NodeId> = ctx.selection.to_vec();
    if ids.len() < 2 {
        return vec![];
    }
    let Some((parent, _)) = ctx.doc.locate(ids[0]) else {
        return vec![];
    };
    if ids
        .iter()
        .skip(1)
        .any(|&id| ctx.doc.locate(id).map(|(p, _)| p != parent).unwrap_or(true))
    {
        return vec![];
    }
    vec![
        ToolOutput::BeginTransaction("Group".into()),
        ToolOutput::Commands(smallvec![EditorCommand::GroupSelection {
            ids,
            parent,
            index: 0,
            group: None,
        }]),
        ToolOutput::CommitTransaction,
    ]
}

fn ungroup(ctx: &MenuContext) -> Vec<ToolOutput> {
    let Some(&id) = ctx.selection.first() else {
        return vec![];
    };
    let Some((g_parent, g_index)) = ctx.doc.locate(id) else {
        return vec![];
    };
    let Some(group) = ctx.doc.nodes.get(id) else {
        return vec![];
    };
    let kids: Vec<NodeId> = group.children.clone();
    if kids.is_empty() {
        return vec![];
    }
    let mut cmds = SmallVec::new();
    for k in kids {
        cmds.push(EditorCommand::MoveNode {
            id: k,
            new_parent: g_parent,
            index: g_index,
        });
    }
    cmds.push(EditorCommand::RemoveNode { id });
    vec![
        ToolOutput::BeginTransaction("Ungroup".into()),
        ToolOutput::Commands(cmds),
        ToolOutput::CommitTransaction,
    ]
}

enum Prim {
    Rect,
    Ellipse,
    Star,
    Text,
}

fn create_primitive(ctx: &MenuContext, prim: Prim) -> Vec<ToolOutput> {
    let pos = ctx.world_pos.unwrap_or(DVec2::new(180.0, 180.0));
    let paint = ctx.current_paint.snapshot(0.0);
    let (name, shape): (&str, ShapeKind) = match prim {
        Prim::Rect => (
            "Rectangle",
            ShapeKind::Rect {
                pos: Animated::new(pos),
                size: Animated::new(DVec2::new(120.0, 120.0)),
                rounded: Animated::new(0.0),
            },
        ),
        Prim::Ellipse => (
            "Ellipse",
            ShapeKind::Ellipse {
                pos: Animated::new(pos),
                size: Animated::new(DVec2::new(120.0, 120.0)),
            },
        ),
        Prim::Star => (
            "Star",
            ShapeKind::Star {
                pos: Animated::new(pos),
                points: Animated::new(5.0),
                inner_r: Animated::new(48.0),
                outer_r: Animated::new(120.0),
                roundness: Animated::new(0.0),
                kind: StarKind::Star,
            },
        ),
        Prim::Text => return create_text(ctx, pos, paint),
    };

    vec![
        ToolOutput::BeginTransaction(format!("Create {name}")),
        ToolOutput::Commands(smallvec![EditorCommand::InsertNode {
            parent: Parent::Comp(ctx.comp),
            index: 0,
            tree: NodeTree::with_children(
                Node::new(name, NodeKind::Group),
                vec![
                    NodeTree::leaf(Node::new("Shape", NodeKind::Shape(shape))),
                    NodeTree::leaf(Node::new(
                        "Fill",
                        NodeKind::Style(StyleKind::Fill {
                            paint,
                            rule: FillRule::NonZero,
                        }),
                    )),
                ],
            ),
        }]),
        ToolOutput::CommitTransaction,
        ToolOutput::SwitchTool(ToolId::Select),
    ]
}

fn create_text(ctx: &MenuContext, pos: DVec2, paint: StylePaint) -> Vec<ToolOutput> {
    let mut text_node = Node::new(
        "Text",
        NodeKind::Text(TextNode {
            text: "Text".into(),
            size: Animated::new(48.0),
            align: TextAlign::Left,
            font: None,
        }),
    );
    text_node.transform.position = Animated::new(pos);
    let tree = NodeTree::with_children(
        Node::new("Text", NodeKind::Group),
        vec![
            NodeTree::leaf(text_node),
            NodeTree::leaf(Node::new(
                "Fill",
                NodeKind::Style(StyleKind::Fill {
                    paint,
                    rule: FillRule::NonZero,
                }),
            )),
        ],
    );
    vec![
        ToolOutput::BeginTransaction("Create text".into()),
        ToolOutput::Commands(smallvec![EditorCommand::InsertNode {
            parent: Parent::Comp(ctx.comp),
            index: 0,
            tree,
        }]),
        ToolOutput::CommitTransaction,
        ToolOutput::SwitchTool(ToolId::Select),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use renamite_model::ShapeKind;

    fn doc_selection(ids: Vec<NodeId>) -> (Document, Vec<NodeId>) {
        let d = Document::empty();
        (d, ids)
    }

    #[test]
    fn empty_canvas_menu_has_create_submenu() {
        let (doc, sel) = doc_selection(vec![]);
        let ctx = empty_canvas_menu(&MenuContext {
            doc: &doc,
            selection: &sel,
            comp: doc.main,
            world_pos: None,
            has_clipboard: false,
            current_paint: &StylePaint::solid(renamite_model::Color::BLACK),
        });
        assert!(matches!(ctx[0], MenuEntry::Submenu { .. }));
        assert!(!ctx.iter().any(|e| matches!(
            e,
            MenuEntry::Action {
                id: MenuAction::Paste,
                ..
            }
        )));
    }

    #[test]
    fn empty_canvas_menu_prepends_paste_when_clipboard() {
        let (doc, sel) = doc_selection(vec![]);
        let ctx = MenuContext {
            doc: &doc,
            selection: &sel,
            comp: doc.main,
            world_pos: None,
            has_clipboard: true,
            current_paint: &StylePaint::solid(renamite_model::Color::BLACK),
        };
        let m = canvas_menu(&ctx);
        assert!(matches!(
            m[0],
            MenuEntry::Action {
                id: MenuAction::Paste,
                ..
            }
        ));
    }

    #[test]
    fn layers_menu_disables_delete_when_locked() {
        let mut doc = Document::empty();
        let locked = doc.create_node(Node::new(
            "g",
            NodeKind::Shape(ShapeKind::Ellipse {
                pos: Animated::new(DVec2::ZERO),
                size: Animated::new(DVec2::ONE),
            }),
        ));
        doc.nodes[locked].locked = true;
        let sel = vec![locked];
        let ctx = MenuContext {
            doc: &doc,
            selection: &sel,
            comp: doc.main,
            world_pos: None,
            has_clipboard: false,
            current_paint: &StylePaint::solid(renamite_model::Color::BLACK),
        };
        let m = layers_menu(&ctx, locked);
        let del = m.iter().find_map(|e| match e {
            MenuEntry::Action {
                id: MenuAction::Delete,
                enabled,
                ..
            } => Some(*enabled),
            _ => None,
        });
        assert_eq!(del, Some(false));
    }

    #[test]
    fn selection_menu_offers_edit_path_for_path_shape() {
        let mut doc = Document::empty();
        let comp = doc.main;
        let path = doc.create_node(Node::new(
            "p",
            NodeKind::Shape(ShapeKind::Path(Animated::new(
                renamite_geometry::VectorPath::from_bez_path(&kurbo::BezPath::new()),
            ))),
        ));
        doc.attach(path, Parent::Comp(comp), 0).unwrap();
        let sel = vec![path];
        let ctx = MenuContext {
            doc: &doc,
            selection: &sel,
            comp,
            world_pos: None,
            has_clipboard: false,
            current_paint: &StylePaint::solid(renamite_model::Color::BLACK),
        };
        let m = selection_canvas_menu(&ctx);
        assert!(m.iter().any(|e| matches!(
            e,
            MenuEntry::Action {
                id: MenuAction::EditPath,
                ..
            }
        )));
    }

    #[test]
    fn dispatch_delete_emits_remove_and_clears_selection() {
        let mut doc = Document::empty();
        let comp = doc.main;
        let a = doc.create_node(Node::new("a", NodeKind::Group));
        doc.attach(a, Parent::Comp(comp), 0).unwrap();
        let sel = vec![a];
        let ctx = MenuContext {
            doc: &doc,
            selection: &sel,
            comp,
            world_pos: None,
            has_clipboard: false,
            current_paint: &StylePaint::solid(renamite_model::Color::BLACK),
        };
        let outs = dispatch_menu_action(&ctx, &MenuAction::Delete);
        assert!(
            outs.iter()
                .any(|o| matches!(o, ToolOutput::RequestSelection(_)))
        );
        assert!(outs.iter().any(|o| matches!(
            o,
            ToolOutput::Commands(c) if matches!(c.as_slice(), [EditorCommand::RemoveNode { .. }])
        )));
    }

    #[test]
    fn dispatch_group_requires_two_same_parent() {
        let mut doc = Document::empty();
        let comp = doc.main;
        let a = doc.create_node(Node::new("a", NodeKind::Group));
        let b = doc.create_node(Node::new("b", NodeKind::Group));
        doc.attach(a, Parent::Comp(comp), 0).unwrap();
        doc.attach(b, Parent::Comp(comp), 1).unwrap();
        let sel = vec![a, b];
        let ctx = MenuContext {
            doc: &doc,
            selection: &sel,
            comp,
            world_pos: None,
            has_clipboard: false,
            current_paint: &StylePaint::solid(renamite_model::Color::BLACK),
        };
        let outs = dispatch_menu_action(&ctx, &MenuAction::Group);
        assert!(outs.iter().any(|o| matches!(o, ToolOutput::Commands(c) if {
            matches!(c.as_slice(),
                [EditorCommand::GroupSelection { .. }])
        })));
    }

    #[test]
    fn mask_menu_shows_invert_and_release_for_mask() {
        let mut doc = Document::empty();
        let comp = doc.main;
        let mask = doc.create_node(Node::new(
            "m",
            NodeKind::Mask(renamite_model::MaskProps {
                inverted: false,
                shape: ShapeKind::Path(Animated::new(
                    renamite_geometry::VectorPath::from_bez_path(&kurbo::BezPath::new()),
                )),
            }),
        ));
        doc.attach(mask, Parent::Comp(comp), 0).unwrap();
        let sel = vec![mask];
        let ctx = MenuContext {
            doc: &doc,
            selection: &sel,
            comp,
            world_pos: None,
            has_clipboard: false,
            current_paint: &StylePaint::solid(renamite_model::Color::BLACK),
        };
        let m = selection_canvas_menu(&ctx);
        assert!(m.iter().any(|e| matches!(
            e,
            MenuEntry::Action {
                id: MenuAction::ToggleMaskInverted,
                ..
            }
        )));
        assert!(m.iter().any(|e| matches!(
            e,
            MenuEntry::Action {
                id: MenuAction::ReleaseMask,
                ..
            }
        )));

        let outs = dispatch_menu_action(&ctx, &MenuAction::ToggleMaskInverted);
        assert!(outs.iter().any(|o| matches!(
            o,
            ToolOutput::Commands(c)
                if matches!(c.as_slice(), [EditorCommand::SetMaskInverted { id, inverted: true }] if *id == mask)
        )));
    }

    #[test]
    fn shape_menu_offers_use_as_clip_mask() {
        let mut doc = Document::empty();
        let comp = doc.main;
        let shape = doc.create_node(Node::new(
            "s",
            NodeKind::Shape(ShapeKind::Ellipse {
                pos: Animated::new(DVec2::ZERO),
                size: Animated::new(DVec2::ONE),
            }),
        ));
        doc.attach(shape, Parent::Comp(comp), 0).unwrap();
        let sel = vec![shape];
        let ctx = MenuContext {
            doc: &doc,
            selection: &sel,
            comp,
            world_pos: None,
            has_clipboard: false,
            current_paint: &StylePaint::solid(renamite_model::Color::BLACK),
        };
        let m = selection_canvas_menu(&ctx);
        assert!(m.iter().any(|e| matches!(
            e,
            MenuEntry::Action {
                id: MenuAction::UseAsClipMask,
                ..
            }
        )));

        let outs = dispatch_menu_action(&ctx, &MenuAction::UseAsClipMask);
        assert!(outs.iter().any(|o| matches!(
            o,
            ToolOutput::Commands(c)
                if matches!(c.as_slice(), [EditorCommand::ConvertToMask { id }] if *id == shape)
        )));
    }
}
