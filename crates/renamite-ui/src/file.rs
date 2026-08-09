//! File lifecycle: New / Open / Save / Save As / Import Lottie / Import SVG /
//! Export Lottie / Export PNG / Export SVG, plus an unsaved-changes guard.
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
    let report = renamite_io_lottie::import_with_report(&json)?;
    if !report.warnings.is_empty() {
        eprintln!(
            "Lottie import completed with {} warning(s)",
            report.warnings.len()
        );
    }
    let stem = Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Imported".into());
    Ok(renamite_io_ren::RenFile::new(report.value, stem))
}

/// Export the current document to Lottie JSON via the OS save picker
/// (non-blocking, works on every target).
pub fn export_lottie(session: &SessionRef) {
    let suggested = format!("{}.json", document_stem(session));
    let report = {
        let borrowed = session.borrow();
        renamite_io_lottie::export_with_report(&borrowed.file.document)
    };
    let report = match report {
        Ok(report) => report,
        Err(error) => {
            report_error(session, error);
            return;
        }
    };
    if !report.warnings.is_empty() {
        eprintln!(
            "Lottie export completed with {} warning(s)",
            report.warnings.len()
        );
    }
    let bytes = match serde_json::to_vec_pretty(&report.value) {
        Ok(bytes) => bytes,
        Err(error) => {
            report_error(session, error);
            return;
        }
    };
    let ops = session.borrow().file_ops.clone();
    renamite_platform::dialogs::save_bytes(
        "Export Lottie",
        suggested,
        &["json"],
        bytes,
        Box::new(move |outcome| {
            if outcome.ok {
                ops.lock().unwrap().push_back(PendingFileOp::Exported);
            } else {
                ops.lock().unwrap().push_back(PendingFileOp::Failed {
                    message: "Lottie export cancelled or failed".into(),
                });
            }
            wake_ui();
        }),
    );
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
        Some(PendingIntent::ImportSvg) => import_svg_inner(session),
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
    s.welcome = true;
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

fn import_svg_bytes(name: &str, data: &[u8]) -> anyhow::Result<renamite_io_ren::RenFile> {
    let report = renamite_io_svg::import_with_report(data)?;
    if !report.warnings.is_empty() {
        eprintln!(
            "SVG import completed with {} warning(s)",
            report.warnings.len()
        );
    }
    let stem = Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Imported".into());
    Ok(renamite_io_ren::RenFile::new(report.value, stem))
}

/// Import an SVG file via an async picker (after an unsaved guard).
pub fn import_svg(session: &SessionRef) {
    if !request_guard(session, PendingIntent::ImportSvg) {
        return;
    }
    import_svg_inner(session);
}

fn import_svg_inner(session: &SessionRef) {
    let ops = { session.borrow().file_ops.clone() };
    renamite_platform::dialogs::pick_open_file(
        "Import SVG",
        &["svg"],
        Box::new(move |picked| {
            let op = match picked {
                None => None,
                Some(PickedFile::Path(p)) => {
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let result = match std::fs::read(&p) {
                        Ok(data) => import_svg_bytes(&name, &data),
                        Err(e) => Err(e.into()),
                    };
                    match result {
                        Ok(file) => Some(PendingFileOp::OpenDone {
                            file: Box::new(file),
                            path: None,
                            message: "Imported SVG",
                        }),
                        Err(e) => Some(PendingFileOp::Failed {
                            message: format!("Import failed: {e}"),
                        }),
                    }
                }
                Some(PickedFile::Bytes { name, data }) => match import_svg_bytes(&name, &data) {
                    Ok(file) => Some(PendingFileOp::OpenDone {
                        file: Box::new(file),
                        path: None,
                        message: "Imported SVG",
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

/// Render the current playhead frame to SVG and write it to a user-chosen
/// path. Desktop uses a blocking save dialog; WASM/Android use the OS picker.
#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
pub fn export_svg(session: &SessionRef) {
    let Some(path) = renamite_platform::dialogs::export_path("Export SVG", "frame.svg", &["svg"])
    else {
        return;
    };
    let report = {
        let borrowed = session.borrow();
        renamite_io_svg::export_with_report(
            &borrowed.file.document,
            borrowed.file.document.main,
            borrowed.playback.head,
        )
    };
    let svg = match report {
        Ok(report) => {
            if !report.warnings.is_empty() {
                eprintln!(
                    "SVG export completed with {} warning(s)",
                    report.warnings.len()
                );
            }
            report.value
        }
        Err(e) => {
            report_error(session, e);
            return;
        }
    };
    match std::fs::write(&path, svg) {
        Ok(()) => set_status(session, format!("Exported {}", path.display())),
        Err(e) => report_error(session, e),
    }
}

#[cfg(any(target_os = "android", target_arch = "wasm32"))]
pub fn export_svg(session: &SessionRef) {
    let svg = {
        let borrowed = session.borrow();
        match renamite_io_svg::export_with_report(
            &borrowed.file.document,
            borrowed.file.document.main,
            borrowed.playback.head,
        ) {
            Ok(report) => report.value,
            Err(e) => {
                report_error(session, e);
                return;
            }
        }
    };
    let ops = { session.borrow().file_ops.clone() };
    renamite_platform::dialogs::save_bytes(
        "Export SVG",
        "frame.svg".to_string(),
        &["svg"],
        svg.into_bytes(),
        Box::new(move |outcome| {
            if outcome.ok {
                ops.lock().unwrap().push_back(PendingFileOp::Exported);
            }
            wake_ui();
        }),
    );
}

/// Read `.ttf` / `.otf` bytes into the project as a font asset (undoable).
/// The family key is derived by `Session::import_font` from the font itself.
pub fn import_font(session: &SessionRef) {
    let ops = { session.borrow().file_ops.clone() };
    renamite_platform::dialogs::pick_open_file(
        "Import Font",
        &["ttf", "otf"],
        Box::new(move |picked| {
            let op = match picked {
                None => None,
                Some(PickedFile::Path(path)) => {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "Font".into());
                    match std::fs::read(&path) {
                        Ok(bytes) => Some(PendingFileOp::ImportFontDone { name, bytes }),
                        Err(e) => Some(PendingFileOp::Failed {
                            message: format!("Font import failed: {e}"),
                        }),
                    }
                }
                Some(PickedFile::Bytes { name, data }) => {
                    Some(PendingFileOp::ImportFontDone { name, bytes: data })
                }
            };
            if let Some(op) = op {
                ops.lock().unwrap().push_back(op);
            }
            wake_ui();
        }),
    );
}

/// Decode the pixel dimensions of image bytes and wrap them in an `ImageAsset`.
fn image_asset_from_bytes(
    name: String,
    bytes: Vec<u8>,
) -> anyhow::Result<renamite_model::ImageAsset> {
    use image::GenericImageView;

    let decoded = image::load_from_memory(&bytes)?;
    let (width, height) = decoded.dimensions();

    let extension = std::path::Path::new(&name)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let mime = match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    };

    Ok(renamite_model::ImageAsset {
        name,
        mime: mime.into(),
        bytes,
        width,
        height,
        srgb: true,
    })
}

/// Read PNG/JPEG/WebP bytes into the project as an image asset (undoable).
/// Decoding happens on the picker thread; the asset is applied on the UI
/// thread via `PendingFileOp::ImportImageDone`.
pub fn import_image(session: &SessionRef) {
    let operations = session.borrow().file_ops.clone();

    renamite_platform::dialogs::pick_open_file(
        "Import Image",
        &["png", "jpg", "jpeg", "webp"],
        Box::new(move |picked| {
            let result = match picked {
                None => None,

                Some(PickedFile::Path(path)) => {
                    let name = path
                        .file_name()
                        .map(|name| {
                            name.to_string_lossy().into_owned()
                        })
                        .unwrap_or_else(|| "Image".into());

                    Some(
                        std::fs::read(path)
                            .map_err(anyhow::Error::from)
                            .and_then(|bytes| {
                                image_asset_from_bytes(name, bytes)
                            }),
                    )
                }

                Some(PickedFile::Bytes { name, data }) => {
                    Some(image_asset_from_bytes(name, data))
                }
            };

            if let Some(result) = result {
                let operation = match result {
                    Ok(asset) => {
                        PendingFileOp::ImportImageDone { asset }
                    }

                    Err(error) => PendingFileOp::Failed {
                        message: format!(
                            "Image import failed: {error}"
                        ),
                    },
                };

                operations
                    .lock()
                    .unwrap()
                    .push_back(operation);
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
fn prepare_export(
    session: &SessionRef,
) -> anyhow::Result<(repose_core::Scene, (u32, u32), renamite_model::Document)> {
    let (scene, artboard, document) = {
        let s = session.borrow();
        let size = s.file.document.compositions[s.file.document.main].size;
        let scene = s.engine.scene().clone();
        (scene, size, s.file.document.clone())
    };
    let (w, h) = clamp_export_size(artboard);
    let view = fit_view(artboard, w, h);
    let mut bridge = renamite_render_bridge::SceneRenderer::new();
    let prepared = bridge.prepare(&scene, &view);
    let mut repose = repose_core::Scene::default();
    bridge.append_repose_scene(&prepared, &mut repose);
    Ok((repose, (w, h), document))
}

/// Rasterize `repose` and encode PNG bytes. Image assets are uploaded from
/// `document` before drawing so image layers resolve. Runs on any thread.
fn render_png_bytes(
    repose: repose_core::Scene,
    w: u32,
    h: u32,
    document: renamite_model::Document,
) -> anyhow::Result<Vec<u8>> {
    let mut gpu = renamite_render_offscreen::OffscreenRenderer::new_blocking(w, h, 4)?;
    gpu.sync_document_images(&document)?;
    gpu.render_png(&repose, Some([1.0, 1.0, 1.0, 1.0]))
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

/// Render the current frame off-thread and write it to a user-chosen PNG path.
/// The render worker runs detached so the editor stays responsive; completion
/// is applied through [`PendingFileOp::ExportFinished`] on the UI thread.
#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
pub fn export_png(session: &SessionRef) {
    if session.borrow().exporting_png {
        set_status(session, "PNG export already in progress");
        return;
    }
    let Some(path) = renamite_platform::dialogs::export_path("Export frame", "frame.png", &["png"])
    else {
        return;
    };
    let (scene, (w, h), document) = match prepare_export(session) {
        Ok(p) => p,
        Err(e) => {
            report_error(session, e);
            return;
        }
    };
    session.borrow_mut().exporting_png = true;
    set_status(session, "Rendering PNG…");
    let ops = session.borrow().file_ops.clone();
    web_workers::spawn(move || {
        let op = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            render_png_bytes(scene, w, h, document)
        })) {
            Ok(Ok(png)) => match std::fs::write(&path, png) {
                Ok(()) => PendingFileOp::ExportFinished {
                    message: format!("Exported {}", path.display()),
                },
                Err(e) => PendingFileOp::Failed {
                    message: format!("PNG export failed: {e}"),
                },
            },
            Ok(Err(e)) => PendingFileOp::Failed {
                message: format!("PNG export failed: {e}"),
            },
            Err(panic) => PendingFileOp::Failed {
                message: format!("PNG export failed: {}", panic_payload(&panic)),
            },
        };
        ops.lock().unwrap().push_back(op);
        wake_ui();
    });
}

/// Render the current frame off-thread, then hand the bytes to the OS save
/// picker (WASM/Android). The picker itself is non-blocking; the render no
/// longer blocks the UI thread.
#[cfg(any(target_os = "android", target_arch = "wasm32"))]
pub fn export_png(session: &SessionRef) {
    if session.borrow().exporting_png {
        set_status(session, "PNG export already in progress");
        return;
    }
    let suggested = format!("{}.png", document_stem(session));
    let (scene, (w, h), document) = match prepare_export(session) {
        Ok(p) => p,
        Err(e) => {
            report_error(session, e);
            return;
        }
    };
    session.borrow_mut().exporting_png = true;
    set_status(session, "Rendering PNG…");
    let ops = session.borrow().file_ops.clone();
    web_workers::spawn(move || {
        let op = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            render_png_bytes(scene, w, h, document)
        })) {
            Ok(Ok(bytes)) => PendingFileOp::ExportPngReady {
                bytes,
                suggested_name: suggested,
            },
            Ok(Err(e)) => PendingFileOp::Failed {
                message: format!("PNG export failed: {e}"),
            },
            Err(panic) => PendingFileOp::Failed {
                message: format!("PNG export failed: {}", panic_payload(&panic)),
            },
        };
        ops.lock().unwrap().push_back(op);
        wake_ui();
    });
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
