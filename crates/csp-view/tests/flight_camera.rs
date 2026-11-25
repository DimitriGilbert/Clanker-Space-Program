//! The chase camera through the real input path at 60 fps —
//! `csp-app`'s frame loop without the window. `View::flight_event`
//! queues what the winit handler queues (`FlightEvent::Orbit`/`Zoom`),
//! the Input schedule drains it, `update_session` hands over the
//! frame's wall time the way `run_frame` does, and `end_frame` runs the
//! camera step.
//!
//! A jittery drag moves yaw and pitch by exactly the event deltas,
//! frame by frame; a wheel burst eases the distance without overshoot;
//! a focus change near or far moves the eye by no more than one step of
//! its travel and leaves the orbit angles alone; a loaded world is a
//! first focus for the camera, never a travel. A craft buried in the
//! ground is orbited from an anchor lifted over it, the camera looking at
//! the craft — a craft at least an eye distance down so that every drag
//! moves the eye as freely as in flight; a focus change from such a craft
//! moves nothing on its first frame, and a loaded one is a first focus
//! too. A craft in the water is orbited from its own CoM: the surface is
//! no floor, and the eye goes under it, down to 0.3 m over the seabed. Dragged straight down to
//! the ground, the eye never comes inside the craft's least distance: it
//! goes round the craft on the floor at exactly that distance, and
//! zooming in never backs it away from the craft. A focus travel whose
//! orbit point crosses the new craft's keep-out carries the eye round it
//! without a jump and without a lunge — in the air as on the ground, over
//! the new craft or round its yaw's own side on the ball of its least
//! distance, never through it — and the previous craft's skirt, carried
//! along, is kept out of the new craft too. From a kept-out eye to a craft
//! away from the camera's side the eye goes round by the NEARER way — the
//! fading skirt used to send it the long way round, metres in one frame.
//! The ride's pace is the eye's own: no frame moves it more than a tenth
//! beyond the one before, in any placement — the way round the craft
//! being longer than the orbit point's run, the eye used to ride that
//! much faster from the frame it entered. And where the eye itself begins
//! a change inside the new craft's least distance, it leaves the ball by
//! the nearest way out and only moves away from the craft's centre until
//! it is out — it used to be handed over along the orbit point's way,
//! under the craft. Through a far focus change the shadow cascade's up
//! stays the focused craft's own, on every frame.
#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::expect_used)]

use std::collections::BTreeMap;

use csp_core::{DQuat, DVec3};
use csp_sim::orbit::{BodyOrbit, PatchedConics};
use csp_sim::{DEFAULT_BUBBLE_RADIUS_M, PhysicsConfig, SimCommand, VesselId, World as SimWorld};
use csp_view::flight::{FlightBody, FlightCameraF64, FlightEvent, FlightInput};
use csp_view::{PLAYER_SOURCE_ID, SourceClass, View};

/// The frame's wall time — 60 fps.
const FRAME_MS: f64 = 1000.0 / 60.0;
/// The camera's zoom easing half-life (s), mirrored from
/// `csp-view/src/flight.rs: EASE_HALF_LIFE_S`.
const HALF_LIFE_S: f64 = 0.080;
/// A focus travel is done under this (m) — `flight.rs: TRAVEL_DONE_M`.
const TRAVEL_DONE_M: f64 = 1.0e-3;
/// …and takes at most this long (s), whatever its length — `flight.rs:
/// TRAVEL_SETTLE_S`.
const TRAVEL_SETTLE_S: f64 = 0.5;
/// The eye's clearance over the ground (m) — `flight.rs:
/// EYE_GROUND_CLEARANCE_M`.
const EYE_GROUND_CLEARANCE_M: f64 = 0.3;
/// The orbit pitch limit (rad) — `flight.rs: PITCH_LIMIT`.
const PITCH_LIMIT: f64 = 89.0 * std::f64::consts::PI / 180.0;
/// The chase camera's vertical FOV (rad) — `flight.rs: FLIGHT_FOV_Y`.
const FOV_Y: f64 = 0.9;
/// The flight viewport every test here solves for (px).
const VIEWPORT_PX: (f64, f64) = (320.0, 240.0);
/// The chord between the two crafts of the far focus change (m) — the
/// distance the defect was measured at.
const CHORD_M: f64 = 850_600.0;
/// `csp-app`'s zoom per wheel line (`ZOOM_LN_PER_LINE`): the distance
/// target is multiplied by `exp(−0.18)` per line scrolled up.
const ZOOM_LN_PER_LINE: f64 = 0.18;

/// A 600 km vacuum body turning once an hour, centred on the bubble
/// origin. Its ground is the bare datum (no relief), so the camera's
/// floor is exactly the clearance over it — 0.3 m over the surface, the
/// seabed included — whether or not the background heightfield bake has
/// landed.
const R_M: f64 = 6.0e5;
const MU: f64 = 3.5316e12;
const SPIN_RAD_S: f64 = 2.0 * std::f64::consts::PI / 3600.0;

/// The share of what is left that one 60 fps frame closes.
fn share() -> f64 {
    1.0 - (-(FRAME_MS / 1000.0) / HALF_LIFE_S).exp2()
}

/// The half-life (s) of a focus travel of `d0_m` — `flight.rs:
/// `travel_half_life`: the zoom's, shortened so that every travel is
/// done in half a second.
fn travel_half_life_s(d0_m: f64) -> f64 {
    HALF_LIFE_S.min(TRAVEL_SETTLE_S / (d0_m / TRAVEL_DONE_M).log2())
}

/// The share of a `d0_m` travel that one 60 fps frame closes.
fn travel_share(d0_m: f64) -> f64 {
    1.0 - (-(FRAME_MS / 1000.0) / travel_half_life_s(d0_m)).exp2()
}

/// The number of frames after which a `d0_m` travel's remainder is
/// exactly a millimetre — done on the frame that first drops under it.
fn travel_frames(d0_m: f64) -> f64 {
    (TRAVEL_DONE_M / d0_m).log2() / (1.0 - travel_share(d0_m)).log2()
}

fn body_view(surface_r: f64, ocean_datum_m: Option<f64>, spin_rad_s: f64) -> View {
    let mut bodies = BTreeMap::new();
    bodies.insert(
        csp_data::BodyId::from("p"),
        BodyOrbit {
            mu_m3_per_s2: MU,
            soi_m: None,
            parent: None,
            elements: None,
        },
    );
    let sim = SimWorld::with_physics(
        PatchedConics::new(bodies),
        PhysicsConfig {
            environment: csp_phys::Environment {
                body_center: DVec3::ZERO,
                radius_m: surface_r,
                mu_m3_per_s2: MU,
                body_spin: DVec3::Z * spin_rad_s,
                atmosphere: None,
                settle_v: csp_phys::DEFAULT_SETTLE_V,
                settle_w: csp_phys::DEFAULT_SETTLE_W,
                settle_ticks: csp_phys::DEFAULT_SETTLE_TICKS,
                friction_mu: csp_phys::DEFAULT_FRICTION_MU,
                drag_cd: csp_phys::DEFAULT_DRAG_CD,
                convective_heat_fraction: csp_phys::DEFAULT_CONVECTIVE_HEAT_FRACTION,
                terrain: None,
                ocean: None,
            },
            dominant: csp_data::BodyId::from("p"),
            bubble_radius_m: DEFAULT_BUBBLE_RADIUS_M,
        },
        1024,
    );
    let mut view = View::with_sim(sim);
    view.world
        .insert_resource(csp_view::terrain::TerrainConfig {
            bodies: BTreeMap::from([(
                "p".to_owned(),
                csp_view::terrain::TerrainBody {
                    seed: 0,
                    amplitude_m: 0.0,
                    profile: csp_core::terrain::ReliefProfile::Legacy,
                    ocean_datum_m,
                    biome: None,
                },
            )]),
            ..csp_view::terrain::TerrainConfig::default()
        });
    view
}

/// The dry body: its surface on the datum `R_M`.
fn vacuum_view() -> View {
    body_view(R_M, None, SPIN_RAD_S)
}

/// The same body standing still: a craft spawned at rest stays where it
/// was put, tick after tick.
fn quiet_view() -> View {
    body_view(R_M, None, 0.0)
}

/// The same body under a 6.2 m ocean with its surface on the datum:
/// the camera's floor is the seabed's, 0.3 m over it — 5.9 m under the
/// surface.
fn flooded_view() -> View {
    body_view(R_M - 6.2, Some(R_M), SPIN_RAD_S)
}

/// One box part of `mass_kg`.
fn box_part(mass_kg: f64, half: DVec3) -> csp_phys::WeldPart {
    csp_phys::WeldPart {
        experiments: Vec::new(),
        lab: None,
        engine: None,
        parachute: None,
        dock_ports: vec![],
        tanks: vec![],
        crew_seats: 0,
        structural: csp_data::StructuralDef::unlimited(),
        mass_kg,
        com: DVec3::ZERO,
        inertia: csp_phys::SymMat3::solid_box(mass_kg, half),
        collider: csp_phys::ColliderShape::Box { half_extents: half },
        force_sites: Vec::new(),
        aero: csp_data::AeroDef::constant(0.0, 0.0, 1.0),
        thermal: csp_data::ThermalDef {
            heat_capacity_j_per_k: 400_000.0,
            max_temp_k: 1_100.0,
            emissivity: 0.8,
            conduction_w_per_k: 60.0,
        },
    }
}

/// Two equal 200 kg boxes — a 2 × 2 × 2 m base and a 2 × 2 × 1 m nose
/// three metres up — so the craft's CoM sits exactly 1.5 m up its axis
/// and its bounding radius is the base's far corner,
/// `sqrt(1² + 1² + 2.5²) = sqrt(6.75) = 2.598 m`.
const RADIUS_M: f64 = 2.598_076_211_353_316;
/// A single box of half-extents `half`: one part at the root, its CoM
/// the craft's own centre.
fn single_box(half: DVec3, mass_kg: f64) -> csp_sim::CraftDef {
    use csp_phys::{PartNode, PartNodeId, PartTransform};
    let mut graph = csp_phys::PartGraph::new(PartNodeId(0));
    graph.insert(
        PartNodeId(0),
        PartNode::new(None, PartTransform::IDENTITY, 0),
    );
    csp_sim::CraftDef {
        part_build_resources: Vec::new(),
        hash_memo: Default::default(),
        part_ids: vec![],
        graph,
        parts: vec![box_part(mass_kg, half)],
        part_costs: vec![0.0],
        reliability: vec![],
    }
}

/// A 2 × 2 × 2 m cube: its CoM at its own centre, 1 m up from the face
/// it stands on, and its bounding radius `sqrt(3) = 1.732 m`.
const CUBE_RADIUS_M: f64 = 1.732_050_807_568_877_2;

/// A 4 × 4 × 0.2 m slab lying flat: its CoM 0.1 m over the ground —
/// under the camera's floor, 0.3 m over it — and its bounding radius
/// `sqrt(2² + 2² + 0.1²) = 2.830 m`.
const SLAB_RADIUS_M: f64 = 2.830_194_339_616_981;

fn cube() -> csp_sim::CraftDef {
    single_box(DVec3::new(1.0, 1.0, 1.0), 400.0)
}

fn slab() -> csp_sim::CraftDef {
    single_box(DVec3::new(2.0, 2.0, 0.1), 400.0)
}

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
            box_part(200.0, DVec3::new(0.5, 0.5, 1.0)),
            box_part(200.0, DVec3::new(0.5, 0.5, 0.5)),
        ],
        part_costs: vec![0.0, 0.0],
        reliability: vec![],
    }
}

/// Spawn a craft at rest with its CoM at `pos`. Its id is the next one
/// minted — the vessel list is read after the spawn's own frame.
fn spawn(view: &mut View, pos: DVec3) {
    spawn_craft(view, stack(), pos);
}

/// Spawn `craft` at rest with its CoM at `pos`.
fn spawn_craft(view: &mut View, craft: csp_sim::CraftDef, pos: DVec3) {
    spawn_craft_moving(view, craft, pos, DVec3::ZERO);
}

/// Spawn `craft` with its CoM at `pos`, moving at `vel`.
fn spawn_craft_moving(view: &mut View, craft: csp_sim::CraftDef, pos: DVec3, vel: DVec3) {
    view.enqueue_command(
        SourceClass::Ui,
        PLAYER_SOURCE_ID,
        SimCommand::SpawnPhysicalVessel {
            craft,
            pos,
            rot: DQuat::IDENTITY,
            vel,
            ang_vel: DVec3::ZERO,
        },
    );
}

/// The ground's own velocity at `pos` — the body spins about +Z — so a
/// craft spawned with it rests on the ground instead of skidding.
fn ground_velocity(pos: DVec3) -> DVec3 {
    DVec3::new(-SPIN_RAD_S * pos.y, SPIN_RAD_S * pos.x, 0.0)
}

/// A craft that rests ON the ground, through the real input path: spawned
/// with its CoM at `pos` riding with the ground, stepped until it has
/// settled and latched (128 quiet ticks in contact), then left alone and
/// framed. The sim is not stepped again. Returns the view and the
/// craft's id.
fn settled_view(craft: csp_sim::CraftDef, pos: DVec3) -> (View, VesselId) {
    let mut view = vacuum_view();
    view.set_flight_viewport(VIEWPORT_PX.0, VIEWPORT_PX.1);
    spawn_craft_moving(&mut view, craft, pos, ground_velocity(pos));
    frame(&mut view);
    let id = view.flight_focus().expect("the craft takes the focus");
    let mut latched = false;
    for _ in 0..600 {
        frame(&mut view);
        if view.sim.snapshot().is_ok() {
            latched = true;
            break;
        }
    }
    assert!(latched, "the craft never settled and latched on the ground");
    for _ in 0..400 {
        still_frame(&mut view);
    }
    (view, id)
}

/// Spawn a craft at `pos` with velocity `vel`.
fn spawn_moving(view: &mut View, pos: DVec3, vel: DVec3) {
    view.enqueue_command(
        SourceClass::Ui,
        PLAYER_SOURCE_ID,
        SimCommand::SpawnPhysicalVessel {
            craft: stack(),
            pos,
            rot: DQuat::IDENTITY,
            vel,
            ang_vel: DVec3::ZERO,
        },
    );
}

/// Spawn a coasting craft on a circular orbit of radius `orbit_r`,
/// `theta` round the equator from +X. It coasts above the bubble, so
/// the sim puts it on rails by itself.
fn spawn_orbiting(view: &mut View, orbit_r: f64, theta: f64) {
    let speed = (MU / orbit_r).sqrt();
    let dir = DVec3::new(theta.cos(), theta.sin(), 0.0);
    let along = DVec3::new(-theta.sin(), theta.cos(), 0.0);
    spawn_moving(view, dir * orbit_r, along * speed);
}

/// One frame of `csp-app`'s loop without the window: the frame's wall
/// time goes in the way `run_frame` hands it over, the queued camera
/// events drain through the Input schedule, the sim steps one tick, the
/// frame ends.
fn frame(view: &mut View) {
    view.update_session(FRAME_MS, 1, 1, None);
    view.begin_frame();
    view.sim_step(view.sim.clock().ticks() + 1)
        .expect("sim step");
    view.set_tick_fraction(0.0);
    view.end_frame();
}

struct Shot {
    eye: DVec3,
    /// The backward axis (unit): anchor → eye. The camera looks along
    /// its negative.
    back: DVec3,
    up: DVec3,
    distance_m: f64,
}

fn shot(view: &View) -> Shot {
    let cam = &view.world.resource::<FlightCameraF64>().0;
    assert!(
        cam.valid,
        "the chase camera must be solved for a staged craft"
    );
    // The reverse-Z projection's third row is `[0, 0, −1, 0]`, so the
    // matrix's last row is minus the BACKWARD axis — it is the forward
    // the camera looks along — and its second row is the up vector;
    // both unit once normalized.
    let back = -DVec3::new(
        cam.view_proj[3][0],
        cam.view_proj[3][1],
        cam.view_proj[3][2],
    );
    let up = DVec3::new(
        cam.view_proj[1][0],
        cam.view_proj[1][1],
        cam.view_proj[1][2],
    );
    Shot {
        eye: cam.eye,
        back,
        up: up / up.norm(),
        distance_m: cam.distance_m,
    }
}

/// `(yaw, pitch, distance, target distance)` after this frame.
fn angles_and_distances(view: &View) -> (f64, f64, f64, f64) {
    let input = view.world.resource::<FlightInput>();
    (
        input.yaw,
        input.pitch,
        input.distance_m(),
        input.distance_target_m(),
    )
}

/// The vessel as staged this frame.
fn staged(view: &View, id: VesselId) -> FlightBody {
    view.world
        .resource::<csp_view::flight::FlightSnapshot>()
        .vessels
        .iter()
        .find(|b| b.id == id.0)
        .cloned()
        .expect("staged")
}

/// The point the chase camera orbits this frame, recovered from its
/// matrix by the camera's own convention — `distance_m` is the actual
/// eye–anchor distance: `eye − back · distance`.
fn anchor(shot: &Shot) -> DVec3 {
    shot.eye - shot.back * shot.distance_m
}

/// The unit direction from the camera's anchor to its eye at
/// `(yaw, pitch)`, in the local-level frame at the anchor (the body is
/// centred on the origin).
fn orbit_dir(at: DVec3, yaw: f64, pitch: f64) -> DVec3 {
    let up = at / at.norm();
    let north = DVec3::Z - up * up.z;
    let north = north / north.norm();
    let east = north.cross(up);
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    north * (cy * cp) + east * (sy * cp) + up * sp
}

fn close(actual: f64, want: f64, tol: f64, what: &str) {
    assert!(
        (actual - want).abs() <= tol,
        "{what}: {actual} vs {want} (tol {tol})"
    );
}

/// Queue `lines` wheel notches — up (zoom in) or down (out) — with the
/// factor `csp-app` computes per line: scroll up shortens the distance.
fn wheel(view: &mut View, lines: usize, in_: bool) {
    let factor = if in_ {
        (-ZOOM_LN_PER_LINE).exp()
    } else {
        ZOOM_LN_PER_LINE.exp()
    };
    for _ in 0..lines {
        view.flight_event(FlightEvent::Zoom { factor });
    }
}

/// One frame, then the zoom invariant for it: the distance stepped
/// toward the target by exactly its share of what was left — never the
/// wrong way, never past it. Returns the new distance.
fn step_checked(view: &mut View, previous: f64, what: &str) -> f64 {
    frame(view);
    let (_, _, now, target) = angles_and_distances(view);
    let left = (target - previous).abs();
    assert!(
        (now - previous) * (target - previous) > 0.0,
        "{what}: {previous} -> {now} with the target at {target} stepped the wrong way or stalled"
    );
    assert!(
        (now - target).abs() < left,
        "{what}: {previous} -> {now} overshot the target {target}"
    );
    close(
        (now - previous).abs(),
        share() * left,
        1.0e-9,
        &format!("{what}: easing step"),
    );
    now
}

/// Run `frames` checked frames — the zoom easing all the while — and
/// return the distance.
fn settle(view: &mut View, frames: usize, what: &str) -> f64 {
    let mut distance = angles_and_distances(view).2;
    for _ in 0..frames {
        distance = step_checked(view, distance, what);
    }
    distance
}

/// Let the framing settle, scroll eight lines in, let that settle: the
/// camera sits on the distance floor. Returns the distance.
fn zoom_to_the_floor(view: &mut View) -> f64 {
    let distance = settle(view, 150, "framing");
    wheel(view, 8, true);
    step_checked(view, distance, "burst in");
    settle(view, 150, "zoomed in")
}

/// The farthest collider-AABB corner of `body` from its CoM.
fn farthest_corner(body: &FlightBody) -> f64 {
    body.parts
        .iter()
        .flat_map(|p| {
            [p.lo.x, p.hi.x].into_iter().flat_map(move |x| {
                [p.lo.y, p.hi.y].into_iter().flat_map(move |y| {
                    [p.lo.z, p.hi.z].into_iter().map(move |z| {
                        (body.pos + body.rot.rotate(DVec3::new(x, y, z)) - body.com).norm()
                    })
                })
            })
        })
        .fold(0.0, f64::max)
}

