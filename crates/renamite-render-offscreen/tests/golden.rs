//! Golden-image tests: fixture docs (built in code) → Player/evaluate →
//! SceneRenderer → OffscreenRenderer → PNG, compared against committed
//! goldens with per-channel tolerance.
//!
//! Regenerate: RENAMITE_BLESS=1 cargo test -p renamite-render-offscreen --test golden
//! Skips (with a warning) when no WGPU adapter is available.

use glam::DVec2;
use renamite_animation::{Animated, EasingHandle, EasingPreset, Frame, Interpolation};
use renamite_behavior_common::ViewTransform;
use renamite_geometry::{Anchor, VectorPath};
use renamite_model::{
    AnimatedDash, Color, Document, FillRule, GradientKind, GradientStop, GradientStops,
    KeyframeData, ModifierKind, Node, NodeKind, Parent, PropPath, ShapeKind, StrokeCap, StrokeJoin,
    StyleKind, StylePaint, TrimMode, Value, evaluate,
};
use renamite_render_bridge::SceneRenderer;
use renamite_render_offscreen::{OffscreenRenderer, fit_view};
use std::path::PathBuf;

const SIZE: u32 = 256;
/// Per-channel deviation below this is "same pixel".
const CHANNEL_TOL: i16 = 8;
/// Fraction of pixels allowed to exceed CHANNEL_TOL (AA variance headroom).
const MAX_DIFF_FRACTION: f64 = 0.005;

fn goldens_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/goldens")
}

fn gpu() -> Option<OffscreenRenderer> {
    match OffscreenRenderer::new_blocking(SIZE, SIZE, 4) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("skipping golden test: {e}");
            None
        }
    }
}

fn render_doc(gpu: &mut OffscreenRenderer, doc: &Document, frame: f64) -> Vec<u8> {
    let comp = doc.main;
    let comp_size = doc.compositions[comp].size;
    let scene = evaluate(doc, comp, frame);
    let view: ViewTransform = fit_view(comp_size, SIZE, SIZE);
    let mut bridge = SceneRenderer::new();
    let prepared = bridge.prepare(&scene, &view);
    let mut repose = repose_core::Scene::default();
    bridge.append_repose_scene(&prepared, &mut repose);
    gpu.render_png(&repose, Some([1.0, 1.0, 1.0, 1.0]))
        .expect("render")
}

fn check_golden(name: &str, actual_png: &[u8]) {
    let path = goldens_dir().join(format!("{name}.png"));
    let bless = std::env::var("RENAMITE_BLESS").is_ok();

    if bless || !path.exists() {
        std::fs::create_dir_all(goldens_dir()).unwrap();
        std::fs::write(&path, actual_png).unwrap();
        if !bless {
            panic!("{name}: golden did not exist; wrote initial golden. Re-run to verify.");
        }
        return;
    }

    let golden = image::load_from_memory(&std::fs::read(&path).unwrap())
        .unwrap()
        .to_rgba8();
    let actual = image::load_from_memory(actual_png).unwrap().to_rgba8();
    assert_eq!(
        golden.dimensions(),
        actual.dimensions(),
        "{name}: size changed"
    );

    let total = (golden.width() * golden.height()) as f64;
    let mut differing = 0usize;
    for (g, a) in golden.pixels().zip(actual.pixels()) {
        let over_tol =
            g.0.iter()
                .zip(a.0.iter())
                .any(|(&gc, &ac)| (gc as i16 - ac as i16).abs() > CHANNEL_TOL);
        if over_tol {
            differing += 1;
        }
    }
    let fraction = differing as f64 / total;
    if fraction > MAX_DIFF_FRACTION {
        let actual_path = goldens_dir().join(format!("{name}.actual.png"));
        std::fs::write(&actual_path, actual_png).unwrap();
        panic!(
            "{name}: {differing} px ({:.3}%) differ beyond tol (limit {:.3}%). \
             Actual written to {}. Bless with RENAMITE_BLESS=1 if intentional.",
            fraction * 100.0,
            MAX_DIFF_FRACTION * 100.0,
            actual_path.display(),
        );
    }
}

