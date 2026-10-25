//! Golden scenario recipes (PLAN §8.3, §8.5).
//!
//! A recipe is a named deterministic driver: build the initial world, then
//! push commands through [`World::enqueue`]/[`World::step_frame`].
//!
//! **Recorder contract** — what makes the §7.6 command log self-contained
//! and reconstructible by `replay::replay`:
//!
//! - a frame's `step_frame` target equals its *drain tick* + 1 — the next
//!   advancing boundary, never a larger guess;
//! - every further advance flows through a logged `SimCommand::AdvanceTo`
//!   (§2.6.3), so the run's real targets are recoverable from the log
//!   itself;
//! - when `step_frame` returns [`AdvanceReport::partial`](csp_sim::AdvanceReport::partial)
//!   (§2.6.5 back-pressure), the same target is re-issued until reached —
//!   the budget bounds one call, never the run.
//!
//! A recipe that breaks the contract records fine but fails `record`'s
//! self-check: [`Replay::final_tick`](crate::replay::Replay::final_tick)
//! pins the run's end, so a stride the log cannot reproduce surfaces as a
//! `ClockMismatch`, not a silent divergence.

use std::path::Path;

use csp_core::SimError;
use csp_sim::{SimCommand, SourceClass, World};

use crate::replay::ReplayError;

/// One recordable scenario: how to build the pristine world and how to
/// drive it. `initial` produces the world whose snapshot becomes the
/// artifact's initial snapshot — it is handed the scenario's frozen
/// fixture dir (§8.3) so a scenario that loads data loads exactly what
/// the golden hash pins; `drive` performs the run, and the resulting
/// [`World::command_log`] is the artifact body.
pub struct ScenarioRecipe {
    /// Scenario name — also its directory under `data/test-fixtures/` (§8.3).
    pub name: &'static str,
    /// The §2.7 save seed the run is recorded under. M0's `World` draws no
    /// seeded streams yet; the field rides the §7.6 artifact so replays
    /// carry it from day one.
    pub save_seed: u64,
    /// Builds the pristine pre-run world from the frozen fixture dir.
    pub initial: fn(&Path) -> Result<World, ReplayError>,
    /// Drives the run; propagates the first sim failure (§2.13).
    pub drive: fn(&mut World) -> Result<(), SimError>,
}

/// Every golden scenario, in registry order.
pub const SCENARIOS: &[ScenarioRecipe] = &[
    ScenarioRecipe {
        name: "trivial",
        save_seed: 0,
        initial: |_| Ok(World::new()),
        drive: drive_trivial,
    },
    ScenarioRecipe {
        name: "flight-launch",
        save_seed: 0,
        initial: flight_initial,
        drive: drive_flight,
    },
];

/// Look a recipe up by scenario name.
pub fn find(name: &str) -> Option<&'static ScenarioRecipe> {
    SCENARIOS.iter().find(|r| r.name == name)
}

/// The M0 trivial golden: a fresh world, ~1000 ticks, and `SetWarp`
/// commands from two source classes so the same-tick ordering rule
/// (§2.2.1) is exercised. The recorded log is:
/// - tick 0:   `Ui:0` `AdvanceTo(500)` — the first frame target;
/// - tick 500: `Ui:0` `SetWarp(4)`, `External:3` `SetWarp(16)` — one drain,
///   two sources;
/// - tick 501: `Ui:0` `AdvanceTo(1000)` — the run's end.
fn drive_trivial(w: &mut World) -> Result<(), SimError> {
    w.enqueue(SourceClass::Ui, 0, SimCommand::AdvanceTo(500));
    w.step_frame(1)?; // drains at tick 0; the logged AdvanceTo drives to 500
    w.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(4));
    w.enqueue(SourceClass::External, 3, SimCommand::SetWarp(16));
    w.step_frame(501)?; // drains both at the tick-500 boundary, then one tick
    w.enqueue(SourceClass::Ui, 0, SimCommand::AdvanceTo(1000));
    w.step_frame(502)?; // drains at tick 501; AdvanceTo drives to 1000
    Ok(())
}

// ---------------------------------------------------------------------------
// flight-launch — the M2 acceptance arc (PLAN M2 exit criterion): an
// authored part-set craft launches from Terrin's surface, reaches orbit,
// comes back on rails → bubble, deorbits, reenters, and lands — the last
// step observed as the §2.6.4 surface-latch transition. The controller is
// a scripted phase machine (not M9a): absolute `SetThrottle`/`SetAttitude`
// commands at a 64-tick cadence, `AdvanceTo` for every further advance
// (the recorder contract).
// ---------------------------------------------------------------------------

use csp_core::{DQuat, DVec3};
use csp_data::BodyId;
use csp_phys::{
    ColliderShape, Environment, ForceSite, ForceSiteKind, PartGraph, PartNode, PartNodeId,
    PartTransform, SymMat3, WeldPart,
};
use csp_sim::orbit::PatchedConics;
use csp_sim::{PhysicsConfig, VesselId, VesselState};

/// The flight scenario's vessel — the first (only) physical spawn.
pub const FLIGHT_VESSEL: VesselId = VesselId(0);

