//! Pure HSV <-> RGB conversion and a bounded recent-swatches ring buffer.
//! No UI dependencies.

use renamite_model::Color;

/// Hue in [0, 360), saturation/value in [0, 1].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hsv {
    pub h: f64,
    pub s: f64,
    pub v: f64,
}

pub fn rgb_to_hsv(c: Color) -> Hsv {
    let (r, g, b) = (c.r, c.g, c.b);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;

    let h = if delta < 1e-9 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta).rem_euclid(6.0))
    } else if max == g {
        60.0 * (((b - r) / delta) + 2.0)
    } else {
        60.0 * (((r - g) / delta) + 4.0)
    };

    let s = if max < 1e-9 { 0.0 } else { delta / max };
    Hsv {
        h: h.rem_euclid(360.0),
        s,
        v: max,
    }
}

pub fn hsv_to_rgb(hsv: Hsv, alpha: f64) -> Color {
    let Hsv { h, s, v } = hsv;
    let c = v * s;
    let h_prime = h / 60.0;
    let x = c * (1.0 - (h_prime.rem_euclid(2.0) - 1.0).abs());
    let m = v - c;

    let (r, g, b) = match h_prime as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };

    Color::rgba(r + m, g + m, b + m, alpha)
}

/// Parse `#RGB`, `#RRGGBB`, or `#RRGGBBAA` (case-insensitive, `#` optional).
pub fn parse_hex(input: &str) -> Option<Color> {
    let s = input.trim().trim_start_matches('#');
    let digit = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    let byte =
        |hi: u8, lo: u8| -> Option<f64> { Some((digit(hi)? * 16 + digit(lo)?) as f64 / 255.0) };
    let bytes = s.as_bytes();
    match bytes.len() {
        3 => Some(Color::rgba(
            digit(bytes[0])? as f64 / 15.0,
            digit(bytes[1])? as f64 / 15.0,
            digit(bytes[2])? as f64 / 15.0,
            1.0,
        )),
        6 => Some(Color::rgba(
            byte(bytes[0], bytes[1])?,
            byte(bytes[2], bytes[3])?,
            byte(bytes[4], bytes[5])?,
            1.0,
        )),
        8 => Some(Color::rgba(
            byte(bytes[0], bytes[1])?,
            byte(bytes[2], bytes[3])?,
            byte(bytes[4], bytes[5])?,
            byte(bytes[6], bytes[7])?,
        )),
        _ => None,
    }
}

pub fn to_hex(c: Color, with_alpha: bool) -> String {
    let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    if with_alpha {
        format!("#{:02X}{:02X}{:02X}{:02X}", b(c.r), b(c.g), b(c.b), b(c.a))
    } else {
        format!("#{:02X}{:02X}{:02X}", b(c.r), b(c.g), b(c.b))
    }
}

/// Bounded MRU ring of recently used colors (most-recent first, deduped).
#[derive(Clone, Debug, Default)]
pub struct SwatchHistory {
    pub colors: Vec<Color>,
    max_len: usize,
}

impl SwatchHistory {
    pub fn new(max_len: usize) -> Self {
        Self {
            colors: Vec::new(),
            max_len,
        }
    }

    pub fn push(&mut self, c: Color) {
        self.colors.retain(|existing| !color_eq(*existing, c));
        self.colors.insert(0, c);
        self.colors.truncate(self.max_len);
    }
}

fn color_eq(a: Color, b: Color) -> bool {
    (a.r - b.r).abs() < 1e-4
        && (a.g - b.g).abs() < 1e-4
        && (a.b - b.b).abs() < 1e-4
        && (a.a - b.a).abs() < 1e-4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn red_hsv_roundtrip() {
        let red = Color::rgba(1.0, 0.0, 0.0, 1.0);
        let hsv = rgb_to_hsv(red);
        assert!((hsv.h - 0.0).abs() < 1e-6);
        assert!((hsv.s - 1.0).abs() < 1e-6);
        assert!((hsv.v - 1.0).abs() < 1e-6);
        let back = hsv_to_rgb(hsv, 1.0);
        assert!((back.r - 1.0).abs() < 1e-6 && back.g.abs() < 1e-6 && back.b.abs() < 1e-6);
    }

    #[test]
    fn cyan_hue_is_180() {
        let c = Color::rgba(0.0, 1.0, 1.0, 1.0);
        let hsv = rgb_to_hsv(c);
        assert!((hsv.h - 180.0).abs() < 1e-6);
    }

    #[test]
    fn gray_has_zero_saturation() {
        let hsv = rgb_to_hsv(Color::rgba(0.5, 0.5, 0.5, 1.0));
        assert!(hsv.s.abs() < 1e-6);
    }

    #[test]
    fn hex_roundtrip_with_alpha() {
        let c = Color::rgba(0.2, 0.4, 0.6, 0.8);
        let hex = to_hex(c, true);
        let parsed = parse_hex(&hex).unwrap();
        assert!((parsed.r - c.r).abs() < 0.01);
        assert!((parsed.a - c.a).abs() < 0.01);
    }

    #[test]
    fn parse_hex_accepts_short_form() {
        let c = parse_hex("#f00").unwrap();
        assert!((c.r - 1.0).abs() < 1e-6);
        assert!(c.g.abs() < 1e-6);
    }

    #[test]
    fn parse_hex_rejects_garbage() {
        assert!(parse_hex("not-a-color").is_none());
        assert!(parse_hex("#ff00").is_none()); // invalid length
    }

    #[test]
    fn swatch_history_dedupes_and_bounds() {
        let mut h = SwatchHistory::new(3);
        h.push(Color::rgba(1.0, 0.0, 0.0, 1.0));
        h.push(Color::rgba(0.0, 1.0, 0.0, 1.0));
        h.push(Color::rgba(1.0, 0.0, 0.0, 1.0)); // dup of first, moves to front
        assert_eq!(h.colors.len(), 2);
        assert_eq!(h.colors[0], Color::rgba(1.0, 0.0, 0.0, 1.0));
        h.push(Color::rgba(0.0, 0.0, 1.0, 1.0));
        h.push(Color::rgba(1.0, 1.0, 0.0, 1.0));
        assert_eq!(h.colors.len(), 3, "bounded to max_len");
    }
}
