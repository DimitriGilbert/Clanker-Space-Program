//! M4c near-flight view — PLAN §3.7's presentation floor over the
//! real sim: the InBubble craft (and its breakup debris) rendered as
//! hull boxes, a ground disc and horizon band for the planet, an
//! engine plume, a **reentry plasma sheath** driven by the sim's own
//! convective-heating flux sample, **ground-contact dust**, and the
//! HUD mirrors. Same §2.5 schedule shape as the map and VAB:
//! `end_frame` stages sim reads into [`FlightSyncInput`],
//! `sync_flight` mirrors them, `flight_present` solves the chase
//! camera + the SFX cue edges, `extract_flight` emits clip-space
//! vertices for `csp_render::flight::FlightRenderer`.
//!
//! **SFX (§3.7 "basic SFX").** M4c ships the cue channel, not an
//! audio device: [`FlightSfx`] edge-detects the sim transitions the
//! SFX row names (engine ignition/cut, staging/jettison, breakup,
//! contact/impact) and flashes them in the flight HUD. The same
//! transition stream is the seam a real audio backend consumes —
//! sound output is deferred to M10's own exit row (no audio stack in
//! the tree; adding one pulls a dependency through §3.7's license
//! audit for zero gate-visible fidelity). PLAN §3.7 carries the
//! deferral note.
//!
//! Determinism: all f64 world math runs on staged values in the
//! schedules; the only f32 is the clip-space cast at emission, like
//! the map projection. Cue TTLs count presentation frames — session
//! state, never sim state.

use std::collections::VecDeque;

use bevy_ecs::prelude::*;
use csp_core::{DQuat, DVec3};
use csp_sim::{VesselState, World};

use crate::map::{MapCameraF64, project_into, solve_orbit_camera};

/// Chase-camera FOV (radians, vertical).
const FLIGHT_FOV_Y: f64 = 0.9;
/// Default camera distance (m) — reframed on focus changes.
const FLIGHT_DEFAULT_DIST: f64 = 60.0;
/// Plasma sheath display window (W/m² of convective input,
/// `FlightBody::plasma_w_m2`): first glow ≈ a fast low pass, full
/// saturation at mild-reentry flux. Display normalization only — the
/// sim's lethality is the M4b temperature record, not this curve.
const PLASMA_ONSET_W_M2: f64 = 5.0e3;
/// See [`PLASMA_ONSET_W_M2`].
const PLASMA_FULL_W_M2: f64 = 8.0e4;

/// One chase-camera input event — `csp-app` translates drags/wheel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FlightEvent {
    /// Orbit delta (radians).
    Orbit {
        /// Yaw increment.
        dyaw: f64,
        /// Pitch increment.
        dpitch: f64,
    },
    /// Multiplicative zoom.
    Zoom {
        /// `> 1` zooms out, `< 1` in.
        factor: f64,
    },
}

/// Session-state chase camera (§2.2-C).
#[derive(Resource)]
pub struct FlightInput {
    /// Orbit yaw (radians).
    pub yaw: f64,
    /// Orbit pitch (radians).
    pub pitch: f64,
    /// Eye–focus distance (m).
    pub distance_m: f64,
    /// Viewport in physical pixels.
    pub viewport_px: (f64, f64),
}

impl Default for FlightInput {
    fn default() -> Self {
        Self {
            yaw: 0.7,
            pitch: 0.25,
            distance_m: FLIGHT_DEFAULT_DIST,
            viewport_px: (1.0, 1.0),
        }
    }
}

/// Drained each frame by `apply_flight_input`.
#[derive(Resource, Default)]
pub struct FlightInputEvents {
    /// Queued events.
    pub pending: Vec<FlightEvent>,
}

/// The environment slice the flight view needs — staged sim data.
#[derive(Clone, Copy, Debug)]
pub struct FlightEnv {
    /// Planet centre (bubble frame).
    pub body_center: DVec3,
    /// Surface radius (m).
    pub radius_m: f64,
    /// Atmosphere top altitude (m), `0` when airless.
    pub atm_top_m: f64,
}

