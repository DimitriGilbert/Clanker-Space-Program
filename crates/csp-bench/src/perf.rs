//! M10-D / PLAN §5.3 — the perf log: every §5.3 budget row as a named
//! check, measured where a headless CI-HOST run is honest, marked
//! hardware-gated where it is not.
//!
//! §5.3's table has two kinds of rows:
//!
//! - **CPU-only rows** — sim ms/tick, ms/frame, events/frame, weld
//!   Stage-1/Stage-2 ms, telemetry bytes/s. These are measured here,
//!   for real, on the reference scenarios the rows name (the 200-part
//!   tower in atmosphere, the 1000-part station in orbit, the 300-part
//!   landed stack, 500 on-rails vessels, 20 colonies under rails warp,
//!   the weld of a 1000-part graph, voxelization of a 300-part craft,
//!   and the telemetry frame of a 1000-part vessel). The sim-CPU rows
//!   measure `World::step_frame` — the whole sim tick, not just the
//!   physics backend — because the budget is written against sim CPU.
//! - **fps rows** — 144/60/30 fps at 1080p. These need the REF-A /
//!   REF-B GPUs (§8.2: "hosted runners have no GPU"). They are
//!   recorded as [`PerfStatus::HardwareGated`] — the milestone asks
//!   for the honest split, not a fake number.
//!
//! The artifact is [`PerfLog`]: `schema_version` + machine identity +
//! one [`PerfEntry`] per row (budget, measured value, pass/fail).
//! `csp-perf` runs [`run_perf_log`] and writes the RON to `docs/perf/`
//! — §8.2's "reference-machine gates with a committed perf log".
//! The budget table itself is [`BUDGET_ROWS`]; adding a row without a
//! measurement fails the coverage test.

use std::path::{Path, PathBuf};
use std::time::Instant;

use csp_core::{DQuat, DVec3};
use csp_data::{BodyId, KeplerElements};
use csp_phys::{
    ColliderShape, ForceSite, ForceSiteKind, PartGraph, PartNode, PartNodeId, PartTransform,
    SymMat3, WeldPart,
};
use csp_sim::{SimCommand, SourceClass, VesselState, World};
use serde::{Deserialize, Serialize};

use crate::replay::ReplayError;

/// The perf log's own schema version (§7.8's sidecar class).
pub const PERF_LOG_SCHEMA_VERSION: u32 = 1;

/// `ms/tick` — per-physics-tick sim CPU.
const UNIT_MS_TICK: &str = "ms/tick";
/// `ms/frame` — per rendered frame / per `step_frame` call.
const UNIT_MS_FRAME: &str = "ms/frame";
/// `ms` — a one-shot synchronous cost.
const UNIT_MS: &str = "ms";
/// `events/frame` — the §2.6.5 back-pressure bound.
const UNIT_EVENTS: &str = "events/frame";
/// `B/s` — telemetry wire rate.
const UNIT_BYTES_S: &str = "B/s";
/// `fps` — the hardware-gated rows.
const UNIT_FPS: &str = "fps";
/// `px` — artefact pixel counts (the §5.4 depth row).
const UNIT_PX: &str = "px";

/// One §5.3 table row, flattened to the checkable fields. `measurable`
/// splits the headless-measurable CPU rows from the REF-hardware fps
/// rows — a measurable row without a [`measure_row`] arm is a harness
/// gap the coverage test catches.
pub struct BudgetRow {
    /// Stable check id — the perf log's key (`"sim.200-part-atmo"`).
    pub id: &'static str,
    /// The §5.3 row text.
    pub scene: &'static str,
    /// The budget's numeric bound (`measured <= budget` passes).
    pub budget: f64,
    /// The unit the number is written in.
    pub unit: &'static str,
    /// `true` = measured headless here; `false` = REF-A/REF-B only.
    pub measurable: bool,
    /// Optional note carried into the log (measurement caveat, or why
    /// the row is hardware-gated).
    pub note: &'static str,
}

