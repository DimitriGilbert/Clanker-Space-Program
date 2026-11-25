//! §2.5 interpolation — the flight view draws every staged vessel
//! between its previous and its current simulation tick, at the frame's
//! fractional tick remainder. Drawn at the tick itself, a 60 fps frame
//! over the 64 Hz sim moves the craft one tick fifteen frames in
//! sixteen and two in the sixteenth: the whole view — the camera
//! included — hitch-steps four times a second.
//!
//! `View::set_tick_fraction` is the app's hand-off; `csp-app`'s warp
//! controller computes the remainder and its `warp.rs` unit tests pin
//! that half. `Frames` here mirrors that controller's
//! integer-nanosecond accumulator line for line — `csp-app` cannot be
//! a dev-dependency of `csp-view` — and drives the view the way
//! `run_frame` does: step, hand over the remainder, end the frame.
#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use csp_core::{DQuat, DVec3, TICK_HZ};
use csp_data::KeplerElements;
use csp_sim::orbit::{BodyOrbit, PatchedConics};
use csp_sim::{DEFAULT_BUBBLE_RADIUS_M, PhysicsConfig, SimCommand, VesselId, World as SimWorld};
use csp_view::flight::{
    DrawnPose, DrawnScene, FlightBody, FlightCameraF64, FlightEnv, FlightSnapshot,
};
use csp_view::{PLAYER_SOURCE_ID, SourceClass, View};

/// Nanoseconds per sim tick — exact (10⁹ = 64 × 15 625 000).
const NANOS_PER_TICK: u128 = 1_000_000_000 / TICK_HZ as u128;
/// The wall dt the even-travel bound is written against.
const FRAME_DT_S: f64 = 1.0 / 60.0;
/// Per-frame drawn travel may vary by at most this much across a run.
const TRAVEL_VARIATION_MAX: f64 = 0.10;

/// The test planet: a 600 km vacuum body turning once an hour.
const R_M: f64 = 6.0e5;
const MU: f64 = 3.5316e12;
const SPIN_RAD_S: f64 = 2.0 * std::f64::consts::PI / 3600.0;

/// `csp-app`'s frame loop without the window — the warp controller's
/// integer-nanosecond accumulator (`csp-app/src/warp.rs`:
/// `target_tick`, `tick_fraction`) at a fixed multiplier.
struct Frames {
    /// Σ dt_wall_ns × multiplier.
    scaled_nanos: u128,
    multiplier: u64,
    /// Hand the view remainder 0 instead — what any harness that
    /// supplies none draws.
    pinned: bool,
    /// The remainder the last frame was handed.
    last_frac: f64,
}

impl Frames {
    fn new(multiplier: u64) -> Self {
        Self {
            scaled_nanos: 0,
            multiplier,
            pinned: false,
            last_frac: 0.0,
        }
    }

    /// One frame at [`FRAME_DT_S`]; returns the ticks it advanced.
    fn step(&mut self, view: &mut View) -> u64 {
        self.step_dt(view, FRAME_DT_S)
    }

    fn step_dt(&mut self, view: &mut View, dt_s: f64) -> u64 {
        let before = view.sim.clock().ticks();
        self.scaled_nanos +=
            u128::from((dt_s * 1_000_000_000.0) as u64) * u128::from(self.multiplier);
        let ticks = self.scaled_nanos * u128::from(TICK_HZ) / 1_000_000_000;
        let target = u64::try_from(ticks).unwrap_or(u64::MAX).max(before);
        view.begin_frame();
        view.sim_step(target).expect("sim step");
        let reached = view.sim.clock().ticks();
        let past = self
            .scaled_nanos
            .saturating_sub(u128::from(reached) * NANOS_PER_TICK);
        self.last_frac = if self.pinned {
            0.0
        } else {
            past.min(NANOS_PER_TICK - 1) as f64 / NANOS_PER_TICK as f64
        };
        view.set_tick_fraction(self.last_frac);
        view.end_frame();
        reached - before
    }
}