fn linear_key(frame: i64, value: Value) -> KeyframeData {
    KeyframeData {
        frame: Frame(frame),
        value,
        interpolation: Interpolation::Linear,
        ease_out: EasingHandle::LINEAR_OUT,
        ease_in: EasingHandle::LINEAR_IN,
    }
}

/// Centered orange ellipse - the basic fill path.
fn fixture_ellipse() -> Document {
    let mut doc = Document::empty();
    let comp = doc.main;
    let (w, h) = doc.compositions[comp].size;
    let shape = doc.create_node(Node::new(
        "e",
        NodeKind::Shape(ShapeKind::Ellipse {
            pos: Animated::new(DVec2::new(w as f64 / 2.0, h as f64 / 2.0)),
            size: Animated::new(DVec2::new(240.0, 160.0)),
        }),
    ));
    let fill = doc.create_node(Node::new(
        "f",
        NodeKind::Style(StyleKind::Fill {
            paint: StylePaint::solid(Color::rgba(0.96, 0.42, 0.18, 1.0)),
            rule: FillRule::NonZero,
        }),
    ));
    doc.attach(shape, Parent::Comp(comp), 0).unwrap();
    doc.attach(fill, Parent::Comp(comp), 1).unwrap();
    doc
}

fn fixture_star() -> Document {
    let mut doc = Document::empty();
    let comp = doc.main;
    let (w, h) = doc.compositions[comp].size;
    let shape = doc.create_node(Node::new(
        "st",
        NodeKind::Shape(ShapeKind::Star {
            pos: Animated::new(DVec2::new(w as f64 / 2.0, h as f64 / 2.0)),
            points: Animated::new(5.0),
            inner_r: Animated::new(48.0),
            outer_r: Animated::new(110.0),
            roundness: Animated::new(0.0),
            kind: renamite_model::StarKind::Star,
        }),
    ));
    let fill = doc.create_node(Node::new(
        "f",
        NodeKind::Style(StyleKind::Fill {
            paint: StylePaint::solid(Color::rgba(0.96, 0.42, 0.18, 1.0)),
            rule: FillRule::NonZero,
        }),
    ));
    doc.attach(shape, Parent::Comp(comp), 0).unwrap();
    doc.attach(fill, Parent::Comp(comp), 1).unwrap();
    doc
}

/// Rect with a wide round-capped stroke - the stroke tessellation path.
fn fixture_stroke() -> Document {
    let mut doc = Document::empty();
    let comp = doc.main;
    let shape = doc.create_node(Node::new(
        "r",
        NodeKind::Shape(ShapeKind::Rect {
            pos: Animated::new(DVec2::new(256.0, 256.0)),
            size: Animated::new(DVec2::new(200.0, 200.0)),
            rounded: Animated::new(0.0),
        }),
    ));
    let stroke = doc.create_node(Node::new(
        "s",
        NodeKind::Style(StyleKind::Stroke {
            paint: StylePaint::solid(Color::rgba(0.1, 0.3, 0.9, 1.0)),
            width: Animated::new(24.0),
            cap: StrokeCap::Round,
            join: StrokeJoin::Round,
            dash: None,
        }),
    ));
    doc.attach(shape, Parent::Comp(comp), 0).unwrap();
    doc.attach(stroke, Parent::Comp(comp), 1).unwrap();
    doc
}

/// Position animated 100→400 over 60 frames with EaseInOut, sampled at 30 -
/// pins the cubic-bezier easing solve, not just endpoints.
fn fixture_eased_midpoint() -> Document {
    let mut doc = fixture_ellipse();
    let shape = doc.compositions[doc.main].children[0];
    let prop = PropPath::new("transform.position");
    let (i, o, e) = EasingPreset::EaseInOut.segment();
    let mut k0 = linear_key(0, Value::DVec2(DVec2::new(-150.0, 0.0)));
    k0.interpolation = i;
    k0.ease_out = o;
    k0.ease_in = e;
    doc.restore_keyframe(shape, &prop, &k0).unwrap();
    doc.restore_keyframe(
        shape,
        &prop,
        &linear_key(60, Value::DVec2(DVec2::new(150.0, 0.0))),
    )
    .unwrap();
    doc
}

