//! Pin warping: a mesh over the artwork that pins drag around.
//!
//! The mesh is a regular grid of cells kept where they touch the art, so the
//! solve stays small and the deformation follows the shape being warped.
//! Positions come out of Laplacian surface editing (Sorkine et al., SCA 2004):
//! every keep-it-as-it-was detail laid over the pins, as one banded
//! positive-definite system solved by Cholesky factorisation.
//!
//! Everything here is a pure function of the rest state and the pins, so a
//! frame can be solved from scratch and still agree with the last one.

use std::collections::HashMap;

use crate::{DVec2, flatten_bez_path, pt};

use kurbo::{Rect, Shape as _};

/// Maximum grid vertices one mesh may carry. The solve costs about
/// `vertices * width^2`, which is what the cap keeps bounded.
pub const MAX_WARP_VERTICES: usize = 2_500;

/// One pin: where it sat on the art, where it is dragged to now, and the turn
/// it holds. `angle` of `None` leaves the art free to turn about the pin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WarpPin {
    pub rest: DVec2,
    pub at: DVec2,
    pub angle: Option<f64>,
}

/// The grid a [`WarpMesh`] cuts its cells from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WarpGrid {
    pub origin: DVec2,
    pub spacing: f64,
    pub cols: usize,
    pub rows: usize,
}

/// A triangle mesh over a rest state, with the lookups a solve needs.
#[derive(Clone, Debug)]
pub struct WarpMesh {
    /// Rest vertex positions, row-major over the grid.
    pub vertices: Vec<DVec2>,
    /// Grid coordinate of each vertex, parallel to `vertices`.
    pub vertex_cells: Vec<(usize, usize)>,
    /// Triangles as vertex indices.
    pub triangles: Vec<[usize; 3]>,
    /// Triangles of each cell, indexed `row * cols + col`.
    pub cell_triangles: Vec<Vec<usize>>,
    /// Neighbours of each vertex, sorted.
    pub adjacency: Vec<Vec<usize>>,
    pub grid: WarpGrid,
}

/// Build the mesh of `path`: grid cells whose centre lies on or inside the art.
///
/// `bounds` caps the covered area, so a large canvas does not spend its
/// vertices on empty space. `None` when nothing is covered, or the grid would
/// exceed [`MAX_WARP_VERTICES`].
pub fn warp_mesh(path: &kurbo::BezPath, bounds: Rect, spacing: f64) -> Option<WarpMesh> {
    let spacing = if spacing.is_finite() && spacing > 1e-6 {
        spacing
    } else {
        32.0
    };
    let bounds = fit_bounds(path, bounds);
    let cols = length_cells(bounds.width(), spacing)?;
    let rows = length_cells(bounds.height(), spacing)?;
    if cells_vertices(cols, rows) > MAX_WARP_VERTICES {
        // A cell holds four corners, and neighbouring cells share them: the
        // mesh is a (cols + 1) by (rows + 1) lattice.
        return None;
    }
    let grid = WarpGrid {
        origin: DVec2::new(bounds.x0, bounds.y0),
        spacing,
        cols,
        rows,
    };

    let mut live: Vec<(usize, usize)> = Vec::new();
    for col in 0..cols {
        for row in 0..rows {
            if in_art(path, cell_centre(&grid, col, row), spacing) {
                live.push((col, row));
            }
        }
    }
    if live.is_empty() {
        return None;
    }

    // Vertices are numbered row-major over the grid, so a triangle's corner
    // indices stay close together and the solve's band stays the grid's width
    // instead of the mesh's size.
    let mut corners: std::collections::BTreeSet<(usize, usize)> = std::collections::BTreeSet::new();
    for &(col, row) in &live {
        corners.insert((col, row));
        corners.insert((col + 1, row));
        corners.insert((col, row + 1));
        corners.insert((col + 1, row + 1));
    }
    let index: HashMap<(usize, usize), usize> = corners
        .iter()
        .enumerate()
        .map(|(vertex, &corner)| (corner, vertex))
        .collect();
    let vertices: Vec<DVec2> = corners
        .iter()
        .map(|&corner| corner_point(&grid, corner))
        .collect();
    let vertex_cells: Vec<(usize, usize)> = corners.into_iter().collect();

    let offsets = [(0usize, 0usize), (1, 0), (0, 1), (1, 1)];
    let mut triangles = Vec::with_capacity(live.len() * 2);
    for &(col, row) in &live {
        let quad: Vec<usize> = offsets
            .iter()
            .map(|&(dc, dr)| index[&(col + dc, row + dr)])
            .collect();
        triangles.push([quad[0], quad[1], quad[2]]);
        triangles.push([quad[0], quad[2], quad[3]]);
    }

    let mut cell_triangles = vec![Vec::new(); cols * rows];
    for (triangle_index, triangle) in triangles.iter().enumerate() {
        let (col, row) = triangle_cell(&vertex_cells, triangle);
        cell_triangles[row * cols + col].push(triangle_index);
    }

    let adjacency = neighbours(&triangles, vertices.len());
    Some(WarpMesh {
        vertices,
        vertex_cells,
        triangles,
        cell_triangles,
        adjacency,
        grid,
    })
}

