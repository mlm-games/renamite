//! Headless runtime for `.ren` projects.
//!
//! Two entry points, one code path:
//!
//! * [`Engine`] - playback state only; borrows `&RenFile` per call. This is what
//!   the editor embeds: History can mutate the document between ticks and the
//!   engine keeps its machine state, hover, and playhead.
//! * [`Player`] - owns a `RenFile` and wraps an `Engine`. Drop-in for apps,
//!   games, and the future web embed.
//!
//! Compositing rule (locked): in machine mode the document's main timeline
//! plays as a background layer and machine `Overrides` win on top of it -
//! which `evaluate_with` already guarantees, since overrides beat keyframes.

use glam::DVec2;
use renamite_animation::{FrameRate, LoopMode, PlayState, Playback};
use renamite_io_ren::RenFile;
use renamite_machine::{MachineId, MachineInstance, PointerEventKind};
use renamite_model::{CompId, NodeId, Overrides, PropPath, Scene, Value, evaluate_with, pick};

pub use renamite_model::{nodes_bounds, pick_box};

#[derive(Debug, thiserror::Error)]
pub enum PlayerError {
    #[error(transparent)]
    Ren(#[from] renamite_io_ren::RenError),
    #[error("composition not found in project")]
    MissingComp,
    #[error("binary .renb support requires the `binary` feature")]
    BinaryDisabled,
    #[error("input is not valid UTF-8 .ren text")]
    InvalidUtf8,
}

/// What drives the evaluated frame and the override patch.
pub enum PlayMode {
    /// Machine overrides composited over a playing background timeline.
    Machine {
        id: MachineId,
        instance: MachineInstance,
        background: Playback,
    },
    /// Plain timeline playback.
    Timeline { playback: Playback },
    /// Editor scrub: locked to an exact frame; ticking does not advance.
    Scrub { frame: f64 },
}

/// Playback state with no document data. Borrows `&RenFile` per call, so the
/// owner (editor or `Player`) is free to mutate the project between calls.
pub struct Engine {
    comp: CompId,
    rate: FrameRate,
    pub mode: PlayMode,
    paused: bool,

    ov: Overrides,           // scratch, reused every tick
    host_ov: Overrides,      // durable host-driven patch; wins over machine/timeline
    scene: Scene,            // last evaluated frame; also the pick surface
    hover: Option<NodeId>,   // Enter/Exit synthesis
    pressed: Option<NodeId>, // Click synthesis
    events: Vec<String>,     // fired this tick, read by host
}

impl Engine {
    /// Machine mode iff `start_machine` names a real machine; else timeline.
    pub fn new(project: &RenFile) -> Result<Self, PlayerError> {
        let comp = project.document.main;
        let c = project
            .document
            .compositions
            .get(comp)
            .ok_or(PlayerError::MissingComp)?;
        let playing = Playback {
            state: PlayState::Playing,
            head: c.range.0.0 as f64,
            loop_mode: LoopMode::Loop,
            range: c.range,
            dir: 1.0,
        };
        let mode = match project.start_machine {
            Some(id) if project.machines.contains_key(id) => PlayMode::Machine {
                id,
                instance: MachineInstance::new(&project.machines[id]),
                background: playing,
            },
            _ => PlayMode::Timeline { playback: playing },
        };
        let mut e = Self {
            comp,
            rate: c.rate,
            mode,
            paused: false,
            ov: Overrides::default(),
            host_ov: Overrides::default(),
            scene: Scene::default(),
            hover: None,
            pressed: None,
            events: Vec::new(),
        };
        e.reevaluate(project);
        Ok(e)
    }

    /// Advance by `dt_secs`, re-evaluate, return events fired this tick.
    pub fn tick(&mut self, project: &RenFile, dt_secs: f64) -> &[String] {
        self.events.clear();
        if self.paused {
            return &self.events;
        }
        self.ov.clear();
        let dt_frames = dt_secs * self.rate.fps();

        let frame = match &mut self.mode {
            PlayMode::Machine {
                id,
                instance,
                background,
            } => {
                background.advance(dt_secs, self.rate);
                if let Some(m) = project.machines.get(*id) {
                    let out = instance.tick(m, &project.clips, dt_frames, &mut self.ov);
                    self.events.extend(out.events);
                }
                background.head
            }
            PlayMode::Timeline { playback } => {
                playback.advance(dt_secs, self.rate);
                playback.head
            }
            PlayMode::Scrub { frame } => *frame,
        };

        self.apply_host_overrides();
        self.scene = evaluate_with(&project.document, self.comp, frame, &self.ov);
        &self.events
    }

