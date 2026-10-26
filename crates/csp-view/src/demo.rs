//! The §6.3 web-demo tier: the reduced `data/` set embedded into the
//! wasm binary and the fixed demo scenario (Terrin only, part count
//! capped, no save import/export — the constraint enforcement lives in
//! `csp_ui::vab::VabSession` and `csp-app`'s outbox handling; this
//! module supplies the data set and the seeded craft).
//!
//! The demo's `data/` tree lives in `apps/demo/data/` — a real,
//! diffable directory a human can inspect and the native tests can
//! read from disk, baked into the wasm artifact via `include_bytes!`
//! (the browser has no filesystem to read it from). It is a *reduced*
//! copy of the shipping set, not a pointer into it — §6.3 states the
//! data sets differ, which is exactly why web↔desktop saves are not
//! portable (the §3.1 data-set hash can never match).
//!
//! `FILES` is the manifest: keep it in lockstep with the directory —
//! the `demo_files_match_disk` test diffs them and fails on drift.

use csp_data::EmbeddedSource;
use csp_ui::vab::VabAction;

use crate::View;

/// §6.3's part-count cap: the demo's editor refuses a placement past
/// this count and refuses a loaded craft file that exceeds it.
pub const DEMO_PART_LIMIT: usize = 60;

/// The virtual root the embedded table is keyed under — `View::new_demo`
/// calls `from_source(source(), DEMO_ROOT)`, so every `dir.join(...)`
/// the loaders make lands on a `FILES` key. `"data"` keeps the keys
/// identical in shape to the desktop paths.
pub const DEMO_ROOT: &str = "data";

/// The §6.3 demo's fixed save seed — a shipped constant, not entropy:
/// the demo scenario is defined reproducible (offer draws and
/// reliability rolls are identical every boot), while the native
/// build seeds from wall-clock entropy (`native_save_seed` in
/// `crate::lib`).
pub(crate) const DEMO_SAVE_SEED: u64 = 0xC5A3_5EED_0000_0001;

/// The starter craft — `(catalog id, parent row)` pairs: `None` sets
/// the root, `Some(i)` `AttachAuto`s under the part placed at index
/// `i`. The pod/tank/engine propulsion chain is unchanged
/// (liftoff TWR ≈ 5); the canister + battery are the science leg out
/// of the box (W1.5-b): the canister runs `exp.materials_study`
/// landed and in space, the battery's 1 kU of `electric_charge` pays
/// for a transmit; both are `tech.start` parts so no research is
/// required. They fly INLINE between the pod and the tank — both
/// now carry 1.25-class stack interfaces — so the science leg rides
/// mid-stack where a 0.625 nose mount would tumble the ascent and
/// a radial bolt-on has no welded joint shape. Shared by the §6.3
/// demo's fixed scenario and the shipping build's first-run seed
/// ([`View::seed_starter_craft`](crate::View::seed_starter_craft)).
const STARTER_CRAFT: [(&str, Option<usize>); 5] = [
    ("csp.pod.mk1", None),
    ("csp.science.canister", Some(0)),
    ("csp.battery.z1k", Some(1)),
    ("csp.tank.s160", Some(2)),
    ("csp.engine.sparc", Some(3)),
];

/// The shipping build's display name for the seeded starter craft —
/// what the VAB session (and a first craft-file save) calls it.
pub(crate) const STARTER_CRAFT_NAME: &str = "Stock Hopper";

/// The demo's display name for the starter craft.
pub(crate) const DEMO_CRAFT_NAME: &str = "Demo Hopper";