/// Cells along a side of `length` at `spacing`, or `None` when degenerate.
fn length_cells(length: f64, spacing: f64) -> Option<usize> {
    if !length.is_finite() || length < 0.0 {
        return None;
    }
    Some(((length / spacing).floor() as usize).max(1))
}

/// Vertices a `cols` by `rows` grid of cells carries.
fn cells_vertices(cols: usize, rows: usize) -> usize {
    cols.saturating_add(1)
        .saturating_mul(rows.saturating_add(1))
}

/// Shrink the cap to the art's own bounds, so a large document does not build
/// a grid mostly made of nothing.
fn fit_bounds(path: &kurbo::BezPath, bounds: Rect) -> Rect {
    let art = path.bounding_box();
    if !art.is_finite() {
        return bounds;
    }
    Rect::new(
        art.x0.max(bounds.x0),
        art.y0.max(bounds.y0),
        art.x1.min(bounds.x1),
        art.y1.min(bounds.y1),
    )
}

fn cell_centre(grid: &WarpGrid, col: usize, row: usize) -> DVec2 {
    DVec2::new(
        grid.origin.x + (col as f64 + 0.5) * grid.spacing,
        grid.origin.y + (row as f64 + 0.5) * grid.spacing,
    )
}

fn corner_point(grid: &WarpGrid, (col, row): (usize, usize)) -> DVec2 {
    DVec2::new(
        grid.origin.x + col as f64 * grid.spacing,
        grid.origin.y + row as f64 * grid.spacing,
    )
}

/// Is `p` inside the art, or within `reach` of its edge (so cells touching a
/// thin fill-less shape still belong to the mesh)?
fn in_art(path: &kurbo::BezPath, p: DVec2, reach: f64) -> bool {
    if path.winding(pt(p)) != 0 {
        return true;
    }
    let reach = if reach.is_finite() {
        reach.max(1e-6)
    } else {
        1e-6
    };
    for contour in flatten_bez_path(path, reach.min(0.25)) {
        if near_polyline(&contour.points, p, reach) {
            return true;
        }
    }
    false
}

fn near_polyline(points: &[DVec2], p: DVec2, reach: f64) -> bool {
    let reach_sq = reach * reach;
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if point_to_segment_sq(p, a, b) <= reach_sq {
            return true;
        }
    }
    false
}

fn point_to_segment_sq(p: DVec2, a: DVec2, b: DVec2) -> f64 {
    let ab = b - a;
    let len_sq = ab.length_squared();
    let t = if len_sq <= 1e-12 {
        0.0
    } else {
        ((p - a).dot(ab) / len_sq).clamp(0.0, 1.0)
    };
    let nearest = a + ab * t;
    (p - nearest).length_squared()
}

/// The grid cell a triangle was built from, which under row-major vertex
/// numbering is the cell of its lowest-numbered corner.
fn triangle_cell(vertex_cells: &[(usize, usize)], triangle: &[usize; 3]) -> (usize, usize) {
    triangle
        .iter()
        .map(|&vertex| vertex_cells[vertex])
        .min()
        .unwrap_or((0, 0))
}