/// A one-planet vacuum system — no atmosphere, so nothing but gravity
/// touches the craft and its speed holds: uneven drawn motion is the
/// view's, never the sim's.
fn vacuum_view() -> View {
    let mut bodies = BTreeMap::new();
    bodies.insert(
        csp_data::BodyId::from("sol"),
        BodyOrbit {
            mu_m3_per_s2: 1.1723e18,
            soi_m: None,
            parent: None,
            elements: None,
        },
    );
    bodies.insert(
        csp_data::BodyId::from("p"),
        BodyOrbit {
            mu_m3_per_s2: MU,
            soi_m: Some(8.4e7),
            parent: Some(csp_data::BodyId::from("sol")),
            elements: Some(KeplerElements {
                semi_major_axis_m: 1.36e10,
                eccentricity: 0.0,
                inclination_rad: 0.0,
                ascending_node_rad: 0.0,
                arg_periapsis_rad: 0.0,
                mean_anomaly_epoch_rad: 0.0,
                period_s: f64::INFINITY,
            }),
        },
    );
    let sim = SimWorld::with_physics(
        PatchedConics::new(bodies),
        PhysicsConfig {
            environment: csp_phys::Environment {
                body_center: DVec3::ZERO,
                radius_m: R_M,
                mu_m3_per_s2: MU,
                body_spin: DVec3::Z * SPIN_RAD_S,
                atmosphere: None,
                settle_v: csp_phys::DEFAULT_SETTLE_V,
                settle_w: csp_phys::DEFAULT_SETTLE_W,
                settle_ticks: csp_phys::DEFAULT_SETTLE_TICKS,
                friction_mu: csp_phys::DEFAULT_FRICTION_MU,
                drag_cd: csp_phys::DEFAULT_DRAG_CD,
                convective_heat_fraction: csp_phys::DEFAULT_CONVECTIVE_HEAT_FRACTION,
                terrain: None,
            },
            dominant: csp_data::BodyId::from("p"),
            bubble_radius_m: DEFAULT_BUBBLE_RADIUS_M,
        },
        1024,
    );
    View::with_sim(sim)
}

/// One box part of `mass_kg`, with or without a small fueled engine on
/// its base. The engine only matters to the rails test — a throttled
/// craft is not coasting, so the sim keeps it in the bubble.
fn box_part(mass_kg: f64, half: DVec3, engine: bool) -> csp_phys::WeldPart {
    let fuel = engine.then(|| csp_data::ResourceId::from("liquid_fuel"));
    csp_phys::WeldPart {
        experiments: Vec::new(),
        lab: None,
        engine: fuel.as_ref().map(|fuel| csp_phys::EngineSpec {
            isp_vac_s: 300.0,
            propellants: vec![(fuel.clone(), 1.0)],
        }),
        parachute: None,
        dock_ports: vec![],
        tanks: fuel
            .map(|fuel| {
                vec![csp_phys::TankSpec {
                    resource: fuel,
                    capacity: 1.0,
                    mass_per_unit_kg: 5.0,
                }]
            })
            .unwrap_or_default(),
        crew_seats: 0,
        structural: csp_data::StructuralDef::unlimited(),
        mass_kg,
        com: DVec3::ZERO,
        inertia: csp_phys::SymMat3::solid_box(mass_kg, half),
        collider: csp_phys::ColliderShape::Box { half_extents: half },
        force_sites: engine
            .then(|| {
                csp_phys::ForceSite::new(
                    DVec3::new(0.0, 0.0, -half.z),
                    DVec3::Z,
                    csp_phys::ForceSiteKind::Thrust,
                    200.0,
                )
            })
            .into_iter()
            .collect(),
        aero: csp_data::AeroDef::constant(0.0, 0.0, 1.0),
        thermal: csp_data::ThermalDef {
            heat_capacity_j_per_k: 400_000.0,
            max_temp_k: 1_100.0,
            emissivity: 0.8,
            conduction_w_per_k: 60.0,
        },
    }
}

