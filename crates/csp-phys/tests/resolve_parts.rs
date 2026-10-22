//! §3.1 resolver integration tests over the *authored* `data/parts/`
//! set: `BuiltPart` records → `EffectivePart`s → a welded body, with
//! hand-computed expectations from the RON files. Complements
//! `csp-data`'s validator gate — the validator proves the files are
//! well-formed; these tests prove they resolve to the numbers the
//! fiction claims.

#![forbid(unsafe_code)]
// Tests signal failure by panicking — §2.13's no-panic rule is for the
// shipping fallible paths, so unwrap/expect are allowed in test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use csp_core::DVec3;
use csp_data::{LoadedParts, ModuleDef, ResourceFile, load_parts, load_resources};
use csp_phys::{
    BuiltPart, FamilyQualifications, ForceSiteKind, PartGraph, PartNode, PartNodeId, PartTransform,
    ResolveError, WeldPart, resolve_parts, weld,
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

fn built(_node: u64, part: &str, variant: &str) -> BuiltPart {
    // The graph binding is positional (`built[i]` ↔ `effective_index == i`)
    // — the helper keeps the node argument so the call sites still read
    // which graph node each record binds to.
    BuiltPart::stock(part.into(), variant.into())
}

/// The unqualified-family input: every fixture family resolves at zero
/// qualification units (§3.4 first knot).
fn no_quals() -> FamilyQualifications {
    FamilyQualifications::new()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-12 * b.abs().max(1.0)
}

/// Stock resolution of the authored set: pod + tank + engine resolve
/// to the numbers the RON fiction claims, tanks full.
#[test]
fn authored_parts_resolve_stock() {
    let defs = defs();
    let res = resources();
    let built = [
        built(0, "csp.pod.mk1", "stock"),
        built(1, "csp.tank.s160", "stock"),
        built(2, "csp.engine.sparc", "stock"),
    ];
    let eff = resolve_parts(&defs, &built, &res, &no_quals(), None).unwrap();
    assert_eq!(eff.len(), 3);

    // pod: dry 800, no tank → 800 kg, cost 2400.
    assert_eq!(eff[0].weld_input.mass_kg, 800.0);
    assert_eq!(eff[0].cost_funds, 2_400.0);
    // tank s160: dry 220 + (162 + 198) u · 5 kg/u = 2020 kg.
    assert_eq!(eff[1].weld_input.mass_kg, 2_020.0);
    assert_eq!(eff[1].cost_funds, 400.0);
    // sparc: dry 900, thrust site rated 200 kN at the −z face.
    assert_eq!(eff[2].weld_input.mass_kg, 900.0);
    assert_eq!(eff[2].cost_funds, 1_500.0);
    let site = &eff[2].weld_input.force_sites[0];
    assert_eq!(site.kind, ForceSiteKind::Thrust);
    assert_eq!(site.rated_n, 200_000.0);
    assert!(close(site.local_pos.z, -0.9));
    assert_eq!(site.local_dir, DVec3::Z);
    // Baseline hazard: λ = 1/400k at qual 1.0.
    assert_eq!(eff[2].reliability.qual_factor, 1.0);
    assert!(close(eff[2].reliability.hazard_per_s, 1.0 / 400_000.0));
}

/// The "qualified" sparc variant: +30% MTBF, +50% rated burn, ×1.2
/// cost — the authored stat-multiplier path end to end.
#[test]
fn qualified_variant_applies_authored_multipliers() {
    let defs = defs();
    let res = resources();
    let built = [built(0, "csp.engine.sparc", "qualified")];
    let eff = resolve_parts(&defs, &built, &res, &no_quals(), None).unwrap();
    let e = &eff[0];
    assert_eq!(e.cost_funds, 1_800.0);
    assert_eq!(e.reliability.base_mtbf_s, 520_000.0);
    let ModuleDef::Engine { rated_burn_s, .. } = &e.modules[0] else {
        panic!("engine expected");
    };
    assert_eq!(*rated_burn_s, 360.0);
}

/// Continuous scaling on the authored tank — the `Continuous` rule's
/// exponents (mass/capacity cubic, structural quadratic) hand-checked.
#[test]
fn scaled_tank_resolves_exponents() {
    let defs = defs();
    let res = resources();
    let mut b = built(0, "csp.tank.s160", "stock");
    b.scale = 2.0;
    let eff = resolve_parts(&defs, &[b], &res, &no_quals(), None).unwrap();
    let t = &eff[0];
    // dry 220·8 = 1760; caps 1296 + 1584 u · 5 kg/u = 14 400.
    assert_eq!(t.weld_input.mass_kg, 16_160.0);
    assert_eq!(t.cost_funds, 3_200.0); // 400 · 2³
    assert_eq!(t.structural.max_axial_n, 3_200_000.0); // 800k · 4
    let ModuleDef::Tank { capacity } = &t.modules[0] else {
        panic!("tank expected");
    };
    assert_eq!(capacity[0].1, 1_296.0);
    assert_eq!(capacity[1].1, 1_584.0);
    // Scale just outside the authored [0.5, 2.0] range is a typed error.
    let mut bad = built(0, "csp.tank.s160", "stock");
    bad.scale = 2.01;
    assert!(matches!(
        resolve_parts(&defs, &[bad], &res, &no_quals(), None),
        Err(ResolveError::InvalidScale { .. })
    ));
    // A member of a Discrete menu resolves; a non-member does not.
    // Discrete carries no exponents — the module doc's geometry-only
    // semantic: size and collider scale, authored stats do not.
    let mut disc = built(1, "csp.structure.adapter", "stock");
    let one = resolve_parts(&defs, std::slice::from_ref(&disc), &res, &no_quals(), None).unwrap();
    disc.scale = 2.0;
    let two = resolve_parts(&defs, std::slice::from_ref(&disc), &res, &no_quals(), None).unwrap();
    assert_eq!(two[0].size_m, one[0].size_m * 2.0); // geometry scales
    assert_eq!(two[0].weld_input.mass_kg, one[0].weld_input.mass_kg); // stats don't
    disc.scale = 1.5;
    assert!(matches!(
        resolve_parts(&defs, &[disc], &res, &no_quals(), None),
        Err(ResolveError::InvalidScale { .. })
    ));
}

/// The full pipeline: authored pod + tank + engine resolve, their
/// `weld_input`s weld into one `WeldedBody` with the hand-computed
/// mass, CoM, and transformed thrust site.
#[test]
fn resolved_parts_weld_end_to_end() {
    let defs = defs();
    let res = resources();
    let built = [
        built(0, "csp.pod.mk1", "stock"),
        built(1, "csp.tank.s160", "stock"),
        built(2, "csp.engine.sparc", "stock"),
    ];
    let eff = resolve_parts(&defs, &built, &res, &no_quals(), None).unwrap();
    let parts: Vec<WeldPart> = eff.iter().map(|e| e.weld_input.clone()).collect();

    // Stack: pod root; tank under it (pod bottom −0.5, tank top +1.0 →
    // offset −1.5); engine under the tank (tank bottom −1.0, sparc
    // top +0.9 → offset −1.9).
    let mut graph = PartGraph::new(PartNodeId(0));
    graph.nodes.insert(
        PartNodeId(0),
        PartNode::new(None, PartTransform::IDENTITY, 0),
    );
    graph.nodes.insert(
        PartNodeId(1),
        PartNode::new(
            Some(PartNodeId(0)),
            PartTransform::new(DVec3::new(0.0, 0.0, -1.5), csp_core::DQuat::IDENTITY),
            1,
        ),
    );
    graph.nodes.insert(
        PartNodeId(2),
        PartNode::new(
            Some(PartNodeId(1)),
            PartTransform::new(DVec3::new(0.0, 0.0, -1.9), csp_core::DQuat::IDENTITY),
            2,
        ),
    );

    let body = weld(&graph, &parts).unwrap();
    // 800 + 2020 + 900.
    assert_eq!(body.mass_kg, 3_720.0);
    // CoM: part CoMs at z 0, −1.5, −3.4 → (2020·−1.5 + 900·−3.4)/3720.
    let expected_com_z = (2_020.0 * -1.5 + 900.0 * -3.4) / 3_720.0;
    assert!(close(body.com_local.z, expected_com_z));
    assert_eq!(body.com_local.x, 0.0);
    assert_eq!(body.com_local.y, 0.0);
    // One thrust site, at the engine's −z face in body frame:
    // −1.5 −1.9 −0.9 = −4.3.
    assert_eq!(body.force_sites.len(), 1);
    assert_eq!(body.force_sites[0].kind, ForceSiteKind::Thrust);
    assert_eq!(body.force_sites[0].rated_n, 200_000.0);
    assert!(close(body.force_sites[0].local_pos.z, -4.3));
    // Three hulls — the cylinder parts became octagonal prisms.
    assert_eq!(body.collider.hulls.len(), 3);
    // TWR sanity vs the authored fiction (Terrin g ≈ 9.81).
    let twr = 200_000.0 / (body.mass_kg * 9.81);
    assert!(twr > 4.0 && twr < 7.0);
}

/// `resolve_parts` is deterministic: two calls bit-identical, and a
/// permuted `built` order yields the same per-part results — each
/// part resolves independently of its siblings.
#[test]
fn resolution_is_deterministic_and_permutation_safe() {
    let defs = defs();
    let res = resources();
    let b = [
        built(0, "csp.pod.mk1", "stock"),
        built(1, "csp.tank.s160", "stock"),
        built(2, "csp.engine.sparc", "qualified"),
    ];
    let a = resolve_parts(&defs, &b, &res, &no_quals(), None).unwrap();
    let c = resolve_parts(&defs, &b, &res, &no_quals(), None).unwrap();
    assert_eq!(a, c);

    let permuted = [b[2].clone(), b[0].clone(), b[1].clone()];
    let p = resolve_parts(&defs, &permuted, &res, &no_quals(), None).unwrap();
    // `built` order is output order, so match on part_id.
    fn find<'a>(v: &'a [csp_phys::EffectivePart], id: &str) -> &'a csp_phys::EffectivePart {
        v.iter().find(|e| e.part_id.as_str() == id).unwrap()
    }
    assert_eq!(find(&p, "csp.pod.mk1"), &a[0]);
    assert_eq!(find(&p, "csp.tank.s160"), &a[1]);
    assert_eq!(find(&p, "csp.engine.sparc"), &a[2]);
}

