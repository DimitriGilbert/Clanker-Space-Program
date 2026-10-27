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
//! - **Phase 4 — done.** Throttle 0 and self-disengage: an engaged
//!   record suppresses the auto bubble→rails edge (the sim's
//!   candidate loop skips a live control producer), so a *completed*
//!   ascent hands the vessel back — the disengage drains next tick
//!   and a coasting, clearing craft rails that same tick.
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

        // Open-conic abort (C26-F6): a solvable conic with no apoapsis
        // is an escape trajectory — there is no apsis to steer for and
        // the target can never be reached, so every powered phase
        // would burn blind (engaging on an already-escaping vessel
        // left phase 1 at throttle 1 forever, the live record
        // suppressing auto-rails the whole way out). Cut the burn and
        // hand the vessel back; the caller can re-engage once the
        // conic closes. `elements: None` is deliberately NOT an abort:
        // a near-radial state (a non-rotating pad's first liftoff
        // ticks) is unsolvable only until the burn grows `h`, and a
        // transient unsolvable blip must not kill a healthy ascent.
        if t.elements.is_some() && t.apoapsis_altitude_m.is_none() {
            api.set_throttle(0.0);
            api.disengage_autopilot();
            return Ok(());
        }

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
                // No decaying-apoapsis drop-back here: reaching phase 2
                // at all required a closed conic (apo `Some`), and
                // `osculating` resolves a future apsis for every closed
                // conic — the old apo-Some ∧ t_apo-None guard could
                // never fire. The real strand (an open conic) is the
                // abort above the match.
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
            // Done — cut the burn and hand the vessel back. The record
            // cannot sit `enabled` here: the auto bubble→rails edge
            // skips any vessel with a live autopilot (a control
            // producer must not be railed mid-flight), so a parked
            // record would hold the finished craft in the bubble
            // forever — no conic, no map prediction. The disengage
            // drains next tick; a coasting, clearing vessel rails on
            // that same tick.
            _ => {
                api.set_throttle(0.0);
                api.disengage_autopilot();
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
            // `source_id` is a u32 channel key, not the full vessel id
            // (see `CommandKey::source_id`): ids above `u32::MAX`
            // share the `(Autopilot, u32::MAX)` channel — one pending
            // FIFO, one `source_seq` — never a smaller id's channel.
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

#[cfg(test)]
// `unwrap`/`expect` in tests is the failure-reporting idiom, not a
// fallible-path determinism risk — the workspace lint's sanctioned
// narrow allow (the csp-sim test-module precedent).
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use csp_core::SimError;
    use csp_data::{BodyId, KeplerElements};
    use csp_sim::orbit::{BodyOrbit, PatchedConics};
    use csp_sim::{
        CraftDef, DEFAULT_BUBBLE_RADIUS_M, DEFAULT_MAX_EVENTS_PER_FRAME, PhysicsConfig, ScriptId,
        SimCommand, TelemetryResidence, VesselState, VesselTelemetry, WakeCondition, World,
    };
    use std::collections::BTreeMap;

    /// Terrin's §3.0 numbers (the csp-sim fixture's constants) — a
    /// single-body bubble world, airless so the rail floor is the
    /// 20 km bubble radius rather than an atmosphere top.
    const MU: f64 = 3.5316e12;
    const RADIUS: f64 = 600_000.0;
    const VID: VesselId = VesselId(0);

    // ------------------------- driver-level -------------------------

    /// A `TickIo` that returns a fixed telemetry snapshot and records
    /// every issued command — the driver's command stream is the unit
    /// under test.
    struct StubIo {
        telemetry: VesselTelemetry,
        issued: Vec<SimCommand>,
    }

    impl TickIo for StubIo {
        fn telemetry(&self, _vessel: VesselId) -> VesselTelemetry {
            self.telemetry.clone()
        }
        fn vessels(&self) -> Vec<VesselId> {
            vec![VID]
        }
        fn issue(&mut self, _class: SourceClass, _source_id: u32, command: SimCommand) {
            self.issued.push(command);
        }
        fn request_wake(&mut self, _s: ScriptId, _c: &WakeCondition) -> Result<u64, SimError> {
            Err(SimError::Recoverable("stub io: no wakes"))
        }
        fn cancel_wake(&mut self, _s: ScriptId) -> Result<(), SimError> {
            Err(SimError::Recoverable("stub io: no wakes"))
        }
        fn log_line(&mut self, _s: ScriptId, _line: &str) {}
    }

    /// A ~200 km × 170 km closed conic — apoapsis above the 120 km
    /// target, periapsis above the 100 km target.
    fn closed_elements() -> KeplerElements {
        KeplerElements {
            semi_major_axis_m: 7.85e5,
            eccentricity: 0.02,
            inclination_rad: 0.0,
            ascending_node_rad: 0.0,
            arg_periapsis_rad: 0.0,
            mean_anomaly_epoch_rad: 0.0,
            period_s: 2_300.0,
        }
    }

    /// An escape conic — `apoapsis` is `None` on `e ≥ 1`.
    fn escape_elements() -> KeplerElements {
        KeplerElements {
            semi_major_axis_m: -1.0e6,
            eccentricity: 1.5,
            inclination_rad: 0.0,
            ascending_node_rad: 0.0,
            arg_periapsis_rad: 0.0,
            mean_anomaly_epoch_rad: 0.0,
            period_s: f64::INFINITY,
        }
    }

    /// A minimal in-bubble snapshot — the fields `fly` reads carry
    /// the conic's answers; the rest are neutral defaults.
    fn telemetry(elements: Option<KeplerElements>) -> VesselTelemetry {
        let closed = elements.as_ref().is_some_and(|e| e.eccentricity < 1.0);
        VesselTelemetry {
            tick: 0,
            found: true,
            residence: TelemetryResidence::InBubble,
            pos_m: DVec3::new(RADIUS + 200_000.0, 0.0, 0.0),
            vel_mps: DVec3::new(0.0, 2_000.0, 0.0),
            rot: csp_core::DQuat::IDENTITY,
            ang_vel_mps: DVec3::ZERO,
            throttle: 0.0,
            altitude_m: Some(200_000.0),
            speed_mps: 2_000.0,
            vertical_speed_mps: 0.0,
            horizontal_speed_mps: 2_000.0,
            mass_kg: Some(1_000.0),
            apoapsis_altitude_m: closed.then_some(200_000.0),
            periapsis_altitude_m: closed.then_some(170_000.0),
            time_to_apoapsis_s: closed.then_some(0.0),
            time_to_periapsis_s: closed.then_some(1_000.0),
            mu_m3_per_s2: Some(MU),
            body: Some(BodyId::from("terrin")),
            elements,
            maneuver_node: None,
            resources: BTreeMap::new(),
            parts: Vec::new(),
        }
    }

    /// An `AutopilotState` parked at `phase` — the persist map IS the
    /// phase store, so fixtures set it directly.
    fn ascent_state(phase: i64) -> AutopilotState {
        let mut persist = PersistMap::new();
        persist.insert("phase".to_string(), PersistValue::Int(phase));
        AutopilotState {
            program: AutopilotProgram::Ascent {
                target_apoapsis_m: 120_000.0,
                target_periapsis_m: 100_000.0,
            },
            enabled: true,
            persist,
        }
    }

    /// C26-F2: a completed ascent disengages itself — the phase-4 arm
    /// issues the throttle cut AND `DisengageAutopilot` (the verb was
    /// always on the driver's `IoControlApi`; it was simply never
    /// called). A record parked `enabled` would suppress the auto
    /// bubble→rails edge forever.
    #[test]
    fn completion_disengages() {
        let mut state = ascent_state(4);
        let mut io = StubIo {
            telemetry: telemetry(Some(closed_elements())),
            issued: Vec::new(),
        };
        AscentAutopilot::new()
            .run_tick(VID, &mut state, 0, &mut io)
            .unwrap();
        assert!(
            io.issued.iter().any(|c| matches!(
                c,
                SimCommand::SetThrottle { throttle, .. } if *throttle == 0.0
            )),
            "phase 4 must cut the throttle: {:?}",
            io.issued
        );
        assert!(
            io.issued
                .iter()
                .any(|c| matches!(c, SimCommand::DisengageAutopilot { .. })),
            "phase 4 must self-disengage: {:?}",
            io.issued
        );
    }

    /// C26-F6: engaging on an already-escaping conic (apo `None` from
    /// tick one — solvable `e ≥ 1` elements) must not burn at
    /// throttle 1 forever: the first tick cuts the burn and
    /// disengages, and nothing else issues.
    #[test]
    fn open_conic_engagement_aborts() {
        let mut state = ascent_state(0);
        let mut io = StubIo {
            telemetry: telemetry(Some(escape_elements())),
            issued: Vec::new(),
        };
        AscentAutopilot::new()
            .run_tick(VID, &mut state, 0, &mut io)
            .unwrap();
        assert!(
            !io.issued.iter().any(|c| matches!(
                c,
                SimCommand::SetThrottle { throttle, .. } if *throttle > 0.0
            )),
            "an open conic must never command a burn: {:?}",
            io.issued
        );
        assert!(
            io.issued
                .iter()
                .any(|c| matches!(c, SimCommand::DisengageAutopilot { .. })),
            "an open conic must disengage: {:?}",
            io.issued
        );
    }

    /// The abort's deliberate carve-out: an UNSOLVABLE conic
    /// (`elements: None` — a non-rotating pad's near-radial first
    /// liftoff ticks) is not an escape. The phase machine keeps
    /// flying; a transient unsolvable blip must not kill an ascent.
    #[test]
    fn unsolvable_conic_does_not_abort() {
        let mut state = ascent_state(0);
        let mut io = StubIo {
            telemetry: telemetry(None),
            issued: Vec::new(),
        };
        AscentAutopilot::new()
            .run_tick(VID, &mut state, 0, &mut io)
            .unwrap();
        assert!(
            io.issued.iter().any(|c| matches!(
                c,
                SimCommand::SetThrottle { throttle, .. } if *throttle == 1.0
            )),
            "phase 0 still flies a solvable-soon conic: {:?}",
            io.issued
        );
        assert!(
            !io.issued
                .iter()
                .any(|c| matches!(c, SimCommand::DisengageAutopilot { .. })),
            "an unsolvable conic must not disengage: {:?}",
            io.issued
        );
    }

    // ------------------------- world-level --------------------------

    /// A one-body system model — Terrin alone, no SOI boundary.
    fn terrin_orbits() -> PatchedConics {
        let mut bodies = BTreeMap::new();
        bodies.insert(
            BodyId::from("terrin"),
            BodyOrbit {
                mu_m3_per_s2: MU,
                soi_m: None,
                parent: None,
                elements: None,
            },
        );
        let mut orbits = PatchedConics::new(bodies);
        orbits.set_body_radius(&BodyId::from("terrin"), RADIUS);
        orbits
    }

    /// A physics world around `terrin` with the real driver installed
    /// — the same wiring `csp-bench`'s flight fixture uses.
    fn terrin_world() -> World {
        let mut w = World::with_physics(
            terrin_orbits(),
            PhysicsConfig {
                environment: csp_phys::Environment {
                    body_center: DVec3::ZERO,
                    radius_m: RADIUS,
                    mu_m3_per_s2: MU,
                    body_spin: DVec3::ZERO,
                    // Airless: no drag to perturb the fixtures and the
                    // rail floor is the bubble radius (620 km).
                    atmosphere: None,
                    settle_v: 0.05,
                    settle_w: 0.05,
                    settle_ticks: 128,
                    friction_mu: 0.5,
                    drag_cd: 1.0,
                    terrain: None,
                },
                dominant: BodyId::from("terrin"),
                bubble_radius_m: DEFAULT_BUBBLE_RADIUS_M,
            },
            DEFAULT_MAX_EVENTS_PER_FRAME,
        );
        w.set_autopilot_driver(Box::new(AscentAutopilot::new()));
        w
    }

    /// A 1 t one-part box with no engine — these tests exercise the
    /// driver's command stream and the rails bookkeeping, not thrust.
    fn test_craft() -> CraftDef {
        let mut graph = csp_phys::PartGraph::new(csp_phys::PartNodeId(0));
        graph.nodes.insert(
            csp_phys::PartNodeId(0),
            csp_phys::PartNode::new(None, csp_phys::PartTransform::IDENTITY, 0),
        );
        CraftDef {
            graph,
            parts: vec![csp_phys::WeldPart {
                experiments: Vec::new(),
                lab: None,
                engine: None,
                parachute: None,
                dock_ports: vec![],
                tanks: vec![],
                crew_seats: 0,
                structural: csp_data::StructuralDef::unlimited(),
                mass_kg: 1_000.0,
                com: DVec3::ZERO,
                inertia: csp_phys::SymMat3::solid_box(1_000.0, DVec3::new(0.5, 0.5, 0.5)),
                collider: csp_phys::ColliderShape::Box {
                    half_extents: DVec3::new(0.5, 0.5, 0.5),
                },
                force_sites: vec![],
                aero: csp_data::AeroDef::constant(1.0, 0.0, 1.0),
                thermal: csp_data::ThermalDef {
                    heat_capacity_j_per_k: 400_000.0,
                    max_temp_k: 1_100.0,
                    emissivity: 0.8,
                    conduction_w_per_k: 60.0,
                },
            }],
            part_costs: vec![0.0],
            reliability: vec![],
            part_ids: vec![],
            hash_memo: Default::default(),
        }
    }

    /// Spawn at `pos`/`vel` and engage the ascent program in the same
    /// drain — both commands ride the (Ui, 0) FIFO, so the record
    /// exists before the tick's auto-rails edge evaluates (an enabled
    /// record is the only thing holding a clearing coaster in the
    /// bubble until the driver finishes).
    fn spawn_and_engage(w: &mut World, pos: DVec3, vel: DVec3) {
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SpawnPhysicalVessel {
                craft: test_craft(),
                pos,
                rot: csp_core::DQuat::IDENTITY,
                vel,
                ang_vel: DVec3::ZERO,
            },
        );
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::EngageAutopilot {
                vessel: VID,
                program: AutopilotProgram::Ascent {
                    target_apoapsis_m: 120_000.0,
                    target_periapsis_m: 100_000.0,
                },
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
    }

    /// C03-F2/C26-F2 end-to-end: the phase machine walks 0→4 on a
    /// coasting clearing conic (every phase exits on its first check),
    /// self-disengages, and the un-suppressed auto bubble→rails edge
    /// rails the vessel — the verified recovery path once the record
    /// is gone.
    #[test]
    fn completion_disengages_and_auto_rails() {
        let mut w = terrin_world();
        // A ~200 km × 170 km conic ~4 min short of apoapsis: r_p clears
        // the 620 km rail floor, apo and pe both exceed the targets.
        let pos = DVec3::new(RADIUS + 199_880.0, 0.0, 0.0);
        let vel = DVec3::new(27.0, 2_081.2, 0.0);
        spawn_and_engage(&mut w, pos, vel);
        // The engaged record is the rails suppression under test.
        assert!(w.autopilots().contains_key(&VID));
        assert!(matches!(w.vessels()[&VID].state, VesselState::InBubble(_)));
        let mut railed = false;
        // One full conic period (~2 326 s ≈ 149k ticks) bounds the
        // worst-case phase-2 coast; the actual run is far shorter.
        for _ in 0..200_000u64 {
            let t = w.clock().ticks();
            w.step_frame(t + 1).unwrap();
            if w.vessels()[&VID].orbit().is_some() {
                railed = true;
                break;
            }
        }
        assert!(railed, "completed ascent never auto-railed");
        assert!(
            !w.autopilots().contains_key(&VID),
            "a completed ascent must remove its autopilot record"
        );
    }

    /// C26-F6 end-to-end: an already-escaping vessel (e ≈ 1.5
    /// inbound, conic periapsis below the rail floor so nothing rails
    /// it early) disengages on the driver's FIRST tick — the throttle
    /// never rises above 0, where the unfixed driver burned forever.
    #[test]
    fn escape_engagement_aborts_without_burning() {
        let mut w = terrin_world();
        // Inbound hyperbola, r_p ≈ 500 km — below the 620 km floor so
        // the vessel stays InBubble until impact; the abort's drain is
        // the only thing that removes the record.
        let pos = DVec3::new(RADIUS + 200_000.0, 0.0, 0.0);
        let vel = DVec3::new(-2_337.0, 2_626.0, 0.0);
        spawn_and_engage(&mut w, pos, vel);
        // Engaged at drain; the abort issued on the same tick's
        // control pass reaches the queue for the next drain.
        assert!(w.autopilots().contains_key(&VID));
        for _ in 0..10u64 {
            let t = w.clock().ticks();
            w.step_frame(t + 1).unwrap();
            assert!(
                !w.autopilots().contains_key(&VID),
                "the abort must disengage at the next drain"
            );
            let body = w.vessels()[&VID].body().expect("vessel left the bubble");
            assert_eq!(body.throttle, 0.0, "the abort must never command a burn");
        }
    }
}
