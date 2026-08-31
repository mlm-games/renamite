//! Platform glue: file dialogs (via `rlobkit-dialogs`) + autosave storage.
//!
//! Pattern mirrors my `repadio`'s `player-platform`: a thin crate gated on the
//! *target* (not a Cargo feature), with a non-blocking callback API that works
//! on every platform and a few blocking helpers reserved for desktop. Dialogs
//! go through `rlobkit-dialogs` so one crate serves every platform (native
//! backends on desktop, browser/Activity pickers on WASM/Android).

use std::path::PathBuf;

/// File dialogs.
pub mod dialogs {
    use std::path::PathBuf;

    /// A file picked by the user: a real path (desktop) or name+bytes
    /// (WASM/Android, where the OS hands us a URI/blob, not a path).
    #[derive(Clone, Debug)]
    pub enum PickedFile {
        Path(PathBuf),
        Bytes { name: String, data: Vec<u8> },
    }

    /// Result of an async save. `path` is `Some` on desktop (the written
    /// filesystem path); WASM/Android drives the save through the OS so they
    /// only report success/failure.
    #[derive(Clone, Debug)]
    pub struct SaveOutcome {
        pub ok: bool,
        pub path: Option<PathBuf>,
    }

    /// Register platform I/O callbacks (Android only). No-op elsewhere.
    /// Must be called once at app startup.
    pub fn init() {
        rlobkit_dialogs::init();
    }

    /// Build the `OpenFileOptions` used for a single-file picker.
    #[allow(dead_code)] // used by the non-desktop target branches
    fn open_options(title: &str, extensions: &[&str]) -> rlobkit_dialogs::picker::OpenFileOptions {
        let exts: Vec<String> = extensions.iter().map(|s| s.to_string()).collect();
        rlobkit_dialogs::picker::OpenFileOptions {
            file_type: rlobkit_dialogs::RlobKitType::Custom {
                extensions: exts,
                mime_types: vec![],
            },
            mode: rlobkit_dialogs::RlobKitMode::Single,
            title: Some(title.to_string()),
            initial_directory: None,
        }
    }

    /// Build `SaveFileOptions` sharing the `RlobKitType` construction.
    #[allow(dead_code)] // used by the non-desktop target branches
    fn save_options(
        title: &str,
        suggested_name: &str,
        extensions: &[&str],
    ) -> rlobkit_dialogs::picker::SaveFileOptions {
        let exts: Vec<String> = extensions.iter().map(|s| s.to_string()).collect();
        rlobkit_dialogs::picker::SaveFileOptions {
            suggested_name: Some(suggested_name.to_string()),
            file_type: Some(rlobkit_dialogs::RlobKitType::Custom {
                extensions: exts,
                mime_types: vec![],
            }),
            title: Some(title.to_string()),
            ..Default::default()
        }
    }

