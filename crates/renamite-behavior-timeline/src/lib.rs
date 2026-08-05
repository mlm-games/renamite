//! Timeline behaviors: keyframe drag / box-select / Alt-easing-cycle, ruler
//! scrub, and easing-curve handle editing. Pure state machines: px-space
//! events in, `EditorCommand`s out. Rows and px mapping are supplied by the
//! host as data (`TimelineLayout` + `&[TimelineRow]`), so the same code runs
//! under Repose panels and JSON fixtures.

use glam::DVec2;
use renamite_animation::{EasingHandle, EasingPreset, Frame, Interpolation};
use renamite_behavior_common::Modifiers;
use renamite_history::{ClipKeyMove, EditorCommand, KeyframeMove, OutputVec, ToolOutput};
use renamite_machine::ClipMap;
use renamite_model::{Document, KeyframeData, NodeId, PropPath, Value};
use serde::{Deserialize, Serialize};
use smallvec::smallvec;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimelineTarget {
    Doc,
    Clip(renamite_machine::ClipId),
}

/// One keyframe, addressed structurally (survives re-render, not undo - the
/// host clears/refreshes selection after undo/redo).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyRef {
    pub node: NodeId,
    pub prop: PropPath,
    pub frame: Frame,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimelineRow {
    pub node: NodeId,
    pub prop: PropPath,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct TimelineLayout {
    pub origin_x: f64,       // px of frame 0
    pub px_per_frame: f64,   // > 0
    pub row_top: f64,        // px of first row's top edge
    pub row_height: f64,     // > 0
    pub key_tolerance_px: f64,
}

impl TimelineLayout {
    pub fn frame_to_x(&self, frame: f64) -> f64 {
        self.origin_x + frame * self.px_per_frame
    }
    pub fn x_to_frame(&self, x: f64) -> f64 {
        (x - self.origin_x) / self.px_per_frame
    }
    pub fn y_to_row(&self, y: f64) -> Option<usize> {
        let r = (y - self.row_top) / self.row_height;
        (r >= 0.0).then_some(r as usize)
    }
    pub fn row_center_y(&self, row: usize) -> f64 {
        self.row_top + (row as f64 + 0.5) * self.row_height
    }
}

pub struct TimelineCtx<'a> {
    pub doc: &'a Document,
    pub clips: &'a ClipMap,
    pub target: TimelineTarget,
    pub rows: &'a [TimelineRow],
    pub layout: TimelineLayout,
    pub range: (Frame, Frame),
    pub playhead: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TimelineEvent {
    Press { pos: DVec2, modifiers: Modifiers },
    Move { pos: DVec2, modifiers: Modifiers },
    Release { pos: DVec2, modifiers: Modifiers },
    DoubleClick { pos: DVec2, modifiers: Modifiers },
    KeyDown(TimelineKey),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimelineKey {
    Delete,
    Escape,
}

/// Screen-space overlay for the host to draw.
#[derive(Clone, Debug, PartialEq)]
pub enum TimelineOverlay {
    None,
    BoxSelect { min: DVec2, max: DVec2 },
    DragDelta { frames: i64 },
}

fn row_key_frames(ctx: &TimelineCtx, row: &TimelineRow) -> Vec<Frame> {
    match ctx.target {
        TimelineTarget::Doc => ctx.doc.key_frames(row.node, &row.prop),
        TimelineTarget::Clip(cid) => ctx
            .clips
            .get(cid)
            .and_then(|c| c.tracks.iter().find(|t| t.node == row.node && t.prop == row.prop))
            .map(|t| t.keys.iter().map(|k| k.frame).collect())
            .unwrap_or_default(),
    }
}

fn key_data(ctx: &TimelineCtx, r: &KeyRef) -> Option<KeyframeData> {
    match ctx.target {
        TimelineTarget::Doc => ctx.doc.keyframe_data(r.node, &r.prop, r.frame),
        TimelineTarget::Clip(cid) => {
            let c = ctx.clips.get(cid)?;
            let t = c.tracks.iter().find(|t| t.node == r.node && t.prop == r.prop)?;
            let i = t.keys.binary_search_by_key(&r.frame, |k| k.frame).ok()?;
            Some(t.keys[i].clone())
        }
    }
}

fn hit_key(ctx: &TimelineCtx, pos: DVec2) -> Option<KeyRef> {
    let row_i = ctx.layout.y_to_row(pos.y)?;
    let row = ctx.rows.get(row_i)?;
    let tol = ctx.layout.key_tolerance_px;
    row_key_frames(ctx, row)
        .into_iter()
        .map(|f| (f, (ctx.layout.frame_to_x(f.0 as f64) - pos.x).abs()))
        .filter(|(_, d)| *d <= tol)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(frame, _)| KeyRef { node: row.node, prop: row.prop.clone(), frame })
}

enum KeyState {
    Idle,
    /// Pressed on a key; may become a drag or resolve as a click on release.
    Pending { press: DVec2, key: KeyRef },
    /// Dragging the selection. `origins` are the frames at drag start;
    /// `delta` is the total applied so far. Incremental moves are emitted per
    /// change; `txn` marks whether BeginTransaction has been emitted.
    Dragging { press: DVec2, origins: Vec<KeyRef>, delta: i64, txn: bool },
    BoxSelect { start: DVec2, current: DVec2 },
}

pub struct TimelineKeyframeBehavior {
    state: KeyState,
    selected: Vec<KeyRef>,
}

const DRAG_THRESHOLD_PX: f64 = 3.0;

impl Default for TimelineKeyframeBehavior {
    fn default() -> Self {
        Self { state: KeyState::Idle, selected: Vec::new() }
    }
}

impl TimelineKeyframeBehavior {
    pub fn selected(&self) -> &[KeyRef] {
        &self.selected
    }

    /// Host calls this after undo/redo/doc-mutation so selection never dangles.
    pub fn retain_valid(&mut self, ctx: &TimelineCtx) {
        self.selected.retain(|r| key_data(ctx, r).is_some());
    }

    pub fn overlay(&self) -> TimelineOverlay {
        match &self.state {
            KeyState::BoxSelect { start, current } => TimelineOverlay::BoxSelect {
                min: start.min(*current),
                max: start.max(*current),
            },
            KeyState::Dragging { delta, .. } if *delta != 0 => {
                TimelineOverlay::DragDelta { frames: *delta }
            }
            _ => TimelineOverlay::None,
        }
    }

    pub fn handle(&mut self, ctx: &TimelineCtx, ev: TimelineEvent) -> OutputVec {
        match ev {
            TimelineEvent::Press { pos, modifiers } => self.on_press(ctx, pos, modifiers),
            TimelineEvent::Move { pos, .. } => self.on_move(ctx, pos),
            TimelineEvent::Release { pos, modifiers } => self.on_release(ctx, pos, modifiers),
            TimelineEvent::DoubleClick { pos, .. } => self.on_double_click(ctx, pos),
            TimelineEvent::KeyDown(k) => self.on_key(ctx, k),
        }
    }

    fn on_press(&mut self, ctx: &TimelineCtx, pos: DVec2, m: Modifiers) -> OutputVec {
        match hit_key(ctx, pos) {
            Some(key) => {
                if m.alt {
                    // Alt+click: cycle easing of THIS key; selection untouched.
                    return match cycle_easing_cmd(ctx, &key) {
                        Some(cmd) => smallvec![
                            ToolOutput::BeginTransaction("Cycle easing".into()),
                            ToolOutput::Commands(smallvec![cmd]),
                            ToolOutput::CommitTransaction,
                        ],
                        None => smallvec![],
                    };
                }
                if m.ctrl {
                    // Toggle now; drag only if it ended up selected.
                    if let Some(i) = self.selected.iter().position(|r| *r == key) {
                        self.selected.remove(i);
                        self.state = KeyState::Idle;
                    } else {
                        self.selected.push(key.clone());
                        self.state = KeyState::Pending { press: pos, key };
                    }
                } else if m.shift {
                    if !self.selected.contains(&key) {
                        self.selected.push(key.clone());
                    }
                    self.state = KeyState::Pending { press: pos, key };
                } else {
                    if !self.selected.contains(&key) {
                        self.selected = vec![key.clone()];
                    }
                    self.state = KeyState::Pending { press: pos, key };
                }
                smallvec![]
            }
            None => {
                if !m.shift && !m.ctrl {
                    self.selected.clear();
                }
                self.state = KeyState::BoxSelect { start: pos, current: pos };
                smallvec![]
            }
        }
    }

    fn on_move(&mut self, ctx: &TimelineCtx, pos: DVec2) -> OutputVec {
        match &mut self.state {
            KeyState::Pending { press, .. } => {
                if (pos - *press).length() >= DRAG_THRESHOLD_PX {
                    let origins = self.selected.clone();
                    self.state = KeyState::Dragging { press: *press, origins, delta: 0, txn: false };
                    // fall through into drag handling on the same event
                    return self.on_move(ctx, pos);
                }
                smallvec![]
            }
            KeyState::Dragging { press, origins, delta, txn } => {
                let raw = ((pos.x - press.x) / ctx.layout.px_per_frame).round() as i64;
                let clamped = clamp_delta(ctx, origins, raw);
                let want = snap_valid_delta(ctx, origins, clamped);
                if want == *delta {
                    return smallvec![]; // stick at last valid delta
                }
                let mut out: OutputVec = smallvec![];
                if !*txn {
                    out.push(ToolOutput::BeginTransaction("Move keyframes".into()));
                    *txn = true;
                }
                out.push(ToolOutput::Commands(smallvec![move_cmd(ctx, origins, *delta, want)]));
                *delta = want;
                out
            }
            KeyState::BoxSelect { current, .. } => {
                *current = pos;
                smallvec![]
            }
            KeyState::Idle => smallvec![],
        }
    }

    fn on_release(&mut self, ctx: &TimelineCtx, _pos: DVec2, m: Modifiers) -> OutputVec {
        let state = std::mem::replace(&mut self.state, KeyState::Idle);
        match state {
            KeyState::Dragging { origins, delta, txn, .. } => {
                // Selection follows the keys to their new frames.
                if delta != 0 {
                    self.selected = origins
                        .iter()
                        .map(|r| KeyRef {
                            node: r.node,
                            prop: r.prop.clone(),
                            frame: Frame(r.frame.0 + delta),
                        })
                        .collect();
                }
                if txn {
                    smallvec![ToolOutput::CommitTransaction]
                } else {
                    smallvec![]
                }
            }
            KeyState::Pending { key, .. } => {
                // Plain click on an already-selected key collapses selection to it.
                if !m.ctrl && !m.shift {
                    self.selected = vec![key];
                }
                smallvec![]
            }
            KeyState::BoxSelect { start, current } => {
                let (min, max) = (start.min(current), start.max(current));
                let mut picked = box_pick(ctx, min, max);
                if m.shift || m.ctrl {
                    for r in picked.drain(..) {
                        if !self.selected.contains(&r) {
                            self.selected.push(r);
                        }
                    }
                } else {
                    self.selected = picked;
                }
                smallvec![]
            }
            KeyState::Idle => smallvec![],
        }
    }

    fn on_double_click(&mut self, ctx: &TimelineCtx, pos: DVec2) -> OutputVec {
        if hit_key(ctx, pos).is_some() {
            return smallvec![]; // double-click on a key: reserved (curve editor focus)
        }
        let Some(row_i) = ctx.layout.y_to_row(pos.y) else { return smallvec![] };
        let Some(row) = ctx.rows.get(row_i) else { return smallvec![] };
        let frame = Frame(
            ctx.layout
                .x_to_frame(pos.x)
                .round()
                .clamp(ctx.range.0 .0 as f64, ctx.range.1 .0 as f64) as i64,
        );
        let Some(cmd) = add_key_cmd(ctx, row, frame) else { return smallvec![] };
        self.selected = vec![KeyRef { node: row.node, prop: row.prop.clone(), frame }];
        smallvec![
            ToolOutput::BeginTransaction("Add keyframe".into()),
            ToolOutput::Commands(smallvec![cmd]),
            ToolOutput::CommitTransaction,
        ]
    }

    fn on_key(&mut self, ctx: &TimelineCtx, k: TimelineKey) -> OutputVec {
        match k {
            TimelineKey::Escape => match std::mem::replace(&mut self.state, KeyState::Idle) {
                KeyState::Dragging { txn: true, .. } => smallvec![ToolOutput::CancelTransaction],
                _ => smallvec![],
            },
            TimelineKey::Delete => {
                if self.selected.is_empty() {
                    return smallvec![];
                }
                let cmds: smallvec::SmallVec<[EditorCommand; 4]> = self
                    .selected
                    .drain(..)
                    .map(|r| match ctx.target {
                        TimelineTarget::Doc => EditorCommand::RemoveKeyframe {
                            id: r.node,
                            prop: r.prop,
                            frame: r.frame,
                        },
                        TimelineTarget::Clip(cid) => EditorCommand::RemoveClipKey {
                            clip: cid,
                            node: r.node,
                            prop: r.prop,
                            frame: r.frame,
                        },
                    })
                    .collect();
                smallvec![
                    ToolOutput::BeginTransaction("Delete keyframes".into()),
                    ToolOutput::Commands(cmds),
                    ToolOutput::CommitTransaction,
                ]
            }
        }
    }
}

/// Clamp so no key leaves the composition range.
fn clamp_delta(ctx: &TimelineCtx, origins: &[KeyRef], want: i64) -> i64 {
    let mut lo = i64::MIN;
    let mut hi = i64::MAX;
    for r in origins {
        lo = lo.max(ctx.range.0 .0 - r.frame.0);
        hi = hi.min(ctx.range.1 .0 - r.frame.0);
    }
    want.clamp(lo, hi)
}

/// Valid iff no destination collides with a STATIONARY key (uniform delta
/// preserves intra-set distinctness, so only stationary keys can collide).
fn delta_valid(ctx: &TimelineCtx, origins: &[KeyRef], delta: i64) -> bool {
    use std::collections::HashSet;
    let moving: HashSet<&KeyRef> = origins.iter().collect();
    for r in origins {
        let dest = Frame(r.frame.0 + delta);
        let row = TimelineRow { node: r.node, prop: r.prop.clone() };
        for f in row_key_frames(ctx, &row) {
            let candidate = KeyRef { node: r.node, prop: r.prop.clone(), frame: f };
            if f == dest && !moving.contains(&candidate) {
                return false;
            }
        }
    }
    true
}

/// Walk from `want` toward 0 until a valid delta is found, so a drag that
/// would collide with a stationary key "sticks" at the last valid offset
/// (standard editor feel) instead of emitting a conflicting command.
fn snap_valid_delta(ctx: &TimelineCtx, origins: &[KeyRef], want: i64) -> i64 {
    if delta_valid(ctx, origins, want) {
        return want;
    }
    let step = if want > 0 { -1 } else { 1 };
    let mut d = want;
    while d != 0 {
        d += step;
        if delta_valid(ctx, origins, d) {
            return d;
        }
    }
    0
}

/// One incremental move command: (origin + applied) → (origin + want).
fn move_cmd(ctx: &TimelineCtx, origins: &[KeyRef], applied: i64, want: i64) -> EditorCommand {
    match ctx.target {
        TimelineTarget::Doc => EditorCommand::MoveKeyframes {
            moves: origins
                .iter()
                .map(|r| KeyframeMove {
                    id: r.node,
                    prop: r.prop.clone(),
                    from: Frame(r.frame.0 + applied),
                    to: Frame(r.frame.0 + want),
                })
                .collect(),
        },
        TimelineTarget::Clip(cid) => EditorCommand::MoveClipKeys {
            moves: origins
                .iter()
                .map(|r| ClipKeyMove {
                    clip: cid,
                    node: r.node,
                    prop: r.prop.clone(),
                    from: Frame(r.frame.0 + applied),
                    to: Frame(r.frame.0 + want),
                })
                .collect(),
        },
    }
}

/// Alt+click: detect current preset (custom curves count as Linear so the
/// cycle always advances), emit the next one.
fn cycle_easing_cmd(ctx: &TimelineCtx, r: &KeyRef) -> Option<EditorCommand> {
    let k = key_data(ctx, r)?;
    let cur = EasingPreset::detect(k.interpolation, k.ease_out, k.ease_in)
        .unwrap_or(EasingPreset::Linear);
    let (interpolation, ease_out, ease_in) = cur.next().segment();
    Some(match ctx.target {
        TimelineTarget::Doc => EditorCommand::SetEasing {
            id: r.node,
            prop: r.prop.clone(),
            frame: r.frame,
            interpolation,
            ease_out,
            ease_in,
        },
        TimelineTarget::Clip(cid) => EditorCommand::AddClipKey {
            clip: cid,
            node: r.node,
            prop: r.prop.clone(),
            key: KeyframeData { interpolation, ease_out, ease_in, ..k },
        },
    })
}

/// Double-click value sourcing: Doc samples the evaluated property at the
/// frame (so the new key is visually a no-op). Clip samples the track's
/// nearest key, falling back to the document value.
fn add_key_cmd(ctx: &TimelineCtx, row: &TimelineRow, frame: Frame) -> Option<EditorCommand> {
    match ctx.target {
        TimelineTarget::Doc => {
            let value = ctx.doc.value_at(row.node, &row.prop, frame.0 as f64).ok()?;
            Some(EditorCommand::AddKeyframe { id: row.node, prop: row.prop.clone(), frame, value })
        }
        TimelineTarget::Clip(cid) => {
            let value = nearest_clip_value(ctx, cid, row, frame)
                .or_else(|| ctx.doc.value_at(row.node, &row.prop, frame.0 as f64).ok())?;
            let (interpolation, ease_out, ease_in) = EasingPreset::Linear.segment();
            Some(EditorCommand::AddClipKey {
                clip: cid,
                node: row.node,
                prop: row.prop.clone(),
                key: KeyframeData { frame, value, interpolation, ease_out, ease_in },
            })
        }
    }
}

fn nearest_clip_value(
    ctx: &TimelineCtx,
    cid: renamite_machine::ClipId,
    row: &TimelineRow,
    frame: Frame,
) -> Option<Value> {
    let c = ctx.clips.get(cid)?;
    let t = c.tracks.iter().find(|t| t.node == row.node && t.prop == row.prop)?;
    t.keys
        .iter()
        .min_by_key(|k| (k.frame.0 - frame.0).abs())
        .map(|k| k.value.clone())
}

fn box_pick(ctx: &TimelineCtx, min: DVec2, max: DVec2) -> Vec<KeyRef> {
    let mut out = Vec::new();
    for (i, row) in ctx.rows.iter().enumerate() {
        let y = ctx.layout.row_center_y(i);
        if y < min.y || y > max.y {
            continue;
        }
        for f in row_key_frames(ctx, row) {
            let x = ctx.layout.frame_to_x(f.0 as f64);
            if x >= min.x && x <= max.x {
                out.push(KeyRef { node: row.node, prop: row.prop.clone(), frame: f });
            }
        }
    }
    out
}

#[derive(Default)]
pub struct TimelineScrubBehavior {
    dragging: bool,
}

impl TimelineScrubBehavior {
    pub fn handle(&mut self, ctx: &TimelineCtx, ev: TimelineEvent) -> OutputVec {
        let set = |x: f64| -> OutputVec {
            let f = ctx
                .layout
                .x_to_frame(x)
                .round()
                .clamp(ctx.range.0 .0 as f64, ctx.range.1 .0 as f64);
            smallvec![ToolOutput::SetPlayhead(f)]
        };
        match ev {
            TimelineEvent::Press { pos, .. } => {
                self.dragging = true;
                set(pos.x)
            }
            TimelineEvent::Move { pos, .. } if self.dragging => set(pos.x),
            TimelineEvent::Release { .. } => {
                self.dragging = false;
                smallvec![]
            }
            _ => smallvec![],
        }
    }
}

/// Maps the unit easing square (x∈[0,1] left→right, y∈[0,1] bottom→top) to px.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct CurveLayout {
    pub origin: DVec2, // px of (0,0) - bottom-left
    pub size: DVec2,   // px extents (y grows DOWN in screen space)
    pub handle_tolerance_px: f64,
}

