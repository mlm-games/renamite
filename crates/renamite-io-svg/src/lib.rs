//! SVG import and export.
//!
//! Import: `usvg` flatten -> paths/groups. Export: static SVG in v0.2; SMIL /
//! SVG sequence later (Glaxnimate 0.6 reworked this).

use renamite_model::Document;

#[derive(Debug, thiserror::Error)]
pub enum SvgError {
    #[error("svg error: {0}")]
    Usvg(#[from] usvg::Error),
    #[error("svg import/export not yet implemented")]
    NotImplemented,
}

/// Import a static SVG document.
pub fn import_svg(_data: &[u8]) -> Result<Document, SvgError> {
    Err(SvgError::NotImplemented)
}

/// Export static SVG.
pub fn export_svg(_doc: &Document) -> Result<String, SvgError> {
    Ok(String::new())
}