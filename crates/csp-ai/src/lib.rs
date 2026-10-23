//! M9a §2.11 — the Rust ascent autopilot: the first-class
//! [`AutopilotDriver`] the sim drives through the same [`ControlApi`]
//! the Rhai host surfaces. Commands stamp `SourceClass::Autopilot`
//! (the plan's declared ordering: scripts first, autopilots second).
//!
//! # The guidance
//!
//! A simple proportional gravity-turn program — M9a scopes this to
//! launch-to-orbit; PEG/landing/rendezvous are later milestones. The
//! phase machine lives in `state.persist` so a save mid-ascent
//! resumes it:
//!
//! - **Phase 0 — liftoff.** Throttle 1, steer surface radial
//!   (`r̂`) until [`TURN_START_ALT_M`].
//! - **Phase 1 — gravity turn.** Pitch over continuously from
//!   vertical at [`TURN_START_ALT_M`] to horizontal at
//!   [`TURN_END_ALT_M`], steering the linear `up↔surface-prograde`
//!   blend — the classic open-loop pitch program.
//! - **Phase 2 — coast.** At the target apoapsis the engine cuts; the
//!   craft coasts to `time_to_apoapsis <= CIRC_LEAD_S` of the apsis.
//! - **Phase 3 — circularize.** Burn prograde until the periapsis
//!   reaches the target altitude (the closed-loop terminal burn —
//!   throttled against the remaining correction).
//! - **Phase 4 — done.** Throttle 0; the record stays until
//!   `DisengageAutopilot`.
//!
//! Every control request is a [`SimCommand`] via [`IoControlApi`] —
//! the autopilot has no privileged state access (§2.10: "nothing
//! bypasses the queue").

use csp_core::DVec3;
use csp_script::{ControlApi, IoControlApi};
use csp_sim::{
    AutopilotDriver, AutopilotProgram, AutopilotState, PersistMap, PersistValue, SourceClass,
    TickIo, VesselId,
};

/// Altitude where the pitch program leaves vertical, m.
const TURN_START_ALT_M: f64 = 1_500.0;
/// Altitude where the pitch program reaches horizontal, m.
const TURN_END_ALT_M: f64 = 60_000.0;
/// Lead time before apoapsis at which the circularization burn
/// starts, s (the cheap stand-in for the burn-time estimate — M9a's
/// "simple proportional guidance" is allowed to be simple).
const CIRC_LEAD_S: f64 = 12.0;
/// Throttle shaping near the circularization target — below this
/// remaining periapsis gap the burn throttles down proportional to
/// the gap (keeps the terminal approach honest, avoids overshoot on
/// low-TWR craft).
const CIRC_THROTTLE_SHAPE_M: f64 = 2_000.0;
/// Apoapsis overshoot guard — well above target the craft stops
/// burning (closed-loop honesty: an already-too-high orbit is not
/// "fixed" by more prograde).
const APO_OVERSHOOT_M: f64 = 5_000.0;

/// M9a's first-class ascent program — `program()` matches the
/// engaged `Ascent` record (parameters read from the record itself,
/// not the driver's copy).
pub struct AscentAutopilot;

impl AscentAutopilot {
    /// Construct the driver — stateless; all run state lives in
    /// `AutopilotState::persist` (the determinism rule).
    pub fn new() -> Self {
        Self
    }