impl CurveLayout {
    pub fn to_px(&self, x: f64, y: f64) -> DVec2 {
        DVec2::new(self.origin.x + x * self.size.x, self.origin.y - y * self.size.y)
    }
    pub fn to_unit(&self, p: DVec2) -> (f64, f64) {
        ((p.x - self.origin.x) / self.size.x, (self.origin.y - p.y) / self.size.y)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurveHandle {
    Out, // left key's outgoing
    In,  // segment's incoming (stored on the left key)
}

enum CurveState {
    Idle,
    Drag { handle: CurveHandle, txn: bool },
}

pub struct EasingCurveBehavior {
    /// The SEGMENT being edited = the left keyframe of the pair.
    pub segment: Option<KeyRef>,
    pub layout: CurveLayout,
    state: CurveState,
}

impl EasingCurveBehavior {
    pub fn new(layout: CurveLayout) -> Self {
        Self { segment: None, layout, state: CurveState::Idle }
    }

    pub fn handle(&mut self, ctx: &TimelineCtx, ev: TimelineEvent) -> OutputVec {
        let Some(seg) = self.segment.clone() else { return smallvec![] };
        let Some(k) = key_data(ctx, &seg) else { return smallvec![] };

        match ev {
            TimelineEvent::Press { pos, .. } => {
                let d_out = (self.layout.to_px(k.ease_out.x, k.ease_out.y) - pos).length();
                let d_in = (self.layout.to_px(k.ease_in.x, k.ease_in.y) - pos).length();
                let tol = self.layout.handle_tolerance_px;
                let handle = if d_out <= d_in && d_out <= tol {
                    Some(CurveHandle::Out)
                } else if d_in <= tol {
                    Some(CurveHandle::In)
                } else {
                    None
                };
                if let Some(handle) = handle {
                    self.state = CurveState::Drag { handle, txn: false };
                }
                smallvec![]
            }
            TimelineEvent::Move { pos, .. } => {
                let CurveState::Drag { handle, txn } = &mut self.state else {
                    return smallvec![];
                };
                let (x, y) = self.layout.to_unit(pos);
                let h = EasingHandle { x: x.clamp(0.0, 1.0), y }; // x clamped, y free (anticipate/overshoot)
                let (ease_out, ease_in) = match handle {
                    CurveHandle::Out => (h, k.ease_in),
                    CurveHandle::In => (k.ease_out, h),
                };
                let cmd = set_easing_cmd(ctx, &seg, &k, Interpolation::CubicBezier, ease_out, ease_in);
                let mut out: OutputVec = smallvec![];
                if !*txn {
                    out.push(ToolOutput::BeginTransaction("Edit easing".into()));
                    *txn = true;
                }
                out.push(ToolOutput::Commands(smallvec![cmd]));
                out
            }
            TimelineEvent::Release { .. } => {
                let committed = matches!(self.state, CurveState::Drag { txn: true, .. });
                self.state = CurveState::Idle;
                if committed {
                    smallvec![ToolOutput::CommitTransaction]
                } else {
                    smallvec![]
                }
            }
            TimelineEvent::KeyDown(TimelineKey::Escape) => {
                let cancel = matches!(self.state, CurveState::Drag { txn: true, .. });
                self.state = CurveState::Idle;
                if cancel {
                    smallvec![ToolOutput::CancelTransaction]
                } else {
                    smallvec![]
                }
            }
            _ => smallvec![],
        }
    }
}

fn set_easing_cmd(
    ctx: &TimelineCtx,
    r: &KeyRef,
    k: &KeyframeData,
    interpolation: Interpolation,
    ease_out: EasingHandle,
    ease_in: EasingHandle,
) -> EditorCommand {
    match ctx.target {
        TimelineTarget::Doc => EditorCommand::SetEasing {
            id: r.node,
            prop: r.prop.clone(),
            frame: r.frame,
            interpolation,
            ease_out,
            ease_in,
        },
        TimelineTarget::Clip(cid) => EditorCommand::AddClipKey {
            clip: cid,
            node: r.node,
            prop: r.prop.clone(),
            key: KeyframeData { interpolation, ease_out, ease_in, ..k.clone() },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use renamite_history::{History, ProjectMut};
    use renamite_machine::{Clip, ClipId, ClipMap, MachineId, MachineMap, Track};
    use renamite_model::{Node, NodeKind, Parent};

    const LAYOUT: TimelineLayout = TimelineLayout {
        origin_x: 0.0,
        px_per_frame: 10.0,
        row_top: 0.0,
        row_height: 20.0,
        key_tolerance_px: 5.0,
    };

    fn f64_key(frame: i64, v: f64) -> KeyframeData {
        KeyframeData {
            frame: Frame(frame),
            value: Value::F64(v),
            interpolation: Interpolation::Linear,
            ease_out: EasingHandle::LINEAR_OUT,
            ease_in: EasingHandle::LINEAR_IN,
        }
    }

    struct World {
        doc: Document,
        clips: ClipMap,
        clip_order: Vec<ClipId>,
        machines: MachineMap,
        machine_order: Vec<MachineId>,
        start: Option<MachineId>,
    }

    impl World {
        fn new() -> Self {
            Self {
                doc: Document::empty(),
                clips: ClipMap::default(),
                clip_order: vec![],
                machines: MachineMap::default(),
                machine_order: vec![],
                start: None,
            }
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
        fn node(&mut self) -> NodeId {
            let id = self.doc.create_node(Node::new("box", NodeKind::Group));
            self.doc.attach(id, Parent::Comp(self.doc.main), 0).unwrap();
            id
        }
        fn doc_key(&mut self, node: NodeId, frame: i64, v: f64) {
            self.doc.add_keyframe(node, &PropPath::new("opacity"), Frame(frame), &Value::F64(v))
                .unwrap();
        }
        fn clip(&mut self, node: NodeId, keys: Vec<(i64, f64)>) -> ClipId {
            let cid = self.clips.insert(Clip {
                name: "c".into(),
                range: (Frame(0), Frame(60)),
                tracks: vec![Track {
                    node,
                    prop: PropPath::new("opacity"),
                    keys: keys.iter().map(|(f, v)| f64_key(*f, *v)).collect(),
                }],
                events: vec![],
            });
            self.clip_order.push(cid);
            cid
        }
    }

    /// Route `OutputVec` through a real History so emitted commands must apply.
    struct Harness {
        w: World,
        h: History,
        applied: usize,
    }
    impl Harness {
        fn new() -> Self {
            Self { w: World::new(), h: History::new(), applied: 0 }
        }
        fn run(&mut self, b: &mut TimelineKeyframeBehavior, target: TimelineTarget, ev: TimelineEvent) {
            let ctx = ctx_for(&self.w, target);
            self.route(b.handle(&ctx, ev));
        }
        fn route(&mut self, outputs: OutputVec) {
            for out in outputs {
                match out {
                    ToolOutput::BeginTransaction(l) => self.h.begin(l),
                    ToolOutput::CommitTransaction => {
                        self.h.commit();
                        self.applied += 1;
                    }
                    ToolOutput::CancelTransaction => self.h.cancel(&mut self.w.pm()).unwrap(),
                    ToolOutput::Commands(cmds) => {
                        for c in cmds {
                            self.h.apply(&mut self.w.pm(), c).expect("command applies");
                        }
                    }
                    _ => {}
                }
            }
        }
        fn frames(&self, node: NodeId) -> Vec<Frame> {
            self.w.doc.key_frames(node, &PropPath::new("opacity"))
        }
    }

    /// Single-row context. The rows are leaked: tiny, per-test, and lets the
    /// returned `TimelineCtx` borrow them without self-referential borrows.
    fn ctx_for<'a>(w: &'a World, target: TimelineTarget) -> TimelineCtx<'a> {
        let rows: &'a [TimelineRow] = Box::leak(Box::new(vec![TimelineRow {
            node: w.doc.nodes.keys().next().unwrap(),
            prop: PropPath::new("opacity"),
        }]));
        TimelineCtx {
            doc: &w.doc,
            clips: &w.clips,
            target,
            rows,
            layout: LAYOUT,
            range: (Frame(0), Frame(60)),
            playhead: 0.0,
        }
    }

    #[test]
    fn alt_click_cycles_linear_to_ease_in() {
        let mut hrn = Harness::new();
        let node = hrn.w.node();
        hrn.w.doc_key(node, 10, 1.0);
        let mut b = TimelineKeyframeBehavior::default();
        hrn.run(&mut b, TimelineTarget::Doc, TimelineEvent::Press {
            pos: DVec2::new(100.0, 10.0), modifiers: Modifiers { shift: false, alt: true, ctrl: false },
        });
        let k = hrn.w.doc.keyframe_data(node, &PropPath::new("opacity"), Frame(10)).unwrap();
        assert_eq!(
            (k.interpolation, k.ease_out, k.ease_in),
            EasingPreset::EaseIn.segment()
        );
    }

    #[test]
    fn alt_click_clip_key_emits_add_clip_key_same_value() {
        let mut w = World::new();
        let node = w.node();
        let cid = w.clip(node, vec![(10, 1.0)]);
        let mut b = TimelineKeyframeBehavior::default();
        let ctx = ctx_for(&w, TimelineTarget::Clip(cid));
        // Alt+press emits a Cycle-easing transaction immediately (selection untouched).
        let out = b.handle(&ctx, TimelineEvent::Press {
            pos: DVec2::new(100.0, 10.0), modifiers: Modifiers { shift: false, alt: true, ctrl: false },
        });
        let mut cmd = None;
        for o in out {
            if let ToolOutput::Commands(cs) = o {
                if let EditorCommand::AddClipKey { clip, key, .. } = &cs[0] {
                    assert_eq!(*clip, cid);
                    assert_eq!(key.frame, Frame(10));
                    assert_eq!(key.value, Value::F64(1.0)); // value preserved
                    cmd = Some(());
                }
            }
        }
        assert!(cmd.is_some(), "expected an AddClipKey command");
        assert!(b.selected().is_empty()); // Alt+click never touches selection
    }

    #[test]
    fn drag_sticks_at_stationary_collision() {
        let mut hrn = Harness::new();
        let node = hrn.w.node();
        for (f, v) in [(0, 0.0), (5, 1.0), (9, 2.0)] {
            hrn.w.doc_key(node, f, v);
        }
        let mut b = TimelineKeyframeBehavior::default();
        b.selected = vec![
            KeyRef { node, prop: PropPath::new("opacity"), frame: Frame(0) },
            KeyRef { node, prop: PropPath::new("opacity"), frame: Frame(5) },
        ];
        // Shift-select both, then drag right; 0->4/5->9 collides with stationary 9 → sticks at +3.
        hrn.run(&mut b, TimelineTarget::Doc, TimelineEvent::Press {
            pos: DVec2::new(0.0, 10.0), modifiers: Modifiers { shift: false, alt: false, ctrl: false },
        });
        hrn.run(&mut b, TimelineTarget::Doc, TimelineEvent::Press {
            pos: DVec2::new(50.0, 10.0), modifiers: Modifiers { shift: true, alt: false, ctrl: false },
        });
        hrn.run(&mut b, TimelineTarget::Doc, TimelineEvent::Move { pos: DVec2::new(90.0, 10.0), modifiers: Modifiers::none() });
        hrn.run(&mut b, TimelineTarget::Doc, TimelineEvent::Release { pos: DVec2::new(90.0, 10.0), modifiers: Modifiers::none() });
        assert_eq!(hrn.frames(node), vec![Frame(3), Frame(8), Frame(9)]);
    }

    #[test]
    fn drag_clamps_to_range() {
        let mut hrn = Harness::new();
        let node = hrn.w.node();
        hrn.w.doc_key(node, 1, 0.5);
        let mut b = TimelineKeyframeBehavior::default();
        b.selected = vec![KeyRef { node, prop: PropPath::new("opacity"), frame: Frame(1) }];
        hrn.run(&mut b, TimelineTarget::Doc, TimelineEvent::Press {
            pos: DVec2::new(10.0, 10.0), modifiers: Modifiers::none(),
        });
        hrn.run(&mut b, TimelineTarget::Doc, TimelineEvent::Move { pos: DVec2::new(10.0 - 50.0, 10.0), modifiers: Modifiers::none() });
        hrn.run(&mut b, TimelineTarget::Doc, TimelineEvent::Release { pos: DVec2::new(10.0 - 50.0, 10.0), modifiers: Modifiers::none() });
        assert_eq!(hrn.frames(node), vec![Frame(0)]); // clamped at range.0
    }

    #[test]
    fn escape_mid_drag_cancels_single_undo_unit() {
        let mut hrn = Harness::new();
        let node = hrn.w.node();
        hrn.w.doc_key(node, 0, 0.0);
        let mut b = TimelineKeyframeBehavior::default();
        b.selected = vec![KeyRef { node, prop: PropPath::new("opacity"), frame: Frame(0) }];
        hrn.run(&mut b, TimelineTarget::Doc, TimelineEvent::Press {
            pos: DVec2::new(0.0, 10.0), modifiers: Modifiers::none(),
        });
        hrn.run(&mut b, TimelineTarget::Doc, TimelineEvent::Move { pos: DVec2::new(50.0, 10.0), modifiers: Modifiers::none() });
        hrn.run(&mut b, TimelineTarget::Doc, TimelineEvent::KeyDown(TimelineKey::Escape));
        assert_eq!(hrn.frames(node), vec![Frame(0)]); // transaction cancelled = no-op
        assert!(!hrn.h.can_undo());
    }

    #[test]
    fn box_select_and_shift_add() {
        let mut w = World::new();
        let node = w.node();
        for f in [2, 4, 6, 8] {
            w.doc_key(node, f, 0.0);
        }
        let mut b = TimelineKeyframeBehavior::default();
        let ctx = ctx_for(&w, TimelineTarget::Doc);
        // Box-select frames 1..5 (x in [10,50]) row 0 → picks 2 and 4.
        b.handle(&ctx, TimelineEvent::Press { pos: DVec2::new(0.0, 10.0), modifiers: Modifiers::none() });
        b.handle(&ctx, TimelineEvent::Move { pos: DVec2::new(50.0, 10.0), modifiers: Modifiers::none() });
        b.handle(&ctx, TimelineEvent::Release { pos: DVec2::new(50.0, 10.0), modifiers: Modifiers::none() });
        assert_eq!(b.selected.len(), 2);
        // Press + Drag to select 6 and 8, but simple check: frames picked are 2 and 4.
        let got: Vec<i64> = b.selected.iter().map(|r| r.frame.0).collect();
        assert_eq!(got, vec![2, 4]);
    }

    #[test]
    fn ctrl_click_toggles() {
        let mut w = World::new();
        let node = w.node();
        w.doc_key(node, 4, 0.0);
        let mut b = TimelineKeyframeBehavior::default();
        let ctx = ctx_for(&w, TimelineTarget::Doc);
        b.handle(&ctx, TimelineEvent::Press {
            pos: DVec2::new(40.0, 10.0), modifiers: Modifiers { shift: false, alt: false, ctrl: true },
        });
        assert_eq!(b.selected.len(), 1);
        b.handle(&ctx, TimelineEvent::Release { pos: DVec2::new(40.0, 10.0), modifiers: Modifiers { shift: false, alt: false, ctrl: true } });
        b.handle(&ctx, TimelineEvent::Press {
            pos: DVec2::new(40.0, 10.0), modifiers: Modifiers { shift: false, alt: false, ctrl: true },
        });
        assert!(b.selected.is_empty());
    }

    #[test]
    fn delete_emits_one_transaction() {
        let mut hrn = Harness::new();
        let node = hrn.w.node();
        for (f, v) in [(0, 0.0), (10, 1.0)] {
            hrn.w.doc_key(node, f, v);
        }
        let mut b = TimelineKeyframeBehavior::default();
        b.selected = vec![
            KeyRef { node, prop: PropPath::new("opacity"), frame: Frame(0) },
            KeyRef { node, prop: PropPath::new("opacity"), frame: Frame(10) },
        ];
        hrn.run(&mut b, TimelineTarget::Doc, TimelineEvent::KeyDown(TimelineKey::Delete));
        assert!(hrn.frames(node).is_empty());
        hrn.h.undo(&mut hrn.w.pm()).unwrap(); // one transaction = one undo
        assert_eq!(hrn.frames(node), vec![Frame(0), Frame(10)]);
    }

    #[test]
    fn double_click_doc_key_is_visual_noop() {
        let mut hrn = Harness::new();
        let node = hrn.w.node();
        hrn.w.doc_key(node, 0, 1.0);
        hrn.w.doc_key(node, 10, 0.0);
        let mut b = TimelineKeyframeBehavior::default();
        // Double-click at frame 5: value should equal evaluated value_at(5).
        let expect = hrn.w.doc.value_at(node, &PropPath::new("opacity"), 5.0).unwrap();
        hrn.run(&mut b, TimelineTarget::Doc, TimelineEvent::DoubleClick { pos: DVec2::new(50.0, 10.0), modifiers: Modifiers::none() });
        let got = hrn.w.doc.keyframe_data(node, &PropPath::new("opacity"), Frame(5)).unwrap().value;
        assert_eq!(got, expect);
    }

    #[test]
    fn scrub_clamps_and_rounds() {
        let mut b = TimelineScrubBehavior::default();
        // ctx with layout mapping 1px=1frame, range 0..10, origin_x 0.
        let w = World::new();
        let ctx = TimelineCtx {
            doc: &w.doc,
            clips: &w.clips,
            target: TimelineTarget::Doc,
            rows: &[],
            layout: TimelineLayout { px_per_frame: 1.0, ..LAYOUT },
            range: (Frame(0), Frame(10)),
            playhead: 0.0,
        };
        let out = b.handle(&ctx, TimelineEvent::Press { pos: DVec2::new(105.0, 40.0), modifiers: Modifiers::none() });
        let f = out.iter().find_map(|o| match o {
            ToolOutput::SetPlayhead(f) => Some(*f),
            _ => None,
        });
        assert_eq!(f, Some(10.0)); // clamped to range
        let out = b.handle(&ctx, TimelineEvent::Move { pos: DVec2::new(3.6, 40.0), modifiers: Modifiers::none() });
        let f = out.iter().find_map(|o| match o {
            ToolOutput::SetPlayhead(f) => Some(*f),
            _ => None,
        });
        assert_eq!(f, Some(4.0)); // rounds
    }

    #[test]
    fn curve_drag_clamps_x_only() {
        let mut hrn = Harness::new();
        let node = hrn.w.node();
        let cid = hrn.w.clip(node, vec![(0, 0.0), (10, 1.0)]);
        let layout = CurveLayout {
            origin: DVec2::ZERO,
            size: DVec2::new(100.0, 100.0),
            handle_tolerance_px: 10.0,
        };
        let mut b = EasingCurveBehavior::new(layout);
        b.segment = Some(KeyRef { node, prop: PropPath::new("opacity"), frame: Frame(0) });
        // Grab the ease_out handle (Linear out = {1/3, 1/3}) at its px position.
        let press = TimelineEvent::Press { pos: layout.to_px(1.0 / 3.0, 1.0 / 3.0), modifiers: Modifiers::none() };
        {
            let ctx = ctx_for(&hrn.w, TimelineTarget::Clip(cid));
            hrn.route(b.handle(&ctx, press));
        }
        // Move to unit x=1.4 → clamped to 1.0; y=-1.3 (anticipate/overshoot) preserved.
        {
            let ctx = ctx_for(&hrn.w, TimelineTarget::Clip(cid));
            let out = b.handle(&ctx, TimelineEvent::Move { pos: DVec2::new(140.0, 130.0), modifiers: Modifiers::none() });
            hrn.route(out);
        }
        {
            let ctx = ctx_for(&hrn.w, TimelineTarget::Clip(cid));
            let out = b.handle(&ctx, TimelineEvent::Release { pos: DVec2::new(140.0, 130.0), modifiers: Modifiers::none() });
            hrn.route(out);
        }
        let k = hrn.w.clips[cid]
            .tracks
            .iter()
            .find(|t| t.node == node)
            .unwrap()
            .keys[0]
            .clone();
        assert_eq!(k.ease_out.x, 1.0); // x clamped to [0,1]
        assert!((k.ease_out.y - (-1.3)).abs() < 1e-6); // y free (anticipate/overshoot)
    }
}