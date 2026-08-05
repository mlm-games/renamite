//! Run JSON timeline fixtures through the real History-backed runner.

use renamite_test_support::timeline_fixture;

#[test]
fn drag_two_keys_right() {
    timeline_fixture::run(include_str!("fixtures/drag_two_keys.json"));
}

#[test]
fn clip_alt_cycle_easing() {
    timeline_fixture::run(include_str!("fixtures/clip_alt_cycle_easing.json"));
}

#[test]
fn clip_drag_one_key() {
    timeline_fixture::run(include_str!("fixtures/clip_drag_one_key.json"));
}
