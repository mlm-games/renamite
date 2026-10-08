#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

/// Mirrors `package.metadata.android.package`. Only consulted if
/// `internal_data_path()` is unavailable, so the two can drift unnoticed; a
/// build script reading the manifest is the fix if that ever matters.
#[cfg(target_os = "android")]
const ANDROID_PACKAGE: &str = "org.mlm.renamite";

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "android")))]
pub fn desktop_main() -> anyhow::Result<()> {
    repose_platform::run_desktop_app_with_config(
        renamite_ui::app,
        repose_platform::AppConfig::default(),
    )
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(start)]
pub fn wasm_start() -> Result<(), JsValue> {
    renamite_ui::init_wasm();
    let mut options = repose_platform::web::WebOptions::new(None);
    options.set_prevent_default(true);
    repose_platform::web::run_web_app(renamite_ui::app, options)
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "C" fn android_main(android_app: winit::platform::android::activity::AndroidApp) {
    rlobkit_app_events::android_log::init(
        env!("CARGO_PKG_NAME"),
        concat!(
            env!("CARGO_PKG_NAME"),
            "=info,renamite_ui=info,renamite_platform=info"
        ),
    );

    renamite_platform::dialogs::init();
    // Autosave lands in app-private storage, not the evictable temp dir:
    // `$HOME` is unset on Android so `ProjectDirs` cannot resolve it.
    renamite_platform::set_android_data_dir(
        android_app
            .internal_data_path()
            .unwrap_or_else(std::env::temp_dir),
        ANDROID_PACKAGE,
    );

    rlobkit_app_events::system_bars::set_system_bars_visible(
        rlobkit_app_events::system_bars::SystemBars {
            status: false,
            navigation: true,
        },
    );

    rlobkit_app_events::insets::set_on_insets(Box::new(|insets| {
        let r = repose_core::locals::WindowInsets {
            top: insets.top,
            bottom: insets.bottom,
            left: insets.left,
            right: insets.right,
            ime_bottom: insets.ime_bottom,
        };
        repose_core::locals::set_window_insets_default(r);
    }));

    let _ = repose_platform::android::run_android_app(android_app, renamite_ui::app);
}
