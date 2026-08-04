//! Versioned `.rmot` format: `serde_json` of `Document` + migrations.

use renamite_model::Document;
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum RmotError {
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported format version {0}")]
    UnsupportedVersion(u32),
    #[error("missing format_version")]
    MissingVersion,
}

const CURRENT_VERSION: u32 = 1;

pub fn save(doc: &Document) -> Result<Vec<u8>, RmotError> {
    let mut root = serde_json::to_value(doc)?;
    root["format_version"] = Value::from(CURRENT_VERSION);
    Ok(serde_json::to_vec(&root)?)
}

pub fn open(bytes: &[u8]) -> Result<Document, RmotError> {
    let mut value: Value = serde_json::from_slice(bytes)?;
    let version = value
        .get("format_version")
        .and_then(Value::as_u64)
        .ok_or(RmotError::MissingVersion)? as u32;
    if version > CURRENT_VERSION {
        return Err(RmotError::UnsupportedVersion(version));
    }
    for v in version..CURRENT_VERSION {
        value = migrate_version(value, v);
    }
    Ok(serde_json::from_value(value)?)
}

/// Migration stub: bump versions as the document format evolves.
fn migrate_version(mut value: Value, from: u32) -> Value {
    value["format_version"] = Value::from(from + 1);
    value
}