/// A two-part stack — a heavy root and a lighter nose three metres up —
/// so a jettison shifts the CoM without touching the body frame.
fn stack() -> csp_sim::CraftDef {
    use csp_phys::{PartNode, PartNodeId, PartTransform};
    let mut graph = csp_phys::PartGraph::new(PartNodeId(0));
    graph.insert(
        PartNodeId(0),
        PartNode::new(None, PartTransform::IDENTITY, 0),
    );
    graph.insert(
        PartNodeId(1),
        PartNode::new(
            Some(PartNodeId(0)),
            PartTransform::new(DVec3::new(0.0, 0.0, 3.0), DQuat::IDENTITY),
            1,
        ),
    );
    csp_sim::CraftDef {
        part_build_resources: Vec::new(),
        hash_memo: Default::default(),
        part_ids: vec![],
        graph,
        parts: vec![
            box_part(600.0, DVec3::new(0.5, 0.5, 1.0), true),
            box_part(200.0, DVec3::new(0.5, 0.5, 0.5), false),
        ],
        part_costs: vec![0.0, 0.0],
        reliability: vec![],
    }
}

/// Spawn the stack `alt_m` up, moving `speed × circular` tangentially
/// and tumbling slowly, and run one frame. A positive `throttle` rides
/// the same drain. Returns the vessel's id.
fn flying(view: &mut View, frames: &mut Frames, alt_m: f64, speed: f64, throttle: f64) -> VesselId {
    let r = R_M + alt_m;
    view.enqueue_command(
        SourceClass::Ui,
        PLAYER_SOURCE_ID,
        SimCommand::SpawnPhysicalVessel {
            craft: stack(),
            pos: DVec3::new(r, 0.0, 0.0),
            rot: DQuat::IDENTITY,
            vel: DVec3::new(0.0, speed * (MU / r).sqrt(), 0.0),
            ang_vel: DVec3::new(0.3, -0.2, 0.5),
        },
    );
    if throttle > 0.0 {
        // The first vessel a fresh world mints — the throttle must ride
        // the spawn's own drain, before anything can rail the craft.
        view.enqueue_command(
            SourceClass::Ui,
            PLAYER_SOURCE_ID,
            SimCommand::SetThrottle {
                vessel: VesselId(0),
                throttle,
            },
        );
    }
    frames.step(view);
    let id = *view.sim.vessels().keys().next_back().expect("spawned");
    assert_eq!(
        view.sim.vessels().len(),
        1,
        "one vessel, so the id is unique"
    );
    id
}

/// The vessel as staged this frame — its last simulation tick's pose.
fn staged(view: &View, id: VesselId) -> FlightBody {
    view.world
        .resource::<FlightSnapshot>()
        .vessels
        .iter()
        .find(|b| b.id == id.0)
        .cloned()
        .expect("staged")
}

/// The staged body as a pose — what a frame draws when it interpolates
/// nothing.
fn as_drawn(b: &FlightBody) -> DrawnPose {
    DrawnPose {
        id: b.id,
        pos: b.pos,
        rot: b.rot,
        com: b.com,
    }
}

/// The vessel's drawn pose this frame.
fn drawn(view: &View, id: VesselId) -> DrawnPose {
    view.world
        .resource::<DrawnScene>()
        .drawn(id.0)
        .expect("drawn this frame")
}

/// The point the chase camera orbits this frame, recovered from its
/// matrix by the camera's own convention — `distance_m` is the actual
/// eye–anchor distance: `eye − z_out · distance`.
fn camera_focus(view: &View) -> DVec3 {
    let cam = &view.world.resource::<FlightCameraF64>().0;
    assert!(cam.valid, "the chase camera must solve for a staged craft");
    let z_out = DVec3::new(
        -cam.view_proj[3][0],
        -cam.view_proj[3][1],
        -cam.view_proj[3][2],
    );
    cam.eye - z_out * cam.distance_m
}

/// The camera orbits the pose the craft is DRAWN at — and with a
/// remainder in hand that pose is not the staged one.
fn assert_camera_follows_the_drawn_craft(view: &View, id: VesselId, frac: f64) {
    let pose = drawn(view, id);
    let gap = (camera_focus(view) - pose.com).norm();
    assert!(gap < 1.0e-3, "camera orbits {gap} m from the drawn craft");
    if frac > 0.01 {
        assert!(
            (pose.com - staged(view, id).com).norm() > 0.0,
            "the drawn pose must sit between ticks, not on the staged one"
        );
    }
}

