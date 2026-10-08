#![allow(clippy::unwrap_used, clippy::panic)]

use std::time::{Duration, Instant};

use renamite_geometry::{Anchor, VectorPath, match_topology};

fn path(points: &[(f64, f64)], closed: bool) -> VectorPath {
    VectorPath {
        anchors: points
            .iter()
            .map(|(x, y)| Anchor::corner(glam::DVec2::new(*x, *y)))
            .collect(),
        closed,
    }
}

fn finite(path: &VectorPath) -> bool {
    path.anchors
        .iter()
        .all(|a| a.pos.is_finite() && a.tan_in.is_finite() && a.tan_out.is_finite())
}

fn positions(path: &VectorPath) -> Vec<glam::DVec2> {
    path.anchors.iter().map(|a| a.pos).collect()
}

/// Split points land on equal arclength, which is a `kurbo` inverse solve rather
/// than an exact parameter, so positions compare within the arclen tolerance.
fn assert_positions_close(got: &VectorPath, want: &[(f64, f64)]) {
    let want: Vec<glam::DVec2> = want.iter().map(|&(x, y)| glam::DVec2::new(x, y)).collect();
    let got = positions(got);
    assert_eq!(got.len(), want.len(), "anchor count: {got:?} vs {want:?}");
    for (i, (g, w)) in got.iter().zip(&want).enumerate() {
        assert!((g - w).length() < 1e-3, "anchor {i} at {g:?}, want {w:?}");
    }
}

/// Every original anchor survives the resample, in order.
fn keeps_anchors(grown: &VectorPath, original: &VectorPath) -> bool {
    let mut i = 0;
    for anchor in &original.anchors {
        while i < grown.anchors.len() && grown.anchors[i].pos != anchor.pos {
            i += 1;
        }
        if i == grown.anchors.len() {
            return false;
        }
        i += 1;
    }
    true
}

/// Alignment only re-starts or turns the contour; it never moves a point.
fn same_points(x: &VectorPath, y: &VectorPath) -> bool {
    let mut a: Vec<_> = x.anchors.iter().map(|p| (p.pos.x, p.pos.y)).collect();
    let mut b: Vec<_> = y.anchors.iter().map(|p| (p.pos.x, p.pos.y)).collect();
    a.sort_by(|p, q| p.0.total_cmp(&q.0).then(p.1.total_cmp(&q.1)));
    b.sort_by(|p, q| p.0.total_cmp(&q.0).then(p.1.total_cmp(&q.1)));
    a == b
}

#[test]
fn square_grows_into_a_star() {
    let square = path(
        &[(0.0, 0.0), (100.0, 0.0), (100.0, 100.0), (0.0, 100.0)],
        true,
    );
    let star = path(
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
    );

    let (a, b) = match_topology(&square, &star).expect("square to star morphs");
    assert_eq!(a.anchors.len(), 8);
    assert_eq!(b.anchors.len(), 8);
    assert!(finite(&a) && finite(&b));
    assert!(keeps_anchors(&a, &square));
    assert!(same_points(&b, &star));
}

#[test]
fn rotation_pairs_the_same_vertices() {
    // The square grows on its first edge (all four tie, the lowest wins), so
    // only the matching start anchor pairs every anchor exactly.
    let square = path(
        &[(0.0, 0.0), (100.0, 0.0), (100.0, 100.0), (0.0, 100.0)],
        true,
    );
    let shifted = path(
        &[
            (100.0, 100.0),
            (0.0, 100.0),
            (0.0, 0.0),
            (50.0, 0.0),
            (100.0, 0.0),
        ],
        true,
    );

    let (a, b) = match_topology(&square, &shifted).expect("closed morphs");
    let paired = [
        (0.0, 0.0),
        (50.0, 0.0),
        (100.0, 0.0),
        (100.0, 100.0),
        (0.0, 100.0),
    ];
    assert_positions_close(&a, &paired);
    assert_positions_close(&b, &paired);
}

#[test]
fn open_paths_turn_around_but_do_not_start_elsewhere() {
    let a = path(&[(0.0, 0.0), (100.0, 0.0)], false);
    let b = path(&[(0.0, 0.0), (40.0, 0.0), (100.0, 0.0)], false);

    let (a, b) = match_topology(&a, &b).expect("open morphs");
    assert_eq!(a.anchors.len(), 3);
    assert_eq!(b.anchors.len(), 3);
    // The far end of an open path stays put: no rotation to choose.
    assert_eq!(b.anchors[0].pos, glam::DVec2::new(0.0, 0.0));
    assert_eq!(b.anchors[2].pos, glam::DVec2::new(100.0, 0.0));
    assert!(keeps_anchors(&b, &path(&[(0.0, 0.0), (100.0, 0.0)], false)));
}

