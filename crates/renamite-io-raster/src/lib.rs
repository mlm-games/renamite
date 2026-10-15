//! Raster sprite-sheet import and pixel-art tracing.
//!
//! Two modes, because pixel art and gradient art want opposite treatment:
//!
//! - [`RasterMode::Trace`] splits each cell into flat-colour regions and
//!   contours them, so the document becomes resolution-independent vector art.
//!   Any cell whose colour count exceeds
//!   [`RasterOptions::max_colors_per_frame`] stays a raster image node instead,
//!   since contouring a gradient costs vertices and loses fidelity.
//! - [`RasterMode::Embed`] attaches the sheet as an image asset and gives each
//!   frame an image node cropped to its cell.
//!
//! Both modes lay frames out as one composition whose rate is the strip's own
//! playback rate, so a host scrubs by frame index exactly as it would a
//! hand-authored document.

mod trace;

use std::io::Cursor;

use glam::{DVec2, DVec4};
use renamite_animation::{Animated, Frame, FrameRate};
use renamite_geometry::{Anchor, VectorPath};
use renamite_model::{
    Asset, BlendMode, Color, CompoundPath, Document, FillRule, ImageAsset, ImageNode, LayerProps,
    Node, NodeKind, Parent, ShapeKind, StyleKind, StylePaint,
};

pub const MAX_INPUT_BYTES: usize = 64 * 1024 * 1024;

/// One non-fatal compatibility warning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RasterWarning {
    pub path: String,
    pub message: String,
}

impl RasterWarning {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

/// Successful conversion plus non-fatal compatibility warnings.
#[derive(Clone, Debug)]
pub struct RasterReport<T> {
    pub value: T,
    pub warnings: Vec<RasterWarning>,
}

#[derive(Debug, thiserror::Error)]
pub enum RasterError {
    #[error("image decoding failed: {0}")]
    Image(String),
    #[error("cell size {0}x{1} exceeds image bounds {2}x{3}")]
    CellOutOfBounds(u32, u32, u32, u32),
    #[error("cell size must be non-zero")]
    ZeroCell,
    #[error("model error: {0}")]
    Model(#[from] renamite_model::ModelError),
    #[error("raster input limit exceeded: {0}")]
    InputLimit(&'static str),
}

/// How a sheet becomes a document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RasterMode {
    /// Contour each flat-colour region into vector paths.
    Trace,
    /// Keep the pixels, one cropped image node per frame.
    Embed,
}

impl Default for RasterMode {
    fn default() -> Self {
        Self::Trace
    }
}

#[derive(Clone, Debug)]
pub struct RasterOptions {
    /// Frame cell size. `None` treats the whole image as a single frame.
    pub cell: Option<(u32, u32)>,
    /// Composition playback rate, i.e. the strip's own frames per second.
    pub rate: FrameRate,
    pub mode: RasterMode,
    /// Traced cells above this many distinct colours stay raster. `None`
    /// contours everything, however many colours it takes.
    pub max_colors_per_frame: Option<usize>,
}

impl Default for RasterOptions {
    fn default() -> Self {
        Self {
            cell: None,
            rate: FrameRate { num: 12, den: 1 },
            mode: RasterMode::default(),
            max_colors_per_frame: Some(64),
        }
    }
}

/// Cells laid out row-major across the sheet.
#[derive(Clone, Copy, Debug)]
pub struct RasterLayout {
    pub cell: (u32, u32),
    pub columns: u32,
    pub frames: u32,
}

pub fn import(
    bytes: &[u8],
    options: &RasterOptions,
) -> Result<RasterReport<Document>, RasterError> {
    import_with_report(bytes, options)
}

pub fn import_with_report(
    bytes: &[u8],
    options: &RasterOptions,
) -> Result<RasterReport<Document>, RasterError> {
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(RasterError::InputLimit("image larger than 64 MiB"));
    }
    let image = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| RasterError::Image(error.to_string()))?
        .decode()
        .map_err(|error| RasterError::Image(error.to_string()))?;
    let (width, height) = (image.width(), image.height());
    let rgba = image.to_rgba8();
    let layout = layout(width, height, options.cell)?;
    build(bytes, rgba.as_raw(), width, height, layout, options)
}

fn layout(width: u32, height: u32, cell: Option<(u32, u32)>) -> Result<RasterLayout, RasterError> {
    let (cw, ch) = cell.unwrap_or((width, height));
    if cw == 0 || ch == 0 {
        return Err(RasterError::ZeroCell);
    }
    if cw > width || ch > height {
        return Err(RasterError::CellOutOfBounds(cw, ch, width, height));
    }
    Ok(RasterLayout {
        cell: (cw, ch),
        columns: width / cw,
        frames: (width / cw) * (height / ch),
    })
}