/// `flight.rs: travel_blend` — how much of what the previous focus left
/// in hand a travel still carries: everything while more than an eye
/// distance of it is left, nothing once it is done, and between those the
/// share of that last eye distance which is left.
fn travel_blend(remaining_m: f64, start_m: f64, eye_m: f64) -> f64 {
    let reach_m = eye_m.min(start_m);
    if reach_m <= TRAVEL_DONE_M {
        return 0.0;
    }
    ((remaining_m - TRAVEL_DONE_M) / (reach_m - TRAVEL_DONE_M)).clamp(0.0, 1.0)
}

/// Drive frames until a focus travel of `travel_m` metres is done — each
/// frame closing the same share of what is left — and one more. The
/// camera looks at the travelling anchor until the last eye distance of
/// the travel, then blends its look-at to the craft. Returns how many
/// frames it took.
fn travel_closes(view: &mut View, first: VesselId, travel_m: f64) -> usize {
    let k = travel_share(travel_m);
    let mut left = travel_m * (1.0 - k);
    let mut frames = 0;
    while left >= TRAVEL_DONE_M {
        frame(view);
        left *= 1.0 - k;
        frames += 1;
        let look_gap = (anchor(&shot(view)) - staged(view, first).com).norm();
        // The frame the remainder drops under a millimetre, the anchor
        // snaps to the CoM and the camera looks at the craft.
        let want = if left >= TRAVEL_DONE_M {
            left * travel_blend(left, travel_m, angles_and_distances(view).2)
        } else {
            0.0
        };
        close(look_gap, want, 1.0e-6, "travel closing");
        assert!(frames < 400, "the travel never ended");
    }
    frame(view);
    close(
        (anchor(&shot(view)) - staged(view, first).com).norm(),
        0.0,
        1.0e-9,
        "travel done: the camera looks at the drawn CoM",
    );
    frames
}

/// R1/R8 at the defaults: a stuttering drag — one move, none, three in
/// a frame, a reversal — changes yaw and pitch by exactly the event
/// deltas every frame, and the eye sits that same frame exactly where
/// the angles put it. Nothing smooths or delays the drag.
#[test]
fn a_jittery_drag_follows_the_pointer_exactly() {
    let mut view = vacuum_view();
    spawn(&mut view, DVec3::new(R_M + 1500.0, 0.0, 0.0));
    frame(&mut view);
    let id = view.flight_focus().expect("the craft takes the focus");
    // Let the framing settle, so the eye travel below is the drag's alone.
    let distance = settle(&mut view, 150, "framing");
    let script: [&[(f64, f64)]; 13] = [
        &[(0.0613, -0.0241)],
        &[],
        &[(0.0173, 0.0091), (-0.0402, 0.0333), (0.0007, -0.0004)],
        &[(-0.0251, 0.0144)],
        &[],
        &[(0.0998, -0.0503), (0.0001, 0.0002)],
        &[(0.0, 0.0)],
        &[(-0.0777, 0.0441)],
        &[],
        &[(0.0122, 0.0088), (0.0031, -0.0017), (0.0290, 0.0155)],
        &[(-0.0064, 0.0022)],
        &[],
        &[(-0.0041, 0.0183)],
    ];
    let (mut yaw, mut pitch, _, _) = angles_and_distances(&view);
    let mut moved = Vec::new();
    let mut last_eye = shot(&view).eye;
    for (i, events) in script.iter().enumerate() {
        for &(dyaw, dpitch) in events.iter() {
            view.flight_event(FlightEvent::Orbit { dyaw, dpitch });
            yaw += dyaw;
            pitch += dpitch;
        }
        frame(&mut view);
        let (now_yaw, now_pitch, distance, _) = angles_and_distances(&view);
        assert_eq!((now_yaw, now_pitch), (yaw, pitch), "frame {i}");
        // The eye, this frame, is the drawn CoM plus the orbit direction
        // at the eased distance — to the last digit.
        let com = staged(&view, id).com;
        let dir = orbit_dir(com, yaw, pitch);
        let eye = shot(&view).eye;
        let offset = eye - com;
        close(offset.norm(), distance, 1.0e-9, "eye distance");
        close(
            (offset / offset.norm()).dot(dir),
            1.0,
            1.0e-12,
            "eye direction",
        );
        moved.push((eye - last_eye).norm());
        last_eye = eye;
    }
    eprintln!(
        "drag: 13 frames at {distance:.3} m, {} events, yaw {yaw:.4} pitch {pitch:.4} rad; eye travel per frame {:?} m",
        script.iter().map(|e| e.len()).sum::<usize>(),
        moved.iter().map(|m| format!("{m:.3}")).collect::<Vec<_>>()
    );
}

/// R2/R3/R4 at 60 fps: the first focus frames the craft at four radii
/// and the camera eases there from its 60 m start. The staged bounding
/// radius is the base box's far corner from the CoM. A wheel burst in
/// eases to the distance floor (1.5 radii), a burst out eases away from
/// it — every frame one share of what is left, never past the target.
#[test]
fn a_wheel_burst_eases_the_zoom_without_overshoot() {
    let mut view = vacuum_view();
    spawn(&mut view, DVec3::new(R_M + 1500.0, 0.0, 0.0));
    frame(&mut view);
    let id = view.flight_focus().expect("the craft takes the focus");

    let far = farthest_corner(&staged(&view, id));
    close(
        far,
        RADIUS_M,
        1.0e-9,
        "farthest collider corner from the CoM",
    );
    let radii = view
        .world
        .resource::<csp_view::flight::CraftRadii>()
        .0
        .clone();
    assert_eq!(radii.len(), 1, "one staged craft, one radius");
    close(radii[0].1, RADIUS_M, 1.0e-9, "staged bounding radius");

    // The spawn's own frame is the first focus: the target is four
    // radii and the camera has already eased one 60 fps step from 60 m.
    let target = angles_and_distances(&view).3;
    close(
        target,
        4.0 * RADIUS_M,
        1.0e-9,
        "first focus: framed at four radii",
    );
    close(
        angles_and_distances(&view).2,
        60.0 + (4.0 * RADIUS_M - 60.0) * share(),
        1.0e-9,
        "the camera eased one step from where it was put",
    );
    let mut distance = settle(&mut view, 150, "framing");
    close(
        distance,
        4.0 * RADIUS_M,
        1.0e-6,
        "settled on the frame distance",
    );

    // Eight lines in, four per frame: the wheel asks for
    // 4 radii × e^−1.44 = 2.46 m, inside the craft — the target stops at
    // 1.5 radii.
    wheel(&mut view, 4, true);
    distance = step_checked(&mut view, distance, "burst in, frame 1");
    wheel(&mut view, 4, true);
    step_checked(&mut view, distance, "burst in, frame 2");
    let target = angles_and_distances(&view).3;
    close(
        target,
        1.5 * RADIUS_M,
        1.0e-9,
        "the target stops at the distance floor",
    );
    assert!(4.0 * RADIUS_M * (-8.0 * ZOOM_LN_PER_LINE).exp() < 1.5 * RADIUS_M);
    distance = settle(&mut view, 150, "zoomed in");
    close(distance, 1.5 * RADIUS_M, 1.0e-6, "settled on the floor");
    let settled_at = distance;

    // Twelve lines out in one frame.
    wheel(&mut view, 12, false);
    step_checked(&mut view, distance, "burst out");
    let target = angles_and_distances(&view).3;
    let asked = 1.5 * RADIUS_M * (12.0 * ZOOM_LN_PER_LINE).exp();
    close(target, asked, 1.0e-9, "the target is twelve lines out");
    distance = settle(&mut view, 120, "zoomed out");
    close(distance, asked, 1.0e-3, "settled twelve lines out");
    eprintln!(
        "zoom: start 60 m -> frame {:.3} m -> floor {settled_at:.3} m -> out {distance:.3} m; share per frame {:.4}",
        4.0 * RADIUS_M,
        share()
    );
}

/// R4/R7 at 60 fps: the focused craft leaves and the other one, 20 m
/// off, takes the focus. The new craft is framed at four radii; the eye
/// moves by exactly one step of the travel and of the reframe — no jump
/// — and the view turns only because "up" is the radial at the anchor,
/// which moved that step. The travel then closes by the same share each
/// frame (its own half-life, shorter than the zoom's here) until the
/// camera orbits the new craft's drawn CoM.
#[test]
fn a_focus_change_moves_the_eye_by_at_most_one_easing_step() {
    let mut view = vacuum_view();
    spawn(&mut view, DVec3::new(R_M + 1500.0, 0.0, 0.0));
    spawn(&mut view, DVec3::new(R_M + 1500.0, 20.0, 0.0));
    frame(&mut view);
    let ids: Vec<VesselId> = view.sim.vessels().keys().copied().collect();
    assert_eq!(ids.len(), 2, "two crafts staged");
    let [first, second] = [ids[0], ids[1]];
    assert_eq!(
        view.flight_focus(),
        Some(second),
        "the newest spawn takes the focus"
    );

    // Zoomed to the floor, so the reframe has distance to ease.
    let distance = zoom_to_the_floor(&mut view);
    close(distance, 1.5 * RADIUS_M, 1.0e-6, "settled at the floor");

    // The frame before the change.
    frame(&mut view);
    let before = shot(&view);
    let anchor_before = anchor(&before);
    close(
        (anchor_before - staged(&view, second).com).norm(),
        0.0,
        1.0e-9,
        "at rest the camera orbits the drawn CoM",
    );

    // The focused craft leaves: the other one takes the focus.
    view.enqueue_command(
        SourceClass::Ui,
        PLAYER_SOURCE_ID,
        SimCommand::TerminateVessel { vessel: second },
    );
    frame(&mut view);
    assert_eq!(view.flight_focus(), Some(first));
    let after = shot(&view);
    let (anchor_after, com) = (anchor(&after), staged(&view, first).com);
    let distance_after = angles_and_distances(&view).2;
    close(
        angles_and_distances(&view).3,
        4.0 * RADIUS_M,
        1.0e-9,
        "the new focus is framed at four radii",
    );

    // What the camera had in hand: the travel from where it orbited to
    // the new craft, and the reframe from the distance it was at.
    let (travel, reframe) = (anchor_before - com, 4.0 * RADIUS_M - distance);
    let travel_k = travel_share(travel.norm());
    let anchor_step = (anchor_after - anchor_before).norm();
    close(
        anchor_step,
        travel_k * travel.norm(),
        1.0e-9,
        "the anchor moved one step of the travel",
    );
    close(
        distance_after - distance,
        share() * reframe,
        1.0e-9,
        "the distance moved one easing step of the reframe",
    );
    let level_turn = anchor_step / anchor_before.norm();
    let bound = travel_k * (travel.norm() + reframe.abs()) + distance_after * level_turn;
    let moved = (after.eye - before.eye).norm();
    assert!(
        moved <= bound + 1.0e-9,
        "the eye moved {moved} m, more than one step of the travel ({bound} m)"
    );
    eprintln!(
        "focus change: travel {:.3} m, reframe {:.3} m, travel share {travel_k:.4} -> eye moved {moved:.4} m (bound {bound:.4}); level turn {level_turn:.2e} rad",
        travel.norm(),
        reframe
    );
    // Orientation: backward axis and up turn by no more than the level
    // frame did.
    let turned = |a: DVec3, b: DVec3| (a - b).norm();
    assert!(
        turned(after.back, before.back) <= level_turn + 1.0e-9,
        "the backward axis turned {}",
        turned(after.back, before.back)
    );
    assert!(
        turned(after.up, before.up) <= level_turn + 1.0e-9,
        "up turned {}",
        turned(after.up, before.up)
    );

    // Every later frame closes the same share of the travel; under a
    // millimetre it is done and the camera orbits the drawn CoM exactly.
    let frames = travel_closes(&mut view, first, travel.norm());
    eprintln!(
        "focus change: the travel closed in {frames} frames ({:.2} s)",
        frames as f64 / 60.0
    );
}

/// The scene the far change was measured on: a craft staged 1 500 m up
/// on +X, zoomed to the distance floor, and a craft on rails a chord of
/// [`CHORD_M`] away — a circular orbit 250 km up, the angle round it
/// fixed by the law of cosines. The orbiting craft coasts above the
/// bubble, so the sim rails it off by itself and it does not take the
/// focus. Returns the two ids.
fn far_change_scene(view: &mut View) -> (VesselId, VesselId) {
    let staged_pos = DVec3::new(R_M + 1500.0, 0.0, 0.0);
    spawn(view, staged_pos);
    frame(view);
    let on_pad = view
        .flight_focus()
        .expect("the staged craft takes the focus");
    let orbit_r = R_M + 250_000.0;
    let (a, b) = (staged_pos.norm(), orbit_r);
    let theta = ((a * a + b * b - CHORD_M * CHORD_M) / (2.0 * a * b)).acos();
    spawn_orbiting(view, orbit_r, theta);
    for _ in 0..4 {
        frame(view);
    }
    let far = *view
        .sim
        .vessels()
        .keys()
        .next_back()
        .expect("the orbiting craft is in the arena");
    assert!(
        matches!(
            view.sim.vessels()[&far].state,
            csp_sim::VesselState::OnRails(_)
        ),
        "a coasting craft above the bubble rails off by itself"
    );
    // Zoomed to the floor, so the reframe has distance to ease.
    zoom_to_the_floor(view);
    frame(view);
    assert_eq!(
        view.flight_focus(),
        Some(on_pad),
        "a craft on rails does not take the focus until recovered"
    );
    (on_pad, far)
}

/// Step a focus travel to its end, a frame at a time — the frame of the
/// change and the one timed frame after it have already run — holding
/// every frame to what a far change must keep: the orbit angles bit-identical, the anchor on or over the
/// ground, the eye a whole eye distance over the floor and exactly where
/// the angles put it about the anchor, and the look-at blending from the
/// travelling anchor to the craft over the last eye distance. `start` is
/// the anchor the eye orbited before the change. Returns the frames it
/// took, the anchor's distance to the new craft after the first of them,
/// and the lowest the eye came over the floor. `start` is the anchor the
/// eye orbited before the change and `com_at_change` the new craft's CoM
/// on the frame of the change; the frame after it has already run.
fn checked_travel(
    view: &mut View,
    to: VesselId,
    angles: (f64, f64),
    start: DVec3,
    com_at_change: DVec3,
) -> (usize, f64, f64) {
    let floor_r = R_M + EYE_GROUND_CLEARANCE_M;
    let (yaw, pitch) = angles;
    // The anchor's offset from the craft eases by the travel's own share.
    let d0_m = (start - com_at_change).norm();
    let k = travel_share(d0_m);
    let mut offset = (start - com_at_change) * (1.0 - k);
    let (mut frames, mut lowest_eye_m, mut first_gap_m) = (0, f64::MAX, 0.0);
    while offset != DVec3::ZERO {
        frames += 1;
        assert!(frames <= 64, "the travel never ended");
        if frames > 1 {
            offset = offset * (1.0 - k);
            frame(view);
            if offset.norm() < TRAVEL_DONE_M {
                offset = DVec3::ZERO;
            }
        }
        let now = shot(view);
        let com = staged(view, to).com;
        let (now_yaw, now_pitch, distance, _) = angles_and_distances(view);
        assert_eq!(
            (now_yaw, now_pitch),
            angles,
            "frame {frames}: the far change touched the orbit angles"
        );
        let travelling = travelling_anchor(com + offset, offset.norm(), distance, floor_r);
        assert!(
            travelling.norm() >= floor_r - 1.0e-6,
            "frame {frames}: the anchor is {} m under the ground",
            floor_r - travelling.norm()
        );
        // Over the flat body the lifted anchor sits one eye distance over
        // the floor, so the eye — further out along its orbit — is at
        // least that high: it never comes down to the ground.
        assert!(
            now.eye.norm() - floor_r >= distance - 1.0e-6,
            "frame {frames}: the eye came down to {} m over the ground, inside one eye distance ({distance} m)",
            now.eye.norm() - floor_r
        );
        close(
            (now.eye - (travelling + orbit_dir(travelling, yaw, pitch) * distance)).norm(),
            0.0,
            1.0e-5,
            &format!("frame {frames}: the eye is not where the angles put it"),
        );
        let look_gap_m = (anchor(&now) - com).norm();
        close(
            look_gap_m,
            (travelling - com).norm() * travel_blend(offset.norm(), d0_m, distance),
            1.0e-6,
            &format!(
                "frame {frames}: the look-at is not the travelling anchor blending to the craft"
            ),
        );
        lowest_eye_m = lowest_eye_m.min(now.eye.norm() - floor_r);
        if frames == 1 {
            first_gap_m = (travelling - com).norm();
        }
    }
    (frames, first_gap_m, lowest_eye_m)
}

/// R7 for a FAR change — the one the defect was measured on. The staged
/// craft holds the focus; a craft on rails a chord of 850.6 km away is
/// recovered with `View::recover_control`, which pins it, and takes the
/// focus when it stages. From then on: the anchor is never under the
/// ground on the way, the eye stays exactly where the orbit angles put
/// it (nothing clamps it to the floor, nothing pushes it out — it never
/// comes down to the ground), yaw and pitch are bit-identical, the
/// travel is done in the frames its own half-life says — half a second
/// — and the camera then orbits the recovered craft's drawn CoM.
#[test]
fn a_far_focus_change_keeps_the_orbit_and_crosses_above_the_ground() {
    let mut view = vacuum_view();
    let (on_pad, far) = far_change_scene(&mut view);
    let before = shot(&view);
    // At rest the camera orbits the drawn CoM — that CoM itself is the
    // anchor the camera stored, to the last digit.
    let anchor_before = staged(&view, on_pad).com;
    close(
        (anchor(&before) - anchor_before).norm(),
        0.0,
        1.0e-9,
        "at rest the camera orbits the drawn CoM",
    );
    let (yaw, pitch, _, _) = angles_and_distances(&view);

    // The press: pin + `ToBubble`, converted and staged next drain.
    assert_eq!(view.recover_control(), Some(far));
    frame(&mut view);
    assert_eq!(
        view.flight_focus(),
        Some(far),
        "the recovered craft took the focus"
    );
    let d0_m = (anchor_before - staged(&view, far).com).norm();
    assert!(
        (d0_m - CHORD_M).abs() < 20_000.0,
        "the crafts are {d0_m:.0} m apart, not about {CHORD_M} m"
    );

    let com_at_change = staged(&view, far).com;
    let (frames, first_gap_m, lowest_eye_m) =
        checked_travel(&mut view, far, (yaw, pitch), anchor_before, com_at_change);
    let done_at = travel_frames(d0_m);
    assert!(
        frames as f64 >= done_at - 1.0e-9 && frames as f64 <= done_at + 1.0 + 1.0e-9,
        "an {d0_m:.0} m travel at half-life {:.1} ms took {frames} frames; a millimetre is left at frame {done_at}",
        travel_half_life_s(d0_m) * 1000.0
    );
    let t_s = frames as f64 * FRAME_MS / 1000.0;
    assert!(
        t_s <= TRAVEL_SETTLE_S + FRAME_MS / 1000.0 + 1.0e-9,
        "the travel took {t_s:.3} s"
    );
    eprintln!(
        "far focus change: {d0_m:.1} m, half-life {:.2} ms, share {:.4}; first frame left {first_gap_m:.3} m; \
         lowest eye {lowest_eye_m:.3} m over the floor; done in {frames} frames ({t_s:.3} s); \
         yaw {yaw:.4} pitch {pitch:.4} rad unchanged",
        travel_half_life_s(d0_m) * 1000.0,
        travel_share(d0_m),
    );
}