/// `(virtual path, bytes)` — the whole §6.3 reduced set, baked in.
/// Keys are `DEMO_ROOT`-relative repo paths; the ordering is
/// cosmetic (`EmbeddedSource` re-keys into a `BTreeMap`).
pub(crate) static FILES: &[(&str, &[u8])] = &[
    (
        "data/balance.ron",
        include_bytes!("../../../apps/demo/data/balance.ron"),
    ),
    (
        "data/resources.ron",
        include_bytes!("../../../apps/demo/data/resources.ron"),
    ),
    (
        "data/loc/en.ron",
        include_bytes!("../../../apps/demo/data/loc/en.ron"),
    ),
    (
        "data/systems/home/system.ron",
        include_bytes!("../../../apps/demo/data/systems/home/system.ron"),
    ),
    (
        "data/systems/home/sites.ron",
        include_bytes!("../../../apps/demo/data/systems/home/sites.ron"),
    ),
    (
        "data/systems/home/solen/body.ron",
        include_bytes!("../../../apps/demo/data/systems/home/solen/body.ron"),
    ),
    (
        "data/systems/home/terrin/body.ron",
        include_bytes!("../../../apps/demo/data/systems/home/terrin/body.ron"),
    ),
    (
        "data/systems/home/terrin/biomes.ron",
        include_bytes!("../../../apps/demo/data/systems/home/terrin/biomes.ron"),
    ),
    (
        "data/systems/home/terrin/biomes.png",
        include_bytes!("../../../apps/demo/data/systems/home/terrin/biomes.png"),
    ),
    (
        "data/parts/csp.battery.z1k.ron",
        include_bytes!("../../../apps/demo/data/parts/csp.battery.z1k.ron"),
    ),
    (
        "data/parts/csp.chute.s125.ron",
        include_bytes!("../../../apps/demo/data/parts/csp.chute.s125.ron"),
    ),
    (
        "data/parts/csp.decoupler.s125.ron",
        include_bytes!("../../../apps/demo/data/parts/csp.decoupler.s125.ron"),
    ),
    (
        "data/parts/csp.engine.ember.ron",
        include_bytes!("../../../apps/demo/data/parts/csp.engine.ember.ron"),
    ),
    (
        "data/parts/csp.engine.sparc.ron",
        include_bytes!("../../../apps/demo/data/parts/csp.engine.sparc.ron"),
    ),
    (
        "data/parts/csp.nosecone.s125.ron",
        include_bytes!("../../../apps/demo/data/parts/csp.nosecone.s125.ron"),
    ),
    (
        "data/parts/csp.pod.mk1.ron",
        include_bytes!("../../../apps/demo/data/parts/csp.pod.mk1.ron"),
    ),
    (
        "data/parts/csp.science.canister.ron",
        include_bytes!("../../../apps/demo/data/parts/csp.science.canister.ron"),
    ),
    (
        "data/parts/csp.structure.adapter.ron",
        include_bytes!("../../../apps/demo/data/parts/csp.structure.adapter.ron"),
    ),
    (
        "data/parts/csp.tank.s160.ron",
        include_bytes!("../../../apps/demo/data/parts/csp.tank.s160.ron"),
    ),
    (
        "data/parts/csp.tank.s320.ron",
        include_bytes!("../../../apps/demo/data/parts/csp.tank.s320.ron"),
    ),
    (
        "data/experiments/exp.materials_study.ron",
        include_bytes!("../../../apps/demo/data/experiments/exp.materials_study.ron"),
    ),
    (
        "data/techtree/home.ron",
        include_bytes!("../../../apps/demo/data/techtree/home.ron"),
    ),
    (
        "data/recipes/recipe.electrolysis.ron",
        include_bytes!("../../../apps/demo/data/recipes/recipe.electrolysis.ron"),
    ),
    (
        "data/recipes/recipe.isru_propellant.ron",
        include_bytes!("../../../apps/demo/data/recipes/recipe.isru_propellant.ron"),
    ),
    (
        "data/recipes/recipe.smelt_ore.ron",
        include_bytes!("../../../apps/demo/data/recipes/recipe.smelt_ore.ron"),
    ),
    (
        "data/contracts/contract.part_test.ron",
        include_bytes!("../../../apps/demo/data/contracts/contract.part_test.ron"),
    ),
    (
        "data/contracts/contract.satellite.ron",
        include_bytes!("../../../apps/demo/data/contracts/contract.satellite.ron"),
    ),
    (
        "data/contracts/contract.survey.ron",
        include_bytes!("../../../apps/demo/data/contracts/contract.survey.ron"),
    ),
];

