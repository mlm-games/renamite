use std::collections::HashMap;

const RIGHT: u8 = 0;
const DOWN: u8 = 1;
const LEFT: u8 = 2;
const UP: u8 = 3;

/// One flat colour and the closed contours that cover it exactly.
#[derive(Clone, Debug)]
pub(crate) struct Region {
    pub color: [u8; 4],
    pub contours: Vec<Vec<[f64; 2]>>,
}

struct Mask {
    w: i32,
    h: i32,
    bits: Vec<bool>,
}

impl Mask {
    fn filled(&self, x: i32, y: i32) -> bool {
        x >= 0
            && y >= 0
            && x < self.w
            && y < self.h
            && self.bits[y as usize * self.w as usize + x as usize]
    }
}

#[inline]
fn step(dir: u8) -> (i32, i32) {
    match dir {
        RIGHT => (1, 0),
        DOWN => (0, 1),
        LEFT => (-1, 0),
        _ => (0, -1),
    }
}

#[inline]
fn heading(from: (i32, i32), to: (i32, i32)) -> u8 {
    match (to.0 - from.0, to.1 - from.1) {
        (1, 0) => RIGHT,
        (0, 1) => DOWN,
        (-1, 0) => LEFT,
        _ => UP,
    }
}

/// Split one cell into flat-colour regions and contour each one.
///
/// Colours are grouped by exact RGBA and transparent pixels are dropped, so a
/// traced region covers precisely the pixels of its own colour. Every pixel
/// belongs to exactly one colour, so regions never overlap and the painter
/// order between them cannot change the result.
pub(crate) fn trace_cell(rgba: &[u8], w: u32, h: u32) -> Vec<Region> {
    let pixels = w as usize * h as usize;
    let mut palette: Vec<[u8; 4]> = Vec::new();
    let mut lookup: HashMap<[u8; 4], u32> = HashMap::new();
    let mut owner = vec![u32::MAX; pixels];

    for p in 0..pixels {
        let color = [
            rgba[p * 4],
            rgba[p * 4 + 1],
            rgba[p * 4 + 2],
            rgba[p * 4 + 3],
        ];
        if color[3] == 0 {
            continue;
        }
        let id = match lookup.get(&color) {
            Some(id) => *id,
            None => {
                let id = palette.len() as u32;
                palette.push(color);
                lookup.insert(color, id);
                id
            }
        };
        owner[p] = id;
    }

    let mut regions = Vec::with_capacity(palette.len());
    for (id, &color) in palette.iter().enumerate() {
        let id = id as u32;
        let mask = Mask {
            w: w as i32,
            h: h as i32,
            bits: owner.iter().map(|o| *o == id).collect(),
        };
        let contours = trace_mask(&mask);
        if !contours.is_empty() {
            regions.push(Region { color, contours });
        }
    }
    regions
}

/// Boundary-walk one mask into closed rectilinear contours.
///
/// Each filled pixel contributes an edge on every side facing an unfilled
/// neighbour, walked so the covered side stays on the same hand. Where a
/// saddle leaves two continuations at a corner, the walk prefers the sharpest
/// right turn, which splits the saddle into the two separate loops the two
/// diagonally-touching components actually have. Picking the left turn instead
/// welds them into a self-crossing figure.
fn trace_mask(mask: &Mask) -> Vec<Vec<[f64; 2]>> {
    let mut edges: HashMap<(i32, i32, u8), ()> = HashMap::new();
    for y in 0..mask.h {
        for x in 0..mask.w {
            if !mask.filled(x, y) {
                continue;
            }
            if !mask.filled(x, y - 1) {
                edges.insert((x, y, RIGHT), ());
            }
            if !mask.filled(x + 1, y) {
                edges.insert((x + 1, y, DOWN), ());
            }
            if !mask.filled(x, y + 1) {
                edges.insert((x + 1, y + 1, LEFT), ());
            }
            if !mask.filled(x - 1, y) {
                edges.insert((x, y + 1, UP), ());
            }
        }
    }

    let seeds: Vec<(i32, i32, u8)> = edges.keys().copied().collect();
    let mut contours = Vec::new();

    for seed in seeds {
        if edges.remove(&seed).is_none() {
            continue;
        }
        let mut ring = vec![(seed.0, seed.1)];
        let (mut cx, mut cy) = (seed.0 + step(seed.2).0, seed.1 + step(seed.2).1);
        let mut dir = seed.2;
        ring.push((cx, cy));

        loop {
            let next = [(dir + 1) % 4, dir, (dir + 3) % 4]
                .into_iter()
                .find(|candidate| edges.contains_key(&(cx, cy, *candidate)));
            let Some(next) = next else { break };
            edges.remove(&(cx, cy, next));
            dir = next;
            let (dx, dy) = step(dir);
            cx += dx;
            cy += dy;
            ring.push((cx, cy));
            if cx == ring[0].0 && cy == ring[0].1 {
                break;
            }
        }

        if let Some(corners) = corners(&ring) {
            contours.push(corners);
        }
    }
    contours
}

/// Drop the points where a traced ring keeps going straight, so a stair-stepped
/// boundary becomes one segment per side instead of one per pixel.
fn corners(ring: &[(i32, i32)]) -> Option<Vec<[f64; 2]>> {
    let closed = ring.len().checked_sub(1)?;
    if closed < 4 || ring[0] != ring[closed] {
        return None;
    }
    let ring = &ring[..closed];

    let mut start = 0;
    for i in 0..closed {
        let corner = ring[(i + 1) % closed];
        if heading(ring[i], corner) != heading(corner, ring[(i + 2) % closed]) {
            start = (i + 1) % closed;
            break;
        }
    }

    let mut out = Vec::with_capacity(closed);
    for k in 0..closed {
        let i = (start + k) % closed;
        let current = ring[i];
        if heading(ring[(i + closed - 1) % closed], current)
            == heading(current, ring[(i + 1) % closed])
        {
            continue;
        }
        out.push([current.0 as f64, current.1 as f64]);
    }
    (out.len() >= 4).then_some(out)
}