/// The travel memory belongs to the session that made it. Loading a save
/// replaces the world: the first frame after `View::load_game` anchors
/// exactly on the loaded craft — a first focus, not a change from
/// wherever the replaced session orbited (a craft 850 km away here) —
/// and leaves yaw and pitch alone.
#[test]
fn a_loaded_world_is_a_first_focus_for_the_camera() {
    let mut view = vacuum_view();
    spawn(&mut view, DVec3::new(R_M + 1500.0, 0.0, 0.0));
    frame(&mut view);
    let saved = view.flight_focus().expect("the craft takes the focus");
    settle(&mut view, 150, "framing");
    // An orbit of its own, so "unchanged" has teeth.
    view.flight_event(FlightEvent::Orbit {
        dyaw: -0.3,
        dpitch: 0.1,
    });
    frame(&mut view);
    let snapshot = view.sim.snapshot().expect("serializable");

    // Another craft, a quarter of the way round the planet, takes the
    // focus; the camera travels to it.
    spawn(&mut view, DVec3::new(0.0, R_M + 1500.0, 0.0));
    frame(&mut view);
    let other = view.flight_focus().expect("the new craft takes the focus");
    assert_ne!(other, saved);
    let mut frames = 0;
    while (anchor(&shot(&view)) - staged(&view, other).com).norm() >= 1.0e-9 {
        frame(&mut view);
        frames += 1;
        assert!(frames <= 64, "the travel to the other craft never ended");
    }
    let (yaw, pitch, _, _) = angles_and_distances(&view);
    assert_ne!((yaw, pitch), (0.7, 0.25), "the session's own orbit angles");

    view.load_game(snapshot).expect("load");
    frame(&mut view);
    assert_eq!(
        view.flight_focus(),
        Some(saved),
        "the save's active vessel is the focus again"
    );
    let first = shot(&view);
    close(
        (anchor(&first) - staged(&view, saved).com).norm(),
        0.0,
        1.0e-9,
        "the first frame after the load anchors exactly on the loaded craft",
    );
    let (now_yaw, now_pitch, _, _) = angles_and_distances(&view);
    assert_eq!(
        (now_yaw, now_pitch),
        (yaw, pitch),
        "the load touched the orbit angles"
    );
    eprintln!(
        "load: first post-load frame anchors on the loaded craft (yaw {now_yaw:.4}, pitch {now_pitch:.4} rad, unchanged); \
         the travel before it took {frames} frames"
    );
}

/// One frame of the loop with the sim clock standing still — the camera
/// events drain through the Input schedule and the camera solves, and a
/// craft stays exactly where the last step left it.
fn still_frame(view: &mut View) {
    view.update_session(FRAME_MS, 1, 1, None);
    view.begin_frame();
    view.end_frame();
}

/// One frame that steps the sim a tick but hands over no frame time: the
/// queued commands drain at the tick and nothing eases.
fn untimed_step(view: &mut View) {
    view.update_session(0.0, 1, 1, None);
    view.begin_frame();
    view.sim_step(view.sim.clock().ticks() + 1)
        .expect("sim step");
    view.set_tick_fraction(0.0);
    view.end_frame();
}

/// `flight.rs: resting_anchor` on a flat floor of radius `floor_r`: the
/// CoM, unless it is under the floor — then `depth + min(depth, eye)` up
/// along the local up.
fn resting_anchor(com: DVec3, distance_m: f64, floor_r: f64) -> DVec3 {
    let (r, dir) = (com.norm(), com / com.norm());
    let depth = floor_r - r;
    if depth > 0.0 {
        dir * (floor_r + depth.min(distance_m))
    } else {
        com
    }
}

/// `flight.rs: travel_anchor` on a flat floor: the point itself, unless
/// it is under the floor — then lifted radially onto the floor plus a
/// margin of its depth up to one eye distance, fading over the last eye
/// distance of the travel.
fn travelling_anchor(point: DVec3, remaining_m: f64, eye_m: f64, floor_r: f64) -> DVec3 {
    let depth = floor_r - point.norm();
    if depth > 0.0 {
        let margin = depth.min(eye_m) * (remaining_m / eye_m).min(1.0);
        point * ((floor_r + margin) / point.norm())
    } else {
        point
    }
}

/// The intent's orbit point about `anchor` — where the eye goes unless
/// the floor lifts it.
fn asked_point(anchor: DVec3, yaw: f64, pitch: f64, distance_m: f64) -> DVec3 {
    anchor + orbit_dir(anchor, yaw, pitch) * distance_m
}

/// The eye the camera owes an intent about a craft whose CoM is `com`:
/// the orbit point about the craft's resting anchor, lifted radially
/// onto a flat floor while it is under it.
fn owed_eye(com: DVec3, yaw: f64, pitch: f64, distance_m: f64, floor_r: f64) -> DVec3 {
    let asked = asked_point(
        resting_anchor(com, distance_m, floor_r),
        yaw,
        pitch,
        distance_m,
    );
    let r = asked.norm();
    if r < floor_r {
        asked * (floor_r / r)
    } else {
        asked
    }
}

/// Bring the camera to `(yaw, pitch)` in one drag, run one still frame,
/// and hold it to the eye the new intent is owed. Returns the eye it
/// landed on.
fn drag_to(view: &mut View, com: DVec3, yaw: f64, pitch: f64, floor_r: f64) -> DVec3 {
    let (now_yaw, now_pitch, _, _) = angles_and_distances(view);
    view.flight_event(FlightEvent::Orbit {
        dyaw: yaw - now_yaw,
        dpitch: pitch - now_pitch,
    });
    still_frame(view);
    let (now_yaw, now_pitch, distance, _) = angles_and_distances(view);
    let eye = shot(view).eye;
    close(
        (eye - owed_eye(com, now_yaw, now_pitch, distance, floor_r)).norm(),
        0.0,
        1.0e-9,
        "the eye is not the intent's orbit point about the resting anchor, lifted onto the floor",
    );
    eye
}

/// How far a free eye `distance_m` from its anchor travels for a drag of
/// `drag_rad` — the chord of its orbit.
fn full_step(distance_m: f64, drag_rad: f64) -> f64 {
    distance_m * 2.0 * (0.5 * drag_rad).sin()
}

/// One frame of a drag: the eye moved, by more than nothing and by no
/// more than the drag's own step, and never came nearer than `rmin` to
/// the craft's CoM. Returns the move (m).
fn dragged(com: DVec3, rmin: f64, drag_step: f64, eye: DVec3, previous: DVec3, what: &str) -> f64 {
    let moved = (eye - previous).norm();
    assert!(moved > 0.0, "{what}: the drag did not move the eye");
    assert!(
        moved <= drag_step * 1.000_000_1,
        "{what}: the eye moved {moved} m for a drag of {drag_step} m"
    );
    let to_com = (eye - com).norm();
    assert!(
        to_com >= rmin,
        "{what}: the eye came {to_com} m from the CoM, inside the least distance {rmin}"
    );
    moved
}

/// A craft spawned so that its CoM is `depth_m` under the test body's
/// datum, framed and settled. The sim is not stepped again, so the craft
/// stays buried. Returns the view and the craft's id.
fn buried_view(depth_m: f64) -> (View, VesselId) {
    let mut view = vacuum_view();
    view.set_flight_viewport(VIEWPORT_PX.0, VIEWPORT_PX.1);
    spawn(&mut view, DVec3::new(R_M - depth_m, 0.0, 0.0));
    frame(&mut view);
    let id = view.flight_focus().expect("the craft takes the focus");
    // Long enough for the eased distance to reach its target outright.
    for _ in 0..400 {
        still_frame(&mut view);
    }
    (view, id)
}

/// The buried craft's staged CoM, after holding the scene to its premise:
/// the craft is deeper under the floor than the eye is out.
fn buried_com(view: &View, id: VesselId) -> DVec3 {
    let com = staged(view, id).com;
    let (floor_r, distance) = (R_M + EYE_GROUND_CLEARANCE_M, angles_and_distances(view).2);
    assert!(
        floor_r - com.x > distance * 2.0,
        "the craft is {:.3} m under the floor with the eye {:.3} m out — not buried deeper than the eye distance",
        floor_r - com.x,
        distance
    );
    com
}

/// A craft BURIED in the ground — its CoM 30 m under the surface, deeper
/// than the eye distance, so with the anchor on the CoM no orbit point
/// clears the floor at any pitch and a vertical drag at the craft's level
/// moves the eye by the sideways share alone, whose slope there is zero —
/// through the real input path. The anchor is lifted instead, the craft's
/// depth plus the eye distance up, so the eye is exactly the intent's
/// orbit point about it, and the camera looks at the craft underneath.
/// A 0.01 rad drag at pitch 0, and from ±0.01 rad, moves the eye the full
/// step of a free eye; so does a yaw drag; the angles take the events'
/// deltas exactly.
#[test]
fn a_buried_craft_is_orbited_from_over_the_ground() {
    let (mut view, id) = buried_view(30.0);
    let com = buried_com(&view, id);
    let floor_r = R_M + EYE_GROUND_CLEARANCE_M;
    let (mut yaw, mut pitch, distance, target) = angles_and_distances(&view);
    assert_eq!(
        (yaw, pitch),
        (0.7, 0.25),
        "the camera starts on its default orbit"
    );
    close(target, 4.0 * RADIUS_M, 1.0e-6, "framed at four radii");
    let depth = floor_r - com.norm();
    let rest = resting_anchor(com, distance, floor_r);
    close(
        (rest - com).norm(),
        depth + distance,
        1.0e-9,
        "the anchor is not the craft's depth plus the eye distance above its CoM",
    );
    let first = shot(&view);
    close(
        (first.eye - owed_eye(com, yaw, pitch, distance, floor_r)).norm(),
        0.0,
        1.0e-9,
        "the eye is not the intent's orbit point about the lifted anchor",
    );
    close(
        (anchor(&first) - com).norm(),
        0.0,
        1.0e-9,
        "the camera does not look at the craft",
    );

    // A 0.01 rad drag at pitch 0 and from either side of it: the full
    // step of a free eye, on the very next frame.
    let mut moved = Vec::new();
    for (from, dpitch) in [(0.0, 0.01), (0.01, -0.01), (0.0, -0.01), (-0.01, 0.01)] {
        let before = drag_to(&mut view, com, 0.7, from, floor_r);
        let after = drag_to(&mut view, com, 0.7, from + dpitch, floor_r);
        let step = (after - before).norm();
        close(step, full_step(distance, 0.01), 1.0e-9, "a 0.01 rad drag");
        moved.push(step);
    }

    // Yaw +0.2 rad: the full arc of the circle the eye rides.
    let before = drag_to(&mut view, com, 0.7, 0.0, floor_r);
    let after = drag_to(&mut view, com, 0.7 + 0.2, 0.0, floor_r);
    let swung = (after - before).norm();
    close(swung, full_step(distance, 0.2), 1.0e-9, "the yaw drag");
    (yaw, pitch) = (angles_and_distances(&view).0, angles_and_distances(&view).1);
    assert_eq!(
        (yaw, pitch),
        (0.7 + 0.2, 0.0),
        "the angles are the player's own, whole"
    );
    eprintln!(
        "buried craft through the input path, {depth:.3} m under the floor, framed at {distance:.3} m: the anchor {:.3} m over the floor, \
         the eye {:.3} m from the craft at pitch 0; a 0.01 rad drag at pitch 0 / +0.01 / 0 / −0.01 moves the eye {:.4} / {:.4} / {:.4} / {:.4} m \
         (a free eye {:.4}); yaw +0.2 swings it {:.4} m (free {:.4})",
        rest.norm() - floor_r,
        (first.eye - com).norm(),
        moved[0],
        moved[1],
        moved[2],
        moved[3],
        full_step(distance, 0.01),
        swung,
        full_step(distance, 0.2),
    );
}

/// Zoomed out past its depth, the buried craft's orbit dips under the
/// floor again — the anchor is only the craft's depth over the floor. A
/// pitch drag run up through the floor crossing and back down through the
/// real input path — 0.03 rad a frame, thirty frames each way — moves the
/// eye every frame by no more than the drag's step: no jump in either
/// direction. Every frame's eye is exactly the lifted orbit point.
#[test]
fn a_buried_crafts_eye_crosses_the_floor_without_a_step() {
    let (mut view, id) = buried_view(30.0);
    let com = buried_com(&view, id);
    let floor_r = R_M + EYE_GROUND_CLEARANCE_M;
    // Eight wheel lines out: 43.86 m of eye, past the craft's depth.
    wheel(&mut view, 8, false);
    for _ in 0..400 {
        still_frame(&mut view);
    }
    let far = angles_and_distances(&view).2;
    let anchor = resting_anchor(com, far, floor_r);
    assert!(
        anchor.norm() - floor_r < far,
        "the orbit of a {:.3} m eye never dips under a floor {:.3} m below its anchor",
        far,
        anchor.norm() - floor_r
    );
    let mut eye = drag_to(&mut view, com, 0.7, -1.2, floor_r);
    // Whether the intent's orbit point is under the floor.
    let intent = |view: &View| {
        let (yaw, pitch, distance, _) = angles_and_distances(view);
        asked_point(anchor, yaw, pitch, distance).norm() < floor_r
    };
    let drag_step = full_step(far, 0.03);
    let (mut crossings, mut widest, mut narrowest) = (0, 0.0_f64, f64::MAX);
    let mut at_crossings = Vec::new();
    for dpitch in [0.03; 30].into_iter().chain([-0.03; 30]) {
        let was_under = intent(&view);
        view.flight_event(FlightEvent::Orbit { dyaw: 0.0, dpitch });
        still_frame(&mut view);
        let now = shot(&view).eye;
        let (yaw, pitch, distance, _) = angles_and_distances(&view);
        close(
            (now - owed_eye(com, yaw, pitch, distance, floor_r)).norm(),
            0.0,
            1.0e-9,
            "the eye is not the lifted orbit point",
        );
        let point = asked_point(anchor, yaw, pitch, distance);
        let moved = (now - eye).norm();
        let ratio = moved / drag_step;
        assert!(moved > 0.0, "pitch {pitch}: the drag did not move the eye");
        assert!(
            ratio <= 1.000_1,
            "pitch {pitch}: the eye moved {moved} m for a drag of {drag_step} m"
        );
        if point.norm() < floor_r {
            close(
                now.norm(),
                floor_r,
                1.0e-9,
                "pitch {pitch}: under the floor",
            );
        } else {
            close(
                (now - point).norm(),
                0.0,
                1.0e-9,
                "pitch {pitch}: over the floor",
            );
        }
        let under = point.norm() < floor_r;
        if under != was_under {
            crossings += 1;
            at_crossings.push((pitch, moved, ratio));
        }
        widest = widest.max(ratio);
        narrowest = narrowest.min(ratio);
        eye = now;
    }
    assert_eq!(crossings, 2, "one crossing each way");
    eprintln!(
        "buried craft through the input path, {far:.3} m eye, anchor {:.3} m over the floor, drag 0.03 rad a frame ({drag_step:.4} m): \
         eye/drag ratio {narrowest:.4}..={widest:.5}; crossings (pitch, moved m, ratio): {at_crossings:?}",
        anchor.norm() - floor_r
    );
}

/// A craft IN THE WATER: its CoM 3 m under the ocean's surface, where
/// the water is 6.2 m deep — 2.9 m over the seabed's floor. The surface
/// is no floor: through the real input path the anchor is the CoM, the
/// eye is the intent's orbit point about it — 3 m under the surface at
/// pitch 0 — and the camera looks at the craft; a 0.01 rad drag at pitch
/// 0, and from ±0.01 rad, moves the eye the full step. Dragged straight
/// down to the pitch limit the eye ends on the seabed's floor: 0.3 m
/// over the bottom, 5.9 m under the surface.
#[test]
fn a_craft_in_the_water_is_orbited_under_the_surface() {
    let mut view = flooded_view();
    view.set_flight_viewport(VIEWPORT_PX.0, VIEWPORT_PX.1);
    spawn(&mut view, DVec3::new(R_M - 3.0, 0.0, 0.0));
    frame(&mut view);
    let id = view
        .flight_focus()
        .expect("the craft in the water takes the focus");
    for _ in 0..400 {
        still_frame(&mut view);
    }
    let com = staged(&view, id).com;
    // The spawn's own tick is a tick of free fall: millimetres.
    close(
        com.norm(),
        R_M - 3.0,
        1.0e-2,
        "the craft's CoM 3 m under the surface",
    );
    let floor_r = R_M - 6.2 + EYE_GROUND_CLEARANCE_M;
    let (yaw, _, distance, _) = angles_and_distances(&view);
    close(distance, 4.0 * RADIUS_M, 1.0e-6, "framed at four radii");
    close(
        (resting_anchor(com, distance, floor_r) - com).norm(),
        0.0,
        1.0e-9,
        "the anchor is not the CoM, 2.9 m over the seabed's floor",
    );

    let level = drag_to(&mut view, com, yaw, 0.0, floor_r);
    close(
        level.norm(),
        (com.norm().powi(2) + distance * distance).sqrt(),
        1.0e-9,
        "a level eye is not level with the CoM",
    );
    assert!(
        R_M - level.norm() > 2.99,
        "the level eye is not some 3 m under the surface: {:.3} m",
        R_M - level.norm()
    );
    close(
        (anchor(&shot(&view)) - com).norm(),
        0.0,
        1.0e-9,
        "the camera does not look at the craft",
    );
    let mut moved = Vec::new();
    for (from, dpitch) in [(0.0, 0.01), (0.01, -0.01), (0.0, -0.01), (-0.01, 0.01)] {
        let before = drag_to(&mut view, com, yaw, from, floor_r);
        let after = drag_to(&mut view, com, yaw, from + dpitch, floor_r);
        let step = (after - before).norm();
        close(step, full_step(distance, 0.01), 1.0e-9, "a 0.01 rad drag");
        moved.push(step);
    }
    let (_, now_pitch, _, _) = angles_and_distances(&view);
    view.flight_event(FlightEvent::Orbit {
        dyaw: 0.0,
        dpitch: -PITCH_LIMIT - now_pitch,
    });
    still_frame(&mut view);
    let bottom = shot(&view).eye;
    close(
        bottom.norm(),
        floor_r,
        1.0e-9,
        "dragged straight down, the eye is not on the seabed's floor",
    );
    close(
        R_M - bottom.norm(),
        5.9,
        1.0e-9,
        "…and so not this far under the surface",
    );
    eprintln!(
        "craft in the water through the input path, CoM 3 m under the surface, framed at {distance:.3} m: the level eye {:.4} m under the surface; \
         a 0.01 rad drag at pitch 0 / +0.01 / 0 / −0.01 moves the eye {:.4} / {:.4} / {:.4} / {:.4} m (a free eye {:.4}); \
         at the pitch limit the eye is {:.3} m under the surface",
        R_M - level.norm(),
        moved[0],
        moved[1],
        moved[2],
        moved[3],
        full_step(distance, 0.01),
        R_M - bottom.norm(),
    );
}

/// A buried craft holding the focus, settled on it, with another craft
/// staged to take the focus from it — on rails a chord of [`CHORD_M`]
/// away for a far change, 1 500 m up and 20 m east of it otherwise.
/// Returns both ids.
fn buried_focus_scene(view: &mut View, far: bool) -> (VesselId, VesselId) {
    view.set_flight_viewport(VIEWPORT_PX.0, VIEWPORT_PX.1);
    let staged_pos = DVec3::new(R_M + 1500.0, 20.0, 0.0);
    if far {
        let orbit_r = R_M + 250_000.0;
        let (a, b) = (staged_pos.norm(), orbit_r);
        let theta = ((a * a + b * b - CHORD_M * CHORD_M) / (2.0 * a * b)).acos();
        spawn_orbiting(view, orbit_r, theta);
    } else {
        spawn(view, staged_pos);
    }
    // Spawned last, the buried craft is the focus until the change.
    spawn(view, DVec3::new(R_M - 30.0, 0.0, 0.0));
    frame(view);
    let buried = view
        .flight_focus()
        .expect("the buried craft takes the focus");
    for _ in 0..150 {
        still_frame(view);
    }
    let other = *view.sim.vessels().keys().next().expect("a staged craft");
    (buried, other)
}

