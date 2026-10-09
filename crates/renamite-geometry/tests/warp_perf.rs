use glam::DVec2;
use kurbo::{BezPath, Rect};
use renamite_geometry::{WarpPin, warp_bandwidth, warp_mesh, warp_solve};

fn square(size: f64) -> BezPath {
    let mut path = BezPath::new();
    path.move_to(kurbo::Point::new(0.0, 0.0));
    path.line_to(kurbo::Point::new(size, 0.0));
    path.line_to(kurbo::Point::new(size, size));
    path.line_to(kurbo::Point::new(0.0, size));
    path.close_path();
    path
}

/// The warp is solved per frame, so its cost has to stay linear in vertices.
/// That holds only while a detail row reaches just one column either way: a row
/// spanning the whole mesh would make the solve cost the square of the art.
#[test]
fn a_full_grid_solves_in_the_grids_band() {
    let cells = 48.0;
    let spacing = 30.0;
    let size = cells * spacing;
    let mesh = warp_mesh(&square(size), Rect::new(0.0, 0.0, size, size), spacing).unwrap();
    assert!(mesh.vertices.len() <= renamite_geometry::MAX_WARP_VERTICES);

    let pins = vec![
        WarpPin {
            rest: DVec2::new(spacing, spacing),
            at: DVec2::new(3.0 * spacing, spacing),
            angle: None,
        },
        WarpPin {
            rest: DVec2::new(size - spacing, size - spacing),
            at: DVec2::new(size - 3.0 * spacing, size - spacing),
            angle: None,
        },
    ];

    let band = warp_bandwidth(&mesh, &pins);
    assert!(
        band <= 2 * cells as usize + 8,
        "band {band} is wider than twice the {cells}-column grid: the solve would cost the square of the mesh"
    );

    let solved = warp_solve(&mesh, &pins);
    assert!(solved.iter().all(|point| point.is_finite()));
    let moved = solved
        .iter()
        .zip(&mesh.vertices)
        .filter(|(moved, rest)| (*moved - *rest).length() > 1e-6)
        .count();
    assert!(
        moved > mesh.vertices.len() / 2,
        "pins moved {moved} of {} vertices",
        mesh.vertices.len()
    );
}
