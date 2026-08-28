//! Canvas tools: Select/Transform (click, drag-move, rotate/scale handles,
//! rubber band, delete) and Rect/Ellipse/Star creation. Pure state machines:
//! world-space events in, EditorCommands out. Behavior reference (clean-room,
//! observed): rotation handle preserves direction and multiple full turns;
//! Shift snaps rotation to 15° and constrains shapes to square/circle
//! (Shift on the star tool draws a regular polygon instead; Alt on the star
//! tool draws from center with 6 points instead of 5);
//! drag = one undo step; Esc cancels the open drag.

use glam::DVec2;
use kurbo::{Affine, ParamCurveNearest, Point, Shape as KurboShape};

use renamite_animation::{Angle, Animated, Frame};
use renamite_behavior_common::{ToolContext, fill::cmd_fill_shape, path::path_edit_target};
use renamite_geometry::{Anchor, AnchorEdit, TangentMode, VectorPath};
use renamite_history::{
    EditorCommand, NodeTree, OutputVec, SelectionChange, ToolId, ToolOutput, resolve_property_edit,
};
use renamite_model::{
    Document, FillRule, GradientKind, Node, NodeId, NodeKind, PaintKind, Parent, PropPath,
    ShapeKind, StarKind, StyleKind, StylePaint, Value, immediate_child_below, node_affine,
    node_is_ancestor, node_transform_context, pick, pick_box, selected_ancestor_for_pick,
    selection_bounds, world_delta_to_parent,
};
use smallvec::{SmallVec, smallvec};
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
pub enum PointerButton {
    Primary,
    Secondary,
    Middle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Escape,
    Delete,
    Backspace,
    Enter,

    /// Insert a node at the midpoint of the segment adjacent to the selection.
    Insert,
    /// Cycle the selected anchor (Shift reverses direction).
    Tab,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,

    /// Semantic node-edit chords (mapped from Shift+letter by the host):
    /// tangent modes and segment conversion.
    NodeCorner,
    NodeSmooth,
    NodeSymmetric,
    SegmentLine,
    SegmentCurve,

    /// Shift+A: synthesize Catmull-Rom-style tangents on selected anchors.
    NodeAutoSmooth,
    /// Shift+B: split the contour at the selected anchor (closed -> open;
    /// open interior anchor -> two contours in a compound path).
    NodeBreak,
    /// Shift+J: join two selected endpoints (closes a contour when they are
    /// its opposite ends, otherwise concatenates end-to-start).
    NodeJoin,
}

/// World-space overlay for the host to draw (screen conversion is the host's job).
#[derive(Clone, Debug, PartialEq)]
pub enum ToolOverlay {
    None,
    RubberBand {
        min: DVec2,
        max: DVec2,
    },
    /// Selection bounds + handle anchor points.
    Selection {
        min: DVec2,
        max: DVec2,
        rotate: DVec2,
        scale: DVec2,

        /// Exact transform anchor in world coordinates for one selected node.
        /// Multi-selection has no editable shared pivot in v1.
        pivot: Option<DVec2>,
    },
    ShapePreview {
        min: DVec2,
        max: DVec2,
        kind: ShapePreviewKind,
    },
    PenPreview {
        anchors: Vec<Anchor>,
        closed: bool,
        hover: Option<DVec2>,
    },
    PathHandles {
        /// Primary contour (`active_anchor` indexes into this one).
        path: VectorPath,
        /// Additional contours of a compound path (display only).
        extra: Vec<VectorPath>,
        active_anchor: Option<usize>,
    },
    /// Gradient axis being dragged (world space: start=end handle endpoints).
    GradientLine {
        start: DVec2,
        end: DVec2,
        radial: bool,
    },
}

/// Which primitive a drag is previewing (shapes an extra rubber-band outline).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapePreviewKind {
    Rect,
    Ellipse,
    Star,
    Polygon,
}

pub struct ToolSet {
    pub select: SelectTool,
    pub rect: ShapeTool,
    pub ellipse: ShapeTool,
    pub star: ShapeTool,
    pub text: TextTool,
    pub pen: PenTool,
    pub path_edit: PathEditTool,
    pub gradient: GradientTool,
    pub fill: FillTool,
    pub dropper: DropperTool,
}

impl Default for ToolSet {
    fn default() -> Self {
        Self {
            select: SelectTool::default(),
            rect: ShapeTool::new(ShapeToolKind::Rect),
            ellipse: ShapeTool::new(ShapeToolKind::Ellipse),
            star: ShapeTool::new(ShapeToolKind::Star),
            text: TextTool,
            pen: PenTool::default(),
            path_edit: PathEditTool::default(),
            gradient: GradientTool::default(),
            fill: FillTool,
            dropper: DropperTool,
        }
    }
}

impl ToolSet {
    pub fn handle(&mut self, id: ToolId, ctx: &ToolContext, ev: CanvasEvent) -> OutputVec {
        match id {
            ToolId::Select | ToolId::Transform => self.select.handle(ctx, ev),
            ToolId::Rect => self.rect.handle(ctx, ev),
            ToolId::Ellipse => self.ellipse.handle(ctx, ev),
            ToolId::Star => self.star.handle(ctx, ev),
            ToolId::Text => self.text.handle(ctx, ev),
            ToolId::Pen => self.pen.handle(ctx, ev),
            ToolId::PathEdit => self.path_edit.handle(ctx, ev),
            ToolId::Gradient => self.gradient.handle(ctx, ev),
            ToolId::Fill => self.fill.handle(ctx, ev),
            ToolId::Dropper => self.dropper.handle(ctx, ev),
        }
    }

    pub fn overlay(&self, id: ToolId, ctx: &ToolContext) -> ToolOverlay {
        match id {
            ToolId::Select | ToolId::Transform => self.select.overlay(ctx),
            ToolId::Rect => self.rect.overlay(ctx),
            ToolId::Ellipse => self.ellipse.overlay(ctx),
            ToolId::Star => self.star.overlay(ctx),
            ToolId::Text => self.text.overlay(ctx),
            ToolId::Pen => self.pen.overlay(ctx),
            ToolId::PathEdit => self.path_edit.overlay(ctx),
            ToolId::Gradient => self.gradient.overlay(ctx),
            ToolId::Fill => self.fill.overlay(ctx),
            ToolId::Dropper => self.dropper.overlay(ctx),
        }
    }

    pub fn is_dragging(&self, id: ToolId) -> bool {
        match id {
            ToolId::Select | ToolId::Transform => self.select.is_dragging(),
            ToolId::Rect | ToolId::Ellipse | ToolId::Star => {
                let t = match id {
                    ToolId::Rect => &self.rect,
                    ToolId::Ellipse => &self.ellipse,
                    ToolId::Star => &self.star,
                    _ => unreachable!(),
                };
                t.is_dragging()
            }
            ToolId::Pen => self.pen.is_dragging(),
            ToolId::PathEdit => self.path_edit.is_dragging(),
            ToolId::Gradient => self.gradient.is_dragging(),
            _ => false,
        }
    }
}

const DRAG_THRESHOLD_PX: f64 = 3.0;
const HANDLE_PX: f64 = 8.0;
const ROTATE_OFFSET_PX: f64 = 24.0;

