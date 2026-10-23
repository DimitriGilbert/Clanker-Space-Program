//! M9d §2.12.1 — the eval suite: the agent-program registry and the
//! directory runner over `data/eval-scenarios/*.ron`.
//!
//! The scenario format's `Program{name, args}` source resolves
//! through [`program`] — the harness's library of feedback-control
//! agents. A program is the eval answer to "agents are not
//! declarable in data": a real controller is a telemetry-feedback
//! computation, so it lives in Rust (same trust level as the
//! recorder's scripted controller) while the *choice* of program,
//! its args, the world, and the assertions stay data.
//!
//! ## Registry
//!
//! - `"noop"` — withdraws immediately; the plumbing probe.
//! - `"ascent-apoapsis"` — the flagship: takes the §2.2.1 control
//!   lease, flies the pitch-program ascent (vertical off the pad,
//!   horizontal by ~60 km, the same profile the M9a Rhai test and
//!   the M2 phase machine fly), cuts throttle when the osculating
//!   apoapsis clears `target_apoapsis_m` (default 120 km), then
//!   withdraws. Args: `vessel` (default 0), `target_apoapsis_m`,
//!   `pitch_start_m`/`pitch_end_m` (the gravity-turn band, defaults
//!   1.5 km/60 km).
//!
//! [`run_suite`] walks a directory of `.ron` scenario files in
//! sorted order and runs each — the suite is additive-by-file: drop
//! a scenario in `data/eval-scenarios/` and the suite picks it up.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use csp_core::DVec3;
use csp_server::telemetry::TelemetrySnapshot;
use csp_sim::{Lease, SimCommand, VesselId};

use crate::agent::{Agent, AgentReply};
use crate::scenario::{EvalError, EvalReport};

/// Resolve a `Program{name, args}` source. `None` = unregistered
/// (the caller maps it to `EvalError::Setup` — an unknown program is
/// a scenario authoring error, not a run failure).
pub fn program(name: &str, args: &BTreeMap<String, f64>) -> Option<Box<dyn Agent>> {
    match name {
        "noop" => Some(Box::new(NoopAgent)),
        "ascent-apoapsis" => Some(Box::new(AscentAgent::new(args))),
        _ => None,
    }
}

/// The program registry's keys — for `tools`-style listing and typo
/// diagnostics.
pub const PROGRAMS: &[&str] = &["noop", "ascent-apoapsis"];

/// `"noop"` — `Done` on the first boundary.
struct NoopAgent;

impl Agent for NoopAgent {
    fn act(&mut self, _snap: &TelemetrySnapshot) -> AgentReply {
        AgentReply::Done
    }
}

/// `"ascent-apoapsis"` — the telemetry-feedback ascent controller.
///
/// State machine per boundary: `Lease` (take the §2.2.1 lease for
/// `client`) → `Climb` (pitch-program steering at full throttle) →
/// `Cut` (throttle 0 at target apoapsis) → `Done`. A vessel that
/// never appears is a `Wait` (setup's problem); a lease that never
/// lands after [`MAX_LEASE_TRIES`] attempts is a `Fail` — a held
/// lease is a run bug, not a retry forever.
struct AscentAgent {
    vessel: u64,
    target_apoapsis_m: f64,
    pitch_start_m: f64,
    pitch_end_m: f64,
    client: u32,
    lease_tries: u32,
    phase: AscentPhase,
}

/// The lease handshake's retry bound — the apply's holder-or-player
/// rule makes a stolen lease a permanent rejection.
const MAX_LEASE_TRIES: u32 = 8;

#[derive(Clone, Copy, PartialEq)]
enum AscentPhase {
    Lease,
    Climb,
    Cut,
}

impl AscentAgent {
    fn new(args: &BTreeMap<String, f64>) -> Self {
        let get = |k: &str, d: f64| args.get(k).copied().unwrap_or(d);
        Self {
            vessel: get("vessel", 0.0) as u64,
            target_apoapsis_m: get("target_apoapsis_m", 120_000.0),
            pitch_start_m: get("pitch_start_m", 1_500.0),
            pitch_end_m: get("pitch_end_m", 60_000.0),
            client: 0,
            lease_tries: 0,
            phase: AscentPhase::Lease,
        }
    }

