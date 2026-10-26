//! M3-C session-model integration tests over the *authored*
//! `data/parts/` set: `CraftSession` → `PartGraph` → `resolve_parts` →
//! `weld` end to end, craft-file round-trips with stable `PartNodeId`s,
//! and the staging model's validation semantics on real defs.
//! Complements the in-module unit tests (synthetic defs): these prove
//! the session model against the files the VAB will actually load.

#![forbid(unsafe_code)]
// Tests signal failure by panicking — §2.13's no-panic rule is for the
// shipping fallible paths, so unwrap/expect are allowed in test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use csp_data::{LoadedParts, ResourceFile, VariantId, load_parts, load_resources};
use csp_phys::{
    AttachEdge, BuiltPart, CraftSession, FamilyQualifications, PartNodeId, SessionWarning,
    resolve_parts, weld,
};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn defs() -> LoadedParts {
    load_parts(&repo_root().join("data/parts")).unwrap()
}

fn resources() -> ResourceFile {
    load_resources(&repo_root().join("data/resources.ron")).unwrap()
}

fn no_quals() -> FamilyQualifications {
    FamilyQualifications::new()
}

fn stock(id: &str) -> BuiltPart {
    BuiltPart::stock(id.into(), "stock".into())
}

/// The four-part sounding stack: pod → tank → decoupler → engine —
/// the session-model analogue of `resolve_parts.rs`'s hand-built
/// graph, so weld outputs are directly comparable.
fn sounding_stack() -> (CraftSession, LoadedParts) {
    let defs = defs();
    let mut s = CraftSession::with_name("sounding");
    let pod = s.add_root(&defs, stock("csp.pod.mk1")).unwrap();
    let tank = s
        .add_part(
            &defs,
            stock("csp.tank.s160"),
            AttachEdge::new(pod, "bottom", "top"),
        )
        .unwrap();
    let dec = s
        .add_part(
            &defs,
            stock("csp.decoupler.s125"),
            AttachEdge::new(tank, "bottom", "top"),
        )
        .unwrap();
    s.add_part(
        &defs,
        stock("csp.engine.sparc"),
        AttachEdge::new(dec, "bottom", "top"),
    )
    .unwrap();
    (s, defs)
}

/// The whole pipeline: session → canonical `(PartGraph, built)` →
/// `resolve_parts` → `weld`, with mass, CoM and the thrust site checked
/// against the authored numbers (pod 800 + tank 2020 + dec 60 + sparc
/// 900 = 3780 kg).
#[test]
fn session_to_weld_end_to_end() {
    let (s, defs) = sounding_stack();
    let res = resources();
    let (graph, built) = s.to_part_graph().unwrap();
    assert_eq!(built.len(), 4);
    let eff = resolve_parts(&defs, &built, &res, &no_quals(), None).unwrap();
    let parts: Vec<csp_phys::WeldPart> = eff.iter().map(|e| e.weld_input.clone()).collect();
    let body = weld(&graph, &parts).unwrap();
    assert_eq!(body.mass_kg, 3_780.0);
    assert_eq!(body.collider.hulls.len(), 4);
    assert_eq!(body.force_sites.len(), 1);
    assert_eq!(body.force_sites[0].rated_n, 200_000.0);
    // The snapped chain's geometry, checked end to end: pod root at 0;
    // tank top +1.0 under pod bottom −0.5 → −1.5; dec top +0.125 under
    // tank bottom −1.0 → −1.125; sparc top +0.9 under dec bottom
    // −0.125 → −1.025. The thrust site sits at sparc's −z face (−0.9
    // in its frame) → body z = −1.5 −1.125 −1.025 −0.9 = −4.55.
    let site_z = body.force_sites[0].local_pos.z;
    assert!((site_z - (-4.55)).abs() < 1e-12, "site z {site_z}");
}

/// `reroot` preserves the physical tree bit-for-bit: the weld output
/// before and after is identical — only `graph.root` moves.
#[test]
fn reroot_preserves_the_weld() {
    let (mut s, defs) = sounding_stack();
    let res = resources();
    let weld_of = |s: &CraftSession| {
        let (g, b) = s.to_part_graph().unwrap();
        let eff = resolve_parts(&defs, &b, &res, &no_quals(), None).unwrap();
        let parts: Vec<csp_phys::WeldPart> = eff.iter().map(|e| e.weld_input.clone()).collect();
        weld(&g, &parts).unwrap()
    };
    let before = weld_of(&s);
    // Re-root at the engine (the deepest node).
    let deepest = *s
        .iter()
        .map(|(id, _)| id)
        .collect::<Vec<_>>()
        .last()
        .unwrap();
    s.reroot(deepest).unwrap();
    assert_eq!(s.root(), Some(deepest));
    let after = weld_of(&s);
    assert_eq!(before, after, "reroot must not move a single body pose");
}