/// A pen-style path with symmetric tangents - pins bezier flattening.
fn fixture_bezier_path() -> Document {
    let mut doc = Document::empty();
    let comp = doc.main;
    let path = VectorPath {
        closed: true,
        anchors: vec![
            Anchor::symmetric(DVec2::new(256.0, 100.0), DVec2::new(90.0, 0.0)),
            Anchor::symmetric(DVec2::new(400.0, 256.0), DVec2::new(0.0, 90.0)),
            Anchor::symmetric(DVec2::new(256.0, 412.0), DVec2::new(-90.0, 0.0)),
            Anchor::symmetric(DVec2::new(112.0, 256.0), DVec2::new(0.0, -90.0)),
        ],
    };
    let shape = doc.create_node(Node::new(
        "p",
        NodeKind::Shape(ShapeKind::Path(Animated::new(path))),
    ));
    let fill = doc.create_node(Node::new(
        "f",
        NodeKind::Style(StyleKind::Fill {
            paint: StylePaint::solid(Color::rgba(0.2, 0.7, 0.4, 1.0)),
            rule: FillRule::NonZero,
        }),
    ));
    doc.attach(shape, Parent::Comp(comp), 0).unwrap();
    doc.attach(fill, Parent::Comp(comp), 1).unwrap();
    doc
}

/// Square stroke trimmed to the first half of its perimeter.
fn fixture_trim_half() -> Document {
    let mut doc = Document::empty();
    let comp = doc.main;
    let group = doc.create_node(Node::new("g", NodeKind::Group));

    let shape = doc.create_node(Node::new(
        "r",
        NodeKind::Shape(ShapeKind::Rect {
            pos: Animated::new(DVec2::new(256.0, 256.0)),
            size: Animated::new(DVec2::new(200.0, 200.0)),
            rounded: Animated::new(0.0),
        }),
    ));
    let trim = doc.create_node(Node::new(
        "t",
        NodeKind::Modifier(ModifierKind::TrimPath {
            start: Animated::new(0.0),
            end: Animated::new(0.5),
            offset: Animated::new(0.0),
            mode: TrimMode::Individually,
        }),
    ));
    let stroke = doc.create_node(Node::new(
        "s",
        NodeKind::Style(StyleKind::Stroke {
            paint: StylePaint::solid(Color::rgba(0.1, 0.3, 0.9, 1.0)),
            width: Animated::new(16.0),
            cap: StrokeCap::Round,
            join: StrokeJoin::Round,
            dash: None,
        }),
    ));

    doc.attach(shape, Parent::Node(group), 0).unwrap();
    doc.attach(trim, Parent::Node(group), 1).unwrap();
    doc.attach(stroke, Parent::Node(group), 2).unwrap();
    doc.attach(group, Parent::Comp(comp), 0).unwrap();
    doc
}

/// `end` animated 0 → 1 over 60 frames; sampled at 30 pins the animated read
/// path through Animated::value_at at a non-boundary frame.
fn fixture_trim_animated() -> Document {
    let mut doc = fixture_trim_half();
    let group = doc.compositions[doc.main].children[0];
    let trim = doc.nodes[group].children[1];
    let prop = PropPath::new("trim.end");
    doc.add_keyframe(trim, &prop, Frame(0), &Value::F64(0.0))
        .unwrap();
    doc.add_keyframe(trim, &prop, Frame(60), &Value::F64(1.0))
        .unwrap();
    doc
}

