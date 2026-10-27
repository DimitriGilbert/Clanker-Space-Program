//! The §3.6 contract-template rules: id↔stem, unique IDs,
//! `schema_version`, per-kind `generate.space` sanity against the
//! body/site/experiment/technode sets, `ModuleReq` part refs, reward
//! amounts, and the template set's [`LocKey`] sweep.

use std::collections::BTreeSet;

use crate::load::LoadedSystem;

use crate::loc::LocFile;
use crate::resource::ResourceFile;
use crate::site::SiteFile;

use super::ValidationError;
use super::common::check_id;

// ---------- contracts (§3.6) ----------

/// One template's `generate.space` vs the system body + experiment
/// id sets — extracted from [`validate_contracts`] for the line
/// budget. `bad` stamps the template's error variant.
#[allow(clippy::too_many_lines)] // lint-allow (§8.6): one arm per kind reads as a table
#[allow(clippy::too_many_arguments)] // lint-allow (§8.6): the cross-reference sets are the signature
fn validate_contract_space(
    cid: &str,
    kind: &crate::contract::ContractKind,
    space: &crate::contract::ContractParamSpace,
    body_ids: &BTreeSet<&str>,
    site_bodies: &BTreeSet<&str>,
    experiment_ids: &BTreeSet<&str>,
    res: &ResourceFile,
    out: &mut Vec<ValidationError>,
) {
    use crate::contract::{ContractParamSpace, ModuleReq, RegimeSpace};
    let bad = |reason: &'static str| ValidationError::BadContract {
        contract: cid.to_string(),
        reason,
    };
    // §3.6's "candidate lists non-empty" rule: an empty `bodies`
    // space pushes an error here — the first offer draw indexes it
    // unconditionally (`bodies.get(pick(rng, bodies.len()))` sim-side)
    // and the generation gate would degrade `Recoverable` on every
    // template pick. The membership half still reports each body that
    // misses the system table.
    let check_bodies = |bodies: &[crate::ids::BodyId], out: &mut Vec<ValidationError>| {
        if bodies.is_empty() {
            out.push(bad("candidate body list is empty"));
        }
        for b in bodies {
            if !body_ids.contains(b.as_str()) {
                out.push(bad("candidate body is not in the system"));
            }
        }
    };
    let range_ok = |r: (f64, f64), what: &'static str, out: &mut Vec<ValidationError>| -> bool {
        if r.0.is_finite() && r.1.is_finite() && r.0 > 0.0 && r.0 <= r.1 {
            true
        } else {
            out.push(bad(what));
            false
        }
    };
    match (kind, space) {
        (
            crate::contract::ContractKind::SatellitePlacement,
            ContractParamSpace::SatellitePlacement {
                bodies,
                periapsis_alt_m,
                apoapsis_alt_m,
            },
        ) => {
            check_bodies(bodies, out);
            let peri_ok = range_ok(
                *periapsis_alt_m,
                "periapsis_alt_m range is not 0 < lo <= hi",
                out,
            );
            let apo_ok = range_ok(
                *apoapsis_alt_m,
                "apoapsis_alt_m range is not 0 < lo <= hi",
                out,
            );
            // Cross-band sanity: partial overlap is legal (the sim
            // orders a drawn pair — see `ContractParamSpace`'s doc),
            // but when the apoapsis band's *ceiling* sits under the
            // periapsis band's *floor* every possible draw inverts —
            // the published window always comes out of the swap, so
            // the authored bands carry none of their stated meaning.
            // Gated on both bands passing `range_ok` so a malformed
            // band reports exactly its own error.
            if peri_ok && apo_ok && apoapsis_alt_m.1 < periapsis_alt_m.0 {
                out.push(bad(
                    "apoapsis_alt_m.hi below periapsis_alt_m.lo — every draw inverts",
                ));
            }
        }
        (
            crate::contract::ContractKind::SurveyRun,
            ContractParamSpace::SurveyRun {
                experiments: exps,
                bodies,
                min_units,
            },
        ) => {
            if exps.is_empty() {
                out.push(bad("survey candidates: no experiments authored"));
            }
            for e in exps {
                if !experiment_ids.contains(e.as_str()) {
                    out.push(bad("survey candidate experiment is not loaded"));
                }
            }
            check_bodies(bodies, out);
            if !(min_units.is_finite() && *min_units > 0.0) {
                out.push(bad("min_units is not positive and finite"));
            }
        }
        (
            crate::contract::ContractKind::PartTest,
            ContractParamSpace::PartTest { modules, regime },
        ) => {
            if modules.is_empty() {
                out.push(bad("part test: no module requirements authored"));
            }
            for m in modules {
                if let ModuleReq::Experiment(e) = m
                    && !experiment_ids.contains(e.as_str())
                {
                    out.push(bad("module requirement names an unloaded experiment"));
                }
            }
            match regime {
                RegimeSpace::Orbit { bodies, min_alt_m } => {
                    check_bodies(bodies, out);
                    range_ok(
                        *min_alt_m,
                        "part-test orbit min_alt range is not 0 < lo <= hi",
                        out,
                    );
                }
                RegimeSpace::Landed { bodies } => {
                    check_bodies(bodies, out);
                }
            }
        }
        (crate::contract::ContractKind::Rescue, ContractParamSpace::Rescue { bodies }) => {
            check_bodies(bodies, out);
        }
        (
            crate::contract::ContractKind::ColonyResupply,
            ContractParamSpace::ColonyResupply {
                bodies,
                resources,
                amount,
            },
        ) => {
            check_bodies(bodies, out);
            // C32-F1: every resupply candidate body must host ≥1
            // site — colonies only ever exist at authored sites
            // (`FoundColony` rejects unknown site ids and no
            // site-creation verb exists), so a siteless body can
            // never grow the store a delivery lands in. The sim's
            // generation gate filters live-capacity per draw, but
            // that is the runtime net, not a license to author a
            // siteless candidate (duva was exactly this miss).
            for b in bodies {
                if body_ids.contains(b.as_str()) && !site_bodies.contains(b.as_str()) {
                    out.push(bad("resupply candidate body has no authored site"));
                }
            }
            if resources.is_empty() {
                out.push(bad("resupply candidates: no resources authored"));
            }
            for r in resources {
                if !res.contains(r) {
                    out.push(bad("resupply candidate resource is not in resources.ron"));
                }
            }
            range_ok(*amount, "resupply amount range is not 0 < lo <= hi", out);
        }
        _ => out.push(bad("kind and generate.space variants disagree")),
    }
}

