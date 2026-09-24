//! Platform glue: file dialogs (via `rlobkit-dialogs`) + autosave storage.
//!
//! Pattern mirrors my `repadio`'s `player-platform`: a thin crate gated on the
//! *target* (not a Cargo feature), with a non-blocking callback API that works
//! on every platform and a few blocking helpers reserved for desktop. Dialogs
//! go through `rlobkit-dialogs` so one crate serves every platform (native
//! backends on desktop, browser/Activity pickers on WASM/Android).

#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
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
            use super::atomic_write;
            std::thread::spawn(move || {
                let outcome = match rlobkit_dialogs::blocking_save_file(
                    title,
                    &suggested_name,
                    &extensions.join(","),
                ) {
                    Some(path) => {
                        let ok = match atomic_write(&path, &data) {
                            Ok(()) => true,
                            Err(error) => {
                                log::error!("save failed for {}: {error}", path.display());
                                false
                            }
                        };
                        SaveOutcome {
                            ok,
                            path: Some(path),
                        }
                    }
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
                let result = rlobkit_dialogs::RlobKit::save_bytes(opts, &data).await;
                let ok = match result {
                    Ok(Some(_)) => true,
                    Ok(None) => {
                        log::warn!("save_bytes returned None (picker dismissed?)");
                        false
                    }
                    Err(e) => {
                        log::error!("save_bytes failed: {e:?}");
                        false
                    }
                };
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

#[cfg(not(target_arch = "wasm32"))]
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::ffi::OsString;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no file name")
    })?;

    let (temporary, mut file) = (0..128)
        .find_map(|_| {
            let mut name = OsString::from(".");
            name.push(file_name);
            name.push(format!(
                ".{}.{}.tmp",
                std::process::id(),
                TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let temporary = parent.join(name);
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
            {
                Ok(file) => Some(Ok((temporary, file))),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(error)),
            }
        })
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "could not allocate a unique temporary file",
            )
        })??;

    let result = (|| {
        match fs::metadata(path) {
            Ok(metadata) => fs::set_permissions(&temporary, metadata.permissions())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace_path(&temporary, path)?;
        sync_parent(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(windows)]
fn replace_path(source: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MoveFileExW};

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    let result =
        unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), MOVEFILE_REPLACE_EXISTING) };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(all(not(windows), not(target_arch = "wasm32")))]
fn replace_path(source: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::rename(source, target)
}

#[cfg(all(unix, not(target_arch = "wasm32")))]
fn sync_parent(parent: &Path) -> std::io::Result<()> {
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(all(not(unix), not(target_arch = "wasm32")))]
fn sync_parent(_parent: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
pub const AUTOSAVE_KEY: &str = "last-session";

#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
pub fn autosave_bytes() -> Option<Vec<u8>> {
    autosave_store().get(AUTOSAVE_KEY)
}

#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
pub fn clear_autosave() {
    let path = autosave_store().dir.join(sanitize_key(AUTOSAVE_KEY));
    let _ = std::fs::remove_file(path);
}

/// Filesystem-backed autosave store (desktop).
#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
pub fn autosave_store() -> DirStore {
    let configured = std::env::var_os("RENAMITE_DATA_DIR")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty());
    let data_dir = dirs::data_dir().map(|path| path.join("renamite"));
    let fallback = std::env::temp_dir().join("renamite");
    let candidates = configured
        .into_iter()
        .chain(data_dir)
        .chain(std::iter::once(fallback));

    for base in candidates {
        let dir = base.join("autosave");
        if std::fs::create_dir_all(&dir).is_ok() {
            return DirStore { dir };
        }
    }

    let dir = std::env::temp_dir().join("renamite").join("autosave");
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

impl DirStore {
    pub fn set_checked(&self, key: &str, value: &[u8]) -> std::io::Result<()> {
        if value.len() > 256 * 1024 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "autosave payload is too large",
            ));
        }
        std::fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(sanitize_key(key));
        #[cfg(not(target_arch = "wasm32"))]
        atomic_write(&path, value)?;
        #[cfg(target_arch = "wasm32")]
        std::fs::write(path, value)?;
        Ok(())
    }
}

impl KvStore for DirStore {
    fn get(&self, key: &str) -> Option<Vec<u8>> {
        let path = self.dir.join(sanitize_key(key));
        let metadata = std::fs::metadata(&path).ok()?;
        if !metadata.is_file() || metadata.len() > 256 * 1024 * 1024 {
            return None;
        }
        std::fs::read(path).ok()
    }
    fn set(&self, key: &str, value: &[u8]) {
        if let Err(error) = self.set_checked(key, value) {
            log::error!("autosave write failed: {error}");
        }
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