/// One hull's axis-aligned box, body frame — the craft's render
/// silhouette at M4c.
#[derive(Clone, Copy, Debug)]
pub struct FlightBox {
    /// AABB min corner, body frame.
    pub lo: DVec3,
    /// AABB max corner, body frame.
    pub hi: DVec3,
}

/// One InBubble vessel's staged render data.
#[derive(Clone, Debug)]
pub struct FlightBody {
    /// The vessel's id (`VesselId.0` — no sim types at render time).
    pub id: u64,
    /// Body-origin position (bubble frame).
    pub pos: DVec3,
    /// Body→world rotation.
    pub rot: DQuat,
    /// CoM velocity (bubble frame).
    pub vel: DVec3,
    /// Throttle [0,1] — plume intensity.
    pub throttle: f64,
    /// `true` for breakup debris (orange tint).
    pub debris: bool,
    /// Whether the last step recorded ground contact — dust VFX flag.
    pub contact: bool,
    /// Convective heating input per unit area (W/m²) —
    /// `CONVECTIVE_HEAT_FRACTION · ½ρ|v_rel|³` from the same
    /// `flow_sample` the M4b thermal tick consumes; the plasma
    /// sheath's intensity driver.
    pub plasma_w_m2: f64,
    /// Live part-graph node count — the staging/jettison SFX edge.
    pub nodes: usize,
    /// Per-part hull AABBs, body frame.
    pub boxes: Vec<FlightBox>,
    /// Thrust sites `(local_pos, local_dir)` — plume emitters.
    pub plumes: Vec<(DVec3, DVec3)>,
    /// World-frame CoM (camera focus + HUD altitude).
    pub com: DVec3,
}

/// What `end_frame` stages out of the sim for the flight view.
#[derive(Resource, Default)]
pub struct FlightSyncInput {
    /// The physics environment — `None` without physics.
    pub env: Option<FlightEnv>,
    /// InBubble vessels, ascending `VesselId`.
    pub vessels: Vec<FlightBody>,
}

/// The `sync` schedule's mirror — identical data, presentation-owned.
#[derive(Resource, Default)]
pub struct FlightSnapshot {
    /// See [`FlightSyncInput::env`].
    pub env: Option<FlightEnv>,
    /// See [`FlightSyncInput::vessels`].
    pub vessels: Vec<FlightBody>,
}

/// HUD mirrors — written by `flight_present`, drawn by
/// `csp_ui::flight_hud`.
#[derive(Resource, Default)]
pub struct FlightUiData {
    /// Focused vessel exists.
    pub has_vessel: bool,
    /// Focused vessel id (for throttle commands), `None` otherwise.
    pub focus: Option<u64>,
    /// Altitude above surface (m).
    pub alt_m: f64,
    /// CoM speed (m/s).
    pub speed_mps: f64,
    /// Vertical speed (m/s).
    pub vspeed_mps: f64,
    /// Throttle [0,1].
    pub throttle: f64,
    /// Total vessels in the world.
    pub vessel_count: usize,
    /// Debris-flagged vessels.
    pub debris_count: usize,
    /// Live SFX cue lines (§3.7's visual SFX representation) —
    /// written from [`FlightSfx`], drawn as event flashes by
    /// `csp_ui::flight_hud`.
    pub sfx: Vec<String>,
}

/// One live cue in the §3.7 visual-SFX channel.
#[derive(Clone, Debug)]
pub struct SfxCue {
    /// The HUD line ("ENGINE IGNITION", "BREAKUP", …).
    pub text: String,
    /// Remaining presentation frames before the cue fades out.
    pub ttl: u32,
}

