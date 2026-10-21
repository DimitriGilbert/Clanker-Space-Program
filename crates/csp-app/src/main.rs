fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    {
        env_logger::init();
        csp_app::run();
    }
}