    fn apply_host_overrides(&mut self) {
        for ((id, prop), value) in self.host_ov.values.iter() {
            self.ov.set(*id, prop.clone(), value.clone());
        }
    }

    /// Re-evaluate at the current head without advancing time. Call after any
    /// editor mutation of the document (History apply/undo/redo).
    pub fn reevaluate(&mut self, project: &RenFile) {
        self.apply_host_overrides();
        self.scene = evaluate_with(&project.document, self.comp, self.head(), &self.ov);
    }

    /// Patch a property for this frame and onward (e.g. game tint / bow stretch).
    /// Host overrides survive `tick` and win over machine overrides; call
    /// [`Player::set_host_override`] to have the scene update immediately.
    pub fn set_host_override(&mut self, id: NodeId, prop: PropPath, v: Value) {
        self.host_ov.set(id, prop, v);
    }

    /// Drop every host override patch.
    pub fn clear_host_overrides(&mut self) {
        self.host_ov.clear();
    }

    /// Deterministic capture: N frames at fixed dt (golden tests, export).
    pub fn bake(&mut self, project: &RenFile, frames: usize, dt_secs: f64) -> Vec<Scene> {
        (0..frames)
            .map(|_| {
                self.tick(project, dt_secs);
                self.scene.clone()
            })
            .collect()
    }

    pub fn play_machine(&mut self, project: &RenFile, id: MachineId) -> bool {
        if !project.machines.contains_key(id) {
            return false;
        }
        let background = self.playing_playback(project);
        self.mode = PlayMode::Machine {
            id,
            instance: MachineInstance::new(&project.machines[id]),
            background,
        };
        self.hover = None;
        self.pressed = None;
        true
    }

    pub fn play_timeline(&mut self, project: &RenFile, loop_mode: LoopMode) {
        let mut playback = self.playing_playback(project);
        playback.loop_mode = loop_mode;
        self.mode = PlayMode::Timeline { playback };
    }

    pub fn set_timeline_playback(&mut self, pb: Playback) {
        match &mut self.mode {
            PlayMode::Timeline { playback } => *playback = pb,
            PlayMode::Scrub { .. } => self.mode = PlayMode::Timeline { playback: pb },
            PlayMode::Machine { .. } => {}
        }
    }

    /// Lock to a frame (editor scrubbing). Machine state is discarded.
    pub fn scrub(&mut self, project: &RenFile, frame: f64) {
        self.mode = PlayMode::Scrub { frame };
        self.reevaluate(project);
    }

    /// Switch the active composition (artboard). Resets playback to its range.
    pub fn set_composition(&mut self, project: &RenFile, comp: CompId) -> bool {
        let Some(c) = project.document.compositions.get(comp) else {
            return false;
        };
        self.comp = comp;
        self.rate = c.rate;
        self.play_timeline(project, LoopMode::Loop);
        self.reevaluate(project);
        true
    }

    pub fn pause(&mut self) {
        self.paused = true;
    }
    pub fn resume(&mut self) {
        self.paused = false;
    }
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    fn playing_playback(&self, project: &RenFile) -> Playback {
        let range = project.document.compositions[self.comp].range;
        Playback {
            state: PlayState::Playing,
            head: range.0.0 as f64,
            loop_mode: LoopMode::Loop,
            range,
            dir: 1.0,
        }
    }

    pub fn scene(&self) -> &Scene {
        &self.scene
    }
    pub fn events(&self) -> &[String] {
        &self.events
    }
    pub fn rate(&self) -> FrameRate {
        self.rate
    }
    pub fn composition(&self) -> CompId {
        self.comp
    }

    pub fn head(&self) -> f64 {
        match &self.mode {
            PlayMode::Machine { background, .. } => background.head,
            PlayMode::Timeline { playback } => playback.head,
            PlayMode::Scrub { frame } => *frame,
        }
    }

