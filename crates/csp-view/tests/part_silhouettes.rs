//! §3.7's presentation floor, asserted end-to-end: the starter
//! pod→tank→engine stack stages silhouette-correct part meshes —
//! capsule vs cylinder vs bell — not three identical boxes. The same
//! classifier drives the VAB's scene, so both views get the check.

#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use csp_view::part_mesh::{self, PartShape};
use csp_view::{View, ViewMode};

/// Profile summary used for the per-part assertions.
fn profile(part: &csp_view::flight::FlightPart) -> &[(f64, f64)] {
    let PartShape::Lathe { profile, .. } = &part.shape else {
        panic!(
            "starter-craft parts must all classify to lathes, got {:?}",
            part.shape
        );
    };
    profile
}

/// One frame through the real pipeline (Input → sim step → Sync →
/// Presentation → UiBuild → RenderExtract).
fn frame(view: &mut View, ticks: u64) {
    let target = view.sim.clock().ticks() + ticks;
    view.begin_frame();
    view.sim_step(target).expect("sim step");
    view.end_frame();
}

/// The flight-side staging: the seeded "Stock Hopper" is pod/tank/
/// engine + canister/battery on the nose (W1.5-b), and the propulsion
/// parts must stage *distinct* silhouettes — a taper, a straight
/// cylinder, and a bell — not the uniform box the M4c placeholder
/// tier emitted three times over.
#[test]
fn starter_stack_stages_distinct_silhouettes() {
    let mut view = View::new().expect("data/ load");
    view.seed_starter_craft().expect("starter seed");
    frame(&mut view, 1); // the spawn lands on the first stepped tick

    // Default catalog ⇒ the kitbash-silhouette path (what this test
    // measures); the real-model path is exercised by art::load().
    let staged = csp_view::flight::capture_sync_input(
        &view.sim,
        &mut csp_view::flight::LatchedWeldCache::default(),
        &csp_view::art::ArtAssets::default(),
        &[],
    );
    let vessel = staged.vessels.first().expect("seeded vessel staged");
    assert_eq!(
        vessel.parts.len(),
        5,
        "pod + tank + engine + canister + battery"
    );
    // The stack is fully axial: engine (bottom, −z) → tank →
    // battery → canister → pod (top, +z) — the W1.5-b science pair
    // flies inline between pod and tank. Sort by z and take the
    // propulsion triple: the lowest two plus the topmost.
    let mut parts = vessel.parts.to_vec();
    parts.sort_by(|a, b| a.transform.offset.z.total_cmp(&b.transform.offset.z));
    let engine = &parts[0];
    let tank = &parts[1];
    let pod = parts.last().expect("five parts");

    // Engine — the bell: max radius on the LOWEST station (nozzle
    // opens −z), a throat, and the mount stepping back up — the
    // non-monotonic signature a pod taper never has.
    let bell = profile(engine);
    let r_max = bell.iter().map(|s| s.1).fold(0.0, f64::max);
    assert!(
        (bell[0].1 - r_max).abs() < 1e-12,
        "engine bell must flare open at its bottom station: {bell:?}"
    );
    assert!(
        bell.windows(2).any(|w| w[1].1 > w[0].1),
        "engine mount must step up past the throat: {bell:?}"
    );

    // Tank — the cylinder: every station the same radius.
    let cyl = profile(tank);
    assert!(
        cyl.windows(2).all(|w| (w[1].1 - w[0].1).abs() < 1e-12),
        "tank must lathe a constant-radius profile: {cyl:?}"
    );

    // Pod — the capsule taper: monotone non-increasing to a narrower
    // top node class.
    let pod_p = profile(pod);
    assert!(
        pod_p.windows(2).all(|w| w[1].1 <= w[0].1 + 1e-12),
        "pod must taper monotonically: {pod_p:?}"
    );
    assert!(
        pod_p.last().unwrap().1 < pod_p[0].1 * 0.8,
        "pod top must be visibly narrower than its base: {pod_p:?}"
    );

    // Distinct silhouettes → distinct mesh signatures (triangle
    // counts + vertex data), never the old uniform 12-tri box.
    let tris: Vec<usize> = [engine, tank, pod]
        .into_iter()
        .map(|p| {
            let mut t = Vec::new();
            part_mesh::emit(&p.shape, &mut t);
            t.len()
        })
        .collect();
    assert!(
        tris.iter().all(|&n| n > 12),
        "silhouettes must beat the 12-tri placeholder box: {tris:?}"
    );
    assert!(
        tris[0] != tris[1] || tris[1] != tris[2] || tris[0] != tris[2],
        "pod/tank/engine must not share one mesh signature: {tris:?}"
    );
}

/// C19-F5: the per-part render data is `Arc`-shared between the
/// fingerprint cache and every staged `FlightBody` — a second
/// capture of an unchanged craft must return the SAME allocation,
/// not a deep clone of every part's lathe profile.
#[test]
fn repeated_captures_share_the_parts_allocation() {
    let mut view = View::new().expect("data/ load");
    view.seed_starter_craft().expect("starter seed");
    frame(&mut view, 1);
    let mut cache = csp_view::flight::LatchedWeldCache::default();
    let art = csp_view::art::ArtAssets::default();
    let a = csp_view::flight::capture_sync_input(&view.sim, &mut cache, &art, &[]);
    let b = csp_view::flight::capture_sync_input(&view.sim, &mut cache, &art, &[]);
    assert_eq!(a.vessels.len(), 1);
    assert!(
        std::sync::Arc::ptr_eq(&a.vessels[0].parts, &b.vessels[0].parts),
        "an unchanged craft's parts must share the cache's allocation"
    );
}

/// The VAB view carries the same shapes — the editor sees the craft
/// as it will fly: the propulsion starter parts are lathes (the
/// battery's authored collider is an honest box), and the emitted
/// packet well past the placeholder-box vertex count (36 per part)
/// the old pass produced.
#[test]
fn vab_packet_is_kitbash_not_boxes() {
    let mut view = View::new().expect("data/ load");
    view.seed_starter_craft().expect("starter seed");
    view.set_mode(ViewMode::Vab);
    frame(&mut view, 1);

    let scene = view.world.resource::<csp_view::vab::VabScene>();
    assert_eq!(scene.parts.len(), 5);
    let lathes = scene
        .parts
        .iter()
        .filter(|p| matches!(p.shape, PartShape::Lathe { .. }))
        .count();
    assert!(
        lathes >= 3,
        "pod/tank/engine must be lathe silhouettes ({lathes} lathes staged)"
    );
    let packet = view.take_vab_packet().expect("VAB packet emitted");
    assert_eq!(packet.pick_ids.len(), 5, "one pick id per part");
    assert!(
        packet.vertices.len() > 180,
        "kitbash meshes must beat 5 placeholder boxes ({} verts)",
        packet.vertices.len()
    );
}
