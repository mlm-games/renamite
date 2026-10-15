use kurbo::BezPath;
use renamite_animation::FrameRate;
use renamite_io_raster::{RasterMode, RasterOptions, import_with_report};
use renamite_model::{FillRule, NodeKind, PaintKind, SceneItem, ScenePaint, ShapeKind};

fn cell(w: u32, h: u32, paint: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let color = paint(x, y);
            let at = (y * w + x) as usize * 4;
            out[at..at + 4].copy_from_slice(&color);
        }
    }
    out
}

fn png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::ImageEncoder::write_image(
        image::codecs::png::PngEncoder::new(&mut bytes),
        rgba,
        w,
        h,
        image::ExtendedColorType::Rgba8,
    )
    .unwrap();
    bytes
}

fn trace(rgba: &[u8], w: u32, h: u32, options: &RasterOptions) -> renamite_model::Document {
    let report = import_with_report(&png(w, h, rgba), options).unwrap();
    assert!(report.warnings.is_empty(), "unexpected warnings");
    report.value
}

fn items(document: &renamite_model::Document, frame: f64) -> Vec<SceneItem> {
    renamite_model::evaluate(document, document.main, frame).items
}

/// Even-odd scanline fill at pixel centres. Traced contours are axis-aligned
/// polylines on integer coordinates, so a sample at y+0.5 never lands on an
/// edge and the crossing parity is exact.
fn paint(path: &BezPath, w: u32, h: u32, color: [u8; 4], out: &mut Vec<[u8; 4]>) {
    let mut segments: Vec<((f64, f64), (f64, f64))> = Vec::new();
    let mut cursor = (0.0f64, 0.0f64);
    let mut origin = (0.0f64, 0.0f64);
    for element in path.elements() {
        // Traced anchors are corners with zero tangents, so renamite emits each
        // straight side as a degenerate cubic that traces exactly the segment.
        let end = match element {
            kurbo::PathEl::MoveTo(p) => {
                origin = (p.x, p.y);
                cursor = origin;
                continue;
            }
            kurbo::PathEl::LineTo(p) => (p.x, p.y),
            kurbo::PathEl::CurveTo(_, _, p) => (p.x, p.y),
            kurbo::PathEl::ClosePath => {
                if cursor != origin {
                    segments.push((cursor, origin));
                    cursor = origin;
                }
                continue;
            }
            other => panic!("unexpected path element {other:?}"),
        };
        segments.push((cursor, end));
        cursor = end;
    }
    if cursor != origin {
        segments.push((cursor, origin));
    }

    for y in 0..h {
        let sample = f64::from(y) + 0.5;
        let mut crossings: Vec<f64> = segments
            .iter()
            .filter(|((_, ay), (_, by))| (*ay > sample) != (*by > sample))
            .map(|((ax, ay), (bx, by))| ax + (sample - ay) * (bx - ax) / (by - ay))
            .collect();
        crossings.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for span in crossings.chunks(2) {
            let (Some(lo), Some(hi)) = (span.first(), span.last()) else {
                continue;
            };
            let from = lo.ceil().max(0.0) as u32;
            let to = hi.ceil().min(f64::from(w)) as u32;
            for x in from..to {
                out[(y * w + x) as usize] = color;
            }
        }
    }
}

fn assert_round_trips(document: &renamite_model::Document, w: u32, h: u32, rgba: &[u8]) {
    let mut got = vec![[0u8, 0, 0, 0]; (w * h) as usize];
    for item in &items(document, 0.0) {
        let PaintKind::Fill(FillRule::EvenOdd) = item.kind else {
            panic!("traced regions must fill with even-odd");
        };
        let ScenePaint::Solid(color) = item.paint else {
            continue;
        };
        let channel = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        paint(
            &item.path,
            w,
            h,
            [
                channel(color.r),
                channel(color.g),
                channel(color.b),
                channel(color.a * item.opacity),
            ],
            &mut got,
        );
    }
    let want: Vec<[u8; 4]> = (0..(w * h) as usize)
        .map(|p| {
            let at = p * 4;
            [rgba[at], rgba[at + 1], rgba[at + 2], rgba[at + 3]]
        })
        .collect();
    assert_eq!(got, want);
}

