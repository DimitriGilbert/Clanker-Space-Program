//! The `data/recipes/*.ron` rules: id↔stem, unique IDs,
//! `schema_version`, per-row resource resolution and rates, a
//! positive-rate output, no self-loops or net-positive yields, and an
//! acyclic cross-recipe digraph.

use std::collections::BTreeSet;

use crate::loc::LocFile;

use crate::resource::ResourceFile;

use super::ValidationError;
use super::common::check_id;

/// `data/recipes/*.ron` checks: `schema_version`, `id` ↔ file stem,
/// unique ids, every `inputs`/`outputs` row a known resource with a
/// finite non-negative rate, at least one positive-rate output (a
/// recipe with no product is dead data), no resource on both sides
/// of one recipe (a self-loop — the recipe graph's degenerate
/// cycle), no net-positive yield (Σ outputs > Σ inputs mints units
/// against no real supply), and an ACYCLIC cross-recipe digraph —
/// §8.2's "cycles" gate: a cyclic converter web can hold itself
/// grounded in the colony solver and, at a non-negative net, mint
/// forever (C32-F2; the runtime pin that catches the same class is
/// `unanchored_mint_pins` in csp-sim).
pub fn validate_recipes(
    loaded: &crate::load::LoadedRecipes,
    res: &ResourceFile,
) -> Vec<ValidationError> {
    let mut out = Vec::new();
    let bad = |id: &str, reason: &str, out: &mut Vec<ValidationError>| {
        out.push(ValidationError::InvalidRecipe {
            id: id.to_string(),
            reason: reason.to_string(),
        });
    };
    let mut seen_ids = BTreeSet::new();
    for lr in &loaded.recipes {
        let d = &lr.def;
        check_id(d.id.as_str(), &mut out);
        let stem = lr
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if stem != d.id.as_str() {
            bad(
                d.id.as_str(),
                "recipe id does not match file stem",
                &mut out,
            );
        }
        if !seen_ids.insert(d.id.clone()) {
            bad(d.id.as_str(), "duplicate recipe id", &mut out);
        }
        if d.schema_version != crate::recipe::RECIPE_SCHEMA_VERSION {
            bad(d.id.as_str(), "unsupported schema_version", &mut out);
        }
        for (what, list) in [("inputs", &d.inputs), ("outputs", &d.outputs)] {
            let mut seen = BTreeSet::new();
            for (r, q) in list {
                if !seen.insert(r.as_str()) {
                    bad(
                        d.id.as_str(),
                        &format!("duplicate resource {r:?} in {what}"),
                        &mut out,
                    );
                }
                if !res.contains(r) {
                    bad(
                        d.id.as_str(),
                        &format!("unknown resource {r:?} in {what}"),
                        &mut out,
                    );
                }
                if !(q.is_finite() && *q >= 0.0) {
                    bad(
                        d.id.as_str(),
                        &format!("non-finite or negative {what} rate for {r:?}"),
                        &mut out,
                    );
                }
            }
        }
        if !d.outputs.iter().any(|(_, q)| *q > 0.0) {
            bad(d.id.as_str(), "recipe produces nothing", &mut out);
        }
        // A positive-rate resource on BOTH sides of one recipe is a
        // self-loop — the recipe graph's degenerate cycle (C32-F2):
        // net-positive it mints, net-negative it quietly eats itself.
        for (r, q) in &d.inputs {
            if *q > 0.0 && d.outputs.iter().any(|(or, oq)| or == r && *oq > 0.0) {
                bad(
                    d.id.as_str(),
                    &format!("resource {r:?} is both an input and an output"),
                    &mut out,
                );
            }
        }
        // Net yield: Σ inputs ≥ Σ outputs — every authored recipe
        // refines at a loss; a net-positive recipe mints units
        // against no real supply even before graph cycles (C32-F2).
        let in_total: f64 = d.inputs.iter().map(|(_, q)| *q).sum();
        let out_total: f64 = d.outputs.iter().map(|(_, q)| *q).sum();
        if out_total - in_total > 1e-9 {
            bad(
                d.id.as_str(),
                "net-positive yield: outputs exceed inputs",
                &mut out,
            );
        }
    }
    check_recipe_graph(loaded, &mut out);
    out
}

