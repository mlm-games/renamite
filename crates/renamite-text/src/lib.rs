//! Text shaping for renamite: string -> `kurbo::BezPath` outlines.
//!
//! Deliberately simple for v1: left-to-right horizontal layout, per-glyph
//! advances, `\n` line breaks. No bidi, no complex-script shaping, no
//! ligatures - documented limits, not silent wrongness. Deterministic: the
//! same input always yields the same path, so goldens and CLI renders match
//! the editor exactly.
//!
//! A process-wide family-name registry (like repose's `repose_text`) maps
//! logical family names to raw font bytes: [`register_font_data`] stores a
//! font keyed by the name its own name table reports, [`font_family_name`]
//! extracts that name, and [`FontRef::for_family`] resolves a
//! `TextNode.font` value to a face, falling back to the bundled default.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use kurbo::BezPath;
use ttf_parser::name::name_id;
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

/// A parsed font face, owning its bytes. Cheap to clone (an `Arc` bump); the
/// face table scan happens once per `face()` call.
#[derive(Clone)]
pub struct FontRef {
    data: Arc<[u8]>,
}

impl FontRef {
    /// Parse and own a copy of `data`. Validates the face up front so
    /// [`FontRef::face`] can be infallible afterward.
    pub fn parse(data: &[u8]) -> Result<Self, TextError> {
        Face::parse(data, 0).map_err(|_| TextError::BadFont)?;
        Ok(Self {
            data: Arc::from(data),
        })
    }

    /// The bundled default face (registered under its family name too, so
    /// `for_family(Some("Noto Sans"))` and `for_family(None)` agree). Shares
    /// the default's single allocated copy of the bytes.
    pub fn default_font() -> Self {
        Self {
            data: default_font_data().clone(),
        }
    }

    /// Resolve the font for a logical family name (`TextNode.font`), falling
    /// back to the bundled default when the name is absent or unknown.
    pub fn for_family(name: Option<&str>) -> Self {
        match name.and_then(|n| registry().lock().unwrap().fonts.get(n).cloned()) {
            Some(data) => Self { data },
            None => Self::default_font(),
        }
    }

    /// The parsed face, borrowing this instance. Never fails: every
    /// constructor validates the data.
    pub fn face(&self) -> Face<'_> {
        Face::parse(&self.data, 0).expect("font data validated at construction")
    }
}

fn default_font_data() -> &'static Arc<[u8]> {
    static DEFAULT: OnceLock<Arc<[u8]>> = OnceLock::new();
    DEFAULT.get_or_init(|| Arc::from(DEFAULT_FONT))
}

/// Raw bytes of the bundled default face, for callers that need to register
/// the font with their own machinery (e.g. `usvg::Options::fontdb_mut`).
pub fn default_font_bytes() -> &'static [u8] {
    DEFAULT_FONT
}

struct Registry {
    fonts: HashMap<String, Arc<[u8]>>,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut fonts = HashMap::new();
        let default = default_font_data().clone();
        if let Some(name) = font_family_name(&default) {
            fonts.insert(name, default);
        }
        Mutex::new(Registry { fonts })
    })
}

/// Extract the family name from raw font bytes: typographic family
/// (`name id 16`) first, standard family (`name id 1`) as fallback. Mirrors
/// `repose_text::font_family_name`.
pub fn font_family_name(bytes: &[u8]) -> Option<String> {
    let face = Face::parse(bytes, 0).ok()?;
    let mut fallback = None;
    for name in face.names() {
        match name.name_id {
            name_id::TYPOGRAPHIC_FAMILY => {
                if let Some(s) = name.to_string() {
                    return Some(s);
                }
            }
            name_id::FAMILY if fallback.is_none() => {
                fallback = name.to_string();
            }
            _ => {}
        }
    }
    fallback
}

/// Register raw font bytes (`ttf`/`otf`) into the process-wide registry,
/// keyed by the family name the font reports. Returns that name, or `None`
/// if the bytes are not a parseable font.
pub fn register_font_data(bytes: Vec<u8>) -> Option<String> {
    let family = font_family_name(&bytes)?;
    registry()
        .lock()
        .unwrap()
        .fonts
        .insert(family.clone(), Arc::from(bytes));
    Some(family)
}

