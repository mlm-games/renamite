//! Text shaping for renamite: string -> `kurbo::BezPath` outlines.
//!
//! Shaping and outline extraction go through the host text stack
//! (`repose_text`: parley shaping + skrifa outlines), so renamite gets script
//! shaping, marks, ligatures, bidirectional runs, font fallback and variable
//! fonts. `\n` is the only line break, and (0, 0) is the first line's baseline
//! start. Deterministic: the same input always yields the same path, so goldens
//! and CLI renders match the editor exactly.
//!
//! A process-wide family-name registry maps logical family names to raw font
//! bytes: [`register_font_data`] stores a font keyed by the name its own name
//! table reports, [`font_family_name`] extracts that name, and
//! [`FontRef::for_family`] resolves a `TextNode.font` value to a face, falling
//! back to the bundled default. Shaping addresses each face by a private alias
//! rather than that name, so a host cannot change the result by having a
//! different font installed under the same family.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

use kurbo::{BezPath, Point, QuadBez};
use repose_text::{Command, FontSynthesis, ShapeOptions, ShapedGlyph, TextDirection};

/// Bundled fallback face (OFL-licensed; see `assets/OFL.txt`).
static DEFAULT_FONT: &[u8] = include_bytes!("../assets/default.ttf");

const FONT_WEIGHT: u16 = 400;
const FONT_STYLE: u8 = 0;

/// Vertical metrics are read at this size, in em. It is an exact multiple of
/// the host's 1/4 px metric quantum, so the em values carry no rounding.
const METRIC_PX: f32 = 1000.0;

/// Clamp a document-supplied length into the `f32` range the host shapes in, so
/// a hostile or unit-mismatched value cannot turn into `inf` and then NaN.
fn to_f32(value: f64) -> f32 {
    if value.is_finite() {
        (value.max(0.0) as f32).clamp(0.0, f32::MAX / 4.0)
    } else {
        0.0
    }
}

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

/// A font face known to the registry, owning its bytes. Cheap to clone (an
/// `Arc` bump). Shaping addresses it by alias, a name only this registry
/// answers to, so the face cannot be shadowed by a like-named system font.
#[derive(Clone)]
pub struct FontRef {
    family: Option<String>,
    data: Arc<[u8]>,
    alias: String,
}

impl FontRef {
    /// Take ownership of `data` and register it with the host. Errors if the
    /// bytes are not a parseable font; callers should fall back to the bundled
    /// default.
    pub fn parse(data: &[u8]) -> Result<Self, TextError> {
        // The evaluator hands us the same bytes every frame, so a payload the
        // registry already knows costs one fingerprint and no copy or parse.
        if let Some(payload) = lookup_payload(data) {
            return Ok(Self {
                family: Some(payload.family),
                data: payload.data,
                alias: payload.alias,
            });
        }
        let family = font_family_name(data).ok_or(TextError::BadFont)?;
        let data: Arc<[u8]> = Arc::from(data);
        let alias = payload_alias(&data);
        register_payload(data.clone(), family.clone(), alias.clone());
        Ok(Self {
            family: Some(family),
            data,
            alias,
        })
    }

    /// The bundled default face (registered under its family name too, so
    /// `for_family(Some("Noto Sans"))` and `for_family(None)` agree). Shares
    /// the default's single allocated copy of the bytes.
    pub fn default_font() -> Self {
        let registry = registry()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let alias = registry
            .default_alias
            .clone()
            .unwrap_or_else(|| payload_alias(DEFAULT_FONT));
        Self {
            family: registry.default_family.clone(),
            data: default_font_data().clone(),
            alias,
        }
    }

