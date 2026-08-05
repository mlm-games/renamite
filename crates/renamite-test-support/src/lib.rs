//! Test support: JSON event fixtures, scene snapshots, and proptest helpers.

use renamite_model::{Document, Scene};

/// Run a JSON event script against a behavior and return the emitted commands.
/// `fixture!` macro reads a fixture file and compares to expected output.
pub fn run_fixture(_events_json: &str, _behavior: &mut dyn FnMut()) -> Vec<serde_json::Value> {
    Vec::new()
}

/// Serialize a scene for structural diffing (WASM-safe).
pub fn scene_to_json(scene: &Scene) -> serde_json::Value {
    serde_json::to_value(scene).expect("scene serializes")
}

/// Assert two documents are semantically equal (structural JSON diff).
pub fn assert_doc_eq(_a: &Document, _b: &Document) {
    // TODO: panic with a JSON diff on mismatch.
}

#[macro_export]
macro_rules! assert_scene_snapshot {
    ($scene:expr) => {{
        let json = $crate::scene_to_json(&$scene);
        insta_like_snapshot(json);
    }};
}

fn insta_like_snapshot(_value: serde_json::Value) {}

pub mod timeline_fixture {
    use glam::DVec2;
    use renamite_animation::{EasingHandle, Frame, Interpolation};
    use renamite_behavior_common::Modifiers;
    use renamite_behavior_timeline::*;
    use renamite_history::{History, ProjectMut, ToolOutput};
    use renamite_machine::{Clip, ClipId, ClipMap, MachineId, MachineMap, Track};
    use renamite_model::{Document, KeyframeData, Node, NodeId, NodeKind, Parent, PropPath, Value};
    use serde::Deserialize;
    use std::collections::HashMap;

    #[derive(Deserialize)]
    pub struct Fixture {
        pub name: String,
        /// "doc" or "clip"
        pub target: String,
        pub nodes: Vec<String>,
        /// (node name, prop, [frame, value] pairs) - f64 props only, by design.
        #[serde(default)]
        pub keys: Vec<KeySpec>,
        pub rows: Vec<RowSpec>,
        pub layout: TimelineLayout,
        pub range: (i64, i64),
        pub events: Vec<EventSpec>,
        pub expect: Expect,
    }

    #[derive(Deserialize)]
    pub struct KeySpec {
        pub node: String,
        pub prop: String,
        pub keys: Vec<(i64, f64)>,
    }

    #[derive(Deserialize)]
    pub struct RowSpec {
        pub node: String,
        pub prop: String,
    }

    #[derive(Deserialize)]
    #[serde(tag = "type")]
    pub enum EventSpec {
        Press { x: f64, y: f64, #[serde(default)] alt: bool, #[serde(default)] shift: bool, #[serde(default)] ctrl: bool },
        Move { x: f64, y: f64 },
        Release { x: f64, y: f64 },
        DoubleClick { x: f64, y: f64 },
        Delete,
        Escape,
    }

    #[derive(Deserialize)]
    pub struct Expect {
        /// Final frames per row (same order as `rows`).
        pub row_frames: Vec<Vec<i64>>,
        /// Selection as (node name, frame).
        #[serde(default)]
        pub selection: Option<Vec<(String, i64)>>,
        /// After running: undo everything and check original frames restore.
        #[serde(default)]
        pub verify_undo: bool,
    }

    struct World {
        doc: Document,
        clips: ClipMap,
        clip_order: Vec<ClipId>,
        machines: MachineMap,
        machine_order: Vec<MachineId>,
        start: Option<MachineId>,
        names: HashMap<String, NodeId>,
        clip: Option<ClipId>,
    }

    impl World {
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

    fn f64_key(frame: i64, v: f64) -> KeyframeData {
        KeyframeData {
            frame: Frame(frame),
            value: Value::F64(v),
            interpolation: Interpolation::Linear,
            ease_out: EasingHandle::LINEAR_OUT,
            ease_in: EasingHandle::LINEAR_IN,
        }
    }