/// What [`flight_run`] observed — the acceptance arc's checkpoints,
/// reportable by tests (the golden recipe ignores it).
#[derive(Clone, Debug, Default)]
pub struct FlightReport {
    /// The elements `ToRails` produced — the "reached orbit" evidence.
    pub orbit_elements: Option<csp_data::KeplerElements>,
    /// Deepest atmospheric speed on the way down (m/s) — the reentry.
    pub max_reentry_speed_mps: f64,
    /// Hottest any part got across the run (K) — the M4b reentry
    /// heating evidence: must clear ambient and stay under the
    /// authored `max_temp_k` for a survivable reentry.
    pub max_part_temp_k: f64,
    /// The tick the vessel surface-latched, if it did.
    pub latch_tick: Option<u64>,
    /// M7-C: the funds the `RecoverVessel` at run end paid out
    /// (micros) — `Some` once the latched vessel was recovered at
    /// the fixture's launch site.
    pub recovery_micros: Option<u64>,
}

impl FlightReport {
    /// Fold the vessel's serialized thermal record into the peak.
    fn sample_thermal(&mut self, vessel: &csp_sim::Vessel) {
        self.max_part_temp_k = vessel
            .thermal
            .part_temps_k
            .values()
            .copied()
            .fold(self.max_part_temp_k, f64::max);
    }
}

/// The scenario's build-from-data half: the fixture's system and balance
/// files, and the Terrin bubble environment (`Environment::for_body` —
/// the spinning datum, so the launch site's co-rotation is real).
fn flight_initial(dir: &Path) -> Result<World, ReplayError> {
    let sys = csp_data::load_system(dir).map_err(ReplayError::Load)?;
    let balance = csp_data::load_balance(&dir.join("balance.ron")).map_err(ReplayError::Load)?;
    let defs: Vec<csp_data::BodyDef> = sys.bodies.iter().map(|b| b.def.clone()).collect();
    let orbits = PatchedConics::from_body_defs(&defs)?;
    let terrin = defs
        .iter()
        .find(|b| b.id == BodyId::from("terrin"))
        .ok_or(ReplayError::MalformedGolden("fixture lacks terrin"))?;
    let environment = Environment::for_body(terrin, DVec3::ZERO, terrin.rotation_period_s);
    let mut world = World::with_physics(
        orbits,
        PhysicsConfig {
            environment,
            dominant: BodyId::from("terrin"),
        },
        balance.advance_to_max_events_per_frame,
    );
    // §2.9's object caps come from the fixture's balance.ron — the
    // same wiring `csp-view` startup does, so fixture-authored caps
    // are honoured instead of the no-data defaults.
    world.set_vessel_caps(csp_sim::VesselCaps {
        max_vessels: balance.max_tracked_vessels,
        max_debris: balance.max_debris,
    });
    // §2.4's docking tunables — same wiring as the caps: the fixture's
    // balance.ron is the source of truth, so fixture-authored capture
    // windows/predicates are honoured instead of the no-data defaults.
    world.set_docking_config(csp_sim::DockingConfig::from_balance(&balance))?;
    // §3.5's transfer tunables — same wiring (M6-D).
    world.set_transfer_config(csp_sim::TransferConfig::from_balance(&balance));
    // M9a §2.10: the script host + ascent autopilot and the per-save
    // op budget — same wiring `csp-view` startup does, so a recorded
    // run that loads scripts replays them.
    world.set_script_op_budget(balance.script_op_budget);
    world.set_script_engine(Box::new(csp_script::RhaiHost::new()));
    world.set_autopilot_driver(Box::new(csp_ai::AscentAutopilot::new()));
    // §3.2's science tunables + experiment defs — same wiring (M7-A):
    // the fixture's balance.ron/experiments/ are the source of truth.
    world.set_science_config(csp_sim::ScienceConfig::from_balance(&balance));
    let experiments =
        csp_data::load_experiments(&dir.join("experiments")).map_err(ReplayError::Load)?;
    world.set_experiments(
        experiments
            .by_id()
            .into_iter()
            .map(|(id, def)| (id, def.clone()))
            .collect(),
    );
    // §3.5's economy tunables + site table — same wiring (M7-C): the
    // fixture's balance.ron and sites.ron are the source of truth.
    world.set_economy_config(csp_sim::economy::EconomyConfig::from_balance(&balance))?;
    let sites = csp_data::load_sites(&dir.join("sites.ron")).map_err(ReplayError::Load)?;
    world.set_sites(
        sites
            .sites
            .iter()
            .map(|s| (s.id.clone(), s.clone()))
            .collect(),
    );
    // M7-D §3.6: the contract templates + board-size tunable — the
    // fixture's `contracts/` dir (empty when the fixture authors
    // none) and its balance.ron are the source of truth.
    world.set_contracts(
        csp_data::load_contracts(&dir.join("contracts"))
            .map_err(ReplayError::Load)?
            .by_id()
            .into_iter()
            .map(|(id, def)| (id, def.clone()))
            .collect(),
    );
    world.set_contract_config(csp_sim::ContractConfig::from_balance(&balance));
    // M8-A §3.5.1: the per-body colony environments — rotation and
    // authored insolation from the fixture's body defs (construction
    // config, the `set_sites` precedent).
    world.set_colony_env(csp_sim::colony_env_from_body_defs(&defs));
    // M8-C §3.5: the life-support tunables — same wiring (the
    // `set_economy_config` precedent); the per-save flag itself is
    // sim state, not construction config.
    world.set_life_support_config(csp_sim::LifeSupportConfig::from_balance(&balance));
    // M8-B §3.5.1: the fixture's `recipes/` dir — empty reads as a
    // recipe-less world (the `set_colony_env` precedent).
    world.set_recipes(
        csp_data::load_recipes(&dir.join("recipes"))
            .map_err(ReplayError::Load)?
            .by_id()
            .into_iter()
            .map(|(id, def)| (id, def.clone()))
            .collect(),
    );
    // M8-B §3.5.1: the fixture's `colonies.ron` — the same
    // source-of-truth wiring `csp-view` startup does; a missing file
    // reads as an empty set, so colony-free fixtures are unaffected.
    // Founded at world build (pre-snapshot), never through the command
    // queue — the records ride `initial_snapshot`, not the log.
    world
        .found_authored_colonies(
            &csp_data::load_colonies(&dir.join("colonies.ron"))
                .map_err(ReplayError::Load)?
                .colonies,
        )
        .map_err(ReplayError::Sim)?;
    Ok(world)
}

