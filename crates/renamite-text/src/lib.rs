//! Text shaping for renamite: string -> `kurbo::BezPath` outlines.
//!
//! Deliberately simple for v1: left-to-right horizontal layout, per-glyph
//! advances, `\n` line breaks. No bidi, no complex-script shaping, no
//! ligatures — documented limits, not silent wrongness. Deterministic: the
//! same input always yields the same path, so goldens and CLI renders match
//! the editor exactly.

use kurbo::BezPath;
use ttf_parser::{Face, GlyphId, OutlineBuilder};

/// Bundled fallback face (OFL-licensed; see `assets/OFL.txt`).
static DEFAULT_FONT: &[u8] = include_bytes!("../assets/default.ttf");

#[derive(Debug, thiserror::Error)]
pub enum TextError {
    #[error("font failed to parse")]
    BadFont,
}

/// Horizontal alignment of each line within the text block.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// A parsed font face, borrowed from a byte slice (a font file or the
/// bundled default). Cheap to construct; the parse is a table scan only.
pub struct FontRef<'a> {
    face: Face<'a>,
}

impl<'a> FontRef<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self, TextError> {
        Ok(Self {
            face: Face::parse(data, 0).map_err(|_| TextError::BadFont)?,
        })
    }

    pub fn default_font() -> Self {
        Self::parse(DEFAULT_FONT).expect("bundled font is valid")
    }
}

/// Collects glyph outline segments into a `BezPath`. Font coordinates are
/// y-up with the baseline at y = 0; the canvas is y-down, so `dy` holds the
/// baseline and the y axis is flipped.
struct PathSink {
    path: BezPath,
    scale: f64,
    dx: f64,
    dy: f64,
}

impl OutlineBuilder for PathSink {
    fn move_to(&mut self, x: f32, y: f32) {
        self.path
            .move_to((self.dx + x as f64 * self.scale, self.dy - y as f64 * self.scale));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.path
            .line_to((self.dx + x as f64 * self.scale, self.dy - y as f64 * self.scale));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.path.quad_to(
            (self.dx + x1 as f64 * self.scale, self.dy - y1 as f64 * self.scale),
            (self.dx + x as f64 * self.scale, self.dy - y as f64 * self.scale),
        );
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.path.curve_to(
            (self.dx + x1 as f64 * self.scale, self.dy - y1 as f64 * self.scale),
            (self.dx + x2 as f64 * self.scale, self.dy - y2 as f64 * self.scale),
            (self.dx + x as f64 * self.scale, self.dy - y as f64 * self.scale),
        );
    }
    fn close(&mut self) {
        self.path.close_path();
    }
}

/// Shape `text` at `size` (px per em) into one combined outline path.
///
/// Origin: (0, 0) is the first line's baseline start; lines advance downward.
pub fn shape_text(font: &FontRef, text: &str, size: f64, align: TextAlign) -> BezPath {
    let face = &font.face;
    let upem = face.units_per_em() as f64;
    let scale = size / upem.max(1.0);
    let line_height = (face.ascender() as f64 - face.descender() as f64
        + face.line_gap() as f64)
        * scale;
    let mut out = BezPath::new();
    for (line_idx, line) in text.split('\n').enumerate() {
        let baseline = line_idx as f64 * line_height;
        let width = line_advance(face, line) * scale;
        let start_x = match align {
            TextAlign::Left => 0.0,
            TextAlign::Center => -width / 2.0,
            TextAlign::Right => -width,
        };
        let mut pen = start_x;
        for ch in line.chars() {
            let gid = face.glyph_index(ch).unwrap_or(GlyphId(0));
            let mut sink = PathSink {
                path: BezPath::new(),
                scale,
                dx: pen,
                dy: baseline,
            };
            let _ = face.outline_glyph(gid, &mut sink);
            out.extend(sink.path);
            pen += face.glyph_hor_advance(gid).unwrap_or(0) as f64 * scale;
        }
    }
    out
}

/// Advance width of one line in font units.
fn line_advance(face: &Face, line: &str) -> f64 {
    line.chars()
        .map(|ch| {
            let gid = face.glyph_index(ch).unwrap_or(GlyphId(0));
            face.glyph_hor_advance(gid).unwrap_or(0) as f64
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::Shape;

    #[test]
    fn nonempty_text_produces_outline() {
        let font = FontRef::default_font();
        let p = shape_text(&font, "Ab", 48.0, TextAlign::Left);
        assert!(!p.elements().is_empty());
        assert!(p.bounding_box().width() > 10.0);
    }

    #[test]
    fn newline_advances_baseline_downward() {
        let font = FontRef::default_font();
        let one = shape_text(&font, "A", 48.0, TextAlign::Left).bounding_box();
        let two = shape_text(&font, "A\nA", 48.0, TextAlign::Left).bounding_box();
        assert!(two.height() > one.height() + 10.0);
    }

    #[test]
    fn center_align_straddles_origin() {
        let font = FontRef::default_font();
        let bb = shape_text(&font, "WW", 48.0, TextAlign::Center).bounding_box();
        assert!(bb.x0 < 0.0 && bb.x1 > 0.0);
    }

    #[test]
    fn shaping_is_deterministic() {
        let font = FontRef::default_font();
        let a = shape_text(&font, "Renamite", 32.0, TextAlign::Left);
        let b = shape_text(&font, "Renamite", 32.0, TextAlign::Left);
        assert_eq!(a.elements(), b.elements());
    }
}