/// Rect with a RoundCorners modifier applied before the fill - pins the
/// anchor-level rounding round-trip (from_bez_path -> round_corners ->
/// to_bez_path) through the modifier pipeline.
fn fixture_round_corners() -> Document {
    let mut doc = Document::empty();
    let comp = doc.main;
    let group = doc.create_node(Node::new("g", NodeKind::Group));

    let shape = doc.create_node(Node::new(
        "r",
        NodeKind::Shape(ShapeKind::Rect {
            pos: Animated::new(DVec2::new(256.0, 256.0)),
            size: Animated::new(DVec2::new(200.0, 140.0)),
            rounded: Animated::new(0.0),
        }),
    ));
    let round = doc.create_node(Node::new(
        "rc",
        NodeKind::Modifier(ModifierKind::RoundCorners {
            radius: Animated::new(28.0),
        }),
    ));
    let fill = doc.create_node(Node::new(
        "f",
        NodeKind::Style(StyleKind::Fill {
            paint: StylePaint::solid(Color::rgba(0.3, 0.6, 0.9, 1.0)),
            rule: FillRule::NonZero,
        }),
    ));

    doc.attach(shape, Parent::Node(group), 0).unwrap();
    doc.attach(round, Parent::Node(group), 1).unwrap();
    doc.attach(fill, Parent::Node(group), 2).unwrap();
    doc.attach(group, Parent::Comp(comp), 0).unwrap();
    doc
}

/// Rect filled with a linear gradient, left (white) → right (blue).
fn fixture_linear_gradient() -> Document {
    let mut doc = Document::empty();
    let comp = doc.main;
    let shape = doc.create_node(Node::new(
        "r",
        NodeKind::Shape(ShapeKind::Rect {
            pos: Animated::new(DVec2::new(256.0, 256.0)),
            size: Animated::new(DVec2::new(240.0, 180.0)),
            rounded: Animated::new(0.0),
        }),
    ));
    let fill = doc.create_node(Node::new(
        "f",
        NodeKind::Style(StyleKind::Fill {
            paint: StylePaint::Gradient(renamite_model::Gradient {
                kind: GradientKind::Linear,
                start: Animated::new(DVec2::new(136.0, 256.0)),
                end: Animated::new(DVec2::new(376.0, 256.0)),
                stops: Animated::new(GradientStops(vec![
                    GradientStop {
                        offset: 0.0,
                        color: Color::rgba(1.0, 1.0, 1.0, 1.0),
                    },
                    GradientStop {
                        offset: 1.0,
                        color: Color::rgba(0.1, 0.2, 0.9, 1.0),
                    },
                ])),
            }),
            rule: FillRule::NonZero,
        }),
    ));
    doc.attach(shape, Parent::Comp(comp), 0).unwrap();
    doc.attach(fill, Parent::Comp(comp), 1).unwrap();
    doc
}

/// Ellipse filled with a radial gradient, center (orange) → edge (transparent).
fn fixture_radial_gradient() -> Document {
    let mut doc = Document::empty();
    let comp = doc.main;
    let shape = doc.create_node(Node::new(
        "e",
        NodeKind::Shape(ShapeKind::Ellipse {
            pos: Animated::new(DVec2::new(256.0, 256.0)),
            size: Animated::new(DVec2::new(240.0, 240.0)),
        }),
    ));
    let fill = doc.create_node(Node::new(
        "f",
        NodeKind::Style(StyleKind::Fill {
            paint: StylePaint::Gradient(renamite_model::Gradient {
                kind: GradientKind::Radial,
                start: Animated::new(DVec2::new(256.0, 256.0)),
                end: Animated::new(DVec2::new(376.0, 256.0)),
                stops: Animated::new(GradientStops(vec![
                    GradientStop {
                        offset: 0.0,
                        color: Color::rgba(0.96, 0.42, 0.18, 1.0),
                    },
                    GradientStop {
                        offset: 1.0,
                        color: Color::rgba(0.96, 0.42, 0.18, 0.0),
                    },
                ])),
            }),
            rule: FillRule::NonZero,
        }),
    ));
    doc.attach(shape, Parent::Comp(comp), 0).unwrap();
    doc.attach(fill, Parent::Comp(comp), 1).unwrap();
    doc
}