/// Drive `n` frames and return the drawn CoM's frame-to-frame travel in
/// metres — one entry per frame.
fn travel(view: &mut View, frames: &mut Frames, id: VesselId, n: usize) -> Vec<f64> {
    // Past the craft's first tick on screen: with no previous pose it
    // is drawn at its current one — a hold of under one tick.
    frames.step(view);
    let mut prev = drawn(view, id).com;
    (0..n)
        .map(|_| {
            frames.step(view);
            assert_camera_follows_the_drawn_craft(view, id, frames.last_frac);
            let now = drawn(view, id).com;
            let step = (now - prev).norm();
            prev = now;
            step
        })
        .collect()
}

/// How uneven the per-frame travel is: the larger of its spread across
/// the run, `(largest − smallest) / mean`, and its largest change from
/// one frame to the next, relative to the earlier frame.
fn variation(steps: &[f64]) -> f64 {
    let lo = steps.iter().cloned().fold(f64::MAX, f64::min);
    let hi = steps.iter().cloned().fold(0.0, f64::max);
    let spread = (hi - lo) / (steps.iter().sum::<f64>() / steps.len() as f64);
    let frame_to_frame = steps
        .windows(2)
        .map(|w| ((w[1] - w[0]) / w[0]).abs())
        .fold(0.0, f64::max);
    spread.max(frame_to_frame)
}

/// A craft in free flight, seen at 60 fps: over 120 frames the drawn
/// travel may vary by at most 10 percent — at ×1, ×2 and ×4 physics
/// warp, where a frame carries one to five ticks.
#[test]
fn drawn_travel_is_even_at_60fps() {
    for multiplier in [1_u64, 2, 4] {
        let mut view = vacuum_view();
        let mut frames = Frames::new(multiplier);
        let id = flying(&mut view, &mut frames, 150_000.0, 0.6, 0.0);
        let steps = travel(&mut view, &mut frames, id, 120);
        let v = variation(&steps);
        eprintln!(
            "travel variation at x{multiplier}, interpolated: {v:.5} ({:.2} m per frame)",
            steps.iter().sum::<f64>() / steps.len() as f64
        );
        assert!(
            v <= TRAVEL_VARIATION_MAX,
            "x{multiplier}: per-frame travel varies by {v:.4} (bound {TRAVEL_VARIATION_MAX})"
        );
    }
}

/// The control that gives the bound its teeth: the same runs with the
/// remainder pinned to zero — every pose drawn at its simulation tick,
/// as before — must break it.
#[test]
fn pinned_remainder_breaks_the_even_travel_bound() {
    for multiplier in [1_u64, 2, 4] {
        let mut view = vacuum_view();
        let mut frames = Frames::new(multiplier);
        frames.pinned = true;
        let id = flying(&mut view, &mut frames, 150_000.0, 0.6, 0.0);
        let steps = travel(&mut view, &mut frames, id, 120);
        let v = variation(&steps);
        eprintln!("travel variation at x{multiplier}, remainder pinned to 0: {v:.5}");
        assert!(
            v > TRAVEL_VARIATION_MAX,
            "x{multiplier}: the tick-stepped view passed the bound ({v:.4})"
        );
    }
}

/// A quaternion's rotation axis and angle.
fn axis_angle(q: DQuat) -> (DVec3, f64) {
    let v = DVec3::new(q.x, q.y, q.z);
    let n = v.norm();
    (v / n, 2.0 * q.w.clamp(-1.0, 1.0).acos())
}