/// What the frame of a focus change did (m).
struct ChangeFrame {
    /// The old craft's own motion over the change's tick.
    own_motion_m: f64,
    /// How far the eye moved.
    moved_m: f64,
    /// How far the view turned.
    turned_m: f64,
}

/// Run the frame a focus moves on from `old` with no frame time —
/// whatever is queued drains at its tick — and hold it to what that frame
/// must keep: the eye moves by no more than the old craft's own motion
/// over the tick and nothing at all once the craft is gone, the view
/// turns by no more than that motion turns it, the camera still looks at
/// where the old craft was, and the orbit angles are whole. `before` is
/// the shot of the frame before, `old_com` the old craft's CoM on it.
/// Returns the new focus and what the frame did.
fn change_frame(
    view: &mut View,
    old: VesselId,
    old_com: DVec3,
    before: &Shot,
    distance: f64,
    angles: (f64, f64),
    what: &str,
) -> (VesselId, ChangeFrame) {
    untimed_step(view);
    let next = view.flight_focus().expect("a craft takes the focus");
    assert_ne!(next, old, "{what}: the old craft kept the focus");
    let after = shot(view);
    let old_is_staged = view
        .world
        .resource::<csp_view::flight::FlightSnapshot>()
        .vessels
        .iter()
        .any(|b| b.id == old.0);
    let own_motion_m = if old_is_staged {
        (staged(view, old).com - old_com).norm()
    } else {
        0.0
    };
    let (moved_m, turned_m) = (
        (after.eye - before.eye).norm(),
        (after.back - before.back).norm(),
    );
    assert!(
        moved_m <= own_motion_m * (1.0 + distance / R_M) + 1.0e-9,
        "{what}: the frame of the change moved the eye {moved_m} m, more than the old craft's own motion {own_motion_m} m"
    );
    assert!(
        turned_m <= own_motion_m / R_M + 1.0e-12,
        "{what}: the view turned {turned_m} on the frame of the change"
    );
    close(
        (anchor(&after) - old_com).norm(),
        0.0,
        1.0e-9,
        &format!("{what}: the frame of the change moved the look-at"),
    );
    let (now_yaw, now_pitch, _, _) = angles_and_distances(view);
    assert_eq!(
        (now_yaw, now_pitch),
        angles,
        "{what}: the focus change touched the orbit angles"
    );
    (
        next,
        ChangeFrame {
            own_motion_m,
            moved_m,
            turned_m,
        },
    )
}

/// Step the travel that follows such a frame, a timed frame at a time
/// until it is done, and hold every frame to what the camera owes: the
/// anchor eases from `start`, the previous craft's resting anchor, toward
/// the new craft; the look-at is the travelling anchor carrying `carry`
/// — the previous craft's look-at, as an offset from `start` — and blends
/// to the new craft over the last eye distance. Returns the frames it
/// took, the first included, and the travel's length (m).
fn travel_from(
    view: &mut View,
    next: VesselId,
    angles: (f64, f64),
    start: DVec3,
    carry: DVec3,
    com_at_change: DVec3,
    what: &str,
) -> (usize, f64) {
    let floor_r = R_M + EYE_GROUND_CLEARANCE_M;
    let (yaw, pitch) = angles;
    let travel_m = (start - com_at_change).norm();
    let k = travel_share(travel_m);
    frame(view);
    let mut offset = (start - com_at_change) * (1.0 - k);
    let mut frames = 1;
    while offset != DVec3::ZERO {
        let com = staged(view, next).com;
        let distance = angles_and_distances(view).2;
        let travelling = travelling_anchor(com + offset, offset.norm(), distance, floor_r);
        let want_look =
            com + (travelling - com + carry) * travel_blend(offset.norm(), travel_m, distance);
        let now = shot(view);
        close(
            (anchor(&now) - want_look).norm(),
            0.0,
            1.0e-6,
            &format!("{what}: frame {frames}: the look-at"),
        );
        close(
            (now.eye - (travelling + orbit_dir(travelling, yaw, pitch) * distance)).norm(),
            0.0,
            1.0e-5,
            &format!("{what}: frame {frames}: the eye is not where the angles put it"),
        );
        assert_eq!(
            (angles_and_distances(view).0, angles_and_distances(view).1),
            angles,
            "{what}: frame {frames}: the travel touched the orbit angles"
        );
        frame(view);
        offset = offset * (1.0 - k);
        if offset.norm() < TRAVEL_DONE_M {
            offset = DVec3::ZERO;
        }
        frames += 1;
        assert!(frames <= 64, "{what}: the travel never ended");
    }
    close(
        (anchor(&shot(view)) - staged(view, next).com).norm(),
        0.0,
        1.0e-9,
        &format!("{what}: travel done, the camera does not look at the new craft"),
    );
    (frames, travel_m)
}

/// A focus change FROM a craft under the floor, through the real input
/// path, three ways: the buried craft is terminated, a craft is launched
/// beside it, a craft 850 km away is recovered. On the frame of the
/// change — no frame time — the eye moves by no more than the old craft's
/// own motion over that tick, and nothing at all when the craft is gone
/// (the travel used to start at the CoM, 30 m under the floor, and be
/// lifted in full on that frame: a jump of 12.963 m). The view turns by no
/// more than that motion turns it, the camera still looks at the buried
/// craft, yaw and pitch are untouched, and the travel then closes as any
/// other.
#[test]
fn a_focus_change_from_a_buried_craft_moves_nothing_on_its_first_frame() {
    let floor_r = R_M + EYE_GROUND_CLEARANCE_M;
    for (change, far) in [
        ("the buried craft is terminated", false),
        ("a craft is launched beside the buried one", false),
        ("a craft 850 km away is recovered", true),
    ] {
        let mut view = vacuum_view();
        let (buried, other) = buried_focus_scene(&mut view, far);
        let before_com = staged(&view, buried).com;
        let before = shot(&view);
        let ((yaw, pitch), distance) = {
            let (yaw, pitch, distance, _) = angles_and_distances(&view);
            ((yaw, pitch), distance)
        };
        let start = resting_anchor(before_com, distance, floor_r);
        close(
            (anchor(&before) - before_com).norm(),
            0.0,
            1.0e-9,
            "at rest the camera looks at the buried craft",
        );
        match change {
            "the buried craft is terminated" => {
                view.enqueue_command(
                    SourceClass::Ui,
                    PLAYER_SOURCE_ID,
                    SimCommand::TerminateVessel { vessel: buried },
                );
            }
            "a craft is launched beside the buried one" => {
                spawn(&mut view, DVec3::new(R_M + 1500.0, 40.0, 0.0));
            }
            _ => assert_eq!(view.recover_control(), Some(other)),
        }

        // The frame of the change, then the travel.
        let (next, change_frame) = change_frame(
            &mut view,
            buried,
            before_com,
            &before,
            distance,
            (yaw, pitch),
            change,
        );
        if change != "a craft is launched beside the buried one" {
            assert_eq!(next, other, "{change}: the wrong craft took the focus");
        }
        let com_at_change = staged(&view, next).com;
        let (frames, travel_m) = travel_from(
            &mut view,
            next,
            (yaw, pitch),
            start,
            before_com - start,
            com_at_change,
            change,
        );
        let t_s = frames as f64 * FRAME_MS / 1000.0;
        assert!(
            t_s <= TRAVEL_SETTLE_S + FRAME_MS / 1000.0 + 1.0e-9,
            "{change}: the travel took {t_s:.3} s"
        );
        let ChangeFrame {
            own_motion_m,
            moved_m,
            turned_m,
        } = change_frame;
        eprintln!(
            "focus change, {change}: the frame of the change moved the eye {moved_m:.6} m for the old craft's own motion {own_motion_m:.6} m \
             and turned the view {turned_m:.2e}; the travel of {travel_m:.1} m then closed in {frames} frames ({t_s:.3} s)",
        );
    }
}

/// The travel memory belongs to the session that made it, for a craft
/// under the floor too. A slab lying flat on the ground — its CoM 0.1 m
/// up, 0.2 m under the camera's floor — settles and latches, so its world
/// can be saved (a craft buried in the ground holds a live contact the sim
/// refuses to save). Loading that save: the first frame after
/// `View::load_game` is already the intent's orbit about the slab's
/// lifted anchor, looking at the slab, and the frame after it moves
/// nothing. A level view of it sits as far over the floor as the anchor.
#[test]
fn a_loaded_craft_under_the_floor_is_a_first_focus_for_the_camera() {
    let floor_r = R_M + EYE_GROUND_CLEARANCE_M;
    let (mut view, id) = settled_view(slab(), DVec3::new(R_M + 0.1, 0.0, 0.0));
    let com = staged(&view, id).com;
    let radius_m = view
        .world
        .resource::<csp_view::flight::CraftRadii>()
        .0
        .iter()
        .find(|(v, _)| *v == id.0)
        .expect("every staged vessel carries its radius")
        .1;
    close(
        radius_m,
        SLAB_RADIUS_M,
        1.0e-9,
        "the slab's bounding radius",
    );
    let (yaw, _, distance, _) = angles_and_distances(&view);
    let depth = floor_r - com.norm();
    assert!(
        depth > 0.0,
        "the slab's CoM is {} m over the floor, not under it",
        -depth
    );
    let snapshot = view.sim.snapshot().expect("the latched slab's world saves");
    // A level view: the eye level with the anchor, the slab's depth over
    // the floor.
    let rest = resting_anchor(com, distance, floor_r);
    close(
        rest.norm() - floor_r,
        depth,
        1.0e-9,
        "the anchor over the floor",
    );
    let level = drag_to(&mut view, com, yaw, 0.0, floor_r);
    close(
        level.norm(),
        (rest.norm() * rest.norm() + distance * distance).sqrt(),
        1.0e-9,
        "a level eye is not level with the anchor",
    );
    // An orbit of its own, so "unchanged" has teeth.
    drag_to(&mut view, com, yaw - 0.3, 0.25 + 0.1, floor_r);

    // Another slab takes the focus — the same craft, so the load's
    // reframe changes nothing — and the camera travels to it.
    spawn_craft(&mut view, slab(), DVec3::new(0.0, R_M + 1500.0, 0.0));
    frame(&mut view);
    let other = view.flight_focus().expect("the new craft takes the focus");
    assert_ne!(other, id);
    let mut frames = 0;
    while (anchor(&shot(&view)) - staged(&view, other).com).norm() >= 1.0e-9 {
        frame(&mut view);
        frames += 1;
        assert!(frames <= 64, "the travel to the other craft never ended");
    }

    view.load_game(snapshot).expect("load");
    still_frame(&mut view);
    assert_eq!(
        view.flight_focus(),
        Some(id),
        "the save's active vessel is the focus again"
    );
    let com = staged(&view, id).com;
    let (now_yaw, now_pitch, now_distance, _) = angles_and_distances(&view);
    let first = shot(&view);
    close(
        (first.eye - owed_eye(com, now_yaw, now_pitch, now_distance, floor_r)).norm(),
        0.0,
        1.0e-9,
        "the first frame after the load is not the intent's orbit about the loaded craft's anchor",
    );
    close(
        (anchor(&first) - com).norm(),
        0.0,
        1.0e-9,
        "the first frame after the load does not look at the loaded craft",
    );
    still_frame(&mut view);
    let second = shot(&view);
    assert_eq!(
        (second.eye, second.back, second.up),
        (first.eye, first.back, first.up),
        "the frame after the load jumped"
    );
    eprintln!(
        "load: slab CoM {:.3} m over the ground, {:.3} m under the floor; its anchor and a level eye {:.3} m over the floor; \
         the first post-load frame orbits the loaded slab's anchor and looks at it (yaw {now_yaw:.4}, pitch {now_pitch:.4} rad, unchanged); \
         the next frame moved nothing; the travel before the load took {frames} frames",
        com.norm() - R_M,
        depth,
        level.norm() - floor_r
    );
}

/// `flight.rs: SKIRT_ONRAMP` — the share of the way across the zone over
/// which the eye settles from the drag's own pace onto the circle's.
const SKIRT_ONRAMP: f64 = 0.5;

/// `flight.rs: skirt_plateau` — how many metres of circle the eye covers
/// for a metre of its orbit point's slide once it has settled onto the
/// circle's pace.
fn skirt_plateau() -> f64 {
    (std::f64::consts::PI - 0.5 * SKIRT_ONRAMP) / (2.0 - 0.5 * SKIRT_ONRAMP)
}

/// `flight.rs: skirt_azimuth` — where the eye is round the circle: the
/// angle (rad) from the turn toward the yaw's own side for an orbit point
/// `u` of the way across the zone, `0` at the turn and `2` at the far edge.
fn skirt_azimuth(u: f64) -> f64 {
    let plateau = skirt_plateau();
    let v = u / SKIRT_ONRAMP;
    if v < 1.0 {
        u + 0.5 * (plateau - 1.0) * SKIRT_ONRAMP * v.powi(3) * (2.0 - v)
    } else {
        0.5 * SKIRT_ONRAMP * (1.0 + plateau) + plateau * (u - SKIRT_ONRAMP)
    }
}

/// How far a floor point is out along the yaw's line from the point under
/// the craft, and how far to the yaw's own side of that line (m) — the
/// rule's own measure of the floor about a craft on the flat body.
fn along_yaw(point: DVec3, com: DVec3, yaw: f64) -> (f64, f64) {
    let up = com / com.norm();
    let along = orbit_dir(com, yaw, 0.0);
    let out = point - up * (R_M + EYE_GROUND_CLEARANCE_M);
    (out.dot(along), out.dot(along.cross(up)))
}

/// Bring the orbit's pitch to `pitch` in one drag and run one still frame.
fn drag_to_pitch(view: &mut View, pitch: f64) {
    let (_, now, _, _) = angles_and_distances(view);
    view.flight_event(FlightEvent::Orbit {
        dyaw: 0.0,
        dpitch: pitch - now,
    });
    still_frame(view);
}

/// What a drag straight down to the pitch limit and back up saw.
struct GroundSweep {
    /// The pitch of the frame the eye entered the zone on (rad), and how
    /// far out along the yaw's line its orbit point was on the frame before
    /// and on that frame (m): the turn lies between the two.
    entry: (f64, [f64; 2]),
    /// The least and the most the eye moved on any frame outside the zone —
    /// the frame that entered it and the frame that left it included (m).
    moved_m: [f64; 2],
    /// The most the eye moved on any frame inside the zone, and the most
    /// that was for a metre of its orbit point's slide (m).
    inside_moved_m: [f64; 2],
    /// The nearest the eye came to the CoM over it all (m).
    nearest_m: f64,
    /// The eye at the pitch limit.
    at_limit: DVec3,
}

/// Drag straight down, 0.01 rad a frame, from the orbit in hand to the
/// pitch limit and back up to where it started, through the real input
/// path, the eye held at `distance` about a craft whose CoM is `com`:
/// what the least-distance rule did. On the way down every frame on the
/// floor is 0.3 m over the ground; outside the zone the eye is the lifted
/// orbit point itself, on it; inside it the eye is exactly the least
/// distance from the CoM. Every frame moves; a frame outside the zone, and
/// the frame that enters it, by no more than the drag's own step, and a
/// frame inside it by no more than `pace` times the orbit point's slide
/// along the floor. The way up is the way down again, to a nanometre, and
/// the frame that leaves the zone moves no more than the drag's own step.
fn sweep_the_ground(
    view: &mut View,
    com: DVec3,
    rmin: f64,
    yaw: f64,
    pace: f64,
    distance: f64,
) -> GroundSweep {
    let floor_r = R_M + EYE_GROUND_CLEARANCE_M;
    let drag_step = full_step(distance, 0.01);
    let start_pitch = angles_and_distances(view).1;
    let frames = ((start_pitch + PITCH_LIMIT) / 0.01).ceil() as usize;
    let pitch_at = |k: usize| (start_pitch - k as f64 * 0.01).max(-PITCH_LIMIT);
    let mut sweep = GroundSweep {
        entry: (0.0, [0.0; 2]),
        moved_m: [f64::MAX, 0.0],
        inside_moved_m: [0.0; 2],
        nearest_m: f64::MAX,
        at_limit: DVec3::ZERO,
    };
    let mut path = vec![shot(view).eye];
    let (mut previous, mut before) = (path[0], (0.0, false));
    for k in 1..=frames {
        drag_to_pitch(view, pitch_at(k));
        let (_, pitch, _, _) = angles_and_distances(view);
        let (eye, what) = (shot(view).eye, format!("pitch {pitch:+.4} rad"));
        let point = owed_eye(com, yaw, pitch, distance, floor_r);
        let (now, inside) = (along_yaw(point, com, yaw).0, (point - com).norm() < rmin);
        let (moved, to_com) = ((eye - previous).norm(), (eye - com).norm());
        if point.norm() < floor_r {
            close(
                eye.norm() - R_M,
                EYE_GROUND_CLEARANCE_M,
                1.0e-9,
                &format!("{what}: the eye over the ground"),
            );
        }
        if inside {
            close(
                to_com,
                rmin,
                1.0e-6,
                &format!("{what}: the eye is not on the least distance"),
            );
        } else {
            close(
                (eye - point).norm(),
                0.0,
                1.0e-9,
                &format!("{what}: outside the zone, the eye is not the lifted orbit point"),
            );
        }
        if inside && before.1 {
            let slid = (now - before.0).abs();
            assert!(moved > 0.0, "{what}: the drag did not move the eye");
            assert!(
                moved <= pace * slid * 1.000_001,
                "{what}: the eye moved {moved} m inside the zone for a slide of {slid} m"
            );
            sweep.inside_moved_m = [
                sweep.inside_moved_m[0].max(moved),
                sweep.inside_moved_m[1].max(moved / slid),
            ];
        } else {
            dragged(com, rmin, drag_step, eye, previous, &what);
            sweep.moved_m = [sweep.moved_m[0].min(moved), sweep.moved_m[1].max(moved)];
        }
        if inside && !before.1 {
            sweep.entry = (pitch, [before.0, now]);
        }
        sweep.nearest_m = sweep.nearest_m.min(to_com);
        path.push(eye);
        (previous, before) = (eye, (now, inside));
    }
    sweep.at_limit = path[frames];
    // Back up over the same pitches.
    for k in (0..frames).rev() {
        drag_to_pitch(view, pitch_at(k));
        let (_, pitch, _, _) = angles_and_distances(view);
        let eye = shot(view).eye;
        close(
            (eye - path[k]).norm(),
            0.0,
            1.0e-9,
            &format!("pitch {pitch:+.4} rad: a reversed drag left the path"),
        );
        let point = owed_eye(com, yaw, pitch, distance, floor_r);
        if before.1 && (point - com).norm() >= rmin {
            let moved = (eye - previous).norm();
            assert!(
                moved <= drag_step * 1.000_000_1,
                "pitch {pitch:+.4} rad, leaving the zone: the eye moved {moved} m for a drag of {drag_step} m"
            );
        }
        (previous, before) = (
            eye,
            (along_yaw(point, com, yaw).0, (point - com).norm() < rmin),
        );
    }
    sweep
}

