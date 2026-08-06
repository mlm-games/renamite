//! File lifecycle: New / Open / Save / Save As / Import Lottie / Export PNG,
//! plus an unsaved-changes guard.
//!
//! Pattern mirrors `repadio`: dialogs go through the non-blocking
//! `renamite_platform::dialogs` API which works on every target (native on
//! desktop, browser/Activity pickers on WASM/Android). The picker callbacks
//! run on worker threads, so they do all parsing/serialization off-thread and
//! hand the UI thread ready-to-apply [`PendingFileOp`]s via
//! [`Session::drain_file_ops`].

use crate::session::{PendingFileOp, PendingIntent, SessionRef, default_file};

use std::path::Path;

use renamite_platform::dialogs::PickedFile;

/// Wake the frame loop from a worker thread (desktop/Android) or directly
/// (WASM, where the callback runs on the main thread).
fn wake_ui() {
    #[cfg(target_arch = "wasm32")]
    repose_core::request_frame();
    #[cfg(not(target_arch = "wasm32"))]
    repose_platform::wake_event_loop();
}

fn document_stem(session: &SessionRef) -> String {
    let s = session.borrow();
    s.current_path
        .as_ref()
        .and_then(|p| p.file_stem().map(|v| v.to_string_lossy().into_owned()))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| s.file.meta.name.clone())
}

fn set_status(session: &SessionRef, msg: impl Into<String>) {
    let mut s = session.borrow_mut();
    s.status = Some(msg.into());
    s.bump();
}

fn report_error(session: &SessionRef, e: impl std::fmt::Display) {
    set_status(session, format!("Error: {e}"));
}

/// True for `.renb` (binary) filenames.
fn is_binary(name: &str) -> bool {
    matches!(
        Path::new(name).extension().and_then(|s| s.to_str()),
        Some(ext) if ext.eq_ignore_ascii_case("renb")
    )
}

/// Parse a `.ren` / `.renb` document from raw bytes (name decides the format).
fn parse_ren(name: &str, data: &[u8]) -> anyhow::Result<renamite_io_ren::RenFile> {
    if is_binary(name) {
        Ok(renamite_io_ren::open_binary(data)?)
    } else {
        Ok(renamite_io_ren::open(std::str::from_utf8(data)?)?)
    }
}

fn import_lottie_bytes(name: &str, data: &[u8]) -> anyhow::Result<renamite_io_ren::RenFile> {
    let json: serde_json::Value = serde_json::from_slice(data)?;
    let doc = renamite_io_lottie::import(&json)?;
    let stem = Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Imported".into());
    Ok(renamite_io_ren::RenFile::new(doc, stem))
}

fn request_guard(session: &SessionRef, intent: PendingIntent) -> bool {
    if !session.borrow().dirty {
        return true;
    }
    session.borrow_mut().request_discard(intent);
    false
}

/// Run the deferred intent (guard was resolved by Save or Discard).
pub fn run_pending_intent(session: &SessionRef) {
    let intent = session.borrow_mut().take_pending_intent();
    match intent {
        Some(PendingIntent::New) => new_document_inner(session),
        Some(PendingIntent::Open) => open_document_inner(session),
        Some(PendingIntent::ImportLottie) => import_lottie_inner(session),
        None => {}
    }
}

/// Guard "Save" button: save the document, then run the deferred intent.
#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
pub fn discard_save(session: &SessionRef) {
    session.borrow().confirm_dialog.dismiss();
    if save_document(session) {
        run_pending_intent(session);
    } else {
        session.borrow_mut().clear_pending_intent();
    }
}

/// Guard "Save" button on non-desktop targets: dispatch the async save; the
/// deferred intent runs when its `SaveOutcome` drains (or is dropped if the
/// save is canceled).
#[cfg(any(target_os = "android", target_arch = "wasm32"))]
pub fn discard_save(session: &SessionRef) {
    session.borrow().confirm_dialog.dismiss();
    save_document(session);
}

/// Guard "Discard" button: drop unsaved changes and run the deferred intent.
pub fn discard_discard(session: &SessionRef) {
    session.borrow().confirm_dialog.dismiss();
    run_pending_intent(session);
}

/// Guard "Cancel" button: abort the deferred action.
pub fn discard_cancel(session: &SessionRef) {
    session.borrow().confirm_dialog.dismiss();
    session.borrow_mut().clear_pending_intent();
}

/// New document, discarding the current one (after an unsaved guard).
pub fn new_document(session: &SessionRef) {
    if !request_guard(session, PendingIntent::New) {
        return;
    }
    new_document_inner(session);
}

fn new_document_inner(session: &SessionRef) {
    let mut s = session.borrow_mut();
    let mut file = default_file();
    file.meta.name = "Untitled".into();
    s.replace_file(file);
    s.current_path = None;
    s.status = Some("New document".into());
}

