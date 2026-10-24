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
/// engine, and each part must stage a *distinct* silhouette — a taper,
/// a straight cylinder, and a bell — not the uniform box the M4c
/// placeholder tier emitted three times over.
#[test]
fn starter_stack_stages_distinct_silhouettes() {
    let mut view = View::new().expect("data/ load");
    view.seed_starter_craft().expect("starter seed");
    frame(&mut view, 1); // the spawn lands on the first stepped tick

    let staged = csp_view::flight::capture_sync_input(
        &view.sim,
        &mut csp_view::flight::LatchedWeldCache::default(),
    );
    let vessel = staged.vessels.first().expect("seeded vessel staged");
    assert_eq!(vessel.parts.len(), 3, "pod + tank + engine");
    // Sort by body-frame z: the stack runs engine (bottom, −z) →
    // tank → pod (top, +z) under §3.1's up-stack convention.
    let mut parts = vessel.parts.clone();
    parts.sort_by(|a, b| a.transform.offset.z.total_cmp(&b.transform.offset.z));
    let [engine, tank, pod]: [csp_view::flight::FlightPart; 3] =
        parts.try_into().expect("three parts");

    // Engine — the bell: max radius on the LOWEST station (nozzle
    // opens −z), a throat, and the mount stepping back up — the
    // non-monotonic signature a pod taper never has.
    let bell = profile(&engine);
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
    let cyl = profile(&tank);
    assert!(
        cyl.windows(2).all(|w| (w[1].1 - w[0].1).abs() < 1e-12),
        "tank must lathe a constant-radius profile: {cyl:?}"
    );

    // Pod — the capsule taper: monotone non-increasing to a narrower
    // top node class.
    let pod_p = profile(&pod);
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
    let tris: Vec<usize> = [&engine, &tank, &pod]
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

/// The VAB view carries the same shapes — the editor sees the craft
/// as it will fly: every starter part a lathe, and the emitted packet
/// well past the three-box vertex count (3·36 = 108) the placeholder
/// pass produced.
#[test]
fn vab_packet_is_kitbash_not_boxes() {
    let mut view = View::new().expect("data/ load");
    view.seed_starter_craft().expect("starter seed");
    view.set_mode(ViewMode::Vab);
    frame(&mut view, 1);

    let scene = view.world.resource::<csp_view::vab::VabScene>();
    assert_eq!(scene.parts.len(), 3);
    for p in &scene.parts {
        assert!(
            matches!(p.shape, PartShape::Lathe { .. }),
            "starter part {:?} must be a lathe silhouette",
            p.node
        );
    }
    let packet = view.take_vab_packet().expect("VAB packet emitted");
    assert_eq!(packet.pick_ids.len(), 3, "one pick id per part");
    assert!(
        packet.vertices.len() > 108,
        "kitbash meshes must beat 3 placeholder boxes ({} verts)",
        packet.vertices.len()
    );
}