/// Dragged straight down to the ground, the eye never comes inside the
/// craft, through the real input path: a 2 m cube with its CoM 1 m up, the
/// least distance 3 m. The lifted eye used to end 0.72 m from the CoM,
/// inside the cube; and the old rule slid it back out the way it came,
/// turning back on a drag that went on the same way. It goes round the
/// cube on the ground instead: it slides toward it along the ground, enters
/// the zone at the turn — where the floor is the least distance from the
/// CoM — and from there rides the circle at exactly that distance, round
/// the yaw's own side. Every frame moves; the frame that enters moves no
/// more than the drag's own step, and no frame inside more than the
/// circle's pace times the orbit point's slide. At the pitch limit the eye
/// is 81° round the circle, 3.000 m from the CoM, where the rule says. A
/// drag back up leaves the zone by no more than the drag's own step and
/// retraces the path to a nanometre.
#[test]
fn the_eye_on_the_ground_goes_round_the_craft_at_its_least_distance() {
    let (mut view, id) = settled_view(cube(), DVec3::new(R_M + 1.0, 0.0, 0.0));
    let com = staged(&view, id).com;
    let radius_m = view
        .world
        .resource::<csp_view::flight::CraftRadii>()
        .0
        .iter()
        .find(|(v, _)| *v == id.0)
        .expect("every staged vessel carries its radius")
        .1;
    close(
        radius_m,
        CUBE_RADIUS_M,
        1.0e-9,
        "the cube's bounding radius",
    );
    let rmin = 3.0_f64.max(1.5 * CUBE_RADIUS_M);
    close(rmin, 3.0, 1.0e-12, "the cube's least distance");
    let floor_r = R_M + EYE_GROUND_CLEARANCE_M;
    let com_height_m = com.norm() - R_M;
    close(
        com_height_m,
        1.0,
        1.0e-3,
        "the cube's CoM a metre over the ground",
    );
    let (yaw, _, distance, _) = angles_and_distances(&view);
    close(
        distance,
        4.0 * CUBE_RADIUS_M,
        1.0e-6,
        "framed at four radii",
    );
    let drag_step = full_step(distance, 0.01);

    let sweep = sweep_the_ground(&mut view, com, rmin, yaw, skirt_plateau(), distance);

    // The turn on flat ground: sqrt(rmin² − H²), H the CoM's height over
    // the floor; the eye entered the zone there.
    let height_m = com_height_m - EYE_GROUND_CLEARANCE_M;
    let turn_m = (rmin * rmin - height_m * height_m).sqrt();
    let (entry_pitch, [before, at]) = sweep.entry;
    assert!(
        before >= turn_m - 1.0e-4 && at <= turn_m + 1.0e-4,
        "the eye entered the zone between {before} and {at} m out, not at the turn {turn_m}"
    );
    // At the pitch limit: on the circle, `skirt_azimuth` round from the turn.
    let limit_point = owed_eye(com, yaw, -PITCH_LIMIT, distance, floor_r);
    let bearing = skirt_azimuth((turn_m - along_yaw(limit_point, com, yaw).0) / turn_m);
    let (out, aside) = along_yaw(sweep.at_limit, com, yaw);
    close(
        out,
        turn_m * bearing.cos(),
        1.0e-4,
        "at the pitch limit: how far out along the yaw's line the eye is",
    );
    close(
        aside,
        turn_m * bearing.sin(),
        1.0e-4,
        "at the pitch limit: how far along the yaw's own side the eye is",
    );
    close(
        (sweep.at_limit - com).norm(),
        rmin,
        1.0e-6,
        "at the pitch limit: the eye's distance from the CoM",
    );
    eprintln!(
        "cube, CoM {com_height_m:.3} m over the ground, least distance {rmin:.3} m, eye {distance:.3} m: dragged straight down, the eye entered the zone at pitch {entry_pitch:+.4} rad \
         (the turn {turn_m:.4} m out on flat ground, bracketed {before:.4}..{at:.4} m); at the pitch limit {:.6} m over the ground, {:.6} m from the CoM, \
         {:.4} m out along the yaw's line and {:.4} m along the yaw's own side — {:.1}° round from the turn, where the rule puts it; \
         nearest eye over the sweep {:.9} m; outside the zone the eye moved {:.4}..{:.4} m a frame (the drag's own step {:.4} m); \
         inside it at most {:.4} m, {:.2} m per metre of slide (the circle's pace {:.2}); a reversed drag retraced every frame to a nanometre",
        sweep.at_limit.norm() - R_M,
        (sweep.at_limit - com).norm(),
        out,
        aside,
        bearing.to_degrees(),
        sweep.nearest_m,
        sweep.moved_m[0],
        sweep.moved_m[1],
        drag_step,
        sweep.inside_moved_m[0],
        sweep.inside_moved_m[1],
        skirt_plateau()
    );
}

/// Where [`wheeled_scene`] puts the two-box craft's root: 2.4 m over the
/// datum. Its CoM is 1.5 m from the root, up its axis — the craft lies
/// along north here — so it is 2.1 m over the camera's floor.
const WHEEL_ROOT: DVec3 = DVec3::new(R_M + 2.4, 0.0, 0.0);

/// The scene the wheel was measured on, on the still body: the two-box
/// craft at [`WHEEL_ROOT`], stepped once, then framed and settled at four
/// radii. Returns the view, the craft's id, its CoM, the yaw and the
/// craft's least distance.
fn wheeled_scene() -> (View, VesselId, DVec3, f64, f64) {
    let mut view = quiet_view();
    view.set_flight_viewport(VIEWPORT_PX.0, VIEWPORT_PX.1);
    spawn(&mut view, WHEEL_ROOT);
    frame(&mut view);
    let id = view.flight_focus().expect("the craft takes the focus");
    close(
        angles_and_distances(&view).3,
        4.0 * RADIUS_M,
        1.0e-9,
        "framed at four radii",
    );
    for _ in 0..400 {
        still_frame(&mut view);
    }
    let (yaw, _, distance, _) = angles_and_distances(&view);
    close(distance, 4.0 * RADIUS_M, 1.0e-6, "settled at four radii");
    let com = staged(&view, id).com;
    (view, id, com, yaw, 3.0_f64.max(1.5 * RADIUS_M))
}

/// Zooming in on the ground never backs the eye away from the craft. With
/// the eye riding the floor the old rule mirrored the orbit point about
/// the turn: two wheel lines in at the turn carried the eye from 3.90 m to
/// 4.76 m from the craft — zooming in moved it out — and a drag on past the
/// closest point brought it back the way it had come. Now the eye's place
/// is a monotonic function of the intent: at, before and past the turn,
/// two lines in make its distance to the craft fall or hold, never rise;
/// it never comes nearer than the least distance; and it never moves back
/// along its way — off the circle toward the craft, on the circle onward
/// round it.
#[test]
fn zooming_in_at_the_turn_never_backs_the_eye_away() {
    let eye_m = 4.0 * RADIUS_M;
    for (what, from_turn) in [
        ("0.4 rad before the turn", 0.4),
        ("at the turn", 0.0),
        ("0.1 rad past the turn", -0.1),
        ("0.4 rad past the turn", -0.4),
    ] {
        let (mut view, _, com, yaw, rmin) = wheeled_scene();
        let height_m = com.norm() - (R_M + EYE_GROUND_CLEARANCE_M);
        let turn_m = (rmin * rmin - height_m * height_m).sqrt();
        let turn_rad = -(turn_m / eye_m).acos();
        drag_to_pitch(&mut view, turn_rad + from_turn);
        let pitch = angles_and_distances(&view).1;
        let mut was = shot(&view).eye;
        let start_m = (was - com).norm();
        wheel(&mut view, 2, true);
        let (mut least, mut most, mut rise, mut trace) = (f64::MAX, 0.0_f64, 0.0_f64, Vec::new());
        let (mut was_place, mut backed) = (along_yaw(was, com, yaw), 0.0_f64);
        for i in 0..150 {
            still_frame(&mut view);
            let eye = shot(&view).eye;
            let (to_com, place) = ((eye - com).norm(), along_yaw(eye, com, yaw));
            assert!(
                to_com >= rmin,
                "{what}: frame {i}: zooming in brought the eye {to_com} m from the craft, inside the least distance {rmin}"
            );
            rise = rise.max(to_com - (was - com).norm());
            backed = backed.max(place.0 - was_place.0);
            (least, most) = (least.min(to_com), most.max(to_com));
            if i < 6 || i == 149 {
                trace.push(format!("{to_com:.3}"));
            }
            was = eye;
            was_place = place;
        }
        assert!(
            rise <= 1.0e-6,
            "{what}: zooming in moved the eye {rise} m away from the craft on a frame"
        );
        assert!(
            backed <= 1.0e-9,
            "{what}: zooming in moved the eye {backed} m back along its way"
        );
        let (_, _, asked, target) = angles_and_distances(&view);
        close(
            target,
            eye_m * (-2.0 * ZOOM_LN_PER_LINE).exp(),
            1.0e-9,
            "two lines in",
        );
        close(asked, target, 1.0e-6, "the zoom settled");
        eprintln!(
            "wheel IN two lines, {what} (pitch {pitch:+.3} rad): asked distance {eye_m:.3} -> {asked:.3} m; \
             the eye's distance to the craft {start_m:.3} -> {:.3} m (smallest {least:.3}, largest {most:.3}); \
             the most it rose on any frame {rise:.1e} m, the most it moved back {backed:.1e} m; distance trace {:?}",
            (shot(&view).eye - com).norm(),
            trace
        );
    }
}

/// A focus travel whose orbit point meets the new craft's keep-out from its
/// far side — the lunge. The two-box craft with the eye at four radii
/// dragged to −1.0 rad, so the orbit point rides the ground 5.6 m out along
/// the yaw; a second craft takes the focus 20 m out along the yaw, beyond
/// the eye. The travelling orbit point crosses the new craft's zone from
/// its far side, and the old rule turned the eye back there: 5.8 m in one
/// frame, after 2.9 m the frame before. Now the frame of the change moves
/// nothing; the eye goes round the new craft's circle from the far edge to
/// the turn — never nearer than the least distance on any frame — and no
/// frame moves it more than a tenth beyond the one before, beyond the
/// millimetre the travel itself snaps at its end.
#[test]
fn a_focus_travel_past_the_new_crafts_far_side_never_lunges() {
    let eye_m = 4.0 * RADIUS_M;
    let (mut view, a, com_a, yaw, rmin) = wheeled_scene();
    let yaw_dir = orbit_dir(com_a, yaw, 0.0);
    drag_to_pitch(&mut view, -1.0);
    let before = shot(&view);
    close(
        (before.eye - owed_eye(com_a, yaw, -1.0, eye_m, R_M + EYE_GROUND_CLEARANCE_M)).norm(),
        0.0,
        1.0e-9,
        "before the change the eye is the lifted orbit point",
    );
    assert!(
        (before.eye - com_a).norm() >= rmin,
        "the eye carries the first craft's skirt"
    );

    // B takes the focus: the newest craft, the same craft put the same way
    // 20 m along the yaw. The frame of the change carries no frame time,
    // and moves nothing.
    spawn(&mut view, WHEEL_ROOT + yaw_dir * 20.0);
    untimed_step(&mut view);
    let b = view.flight_focus().expect("a craft takes the focus");
    assert_ne!(b, a, "the new craft took the focus");
    let com_b = staged(&view, b).com;
    let eye = shot(&view).eye;
    close(
        (eye - before.eye).norm(),
        0.0,
        1.0e-9,
        "the frame of the change moved the eye",
    );
    assert!(
        (eye - com_b).norm() >= rmin,
        "the eye starts nearer than the least distance to the new craft: {:.3} m",
        (eye - com_b).norm()
    );
    // The travel: from where the eye orbited to the new craft, 20 m along
    // the yaw's line, the orbit point riding the ground across the zone.
    let (yaw_now, pitch, distance, _) = angles_and_distances(&view);
    let orbit_point = owed_eye(
        com_b,
        yaw_now,
        pitch,
        distance,
        R_M + EYE_GROUND_CLEARANCE_M,
    );
    let (out, _) = along_yaw(orbit_point, com_b, yaw);
    close(
        out,
        5.6,
        5.0e-2,
        "the orbit point's rest is 5.6 m out along the yaw",
    );
    let first = (staged(&view, a).com - com_b).norm();
    close(first, 20.0, 5.0e-2, "the travel's length");

    let (mut previous, mut last_move, mut frames) = (eye, None, 0);
    let (mut moves, mut nearest_m, mut widest_ratio) = (Vec::new(), f64::MAX, 0.0_f64);
    loop {
        still_frame(&mut view);
        frames += 1;
        assert!(frames <= 64, "the travel never ended");
        let (eye, com) = (shot(&view).eye, staged(&view, b).com);
        let (moved, to_com) = ((eye - previous).norm(), (eye - com).norm());
        assert!(
            to_com >= rmin,
            "frame {frames}: the eye came {to_com} m from the new craft's CoM, inside the least distance {rmin}"
        );
        // Beyond the millimetre the travel itself snaps at its end.
        if let Some(last) = last_move
            && last > TRAVEL_DONE_M
        {
            let ratio = moved / last;
            widest_ratio = widest_ratio.max(ratio);
            assert!(
                moved <= 1.1 * last + TRAVEL_DONE_M,
                "frame {frames}: the eye moved {moved:.3} m after {last:.3} m, {ratio:.2} times"
            );
        }
        moves.push(moved);
        nearest_m = nearest_m.min(to_com);
        (previous, last_move) = (eye, Some(moved));
        if (anchor(&shot(&view)) - com).norm() < 1.0e-9 {
            break;
        }
    }
    assert!(
        moves.len() >= 8,
        "the travel closed in {frames} frames: too few to cross the zone"
    );
    assert!(
        nearest_m < rmin + 1.0e-6,
        "the eye never rode the new craft's circle: nearest {nearest_m} m, least distance {rmin} m"
    );
    eprintln!(
        "focus travel of 20 m past the new craft's far side (the orbit point on the ground, pitch -1.0 rad): the frame of the change moved the eye 0 m; \
         then {frames} frames, the eye moving per frame {:?} m; the most any frame's move was of the one before {widest_ratio:.3} \
         (over the frames moving more than a millimetre); \
         never nearer than {nearest_m:.6} m to the new craft's CoM (least distance {rmin:.3} m)",
        moves.iter().map(|m| format!("{m:.2}")).collect::<Vec<_>>()
    );
}

/// What a focus travel, taken a timed frame at a time, saw.
struct Past {
    /// The eye and the new craft's CoM on every frame.
    eyes: Vec<DVec3>,
    coms: Vec<DVec3>,
    /// The eye's move on each of them (m).
    moves: Vec<f64>,
    /// The nearest the eye came to the new craft's CoM (m).
    nearest_m: f64,
    /// The most any frame's move was of the one before, over the frames
    /// moving more than a millimetre.
    widest_ratio: f64,
    /// The frames the eye spent on the new craft's ball — the least
    /// distance from its CoM to within a micron.
    on_the_ball: usize,
    /// The eye's offset from that CoM over them: at its highest, and at
    /// its farthest along the yaw's own side (m).
    top: DVec3,
    flank: DVec3,
    /// The least of its share along the yaw's own side over them (m).
    least_flank_m: f64,
    /// The least and the most of its height over the test body's ground
    /// over the frames (m).
    over_ground_m: [f64; 2],
    /// The eye's distance from the CoM on the frame of the change (m).
    start_m: f64,
    /// Over the frames before the eye was the least distance out from the
    /// CoM — a travel it began inside it — the most any frame's move was
    /// of the one before, over the frames moving more than a millimetre.
    inside_widest_ratio: f64,
    /// The eye's distance from the CoM on the frame before each of them.
    was: Vec<f64>,
}

/// What a focus travel is held to.
struct Kept {
    /// The orbit's angles, held whole.
    angles: (f64, f64),
    /// The yaw's horizontal at the new craft.
    yaw_dir: DVec3,
    /// The new craft's least distance (m).
    rmin: f64,
    /// The floor's radius (m) where the eye must stay on or over it.
    floor_r: Option<f64>,
    /// The most a frame's move may be of the one before, over the frames
    /// moving more than a millimetre.
    bound: Option<f64>,
    /// The eye itself began the travel inside the craft's least distance.
    /// The travel is then held to what leaving a ball owes
    /// (`flight.rs: FocusTravel::began_inside`): the eye's distance to the
    /// craft's CoM never falls from where it began, and once it is the
    /// least distance out it never is less again.
    began_inside: bool,
}

/// The craft's local up, on bodies centred on the origin.
fn local_up(com: DVec3) -> DVec3 {
    com / com.norm()
}

/// How far `point` is from the segment `from`…`to` (m): the orbit point's
/// way across the new craft's ball.
fn way_passes(from: DVec3, to: DVec3, point: DVec3) -> f64 {
    let way = to - from;
    let share = ((point - from).dot(way) / way.dot(way)).clamp(0.0, 1.0);
    (point - (from + way * share)).norm()
}

/// Step a focus travel, a timed frame at a time, until the camera looks
/// at the new craft, and hold every frame to what the keep-out owes: the
/// eye never nearer than the least distance to the craft's CoM — exactly
/// as positions measure it — or, where it began the travel inside it
/// ([`Kept::began_inside`]), never nearer than it began and never inside
/// the least distance again once out of it, and on the ball exactly on
/// every frame it is out while the orbit point is in; on or over the
/// floor where one is given; no frame moving beyond [`Kept::bound`] times
/// the one before, beyond the millimetre the travel snaps at its end; and
/// the orbit angles whole. `start` is the eye on the frame of the change
/// and `to` the new craft. Returns what the travel saw, with the eye's
/// height over the test body's ground over the frames.
fn travel_past(view: &mut View, to: VesselId, start: DVec3, kept: &Kept) -> Past {
    let (angles, yaw_dir, rmin) = (kept.angles, kept.yaw_dir, kept.rmin);
    let up = local_up(staged(view, to).com);
    let side = yaw_dir.cross(up);
    let mut past = Past {
        eyes: Vec::new(),
        coms: Vec::new(),
        moves: Vec::new(),
        nearest_m: f64::MAX,
        widest_ratio: 0.0,
        on_the_ball: 0,
        top: DVec3::ZERO,
        flank: DVec3::ZERO,
        least_flank_m: f64::MAX,
        over_ground_m: [f64::MAX, f64::MIN],
        start_m: (start - staged(view, to).com).norm(),
        inside_widest_ratio: 0.0,
        was: Vec::new(),
    };
    let (mut previous, mut last_move) = (start, None);
    // The eye has been the least distance out from the CoM: from there it
    // never is less again.
    let mut out = false;
    loop {
        still_frame(view);
        let frames = past.eyes.len() + 1;
        assert!(frames <= 64, "the travel never ended");
        let (eye, com) = (shot(view).eye, staged(view, to).com);
        let (moved, to_com, over) = ((eye - previous).norm(), (eye - com).norm(), eye - com);
        let was_com = (previous - com).norm();
        assert!(
            to_com >= rmin || (kept.began_inside && !out && to_com >= was_com - 1.0e-9),
            "frame {frames}: the eye came {to_com} m from the new craft's CoM, inside the least distance {rmin}"
        );
        if to_com >= rmin - 1.0e-6 {
            // Out of the ball: it never goes back in.
            out = true;
        }
        // Beyond the millimetre the travel itself snaps at its end.
        if let Some(last) = last_move
            && last > TRAVEL_DONE_M
        {
            let ratio = moved / last;
            past.widest_ratio = past.widest_ratio.max(ratio);
            if !out {
                past.inside_widest_ratio = past.inside_widest_ratio.max(ratio);
            }
            if let Some(bound) = kept.bound {
                assert!(
                    moved <= bound * last + TRAVEL_DONE_M,
                    "frame {frames}: the eye moved {moved:.3} m after {last:.3} m, {ratio:.3} times"
                );
            }
        }
        let (now_yaw, now_pitch, _, _) = angles_and_distances(view);
        assert_eq!(
            (now_yaw, now_pitch),
            angles,
            "frame {frames}: the travel touched the orbit angles"
        );
        if let Some(floor_r) = kept.floor_r {
            assert!(
                eye.norm() >= floor_r - 1.0e-6,
                "frame {frames}: the eye is {} m under the floor",
                floor_r - eye.norm()
            );
        }
        // On the ball: exactly the least distance from the CoM once the
        // eye is out to it — to within a micron.
        if out && to_com <= rmin + 1.0e-6 {
            close(
                to_com,
                rmin,
                1.0e-6,
                &format!("frame {frames}: the eye on the ball"),
            );
            past.on_the_ball += 1;
            if over.dot(up) > past.top.dot(up) {
                past.top = over;
            }
            if over.dot(side) > past.flank.dot(side) {
                past.flank = over;
            }
            past.least_flank_m = past.least_flank_m.min(over.dot(side));
        }
        past.nearest_m = past.nearest_m.min(to_com);
        let over_ground = eye.norm() - R_M;
        past.over_ground_m = [
            past.over_ground_m[0].min(over_ground),
            past.over_ground_m[1].max(over_ground),
        ];
        past.eyes.push(eye);
        past.coms.push(com);
        past.was.push(was_com);
        past.moves.push(moved);
        (previous, last_move) = (eye, Some(moved));
        if (anchor(&shot(view)) - com).norm() < 1.0e-9 {
            break;
        }
    }
    past
}