/// The embedded `data/` tree for the demo — an [`EmbeddedSource`] over
/// [`FILES`], consumed by `View::new_demo` (and by the validators in
/// the demo tests).
pub fn source() -> EmbeddedSource {
    EmbeddedSource::new(FILES)
}

/// Seed a starter craft: build it through the VAB session's own action
/// channel (so the editor opens on a real, editable craft — the
/// discovery path), then launch it onto the pad via the same
/// `SpawnPhysicalVessel` command the LAUNCH button emits. The spawn
/// lands in the §7.6 command log like every other mutation.
///
/// Shared by [`View::new_demo`] (the fixed §6.3 scenario) and
/// [`View::seed_starter_craft`](crate::View::seed_starter_craft) (the
/// shipping build's first run). A failure here is a broken data set —
/// the caller surfaces it as a startup error rather than a silent
/// empty pad.
pub(crate) fn seed_pad_craft(view: &mut View, name: &str) -> Result<(), String> {
    let built = view.with_vab_session(|v| {
        v.apply(VabAction::Rename(name.to_string()))
            .map_err(|e| e.to_string())?;
        // The placed-node row each `STARTER_CRAFT` parent index names —
        // `AttachAuto` needs the parent's `PartNodeId`, and
        // `selected()` reports the freshly placed node after every
        // attach.
        let mut placed: Vec<csp_phys::PartNodeId> = Vec::with_capacity(STARTER_CRAFT.len());
        for (part, parent) in STARTER_CRAFT {
            let id = csp_data::PartId::from(part);
            match parent {
                None => v.apply(VabAction::SetRoot(id)).map_err(|e| e.to_string())?,
                Some(i) => v
                    .apply(VabAction::AttachAuto {
                        part: id,
                        parent: placed[i],
                    })
                    .map_err(|e| e.to_string())?,
            }
            placed.push(v.selected().ok_or("starter craft: no node placed")?);
        }
        // The launch path resolves the craft from the session's cached
        // analysis — recompute now (the VAB's own frames would do it,
        // but the seed launches before the editor ever opens).
        v.recompute();
        if v.analysis().is_none() {
            return Err(format!(
                "starter craft failed to resolve: {}",
                v.analysis_error().unwrap_or("no analysis")
            ));
        }
        Ok(())
    });
    built?;
    // A refused launch only queues a VAB Notice — nobody reads the
    // session's notice row at startup, so the `Err` is what surfaces
    // the failure (the caller turns it into a startup error rather
    // than leaving a silent empty pad).
    view.launch_craft()?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// The `apps/demo/data/` tree on disk and the embedded `FILES`
    /// manifest must name the same set — a file added to one and not
    /// the other is silent drift (the wasm artifact would quietly
    /// diverge from what a reader sees in the repo).
    #[test]
    fn demo_files_match_disk() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/demo/data");
        let mut disk = Vec::new();
        let mut pending = vec![root.clone()];
        while let Some(dir) = pending.pop() {
            for e in std::fs::read_dir(&dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    pending.push(p);
                } else {
                    disk.push(p);
                }
            }
        }
        let mut disk: Vec<PathBuf> = disk
            .into_iter()
            .map(|p| PathBuf::from("data").join(p.strip_prefix(&root).unwrap()))
            .collect();
        disk.sort();
        let mut embedded: Vec<PathBuf> = FILES.iter().map(|(p, _)| PathBuf::from(p)).collect();
        embedded.sort();
        assert_eq!(
            embedded, disk,
            "apps/demo/data and the embedded FILES table disagree"
        );
    }
}