    /// The pitch-program steering direction — `up·sin(pitch) +
    /// prograde·cos(pitch)` over the gravity-turn band, the same
    /// profile the M9a Rhai test drives. `None` = indeterminate
    /// frame (coasting at the origin — degenerate; hold).
    fn steer_dir(&self, pos: DVec3, vel: DVec3, alt: f64) -> DVec3 {
        let r = pos.norm();
        let up = if r > 0.0 { pos / r } else { DVec3::X };
        let v_r = vel.dot(up);
        let v_h = vel - up * v_r;
        let prograde = if v_h.norm() > 1e-3 {
            v_h / v_h.norm()
        } else {
            // No downrange component yet — pick any horizontal axis
            // orthogonal to `up` (the Rhai version's fallback).
            let axis = up.cross(DVec3::Y);
            if axis.norm() > 1e-9 {
                axis / axis.norm()
            } else {
                up.cross(DVec3::Z) / up.cross(DVec3::Z).norm()
            }
        };
        let f =
            ((alt - self.pitch_start_m) / (self.pitch_end_m - self.pitch_start_m)).clamp(0.0, 1.0);
        let pitch = core::f64::consts::FRAC_PI_2 * (1.0 - f);
        let (sp, cp) = (libm::sin(pitch), libm::cos(pitch));
        up * sp + prograde * cp
    }
}

impl Agent for AscentAgent {
    fn bind(&mut self, client: u32) {
        self.client = client;
    }

    fn act(&mut self, snap: &TelemetrySnapshot) -> AgentReply {
        let vessel = VesselId(self.vessel);
        let Some(v) = snap.vessels.get(&self.vessel) else {
            return AgentReply::Wait;
        };
        if !v.telemetry.found {
            return AgentReply::Wait;
        }
        match self.phase {
            AscentPhase::Lease => {
                if v.control_lease == Lease::Agent(self.client) {
                    self.phase = AscentPhase::Climb;
                    // Fall through next boundary — one action per
                    // lockstep boundary keeps the transcript honest.
                    return AgentReply::Act(vec![]);
                }
                self.lease_tries += 1;
                if self.lease_tries > MAX_LEASE_TRIES {
                    return AgentReply::Fail(format!(
                        "lease for vessel {} never granted",
                        vessel.0
                    ));
                }
                AgentReply::Act(vec![SimCommand::TakeControl {
                    vessel,
                    lease: Lease::Agent(self.client),
                }])
            }
            AscentPhase::Climb => {
                let apo = v.telemetry.apoapsis_altitude_m.unwrap_or(-1.0);
                if apo >= self.target_apoapsis_m {
                    self.phase = AscentPhase::Cut;
                    return AgentReply::Act(vec![SimCommand::SetThrottle {
                        vessel,
                        throttle: 0.0,
                    }]);
                }
                let pos = v.telemetry.pos_m;
                let vel = v.telemetry.vel_mps;
                let dir = self.steer_dir(pos, vel, v.telemetry.altitude_m.unwrap_or(0.0));
                AgentReply::Act(vec![
                    SimCommand::SetAttitude {
                        vessel,
                        rot: crate::scenarios::thrust_attitude(dir),
                        ang_vel: DVec3::ZERO,
                    },
                    SimCommand::SetThrottle {
                        vessel,
                        throttle: 1.0,
                    },
                ])
            }
            AscentPhase::Cut => AgentReply::Done,
        }
    }
}

/// One file's suite result — the parsed scenario's report, or the
/// load/run failure it produced. Kept whole (not early-exit) so the
/// suite reports every scenario's verdict in one pass.
#[derive(Debug)]
pub struct SuiteEntry {
    /// The `*.ron` path.
    pub path: PathBuf,
    /// The outcome — `Ok(report)` carries the verdict; `Err` the
    /// harness failure (a suite counts it as a failure).
    pub outcome: Result<EvalReport, EvalError>,
}

/// Run every `*.ron` in `dir` (sorted by file name — deterministic
/// suite order), returning one entry per file. `fixtures_root`
/// resolves `WorldSpec::Fixture{name}` (this repo's convention:
/// `data/test-fixtures/`).
pub fn run_suite(dir: &Path, fixtures_root: &Path) -> Result<Vec<SuiteEntry>, EvalError> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(EvalError::Io)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("ron"))
        .collect();
    files.sort();
    Ok(files
        .into_iter()
        .map(|path| {
            let outcome = crate::scenario::load_scenario(&path)
                .and_then(|f| crate::scenario::run_scenario(&f, fixtures_root));
            SuiteEntry { path, outcome }
        })
        .collect())
}
