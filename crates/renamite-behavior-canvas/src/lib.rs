//! Canvas tools: Select/Transform (click, drag-move, rotate/scale handles,
//! rubber band, delete) and Rect/Ellipse creation. Pure state machines:
//! world-space events in, EditorCommands out. Behavior reference (clean-room,
//! observed): rotation handle preserves direction and multiple full turns;
//! Shift snaps rotation to 15° and constrains shapes to square/circle;
//! drag = one undo step; Esc cancels the open drag.

use glam::DVec2;
use renamite_animation::{Angle, Animated, Frame};
use renamite_behavior_common::{ToolContext, path::path_edit_target};
use renamite_geometry::{Anchor, AnchorEdit, TangentMode, VectorPath};
use renamite_history::{
    EditorCommand, NodeTree, OutputVec, SelectionChange, ToolId, ToolOutput,
    resolve_property_edit,
};
use renamite_model::{
    Color, FillRule, Node, NodeId, NodeKind, Parent, PropPath, ShapeKind, StyleKind, Value,
    nodes_bounds, pick, pick_box,
};
use smallvec::smallvec;
use std::f64::consts::{PI, TAU};

#[derive(Clone, Debug)]
pub enum CanvasEvent {
    PointerDown { pos: DVec2, button: PointerButton },
    PointerMove { pos: DVec2 },
    PointerUp { pos: DVec2, button: PointerButton },
    KeyDown(Key),
    DoubleClick { pos: DVec2 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerButton { Primary, Secondary, Middle }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key { Escape, Delete, Backspace, Enter }

/// World-space overlay for the host to draw (screen conversion is the host's job).
#[derive(Clone, Debug, PartialEq)]
pub enum ToolOverlay {
    None,
    RubberBand { min: DVec2, max: DVec2 },
    /// Selection bounds + handle anchor points.
    Selection { min: DVec2, max: DVec2, rotate: DVec2, scale: DVec2 },
    ShapePreview { min: DVec2, max: DVec2, ellipse: bool },
    PenPreview { anchors: Vec<Anchor>, closed: bool, hover: Option<DVec2> },
    PathHandles { path: VectorPath, active_anchor: Option<usize> },
}

pub struct ToolSet {
    pub select: SelectTool,
    pub rect: ShapeTool,
    pub ellipse: ShapeTool,
    pub pen: PenTool,
    pub path_edit: PathEditTool,
}

impl Default for ToolSet {
    fn default() -> Self {
        Self {
            select: SelectTool::default(),
            rect: ShapeTool::new(ShapeToolKind::Rect),
            ellipse: ShapeTool::new(ShapeToolKind::Ellipse),
            pen: PenTool::default(),
            path_edit: PathEditTool::default(),
        }
    }
}

impl ToolSet {
    pub fn handle(&mut self, id: ToolId, ctx: &ToolContext, ev: CanvasEvent) -> OutputVec {
        match id {
            ToolId::Select | ToolId::Transform => self.select.handle(ctx, ev),
            ToolId::Rect => self.rect.handle(ctx, ev),
            ToolId::Ellipse => self.ellipse.handle(ctx, ev),
            ToolId::Pen => self.pen.handle(ctx, ev),
            ToolId::PathEdit => self.path_edit.handle(ctx, ev),
            _ => smallvec![], // Star/Gradient/Fill: not implemented yet
        }
    }

    pub fn overlay(&self, id: ToolId, ctx: &ToolContext) -> ToolOverlay {
        match id {
            ToolId::Select | ToolId::Transform => self.select.overlay(ctx),
            ToolId::Rect => self.rect.overlay(ctx),
            ToolId::Ellipse => self.ellipse.overlay(ctx),
            ToolId::Pen => self.pen.overlay(ctx),
            ToolId::PathEdit => self.path_edit.overlay(ctx),
            _ => ToolOverlay::None,
        }
    }
}

const DRAG_THRESHOLD_PX: f64 = 3.0;
const HANDLE_PX: f64 = 8.0;
const ROTATE_OFFSET_PX: f64 = 24.0;

enum SelState {
    Idle,
    Pending { press: DVec2, node: NodeId },
    DragMove { last: DVec2, txn: bool },
    RubberBand { start: DVec2, current: DVec2 },
    DragRotate { pivot: DVec2, start: f64, acc: f64, node: NodeId, base_deg: f64, txn: bool },
    DragScale { pivot: DVec2, start_dist: f64, node: NodeId, base: DVec2, txn: bool },
}

#[derive(Default)]
pub struct SelectTool {
    state: Option<SelState>,
}

impl SelectTool {
    fn st(&mut self) -> &mut SelState {
        self.state.get_or_insert(SelState::Idle)
    }

    pub fn overlay(&self, ctx: &ToolContext) -> ToolOverlay {
        if let Some(SelState::RubberBand { start, current }) = &self.state {
            return ToolOverlay::RubberBand { min: start.min(*current), max: start.max(*current) };
        }
        if let Some((min, max)) = nodes_bounds(ctx.scene, &ctx.selection.nodes) {
            let (rot, scl) = handles(ctx, min, max);
            return ToolOverlay::Selection { min, max, rotate: rot, scale: scl };
        }
        ToolOverlay::None
    }

    pub fn handle(&mut self, ctx: &ToolContext, ev: CanvasEvent) -> OutputVec {
        match ev {
            CanvasEvent::PointerDown { pos, button: PointerButton::Primary } => self.press(ctx, pos),
            CanvasEvent::PointerMove { pos } => self.moved(ctx, pos),
            CanvasEvent::PointerUp { pos, button: PointerButton::Primary } => self.release(ctx, pos),
            CanvasEvent::KeyDown(Key::Escape) => self.escape(),
            CanvasEvent::KeyDown(Key::Delete) | CanvasEvent::KeyDown(Key::Backspace) => self.delete(ctx),
            _ => smallvec![],
        }
    }

    fn press(&mut self, ctx: &ToolContext, pos: DVec2) -> OutputVec {
        // 1. Handles win over shapes - single-node selection only (v1).
        if let [node] = ctx.selection.nodes[..]
            && let Some((min, max)) = nodes_bounds(ctx.scene, &ctx.selection.nodes)
        {
            let (rot, scl) = handles(ctx, min, max);
            let tol = ctx.view.world_tolerance(HANDLE_PX);
            let pivot = (min + max) * 0.5;
            if (pos - rot).length() <= tol {
                let base_deg = rotation_deg(ctx, node);
                *self.st() = SelState::DragRotate {
                    pivot, start: angle_of(pos - pivot), acc: 0.0, node, base_deg, txn: false,
                };
                return smallvec![];
            }
            if (pos - scl).length() <= tol {
                let base = scale_of(ctx, node);
                // Pivot = opposite corner (min), standard corner-scale feel.
                *self.st() = SelState::DragScale {
                    pivot: min, start_dist: (pos - min).length().max(1e-6), node, base, txn: false,
                };
                return smallvec![];
            }
        }
        // 2. Shape pick (skip locked nodes).
        match pick(ctx.scene, pos).filter(|n| !ctx.doc.nodes.get(*n).is_none_or(|x| x.locked)) {
            Some(node) => {
                let mut out: OutputVec = smallvec![];
                if ctx.modifiers.ctrl {
                    out.push(ToolOutput::RequestSelection(SelectionChange::Toggle(node)));
                } else if ctx.modifiers.shift {
                    if !ctx.selection.contains(node) {
                        let mut s = ctx.selection.nodes.clone();
                        s.push(node);
                        out.push(ToolOutput::RequestSelection(SelectionChange::Set(s)));
                    }
                } else if !ctx.selection.contains(node) {
                    out.push(ToolOutput::RequestSelection(SelectionChange::Set(vec![node])));
                }
                *self.st() = SelState::Pending { press: pos, node };
                out
            }
            None => {
                let mut out: OutputVec = smallvec![];
                if !ctx.modifiers.shift && !ctx.modifiers.ctrl {
                    out.push(ToolOutput::RequestSelection(SelectionChange::Set(vec![])));
                }
                *self.st() = SelState::RubberBand { start: pos, current: pos };
                out
            }
        }
    }

    fn moved(&mut self, ctx: &ToolContext, pos: DVec2) -> OutputVec {
        match self.st() {
            SelState::Pending { press, .. } => {
                if (pos - *press).length() >= ctx.view.world_tolerance(DRAG_THRESHOLD_PX) {
                    let press = *press;
                    *self.st() = SelState::DragMove { last: press, txn: false };
                    return self.moved(ctx, pos);
                }
                smallvec![]
            }
            SelState::DragMove { last, txn } => {
                let delta = pos - *last;
                *last = pos;
                if delta.length_squared() == 0.0 { return smallvec![]; }
                let mut out: OutputVec = smallvec![];
                if !*txn { out.push(ToolOutput::BeginTransaction("Move".into())); *txn = true; }
                let prop = PropPath::new("transform.position");
                let cmds = ctx.selection.nodes.iter().filter_map(|&n| {
                    let Ok(Value::DVec2(cur)) = ctx.doc.value_at(n, &prop, ctx.playhead.0 as f64) else { return None };
                    Some(resolve_property_edit(ctx.doc, n, &prop, Value::DVec2(cur + delta), ctx.playhead, ctx.record))
                }).collect();
                out.push(ToolOutput::Commands(cmds));
                out
            }
            SelState::DragRotate { pivot, start, acc, node, base_deg, txn } => {
                let raw = angle_of(pos - *pivot) - *start;
                *acc = unwrap_continuous(*acc, raw);
                let mut deg = *base_deg + acc.to_degrees();
                if ctx.modifiers.shift { deg = (deg / 15.0).round() * 15.0; }
                let (node, base) = (*node, *txn);
                let mut out: OutputVec = smallvec![];
                if !base { out.push(ToolOutput::BeginTransaction("Rotate".into())); *txn = true; }
                out.push(ToolOutput::Commands(smallvec![resolve_property_edit(
                    ctx.doc, node, &PropPath::new("transform.rotation"),
                    Value::Angle(Angle(deg)), ctx.playhead, ctx.record,
                )]));
                out
            }
            SelState::DragScale { pivot, start_dist, node, base, txn } => {
                let factor = ((pos - *pivot).length() / *start_dist).max(0.01);
                let new = *base * factor; // uniform (v1)
                let (node, started) = (*node, *txn);
                let mut out: OutputVec = smallvec![];
                if !started { out.push(ToolOutput::BeginTransaction("Scale".into())); *txn = true; }
                out.push(ToolOutput::Commands(smallvec![resolve_property_edit(
                    ctx.doc, node, &PropPath::new("transform.scale"),
                    Value::DVec2(new), ctx.playhead, ctx.record,
                )]));
                out
            }
            SelState::RubberBand { current, .. } => { *current = pos; smallvec![] }
            SelState::Idle => smallvec![],
        }
    }

    fn release(&mut self, ctx: &ToolContext, _pos: DVec2) -> OutputVec {
        match std::mem::replace(self.st(), SelState::Idle) {
            SelState::DragMove { txn, .. }
            | SelState::DragRotate { txn, .. }
            | SelState::DragScale { txn, .. } => {
                if txn { smallvec![ToolOutput::CommitTransaction] } else { smallvec![] }
            }
            SelState::Pending { node, .. } => {
                // Plain click on already-multi-selected collapses to just it.
                if !ctx.modifiers.ctrl && !ctx.modifiers.shift && ctx.selection.nodes.len() > 1 {
                    smallvec![ToolOutput::RequestSelection(SelectionChange::Set(vec![node]))]
                } else { smallvec![] }
            }
            SelState::RubberBand { start, current } => {
                let (min, max) = (start.min(current), start.max(current));
                let mut picked = pick_box(ctx.scene, min, max);
                if ctx.modifiers.shift || ctx.modifiers.ctrl {
                    let mut s = ctx.selection.nodes.clone();
                    for n in picked.drain(..) { if !s.contains(&n) { s.push(n); } }
                    picked = s;
                }
                smallvec![ToolOutput::RequestSelection(SelectionChange::Set(picked))]
            }
            SelState::Idle => smallvec![],
        }
    }

    fn escape(&mut self) -> OutputVec {
        match std::mem::replace(self.st(), SelState::Idle) {
            SelState::DragMove { txn: true, .. }
            | SelState::DragRotate { txn: true, .. }
            | SelState::DragScale { txn: true, .. } => smallvec![ToolOutput::CancelTransaction],
            _ => smallvec![],
        }
    }

    fn delete(&mut self, ctx: &ToolContext) -> OutputVec {
        if ctx.selection.is_empty() { return smallvec![]; }
        let cmds = ctx.selection.nodes.iter().map(|&id| EditorCommand::RemoveNode { id }).collect();
        smallvec![
            ToolOutput::BeginTransaction("Delete".into()),
            ToolOutput::Commands(cmds),
            ToolOutput::CommitTransaction,
            ToolOutput::RequestSelection(SelectionChange::Set(vec![])),
        ]
    }
}

fn handles(ctx: &ToolContext, min: DVec2, max: DVec2) -> (DVec2, DVec2) {
    let cx = (min.x + max.x) * 0.5;
    let rot = DVec2::new(cx, min.y - ctx.view.world_tolerance(ROTATE_OFFSET_PX));
    (rot, max) // scale handle = bottom-right corner
}

fn rotation_deg(ctx: &ToolContext, node: NodeId) -> f64 {
    match ctx.doc.value_at(node, &PropPath::new("transform.rotation"), ctx.playhead.0 as f64) {
        Ok(Value::Angle(a)) => a.0,
        Ok(Value::F64(v)) => v,
        _ => 0.0,
    }
}

fn scale_of(ctx: &ToolContext, node: NodeId) -> DVec2 {
    match ctx.doc.value_at(node, &PropPath::new("transform.scale"), ctx.playhead.0 as f64) {
        Ok(Value::DVec2(v)) => v,
        _ => DVec2::splat(100.0),
    }
}

fn angle_of(v: DVec2) -> f64 { v.y.atan2(v.x) }

/// Multi-turn unwrap: keep `raw` continuous with `acc` (Glaxnimate 0.6:
/// 3 physical turns = 1080°, never re-wrapped to 0).
fn unwrap_continuous(acc: f64, raw: f64) -> f64 {
    let mut d = raw - acc;
    while d > PI { d -= TAU; }
    while d < -PI { d += TAU; }
    acc + d
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ShapeToolKind { Rect, Ellipse }

pub struct ShapeTool {
    kind: ShapeToolKind,
    drag: Option<(DVec2, DVec2)>, // (start, current)
}

impl ShapeTool {
    pub fn new(kind: ShapeToolKind) -> Self { Self { kind, drag: None } }

    pub fn overlay(&self, ctx: &ToolContext) -> ToolOverlay {
        match self.drag {
            Some((s, c)) => {
                let (min, max) = constrained_rect(s, c, ctx.modifiers.shift, ctx.modifiers.alt);
                ToolOverlay::ShapePreview { min, max, ellipse: self.kind == ShapeToolKind::Ellipse }
            }
            None => ToolOverlay::None,
        }
    }

    pub fn handle(&mut self, ctx: &ToolContext, ev: CanvasEvent) -> OutputVec {
        match ev {
            CanvasEvent::PointerDown { pos, button: PointerButton::Primary } => {
                self.drag = Some((pos, pos));
                smallvec![]
            }
            CanvasEvent::PointerMove { pos } => {
                if let Some((_, c)) = &mut self.drag { *c = pos; }
                smallvec![]
            }
            CanvasEvent::PointerUp { pos, button: PointerButton::Primary } => {
                let Some((start, _)) = self.drag.take() else { return smallvec![] };
                let (min, max) = constrained_rect(start, pos, ctx.modifiers.shift, ctx.modifiers.alt);
                let size = max - min;
                if size.x < 1.0 || size.y < 1.0 { return smallvec![]; }
                let center = (min + max) * 0.5;
                let (name, shape) = match self.kind {
                    ShapeToolKind::Rect => ("Rectangle", ShapeKind::Rect {
                        pos: Animated::new(center), size: Animated::new(size), rounded: Animated::new(0.0),
                    }),
                    ShapeToolKind::Ellipse => ("Ellipse", ShapeKind::Ellipse {
                        pos: Animated::new(center), size: Animated::new(size),
                    }),
                };
                let tree = NodeTree::with_children(
                    Node::new(name, NodeKind::Group),
                    vec![
                        NodeTree::leaf(Node::new("Shape", NodeKind::Shape(shape))),
                        NodeTree::leaf(Node::new("Fill", NodeKind::Style(StyleKind::Fill {
                            color: Animated::new(Color::rgba(0.96, 0.42, 0.18, 1.0)),
                            rule: FillRule::NonZero,
                        }))),
                    ],
                );
                smallvec![
                    ToolOutput::BeginTransaction(format!("Create {name}")),
                    ToolOutput::Commands(smallvec![EditorCommand::InsertNode {
                        parent: Parent::Comp(ctx.comp), index: 0, tree,
                    }]),
                    ToolOutput::CommitTransaction,
                    ToolOutput::SwitchTool(ToolId::Select),
                ]
            }
            CanvasEvent::KeyDown(Key::Escape) => { self.drag = None; smallvec![] }
            _ => smallvec![],
        }
    }
}

/// Shift = square/circle; Alt = grow from center.
fn constrained_rect(start: DVec2, current: DVec2, shift: bool, alt: bool) -> (DVec2, DVec2) {
    let mut d = current - start;
    if shift {
        let m = d.x.abs().max(d.y.abs());
        d = DVec2::new(m * d.x.signum(), m * d.y.signum());
    }
    let (a, b) = if alt { (start - d, start + d) } else { (start, start + d) };
    (a.min(b), a.max(b))
}

const CLOSE_THRESHOLD_PX: f64 = 10.0;
const ANCHOR_HIT_PX: f64 = 7.0;
const TANGENT_HIT_PX: f64 = 5.0;

enum PenState {
    Idle,
    Building { anchors: Vec<Anchor>, hover: DVec2 },
    DraggingTangent { anchors: Vec<Anchor>, index: usize },
}

pub struct PenTool {
    state: PenState,
}

impl Default for PenTool {
    fn default() -> Self {
        Self { state: PenState::Idle }
    }
}

impl PenTool {
    pub fn overlay(&self, _ctx: &ToolContext) -> ToolOverlay {
        match &self.state {
            PenState::Idle => ToolOverlay::None,
            PenState::Building { anchors, hover } => ToolOverlay::PenPreview {
                anchors: anchors.clone(),
                closed: false,
                hover: Some(*hover),
            },
            PenState::DraggingTangent { anchors, .. } => ToolOverlay::PenPreview {
                anchors: anchors.clone(),
                closed: false,
                hover: None,
            },
        }
    }

    pub fn handle(&mut self, ctx: &ToolContext, ev: CanvasEvent) -> OutputVec {
        match ev {
            CanvasEvent::PointerDown { pos, button: PointerButton::Primary } => self.press(ctx, pos),
            CanvasEvent::PointerMove { pos } => self.moved(pos),
            CanvasEvent::PointerUp { pos, button: PointerButton::Primary } => self.release(pos),
            CanvasEvent::KeyDown(Key::Enter) => self.finish(ctx, false),
            CanvasEvent::KeyDown(Key::Escape) => {
                self.state = PenState::Idle;
                smallvec![]
            }
            CanvasEvent::KeyDown(Key::Backspace) => self.backspace(),
            _ => smallvec![],
        }
    }

    fn press(&mut self, ctx: &ToolContext, pos: DVec2) -> OutputVec {
        match &mut self.state {
            PenState::Idle => {
                // First click enters tangent-drag mode immediately: dragging the
                // first anchor can already create a smooth point.
                self.state = PenState::DraggingTangent {
                    anchors: vec![Anchor::corner(pos)],
                    index: 0,
                };
                smallvec![]
            }
            PenState::Building { anchors, .. } => {
                if anchors.len() >= 2 {
                    let tol = ctx.view.world_tolerance(CLOSE_THRESHOLD_PX);
                    if (pos - anchors[0].pos).length() <= tol {
                        return self.finish(ctx, true);
                    }
                }
                anchors.push(Anchor::corner(pos));
                let idx = anchors.len() - 1;
                let anchors = anchors.clone();
                self.state = PenState::DraggingTangent { anchors, index: idx };
                smallvec![]
            }
            PenState::DraggingTangent { .. } => smallvec![],
        }
    }

    fn moved(&mut self, pos: DVec2) -> OutputVec {
        match &mut self.state {
            PenState::Idle => smallvec![],
            PenState::Building { hover, .. } => {
                *hover = pos;
                smallvec![]
            }
            PenState::DraggingTangent { anchors, index } => {
                let a = &mut anchors[*index];
                let tan = pos - a.pos;

                // Zero-length drag stays a corner.
                if tan.length_squared() < 1e-12 {
                    a.tan_in = DVec2::ZERO;
                    a.tan_out = DVec2::ZERO;
                    a.mode = TangentMode::Corner;
                } else {
                    a.tan_out = tan;
                    a.tan_in = -tan;
                    a.mode = TangentMode::Symmetric;
                }
                smallvec![]
            }
        }
    }

    fn release(&mut self, pos: DVec2) -> OutputVec {
        if let PenState::DraggingTangent { anchors, .. } = &self.state {
            let anchors = anchors.clone();
            self.state = PenState::Building { anchors, hover: pos };
        }
        smallvec![]
    }

    fn backspace(&mut self) -> OutputVec {
        match &mut self.state {
            PenState::Idle => {}
            PenState::Building { anchors, .. } => {
                if anchors.pop().is_none() || anchors.is_empty() {
                    self.state = PenState::Idle;
                }
            }
            PenState::DraggingTangent { anchors, .. } => {
                anchors.pop();
                if anchors.is_empty() {
                    self.state = PenState::Idle;
                } else {
                    let hover = anchors.last().unwrap().pos;
                    let anchors = anchors.clone();
                    self.state = PenState::Building { anchors, hover };
                }
            }
        }
        smallvec![]
    }

    fn finish(&mut self, ctx: &ToolContext, closed: bool) -> OutputVec {
        let anchors = match std::mem::replace(&mut self.state, PenState::Idle) {
            PenState::Idle => return smallvec![],
            PenState::Building { anchors, .. } => anchors,
            PenState::DraggingTangent { anchors, .. } => anchors,
        };

        if anchors.len() < 2 {
            return smallvec![];
        }

        let shape = Node::new(
            "Shape",
            NodeKind::Shape(ShapeKind::Path(Animated::new(VectorPath { anchors, closed }))),
        );
        let fill = Node::new(
            "Fill",
            NodeKind::Style(StyleKind::Fill {
                color: Animated::new(Color::rgba(0.96, 0.42, 0.18, 1.0)),
                rule: FillRule::NonZero,
            }),
        );

        let tree = NodeTree::with_children(
            Node::new("Path", NodeKind::Group),
            vec![NodeTree::leaf(shape), NodeTree::leaf(fill)],
        );

        smallvec![
            ToolOutput::BeginTransaction("Create path".into()),
            ToolOutput::Commands(smallvec![EditorCommand::InsertNode {
                parent: Parent::Comp(ctx.comp),
                index: 0,
                tree,
            }]),
            ToolOutput::CommitTransaction,
            ToolOutput::SwitchTool(ToolId::PathEdit),
        ]
    }
}

enum PathEditState {
    Idle,
    DragAnchor { node: NodeId, index: usize, edit_frame: Option<Frame>, txn: bool },
    DragTanIn { node: NodeId, index: usize, edit_frame: Option<Frame>, txn: bool },
    DragTanOut { node: NodeId, index: usize, edit_frame: Option<Frame>, txn: bool },
}

pub struct PathEditTool {
    state: PathEditState,
    pub selected_anchor: Option<usize>,
}

impl Default for PathEditTool {
    fn default() -> Self {
        Self { state: PathEditState::Idle, selected_anchor: None }
    }
}

impl PathEditTool {
    /// Accept either a selected path node, or a selected group with exactly one
    /// direct Path child (so the group Pen creates works right after switch).
    fn editable_path_node(ctx: &ToolContext) -> Option<NodeId> {
        let &[sel] = ctx.selection.nodes.as_slice() else { return None };
        let node = ctx.doc.nodes.get(sel)?;

        match &node.kind {
            NodeKind::Shape(ShapeKind::Path(_)) => Some(sel),
            NodeKind::Group | NodeKind::Layer(_) => {
                let mut path_children = node.children.iter().copied().filter(|id| {
                    matches!(
                        ctx.doc.nodes.get(*id).map(|n| &n.kind),
                        Some(NodeKind::Shape(ShapeKind::Path(_)))
                    )
                });
                let first = path_children.next()?;
                if path_children.next().is_none() {
                    Some(first)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn current_path(ctx: &ToolContext) -> Option<VectorPath> {
        let id = Self::editable_path_node(ctx)?;
        let node = ctx.doc.nodes.get(id)?;
        match &node.kind {
            NodeKind::Shape(ShapeKind::Path(a)) => Some(a.value_at(ctx.playhead.0 as f64)),
            _ => None,
        }
    }

    pub fn overlay(&self, ctx: &ToolContext) -> ToolOverlay {
        match Self::current_path(ctx) {
            Some(path) => ToolOverlay::PathHandles {
                path,
                active_anchor: self.selected_anchor,
            },
            None => ToolOverlay::None,
        }
    }

    pub fn handle(&mut self, ctx: &ToolContext, ev: CanvasEvent) -> OutputVec {
        match ev {
            CanvasEvent::PointerDown { pos, button: PointerButton::Primary } => self.press(ctx, pos),
            CanvasEvent::PointerMove { pos } => self.moved(ctx, pos),
            CanvasEvent::PointerUp { .. } => self.release(),
            CanvasEvent::KeyDown(Key::Escape) => self.escape(),
            CanvasEvent::KeyDown(Key::Delete) | CanvasEvent::KeyDown(Key::Backspace) => self.delete_anchor(ctx),
            CanvasEvent::DoubleClick { pos } => self.insert_anchor(ctx, pos),
            _ => smallvec![],
        }
    }

    /// (id, seed) for the current edit; `seed` is an AddKeyframe when one is needed.
    fn edit_target(&self, ctx: &ToolContext, id: NodeId) -> (Option<Frame>, Option<EditorCommand>) {
        path_edit_target(ctx.doc, id, ctx.playhead, ctx.record).unwrap_or((None, None))
    }

    fn begin_drag(
        &mut self,
        state: PathEditState,
        seed: Option<EditorCommand>,
    ) -> OutputVec {
        let mut out: OutputVec = smallvec![];
        if let Some(seed) = seed {
            out.push(ToolOutput::BeginTransaction("Edit path".into()));
            out.push(ToolOutput::Commands(smallvec![seed]));
        }
        self.state = state;
        out
    }

    fn press(&mut self, ctx: &ToolContext, pos: DVec2) -> OutputVec {
        let Some(id) = Self::editable_path_node(ctx) else { return smallvec![] };
        let Some(path) = Self::current_path(ctx) else { return smallvec![] };

        let tol_anchor = ctx.view.world_tolerance(ANCHOR_HIT_PX);
        let tol_tangent = ctx.view.world_tolerance(TANGENT_HIT_PX);

        // Anchor hit.
        for (i, a) in path.anchors.iter().enumerate() {
            if (pos - a.pos).length() <= tol_anchor {
                if ctx.modifiers.alt {
                    let new_mode = a.mode.cycled();
                    let (edit_frame, seed) = self.edit_target(ctx, id);
                    let mut cmds: OutputVec = smallvec![];
                    if let Some(seed) = seed {
                        cmds.push(ToolOutput::Commands(smallvec![seed]));
                    }
                    cmds.push(ToolOutput::Commands(smallvec![EditorCommand::EditAnchors {
                        id,
                        frame: edit_frame,
                        edits: vec![AnchorEdit::SetMode { index: i, mode: new_mode }],
                    }]));
                    let mut out = smallvec![ToolOutput::BeginTransaction("Cycle tangent mode".into())];
                    out.extend(cmds);
                    out.push(ToolOutput::CommitTransaction);
                    return out;
                }

                self.selected_anchor = Some(i);
                let (edit_frame, seed) = self.edit_target(ctx, id);
                return self.begin_drag(
                    PathEditState::DragAnchor { node: id, index: i, edit_frame, txn: seed.is_some() },
                    seed,
                );
            }
        }

        // Tangent handle hit.
        for (i, a) in path.anchors.iter().enumerate() {
            let in_tip = a.pos + a.tan_in;
            let out_tip = a.pos + a.tan_out;

            if a.tan_in.length_squared() > 1e-12 && (pos - in_tip).length() <= tol_tangent {
                self.selected_anchor = Some(i);
                let (edit_frame, seed) = self.edit_target(ctx, id);
                return self.begin_drag(
                    PathEditState::DragTanIn { node: id, index: i, edit_frame, txn: seed.is_some() },
                    seed,
                );
            }

            if a.tan_out.length_squared() > 1e-12 && (pos - out_tip).length() <= tol_tangent {
                self.selected_anchor = Some(i);
                let (edit_frame, seed) = self.edit_target(ctx, id);
                return self.begin_drag(
                    PathEditState::DragTanOut { node: id, index: i, edit_frame, txn: seed.is_some() },
                    seed,
                );
            }
        }

        self.selected_anchor = None;
        smallvec![]
    }

    fn moved(&mut self, ctx: &ToolContext, pos: DVec2) -> OutputVec {
        match &mut self.state {
            PathEditState::Idle => smallvec![],

            PathEditState::DragAnchor { node, index, edit_frame, txn } => {
                let mut out: OutputVec = smallvec![];
                if !*txn {
                    out.push(ToolOutput::BeginTransaction("Edit path".into()));
                    *txn = true;
                }
                out.push(ToolOutput::Commands(smallvec![EditorCommand::EditAnchors {
                    id: *node,
                    frame: *edit_frame,
                    edits: vec![AnchorEdit::SetPos { index: *index, pos }],
                }]));
                out
            }

            PathEditState::DragTanIn { node, index, edit_frame, txn } => {
                let anchor_pos = Self::current_path(ctx)
                    .and_then(|p| p.anchors.get(*index).copied())
                    .map(|a| a.pos)
                    .unwrap_or(DVec2::ZERO);
                // Tangent is relative to the anchor, not absolute/world space.
                let tan = pos - anchor_pos;

                let mut out: OutputVec = smallvec![];
                if !*txn {
                    out.push(ToolOutput::BeginTransaction("Edit path".into()));
                    *txn = true;
                }
                out.push(ToolOutput::Commands(smallvec![EditorCommand::EditAnchors {
                    id: *node,
                    frame: *edit_frame,
                    edits: vec![AnchorEdit::SetTanIn { index: *index, tan }],
                }]));
                out
            }

            PathEditState::DragTanOut { node, index, edit_frame, txn } => {
                let anchor_pos = Self::current_path(ctx)
                    .and_then(|p| p.anchors.get(*index).copied())
                    .map(|a| a.pos)
                    .unwrap_or(DVec2::ZERO);
                // Tangent is relative to the anchor, not absolute/world space.
                let tan = pos - anchor_pos;

                let mut out: OutputVec = smallvec![];
                if !*txn {
                    out.push(ToolOutput::BeginTransaction("Edit path".into()));
                    *txn = true;
                }
                out.push(ToolOutput::Commands(smallvec![EditorCommand::EditAnchors {
                    id: *node,
                    frame: *edit_frame,
                    edits: vec![AnchorEdit::SetTanOut { index: *index, tan }],
                }]));
                out
            }
        }
    }

    fn release(&mut self) -> OutputVec {
        let txn = match &self.state {
            PathEditState::Idle => false,
            PathEditState::DragAnchor { txn, .. }
            | PathEditState::DragTanIn { txn, .. }
            | PathEditState::DragTanOut { txn, .. } => *txn,
        };
        self.state = PathEditState::Idle;
        if txn { smallvec![ToolOutput::CommitTransaction] } else { smallvec![] }
    }

    fn escape(&mut self) -> OutputVec {
        let txn = match &self.state {
            PathEditState::Idle => false,
            PathEditState::DragAnchor { txn, .. }
            | PathEditState::DragTanIn { txn, .. }
            | PathEditState::DragTanOut { txn, .. } => *txn,
        };
        self.state = PathEditState::Idle;
        if txn { smallvec![ToolOutput::CancelTransaction] } else { smallvec![] }
    }

    fn delete_anchor(&mut self, ctx: &ToolContext) -> OutputVec {
        let Some(index) = self.selected_anchor else { return smallvec![] };
        let Some(id) = Self::editable_path_node(ctx) else { return smallvec![] };
        let Some(path) = Self::current_path(ctx) else { return smallvec![] };
        if path.anchors.len() <= 2 {
            return smallvec![];
        }

        let (edit_frame, seed) = self.edit_target(ctx, id);

        let mut cmds: OutputVec = smallvec![];
        if let Some(seed) = seed {
            cmds.push(ToolOutput::Commands(smallvec![seed]));
        }
        cmds.push(ToolOutput::Commands(smallvec![EditorCommand::EditAnchors {
            id,
            frame: edit_frame,
            edits: vec![AnchorEdit::Delete { index }],
        }]));

        self.selected_anchor = None;
        let mut out = smallvec![ToolOutput::BeginTransaction("Delete anchor".into())];
        out.extend(cmds);
        out.push(ToolOutput::CommitTransaction);
        out
    }

    fn insert_anchor(&mut self, ctx: &ToolContext, pos: DVec2) -> OutputVec {
        let Some(id) = Self::editable_path_node(ctx) else { return smallvec![] };
        let Some(path) = Self::current_path(ctx) else { return smallvec![] };
        let Some((seg, t, dist)) = path.nearest_segment(pos) else { return smallvec![] };

        if dist > ctx.view.world_tolerance(20.0) {
            return smallvec![];
        }

        let mut new_path = path.clone();
        let _ = new_path.insert_anchor_at(seg, t);

        let (edit_frame, seed) = self.edit_target(ctx, id);

        let mut cmds: OutputVec = smallvec![];
        if let Some(seed) = seed {
            cmds.push(ToolOutput::Commands(smallvec![seed]));
        }

        let value = Value::Path(new_path);
        let prop = PropPath::new("shape.path");
        cmds.push(ToolOutput::Commands(smallvec![match edit_frame {
            Some(frame) => EditorCommand::AddKeyframe { id, prop, frame, value },
            None => EditorCommand::SetStatic { id, prop, value },
        }]));

        self.selected_anchor = Some(seg + 1);
        let mut out = smallvec![ToolOutput::BeginTransaction("Insert anchor".into())];
        out.extend(cmds);
        out.push(ToolOutput::CommitTransaction);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use renamite_animation::Frame;
    use renamite_behavior_common::{Modifiers, Selection, SnapConfig, ViewTransform};
    use renamite_history::{History, ProjectMut};
    use renamite_model::{Document, Scene, evaluate};

    struct World {
        doc: Document,
        clips: renamite_machine::ClipMap,
        clip_order: Vec<renamite_machine::ClipId>,
        machines: renamite_machine::MachineMap,
        machine_order: Vec<renamite_machine::MachineId>,
        start: Option<renamite_machine::MachineId>,
        selection: Selection,
        shape: NodeId,
    }

    impl World {
        fn new() -> Self {
            let mut doc = Document::empty();
            let comp = doc.main;
            let shape = doc.create_node(Node::new("box", NodeKind::Shape(ShapeKind::Rect {
                pos: Animated::new(DVec2::new(100.0, 100.0)),
                size: Animated::new(DVec2::splat(50.0)),
                rounded: Animated::new(0.0),
            })));
            let fill = doc.create_node(Node::new("fill", NodeKind::Style(StyleKind::Fill {
                color: Animated::new(Color::BLACK), rule: FillRule::NonZero,
            })));
            doc.attach(shape, Parent::Comp(comp), 0).unwrap();
            doc.attach(fill, Parent::Comp(comp), 1).unwrap();
            Self {
                doc, clips: Default::default(), clip_order: vec![],
                machines: Default::default(), machine_order: vec![], start: None,
                selection: Selection::default(), shape,
            }
        }
        fn scene(&self) -> Scene { evaluate(&self.doc, self.doc.main, 0.0) }
        fn pm(&mut self) -> ProjectMut<'_> {
            ProjectMut {
                document: &mut self.doc,
                clips: &mut self.clips, clip_order: &mut self.clip_order,
                machines: &mut self.machines, machine_order: &mut self.machine_order,
                start_machine: &mut self.start,
            }
        }
    }

    fn drive(w: &mut World, tool: &mut SelectTool, h: &mut History, ev: CanvasEvent, m: Modifiers) {
        let scene = w.scene();
        let outs = {
            let ctx = ToolContext {
                doc: &w.doc, scene: &scene, comp: w.doc.main, selection: &w.selection,
                playhead: Frame(0), record: false, view: ViewTransform::identity(),
                snap: SnapConfig { grid: None, anchor: false, guide: false }, modifiers: m,
            };
            tool.handle(&ctx, ev)
        };
        for o in outs {
            match o {
                ToolOutput::BeginTransaction(l) => h.begin(l),
                ToolOutput::CommitTransaction => h.commit(),
                ToolOutput::CancelTransaction => h.cancel(&mut w.pm()).unwrap(),
                ToolOutput::Commands(cmds) => for c in cmds { h.apply(&mut w.pm(), c).unwrap(); },
                ToolOutput::RequestSelection(SelectionChange::Set(n)) => w.selection.nodes = n,
                ToolOutput::RequestSelection(SelectionChange::Toggle(n)) => {
                    if let Some(i) = w.selection.nodes.iter().position(|&x| x == n) {
                        w.selection.nodes.remove(i);
                    } else { w.selection.nodes.push(n); }
                }
                _ => {}
            }
        }
    }

    #[test]
    fn click_selects_drag_moves_one_undo() {
        let (mut w, mut t, mut h) = (World::new(), SelectTool::default(), History::new());
        let m = Modifiers::none();
        drive(&mut w, &mut t, &mut h, CanvasEvent::PointerDown { pos: DVec2::new(100.0, 100.0), button: PointerButton::Primary }, m);
        assert_eq!(w.selection.nodes, vec![w.shape]);
        drive(&mut w, &mut t, &mut h, CanvasEvent::PointerMove { pos: DVec2::new(120.0, 100.0) }, m);
        drive(&mut w, &mut t, &mut h, CanvasEvent::PointerMove { pos: DVec2::new(140.0, 110.0) }, m);
        drive(&mut w, &mut t, &mut h, CanvasEvent::PointerUp { pos: DVec2::new(140.0, 110.0), button: PointerButton::Primary }, m);

        let p = w.doc.value_at(w.shape, &PropPath::new("transform.position"), 0.0).unwrap();
        assert_eq!(p, Value::DVec2(DVec2::new(40.0, 10.0))); // moved by total drag delta
        h.undo(&mut w.pm()).unwrap();
        assert!(!h.can_undo(), "whole drag = one undo step");
        assert_eq!(
            w.doc.value_at(w.shape, &PropPath::new("transform.position"), 0.0).unwrap(),
            Value::DVec2(DVec2::ZERO)
        );
    }

    #[test]
    fn escape_cancels_drag_completely() {
        let (mut w, mut t, mut h) = (World::new(), SelectTool::default(), History::new());
        let m = Modifiers::none();
        drive(&mut w, &mut t, &mut h, CanvasEvent::PointerDown { pos: DVec2::new(100.0, 100.0), button: PointerButton::Primary }, m);
        drive(&mut w, &mut t, &mut h, CanvasEvent::PointerMove { pos: DVec2::new(150.0, 100.0) }, m);
        drive(&mut w, &mut t, &mut h, CanvasEvent::KeyDown(Key::Escape), m);
        assert_eq!(
            w.doc.value_at(w.shape, &PropPath::new("transform.position"), 0.0).unwrap(),
            Value::DVec2(DVec2::ZERO)
        );
        assert!(!h.can_undo());
    }

    #[test]
    fn rotation_preserves_multiple_turns() {
        // Pure math pin: 1.5 turns of unwrapping = 3π, not π.
        let mut acc = 0.0;
        let steps = 60;
        for i in 1..=steps {
            let raw = (3.0 * PI) * (i as f64 / steps as f64); // sweep 540° in raw angle space
            acc = unwrap_continuous(acc, wrap(raw));
        }
        assert!((acc - 3.0 * PI).abs() < 1e-9, "acc={acc}, expected 3π");
        fn wrap(a: f64) -> f64 { let mut a = a % TAU; if a > PI { a -= TAU; } a }
    }

    #[test]
    fn rubber_band_selects_contained() {
        let (mut w, mut t, mut h) = (World::new(), SelectTool::default(), History::new());
        let m = Modifiers::none();
        drive(&mut w, &mut t, &mut h, CanvasEvent::PointerDown { pos: DVec2::new(0.0, 0.0), button: PointerButton::Primary }, m);
        drive(&mut w, &mut t, &mut h, CanvasEvent::PointerMove { pos: DVec2::new(300.0, 300.0) }, m);
        drive(&mut w, &mut t, &mut h, CanvasEvent::PointerUp { pos: DVec2::new(300.0, 300.0), button: PointerButton::Primary }, m);
        assert_eq!(w.selection.nodes, vec![w.shape]);
    }

    #[test]
    fn delete_detaches_and_is_undoable() {
        let (mut w, mut t, mut h) = (World::new(), SelectTool::default(), History::new());
        w.selection.nodes = vec![w.shape];
        drive(&mut w, &mut t, &mut h, CanvasEvent::KeyDown(Key::Delete), Modifiers::none());
        assert!(w.doc.locate(w.shape).is_none());
        h.undo(&mut w.pm()).unwrap();
        assert!(w.doc.locate(w.shape).is_some());
    }

    #[test]
    fn rect_tool_creates_group_with_shape_and_fill() {
        let mut w = World::new();
        let mut h = History::new();
        let mut tool = ShapeTool::new(ShapeToolKind::Rect);
        let before = w.doc.compositions[w.doc.main].children.len();
        let scene = w.scene();
        let mut mk = |ev| {
            let ctx = ToolContext {
                doc: &w.doc, scene: &scene, comp: w.doc.main, selection: &w.selection,
                playhead: Frame(0), record: false, view: ViewTransform::identity(),
                snap: SnapConfig { grid: None, anchor: false, guide: false },
                modifiers: Modifiers::none(),
            };
            tool.handle(&ctx, ev)
        };
        let mut all: Vec<ToolOutput> = vec![];
        all.extend(mk(CanvasEvent::PointerDown { pos: DVec2::new(200.0, 200.0), button: PointerButton::Primary }));
        all.extend(mk(CanvasEvent::PointerMove { pos: DVec2::new(260.0, 240.0) }));
        all.extend(mk(CanvasEvent::PointerUp { pos: DVec2::new(260.0, 240.0), button: PointerButton::Primary }));
        drop(scene);
        for o in all {
            match o {
                ToolOutput::BeginTransaction(l) => h.begin(l),
                ToolOutput::CommitTransaction => h.commit(),
                ToolOutput::Commands(cmds) => for c in cmds { h.apply(&mut w.pm(), c).unwrap(); },
                _ => {}
            }
        }
        let comp = &w.doc.compositions[w.doc.main];
        assert_eq!(comp.children.len(), before + 1);
        let group = &w.doc.nodes[comp.children[0]];
        assert_eq!(group.children.len(), 2); // shape + fill
        h.undo(&mut w.pm()).unwrap();
        assert_eq!(w.doc.compositions[w.doc.main].children.len(), before);
    }

    #[test]
    fn shift_constrains_to_square() {
        let (min, max) = constrained_rect(DVec2::ZERO, DVec2::new(40.0, 10.0), true, false);
        assert_eq!(max - min, DVec2::splat(40.0));
    }

    /// Route outputs while recording the first created node (for selection).
    fn route(w: &mut World, h: &mut History, out: OutputVec) -> Option<NodeId> {
        let mut created = None;
        for o in out {
            match o {
                ToolOutput::BeginTransaction(l) => h.begin(l),
                ToolOutput::CommitTransaction => h.commit(),
                ToolOutput::CancelTransaction => h.cancel(&mut w.pm()).unwrap(),
                ToolOutput::Commands(cmds) => for c in cmds {
                    created = h.apply(&mut w.pm(), c).unwrap().created;
                },
                ToolOutput::RequestSelection(SelectionChange::Set(n)) => w.selection.nodes = n,
                ToolOutput::RequestSelection(SelectionChange::Toggle(n)) => {
                    if let Some(i) = w.selection.nodes.iter().position(|&x| x == n) {
                        w.selection.nodes.remove(i);
                    } else { w.selection.nodes.push(n); }
                }
                _ => {}
            }
        }
        created
    }

    /// A group with exactly one Path child (the shape Pen creates).
    struct PathWorld {
        w: World,
        group: NodeId,
        path: NodeId,
    }

    impl PathWorld {
        fn new() -> Self {
            let mut w = World::new();
            let path = w.doc.create_node(Node::new("Shape", NodeKind::Shape(ShapeKind::Path(
                Animated::new(VectorPath {
                    closed: true,
                    anchors: vec![
                        Anchor::corner(DVec2::new(100.0, 100.0)),
                        Anchor::corner(DVec2::new(200.0, 100.0)),
                        Anchor::corner(DVec2::new(200.0, 200.0)),
                        Anchor::corner(DVec2::new(100.0, 200.0)),
                    ],
                }),
            ))));
            let fill = w.doc.create_node(Node::new("Fill", NodeKind::Style(StyleKind::Fill {
                color: Animated::new(Color::BLACK), rule: FillRule::NonZero,
            })));
            let group = w.doc.create_node(Node::new("Path", NodeKind::Group));
            w.doc.attach(path, Parent::Node(group), 0).unwrap();
            w.doc.attach(fill, Parent::Node(group), 1).unwrap();
            w.doc.attach(group, Parent::Comp(w.doc.main), 0).unwrap();
            Self { w, group, path }
        }

        fn drive(&mut self, tool: &mut PathEditTool, h: &mut History, ev: CanvasEvent, m: Modifiers) {
            let scene = self.w.scene();
            let outs = {
                let ctx = ToolContext {
                    doc: &self.w.doc, scene: &scene, comp: self.w.doc.main,
                    selection: &self.w.selection, playhead: Frame(0), record: false,
                    view: ViewTransform::identity(),
                    snap: SnapConfig { grid: None, anchor: false, guide: false },
                    modifiers: m,
                };
                tool.handle(&ctx, ev)
            };
            route(&mut self.w, h, outs);
        }
    }

    fn path_value(w: &World, id: NodeId) -> VectorPath {
        match &w.doc.nodes[id].kind {
            NodeKind::Shape(ShapeKind::Path(a)) => a.value_at(0.0),
            _ => panic!("not a path"),
        }
    }

    #[test]
    fn pen_click_click_enter_creates_open_path() {
        let mut w = World::new();
        let mut h = History::new();
        let mut tool = PenTool::default();
        let before = w.doc.compositions[w.doc.main].children.len();
        let scene = w.scene();
        let mut mk = |ev| {
            let ctx = ToolContext {
                doc: &w.doc, scene: &scene, comp: w.doc.main, selection: &w.selection,
                playhead: Frame(0), record: false, view: ViewTransform::identity(),
                snap: SnapConfig { grid: None, anchor: false, guide: false },
                modifiers: Modifiers::none(),
            };
            tool.handle(&ctx, ev)
        };
        let mut all: Vec<ToolOutput> = vec![];
        for (x, y) in [(100.0, 100.0), (200.0, 100.0)] {
            all.extend(mk(CanvasEvent::PointerDown { pos: DVec2::new(x, y), button: PointerButton::Primary }));
            all.extend(mk(CanvasEvent::PointerUp { pos: DVec2::new(x, y), button: PointerButton::Primary }));
        }
        all.extend(mk(CanvasEvent::KeyDown(Key::Enter)));
        drop(scene);
        let mut committed = false;
        let mut switched = false;
        for o in all {
            match o {
                ToolOutput::BeginTransaction(l) => h.begin(l),
                ToolOutput::CommitTransaction => { h.commit(); committed = true; }
                ToolOutput::Commands(cmds) => for c in cmds { h.apply(&mut w.pm(), c).unwrap(); },
                ToolOutput::SwitchTool(t) => switched = t == ToolId::PathEdit,
                _ => {}
            }
        }
        assert!(committed);
        assert!(switched, "finish must switch to PathEdit");
        let comp = &w.doc.compositions[w.doc.main];
        assert_eq!(comp.children.len(), before + 1);
        let group = &w.doc.nodes[comp.children[0]];
        assert_eq!(group.children.len(), 2);
        match &w.doc.nodes[group.children[0]].kind {
            NodeKind::Shape(ShapeKind::Path(p)) => {
                assert_eq!(p.base.anchors.len(), 2);
                assert!(!p.base.closed);
            }
            _ => panic!("expected path shape"),
        }
        h.undo(&mut w.pm()).unwrap();
        assert_eq!(w.doc.compositions[w.doc.main].children.len(), before);
    }

    #[test]
    fn pen_click_near_first_anchor_closes_path() {
        let mut w = World::new();
        let mut h = History::new();
        let mut tool = PenTool::default();
        let scene = w.scene();
        let mut mk = |ev| {
            let ctx = ToolContext {
                doc: &w.doc, scene: &scene, comp: w.doc.main, selection: &w.selection,
                playhead: Frame(0), record: false, view: ViewTransform::identity(),
                snap: SnapConfig { grid: None, anchor: false, guide: false },
                modifiers: Modifiers::none(),
            };
            tool.handle(&ctx, ev)
        };
        let mut all: Vec<ToolOutput> = vec![];
        for (x, y) in [(100.0, 100.0), (200.0, 100.0), (200.0, 200.0)] {
            all.extend(mk(CanvasEvent::PointerDown { pos: DVec2::new(x, y), button: PointerButton::Primary }));
            all.extend(mk(CanvasEvent::PointerUp { pos: DVec2::new(x, y), button: PointerButton::Primary }));
        }
        // Click near the first anchor: should close, not add a 4th point.
        all.extend(mk(CanvasEvent::PointerDown { pos: DVec2::new(105.0, 105.0), button: PointerButton::Primary }));
        all.extend(mk(CanvasEvent::PointerUp { pos: DVec2::new(105.0, 105.0), button: PointerButton::Primary }));
        drop(scene);
        for o in all {
            match o {
                ToolOutput::BeginTransaction(l) => h.begin(l),
                ToolOutput::CommitTransaction => h.commit(),
                ToolOutput::Commands(cmds) => for c in cmds { h.apply(&mut w.pm(), c).unwrap(); },
                _ => {}
            }
        }
        let comp = &w.doc.compositions[w.doc.main];
        let group = &w.doc.nodes[comp.children[0]];
        match &w.doc.nodes[group.children[0]].kind {
            NodeKind::Shape(ShapeKind::Path(p)) => {
                assert!(p.base.closed);
                assert_eq!(p.base.anchors.len(), 3);
            }
            _ => panic!("expected path shape"),
        }
    }

    #[test]
    fn pen_drag_creates_symmetric_tangent() {
        let mut tool = PenTool::default();
        let scene = renamite_model::Scene::default();
        let doc = Document::empty();
        let sel = Selection::default();
        let ctx = ToolContext {
            doc: &doc, scene: &scene, comp: doc.main, selection: &sel,
            playhead: Frame(0), record: false, view: ViewTransform::identity(),
            snap: SnapConfig { grid: None, anchor: false, guide: false },
            modifiers: Modifiers::none(),
        };
        // First click-drag: anchor 0 becomes smooth/symmetric immediately.
        tool.handle(&ctx, CanvasEvent::PointerDown { pos: DVec2::new(0.0, 0.0), button: PointerButton::Primary });
        tool.handle(&ctx, CanvasEvent::PointerMove { pos: DVec2::new(30.0, 0.0) });
        tool.handle(&ctx, CanvasEvent::PointerUp { pos: DVec2::new(30.0, 0.0), button: PointerButton::Primary });
        let ToolOverlay::PenPreview { anchors, .. } = tool.overlay(&ctx) else { panic!("expected preview") };
        let a = anchors[0];
        assert_eq!(a.mode, TangentMode::Symmetric);
        assert_eq!(a.tan_out, DVec2::new(30.0, 0.0));
        assert_eq!(a.tan_in, DVec2::new(-30.0, 0.0));
    }

    #[test]
    fn pen_escape_cancels_no_undo() {
        let mut tool = PenTool::default();
        let scene = renamite_model::Scene::default();
        let doc = Document::empty();
        let sel = Selection::default();
        let ctx = ToolContext {
            doc: &doc, scene: &scene, comp: doc.main, selection: &sel,
            playhead: Frame(0), record: false, view: ViewTransform::identity(),
            snap: SnapConfig { grid: None, anchor: false, guide: false },
            modifiers: Modifiers::none(),
        };
        tool.handle(&ctx, CanvasEvent::PointerDown { pos: DVec2::new(0.0, 0.0), button: PointerButton::Primary });
        tool.handle(&ctx, CanvasEvent::PointerUp { pos: DVec2::new(0.0, 0.0), button: PointerButton::Primary });
        tool.handle(&ctx, CanvasEvent::KeyDown(Key::Escape));
        let outs = tool.handle(&ctx, CanvasEvent::KeyDown(Key::Enter));
        assert!(outs.is_empty(), "after Esc the path must be discarded");
    }

    #[test]
    fn drag_anchor_moves_position_one_undo() {
        let mut pw = PathWorld::new();
        pw.w.selection.nodes = vec![pw.group]; // group, not the path node
        let mut tool = PathEditTool::default();
        let mut h = History::new();
        pw.drive(&mut tool, &mut h, CanvasEvent::PointerDown { pos: DVec2::new(100.0, 100.0), button: PointerButton::Primary }, Modifiers::none());
        pw.drive(&mut tool, &mut h, CanvasEvent::PointerMove { pos: DVec2::new(120.0, 130.0) }, Modifiers::none());
        pw.drive(&mut tool, &mut h, CanvasEvent::PointerUp { pos: DVec2::new(120.0, 130.0), button: PointerButton::Primary }, Modifiers::none());
        let p = path_value(&pw.w, pw.path);
        assert_eq!(p.anchors[0].pos, DVec2::new(120.0, 130.0));
        h.undo(&mut pw.w.pm()).unwrap();
        assert!(!h.can_undo(), "whole drag = one undo step");
        assert_eq!(path_value(&pw.w, pw.path).anchors[0].pos, DVec2::new(100.0, 100.0));
    }

    #[test]
    fn alt_click_cycles_tangent_mode() {
        let mut pw = PathWorld::new();
        pw.w.selection.nodes = vec![pw.path];
        let mut tool = PathEditTool::default();
        let mut h = History::new();
        let mut alt = Modifiers::none();
        alt.alt = true;
        pw.drive(&mut tool, &mut h, CanvasEvent::PointerDown { pos: DVec2::new(100.0, 100.0), button: PointerButton::Primary }, alt);
        pw.drive(&mut tool, &mut h, CanvasEvent::PointerUp { pos: DVec2::new(100.0, 100.0), button: PointerButton::Primary }, Modifiers::none());
        let p = path_value(&pw.w, pw.path);
        assert_eq!(p.anchors[0].mode, TangentMode::Smooth);
        // And the synthetic tangent from Corner->Smooth is applied.
        assert!(p.anchors[0].tan_out.length() > 0.0);
    }

    #[test]
    fn double_click_segment_inserts_anchor() {
        let mut pw = PathWorld::new();
        pw.w.selection.nodes = vec![pw.path];
        let mut tool = PathEditTool::default();
        let mut h = History::new();
        let n = path_value(&pw.w, pw.path).anchors.len();
        pw.drive(&mut tool, &mut h, CanvasEvent::DoubleClick { pos: DVec2::new(150.0, 100.0) }, Modifiers::none());
        let p = path_value(&pw.w, pw.path);
        assert_eq!(p.anchors.len(), n + 1);
        assert_eq!(tool.selected_anchor, Some(1)); // inserted anchor is active
    }

    #[test]
    fn record_mode_seeds_keyframe_before_edit() {
        let mut pw = PathWorld::new();
        pw.w.selection.nodes = vec![pw.path];
        let mut tool = PathEditTool::default();
        let mut h = History::new();
        // Press with record=true at playhead 0 seeds a keyframe before the edit.
        let scene = pw.w.scene();
        let outs = {
            let ctx = ToolContext {
                doc: &pw.w.doc, scene: &scene, comp: pw.w.doc.main,
                selection: &pw.w.selection, playhead: Frame(0), record: true,
                view: ViewTransform::identity(),
                snap: SnapConfig { grid: None, anchor: false, guide: false },
                modifiers: Modifiers::none(),
            };
            tool.handle(&ctx, CanvasEvent::PointerDown { pos: DVec2::new(100.0, 100.0), button: PointerButton::Primary })
        };
        route(&mut pw.w, &mut h, outs);
        // The seed AddKeyframe was applied even though the drag is still open.
        assert!(pw.w.doc.keyframe_data(pw.path, &PropPath::new("shape.path"), Frame(0)).is_some());
    }
}