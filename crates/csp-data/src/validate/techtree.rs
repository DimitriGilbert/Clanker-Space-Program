//! The §3.3 tech-tree rules over the merged `data/techtree/` set:
//! schema versions, unique well-formed node/track IDs, cost monotonic
//! `requires` edges, no cycles, grant/track shape, family reachability
//! against [`LoadedParts`], and the tree's [`LocKey`] sweep.

use std::collections::{BTreeMap, BTreeSet};

use crate::ids::LocKey;

use crate::load::LoadedParts;
use crate::loc::LocFile;
use crate::techtree::{TechNode, UpgradeTrack};

use super::ValidationError;
use super::common::check_id;

// ---------- tech tree (§3.3) ----------

/// §3.3's tree rules over the merged `data/techtree/` set, checked
/// against the loaded parts (the "every part reachable" half needs the
/// `tech_requirement` edges only `LoadedParts` carries):
///
/// - `schema_version` current on every fragment;
/// - node/track ids unique and well-formed;
/// - `science_cost` finite and ≥ 0; `requires` resolvable, deduplicated,
///   and cost-monotonic per edge (`node.cost ≥ prereq.cost` — §3.3's
///   "cost monotonic in depth", read non-decreasing so equal-cost tiers
///   stay legal);
/// - **no cycles** — a `requires` walk never revisits a node;
/// - every grant names a real track with a cap inside
///   `1..=track.levels.len()`, no track granted twice on one node;
/// - track shape: non-empty levels, `science_cost` finite ≥ 0,
///   `Multiplier` finite > 0, `Delta` finite, `len ≤ u8::MAX`
///   (grant/state counters are `u8`), `(family, stat_path)` unique;
/// - every track's family has ≥ 1 part and its `stat_path` resolves on
///   **every** part of the family — a frozen upgrade the resolver
///   cannot reach is a `ResolveError`, so authoring must guarantee
///   reachability ahead of manufacture — and a `Delta`-carrying track
///   needs a positive finite base everywhere it resolves (the
///   manufacture compose folds `(base + Σ deltas) / base`; a zero or
///   NaN base `InvalidStat`s at resolve);
/// - **every part reachable** — each `PartDef::tech_requirement` names
///   a node in the tree.
pub fn validate_techtree(
    loaded: &crate::load::LoadedTechTree,
    parts: &LoadedParts,
) -> Vec<ValidationError> {
    let mut out = Vec::new();
    // Pass 1: identity + shape — the id → def maps every later pass
    // checks references against (a fragment may reference nodes
    // authored in a *different* file).
    let (node_ids, track_ids) = tree_identity(loaded, &mut out);
    check_tree_nodes(&node_ids, &track_ids, &mut out);
    check_tree_cycles(&node_ids, &mut out);
    check_tree_tracks(&track_ids, parts, &mut out);
    // §3.3 "every part reachable" — each part's `tech_requirement`
    // must name a node in the merged tree.
    for p in &parts.parts {
        if !node_ids.contains_key(p.def.tech_requirement.as_str()) {
            out.push(ValidationError::UnknownTechRequirement {
                part: p.def.id.as_str().to_string(),
                tech: p.def.tech_requirement.as_str().to_string(),
            });
        }
    }
    out
}

/// Pass 1 — schema versions, id well-formedness, duplicate ids.
/// Returns the merged `id → def` maps the reference passes consume.
fn tree_identity<'a>(
    loaded: &'a crate::load::LoadedTechTree,
    out: &mut Vec<ValidationError>,
) -> (
    BTreeMap<&'a str, &'a TechNode>,
    BTreeMap<&'a str, &'a UpgradeTrack>,
) {
    let mut node_ids = BTreeMap::new();
    let mut track_ids = BTreeMap::new();
    for f in &loaded.trees {
        if f.def.schema_version != crate::techtree::TECHTREE_SCHEMA_VERSION {
            out.push(ValidationError::SchemaVersion {
                file: format!("techtree/{}", f.path.display()),
                found: f.def.schema_version,
                expected: crate::techtree::TECHTREE_SCHEMA_VERSION,
            });
        }
        for n in &f.def.nodes {
            let id = n.id.as_str();
            check_id(id, out);
            if let std::collections::btree_map::Entry::Vacant(v) = node_ids.entry(id) {
                v.insert(n);
            } else {
                out.push(ValidationError::DuplicateTechNodeId { id: id.to_string() });
            }
        }
        for t in &f.def.tracks {
            let id = t.id.as_str();
            check_id(id, out);
            if let std::collections::btree_map::Entry::Vacant(v) = track_ids.entry(id) {
                v.insert(t);
            } else {
                out.push(ValidationError::DuplicateUpgradeTrackId { id: id.to_string() });
            }
        }
    }
    (node_ids, track_ids)
}

