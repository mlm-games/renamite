#![allow(clippy::unwrap_used, clippy::panic)]

use glam::DVec2;
use proptest::prelude::*;

use renamite_animation::Tween;
use renamite_geometry::{Anchor, VectorPath, match_topology};

fn path(points: &[(f64, f64)], closed: bool) -> VectorPath {
    VectorPath {
        anchors: points
            .iter()
            .map(|(x, y)| Anchor::corner(DVec2::new(*x, *y)))
            .collect(),
        closed,
    }
}

fn square() -> VectorPath {
    path(
        &[(0.0, 0.0), (100.0, 0.0), (100.0, 100.0), (0.0, 100.0)],
        true,
    )
}

fn star() -> VectorPath {
    path(
        &[
            (50.0, -10.0),
            (71.2, 28.8),
            (110.0, 50.0),
            (71.2, 71.2),
            (50.0, 110.0),
            (28.8, 71.2),
            (-10.0, 50.0),
            (28.8, 28.8),
        ],
        true,
    )
}

fn finite(path: &VectorPath) -> bool {
    path.anchors
        .iter()
        .all(|a| a.pos.is_finite() && a.tan_in.is_finite() && a.tan_out.is_finite())
}

fn positions(path: &VectorPath) -> Vec<DVec2> {
    path.anchors.iter().map(|a| a.pos).collect()
}

#[test]
fn square_tweens_into_a_star() {
    let (a, b) = (square(), star());
    let (start, end) = match_topology(&a, &b).unwrap();

    // The resting ends are the keyframes themselves, not the matched pair: a
    // keyframe's own anchor list is what the file stores and what a scrub
    // sitting on it must draw.
    assert_eq!(VectorPath::tween(&a, &b, 0.0), a);
    assert_eq!(VectorPath::tween(&a, &b, 1.0), b);

    let mid = VectorPath::tween(&a, &b, 0.5);
    assert_eq!(mid.anchors.len(), 8);
    assert!(mid.closed);
    assert!(finite(&mid));
    // Half way sits between the paired anchors instead of snapping to either.
    for (i, ((x, y), z)) in start
        .anchors
        .iter()
        .zip(&end.anchors)
        .zip(&mid.anchors)
        .enumerate()
    {
        assert!(
            (z.pos - (x.pos + y.pos) * 0.5).length() < 1e-9,
            "anchor {i}"
        );
    }
}

#[test]
fn holds_when_the_closure_disagrees() {
    let (closed, open) = (square(), star());
    let open = VectorPath {
        closed: false,
        ..open
    };
    assert!(match_topology(&closed, &open).is_none());
    assert_eq!(VectorPath::tween(&closed, &open, 0.5), closed);
    assert_eq!(VectorPath::tween(&closed, &open, 1.0), open);
}

#[test]
fn holds_on_degenerate_input_without_panicking() {
    let empty = VectorPath::default();
    let collapsed = path(&[(10.0, 10.0), (10.0, 10.0), (10.0, 10.0)], true);
    assert_eq!(VectorPath::tween(&empty, &star(), 0.5), empty);
    assert_eq!(VectorPath::tween(&square(), &collapsed, 0.5), square());
    assert_eq!(VectorPath::tween(&collapsed, &empty, 1.0), empty);
}

#[test]
fn easing_still_overshoots_past_the_end_key() {
    let a = path(&[(0.0, 0.0), (100.0, 0.0)], false);
    let b = path(&[(20.0, 0.0), (140.0, 0.0)], false);
    let past = VectorPath::tween(&a, &b, 1.5);
    for ((x, y), z) in a.anchors.iter().zip(&b.anchors).zip(&past.anchors) {
        assert!((z.pos - (x.pos + (y.pos - x.pos) * 1.5)).length() < 1e-9);
    }
}

fn coord() -> impl Strategy<Value = f64> {
    prop_oneof![
        -1e6f64..1e6f64,
        any::<f64>().prop_filter("finite", |v: &f64| v.is_finite())
    ]
}

fn anchor() -> impl Strategy<Value = Anchor> {
    (coord(), coord(), coord(), coord()).prop_map(|(x, y, ix, iy)| Anchor {
        pos: DVec2::new(x, y),
        tan_in: DVec2::new(ix, iy),
        tan_out: DVec2::new(iy, ix),
        mode: renamite_geometry::TangentMode::Smooth,
    })
}

/// Both paths at once, so a and b can be given the same anchor count.
fn pair() -> impl Strategy<Value = (VectorPath, VectorPath)> {
    proptest::collection::vec(anchor(), 6..16).prop_map(|anchors| {
        let mid = anchors.len() / 2;
        let mut a = anchors[..mid].to_vec();
        let mut b = anchors[mid..].to_vec();
        let filler = a[0];
        while a.len() < b.len() {
            a.push(filler);
        }
        while b.len() < a.len() {
            b.push(filler);
        }
        let build = |anchors: Vec<Anchor>| VectorPath {
            anchors,
            closed: true,
        };
        (build(a), build(b))
    })
}

proptest! {
    #[test]
    fn tween_of_matching_topology_stays_finite_and_exact_at_the_ends(pair in pair()) {
        let (a, b) = pair;
        prop_assert_eq!(positions(&VectorPath::tween(&a, &b, 0.0)), positions(&a));
        prop_assert_eq!(positions(&VectorPath::tween(&a, &b, 1.0)), positions(&b));
        for t in [0.1, 0.25, 0.5, 0.75, 0.9] {
            prop_assert!(finite(&VectorPath::tween(&a, &b, t)));
        }
    }

    #[test]
    fn morphing_paths_of_any_topology_never_produce_non_finite_anchors(pair in pair()) {
        if let Some((a, b)) = match_topology(&pair.0, &pair.1) {
            prop_assert_eq!(a.anchors.len(), b.anchors.len());
            prop_assert!(finite(&VectorPath::tween(&a, &b, 0.5)));
        }
    }
}