    /// Active state index per machine layer, or `None` when not in machine mode.
    pub fn active_machine_states(&self) -> Option<Vec<usize>> {
        let PlayMode::Machine { instance, .. } = &self.mode else {
            return None;
        };

        Some(instance.layer_states().collect())
    }

    pub fn set_bool(&mut self, project: &RenFile, name: &str, v: bool) -> bool {
        self.with_input(project, name, |inst, i| inst.set_bool(i, v))
    }
    pub fn set_number(&mut self, project: &RenFile, name: &str, v: f64) -> bool {
        self.with_input(project, name, |inst, i| inst.set_number(i, v))
    }
    pub fn fire(&mut self, project: &RenFile, name: &str) -> bool {
        self.with_input(project, name, |inst, i| inst.fire(i))
    }

    fn with_input(
        &mut self,
        project: &RenFile,
        name: &str,
        f: impl FnOnce(&mut MachineInstance, usize),
    ) -> bool {
        if let PlayMode::Machine { id, instance, .. } = &mut self.mode
            && let Some(m) = project.machines.get(*id)
            && let Some(i) = MachineInstance::input_index(m, name)
        {
            f(instance, i);
            return true;
        }
        false
    }

    pub fn pointer_move(&mut self, project: &RenFile, pt: DVec2) {
        let hit = pick(&self.scene, pt);
        if hit != self.hover {
            if let Some(prev) = self.hover {
                self.route(project, prev, PointerEventKind::Exit);
            }
            if let Some(now) = hit {
                self.route(project, now, PointerEventKind::Enter);
            }
            self.hover = hit;
        }
    }

    pub fn pointer_down(&mut self, project: &RenFile, pt: DVec2) {
        self.pressed = pick(&self.scene, pt);
        if let Some(n) = self.pressed {
            self.route(project, n, PointerEventKind::Down);
        }
    }

    pub fn pointer_up(&mut self, project: &RenFile, pt: DVec2) {
        if let Some(n) = pick(&self.scene, pt) {
            self.route(project, n, PointerEventKind::Up);
            if self.pressed == Some(n) {
                self.route(project, n, PointerEventKind::Click);
            }
        }
        self.pressed = None;
    }

    /// Pointer left the surface: synthesize Exit, clear press state.
    pub fn pointer_leave(&mut self, project: &RenFile) {
        if let Some(prev) = self.hover.take() {
            self.route(project, prev, PointerEventKind::Exit);
        }
        self.pressed = None;
    }

    fn route(&mut self, project: &RenFile, node: NodeId, kind: PointerEventKind) {
        if let PlayMode::Machine { id, instance, .. } = &mut self.mode
            && let Some(m) = project.machines.get(*id)
        {
            instance.pointer_event(m, node, kind);
        }
    }
}

/// Topmost pickable item under `pt` (world space). Moved to `renamite-model`
/// so canvas behaviors can hit-test without pulling the player in.
/// See [`renamite_model::pick`].

/// Self-contained player: owns the project, hides the borrow plumbing.
pub struct Player {
    pub project: RenFile,
    pub engine: Engine,
}

impl Player {
    pub fn new(project: RenFile) -> Result<Self, PlayerError> {
        let engine = Engine::new(&project)?;
        Ok(Self { project, engine })
    }

    pub fn from_ren_str(text: &str) -> Result<Self, PlayerError> {
        Self::new(renamite_io_ren::open(text)?)
    }

    #[cfg(feature = "binary")]
    pub fn from_ren_bytes(bytes: &[u8]) -> Result<Self, PlayerError> {
        Self::new(renamite_io_ren::open_binary(bytes)?)
    }

    /// Load from raw bytes, auto-detecting `.ren` (text) vs `.renb` (binary)
    /// by magic header. `.renb` requires the `binary` feature.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, PlayerError> {
        if renamite_io_ren::is_binary(bytes) {
            #[cfg(feature = "binary")]
            {
                return Self::from_ren_bytes(bytes);
            }
            #[cfg(not(feature = "binary"))]
            {
                return Err(PlayerError::BinaryDisabled);
            }
        }
        let text = std::str::from_utf8(bytes).map_err(|_| PlayerError::InvalidUtf8)?;
        Self::from_ren_str(text)
    }

