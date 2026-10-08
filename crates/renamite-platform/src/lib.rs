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

use game_utils::save_store::SaveStore;
use game_utils::storage::FsStorage;

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
                let ok = match futures_lite::future::block_on(rlobkit_dialogs::RlobKit::save_bytes(
                    opts, &data,
                )) {
                    Ok(Some(_)) => true,
                    Ok(None) => {
                        log::warn!("save_bytes: picker dismissed, nothing written");
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

pub const AUTOSAVE_KEY: &str = "last-session.ren";

/// Why an autosave write did not land, when the payload itself is fine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutosaveSkip {
    /// Larger than the backend can hold. On wasm the store is
    /// `localStorage`-backed (~5 MB total, base64-inflated), so a large
    /// document is skipped rather than evicting unrelated keys.
    TooLarge { len: usize, cap: usize },
    /// The backend refused the write (quota, permissions, read-only volume).
    /// The previous shadow copy is left intact.
    WriteFailed,
}

/// Largest autosave payload the active backend accepts.
pub fn autosave_cap() -> usize {
    // `ropfs::sync::Fs` is localStorage: ~5 MB for the whole origin, every
    // value base64-inflated 4/3, and a write holds `temp` + `target` + `.bak`
    // live at once. 1 MB of document is ~4 MB of quota at the peak, which
    // leaves room for the rest of the origin's keys.
    #[cfg(target_arch = "wasm32")]
    {
        1024 * 1024
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        256 * 1024 * 1024
    }
}

/// Crash-recovery autosave: a single shadow copy of the open document in the
/// app's data dir, never next to the user's own files.
///
/// Backed by game-utils' [`FsStorage`], which is `std::fs` on native and a
/// `ropfs` localStorage shim on wasm, so this works on every target.
pub fn autosave_store() -> SaveStore<FsStorage> {
    let mut store =
        SaveStore::new(autosave_dir(), AUTOSAVE_KEY).with_validator(is_autosave_payload);
    // A torn shadow copy is worth nothing, and on wasm keeping one would
    // spend scarce quota on a garbage document the user can never open.
    store.quarantine_corrupt = false;
    store
}

/// The autosave payload is a whole `.ren` document, so it is neither JSON nor
/// necessarily RON (`save_binary` postcard output is also valid). Accept any
/// non-empty payload and let the document parser be the real integrity check;
/// the store's validator exists to avoid quarantining on a torn write.
fn is_autosave_payload(bytes: &[u8]) -> bool {
    !bytes.is_empty()
}

fn autosave_dir() -> PathBuf {
    let base = std::env::var_os("RENAMITE_DATA_DIR")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty());
    base.unwrap_or_else(default_data_dir)
}

#[cfg(target_arch = "wasm32")]
fn default_data_dir() -> PathBuf {
    // `FsStorage` on wasm is `ropfs::sync::Fs`: paths are virtual keys
    // hydrated from localStorage, and `directories` cannot resolve a home dir
    // in a browser. A relative path matches game-utils' own wasm convention.
    PathBuf::from("renamite")
}

#[cfg(all(not(target_os = "android"), not(target_arch = "wasm32")))]
fn default_data_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "renamite")
        .map(|dirs| dirs.data_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("renamite"))
}

#[cfg(target_os = "android")]
fn default_data_dir() -> PathBuf {
    // `ProjectDirs` is unusable on Android ($HOME unset), so this is the
    // runtime internal data dir recorded at boot, else app-private storage
    // under the real package id, else the evictable temp dir.
    game_utils::storage::android_data_dir(android_package())
}

#[cfg(target_os = "android")]
static ANDROID_PACKAGE: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

#[cfg(target_os = "android")]
fn android_package() -> &'static str {
    ANDROID_PACKAGE.get().copied().unwrap_or("org.mlm.renamite")
}

/// Record the Android runtime internal data dir so [`default_data_dir`] lands
/// in app-private storage instead of the evictable temp dir. `package` must
/// match the application id, and is only consulted if the runtime dir was
/// never recorded. Call once from `android_main`. No-op on other targets.
#[cfg(target_os = "android")]
pub fn set_android_data_dir(path: PathBuf, package: &'static str) {
    let _ = ANDROID_PACKAGE.set(package);
    game_utils::storage::set_android_data_dir(path);
}

#[cfg(not(target_os = "android"))]
pub fn set_android_data_dir(_path: PathBuf, _package: &'static str) {}

pub fn autosave_bytes() -> Option<Vec<u8>> {
    let store = autosave_store();
    if let Some(bytes) = store.load(&is_autosave_payload, &[]).data {
        return Some(bytes);
    }
    // The pre-`game-utils` store was `<data>/autosave/last-session`; adopt it
    // once so an upgrade keeps the recovery it would otherwise drop.
    adopt_legacy_autosave(&store)
}

#[cfg(all(not(target_os = "android"), not(target_arch = "wasm32")))]
fn adopt_legacy_autosave(store: &SaveStore<FsStorage>) -> Option<Vec<u8>> {
    let legacy_dir = store.dir.join("autosave");
    let legacy = legacy_dir.join("last-session");
    let bytes = std::fs::read(&legacy).ok().filter(|b| !b.is_empty())?;
    if store.write(&bytes).is_ok() {
        let _ = std::fs::remove_file(&legacy);
        let _ = std::fs::remove_dir(&legacy_dir);
    }
    Some(bytes)
}

#[cfg(any(target_os = "android", target_arch = "wasm32"))]
fn adopt_legacy_autosave(_store: &SaveStore<FsStorage>) -> Option<Vec<u8>> {
    None
}

/// Write the shadow copy, or report why it was skipped. A failed write leaves
/// the previous copy in place.
pub fn set_autosave(value: &[u8]) -> Result<(), AutosaveSkip> {
    let cap = autosave_cap();
    if value.len() > cap {
        return Err(AutosaveSkip::TooLarge {
            len: value.len(),
            cap,
        });
    }
    if let Err(error) = autosave_store().write(value) {
        log::error!("autosave write failed: {error}");
        return Err(AutosaveSkip::WriteFailed);
    }
    Ok(())
}

pub fn clear_autosave() {
    autosave_store().delete();
}

/// Monotonic-ish milliseconds since the Unix epoch.
pub fn now_ms() -> f64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}