    /// Resolve the font for a logical family name (`TextNode.font`), falling
    /// back to the bundled default when the name is absent or unknown.
    pub fn for_family(name: Option<&str>) -> Self {
        let found = name.and_then(|name| {
            let registry = registry()
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let data = registry.fonts.get(name)?.clone();
            let alias = registry
                .payloads
                .get(&payload_key(&data))
                .map(|p| p.alias.clone())
                .unwrap_or_else(|| payload_alias(&data));
            Some(Self {
                family: Some(name.to_string()),
                data,
                alias,
            })
        });
        found.unwrap_or_else(Self::default_font)
    }

    /// Family name the face's own name table reports, or `None` if the bytes
    /// carry no name at all. This is what a document stores; shaping does not
    /// use it, since a like-named font on the host could answer to it.
    pub fn family(&self) -> Option<&str> {
        self.family.as_deref()
    }

    /// The raw bytes this face owns.
    pub fn bytes(&self) -> &[u8] {
        &self.data
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
    /// Family name -> bytes: what [`registered_families`] and
    /// [`FontRef::for_family`] see.
    fonts: HashMap<String, Arc<[u8]>>,
    /// Payload fingerprint -> that payload's family, bytes and host alias, so a
    /// repeat payload resolves without copying or parsing anything.
    payloads: HashMap<u64, Payload>,
    default_family: Option<String>,
    default_alias: Option<String>,
}

struct Payload {
    family: String,
    data: Arc<[u8]>,
    alias: String,
}

impl Clone for Payload {
    fn clone(&self) -> Self {
        Self {
            family: self.family.clone(),
            data: self.data.clone(),
            alias: self.alias.clone(),
        }
    }
}

/// Name renamite shapes this payload by. Derived from the fingerprint, so it is
/// stable across runs and unique per payload, and no font installed on the host
/// can answer to it.
fn payload_alias(bytes: &[u8]) -> String {
    format!("renamite-{:016x}", payload_key(bytes))
}

impl Registry {
    fn new() -> Self {
        Self {
            fonts: HashMap::new(),
            payloads: HashMap::new(),
            default_family: None,
            default_alias: None,
        }
    }

    fn with_default() -> Self {
        let mut registry = Self::new();
        let Some(family) = font_family_name(DEFAULT_FONT) else {
            return registry;
        };
        let alias = payload_alias(DEFAULT_FONT);
        registry.insert(default_font_data().clone(), family.clone(), alias.clone());
        registry.default_family = Some(family);
        registry.default_alias = Some(alias);
        registry
    }

    /// Returns true when this payload is new to the host.
    fn insert(&mut self, data: Arc<[u8]>, family: String, alias: String) -> bool {
        self.fonts.insert(family.clone(), data.clone());
        self.payloads
            .insert(
                payload_key(&data),
                Payload {
                    family,
                    data,
                    alias,
                },
            )
            .is_none()
    }
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Registry::with_default()))
}

/// Fingerprint of a font payload. Font bytes are large and the evaluator hands
/// us the same slice every frame, so registration is guarded by a bounded
/// sample of the payload rather than a hash of all of it.
fn payload_key(bytes: &[u8]) -> u64 {
    const SAMPLE: usize = 1024;
    let head = bytes.len().min(SAMPLE);
    let mut hasher = DefaultHasher::new();
    bytes.len().hash(&mut hasher);
    hasher.write(&bytes[..head]);
    hasher.write(&bytes[bytes.len() - head..]);
    hasher.finish()
}

/// Look up a payload the registry may already hold. The fingerprint samples the
/// payload, so a hit is confirmed by comparing the bytes before handing them
/// back: a collision must cost a re-registration, never the wrong font.
fn lookup_payload(bytes: &[u8]) -> Option<Payload> {
    let registry = registry()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let payload = registry.payloads.get(&payload_key(bytes))?.clone();
    (payload.data.as_ref() == bytes).then_some(payload)
}

fn register_payload(data: Arc<[u8]>, family: String, alias: String) {
    let fresh = {
        let mut registry = registry()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        registry.insert(data.clone(), family, alias.clone())
    };
    if fresh {
        repose_text::register_font_as(&data, &alias);
    }
}