fn build(
    bytes: &[u8],
    sheet: &[u8],
    width: u32,
    height: u32,
    layout: RasterLayout,
    options: &RasterOptions,
) -> Result<RasterReport<Document>, RasterError> {
    let (cw, ch) = layout.cell;
    let mut warnings = Vec::new();

    if width % cw != 0 || height % ch != 0 {
        warnings.push(RasterWarning::new(
            "cell",
            format!(
                "sheet {width}x{height} is not a whole number of {cw}x{ch} cells; the trailing \
                 partial row and column are dropped"
            ),
        ));
    }

    // `None` marks a cell that stays raster. Embed mode never traces at all, and
    // trace mode only demotes cells it declines to contour, so the sheet asset
    // is created exactly when at least one cell needs it.
    let mut cells: Vec<Option<Vec<trace::Region>>> = Vec::with_capacity(layout.frames as usize);
    let mut asset_needed = options.mode == RasterMode::Embed;
    if options.mode == RasterMode::Trace {
        for frame in 0..layout.frames {
            let column = frame % layout.columns;
            let row = frame / layout.columns;
            let origin = (column * cw, row * ch);
            let mut buffer = vec![0u8; (cw * ch * 4) as usize];
            for y in 0..ch {
                let src = ((origin.1 + y) * width + origin.0) as usize * 4;
                let dst = (y * cw * 4) as usize;
                buffer[dst..dst + (cw * 4) as usize]
                    .copy_from_slice(&sheet[src..src + (cw * 4) as usize]);
            }
            let regions = trace::trace_cell(&buffer, cw, ch);
            let over_budget = options
                .max_colors_per_frame
                .is_some_and(|cap| regions.len() > cap);
            if over_budget {
                asset_needed = true;
                warnings.push(RasterWarning::new(
                    format!("frames/{frame}"),
                    format!(
                        "{} distinct colours exceeds the trace limit of {}; this frame is \
                         embedded as raster instead",
                        regions.len(),
                        options.max_colors_per_frame.unwrap_or_default()
                    ),
                ));
                cells.push(None);
            } else {
                cells.push(Some(regions));
            }
        }
    } else {
        cells.resize(layout.frames as usize, None);
    }

    let mut document = Document::empty();
    {
        let composition = document
            .main_composition_mut()
            .expect("empty document always has a main composition");
        composition.size = (cw, ch);
        composition.rate = options.rate;
        composition.range = (
            Frame(0),
            Frame(if layout.frames > 1 {
                i64::from(layout.frames) - 1
            } else {
                1
            }),
        );
    }

    let asset = asset_needed.then(|| {
        let id = document.assets.insert(Asset::Image(ImageAsset {
            name: "sheet".into(),
            mime: "image/png".into(),
            bytes: bytes.to_vec(),
            width,
            height,
            srgb: true,
        }));
        document.asset_order.push(id);
        id
    });

    let root = document.create_node(Node::new("Sprite", NodeKind::Group));
    document.attach(root, Parent::Comp(document.main), 0)?;

    for frame in 0..layout.frames {
        let column = frame % layout.columns;
        let row = frame / layout.columns;
        let layer = document.create_node(Node::new(
            format!("frame{frame}"),
            NodeKind::Layer(LayerProps {
                in_frame: Frame(i64::from(frame)),
                out_frame: Frame(i64::from(frame)),
                time_stretch: 1.0,
                blend: BlendMode::Normal,
            }),
        ));
        document.attach(layer, Parent::Node(root), frame as usize)?;

        match (asset, &cells[frame as usize]) {
            (Some(asset), None) => {
                let node = document.create_node(Node::new(
                    format!("raster{frame}"),
                    NodeKind::Image(ImageNode {
                        asset,
                        tint: Animated::new(Color::WHITE),
                        crop: DVec4::new(
                            f64::from(column * cw) / f64::from(width),
                            f64::from(row * ch) / f64::from(height),
                            f64::from(cw) / f64::from(width),
                            f64::from(ch) / f64::from(height),
                        ),
                    }),
                ));
                document.attach(node, Parent::Node(layer), 0)?;
            }
            (_, Some(regions)) => {
                for (index, region) in regions.iter().enumerate() {
                    let group =
                        document.create_node(Node::new(format!("fill{index}"), NodeKind::Group));
                    document.attach(group, Parent::Node(layer), index)?;
                    let shape = document.create_node(Node::new(
                        "path",
                        NodeKind::Shape(ShapeKind::CompoundPath(CompoundPath {
                            contours: region
                                .contours
                                .iter()
                                .map(|contour| Animated::new(vector_path(contour)))
                                .collect(),
                        })),
                    ));
                    document.attach(shape, Parent::Node(group), 0)?;
                    let fill = document.create_node(Node::new(
                        "paint",
                        NodeKind::Style(StyleKind::Fill {
                            paint: StylePaint::Solid {
                                color: Animated::new(Color::rgba(
                                    f64::from(region.color[0]) / 255.0,
                                    f64::from(region.color[1]) / 255.0,
                                    f64::from(region.color[2]) / 255.0,
                                    f64::from(region.color[3]) / 255.0,
                                )),
                            },
                            rule: FillRule::EvenOdd,
                        }),
                    ));
                    document.attach(fill, Parent::Node(group), 1)?;
                }
            }
            (None, None) => unreachable!("a raster frame always carries an asset"),
        }
    }

    Ok(RasterReport {
        value: document,
        warnings,
    })
}

fn vector_path(points: &[[f64; 2]]) -> VectorPath {
    VectorPath {
        anchors: points
            .iter()
            .map(|point| Anchor::corner(DVec2::new(point[0], point[1])))
            .collect(),
        closed: true,
    }
}