enum SelState {
    Idle,
    Pending {
        press: DVec2,
        node: NodeId,
    },
    DragMove {
        last: DVec2,
        txn: bool,
    },
    RubberBand {
        start: DVec2,
        current: DVec2,
    },
    DragRotate {
        pivot: DVec2,
        start: f64,
        acc: f64,
        node: NodeId,
        base_deg: f64,
        /// Position at drag start.
        base_position: DVec2,
        /// World to parent coordinate transform captured at drag start.
        world_to_parent: Affine,
        txn: bool,
    },
    DragScale {
        pivot: DVec2,
        start_dist: f64,
        node: NodeId,
        base: DVec2,
        base_position: DVec2,
        world_to_parent: Affine,
        txn: bool,
    },
    DragPivot {
        node: NodeId,
        base_anchor: DVec2,
        base_position: DVec2,

        /// World -> parent coordinate transform captured at drag start.
        world_to_parent: Affine,

        /// Parent-space vector -> local-anchor-space vector.
        parent_to_anchor: Affine,

        txn: bool,
    },
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
            return ToolOverlay::RubberBand {
                min: start.min(*current),
                max: start.max(*current),
            };
        }
        let Some((min, max)) = selection_bounds(ctx.doc, ctx.scene, &ctx.selection.nodes) else {
            return ToolOverlay::None;
        };

        let (rotate, scale) = handles(ctx, min, max);

        let pivot = match ctx.selection.nodes.as_slice() {
            [node] => node_transform_context(ctx.doc, *node, ctx.playhead.0 as f64)
                .map(|context| context.pivot_world),

            _ => None,
        };

        ToolOverlay::Selection {
            min,
            max,
            rotate,
            scale,
            pivot,
        }
    }

    pub fn is_dragging(&self) -> bool {
        matches!(
            self.state,
            Some(SelState::Pending { .. })
                | Some(SelState::DragMove { .. })
                | Some(SelState::RubberBand { .. })
                | Some(SelState::DragRotate { .. })
                | Some(SelState::DragScale { .. })
                | Some(SelState::DragPivot { .. })
        )
    }

    pub fn handle(&mut self, ctx: &ToolContext, ev: CanvasEvent) -> OutputVec {
        match ev {
            CanvasEvent::PointerDown {
                pos,
                button: PointerButton::Primary,
            } => self.press(ctx, pos),
            CanvasEvent::PointerMove { pos } => self.moved(ctx, pos),
            CanvasEvent::PointerUp {
                pos,
                button: PointerButton::Primary,
            } => self.release(ctx, pos),
            CanvasEvent::KeyDown(Key::Escape) => self.escape(),
            CanvasEvent::KeyDown(Key::Delete) | CanvasEvent::KeyDown(Key::Backspace) => {
                self.delete(ctx)
            }
            CanvasEvent::DoubleClick { pos } => self.double_click(ctx, pos),
            _ => smallvec![],
        }
    }

    fn press(&mut self, ctx: &ToolContext, pos: DVec2) -> OutputVec {
        // 1. Handles win over shapes - single-node selection only (v1).
        if let [node] = ctx.selection.nodes.as_slice()
            && let Some((min, max)) = selection_bounds(ctx.doc, ctx.scene, &ctx.selection.nodes)
        {
            let tolerance = ctx.view.world_tolerance(HANDLE_PX);
            let transform = node_transform_context(ctx.doc, *node, ctx.playhead.0 as f64);

            if let Some(transform) = transform
                && (pos - transform.pivot_world).length() <= tolerance
            {
                let parent_det = determinant(transform.parent_world);
                let linear_det = determinant(transform.linear);

                if parent_det.abs() > 1e-12 && linear_det.abs() > 1e-12 {
                    self.state = Some(SelState::DragPivot {
                        node: *node,
                        base_anchor: transform.anchor,
                        base_position: transform.position,
                        world_to_parent: transform.parent_world.inverse(),
                        parent_to_anchor: transform.linear.inverse(),
                        txn: false,
                    });

                    return smallvec![];
                }
            }

            let (rotate, scale) = handles(ctx, min, max);
            let pivot = (min + max) * 0.5;

            if (pos - rotate).length() <= tolerance {
                let world_to_parent = transform.map(|t| t.parent_world.inverse());

                *self.st() = SelState::DragRotate {
                    pivot,
                    start: angle_of(pos - pivot),
                    acc: 0.0,
                    node: *node,
                    base_deg: rotation_deg(ctx, *node),
                    base_position: position_of(ctx, *node),
                    world_to_parent: world_to_parent.unwrap_or(Affine::IDENTITY),
                    txn: false,
                };

                return smallvec![];
            }

            if (pos - scale).length() <= tolerance {
                let world_to_parent = transform.map(|t| t.parent_world.inverse());

                *self.st() = SelState::DragScale {
                    pivot: min,
                    start_dist: (pos - min).length().max(1e-6),
                    node: *node,
                    base: scale_of(ctx, *node),
                    base_position: position_of(ctx, *node),
                    world_to_parent: world_to_parent.unwrap_or(Affine::IDENTITY),
                    txn: false,
                };

                return smallvec![];
            }
        }
        match pick(ctx.scene, pos).filter(|n| !ctx.doc.nodes.get(*n).is_none_or(|x| x.locked)) {
            Some(picked) => {
                let target = selected_ancestor_for_pick(ctx.doc, picked, &ctx.selection.nodes)
                    .unwrap_or(picked);

                let mut out: OutputVec = smallvec![];
                if ctx.modifiers.ctrl {
                    out.push(ToolOutput::RequestSelection(SelectionChange::Toggle(
                        target,
                    )));
                } else if ctx.modifiers.shift {
                    if !ctx.selection.contains(target) {
                        let mut s = ctx.selection.nodes.clone();
                        s.push(target);
                        out.push(ToolOutput::RequestSelection(SelectionChange::Set(s)));
                    }
                } else if !ctx.selection.contains(target) {
                    out.push(ToolOutput::RequestSelection(SelectionChange::Set(vec![
                        target,
                    ])));
                }
                *self.st() = SelState::Pending {
                    press: pos,
                    node: target,
                };
                out
            }
            None => {
                let mut out: OutputVec = smallvec![];
                if !ctx.modifiers.shift && !ctx.modifiers.ctrl {
                    out.push(ToolOutput::RequestSelection(SelectionChange::Set(vec![])));
                }
                *self.st() = SelState::RubberBand {
                    start: pos,
                    current: pos,
                };
                out
            }
        }
    }

    fn moved(&mut self, ctx: &ToolContext, pos: DVec2) -> OutputVec {
        match self.st() {
            SelState::Pending { press, .. } => {
                if (pos - *press).length() >= ctx.view.world_tolerance(DRAG_THRESHOLD_PX) {
                    let press = *press;
                    *self.st() = SelState::DragMove {
                        last: press,
                        txn: false,
                    };
                    return self.moved(ctx, pos);
                }
                smallvec![]
            }
            SelState::DragMove { last, txn } => {
                let delta = pos - *last;
                *last = pos;
                if delta.length_squared() == 0.0 {
                    return smallvec![];
                }
                let mut out: OutputVec = smallvec![];
                if !*txn {
                    out.push(ToolOutput::BeginTransaction("Move".into()));
                    *txn = true;
                }
                let prop = PropPath::new("transform.position");
                let cmds = ctx
                    .selection
                    .nodes
                    .iter()
                    .filter_map(|&node| {
                        let Ok(Value::DVec2(current)) =
                            ctx.doc.value_at(node, &prop, ctx.playhead.0 as f64)
                        else {
                            return None;
                        };
                        let local_delta =
                            world_delta_to_parent(ctx.doc, node, ctx.playhead.0 as f64, delta)
                                .unwrap_or(delta);
                        Some(resolve_property_edit(
                            ctx.doc,
                            node,
                            &prop,
                            Value::DVec2(current + local_delta),
                            ctx.playhead,
                            ctx.record,
                        ))
                    })
                    .collect();
                out.push(ToolOutput::Commands(cmds));
                out
            }
            SelState::DragRotate {
                pivot,
                start,
                acc,
                node,
                base_deg,
                base_position,
                world_to_parent,
                txn,
            } => {
                let raw = angle_of(pos - *pivot) - *start;
                *acc = unwrap_continuous(*acc, raw);
                let mut deg = *base_deg + acc.to_degrees();
                if ctx.modifiers.shift {
                    deg = (deg / 15.0).round() * 15.0;
                }
                let delta_rad = (deg - *base_deg).to_radians();
                let parent_pt = *world_to_parent * Point::new(pivot.x, pivot.y);
                let u = DVec2::new(parent_pt.x, parent_pt.y) - *base_position;
                let new_position = *base_position + u - affine_vector(Affine::rotate(delta_rad), u);

                let (node, base) = (*node, *txn);
                let mut out: OutputVec = smallvec![];
                if !base {
                    out.push(ToolOutput::BeginTransaction("Rotate".into()));
                    *txn = true;
                }
                out.push(ToolOutput::Commands(smallvec![
                    resolve_property_edit(
                        ctx.doc,
                        node,
                        &PropPath::new("transform.rotation"),
                        Value::Angle(Angle(deg)),
                        ctx.playhead,
                        ctx.record,
                    ),
                    resolve_property_edit(
                        ctx.doc,
                        node,
                        &PropPath::new("transform.position"),
                        Value::DVec2(new_position),
                        ctx.playhead,
                        ctx.record,
                    ),
                ]));
                out
            }
            SelState::DragScale {
                pivot,
                start_dist,
                node,
                base,
                base_position,
                world_to_parent,
                txn,
            } => {
                let factor = ((pos - *pivot).length() / *start_dist).max(0.01);
                let new = *base * factor; // uniform (v1)
                let parent_pt = *world_to_parent * Point::new(pivot.x, pivot.y);
                let new_position = *base_position
                    + (DVec2::new(parent_pt.x, parent_pt.y) - *base_position) * (1.0 - factor);

                let (node, started) = (*node, *txn);
                let mut out: OutputVec = smallvec![];
                if !started {
                    out.push(ToolOutput::BeginTransaction("Scale".into()));
                    *txn = true;
                }
                out.push(ToolOutput::Commands(smallvec![
                    resolve_property_edit(
                        ctx.doc,
                        node,
                        &PropPath::new("transform.scale"),
                        Value::DVec2(new),
                        ctx.playhead,
                        ctx.record,
                    ),
                    resolve_property_edit(
                        ctx.doc,
                        node,
                        &PropPath::new("transform.position"),
                        Value::DVec2(new_position),
                        ctx.playhead,
                        ctx.record,
                    ),
                ]));
                out
            }
            SelState::DragPivot {
                node,
                base_anchor,
                base_position,
                world_to_parent,
                parent_to_anchor,
                txn,
            } => {
                let parent_point = *world_to_parent * Point::new(pos.x, pos.y);

                let new_position = DVec2::new(parent_point.x, parent_point.y);

                let position_delta = new_position - *base_position;

                let anchor_delta = affine_vector(*parent_to_anchor, position_delta);

                let new_anchor = *base_anchor + anchor_delta;

                let mut output: OutputVec = smallvec![];

                if !*txn {
                    output.push(ToolOutput::BeginTransaction("Move pivot".into()));

                    *txn = true;
                }

                let anchor_command = resolve_property_edit(
                    ctx.doc,
                    *node,
                    &PropPath::new("transform.anchor"),
                    Value::DVec2(new_anchor),
                    ctx.playhead,
                    ctx.record,
                );

                let position_command = resolve_property_edit(
                    ctx.doc,
                    *node,
                    &PropPath::new("transform.position"),
                    Value::DVec2(new_position),
                    ctx.playhead,
                    ctx.record,
                );

                output.push(ToolOutput::Commands(smallvec![
                    anchor_command,
                    position_command,
                ]));

                output
            }
            SelState::RubberBand { current, .. } => {
                *current = pos;
                smallvec![ToolOutput::Invalidate]
            }
            SelState::Idle => smallvec![],
        }
    }

    fn release(&mut self, ctx: &ToolContext, _pos: DVec2) -> OutputVec {
        match std::mem::replace(self.st(), SelState::Idle) {
            SelState::DragMove { txn, .. }
            | SelState::DragRotate { txn, .. }
            | SelState::DragScale { txn, .. }
            | SelState::DragPivot { txn, .. } => {
                if txn {
                    smallvec![ToolOutput::CommitTransaction]
                } else {
                    smallvec![]
                }
            }
            SelState::Pending { node, .. } => {
                // Plain click on already-multi-selected collapses to just it.
                if !ctx.modifiers.ctrl && !ctx.modifiers.shift && ctx.selection.nodes.len() > 1 {
                    smallvec![ToolOutput::RequestSelection(SelectionChange::Set(vec![
                        node
                    ]))]
                } else {
                    smallvec![]
                }
            }
            SelState::RubberBand { start, current } => {
                let (min, max) = (start.min(current), start.max(current));
                let mut picked = pick_box(ctx.scene, min, max);
                if ctx.modifiers.shift || ctx.modifiers.ctrl {
                    let mut s = ctx.selection.nodes.clone();
                    for n in picked.drain(..) {
                        if !s.contains(&n) {
                            s.push(n);
                        }
                    }
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
            | SelState::DragScale { txn: true, .. }
            | SelState::DragPivot { txn: true, .. } => smallvec![ToolOutput::CancelTransaction],
            _ => smallvec![],
        }
    }

    fn delete(&mut self, ctx: &ToolContext) -> OutputVec {
        if ctx.selection.is_empty() {
            return smallvec![];
        }
        let cmds = ctx
            .selection
            .nodes
            .iter()
            .map(|&id| EditorCommand::RemoveNode { id })
            .collect();
        smallvec![
            ToolOutput::BeginTransaction("Delete".into()),
            ToolOutput::Commands(cmds),
            ToolOutput::CommitTransaction,
            ToolOutput::RequestSelection(SelectionChange::Set(vec![])),
        ]
    }

    fn double_click(&mut self, ctx: &ToolContext, pos: DVec2) -> OutputVec {
        let Some(picked) = pick(ctx.scene, pos) else {
            return smallvec![];
        };

        let target = match ctx.selection.nodes.as_slice() {
            [selected] if node_is_ancestor(ctx.doc, *selected, picked) => {
                immediate_child_below(ctx.doc, *selected, picked).unwrap_or(picked)
            }

            _ => picked,
        };

        smallvec![ToolOutput::RequestSelection(SelectionChange::Set(vec![
            target
        ]),)]
    }
}

fn handles(ctx: &ToolContext, min: DVec2, max: DVec2) -> (DVec2, DVec2) {
    let cx = (min.x + max.x) * 0.5;
    let rot = DVec2::new(cx, min.y - ctx.view.world_tolerance(ROTATE_OFFSET_PX));
    (rot, max) // scale handle = bottom-right corner
}

fn rotation_deg(ctx: &ToolContext, node: NodeId) -> f64 {
    match ctx.doc.value_at(
        node,
        &PropPath::new("transform.rotation"),
        ctx.playhead.0 as f64,
    ) {
        Ok(Value::Angle(a)) => a.0,
        Ok(Value::F64(v)) => v,
        _ => 0.0,
    }
}

fn scale_of(ctx: &ToolContext, node: NodeId) -> DVec2 {
    match ctx.doc.value_at(
        node,
        &PropPath::new("transform.scale"),
        ctx.playhead.0 as f64,
    ) {
        Ok(Value::DVec2(v)) => v,
        _ => DVec2::splat(100.0),
    }
}

fn position_of(ctx: &ToolContext, node: NodeId) -> DVec2 {
    match ctx.doc.value_at(
        node,
        &PropPath::new("transform.position"),
        ctx.playhead.0 as f64,
    ) {
        Ok(Value::DVec2(v)) => v,
        _ => DVec2::ZERO,
    }
}

fn angle_of(v: DVec2) -> f64 {
    v.y.atan2(v.x)
}

fn determinant(affine: Affine) -> f64 {
    let [a, b, c, d, _, _] = affine.as_coeffs();
    a * d - b * c
}

fn affine_vector(affine: Affine, value: DVec2) -> DVec2 {
    let [a, b, c, d, _, _] = affine.as_coeffs();

    DVec2::new(a * value.x + c * value.y, b * value.x + d * value.y)
}

/// Multi-turn unwrap: keep `raw` continuous with `acc` (Glaxnimate 0.6:
/// 3 physical turns = 1080°, never re-wrapped to 0).
fn unwrap_continuous(acc: f64, raw: f64) -> f64 {
    let mut d = raw - acc;
    while d > PI {
        d -= TAU;
    }
    while d < -PI {
        d += TAU;
    }
    acc + d
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GradHandle {
    Start,
    End,
}

#[derive(Default)]
enum GradState {
    #[default]
    Idle,
    Drag {
        /// World-space press point (drag-threshold anchor).
        press: DVec2,
        /// The style node (Fill/Stroke) whose paint is being edited.
        style: NodeId,
        kind: GradientKind,
        /// Shape-local <-> world (gradient handles are authored in the
        /// shape's local space and folded with its own affine).
        l2w: Affine,
        w2l: Affine,
        start: DVec2,
        end: DVec2,
        active: GradHandle,
        txn: bool,
        /// True once ConvertToGradient was emitted (solid -> gradient).
        converted: bool,
        /// True once the pointer passed the drag threshold.
        dragging: bool,
    },
}

#[derive(Default)]
pub struct GradientTool {
    state: GradState,
}

impl GradientTool {
    pub fn is_dragging(&self) -> bool {
        matches!(
            self.state,
            GradState::Drag { dragging: true, .. }
        )
    }

    pub fn overlay(&self, _ctx: &ToolContext) -> ToolOverlay {
        let GradState::Drag {
            l2w,
            start,
            end,
            kind,
            dragging,
            ..
        } = &self.state
        else {
            return ToolOverlay::None;
        };
        if !*dragging {
            return ToolOverlay::None;
        }
        let s = *l2w * Point::new(start.x, start.y);
        let e = *l2w * Point::new(end.x, end.y);
        ToolOverlay::GradientLine {
            start: DVec2::new(s.x, s.y),
            end: DVec2::new(e.x, e.y),
            radial: *kind == GradientKind::Radial,
        }
    }

    pub fn handle(&mut self, ctx: &ToolContext, ev: CanvasEvent) -> OutputVec {
        match ev {
            CanvasEvent::PointerDown {
                pos,
                button: PointerButton::Primary,
            } => self.press(ctx, pos),
            CanvasEvent::PointerMove { pos } => self.moved(ctx, pos),
            CanvasEvent::PointerUp {
                pos,
                button: PointerButton::Primary,
            } => self.release(pos),
            CanvasEvent::KeyDown(Key::Escape) => self.escape(),
            _ => smallvec![],
        }
    }

    fn press(&mut self, ctx: &ToolContext, pos: DVec2) -> OutputVec {
        let Some(shape) =
            pick(ctx.scene, pos).filter(|n| !ctx.doc.nodes.get(*n).is_none_or(|x| x.locked))
        else {
            return smallvec![];
        };
        // Prefer the topmost Fill item for this shape; fall back to stroke.
        let item = ctx
            .scene
            .items
            .iter()
            .rev()
            .find(|it| it.node == shape && matches!(it.kind, PaintKind::Fill(_)))
            .or_else(|| ctx.scene.items.iter().rev().find(|it| it.node == shape));
        let Some(item) = item else {
            return smallvec![];
        };
        let style = item.style;
        let l2w = node_affine(ctx.doc, shape, ctx.playhead.0 as f64);
        let w2l = l2w.inverse();
        let local = {
            let p = w2l * Point::new(pos.x, pos.y);
            DVec2::new(p.x, p.y)
        };
        let frame = ctx.playhead.0 as f64;
        let existing = ctx.doc.nodes.get(style).and_then(|n| match &n.kind {
            NodeKind::Style(st) => Some(st.paint().clone()),
            _ => None,
        });
        let (kind, start, end, active, converted) = match existing {
            Some(StylePaint::Gradient(g)) => {
                let s = g.start.value_at(frame);
                let e = g.end.value_at(frame);
                // Grab whichever handle is nearer the pointer.
                let ws = {
                    let p = l2w * Point::new(s.x, s.y);
                    DVec2::new(p.x, p.y)
                };
                let we = {
                    let p = l2w * Point::new(e.x, e.y);
                    DVec2::new(p.x, p.y)
                };
                let active = if (pos - ws).length() < (pos - we).length() {
                    GradHandle::Start
                } else {
                    GradHandle::End
                };
                (g.kind, s, e, active, true)
            }
            _ => {
                let kind = if ctx.modifiers.shift {
                    GradientKind::Radial
                } else {
                    GradientKind::Linear
                };
                (kind, local, local, GradHandle::End, false)
            }
        };
        self.state = GradState::Drag {
            press: pos,
            style,
            kind,
            l2w,
            w2l,
            start,
            end,
            active,
            txn: false,
            converted,
            dragging: false,
        };
        smallvec![ToolOutput::RequestSelection(SelectionChange::Set(vec![
            shape
        ]))]
    }

    fn moved(&mut self, ctx: &ToolContext, pos: DVec2) -> OutputVec {
        let GradState::Drag {
            press,
            style,
            kind,
            w2l,
            start,
            end,
            active,
            txn,
            converted,
            dragging,
            ..
        } = &mut self.state
        else {
            return smallvec![];
        };
        if (pos - *press).length() < ctx.view.world_tolerance(DRAG_THRESHOLD_PX) {
            return smallvec![];
        }
        *dragging = true;
        let local = {
            let p = *w2l * Point::new(pos.x, pos.y);
            DVec2::new(p.x, p.y)
        };
        match active {
            GradHandle::Start => *start = local,
            GradHandle::End => *end = local,
        }
        let mut out: OutputVec = smallvec![];
        if !*txn {
            out.push(ToolOutput::BeginTransaction("Gradient".into()));
            *txn = true;
        }
        if !*converted {
            // Solid -> gradient: seed the axis so the convert is invisible
            // until the drag separates the handles; stops hold the base color.
            out.push(ToolOutput::Commands(smallvec![
                EditorCommand::ConvertToGradient {
                    id: *style,
                    kind: *kind,
                    start: *start,
                    end: *end,
                }
            ]));
            *converted = true;
        } else {
            let (prop, value) = match active {
                GradHandle::Start => ("grad.start", *start),
                GradHandle::End => ("grad.end", *end),
            };
            out.push(ToolOutput::Commands(smallvec![resolve_property_edit(
                ctx.doc,
                *style,
                &PropPath::new(prop),
                Value::DVec2(value),
                ctx.playhead,
                ctx.record,
            )]));
        }
        out
    }

    fn release(&mut self, _pos: DVec2) -> OutputVec {
        match std::mem::replace(&mut self.state, GradState::Idle) {
            GradState::Drag { txn: true, .. } => smallvec![ToolOutput::CommitTransaction],
            _ => smallvec![],
        }
    }

    fn escape(&mut self) -> OutputVec {
        match std::mem::replace(&mut self.state, GradState::Idle) {
            GradState::Drag { txn: true, .. } => smallvec![ToolOutput::CancelTransaction],
            _ => smallvec![],
        }
    }
}

#[derive(Default)]
pub struct FillTool;

impl FillTool {
    pub fn overlay(&self, _ctx: &ToolContext) -> ToolOverlay {
        ToolOverlay::None
    }

    pub fn handle(&mut self, ctx: &ToolContext, ev: CanvasEvent) -> OutputVec {
        match ev {
            CanvasEvent::PointerDown {
                pos,
                button: PointerButton::Primary,
            } => {
                let Some(shape) = pick(ctx.scene, pos) else {
                    return smallvec![];
                };

                if ctx.doc.nodes.get(shape).is_none_or(|n| n.locked) {
                    return smallvec![];
                }

                let paint = ctx.current_paint.snapshot(ctx.playhead.0 as f64);

                let Some(cmd) = cmd_fill_shape(ctx.doc, shape, paint) else {
                    return smallvec![];
                };

                smallvec![
                    ToolOutput::BeginTransaction("Fill".into()),
                    ToolOutput::Commands(smallvec![cmd]),
                    ToolOutput::CommitTransaction,
                ]
            }
            _ => smallvec![],
        }
    }
}

/// Eyedropper: sample the resolved paint under the pointer, update the
/// current-paint swatch, and (optionally) push it onto the selection's styles.
///
/// - Default click applies the sampled paint to selected Fill styles.
/// - Shift+click applies to Stroke styles.
/// - Alt samples the color but preserves the target's existing alpha.
#[derive(Default)]
pub struct DropperTool;

impl DropperTool {
    pub fn overlay(&self, _ctx: &ToolContext) -> ToolOverlay {
        ToolOverlay::None
    }

    pub fn handle(&mut self, ctx: &ToolContext, ev: CanvasEvent) -> OutputVec {
        let CanvasEvent::PointerDown {
            pos,
            button: PointerButton::Primary,
        } = ev
        else {
            return smallvec![];
        };

        // Topmost item whose geometry covers (or nearly covers) the point.
        let Some(item) = ctx
            .scene
            .items
            .iter()
            .rev()
            .find(|it| it.opacity > 0.0 && paint_covers(it, pos))
            .map(|it| it.clone())
        else {
            return smallvec![];
        };

        let mut sampled = item.paint.color_at(pos);
        if ctx.modifiers.alt {
            // Preserve the current swatch alpha; only take the sampled RGB.
            sampled.a = ctx.current_paint.base_color().a;
        }
        let paint = StylePaint::solid(sampled);

        let targets = style_targets(ctx.doc, &ctx.selection.nodes, ctx.modifiers.shift);
        if targets.is_empty() {
            return smallvec![ToolOutput::SetCurrentPaint(paint)];
        }
        let paint_cmds: SmallVec<[EditorCommand; 4]> = targets
            .into_iter()
            .map(|id| EditorCommand::SetPaint {
                id,
                paint: paint.clone(),
            })
            .collect();
        smallvec![
            ToolOutput::SetCurrentPaint(paint),
            ToolOutput::BeginTransaction("Apply paint".into()),
            ToolOutput::Commands(paint_cmds),
            ToolOutput::CommitTransaction,
        ]
    }
}

/// True when `pos` is inside (or within stroke width of) an evaluated item.
fn paint_covers(item: &renamite_model::SceneItem, pos: DVec2) -> bool {
    let q = Point::new(pos.x, pos.y);
    let padding = match &item.kind {
        renamite_model::PaintKind::Stroke(s) => (s.width * 0.5).max(1.0),
        renamite_model::PaintKind::Fill(_) => 0.0,
    };
    if !item
        .path
        .bounding_box()
        .inflate(padding, padding)
        .contains(q)
    {
        return false;
    }
    match &item.kind {
        renamite_model::PaintKind::Fill(rule) => match rule {
            FillRule::NonZero => item.path.winding(q) != 0,
            FillRule::EvenOdd => item.path.winding(q) % 2 != 0,
        },
        renamite_model::PaintKind::Stroke(_) => {
            let mut best = f64::MAX;
            for seg in item.path.segments() {
                best = best.min(seg.nearest(q, 1e-6).distance_sq);
            }
            best.sqrt() <= padding
        }
    }
}

/// Immediate applicable Fill (or Stroke with `want_stroke`) style nodes for a
/// selection, mirroring `fill_style_for` scope resolution. Shared style nodes
/// are deduplicated so one node is never written twice.
fn style_targets(doc: &Document, selection: &[NodeId], want_stroke: bool) -> Vec<NodeId> {
    fn nearest_style(doc: &Document, shape: NodeId, want_stroke: bool) -> Option<NodeId> {
        let mut scope = doc.locate(shape).map(|(p, _)| p)?;
        loop {
            let children: Vec<NodeId> = match scope {
                Parent::Comp(c) => doc.compositions.get(c)?.children.clone(),
                Parent::Node(p) => doc.nodes.get(p)?.children.clone(),
            };
            let wanted: fn(&NodeKind) -> bool = if want_stroke {
                |k| matches!(k, NodeKind::Style(StyleKind::Stroke { .. }))
            } else {
                |k| matches!(k, NodeKind::Style(StyleKind::Fill { .. }))
            };
            if let Some(found) = children
                .iter()
                .rev()
                .copied()
                .find(|id| doc.nodes.get(*id).is_some_and(|n| wanted(&n.kind)))
            {
                return Some(found);
            }
            match scope {
                Parent::Comp(_) => return None,
                Parent::Node(p) => scope = doc.locate(p).map(|(parent, _)| parent)?,
            }
        }
    }

    let mut out: Vec<NodeId> = Vec::new();
    for id in selection
        .iter()
        .copied()
        .filter(|id| doc.nodes.contains_key(*id))
    {
        // Prefer the selected node itself when it IS a style node.
        let is_wanted_style = doc.nodes.get(id).is_some_and(|n| {
            if want_stroke {
                matches!(n.kind, NodeKind::Style(StyleKind::Stroke { .. }))
            } else {
                matches!(n.kind, NodeKind::Style(StyleKind::Fill { .. }))
            }
        });
        let target = if is_wanted_style {
            Some(id)
        } else {
            nearest_style(doc, id, want_stroke)
        };
        if let Some(t) = target
            && !out.contains(&t)
        {
            out.push(t);
        }
    }
    out
}

/// Click-to-place text. Creates a Text node + sibling Fill in a group, with
/// the click point as the text baseline via the node's own transform.
#[derive(Default)]
pub struct TextTool;
impl TextTool {
    pub fn overlay(&self, _ctx: &ToolContext) -> ToolOverlay {
        ToolOverlay::None
    }

    pub fn handle(&mut self, ctx: &ToolContext, ev: CanvasEvent) -> OutputVec {
        let CanvasEvent::PointerDown {
            pos,
            button: PointerButton::Primary,
        } = ev
        else {
            return smallvec![];
        };
        let mut text_node = Node::new(
            "Text",
            NodeKind::Text(renamite_model::TextNode {
                text: "Text".into(),
                size: Animated::new(48.0),
                align: renamite_model::TextAlign::Left,
                font: None,
            }),
        );
        // Place the baseline at the click point via the node's own transform.
        text_node.transform.position = Animated::new(pos);
        let tree = NodeTree::with_children(
            Node::new("Text", NodeKind::Group),
            vec![
                NodeTree::leaf(text_node),
                NodeTree::leaf(Node::new(
                    "Fill",
                    NodeKind::Style(StyleKind::Fill {
                        paint: ctx.current_paint.snapshot(ctx.playhead.0 as f64),
                        rule: FillRule::NonZero,
                    }),
                )),
            ],
        );
        smallvec![
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
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ShapeToolKind {
    Rect,
    Ellipse,
    Star,
    Polygon,
}

pub struct ShapeTool {
    kind: ShapeToolKind,
    drag: Option<(DVec2, DVec2)>, // (start, current)
}

impl ShapeTool {
    pub fn new(kind: ShapeToolKind) -> Self {
        Self { kind, drag: None }
    }

    pub fn overlay(&self, ctx: &ToolContext) -> ToolOverlay {
        match self.drag {
            Some((s, c)) => {
                let (min, max) = constrained_rect(s, c, ctx.modifiers.shift, ctx.modifiers.alt);
                ToolOverlay::ShapePreview {
                    min,
                    max,
                    kind: self.preview_kind(ctx),
                }
            }
            None => ToolOverlay::None,
        }
    }

    fn preview_kind(&self, ctx: &ToolContext) -> ShapePreviewKind {
        match self.kind {
            ShapeToolKind::Rect => ShapePreviewKind::Rect,
            ShapeToolKind::Ellipse => ShapePreviewKind::Ellipse,
            ShapeToolKind::Star if ctx.modifiers.shift => ShapePreviewKind::Polygon,
            ShapeToolKind::Star => ShapePreviewKind::Star,
            ShapeToolKind::Polygon => ShapePreviewKind::Polygon,
        }
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub fn handle(&mut self, ctx: &ToolContext, ev: CanvasEvent) -> OutputVec {
        match ev {
            CanvasEvent::PointerDown {
                pos,
                button: PointerButton::Primary,
            } => {
                self.drag = Some((pos, pos));
                smallvec![ToolOutput::Invalidate]
            }
            CanvasEvent::PointerMove { pos } => {
                if let Some((_, c)) = &mut self.drag {
                    *c = pos;
                    smallvec![ToolOutput::Invalidate]
                } else {
                    smallvec![]
                }
            }
            CanvasEvent::PointerUp {
                pos,
                button: PointerButton::Primary,
            } => {
                let Some((start, _)) = self.drag.take() else {
                    return smallvec![];
                };
                let (min, max) =
                    constrained_rect(start, pos, ctx.modifiers.shift, ctx.modifiers.alt);
                let size = max - min;
                if size.x < 1.0 || size.y < 1.0 {
                    return smallvec![];
                }
                let center = (min + max) * 0.5;
                let outer = size.x.min(size.y).abs() * 0.5;
                let (name, shape) = match self.kind {
                    ShapeToolKind::Rect => (
                        "Rectangle",
                        ShapeKind::Rect {
                            pos: Animated::new(center),
                            size: Animated::new(size),
                            rounded: Animated::new(0.0),
                        },
                    ),
                    ShapeToolKind::Ellipse => (
                        "Ellipse",
                        ShapeKind::Ellipse {
                            pos: Animated::new(center),
                            size: Animated::new(size),
                        },
                    ),
                    // Shift turns the star tool into a regular polygon.
                    ShapeToolKind::Star if ctx.modifiers.shift => (
                        "Polygon",
                        ShapeKind::Polygon {
                            pos: Animated::new(center),
                            points: Animated::new(6.0),
                            outer_r: Animated::new(outer),
                            roundness: Animated::new(0.0),
                        },
                    ),
                    // Alt = 6-point star instead of 5.
                    ShapeToolKind::Star => (
                        "Star",
                        ShapeKind::Star {
                            pos: Animated::new(center),
                            points: Animated::new(if ctx.modifiers.alt { 6.0 } else { 5.0 }),
                            inner_r: Animated::new(outer * 0.4),
                            outer_r: Animated::new(outer),
                            roundness: Animated::new(0.0),
                            kind: StarKind::Star,
                        },
                    ),
                    ShapeToolKind::Polygon => (
                        "Polygon",
                        ShapeKind::Polygon {
                            pos: Animated::new(center),
                            points: Animated::new(6.0),
                            outer_r: Animated::new(outer),
                            roundness: Animated::new(0.0),
                        },
                    ),
                };
                let tree = NodeTree::with_children(
                    Node::new(name, NodeKind::Group),
                    vec![
                        NodeTree::leaf(Node::new("Shape", NodeKind::Shape(shape))),
                        NodeTree::leaf(Node::new(
                            "Fill",
                            NodeKind::Style(StyleKind::Fill {
                                paint: ctx.current_paint.snapshot(ctx.playhead.0 as f64),
                                rule: FillRule::NonZero,
                            }),
                        )),
                    ],
                );
                smallvec![
                    ToolOutput::BeginTransaction(format!("Create {name}")),
                    ToolOutput::Commands(smallvec![EditorCommand::InsertNode {
                        parent: Parent::Comp(ctx.comp),
                        index: 0,
                        tree,
                    }]),
                    ToolOutput::CommitTransaction,
                    ToolOutput::SwitchTool(ToolId::Select),
                ]
            }
            CanvasEvent::KeyDown(Key::Escape) => {
                self.drag = None;
                smallvec![ToolOutput::Invalidate]
            }
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
    let (a, b) = if alt {
        (start - d, start + d)
    } else {
        (start, start + d)
    };
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
        Self {
            state: PenState::Idle,
        }
    }
}

impl PenTool {
    pub fn is_dragging(&self) -> bool {
        matches!(self.state, PenState::DraggingTangent { .. })
    }

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
            CanvasEvent::PointerDown {
                pos,
                button: PointerButton::Primary,
            } => self.press(ctx, pos),
            CanvasEvent::PointerMove { pos } => self.moved(pos),
            CanvasEvent::PointerUp {
                pos,
                button: PointerButton::Primary,
            } => self.release(pos),
            CanvasEvent::KeyDown(Key::Enter) => self.finish(ctx, false),
            CanvasEvent::KeyDown(Key::Escape) => {
                self.state = PenState::Idle;
                smallvec![ToolOutput::Invalidate]
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
                smallvec![ToolOutput::Invalidate]
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
                self.state = PenState::DraggingTangent {
                    anchors,
                    index: idx,
                };
                smallvec![ToolOutput::Invalidate]
            }
            PenState::DraggingTangent { .. } => smallvec![ToolOutput::Invalidate],
        }
    }

    fn moved(&mut self, pos: DVec2) -> OutputVec {
        match &mut self.state {
            PenState::Idle => smallvec![],
            PenState::Building { hover, .. } => {
                *hover = pos;
                smallvec![ToolOutput::Invalidate]
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
                smallvec![ToolOutput::Invalidate]
            }
        }
    }

    fn release(&mut self, pos: DVec2) -> OutputVec {
        if let PenState::DraggingTangent { anchors, .. } = &self.state {
            let anchors = anchors.clone();
            self.state = PenState::Building {
                anchors,
                hover: pos,
            };
        }
        smallvec![ToolOutput::Invalidate]
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
        smallvec![ToolOutput::Invalidate]
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
            NodeKind::Shape(ShapeKind::Path(Animated::new(VectorPath {
                anchors,
                closed,
            }))),
        );
        let fill = Node::new(
            "Fill",
            NodeKind::Style(StyleKind::Fill {
                paint: ctx.current_paint.snapshot(ctx.playhead.0 as f64),
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
    DragAnchor {
        node: NodeId,
        index: usize,
        edit_frame: Option<Frame>,
        txn: bool,
    },
    DragTanIn {
        node: NodeId,
        index: usize,
        edit_frame: Option<Frame>,
        txn: bool,
    },
    DragTanOut {
        node: NodeId,
        index: usize,
        edit_frame: Option<Frame>,
        txn: bool,
    },
}

/// A selected anchor reference for multi-anchor operations (join). `contour`
/// indexes into a shape's contour list (`Path` = one contour; `CompoundPath` =
/// several).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnchorRef {
    pub node: NodeId,
    pub contour: usize,
    pub anchor: usize,
}

/// Catmull-Rom-style tangent synthesis (Inkscape auto-smooth): tangents run
/// along `(next - prev)` scaled to a third of each adjacent edge. Endpoints of
/// open contours get a single-sided third-length tangent.
fn auto_smooth(path: &mut VectorPath, index: usize) {
    let len = path.anchors.len();
    if len < 2 || index >= len {
        return;
    }

    let prev = if index > 0 {
        Some(path.anchors[index - 1].pos)
    } else if path.closed {
        Some(path.anchors[len - 1].pos)
    } else {
        None
    };

    let next = if index + 1 < len {
        Some(path.anchors[index + 1].pos)
    } else if path.closed {
        Some(path.anchors[0].pos)
    } else {
        None
    };

    let anchor = &mut path.anchors[index];

    match (prev, next) {
        (Some(prev), Some(next)) => {
            let direction = (next - prev).normalize_or_zero();
            anchor.tan_in = -direction * (anchor.pos - prev).length() / 3.0;
            anchor.tan_out = direction * (next - anchor.pos).length() / 3.0;
            anchor.mode = TangentMode::Smooth;
        }
        (Some(prev), None) => {
            anchor.tan_in = (prev - anchor.pos) / 3.0;
            anchor.tan_out = DVec2::ZERO;
            anchor.mode = TangentMode::Corner;
        }
        (None, Some(next)) => {
            anchor.tan_in = DVec2::ZERO;
            anchor.tan_out = (next - anchor.pos) / 3.0;
            anchor.mode = TangentMode::Corner;
        }
        _ => {}
    }
}

pub struct PathEditTool {
    state: PathEditState,
    pub selected_anchor: Option<usize>,
    /// Anchor under the selection, compound-aware (set on every anchor hit).
    pub selected_ref: Option<AnchorRef>,
    /// Endpoint refs gathered with Shift+click, consumed by Join (Shift+J).
    pub selected_endpoints: Vec<AnchorRef>,
}

impl Default for PathEditTool {
    fn default() -> Self {
        Self {
            state: PathEditState::Idle,
            selected_anchor: None,
            selected_ref: None,
            selected_endpoints: Vec::new(),
        }
    }
}

impl PathEditTool {
    /// Accept either a selected path node, or a selected group with exactly one
    /// direct Path child (so the group Pen creates works right after switch).
    fn editable_path_node(ctx: &ToolContext) -> Option<NodeId> {
        let &[sel] = ctx.selection.nodes.as_slice() else {
            return None;
        };
        let node = ctx.doc.nodes.get(sel)?;

        match &node.kind {
            NodeKind::Shape(ShapeKind::Path(_) | ShapeKind::CompoundPath(_)) => Some(sel),
            NodeKind::Group | NodeKind::Layer(_) => {
                let mut path_children = node.children.iter().copied().filter(|id| {
                    matches!(
                        ctx.doc.nodes.get(*id).map(|n| &n.kind),
                        Some(
                            NodeKind::Shape(ShapeKind::Path(_))
                                | NodeKind::Shape(ShapeKind::CompoundPath(_))
                        )
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
        let (_, contours) = Self::current_contours(ctx)?;
        contours.into_iter().next()
    }

    /// `(node id, contours)` for the edited shape. `ShapeKind::Path` is a
    /// one-contour list; `CompoundPath` exposes every contour at the frame.
    fn current_contours(ctx: &ToolContext) -> Option<(NodeId, Vec<VectorPath>)> {
        let id = Self::editable_path_node(ctx)?;
        let node = ctx.doc.nodes.get(id)?;
        let contours = match &node.kind {
            NodeKind::Shape(ShapeKind::Path(a)) => vec![a.value_at(ctx.playhead.0 as f64)],
            NodeKind::Shape(ShapeKind::CompoundPath(c)) => c
                .contours
                .iter()
                .map(|p| p.value_at(ctx.playhead.0 as f64))
                .collect(),
            _ => return None,
        };
        Some((id, contours))
    }

    pub fn is_dragging(&self) -> bool {
        !matches!(self.state, PathEditState::Idle)
    }

    pub fn overlay(&self, ctx: &ToolContext) -> ToolOverlay {
        match Self::current_contours(ctx) {
            Some((_, contours)) if !contours.is_empty() => {
                let mut iter = contours.into_iter();
                let primary = iter.next().unwrap_or_default();
                ToolOverlay::PathHandles {
                    path: primary,
                    extra: iter.collect(),
                    active_anchor: self.selected_anchor,
                }
            }
            _ => ToolOverlay::None,
        }
    }

    pub fn handle(&mut self, ctx: &ToolContext, ev: CanvasEvent) -> OutputVec {
        match ev {
            CanvasEvent::PointerDown {
                pos,
                button: PointerButton::Primary,
            } => self.press(ctx, pos),
            CanvasEvent::PointerMove { pos } => self.moved(ctx, pos),
            CanvasEvent::PointerUp { .. } => self.release(),
            CanvasEvent::KeyDown(Key::Escape) => self.escape(),
            CanvasEvent::KeyDown(Key::Delete) | CanvasEvent::KeyDown(Key::Backspace) => {
                self.delete_anchor(ctx)
            }
            CanvasEvent::KeyDown(Key::Insert) => self.insert_at_selected_midpoint(ctx),
            CanvasEvent::KeyDown(Key::Tab) => self.cycle_selected_anchor(ctx, ctx.modifiers.shift),
            CanvasEvent::KeyDown(Key::ArrowLeft) => {
                self.nudge_selected_anchor(ctx, DVec2::new(-1.0, 0.0))
            }
            CanvasEvent::KeyDown(Key::ArrowRight) => {
                self.nudge_selected_anchor(ctx, DVec2::new(1.0, 0.0))
            }
            CanvasEvent::KeyDown(Key::ArrowUp) => {
                self.nudge_selected_anchor(ctx, DVec2::new(0.0, -1.0))
            }
            CanvasEvent::KeyDown(Key::ArrowDown) => {
                self.nudge_selected_anchor(ctx, DVec2::new(0.0, 1.0))
            }
            CanvasEvent::KeyDown(Key::NodeCorner) => {
                self.set_selected_mode(ctx, TangentMode::Corner)
            }
            CanvasEvent::KeyDown(Key::NodeSmooth) => {
                self.set_selected_mode(ctx, TangentMode::Smooth)
            }
            CanvasEvent::KeyDown(Key::NodeSymmetric) => {
                self.set_selected_mode(ctx, TangentMode::Symmetric)
            }
            CanvasEvent::KeyDown(Key::SegmentLine) => self.selected_segment_to_line(ctx),
            CanvasEvent::KeyDown(Key::SegmentCurve) => self.selected_segment_to_curve(ctx),
            CanvasEvent::KeyDown(Key::NodeAutoSmooth) => self.auto_smooth_selected(ctx),
            CanvasEvent::KeyDown(Key::NodeBreak) => self.break_at_selected_node(ctx),
            CanvasEvent::KeyDown(Key::NodeJoin) => self.join_selected_endpoints(ctx),
            CanvasEvent::DoubleClick { pos } => self.insert_anchor(ctx, pos),
            _ => smallvec![],
        }
    }

    /// (id, seed) for the current edit; `seed` is an AddKeyframe when one is needed.
    fn edit_target(&self, ctx: &ToolContext, id: NodeId) -> (Option<Frame>, Option<EditorCommand>) {
        path_edit_target(ctx.doc, id, ctx.playhead, ctx.record).unwrap_or((None, None))
    }

    fn begin_drag(&mut self, state: PathEditState, seed: Option<EditorCommand>) -> OutputVec {
        let mut out: OutputVec = smallvec![];
        if let Some(seed) = seed {
            out.push(ToolOutput::BeginTransaction("Edit path".into()));
            out.push(ToolOutput::Commands(smallvec![seed]));
        }
        self.state = state;
        out
    }

    fn press(&mut self, ctx: &ToolContext, pos: DVec2) -> OutputVec {
        let Some(id) = Self::editable_path_node(ctx) else {
            return smallvec![];
        };
        let Some((_, contours)) = Self::current_contours(ctx) else {
            return smallvec![];
        };

        let tol_anchor = ctx.view.world_tolerance(ANCHOR_HIT_PX);
        let tol_tangent = ctx.view.world_tolerance(TANGENT_HIT_PX);
        let plain_single = {
            matches!(
                ctx.doc.nodes.get(id).map(|n| &n.kind),
                Some(NodeKind::Shape(ShapeKind::Path(_)))
            )
        };

        // Anchor hit (compound-aware).
        for (ci, path) in contours.iter().enumerate() {
            for (i, a) in path.anchors.iter().enumerate() {
                if (pos - a.pos).length() > tol_anchor {
                    continue;
                }

                let is_endpoint = !path.closed && (i == 0 || i + 1 == path.anchors.len());
                if ctx.modifiers.shift && is_endpoint {
                    // Shift+click gathers endpoint references for Join.
                    let r = AnchorRef {
                        node: id,
                        contour: ci,
                        anchor: i,
                    };
                    if !self.selected_endpoints.contains(&r) {
                        self.selected_endpoints.push(r);
                        if self.selected_endpoints.len() > 2 {
                            self.selected_endpoints.remove(0);
                        }
                    }
                    self.selected_ref = Some(r);
                    if ci == 0 {
                        self.selected_anchor = Some(i);
                    }
                    return smallvec![ToolOutput::Invalidate];
                }

                self.selected_ref = Some(AnchorRef {
                    node: id,
                    contour: ci,
                    anchor: i,
                });
                self.selected_anchor = (ci == 0).then_some(i);

                if ctx.modifiers.alt {
                    let new_mode = a.mode.cycled();
                    let (edit_frame, seed) = self.edit_target(ctx, id);
                    let mut cmds: OutputVec = smallvec![];
                    if let Some(seed) = seed {
                        cmds.push(ToolOutput::Commands(smallvec![seed]));
                    }
                    cmds.push(ToolOutput::Commands(smallvec![
                        EditorCommand::EditAnchors {
                            id,
                            frame: edit_frame,
                            edits: vec![AnchorEdit::SetMode {
                                index: i,
                                mode: new_mode
                            }],
                        }
                    ]));
                    let mut out =
                        smallvec![ToolOutput::BeginTransaction("Cycle tangent mode".into())];
                    out.extend(cmds);
                    out.push(ToolOutput::CommitTransaction);
                    return out;
                }

                // Legacy drag machinery edits `shape.path` directly: only the
                // single contour of a plain Path node qualifies.
                if !(plain_single && ci == 0) {
                    return smallvec![ToolOutput::Invalidate];
                }

                let (edit_frame, seed) = self.edit_target(ctx, id);
                return self.begin_drag(
                    PathEditState::DragAnchor {
                        node: id,
                        index: i,
                        edit_frame,
                        txn: seed.is_some(),
                    },
                    seed,
                );
            }
        }

        // Tangent handle hit.
        for (ci, path) in contours.iter().enumerate() {
            if ci != 0 || !plain_single {
                break; // tangent drags stay on plain single-contour paths
            }
            for (i, a) in path.anchors.iter().enumerate() {
                let in_tip = a.pos + a.tan_in;
                let out_tip = a.pos + a.tan_out;

                if a.tan_in.length_squared() > 1e-12 && (pos - in_tip).length() <= tol_tangent {
                    self.selected_anchor = Some(i);
                    self.selected_ref = Some(AnchorRef {
                        node: id,
                        contour: ci,
                        anchor: i,
                    });
                    let (edit_frame, seed) = self.edit_target(ctx, id);
                    return self.begin_drag(
                        PathEditState::DragTanIn {
                            node: id,
                            index: i,
                            edit_frame,
                            txn: seed.is_some(),
                        },
                        seed,
                    );
                }

                if a.tan_out.length_squared() > 1e-12 && (pos - out_tip).length() <= tol_tangent {
                    self.selected_anchor = Some(i);
                    self.selected_ref = Some(AnchorRef {
                        node: id,
                        contour: ci,
                        anchor: i,
                    });
                    let (edit_frame, seed) = self.edit_target(ctx, id);
                    return self.begin_drag(
                        PathEditState::DragTanOut {
                            node: id,
                            index: i,
                            edit_frame,
                            txn: seed.is_some(),
                        },
                        seed,
                    );
                }
            }
        }

        self.selected_anchor = None;
        self.selected_ref = None;
        smallvec![]
    }

    fn moved(&mut self, ctx: &ToolContext, pos: DVec2) -> OutputVec {
        match &mut self.state {
            PathEditState::Idle => smallvec![],

            PathEditState::DragAnchor {
                node,
                index,
                edit_frame,
                txn,
            } => {
                let mut out: OutputVec = smallvec![];
                if !*txn {
                    out.push(ToolOutput::BeginTransaction("Edit path".into()));
                    *txn = true;
                }
                out.push(ToolOutput::Commands(smallvec![
                    EditorCommand::EditAnchors {
                        id: *node,
                        frame: *edit_frame,
                        edits: vec![AnchorEdit::SetPos { index: *index, pos }],
                    }
                ]));
                out
            }

            PathEditState::DragTanIn {
                node,
                index,
                edit_frame,
                txn,
            } => {
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
                out.push(ToolOutput::Commands(smallvec![
                    EditorCommand::EditAnchors {
                        id: *node,
                        frame: *edit_frame,
                        edits: vec![AnchorEdit::SetTanIn { index: *index, tan }],
                    }
                ]));
                out
            }

            PathEditState::DragTanOut {
                node,
                index,
                edit_frame,
                txn,
            } => {
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
                out.push(ToolOutput::Commands(smallvec![
                    EditorCommand::EditAnchors {
                        id: *node,
                        frame: *edit_frame,
                        edits: vec![AnchorEdit::SetTanOut { index: *index, tan }],
                    }
                ]));
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
        if txn {
            smallvec![ToolOutput::CommitTransaction]
        } else {
            smallvec![]
        }
    }

    fn escape(&mut self) -> OutputVec {
        let txn = match &self.state {
            PathEditState::Idle => false,
            PathEditState::DragAnchor { txn, .. }
            | PathEditState::DragTanIn { txn, .. }
            | PathEditState::DragTanOut { txn, .. } => *txn,
        };
        self.state = PathEditState::Idle;
        if txn {
            smallvec![ToolOutput::CancelTransaction]
        } else {
            smallvec![]
        }
    }

    fn delete_anchor(&mut self, ctx: &ToolContext) -> OutputVec {
        let Some(index) = self.selected_anchor else {
            return smallvec![];
        };
        let Some(id) = Self::editable_path_node(ctx) else {
            return smallvec![];
        };
        let Some(path) = Self::current_path(ctx) else {
            return smallvec![];
        };
        if path.anchors.len() <= 2 {
            return smallvec![];
        }

        let (edit_frame, seed) = self.edit_target(ctx, id);

        let mut cmds: OutputVec = smallvec![];
        if let Some(seed) = seed {
            cmds.push(ToolOutput::Commands(smallvec![seed]));
        }
        cmds.push(ToolOutput::Commands(smallvec![
            EditorCommand::EditAnchors {
                id,
                frame: edit_frame,
                edits: vec![AnchorEdit::Delete { index }],
            }
        ]));

        self.selected_anchor = None;
        let mut out = smallvec![ToolOutput::BeginTransaction("Delete anchor".into())];
        out.extend(cmds);
        out.push(ToolOutput::CommitTransaction);
        out
    }

    fn insert_anchor(&mut self, ctx: &ToolContext, pos: DVec2) -> OutputVec {
        let Some(id) = Self::editable_path_node(ctx) else {
            return smallvec![];
        };
        let Some(path) = Self::current_path(ctx) else {
            return smallvec![];
        };
        let Some((seg, t, dist)) = path.nearest_segment(pos) else {
            return smallvec![];
        };

        if dist > ctx.view.world_tolerance(20.0) {
            return smallvec![];
        }

        let mut new_path = path.clone();
        let _ = new_path.insert_anchor_at(seg, t);

        self.selected_anchor = Some(seg + 1);
        self.commit_path_value(ctx, id, new_path, "Insert anchor")
    }

    fn segment_adjacent_to(path: &VectorPath, index: usize) -> Option<usize> {
        if path.anchors.len() < 2 {
            return None;
        }
        if path.closed {
            return Some(index % path.anchors.len());
        }
        if index + 1 < path.anchors.len() {
            Some(index)
        } else {
            Some(index - 1)
        }
    }

    fn commit_path_value(
        &mut self,
        ctx: &ToolContext,
        id: NodeId,
        new_path: VectorPath,
        label: &str,
    ) -> OutputVec {
        let (edit_frame, seed) = self.edit_target(ctx, id);

        let mut out: OutputVec = smallvec![ToolOutput::BeginTransaction(label.into())];
        if let Some(seed) = seed {
            out.push(ToolOutput::Commands(smallvec![seed]));
        }

        let value = Value::Path(new_path);
        let prop = PropPath::new("shape.path");
        out.push(ToolOutput::Commands(smallvec![match edit_frame {
            Some(frame) => EditorCommand::AddKeyframe {
                id,
                prop,
                frame,
                value
            },
            None => EditorCommand::SetStatic { id, prop, value },
        }]));

        out.push(ToolOutput::CommitTransaction);
        out
    }

    fn insert_at_selected_midpoint(&mut self, ctx: &ToolContext) -> OutputVec {
        let Some(index) = self.selected_anchor else {
            return smallvec![];
        };
        let Some(id) = Self::editable_path_node(ctx) else {
            return smallvec![];
        };
        let Some(path) = Self::current_path(ctx) else {
            return smallvec![];
        };
        let Some(seg) = Self::segment_adjacent_to(&path, index) else {
            return smallvec![];
        };

        let mut new_path = path.clone();
        if new_path.insert_anchor_at(seg, 0.5).is_err() {
            return smallvec![];
        }

        self.selected_anchor = Some(seg + 1);
        self.commit_path_value(ctx, id, new_path, "Insert node")
    }

    /// Tab / Shift+Tab: walk the anchor list, wrapping around.
    fn cycle_selected_anchor(&mut self, ctx: &ToolContext, back: bool) -> OutputVec {
        let Some(path) = Self::current_path(ctx) else {
            return smallvec![];
        };
        let n = path.anchors.len();
        if n == 0 {
            return smallvec![];
        }
        self.selected_anchor = Some(match self.selected_anchor {
            None => {
                if back {
                    n - 1
                } else {
                    0
                }
            }
            Some(i) => {
                if back {
                    (i + n - 1) % n
                } else {
                    (i + 1) % n
                }
            }
        });
        smallvec![ToolOutput::Invalidate]
    }

    /// Arrows: move the selected anchor. Alt = 1 screen px, Shift = 20px,
    /// default = 2px (world units).
    fn nudge_selected_anchor(&mut self, ctx: &ToolContext, dir: DVec2) -> OutputVec {
        let Some(index) = self.selected_anchor else {
            return smallvec![];
        };
        let Some(id) = Self::editable_path_node(ctx) else {
            return smallvec![];
        };
        let Some(pos) = Self::current_path(ctx).and_then(|p| p.anchors.get(index).map(|a| a.pos))
        else {
            return smallvec![];
        };

        let amount = if ctx.modifiers.alt {
            1.0 / ctx.view.scale
        } else if ctx.modifiers.shift {
            20.0
        } else {
            2.0
        };

        let (edit_frame, seed) = self.edit_target(ctx, id);
        let mut out: OutputVec = smallvec![ToolOutput::BeginTransaction("Nudge node".into())];
        if let Some(seed) = seed {
            out.push(ToolOutput::Commands(smallvec![seed]));
        }
        out.push(ToolOutput::Commands(smallvec![
            EditorCommand::EditAnchors {
                id,
                frame: edit_frame,
                edits: vec![AnchorEdit::SetPos {
                    index,
                    pos: pos + dir * amount,
                }],
            }
        ]));
        out.push(ToolOutput::CommitTransaction);
        out
    }

    /// Shift+C / Shift+S / Shift+Y (Shift+A aliases Smooth): set the selected
    /// anchor's tangent mode.
    fn set_selected_mode(&mut self, ctx: &ToolContext, mode: TangentMode) -> OutputVec {
        let Some(index) = self.selected_anchor else {
            return smallvec![];
        };
        let Some(id) = Self::editable_path_node(ctx) else {
            return smallvec![];
        };

        let (edit_frame, seed) = self.edit_target(ctx, id);
        let mut out: OutputVec =
            smallvec![ToolOutput::BeginTransaction("Change tangent mode".into())];
        if let Some(seed) = seed {
            out.push(ToolOutput::Commands(smallvec![seed]));
        }
        out.push(ToolOutput::Commands(smallvec![
            EditorCommand::EditAnchors {
                id,
                frame: edit_frame,
                edits: vec![AnchorEdit::SetMode { index, mode }],
            }
        ]));
        out.push(ToolOutput::CommitTransaction);
        out
    }

    fn convert_selected_segment(
        &mut self,
        ctx: &ToolContext,
        label: &str,
        build: impl FnOnce(&VectorPath, usize, usize) -> Vec<AnchorEdit>,
    ) -> OutputVec {
        let Some(index) = self.selected_anchor else {
            return smallvec![];
        };
        let Some(id) = Self::editable_path_node(ctx) else {
            return smallvec![];
        };
        let Some(path) = Self::current_path(ctx) else {
            return smallvec![];
        };
        let Some(seg) = Self::segment_adjacent_to(&path, index) else {
            return smallvec![];
        };
        let next = (seg + 1) % path.anchors.len();

        let (edit_frame, seed) = self.edit_target(ctx, id);
        let mut out: OutputVec = smallvec![ToolOutput::BeginTransaction(label.into())];
        if let Some(seed) = seed {
            out.push(ToolOutput::Commands(smallvec![seed]));
        }
        out.push(ToolOutput::Commands(smallvec![
            EditorCommand::EditAnchors {
                id,
                frame: edit_frame,
                edits: build(&path, seg, next),
            }
        ]));
        out.push(ToolOutput::CommitTransaction);
        out
    }

    /// Shift+L: straighten the segment adjacent to the selection.
    fn selected_segment_to_line(&mut self, ctx: &ToolContext) -> OutputVec {
        self.convert_selected_segment(ctx, "Segment to line", |_path, a, b| {
            vec![
                AnchorEdit::SetTanOut {
                    index: a,
                    tan: DVec2::ZERO,
                },
                AnchorEdit::SetTanIn {
                    index: b,
                    tan: DVec2::ZERO,
                },
            ]
        })
    }

    /// Shift+U: give the adjacent segment default tangents (thirds rule).
    fn selected_segment_to_curve(&mut self, ctx: &ToolContext) -> OutputVec {
        self.convert_selected_segment(ctx, "Segment to curve", |path, a, b| {
            let d = path.anchors[b].pos - path.anchors[a].pos;
            vec![
                AnchorEdit::SetTanOut {
                    index: a,
                    tan: d / 3.0,
                },
                AnchorEdit::SetTanIn {
                    index: b,
                    tan: -d / 3.0,
                },
            ]
        })
    }

    fn write_contours(
        &mut self,
        _ctx: &ToolContext,
        id: NodeId,
        contours: Vec<VectorPath>,
        label: &str,
    ) -> OutputVec {
        let kind = if contours.len() == 1 {
            ShapeKind::Path(Animated::new(
                contours.into_iter().next().unwrap_or_default(),
            ))
        } else {
            ShapeKind::CompoundPath(renamite_model::CompoundPath {
                contours: contours.into_iter().map(Animated::new).collect(),
            })
        };
        smallvec![
            ToolOutput::BeginTransaction(label.into()),
            ToolOutput::Commands(smallvec![EditorCommand::SetNodeKind {
                id,
                kind: NodeKind::Shape(kind),
            }]),
            ToolOutput::CommitTransaction,
        ]
    }

    /// Shift+A: synthesize auto-smooth tangents on the selected anchor.
    fn auto_smooth_selected(&mut self, ctx: &ToolContext) -> OutputVec {
        let Some(reference) = self.selected_ref else {
            return smallvec![];
        };
        let Some((id, mut contours)) = Self::current_contours(ctx) else {
            return smallvec![];
        };
        let Some(path) = contours.get_mut(reference.contour) else {
            return smallvec![];
        };
        auto_smooth(path, reference.anchor);

        let is_plain = matches!(
            ctx.doc.nodes.get(id).map(|n| &n.kind),
            Some(NodeKind::Shape(ShapeKind::Path(_)))
        );
        if is_plain && reference.contour == 0 {
            self.selected_anchor = Some(reference.anchor);
            let value = contours.into_iter().next().unwrap_or_default();
            return self.commit_path_value(ctx, id, value, "Auto smooth");
        }
        self.write_contours(ctx, id, contours, "Auto smooth")
    }

    /// Shift+B: break the contour open at the selected anchor.
    ///
    /// Closed contour: duplicate the anchor (one copy first with `tan_in`
    /// cleared, one last with `tan_out` cleared) and open it. Open interior
    /// anchor: split into two contours stored in a compound path.
    fn break_at_selected_node(&mut self, ctx: &ToolContext) -> OutputVec {
        let Some(reference) = self.selected_ref else {
            return smallvec![];
        };
        let Some((id, mut contours)) = Self::current_contours(ctx) else {
            return smallvec![];
        };
        let Some(source) = contours.get(reference.contour).cloned() else {
            return smallvec![];
        };

        let broken: Vec<VectorPath> = if source.closed {
            let n = source.anchors.len();
            if n < 2 {
                return smallvec![];
            }
            // Rotate so the selected anchor is first, then append its
            // duplicate; clear start `tan_in` / end `tan_out`.
            let rotated: Vec<Anchor> = (0..n)
                .map(|k| source.anchors[(reference.anchor + k) % n])
                .collect();
            let mut start = rotated[0];
            start.tan_in = DVec2::ZERO;
            let mut end = start;
            end.tan_out = DVec2::ZERO;
            let mut anchors = Vec::with_capacity(n + 1);
            anchors.push(start);
            anchors.extend(rotated.into_iter().skip(1));
            anchors.push(end);
            vec![VectorPath {
                anchors,
                closed: false,
            }]
        } else {
            let i = reference.anchor;
            if !(i > 0 && i + 1 < source.anchors.len()) {
                return smallvec![]; // endpoint: already broken
            }
            let mut first = VectorPath {
                anchors: source.anchors[..=i].to_vec(),
                closed: false,
            };
            if let Some(last) = first.anchors.last_mut() {
                last.tan_out = DVec2::ZERO;
            }
            let mut second = VectorPath {
                anchors: source.anchors[i..].to_vec(),
                closed: false,
            };
            second.anchors[0].tan_in = DVec2::ZERO;
            vec![first, second]
        };

        let label = if source.closed {
            "Break at node"
        } else {
            "Split node"
        };
        let was_plain = matches!(
            ctx.doc.nodes.get(id).map(|n| &n.kind),
            Some(NodeKind::Shape(ShapeKind::Path(_)))
        );
        contours.splice(reference.contour..=reference.contour, broken);
        if was_plain && contours.len() == 1 {
            self.selected_anchor = Some(0);
            self.selected_ref = Some(AnchorRef {
                node: id,
                contour: 0,
                anchor: 0,
            });
            let value = contours.into_iter().next().unwrap_or_default();
            return self.commit_path_value(ctx, id, value, label);
        }
        self.selected_anchor = None;
        self.selected_ref = Some(AnchorRef {
            node: id,
            contour: reference.contour,
            anchor: 0,
        });
        self.write_contours(ctx, id, contours, label)
    }

    /// Shift+J: join two gathered endpoint references.
    ///
    /// Opposite ends of one open contour close it; endpoints of different
    /// contours concatenate end-to-start (reversing either side as needed),
    /// merging coincident positions at the junction.
    fn join_selected_endpoints(&mut self, ctx: &ToolContext) -> OutputVec {
        if self.selected_endpoints.len() < 2 {
            return smallvec![];
        }
        let (a, b) = (self.selected_endpoints[0], self.selected_endpoints[1]);
        if a.node != b.node {
            return smallvec![]; // v1: joins stay within one shape node
        }
        let Some((id, mut contours)) = Self::current_contours(ctx) else {
            return smallvec![];
        };

        let endpoint_of = |contours: &[VectorPath], r: AnchorRef| -> Option<bool> {
            let p = contours.get(r.contour)?;
            if p.closed || p.anchors.len() < 2 {
                return None; // reject closed paths and degenerate contours
            }
            Some(r.anchor == 0 || r.anchor + 1 == p.anchors.len())
        };
        if endpoint_of(&contours, a) != Some(true) || endpoint_of(&contours, b) != Some(true) {
            return smallvec![];
        }

        let label = "Join nodes";
        if a.contour == b.contour {
            let Some(p) = contours.get_mut(a.contour) else {
                return smallvec![];
            };
            let last = p.anchors.len() - 1;
            if !((a.anchor == 0 && b.anchor == last) || (a.anchor == last && b.anchor == 0)) {
                return smallvec![]; // same-contour joins need opposite ends
            }
            // Merge coincident endpoint positions into one anchor.
            let last_anchor = *p.anchors.last().unwrap();
            if (last_anchor.pos - p.anchors[0].pos).length_squared() <= 1e-12 {
                p.anchors[0].tan_in = last_anchor.tan_in;
                p.anchors.pop();
            }
            p.closed = true;

            let was_plain = matches!(
                ctx.doc.nodes.get(id).map(|n| &n.kind),
                Some(NodeKind::Shape(ShapeKind::Path(_)))
            );
            if was_plain && a.contour == 0 {
                self.selected_anchor = Some(0);
                let value = contours.into_iter().next().unwrap_or_default();
                return self.commit_path_value(ctx, id, value, label);
            }
            return self.write_contours(ctx, id, contours, label);
        }

        // Different contours: orient both so A's end meets B's start.
        if a.contour >= contours.len() || b.contour >= contours.len() {
            return smallvec![];
        }
        let mut left = contours[a.contour].clone();
        if a.anchor == 0 {
            left.reverse(); // want A to be the END of `left`
        }
        let mut right = contours[b.contour].clone();
        if b.anchor + 1 == right.anchors.len() {
            right.reverse(); // want B to be the START of `right`
        }

        // Merge coincident junction positions.
        if (left.anchors.last().unwrap().pos - right.anchors[0].pos).length_squared() <= 1e-12 {
            let first = *right.anchors.first().unwrap();
            if let Some(junction) = left.anchors.last_mut() {
                junction.tan_out = first.tan_out;
            }
            left.anchors.extend(right.anchors.into_iter().skip(1));
        } else {
            left.anchors.extend(right.anchors);
        }

        // Splice replaces one contour with one merged contour (length is
        // unchanged), so `b`'s index stays valid for removal.
        contours.splice(a.contour..=a.contour, std::iter::once(left));
        contours.remove(b.contour);
        let merged_index = a.contour.min(contours.len().saturating_sub(1));
        self.selected_endpoints.clear();
        self.selected_anchor = None;
        self.selected_ref = Some(AnchorRef {
            node: id,
            contour: merged_index,
            anchor: 0,
        });
        self.write_contours(ctx, id, contours, label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use renamite_animation::Frame;
    use renamite_behavior_common::{Modifiers, Selection, SnapConfig, ViewTransform};
    use renamite_history::{History, ProjectMut};
    use std::sync::LazyLock;

    /// The editor's current-paint swatch has no test hook; a shared black fill
    /// keeps `ToolContext` borrows simple (static ref, no temporaries).
    static TEST_PAINT: LazyLock<StylePaint> = LazyLock::new(|| StylePaint::solid(Color::BLACK));
    use renamite_model::{Color, Document, GradientStops, Scene, evaluate};

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
            let shape = doc.create_node(Node::new(
                "box",
                NodeKind::Shape(ShapeKind::Rect {
                    pos: Animated::new(DVec2::new(100.0, 100.0)),
                    size: Animated::new(DVec2::splat(50.0)),
                    rounded: Animated::new(0.0),
                }),
            ));
            let fill = doc.create_node(Node::new(
                "fill",
                NodeKind::Style(StyleKind::Fill {
                    paint: StylePaint::solid(Color::BLACK),
                    rule: FillRule::NonZero,
                }),
            ));
            doc.attach(shape, Parent::Comp(comp), 0).unwrap();
            doc.attach(fill, Parent::Comp(comp), 1).unwrap();
            Self {
                doc,
                clips: Default::default(),
                clip_order: vec![],
                machines: Default::default(),
                machine_order: vec![],
                start: None,
                selection: Selection::default(),
                shape,
            }
        }
        fn scene(&self) -> Scene {
            evaluate(&self.doc, self.doc.main, 0.0)
        }
        fn pm(&mut self) -> ProjectMut<'_> {
            ProjectMut {
                document: &mut self.doc,
                clips: &mut self.clips,
                clip_order: &mut self.clip_order,
                machines: &mut self.machines,
                machine_order: &mut self.machine_order,
                start_machine: &mut self.start,
            }
        }
    }

    fn ctx_of<'a>(w: &'a World, scene: &'a Scene, m: Modifiers) -> ToolContext<'a> {
        ToolContext {
            doc: &w.doc,
            scene,
            comp: w.doc.main,
            selection: &w.selection,
            playhead: Frame(0),
            record: false,
            view: ViewTransform::identity(),
            snap: SnapConfig {
                grid: None,
                anchor: false,
                guide: false,
            },
            modifiers: m,
            current_paint: &TEST_PAINT,
        }
    }

    fn drive(w: &mut World, tool: &mut SelectTool, h: &mut History, ev: CanvasEvent, m: Modifiers) {
        let scene = w.scene();
        let outs = {
            let ctx = ctx_of(w, &scene, m);
            tool.handle(&ctx, ev)
        };
        for o in outs {
            match o {
                ToolOutput::BeginTransaction(l) => h.begin(l),
                ToolOutput::CommitTransaction => h.commit(),
                ToolOutput::CancelTransaction => h.cancel(&mut w.pm()).unwrap(),
                ToolOutput::Commands(cmds) => {
                    for c in cmds {
                        h.apply(&mut w.pm(), c).unwrap();
                    }
                }
                ToolOutput::RequestSelection(SelectionChange::Set(n)) => w.selection.nodes = n,
                ToolOutput::RequestSelection(SelectionChange::Toggle(n)) => {
                    if let Some(i) = w.selection.nodes.iter().position(|&x| x == n) {
                        w.selection.nodes.remove(i);
                    } else {
                        w.selection.nodes.push(n);
                    }
                }
                _ => {}
            }
        }
    }

    fn drive_grad(
        w: &mut World,
        tool: &mut GradientTool,
        h: &mut History,
        ev: CanvasEvent,
        m: Modifiers,
    ) {
        let scene = w.scene();
        let outs = {
            let ctx = ToolContext {
                doc: &w.doc,
                scene: &scene,
                comp: w.doc.main,
                selection: &w.selection,
                playhead: Frame(0),
                record: false,
                view: ViewTransform::identity(),
                snap: SnapConfig {
                    grid: None,
                    anchor: false,
                    guide: false,
                },
                modifiers: m,
                current_paint: &StylePaint::solid(Color::BLACK),
            };
            tool.handle(&ctx, ev)
        };
        for o in outs {
            match o {
                ToolOutput::BeginTransaction(l) => h.begin(l),
                ToolOutput::CommitTransaction => h.commit(),
                ToolOutput::CancelTransaction => h.cancel(&mut w.pm()).unwrap(),
                ToolOutput::Commands(cmds) => {
                    for c in cmds {
                        h.apply(&mut w.pm(), c).unwrap();
                    }
                }
                ToolOutput::RequestSelection(SelectionChange::Set(n)) => w.selection.nodes = n,
                _ => {}
            }
        }
    }

    fn fill_style(w: &World) -> NodeId {
        w.doc.compositions[w.doc.main]
            .children
            .iter()
            .copied()
            .find(|&id| {
                matches!(
                    w.doc.nodes[id].kind,
                    NodeKind::Style(StyleKind::Fill { .. })
                )
            })
            .unwrap()
    }

    #[test]
    fn fill_tool_replaces_following_fill_paint_and_is_undoable() {
        let mut w = World::new();
        let mut h = History::new();
        let mut tool = FillTool;

        let new_paint = StylePaint::solid(Color::rgba(0.0, 1.0, 0.0, 1.0));
        let scene = w.scene();
        let ctx = ToolContext {
            doc: &w.doc,
            scene: &scene,
            comp: w.doc.main,
            selection: &w.selection,
            playhead: Frame(0),
            record: false,
            view: ViewTransform::identity(),
            snap: SnapConfig {
                grid: None,
                anchor: false,
                guide: false,
            },
            modifiers: Modifiers::none(),
            current_paint: &new_paint,
        };

        let outs = tool.handle(
            &ctx,
            CanvasEvent::PointerDown {
                pos: DVec2::new(100.0, 100.0),
                button: PointerButton::Primary,
            },
        );
        drop(scene);

        for out in outs {
            match out {
                ToolOutput::BeginTransaction(l) => h.begin(l),
                ToolOutput::CommitTransaction => h.commit(),
                ToolOutput::Commands(cmds) => {
                    for cmd in cmds {
                        h.apply(&mut w.pm(), cmd).unwrap();
                    }
                }
                _ => {}
            }
        }

        let fill = w.doc.compositions[w.doc.main]
            .children
            .iter()
            .copied()
            .find(|&id| {
                matches!(
                    w.doc.nodes[id].kind,
                    NodeKind::Style(StyleKind::Fill { .. })
                )
            })
            .unwrap();

        let NodeKind::Style(StyleKind::Fill { paint, .. }) = &w.doc.nodes[fill].kind else {
            panic!()
        };
        assert_eq!(paint.base_color(), Color::rgba(0.0, 1.0, 0.0, 1.0));

        h.undo(&mut w.pm()).unwrap();
        let NodeKind::Style(StyleKind::Fill { paint, .. }) = &w.doc.nodes[fill].kind else {
            panic!()
        };
        assert_eq!(paint.base_color(), Color::BLACK);
    }

    #[test]
    fn fill_tool_does_nothing_on_empty_space() {
        let w = World::new();
        let mut tool = FillTool;
        let paint = StylePaint::solid(Color::WHITE);
        let scene = w.scene();
        let ctx = ToolContext {
            doc: &w.doc,
            scene: &scene,
            comp: w.doc.main,
            selection: &w.selection,
            playhead: Frame(0),
            record: false,
            view: ViewTransform::identity(),
            snap: SnapConfig {
                grid: None,
                anchor: false,
                guide: false,
            },
            modifiers: Modifiers::none(),
            current_paint: &paint,
        };
        let outs = tool.handle(
            &ctx,
            CanvasEvent::PointerDown {
                pos: DVec2::new(10_000.0, 10_000.0),
                button: PointerButton::Primary,
            },
        );
        assert!(outs.is_empty());
    }

    #[test]
    fn gradient_tool_converts_fill_and_drags_axis() {
        let (mut w, mut t, mut h) = (World::new(), GradientTool::default(), History::new());
        let m = Modifiers::none();
        // Rect spans (75,75)-(125,125). Press near its top-left corner.
        drive_grad(
            &mut w,
            &mut t,
            &mut h,
            CanvasEvent::PointerDown {
                pos: DVec2::new(90.0, 90.0),
                button: PointerButton::Primary,
            },
            m,
        );
        assert_eq!(w.selection.nodes, vec![w.shape]);
        let fill = fill_style(&w);
        drive_grad(
            &mut w,
            &mut t,
            &mut h,
            CanvasEvent::PointerMove {
                pos: DVec2::new(110.0, 90.0),
            },
            m,
        );
        drive_grad(
            &mut w,
            &mut t,
            &mut h,
            CanvasEvent::PointerUp {
                pos: DVec2::new(110.0, 90.0),
                button: PointerButton::Primary,
            },
            m,
        );
        let paint = match &w.doc.nodes[fill].kind {
            NodeKind::Style(StyleKind::Fill { paint, .. }) => paint.clone(),
            _ => panic!("style node missing"),
        };
        let StylePaint::Gradient(g) = paint else {
            panic!("expected gradient paint");
        };
        assert_eq!(g.kind, GradientKind::Linear);
        assert_eq!(g.start.value_at(0.0), DVec2::new(90.0, 90.0));
        assert_eq!(g.end.value_at(0.0), DVec2::new(110.0, 90.0));
        // Undo restores the exact solid; redo re-applies the gradient.
        h.undo(&mut w.pm()).unwrap();
        let paint = match &w.doc.nodes[fill].kind {
            NodeKind::Style(StyleKind::Fill { paint, .. }) => paint.clone(),
            _ => panic!("style node missing"),
        };
        assert!(matches!(paint, StylePaint::Solid { .. }));
        h.redo(&mut w.pm()).unwrap();
        let paint = match &w.doc.nodes[fill].kind {
            NodeKind::Style(StyleKind::Fill { paint, .. }) => paint.clone(),
            _ => panic!("style node missing"),
        };
        assert!(matches!(paint, StylePaint::Gradient(_)));
    }

    #[test]
    fn gradient_tool_reuses_existing_axis_and_drag_is_one_undo() {
        let (mut w, mut t, mut h) = (World::new(), GradientTool::default(), History::new());
        let fill = fill_style(&w);
        let grad = StylePaint::linear(
            DVec2::new(80.0, 90.0),
            DVec2::new(120.0, 90.0),
            GradientStops::default(),
        );
        let Some(prev) = w.doc.nodes.get_mut(fill).map(|n| {
            let NodeKind::Style(st) = &mut n.kind else {
                unreachable!()
            };
            st.swap_paint(grad)
        }) else {
            unreachable!()
        };
        assert!(matches!(prev, StylePaint::Solid { .. }));
        let m = Modifiers::none();
        // Press near the START handle (world (80,90)); move past threshold.
        drive_grad(
            &mut w,
            &mut t,
            &mut h,
            CanvasEvent::PointerDown {
                pos: DVec2::new(81.0, 90.0),
                button: PointerButton::Primary,
            },
            m,
        );
        drive_grad(
            &mut w,
            &mut t,
            &mut h,
            CanvasEvent::PointerMove {
                pos: DVec2::new(70.0, 90.0),
            },
            m,
        );
        drive_grad(
            &mut w,
            &mut t,
            &mut h,
            CanvasEvent::PointerUp {
                pos: DVec2::new(70.0, 90.0),
                button: PointerButton::Primary,
            },
            m,
        );
        let paint = match &w.doc.nodes[fill].kind {
            NodeKind::Style(StyleKind::Fill { paint, .. }) => paint.clone(),
            _ => panic!("style node missing"),
        };
        let StylePaint::Gradient(g) = paint else {
            panic!("expected gradient paint");
        };
        assert_eq!(g.start.value_at(0.0), DVec2::new(70.0, 90.0));
        assert_eq!(g.end.value_at(0.0), DVec2::new(120.0, 90.0));
    }

    #[test]
    fn gradient_tool_overlay_and_radial_shift() {
        let (mut w, mut t, mut h) = (World::new(), GradientTool::default(), History::new());
        let m = Modifiers {
            shift: true,
            alt: false,
            ctrl: false,
        };
        drive_grad(
            &mut w,
            &mut t,
            &mut h,
            CanvasEvent::PointerDown {
                pos: DVec2::new(90.0, 90.0),
                button: PointerButton::Primary,
            },
            m,
        );
        // No overlay before the first move.
        {
            let scene = w.scene();
            let ctx = ToolContext {
                doc: &w.doc,
                scene: &scene,
                comp: w.doc.main,
                selection: &w.selection,
                playhead: Frame(0),
                record: false,
                view: ViewTransform::identity(),
                snap: SnapConfig {
                    grid: None,
                    anchor: false,
                    guide: false,
                },
                modifiers: m,
                current_paint: &StylePaint::solid(Color::BLACK),
            };
            assert_eq!(t.overlay(&ctx), ToolOverlay::None);
        }
        drive_grad(
            &mut w,
            &mut t,
            &mut h,
            CanvasEvent::PointerMove {
                pos: DVec2::new(90.0, 110.0),
            },
            m,
        );
        // After the move the axis line is shown with radial=true.
        {
            let scene = w.scene();
            let ctx = ToolContext {
                doc: &w.doc,
                scene: &scene,
                comp: w.doc.main,
                selection: &w.selection,
                playhead: Frame(0),
                record: false,
                view: ViewTransform::identity(),
                snap: SnapConfig {
                    grid: None,
                    anchor: false,
                    guide: false,
                },
                modifiers: m,
                current_paint: &StylePaint::solid(Color::BLACK),
            };
            assert_eq!(
                t.overlay(&ctx),
                ToolOverlay::GradientLine {
                    start: DVec2::new(90.0, 90.0),
                    end: DVec2::new(90.0, 110.0),
                    radial: true,
                }
            );
        }
        drive_grad(
            &mut w,
            &mut t,
            &mut h,
            CanvasEvent::PointerUp {
                pos: DVec2::new(90.0, 110.0),
                button: PointerButton::Primary,
            },
            m,
        );
        // Shift forced a radial gradient.
        let paint = match &w.doc.nodes[fill_style(&w)].kind {
            NodeKind::Style(StyleKind::Fill { paint, .. }) => paint.clone(),
            _ => panic!("style node missing"),
        };
        let StylePaint::Gradient(g) = paint else {
            panic!("expected gradient paint");
        };
        assert_eq!(g.kind, GradientKind::Radial);
    }

    #[test]
    fn click_selects_drag_moves_one_undo() {
        let (mut w, mut t, mut h) = (World::new(), SelectTool::default(), History::new());
        let m = Modifiers::none();
        drive(
            &mut w,
            &mut t,
            &mut h,
            CanvasEvent::PointerDown {
                pos: DVec2::new(100.0, 100.0),
                button: PointerButton::Primary,
            },
            m,
        );
        assert_eq!(w.selection.nodes, vec![w.shape]);
        drive(
            &mut w,
            &mut t,
            &mut h,
            CanvasEvent::PointerMove {
                pos: DVec2::new(120.0, 100.0),
            },
            m,
        );
        drive(
            &mut w,
            &mut t,
            &mut h,
            CanvasEvent::PointerMove {
                pos: DVec2::new(140.0, 110.0),
            },
            m,
        );
        drive(
            &mut w,
            &mut t,
            &mut h,
            CanvasEvent::PointerUp {
                pos: DVec2::new(140.0, 110.0),
                button: PointerButton::Primary,
            },
            m,
        );

        let p = w
            .doc
            .value_at(w.shape, &PropPath::new("transform.position"), 0.0)
            .unwrap();
        assert_eq!(p, Value::DVec2(DVec2::new(40.0, 10.0))); // moved by total drag delta
        h.undo(&mut w.pm()).unwrap();
        assert!(!h.can_undo(), "whole drag = one undo step");
        assert_eq!(
            w.doc
                .value_at(w.shape, &PropPath::new("transform.position"), 0.0)
                .unwrap(),
            Value::DVec2(DVec2::ZERO)
        );
    }

    #[test]
    fn escape_cancels_drag_completely() {
        let (mut w, mut t, mut h) = (World::new(), SelectTool::default(), History::new());
        let m = Modifiers::none();
        drive(
            &mut w,
            &mut t,
            &mut h,
            CanvasEvent::PointerDown {
                pos: DVec2::new(100.0, 100.0),
                button: PointerButton::Primary,
            },
            m,
        );
        drive(
            &mut w,
            &mut t,
            &mut h,
            CanvasEvent::PointerMove {
                pos: DVec2::new(150.0, 100.0),
            },
            m,
        );
        drive(&mut w, &mut t, &mut h, CanvasEvent::KeyDown(Key::Escape), m);
        assert_eq!(
            w.doc
                .value_at(w.shape, &PropPath::new("transform.position"), 0.0)
                .unwrap(),
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
        fn wrap(a: f64) -> f64 {
            let mut a = a % TAU;
            if a > PI {
                a -= TAU;
            }
            a
        }
    }

    #[test]
    fn rubber_band_selects_contained() {
        let (mut w, mut t, mut h) = (World::new(), SelectTool::default(), History::new());
        let m = Modifiers::none();
        drive(
            &mut w,
            &mut t,
            &mut h,
            CanvasEvent::PointerDown {
                pos: DVec2::new(0.0, 0.0),
                button: PointerButton::Primary,
            },
            m,
        );
        drive(
            &mut w,
            &mut t,
            &mut h,
            CanvasEvent::PointerMove {
                pos: DVec2::new(300.0, 300.0),
            },
            m,
        );
        drive(
            &mut w,
            &mut t,
            &mut h,
            CanvasEvent::PointerUp {
                pos: DVec2::new(300.0, 300.0),
                button: PointerButton::Primary,
            },
            m,
        );
        assert_eq!(w.selection.nodes, vec![w.shape]);
    }

    #[test]
    fn delete_detaches_and_is_undoable() {
        let (mut w, mut t, mut h) = (World::new(), SelectTool::default(), History::new());
        w.selection.nodes = vec![w.shape];
        drive(
            &mut w,
            &mut t,
            &mut h,
            CanvasEvent::KeyDown(Key::Delete),
            Modifiers::none(),
        );
        assert!(w.doc.locate(w.shape).is_none());
        h.undo(&mut w.pm()).unwrap();
        assert!(w.doc.locate(w.shape).is_some());
    }

    #[test]
    fn dragging_selected_group_moves_descendants() {
        let mut world = World::new();

        let comp = world.doc.main;
        let group = world.doc.create_node(Node::new("group", NodeKind::Group));
        let fill = fill_style(&world);

        world.doc.detach(world.shape).unwrap();
        world.doc.detach(fill).unwrap();
        world
            .doc
            .attach(world.shape, Parent::Node(group), 0)
            .unwrap();
        world.doc.attach(fill, Parent::Node(group), 1).unwrap();
        world.doc.attach(group, Parent::Comp(comp), 0).unwrap();

        world.selection.nodes = vec![group];

        let before = world.scene();
        let before_bounds = selection_bounds(&world.doc, &before, &[group]).unwrap();

        let mut tool = SelectTool::default();
        let mut history = History::new();

        drive(
            &mut world,
            &mut tool,
            &mut history,
            CanvasEvent::PointerDown {
                pos: DVec2::new(100.0, 100.0),
                button: PointerButton::Primary,
            },
            Modifiers::none(),
        );

        drive(
            &mut world,
            &mut tool,
            &mut history,
            CanvasEvent::PointerMove {
                pos: DVec2::new(140.0, 120.0),
            },
            Modifiers::none(),
        );

        drive(
            &mut world,
            &mut tool,
            &mut history,
            CanvasEvent::PointerUp {
                pos: DVec2::new(140.0, 120.0),
                button: PointerButton::Primary,
            },
            Modifiers::none(),
        );

        let after = world.scene();
        let after_bounds = selection_bounds(&world.doc, &after, &[group]).unwrap();

        assert!(((after_bounds.0 - before_bounds.0) - DVec2::new(40.0, 20.0)).length() < 1e-6);

        history.undo(&mut world.pm()).unwrap();

        let restored = world.scene();
        let restored_bounds = selection_bounds(&world.doc, &restored, &[group]).unwrap();

        assert_eq!(restored_bounds, before_bounds);
    }

    #[test]
    fn moving_group_pivot_preserves_rendered_geometry() {
        let mut world = World::new();
        let comp = world.doc.main;

        let group = world.doc.create_node(Node::new("group", NodeKind::Group));
        let fill = fill_style(&world);

        world.doc.detach(world.shape).unwrap();
        world.doc.detach(fill).unwrap();

        world
            .doc
            .attach(world.shape, Parent::Node(group), 0)
            .unwrap();

        world.doc.attach(fill, Parent::Node(group), 1).unwrap();

        world.doc.attach(group, Parent::Comp(comp), 0).unwrap();

        world.selection.nodes = vec![group];

        let before = world.scene();
        let before_path = before.items[0].path.clone();

        let pivot = node_transform_context(&world.doc, group, 0.0)
            .unwrap()
            .pivot_world;

        let mut tool = SelectTool::default();
        let mut history = History::new();

        drive(
            &mut world,
            &mut tool,
            &mut history,
            CanvasEvent::PointerDown {
                pos: pivot,
                button: PointerButton::Primary,
            },
            Modifiers::none(),
        );

        drive(
            &mut world,
            &mut tool,
            &mut history,
            CanvasEvent::PointerMove {
                pos: pivot + DVec2::new(25.0, 15.0),
            },
            Modifiers::none(),
        );

        drive(
            &mut world,
            &mut tool,
            &mut history,
            CanvasEvent::PointerUp {
                pos: pivot + DVec2::new(25.0, 15.0),
                button: PointerButton::Primary,
            },
            Modifiers::none(),
        );

        let after = world.scene();

        assert_eq!(
            before_path.elements(),
            after.items[0].path.elements(),
            "compensated pivot movement must not move rendered content",
        );

        let anchor = world
            .doc
            .value_at(group, &PropPath::new("transform.anchor"), 0.0)
            .unwrap();

        let position = world
            .doc
            .value_at(group, &PropPath::new("transform.position"), 0.0)
            .unwrap();

        assert_eq!(anchor, Value::DVec2(DVec2::new(25.0, 15.0)));

        assert_eq!(position, Value::DVec2(DVec2::new(25.0, 15.0)));

        history.undo(&mut world.pm()).unwrap();

        assert!(!history.can_undo());
    }

    #[test]
    fn pivot_compensation_is_exact_under_rotation_and_scale() {
        let mut doc = Document::empty();

        let group = doc.create_node(Node::new("group", NodeKind::Group));

        doc.nodes[group].transform.rotation = Animated::new(Angle(35.0));

        doc.nodes[group].transform.scale = Animated::new(DVec2::new(180.0, 70.0));

        doc.attach(group, Parent::Comp(doc.main), 0).unwrap();

        let before = node_transform_context(&doc, group, 0.0).unwrap().world;

        let context = node_transform_context(&doc, group, 0.0).unwrap();

        let new_position = context.position + DVec2::new(30.0, -12.0);

        let local_delta = affine_vector(context.linear.inverse(), new_position - context.position);

        doc.set_static(
            group,
            &PropPath::new("transform.position"),
            &Value::DVec2(new_position),
        )
        .unwrap();

        doc.set_static(
            group,
            &PropPath::new("transform.anchor"),
            &Value::DVec2(context.anchor + local_delta),
        )
        .unwrap();

        let after = node_transform_context(&doc, group, 0.0).unwrap().world;

        for (a, b) in before.as_coeffs().iter().zip(after.as_coeffs()) {
            assert!((*a - b).abs() < 1e-9, "before={before:?}, after={after:?}",);
        }
    }

    #[test]
    fn rotate_keeps_selection_pivot_fixed() {
        let mut world = World::new();
        world.selection.nodes = vec![world.shape];

        let scene = world.scene();
        let ctx = ctx_of(&world, &scene, Modifiers::none());
        let (min, max) = selection_bounds(&world.doc, &scene, &world.selection.nodes).unwrap();
        let (rotate_handle, _) = handles(&ctx, min, max);
        let pivot = (min + max) * 0.5;

        let before = node_transform_context(&world.doc, world.shape, 0.0)
            .unwrap()
            .world;
        let local = before.inverse() * Point::new(pivot.x, pivot.y);

        let mut tool = SelectTool::default();
        let mut history = History::new();

        drive(
            &mut world,
            &mut tool,
            &mut history,
            CanvasEvent::PointerDown {
                pos: rotate_handle,
                button: PointerButton::Primary,
            },
            Modifiers::none(),
        );

        // Move the mouse +90° around the pivot (rotate handle starts at -90°).
        let target = pivot + DVec2::new(50.0, 0.0);
        drive(
            &mut world,
            &mut tool,
            &mut history,
            CanvasEvent::PointerMove { pos: target },
            Modifiers::none(),
        );
        drive(
            &mut world,
            &mut tool,
            &mut history,
            CanvasEvent::PointerUp {
                pos: target,
                button: PointerButton::Primary,
            },
            Modifiers::none(),
        );

        let rotation = world
            .doc
            .value_at(world.shape, &PropPath::new("transform.rotation"), 0.0)
            .unwrap();
        assert_eq!(rotation, Value::Angle(Angle(90.0)));

        let after = node_transform_context(&world.doc, world.shape, 0.0)
            .unwrap()
            .world;
        let img = after * local;
        assert!(
            (img.x - pivot.x).abs() < 1e-6 && (img.y - pivot.y).abs() < 1e-6,
            "pivot drifted: expected {pivot:?}, got ({}, {})",
            img.x,
            img.y,
        );

        history.undo(&mut world.pm()).unwrap();
        let rotation_back = world
            .doc
            .value_at(world.shape, &PropPath::new("transform.rotation"), 0.0)
            .unwrap();
        assert_eq!(rotation_back, Value::Angle(Angle(0.0)));
    }

    #[test]
    fn scale_keeps_opposite_corner_pinned() {
        let mut world = World::new();
        world.selection.nodes = vec![world.shape];

        let scene = world.scene();
        let ctx = ctx_of(&world, &scene, Modifiers::none());
        let (min, max) = selection_bounds(&world.doc, &scene, &world.selection.nodes).unwrap();
        let (_, scale_handle) = handles(&ctx, min, max);

        // Local point sitting under the fixed corner before the drag.
        let before = node_transform_context(&world.doc, world.shape, 0.0)
            .unwrap()
            .world;
        let local = before.inverse() * Point::new(min.x, min.y);

        let mut tool = SelectTool::default();
        let mut history = History::new();

        drive(
            &mut world,
            &mut tool,
            &mut history,
            CanvasEvent::PointerDown {
                pos: scale_handle,
                button: PointerButton::Primary,
            },
            Modifiers::none(),
        );

        // Drag the bottom-right corner to double the distance from `min`.
        let target = min + (max - min) * 2.0;
        drive(
            &mut world,
            &mut tool,
            &mut history,
            CanvasEvent::PointerMove { pos: target },
            Modifiers::none(),
        );
        drive(
            &mut world,
            &mut tool,
            &mut history,
            CanvasEvent::PointerUp {
                pos: target,
                button: PointerButton::Primary,
            },
            Modifiers::none(),
        );

        let scale = world
            .doc
            .value_at(world.shape, &PropPath::new("transform.scale"), 0.0)
            .unwrap();
        assert_eq!(scale, Value::DVec2(DVec2::splat(200.0)));

        // The opposite corner stays pinned in world space.
        let after = node_transform_context(&world.doc, world.shape, 0.0)
            .unwrap()
            .world;
        let img = after * local;
        assert!(
            (img.x - min.x).abs() < 1e-6 && (img.y - min.y).abs() < 1e-6,
            "corner drifted: expected {min:?}, got ({}, {})",
            img.x,
            img.y,
        );

        let scene = world.scene();
        let (min_after, _) = selection_bounds(&world.doc, &scene, &world.selection.nodes).unwrap();
        assert!((min_after - min).length() < 1e-6);

        history.undo(&mut world.pm()).unwrap();
        let scale_back = world
            .doc
            .value_at(world.shape, &PropPath::new("transform.scale"), 0.0)
            .unwrap();
        assert_eq!(scale_back, Value::DVec2(DVec2::splat(100.0)));
    }

    #[test]
    fn child_click_preserves_selected_group() {
        let mut world = World::new();
        let comp = world.doc.main;

        let group = world.doc.create_node(Node::new("group", NodeKind::Group));
        let fill = fill_style(&world);

        world.doc.detach(world.shape).unwrap();
        world.doc.detach(fill).unwrap();
        world
            .doc
            .attach(world.shape, Parent::Node(group), 0)
            .unwrap();
        world.doc.attach(fill, Parent::Node(group), 1).unwrap();
        world.doc.attach(group, Parent::Comp(comp), 0).unwrap();

        world.selection.nodes = vec![group];

        let mut tool = SelectTool::default();
        let mut history = History::new();

        // Pointer down + up at the child's center must not replace group.
        drive(
            &mut world,
            &mut tool,
            &mut history,
            CanvasEvent::PointerDown {
                pos: DVec2::new(100.0, 100.0),
                button: PointerButton::Primary,
            },
            Modifiers::none(),
        );

        assert_eq!(world.selection.nodes, vec![group]);

        drive(
            &mut world,
            &mut tool,
            &mut history,
            CanvasEvent::PointerUp {
                pos: DVec2::new(100.0, 100.0),
                button: PointerButton::Primary,
            },
            Modifiers::none(),
        );

        assert_eq!(world.selection.nodes, vec![group]);
    }

    #[test]
    fn double_click_selects_immediate_group_child() {
        // Outer -> Inner -> Shape.
        let mut world = World::new();
        let comp = world.doc.main;

        let outer = world.doc.create_node(Node::new("outer", NodeKind::Group));
        let inner = world.doc.create_node(Node::new("inner", NodeKind::Group));
        let fill = fill_style(&world);

        world.doc.detach(world.shape).unwrap();
        world.doc.detach(fill).unwrap();
        world
            .doc
            .attach(world.shape, Parent::Node(inner), 0)
            .unwrap();
        world.doc.attach(fill, Parent::Node(inner), 1).unwrap();
        world.doc.attach(inner, Parent::Node(outer), 0).unwrap();
        world.doc.attach(outer, Parent::Comp(comp), 0).unwrap();

        // Select Outer, double-click Shape. Selection must contain Inner.
        world.selection.nodes = vec![outer];

        let mut tool = SelectTool::default();
        let mut history = History::new();

        drive(
            &mut world,
            &mut tool,
            &mut history,
            CanvasEvent::DoubleClick {
                pos: DVec2::new(100.0, 100.0),
            },
            Modifiers::none(),
        );

        assert_eq!(world.selection.nodes, vec![inner]);
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
                doc: &w.doc,
                scene: &scene,
                comp: w.doc.main,
                selection: &w.selection,
                playhead: Frame(0),
                record: false,
                view: ViewTransform::identity(),
                snap: SnapConfig {
                    grid: None,
                    anchor: false,
                    guide: false,
                },
                modifiers: Modifiers::none(),
                current_paint: &StylePaint::solid(Color::BLACK),
            };
            tool.handle(&ctx, ev)
        };
        let mut all: Vec<ToolOutput> = vec![];
        all.extend(mk(CanvasEvent::PointerDown {
            pos: DVec2::new(200.0, 200.0),
            button: PointerButton::Primary,
        }));
        all.extend(mk(CanvasEvent::PointerMove {
            pos: DVec2::new(260.0, 240.0),
        }));
        all.extend(mk(CanvasEvent::PointerUp {
            pos: DVec2::new(260.0, 240.0),
            button: PointerButton::Primary,
        }));
        drop(scene);
        for o in all {
            match o {
                ToolOutput::BeginTransaction(l) => h.begin(l),
                ToolOutput::CommitTransaction => h.commit(),
                ToolOutput::Commands(cmds) => {
                    for c in cmds {
                        h.apply(&mut w.pm(), c).unwrap();
                    }
                }
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

    fn shape_drag(tool: &mut ShapeTool, w: &World, m: Modifiers) -> Vec<ToolOutput> {
        let scene = w.scene();
        let mut mk = |ev| {
            let ctx = ToolContext {
                doc: &w.doc,
                scene: &scene,
                comp: w.doc.main,
                selection: &w.selection,
                playhead: Frame(0),
                record: false,
                view: ViewTransform::identity(),
                snap: SnapConfig {
                    grid: None,
                    anchor: false,
                    guide: false,
                },
                modifiers: m,
                current_paint: &StylePaint::solid(Color::BLACK),
            };
            tool.handle(&ctx, ev)
        };
        let mut all: Vec<ToolOutput> = vec![];
        all.extend(mk(CanvasEvent::PointerDown {
            pos: DVec2::new(200.0, 200.0),
            button: PointerButton::Primary,
        }));
        all.extend(mk(CanvasEvent::PointerMove {
            pos: DVec2::new(280.0, 280.0),
        }));
        all.extend(mk(CanvasEvent::PointerUp {
            pos: DVec2::new(280.0, 280.0),
            button: PointerButton::Primary,
        }));
        drop(scene);
        all
    }

    fn apply_all(w: &mut World, h: &mut History, all: Vec<ToolOutput>) {
        for o in all {
            match o {
                ToolOutput::BeginTransaction(l) => h.begin(l),
                ToolOutput::CommitTransaction => h.commit(),
                ToolOutput::Commands(cmds) => {
                    for c in cmds {
                        h.apply(&mut w.pm(), c).unwrap();
                    }
                }
                _ => {}
            }
        }
    }

    #[test]
    fn star_tool_creates_group_with_star_and_fill() {
        let mut w = World::new();
        let mut h = History::new();
        let mut tool = ShapeTool::new(ShapeToolKind::Star);
        let before = w.doc.compositions[w.doc.main].children.len();
        let all = shape_drag(&mut tool, &w, Modifiers::none());
        assert!(
            all.iter()
                .any(|o| matches!(o, ToolOutput::SwitchTool(ToolId::Select)))
        );
        apply_all(&mut w, &mut h, all);
        let comp = &w.doc.compositions[w.doc.main];
        assert_eq!(comp.children.len(), before + 1);
        let group = &w.doc.nodes[comp.children[0]];
        assert_eq!(group.children.len(), 2); // shape + fill
        match &w.doc.nodes[group.children[0]].kind {
            NodeKind::Shape(ShapeKind::Star {
                points,
                kind,
                outer_r,
                inner_r,
                ..
            }) => {
                assert_eq!(*kind, StarKind::Star);
                assert!((points.base - 5.0).abs() < 1e-9);
                assert!(outer_r.base > inner_r.base);
            }
            other => panic!("expected Star, got {other:?}"),
        }
        h.undo(&mut w.pm()).unwrap();
        assert_eq!(w.doc.compositions[w.doc.main].children.len(), before);
    }

    #[test]
    fn star_tool_shift_makes_polygon() {
        let mut w = World::new();
        let mut h = History::new();
        let mut tool = ShapeTool::new(ShapeToolKind::Star);
        let before = w.doc.compositions[w.doc.main].children.len();
        let all = shape_drag(
            &mut tool,
            &w,
            Modifiers {
                shift: true,
                ..Modifiers::none()
            },
        );
        apply_all(&mut w, &mut h, all);
        let comp = &w.doc.compositions[w.doc.main];
        assert_eq!(comp.children.len(), before + 1);
        let group = &w.doc.nodes[comp.children[0]];
        match &w.doc.nodes[group.children[0]].kind {
            NodeKind::Shape(ShapeKind::Polygon {
                points, outer_r, ..
            }) => {
                assert!((points.base - 6.0).abs() < 1e-9);
                assert!(outer_r.base > 0.0);
            }
            other => panic!("expected Polygon, got {other:?}"),
        }
    }

    #[test]
    fn star_tool_alt_makes_6_point_star() {
        let mut w = World::new();
        let mut h = History::new();
        let mut tool = ShapeTool::new(ShapeToolKind::Star);
        let all = shape_drag(
            &mut tool,
            &w,
            Modifiers {
                alt: true,
                ..Modifiers::none()
            },
        );
        apply_all(&mut w, &mut h, all);
        let comp = &w.doc.compositions[w.doc.main];
        let group = &w.doc.nodes[comp.children[0]];
        match &w.doc.nodes[group.children[0]].kind {
            NodeKind::Shape(ShapeKind::Star { points, .. }) => {
                assert!((points.base - 6.0).abs() < 1e-9);
            }
            other => panic!("expected Star, got {other:?}"),
        }
    }

    #[test]
    fn star_tool_tiny_drag_cancels() {
        let w = World::new();
        let mut tool = ShapeTool::new(ShapeToolKind::Star);
        let before = w.doc.compositions[w.doc.main].children.len();
        let scene = w.scene();
        let mut mk = |ev| {
            let ctx = ToolContext {
                doc: &w.doc,
                scene: &scene,
                comp: w.doc.main,
                selection: &w.selection,
                playhead: Frame(0),
                record: false,
                view: ViewTransform::identity(),
                snap: SnapConfig {
                    grid: None,
                    anchor: false,
                    guide: false,
                },
                modifiers: Modifiers::none(),
                current_paint: &StylePaint::solid(Color::BLACK),
            };
            tool.handle(&ctx, ev)
        };
        mk(CanvasEvent::PointerDown {
            pos: DVec2::new(200.0, 200.0),
            button: PointerButton::Primary,
        });
        mk(CanvasEvent::PointerMove {
            pos: DVec2::new(200.5, 200.5),
        });
        mk(CanvasEvent::PointerUp {
            pos: DVec2::new(200.5, 200.5),
            button: PointerButton::Primary,
        });
        drop(scene);
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
                ToolOutput::Commands(cmds) => {
                    for c in cmds {
                        created = h.apply(&mut w.pm(), c).unwrap().created;
                    }
                }
                ToolOutput::RequestSelection(SelectionChange::Set(n)) => w.selection.nodes = n,
                ToolOutput::RequestSelection(SelectionChange::Toggle(n)) => {
                    if let Some(i) = w.selection.nodes.iter().position(|&x| x == n) {
                        w.selection.nodes.remove(i);
                    } else {
                        w.selection.nodes.push(n);
                    }
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
            let path = w.doc.create_node(Node::new(
                "Shape",
                NodeKind::Shape(ShapeKind::Path(Animated::new(VectorPath {
                    closed: true,
                    anchors: vec![
                        Anchor::corner(DVec2::new(100.0, 100.0)),
                        Anchor::corner(DVec2::new(200.0, 100.0)),
                        Anchor::corner(DVec2::new(200.0, 200.0)),
                        Anchor::corner(DVec2::new(100.0, 200.0)),
                    ],
                }))),
            ));
            let fill = w.doc.create_node(Node::new(
                "Fill",
                NodeKind::Style(StyleKind::Fill {
                    paint: StylePaint::solid(Color::BLACK),
                    rule: FillRule::NonZero,
                }),
            ));
            let group = w.doc.create_node(Node::new("Path", NodeKind::Group));
            w.doc.attach(path, Parent::Node(group), 0).unwrap();
            w.doc.attach(fill, Parent::Node(group), 1).unwrap();
            w.doc.attach(group, Parent::Comp(w.doc.main), 0).unwrap();
            Self { w, group, path }
        }

        fn drive(
            &mut self,
            tool: &mut PathEditTool,
            h: &mut History,
            ev: CanvasEvent,
            m: Modifiers,
        ) {
            let scene = self.w.scene();
            let outs = {
                let ctx = ToolContext {
                    doc: &self.w.doc,
                    scene: &scene,
                    comp: self.w.doc.main,
                    selection: &self.w.selection,
                    playhead: Frame(0),
                    record: false,
                    view: ViewTransform::identity(),
                    snap: SnapConfig {
                        grid: None,
                        anchor: false,
                        guide: false,
                    },
                    modifiers: m,
                    current_paint: &StylePaint::solid(Color::BLACK),
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
                doc: &w.doc,
                scene: &scene,
                comp: w.doc.main,
                selection: &w.selection,
                playhead: Frame(0),
                record: false,
                view: ViewTransform::identity(),
                snap: SnapConfig {
                    grid: None,
                    anchor: false,
                    guide: false,
                },
                modifiers: Modifiers::none(),
                current_paint: &StylePaint::solid(Color::BLACK),
            };
            tool.handle(&ctx, ev)
        };
        let mut all: Vec<ToolOutput> = vec![];
        for (x, y) in [(100.0, 100.0), (200.0, 100.0)] {
            all.extend(mk(CanvasEvent::PointerDown {
                pos: DVec2::new(x, y),
                button: PointerButton::Primary,
            }));
            all.extend(mk(CanvasEvent::PointerUp {
                pos: DVec2::new(x, y),
                button: PointerButton::Primary,
            }));
        }
        all.extend(mk(CanvasEvent::KeyDown(Key::Enter)));
        drop(scene);
        let mut committed = false;
        let mut switched = false;
        for o in all {
            match o {
                ToolOutput::BeginTransaction(l) => h.begin(l),
                ToolOutput::CommitTransaction => {
                    h.commit();
                    committed = true;
                }
                ToolOutput::Commands(cmds) => {
                    for c in cmds {
                        h.apply(&mut w.pm(), c).unwrap();
                    }
                }
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
                doc: &w.doc,
                scene: &scene,
                comp: w.doc.main,
                selection: &w.selection,
                playhead: Frame(0),
                record: false,
                view: ViewTransform::identity(),
                snap: SnapConfig {
                    grid: None,
                    anchor: false,
                    guide: false,
                },
                modifiers: Modifiers::none(),
                current_paint: &StylePaint::solid(Color::BLACK),
            };
            tool.handle(&ctx, ev)
        };
        let mut all: Vec<ToolOutput> = vec![];
        for (x, y) in [(100.0, 100.0), (200.0, 100.0), (200.0, 200.0)] {
            all.extend(mk(CanvasEvent::PointerDown {
                pos: DVec2::new(x, y),
                button: PointerButton::Primary,
            }));
            all.extend(mk(CanvasEvent::PointerUp {
                pos: DVec2::new(x, y),
                button: PointerButton::Primary,
            }));
        }
        // Click near the first anchor: should close, not add a 4th point.
        all.extend(mk(CanvasEvent::PointerDown {
            pos: DVec2::new(105.0, 105.0),
            button: PointerButton::Primary,
        }));
        all.extend(mk(CanvasEvent::PointerUp {
            pos: DVec2::new(105.0, 105.0),
            button: PointerButton::Primary,
        }));
        drop(scene);
        for o in all {
            match o {
                ToolOutput::BeginTransaction(l) => h.begin(l),
                ToolOutput::CommitTransaction => h.commit(),
                ToolOutput::Commands(cmds) => {
                    for c in cmds {
                        h.apply(&mut w.pm(), c).unwrap();
                    }
                }
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
            doc: &doc,
            scene: &scene,
            comp: doc.main,
            selection: &sel,
            playhead: Frame(0),
            record: false,
            view: ViewTransform::identity(),
            snap: SnapConfig {
                grid: None,
                anchor: false,
                guide: false,
            },
            modifiers: Modifiers::none(),
            current_paint: &StylePaint::solid(Color::BLACK),
        };
        // First click-drag: anchor 0 becomes smooth/symmetric immediately.
        tool.handle(
            &ctx,
            CanvasEvent::PointerDown {
                pos: DVec2::new(0.0, 0.0),
                button: PointerButton::Primary,
            },
        );
        tool.handle(
            &ctx,
            CanvasEvent::PointerMove {
                pos: DVec2::new(30.0, 0.0),
            },
        );
        tool.handle(
            &ctx,
            CanvasEvent::PointerUp {
                pos: DVec2::new(30.0, 0.0),
                button: PointerButton::Primary,
            },
        );
        let ToolOverlay::PenPreview { anchors, .. } = tool.overlay(&ctx) else {
            panic!("expected preview")
        };
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
            doc: &doc,
            scene: &scene,
            comp: doc.main,
            selection: &sel,
            playhead: Frame(0),
            record: false,
            view: ViewTransform::identity(),
            snap: SnapConfig {
                grid: None,
                anchor: false,
                guide: false,
            },
            modifiers: Modifiers::none(),
            current_paint: &StylePaint::solid(Color::BLACK),
        };
        tool.handle(
            &ctx,
            CanvasEvent::PointerDown {
                pos: DVec2::new(0.0, 0.0),
                button: PointerButton::Primary,
            },
        );
        tool.handle(
            &ctx,
            CanvasEvent::PointerUp {
                pos: DVec2::new(0.0, 0.0),
                button: PointerButton::Primary,
            },
        );
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
        pw.drive(
            &mut tool,
            &mut h,
            CanvasEvent::PointerDown {
                pos: DVec2::new(100.0, 100.0),
                button: PointerButton::Primary,
            },
            Modifiers::none(),
        );
        pw.drive(
            &mut tool,
            &mut h,
            CanvasEvent::PointerMove {
                pos: DVec2::new(120.0, 130.0),
            },
            Modifiers::none(),
        );
        pw.drive(
            &mut tool,
            &mut h,
            CanvasEvent::PointerUp {
                pos: DVec2::new(120.0, 130.0),
                button: PointerButton::Primary,
            },
            Modifiers::none(),
        );
        let p = path_value(&pw.w, pw.path);
        assert_eq!(p.anchors[0].pos, DVec2::new(120.0, 130.0));
        h.undo(&mut pw.w.pm()).unwrap();
        assert!(!h.can_undo(), "whole drag = one undo step");
        assert_eq!(
            path_value(&pw.w, pw.path).anchors[0].pos,
            DVec2::new(100.0, 100.0)
        );
    }

    #[test]
    fn alt_click_cycles_tangent_mode() {
        let mut pw = PathWorld::new();
        pw.w.selection.nodes = vec![pw.path];
        let mut tool = PathEditTool::default();
        let mut h = History::new();
        let mut alt = Modifiers::none();
        alt.alt = true;
        pw.drive(
            &mut tool,
            &mut h,
            CanvasEvent::PointerDown {
                pos: DVec2::new(100.0, 100.0),
                button: PointerButton::Primary,
            },
            alt,
        );
        pw.drive(
            &mut tool,
            &mut h,
            CanvasEvent::PointerUp {
                pos: DVec2::new(100.0, 100.0),
                button: PointerButton::Primary,
            },
            Modifiers::none(),
        );
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
        pw.drive(
            &mut tool,
            &mut h,
            CanvasEvent::DoubleClick {
                pos: DVec2::new(150.0, 100.0),
            },
            Modifiers::none(),
        );
        let p = path_value(&pw.w, pw.path);
        assert_eq!(p.anchors.len(), n + 1);
        assert_eq!(tool.selected_anchor, Some(1)); // inserted anchor is active
    }

    #[test]
    fn insert_key_adds_midpoint_node_and_selects_it() {
        let mut pw = PathWorld::new();
        pw.w.selection.nodes = vec![pw.path];
        let mut tool = PathEditTool::default();
        let mut h = History::new();
        // Select anchor 0 by pressing on it.
        pw.drive(
            &mut tool,
            &mut h,
            CanvasEvent::PointerDown {
                pos: DVec2::new(100.0, 100.0),
                button: PointerButton::Primary,
            },
            Modifiers::none(),
        );
        pw.drive(
            &mut tool,
            &mut h,
            CanvasEvent::PointerUp {
                pos: DVec2::new(100.0, 100.0),
                button: PointerButton::Primary,
            },
            Modifiers::none(),
        );
        pw.drive(
            &mut tool,
            &mut h,
            CanvasEvent::KeyDown(Key::Insert),
            Modifiers::none(),
        );

        let p = path_value(&pw.w, pw.path);
        assert_eq!(p.anchors.len(), 5);
        assert_eq!(tool.selected_anchor, Some(1));
        assert_eq!(p.anchors[1].pos, DVec2::new(150.0, 100.0));

        h.undo(&mut pw.w.pm()).unwrap();
        assert_eq!(path_value(&pw.w, pw.path).anchors.len(), 4);
    }

    #[test]
    fn tab_cycles_anchor_selection() {
        let mut pw = PathWorld::new();
        pw.w.selection.nodes = vec![pw.path];
        let mut tool = PathEditTool::default();
        let mut h = History::new();

        pw.drive(
            &mut tool,
            &mut h,
            CanvasEvent::KeyDown(Key::Tab),
            Modifiers::none(),
        );
        assert_eq!(tool.selected_anchor, Some(0));
        pw.drive(
            &mut tool,
            &mut h,
            CanvasEvent::KeyDown(Key::Tab),
            Modifiers::none(),
        );
        assert_eq!(tool.selected_anchor, Some(1));
        pw.drive(
            &mut tool,
            &mut h,
            CanvasEvent::KeyDown(Key::Tab),
            Modifiers {
                shift: true,
                ..Modifiers::none()
            },
        );
        assert_eq!(tool.selected_anchor, Some(0));
    }

    #[test]
    fn arrow_keys_nudge_selected_anchor_and_undo() {
        let mut pw = PathWorld::new();
        pw.w.selection.nodes = vec![pw.path];
        let mut tool = PathEditTool::default();
        let mut h = History::new();
        pw.drive(
            &mut tool,
            &mut h,
            CanvasEvent::PointerDown {
                pos: DVec2::new(100.0, 100.0),
                button: PointerButton::Primary,
            },
            Modifiers::none(),
        );
        pw.drive(
            &mut tool,
            &mut h,
            CanvasEvent::PointerUp {
                pos: DVec2::new(100.0, 100.0),
                button: PointerButton::Primary,
            },
            Modifiers::none(),
        );

        pw.drive(
            &mut tool,
            &mut h,
            CanvasEvent::KeyDown(Key::ArrowRight),
            Modifiers::none(),
        );
        assert_eq!(
            path_value(&pw.w, pw.path).anchors[0].pos,
            DVec2::new(102.0, 100.0)
        );

        pw.drive(
            &mut tool,
            &mut h,
            CanvasEvent::KeyDown(Key::ArrowUp),
            Modifiers {
                shift: true,
                ..Modifiers::none()
            },
        );
        assert_eq!(
            path_value(&pw.w, pw.path).anchors[0].pos,
            DVec2::new(102.0, 80.0)
        );

        // Repeated nudges of the same anchor coalesce into one undo step
        // (history merges same-label transactions with coalescing commands),
        // exactly like a live drag.
        h.undo(&mut pw.w.pm()).unwrap();
        assert_eq!(
            path_value(&pw.w, pw.path).anchors[0].pos,
            DVec2::new(100.0, 100.0)
        );
        assert!(!h.can_undo());
    }

    #[test]
    fn node_mode_keys_set_tangent_mode_and_undo() {
        let mut pw = PathWorld::new();
        pw.w.selection.nodes = vec![pw.path];
        let mut tool = PathEditTool::default();
        let mut h = History::new();
        pw.drive(
            &mut tool,
            &mut h,
            CanvasEvent::PointerDown {
                pos: DVec2::new(200.0, 100.0),
                button: PointerButton::Primary,
            },
            Modifiers::none(),
        );
        pw.drive(
            &mut tool,
            &mut h,
            CanvasEvent::PointerUp {
                pos: DVec2::new(200.0, 100.0),
                button: PointerButton::Primary,
            },
            Modifiers::none(),
        );

        pw.drive(
            &mut tool,
            &mut h,
            CanvasEvent::KeyDown(Key::NodeSymmetric),
            Modifiers::none(),
        );
        assert_eq!(
            path_value(&pw.w, pw.path).anchors[1].mode,
            TangentMode::Symmetric
        );

        h.undo(&mut pw.w.pm()).unwrap();
        assert_eq!(
            path_value(&pw.w, pw.path).anchors[1].mode,
            TangentMode::Corner
        );
    }

    #[test]
    fn segment_line_curve_convert_adjacent_segment() {
        let mut pw = PathWorld::new();
        pw.w.selection.nodes = vec![pw.path];
        let mut tool = PathEditTool::default();
        let mut h = History::new();
        pw.drive(
            &mut tool,
            &mut h,
            CanvasEvent::PointerDown {
                pos: DVec2::new(100.0, 100.0),
                button: PointerButton::Primary,
            },
            Modifiers::none(),
        );
        pw.drive(
            &mut tool,
            &mut h,
            CanvasEvent::PointerUp {
                pos: DVec2::new(100.0, 100.0),
                button: PointerButton::Primary,
            },
            Modifiers::none(),
        );

        // Curve: thirds-rule tangents across segment 0 -> 1.
        pw.drive(
            &mut tool,
            &mut h,
            CanvasEvent::KeyDown(Key::SegmentCurve),
            Modifiers::none(),
        );
        let p = path_value(&pw.w, pw.path);
        assert_eq!(p.anchors[0].tan_out, DVec2::new(100.0 / 3.0, 0.0));
        assert_eq!(p.anchors[1].tan_in, DVec2::new(-100.0 / 3.0, 0.0));

        // Line: both tangents across the segment collapse to zero.
        pw.drive(
            &mut tool,
            &mut h,
            CanvasEvent::KeyDown(Key::SegmentLine),
            Modifiers::none(),
        );
        let p = path_value(&pw.w, pw.path);
        assert_eq!(p.anchors[0].tan_out, DVec2::ZERO);
        assert_eq!(p.anchors[1].tan_in, DVec2::ZERO);

        h.undo(&mut pw.w.pm()).unwrap();
        let p = path_value(&pw.w, pw.path);
        assert_eq!(p.anchors[0].tan_out, DVec2::new(100.0 / 3.0, 0.0));
        h.undo(&mut pw.w.pm()).unwrap();
        let p = path_value(&pw.w, pw.path);
        assert_eq!(p.anchors[0].tan_out, DVec2::ZERO);
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
                doc: &pw.w.doc,
                scene: &scene,
                comp: pw.w.doc.main,
                selection: &pw.w.selection,
                playhead: Frame(0),
                record: true,
                view: ViewTransform::identity(),
                snap: SnapConfig {
                    grid: None,
                    anchor: false,
                    guide: false,
                },
                modifiers: Modifiers::none(),
                current_paint: &StylePaint::solid(Color::BLACK),
            };
            tool.handle(
                &ctx,
                CanvasEvent::PointerDown {
                    pos: DVec2::new(100.0, 100.0),
                    button: PointerButton::Primary,
                },
            )
        };
        route(&mut pw.w, &mut h, outs);
        // The seed AddKeyframe was applied even though the drag is still open.
        assert!(
            pw.w.doc
                .keyframe_data(pw.path, &PropPath::new("shape.path"), Frame(0))
                .is_some()
        );
    }

    #[test]
    fn shape_tool_uses_current_paint() {
        let mut world = World::new();
        let mut history = History::new();
        let mut tool = ShapeTool::new(ShapeToolKind::Rect);

        let current = StylePaint::solid(Color::rgba(0.1, 0.8, 0.3, 1.0));
        let scene = world.scene();

        let context = |modifiers| ToolContext {
            doc: &world.doc,
            scene: &scene,
            comp: world.doc.main,
            selection: &world.selection,
            playhead: Frame(0),
            record: false,
            view: ViewTransform::identity(),
            snap: SnapConfig {
                grid: None,
                anchor: false,
                guide: false,
            },
            modifiers,
            current_paint: &current,
        };

        let mut outputs = Vec::new();
        outputs.extend(tool.handle(
            &context(Modifiers::none()),
            CanvasEvent::PointerDown {
                pos: DVec2::new(200.0, 200.0),
                button: PointerButton::Primary,
            },
        ));
        outputs.extend(tool.handle(
            &context(Modifiers::none()),
            CanvasEvent::PointerMove {
                pos: DVec2::new(260.0, 260.0),
            },
        ));
        outputs.extend(tool.handle(
            &context(Modifiers::none()),
            CanvasEvent::PointerUp {
                pos: DVec2::new(260.0, 260.0),
                button: PointerButton::Primary,
            },
        ));

        drop(scene);
        apply_all(&mut world, &mut history, outputs);

        let group = world.doc.compositions[world.doc.main].children[0];
        let fill = world.doc.nodes[group]
            .children
            .iter()
            .copied()
            .find(|id| {
                matches!(
                    world.doc.nodes[*id].kind,
                    NodeKind::Style(StyleKind::Fill { .. })
                )
            })
            .unwrap();

        let NodeKind::Style(StyleKind::Fill { paint, .. }) = &world.doc.nodes[fill].kind else {
            panic!("expected fill");
        };

        assert_eq!(paint.base_color(), Color::rgba(0.1, 0.8, 0.3, 1.0));
    }

    #[test]
    fn text_tool_creates_group_with_text_and_fill() {
        let mut world = World::new();
        let mut history = History::new();
        let mut tool = TextTool::default();
        let before = world.doc.compositions[world.doc.main].children.len();

        let scene = world.scene();
        let current = StylePaint::solid(Color::rgba(0.1, 0.8, 0.3, 1.0));
        let outs = {
            let ctx = ToolContext {
                doc: &world.doc,
                scene: &scene,
                comp: world.doc.main,
                selection: &world.selection,
                playhead: Frame(0),
                record: false,
                view: ViewTransform::identity(),
                snap: SnapConfig {
                    grid: None,
                    anchor: false,
                    guide: false,
                },
                modifiers: Modifiers::none(),
                current_paint: &current,
            };
            tool.handle(
                &ctx,
                CanvasEvent::PointerDown {
                    pos: DVec2::new(100.0, 200.0),
                    button: PointerButton::Primary,
                },
            )
        };
        drop(scene);
        assert!(
            outs.iter()
                .any(|o| matches!(o, ToolOutput::SwitchTool(ToolId::Select)))
        );
        apply_all(&mut world, &mut history, outs.into_vec());

        let comp = &world.doc.compositions[world.doc.main];
        assert_eq!(comp.children.len(), before + 1);
        let group = &world.doc.nodes[comp.children[0]];
        assert_eq!(group.children.len(), 2); // text + fill

        let NodeKind::Text(text) = &world.doc.nodes[group.children[0]].kind else {
            panic!("expected text node");
        };
        assert_eq!(text.text, "Text");
        let transform = &world.doc.nodes[group.children[0]].transform;
        assert_eq!(transform.position.base, DVec2::new(100.0, 200.0));

        let fill = group
            .children
            .iter()
            .copied()
            .find(|id| {
                matches!(
                    world.doc.nodes[*id].kind,
                    NodeKind::Style(StyleKind::Fill { .. })
                )
            })
            .unwrap();
        let NodeKind::Style(StyleKind::Fill { paint, .. }) = &world.doc.nodes[fill].kind else {
            panic!("expected fill");
        };
        assert_eq!(paint.base_color(), Color::rgba(0.1, 0.8, 0.3, 1.0));

        history.undo(&mut world.pm()).unwrap();
        assert_eq!(
            world.doc.compositions[world.doc.main].children.len(),
            before
        );
    }
}