/// Every §5.3 budget row — the perf log's complete coverage. CPU rows
/// come first, then the hardware-gated fps rows.
pub const BUDGET_ROWS: &[BudgetRow] = &[
    BudgetRow {
        id: "sim.200-part-atmo",
        scene: "200-part vessel in atmosphere (sim CPU)",
        budget: 0.5,
        unit: UNIT_MS_TICK,
        measurable: true,
        note: "step_frame mean over the flight-launch fixture's Terrin, \
               200-part tower at 20 km on an orbital tangent. C29-F1 \
               triage: red since the post-baseline growth phase (57 \
               commits / +58k LOC past the 2026-10-23 log) — the \
               deviation is recorded here, not re-budgeted away",
    },
    BudgetRow {
        id: "sim.1000-part-orbit",
        scene: "1000-part station in orbit (sim CPU)",
        budget: 2.0,
        unit: UNIT_MS_TICK,
        measurable: true,
        note: "step_frame mean, 1000-part tower on a 300 km circular \
               orbit. C29-F1 triage: red since the post-baseline growth \
               phase (57 commits / +58k LOC past the 2026-10-23 log) — \
               the deviation is recorded here, not re-budgeted away",
    },
    BudgetRow {
        id: "sim.300-part-landed",
        scene: "300-part vessel landed, gear in contact (sim CPU)",
        budget: 1.0,
        unit: UNIT_MS_TICK,
        measurable: true,
        note: "step_frame mean, 300-part tower resting on the pad in contact",
    },
    BudgetRow {
        id: "sim.map-500-rails",
        scene: "map view, 500 on-rails objects (sim CPU)",
        budget: 0.2,
        unit: UNIT_MS_FRAME,
        measurable: true,
        note: "step_frame mean at 1-tick targets, 500 SpawnVessel on rails",
    },
    BudgetRow {
        id: "warp.20-colonies.events",
        scene: "rails warp ×10⁵, 20 colonies (event bound)",
        budget: 2000.0,
        unit: UNIT_EVENTS,
        measurable: true,
        note: "max fired events per step_frame call over long advances — \
               the §2.6.5 back-pressure bound",
    },
    BudgetRow {
        id: "warp.20-colonies.ms",
        scene: "rails warp ×10⁵, 20 colonies (frame cost)",
        budget: 4.0,
        unit: UNIT_MS_FRAME,
        measurable: true,
        note: "mean ms per step_frame call at MAX_TICKS_PER_FRAME strides",
    },
    BudgetRow {
        id: "weld.stage1-1000-part",
        scene: "Weld Stage 1, 1000-part station (synchronous)",
        budget: 20.0,
        unit: UNIT_MS,
        measurable: true,
        note: "csp_phys::weld mean over repeat calls on the same graph",
    },
    BudgetRow {
        id: "weld.stage2-300-part",
        scene: "Weld Stage 2 (voxelization), 300-part craft (native)",
        budget: 500.0,
        unit: UNIT_MS,
        measurable: true,
        note: "build_aero_tables over the 66-direction table; the wasm ≤2 s \
               row is incremental-across-frames and hardware-gated separately",
    },
    BudgetRow {
        id: "telemetry.1000-part",
        scene: "telemetry stream, 1000-part vessel @16 Hz",
        budget: 256.0 * 1024.0,
        unit: UNIT_BYTES_S,
        measurable: true,
        note: "dashboard field-set projection serialized to JSON; ≤16 KiB \
               per frame at 16 Hz",
    },
    BudgetRow {
        id: "vab.interaction-300-part",
        scene: "VAB interaction latency, 300-part craft (op + frame)",
        budget: 16.0,
        unit: UNIT_MS,
        measurable: true,
        note: "worst of 5 trials: one session op + one full end_frame \
               (dirty recompute + egui pass + packet build) — the §5.4 \
               kill criterion is CPU work and headless-measurable; the \
               hard release assert lives in csp-view/tests/vab_latency.rs \
               (C17-F5)",
    },
    BudgetRow {
        id: "render.depth-fight-1e7",
        scene: "§5.4 R1 depth strategy — 1 m object crossing a surface at 10⁷ m",
        budget: 1.0,
        unit: UNIT_PX,
        measurable: true,
        note: "order-dependent pixels at 1080p across two draw orders of \
               the §5.4 scene on the shipped flight pipeline (reverse-Z \
               f32 clip, Depth32Float, GreaterEqual, 4×MSAA); measured by \
               csp_view::depth_probe on a headless adapter (lavapipe on \
               CI — C16-F5). Over the bound, §2.3's far-field pass is owed",
    },
    // --- hardware-gated fps rows (§8.2: reference-machine gates) ---
    BudgetRow {
        id: "fps.200-part-atmo.native",
        scene: "200-part vessel in atmosphere, 1080p (native)",
        budget: 144.0,
        unit: UNIT_FPS,
        measurable: false,
        note: "requires a GPU — REF-A / REF-B reference-machine gate (§8.2)",
    },
    BudgetRow {
        id: "fps.200-part-atmo.web",
        scene: "200-part vessel in atmosphere, 1080p (web)",
        budget: 60.0,
        unit: UNIT_FPS,
        measurable: false,
        note: "requires Chrome stable on REF-A — reference-machine gate (§8.2)",
    },
    BudgetRow {
        id: "fps.1000-part-orbit.native",
        scene: "1000-part station in orbit, 1080p (native)",
        budget: 60.0,
        unit: UNIT_FPS,
        measurable: false,
        note: "requires a GPU — REF-A / REF-B reference-machine gate (§8.2)",
    },
    BudgetRow {
        id: "fps.1000-part-orbit.web",
        scene: "1000-part station in orbit, 1080p (web)",
        budget: 30.0,
        unit: UNIT_FPS,
        measurable: false,
        note: "requires Chrome stable on REF-A — reference-machine gate (§8.2)",
    },
    BudgetRow {
        id: "fps.300-part-landed.native",
        scene: "300-part vessel landed, 1080p (native)",
        budget: 144.0,
        unit: UNIT_FPS,
        measurable: false,
        note: "requires a GPU — REF-A / REF-B reference-machine gate (§8.2)",
    },
    BudgetRow {
        id: "fps.300-part-landed.web",
        scene: "300-part vessel landed, 1080p (web)",
        budget: 60.0,
        unit: UNIT_FPS,
        measurable: false,
        note: "requires Chrome stable on REF-A — reference-machine gate (§8.2)",
    },
    BudgetRow {
        id: "fps.map-500-rails.native",
        scene: "map view, 500 on-rails objects, 1080p (native)",
        budget: 144.0,
        unit: UNIT_FPS,
        measurable: false,
        note: "requires a GPU — REF-A / REF-B reference-machine gate (§8.2)",
    },
    BudgetRow {
        id: "fps.map-500-rails.web",
        scene: "map view, 500 on-rails objects, 1080p (web)",
        budget: 60.0,
        unit: UNIT_FPS,
        measurable: false,
        note: "requires Chrome stable on REF-A — reference-machine gate (§8.2)",
    },
    BudgetRow {
        id: "weld.stage2-300-part.wasm",
        scene: "Weld Stage 2 (voxelization), 300-part craft (web, incremental)",
        budget: 2000.0,
        unit: UNIT_MS,
        measurable: false,
        note: "wasm-only row — incremental across frames, no worker exists \
               on wasm (§5.3); measured on the web tier",
    },
];

/// The outcome of one budget check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PerfStatus {
    /// Measured within budget.
    Pass,
    /// Measured over budget, or the measurement harness failed.
    Fail,
    /// Not headless-measurable — a REF-A/REF-B reference-machine gate.
    HardwareGated,
}

/// One perf-log line — the budget, what was measured, and the verdict.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PerfEntry {
    /// [`BudgetRow::id`].
    pub id: String,
    /// [`BudgetRow::scene`].
    pub scene: String,
    /// The budget bound (`measured_value <= budget_value` passes).
    pub budget_value: f64,
    /// The unit both numbers are written in.
    pub unit: String,
    /// The measured value — `None` for hardware-gated rows and
    /// measurement failures.
    pub measured_value: Option<f64>,
    /// Measurement detail (tick counts, the seeded scene shape, the
    /// harness error when `measured_value` is `None` on a measurable row).
    pub detail: Option<String>,
    /// The verdict.
    pub status: PerfStatus,
}

/// The machine a log was measured on — §5.3's "a budget without a
/// reference machine is unmeasurable" made fields.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Machine {
    /// `CSP_PERF_MACHINE` when set (REF-A/REF-B name themselves), else
    /// `ci-host` under CI, else the hostname.
    pub name: String,
    /// `std::env::consts::OS`.
    pub os: String,
    /// `std::env::consts::ARCH`.
    pub arch: String,
    /// `debug` or `release` — §5.3 budgets are release numbers; a
    /// debug-profile log records honest Pass/Fail under a diagnostic
    /// profile, not a claim.
    pub profile: String,
    /// `CSP_RUSTC_VERSION` from the build-script fingerprint env.
    pub rustc: String,
}

/// The committed perf log artifact (§8.2's "committed perf log").
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PerfLog {
    /// [`PERF_LOG_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The measuring machine.
    pub machine: Machine,
    /// One entry per [`BUDGET_ROWS`] row, in table order.
    pub entries: Vec<PerfEntry>,
}

impl PerfLog {
    /// `true` when no measurable row failed — the `csp-perf` exit
    /// verdict (hardware-gated rows do not block).
    pub fn all_passed(&self) -> bool {
        self.entries.iter().all(|e| e.status != PerfStatus::Fail)
    }
}

/// Detect the machine identity for the log header. `CSP_PERF_MACHINE`
/// overrides everything (a REF machine names itself); `CI` marks the
/// CI-HOST row §5.3 reserves for CPU-only checks.
pub fn detect_machine() -> Machine {
    let name = std::env::var("CSP_PERF_MACHINE")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var("CI")
                .ok()
                .filter(|v| !v.is_empty() && v != "false")
                .map(|_| "ci-host".to_string())
        })
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "unknown".to_string());
    Machine {
        name,
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        profile: if cfg!(debug_assertions) {
            "debug".to_string()
        } else {
            "release".to_string()
        },
        rustc: option_env!("CSP_RUSTC_VERSION")
            .unwrap_or("unknown")
            .to_string(),
    }
}