/// §3.6's template rules over the `data/contracts/` set, checked
/// against the system bodies, the experiment defs, the resource
/// registry, the site table, and the merged tech tree (the
/// cross-reference set the prereqs/candidates name):
///
/// - `schema_version` current, `id` ↔ file-stem agreement, unique
///   ids, well-formed id;
/// - `kind` ↔ `generate.space` variant agreement;
/// - `weight` finite > 0; rewards finite ≥ 0; `deadline_days`
///   `None` or finite > 0;
/// - candidate lists non-empty, ids resolvable (bodies in the
///   system, experiments in the loaded set, `ColonyResupply`
///   resources in `resources.ron`), draw ranges finite
///   with `0 ≤ lo ≤ hi` (altitude `lo > 0` — a zero-altitude orbit
///   band intersects the surface), and — for `SatellitePlacement` —
///   the bands must not be fully inverted (`apoapsis_alt_m.hi <
///   periapsis_alt_m.lo` would invert every draw; partial overlap
///   stays legal — the sim orders the pair);
/// - `ColonyResupply` candidate bodies each host ≥1 site — colonies
///   exist only at authored sites, so a siteless body can never
///   grow the store a delivery needs (C32-F1);
/// - prerequisites well-formed: no duplicates, `count > 0`, tech
///   ids resolvable in the merged tree, body ids in the system, and
///   `ContractsCompleted { kind }` naming an authored template kind
///   (an unauthored kind is a silently unsatisfiable gate —
///   C32-F6);
/// - the tree's `unlocks.contract_types` ids each name an authored
///   template — the other direction is free (a template may exist
///   ununlocked).
#[allow(clippy::too_many_lines)] // lint-allow (§8.6): the per-field check list reads as a list
pub fn validate_contracts(
    loaded: &crate::load::LoadedContracts,
    sys: &LoadedSystem,
    experiments: &crate::load::LoadedExperiments,
    techtree: &crate::load::LoadedTechTree,
    res: &ResourceFile,
    sites: &SiteFile,
) -> Vec<ValidationError> {
    use crate::contract::ContractPrereq;
    let mut out = Vec::new();
    let body_ids: BTreeSet<&str> = sys.bodies.iter().map(|b| b.def.id.as_str()).collect();
    // The bodies any colony can ever sit on: `FoundColony` resolves
    // authored sites only, so the resupply candidate check reads
    // this set (C32-F1).
    let site_bodies: BTreeSet<&str> = sites.sites.iter().map(|s| s.body.as_str()).collect();
    let experiment_ids: BTreeSet<&str> = experiments
        .experiments
        .iter()
        .map(|e| e.def.id.as_str())
        .collect();
    let tree = techtree.merged();
    let node_ids: BTreeSet<&str> = tree.nodes.iter().map(|n| n.id.as_str()).collect();
    // The kind set `ContractsCompleted` prereqs may name — an
    // unauthored kind is a prereq no run can satisfy (C32-F6).
    let authored_kinds: BTreeSet<crate::contract::ContractKind> =
        loaded.contracts.iter().map(|c| c.def.kind).collect();
    let mut seen_ids = BTreeSet::new();
    let mut template_ids: BTreeSet<&str> = BTreeSet::new();
    for lc in &loaded.contracts {
        let d = &lc.def;
        let cid = d.id.as_str();
        template_ids.insert(cid);
        if d.schema_version != crate::contract::CONTRACT_SCHEMA_VERSION {
            out.push(ValidationError::SchemaVersion {
                file: format!("contracts/{}", lc.path.display()),
                found: d.schema_version,
                expected: crate::contract::CONTRACT_SCHEMA_VERSION,
            });
        }
        if lc
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
            != cid
        {
            out.push(ValidationError::BadContract {
                contract: cid.to_string(),
                reason: "contract id does not match file stem",
            });
        }
        if !seen_ids.insert(d.id.clone()) {
            out.push(ValidationError::BadContract {
                contract: cid.to_string(),
                reason: "duplicate contract id",
            });
        }
        check_id(cid, &mut out);
        let bad = |reason: &'static str| ValidationError::BadContract {
            contract: cid.to_string(),
            reason,
        };
        if !(d.generate.weight.is_finite() && d.generate.weight > 0.0) {
            out.push(bad("generator weight is not finite and > 0"));
        }
        for (name, v) in [
            ("advance_funds", d.rewards.advance_funds),
            ("completion_funds", d.rewards.completion_funds),
            ("failure_penalty_funds", d.rewards.failure_penalty_funds),
        ] {
            if !(v.is_finite() && v >= 0.0) {
                out.push(bad(match name {
                    "advance_funds" => "advance_funds is not finite and >= 0",
                    "completion_funds" => "completion_funds is not finite and >= 0",
                    _ => "failure_penalty_funds is not finite and >= 0",
                }));
            }
        }
        if let Some(days) = d.deadline_days
            && !(days.is_finite() && days > 0.0)
        {
            out.push(bad("deadline_days is not positive and finite"));
        }
        validate_contract_space(
            cid,
            &d.kind,
            &d.generate.space,
            &body_ids,
            &site_bodies,
            &experiment_ids,
            res,
            &mut out,
        );
        let mut prereqs = BTreeSet::new();
        for p in &d.prerequisites {
            if !prereqs.insert(p.clone()) {
                out.push(bad("duplicate prerequisite"));
            }
            match p {
                ContractPrereq::TechResearched(n) => {
                    if !node_ids.contains(n.as_str()) {
                        out.push(bad("prerequisite tech node is not in the tree"));
                    }
                }
                ContractPrereq::ContractsCompleted { kind, count } => {
                    if *count == 0 {
                        out.push(bad("ContractsCompleted count of 0 is a tautology"));
                    }
                    // C32-F6: the kind must name an authored template
                    // kind — an unauthored one is a prereq no run can
                    // ever satisfy, which silently gates the whole
                    // template off the board.
                    if !authored_kinds.contains(kind) {
                        out.push(bad("ContractsCompleted names an unauthored contract kind"));
                    }
                }
                ContractPrereq::BodyVisited(b) => {
                    if !body_ids.contains(b.as_str()) {
                        out.push(bad("BodyVisited names a body not in the system"));
                    }
                }
                ContractPrereq::CrewLostAtMost(_) => {}
            }
        }
    }
    // The tree's `contract_types` unlocks must resolve — an unlock
    // naming a template nobody authored is dead authored surface.
    // C08-F6's drift guard, second half: generation now gates on the
    // unlock AND the template's own `prerequisites` — a template
    // unlocked by node N whose prereqs do not `TechResearched(N)`
    // makes the two authored gates disagree (offered-or-not vs
    // offered-and-eligible), so the validator pins them together.
    for n in &tree.nodes {
        for c in &n.unlocks.contract_types {
            if !template_ids.contains(c.as_str()) {
                out.push(ValidationError::UnknownContractType {
                    node: n.id.as_str().to_string(),
                    contract_type: c.as_str().to_string(),
                });
                continue;
            }
            let template = loaded
                .contracts
                .iter()
                .map(|lc| &lc.def)
                .find(|d| d.id == *c);
            if let Some(d) = template
                && !d
                    .prerequisites
                    .iter()
                    .any(|p| matches!(p, ContractPrereq::TechResearched(id) if id == &n.id))
            {
                out.push(ValidationError::BadContract {
                    contract: c.as_str().to_string(),
                    reason: "unlocked by a tech node its prerequisites do not require \
                         (add TechResearched of the unlocking node)",
                });
            }
        }
    }
    out
}

/// Loc resolution for contract templates (the
/// [`crate::validate::validate_techtree_loc_keys`] pattern): display name +
/// description must resolve in `en.ron`.
pub fn validate_contract_loc_keys(
    loaded: &crate::load::LoadedContracts,
    loc: &LocFile,
) -> Vec<ValidationError> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for lc in &loaded.contracts {
        for key in [&lc.def.display_name, &lc.def.description] {
            if seen.insert(key.as_str().to_string()) && !loc.contains(key) {
                out.push(ValidationError::UnresolvedLocKey {
                    context: format!("contract {}", lc.def.id),
                    key: key.as_str().to_string(),
                });
            }
        }
    }
    out
}