/// Last frame's edge-detection summary for [`FlightSfx`].
#[derive(Clone, Copy, Debug)]
struct SfxPrev {
    /// Focused vessel id (`vessels[0]` — the chase target).
    focus: u64,
    /// `throttle > 0` — the ignition/cut edge.
    throttle_on: bool,
    /// Focused craft's live node count — the jettison edge.
    nodes: usize,
    /// Debris-flagged vessel count — the breakup edge.
    debris: usize,
    /// InBubble vessel count — the impact-removal edge.
    vessels: usize,
    /// Focused vessel's contact flag — the touchdown edge.
    contact: bool,
}

/// §3.7's "basic SFX" as a visual cue channel (module docs carry the
/// audio deferral): `flight_present` edge-detects the named
/// transitions on the staged snapshot and flashes them for ~1.5 s.
/// Session state only — TTLs count *presentation frames*, never sim
/// ticks, and nothing here feeds back into the sim.
#[derive(Resource, Default)]
pub struct FlightSfx {
    /// Live cue stack, oldest first.
    pub cues: VecDeque<SfxCue>,
    /// Last frame's edge state — `None` until the first present pass.
    prev: Option<SfxPrev>,
}

/// Cue lifetime in frames — ~1.5 s at 60 fps.
const SFX_TTL_FRAMES: u32 = 90;

/// The RenderExtract product — `None` while no flight scene exists.
#[derive(Resource, Default)]
pub struct FlightFrame(pub Option<csp_render::flight::FlightPacket>);

/// `end_frame` staging: pull the flight view's slice out of `World`
/// (the only sim read this module does — §2.5).
pub fn capture_sync_input(sim: &World) -> FlightSyncInput {
    let env = sim.environment().map(|e| FlightEnv {
        body_center: e.body_center,
        radius_m: e.radius_m,
        atm_top_m: e.atmosphere.as_ref().map_or(0.0, |a| a.top_m),
    });
    let mut vessels = Vec::new();
    for (id, vessel) in sim.vessels() {
        let VesselState::InBubble(body) = &vessel.state else {
            continue;
        };
        let boxes = body
            .collider
            .hulls
            .iter()
            .map(|h| {
                let mut lo = DVec3::new(f64::MAX, f64::MAX, f64::MAX);
                let mut hi = DVec3::new(-f64::MAX, -f64::MAX, -f64::MAX);
                for v in &h.vertices {
                    lo = DVec3::new(lo.x.min(v.x), lo.y.min(v.y), lo.z.min(v.z));
                    hi = DVec3::new(hi.x.max(v.x), hi.y.max(v.y), hi.z.max(v.z));
                }
                FlightBox { lo, hi }
            })
            .collect();
        let plumes = body
            .force_sites
            .iter()
            .filter(|s| s.kind == csp_phys::ForceSiteKind::Thrust)
            .map(|s| (s.local_pos, s.local_dir))
            .collect();
        // §3.7's plasma driver: the per-area convective input the M4b
        // thermal tick itself integrates — same `flow_sample`, same
        // `v_rel`, so the glow tracks the sim's heating, not a lookalike.
        let plasma_w_m2 = sim.environment().map_or(0.0, |e| {
            let flow = csp_phys::flow_sample(e, body);
            csp_phys::CONVECTIVE_HEAT_FRACTION * 0.5 * flow.density_kg_m3 * flow.speed_mps.powi(3)
        });
        vessels.push(FlightBody {
            id: id.0,
            pos: body.pos,
            rot: body.rot,
            vel: body.vel,
            throttle: body.throttle,
            debris: vessel.debris_since.is_some(),
            contact: sim.ground_contact(*id),
            plasma_w_m2,
            nodes: vessel.craft.as_ref().map_or(0, |c| c.graph.nodes.len()),
            boxes,
            plumes,
            com: body.com_world(),
        });
    }
    FlightSyncInput { env, vessels }
}

/// `Input` schedule — drain camera events into `FlightInput`.
pub(crate) fn apply_flight_input(
    mut events: ResMut<FlightInputEvents>,
    mut input: ResMut<FlightInput>,
) {
    for e in events.pending.drain(..) {
        match e {
            FlightEvent::Orbit { dyaw, dpitch } => {
                input.yaw += dyaw;
                input.pitch = (input.pitch + dpitch).clamp(-1.45, 1.45);
            }
            FlightEvent::Zoom { factor } => {
                input.distance_m = (input.distance_m * factor).clamp(5.0, 1.0e6);
            }
        }
    }
}