/// The whole §5.3 suite: measured rows + the hardware-gated rows
/// recorded honestly. `machine` overrides detection (the bin's
/// `--machine` flag; `None` = [`detect_machine`]).
pub fn run_perf_log(machine: Option<Machine>) -> PerfLog {
    let machine = machine.unwrap_or_else(detect_machine);
    let entries = BUDGET_ROWS
        .iter()
        .map(|row| {
            if !row.measurable {
                return PerfEntry {
                    id: row.id.to_string(),
                    scene: row.scene.to_string(),
                    budget_value: row.budget,
                    unit: row.unit.to_string(),
                    measured_value: None,
                    detail: Some(row.note.to_string()),
                    status: PerfStatus::HardwareGated,
                };
            }
            match measure_row(row.id) {
                Ok(Measurement { value, detail }) => PerfEntry {
                    id: row.id.to_string(),
                    scene: row.scene.to_string(),
                    budget_value: row.budget,
                    unit: row.unit.to_string(),
                    measured_value: Some(value),
                    detail: Some(format!("{detail}; {}", row.note)),
                    status: if value <= row.budget {
                        PerfStatus::Pass
                    } else {
                        PerfStatus::Fail
                    },
                },
                Err(e) => PerfEntry {
                    id: row.id.to_string(),
                    scene: row.scene.to_string(),
                    budget_value: row.budget,
                    unit: row.unit.to_string(),
                    measured_value: None,
                    detail: Some(format!("harness error: {e}")),
                    status: PerfStatus::Fail,
                },
            }
        })
        .collect();
    PerfLog {
        schema_version: PERF_LOG_SCHEMA_VERSION,
        machine,
        entries,
    }
}

/// Serialize the log as RON and write it (creating parent dirs) — the
/// `docs/perf/perf-log-<machine>-<profile>.ron` artifact §8.2 commits.
pub fn write_perf_log(log: &PerfLog, path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = ron::ser::to_string_pretty(log, ron::ser::PrettyConfig::default())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, text)
}

/// Load a committed perf log — the regression baseline the release CI
/// job diffs the fresh measurement against (`csp-perf --baseline`).
pub fn load_perf_log(path: &Path) -> std::io::Result<PerfLog> {
    let text = std::fs::read_to_string(path)?;
    ron::de::from_str(&text).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// The regression tolerance a committed perf log enforces — a row
/// more than this fraction *slower* than the last committed
/// measurement is a regression. The budget is the §5.3 number; the
/// tolerance is what machine noise is allowed under (a real slowdown
/// lives over it).
pub const REGRESSION_TOLERANCE: f64 = 0.10;

/// One row's regression verdict from [`regression_diff`] — the row,
/// the two numbers, and the rule that fired.
#[derive(Clone, Debug)]
pub struct PerfRegression {
    /// [`BudgetRow::id`].
    pub id: String,
    /// The committed baseline's measured value (`None` when the
    /// baseline row carried none).
    pub baseline: Option<f64>,
    /// The fresh measurement (`None` when the measurable row failed
    /// to measure at all).
    pub measured: Option<f64>,
    /// The budget bound the row is written against.
    pub budget: f64,
    /// The rule that fired, for the report line.
    pub reason: String,
}

/// Diff `current` against the last committed `baseline` — the release
/// CI regression gate. Rows match by `id`; a row **regresses** when:
///
/// - its measurement exceeds the baseline's by more than
///   [`REGRESSION_TOLERANCE`] — a real slowdown above machine noise;
/// - it **newly crosses its budget** (`baseline ≤ budget` but
///   `measured > budget`) — a §5.3 Pass→Fail flip, however small;
/// - a measurable row produced **no measurement** — a harness failure
///   can never be a green row.
///
/// Rows absent from `baseline` are new — recorded, not regressions.
/// Hardware-gated rows carry no measurement and never diff.
pub fn regression_diff(current: &PerfLog, baseline: &PerfLog) -> Vec<PerfRegression> {
    let base: std::collections::BTreeMap<&str, &PerfEntry> = baseline
        .entries
        .iter()
        .map(|e| (e.id.as_str(), e))
        .collect();
    let mut out = Vec::new();
    for e in &current.entries {
        if e.status == PerfStatus::HardwareGated {
            continue;
        }
        let b = base.get(e.id.as_str()).copied();
        match (b.and_then(|b| b.measured_value), e.measured_value) {
            (_, None) => out.push(PerfRegression {
                id: e.id.clone(),
                baseline: b.and_then(|b| b.measured_value),
                measured: None,
                budget: e.budget_value,
                reason: "measurable row produced no measurement".to_string(),
            }),
            (Some(bv), Some(mv)) if mv > bv * (1.0 + REGRESSION_TOLERANCE) => {
                out.push(PerfRegression {
                    id: e.id.clone(),
                    baseline: Some(bv),
                    measured: Some(mv),
                    budget: e.budget_value,
                    reason: format!(
                        "{mv:.3} > {:.0}% over committed {bv:.3}",
                        REGRESSION_TOLERANCE * 100.0
                    ),
                });
            }
            (Some(bv), Some(mv)) if bv <= e.budget_value && mv > e.budget_value => {
                out.push(PerfRegression {
                    id: e.id.clone(),
                    baseline: Some(bv),
                    measured: Some(mv),
                    budget: e.budget_value,
                    reason: format!("newly over budget ({bv:.3} ≤ {} < {mv:.3})", e.budget_value),
                });
            }
            _ => {}
        }
    }
    out
}

/// The default artifact path — `docs/perf/perf-log-<name>-<profile>.ron`
/// under the workspace root (the crate manifest's `../..`).
pub fn default_log_path(machine: &Machine) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/perf")
        .join(format!("perf-log-{}-{}.ron", machine.name, machine.profile))
}

/// One measurement's result: the value in the row's unit plus the
/// human-readable detail the log records (tick counts, scene shape).
struct Measurement {
    value: f64,
    detail: String,
}

/// Dispatch a measurable row to its benchmark. Every `measurable`
/// [`BudgetRow`] must have an arm — the coverage test asserts the
/// whole set resolves.
fn measure_row(id: &str) -> Result<Measurement, ReplayError> {
    match id {
        "sim.200-part-atmo" => measure_atmo_200(),
        "sim.1000-part-orbit" => measure_orbit_1000(),
        "sim.300-part-landed" => measure_landed_300(),
        "sim.map-500-rails" => measure_rails_500(),
        "warp.20-colonies.events" => measure_colony_warp_events(),
        "warp.20-colonies.ms" => measure_colony_warp_ms(),
        "weld.stage1-1000-part" => measure_weld1_1000(),
        "weld.stage2-300-part" => measure_voxelize_300(),
        "telemetry.1000-part" => measure_telemetry_1000(),
        "vab.interaction-300-part" => measure_vab_300(),
        "render.depth-fight-1e7" => measure_depth_fight_1e7(),
        _ => Err(ReplayError::MalformedGolden(
            "perf: measurable row without a benchmark arm",
        )),
    }
}

/// The per-tick measurement's tick count — a debug build runs a
/// shorter window so `cargo test` stays fast; the release number is
/// the one the budget is written against.
fn measure_ticks() -> u64 {
    if cfg!(debug_assertions) { 64 } else { 256 }
}