#[test]
fn solid_square_is_one_rectangle() {
    let rgba = cell(4, 4, |_, _| [200, 30, 40, 255]);
    let items = items(&trace(&rgba, 4, 4, &RasterOptions::default()), 0.0);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].path.elements().len(), 6);
    assert_round_trips(&trace(&rgba, 4, 4, &RasterOptions::default()), 4, 4, &rgba);
}

#[test]
fn transparency_is_not_drawn() {
    let mut rgba = cell(4, 4, |_, _| [0, 0, 0, 0]);
    rgba[(1 * 4 + 1) as usize * 4..][..4].copy_from_slice(&[10, 20, 30, 255]);
    assert_round_trips(&trace(&rgba, 4, 4, &RasterOptions::default()), 4, 4, &rgba);
}

#[test]
fn checkerboard_round_trips() {
    let rgba = cell(8, 8, |x, y| {
        if (x + y) % 2 == 0 {
            [255, 255, 255, 255]
        } else {
            [0, 0, 0, 255]
        }
    });
    assert_round_trips(&trace(&rgba, 8, 8, &RasterOptions::default()), 8, 8, &rgba);
}

#[test]
fn diagonal_saddle_splits_into_two_regions() {
    let rgba = cell(2, 2, |x, y| {
        if x == y {
            [255, 0, 0, 255]
        } else {
            [0, 0, 255, 255]
        }
    });
    let items = items(&trace(&rgba, 2, 2, &RasterOptions::default()), 0.0);
    assert_eq!(items.len(), 2, "a saddle must not weld the two diagonals");
    for item in &items {
        assert_eq!(
            item.path.elements().len(),
            12,
            "each colour keeps its two diagonal unit squares"
        );
    }
}

#[test]
fn hole_in_a_ring_is_even_odd() {
    let rgba = cell(5, 5, |x, y| {
        if x == 0 || y == 0 || x == 4 || y == 4 {
            [255, 255, 255, 255]
        } else {
            [0, 0, 0, 0]
        }
    });
    let items = items(&trace(&rgba, 5, 5, &RasterOptions::default()), 0.0);
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].path.elements().len(),
        12,
        "outer ring plus inner hole, each move + four sides + close"
    );
    assert_round_trips(&trace(&rgba, 5, 5, &RasterOptions::default()), 5, 5, &rgba);
}

#[test]
fn collinear_runs_collapse() {
    let rgba = cell(16, 4, |_x, y| {
        if y == 0 || y == 3 {
            [1, 2, 3, 255]
        } else {
            [4, 5, 6, 255]
        }
    });
    let items = items(&trace(&rgba, 16, 4, &RasterOptions::default()), 0.0);
    assert_eq!(items.len(), 2);
    assert_eq!(
        items[0].path.elements().len(),
        6,
        "a full-width band is four corners"
    );
}

#[test]
fn stepped_staircase_is_exact() {
    let rgba = cell(6, 6, |x, y| {
        if x + y < 5 {
            [255, 255, 0, 255]
        } else {
            [0, 0, 0, 0]
        }
    });
    assert_round_trips(&trace(&rgba, 6, 6, &RasterOptions::default()), 6, 6, &rgba);
}

#[test]
fn partial_alpha_is_carried_into_the_fill() {
    let rgba = cell(3, 3, |_, _| [10, 20, 30, 128]);
    let items = items(&trace(&rgba, 3, 3, &RasterOptions::default()), 0.0);
    assert_eq!(items.len(), 1);
    let ScenePaint::Solid(color) = items[0].paint else {
        panic!("expected a solid fill");
    };
    assert!(
        (color.a - 128.0 / 255.0).abs() < 0.01,
        "alpha was {}",
        color.a
    );
}