/// Batch-level errors: unknown part id reports node + id, and a
/// mismatched `usages` slice is a typed contract error.
#[test]
fn batch_errors_are_typed() {
    let defs = defs();
    let res = resources();
    let bad = built(7, "csp.engine.deleted", "stock");
    match resolve_parts(&defs, &[bad], &res, &no_quals(), None) {
        Err(ResolveError::UnknownPartId { part_id, index }) => {
            assert_eq!(part_id.as_str(), "csp.engine.deleted");
            assert_eq!(index, 0); // position in `built` = its effective_index
        }
        other => panic!("expected UnknownPartId, got {other:?}"),
    }
    let ok = built(0, "csp.pod.mk1", "stock");
    assert!(matches!(
        resolve_parts(
            &defs,
            std::slice::from_ref(&ok),
            &res,
            &no_quals(),
            Some(&[])
        ),
        Err(ResolveError::UsageCountMismatch { .. })
    ));
    // A matching-length usages slice resolves with wear applied —
    // the pod's 800k MTBF means 400k operating seconds is ×1.5.
    let worn = csp_phys::PartUsage {
        operating_s: 400_000.0,
        ..csp_phys::PartUsage::FRESH
    };
    let eff = resolve_parts(&defs, &[ok], &res, &no_quals(), Some(&[worn])).unwrap();
    assert_eq!(eff[0].reliability.wear_multiplier, 1.5);
}