/// The drawn pose IS the interpolation: each frame sits exactly the
/// remainder of the way from the pose staged at the previous tick to
/// the one at the current tick — across one-tick steps, across the
/// four- and five-tick steps a ×4 frame carries, and through the frames
/// at 240 fps that advance nothing and keep closing on the same current
/// pose. The rotation turns the same share of the step's turn about the
/// step's axis.
#[test]
fn drawn_pose_is_the_remainder_between_the_last_two_ticks() {
    for (multiplier, dt_s) in [(1, FRAME_DT_S), (4, FRAME_DT_S), (1, 1.0 / 240.0)] {
        let mut view = vacuum_view();
        let mut frames = Frames::new(multiplier);
        let id = flying(&mut view, &mut frames, 150_000.0, 0.6, 0.0);
        // The staged pose at the current tick, and at the one before.
        let mut cur = (view.sim.clock().ticks(), staged(&view, id));
        let mut prev: Option<(u64, FlightBody)> = None;
        let (mut zero_tick, mut multi_tick, mut checked) = (0, 0, 0);
        for _ in 0..120 {
            let advanced = frames.step_dt(&mut view, dt_s);
            zero_tick += u32::from(advanced == 0);
            multi_tick += u32::from(advanced > 1);
            let tick = view.sim.clock().ticks();
            if tick > cur.0 {
                prev = Some(cur);
                cur = (tick, staged(&view, id));
            }
            let Some((prev_tick, before)) = prev.as_ref() else {
                // Its first tick on screen: no previous pose.
                assert_eq!(drawn(&view, id), as_drawn(&cur.1));
                continue;
            };
            let now = &cur.1;
            let span = (cur.0 - prev_tick) as f64;
            let t = ((cur.0 - 1 - prev_tick) as f64 + frames.last_frac) / span;
            let pose = drawn(&view, id);
            let want = before.pos + (now.pos - before.pos) * t;
            assert!(
                (pose.pos - want).norm() < 1.0e-6,
                "position {:?} vs {want:?}",
                pose.pos
            );
            let want = before.com + (now.com - before.com) * t;
            assert!(
                (pose.com - want).norm() < 1.0e-6,
                "CoM {:?} vs {want:?}",
                pose.com
            );
            let (step_axis, step_angle) = axis_angle(now.rot * before.rot.conjugate());
            let (got_axis, got_angle) = axis_angle(pose.rot * before.rot.conjugate());
            assert!((step_axis - got_axis).norm() < 1.0e-6, "off the step's arc");
            assert!(
                (got_angle - t * step_angle).abs() < 1.0e-5,
                "turned {got_angle} of {}, wanted {}",
                step_angle,
                t * step_angle
            );
            assert!((pose.rot.norm() - 1.0).abs() < 1.0e-12);
            checked += 1;
        }
        assert!(checked > 60);
        if multiplier == 4 {
            assert!(multi_tick == 120, "a ×4 frame carries several ticks");
        } else if dt_s < FRAME_DT_S {
            assert!(zero_tick > 60 && multi_tick == 0);
        } else {
            assert!(zero_tick == 0 && multi_tick > 0);
        }
    }
}

/// R5: a jettison re-welds the craft under the same id — its pose
/// history must survive. The body frame travels evenly across the
/// firing, and the drawn CoM, which the sim shifts at that tick, never
/// passes the staged one.
#[test]
fn a_jettison_does_not_jump_the_craft() {
    let mut view = vacuum_view();
    let mut frames = Frames::new(1);
    let id = flying(&mut view, &mut frames, 150_000.0, 0.6, 0.0);
    // Past the first-tick hold (see `travel`).
    frames.step(&mut view);
    let mut prev_pose = drawn(&view, id);
    let mut prev_body = staged(&view, id);
    let mut steps = Vec::new();
    let mut com_shift_m = None;
    for k in 0..120 {
        if k == 60 {
            view.enqueue_command(
                SourceClass::Ui,
                PLAYER_SOURCE_ID,
                SimCommand::JettisonPart {
                    vessel: id,
                    node: csp_phys::PartNodeId(1),
                },
            );
        }
        frames.step(&mut view);
        let body = staged(&view, id);
        let pose = drawn(&view, id);
        assert_camera_follows_the_drawn_craft(&view, id, frames.last_frac);
        steps.push((pose.pos - prev_pose.pos).norm());
        // The drawn CoM is no further from the staged one than the
        // previous tick's was — never beyond it.
        assert!(
            (pose.com - body.com).norm() <= (prev_body.com - body.com).norm() + 1.0e-6,
            "the drawn CoM passed the staged one"
        );
        if body.nodes < prev_body.nodes {
            // The CoM inside the body frame — the shift the jettison
            // is, stripped of the stack's travel.
            let local = |b: &FlightBody| b.rot.conjugate().rotate(b.com - b.pos);
            com_shift_m = Some((local(&body) - local(&prev_body)).norm());
        }
        prev_pose = pose;
        prev_body = body;
    }
    assert_eq!(prev_body.nodes, 1, "the nose was jettisoned");
    assert!(
        com_shift_m.is_some_and(|m| m > 0.1),
        "the jettison must move the CoM for this test to mean anything: {com_shift_m:?}"
    );
    let v = variation(&steps);
    eprintln!("travel variation across a jettison at x1: {v:.5}");
    assert!(
        v <= TRAVEL_VARIATION_MAX,
        "the jettison frame jumped: travel varies by {v:.4}"
    );
}