/// All registered family names (including the bundled default), sorted.
pub fn registered_families() -> Vec<String> {
    let mut names: Vec<String> = registry().lock().unwrap().fonts.keys().cloned().collect();
    names.sort();
    names
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
        self.path.move_to((
            self.dx + x as f64 * self.scale,
            self.dy - y as f64 * self.scale,
        ));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.path.line_to((
            self.dx + x as f64 * self.scale,
            self.dy - y as f64 * self.scale,
        ));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.path.quad_to(
            (
                self.dx + x1 as f64 * self.scale,
                self.dy - y1 as f64 * self.scale,
            ),
            (
                self.dx + x as f64 * self.scale,
                self.dy - y as f64 * self.scale,
            ),
        );
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.path.curve_to(
            (
                self.dx + x1 as f64 * self.scale,
                self.dy - y1 as f64 * self.scale,
            ),
            (
                self.dx + x2 as f64 * self.scale,
                self.dy - y2 as f64 * self.scale,
            ),
            (
                self.dx + x as f64 * self.scale,
                self.dy - y as f64 * self.scale,
            ),
        );
    }
    fn close(&mut self) {
        self.path.close_path();
    }
}

/// Shape `text` from raw font bytes (TTF/OTF). Errors if the bytes are not a
/// parseable font; callers should fall back to the bundled default.
pub fn shape_text_from_bytes(
    bytes: &[u8],
    text: &str,
    size: f64,
    align: TextAlign,
) -> Result<BezPath, TextError> {
    let font = FontRef::parse(bytes)?;
    Ok(shape_text(&font, text, size, align))
}

/// Shape `text` with the bundled default face.
pub fn shape_text_default(text: &str, size: f64, align: TextAlign) -> BezPath {
    shape_text(&FontRef::default_font(), text, size, align)
}

/// Shape `text` at `size` (px per em) into one combined outline path.
///
/// Origin: (0, 0) is the first line's baseline start; lines advance downward.
pub fn shape_text(font: &FontRef, text: &str, size: f64, align: TextAlign) -> BezPath {
    let face = font.face();
    let upem = face.units_per_em() as f64;
    let scale = size / upem.max(1.0);
    let line_height =
        (face.ascender() as f64 - face.descender() as f64 + face.line_gap() as f64) * scale;
    let mut out = BezPath::new();
    for (line_idx, line) in text.split('\n').enumerate() {
        let baseline = line_idx as f64 * line_height;
        let width = line_advance(&face, line) * scale;
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

    #[test]
    fn bundled_font_reports_family_name() {
        let name = font_family_name(DEFAULT_FONT).expect("bundled font has a name");
        assert!(!name.is_empty());
        assert!(
            registered_families().contains(&name),
            "default should be registered under its own family name, got {:?}",
            registered_families()
        );
    }

    #[test]
    fn for_family_none_resolves_to_default() {
        let font = FontRef::for_family(None);
        let p = shape_text(&font, "Ab", 48.0, TextAlign::Left);
        assert!(!p.elements().is_empty());
    }

    #[test]
    fn for_family_unknown_falls_back_to_default() {
        let font = FontRef::for_family(Some("Definitely Not A Font"));
        let p = shape_text(&font, "Ab", 48.0, TextAlign::Left);
        assert!(!p.elements().is_empty());
    }

    #[test]
    fn register_font_data_keys_by_family_and_resolves() {
        let name = register_font_data(DEFAULT_FONT.to_vec()).expect("valid font registers");
        assert_eq!(
            name,
            font_family_name(DEFAULT_FONT).unwrap(),
            "register returns the same family the font reports"
        );
        assert!(registered_families().contains(&name));

        let by_name = FontRef::for_family(Some(&name));
        let by_default = FontRef::default_font();
        let a = shape_text(&by_name, "Renamite", 32.0, TextAlign::Left);
        let b = shape_text(&by_default, "Renamite", 32.0, TextAlign::Left);
        assert_eq!(
            a.elements(),
            b.elements(),
            "registry hit shapes identically to the default face"
        );
    }

    #[test]
    fn register_invalid_bytes_returns_none() {
        assert_eq!(register_font_data(b"not a font".to_vec()), None);
        assert_eq!(font_family_name(b"not a font"), None);
    }
}