#[test]
fn sprite_sheet_splits_into_frames() {
    let frame = cell(4, 4, |x, _| [x as u8 * 60, 0, 0, 255]);
    let mut rgba = vec![0u8; 4 * 4 * 2 * 4];
    rgba[..64].copy_from_slice(&frame);
    rgba[64..].copy_from_slice(&frame);
    let options = RasterOptions {
        cell: Some((4, 4)),
        rate: FrameRate { num: 8, den: 1 },
        ..RasterOptions::default()
    };
    let document = trace(&rgba, 8, 4, &options);
    assert_eq!(items(&document, 0.0).len(), 4);
    assert_eq!(items(&document, 1.0).len(), 4);
    assert_eq!(
        document.main_composition().unwrap().rate,
        FrameRate { num: 8, den: 1 }
    );
}

#[test]
fn embed_mode_keeps_pixels_as_image_nodes() {
    let rgba = cell(4, 4, |_, _| [9, 9, 9, 255]);
    let options = RasterOptions {
        mode: RasterMode::Embed,
        ..RasterOptions::default()
    };
    let document = trace(&rgba, 4, 4, &options);
    assert_eq!(document.assets.len(), 1);
    let items = items(&document, 0.0);
    assert_eq!(items.len(), 1);
    assert!(
        matches!(items[0].paint, ScenePaint::Image { .. }),
        "embed mode must not contour"
    );
    let shapes = document
        .nodes
        .iter()
        .filter(|(_, node)| matches!(node.kind, NodeKind::Shape(_)))
        .count();
    assert_eq!(shapes, 0, "embed mode must build no shape nodes");
}

#[test]
fn over_budget_frame_falls_back_to_raster() {
    let rgba = cell(16, 16, |x, y| [(x * 16) as u8, (y * 16) as u8, 0, 255]);
    let options = RasterOptions {
        max_colors_per_frame: Some(8),
        ..RasterOptions::default()
    };
    let report = import_with_report(&png(16, 16, &rgba), &options).unwrap();
    assert_eq!(report.warnings.len(), 1);
    assert_eq!(report.value.assets.len(), 1);
    let items = items(&report.value, 0.0);
    assert!(matches!(items[0].paint, ScenePaint::Image { .. }));
}

#[test]
fn traced_document_validates() {
    let rgba = cell(6, 6, |x, y| {
        if x < 3 {
            [255, 0, 0, 255]
        } else if y < 3 {
            [0, 255, 0, 255]
        } else {
            [0, 0, 255, 128]
        }
    });
    let file =
        renamite_io_ren::RenFile::new(trace(&rgba, 6, 6, &RasterOptions::default()), "raster");
    let report = renamite_validate::validate(&file);
    assert!(!report.has_errors(), "errors: {:?}", report.diagnostics);
}

#[test]
fn generated_document_survives_ren_round_trip() {
    let rgba = cell(8, 8, |x, y| {
        if (x * y) % 3 == 0 {
            [255, 128, 0, 255]
        } else {
            [0, 64, 200, 255]
        }
    });
    let document = trace(&rgba, 8, 8, &RasterOptions::default());
    let text = renamite_io_ren::save(&renamite_io_ren::RenFile::new(document, "raster")).unwrap();
    let reopened = renamite_io_ren::open(&text).unwrap();
    assert_round_trips(&reopened.document, 8, 8, &rgba);
}

#[test]
fn traced_shapes_are_compound_paths() {
    let rgba = cell(4, 4, |_, _| [1, 1, 1, 255]);
    let document = trace(&rgba, 4, 4, &RasterOptions::default());
    let shapes = document
        .nodes
        .iter()
        .filter(|(_, node)| matches!(node.kind, NodeKind::Shape(ShapeKind::CompoundPath(_))))
        .count();
    assert_eq!(shapes, 1);
}