/// Neighbours per vertex, sorted and deduplicated so the banded solve does not
/// depend on hash iteration order.
fn neighbours(triangles: &[[usize; 3]], count: usize) -> Vec<Vec<usize>> {
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); count];
    for triangle in triangles {
        for (i, &a) in triangle.iter().enumerate() {
            for &b in triangle.iter().skip(i + 1) {
                out[a].push(b);
                out[b].push(a);
            }
        }
    }
    for list in &mut out {
        list.sort_unstable();
        list.dedup();
    }
    out
}

/// Where a point sits in the mesh: triangle index plus barycentric weights.
///
/// Starting from the point's own grid cell keeps the search local, which is
/// what makes per-anchor mapping affordable.
pub fn mesh_locate(mesh: &WarpMesh, p: DVec2) -> Option<(usize, [f64; 3])> {
    let (col, row) = cell_of(&mesh.grid, p)?;
    for &triangle_index in mesh.cell_triangles.get(row * mesh.grid.cols + col)? {
        let [a, b, c] = mesh.triangles[triangle_index];
        if let Some(weights) = barycentric(mesh.vertices[a], mesh.vertices[b], mesh.vertices[c], p)
        {
            return Some((triangle_index, weights));
        }
    }
    None
}

fn cell_of(grid: &WarpGrid, p: DVec2) -> Option<(usize, usize)> {
    // The far edge of the grid belongs to its last cell: art and pins often
    // land exactly on the bounds the grid was cut to.
    let col = (((p.x - grid.origin.x) / grid.spacing).floor()).clamp(0.0, grid.cols as f64 - 1.0);
    let row = (((p.y - grid.origin.y) / grid.spacing).floor()).clamp(0.0, grid.rows as f64 - 1.0);
    if !col.is_finite() || !row.is_finite() || col < 0.0 || row < 0.0 {
        return None;
    }
    Some((col as usize, row as usize))
}

/// Weights of `p` inside triangle `(a, b, c)`; `None` outside it.
fn barycentric(a: DVec2, b: DVec2, c: DVec2, p: DVec2) -> Option<[f64; 3]> {
    let (v0, v1, v2) = (b - a, c - a, p - a);
    let (d00, d01, d11) = (v0.dot(v0), v0.dot(v1), v1.dot(v1));
    let (d20, d21) = (v2.dot(v0), v2.dot(v1));
    let den = d00 * d11 - d01 * d01;
    if !den.is_finite() || den.abs() <= 1e-12 {
        return None;
    }
    let (v, w) = ((d11 * d20 - d01 * d21) / den, (d00 * d21 - d01 * d20) / den);
    let u = 1.0 - v - w;
    let slack = 1e-6;
    if v < -slack || w < -slack || u < -slack {
        return None;
    }
    Some([u, v, w])
}

/// Map a rest point through a solve result.
pub fn warp_point(mesh: &WarpMesh, moved: &[DVec2], triangle: usize, weights: [f64; 3]) -> DVec2 {
    let [a, b, c] = mesh.triangles[triangle];
    moved[a] * weights[0] + moved[b] * weights[1] + moved[c] * weights[2]
}

/// Solve the mesh to its warped positions under `pins`.
///
/// With no pins, or an unsolvable system, the rest state comes back.
pub fn warp_solve(mesh: &WarpMesh, pins: &[WarpPin]) -> Vec<DVec2> {
    let n = mesh.vertices.len();
    let mut out = mesh.vertices.clone();
    if pins.is_empty() || n == 0 {
        return out;
    }
    let rows = warp_rows(mesh, pins);
    let (mut matrix, rhs_x, rhs_y) = normal_equations(&rows, n);
    if !matrix.factor() {
        return out;
    }
    let (mut x, mut y) = (rhs_x, rhs_y);
    if !matrix.solve(&mut x) || !matrix.solve(&mut y) {
        return out;
    }
    for vertex in 0..n {
        let candidate = DVec2::new(x[vertex], y[vertex]);
        if candidate.is_finite() {
            out[vertex] = candidate;
        }
    }
    out
}

