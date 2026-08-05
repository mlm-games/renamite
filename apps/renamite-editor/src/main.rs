fn main() -> anyhow::Result<()> {
    #[cfg(all(not(target_os = "android"), not(target_arch = "wasm32")))]
    {
        if std::env::var("RUST_LOG").is_err() {
            unsafe {
                std::env::set_var("RUST_LOG", "info");
            }
        }

        env_logger::init();
        renamite_editor::desktop_main()
    }

    #[cfg(any(target_os = "android", target_arch = "wasm32"))]
    {
        Ok(())
    }
}