/// Extract the family name from raw font bytes: typographic family
/// (`name id 16`) first, standard family (`name id 1`) as fallback.
pub fn font_family_name(bytes: &[u8]) -> Option<String> {
    repose_text::font_family_name(bytes).filter(|name| !name.is_empty())
}

/// Register raw font bytes (`ttf`/`otf`) into the process-wide registry,
/// keyed by the family name the font reports. Returns that name, or `None`
/// if the bytes are not a parseable font.
pub fn register_font_data(bytes: Vec<u8>) -> Option<String> {
    let family = font_family_name(&bytes)?;
    let alias = payload_alias(&bytes);
    register_payload(Arc::from(bytes), family.clone(), alias);
    Some(family)
}

/// All registered family names (including the bundled default), sorted.
pub fn registered_families() -> Vec<String> {
    let registry = registry()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let mut names: Vec<String> = registry.fonts.keys().cloned().collect();
    names.sort();
    names
}

/// Shape `text` from raw font bytes (TTF/OTF). Errors if the bytes are not a
/// parseable font; callers should fall back to the bundled default.
pub fn shape_text_from_bytes(
    bytes: &[u8],
    text: &str,
    size: f64,
    align: TextAlign,
    tracking: f64,
    leading: f64,
) -> Result<BezPath, TextError> {
    let font = FontRef::parse(bytes)?;
    Ok(shape_text(&font, text, size, align, tracking, leading))
}

/// Shape `text` with the bundled default face.
pub fn shape_text_default(
    text: &str,
    size: f64,
    align: TextAlign,
    tracking: f64,
    leading: f64,
) -> BezPath {
    shape_text(
        &FontRef::default_font(),
        text,
        size,
        align,
        tracking,
        leading,
    )
}

/// Shape `text` at `size` (px per em) into one combined outline path.
///
/// Origin: (0, 0) is the first line's baseline start; lines advance downward.
/// `tracking` is extra advance per glyph in px. `leading` is extra line spacing in px added to the face's default line height.
pub fn shape_text(
    font: &FontRef,
    text: &str,
    size: f64,
    align: TextAlign,
    tracking: f64,
    leading: f64,
) -> BezPath {
    // The host shapes in `f32`, so a finite `f64` that overflows it (1e39 from
    // an unvalidated document) would reach the outline maths as inf and turn the
    // whole block into NaN points.
    let size = to_f32(size);
    if size <= 1e-9 {
        return BezPath::new();
    }
    let tracking = to_f32(tracking);
    let leading = to_f32(leading);
    let line_height = line_height(Some(&font.alias), size as f64, leading as f64);
    let mut out = BezPath::new();
    for (index, line) in text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .enumerate()
    {
        let Some(glyphs) = shape_line(Some(&font.alias), line, size, tracking) else {
            continue;
        };
        if glyphs.is_empty() {
            continue;
        }
        let width = line_advance(&glyphs, tracking) as f64;
        let origin = match align {
            TextAlign::Left => 0.0,
            TextAlign::Center => -width / 2.0,
            TextAlign::Right => -width,
        };
        let baseline = index as f64 * line_height;
        for glyph in glyphs.iter() {
            append_glyph(
                &mut out,
                glyph,
                origin + glyph.x as f64,
                baseline,
                size as f64,
            );
        }
    }
    out
}

fn shape_options<'a>(family: Option<&'a str>, tracking: f32) -> ShapeOptions<'a> {
    ShapeOptions {
        font_family: family,
        font_weight: FONT_WEIGHT,
        font_style: FONT_STYLE,
        letter_spacing: tracking,
        font_variation_settings: None,
        text_direction: TextDirection::Auto,
        font_synthesis: FontSynthesis::None,
    }
}

