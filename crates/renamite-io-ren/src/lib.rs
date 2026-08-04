//! `.ren` - renamite project format, in RON.
//!
//! One file = artboards (Document) + named Clips + Machines + start config.
//! RON gives: comments in source files, trailing commas, real enum syntax
//! (`Clip(clip: ..., loop_mode: Loop)`), and clean git diffs. (for a better alternative to .riv)

use renamite_machine::{ClipMap, MachineId, MachineMap};
use renamite_model::Document;
use serde::{Deserialize, Serialize};

pub const EXT: &str = "ren";
pub const EXT_BINARY: &str = "renb";
pub const CURRENT_VERSION: u32 = 1;
#[cfg(feature = "binary")]
const RENB_MAGIC: &[u8; 4] = b"RENB";

#[derive(Clone, Serialize, Deserialize)]
pub struct RenFile {
    pub format_version: u32,
    pub meta: Meta,
    pub document: Document,
    #[serde(default)]
    pub clips: ClipMap,
    #[serde(default)]
    pub machines: MachineMap,
    /// Machine auto-started by runtimes/preview (None = plain timeline playback).
    #[serde(default)]
    pub start_machine: Option<MachineId>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Meta {
    #[serde(default)] pub name: String,
    #[serde(default)] pub author: String,
    #[serde(default)] pub generator: String,
}

impl RenFile {
    pub fn new(document: Document, name: impl Into<String>) -> Self {
        Self {
            format_version: CURRENT_VERSION,
            meta: Meta { name: name.into(), generator: "renamite".into(), ..Default::default() },
            document,
            clips: ClipMap::default(),
            machines: MachineMap::default(),
            start_machine: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RenError {
    #[error("ron parse error: {0}")]
    Parse(#[from] ron::error::SpannedError),
    #[error("ron write error: {0}")]
    Write(#[from] ron::Error),
    #[error("unsupported .ren version {0} (this build reads <= {CURRENT_VERSION})")]
    UnsupportedVersion(u32),
    #[error("not a .renb file (bad magic)")]
    BadMagic,
    #[cfg(feature = "binary")]
    #[error("postcard error: {0}")]
    Postcard(#[from] postcard::Error),
}

pub fn save(file: &RenFile) -> Result<String, RenError> {
let cfg = ron::ser::PrettyConfig::default()
        .struct_names(true); // `Keyframe(frame: ...)`
    let mut s = String::from("// renamite project: https://github.com/mlm-games/renamite\n");
    s.push_str(&ron::ser::to_string_pretty(file, cfg)?);
    Ok(s)
}

pub fn open(text: &str) -> Result<RenFile, RenError> {
    // Peek `format_version` name-agnostically (top-level RON carries its struct
    // label, so a typed `Head` wouldn't match) before full deserialization.
    let raw: ron::value::Value = ron::from_str(text)?;
    let version = peek_version(&raw).unwrap_or(CURRENT_VERSION as i64) as u32;
    if version > CURRENT_VERSION {
        return Err(RenError::UnsupportedVersion(version));
    }
    // v1: no migrations yet. When v2 lands: migrate the Value, then type.
    Ok(ron::from_str(text)?)
}

fn peek_version(v: &ron::value::Value) -> Option<i64> {
    let ron::value::Value::Map(map) = v else { return None };
    match map.get(&ron::value::Value::String("format_version".into())) {
        Some(ron::value::Value::Number(n)) => number_i64(n),
        _ => None,
    }
}

fn number_i64(n: &ron::value::Number) -> Option<i64> {
    use ron::value::Number::*;
    match n {
        I8(v) => Some(*v as i64), I16(v) => Some(*v as i64), I32(v) => Some(*v as i64),
        I64(v) => Some(*v),
        U8(v) => Some(*v as i64), U16(v) => Some(*v as i64), U32(v) => Some(*v as i64),
        U64(v) => Some(*v as i64),
        F32(v) => Some(v.get() as i64), F64(v) => Some(v.get() as i64),
        _ => None,
    }
}

#[cfg(feature = "binary")]
pub fn save_binary(file: &RenFile) -> Result<Vec<u8>, RenError> {
    let mut out = RENB_MAGIC.to_vec();
    out.extend_from_slice(&CURRENT_VERSION.to_le_bytes());
    out.extend(postcard::to_stdvec(file)?);
    Ok(out)
}

#[cfg(feature = "binary")]
pub fn open_binary(bytes: &[u8]) -> Result<RenFile, RenError> {
    let (magic, rest) = bytes.split_at_checked(8).ok_or(RenError::BadMagic)?;
    if &magic[..4] != RENB_MAGIC { return Err(RenError::BadMagic); }
    let version = u32::from_le_bytes(magic[4..8].try_into().unwrap());
    if version > CURRENT_VERSION { return Err(RenError::UnsupportedVersion(version)); }
    Ok(postcard::from_bytes(rest)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ron_roundtrip() {
        let f = RenFile::new(renamite_model::Document::empty(), "test");
        let text = save(&f).unwrap();
        assert!(text.starts_with("//"));
        assert!(text.contains("format_version"));
        let back = open(&text).unwrap();
        assert_eq!(back.format_version, CURRENT_VERSION);
        assert_eq!(back.meta.name, "test");
    }

    #[test]
    fn future_version_rejected() {
        let f = RenFile {
            format_version: 999,
            ..RenFile::new(renamite_model::Document::empty(), "x")
        };
        let text = save(&f).unwrap();
        assert!(matches!(open(&text), Err(RenError::UnsupportedVersion(999))));
    }

    #[cfg(feature = "binary")]
    #[test]
    fn binary_roundtrip() {
        let f = RenFile::new(renamite_model::Document::empty(), "bin");
        let back = open_binary(&save_binary(&f).unwrap()).unwrap();
        assert_eq!(back.meta.name, "bin");
    }
}