/// Pass 2 — node shape: `science_cost` finite ≥ 0, `requires`
/// resolvable/deduplicated/cost-monotonic, grant refs and caps.
fn check_tree_nodes(
    node_ids: &BTreeMap<&str, &TechNode>,
    track_ids: &BTreeMap<&str, &UpgradeTrack>,
    out: &mut Vec<ValidationError>,
) {
    for (&id, n) in node_ids {
        if !(n.science_cost.is_finite() && n.science_cost >= 0.0) {
            out.push(ValidationError::BadTechNode {
                node: id.to_string(),
                reason: "science_cost is not finite and >= 0",
            });
        }
        let mut prereqs = BTreeSet::new();
        for r in &n.requires {
            if !prereqs.insert(r.as_str()) {
                out.push(ValidationError::BadTechNode {
                    node: id.to_string(),
                    reason: "duplicate prerequisite",
                });
            }
            match node_ids.get(r.as_str()) {
                None => out.push(ValidationError::UnknownTechPrerequisite {
                    node: id.to_string(),
                    prerequisite: r.as_str().to_string(),
                }),
                Some(p) => {
                    if n.science_cost.is_finite()
                        && p.science_cost.is_finite()
                        && n.science_cost < p.science_cost
                    {
                        out.push(ValidationError::TechCostNotMonotonic {
                            node: id.to_string(),
                            prerequisite: r.as_str().to_string(),
                        });
                    }
                }
            }
        }
        let mut granted = BTreeSet::new();
        for (track, cap) in &n.unlocks.track_levels {
            if !granted.insert(track.as_str()) {
                out.push(ValidationError::BadTechNode {
                    node: id.to_string(),
                    reason: "track granted twice on one node",
                });
            }
            match track_ids.get(track.as_str()) {
                None => out.push(ValidationError::UnknownUpgradeTrack {
                    node: id.to_string(),
                    track: track.as_str().to_string(),
                }),
                Some(t) => {
                    if *cap == 0 {
                        out.push(ValidationError::BadTechNode {
                            node: id.to_string(),
                            reason: "track_levels grant of 0 unlocks nothing",
                        });
                    } else if usize::from(*cap) > t.levels.len() {
                        out.push(ValidationError::BadTechNode {
                            node: id.to_string(),
                            reason: "track_levels grant exceeds the track's authored levels",
                        });
                    }
                }
            }
        }
        for f_id in &n.unlocks.facilities {
            check_id(f_id.as_str(), out);
        }
        for c_id in &n.unlocks.contract_types {
            check_id(c_id.as_str(), out);
        }
    }
}

/// Pass 3 — cycles: iterative DFS over `requires` edges. `done`
/// marks fully-explored nodes, `visiting` the current DFS path;
/// a `requires` edge landing on a `visiting` node is a cycle.
/// Missing refs were already reported; the walk skips them (a
/// dangling edge cannot close a loop).
fn check_tree_cycles(node_ids: &BTreeMap<&str, &TechNode>, out: &mut Vec<ValidationError>) {
    let mut done: BTreeSet<&str> = BTreeSet::new();
    for &id in node_ids.keys() {
        // `stack` is the current DFS path; `visiting` its membership.
        let mut stack: Vec<&str> = vec![id];
        let mut visiting: BTreeSet<&str> = [id].into_iter().collect();
        while let Some(&cur) = stack.last() {
            if done.contains(cur) {
                stack.pop();
                visiting.remove(cur);
                continue;
            }
            let mut advanced = false;
            for r in &node_ids[cur].requires {
                let rid = r.as_str();
                if visiting.contains(rid) {
                    out.push(ValidationError::TechCycle {
                        node: rid.to_string(),
                    });
                    continue;
                }
                if node_ids.contains_key(rid) && !done.contains(rid) {
                    stack.push(rid);
                    visiting.insert(rid);
                    advanced = true;
                    break;
                }
            }
            if !advanced {
                done.insert(cur);
                stack.pop();
                visiting.remove(cur);
            }
        }
    }
}

