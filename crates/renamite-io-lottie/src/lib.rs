//! Lottie I/O.
//!
//! Separate DTO module - never `#[serde]` the internal model onto Lottie field
//! names. Unknown fields pass through so open -> tweak -> export doesn't strip
//! unsupported features.

use renamite_animation::FrameRate;
use renamite_model::Document;
use serde_json::Value;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct LottieVersion(pub u32);

/// Import a Lottie JSON document into the internal model.
pub fn import(_json: &Value) -> Result<Document, LottieError> {
    let doc = Document {
        format_version: 1,
        compositions: Default::default(),
        nodes: Default::default(),
        assets: Default::default(),
        main: Default::default(),
    };
    let _ = doc;
    Err(LottieError::NotImplemented)
}

/// Export the internal model to a Lottie JSON value.
pub fn export(_doc: &Document) -> Result<Value, LottieError> {
    Ok(Value::Null)
}

/// Always 60fps for now (Lottie's default frame rate handling).
pub fn default_rate() -> FrameRate {
    FrameRate { num: 60, den: 1 }
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum LottieError {
    #[error("lottie import/export not yet implemented")]
    NotImplemented,
    #[error("unsupported lottie feature: {0}")]
    Unsupported(&'static str),
}