/// How strongly a pin holds a triangle's vertices in place, against the
/// Laplacian detail rows.
const PIN_WEIGHT: f64 = 1e4;

/// One least-squares row: `terms` coefficients over vertices, and the result
/// the row asks for.
struct WarpRow {
    terms: Vec<(usize, f64)>,
    target_x: f64,
    target_y: f64,
}

/// Every row the warp asks for: the detail rows over all vertices, then one
/// row per pin.
fn warp_rows(mesh: &WarpMesh, pins: &[WarpPin]) -> Vec<WarpRow> {
    let mut rows = Vec::with_capacity(mesh.vertices.len() + pins.len());

    // Detail: every vertex keeps its Laplacian, its difference from the mean of
    // its neighbours. A vertex with no neighbours can only hold still.
    for (vertex, neighbours) in mesh.adjacency.iter().enumerate() {
        if neighbours.is_empty() {
            rows.push(WarpRow {
                terms: vec![(vertex, 1.0)],
                target_x: 0.0,
                target_y: 0.0,
            });
            continue;
        }
        let (neighbour_x, neighbour_y) = neighbours.iter().fold((0.0, 0.0), |(x, y), &n| {
            (x + mesh.vertices[n].x, y + mesh.vertices[n].y)
        });
        let rest = mesh.vertices[vertex];
        let mut terms = Vec::with_capacity(neighbours.len() + 1);
        terms.push((vertex, neighbours.len() as f64));
        terms.extend(neighbours.iter().map(|&neighbour| (neighbour, -1.0)));
        rows.push(WarpRow {
            terms,
            target_x: neighbours.len() as f64 * rest.x - neighbour_x,
            target_y: neighbours.len() as f64 * rest.y - neighbour_y,
        });
    }

    // Pins: the triangle under the pin's rest position moves to the pin.
    for pin in pins {
        let Some((triangle_index, weights)) = mesh_locate(mesh, pin.rest) else {
            continue;
        };
        // A pin turns the art about itself instead of only moving it.
        let mut target = pin.at;
        if let Some(angle) = pin.angle {
            let [a, b, c] = mesh.triangles[triangle_index];
            let barycentre = mesh.vertices[a] * weights[0]
                + mesh.vertices[b] * weights[1]
                + mesh.vertices[c] * weights[2];
            target = pin.at + rotate(barycentre - pin.rest, angle);
        }
        rows.push(WarpRow {
            terms: mesh.triangles[triangle_index]
                .iter()
                .zip(weights)
                .map(|(&vertex, weight)| (vertex, PIN_WEIGHT * weight))
                .collect(),
            target_x: PIN_WEIGHT * target.x,
            target_y: PIN_WEIGHT * target.y,
        });
    }

    rows
}

/// The band width the warp's solve would use, which is what its per-frame
/// cost scales with.
pub fn warp_bandwidth(mesh: &WarpMesh, pins: &[WarpPin]) -> usize {
    rows_bandwidth(&warp_rows(mesh, pins))
}

/// The widest span of vertex indices any row covers, so nothing a row touches
/// falls outside the band.
fn rows_bandwidth(rows: &[WarpRow]) -> usize {
    rows.iter()
        .map(|row| {
            let span =
                |pick: fn(&(usize, f64)) -> usize| row.terms.iter().map(pick).max().unwrap_or(0);
            span(|(vertex, _)| *vertex).saturating_sub(
                row.terms
                    .iter()
                    .map(|(vertex, _)| *vertex)
                    .min()
                    .unwrap_or(0),
            ) + 1
        })
        .max()
        .unwrap_or(1)
        .max(1)
}