#[test]
fn golden_ellipse_fill() {
    let Some(mut gpu) = gpu() else { return };
    check_golden(
        "ellipse_fill",
        &render_doc(&mut gpu, &fixture_ellipse(), 0.0),
    );
}

#[test]
fn golden_star() {
    let Some(mut gpu) = gpu() else { return };
    check_golden("star_fill", &render_doc(&mut gpu, &fixture_star(), 0.0));
}

#[test]
fn golden_stroke_round() {
    let Some(mut gpu) = gpu() else { return };
    check_golden(
        "stroke_round",
        &render_doc(&mut gpu, &fixture_stroke(), 0.0),
    );
}

#[test]
fn golden_eased_midpoint_frame30() {
    let Some(mut gpu) = gpu() else { return };
    check_golden(
        "eased_mid_f30",
        &render_doc(&mut gpu, &fixture_eased_midpoint(), 30.0),
    );
}

#[test]
fn golden_bezier_path() {
    let Some(mut gpu) = gpu() else { return };
    check_golden(
        "bezier_path",
        &render_doc(&mut gpu, &fixture_bezier_path(), 0.0),
    );
}

#[test]
fn golden_trim_half() {
    let Some(mut gpu) = gpu() else { return };
    check_golden(
        "trim_half",
        &render_doc(&mut gpu, &fixture_trim_half(), 0.0),
    );
}

#[test]
fn golden_trim_animated_midpoint() {
    let Some(mut gpu) = gpu() else { return };
    check_golden(
        "trim_anim_mid",
        &render_doc(&mut gpu, &fixture_trim_animated(), 30.0),
    );
}

#[test]
fn golden_round_corners() {
    let Some(mut gpu) = gpu() else { return };
    check_golden(
        "round_corners",
        &render_doc(&mut gpu, &fixture_round_corners(), 0.0),
    );
}

#[test]
fn golden_linear_gradient() {
    let Some(mut gpu) = gpu() else { return };
    check_golden(
        "linear_gradient",
        &render_doc(&mut gpu, &fixture_linear_gradient(), 0.0),
    );
}

#[test]
fn golden_radial_gradient() {
    let Some(mut gpu) = gpu() else { return };
    check_golden(
        "radial_gradient",
        &render_doc(&mut gpu, &fixture_radial_gradient(), 0.0),
    );
}

/// A horizontal line with a round-capped dashed stroke - pins the dash ->
/// open-subpath -> Lyon caps pipeline.
fn fixture_dashed_stroke() -> Document {
    let mut document = Document::empty();
    let comp = document.main;

    let path = VectorPath {
        closed: false,
        anchors: vec![
            Anchor::corner(DVec2::new(80.0, 256.0)),
            Anchor::corner(DVec2::new(432.0, 256.0)),
        ],
    };

    let shape = document.create_node(Node::new(
        "Line",
        NodeKind::Shape(ShapeKind::Path(Animated::new(path))),
    ));

    let stroke = document.create_node(Node::new(
        "Dashed Stroke",
        NodeKind::Style(StyleKind::Stroke {
            paint: StylePaint::solid(Color::rgba(0.1, 0.3, 0.9, 1.0)),
            width: Animated::new(18.0),
            cap: StrokeCap::Round,
            join: StrokeJoin::Round,
            dash: Some(AnimatedDash {
                dashes: vec![Animated::new(32.0), Animated::new(18.0)],
                offset: Animated::new(0.0),
            }),
        }),
    ));

    document.attach(shape, Parent::Comp(comp), 0).unwrap();

    document.attach(stroke, Parent::Comp(comp), 1).unwrap();

    document
}

#[test]
fn golden_dashed_stroke() {
    let Some(mut gpu) = gpu() else { return };

    check_golden(
        "dashed_stroke",
        &render_doc(&mut gpu, &fixture_dashed_stroke(), 0.0),
    );
}