#[test]
fn reversed_correspondence_is_preferred() {
    let line = path(&[(0.0, 0.0), (100.0, 0.0), (50.0, 80.0)], true);
    // Same triangle, the split edge walked the other way round.
    let flipped = path(&[(50.0, 80.0), (100.0, 0.0), (50.0, 0.0), (0.0, 0.0)], true);

    let (a, b) = match_topology(&line, &flipped).expect("closed morphs");
    let want = positions(&a);
    assert_positions_close(&b, &want.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>());
}

#[test]
fn refuses_mismatched_closure() {
    let square = path(
        &[(0.0, 0.0), (100.0, 0.0), (100.0, 100.0), (0.0, 100.0)],
        true,
    );
    let open = path(
        &[(0.0, 0.0), (50.0, -10.0), (71.2, 28.8), (110.0, 50.0)],
        false,
    );
    assert!(match_topology(&square, &open).is_none());
    assert!(match_topology(&open, &square).is_none());
}

#[test]
fn refuses_paths_it_cannot_resample() {
    let star = path(
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
    );
    let empty = VectorPath {
        anchors: vec![],
        closed: true,
    };
    let single = path(&[(10.0, 10.0)], true);
    let sliver = path(&[(10.0, 10.0), (20.0, 20.0)], true);
    // Every anchor on one spot: nothing to split.
    let collapsed = path(&[(10.0, 10.0), (10.0, 10.0), (10.0, 10.0)], true);
    let mut nan = path(&[(0.0, 0.0), (100.0, 0.0)], false);
    nan.anchors[1].pos = glam::DVec2::new(f64::NAN, 0.0);

    for other in [
        empty.clone(),
        single,
        sliver,
        collapsed,
        nan,
        VectorPath {
            anchors: vec![],
            closed: false,
        },
    ] {
        assert!(match_topology(&star, &other).is_none());
        assert!(match_topology(&other, &star).is_none());
    }
}

#[test]
fn equal_counts_pass_through_untouched() {
    let a = path(&[(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)], true);
    let mut b = path(&[(5.0, 5.0), (90.0, 20.0), (50.0, 60.0)], true);
    b.anchors[1].mode = renamite_geometry::TangentMode::Symmetric;
    b.anchors[1].tan_in = glam::DVec2::new(-4.0, 0.0);
    b.anchors[1].tan_out = glam::DVec2::new(4.0, 0.0);

    let (left, right) = match_topology(&a, &b).expect("equal counts morph");
    assert_eq!(left, a);
    assert_eq!(right, b);
}

/// A keyframe running to thousands of anchors used to split the longest
/// segment over and over, a vector insert per anchor: quadratic in the target
/// and seconds per scrubbed frame. The resample is one pass now.
#[test]
fn growing_to_many_anchors_stays_linear() {
    let square = path(
        &[(0.0, 0.0), (100.0, 0.0), (100.0, 100.0), (0.0, 100.0)],
        true,
    );
    let target = 20_000;
    let dense = path(
        &(0..target)
            .map(|i| {
                let t = i as f64 / target as f64;
                (t * 1000.0, (t * std::f64::consts::TAU).sin() * 50.0)
            })
            .collect::<Vec<_>>(),
        true,
    );

    let start = Instant::now();
    let (a, b) = match_topology(&square, &dense).expect("closed morphs");
    let elapsed = start.elapsed();

    assert_eq!(a.anchors.len(), target);
    assert_eq!(b.anchors.len(), target);
    assert!(finite(&a) && finite(&b));
    assert_eq!(a.closed, square.closed);
    assert!(
        elapsed < Duration::from_secs(2),
        "growing to {target} anchors took {elapsed:?}"
    );
}

/// Over the rotation-search cap the start anchor is no longer searched for, but
/// the direction still is and the result must stay usable.
#[test]
fn many_anchors_still_morph_without_searching_rotations() {
    let ring = |phase: f64, n: usize| {
        path(
            &(0..n)
                .map(|i| {
                    let a = i as f64 / n as f64 * std::f64::consts::TAU + phase;
                    (a.cos() * 100.0, a.sin() * 100.0)
                })
                .collect::<Vec<_>>(),
            true,
        )
    };
    let n = 4_000;
    let (a, b) = match_topology(&ring(0.0, n), &ring(0.7, n)).expect("closed morphs");
    assert_eq!(a.anchors.len(), n);
    assert_eq!(b.anchors.len(), n);
    assert!(finite(&a) && finite(&b));
}
