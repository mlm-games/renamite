#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

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
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );

    let _ = repose_platform::android::run_android_app(android_app, renamite_ui::app);
}
