//! SVG import and export.
//!
//! Import: parse static SVG through `usvg` (which resolves CSS, references,
//! `<use>`, primitives, images, and flattened text) into a Renamite
//! [`Document`](renamite_model::Document).
//!
//! Export: a frame snapshot. `Document + frame -> evaluate() -> Scene -> SVG
//! XML`. SMIL/scripting animation is intentionally out of scope.
//!
//! Both directions report non-fatal compatibility problems through
//! [`import_with_report`] / [`export_with_report`].

mod export;
mod import;
mod paint;
mod path;

use renamite_model::{CompId, Document};

/// One non-fatal compatibility warning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SvgWarning {
    pub path: String,
    pub message: String,
}

/// Successful conversion plus non-fatal compatibility warnings.
#[derive(Clone, Debug)]
pub struct SvgReport<T> {
    pub value: T,
    pub warnings: Vec<SvgWarning>,
}

#[derive(Debug, thiserror::Error)]
pub enum SvgError {
    #[error("SVG parse error: {0}")]
    Parse(#[from] usvg::Error),
    #[error("invalid UTF-8")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("missing composition")]
    MissingComposition,
    #[error("model error: {0}")]
    Model(#[from] renamite_model::ModelError),
    #[error("image decoding failed: {0}")]
    Image(String),
}

/// Import a static SVG document, discarding non-fatal warnings.
pub fn import(bytes: &[u8]) -> Result<Document, SvgError> {
    Ok(import_with_report(bytes)?.value)
}

/// Import a static SVG document, returning non-fatal warnings.
pub fn import_with_report(bytes: &[u8]) -> Result<SvgReport<Document>, SvgError> {
    import::import_with_report(bytes)
}

/// Export a composition at a specific frame, discarding non-fatal warnings.
pub fn export_frame(
    document: &Document,
    composition: CompId,
    frame: f64,
) -> Result<String, SvgError> {
    Ok(export_with_report(document, composition, frame)?.value)
}

/// Export a composition at a specific frame, returning non-fatal warnings.
pub fn export_with_report(
    document: &Document,
    composition: CompId,
    frame: f64,
) -> Result<SvgReport<String>, SvgError> {
    export::export_with_report(document, composition, frame)
}
