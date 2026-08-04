//! Platform backends: files, clipboard, autosave.
//!
//! Callback-only (no `async` in the trait) so web (`<input type=file>`,
//! Blob download, IndexedDB) and native (`rfd`, `std::fs`) stay symmetric.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct FileFilter {
    pub name: String,
    pub extensions: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoadedFile {
    pub name: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct SaveResult {
    pub ok: bool,
}

/// Async callbacks as boxed closures so both backends stay simple.
pub type Callback<T> = Box<dyn FnOnce(T) + Send>;

pub trait Platform: Send + Sync {
    fn open_file(&self, filters: &[FileFilter], cb: Callback<Option<LoadedFile>>);
    fn save_file(&self, suggested_name: &str, bytes: Vec<u8>, cb: Callback<SaveResult>);
    fn clipboard_set(&self, mime: &str, bytes: Vec<u8>);
    fn clipboard_get(&self, mime: &str, cb: Callback<Option<Vec<u8>>>);
    fn autosave_store(&self) -> Box<dyn KvStore>;
    fn now_ms(&self) -> f64;
}

/// Durable key/value storage for autosave: dir native / IndexedDB web.
pub trait KvStore: Send + Sync {
    fn get(&self, key: &str) -> Option<Vec<u8>>;
    fn set(&self, key: &str, value: &[u8]);
}

/// Native backend using `rfd` dialogs and the filesystem.
#[cfg(feature = "native")]
pub struct NativePlatform;

#[cfg(feature = "native")]
impl Platform for NativePlatform {
    fn open_file(&self, _filters: &[FileFilter], _cb: Callback<Option<LoadedFile>>) {
        // TODO: rfd::FileDialog
    }
    fn save_file(&self, _suggested_name: &str, _bytes: Vec<u8>, _cb: Callback<SaveResult>) {}
    fn clipboard_set(&self, _mime: &str, _bytes: Vec<u8>) {}
    fn clipboard_get(&self, _mime: &str, _cb: Callback<Option<Vec<u8>>>) {}
    fn autosave_store(&self) -> Box<dyn KvStore> {
        Box::new(DirStore { dir: std::path::PathBuf::new() })
    }
    fn now_ms(&self) -> f64 {
        0.0
    }
}

/// Filesystem-backed store.
pub struct DirStore {
    pub dir: std::path::PathBuf,
}

impl KvStore for DirStore {
    fn get(&self, _key: &str) -> Option<Vec<u8>> {
        None
    }
    fn set(&self, _key: &str, _value: &[u8]) {}
}