/// The flight-launch fixture world — Terrin with its real atmosphere
/// and spin. §8.3's frozen fixture: the benchmark pins the same data
/// the golden hashes pin, so a balance change cannot silently move
/// the measured scene.
fn fixture_world() -> Result<World, ReplayError> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/test-fixtures/flight-launch");
    let recipe = crate::scenarios::find("flight-launch")
        .ok_or(ReplayError::MalformedGolden("no flight-launch recipe"))?;
    (recipe.initial)(&dir)
}

/// The shipping `data/` world — the full home system (solen, terrin,
/// rask, duva), its authored colony, site resource yields, and the
/// recipe table. The colony warp scene needs the yields/recipes the
/// frozen fixture lacks. Same construction-config wiring order the
/// scenario path uses.
fn shipping_world() -> Result<World, ReplayError> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let home = root.join("systems").join("home");
    let sys = csp_data::load_system(&home).map_err(ReplayError::Load)?;
    let balance = csp_data::load_balance(&root.join("balance.ron")).map_err(ReplayError::Load)?;
    let defs: Vec<csp_data::BodyDef> = sys.bodies.iter().map(|b| b.def.clone()).collect();
    let orbits = csp_sim::orbit::PatchedConics::from_body_defs(&defs)?;
    let terrin = defs
        .iter()
        .find(|b| b.id == BodyId::from("terrin"))
        .ok_or(ReplayError::MalformedGolden("shipping data lacks terrin"))?;
    let mut w = World::with_physics(
        orbits,
        csp_sim::PhysicsConfig {
            environment: csp_phys::Environment::for_body(
                terrin,
                DVec3::ZERO,
                terrin.rotation_period_s,
            ),
            dominant: BodyId::from("terrin"),
            bubble_radius_m: csp_sim::DEFAULT_BUBBLE_RADIUS_M,
        },
        balance.advance_to_max_events_per_frame,
    );
    w.set_vessel_caps(csp_sim::VesselCaps {
        max_vessels: balance.max_tracked_vessels,
        max_debris: balance.max_debris,
    });
    w.set_colony_env(csp_sim::colony_env_from_body_defs(&defs));
    let sites = csp_data::load_sites(&home.join("sites.ron")).map_err(ReplayError::Load)?;
    w.set_sites(
        sites
            .sites
            .iter()
            .map(|s| (s.id.clone(), s.clone()))
            .collect(),
    );
    w.set_recipes(
        csp_data::load_recipes(&root.join("recipes"))
            .map_err(ReplayError::Load)?
            .by_id()
            .into_iter()
            .map(|(id, def)| (id, def.clone()))
            .collect(),
    );
    w.found_authored_colonies(
        &csp_data::load_colonies(&home.join("colonies.ron"))
            .map_err(ReplayError::Load)?
            .colonies,
    )
    .map_err(|_| ReplayError::MalformedGolden("perf: aero-table build failed"))?;
    Ok(w)
}

/// The §5.3 tower — `n` 100 kg box parts stacked along the craft's
/// +z axis at 0.25 m pitch (the same construction the existing
/// step-cost measurement uses). `engine` bolts a 200 kN thrust site
/// onto the base when the scene should fly.
fn tower_craft(n: u32, engine: bool) -> csp_sim::CraftDef {
    let mut graph = PartGraph::new(PartNodeId(0));
    let mut parts = Vec::with_capacity(n as usize);
    for i in 0..n {
        let id = PartNodeId(u64::from(i));
        graph.nodes.insert(
            id,
            PartNode::new(
                if i == 0 {
                    None
                } else {
                    Some(PartNodeId(u64::from(i - 1)))
                },
                PartTransform::new(
                    DVec3::new(0.0, 0.0, if i == 0 { 0.0 } else { 0.25 }),
                    DQuat::IDENTITY,
                ),
                i,
            ),
        );
        let mut part = WeldPart {
            experiments: Vec::new(),
            lab: None,
            engine: None,
            parachute: None,
            dock_ports: vec![],
            tanks: vec![],
            crew_seats: 0,
            structural: csp_data::StructuralDef::unlimited(),
            mass_kg: 100.0,
            com: DVec3::ZERO,
            inertia: SymMat3::solid_box(100.0, DVec3::new(0.5, 0.5, 0.125)),
            collider: ColliderShape::Box {
                half_extents: DVec3::new(0.5, 0.5, 0.125),
            },
            force_sites: Vec::new(),
            aero: csp_data::AeroDef::constant(1.0, 0.0, 1.0),
            thermal: csp_data::ThermalDef {
                heat_capacity_j_per_k: 400_000.0,
                max_temp_k: 1_100.0,
                emissivity: 0.8,
                conduction_w_per_k: 60.0,
            },
        };
        if engine && i == 0 {
            part.force_sites.push(ForceSite::new(
                DVec3::new(0.0, 0.0, -0.125),
                DVec3::Z,
                ForceSiteKind::Thrust,
                200_000.0,
            ));
        }
        parts.push(part);
    }
    csp_sim::CraftDef {
        hash_memo: Default::default(),
        part_ids: vec![],
        graph,
        parts,
        part_costs: vec![1_000.0; n as usize],
        reliability: vec![],
    }
}

/// The shared per-tick measurement: spawn `craft` at `pos`/`rot`/`vel`
/// on the fixture world, warm up a few ticks past the spawn drain,
/// then mean `step_frame` wall cost over `measure_ticks()` ticks while
/// the vessel stays in the bubble. Returns `(ms/tick, detail)`; a
/// scene that loses the vessel early reports its measured tick count
/// honestly (the caller's bound still applies).
fn measure_tick_scene(
    craft: csp_sim::CraftDef,
    pos: DVec3,
    rot: DQuat,
    vel: DVec3,
) -> Result<Measurement, ReplayError> {
    let mut w = fixture_world()?;
    w.enqueue(
        SourceClass::Ui,
        0,
        SimCommand::SpawnPhysicalVessel {
            craft,
            pos,
            rot,
            vel,
            ang_vel: DVec3::ZERO,
        },
    );
    // The spawn applies at the next boundary; the vessel id is 0.
    w.step_frame(w.clock().ticks() + 1)
        .map_err(ReplayError::Sim)?;
    let vessel = csp_sim::VesselId(0);
    if !w.vessels().contains_key(&vessel) {
        return Err(ReplayError::MalformedGolden("perf scene: spawn rejected"));
    }
    // Warmup — first-tick caches (aero ensure path, arena allocs) are
    // a startup cost, not the steady tick the budget measures.
    for _ in 0..8 {
        w.step_frame(w.clock().ticks() + 1)
            .map_err(ReplayError::Sim)?;
    }
    let ticks = measure_ticks();
    let mut measured = 0u64;
    let t0 = Instant::now();
    for _ in 0..ticks {
        if !matches!(
            w.vessels().get(&vessel).map(|v| &v.state),
            Some(VesselState::InBubble(_))
        ) {
            break;
        }
        w.step_frame(w.clock().ticks() + 1)
            .map_err(ReplayError::Sim)?;
        measured += 1;
    }
    let elapsed = t0.elapsed().as_secs_f64() * 1e3;
    if measured == 0 {
        return Err(ReplayError::MalformedGolden(
            "perf scene: vessel left the bubble before measurement",
        ));
    }
    Ok(Measurement {
        value: elapsed / measured as f64,
        detail: format!("{measured} ticks measured"),
    })
}