/// Open `.ren` / `.renb` via an async file dialog (after an unsaved guard).
pub fn open_document(session: &SessionRef) {
    if !request_guard(session, PendingIntent::Open) {
        return;
    }
    open_document_inner(session);
}

fn open_document_inner(session: &SessionRef) {
    let ops = { session.borrow().file_ops.clone() };
    renamite_platform::dialogs::pick_open_file(
        "Open project",
        &["ren", "renb"],
        Box::new(move |picked| {
            let op = match picked {
                None => None,
                Some(PickedFile::Path(p)) => match read_ren(&p) {
                    Ok(file) => Some(PendingFileOp::OpenDone {
                        file: Box::new(file),
                        path: Some(p),
                        message: "Opened",
                    }),
                    Err(e) => Some(PendingFileOp::Failed {
                        message: format!("Open failed: {e}"),
                    }),
                },
                Some(PickedFile::Bytes { name, data }) => match parse_ren(&name, &data) {
                    Ok(file) => Some(PendingFileOp::OpenDone {
                        file: Box::new(file),
                        path: None,
                        message: "Opened",
                    }),
                    Err(e) => Some(PendingFileOp::Failed {
                        message: format!("Open failed: {e}"),
                    }),
                },
            };
            if let Some(op) = op {
                ops.lock().unwrap().push_back(op);
            }
            wake_ui();
        }),
    );
}

/// Save to `current_path`, or fall back to Save As. Returns true if written.
#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
pub fn save_document(session: &SessionRef) -> bool {
    let path = session.borrow().current_path.clone();
    match path {
        Some(p) => write_ren(session, &p),
        None => save_document_as(session),
    }
}

/// Non-desktop: there is never a filesystem path, so Save always runs the
/// async Save As flow.
#[cfg(any(target_os = "android", target_arch = "wasm32"))]
pub fn save_document(session: &SessionRef) -> bool {
    save_document_as(session)
}

/// Save As. Desktop uses a blocking dialog + sync write (keeps the unsaved
/// guard flow correct); WASM/Android use the async `save_bytes` picker.
#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
pub fn save_document_as(session: &SessionRef) -> bool {
    let suggested = format!("{}.ren", document_stem(session));
    match renamite_platform::dialogs::export_path("Save project", &suggested, &["ren", "renb"]) {
        Some(path) => write_ren(session, &path),
        None => false,
    }
}

#[cfg(any(target_os = "android", target_arch = "wasm32"))]
pub fn save_document_as(session: &SessionRef) -> bool {
    let suggested = format!("{}.ren", document_stem(session));
    let bytes = match session.borrow().save_snapshot() {
        Ok(b) => b,
        Err(e) => {
            report_error(session, e);
            return false;
        }
    };
    let ops = { session.borrow().file_ops.clone() };
    renamite_platform::dialogs::save_bytes(
        "Save project",
        suggested,
        &["ren"],
        bytes,
        Box::new(move |outcome| {
            ops.lock().unwrap().push_back(PendingFileOp::SaveOutcome {
                ok: outcome.ok,
                path: None,
            });
            wake_ui();
        }),
    );
    true
}

/// Import a Lottie JSON file via an async picker (after an unsaved guard).
pub fn import_lottie(session: &SessionRef) {
    if !request_guard(session, PendingIntent::ImportLottie) {
        return;
    }
    import_lottie_inner(session);
}

fn import_lottie_inner(session: &SessionRef) {
    let ops = { session.borrow().file_ops.clone() };
    renamite_platform::dialogs::pick_open_file(
        "Import Lottie",
        &["json"],
        Box::new(move |picked| {
            let op = match picked {
                None => None,
                Some(PickedFile::Path(p)) => {
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let result = match std::fs::read(&p) {
                        Ok(data) => import_lottie_bytes(&name, &data),
                        Err(e) => Err(e.into()),
                    };
                    match result {
                        Ok(file) => Some(PendingFileOp::OpenDone {
                            file: Box::new(file),
                            path: None,
                            message: "Imported Lottie",
                        }),
                        Err(e) => Some(PendingFileOp::Failed {
                            message: format!("Import failed: {e}"),
                        }),
                    }
                }
                Some(PickedFile::Bytes { name, data }) => match import_lottie_bytes(&name, &data) {
                    Ok(file) => Some(PendingFileOp::OpenDone {
                        file: Box::new(file),
                        path: None,
                        message: "Imported Lottie",
                    }),
                    Err(e) => Some(PendingFileOp::Failed {
                        message: format!("Import failed: {e}"),
                    }),
                },
            };
            if let Some(op) = op {
                ops.lock().unwrap().push_back(op);
            }
            wake_ui();
        }),
    );
}