fn shape_line(
    family: Option<&str>,
    line: &str,
    size: f32,
    tracking: f32,
) -> Option<Arc<[ShapedGlyph]>> {
    let options = shape_options(family, tracking);
    if let Ok(shaped) = repose_text::shape_line_with_options_vector(line, size, 0.0, options) {
        return Some(Arc::clone(&shaped.glyphs));
    }
    // Shaping is refused outright for a few inputs (an empty line, a face with
    // no usable glyphs). Those leave no glyphs to draw, which is a correct
    // empty result, so only the synthesis retry is worth distinguishing: the
    // host refuses synthesis-free shaping when it wanted to fake a weight or a
    // slant, and a synthetic slant beats dropping the text.
    match repose_text::shape_line_with_options_vector(
        line,
        size,
        0.0,
        ShapeOptions {
            font_synthesis: FontSynthesis::Unspecified,
            ..options
        },
    ) {
        Ok(shaped) => Some(Arc::clone(&shaped.glyphs)),
        Err(_) => None,
    }
}

/// Advance width of one shaped line. The host adds tracking after every glyph
/// while renamite tracks between glyphs only, so the trailing one is dropped.
fn line_advance(glyphs: &[ShapedGlyph], tracking: f32) -> f32 {
    glyphs.iter().map(|glyph| glyph.advance).sum::<f32>() - tracking
}

/// Line advance in px: the face's own line height plus `leading`. The host
/// exposes ascent and descent but not the `hhea` line gap, so this is the
/// content height of the face rather than parley's preferred line height.
fn line_height(family: Option<&str>, size: f64, leading: f64) -> f64 {
    let (ascent, descent) =
        repose_text::primary_font_vertical_metrics(family, FONT_WEIGHT, METRIC_PX);    let line_height_em = (ascent + descent) as f64 / METRIC_PX as f64;
    let line_height = if line_height_em.is_finite() && line_height_em > 0.0 {
        line_height_em * size
    } else {
        size
    };
    (line_height + leading).max(size * 0.2)
}

