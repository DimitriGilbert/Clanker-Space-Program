fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    {
        // §2.14: the tee logger feeds both env_logger's output and the
        // bug-report bundle's last-500-lines tail.
        csp_app::logring::init();
        if std::env::args().skip(1).any(|a| a == "--bug-report") {
            std::process::exit(match csp_app::bug_report_cli() {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("--bug-report: {e}");
                    1
                }
            });
        }
        csp_app::run();
    }
}