/// The frame a change of focus moves on, through the real input path: the
/// two-box craft is spawned with its root `at` — put the way the first
/// one was, its CoM is as far from `at` as the first craft's CoM is from
/// [`WHEEL_ROOT`] — and the sim steps a tick with no frame time. Returns
/// the new focus, the frame held to what it owes: the new craft took the
/// focus, and neither the eye nor the view moved.
fn change_untimed(view: &mut View, from: VesselId, at: DVec3) -> VesselId {
    let before = shot(view);
    spawn(view, at);
    untimed_step(view);
    let next = view.flight_focus().expect("a craft takes the focus");
    assert_ne!(next, from, "the new craft took the focus");
    let after = shot(view);
    close(
        (after.eye - before.eye).norm(),
        0.0,
        1.0e-9,
        "the frame of the change moved the eye",
    );
    close(
        (after.back - before.back).norm(),
        0.0,
        1.0e-9,
        "the frame of the change turned the view",
    );
    next
}

/// Hold the travel `past` — from a craft whose CoM is `com_a` to one whose
/// CoM is `com_b`, the orbit at `angles` and `eye_m` out, the two of one
/// size — to where its orbit point is: about the travelling anchor, one
/// travel share a frame closer to the new craft. Outside the ball the
/// eye is that point itself. Returns the frames the point was inside the
/// ball.
fn follows_the_orbit_point(
    past: &Past,
    com_a: DVec3,
    com_b: DVec3,
    angles: (f64, f64),
    eye_m: f64,
    rmin: f64,
) -> usize {
    let (yaw, pitch) = angles;
    let k = travel_share((com_a - com_b).norm());
    let (mut offset, mut inside) = (com_a - com_b, 0);
    for (frames, (eye, com)) in past.eyes.iter().zip(&past.coms).enumerate() {
        let frame = frames + 1;
        offset = offset * (1.0 - k);
        // Under a millimetre the camera settles on the new craft that
        // same frame.
        if offset.norm() < TRAVEL_DONE_M {
            offset = DVec3::ZERO;
        }
        let travelling = com_b + offset;
        let point = travelling + orbit_dir(travelling, yaw, pitch) * eye_m;
        if (point - *com).norm() >= rmin {
            close(
                (*eye - point).norm(),
                0.0,
                1.0e-6,
                &format!("frame {frame}: outside the ball, the eye is not the orbit point"),
            );
        } else {
            inside += 1;
        }
    }
    inside
}

/// Hold what the travel `past` saw on the new craft's ball to the way its
/// orbit point crossed it: over the craft, in the yaw's own vertical
/// plane, or — the way being a diameter — round the yaw's own side, level
/// with the CoM. `up` is the craft's local up and `side` the yaw's own
/// side.
fn went_over_or_round(past: &Past, up: DVec3, side: DVec3, rmin: f64, over_the_top: bool) {
    if over_the_top {
        assert!(
            past.top.dot(up) > 0.8 * rmin,
            "the eye rose {:.3} m above the CoM, not over the craft",
            past.top.dot(up)
        );
        assert!(
            past.flank.dot(side).abs() < 0.05 && past.least_flank_m > -0.05,
            "the eye left the yaw's vertical plane: {:.3}..{:.3} m along the yaw's own side",
            past.least_flank_m,
            past.flank.dot(side)
        );
    } else {
        assert!(
            past.flank.dot(side) > 0.8 * rmin,
            "the eye came {:.3} m along the yaw's own side",
            past.flank.dot(side)
        );
        assert!(
            past.top.dot(up) < 0.05,
            "the eye rose {:.3} m above the CoM's level: not round the side",
            past.top.dot(up)
        );
        assert!(
            past.least_flank_m > -0.05,
            "the eye crossed the yaw's line: {:.3} m over",
            past.least_flank_m
        );
    }
}

/// A focus travel whose orbit point crosses the new craft's least
/// distance in the AIR: the default camera (pitch +0.25 rad) and a level
/// one, a second craft 20 m and 60 m toward the camera. The eye used to be
/// carried straight through — 2.585 m from the new craft's CoM on the
/// default camera, 0.057 m, through the craft, on a level one — because
/// the keep-out only ever read an orbit point the floor had lifted. The
/// new craft's keep-out is a ball about it: the eye rides its surface
/// from where the orbit point enters to where it leaves — over the craft
/// on the default camera, round its yaw's own side on a level one, the
/// way's being a diameter — exactly the least distance from the CoM on
/// every frame it is on the ball, and the orbit point itself before it
/// enters and after it leaves: no jump at either end, no frame a tenth
/// beyond the one before, and the frame of the change moves nothing.
#[test]
fn a_focus_travel_through_the_air_rides_the_new_crafts_ball() {
    let (eye_m, floor_r) = (4.0 * RADIUS_M, R_M + EYE_GROUND_CLEARANCE_M);
    for (what, pitch, over_the_top) in [
        ("the default camera", 0.25, true),
        ("a level camera", 0.0, false),
    ] {
        for length_m in [20.0, 60.0] {
            let (mut view, a, com_a, yaw, rmin) = wheeled_scene();
            let (yaw_dir, up) = (orbit_dir(com_a, yaw, 0.0), local_up(com_a));
            let side = yaw_dir.cross(up);
            drag_to_pitch(&mut view, pitch);
            let before = shot(&view);
            // The eye is free: in the air, exactly where the angles put it.
            assert!(
                before.eye.norm() - floor_r > 1.0,
                "the eye is not in the air"
            );
            close(
                (before.eye - asked_point(com_a, yaw, pitch, eye_m)).norm(),
                0.0,
                1.0e-9,
                "the eye is not the free orbit point",
            );

            // The same craft takes the focus `length_m` toward the camera.
            // The frame of the change carries no frame time and moves
            // nothing.
            let b = change_untimed(&mut view, a, WHEEL_ROOT + yaw_dir * length_m);
            let com_b = staged(&view, b).com;
            close(
                (com_b - com_a).norm(),
                length_m,
                1.0e-2,
                "the crafts' distance",
            );
            // The premise: the orbit point's way — from where it starts
            // to where it comes to rest, a free point throughout — passes
            // within the least distance of the new craft's CoM.
            let (set_out, rest) = (
                asked_point(com_a, yaw, pitch, eye_m),
                asked_point(com_b, yaw, pitch, eye_m),
            );
            let passes_m = way_passes(set_out, rest, com_b);
            assert!(
                passes_m < rmin,
                "the orbit point's way passes {passes_m:.3} m from the new craft's CoM: outside the least distance {rmin}"
            );

            let past = travel_past(
                &mut view,
                b,
                before.eye,
                &Kept {
                    angles: (yaw, pitch),
                    yaw_dir,
                    rmin,
                    floor_r: Some(floor_r),
                    bound: Some(1.1),
                    began_inside: false,
                },
            );
            // On the ball on exactly the frames the orbit point was inside
            // it, and the orbit point itself on every other.
            let inside = follows_the_orbit_point(&past, com_a, com_b, (yaw, pitch), eye_m, rmin);
            assert_eq!(
                inside, past.on_the_ball,
                "the eye was on the ball on other frames than the orbit point was inside it"
            );
            assert!(
                inside >= 1,
                "the modelled orbit point never entered the ball"
            );
            went_over_or_round(&past, up, side, rmin, over_the_top);
            for _ in 0..3 {
                still_frame(&mut view);
            }
            close(
                (shot(&view).eye - rest).norm(),
                0.0,
                1.0e-9,
                "at rest the eye is not the orbit point about the new craft",
            );
            eprintln!(
                "focus travel through the air, {what} (pitch {pitch:+.2} rad), second craft {length_m:.0} m toward the camera: the frame of the change moved the eye 0 m; \
                 the orbit point's way passes {passes_m:.3} m from the new craft's CoM; then {} frames, the eye moving per frame {:?} m; \
                 on the ball for {} of them, at most {:.3} m above the CoM and {:.3} m along the yaw's own side (least {:.3} m); \
                 the most any frame's move was of the one before {:.3}; never nearer than {:.6} m to the CoM (least distance {rmin:.6} m)",
                past.eyes.len(),
                past.moves
                    .iter()
                    .map(|m| format!("{m:.2}"))
                    .collect::<Vec<_>>(),
                past.on_the_ball,
                past.top.dot(up),
                past.flank.dot(side),
                past.least_flank_m,
                past.widest_ratio,
                past.nearest_m,
            );
        }
    }
}

/// At rest on the flat body, the eye is on a craft's circle on the
/// ground — exactly the least distance from its CoM, the clearance over
/// the ground — where the rule at rest puts it for an orbit point the
/// floor lifted into the craft's zone.
fn rests_on_the_circle(eye: DVec3, com: DVec3, rmin: f64, what: &str) {
    close(
        (eye - com).norm(),
        rmin,
        1.0e-6,
        &format!("{what}: the eye is not on the craft's circle"),
    );
    close(
        eye.norm() - R_M,
        EYE_GROUND_CLEARANCE_M,
        1.0e-9,
        &format!("{what}: the eye is not on the ground"),
    );
}

/// A carried skirt is kept out of the new craft too. The eye rides the
/// OLD craft's circle on the ground — its orbit point dragged to −1.4 rad
/// with a second craft 12 m toward the camera, and to the pitch limit
/// with one 3 m toward it — and the old craft's skirt travels on with the
/// orbit point. Added on AFTER the new craft's keep-out it used to carry
/// the eye inside the new craft's least distance: 3.783 m for one frame,
/// and 3.856 m for 27 frames. The keep-out reads the orbit point with the
/// skirt still on it: the eye is never nearer than the least distance on
/// any frame, no frame moves it a tenth beyond the one before, the frame
/// of the change moves nothing, and at rest it is on the new craft's
/// circle on the ground — where the rule at rest puts it.
#[test]
fn a_carried_skirt_is_kept_out_of_the_new_craft_too() {
    let (eye_m, floor_r) = (4.0 * RADIUS_M, R_M + EYE_GROUND_CLEARANCE_M);
    for (what, pitch, length_m, began_inside) in [
        (
            "pitch -1.4 rad, a second craft 12 m toward the camera",
            -1.4,
            12.0,
            false,
        ),
        (
            "the pitch limit, a second craft 3 m toward the camera",
            -PITCH_LIMIT,
            3.0,
            true,
        ),
    ] {
        let (mut view, a, com_a, yaw, rmin) = wheeled_scene();
        let yaw_dir = orbit_dir(com_a, yaw, 0.0);
        drag_to_pitch(&mut view, pitch);
        let before = shot(&view);
        // The eye is kept out of the OLD craft: on its circle, on the
        // ground.
        rests_on_the_circle(before.eye, com_a, rmin, "before the change");

        // The second craft takes the focus. The frame of the change
        // carries no frame time and moves nothing.
        let b = change_untimed(&mut view, a, WHEEL_ROOT + yaw_dir * length_m);
        let com_b = staged(&view, b).com;
        // The premise: the eye, on the old craft's circle, begins outside
        // the new craft's least distance; its orbit point comes to rest
        // inside it — and, at the pitch limit with the craft 3 m away,
        // begins inside it too.
        let (set_out, rest_point) = (
            owed_eye(com_a, yaw, pitch, eye_m, floor_r),
            owed_eye(com_b, yaw, pitch, eye_m, floor_r),
        );
        let to_com_b = (set_out - com_b).norm();
        assert_eq!(
            to_com_b < rmin,
            began_inside,
            "{what}: the orbit point begins {to_com_b:.3} m from the new craft's CoM (least distance {rmin:.3} m)"
        );
        assert!(
            (rest_point - com_b).norm() < rmin,
            "{what}: the orbit point does not come to rest inside the new craft's least distance"
        );
        assert!(
            (before.eye - com_b).norm() > rmin,
            "{what}: the eye begins {:.3} m from the new craft's CoM: inside the least distance {rmin}",
            (before.eye - com_b).norm()
        );

        let past = travel_past(
            &mut view,
            b,
            before.eye,
            &Kept {
                angles: (yaw, pitch),
                yaw_dir,
                rmin,
                floor_r: Some(floor_r),
                bound: Some(1.1),
                began_inside: false,
            },
        );
        for _ in 0..3 {
            still_frame(&mut view);
        }
        let rest = shot(&view).eye;
        rests_on_the_circle(rest, com_b, rmin, "at rest");
        eprintln!(
            "carried skirt kept out of the new craft, {what}: the frame of the change moved the eye 0 m; \
             the eye starts {:.3} m from the new craft's CoM, its orbit point {to_com_b:.3} m; \
             then {} frames, the eye moving per frame {:?} m, on the ball for {} of them; \
             the most any frame's move was of the one before {:.3}; never nearer than {:.6} m to the CoM (least distance {rmin:.6} m); \
             at rest {:.6} m from the CoM",
            (before.eye - com_b).norm(),
            past.eyes.len(),
            past.moves
                .iter()
                .map(|m| format!("{m:.2}"))
                .collect::<Vec<_>>(),
            past.on_the_ball,
            past.widest_ratio,
            past.nearest_m,
            (rest - com_b).norm(),
        );
    }
}

/// Scenes where the eye itself begins the change inside the new craft's
/// least distance: the pitch it is dragged to, how far the second craft is
/// from the first, and where.
const INSIDE_STARTS: [(f64, f64, Placement); 5] = [
    // On the ground 3.77 m out along the yaw: the new craft's CoM 2.4 m
    // from the eye, and the orbit point's own way on through its zone.
    (-1.20, 5.0, Placement::Toward),
    // On the ground 5.6 m out: 3.2 m from the eye.
    (-1.00, 8.0, Placement::Toward),
    // On the ground 10.07 m out, the new craft 2.85 m from the eye: its
    // way leaves the ball within two frames.
    (-0.25, 12.0, Placement::Toward),
    // In the air, 1.03 m over the ground.
    (-0.10, 12.0, Placement::Toward),
    // On the first craft's circle, 3.2 m to the yaw's own side of it:
    // the new craft 2.8 m from the eye.
    (-PITCH_LIMIT, 5.0, Placement::YawSide),
];

/// The scene of a change from inside the new craft's ball, standing on
/// the frame of the change.
struct Begun {
    view: View,
    /// The new craft, its CoM, and the local up.
    b: VesselId,
    com_b: DVec3,
    up: DVec3,
    /// The yaw's horizontal at the new craft.
    yaw_dir: DVec3,
    /// The orbit as the input holds it after the drag.
    angles: (f64, f64),
    distance: f64,
    /// The camera after the frame of the change, and the eye's move on it.
    after: Shot,
    moved_m: f64,
    /// The eye's distance from the new craft's CoM on the frame before (m).
    start_m: f64,
    /// The change began on the ground.
    on_the_ground: bool,
}

/// One of [`INSIDE_STARTS`] through the real input path up to and
/// including the frame of the change — carrying frame time where `timed`
/// — held to what that frame owes: the new craft took the focus; the eye
/// began inside its least distance; and the frame moved nothing, or with
/// frame time moved the eye no more than a tenth beyond the orbit point's
/// own step and brought it no nearer to the CoM. `what` names the change.
fn change_from_inside(
    pitch: f64,
    length_m: f64,
    placement: Placement,
    timed: bool,
    what: &str,
) -> Begun {
    let floor_r = R_M + EYE_GROUND_CLEARANCE_M;
    let rmin = 3.0_f64.max(1.5 * RADIUS_M);
    let (mut view, a, com_a, yaw, _) = wheeled_scene();
    let up = local_up(com_a);
    let yaw_dir = orbit_dir(com_a, yaw, 0.0);
    let side = yaw_dir.cross(up);
    drag_to_pitch(&mut view, pitch);
    // The orbit as the input holds it after the drag — the pitch to the
    // input's own last bit, not the drag's.
    let (yaw, pitch, distance, _) = angles_and_distances(&view);
    let before = shot(&view);
    // What the previous focus left in hand: the eye's offset from its
    // own orbit point, which the carried point starts with and the travel
    // hands over.
    let skirt = before.eye - owed_eye(com_a, yaw, pitch, distance, floor_r);
    let on_the_ground = before.eye.norm() <= floor_r + 1.0e-6;
    let at = WHEEL_ROOT + placement.offset(yaw_dir, side, length_m);
    spawn(&mut view, at);
    if timed {
        frame(&mut view);
    } else {
        untimed_step(&mut view);
    }
    let b = view.flight_focus().expect("a craft takes the focus");
    assert_ne!(b, a, "{what}: the new craft took the focus");
    let com_b = staged(&view, b).com;
    let after = shot(&view);
    let start_m = (before.eye - com_b).norm();
    assert!(
        start_m < rmin,
        "{what}: the eye does not begin inside the new craft's least distance: {start_m:.3} m from its CoM ({rmin:.3} m)"
    );
    let now_m = (after.eye - com_b).norm();
    assert!(
        now_m >= start_m - 1.0e-9,
        "{what}: the frame of the change brought the eye {now_m} m from the CoM, from {start_m} m"
    );
    let moved_m = (after.eye - before.eye).norm();
    if timed {
        // The orbit point's own step: the travel's, and of the first
        // craft's skirt what the travel hands over of it.
        let travel_m = (com_a - com_b).norm();
        let k = travel_share(travel_m);
        let handed = skirt.norm() * (1.0 - travel_blend(travel_m * (1.0 - k), travel_m, distance));
        let step_m = k * travel_m + handed;
        assert!(
            moved_m <= 1.1 * step_m * 1.001 + 1.0e-6,
            "{what}: the frame of the change moved the eye {moved_m} m for the orbit point's own step {step_m} m"
        );
        assert!(
            moved_m > 0.0,
            "{what}: the frame of the change moved nothing"
        );
    } else {
        close(
            moved_m,
            0.0,
            1.0e-9,
            "{what}: the frame of the change moved the eye",
        );
        close(
            (after.back - before.back).norm(),
            0.0,
            1.0e-9,
            "{what}: the frame of the change turned the view",
        );
    }
    Begun {
        view,
        b,
        com_b,
        up,
        yaw_dir,
        angles: (yaw, pitch),
        distance,
        after,
        moved_m,
        start_m,
        on_the_ground,
    }
}

/// What the travel after such a frame of change came to.
struct LeftBall {
    /// The eye's move on each of the travel's frames (m).
    moves: Vec<f64>,
    on_the_ball: usize,
    /// The most any frame's move was of the one before.
    widest_ratio: f64,
    /// How much further from the CoM the eye's nearest was than where it
    /// began (m).
    gain_m: f64,
    /// The least and the most of the eye's height over the ground (m).
    over_ground_m: [f64; 2],
    /// The eye's move on the travel's last frame (m).
    end_m: f64,
}