/// R5: the focus vessel that converts to rails keeps staging off its
/// conic, and back. Across both edges the drawn travel stays even and
/// the camera keeps orbiting the drawn craft.
#[test]
fn a_craft_moving_between_bubble_and_rails_does_not_jump() {
    let mut view = vacuum_view();
    let mut frames = Frames::new(1);
    // Throttled, so the coasting rule keeps the clearing orbit in the
    // bubble while it takes the focus.
    let id = flying(&mut view, &mut frames, 100_000.0, 1.0, 1.0);
    // Past the first-tick hold (see `travel`).
    frames.step(&mut view);
    let throttle = |view: &mut View, t: f64| {
        view.enqueue_command(
            SourceClass::Ui,
            PLAYER_SOURCE_ID,
            SimCommand::SetThrottle {
                vessel: id,
                throttle: t,
            },
        )
    };
    let on_rails = |view: &View| {
        matches!(
            view.sim.vessels()[&id].state,
            csp_sim::VesselState::OnRails(_)
        )
    };
    let mut prev = drawn(&view, id).com;
    let mut steps = Vec::new();
    for k in 0..180 {
        match k {
            30 => throttle(&mut view, 0.0),
            90 => {
                view.enqueue_command(
                    SourceClass::Ui,
                    PLAYER_SOURCE_ID,
                    SimCommand::ToBubble { vessel: id },
                );
                throttle(&mut view, 1.0);
            }
            _ => {}
        }
        frames.step(&mut view);
        assert_camera_follows_the_drawn_craft(&view, id, frames.last_frac);
        let now = drawn(&view, id).com;
        steps.push((on_rails(&view), (now - prev).norm()));
        prev = now;
    }
    assert!(
        !steps[29].0 && steps[45].0,
        "the craft must rail once it coasts"
    );
    assert!(
        steps[89].0 && !steps[100].0,
        "ToBubble must return it to the bubble"
    );
    let travel: Vec<f64> = steps.iter().map(|s| s.1).collect();
    let v = variation(&travel);
    eprintln!("travel variation across bubble <-> rails at x1: {v:.5}");
    assert!(
        v <= TRAVEL_VARIATION_MAX,
        "a residence edge jumped: travel varies by {v:.4}"
    );
}

/// The shipped world with its starter craft berthed on the pad — the
/// app's own first launch, delivered surface-latched at the first drain.
fn pad_view() -> (View, VesselId, Frames) {
    let mut view = View::new().expect("data/ load");
    view.seed_starter_craft().expect("starter seed");
    let mut frames = Frames::new(1);
    frames.step(&mut view);
    let id = view
        .flight_focus()
        .expect("the delivered craft holds focus");
    (view, id, frames)
}

/// Where `com` sits in the planet's turning frame at spin phase `phase`
/// — a constant for anything resting on the ground.
fn body_fixed(env: &FlightEnv, com: DVec3, phase: f64) -> DVec3 {
    DQuat::from_axis_angle(env.spin_axis, -phase).rotate(com - env.body_center)
}