    pub fn tick(&mut self, dt_secs: f64) -> &[String] {
        self.engine.tick(&self.project, dt_secs)
    }
    pub fn bake(&mut self, frames: usize, dt_secs: f64) -> Vec<Scene> {
        self.engine.bake(&self.project, frames, dt_secs)
    }
    pub fn scene(&self) -> &Scene {
        self.engine.scene()
    }
    pub fn events(&self) -> &[String] {
        self.engine.events()
    }
    pub fn head(&self) -> f64 {
        self.engine.head()
    }
    pub fn rate(&self) -> FrameRate {
        self.engine.rate()
    }
    pub fn composition(&self) -> CompId {
        self.engine.composition()
    }
    pub fn active_machine_states(&self) -> Option<Vec<usize>> {
        self.engine.active_machine_states()
    }

    /// Pixel size of the active composition's artboard.
    pub fn artboard_size(&self) -> (u32, u32) {
        self.project.document.compositions[self.engine.composition()].size
    }

    /// Re-evaluate at the current head without advancing time. Call after any
    /// mutation of `self.project` (e.g. live edits from the embed host).
    pub fn reevaluate(&mut self) {
        self.engine.reevaluate(&self.project);
    }

    pub fn pause(&mut self) {
        self.engine.pause();
    }
    pub fn resume(&mut self) {
        self.engine.resume();
    }
    pub fn is_paused(&self) -> bool {
        self.engine.is_paused()
    }

    /// Lock to a frame. Machine state is discarded.
    pub fn scrub(&mut self, frame: f64) {
        self.engine.scrub(&self.project, frame);
    }

    /// Switch the active composition (artboard). Resets playback to its range.
    pub fn set_composition(&mut self, comp: CompId) -> bool {
        self.engine.set_composition(&self.project, comp)
    }

    /// Patch a property for this frame and onward (e.g. game tint / bow stretch).
    /// Host overrides survive `tick`, win over machine overrides, and take
    /// effect immediately (the scene is re-evaluated here).
    pub fn set_host_override(&mut self, id: NodeId, prop: PropPath, v: Value) {
        self.engine.set_host_override(id, prop, v);
        self.engine.reevaluate(&self.project);
    }

    /// Drop every host override patch and re-evaluate.
    pub fn clear_host_overrides(&mut self) {
        self.engine.clear_host_overrides();
        self.engine.reevaluate(&self.project);
    }

    pub fn set_bool(&mut self, name: &str, v: bool) -> bool {
        self.engine.set_bool(&self.project, name, v)
    }
    pub fn set_number(&mut self, name: &str, v: f64) -> bool {
        self.engine.set_number(&self.project, name, v)
    }
    pub fn fire(&mut self, name: &str) -> bool {
        self.engine.fire(&self.project, name)
    }

    /// Pointer events apply machine listener side-effects immediately: each
    /// routes the pick, then re-evaluates with a zero-dt tick (editor parity).
    pub fn pointer_move(&mut self, pt: DVec2) {
        self.engine.pointer_move(&self.project, pt);
        let _ = self.engine.tick(&self.project, 0.0);
    }
    pub fn pointer_down(&mut self, pt: DVec2) {
        self.engine.pointer_down(&self.project, pt);
        let _ = self.engine.tick(&self.project, 0.0);
    }
    pub fn pointer_up(&mut self, pt: DVec2) {
        self.engine.pointer_up(&self.project, pt);
        let _ = self.engine.tick(&self.project, 0.0);
    }
    pub fn pointer_leave(&mut self) {
        self.engine.pointer_leave(&self.project);
        let _ = self.engine.tick(&self.project, 0.0);
    }