/// Append one glyph outline. Outline commands are in em units, y-up with the
/// baseline at y = 0; the canvas is y-down, so `baseline` holds the baseline
/// and the y axis is flipped.
fn append_glyph(out: &mut BezPath, glyph: &ShapedGlyph, pen_x: f64, baseline: f64, scale: f64) {
    let Some(commands) = repose_text::extract_outline_commands_shared(&glyph.cache_key) else {
        return;
    };
    let map = |x: f32, y: f32| Point::new(pen_x + x as f64 * scale, baseline - y as f64 * scale);
    let mut start = Point::ZERO;
    for command in commands.iter() {
        match *command {
            Command::MoveTo(x, y) => {
                start = map(x, y);
                out.move_to(start);
            }
            Command::LineTo(x, y) => {
                start = map(x, y);
                out.line_to(start);
            }
            Command::QuadTo(x1, y1, x, y) => {
                let cubic = QuadBez::new(start, map(x1, y1), map(x, y)).raise();
                start = cubic.p3;
                out.curve_to(cubic.p1, cubic.p2, cubic.p3);
            }
            Command::CurveTo(x1, y1, x2, y2, x, y) => {
                start = map(x, y);
                out.curve_to(map(x1, y1), map(x2, y2), start);
            }
            Command::Close => out.close_path(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kurbo::PathEl;

    fn bbox(path: &BezPath) -> (f64, f64, f64, f64) {
        let mut bounds = (
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        );
        for element in path.elements() {
            let point = match *element {
                PathEl::MoveTo(p) | PathEl::LineTo(p) => p,
                PathEl::QuadTo(c, p) => {
                    bounds.0 = bounds.0.min(c.x).min(p.x);
                    bounds.1 = bounds.1.min(c.y).min(p.y);
                    bounds.2 = bounds.2.max(c.x).max(p.x);
                    bounds.3 = bounds.3.max(c.y).max(p.y);
                    continue;
                }
                PathEl::CurveTo(c1, c2, p) => {
                    for c in [c1, c2] {
                        bounds.0 = bounds.0.min(c.x);
                        bounds.1 = bounds.1.min(c.y);
                        bounds.2 = bounds.2.max(c.x);
                        bounds.3 = bounds.3.max(c.y);
                    }
                    p
                }
                PathEl::ClosePath => continue,
            };
            bounds.0 = bounds.0.min(point.x);
            bounds.1 = bounds.1.min(point.y);
            bounds.2 = bounds.2.max(point.x);
            bounds.3 = bounds.3.max(point.y);
        }
        bounds
    }

    #[test]
    fn outlines_are_finite_and_sit_above_the_first_baseline() {
        let path = shape_text_default("Hello", 48.0, TextAlign::Left, 0.0, 0.0);
        assert!(!path.elements().is_empty());
        assert!(path.is_finite(), "{path:?}");
        let (min_x, min_y, max_x, max_y) = bbox(&path);
        assert!(min_x >= 0.0, "left aligned line starts at x = 0");
        assert!(max_y < 1.0, "ink sits above the baseline: {min_y} {max_y}");
        assert!(min_x < max_x, "line has width");
        // The ascender of the bundled face at 48px is ~37px above the baseline.
        assert!(
            (max_y - min_y) > 30.0 && (max_y - min_y) < 48.0,
            "{min_y} {max_y}"
        );
    }

    #[test]
    fn empty_and_blank_text_shape_to_nothing() {
        for text in ["", "   ", "\n", " "] {
            let path = shape_text_default(text, 48.0, TextAlign::Left, 0.0, 0.0);
            assert!(path.elements().is_empty(), "{text:?} produced {path:?}");
        }
    }

    #[test]
    fn alignment_shifts_the_line_and_leading_stacks_lines() {
        let left = shape_text_default("Hello", 48.0, TextAlign::Left, 0.0, 0.0);
        let center = shape_text_default("Hello", 48.0, TextAlign::Center, 0.0, 0.0);
        let right = shape_text_default("Hello", 48.0, TextAlign::Right, 0.0, 0.0);
        let mid = |path: &BezPath| {
            let (min_x, _, max_x, _) = bbox(path);
            0.5 * (min_x + max_x)
        };
        assert!(mid(&left) > 10.0, "left aligned line starts at x = 0");
        assert!(mid(&center).abs() < 1.0, "centred line straddles x = 0");
        assert!(mid(&right) < -10.0, "right aligned line ends at x = 0");
        assert!(bbox(&right).2 < 0.0, "right aligned advance ends at x = 0");

        let one = shape_text_default("Hello", 48.0, TextAlign::Left, 0.0, 0.0);
        let two = shape_text_default("Hello\nHello", 48.0, TextAlign::Left, 0.0, 0.0);
        let spaced = shape_text_default("Hello\nHello", 48.0, TextAlign::Left, 0.0, 20.0);
        let single_bottom = bbox(&one).3;
        assert!(
            bbox(&two).3 > single_bottom,
            "second line advances downward"
        );
        // The bundled face's line height is 1.362em, so ~65px at 48px.
        let step = bbox(&two).3 - single_bottom;
        assert!((step - 65.376).abs() < 0.5, "line step {step}");
        assert!((bbox(&spaced).3 - single_bottom - step - 20.0).abs() < 1e-9);
    }

    #[test]
    fn unknown_families_and_unparseable_bytes_use_the_default() {
        assert!(registered_families().contains(&font_family_name(default_font_bytes()).unwrap()));
        let unknown = FontRef::for_family(Some("No Such Family"));
        assert_eq!(unknown.family(), FontRef::default_font().family());
        assert_eq!(
            shape_text(&unknown, "Hello", 48.0, TextAlign::Left, 0.0, 0.0)
                .elements()
                .len(),
            shape_text_default("Hello", 48.0, TextAlign::Left, 0.0, 0.0)
                .elements()
                .len()
        );

        assert!(matches!(
            shape_text_from_bytes(
                b"definitely not a font",
                "Hello",
                48.0,
                TextAlign::Left,
                0.0,
                0.0
            ),
            Err(TextError::BadFont)
        ));
    }

    #[test]
    fn registered_bytes_shape_and_register_their_family() {
        let family = font_family_name(default_font_bytes()).expect("bundled face has a name");
        let registered = register_font_data(default_font_bytes().to_vec());
        assert_eq!(registered.as_deref(), Some(family.as_str()));
        assert!(registered_families().contains(&family));

        let path = shape_text_from_bytes(
            default_font_bytes(),
            "Renamite",
            32.0,
            TextAlign::Left,
            2.0,
            0.0,
        )
        .expect("bundled bytes parse");
        assert!(path.is_finite() && !path.elements().is_empty(), "{path:?}");
        // Tracking widens the line.
        let tight = shape_text_from_bytes(
            default_font_bytes(),
            "Renamite",
            32.0,
            TextAlign::Left,
            0.0,
            0.0,
        )
        .expect("bundled bytes parse");
        assert!(bbox(&path).2 > bbox(&tight).2);
    }

    #[test]
    fn non_finite_metrics_are_treated_as_zero() {
        let path = shape_text_default("Hello", f64::NAN, TextAlign::Left, 0.0, 0.0);
        assert!(path.elements().is_empty());
        let path = shape_text_default("Hello", 48.0, TextAlign::Left, f64::NAN, f64::NAN);
        assert_eq!(
            path.elements().len(),
            shape_text_default("Hello", 48.0, TextAlign::Left, 0.0, 0.0)
                .elements()
                .len()
        );
    }
}

#[cfg(test)]
mod hostile_input {
    use super::*;
    use kurbo::PathEl;

    fn all_finite(path: &BezPath) -> bool {
        path.elements().iter().all(|e| match e {
            PathEl::MoveTo(p) | PathEl::LineTo(p) => p.x.is_finite() && p.y.is_finite(),
            PathEl::QuadTo(c, p) | PathEl::CurveTo(_, c, p) => {
                [c.x, c.y, p.x, p.y].iter().all(|v| v.is_finite())
            }
            PathEl::ClosePath => true,
        })
    }

    /// A document-supplied size that overflows the host's `f32` must not reach
    /// the outline maths: `inf` there becomes NaN points and the text vanishes.
    #[test]
    fn sizes_that_overflow_f32_stay_empty_or_finite() {
        for size in [f64::MAX, 1e39, 1e300, f64::INFINITY, f64::NAN] {
            let path = shape_text_default("Hello", size, TextAlign::Left, 0.0, 0.0);
            assert!(
                all_finite(&path),
                "size {size} produced non-finite geometry"
            );
        }
    }

    /// Same for tracking and leading, which are added to per-glyph advances.
    #[test]
    fn hostile_tracking_and_leading_stay_finite() {
        for tracking in [f64::MAX, 1e39, 1e300, f64::INFINITY, f64::NAN] {
            let path = shape_text_default("Hi", 48.0, TextAlign::Center, tracking, 0.0);
            assert!(all_finite(&path), "tracking {tracking} was non-finite");
        }
        for leading in [f64::MAX, 1e39, f64::INFINITY, f64::NAN] {
            let path = shape_text_default("Hi", 48.0, TextAlign::Left, 0.0, leading);
            assert!(all_finite(&path), "leading {leading} was non-finite");
        }
    }

    /// Zero and negative sizes stay empty rather than inverting the glyphs.
    #[test]
    fn non_positive_size_draws_nothing() {
        for size in [0.0, -1.0, -1e9] {
            assert!(
                shape_text_default("Hello", size, TextAlign::Left, 0.0, 0.0)
                    .elements()
                    .is_empty()
            );
        }
    }
}