/// The fixture environment's orbital tangent at `r`: the +x pad
/// convention's prograde is +y.
fn orbital_vel(env: &csp_phys::Environment, r: f64) -> DVec3 {
    DVec3::Y * libm::sqrt(env.mu_m3_per_s2 / r)
}

/// §5.3 "200-part vessel in atmosphere": the tower at 20 km on an
/// orbital tangent — aerodynamic forces, thermal, structural all live.
fn measure_atmo_200() -> Result<Measurement, ReplayError> {
    let w = fixture_world()?;
    let env = w
        .environment()
        .cloned()
        .ok_or(ReplayError::MalformedGolden("no env"))?;
    let r = env.radius_m + 20_000.0;
    let mut m = measure_tick_scene(
        tower_craft(200, true),
        DVec3::new(r, 0.0, 0.0),
        DQuat::IDENTITY,
        orbital_vel(&env, r),
    )?;
    m.detail.push_str(", 200 parts at 20 km in atmosphere");
    Ok(m)
}

/// §5.3 "1000-part station in orbit": the tower at 300 km, vacuum.
/// The spawn rides a dipping conic (0.6-circular): a clearing conic
/// auto-rails at the first stepped tick now, and the measurement is
/// the *bubble* step cost — the tower must stay `InBubble` for it.
fn measure_orbit_1000() -> Result<Measurement, ReplayError> {
    let w = fixture_world()?;
    let env = w
        .environment()
        .cloned()
        .ok_or(ReplayError::MalformedGolden("no env"))?;
    let r = env.radius_m + 300_000.0;
    let mut m = measure_tick_scene(
        tower_craft(1000, false),
        DVec3::new(r, 0.0, 0.0),
        DQuat::IDENTITY,
        orbital_vel(&env, r) * 0.6,
    )?;
    m.detail.push_str(", 1000 parts at 300 km");
    Ok(m)
}

/// §5.3 "300-part vessel landed, gear in contact": the tower lying on
/// the pad (rot maps the stack axis horizontal) at the surface
/// co-rotation velocity — the contact solver runs every tick.
fn measure_landed_300() -> Result<Measurement, ReplayError> {
    let w = fixture_world()?;
    let env = w
        .environment()
        .cloned()
        .ok_or(ReplayError::MalformedGolden("no env"))?;
    // Lying on its side: half-width 0.5 m, a hair above the surface.
    let pos = DVec3::new(env.radius_m + 0.55, 0.0, 0.0);
    let rot = DQuat::from_axis_angle(DVec3::Y, core::f64::consts::FRAC_PI_2);
    let vel = env.body_spin.cross(pos - env.body_center);
    let mut m = measure_tick_scene(tower_craft(300, false), pos, rot, vel)?;
    m.detail.push_str(", 300 parts landed in contact");
    Ok(m)
}

/// §5.3 "map view, 500 on-rails objects": 500 `SpawnVessel` elements-
/// only vessels spread through Terrin's SOI, then the mean per-frame
/// `step_frame` cost at one-tick targets.
fn measure_rails_500() -> Result<Measurement, ReplayError> {
    let mut w = shipping_world()?;
    let mu = w
        .environment()
        .ok_or(ReplayError::MalformedGolden("no env"))?
        .mu_m3_per_s2;
    for i in 0..500u64 {
        // Semi-major axes spread 700 km → ~5.9e6 m, all inside
        // Terrin's 8.4e7 m SOI; inclinations/Ω phase the field.
        let a = 700_000.0 + f64::from((i % 130) as u32) * 40_000.0;
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SpawnVessel {
                parent: BodyId::from("terrin"),
                elements: KeplerElements {
                    semi_major_axis_m: a,
                    eccentricity: (i % 9) as f64 * 0.005,
                    inclination_rad: (i % 12) as f64 * 0.05,
                    ascending_node_rad: (i % 24) as f64 * 0.26,
                    arg_periapsis_rad: (i % 8) as f64 * 0.4,
                    mean_anomaly_epoch_rad: (i % 64) as f64 * 0.098,
                    // `period_s` is redundant (the model never reads
                    // it) but the spawn/restore gates check its sign —
                    // author the closed form like the system sheets.
                    period_s: 2.0 * core::f64::consts::PI * libm::sqrt(a * a * a / mu),
                },
            },
        );
    }
    w.step_frame(w.clock().ticks() + 1)
        .map_err(ReplayError::Sim)?;
    let spawned = w.vessels().len();
    if spawned < 500 {
        return Err(ReplayError::MalformedGolden(
            "perf scene: rails spawns rejected",
        ));
    }
    // Warmup — the spawn boundary's mass SOI scheduling is one-time.
    for _ in 0..4 {
        w.step_frame(w.clock().ticks() + 1)
            .map_err(ReplayError::Sim)?;
    }
    let frames = measure_ticks();
    let t0 = Instant::now();
    for _ in 0..frames {
        w.step_frame(w.clock().ticks() + 1)
            .map_err(ReplayError::Sim)?;
    }
    let elapsed = t0.elapsed().as_secs_f64() * 1e3;
    Ok(Measurement {
        value: elapsed / frames as f64,
        detail: format!("{spawned} on-rails vessels, {frames} frames"),
    })
}

/// The colony def the warp scene founds — the authored rask-mines
/// shape (solar + battery + ore drill + smelter + stores) retargeted
/// at a Terrin site, so terminator crossings, store bounds, and power
/// transitions all publish events during the warp.
fn perf_colony_def(site: &str) -> csp_data::ColonyDef {
    csp_data::ColonyDef {
        site: csp_data::SiteId::from(site),
        display_name: csp_data::LocKey::from("colony.perf"),
        modules: vec![
            csp_data::ColonyModuleDef::Solar {
                area_m2: 40.0,
                efficiency: 0.20,
            },
            csp_data::ColonyModuleDef::Battery { capacity: 5_000.0 },
            csp_data::ColonyModuleDef::Drill {
                resource: csp_data::ResourceId::from("ore"),
                base_rate_per_s: 0.5,
                min_concentration: 0.5,
                power_ec_per_s: 2.0,
            },
            csp_data::ColonyModuleDef::Converter {
                recipe: csp_data::RecipeId::from("recipe.smelt_ore"),
                priority: 5,
                power_ec_per_s: 1.5,
            },
            csp_data::ColonyModuleDef::Store {
                resource: csp_data::ResourceId::from("ore"),
                capacity: 100.0,
            },
            csp_data::ColonyModuleDef::Store {
                resource: csp_data::ResourceId::from("metal"),
                capacity: 60.0,
            },
        ],
        stores: vec![
            csp_data::ColonyStoreInit {
                resource: csp_data::ResourceId::from("electric_charge"),
                level: 3_000.0,
            },
            csp_data::ColonyStoreInit {
                resource: csp_data::ResourceId::from("ore"),
                level: 10.0,
            },
        ],
    }
}

