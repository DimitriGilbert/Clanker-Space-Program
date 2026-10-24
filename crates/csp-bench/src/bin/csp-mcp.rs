//! `csp-mcp` (PLAN §2.12.1) — the stdio MCP host: newline-delimited
//! JSON-RPC over a headless sim. An eval harness spawns this bin; it
//! never attaches to a running game (that path is HTTP-only).
//!
//! ```text
//! csp-mcp [--empty | --fixture <dir>] [--save-dir <dir>] [--client <name>]
//!         [--dashboard-dir <dir>]
//! ```
//!
//! - `--empty` (default): a bare `World::new()`.
//! - `--fixture <dir>`: build the flight recipe's world from a frozen
//!   fixture dir (the `csp-replay` harness's `flight-launch`
//!   construction — data set, physics env, script engine, autopilot).
//! - `--save-dir`: where `session.*` tools read/write `.cspsave`s
//!   (default: `./csp-saves`). On `session.load`, the fixture's
//!   construction config is re-installed (§7.3).
//! - `--dashboard-dir`: where the M9d `dashboard.*` tools read/write
//!   the §2.12.2 sidecar `*.json`s (default: `./csp-dashboards`).
//! - `--client`: this process's client name — the `source_id` its
//!   commands stamp under (default `"local"` → index 0).

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use csp_server::backend::{ClientRegistry, LocalGame};
use csp_server::mcp::Mcp;
use csp_sim::World;

const USAGE: &str = "usage: csp-mcp [--empty | --fixture <dir>] [--save-dir <dir>] \
                     [--client <name>] [--dashboard-dir <dir>]";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("csp-mcp: {e}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), String> {
    let mut fixture: Option<PathBuf> = None;
    let mut save_dir = PathBuf::from("csp-saves");
    let mut dashboard_dir = PathBuf::from("csp-dashboards");
    let mut client_name = "local".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--empty" => fixture = None,
            "--fixture" => {
                fixture = Some(PathBuf::from(args.next().ok_or("--fixture needs a dir")?));
            }
            "--save-dir" => {
                save_dir = PathBuf::from(args.next().ok_or("--save-dir needs a dir")?);
            }
            "--dashboard-dir" => {
                dashboard_dir = PathBuf::from(args.next().ok_or("--dashboard-dir needs a dir")?);
            }
            "--client" => {
                client_name = args.next().ok_or("--client needs a name")?;
            }
            other => return Err(format!("{USAGE}\nunknown arg {other}")),
        }
    }

    let world = match &fixture {
        Some(dir) => build_fixture_world(dir)?,
        None => World::new(),
    };
    let mut game = LocalGame::new(world, save_dir);
    // On `session.load` the fixture's script engine and autopilot
    // driver must be re-installed — they are §7.3's data-derived
    // channel, never in the snapshot (`World::restore` leaves them
    // `None`). `script_op_budget` IS serialized, but the reload
    // re-imposes the fixture's own balance.ron over whatever budget a
    // foreign save happened to carry.
    if let Some(dir) = fixture {
        game = game.with_reconfigure(move |w: &mut World| {
            let balance = dir.join("balance.ron");
            match csp_data::load_balance(&balance) {
                Ok(bal) => w.set_script_op_budget(bal.script_op_budget),
                Err(e) => eprintln!(
                    "csp-mcp: reloading {} failed ({e}) — keeping the loaded \
                     save's script op budget",
                    balance.display()
                ),
            }
            w.set_script_engine(Box::new(csp_script::RhaiHost::new()));
            w.set_autopilot_driver(Box::new(csp_ai::AscentAutopilot::new()));
        });
    }

    let registry = ClientRegistry::new();
    let client = registry.id(&client_name);
    // Arc<LocalGame> is !Send by design — `World` cannot cross
    // threads and this process is single-threaded anyway.
    #[allow(clippy::arc_with_non_send_sync)]
    let game = Arc::new(game);
    // M9d §2.12.2: the `dashboard.*` tools' sidecar store.
    let dashboards = Arc::new(csp_server::dashboard::DashboardStore::new(dashboard_dir));
    let mcp = Mcp::with_dashboards(game, Some(dashboards));

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => return Err(format!("stdin: {e}")),
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                // JSON-RPC parse error — report and keep serving.
                let err = serde_json::json!({
                    "jsonrpc": "2.0", "id": null,
                    "error": {"code": -32700, "message": format!("parse error: {e}")},
                });
                writeln!(out, "{err}").map_err(|e| e.to_string())?;
                continue;
            }
        };
        if let Some(resp) = mcp.handle(client, &msg)
            && writeln!(out, "{resp}").is_err()
        {
            return Err("stdout closed".to_string());
        }
        if out.flush().is_err() {
            return Err("stdout flush failed".to_string());
        }
    }
    Ok(())
}

/// Build the flight recipe's world from a fixture dir — the same
/// construction `csp-replay record flight-launch` uses, so a
/// fixture-driven `csp-mcp` sees exactly the golden's starting state.
fn build_fixture_world(dir: &Path) -> Result<World, String> {
    let recipe =
        csp_bench::scenarios::find("flight-launch").ok_or("flight-launch recipe not registered")?;
    (recipe.initial)(dir).map_err(|e| format!("fixture {dir:?}: {e}"))
}
