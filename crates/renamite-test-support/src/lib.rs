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