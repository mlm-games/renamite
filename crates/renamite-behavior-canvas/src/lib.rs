//! Canvas tools: Select/Transform (click, drag-move, rotate/scale handles,
//! rubber band, delete) and Rect/Ellipse creation. Pure state machines:
//! world-space events in, EditorCommands out. Behavior reference (clean-room,
//! observed): rotation handle preserves direction and multiple full turns;
//! Shift snaps rotation to 15° and constrains shapes to square/circle;
//! drag = one undo step; Esc cancels the open drag.

use glam::DVec2;
use renamite_animation::{Angle, Animated};
use renamite_behavior_common::ToolContext;
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
}

pub struct ToolSet {
    pub select: SelectTool,
    pub rect: ShapeTool,
    pub ellipse: ShapeTool,
}

impl Default for ToolSet {
    fn default() -> Self {
        Self {
            select: SelectTool::default(),
            rect: ShapeTool::new(ShapeToolKind::Rect),
            ellipse: ShapeTool::new(ShapeToolKind::Ellipse),
        }
    }
}

impl ToolSet {
    pub fn handle(&mut self, id: ToolId, ctx: &ToolContext, ev: CanvasEvent) -> OutputVec {
        match id {
            ToolId::Select | ToolId::Transform => self.select.handle(ctx, ev),
            ToolId::Rect => self.rect.handle(ctx, ev),
            ToolId::Ellipse => self.ellipse.handle(ctx, ev),
            _ => smallvec![], // Pen/PathEdit/Star/Gradient/Fill: not implemented yet
        }
    }

    pub fn overlay(&self, id: ToolId, ctx: &ToolContext) -> ToolOverlay {
        match id {
            ToolId::Select | ToolId::Transform => self.select.overlay(ctx),
            ToolId::Rect => self.rect.overlay(ctx),
            ToolId::Ellipse => self.ellipse.overlay(ctx),
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
        // 1. Handles win over shapes — single-node selection only (v1).
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
}