/// The normal equations of `rows`, as one symmetric system both axes share.
fn normal_equations(rows: &[WarpRow], n: usize) -> (BandMatrix, Vec<f64>, Vec<f64>) {
    let bandwidth = rows_bandwidth(rows);
    let mut matrix = BandMatrix::new(n, bandwidth);
    let mut rhs_x = vec![0.0; n];
    let mut rhs_y = vec![0.0; n];
    for row in rows {
        for (index, &(vertex, weight)) in row.terms.iter().enumerate() {
            // Each unordered pair once: `add` mirrors into the lower band.
            for &(other, other_weight) in &row.terms[index..] {
                matrix.add(vertex, other, weight * other_weight);
            }
            rhs_x[vertex] += weight * row.target_x;
            rhs_y[vertex] += weight * row.target_y;
        }
    }
    (matrix, rhs_x, rhs_y)
}

fn rotate(v: DVec2, angle: f64) -> DVec2 {
    DVec2::new(
        v.x * angle.cos() - v.y * angle.sin(),
        v.x * angle.sin() + v.y * angle.cos(),
    )
}

/// Banded symmetric positive-definite matrix, lower band stored row by row.
#[derive(Clone, Debug)]
pub struct BandMatrix {
    n: usize,
    bw: usize,
    /// Row `i` holds columns `i-bw..=i` at offsets `0..=bw`.
    a: Vec<f64>,
}

impl BandMatrix {
    pub fn new(n: usize, bw: usize) -> Self {
        Self {
            n,
            bw,
            a: vec![0.0; n * (bw + 1)],
        }
    }

    fn idx(&self, i: usize, j: usize) -> usize {
        i * (self.bw + 1) + (i - j)
    }

    /// Add `value` at `(i, j)`. Only the lower band is stored, so the upper
    /// half is mirrored into it.
    pub fn add(&mut self, i: usize, j: usize, value: f64) {
        if !value.is_finite() {
            return;
        }
        let (i, j) = if i < j { (j, i) } else { (i, j) };
        if j + self.bw < i {
            return;
        }
        let index = self.idx(i, j);
        self.a[index] += value;
    }

    /// Cholesky: `L * L^T = self`, stored in place in the band.
    pub fn factor(&mut self) -> bool {
        for i in 0..self.n {
            for j in self.low(i)..=i {
                let mut sum = self.get(i, j);
                for k in self.low(j)..j {
                    sum -= self.get(i, k) * self.get(j, k);
                }
                if i == j {
                    if !sum.is_finite() || sum <= 0.0 {
                        return false;
                    }
                    self.set(i, j, sum.sqrt());
                } else {
                    let diagonal = self.get(j, j);
                    if !diagonal.is_finite() || diagonal <= 0.0 {
                        return false;
                    }
                    self.set(i, j, sum / diagonal);
                }
            }
        }
        true
    }

    /// Solve in place: forward then back substitution.
    pub fn solve(&self, b: &mut [f64]) -> bool {
        let mut y = vec![0.0; self.n];
        for i in 0..self.n {
            let low = self.low(i);
            let mut sum = b[i];
            for (offset, &solved) in y[low..i].iter().enumerate() {
                sum -= self.get(i, low + offset) * solved;
            }
            let diagonal = self.get(i, i);
            if !diagonal.is_finite() || diagonal <= 0.0 {
                return false;
            }
            y[i] = sum / diagonal;
        }
        for i in (0..self.n).rev() {
            let mut sum = y[i];
            for k in (i + 1..=self.high(i)).rev() {
                sum -= self.get(k, i) * b[k];
            }
            let diagonal = self.get(i, i);
            if !diagonal.is_finite() || diagonal <= 0.0 {
                return false;
            }
            b[i] = sum / diagonal;
        }
        true
    }

    /// First column of row `i` inside the band.
    fn low(&self, i: usize) -> usize {
        i.saturating_sub(self.bw)
    }

    /// Last column of row `i` inside the band.
    fn high(&self, i: usize) -> usize {
        (i + self.bw).min(self.n.saturating_sub(1))
    }

    fn get(&self, i: usize, j: usize) -> f64 {
        if i < j || j + self.bw < i {
            return 0.0;
        }
        self.a[self.idx(i, j)]
    }

    fn set(&mut self, i: usize, j: usize, value: f64) {
        let index = self.idx(i, j);
        self.a[index] = value;
    }
}
