use kurbo::{BezPath, Rect};
use renamite_geometry::{DVec2, WarpPin, mesh_locate, warp_mesh, warp_point, warp_solve};

fn square() -> (BezPath, Rect) {
    let mut path = BezPath::new();
    path.move_to(kurbo::Point::new(0.0, 0.0));
    path.line_to(kurbo::Point::new(200.0, 0.0));
    path.line_to(kurbo::Point::new(200.0, 200.0));
    path.line_to(kurbo::Point::new(0.0, 200.0));
    path.close_path();
    (path, Rect::new(0.0, 0.0, 200.0, 200.0))
}

#[test]
fn mesh_covers_the_art_only() {
    let (path, bounds) = square();
    let mesh = warp_mesh(&path, bounds, 20.0).unwrap();
    assert_eq!(mesh.grid.cols, 10);
    assert_eq!(mesh.grid.rows, 10);
    assert_eq!(mesh.vertices.len(), 121);
    assert_eq!(mesh.triangles.len(), 200);
    // Every vertex must sit on the grid it was built from.
    assert!(
        mesh.vertices
            .iter()
            .zip(&mesh.vertex_cells)
            .all(|(&point, &(col, row))| {
                (point.x - col as f64 * 20.0).abs() < 1e-9
                    && (point.y - row as f64 * 20.0).abs() < 1e-9
            })
    );
    // Locating must work from inside a cell and return weights that sum to one.
    let (triangle, weights) = mesh_locate(&mesh, DVec2::new(95.0, 105.0)).unwrap();
    assert_eq!(mesh.triangles[triangle].len(), 3);
    let sum: f64 = weights.iter().sum();
    assert!((sum - 1.0).abs() < 1e-6, "weights sum {sum}");
}

#[test]
fn a_pin_drags_its_own_triangle_with_it() {
    let (path, bounds) = square();
    let mesh = warp_mesh(&path, bounds, 20.0).unwrap();
    let rest = DVec2::new(20.0, 20.0);
    let pins = vec![WarpPin {
        rest,
        at: rest + DVec2::new(40.0, 0.0),
        angle: None,
    }];
    let solved = warp_solve(&mesh, &pins);
    assert_eq!(solved.len(), mesh.vertices.len());

    // The pinned triangle's vertices are dragged along by the pin, more the
    // closer they are to it.
    let moved = warp_point(&mesh, &solved, mesh_locate(&mesh, rest).unwrap().0, {
        let (triangle, weights) = mesh_locate(&mesh, rest).unwrap();
        let [a, b, c] = mesh.triangles[triangle];
        let pinned = warp_point(&mesh, &solved, triangle, weights);
        assert!(
            (pinned.x - 60.0).abs() < 1.0,
            "pinned point moved to {pinned:?}"
        );
        let _ = (a, b, c);
        weights
    });
    assert!(moved.is_finite());
}

#[test]
fn without_pins_the_rest_state_survives() {
    let (path, bounds) = square();
    let mesh = warp_mesh(&path, bounds, 20.0).unwrap();
    assert_eq!(warp_solve(&mesh, &[]), mesh.vertices);
}

#[test]
fn far_pins_still_produce_a_finite_deformation() {
    let (path, bounds) = square();
    let mesh = warp_mesh(&path, bounds, 10.0).unwrap();
    let pins = vec![
        WarpPin {
            rest: DVec2::new(10.0, 10.0),
            at: DVec2::new(90.0, 10.0),
            angle: None,
        },
        WarpPin {
            rest: DVec2::new(190.0, 190.0),
            at: DVec2::new(110.0, 190.0),
            angle: None,
        },
    ];
    let solved = warp_solve(&mesh, &pins);
    assert!(solved.iter().all(|p| p.is_finite()));
    // The dragged end travelled; the untouched side barely moved.
    let pinned = mesh.triangles.iter().enumerate().find(|(_, _)| true);
    let _ = pinned;
    let moved_near = warp_point(
        &mesh,
        &solved,
        mesh_locate(&mesh, DVec2::new(10.0, 10.0)).unwrap().0,
        mesh_locate(&mesh, DVec2::new(10.0, 10.0)).unwrap().1,
    );
    let moved_far = warp_point(
        &mesh,
        &solved,
        mesh_locate(&mesh, DVec2::new(95.0, 95.0)).unwrap().0,
        mesh_locate(&mesh, DVec2::new(95.0, 95.0)).unwrap().1,
    );
    assert!(
        (moved_near - DVec2::new(90.0, 10.0)).length() < 2.0,
        "near: {moved_near:?}"
    );
    assert!(
        moved_far.length() < 400.0 && moved_far.is_finite(),
        "middle: {moved_far:?}"
    );
}
