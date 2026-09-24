//! `.ren` - renamite project format, in RON.
//!
//! One file = artboards (Document) + named Clips + Machines + start config.
//! RON gives: comments in source files, trailing commas, real enum syntax
//! (`Clip(clip: ..., loop_mode: Loop)`), and clean git diffs. (for a better alternative to .riv)

use renamite_machine::{ClipId, ClipMap, MachineId, MachineMap, StateKind};
use renamite_model::Document;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const EXT: &str = "ren";
pub const EXT_BINARY: &str = "renb";
pub const CURRENT_VERSION: u32 = 1;
pub const MAX_TEXT_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_BINARY_BYTES: usize = 256 * 1024 * 1024;
const MAX_RON_DEPTH: usize = 256;
const MAX_RON_ITEMS: usize = 2_000_000;
const MAX_RON_STRING_BYTES: usize = 64 * 1024 * 1024;
const RENB_MAGIC: &[u8; 4] = b"RENB";

#[derive(Clone, Debug, Serialize, Deserialize)]
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
    /// Attached/live clips in UI order. Arena entries outside this vec are undo
    /// history; `garbage_collect` drops them before save.
    #[serde(default)]
    pub clip_order: Vec<ClipId>,
    /// Attached/live machines in UI order.
    #[serde(default)]
    pub machine_order: Vec<MachineId>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Meta {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub generator: String,
}

impl RenFile {
    pub fn new(document: Document, name: impl Into<String>) -> Self {
        Self {
            format_version: CURRENT_VERSION,
            meta: Meta {
                name: name.into(),
                generator: "renamite".into(),
                ..Default::default()
            },
            document,
            clips: ClipMap::default(),
            machines: MachineMap::default(),
            start_machine: None,
            clip_order: Vec::new(),
            machine_order: Vec::new(),
        }
    }

    /// Repair invariants after parsing (legacy files predate order vecs).
    pub fn normalize(&mut self) {
        self.document.normalize_assets();
        self.clip_order.retain(|id| self.clips.contains_key(*id));
        let seen: HashSet<_> = self.clip_order.iter().copied().collect();
        for id in self.clips.keys() {
            if !seen.contains(&id) {
                self.clip_order.push(id);
            }
        }
        self.machine_order
            .retain(|id| self.machines.contains_key(*id));
        let seen: HashSet<_> = self.machine_order.iter().copied().collect();
        for id in self.machines.keys() {
            if !seen.contains(&id) {
                self.machine_order.push(id);
            }
        }
        if let Some(s) = self.start_machine
            && !self.machines.contains_key(s)
        {
            self.start_machine = None;
        }
    }