/// §5.3 "rails warp ×10⁵, 20 colonies": the shipping world's authored
/// colony plus 19 founded ones, then long `step_frame` advances —
/// each call is a frame, bounded by `MAX_TICKS_PER_FRAME` and the
/// §2.6.5 event budget. Returns `(max events/call, mean ms/call, detail)`.
fn colony_warp_run() -> Result<(u64, f64, String), ReplayError> {
    let mut w = shipping_world()?;
    // The authored rask-mines colony + 19 founded on the two Terrin
    // sites → the row's 20 colonies.
    for i in 0..19u32 {
        let site = if i % 2 == 0 {
            "terrin-launch"
        } else {
            "terrin-downrange"
        };
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::FoundColony {
                def: perf_colony_def(site),
            },
        );
    }
    w.step_frame(w.clock().ticks() + 1)
        .map_err(ReplayError::Sim)?;
    // Warmup warp — the founding recompute's first publish is one-time.
    w.step_frame(w.clock().ticks() + csp_sim::MAX_TICKS_PER_FRAME)
        .map_err(ReplayError::Sim)?;
    let frames: u64 = if cfg!(debug_assertions) { 4 } else { 8 };
    let mut max_events = 0u64;
    let mut total_ms = 0.0;
    let mut total_events = 0u64;
    for _ in 0..frames {
        let t0 = Instant::now();
        let report = w
            .step_frame(w.clock().ticks() + csp_sim::MAX_TICKS_PER_FRAME)
            .map_err(ReplayError::Sim)?;
        total_ms += t0.elapsed().as_secs_f64() * 1e3;
        max_events = max_events.max(report.events_processed);
        total_events += report.events_processed;
    }
    Ok((
        max_events,
        total_ms / frames as f64,
        format!("{frames} warp frames, {total_events} events total"),
    ))
}

fn measure_colony_warp_events() -> Result<Measurement, ReplayError> {
    let (max_events, _, detail) = colony_warp_run()?;
    Ok(Measurement {
        value: max_events as f64,
        detail,
    })
}

fn measure_colony_warp_ms() -> Result<Measurement, ReplayError> {
    let (_, ms, detail) = colony_warp_run()?;
    Ok(Measurement { value: ms, detail })
}

/// §5.3 "Weld Stage 1, 1000-part station ≤20 ms synchronous": mean
/// `csp_phys::weld` cost on the 1000-part tower graph.
fn measure_weld1_1000() -> Result<Measurement, ReplayError> {
    let craft = tower_craft(1000, false);
    let calls: u32 = if cfg!(debug_assertions) { 2 } else { 5 };
    let t0 = Instant::now();
    for _ in 0..calls {
        let body = csp_phys::weld(&craft.graph, &craft.parts)
            .map_err(|_| ReplayError::MalformedGolden("perf: weld failed"))?;
        if body.collider.hulls.len() != 1000 {
            return Err(ReplayError::MalformedGolden("perf: weld hull count wrong"));
        }
    }
    let ms = t0.elapsed().as_secs_f64() * 1e3 / f64::from(calls);
    Ok(Measurement {
        value: ms,
        detail: format!("{calls} weld calls"),
    })
}

/// §5.3 "Weld Stage 2 (voxelization), 300-part craft ≤500 ms native":
/// the full `build_aero_tables` — voxelize + the 66-direction sweep.
fn measure_voxelize_300() -> Result<Measurement, ReplayError> {
    let craft = tower_craft(300, false);
    let body = csp_phys::weld(&craft.graph, &craft.parts)
        .map_err(|_| ReplayError::MalformedGolden("perf: weld failed"))?;
    let t0 = Instant::now();
    let tables = csp_phys::build_aero_tables(
        &body,
        &craft.graph,
        &craft.parts,
        csp_phys::ConfigMask::ALL,
        0,
    )
    .map_err(|_| ReplayError::MalformedGolden("perf: aero-table build failed"))?;
    let ms = t0.elapsed().as_secs_f64() * 1e3;
    if tables.parts.len() != 300 {
        return Err(ReplayError::MalformedGolden("perf: table part count wrong"));
    }
    Ok(Measurement {
        value: ms,
        detail: format!("voxel edge {:.3} m", tables.voxel_edge_m),
    })
}

/// §5.3 "telemetry stream, 1000-part vessel @16 Hz ≤256 KB/s": the
/// shipped dashboard's field set projected + JSON-encoded over a
/// snapshot of the 1000-part world — bytes/frame × 16 Hz = B/s.
fn measure_telemetry_1000() -> Result<Measurement, ReplayError> {
    let mut w = fixture_world()?;
    let env = w
        .environment()
        .cloned()
        .ok_or(ReplayError::MalformedGolden("no env"))?;
    let r = env.radius_m + 300_000.0;
    w.enqueue(
        SourceClass::Ui,
        0,
        SimCommand::SpawnPhysicalVessel {
            craft: tower_craft(1000, false),
            pos: DVec3::new(r, 0.0, 0.0),
            rot: DQuat::IDENTITY,
            vel: orbital_vel(&env, r),
            ang_vel: DVec3::ZERO,
        },
    );
    w.step_frame(w.clock().ticks() + 1)
        .map_err(ReplayError::Sim)?;
    let snap = csp_server::telemetry::TelemetrySnapshot::from_world(&w);
    // The shipped default dashboard's field set — the same list
    // `ws_subscription_stays_within_byte_budget` subscribes.
    let fields: Vec<String> = [
        "tick",
        "warp_level",
        "vessels.0.altitude_m",
        "vessels.0.speed_mps",
        "vessels.0.vertical_speed_mps",
        "vessels.0.horizontal_speed_mps",
        "vessels.0.apoapsis_altitude_m",
        "vessels.0.periapsis_altitude_m",
        "vessels.0.time_to_apoapsis_s",
        "vessels.0.throttle",
        "vessels.0.mass_kg",
        "vessels.0.residence",
        "vessels.0.pos_m",
        "vessels.0.vel_mps",
        "vessels.0.elements",
        "vessels.0.maneuver_node",
        "vessels.0.control_lease",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    let (map, unknown) = snap.project(&fields);
    if !unknown.is_empty() {
        return Err(ReplayError::MalformedGolden(
            "perf: telemetry field missing",
        ));
    }
    let bytes = serde_json::to_vec(&map)
        .map_err(|_| ReplayError::MalformedGolden("perf: telemetry encode"))?
        .len();
    Ok(Measurement {
        value: (bytes * 16) as f64,
        detail: format!("{bytes} B/frame × 16 Hz"),
    })
}

/// §5.4's kill criterion, measured headless — the latency is CPU work
/// (session op + dirty-gated recompute + egui pass + RenderExtract
/// packet build), so no GPU is needed. Mirrors the probe in
/// `csp-view/tests/vab_latency.rs` (that test owns the hard 16 ms
/// release assert; this row exists so the committed log carries the
/// same measurement under the movement band — C17-F5). The 300-part
/// stack is built through the real op channel, same as the test.
fn measure_vab_300() -> Result<Measurement, ReplayError> {
    use csp_ui::vab::VabAction;

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let parts = csp_data::load_parts(&root.join("parts")).map_err(ReplayError::Load)?;
    let res = csp_data::load_resources(&root.join("resources.ron")).map_err(ReplayError::Load)?;

    let mut view = csp_view::View::with_sim(World::new());
    view.install_vab_catalog(parts, res, csp_data::data_set_hash(&root).ok());
    view.set_mode(csp_view::ViewMode::Vab);

    view.with_vab_session(|v| v.apply(VabAction::SetRoot(csp_data::PartId::from("csp.tank.s320"))))
        .map_err(|_| ReplayError::MalformedGolden("perf: vab SetRoot rejected"))?;
    let mut parent =
        view.with_vab_session(|v| v.session().root())
            .ok_or(ReplayError::MalformedGolden(
                "perf: vab session has no root",
            ))?;
    for _ in 1..300 {
        view.with_vab_session(|v| {
            v.apply(VabAction::AttachAuto {
                part: csp_data::PartId::from("csp.tank.s320"),
                parent,
            })
        })
        .map_err(|_| ReplayError::MalformedGolden("perf: vab AttachAuto rejected"))?;
        parent = view
            .with_vab_session(|v| v.session().iter().last().map(|(id, _)| id))
            .ok_or(ReplayError::MalformedGolden("perf: vab session emptied"))?;
    }
    if view.with_vab_session(|v| v.session().len()) != 300 {
        return Err(ReplayError::MalformedGolden(
            "perf: vab stack did not reach 300 parts",
        ));
    }

    // One unmeasured frame first — the 300-op build left the session
    // dirty, and measuring THAT frame would bill the full recompute to
    // the "clean" warm-up. Same ordering the test uses.
    view.begin_frame();
    view.end_frame();

    let mut worst_ms = 0.0f64;
    for _ in 0..5 {
        let top = view
            .with_vab_session(|v| v.session().iter().last().map(|(id, _)| id))
            .ok_or(ReplayError::MalformedGolden("perf: vab session emptied"))?;
        let t = Instant::now();
        view.with_vab_session(|v| {
            let _ = v.apply(VabAction::SetStage {
                node: top,
                stage: 0,
            });
            let n = v.session().iter().nth(10).map(|(id, _)| id);
            if let Some(n) = n {
                let _ = v.apply(VabAction::SetScale {
                    node: n,
                    scale: 1.0,
                });
            }
        });
        let op_ms = t.elapsed().as_secs_f64() * 1000.0;
        let t = Instant::now();
        view.begin_frame();
        view.end_frame();
        worst_ms = worst_ms.max(op_ms + t.elapsed().as_secs_f64() * 1000.0);
    }
    Ok(Measurement {
        value: worst_ms,
        detail: "worst of 5 trials, 300-part stack via VabAction ops".to_string(),
    })
}

/// A surfaceless adapter + device for the §5.4 depth row — `None` when
/// no driver exists (the probe's honest harness error). LowPower
/// prefers the CPU adapter (lavapipe): measurements should run on the
/// weakest conformant implementation — the same policy the render gate
/// applies (`csp-view/tests/render_gate.rs`).
fn headless_gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        ..Default::default()
    }))
    .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("perf-depth-row"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        ..Default::default()
    }))
    .ok()?;
    Some((device, queue))
}