/// The authored part set (§2.4/§3.1 — scenario-side, no VAB): a 5 t
/// single-stage booster, a 6 m × 1.5 m box with a 200 kN engine on its
/// base pointing down the body +z axis. TWR ≈ 4 at Terrin surface.
/// `pub` for M9d: the eval suite's `PadLaunch` and the `csp-mcp`
/// mission e2e spawn this same craft.
pub fn flight_craft() -> csp_sim::CraftDef {
    let mut graph = PartGraph::new(PartNodeId(0));
    graph.nodes.insert(
        PartNodeId(0),
        PartNode::new(None, PartTransform::IDENTITY, 0),
    );
    let booster = WeldPart {
        experiments: Vec::new(),
        lab: None,
        engine: None,
        dock_ports: vec![],
        tanks: vec![],
        crew_seats: 0,
        structural: csp_data::StructuralDef::unlimited(),
        mass_kg: 5_000.0,
        com: DVec3::ZERO,
        inertia: SymMat3::solid_box(5_000.0, DVec3::new(0.75, 0.75, 3.0)),
        collider: ColliderShape::Box {
            half_extents: DVec3::new(0.75, 0.75, 3.0),
        },
        force_sites: vec![ForceSite::new(
            DVec3::new(0.0, 0.0, -3.0),
            DVec3::Z,
            ForceSiteKind::Thrust,
            200_000.0,
        )],
        aero: csp_data::AeroDef::constant(1.0, 0.0, 1.0),
        thermal: csp_data::ThermalDef {
            heat_capacity_j_per_k: 400_000.0,
            max_temp_k: 1_100.0,
            emissivity: 0.8,
            conduction_w_per_k: 60.0,
        },
    };
    csp_sim::CraftDef {
        part_ids: vec![],
        graph,
        parts: vec![booster],
        // M7-C: a modest 5 t booster's build cost — the manufacture
        // charge at spawn and the recovery payout at the latch all
        // exercise it.
        part_costs: vec![50_000.0],
        // M9b: the bench's authored craft carries no reliability
        // model (the empty-table convention).
        reliability: vec![],
    }
}

/// Quaternion taking body +z (the thrust axis) to `dir` — the scripted
/// controller's attitude surface. `pub` for M9d: the eval suite's
/// ascent program steers with the same convention.
pub fn thrust_attitude(dir: DVec3) -> DQuat {
    let n = dir.norm();
    if n.partial_cmp(&0.0) != Some(core::cmp::Ordering::Greater) {
        return DQuat::IDENTITY;
    }
    let d = dir / n;
    let cos = DVec3::Z.dot(d);
    if cos > 0.999_999 {
        return DQuat::IDENTITY;
    }
    if cos < -0.999_999 {
        return DQuat::from_axis_angle(DVec3::X, core::f64::consts::PI);
    }
    DQuat::from_axis_angle(DVec3::Z.cross(d), libm::acos(cos))
}