    /// Drop arena entries not reachable from the order vecs. Mirror of
    /// Document::garbage_collect.
    pub fn garbage_collect(&mut self) {
        let mut live_m: HashSet<_> = self.machine_order.iter().copied().collect();
        if let Some(start) = self.start_machine {
            live_m.insert(start);
        }
        self.machines.retain(|id, _| live_m.contains(&id));
        if let Some(s) = self.start_machine
            && !self.machines.contains_key(s)
        {
            self.start_machine = None;
        }
        if let Some(s) = self.start_machine
            && !self.machine_order.contains(&s)
        {
            self.machine_order.push(s);
        }
        let mut live_c: HashSet<_> = self.clip_order.iter().copied().collect();
        for m in self.machines.values() {
            for layer in &m.layers {
                for state in &layer.states {
                    match &state.kind {
                        StateKind::Clip { clip, .. } => {
                            live_c.insert(*clip);
                        }
                        StateKind::Blend1D { children, .. } => {
                            for ch in children {
                                live_c.insert(ch.clip);
                            }
                        }
                        StateKind::Empty => {}
                    }
                }
            }
        }
        self.clips.retain(|id, _| live_c.contains(&id));
        let mut clip_seen = self.clip_order.iter().copied().collect::<HashSet<_>>();
        self.clip_order
            .retain(|id| self.clips.contains_key(*id) && clip_seen.insert(*id));
        for id in self.clips.keys() {
            if live_c.contains(&id) && clip_seen.insert(id) {
                self.clip_order.push(id);
            }
        }
        self.document.garbage_collect();
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
    #[error("ren input limit exceeded: {0}")]
    InputLimit(&'static str),
    #[cfg(feature = "binary")]
    #[error("postcard error: {0}")]
    Postcard(#[from] postcard::Error),
}

pub fn save(file: &RenFile) -> Result<String, RenError> {
    let cfg = ron::ser::PrettyConfig::default()
        .struct_names(true) // `Keyframe(frame: ...)`
        .new_line("\n");
    let mut s = String::from("// renamite project: https://github.com/mlm-games/renamite\n");
    s.push_str(&ron::ser::to_string_pretty(file, cfg)?);
    Ok(s)
}

pub fn open(text: &str) -> Result<RenFile, RenError> {
    let mut file = open_unormalized(text)?;
    file.normalize();
    Ok(file)
}

pub fn open_unormalized(text: &str) -> Result<RenFile, RenError> {
    preflight_ron(text)?;
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

fn preflight_ron(text: &str) -> Result<(), RenError> {
    let bytes = text.as_bytes();
    if bytes.len() > MAX_TEXT_BYTES {
        return Err(RenError::InputLimit("text input is too large"));
    }

    let mut index = 0usize;
    let mut depth = 0usize;
    let mut items = 0usize;
    let mut string_len = 0usize;
    let mut escaped = false;
    let mut state = 0u8;
    let mut raw_hashes = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        if state == 1 {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                state = 0;
            } else {
                string_len += 1;
                if string_len > MAX_RON_STRING_BYTES {
                    return Err(RenError::InputLimit("RON string is too large"));
                }
            }
            index += 1;
            continue;
        }
        if state == 2 {
            if byte == b'\'' {
                state = 0;
            } else if byte == b'\\' {
                index += 1;
            }
            index += 1;
            continue;
        }
        if state == 3 {
            if byte == b'"' {
                let hashes = bytes
                    .get(index + 1..index + 1 + raw_hashes)
                    .is_some_and(|tail| tail.iter().all(|&value| value == b'#'));
                if hashes {
                    index += raw_hashes + 1;
                    state = 0;
                    string_len = 0;
                }
            } else {
                string_len += 1;
                if string_len > MAX_RON_STRING_BYTES {
                    return Err(RenError::InputLimit("RON string is too large"));
                }
            }
            index += 1;
            continue;
        }
        if state == 4 {
            if byte == b'*' && bytes.get(index + 1) == Some(&b'/') {
                state = 0;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }

        if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            state = 4;
            index += 2;
            continue;
        }
        if byte == b'"' {
            state = 1;
            escaped = false;
            string_len = 0;
            index += 1;
            continue;
        }
        if byte == b'\'' {
            state = 2;
            index += 1;
            continue;
        }
        if byte == b'r' {
            let mut cursor = index + 1;
            while bytes.get(cursor) == Some(&b'#') {
                cursor += 1;
            }
            if cursor > index + 1 && bytes.get(cursor) == Some(&b'"') {
                raw_hashes = cursor - index - 1;
                state = 3;
                string_len = 0;
                index = cursor + 1;
                continue;
            }
        }
        if matches!(byte, b'(' | b'[' | b'{') {
            depth += 1;
            items = items.saturating_add(1);
            if depth > MAX_RON_DEPTH {
                return Err(RenError::InputLimit("RON nesting is too deep"));
            }
        } else if matches!(byte, b')' | b']' | b'}') {
            depth = depth.saturating_sub(1);
        } else if byte == b',' {
            items = items.saturating_add(1);
        }
        if items > MAX_RON_ITEMS {
            return Err(RenError::InputLimit("RON contains too many values"));
        }
        index += 1;
    }
    Ok(())
}

fn preflight_postcard(bytes: &[u8]) -> Result<(), RenError> {
    if bytes.len() > MAX_BINARY_BYTES {
        return Err(RenError::InputLimit("binary input is too large"));
    }
    Ok(())
}

fn peek_version(v: &ron::value::Value) -> Option<i64> {
    let ron::value::Value::Map(map) = v else {
        return None;
    };
    match map.get(&ron::value::Value::String("format_version".into())) {
        Some(ron::value::Value::Number(n)) => number_i64(n),
        _ => None,
    }
}

fn number_i64(n: &ron::value::Number) -> Option<i64> {
    use ron::value::Number::*;
    match n {
        I8(v) => Some(*v as i64),
        I16(v) => Some(*v as i64),
        I32(v) => Some(*v as i64),
        I64(v) => Some(*v),
        U8(v) => Some(*v as i64),
        U16(v) => Some(*v as i64),
        U32(v) => Some(*v as i64),
        U64(v) => Some(*v as i64),
        F32(v) => Some(v.get() as i64),
        F64(v) => Some(v.get() as i64),
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

/// True when `bytes` starts with the `.renb` header (magic + version).
/// Shared so loaders (CLI, `Player::from_bytes`, host asset readers) agree on
/// one source of truth instead of re-sniffing the magic themselves.
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && &bytes[..4] == RENB_MAGIC
}

#[cfg(feature = "binary")]
pub fn open_binary(bytes: &[u8]) -> Result<RenFile, RenError> {
    let mut file = open_binary_unormalized(bytes)?;
    file.normalize();
    Ok(file)
}

#[cfg(feature = "binary")]
mod legacy_binary;

#[cfg(feature = "binary")]
pub fn open_binary_unormalized(bytes: &[u8]) -> Result<RenFile, RenError> {
    if bytes.len() > MAX_BINARY_BYTES {
        return Err(RenError::InputLimit("binary input is too large"));
    }
    let (magic, rest) = bytes.split_at_checked(8).ok_or(RenError::BadMagic)?;
    if &magic[..4] != RENB_MAGIC {
        return Err(RenError::BadMagic);
    }
    let version = u32::from_le_bytes(magic[4..8].try_into().unwrap());
    if version > CURRENT_VERSION {
        return Err(RenError::UnsupportedVersion(version));
    }
    preflight_postcard(rest)?;
    match postcard::from_bytes::<RenFile>(rest) {
        Ok(file) => Ok(file),
        Err(error) => legacy_binary::decode(rest).map_err(|_| error.into()),
    }
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

    #[cfg(feature = "binary")]
    #[test]
    fn binary_roundtrip() {
        let f = RenFile::new(renamite_model::Document::empty(), "bin");
        let back = open_binary(&save_binary(&f).unwrap()).unwrap();
        assert_eq!(back.meta.name, "bin");
    }
}
