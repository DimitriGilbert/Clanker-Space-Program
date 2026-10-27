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
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.iter().any(|a| a == "--serve") {
            // §2.12.3's flag surface is *parsed* — an unrecognized
            // argument is a loud refusal, never a silently ignored
            // `--port 5000` (C27-F1).
            std::process::exit(
                match csp_app::parse_serve_flags(&args).and_then(|f| csp_app::serve_cli(&f)) {
                    Ok(()) => 0,
                    Err(e) => {
                        eprintln!("--serve: {e}");
                        1
                    }
                },
            );
        }
        csp_app::run();
    }
}