/// Step the travel that follows to its end and hold it to what leaving the
/// ball owes ([`travel_past`]'s, and): on its way out the eye moves away
/// from the craft — its distance to the CoM grows, and so does its
/// distance out from the vertical through it; a change that began on the
/// ground keeps the eye on it; and the travel's last frame is no step.
fn leave_the_ball(begun: &mut Begun, what: &str) -> LeftBall {
    let (rmin, floor_r) = (3.0_f64.max(1.5 * RADIUS_M), R_M + EYE_GROUND_CLEARANCE_M);
    let Begun {
        view,
        b,
        com_b,
        up,
        yaw_dir,
        angles,
        after,
        start_m,
        on_the_ground,
        ..
    } = begun;
    let (com_b, up) = (*com_b, *up);
    let past = travel_past(
        view,
        *b,
        after.eye,
        &Kept {
            angles: *angles,
            yaw_dir: *yaw_dir,
            rmin,
            floor_r: Some(floor_r),
            bound: Some(1.1),
            began_inside: true,
        },
    );
    assert!(
        past.on_the_ball > 0,
        "{what}: the eye never rode the new craft's ball"
    );
    let over = |eye: DVec3| {
        let out = eye - com_b;
        (out - up * out.dot(up)).norm()
    };
    let mut was_over_m = over(after.eye);
    for i in 0..past.eyes.len() {
        let (was_m, now_m) = (past.was[i], (past.eyes[i] - past.coms[i]).norm());
        if was_m < rmin - 1.0e-6 && past.moves[i] > TRAVEL_DONE_M {
            assert!(
                now_m > was_m,
                "{what}: frame {}: on its way out the eye came {now_m} m from the CoM after {was_m} m",
                i + 1
            );
            let out_m = over(past.eyes[i]);
            assert!(
                out_m >= was_over_m - 1.0e-9,
                "{what}: frame {}: on its way out the eye moved back toward the craft",
                i + 1
            );
        }
        was_over_m = over(past.eyes[i]);
    }
    let over_ground_m = past.over_ground_m;
    let [low, high] = over_ground_m;
    if *on_the_ground {
        assert!(
            low >= EYE_GROUND_CLEARANCE_M - CARRIED_OFF_THE_GROUND_M
                && high <= EYE_GROUND_CLEARANCE_M + CARRIED_OFF_THE_GROUND_M,
            "{what}: the eye was {low:.6}..{high:.6} m over the ground over the travel"
        );
    }
    let end_m = *past.moves.last().expect("the travel's frames");
    assert!(
        end_m <= 2.0 * TRAVEL_DONE_M,
        "{what}: the frame the travel ended on moved the eye {end_m} m: a step"
    );
    LeftBall {
        moves: past.moves,
        on_the_ball: past.on_the_ball,
        widest_ratio: past.widest_ratio,
        gain_m: past.nearest_m - *start_m,
        over_ground_m,
        end_m,
    }
}

/// A focus change that begins with the eye INSIDE the new craft's least
/// distance ([`INSIDE_STARTS`]), through the real input path, each on a
/// frame of the change without frame time and on one with it. The orbit
/// point's own way runs on from where the eye is — through the new
/// craft's zone — and the eye used to be handed over along it: under the
/// craft, 1.2 m from its CoM, before it came out. The frame of the change
/// moves nothing (or, given frame time, moves the eye no more than a
/// tenth beyond the orbit point's own step, and no nearer); then the eye
/// leaves the ball by the nearest way out — on the ground, away from the
/// craft — and only ever moves away from the CoM until it is the least
/// distance out, and never inside again; no frame moves it a tenth beyond
/// the one before; a change that began on the ground keeps the eye on it;
/// and the travel ends where the rule at rest puts the eye, with no step.
#[test]
fn a_focus_change_from_inside_the_new_crafts_ball_leaves_by_the_nearest_way_out() {
    let floor_r = R_M + EYE_GROUND_CLEARANCE_M;
    let rmin = 3.0_f64.max(1.5 * RADIUS_M);
    let (mut widest, mut least_gain_m, mut largest_end_m) = (0.0_f64, f64::MAX, 0.0_f64);
    for (pitch, length_m, placement) in INSIDE_STARTS {
        for timed in [false, true] {
            let what = format!(
                "pitch {pitch:.2} rad, the second craft {length_m:.0} m {}, the frame of the change {}",
                placement.name(),
                if timed { "timed" } else { "untimed" }
            );
            let mut begun = change_from_inside(pitch, length_m, placement, timed, &what);
            let (start_m, moved_m) = (begun.start_m, begun.moved_m);
            let left = leave_the_ball(&mut begun, &what);
            widest = widest.max(left.widest_ratio);
            least_gain_m = least_gain_m.min(left.gain_m);
            largest_end_m = largest_end_m.max(left.end_m);

            // At rest: where the rule at rest puts the eye — the orbit
            // point about the new craft, or its circle where that is
            // inside the least distance.
            let Begun {
                view,
                com_b,
                angles: (yaw, pitch),
                distance,
                ..
            } = &mut begun;
            for _ in 0..3 {
                still_frame(view);
            }
            let rest = shot(view).eye;
            let point = owed_eye(*com_b, *yaw, *pitch, *distance, floor_r);
            if (point - *com_b).norm() < rmin {
                rests_on_the_circle(rest, *com_b, rmin, &format!("{what}: at rest"));
            } else {
                close(
                    (rest - point).norm(),
                    0.0,
                    1.0e-9,
                    &format!("{what}: at rest the eye is not the orbit point about the new craft"),
                );
            }
            let [low, high] = left.over_ground_m;
            eprintln!(
                "focus change from inside the new craft's ball, {what}: the eye begins {start_m:.3} m from the CoM (least distance {rmin:.3} m); \
                 the frame of the change moved it {:.1e} m; then {} frames, on the ball for {}, the eye moving per frame {:?} m; \
                 the most any frame's move was of the one before {:.3}; it only ever moved away from the CoM until out, {:.3} m further from it than it began at the least; \
                 over the ground {low:.6}..{high:.6} m; the travel's last frame moved it {:.1e} m; at rest {:.6} m from the CoM",
                moved_m,
                left.moves.len(),
                left.on_the_ball,
                left.moves
                    .iter()
                    .map(|m| format!("{m:.3}"))
                    .collect::<Vec<_>>(),
                left.widest_ratio,
                left.gain_m,
                left.end_m,
                (rest - *com_b).norm(),
            );
        }
    }
    assert!(
        least_gain_m >= 0.0,
        "the eye came {} m nearer than it began",
        -least_gain_m
    );
    eprintln!(
        "focus change from inside the new craft's ball, the {} changes: the most any frame's move was of the one before {widest:.3}; \
         the eye's nearest came {least_gain_m:.3} m short of where it began at the least; \
         the largest move on the frame a travel ended on {largest_end_m:.1e} m",
        INSIDE_STARTS.len() * 2
    );
}

/// The REAL pad, through the input path: the starter craft focused, and
/// the two-box craft taking the focus from it 20 m and then 60 m toward
/// the camera — on the default camera, then on a level one. The orbit
/// point's way crosses the new craft's ball — 2.57 m above its CoM on the
/// default camera, through the craft on a level one — and the eye used to
/// be carried along it, straight in. It is never nearer than the new
/// craft's least distance on any frame, rides its ball, no frame moves a
/// tenth beyond the one before, and the frame of the change moves
/// nothing. The camera is framed as the new craft will be beforehand, so
/// that nothing but the travel moves the eye.
#[test]
fn on_the_real_pad_a_focus_travel_through_the_air_keeps_the_least_distance() {
    let rmin = 3.0_f64.max(1.5 * RADIUS_M);
    let mut view = View::new().expect("data/ load");
    view.seed_starter_craft().expect("starter seed");
    view.set_flight_viewport(VIEWPORT_PX.0, VIEWPORT_PX.1);
    for _ in 0..3 {
        frame(&mut view);
    }
    wait_for_terrain(&mut view);
    for _ in 0..150 {
        still_frame(&mut view);
    }
    let mut focus = view.flight_focus().expect("the starter craft is focused");
    for (what, pitch) in [("the default camera", 0.25), ("a level camera", 0.0)] {
        for length_m in [20.0, 60.0] {
            let com_a = staged(&view, focus).com;
            let up = local_up(com_a);
            let yaw = angles_and_distances(&view).0;
            let yaw_dir = orbit_dir(com_a, yaw, 0.0);
            let side = yaw_dir.cross(up);
            // Framed as the new craft will be: the reframe has nothing to
            // ease, and the travel is all that moves the eye.
            view.world
                .resource_mut::<FlightInput>()
                .set_distance(4.0 * RADIUS_M);
            drag_to_pitch(&mut view, pitch);
            let (_, _, distance, _) = angles_and_distances(&view);
            let before = shot(&view);
            // A free eye, a whole eye distance out from the old craft:
            // the floor has nothing to say to it.
            close(
                (before.eye - com_a).norm(),
                distance,
                1.0e-6,
                "the eye is not the free orbit point about the old craft",
            );

            // The second craft takes the focus, `length_m` toward the
            // camera. The frame of the change carries no frame time and
            // moves nothing.
            spawn(&mut view, com_a + yaw_dir * length_m);
            untimed_step(&mut view);
            let next = view.flight_focus().expect("a craft takes the focus");
            assert_ne!(next, focus, "the new craft took the focus");
            let after = shot(&view);
            close(
                (after.eye - before.eye).norm(),
                0.0,
                1.0e-9,
                "the frame of the change moved the eye",
            );
            close(
                (after.back - before.back).norm(),
                0.0,
                1.0e-9,
                "the frame of the change turned the view",
            );

            let past = travel_past(
                &mut view,
                next,
                before.eye,
                &Kept {
                    angles: (yaw, pitch),
                    yaw_dir,
                    rmin,
                    floor_r: None,
                    bound: Some(1.1),
                    began_inside: false,
                },
            );
            assert!(
                past.on_the_ball >= 1,
                "the eye never rode the new craft's ball"
            );
            eprintln!(
                "real pad, focus travel through the air, {what} (pitch {pitch:+.2} rad), the two-box craft {length_m:.0} m toward the camera taking the focus (least distance {rmin:.3} m, the eye framed at {distance:.3} m): \
                 the frame of the change moved the eye 0 m; then {} frames, the eye moving per frame {:?} m, on the ball for {} of them, \
                 at most {:.3} m above the CoM and {:.3} m along the yaw's own side; \
                 the most any frame's move was of the one before {:.3}; never nearer than {:.6} m to the CoM (least distance {rmin:.6} m)",
                past.eyes.len(),
                past.moves
                    .iter()
                    .map(|m| format!("{m:.2}"))
                    .collect::<Vec<_>>(),
                past.on_the_ball,
                past.top.dot(up),
                past.flank.dot(side),
                past.widest_ratio,
                past.nearest_m,
            );
            focus = next;
        }
    }
}