/// Osculating `(r_a, r_p)` radius estimates from cartesian state —
/// the controller's apoapsis/periapsis read.
fn apsis(r_vec: DVec3, v_vec: DVec3, mu: f64) -> (f64, f64) {
    let p = r_vec.cross(v_vec).norm_squared() / mu;
    let energy = v_vec.norm_squared() / 2.0 - mu / r_vec.norm();
    let a = -mu / (2.0 * energy);
    // `p = a(1 − e²)` — for an open trajectory `a < 0` gives `e > 1`
    // and the r_a formula yields a negative value, which callers treat
    // as "no apoapsis".
    let e = libm::sqrt((1.0 - p / a).max(0.0));
    (p / (1.0 - e), p / (1.0 + e))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FlightPhase {
    Ascend,
    Orbit,
    Deorbit,
    Descend,
}

/// The recipe's pad spawn — equatorial site on the +x axis, body +z
/// (thrust) pointed radially up, velocity = surface co-rotation
/// (ω×r); drains one boundary and returns the environment the caller
/// needs for orbit checks. Shared by `flight_run`, the M9a tests, and
/// M9d's `SetupSpec::PadLaunch` — one spawn convention for every
/// physical-vessel scenario.
pub fn spawn_pad(w: &mut World) -> Result<Environment, SimError> {
    let env = w
        .environment()
        .ok_or(SimError::Fatal("flight scenario requires a physics world"))?
        .clone();
    let pad = DVec3::new(env.radius_m + 3.5, 0.0, 0.0);
    let rot0 = DQuat::from_axis_angle(DVec3::Y, core::f64::consts::FRAC_PI_2);
    w.enqueue(
        SourceClass::Ui,
        0,
        SimCommand::SpawnPhysicalVessel {
            craft: flight_craft(),
            pos: pad,
            rot: rot0,
            vel: env.body_spin.cross(pad - env.body_center),
            ang_vel: DVec3::ZERO,
        },
    );
    w.step_frame(w.clock().ticks() + 1)?;
    Ok(env)
}

/// The scripted controller driving the launch→orbit→reentry→land arc.
/// Every decision reads only sim state — a deterministic function, so
/// the command log it emits replays identically.
#[allow(clippy::too_many_lines)] // the M7-C recovery tail grew it past the limit
pub fn flight_run(w: &mut World) -> Result<FlightReport, SimError> {
    let mut report = FlightReport::default();
    let env = spawn_pad(w)?;
    let mu = env.mu_m3_per_s2;
    let radius = env.radius_m;
    let atm_top = env.atmosphere.as_ref().map_or(0.0, |a| a.top_m);

    let mut phase = FlightPhase::Ascend;
    for _ in 0..800_000u64 {
        if matches!(
            w.vessels()[&FLIGHT_VESSEL].state,
            VesselState::SurfaceLatched(_)
        ) {
            report.latch_tick = Some(w.clock().ticks());
            break;
        }
        let clock = w.clock().ticks();

        // On rails there is no body — read state only when present.
        report.sample_thermal(&w.vessels()[&FLIGHT_VESSEL]);
        let (rel, vel) = match w.vessels()[&FLIGHT_VESSEL].body() {
            Some(b) => (b.com_world() - env.body_center, b.vel),
            None => (DVec3::ZERO, DVec3::ZERO),
        };
        let frame = FlightFrame::new(rel, vel, &env);
        let mut commands: Vec<SimCommand> = Vec::new();
        let frame_ticks: u64;
        match phase {
            FlightPhase::Ascend => {
                let (cs, ft, next) = ascend_step(&frame, mu, radius);
                commands.extend(cs);
                frame_ticks = ft;
                phase = next;
            }

            FlightPhase::Orbit => {
                report.orbit_elements = w.vessels()[&FLIGHT_VESSEL]
                    .orbit()
                    .map(|o| o.elements.clone());
                // ~8 min on rails — an instant closed-form jump — then
                // re-materialize for the deorbit leg.
                w.enqueue(SourceClass::Ui, 0, SimCommand::AdvanceTo(clock + 30_000));
                w.step_frame(clock + 1)?;
                w.enqueue(
                    SourceClass::Ui,
                    0,
                    SimCommand::ToBubble {
                        vessel: FLIGHT_VESSEL,
                    },
                );
                w.step_frame(w.clock().ticks() + 1)?;
                phase = FlightPhase::Deorbit;
                continue;
            }
            FlightPhase::Deorbit => {
                let (_, r_p) = apsis(frame.rel, frame.vel, mu);
                commands.push(SimCommand::SetAttitude {
                    vessel: FLIGHT_VESSEL,
                    rot: thrust_attitude(-frame.v_horiz),
                    ang_vel: DVec3::ZERO,
                });
                commands.push(SimCommand::SetThrottle {
                    vessel: FLIGHT_VESSEL,
                    throttle: 1.0,
                });
                if r_p < radius + 20_000.0 {
                    phase = FlightPhase::Descend;
                }
                frame_ticks = 512;
            }
            FlightPhase::Descend => {
                let (cs, ft) = descend_step(&frame, &env, atm_top, &mut report);
                commands.extend(cs);
                frame_ticks = ft;
            }
        }
        commands.push(SimCommand::AdvanceTo(clock + frame_ticks));
        for c in commands {
            w.enqueue(SourceClass::Ui, 0, c);
        }
        w.step_frame(clock + 1)?;
    }
    if report.latch_tick.is_none() {
        return Err(SimError::Fatal("flight scenario never latched"));
    }
    // M7-C §3.5: the flight's recovery — the vessel latched on Terrin,
    // so `RecoverVessel` at the launch site (efficiency 1.0) credits
    // its contents and part value and removes it. The command is
    // logged, so the golden replays it identically.
    let before = w.funds_micros();
    w.enqueue(
        SourceClass::Ui,
        0,
        SimCommand::RecoverVessel {
            vessel: FLIGHT_VESSEL,
            site: Some(csp_data::SiteId::from("terrin-launch")),
        },
    );
    w.step_frame(w.clock().ticks() + 1)?;
    report.recovery_micros = Some(w.funds_micros() - before);
    Ok(report)
}

fn drive_flight(w: &mut World) -> Result<(), SimError> {
    flight_run(w).map(|_| ())
}

/// The per-iteration kinematic picture the phase logic reads: the
/// body's parent-relative state decomposed into radial and horizontal
/// components. `prograde` is the in-plane velocity direction, falling
/// back to local east (co-rotation) before the vessel has downrange
/// speed.
struct FlightFrame {
    rel: DVec3,
    vel: DVec3,
    r: f64,
    up: DVec3,
    v_r: f64,
    v_horiz: DVec3,
    prograde: DVec3,
}

impl FlightFrame {
    fn new(rel: DVec3, vel: DVec3, env: &Environment) -> Self {
        let r = rel.norm();
        let up = if r > 0.0 { rel / r } else { DVec3::X };
        let v_r = vel.dot(up);
        let v_horiz = vel - up * v_r;
        let prograde = if v_horiz.norm() > 50.0 {
            v_horiz / v_horiz.norm()
        } else {
            let e = env.body_spin.cross(rel);
            if e.norm() > 1e-9 {
                e / e.norm()
            } else {
                DVec3::Y
            }
        };
        Self {
            rel,
            vel,
            r,
            up,
            v_r,
            v_horiz,
            prograde,
        }
    }
}

/// The Ascend phase's command block: the two-leg powered ascent —
/// climb-pitch pump until osculating apoapsis reaches ~75 km, then a
/// Δv-tapered horizontal circularization burn gated to the near-apo /
/// descending half (a prograde burn while climbing pumps apoapsis
/// endlessly — the runaway this structure exists to prevent).
/// Returns `(commands, frame_ticks, next_phase)`.
fn ascend_step(f: &FlightFrame, mu: f64, radius: f64) -> (Vec<SimCommand>, u64, FlightPhase) {
    let v_t = f.v_horiz.norm();
    let v_circ = libm::sqrt(mu / f.r);
    let dv = v_circ - v_t;
    let (r_a, r_p) = apsis(f.rel, f.vel, mu);
    let e = if r_a + r_p > 0.0 {
        (r_a - r_p) / (r_a + r_p)
    } else {
        1.0
    };
    let alt = f.r - radius;
    let t = ((alt - 500.0) / 54_500.0).clamp(0.0, 1.0);
    let pitch = (4.0 + 76.0 * t) * core::f64::consts::PI / 180.0;
    let mut commands = Vec::new();
    let mut frame_ticks = 64;
    let mut next = FlightPhase::Ascend;
    if e < 0.1 && r_p > radius + 65_000.0 && r_a < radius + 800_000.0 {
        commands.push(SimCommand::ToRails {
            vessel: FLIGHT_VESSEL,
        });
        next = FlightPhase::Orbit;
    } else {
        let (att_dir, throttle) = if r_a < radius + 75_000.0 {
            let (sp, cp) = (libm::sin(pitch), libm::cos(pitch));
            (f.up * cp + f.prograde * sp, 1.0)
        } else if f.v_r > 60.0 {
            // Still climbing toward apoapsis — pure coast.
            (f.prograde, 0.0)
        } else {
            // Near apo (v_r ≈ 0) or descending: the dv taper — at apo,
            // dv = v_circ(ra) − v_apo is exactly the circularization
            // deficit. Prograde raises periapsis, retrograde corrects
            // overshoot; the deadband cuts the residual creep that
            // inflated the orbit before.
            let dir = if dv >= 0.0 { f.prograde } else { -f.v_horiz };
            let thr = if dv.abs() < 5.0 {
                0.0
            } else {
                (dv.abs() / 30.0).clamp(0.0, 1.0)
            };
            (dir, thr)
        };
        commands.push(SimCommand::SetAttitude {
            vessel: FLIGHT_VESSEL,
            rot: thrust_attitude(att_dir),
            ang_vel: DVec3::ZERO,
        });
        commands.push(SimCommand::SetThrottle {
            vessel: FLIGHT_VESSEL,
            throttle,
        });
        if throttle <= 0.0 && r_a > radius + 75_000.0 {
            // Ballistic climb to apoapsis — nothing changes.
            frame_ticks = 4096;
        }
    }
    (commands, frame_ticks, next)
}

/// The Descend phase's command block: reentry-speed bookkeeping for
/// the report, plus the terminal retro burn below 3 km that makes the
/// landing a landing, not a crater. Returns `(commands, frame_ticks)` —
/// bigger jumps while the vessel is far above the brake gate.
fn descend_step(
    f: &FlightFrame,
    env: &Environment,
    atm_top: f64,
    report: &mut FlightReport,
) -> (Vec<SimCommand>, u64) {
    let alt = f.r - env.radius_m;
    if alt < atm_top && f.vel.norm() > report.max_reentry_speed_mps {
        report.max_reentry_speed_mps = f.vel.norm();
    }
    let v_rel = f.vel - env.body_spin.cross(f.rel);
    let frame_ticks = if alt > 5_000.0 && v_rel.norm() < 60.0 {
        4096
    } else if alt > 3_000.0 {
        256
    } else {
        64
    };
    let mut commands = Vec::new();
    if alt < 3_000.0 && v_rel.norm() > 60.0 {
        commands.push(SimCommand::SetAttitude {
            vessel: FLIGHT_VESSEL,
            rot: thrust_attitude(-v_rel),
            ang_vel: DVec3::ZERO,
        });
        commands.push(SimCommand::SetThrottle {
            vessel: FLIGHT_VESSEL,
            throttle: 1.0,
        });
    } else {
        commands.push(SimCommand::SetThrottle {
            vessel: FLIGHT_VESSEL,
            throttle: 0.0,
        });
    }
    (commands, frame_ticks)
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers the
// shipping fallible paths.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn fixture_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../data/test-fixtures/flight-launch")
    }

    /// The M2 acceptance arc in the sim: the authored craft launches,
    /// reaches a real orbit (periapsis above the 70 km atmosphere),
    /// re-materializes, reenters (measurable drag-side speed), and
    /// lands into a surface latch.
    #[test]
    fn flight_arc_launch_orbit_reentry_land() {
        let mut w = flight_initial(&fixture_dir()).unwrap();
        let report = flight_run(&mut w).unwrap();
        let el = report.orbit_elements.expect("never reached rails");
        let r = 6.000e5;
        let a = el.semi_major_axis_m;
        let rp = a * (1.0 - el.eccentricity);
        let ra = a * (1.0 + el.eccentricity);
        // A real orbit: periapsis above the atmosphere, apoapsis
        // bounded (the controller targets ~75 km).
        assert!(rp > r + 70_000.0, "periapsis {}", rp - r);
        assert!(ra < r + 200_000.0, "apoapsis {}", ra - r);
        // Reentry happened — atmospheric speed on descent.
        assert!(report.max_reentry_speed_mps > 100.0);
        // M4b: the heat pulse was real — the part rose well above
        // ambient but stayed under its authored 1100 K max_temp.
        // (The fixture reenters at ~2 km/s; the authored craft is a
        // survivable one, not a lethal one.)
        assert!(
            report.max_part_temp_k > 300.0,
            "no reentry heating observed: {} K",
            report.max_part_temp_k
        );
        eprintln!(
            "flight reentry peak part temp: {:.1} K",
            report.max_part_temp_k
        );
        assert!(
            report.max_part_temp_k < 1_100.0,
            "authored reentry cooked the part: {} K",
            report.max_part_temp_k
        );
        assert!(report.latch_tick.is_some());
        // M7-C: the latched vessel was recovered at the launch site —
        // out of the arena, its part value paid out at efficiency
        // 1.0, and its (empty) tanks credited nothing. A 50 000-fund
        // craft minus the 50 000-fund manufacture charge: the run's
        // net funds move is zero round-trip minus nothing, so
        // recovery_micros = the full cost back.
        let micros = report.recovery_micros.expect("vessel never recovered");
        assert!(micros > 0);
        assert!(!w.vessels().contains_key(&FLIGHT_VESSEL));
    }

    /// §5.3's per-tick budget — the honest measurement, not a
    /// guarantee: mean `SimpleBackend::step` cost for a ~200-part
    /// welded vessel, free-flight and in-contact. The release bound
    /// pins REF-A's 0.5 ms/tick; the debug bound is deliberately loose
    /// — this machine's number is reported, not hidden.
    #[test]
    fn measure_step_cost_200_part() {
        use csp_phys::{PhysicsBackend, SimpleBackend};

        // A ~200-part craft: a 10×20 tower of box parts.
        let mut graph = PartGraph::new(PartNodeId(0));
        let mut parts = Vec::new();
        for i in 0..200u32 {
            let id = PartNodeId(u64::from(i));
            let parent = if i == 0 {
                None
            } else {
                Some(PartNodeId(u64::from(i - 1)))
            };
            graph.nodes.insert(
                id,
                PartNode::new(
                    parent,
                    PartTransform::new(
                        DVec3::new(0.0, 0.0, if i == 0 { 0.0 } else { 0.25 }),
                        DQuat::IDENTITY,
                    ),
                    i,
                ),
            );
            parts.push(WeldPart {
                experiments: Vec::new(),
                lab: None,
                engine: None,
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
            });
        }
        let mut body = csp_phys::weld(&graph, &parts).unwrap();
        assert_eq!(body.collider.hulls.len(), 200);
        let env = env_for_measure();
        let ticks = 512u64;
        // Free flight (no contacts).
        body.pos = DVec3::new(env.radius_m + 100_000.0, 0.0, 0.0);
        body.vel = DVec3::new(0.0, libm::sqrt(env.mu_m3_per_s2 / body.pos.norm()), 0.0);
        let mut backend = SimpleBackend::new(env.clone());
        let t0 = std::time::Instant::now();
        for _ in 0..ticks {
            backend.step(std::slice::from_mut(&mut body), &[]);
        }
        let free = t0.elapsed();
        // In contact: resting on the ground.
        body.pos = DVec3::new(env.radius_m + 3.1, 0.0, 0.0);
        body.rot = DQuat::from_axis_angle(DVec3::Y, core::f64::consts::FRAC_PI_2);
        body.vel = env.body_spin.cross(body.pos);
        body.settle = Default::default();
        let mut backend = SimpleBackend::new(env);
        let t0 = std::time::Instant::now();
        for _ in 0..ticks {
            backend.step(std::slice::from_mut(&mut body), &[]);
        }
        let contact = t0.elapsed();
        let free_us = free.as_secs_f64() * 1e6 / ticks as f64;
        let contact_us = contact.as_secs_f64() * 1e6 / ticks as f64;
        eprintln!(
            "200-part step cost: free={free_us:.0} µs/tick, contact={contact_us:.0} µs/tick ({:?} build)",
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
        );
        if cfg!(debug_assertions) {
            assert!(free_us < 50_000.0 && contact_us < 50_000.0);
        } else {
            // REF-A budget: ≤0.5 ms/tick for a 200-part vessel.
            assert!(
                free_us < 500.0 && contact_us < 500.0,
                "over REF-A budget: free={free_us}µs contact={contact_us}µs"
            );
        }
    }

    /// §2.4.1's REF-A target: the Stage-2 voxelize + 66-direction
    /// sweep for a ~300-part craft completes well under 100 ms in
    /// release (the editor install path is off-tick; this pins the
    /// cost so it stays there). The debug bound is deliberately loose
    /// — the machine's number is printed either way.
    #[test]
    fn measure_voxelize_300_part() {
        let mut graph = PartGraph::new(PartNodeId(0));
        let mut parts = Vec::new();
        for i in 0..300u32 {
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
            parts.push(WeldPart {
                experiments: Vec::new(),
                lab: None,
                engine: None,
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
                aero: csp_data::AeroDef::constant(0.8, 0.2, 1.0),
                thermal: csp_data::ThermalDef {
                    heat_capacity_j_per_k: 400_000.0,
                    max_temp_k: 1_100.0,
                    emissivity: 0.8,
                    conduction_w_per_k: 60.0,
                },
            });
        }
        let body = csp_phys::weld(&graph, &parts).unwrap();
        let t0 = std::time::Instant::now();
        let tables =
            csp_phys::build_aero_tables(&body, &graph, &parts, csp_phys::ConfigMask::ALL, 0)
                .unwrap();
        let elapsed = t0.elapsed();
        assert_eq!(tables.parts.len(), 300);
        assert!(tables.cd_knots.iter().all(|v| v.is_finite()));
        let ms = elapsed.as_secs_f64() * 1000.0;
        eprintln!(
            "voxelize+tables 300 parts: {ms:.1} ms (edge {:.3} m)",
            tables.voxel_edge_m
        );
        if cfg!(debug_assertions) {
            assert!(ms < 30_000.0, "debug voxelize {ms} ms");
        } else {
            // REF-A's <100 ms for the exit-row case.
            assert!(ms < 100.0, "voxelize {ms} ms over 100 ms target");
        }
    }

    /// The measurement env — Terrin's numbers (non-spinning: a
    /// free-flight cost isolate). Terrain is detached: the "in
    /// contact" half poses its body at `radius + 3.1` — a seeded
    /// ±amplitude surface would either float it kilometres or bury
    /// it, and neither measures the resting-solve cost the fixture
    /// is named for.
    fn env_for_measure() -> Environment {
        let mut env = Environment::for_body(
            &csp_data::BodyDef {
                schema_version: 1,
                id: BodyId::from("terrin"),
                display_name: csp_data::LocKey::from("t"),
                parent: None,
                radius_m: 6.000e5,
                mu_m3_per_s2: 3.5316e12,
                soi_m: None,
                rotation_period_s: None,
                tilt_rad: 0.0,
                orbit: None,
                atmosphere: None,
                ocean: None,
                terrain: csp_data::TerrainDef {
                    noise_seed: 0,
                    heightmap: None,
                },
                biome_map: None,
                surface_resources: Vec::new(),
                insolation_w_m2: None,
            },
            DVec3::ZERO,
            None,
        );
        env.terrain = None;
        env
    }

    /// Osculating `(apo, peri)` ALTITUDES of `FLIGHT_VESSEL` — the
    /// orbit-check the autopilot/script tests assert against.
    fn vessel_apsides(w: &World, env: &Environment) -> Option<(f64, f64)> {
        let v = w.vessels().get(&FLIGHT_VESSEL)?;
        let (rel, vel, mu) = match &v.state {
            VesselState::InBubble(b) => (b.com_world() - env.body_center, b.vel, env.mu_m3_per_s2),
            VesselState::OnRails(o) => {
                let mu = w.orbits().body_orbit(&o.parent).ok()?.mu_m3_per_s2;
                let (r, v) = csp_sim::orbit::OrbitModel::state_at(w.orbits(), o, w.clock()).ok()?;
                (r, v, mu)
            }
            VesselState::SurfaceLatched(_) => return None,
        };
        let (ra, rp) = apsis(rel, vel, mu);
        Some((ra - env.radius_m, rp - env.radius_m))
    }

    /// M9a §2.10/§2.11 acceptance: `EngageAutopilot{Ascent}` on the
    /// authored launch craft drives it to a closed orbit entirely
    /// above the atmosphere — the built-in Rust autopilot flying the
    /// same `ControlApi` a script would.
    #[test]
    fn autopilot_ascent_reaches_orbit() {
        let mut w = flight_initial(&fixture_dir()).unwrap();
        let env = spawn_pad(&mut w).unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::EngageAutopilot {
                vessel: FLIGHT_VESSEL,
                program: csp_sim::AutopilotProgram::Ascent {
                    target_apoapsis_m: 120_000.0,
                    target_periapsis_m: 100_000.0,
                },
            },
        );
        w.step_frame(2).unwrap();
        let mut reached = None;
        for _ in 0..600_000u64 {
            let t = w.clock().ticks();
            w.step_frame(t + 1).unwrap();
            if let Some((apo, pe)) = vessel_apsides(&w, &env) {
                // Orbit achieved: periapsis comfortably above the
                // atmosphere, apoapsis in the target band.
                if pe > env.atmosphere.as_ref().map_or(70_000.0, |a| a.top_m) && apo > 100_000.0 {
                    reached = Some((apo, pe));
                    break;
                }
            }
        }
        let (apo, pe) = reached.expect("autopilot never reached orbit");
        assert!(apo < 400_000.0, "runaway apoapsis {apo}");
        eprintln!("autopilot orbit: apo {apo:.0} pe {pe:.0}");
        assert!(w.autopilots()[&FLIGHT_VESSEL].enabled);
    }

    /// M9a §2.10 acceptance: a real Rhai ascent script — `on_tick`
    /// pitch program over `persist` state + `ctx.vessel` telemetry —
    /// drives the same craft above the target apoapsis with a
    /// throttle cut, proving the script path (telemetry read →
    /// command → next-tick drain → physics).
    #[test]
    fn rhai_ascent_script_reaches_apoapsis() {
        const SRC: &str = r#"
            fn on_tick(ctx) {
                let v = ctx.vessel;
                if !v.found { return; }
                let apo = v.apoapsis;
                if apo != apo { apo = -1.0; } // NaN → still climbing
                if apo >= 120000.0 {
                    ctx.persist["done"] = true;
                    set_throttle(0.0);
                    return;
                }
                ctx.persist["done"] = false;
                set_throttle(1.0);
                let alt = v.altitude;
                // pitch: vertical below 1.5 km, horizontal by 60 km
                let f = ((alt - 1500.0) / 58500.0).max(0.0).min(1.0);
                let pitch = (1.5707963 * (1.0 - f));
                let up = v.pos;
                let un = up.x * up.x + up.y * up.y + up.z * up.z;
                un = un.sqrt();
                let ux = up.x / un; let uy = up.y / un; let uz = up.z / un;
                let vv = v.vel;
                let vr = (vv.x * ux + vv.y * uy + vv.z * uz);
                let hx = vv.x - vr * ux; let hy = vv.y - vr * uy; let hz = vv.z - vr * uz;
                let hn = (hx * hx + hy * hy + hz * hz).sqrt();
                let px = 0.0; let py = 0.0; let pz = 0.0;
                if hn > 0.001 { px = hx / hn; py = hy / hn; pz = hz / hn; }
                else { px = 1.0 - ux.abs(); py = 0.0; pz = uz; }
                let cp = pitch.cos(); let sp = pitch.sin();
                steer(ux * sp + px * cp, uy * sp + py * cp, uz * sp + pz * cp);
            }
        "#;
        let mut w = flight_initial(&fixture_dir()).unwrap();
        let env = spawn_pad(&mut w).unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::LoadScript {
                vessel: FLIGHT_VESSEL,
                source: SRC.to_string(),
            },
        );
        w.step_frame(2).unwrap();
        assert!(
            w.scripts()[&csp_sim::ScriptId(0)].enabled,
            "script must compile"
        );
        let mut done = false;
        for _ in 0..400_000u64 {
            let t = w.clock().ticks();
            w.step_frame(t + 1).unwrap();
            let s = &w.scripts()[&csp_sim::ScriptId(0)];
            assert!(s.enabled, "script faulted: {:?}", s.disabled_reason);
            if matches!(
                s.persist.get("done"),
                Some(csp_sim::PersistValue::Bool(true))
            ) {
                done = true;
                break;
            }
        }
        assert!(done, "script never cut throttle at target apoapsis");
        let (apo, _pe) = vessel_apsides(&w, &env).expect("no conic");
        assert!(apo >= 120_000.0, "apoapsis {apo}");
    }
}