/// Serialize + write the document to `path`, honoring `.ren` vs `.renb`.
#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
fn write_ren(session: &SessionRef, path: &Path) -> bool {
    let bytes = if is_binary(path.to_string_lossy().as_ref()) {
        session.borrow().pack_snapshot()
    } else {
        session.borrow().save_snapshot()
    };
    match bytes {
        Ok(bytes) => match std::fs::write(path, bytes) {
            Ok(()) => {
                session.borrow_mut().mark_saved(Some(path.to_path_buf()));
                set_status(session, "Saved");
                true
            }
            Err(e) => {
                report_error(session, e);
                false
            }
        },
        Err(e) => {
            report_error(session, e);
            false
        }
    }
}

fn read_ren(path: &Path) -> anyhow::Result<renamite_io_ren::RenFile> {
    let data = std::fs::read(path)?;
    parse_ren(&path.to_string_lossy(), &data)
}

/// Prepare the current playhead scene for export: build the Repose scene and
/// clamp the export pixel size. Pure CPU, runs on any thread.
fn prepare_export(session: &SessionRef) -> anyhow::Result<(repose_core::Scene, (u32, u32))> {
    let (scene, artboard) = {
        let s = session.borrow();
        let size = s.file.document.compositions[s.file.document.main].size;
        let scene = s.engine.scene().clone();
        (scene, size)
    };
    let (w, h) = clamp_export_size(artboard);
    let view = fit_view(artboard, w, h);
    let mut bridge = renamite_render_bridge::SceneRenderer::new();
    let prepared = bridge.prepare(&scene, &view);
    let mut repose = repose_core::Scene::default();
    bridge.append_repose_scene(&prepared, &mut repose);
    Ok((repose, (w, h)))
}

/// Rasterize `repose` on a background thread and encode PNG bytes.
fn render_offscreen_worker(repose: repose_core::Scene, w: u32, h: u32) -> anyhow::Result<Vec<u8>> {
    web_workers::scope(|scope| {
        let result = scope
            .spawn(move || -> anyhow::Result<Vec<u8>> {
                let mut gpu = renamite_render_offscreen::OffscreenRenderer::new_blocking(w, h, 4)?;
                gpu.render_png(&repose, Some([1.0, 1.0, 1.0, 1.0]))
            })
            .join();
        match result {
            Ok(Ok(png)) => Ok(png),
            Ok(Err(e)) => Err(e),
            Err(panic) => Err(anyhow::anyhow!(
                "export worker panicked: {}",
                panic_payload(&panic)
            )),
        }
    })
}

/// Best-effort string for a panic payload (`Box<dyn Any + Send>`).
fn panic_payload(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// Render the current frame and write it to a user-chosen PNG path. Desktop
/// uses a blocking save dialog; WASM/Android save through the OS picker.
#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
pub fn export_png(session: &SessionRef) {
    let Some(path) = renamite_platform::dialogs::export_path("Export frame", "frame.png", &["png"])
    else {
        return;
    };
    let png = match prepare_export(session)
        .and_then(|(scene, (w, h))| render_offscreen_worker(scene, w, h))
    {
        Ok(png) => png,
        Err(e) => {
            report_error(session, e);
            return;
        }
    };
    match std::fs::write(&path, png) {
        Ok(()) => set_status(session, format!("Exported {}", path.display())),
        Err(e) => report_error(session, e),
    }
}

#[cfg(any(target_os = "android", target_arch = "wasm32"))]
pub fn export_png(session: &SessionRef) {
    let png = match prepare_export(session)
        .and_then(|(scene, (w, h))| render_offscreen_worker(scene, w, h))
    {
        Ok(png) => png,
        Err(e) => {
            report_error(session, e);
            return;
        }
    };
    let ops = { session.borrow().file_ops.clone() };
    renamite_platform::dialogs::save_bytes(
        "Export frame",
        "frame.png".to_string(),
        &["png"],
        png,
        Box::new(move |outcome| {
            if outcome.ok {
                ops.lock().unwrap().push_back(PendingFileOp::Exported);
            }
            wake_ui();
        }),
    );
}

fn clamp_export_size(artboard: (u32, u32)) -> (u32, u32) {
    const MAX: u32 = 4096;
    let (w, h) = (artboard.0.max(1), artboard.1.max(1));
    let scale = (MAX as f64 / w as f64).min(MAX as f64 / h as f64).min(1.0);
    (
        (w as f64 * scale).round() as u32,
        (h as f64 * scale).round() as u32,
    )
}

fn fit_view(artboard: (u32, u32), w: u32, h: u32) -> renamite_behavior_common::ViewTransform {
    let scale = (w as f64 / artboard.0.max(1) as f64)
        .min(h as f64 / artboard.1.max(1) as f64)
        .max(1e-6);
    renamite_behavior_common::ViewTransform {
        scale,
        offset: glam::DVec2::new(
            (w as f64 - artboard.0 as f64 * scale) * 0.5,
            (h as f64 - artboard.1 as f64 * scale) * 0.5,
        ),
    }
}