/// §5.4 R1's row — the 10⁷ m / 1 m-object depth-fight count the PRD
/// requires "recorded in the perf log" (C16-F5). The scene and the
/// shipped flight pipeline it renders through live in
/// `csp_view::depth_probe`; this arm owns the harness controls so a
/// bogus count can never read as a measurement:
///
/// - a run at 1 km with the same construction must report ZERO
///   order-dependent pixels — f32 depth resolves trivially there, so
///   a nonzero control means the probe is broken, not the format;
/// - a fully coplanar run (0 m spread) must report a large count —
///   every covered pixel IS order-dependent by construction, so a
///   small count means the differ is blind.
fn measure_depth_fight_1e7() -> Result<Measurement, ReplayError> {
    let Some((device, queue)) = headless_gpu() else {
        return Err(ReplayError::MalformedGolden(
            "perf: no headless GPU adapter — the §5.4 depth row needs a \
             Vulkan ICD (mesa-vulkan-drivers ships lavapipe)",
        ));
    };
    let probe = |depth_m: f64, spread_m: f64| {
        csp_view::depth_probe::depth_fight_pixels(&device, &queue, depth_m, spread_m)
            .map_err(|_| ReplayError::MalformedGolden("perf: depth probe readback failed"))
    };
    let near = probe(1.0e3, 1.0)?;
    if near.order_dependent_px != 0 {
        return Err(ReplayError::MalformedGolden(
            "perf: depth probe shows order-dependent pixels at 1 km — \
             the harness is broken, not the depth format",
        ));
    }
    let coplanar = probe(1.0e3, 0.0)?;
    if coplanar.order_dependent_px < 100_000 {
        return Err(ReplayError::MalformedGolden(
            "perf: depth probe undercounts a fully coplanar pair — \
             the differ is blind",
        ));
    }
    let m = probe(1.0e7, 1.0)?;
    Ok(Measurement {
        value: f64::from(m.order_dependent_px),
        detail: format!(
            "{}×{} two-order diff, controls 1 km/1 m → {} px and 1 km/coplanar → {} px",
            m.width, m.height, near.order_dependent_px, coplanar.order_dependent_px
        ),
    })
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers the
// shipping fallible paths. Debug runs assert the measurements resolve
// (a debug profile is a diagnostic, not the budget claim — the release
// bounds are asserted only when `debug_assertions` is off, matching the
// existing measure_* convention in `scenarios.rs`).
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Strict-budget mode: `CSP_PERF_STRICT=1` or a
    /// `CSP_PERF_MACHINE=ref-*` name asserts `measured <= budget` —
    /// the §5.3 number as the milestone writes it. Everywhere else a
    /// measurement test is a *diagnostic*: it asserts the value is
    /// finite and prints measured-vs-budget, and the real enforcement
    /// is the committed-log regression diff (`csp-perf --baseline`,
    /// run by the release CI job) — never a `budget × 400` bound that
    /// reads like the budget while gating nothing.
    fn strict_budget() -> bool {
        std::env::var("CSP_PERF_STRICT").is_ok_and(|v| !v.is_empty() && v != "0")
            || std::env::var("CSP_PERF_MACHINE")
                .map(|m| m.to_ascii_lowercase().starts_with("ref-"))
                .unwrap_or(false)
    }

    /// The one assertion shape every measurement test shares: the
    /// value must be finite (a NaN/±inf measurement is a harness
    /// bug), the measured-vs-budget number is printed for the log —
    /// and only in [`strict_budget`] mode is the §5.3 bound itself
    /// asserted.
    fn check_value(row: &'static BudgetRow, value: f64, detail: &str) {
        assert!(value.is_finite(), "{}: non-finite measurement", row.id);
        eprintln!(
            "{}: {value:.3} {} (budget {:.3}; {:.2}×) — {detail}",
            row.id,
            row.unit,
            row.budget,
            value / row.budget,
        );
        if strict_budget() {
            assert!(
                value <= row.budget,
                "{}: {value:.3} {} over the §5.3 budget {:.3}",
                row.id,
                row.unit,
                row.budget
            );
        }
    }

    /// §8.5's "every measurable row resolves" — the coverage pin: each
    /// `measurable` [`BudgetRow`] has a [`measure_row`] arm and every
    /// entry the log emits corresponds to a row.
    #[test]
    fn every_measurable_row_has_a_benchmark() {
        let ids: Vec<&str> = BUDGET_ROWS.iter().map(|r| r.id).collect();
        // Unique ids — a dup would double-report one scene.
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(ids.len(), sorted.len(), "duplicate budget row ids");
        // The §5.3 table: 9 measurable CPU rows + 9 gated (8 fps + wasm
        // voxelize); the C17-F5 `vab.interaction-300-part` row is the
        // §5.4 kill criterion riding this log, and the C16-F5
        // `render.depth-fight-1e7` row is §5.4's R1 depth measurement —
        // so measurable is 11.
        assert_eq!(
            BUDGET_ROWS.iter().filter(|r| r.measurable).count(),
            11,
            "the §5.3 CPU rows drifted from the table"
        );
        assert_eq!(
            BUDGET_ROWS.iter().filter(|r| !r.measurable).count(),
            9,
            "the hardware-gated rows drifted"
        );
    }

    /// The serialization contract — the log round-trips through RON
    /// with its schema version (§7.8 sidecar).
    #[test]
    fn perf_log_ron_round_trip() {
        let log = PerfLog {
            schema_version: PERF_LOG_SCHEMA_VERSION,
            machine: detect_machine(),
            entries: vec![PerfEntry {
                id: "x".into(),
                scene: "y".into(),
                budget_value: 1.0,
                unit: UNIT_MS.into(),
                measured_value: Some(0.5),
                detail: None,
                status: PerfStatus::Pass,
            }],
        };
        let text = ron::ser::to_string_pretty(&log, ron::ser::PrettyConfig::default()).unwrap();
        let back: PerfLog = ron::de::from_str(&text).unwrap();
        assert_eq!(back.schema_version, PERF_LOG_SCHEMA_VERSION);
        assert_eq!(back.entries.len(), 1);
        assert_eq!(back.entries[0].status, PerfStatus::Pass);
    }

    /// Each measurement resolves to a finite value and reports
    /// measured-vs-budget — strict assertion of the §5.3 number only
    /// in [`strict_budget`] mode; the release CI job's committed-log
    /// regression diff is the always-on gate.
    #[test]
    fn sim_200_part_atmo_within_budget() {
        let m = measure_atmo_200().unwrap();
        check_value(&BUDGET_ROWS[0], m.value, &m.detail);
    }

    #[test]
    fn sim_1000_part_orbit_within_budget() {
        let m = measure_orbit_1000().unwrap();
        check_value(&BUDGET_ROWS[1], m.value, &m.detail);
    }

    #[test]
    fn sim_300_part_landed_within_budget() {
        let m = measure_landed_300().unwrap();
        check_value(&BUDGET_ROWS[2], m.value, &m.detail);
    }

    #[test]
    fn sim_map_500_rails_within_budget() {
        let m = measure_rails_500().unwrap();
        check_value(&BUDGET_ROWS[3], m.value, &m.detail);
    }

    #[test]
    fn warp_20_colonies_within_budgets() {
        let (max_events, ms, detail) = colony_warp_run().unwrap();
        check_value(&BUDGET_ROWS[4], max_events as f64, &detail);
        check_value(&BUDGET_ROWS[5], ms, &detail);
    }

    #[test]
    fn weld_stage1_1000_within_budget() {
        let m = measure_weld1_1000().unwrap();
        check_value(&BUDGET_ROWS[6], m.value, &m.detail);
    }

    #[test]
    fn weld_stage2_300_within_budget() {
        let m = measure_voxelize_300().unwrap();
        check_value(&BUDGET_ROWS[7], m.value, &m.detail);
    }

    #[test]
    fn telemetry_1000_within_budget() {
        let m = measure_telemetry_1000().unwrap();
        check_value(&BUDGET_ROWS[8], m.value, &m.detail);
    }

    /// §5.4 R1's row — the only GPU-backed measurement in the suite,
    /// so it follows the render gate's skip policy: no adapter is a
    /// `SKIP` line locally and a FAILURE under
    /// `CSP_RENDER_GATE_REQUIRED` (CI installs lavapipe so the row can
    /// never quietly degenerate into a green no-op — the same defect
    /// class the render gate's required mode exists for).
    #[test]
    fn render_depth_fight_row_measures() {
        if headless_gpu().is_none() {
            assert!(
                !(std::env::var("CSP_RENDER_GATE_REQUIRED")
                    .is_ok_and(|v| !v.is_empty() && v != "0")),
                "render.depth-fight-1e7: CSP_RENDER_GATE_REQUIRED is set but \
                 no headless GPU adapter exists — install a Vulkan ICD"
            );
            eprintln!("perf: SKIP render.depth-fight-1e7 — no headless GPU adapter");
            return;
        }
        let m = measure_depth_fight_1e7().unwrap();
        let row = BUDGET_ROWS
            .iter()
            .find(|r| r.id == "render.depth-fight-1e7")
            .unwrap();
        check_value(row, m.value, &m.detail);
    }

    /// The committed-log regression gate's rule set, pinned on
    /// synthetic logs: >10% slowdowns flag, a §5.3 Pass→Fail crossing
    /// flags under the noise band, an unmeasured measurable row
    /// flags, and noise / improvements / gated rows do not.
    #[test]
    fn regression_diff_flags_what_it_must() {
        fn entry(id: &str, budget: f64, measured: Option<f64>, status: PerfStatus) -> PerfEntry {
            PerfEntry {
                id: id.to_string(),
                scene: id.to_string(),
                budget_value: budget,
                unit: "ms".to_string(),
                measured_value: measured,
                detail: None,
                status,
            }
        }
        let log = |entries: Vec<PerfEntry>| PerfLog {
            schema_version: PERF_LOG_SCHEMA_VERSION,
            machine: detect_machine(),
            entries,
        };
        let base = log(vec![
            entry("noise", 2.0, Some(1.0), PerfStatus::Pass),
            entry("slow", 1.0, Some(1.0), PerfStatus::Pass),
            entry("crossing", 0.5, Some(0.49), PerfStatus::Pass),
            entry("already-fail", 0.5, Some(0.7), PerfStatus::Fail),
            entry("dead", 1.0, Some(1.0), PerfStatus::Pass),
            entry("gated", 60.0, None, PerfStatus::HardwareGated),
        ]);
        let cur = log(vec![
            entry("noise", 2.0, Some(1.09), PerfStatus::Pass), // +9% — noise
            entry("slow", 1.0, Some(1.11), PerfStatus::Fail),  // +11% — regression
            entry("crossing", 0.5, Some(0.51), PerfStatus::Fail), // under +10% but Pass→Fail
            entry("already-fail", 0.5, Some(0.72), PerfStatus::Fail), // <+10% on a red row
            entry("dead", 1.0, None, PerfStatus::Fail),        // unmeasured measurable row
            entry("gated", 60.0, None, PerfStatus::HardwareGated),
            entry("new-row", 1.0, Some(99.0), PerfStatus::Fail), // absent in baseline
        ]);
        let regs = regression_diff(&cur, &base);
        let ids: Vec<&str> = regs.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["slow", "crossing", "dead"], "{regs:?}");
        // And the real committed log parses — the baseline the
        // release job diffs against must load or the gate is a no-op.
        let committed = load_perf_log(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../docs/perf/perf-log-devbox-release.ron"),
        )
        .expect("committed perf log parses");
        assert!(!committed.entries.is_empty());
    }
}