/// Pump still frames until the flight packet carries the vacuum body's
/// baked heightfield: until it lands the terrain is sky-only and the
/// cascade row is disarmed by design.
fn wait_for_terrain(view: &mut View) {
    for _ in 0..12_000 {
        still_frame(view);
        let landed = view
            .take_flight_packet()
            .is_some_and(|p| p.terrain.is_some_and(|t| t.heightfield.is_some()));
        if landed {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the background bake never landed in the flight packet");
}

/// The cascade row the frame owes for an `up`: the solved camera's
/// forward (the reverse-Z projection's third row), the staged sun and
/// the default cascade budget — `solve_cascades` called exactly as
/// `cascade_uniforms` calls it.
fn cascade_row(view: &View, up: DVec3) -> csp_render::shadow::ShadowUniforms {
    let cam = &view.world.resource::<FlightCameraF64>().0;
    let f = DVec3::new(
        cam.view_proj[3][0],
        cam.view_proj[3][1],
        cam.view_proj[3][2],
    );
    let n = f.norm();
    let forward = if n > 1.0e-12 { f / n } else { DVec3::X };
    let sun_dir = view
        .world
        .resource::<csp_view::flight::FlightSnapshot>()
        .env
        .as_ref()
        .expect("physics")
        .sun_dir;
    csp_render::shadow::solve_cascades(
        &csp_render::shadow::ShadowCamera {
            sun_dir,
            forward,
            up,
            fov_y: FOV_Y,
            aspect: VIEWPORT_PX.0 / VIEWPORT_PX.1,
            near_m: 0.1,
        },
        &csp_render::shadow::CascadeConfig::default(),
    )
    .uniforms
}

/// The cascade's local up is the focused CRAFT's — the direction of its
/// drawn CoM from the body centre — on every frame of a far focus
/// change. The camera's anchor spends that travel hundreds of kilometres
/// from the craft whose shadows the cascade has to cover, and the up used
/// to be read off the anchor. Through the scripted far change (the scene
/// of `a_far_focus_change_keeps_the_orbit_and_crosses_above_the_ground`),
/// frame by frame: the row is armed and bit-identical to the solve with
/// the craft's own radial and — while the camera's anchor is far enough
/// away for the two radials to be more than 2° apart — to nothing that
/// anchor could have produced.
#[test]
fn a_far_focus_change_keeps_the_cascade_up_on_the_craft() {
    let mut view = vacuum_view();
    view.set_flight_viewport(VIEWPORT_PX.0, VIEWPORT_PX.1);
    let (on_pad, far) = far_change_scene(&mut view);
    wait_for_terrain(&mut view);
    assert_eq!(
        view.flight_focus(),
        Some(on_pad),
        "a craft on rails does not take the focus until recovered"
    );
    assert_eq!(view.recover_control(), Some(far));
    let apart = (2.0_f64).to_radians().cos();
    let (mut frames, mut apart_frames, mut first_gap_m) = (0, 0, 0.0);
    loop {
        frame(&mut view);
        frames += 1;
        assert!(frames <= 64, "the travel never ended");
        assert_eq!(
            view.flight_focus(),
            Some(far),
            "the recovered craft holds the focus"
        );
        let shadow = view.take_flight_packet().expect("a flight packet").shadow;
        let at = anchor(&shot(&view));
        let com = staged(&view, far).com;
        let (craft_up, anchor_up) = (com / com.norm(), at / at.norm());
        let want = cascade_row(&view, craft_up);
        assert!(
            want.armed(),
            "frame {frames}: the sun clears the craft's horizon, so the cascades arm"
        );
        assert_eq!(
            shadow.pars, want.pars,
            "frame {frames}: the armed row's bands"
        );
        assert_eq!(
            shadow.vp, want.vp,
            "frame {frames}: the cascade's up is not the focused craft's"
        );
        assert_eq!(
            shadow.cascade, want.cascade,
            "frame {frames}: the cascade's up is not the focused craft's"
        );
        let gap_m = (at - com).norm();
        if frames == 1 {
            first_gap_m = gap_m;
        }
        if craft_up.dot(anchor_up) < apart {
            apart_frames += 1;
            assert_ne!(
                shadow.vp,
                cascade_row(&view, anchor_up).vp,
                "frame {frames}: the row is what the camera's anchor gives {gap_m:.0} m from the craft"
            );
        }
        if gap_m < 1.0e-9 {
            break;
        }
    }
    assert!(
        apart_frames >= 3,
        "only {apart_frames} frames had the anchor's radial 2° or more from the craft's"
    );
    eprintln!(
        "far focus change: {frames} frames checked, the cascade up the craft's on every one; \
         the camera's anchor started {first_gap_m:.0} m from the craft and its radial was 2° or more \
         from the craft's for {apart_frames} frames, where the anchor's own up gives a different row"
    );
}

/// An angle in radians wound into `(-π, π]` — `flight.rs: wrap_angle`.
fn wrap_angle(radians: f64) -> f64 {
    let turn = 2.0 * std::f64::consts::PI;
    radians - turn * ((radians - std::f64::consts::PI) / turn).ceil()
}

/// How far the eye went round the new craft on its ball over a travel
/// (rad): its bearing about the craft's vertical — read in the yaw's frame
/// at the craft, from the yaw's side toward the yaw's own — summed frame
/// to frame over the frames it was on the ball. A way round by the NEARER
/// side is half a turn at most, whichever leg it rides; the long way round
/// the craft is more.
fn round_the_craft(past: &Past, yaw: f64, rmin: f64) -> f64 {
    let (mut travel, mut last) = (0.0, Option::<f64>::None);
    for (eye, com) in past.eyes.iter().zip(&past.coms) {
        if ((*eye - *com).norm() - rmin).abs() <= 1.0e-6 {
            let (out, aside) = along_yaw(*eye, *com, yaw);
            let bearing = aside.atan2(out);
            if let Some(before) = last {
                travel += wrap_angle(bearing - before).abs();
            }
            last = Some(bearing);
        } else {
            last = None;
        }
    }
    travel
}

/// The frame of a travel on which any frame's move was the most of the one
/// before, over the frames moving more than a millimetre: its number from
/// 1, the ratio, and the two moves (m).
fn widest_step(past: &Past) -> Option<(usize, f64, f64, f64)> {
    let mut widest: Option<(usize, f64, f64, f64)> = None;
    for i in 1..past.moves.len() {
        let (last, moved) = (past.moves[i - 1], past.moves[i]);
        if last > TRAVEL_DONE_M && moved / last > widest.map_or(0.0, |w| w.1) {
            widest = Some((i + 1, moved / last, last, moved));
        }
    }
    widest
}

/// How far off the ground the eye may be carried over a travel (m). The
/// previous focus's skirt is a tangent of ITS ground: still carried tens of
/// metres along, it takes the orbit point a fraction of a millimetre off
/// the floor — 0.3 mm at the most over the sweep below.
const CARRIED_OFF_THE_GROUND_M: f64 = 5.0e-4;

/// Hold a travel to the nearer way round the new craft — half a turn at
/// most — and return how far round the eye did go (rad).
fn went_the_nearer_way(past: &Past, yaw: f64, rmin: f64, what: &str) -> f64 {
    let round = round_the_craft(past, yaw, rmin);
    assert!(
        round <= std::f64::consts::PI + 1.0e-6,
        "{what}: the eye went {round:.3} rad round the new craft — the long way"
    );
    round
}

/// What one of the twelve changes saw.
struct Away {
    /// The change: the pitch and the second craft's distance.
    what: String,
    /// The eye's distance from the new craft's CoM on the frame of the
    /// change, and its orbit point's (m).
    start_m: [f64; 2],
    past: Past,
    /// How far the eye went round the new craft (rad).
    round: f64,
    /// The eye's distance from the new craft's CoM at rest (m).
    at_rest_m: f64,
}

/// One kept-out eye's change to a second craft `length_m` away from the
/// camera's side, the eye dragged to `pitch` on the first craft's circle on
/// the ground: through the real input path, held to everything the change
/// owes. The frame of the change moves nothing; the eye and its orbit point
/// begin outside the new craft's least distance, which the orbit point
/// comes to rest inside; the eye stays on the ground all through the
/// travel; never comes nearer than the least distance; goes round by the
/// nearer way; no frame moves it more than a tenth beyond the one before,
/// beyond the millimetre the travel snaps at its end; and at rest it is on
/// the new craft's circle on the ground.
fn kept_out_change_away(pitch: f64, length_m: f64) -> Away {
    let (eye_m, floor_r) = (4.0 * RADIUS_M, R_M + EYE_GROUND_CLEARANCE_M);
    let what =
        format!("pitch {pitch:.2} rad, a second craft {length_m:.0} m away from the camera's side");
    let (mut view, a, com_a, yaw, rmin) = wheeled_scene();
    let yaw_dir = orbit_dir(com_a, yaw, 0.0);
    drag_to_pitch(&mut view, pitch);
    let before = shot(&view);
    // The eye is kept out of the OLD craft: on its circle, on the ground.
    rests_on_the_circle(
        before.eye,
        com_a,
        rmin,
        &format!("{what}: before the change"),
    );

    // The second craft takes the focus. The frame of the change carries
    // no frame time and moves nothing.
    let b = change_untimed(&mut view, a, WHEEL_ROOT - yaw_dir * length_m);
    let com_b = staged(&view, b).com;
    close(
        (com_b - com_a).norm(),
        length_m,
        1.0e-2,
        &format!("{what}: the crafts' distance"),
    );
    // The premise: the eye, on the old craft's circle, and its orbit point
    // begin outside the new craft's least distance, and the orbit point
    // comes to rest inside it.
    let (set_out, rest_point) = (
        owed_eye(com_a, yaw, pitch, eye_m, floor_r),
        owed_eye(com_b, yaw, pitch, eye_m, floor_r),
    );
    for (which, point) in [("the eye", before.eye), ("its orbit point", set_out)] {
        assert!(
            (point - com_b).norm() > rmin,
            "{what}: {which} begins {:.3} m from the new craft's CoM, inside the least distance {rmin:.3} m",
            (point - com_b).norm()
        );
    }
    assert!(
        (rest_point - com_b).norm() < rmin,
        "{what}: the orbit point does not come to rest inside the new craft's least distance"
    );

    let past = travel_past(
        &mut view,
        b,
        before.eye,
        &Kept {
            angles: (yaw, pitch),
            yaw_dir,
            rmin,
            floor_r: None,
            bound: Some(1.1),
            began_inside: false,
        },
    );
    assert!(
        past.on_the_ball >= 1,
        "{what}: the eye never rode the new craft's ball"
    );
    // On the ground all through the travel: the circle the rule at rest
    // rides, and nothing else.
    let [low, high] = past.over_ground_m;
    assert!(
        low >= EYE_GROUND_CLEARANCE_M - CARRIED_OFF_THE_GROUND_M
            && high <= EYE_GROUND_CLEARANCE_M + CARRIED_OFF_THE_GROUND_M,
        "{what}: the eye was {low:.6}..{high:.6} m over the ground over the travel"
    );
    let round = went_the_nearer_way(&past, yaw, rmin, &what);
    for _ in 0..3 {
        still_frame(&mut view);
    }
    let at_rest = shot(&view).eye;
    rests_on_the_circle(at_rest, com_b, rmin, &format!("{what}: at rest"));
    Away {
        what,
        start_m: [(before.eye - com_b).norm(), (set_out - com_b).norm()],
        past,
        round,
        at_rest_m: (at_rest - com_b).norm(),
    }
}

/// A kept-out eye's change to a craft BEYOND it — away from the camera's
/// side — does not lunge. The two-box craft with the eye on its circle on
/// the ground, at −1.25, −1.30 and −1.40 rad; a second craft takes the
/// focus 6, 12, 20 and 40 m away from the camera's side, so the orbit
/// point — the first craft's skirt still carried on it — slides in over
/// the ground and comes to rest inside the new craft's least distance.
/// The carry fades as the travel closes, and that gave the orbit point's
/// way a few millimetres of drift toward the yaw's own side. Read as the
/// way the eye "set off", it turned the eye back: the LONG way round the
/// craft, and the frame the way entered on moved the eye 4.6 m after
/// 0.2 m — 23 times — with the eye rising metres off the ground. The eye
/// goes round by the nearer way now, always: the short arc from the entry
/// to the eye the rule at rest puts there ([`kept_out_change_away`] holds
/// each change to all of it).
#[test]
fn a_kept_out_eyes_change_to_a_craft_away_from_the_camera_does_not_lunge() {
    let rmin = 3.0_f64.max(1.5 * RADIUS_M);
    let (mut widest, mut roundest) = (0.0_f64, 0.0_f64);
    let (mut lowest, mut highest, mut nearest) = (f64::MAX, f64::MIN, f64::MAX);
    for pitch in [-1.25, -1.30, -1.40] {
        for length_m in [6.0, 12.0, 20.0, 40.0] {
            let away = kept_out_change_away(pitch, length_m);
            let past = &away.past;
            let [low, high] = past.over_ground_m;
            let (frame, ratio, last, moved) =
                widest_step(past).expect("a frame moving more than a millimetre");
            widest = widest.max(past.widest_ratio);
            roundest = roundest.max(away.round);
            lowest = lowest.min(low);
            highest = highest.max(high);
            nearest = nearest.min(past.nearest_m);
            eprintln!(
                "kept-out eye, {}: the frame of the change moved the eye 0 m; the eye starts {:.3} m from the new craft's CoM, its orbit point {:.3} m; \
                 then {} frames, the eye moving per frame {:?} m, on the ball for {} of them, {:.3} rad round the craft; \
                 the most any frame's move was of the one before {ratio:.3} (frame {frame}, {moved:.4} m after {last:.4} m); \
                 never nearer than {:.6} m to the CoM (least distance {rmin:.6} m); on the ground {low:.6}..{high:.6} m over it; at rest {:.6} m from the CoM",
                away.what,
                away.start_m[0],
                away.start_m[1],
                past.eyes.len(),
                past.moves
                    .iter()
                    .map(|m| format!("{m:.3}"))
                    .collect::<Vec<_>>(),
                past.on_the_ball,
                away.round,
                past.nearest_m,
                away.at_rest_m,
            );
        }
    }
    eprintln!(
        "kept-out eye, the twelve changes: the most any frame's move was of the one before {widest:.3}; \
         the most the eye went round a new craft {roundest:.3} rad; \
         the eye over the ground {lowest:.6}..{highest:.6} m; the nearest it came to a new craft {nearest:.6} m"
    );
}

/// Where the sweep puts the second craft from the first, in the yaw's
/// frame at the first.
#[derive(Clone, Copy)]
enum Placement {
    /// Along the yaw, toward the camera's side.
    Toward,
    /// Across the yaw's line, on the yaw's own side.
    YawSide,
    /// Along the yaw, away from the camera's side.
    Away,
    /// Across the yaw's line, against the yaw's own side.
    AgainstYawSide,
}

/// The sweep takes the placements in this order, each from the craft that
/// holds the focus: every craft alive at once is then at least the sweep's
/// shortest length from every other, and after the four the walk is back
/// where it set out.
const SWEEP_PLACEMENTS: [Placement; 4] = [
    Placement::Toward,
    Placement::YawSide,
    Placement::Away,
    Placement::AgainstYawSide,
];

/// How far the second craft is from the first over the sweep (m).
const SWEEP_LENGTHS_M: [f64; 7] = [3.0, 5.0, 8.0, 12.0, 20.0, 40.0, 60.0];

impl Placement {
    /// What the sweep calls it.
    fn name(self) -> &'static str {
        match self {
            Self::Toward => "toward the camera's side",
            Self::YawSide => "on the yaw's own side",
            Self::Away => "away from the camera's side",
            Self::AgainstYawSide => "against the yaw's own side",
        }
    }

    fn offset(self, yaw_dir: DVec3, side: DVec3, length_m: f64) -> DVec3 {
        match self {
            Self::Toward => yaw_dir * length_m,
            Self::YawSide => side * length_m,
            Self::Away => yaw_dir * -length_m,
            Self::AgainstYawSide => side * -length_m,
        }
    }
}

/// What one placement's changes came to over the sweep.
struct Tally {
    changes: usize,
    /// The changes whose eye rode the new craft's ball, and those whose
    /// eye itself began inside the new craft's least distance.
    rode: usize,
    began_inside: usize,
    /// The changes with a frame moving more than a tenth beyond the one
    /// before, over the frames moving more than a millimetre.
    over: usize,
    /// Over the changes whose eye began inside the new craft's least
    /// distance: the least its nearest ever came to where it began (m) —
    /// never less — and the most any frame's move was of the one before
    /// before the eye was out.
    began_inside_m: [f64; 2],
    /// The most any frame's move was of the one before, and where: the
    /// change, the frame, and the two moves.
    widest: f64,
    where_widest: String,
    /// The most the eye went round a new craft (rad).
    roundest: f64,
    /// The least and the most of the eye's height over the ground over
    /// the changes that began on the ground (m).
    over_ground_m: [f64; 2],
}

impl Tally {
    fn new() -> Self {
        Self {
            changes: 0,
            rode: 0,
            began_inside: 0,
            over: 0,
            began_inside_m: [f64::MAX, 0.0],
            widest: 0.0,
            where_widest: String::new(),
            roundest: 0.0,
            over_ground_m: [f64::MAX, f64::MIN],
        }
    }
}

/// What one of the sweep's changes came to.
struct Swept {
    /// The new focus.
    focus: VesselId,
    past: Past,
    /// The eye itself began inside the new craft's least distance.
    began_inside: bool,
    /// The change began on the ground (and the eye stayed on it).
    on_the_ground: bool,
    /// How far the eye went round the new craft (rad).
    round: f64,
}

/// One of the sweep's changes: the second craft `length_m` from the craft
/// that holds the focus, `placement` from it, the orbit at `angles`; the
/// craft that held the focus two changes back, `stale`, is terminated on
/// the change's tick. Through the real input path, the change is held to
/// what it owes: the frame of the change moves nothing; the eye never goes
/// under the floor, and a change that began on the ground ends on it at
/// the clearance over it; the eye never comes nearer than the new craft's
/// least distance, or, having begun inside it, never nearer than it began
/// and never inside it again once out; it goes round by the nearer way;
/// and in every placement no frame moves it more than a tenth beyond the
/// one before, beyond the millimetre the travel snaps at its end.
fn swept_change(
    view: &mut View,
    focus: VesselId,
    stale: Option<VesselId>,
    angles: (f64, f64),
    length_m: f64,
    placement: Placement,
) -> Swept {
    let rmin = 3.0_f64.max(1.5 * RADIUS_M);
    let (yaw, pitch) = angles;
    let what = format!(
        "pitch {pitch:.4} rad, the second craft {length_m:.0} m {}",
        placement.name()
    );
    let (com_a, root) = (staged(view, focus).com, staged(view, focus).pos);
    let yaw_dir = orbit_dir(com_a, yaw, 0.0);
    let side = yaw_dir.cross(local_up(com_a));
    // The new craft's origin, as far from the first's as the sweep asks,
    // back on the first's own sphere: the walk stays over the ground.
    let asked = root + placement.offset(yaw_dir, side, length_m);
    let at = asked * (root.norm() / asked.norm());
    if let Some(old) = stale {
        view.enqueue_command(
            SourceClass::Ui,
            PLAYER_SOURCE_ID,
            SimCommand::TerminateVessel { vessel: old },
        );
    }
    let before = shot(view);
    let b = change_untimed(view, focus, at);
    if let Some(old) = stale {
        assert!(
            view.sim.vessels().get(&old).is_none(),
            "{what}: the craft terminated on this tick is still in the sim"
        );
    }
    let com_b = staged(view, b).com;
    close(
        (com_b - com_a).norm(),
        length_m,
        1.0e-2,
        &format!("{what}: the crafts' distance"),
    );
    let began_inside = (before.eye - com_b).norm() < rmin;
    let began_on_the_ground = before.eye.norm() <= R_M + EYE_GROUND_CLEARANCE_M + 1.0e-6;
    let past = travel_past(
        view,
        b,
        before.eye,
        &Kept {
            angles,
            yaw_dir,
            rmin,
            floor_r: None,
            bound: Some(1.1),
            began_inside,
        },
    );
    let [low, high] = past.over_ground_m;
    assert!(
        low >= EYE_GROUND_CLEARANCE_M - CARRIED_OFF_THE_GROUND_M,
        "{what}: the eye went {low:.6} m over the ground, under the floor"
    );
    // A change that began on the ground ends on it, at the clearance
    // over it: its eye was never off the ground.
    if began_on_the_ground {
        assert!(
            high <= EYE_GROUND_CLEARANCE_M + CARRIED_OFF_THE_GROUND_M,
            "{what}: the eye rose {high:.6} m over the ground off it"
        );
    }
    let round = went_the_nearer_way(&past, yaw, rmin, &what);
    Swept {
        focus: b,
        past,
        began_inside,
        on_the_ground: began_on_the_ground,
        round,
    }
}

/// Count one change on its placement's tally.
fn count(tally: &mut Tally, swept: &Swept, pitch: f64, length_m: f64) {
    let past = &swept.past;
    let step = widest_step(past);
    if step.is_some_and(|s| s.1 > 1.1) {
        tally.over += 1;
    }
    if past.widest_ratio > tally.widest {
        let (frame, _, last, moved) = step.expect("a frame moving more than a millimetre");
        tally.widest = past.widest_ratio;
        tally.where_widest = format!(
            "the second craft {length_m:.0} m, pitch {pitch:.4} rad: frame {frame}, {moved:.4} m after {last:.4} m"
        );
    }
    tally.changes += 1;
    tally.rode += usize::from(past.on_the_ball > 0);
    if swept.began_inside {
        tally.began_inside += 1;
        tally.began_inside_m = [
            tally.began_inside_m[0].min(past.nearest_m - past.start_m),
            tally.began_inside_m[1].max(past.inside_widest_ratio),
        ];
    }
    tally.roundest = tally.roundest.max(swept.round);
    if swept.on_the_ground {
        let [low, high] = past.over_ground_m;
        tally.over_ground_m = [
            tally.over_ground_m[0].min(low),
            tally.over_ground_m[1].max(high),
        ];
    }
}

/// Run the sweep's changes — the pitch dragged to each of `pitches` in
/// turn, then the second craft at every length in every placement — each
/// from the craft the one before ended on, in one session, and count each
/// on its placement's tally. Returns how many changes began with the eye
/// itself inside the new craft's least distance, and how many began on the
/// ground.
fn sweep(tallies: &mut [Tally; 4], pitches: &[f64]) -> [usize; 2] {
    let (mut view, mut focus, _, _, _) = wheeled_scene();
    // The craft that held the focus two changes back is gone before the
    // next is spawned: no more than three are ever alive.
    let mut stale: Option<VesselId> = None;
    let mut began = [0; 2];
    for &pitch in pitches {
        drag_to_pitch(&mut view, pitch);
        // The orbit as the input holds it after the drag — the pitch to
        // the input's own last bit, not the drag's.
        let angles = {
            let (yaw, pitch, _, _) = angles_and_distances(&view);
            (yaw, pitch)
        };
        for length_m in SWEEP_LENGTHS_M {
            for (placement, tally) in SWEEP_PLACEMENTS.into_iter().zip(tallies.iter_mut()) {
                let swept = swept_change(&mut view, focus, stale, angles, length_m, placement);
                began[0] += usize::from(swept.began_inside);
                began[1] += usize::from(swept.on_the_ground);
                count(tally, &swept, pitch, length_m);
                (stale, focus) = (Some(focus), swept.focus);
            }
        }
    }
    began
}

/// The sweep's totals and each placement's, as the tests print them.
fn report_sweep(tallies: &[Tally; 4], began: [usize; 2]) {
    let changes: usize = tallies.iter().map(|t| t.changes).sum();
    let rode: usize = tallies.iter().map(|t| t.rode).sum();
    let over: usize = tallies.iter().map(|t| t.over).sum();
    let inside: usize = tallies.iter().map(|t| t.began_inside).sum();
    let roundest = tallies.iter().map(|t| t.roundest).fold(0.0, f64::max);
    let widest = tallies.iter().map(|t| t.widest).fold(0.0, f64::max);
    let inside_widest = tallies
        .iter()
        .map(|t| t.began_inside_m[1])
        .fold(0.0, f64::max);
    let gained = tallies
        .iter()
        .map(|t| t.began_inside_m[0])
        .fold(f64::MAX, f64::min);
    let [mut lowest, mut highest] = [f64::MAX, f64::MIN];
    for tally in tallies {
        lowest = lowest.min(tally.over_ground_m[0]);
        highest = highest.max(tally.over_ground_m[1]);
    }
    assert_eq!(changes, 868, "the sweep's changes");
    assert!(
        rode > 300,
        "only {rode} of {changes} changes rode the new craft's ball"
    );
    assert!(
        began[1] > 700,
        "only {} changes began on the ground",
        began[1]
    );
    assert_eq!(inside, began[0], "the sweep's changes that began inside");
    assert!(
        widest <= 1.1,
        "a frame moved the eye {widest:.3} times the one before"
    );
    assert_eq!(
        over, 0,
        "{over} changes with a frame more than a tenth beyond the one before"
    );
    assert!(
        gained >= 0.0,
        "over the {inside} changes that began inside the ball the eye came {:.3} m nearer than it began",
        -gained
    );
    eprintln!(
        "kept-out eye, the sweep: {changes} changes — the pitch from -0.10 rad to the limit, the second craft 3..60 m toward the camera's side, away from it, on the yaw's own side and against it; \
         the eye rode the new craft's ball in {rode} of them, began inside its least distance in {inside}; \
         {} began on the ground and stayed at the clearance over it, {lowest:.6}..{highest:.6} m over the ground; \
         the most it went round a new craft {roundest:.3} rad (a half turn is {:.3}); \
         the most any frame's move was of the one before {widest:.3} (a tenth is the most), {over} changes with a frame more than a tenth beyond the one before; \
         over the changes that began inside the ball the eye's nearest came {gained:.3} m short of where it began at the least (never nearer), moving at most {inside_widest:.3} times the one before while still inside",
        began[1],
        std::f64::consts::PI,
    );
    for (placement, tally) in SWEEP_PLACEMENTS.into_iter().zip(tallies) {
        eprintln!(
            "kept-out eye, the sweep, {} (held to the tenth): {} changes, the eye on the ball in {}, begun inside in {}; \
             the most any frame's move was of the one before {:.3} at {}; {} changes over the tenth; \
             the most the eye went round {:.3} rad; \
             begun inside: {}, at most {:.3} times the one before while still inside; \
             over the changes that began on the ground the eye was {:.6}..{:.6} m over it",
            placement.name(),
            tally.changes,
            tally.rode,
            tally.began_inside,
            tally.widest,
            tally.where_widest,
            tally.over,
            tally.roundest,
            if tally.began_inside == 0 {
                "none".to_owned()
            } else {
                format!(
                    "the eye's nearest {:.3} m from where it began at the least",
                    tally.began_inside_m[0]
                )
            },
            tally.began_inside_m[1],
            tally.over_ground_m[0],
            tally.over_ground_m[1],
        );
    }
}

/// The sweep: the pitch dragged from −0.10 rad to the limit in steps of
/// 0.05 rad — thirty steps, then the limit itself — and a second craft 3,
/// 5, 8, 12, 20, 40 and 60 m from the first toward the camera's side, away
/// from it, on the yaw's own side and against it: 868 changes, through the
/// real input path, each from the craft the one before ended on, in one
/// session ([`swept_change`] holds each to what it owes). The eye stays on
/// the ground, at the clearance over it, through every change that began on
/// the ground and never goes under the floor; it never comes nearer than
/// the new craft's least distance once outside it; where it began inside,
/// it only moves away from the craft's centre until it is the least
/// distance out, and never inside again; it goes round the new craft by
/// the NEARER way — half a turn at most; and in every placement no frame
/// moves it more than a tenth beyond the one before, beyond the
/// millimetre the travel snaps at its end.
///
/// Toward the camera's side and on the yaw's own side the way round is
/// LONGER than the orbit point's remaining run — toward, the orbit point's
/// way runs straight through the new craft's zone and the eye goes half a
/// turn or more round its circle; on the yaw's own side, the eye comes in
/// from the side against the yaw with most of the circle still to go. The
/// ride used to take the orbit point's progress as its own, and from the
/// frame it entered the eye moved 1.78 times what it had the frame before
/// (1.23 toward the camera's side). Its pace is its own now: a frame moves
/// it at most a tenth beyond the one before, and held back it makes up
/// what it owes as its pace allows.
///
/// Toward the camera's side the eye itself began inside the new craft's
/// zone in 73 of the changes. It used to be handed over along the orbit
/// point's way — through the craft: 0.62 m from its CoM before it came
/// out, in the worst. It leaves by the nearest way out now, and never
/// comes nearer than it began.
#[test]
fn over_the_sweep_a_kept_out_eyes_change_rounds_the_new_craft_at_an_even_pace() {
    let mut pitches: Vec<f64> = (0..30).map(|k| -0.10 - 0.05 * (k as f64)).collect();
    pitches.push(-PITCH_LIMIT);
    let mut tallies: [Tally; 4] = std::array::from_fn(|_| Tally::new());
    let began = sweep(&mut tallies, &pitches);
    report_sweep(&tallies, began);
}