/// R5 + R3 on the real pad. A just-delivered craft has no previous
/// pose: its first frame draws it exactly as staged. From then on it
/// rides the ground it is drawn on — its place in the planet's turning
/// frame, taken at the DRAWN spin phase, holds to the centimetre, where
/// the staged tick's phase slides it metres back and forth — no frame's
/// travel overshoots the surface's own, and the clock the plume flicker
/// and the ocean scroll read advances with the frames, not in 64 Hz
/// steps.
#[test]
fn delivered_craft_is_drawn_at_its_pose_and_rides_the_drawn_ground() {
    let (mut view, id, mut frames) = pad_view();
    assert!(
        staged(&view, id).latched,
        "the berth delivers surface-latched"
    );
    assert_eq!(
        drawn(&view, id),
        as_drawn(&staged(&view, id)),
        "no previous pose: drawn at the current one"
    );
    // The ground's own travel in a frame — the latched craft's staged
    // velocity is the surface's co-rotation.
    let per_frame = staged(&view, id).vel.norm() * FRAME_DT_S;
    let mut prev = drawn(&view, id).com;
    let mut steps = Vec::new();
    let mut ground: Option<DVec3> = None;
    let mut tick_phase_slides_m: f64 = 0.0;
    let mut last_time: Option<f64> = None;
    for _ in 0..120 {
        frames.step(&mut view);
        let scene = view.world.resource::<DrawnScene>();
        let time = scene.time_s().expect("a frame has been drawn");
        if let Some(t0) = last_time {
            assert!(
                (time - t0 - FRAME_DT_S).abs() < 1.0e-6,
                "the drawn clock moved {} s in a frame",
                time - t0
            );
        }
        last_time = Some(time);
        let drawn_env = scene.env().expect("Terrin has physics");
        let pose = drawn(&view, id);
        steps.push((pose.com - prev).norm());
        prev = pose.com;
        let here = body_fixed(drawn_env, pose.com, drawn_env.spin_phase);
        // The same point under the phase the sim's tick stamps — the
        // ground as it was drawn before.
        let tick_phase = view
            .world
            .resource::<FlightSnapshot>()
            .env
            .as_ref()
            .expect("staged environment")
            .spin_phase;
        let off_by = (body_fixed(drawn_env, pose.com, tick_phase) - here).norm();
        tick_phase_slides_m = tick_phase_slides_m.max(off_by);
        if let Some(first) = ground {
            let drift = (here - first).norm();
            assert!(
                drift < 0.01,
                "the craft slid {drift} m over the drawn ground"
            );
        } else {
            ground = Some(here);
        }
    }
    assert!(staged(&view, id).latched, "still parked after two seconds");
    assert!(
        steps.iter().all(|d| *d <= 1.10 * per_frame),
        "a frame overshot the surface's travel: {}",
        steps
            .iter()
            .map(|d| format!("{d:.2}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let v = variation(&steps[1..]);
    eprintln!("travel variation of the parked craft on the pad, x1: {v:.5}");
    assert!(v <= TRAVEL_VARIATION_MAX, "travel varies by {v:.4}");
    assert!(
        tick_phase_slides_m > 0.5,
        "the integer-tick phase must slide the craft metres: {tick_phase_slides_m}"
    );
}

/// R5: a parked craft rides the drawn ground until a throttle command
/// unlatches it — and the frame it becomes a bubble body moves it no
/// more and no less than the frames around it.
#[test]
fn unlatching_a_parked_craft_does_not_jump() {
    let (mut view, id, mut frames) = pad_view();
    let mut prev = drawn(&view, id).com;
    let mut steps = Vec::new();
    let mut unlatched_at = None;
    for k in 0..160 {
        if k == 60 {
            view.enqueue_command(
                SourceClass::Ui,
                PLAYER_SOURCE_ID,
                SimCommand::SetThrottle {
                    vessel: id,
                    throttle: 1.0,
                },
            );
        }
        frames.step(&mut view);
        let body = staged(&view, id);
        let pose = drawn(&view, id);
        steps.push((body.latched, (pose.com - prev).norm()));
        prev = pose.com;
        if unlatched_at.is_none() && !body.latched {
            unlatched_at = Some(k);
        }
    }
    let at = unlatched_at.expect("the throttle must unlatch the parked craft");
    assert!(at >= 60, "it was parked until the command");
    assert!(
        steps[..60].iter().all(|s| s.0),
        "the craft must ride the pad until the command"
    );
    // Forty frames either side of the edge.
    assert!(
        at + 40 <= steps.len(),
        "unlatched too late to frame the edge: {at}"
    );
    let around: Vec<f64> = steps[at - 40..at + 40].iter().map(|s| s.1).collect();
    let v = variation(&around);
    eprintln!("travel variation across the unlatch at x1: {v:.5}");
    assert!(
        v <= TRAVEL_VARIATION_MAX,
        "the unlatch frame jumped: travel varies by {v:.4}"
    );
}