/// Pass 4 — track shape + family coverage: `(family, stat_path)`
/// unique, non-empty sane levels, ≥ 1 part in the family, and the
/// `stat_path` resolving on *every* family part.
fn check_tree_tracks(
    track_ids: &BTreeMap<&str, &UpgradeTrack>,
    parts: &LoadedParts,
    out: &mut Vec<ValidationError>,
) {
    let mut family_stat: BTreeSet<(&str, crate::part::StatPath)> = BTreeSet::new();
    for (&id, t) in track_ids {
        if !family_stat.insert((t.family.as_str(), t.stat_path)) {
            out.push(ValidationError::BadUpgradeTrack {
                track: id.to_string(),
                reason: "duplicate (family, stat_path) across tracks",
            });
        }
        check_id(t.family.as_str(), out);
        if t.levels.is_empty() {
            out.push(ValidationError::BadUpgradeTrack {
                track: id.to_string(),
                reason: "no levels authored",
            });
        }
        if t.levels.len() > u8::MAX as usize {
            out.push(ValidationError::BadUpgradeTrack {
                track: id.to_string(),
                reason: "more levels than the u8 level counter can hold",
            });
        }
        let mut has_delta = false;
        for l in &t.levels {
            if !(l.science_cost.is_finite() && l.science_cost >= 0.0) {
                out.push(ValidationError::BadUpgradeTrack {
                    track: id.to_string(),
                    reason: "level science_cost is not finite and >= 0",
                });
            }
            match l.multiplier_or_delta {
                crate::techtree::UpgradeEffect::Multiplier(m) => {
                    if !(m.is_finite() && m > 0.0) {
                        out.push(ValidationError::BadUpgradeTrack {
                            track: id.to_string(),
                            reason: "Multiplier effect is not finite and > 0",
                        });
                    }
                }
                crate::techtree::UpgradeEffect::Delta(d) => {
                    has_delta = true;
                    if !d.is_finite() {
                        out.push(ValidationError::BadUpgradeTrack {
                            track: id.to_string(),
                            reason: "Delta effect is not finite",
                        });
                    }
                }
            }
        }
        let mut in_family = 0usize;
        for p in &parts.parts {
            if p.def.family == t.family {
                in_family += 1;
                match t.stat_path.base_value(&p.def) {
                    None => {
                        out.push(ValidationError::BadUpgradeTrack {
                            track: id.to_string(),
                            reason: "stat_path does not resolve on a part of the family",
                        });
                    }
                    // A `Delta` level composes `(base + Σ deltas) /
                    // base` at manufacture — the same gate
                    // `csp_phys::resolve::compose_upgrade_levels`
                    // runs: the authored base must be positive and
                    // finite or every `resolve_built_part` on the
                    // family `InvalidStat`s. `Some(0.0)` (a
                    // zero-torque `Control`, say) used to pass here.
                    Some(base) if has_delta && !(base.is_finite() && base > 0.0) => {
                        out.push(ValidationError::BadUpgradeTrack {
                            track: id.to_string(),
                            reason: "Delta track's stat_path base is not positive and finite on a family part",
                        });
                    }
                    _ => {}
                }
            }
        }
        if in_family == 0 {
            out.push(ValidationError::BadUpgradeTrack {
                track: id.to_string(),
                reason: "family has no parts",
            });
        }
    }
}

/// Loc resolution for the tech tree (the
/// [`crate::validate::validate_part_loc_keys`] pattern): every node/track display name
/// and node description must resolve in `en.ron`.
pub fn validate_techtree_loc_keys(
    loaded: &crate::load::LoadedTechTree,
    loc: &LocFile,
) -> Vec<ValidationError> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut check = |context: String, key: &LocKey| {
        if seen.insert(key.as_str().to_string()) && !loc.contains(key) {
            out.push(ValidationError::UnresolvedLocKey {
                context,
                key: key.as_str().to_string(),
            });
        }
    };
    for f in &loaded.trees {
        for n in &f.def.nodes {
            check(format!("techtree node {}", n.id), &n.display_name);
            check(format!("techtree node {}", n.id), &n.description);
        }
        for t in &f.def.tracks {
            check(format!("techtree track {}", t.id), &t.display_name);
        }
    }
    out
}