/// `Sync` schedule — copy the staged input into the presentation
/// mirror.
pub(crate) fn sync_flight(staged: Res<FlightSyncInput>, mut snap: ResMut<FlightSnapshot>) {
    snap.env = staged.env;
    snap.vessels = staged.vessels.clone();
}

/// `Presentation` schedule — chase-camera solve + HUD mirrors + the
/// §3.7 SFX cue edges (the schedule's "VFX, audio cues" slot).
pub(crate) fn flight_present(
    input: Res<FlightInput>,
    snap: Res<FlightSnapshot>,
    mut cam: ResMut<MapCameraF64>,
    mut ui: ResMut<FlightUiData>,
    mut sfx: ResMut<FlightSfx>,
) {
    ui.vessel_count = snap.vessels.len();
    ui.debris_count = snap.vessels.iter().filter(|v| v.debris).count();

    // SFX edge detection — the §3.7 row's named transitions, detected
    // on the staged snapshot (never the sim). Per-focus edges apply
    // only while the SAME vessel stays focused; a focus switch
    // re-baselines instead of firing a spurious cut/contact edge.
    let cur = snap.vessels.first().map(|f| SfxPrev {
        focus: f.id,
        throttle_on: f.throttle > 0.0,
        nodes: f.nodes,
        debris: ui.debris_count,
        vessels: snap.vessels.len(),
        contact: f.contact,
    });
    match (sfx.prev, cur) {
        (Some(p), Some(c)) => {
            let mut push = |text: &str| {
                sfx.cues.push_back(SfxCue {
                    text: text.to_owned(),
                    ttl: SFX_TTL_FRAMES,
                });
            };
            if c.focus == p.focus {
                if c.throttle_on && !p.throttle_on {
                    push("ENGINE IGNITION");
                } else if !c.throttle_on && p.throttle_on {
                    push("ENGINE CUT");
                }
                if c.nodes < p.nodes {
                    push("STAGE — PART JETTISONED");
                }
                if c.contact && !p.contact {
                    push("CONTACT");
                }
            }
            if c.debris > p.debris {
                push("BREAKUP");
            }
            if c.debris < p.debris {
                // §2.9: in-bubble debris leaves only via impact removal
                // (it never latches).
                push("DEBRIS IMPACT");
            }
            if c.vessels < p.vessels && c.debris >= p.debris {
                push("VESSEL REMOVED");
            }
        }
        (Some(_), None) => sfx.cues.push_back(SfxCue {
            text: "VESSEL LEFT BUBBLE".to_owned(),
            ttl: SFX_TTL_FRAMES,
        }),
        (None, _) => {} // first frame — baseline only
    }
    for cue in &mut sfx.cues {
        cue.ttl = cue.ttl.saturating_sub(1);
    }
    while sfx.cues.front().is_some_and(|c| c.ttl == 0) {
        sfx.cues.pop_front();
    }
    ui.sfx = sfx.cues.iter().map(|c| c.text.clone()).collect();
    sfx.prev = cur;

    let Some(focus) = snap.vessels.first() else {
        cam.valid = false;
        ui.has_vessel = false;
        ui.focus = None;
        return;
    };
    *cam = solve_orbit_camera(
        input.yaw,
        input.pitch,
        input.distance_m,
        input.viewport_px,
        focus.com,
        FLIGHT_FOV_Y,
        0.1,
    );
    ui.has_vessel = true;
    ui.focus = Some(focus.id);
    ui.speed_mps = focus.vel.norm();
    ui.throttle = focus.throttle;
    if let Some(env) = snap.env {
        let rel = focus.com - env.body_center;
        let r = rel.norm();
        ui.alt_m = r - env.radius_m;
        ui.vspeed_mps = if r > 0.0 { focus.vel.dot(rel / r) } else { 0.0 };
    }
}

