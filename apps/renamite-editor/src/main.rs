fn main() -> anyhow::Result<()> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        if std::env::var("RUST_LOG").is_err() {
            unsafe { std::env::set_var("RUST_LOG", "info") };
        }
        env_logger::init();
    }
    renamite_ui::run()
}