    pub fn play_machine(&mut self, id: MachineId) -> bool {
        self.engine.play_machine(&self.project, id)
    }
    pub fn play_timeline(&mut self, loop_mode: LoopMode) {
        self.engine.play_timeline(&self.project, loop_mode)
    }
    pub fn set_timeline_playback(&mut self, pb: Playback) {
        self.engine.set_timeline_playback(pb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::Shape;
    use renamite_animation::{Animated, EasingHandle, Frame, Interpolation};
    use renamite_machine::{
        Clip, ClipMap, Condition, InputDef, InputKind, Listener, ListenerAction, Machine,
        MachineLayer, MachineMap, State, StateKind, Track, Transition,
    };
    use renamite_model::{
        BlendMode, ClipPath, Color, Document, FillRule, KeyframeData, Node, NodeKind, PaintKind,
        Parent, PropPath, SceneItem, ScenePaint, ShapeKind, StyleKind, StylePaint, Value,
    };

    fn rect_kind(size: f64) -> NodeKind {
        NodeKind::Shape(ShapeKind::Rect {
            pos: Animated::new(DVec2::ZERO),
            size: Animated::new(DVec2::splat(size)),
            rounded: Animated::new(0.0),
        })
    }
    fn fill_kind() -> NodeKind {
        NodeKind::Style(StyleKind::Fill {
            paint: StylePaint::solid(Color::BLACK),
            rule: FillRule::NonZero,
        })
    }
    fn key_v2(f: i64, x: f64, y: f64) -> KeyframeData {
        KeyframeData {
            frame: Frame(f),
            value: Value::DVec2(DVec2::new(x, y)),
            interpolation: Interpolation::Linear,
            ease_out: EasingHandle::LINEAR_OUT,
            ease_in: EasingHandle::LINEAR_IN,
        }
    }

    fn center_x(s: &Scene) -> f64 {
        s.items[0].path.bounding_box().center().x
    }

    /// 100×100 box + fill under main. Returns (project, shape, fill).
    fn static_box() -> (RenFile, NodeId, NodeId) {
        let mut doc = Document::empty();
        let comp = doc.main;
        let shape = doc.create_node(Node::new("box", rect_kind(100.0)));
        doc.attach(shape, Parent::Comp(comp), 0).unwrap();
        let fill = doc.create_node(Node::new("fill", fill_kind()));
        doc.attach(fill, Parent::Comp(comp), 1).unwrap();
        (RenFile::new(doc, "static"), shape, fill)
    }

    /// static_box + machine: `go` moves the box to x=50 over 10 frames.
    fn moving_box() -> (RenFile, NodeId, NodeId) {
        let (mut f, shape, fill) = static_box();
        let mut clips = ClipMap::default();
        let mv = clips.insert(Clip {
            name: "move".into(),
            range: (Frame(0), Frame(10)),
            tracks: vec![Track {
                node: shape,
                prop: PropPath::new("transform.position"),
                keys: vec![key_v2(0, 0.0, 0.0), key_v2(10, 50.0, 0.0)],
            }],
            events: vec![],
        });
        let machine = Machine {
            name: "hover".into(),
            inputs: vec![InputDef {
                name: "go".into(),
                kind: InputKind::Bool { default: false },
            }],
            layers: vec![MachineLayer {
                name: "base".into(),
                entry: 0,
                any_transitions: vec![],
                states: vec![
                    State {
                        name: "Idle".into(),
                        kind: StateKind::Empty,
                        transitions: vec![Transition {
                            to: 1,
                            duration: 0.0,
                            exit_time: None,
                            conditions: vec![Condition::BoolIs {
                                input: 0,
                                value: true,
                            }],
                        }],
                    },
                    State {
                        name: "Move".into(),
                        kind: StateKind::Clip {
                            clip: mv,
                            speed: 1.0,
                            loop_mode: LoopMode::Once,
                        },
                        transitions: vec![],
                    },
                ],
            }],
            listeners: vec![Listener {
                node: shape,
                event: PointerEventKind::Enter,
                action: ListenerAction::SetBool {
                    input: 0,
                    value: true,
                },
            }],
        };
        let mut machines = MachineMap::default();
        let id = machines.insert(machine);
        f.clips = clips;
        f.machines = machines;
        f.start_machine = Some(id);
        (f, shape, fill)
    }

    #[test]
    fn timeline_plays_and_renders() {
        let (proj, _, _) = static_box();
        let mut p = Player::new(proj).unwrap();
        assert_eq!(p.scene().items.len(), 1);
        let h0 = p.head();
        p.tick(0.5);
        assert!(p.head() > h0);
    }

    /// THE design-pinning test: document timeline animates opacity underneath,
    /// machine overrides position on top - both visible in the same item.
    #[test]
    fn machine_composites_over_background_timeline() {
        let (mut proj, _, fill) = moving_box();
        let prop = PropPath::new("opacity");
        proj.document
            .add_keyframe(fill, &prop, Frame(0), &Value::F64(0.0))
            .unwrap();
        proj.document
            .add_keyframe(fill, &prop, Frame(60), &Value::F64(1.0))
            .unwrap();

        let mut p = Player::new(proj).unwrap();
        assert!(p.set_bool("go", true));
        p.tick(0.25); // transition fires, background head=15
        p.tick(0.25); // machine time 15 > clip len → clamped x=50; head=30

        let item = &p.scene().items[0];
        assert!(
            (center_x(p.scene()) - 50.0).abs() < 1.0,
            "machine override applied"
        );
        assert!(
            (item.opacity - 0.5).abs() < 0.02,
            "background timeline still animating (opacity={})",
            item.opacity
        );
    }

    #[test]
    fn host_override_wins_over_machine_and_survives_tick() {
        let (proj, shape, _) = moving_box();
        let mut p = Player::new(proj).unwrap();
        assert!(p.set_bool("go", true));
        p.tick(0.5); // transition fires, machine starts
        p.tick(0.5); // machine time > clip len → clamped x=50
        assert!((center_x(p.scene()) - 50.0).abs() < 1.0);

        // Host pins position; must win over the machine's override next tick.
        p.set_host_override(
            shape,
            PropPath::new("transform.position"),
            Value::DVec2(DVec2::new(120.0, 0.0)),
        );
        assert!(
            (center_x(p.scene()) - 120.0).abs() < 1e-6,
            "set_host_override re-evaluates immediately"
        );
        p.tick(0.25);
        assert!(
            (center_x(p.scene()) - 120.0).abs() < 1e-6,
            "host override survives tick"
        );

        p.clear_host_overrides();
        p.tick(0.25);
        assert!(
            (center_x(p.scene()) - 50.0).abs() < 1.0,
            "machine override returns after clear"
        );
    }

    #[test]
    fn scrub_locks_frame() {
        let (mut proj, _, fill) = static_box();
        let prop = PropPath::new("opacity");
        proj.document
            .add_keyframe(fill, &prop, Frame(0), &Value::F64(0.0))
            .unwrap();
        proj.document
            .add_keyframe(fill, &prop, Frame(60), &Value::F64(1.0))
            .unwrap();

        let mut e = Engine::new(&proj).unwrap();
        e.scrub(&proj, 30.0);
        e.tick(&proj, 1.0); // must not advance
        assert_eq!(e.head(), 30.0);
        assert!((e.scene().items[0].opacity - 0.5).abs() < 1e-9);
    }

    #[test]
    fn range_edit_is_reflected_in_loop_bounds() {
        let (mut proj, _, _) = static_box();
        let mut e = Engine::new(&proj).unwrap();
        assert_eq!(e.head(), 0.0);

        // Editor widens the comp (e.g. 0-180 -> 0-300) and pushes playback state.
        proj.document.compositions[proj.document.main].range = (Frame(0), Frame(300));
        e.set_timeline_playback(Playback {
            state: PlayState::Playing,
            head: 0.0,
            loop_mode: LoopMode::Loop,
            range: (Frame(0), Frame(300)),
            dir: 1.0,
        });

        // Old bound would wrap at 180; a 300-wide comp must keep advancing past it.
        e.tick(&proj, 3.1); // 186 frames @ 60fps
        assert!(
            e.head() > 180.0,
            "loop must use the new end frame, head={}",
            e.head()
        );
        assert!(e.head() <= 300.0);

        // And it wraps at 300, not at 180.
        e.tick(&proj, 2.0); // +120 frames -> 306 -> wraps to 6
        assert!(
            e.head() < 180.0,
            "loop must wrap at the new end frame, head={}",
            e.head()
        );
    }

    #[test]
    fn scrub_unlocks_on_play() {
        let (mut proj, _, fill) = static_box();
        let prop = PropPath::new("opacity");
        proj.document
            .add_keyframe(fill, &prop, Frame(0), &Value::F64(0.0))
            .unwrap();
        proj.document
            .add_keyframe(fill, &prop, Frame(60), &Value::F64(1.0))
            .unwrap();

        let mut e = Engine::new(&proj).unwrap();
        e.scrub(&proj, 30.0);
        e.tick(&proj, 1.0); // locked by scrub
        assert_eq!(e.head(), 30.0);

        // Editor presses play: pushes its playback state, leaving the lock.
        let mut e2 = e;
        e2.set_timeline_playback(Playback {
            state: PlayState::Playing,
            head: 30.0,
            loop_mode: LoopMode::Loop,
            range: (Frame(0), Frame(180)),
            dir: 1.0,
        });
        e2.tick(&proj, 1.0); // 60 frames later
        assert!(
            e2.head() > 30.0,
            "play after scrub must advance, head={}",
            e2.head()
        );
    }

    #[test]
    fn pick_hits_topmost_and_misses_outside() {
        let (proj, shape, _) = static_box();
        let p = Player::new(proj).unwrap();
        assert_eq!(pick(p.scene(), DVec2::ZERO), Some(shape));
        assert_eq!(pick(p.scene(), DVec2::new(500.0, 500.0)), None);
    }

    #[test]
    fn pick_respects_clip_mask() {
        let (_, shape, _) = static_box();
        let big = kurbo::Rect::new(-100.0, -100.0, 100.0, 100.0).to_path(0.1);
        let small = kurbo::Rect::new(-10.0, -10.0, 10.0, 10.0).to_path(0.1);
        let scene = Scene {
            clips: vec![ClipPath {
                path: small,
                rule: FillRule::NonZero,
            }],
            items: vec![SceneItem {
                path: big,
                node: shape,
                style: shape,
                paint: ScenePaint::Solid(Color::BLACK),
                kind: PaintKind::Fill(FillRule::NonZero),
                opacity: 1.0,
                clips: vec![0],
                blend: BlendMode::Normal,
            }],
        };
        assert_eq!(pick(&scene, DVec2::ZERO), Some(shape));
        assert_eq!(pick(&scene, DVec2::new(50.0, 50.0)), None);
    }

    #[test]
    fn hover_enter_drives_listener_and_leave_exits() {
        let (proj, _, _) = moving_box();
        let mut p = Player::new(proj).unwrap();
        p.tick(0.1);
        assert!(center_x(p.scene()).abs() < 1e-6);

        p.pointer_move(DVec2::ZERO); // Enter → go=true
        for _ in 0..5 {
            p.tick(0.1);
        }
        assert!(center_x(p.scene()) > 10.0);
        p.pointer_leave(); // must not panic / must clear hover state
    }

    /// THE ownership-pinning test: mutate the document mid-playback; the
    /// engine keeps machine state and reflects the edit on the next tick.
    #[test]
    fn live_edit_between_ticks_preserves_machine_state() {
        let (mut proj, _, fill) = moving_box();
        let mut e = Engine::new(&proj).unwrap();
        e.set_bool(&proj, "go", true);
        e.tick(&proj, 0.25);
        e.tick(&proj, 0.25);
        assert!((center_x(e.scene()) - 50.0).abs() < 1.0);

        let red = Color::rgba(1.0, 0.0, 0.0, 1.0);
        proj.document
            .set_static(fill, &PropPath::new("fill.color"), &Value::Color(red))
            .unwrap();

        e.tick(&proj, 0.01);
        let item = &e.scene().items[0];
        assert_eq!(
            item.paint,
            ScenePaint::Solid(red),
            "edit visible immediately"
        );
        assert!(
            (center_x(e.scene()) - 50.0).abs() < 1.0,
            "machine override survived the edit"
        );
    }

    #[test]
    fn bake_is_deterministic() {
        let make = || {
            let (proj, _, _) = moving_box();
            let mut p = Player::new(proj).unwrap();
            p.set_bool("go", true);
            p.bake(20, 1.0 / 60.0)
        };
        assert_eq!(make(), make());
    }
}
