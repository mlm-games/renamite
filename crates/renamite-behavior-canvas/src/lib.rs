//! Canvas tools: Select/Transform (click, drag-move, rotate/scale handles,
//! rubber band, delete) and Rect/Ellipse/Star creation. Pure state machines:
//! world-space events in, EditorCommands out. Behavior reference (clean-room,
//! observed): rotation handle preserves direction and multiple full turns;
//! Ctrl (or Shift) snaps rotation to 15° and Shift constrains shapes to
//! square/circle (Shift on the star tool draws a regular polygon instead;
//! Alt on the star tool draws from center with 6 points instead of 5);
//! drag = one undo step; Esc cancels the open drag.

use glam::DVec2;
use kurbo::{Affine, ParamCurveNearest, Point, Shape as KurboShape};

use renamite_animation::{Angle, Animated, Frame};
use renamite_behavior_common::{
    ToolContext, fill::cmd_fill_shape, path::path_edit_target, snap::SNAP_TOLERANCE_PX,
};
use renamite_geometry::{Anchor, AnchorEdit, TangentMode, VectorPath};
use renamite_history::{
    EditorCommand, NodeTree, OutputVec, SelectionChange, ToolId, ToolOutput, resolve_property_edit,
};
use renamite_model::{
    Document, FillRule, GradientKind, Node, NodeId, NodeKind, PaintKind, Parent, PropPath,
    ShapeKind, StarKind, StyleKind, StylePaint, Value, immediate_child_below, node_is_ancestor,
    node_transform_context, node_world_affine, pick_box_selectable, pick_selectable,
    pick_selectable_with_leaf, selected_ancestor_for_pick, selection_bounds, world_delta_to_parent,
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

        /// Pivot location in world coordinates for one selected node.
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
    /// `stops` is the live edit-state: existing gradients carry their real
    /// stops, fresh drags carry the current-paint stops being seeded.
    GradientLine {
        start: DVec2,
        end: DVec2,
        radial: bool,
        stops: renamite_model::GradientStops,
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

    pub fn is_active(&self, id: ToolId) -> bool {
        match id {
            ToolId::Select | ToolId::Transform => self.select.is_active(),
            _ => self.is_dragging(id),
        }
    }

    pub fn cancel(&mut self, id: ToolId) -> OutputVec {
        match id {
            ToolId::Select | ToolId::Transform => self.select.cancel(),
            ToolId::Rect => self.rect.cancel(),
            ToolId::Ellipse => self.ellipse.cancel(),
            ToolId::Star => self.star.cancel(),
            ToolId::Pen => self.pen.cancel(),
            ToolId::PathEdit => self.path_edit.cancel(),
            ToolId::Gradient => self.gradient.cancel(),
            _ => smallvec![],
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
            [node] => selection_pivot(ctx, *node, min, max),
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
            Some(SelState::DragMove { .. })
                | Some(SelState::RubberBand { .. })
                | Some(SelState::DragRotate { .. })
                | Some(SelState::DragScale { .. })
                | Some(SelState::DragPivot { .. })
        )
    }

    pub fn is_active(&self) -> bool {
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

    pub fn cancel(&mut self) -> OutputVec {
        match std::mem::replace(self.st(), SelState::Idle) {
            SelState::DragMove { txn: true, .. }
            | SelState::DragRotate { txn: true, .. }
            | SelState::DragScale { txn: true, .. }
            | SelState::DragPivot { txn: true, .. } => smallvec![ToolOutput::CancelTransaction],
            _ => smallvec![],
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
            } => self.release(ctx, pos),
            CanvasEvent::KeyDown(Key::Escape) => self.escape(),
            CanvasEvent::KeyDown(Key::Delete) | CanvasEvent::KeyDown(Key::Backspace) => {
                self.delete(ctx)
            }
            CanvasEvent::KeyDown(Key::ArrowLeft) => {
                self.nudge_selection(ctx, DVec2::new(-1.0, 0.0))
            }
            CanvasEvent::KeyDown(Key::ArrowRight) => {
                self.nudge_selection(ctx, DVec2::new(1.0, 0.0))
            }
            CanvasEvent::KeyDown(Key::ArrowUp) => self.nudge_selection(ctx, DVec2::new(0.0, -1.0)),
            CanvasEvent::KeyDown(Key::ArrowDown) => self.nudge_selection(ctx, DVec2::new(0.0, 1.0)),
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
            let display_pivot = selection_pivot(ctx, *node, min, max);
            let pivot_hit = display_pivot.is_some_and(|pivot| (pos - pivot).length() <= tolerance)
                || transform
                    .is_some_and(|context| (pos - context.pivot_world).length() <= tolerance);

            if pivot_hit && let Some(transform) = transform {
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
        match pick_selectable(ctx.doc, ctx.scene, ctx.comp, pos) {
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
                let raw_delta = pos - *last;
                *last = pos;
                if raw_delta.length_squared() == 0.0 {
                    return smallvec![];
                }
                let bounds = selection_bounds(ctx.doc, ctx.scene, &ctx.selection.nodes);
                let delta = apply_canvas_snap_delta(ctx, bounds, raw_delta);
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
                    .filter(|&&node| {
                        !ctx.selection.nodes.iter().any(|&ancestor| {
                            ancestor != node && node_is_ancestor(ctx.doc, ancestor, node)
                        })
                    })
                    .filter_map(|&node| {
                        if ctx.doc.nodes.get(node).is_some_and(|node| node.locked) {
                            return None;
                        }
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
                if ctx.modifiers.shift || ctx.modifiers.ctrl {
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
                let pos = apply_canvas_snap(ctx, pos);
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
                let mut picked = pick_box_selectable(ctx.doc, ctx.scene, ctx.comp, min, max);
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
            .filter_map(|&id| {
                (!ctx.doc.nodes.get(id).is_some_and(|node| node.locked))
                    .then_some(EditorCommand::RemoveNode { id })
            })
            .collect::<SmallVec<[_; 4]>>();
        if cmds.is_empty() {
            return smallvec![];
        }
        smallvec![
            ToolOutput::BeginTransaction("Delete".into()),
            ToolOutput::Commands(cmds),
            ToolOutput::CommitTransaction,
            ToolOutput::RequestSelection(SelectionChange::Set(vec![])),
        ]
    }

    fn nudge_selection(&self, ctx: &ToolContext, dir: DVec2) -> OutputVec {
        if ctx.selection.is_empty() {
            return smallvec![];
        }
        if self.is_dragging() {
            return smallvec![];
        }
        let scale = ctx.view.scale.max(1e-6);
        let step = if ctx.modifiers.alt {
            1.0
        } else if ctx.modifiers.shift {
            20.0
        } else {
            2.0
        } / scale;
        let delta = dir * step;
        if !delta.is_finite() {
            return smallvec![];
        }
        let frame = ctx.playhead.0 as f64;
        let prop = PropPath::new("transform.position");
        let cmds: SmallVec<[EditorCommand; 4]> = ctx
            .selection
            .nodes
            .iter()
            .copied()
            .filter(|&id| {
                !ctx.selection
                    .nodes
                    .iter()
                    .any(|&anc| anc != id && node_is_ancestor(ctx.doc, anc, id))
            })
            .filter_map(|id| {
                let node = ctx.doc.nodes.get(id)?;
                if node.locked {
                    return None;
                }
                let Ok(Value::DVec2(current)) = ctx.doc.value_at(id, &prop, frame) else {
                    return None;
                };
                let local = world_delta_to_parent(ctx.doc, id, frame, delta).unwrap_or(delta);
                if !local.is_finite() {
                    return None;
                }
                Some(resolve_property_edit(
                    ctx.doc,
                    id,
                    &prop,
                    Value::DVec2(current + local),
                    ctx.playhead,
                    ctx.record,
                ))
            })
            .collect();
        if cmds.is_empty() {
            return smallvec![];
        }
        smallvec![
            ToolOutput::BeginTransaction("Nudge".into()),
            ToolOutput::Commands(cmds),
            ToolOutput::CommitTransaction,
        ]
    }

    fn double_click(&mut self, ctx: &ToolContext, pos: DVec2) -> OutputVec {
        let Some((outer, leaf)) = pick_selectable_with_leaf(ctx.doc, ctx.scene, ctx.comp, pos)
        else {
            return smallvec![];
        };

        let target = match ctx.selection.nodes.as_slice() {
            [selected] if *selected == outer => {
                immediate_child_below(ctx.doc, *selected, leaf).unwrap_or(leaf)
            }
            [selected] if node_is_ancestor(ctx.doc, *selected, leaf) => {
                immediate_child_below(ctx.doc, *selected, leaf).unwrap_or(leaf)
            }

            _ => outer,
        };

        smallvec![ToolOutput::RequestSelection(SelectionChange::Set(vec![
            target
        ]),)]
    }
}

fn handles(ctx: &ToolContext, min: DVec2, max: DVec2) -> (DVec2, DVec2) {
    let cx = (min.x + max.x) * 0.5;
    let rot = DVec2::new(cx, min.y - ctx.view.world_tolerance(ROTATE_OFFSET_PX));
    (rot, max)
}

fn selection_pivot(ctx: &ToolContext, node: NodeId, min: DVec2, max: DVec2) -> Option<DVec2> {
    let center = (min + max) * 0.5;
    let Some(transform) = node_transform_context(ctx.doc, node, ctx.playhead.0 as f64) else {
        return Some(center);
    };
    let pivot = transform.pivot_world;
    let inside = pivot.x >= min.x && pivot.x <= max.x && pivot.y >= min.y && pivot.y <= max.y;
    if inside { Some(pivot) } else { Some(center) }
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

fn gradient_overlay_stops(
    ctx: &ToolContext,
    style: NodeId,
    frame: f64,
) -> renamite_model::GradientStops {
    if let Some(StylePaint::Gradient(g)) = ctx.doc.nodes.get(style).and_then(|n| match &n.kind {
        NodeKind::Style(st) => Some(st.paint().clone()),
        _ => None,
    }) {
        return g.stops.value_at(frame);
    }
    match ctx.current_paint {
        StylePaint::Gradient(g) => g.stops.value_at(frame),
        StylePaint::Solid { .. } => renamite_model::GradientStops(vec![
            renamite_model::GradientStop {
                offset: 0.0,
                color: ctx.current_paint.base_color(),
            },
            renamite_model::GradientStop {
                offset: 1.0,
                color: ctx.current_paint.base_color(),
            },
        ]),
    }
}

impl GradientTool {
    pub fn is_dragging(&self) -> bool {
        matches!(self.state, GradState::Drag { dragging: true, .. })
    }

    pub fn cancel(&mut self) -> OutputVec {
        match std::mem::replace(&mut self.state, GradState::Idle) {
            GradState::Drag { txn: true, .. } => smallvec![ToolOutput::CancelTransaction],
            _ => smallvec![],
        }
    }

    pub fn overlay(&self, ctx: &ToolContext) -> ToolOverlay {
        let GradState::Drag {
            l2w,
            start,
            end,
            kind,
            style,
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
            stops: gradient_overlay_stops(ctx, *style, ctx.playhead.0 as f64),
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
        let Some((outer, shape)) = pick_selectable_with_leaf(ctx.doc, ctx.scene, ctx.comp, pos)
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
        let Some(l2w) = node_world_affine(ctx.doc, shape, ctx.playhead.0 as f64) else {
            return smallvec![];
        };
        let w2l = l2w.inverse();
        if !w2l.as_coeffs().iter().all(|value| value.is_finite()) {
            return smallvec![];
        }
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
            outer
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
        let snapped = apply_canvas_snap(ctx, pos);
        let local = {
            let p = *w2l * Point::new(snapped.x, snapped.y);
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
                let Some((_, shape)) = pick_selectable_with_leaf(ctx.doc, ctx.scene, ctx.comp, pos)
                else {
                    return smallvec![];
                };

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
            .find(|it| it.opacity > 0.0 && paint_covers(ctx.scene, it, pos))
            .cloned()
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
fn paint_covers(
    scene: &renamite_model::Scene,
    item: &renamite_model::SceneItem,
    pos: DVec2,
) -> bool {
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
    if !item.clips.iter().all(|&index| {
        let Some(clip) = scene.clips.get(index as usize) else {
            return false;
        };
        match clip.rule {
            FillRule::NonZero => clip.path.winding(q) != 0,
            FillRule::EvenOdd => clip.path.winding(q) % 2 != 0,
        }
    }) {
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

fn centered_group(name: impl Into<String>, center: DVec2) -> Node {
    let mut group = Node::new(name, NodeKind::Group);
    group.transform.position = Animated::new(center);
    group.transform.anchor = Animated::new(center);
    group
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
        let pos = apply_canvas_snap(ctx, pos);
        let mut text_node = Node::new(
            "Text",
            NodeKind::Text(renamite_model::TextNode {
                text: "Text".into(),
                size: Animated::new(48.0),
                align: renamite_model::TextAlign::Left,
                font: None,
                tracking: Animated::new(0.0),
                leading: Animated::new(0.0),
            }),
        );
        // Place the baseline at the click point via the node's own transform.
        text_node.transform.position = Animated::new(pos);
        let tree = NodeTree::with_children(
            centered_group("Text", pos),
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

    pub fn cancel(&mut self) -> OutputVec {
        self.drag = None;
        smallvec![]
    }

    pub fn handle(&mut self, ctx: &ToolContext, ev: CanvasEvent) -> OutputVec {
        match ev {
            CanvasEvent::PointerDown {
                pos,
                button: PointerButton::Primary,
            } => {
                let pos = apply_canvas_snap(ctx, pos);
                self.drag = Some((pos, pos));
                smallvec![ToolOutput::Invalidate]
            }
            CanvasEvent::PointerMove { pos } => {
                if let Some((_, c)) = &mut self.drag {
                    *c = apply_canvas_snap(ctx, pos);
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
                let pos = apply_canvas_snap(ctx, pos);
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
                    centered_group(name, center),
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

fn apply_canvas_snap(ctx: &ToolContext, raw: DVec2) -> DVec2 {
    use renamite_behavior_common::snap::{SnapInput, snap_point};
    let input = SnapInput {
        doc: ctx.doc,
        items: &ctx.scene.items,
        selected: &ctx.selection.nodes,
    };
    snap_point(
        &ctx.snap,
        &input,
        ctx.guides,
        raw,
        ctx.view.world_tolerance(SNAP_TOLERANCE_PX),
        None,
    )
}

fn apply_canvas_snap_delta(
    ctx: &ToolContext,
    bounds: Option<(DVec2, DVec2)>,
    delta: DVec2,
) -> DVec2 {
    use renamite_behavior_common::snap::{SnapInput, snap_delta};
    let input = SnapInput {
        doc: ctx.doc,
        items: &ctx.scene.items,
        selected: &ctx.selection.nodes,
    };
    snap_delta(
        &ctx.snap,
        &input,
        ctx.guides,
        bounds,
        delta,
        ctx.view.world_tolerance(SNAP_TOLERANCE_PX),
        None,
    )
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

    pub fn cancel(&mut self) -> OutputVec {
        if matches!(
            self.state,
            PenState::Building { .. } | PenState::DraggingTangent { .. }
        ) {
            self.state = PenState::Idle;
        }
        smallvec![]
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
            CanvasEvent::PointerMove { pos } => self.moved(ctx, pos),
            CanvasEvent::PointerUp {
                pos,
                button: PointerButton::Primary,
            } => self.release(ctx, pos),
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
        let pos = apply_canvas_snap(ctx, pos);
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

    fn moved(&mut self, ctx: &ToolContext, pos: DVec2) -> OutputVec {
        let pos = apply_canvas_snap(ctx, pos);
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

    fn release(&mut self, ctx: &ToolContext, pos: DVec2) -> OutputVec {
        let pos = apply_canvas_snap(ctx, pos);
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
        if node.locked {
            return None;
        }

        match &node.kind {
            NodeKind::Shape(ShapeKind::Path(_) | ShapeKind::CompoundPath(_)) => Some(sel),
            NodeKind::Group | NodeKind::Layer(_) => {
                let mut path_children = node.children.iter().copied().filter(|id| {
                    ctx.doc.nodes.get(*id).is_some_and(|child| {
                        !child.locked
                            && matches!(
                                &child.kind,
                                NodeKind::Shape(ShapeKind::Path(_))
                                    | NodeKind::Shape(ShapeKind::CompoundPath(_))
                            )
                    })
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
    /// one-contour list. `CompoundPath` exposes every contour at the frame.
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

    pub fn cancel(&mut self) -> OutputVec {
        match std::mem::replace(&mut self.state, PathEditState::Idle) {
            PathEditState::DragAnchor { txn: true, .. }
            | PathEditState::DragTanIn { txn: true, .. }
            | PathEditState::DragTanOut { txn: true, .. } => {
                smallvec![ToolOutput::CancelTransaction]
            }
            _ => smallvec![],
        }
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
                                mode: new_mode,
                                tan_in: None,
                                tan_out: None,
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
                let pos = apply_canvas_snap(ctx, pos);
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
        let pos = apply_canvas_snap(ctx, pos);
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
    /// default = 2px (screen px, i.e. divided by zoom).
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
            20.0 / ctx.view.scale
        } else {
            2.0 / ctx.view.scale
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
                edits: vec![AnchorEdit::SetMode {
                    index,
                    mode,
                    tan_in: None,
                    tan_out: None,
                }],
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
            let mut end = rotated[0];
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
    /// Opposite ends of one open contour close it. Endpoints of different
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