/// `RenderExtract` — emit the scene's clip-space triangles.
#[allow(clippy::too_many_lines)]
pub(crate) fn extract_flight(
    input: Res<FlightInput>,
    snap: Res<FlightSnapshot>,
    cam: Res<MapCameraF64>,
    mut frame: ResMut<FlightFrame>,
) {
    let mut packet = csp_render::flight::FlightPacket::default();
    let v = &mut packet.vertices;
    // Sky: a far-depth fullscreen gradient (two tris at NDC z≈0 —
    // reverse-Z far), horizon blue at the bottom fading up.
    let zenith = [0.05, 0.09, 0.22, 1.0];
    let horizon = [0.35, 0.5, 0.75, 1.0];
    let q = [
        ([-1.0f32, -1.0, 0.0], horizon),
        ([1.0, -1.0, 0.0], horizon),
        ([1.0, 1.0, 0.0], zenith),
        ([-1.0, -1.0, 0.0], horizon),
        ([1.0, 1.0, 0.0], zenith),
        ([-1.0, 1.0, 0.0], zenith),
    ];
    for (pos, color) in q {
        v.push(csp_render::flight::FlightVertex { pos, color });
    }
    if !cam.valid {
        frame.0 = Some(packet);
        return;
    }
    let vp = &cam.view_proj;
    let eye = cam.eye;
    let mut emit = |world: DVec3, color: [f32; 4]| -> Option<[f32; 3]> {
        let ndc = project_into(vp, eye, world)?;
        v.push(csp_render::flight::FlightVertex { pos: ndc, color });
        Some(ndc)
    };

    if let Some(env) = snap.env {
        // Ground disc: a 96-seg triangle fan at surface level centred
        // under the camera; radius = 1.4× the horizon distance so
        // the planet edge (and curvature) reads.
        let center_dir = {
            let d = eye - env.body_center;
            if d.norm() > 0.0 {
                d / d.norm()
            } else {
                DVec3::Y
            }
        };
        let center = env.body_center + center_dir * env.radius_m;
        let alt = ((eye - env.body_center).norm() - env.radius_m).max(1.0);
        let horizon_d = ((env.radius_m + alt).powi(2) - env.radius_m.powi(2)).sqrt();
        let disc_r = (horizon_d * 1.4).max(input.distance_m * 4.0);
        let (t1, t2) = {
            let up = center_dir;
            let a = if up.y.abs() < 0.9 { DVec3::Y } else { DVec3::X };
            let t1 = up.cross(a) / up.cross(a).norm();
            (t1, up.cross(t1))
        };
        let n = 96usize;
        for i in 0..n {
            let (a0, a1) = (
                (i as f64) * 2.0 * core::f64::consts::PI / n as f64,
                ((i + 1) as f64) * 2.0 * core::f64::consts::PI / n as f64,
            );
            let (s0, c0) = (a0.sin(), a0.cos());
            let (s1, c1) = (a1.sin(), a1.cos());
            let p0 = center + t1 * (c0 * disc_r) + t2 * (s0 * disc_r);
            let p1 = center + t1 * (c1 * disc_r) + t2 * (s1 * disc_r);
            emit(center, [0.22, 0.32, 0.18, 1.0]);
            emit(p0, [0.22, 0.32, 0.18, 1.0]);
            emit(p1, [0.2, 0.3, 0.17, 1.0]);
        }
    }

    // Craft: per-hull boxes, flat lambert shading against a fixed sun
    // direction; debris tints orange.
    let sun = {
        let s = DVec3::new(0.4, 0.3, 0.86);
        s / s.norm()
    };
    // Box corner indices per face — hull shells and the plasma sheath
    // share the table.
    const FACES: [([usize; 4], DVec3); 6] = [
        ([0, 1, 2, 3], DVec3::new(0.0, 0.0, -1.0)),
        ([5, 4, 7, 6], DVec3::new(0.0, 0.0, 1.0)),
        ([1, 5, 6, 2], DVec3::new(1.0, 0.0, 0.0)),
        ([4, 0, 3, 7], DVec3::new(-1.0, 0.0, 0.0)),
        ([3, 2, 6, 7], DVec3::new(0.0, 1.0, 0.0)),
        ([4, 5, 1, 0], DVec3::new(0.0, -1.0, 0.0)),
    ];
    for b in &snap.vessels {
        let base: [f32; 4] = if b.debris {
            [0.85, 0.45, 0.2, 1.0]
        } else {
            [0.75, 0.8, 0.88, 1.0]
        };
        for bx in &b.boxes {
            let corners: [DVec3; 8] = [
                DVec3::new(bx.lo.x, bx.lo.y, bx.lo.z),
                DVec3::new(bx.hi.x, bx.lo.y, bx.lo.z),
                DVec3::new(bx.hi.x, bx.hi.y, bx.lo.z),
                DVec3::new(bx.lo.x, bx.hi.y, bx.lo.z),
                DVec3::new(bx.lo.x, bx.lo.y, bx.hi.z),
                DVec3::new(bx.hi.x, bx.lo.y, bx.hi.z),
                DVec3::new(bx.hi.x, bx.hi.y, bx.hi.z),
                DVec3::new(bx.lo.x, bx.hi.y, bx.hi.z),
            ];
            for (idx, n_b) in FACES {
                let n_w = b.rot.rotate(n_b);
                let shade = (0.45 + 0.55 * n_w.dot(sun).max(0.0)) as f32;
                let color = [base[0] * shade, base[1] * shade, base[2] * shade, base[3]];
                let w: Vec<DVec3> = idx
                    .iter()
                    .map(|&i| b.pos + b.rot.rotate(corners[i]))
                    .collect();
                emit(w[0], color);
                emit(w[1], color);
                emit(w[2], color);
                emit(w[0], color);
                emit(w[2], color);
                emit(w[3], color);
            }
        }
        // Engine plume: a cone fan along −local_dir per thrust site.
        if b.throttle > 0.0 {
            for (p0, dir) in &b.plumes {
                let dir_w = {
                    let d = b.rot.rotate(*dir);
                    d / d.norm()
                };
                let tip = b.pos + b.rot.rotate(*p0);
                let len = 6.0 + 30.0 * b.throttle;
                let base_p = tip - dir_w * len;
                let (u1, u2) = {
                    let a = if dir_w.y.abs() < 0.9 {
                        DVec3::Y
                    } else {
                        DVec3::X
                    };
                    let u1 = {
                        let u = dir_w.cross(a);
                        u / u.norm()
                    };
                    (u1, dir_w.cross(u1))
                };
                let r = 0.6 + 0.9 * b.throttle;
                let col = [1.0, 0.55 + 0.25 * b.throttle as f32, 0.15, 0.7];
                for i in 0..8 {
                    let (a0, a1) = (
                        (i as f64) * core::f64::consts::PI * 0.25,
                        ((i + 1) as f64) * core::f64::consts::PI * 0.25,
                    );
                    let q0 = base_p + u1 * (a0.cos() * r) + u2 * (a0.sin() * r);
                    let q1 = base_p + u1 * (a1.cos() * r) + u2 * (a1.sin() * r);
                    emit(tip, col);
                    emit(q0, col);
                    emit(q1, col);
                }
            }
        }

        // §3.7 reentry plasma: an emissive sheath hugging the hull —
        // each box re-emitted expanded by an intensity-scaled margin,
        // alpha-blended over the hull so the craft reads as glowing
        // inside its shock layer. Intensity is the staged convective
        // input normalized through the display window; the shell is
        // flat-lit (emissive, no sun term).
        let plasma_i = ((b.plasma_w_m2 - PLASMA_ONSET_W_M2)
            / (PLASMA_FULL_W_M2 - PLASMA_ONSET_W_M2))
            .clamp(0.0, 1.0);
        if plasma_i > 0.0 {
            let m = 0.15 + 0.35 * plasma_i;
            let col: [f32; 4] = [
                1.0,
                (0.3 + 0.6 * plasma_i) as f32,
                (0.05 + 0.35 * plasma_i) as f32,
                (0.12 + 0.5 * plasma_i) as f32,
            ];
            let d = DVec3::new(m, m, m);
            for bx in &b.boxes {
                let lo = bx.lo - d;
                let hi = bx.hi + d;
                let corners: [DVec3; 8] = [
                    DVec3::new(lo.x, lo.y, lo.z),
                    DVec3::new(hi.x, lo.y, lo.z),
                    DVec3::new(hi.x, hi.y, lo.z),
                    DVec3::new(lo.x, hi.y, lo.z),
                    DVec3::new(lo.x, lo.y, hi.z),
                    DVec3::new(hi.x, lo.y, hi.z),
                    DVec3::new(hi.x, hi.y, hi.z),
                    DVec3::new(lo.x, hi.y, hi.z),
                ];
                for (idx, _) in FACES {
                    let w: Vec<DVec3> = idx
                        .iter()
                        .map(|&i| b.pos + b.rot.rotate(corners[i]))
                        .collect();
                    emit(w[0], col);
                    emit(w[1], col);
                    emit(w[2], col);
                    emit(w[0], col);
                    emit(w[2], col);
                    emit(w[3], col);
                }
            }
        }

        // §3.7 ground-contact dust: a translucent tan ring hugging
        // the surface under the vessel's lowest hull vertex.
        if b.contact
            && let Some(env) = snap.env
        {
            let mut low = b.com;
            let mut low_alt = f64::MAX;
            let mut extent = 1.0_f64;
            for bx in &b.boxes {
                for c in [
                    DVec3::new(bx.lo.x, bx.lo.y, bx.lo.z),
                    DVec3::new(bx.hi.x, bx.lo.y, bx.lo.z),
                    DVec3::new(bx.hi.x, bx.hi.y, bx.lo.z),
                    DVec3::new(bx.lo.x, bx.hi.y, bx.lo.z),
                    DVec3::new(bx.lo.x, bx.lo.y, bx.hi.z),
                    DVec3::new(bx.hi.x, bx.lo.y, bx.hi.z),
                    DVec3::new(bx.hi.x, bx.hi.y, bx.hi.z),
                    DVec3::new(bx.lo.x, bx.hi.y, bx.hi.z),
                ] {
                    let w = b.pos + b.rot.rotate(c);
                    extent = extent.max((w - b.pos).norm());
                    let alt = (w - env.body_center).norm() - env.radius_m;
                    if alt < low_alt {
                        low_alt = alt;
                        low = w;
                    }
                }
            }
            let dir = {
                let d = low - env.body_center;
                if d.norm() > 0.0 {
                    d / d.norm()
                } else {
                    DVec3::X
                }
            };
            // Puff sits a hair above the surface so depth ties resolve
            // for the dust.
            let base = env.body_center + dir * (env.radius_m + 0.3);
            let (t1, t2) = {
                let a = if dir.y.abs() < 0.9 {
                    DVec3::Y
                } else {
                    DVec3::X
                };
                let t1 = {
                    let t = dir.cross(a);
                    t / t.norm()
                };
                (t1, dir.cross(t1))
            };
            let dust_r = extent * 1.6 + 1.0;
            let col = [0.62, 0.55, 0.42, 0.35];
            for i in 0..12 {
                let (a0, a1) = (
                    (i as f64) * core::f64::consts::PI / 6.0,
                    ((i + 1) as f64) * core::f64::consts::PI / 6.0,
                );
                let p0 = base + t1 * (a0.cos() * dust_r) + t2 * (a0.sin() * dust_r);
                let p1 = base + t1 * (a1.cos() * dust_r) + t2 * (a1.sin() * dust_r);
                emit(base, col);
                emit(p0, col);
                emit(p1, col);
            }
        }
    }
    frame.0 = Some(packet);
}
