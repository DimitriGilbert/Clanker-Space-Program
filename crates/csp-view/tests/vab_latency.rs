//! §5.4's VAB kill criterion, measured (M3 exit row: "VAB latency
//! ≤16 ms at 300 parts on REF-A").
//!
//! Pointer-down → visual response is, at this tier: one session op
//! (`AttachAuto`/`SetStage`/…), the dirty-gated `recompute`
//! (`resolve_parts` + `analyze` + `validate` + `body_transforms`), and
//! a full frame's `end_frame` (Sync → Presentation → UiBuild →
//! RenderExtract — including the real egui pass and the packet build).
//! The GPU pick readback adds one submit + map_async, which lands one
//! frame later — inside the budget by construction.
//!
//! The 16 ms bound is a *release* number (REF-A). In debug builds the
//! same work legitimately costs ~10× more, so the debug assertion is a
//! 200 ms regression alarm, and the printed breakdown is the honest
//! record for the perf log.

#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::time::Instant;

use csp_data::{PartId, load_parts, load_resources};
use csp_sim::World as SimWorld;
use csp_ui::vab::VabAction;
use csp_view::{View, ViewMode};

fn data_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data")
}

/// The worst-case interaction measured: one op applied at a 300-part
/// craft, then the whole frame pipeline (session op → recompute →
/// scene → egui pass → render packet).
#[test]
#[allow(clippy::too_many_lines)]
fn vab_interaction_latency_at_300_parts() {
    let root = data_root();
    let parts = load_parts(&root.join("parts")).unwrap();
    let res = load_resources(&root.join("resources.ron")).unwrap();

    let mut view = View::with_sim(SimWorld::new());
    view.install_vab_catalog(parts, res, csp_data::data_set_hash(&root).ok());
    view.set_mode(ViewMode::Vab);

    // Build a 300-part stack through the real op channel — root tank +
    // 299 attached tanks (csp.tank.s320 has matching top/bottom 1.25 m
    // nodes). Time the build too: it is 300 op+no-recompute calls.
    let t0 = Instant::now();
    view.with_vab_session(|v| {
        v.apply(VabAction::SetRoot(PartId::from("csp.tank.s320")))
            .unwrap();
    });
    let mut parent = view.with_vab_session(|v| v.session().root().unwrap());
    for _ in 1..300 {
        view.with_vab_session(|v| {
            v.apply(VabAction::AttachAuto {
                part: PartId::from("csp.tank.s320"),
                parent,
            })
            .unwrap();
        });
        parent = view.with_vab_session(|v| v.session().iter().last().unwrap().0);
    }
    let build_ms = t0.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(view.with_vab_session(|v| v.session().len()), 300);

    // Per-frame cost with a clean session (camera-only frame): the
    // steady-state budget the readout's dirty-gating buys. The 300-op
    // build left the session dirty, so one unmeasured frame runs the
    // recompute first — measuring THAT frame would bill the full
    // resolve+analyze pass to the "clean" number.
    view.begin_frame();
    view.end_frame();
    view.begin_frame();
    let t = Instant::now();
    view.end_frame();
    let clean_frame_ms = t.elapsed().as_secs_f64() * 1000.0;

    // The interaction measure: one op + one full end_frame. Take the
    // worst of 5 trials so allocator warm-up doesn't flatter it.
    let mut worst_op_ms = 0.0f64;
    let mut worst_frame_ms = 0.0f64;
    for _ in 0..5 {
        let top = view.with_vab_session(|v| v.session().iter().last().unwrap().0);
        let t = Instant::now();
        view.with_vab_session(|v| {
            v.apply(VabAction::SetStage {
                node: top,
                stage: 0,
            })
            .unwrap();
            // Scale ping-pong: a mutating op each trial.
            let n = v.session().iter().nth(10).unwrap().0;
            v.apply(VabAction::SetScale {
                node: n,
                scale: 1.0,
            })
            .ok();
        });
        let op_ms = t.elapsed().as_secs_f64() * 1000.0;
        let t = Instant::now();
        view.begin_frame();
        view.end_frame();
        let frame_ms = t.elapsed().as_secs_f64() * 1000.0;
        worst_op_ms = worst_op_ms.max(op_ms);
        worst_frame_ms = worst_frame_ms.max(frame_ms);
    }

    // Frame-worth of explicit sub-ops for the report: to_part_graph +
    // resolve + validate + analyze + body_transforms.
    let t = Instant::now();
    let (g, b) = view
        .with_vab_session(|v| v.catalog().map(|c| v.session().to_part_graph(c)))
        .unwrap()
        .unwrap();
    let graph_ms = t.elapsed().as_secs_f64() * 1000.0;

    let parts = load_parts(&root.join("parts")).unwrap();
    let res = load_resources(&root.join("resources.ron")).unwrap();
    let t = Instant::now();
    let _eff = csp_phys::resolve::resolve_parts(
        &parts,
        &b,
        &res,
        &csp_phys::FamilyQualifications::new(),
        None,
    )
    .unwrap();
    let resolve_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let _v = view.with_vab_session(|v| v.session().validate(&parts));
    let validate_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let _a = view
        .with_vab_session(|v| csp_phys::analyze(v.session(), &parts, &res))
        .unwrap();
    let analyze_ms = t.elapsed().as_secs_f64() * 1000.0;
    let _ = g;

    let total = worst_op_ms + worst_frame_ms;
    eprintln!(
        "VAB latency @300 parts (debug={}):\n  build(300 ops) {:.2} ms\n  \
         clean frame {:.2} ms\n  op worst {:.2} ms\n  frame worst {:.2} ms \
         (→ total {:.2})\n  to_part_graph {:.2}  resolve {:.2}  validate {:.2}  analyze {:.2}",
        cfg!(debug_assertions),
        build_ms,
        clean_frame_ms,
        worst_op_ms,
        worst_frame_ms,
        total,
        graph_ms,
        resolve_ms,
        validate_ms,
        analyze_ms,
    );

    // §5.4's number is release-mode REF-A; debug gets a documented
    // regression band instead of a flaky hard gate.
    let budget_ms = if cfg!(debug_assertions) { 200.0 } else { 16.0 };
    assert!(
        total <= budget_ms,
        "VAB interaction latency {total:.2} ms exceeds {budget_ms} ms budget \
         (§5.4 kill criterion is 16 ms release at 300 parts)"
    );
}