/// `PartNodeId` stability across save → load → save: ids, edges,
/// stages, variants and the `next_id` allocator all round-trip, and the
/// bytes are a fixed point.
#[test]
fn save_load_preserves_ids_and_bytes_are_canonical() {
    let (mut s, defs) = sounding_stack();
    let ids: Vec<PartNodeId> = s.iter().map(|(id, _)| id).collect();
    // Stage the decoupler above the engine (decoupler fires first).
    s.set_stage(ids[2], 1).unwrap();
    s.set_stage(ids[3], 0).unwrap();
    // A variant swap that must round-trip.
    s.set_variant(&defs, ids[3], VariantId::from("qualified"))
        .unwrap();
    // Remove and re-add nothing — but poke the allocator so `next_id`
    // is ahead of the live set (a hole the file must preserve). The pod
    // top is 0.625-class; the chute's `mount` node matches it (the
    // snap transform reorients the radial mount — the size class is
    // what the attach gate reads).
    let spare = s
        .add_part(
            &defs,
            stock("csp.chute.s125"),
            AttachEdge::new(ids[0], "top", "mount"),
        )
        .unwrap();
    s.remove_part(spare).unwrap();

    let bytes = s.to_bytes().unwrap();
    let loaded = CraftSession::from_bytes(&bytes).unwrap();
    assert_eq!(loaded, s, "every session field round-trips");
    // Ids are identical, not just equal in shape.
    let loaded_ids: Vec<PartNodeId> = loaded.iter().map(|(id, _)| id).collect();
    assert_eq!(loaded_ids, ids);
    assert_eq!(
        loaded.node(ids[3]).unwrap().built().variant.as_str(),
        "qualified"
    );
    // Re-serializing is byte-identical — the canonical form.
    assert_eq!(loaded.to_bytes().unwrap(), bytes);
    // A fresh allocation on the loaded session does not collide.
    let mut loaded = loaded;
    let n = loaded
        .add_part(
            &defs,
            stock("csp.chute.s125"),
            AttachEdge::new(ids[0], "top", "mount"),
        )
        .unwrap();
    assert!(n.0 > spare.0, "allocator continued past the hole");
}

/// Staging on the authored set: a decoupler staged above the engine it
/// separates starves that engine (warning, never an error — the craft
/// still validates launchable); moving the engine to an earlier-firing
/// stage clears it.
#[test]
fn staging_validation_against_real_defs() {
    let (mut s, defs) = sounding_stack();
    let ids: Vec<PartNodeId> = s.iter().map(|(id, _)| id).collect();
    // Everything stage 0: engine's region covers the whole craft — the
    // s160 tank carries liquid_fuel + oxidizer; clean.
    let v = s.validate(&defs);
    assert!(v.errors.is_empty() && v.warnings.is_empty(), "{v:?}");
    // Decoupler at stage 1, engine at stage 0: the decoupler severs
    // before the engine's stage fires — the engine's supply region is
    // itself alone.
    s.set_stage(ids[2], 1).unwrap();
    let v = s.validate(&defs);
    assert!(v.is_launchable());
    let missing = v
        .warnings
        .iter()
        .find_map(|w| match w {
            SessionWarning::EngineWithoutPropellant { node, missing, .. } if *node == ids[3] => {
                Some(missing.clone())
            }
            _ => None,
        })
        .expect("severed engine must warn");
    assert_eq!(missing.len(), 2, "both sparc propellants uncovered");
    // Engine firing *before* the separation (higher stage index) sees
    // its tank.
    s.set_stage(ids[3], 2).unwrap();
    let v = s.validate(&defs);
    assert!(
        !v.warnings
            .iter()
            .any(|w| matches!(w, SessionWarning::EngineWithoutPropellant { .. }))
    );
    // Stage order is descending — stage 2 (engine) fires before 1 (dec).
    assert_eq!(s.stage_order(), vec![2, 1, 0]);
}

/// The craft file rejects non-member scaling and bogus variants at the
/// *session* boundary, before resolve ever sees them — and a post-attach
/// rescale surfaces the size-class drift as a warning, not silence.
#[test]
fn session_gates_editor_edits() {
    let defs = defs();
    // The adapter's Discrete menu admits 0.5/1.0/2.0 — 1.5 is refused at
    // the session boundary.
    let mut s = CraftSession::new();
    let adapter = s.add_root(&defs, stock("csp.structure.adapter")).unwrap();
    let tank = s
        .add_part(
            &defs,
            stock("csp.tank.s160"),
            AttachEdge::new(adapter, "bottom", "top"),
        )
        .unwrap();
    assert!(s.set_scale(&defs, adapter, 1.5).is_err());
    // Scaling the *parent* adapter post-attach changes its node
    // diameters (1.25 → 2.5) while the 1.25-class child stays put —
    // validate flags the drift on the edge (permitted, warned).
    s.set_scale(&defs, adapter, 2.0).unwrap();
    let v = s.validate(&defs);
    assert!(v.warnings.iter().any(|w| matches!(
        w,
        SessionWarning::AttachDiameterMismatch { child } if *child == tank
    )));
}