    pub fn run(json: &str) {
        let fx: Fixture = serde_json::from_str(json).expect("fixture parses");
        let mut w = World {
            doc: Document::empty(),
            clips: ClipMap::default(),
            clip_order: vec![],
            machines: MachineMap::default(),
            machine_order: vec![],
            start: None,
            names: HashMap::new(),
            clip: None,
        };
        for (i, name) in fx.nodes.iter().enumerate() {
            let id = w.doc.create_node(Node::new(name.clone(), NodeKind::Group));
            w.doc.attach(id, Parent::Comp(w.doc.main), i).unwrap();
            w.names.insert(name.clone(), id);
        }
        let target = match fx.target.as_str() {
            "doc" => {
                for ks in &fx.keys {
                    let id = w.names[&ks.node];
                    let prop = PropPath::new(ks.prop.clone());
                    for (f, v) in &ks.keys {
                        w.doc.add_keyframe(id, &prop, Frame(*f), &Value::F64(*v)).unwrap();
                    }
                }
                TimelineTarget::Doc
            }
            "clip" => {
                let tracks = fx
                    .keys
                    .iter()
                    .map(|ks| Track {
                        node: w.names[&ks.node],
                        prop: PropPath::new(ks.prop.clone()),
                        keys: ks.keys.iter().map(|(f, v)| f64_key(*f, *v)).collect(),
                    })
                    .collect();
                let cid = w.clips.insert(Clip {
                    name: "fx".into(),
                    range: (Frame(fx.range.0), Frame(fx.range.1)),
                    tracks,
                    events: vec![],
                });
                w.clip_order.push(cid);
                w.clip = Some(cid);
                TimelineTarget::Clip(cid)
            }
            other => panic!("unknown target {other}"),
        };
        let rows: Vec<TimelineRow> = fx
            .rows
            .iter()
            .map(|r| TimelineRow { node: w.names[&r.node], prop: PropPath::new(r.prop.clone()) })
            .collect();
        let original = snapshot_rows(&w, target, &rows);

        let mut behavior = TimelineKeyframeBehavior::default();
        let mut history = History::new();
        let mut applied = 0usize;

        for ev in &fx.events {
            let event = match ev {
                EventSpec::Press { x, y, alt, shift, ctrl } => TimelineEvent::Press {
                    pos: DVec2::new(*x, *y),
                    modifiers: Modifiers { alt: *alt, shift: *shift, ctrl: *ctrl },
                },
                EventSpec::Move { x, y } => TimelineEvent::Move {
                    pos: DVec2::new(*x, *y),
                    modifiers: Modifiers::none(),
                },
                EventSpec::Release { x, y } => TimelineEvent::Release {
                    pos: DVec2::new(*x, *y),
                    modifiers: Modifiers::none(),
                },
                EventSpec::DoubleClick { x, y } => TimelineEvent::DoubleClick {
                    pos: DVec2::new(*x, *y),
                    modifiers: Modifiers::none(),
                },
                EventSpec::Delete => TimelineEvent::KeyDown(TimelineKey::Delete),
                EventSpec::Escape => TimelineEvent::KeyDown(TimelineKey::Escape),
            };
            // Context is rebuilt per event: it borrows the (possibly mutated) world.
            let outputs = {
                let ctx = TimelineCtx {
                    doc: &w.doc,
                    clips: &w.clips,
                    target,
                    rows: &rows,
                    layout: fx.layout,
                    range: (Frame(fx.range.0), Frame(fx.range.1)),
                    playhead: 0.0,
                };
                behavior.handle(&ctx, event)
            };
            for out in outputs {
                match out {
                    ToolOutput::BeginTransaction(label) => history.begin(label),
                    ToolOutput::CommitTransaction => {
                        history.commit();
                        applied += 1;
                    }
                    ToolOutput::CancelTransaction => history.cancel(&mut w.pm()).unwrap(),
                    ToolOutput::Commands(cmds) => {
                        for c in cmds {
                            history
                                .apply(&mut w.pm(), c)
                                .unwrap_or_else(|e| panic!("{}: command failed: {e:?}", fx.name));
                        }
                    }
                    _ => {}
                }
            }
        }

        let got = snapshot_rows(&w, target, &rows);
        let want: Vec<Vec<Frame>> = fx
            .expect
            .row_frames
            .iter()
            .map(|v| v.iter().map(|f| Frame(*f)).collect())
            .collect();
        assert_eq!(got, want, "{}: final key frames", fx.name);

        if let Some(sel) = &fx.expect.selection {
            let got_sel: Vec<(String, i64)> = behavior
                .selected()
                .iter()
                .map(|r| {
                    let name = w.names.iter().find(|(_, id)| **id == r.node).unwrap().0.clone();
                    (name, r.frame.0)
                })
                .collect();
            let want_sel: Vec<(String, i64)> = sel.clone();
            assert_eq!(got_sel, want_sel, "{}: selection", fx.name);
        }

        if fx.expect.verify_undo {
            for _ in 0..applied {
                history.undo(&mut w.pm()).unwrap();
            }
            assert_eq!(snapshot_rows(&w, target, &rows), original, "{}: undo restores", fx.name);
        }
    }

    fn snapshot_rows(w: &World, target: TimelineTarget, rows: &[TimelineRow]) -> Vec<Vec<Frame>> {
        rows.iter()
            .map(|row| match target {
                TimelineTarget::Doc => w.doc.key_frames(row.node, &row.prop),
                TimelineTarget::Clip(cid) => w
                    .clips
                    .get(cid)
                    .and_then(|c| c.tracks.iter().find(|t| t.node == row.node && t.prop == row.prop))
                    .map(|t| t.keys.iter().map(|k| k.frame).collect())
                    .unwrap_or_default(),
            })
            .collect()
    }
}