    /// The phase machine — one tick of guidance. `state.persist` is
    /// the phase store: `phase` INT 0..=4.
    fn fly(
        api: &mut dyn ControlApi,
        persist: &mut PersistMap,
        target_apo: f64,
        target_pe: f64,
    ) -> Result<(), String> {
        let t = api.telemetry();
        if !t.found {
            return Err("autopilot: vessel telemetry missing".to_string());
        }
        let phase = persist_i64(persist, "phase", 0);
        let alt = t.altitude_m.unwrap_or(0.0);
        let up = if t.pos_m.norm() > 0.0 {
            t.pos_m / t.pos_m.norm()
        } else {
            DVec3::Y
        };
        // Surface prograde — the horizontal (downrange) velocity
        // component; at liftoff there is none, so fall back to any
        // axis perpendicular to `up` for a stable blend.
        let vh = t.vel_mps - up * t.vel_mps.dot(up);
        let prograde = if vh.norm() > 1.0e-3 {
            vh / vh.norm()
        } else if up.y.abs() < 0.9 {
            up.cross(DVec3::Y)
        } else {
            up.cross(DVec3::X)
        };
        let steer_up_forward = |pitch: f64| {
            // `pitch` rad above horizon: dir = cos·prograde + sin·up.
            up * libm::sin(pitch) + prograde * libm::cos(pitch)
        };

        match phase {
            // Liftoff: straight up to TURN_START_ALT_M.
            0 => {
                api.set_throttle(1.0);
                api.steer_dir(up);
                if alt >= TURN_START_ALT_M {
                    set_i64(persist, "phase", 1);
                }
            }
            // Gravity turn: pitch from vertical to horizontal over the
            // altitude window; keep burning until the target apoapsis
            // is reached.
            1 => {
                let f = ((alt - TURN_START_ALT_M) / (TURN_END_ALT_M - TURN_START_ALT_M))
                    .clamp(0.0, 1.0);
                let pitch = core::f64::consts::FRAC_PI_2 * (1.0 - f);
                api.set_throttle(1.0);
                api.steer_dir(steer_up_forward(pitch));
                match t.apoapsis_altitude_m {
                    Some(apo) if apo >= target_apo => {
                        api.set_throttle(0.0);
                        set_i64(persist, "phase", 2);
                    }
                    _ => {}
                }
            }
            // Coast to apoapsis — wait out the ballistic arc.
            2 => {
                api.set_throttle(0.0);
                // Keep the nose on prograde for the burn (the attitude
                // command is free to re-issue each tick).
                api.steer_dir(prograde);
                let lead = t.time_to_apoapsis_s.unwrap_or(f64::INFINITY);
                if lead <= CIRC_LEAD_S {
                    set_i64(persist, "phase", 3);
                }
                // Safety: a decaying apoapsis (never made it) drops
                // back to the burn phase.
                if t.apoapsis_altitude_m
                    .is_some_and(|a| a < target_apo - APO_OVERSHOOT_M)
                    && t.time_to_apoapsis_s.is_none()
                {
                    set_i64(persist, "phase", 1);
                }
            }
            // Circularize: prograde burn until periapsis ≥ target —
            // throttled against the remaining gap near the end.
            3 => {
                api.steer_dir(prograde);
                let pe = t.periapsis_altitude_m.unwrap_or(f64::NEG_INFINITY);
                let apo = t.apoapsis_altitude_m.unwrap_or(f64::INFINITY);
                if pe >= target_pe || apo > target_apo + APO_OVERSHOOT_M {
                    api.set_throttle(0.0);
                    set_i64(persist, "phase", 4);
                } else {
                    let gap = (target_pe - pe).max(0.0);
                    api.set_throttle((gap / CIRC_THROTTLE_SHAPE_M).clamp(0.05, 1.0));
                }
            }
            // Done — drift until disengaged.
            _ => {
                api.set_throttle(0.0);
            }
        }
        Ok(())
    }
}

impl Default for AscentAutopilot {
    fn default() -> Self {
        Self::new()
    }
}

impl AutopilotDriver for AscentAutopilot {
    fn program(&self) -> AutopilotProgram {
        AutopilotProgram::Ascent {
            target_apoapsis_m: 0.0,
            target_periapsis_m: 0.0,
        }
    }

    fn run_tick(
        &mut self,
        vessel: VesselId,
        state: &mut AutopilotState,
        _tick: u64,
        io: &mut dyn TickIo,
    ) -> Result<(), String> {
        let (target_apo, target_pe) = match &state.program {
            AutopilotProgram::Ascent {
                target_apoapsis_m,
                target_periapsis_m,
            } => (*target_apoapsis_m, *target_periapsis_m),
        };
        let mut api = IoControlApi {
            io,
            class: SourceClass::Autopilot,
            source_id: (vessel.0.min(u64::from(u32::MAX))) as u32,
            vessel,
            script: None,
        };
        Self::fly(&mut api, &mut state.persist, target_apo, target_pe)
    }
}

fn persist_i64(map: &PersistMap, key: &str, default: i64) -> i64 {
    match map.get(key) {
        Some(PersistValue::Int(v)) => *v,
        _ => default,
    }
}

fn set_i64(map: &mut PersistMap, key: &str, v: i64) {
    map.insert(key.to_string(), PersistValue::Int(v));
}