/// The cross-recipe digraph must be ACYCLIC (PLAN §8.2's "cycles"
/// gate, C32-F2): edge r→s when a positive-rate output of r is a
/// positive-rate input of s. Any directed cycle lets a converter
/// web hold itself grounded in the colony solver and — at a
/// non-negative aggregate net — mint forever (the C09-F1 runtime
/// hole, now pinned sim-side; this is the authored-data gate that
/// keeps it unshippable). Duplicate ids are already flagged by
/// [`validate_recipes`]; `by_id`'s first-wins table keeps this
/// build deterministic regardless. Every cyclic recipe is flagged
/// once, naming the whole component, in ascending id order.
fn check_recipe_graph(loaded: &crate::load::LoadedRecipes, out: &mut Vec<ValidationError>) {
    let by_id = loaded.by_id();
    let ids: Vec<_> = by_id.keys().cloned().collect();
    // `feeds[i]` — the recipes whose outputs i consumes.
    let feeds: Vec<BTreeSet<usize>> = ids
        .iter()
        .map(|id| {
            let def = by_id[id];
            let ins: BTreeSet<_> = def
                .inputs
                .iter()
                .filter(|(_, q)| *q > 0.0)
                .map(|(r, _)| r)
                .collect();
            ids.iter()
                .enumerate()
                .filter(|(_, other)| {
                    **other != *id
                        && by_id[*other]
                            .outputs
                            .iter()
                            .any(|(r, q)| *q > 0.0 && ins.contains(r))
                })
                .map(|(j, _)| j)
                .collect()
        })
        .collect();
    // Mutual reachability partitions the graph into SCCs — the
    // recipe count is small, so the per-source walk stays trivial,
    // and ascending `ids` order (BTreeMap) makes the report
    // deterministic. An SCC of ≥2 IS a directed cycle.
    let reach: Vec<BTreeSet<usize>> = (0..ids.len())
        .map(|i| {
            let mut seen = BTreeSet::new();
            let mut stack: Vec<usize> = feeds[i].iter().copied().collect();
            while let Some(j) = stack.pop() {
                if seen.insert(j) {
                    stack.extend(feeds[j].iter().copied());
                }
            }
            seen
        })
        .collect();
    let mut assigned = vec![false; ids.len()];
    for i in 0..ids.len() {
        if assigned[i] {
            continue;
        }
        let members: Vec<usize> = (0..ids.len())
            .filter(|&j| j == i || (reach[i].contains(&j) && reach[j].contains(&i)))
            .collect();
        for &j in &members {
            assigned[j] = true;
        }
        if members.len() > 1 {
            let cycle = members
                .iter()
                .map(|&j| ids[j].as_str())
                .collect::<Vec<_>>()
                .join(" ↔ ");
            for &j in &members {
                out.push(ValidationError::InvalidRecipe {
                    id: ids[j].as_str().to_string(),
                    reason: format!("recipe graph cycle: {cycle}"),
                });
            }
        }
    }
}

/// Loc resolution for the recipe set (the [`crate::validate::validate_site_loc_keys`]
/// pattern): every recipe's `display_name` must resolve in `en.ron`.
/// Separate from [`validate_recipes`] because the loc file lives
/// outside the recipes directory.
pub fn validate_recipe_loc_keys(
    loaded: &crate::load::LoadedRecipes,
    loc: &LocFile,
) -> Vec<ValidationError> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for lr in &loaded.recipes {
        let key = &lr.def.display_name;
        if seen.insert(key.as_str().to_string()) && !loc.contains(key) {
            out.push(ValidationError::UnresolvedLocKey {
                context: format!("recipe {}", lr.def.id),
                key: key.as_str().to_string(),
            });
        }
    }
    out
}