    /// Non-blocking single-file open dialog. Works on every target:
    /// desktop spawns a thread running the native blocking dialog. Android
    /// spawns a thread driving the Activity picker. WASM runs the browser
    /// picker on the main thread. `on_done(None)` fires on cancel/error.
    pub fn pick_open_file(
        title: &'static str,
        extensions: &'static [&'static str],
        on_done: Box<dyn FnOnce(Option<PickedFile>) + Send + 'static>,
    ) {
        #[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
        {
            std::thread::spawn(move || {
                on_done(
                    rlobkit_dialogs::blocking_open_file(title, extensions).map(PickedFile::Path),
                );
            });
        }
        #[cfg(target_os = "android")]
        {
            std::thread::spawn(move || {
                let opts = open_options(title, extensions);
                let picked = futures_lite::future::block_on(
                    rlobkit_dialogs::RlobKit::open_file_picker(opts),
                )
                .ok()
                .flatten()
                .and_then(|mut v| v.pop())
                .and_then(|f| {
                    let name = f.name().to_string();
                    match f.read_bytes() {
                        Ok(data) => Some(PickedFile::Bytes {
                            name,
                            data: data.to_vec(),
                        }),
                        Err(e) => {
                            log::error!("read picker file failed: {e}");
                            None
                        }
                    }
                });
                on_done(picked);
            });
        }
        #[cfg(target_arch = "wasm32")]
        {
            let opts = open_options(title, extensions);
            wasm_bindgen_futures::spawn_local(async move {
                let picked = rlobkit_dialogs::RlobKit::open_file_picker(opts)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|mut v| v.pop())
                    .and_then(|f| {
                        let name = f.name().to_string();
                        match f
                            .data()
                            .map(|b| b.to_vec())
                            .or_else(|| f.read_bytes().ok().map(|b| b.to_vec()))
                        {
                            Some(data) => Some(PickedFile::Bytes { name, data }),
                            None => None,
                        }
                    });
                on_done(picked);
            });
        }
    }

    /// Non-blocking save dialog that writes `data`. Works on every target.
    /// `on_done` is called with the outcome after the OS finishes.
    pub fn save_bytes(
        title: &'static str,
        suggested_name: String,
        extensions: &'static [&'static str],
        data: Vec<u8>,
        on_done: Box<dyn FnOnce(SaveOutcome) + Send + 'static>,
    ) {
        #[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
        {
            std::thread::spawn(move || {
                let outcome = match rlobkit_dialogs::blocking_save_file(
                    title,
                    &suggested_name,
                    &extensions.join(","),
                ) {
                    Some(path) => SaveOutcome {
                        ok: std::fs::write(&path, &data).is_ok(),
                        path: Some(path),
                    },
                    None => SaveOutcome {
                        ok: false,
                        path: None,
                    },
                };
                on_done(outcome);
            });
        }
        #[cfg(target_os = "android")]
        {
            std::thread::spawn(move || {
                let opts = save_options(title, &suggested_name, extensions);
                let ok = futures_lite::future::block_on(rlobkit_dialogs::RlobKit::save_bytes(
                    opts, &data,
                ))
                .ok()
                .flatten()
                .is_some();
                on_done(SaveOutcome { ok, path: None });
            });
        }
        #[cfg(target_arch = "wasm32")]
        {
            let opts = save_options(title, &suggested_name, extensions);
            wasm_bindgen_futures::spawn_local(async move {
                let ok = rlobkit_dialogs::RlobKit::save_bytes(opts, &data)
                    .await
                    .ok()
                    .flatten()
                    .is_some();
                on_done(SaveOutcome { ok, path: None });
            });
        }
    }

    /// Ask for a write path *without* writing. Blocking. Desktop only
    /// (used by the synchronous Save flow so the unsaved guard stays correct).
    #[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
    pub fn export_path(title: &str, suggested_name: &str, extensions: &[&str]) -> Option<PathBuf> {
        rlobkit_dialogs::blocking_save_file(title, suggested_name, &extensions.join(","))
    }
}

/// Filesystem-backed autosave store (desktop).
#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
pub fn autosave_store() -> DirStore {
    let base = std::env::var_os("RENAMITE_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_DATA_HOME").map(|p| PathBuf::from(p).join("renamite")))
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("autosave");
    let _ = std::fs::create_dir_all(&dir);
    DirStore { dir }
}

/// Durable key/value storage for autosave.
pub trait KvStore: Send + Sync {
    fn get(&self, key: &str) -> Option<Vec<u8>>;
    fn set(&self, key: &str, value: &[u8]);
}

/// Filesystem-backed store.
pub struct DirStore {
    pub dir: PathBuf,
}

impl KvStore for DirStore {
    fn get(&self, key: &str) -> Option<Vec<u8>> {
        let path = self.dir.join(sanitize_key(key));
        std::fs::read(&path).ok()
    }
    fn set(&self, key: &str, value: &[u8]) {
        let path = self.dir.join(sanitize_key(key));
        let _ = std::fs::write(&path, value);
    }
}

fn sanitize_key(key: &str) -> String {
    key.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Monotonic-ish milliseconds since the Unix epoch.
pub fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}
