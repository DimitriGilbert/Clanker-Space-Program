//! The colony-file rules: site refs resolve in `sites.ron`, module
//! fields are finite/positive and resource lists resolve, store and
//! battery capacities are sane, and each module's power draw is
//! covered by exactly one source.

use std::collections::{BTreeMap, BTreeSet};

use crate::loc::LocFile;

use crate::resource::ResourceFile;
use crate::site::{SiteDef, SiteFile};

use super::ValidationError;
use super::common::check_id;

fn bad_colony(site: &str, reason: String, out: &mut Vec<ValidationError>) {
    out.push(ValidationError::InvalidColony {
        site: site.to_string(),
        reason,
    });
}

/// `(resource, rate)` list checks for colony modules — unique, known,
/// finite, non-negative.
fn check_colony_pairs(
    site: &str,
    what: &str,
    list: &[(crate::ids::ResourceId, f64)],
    res: &ResourceFile,
    out: &mut Vec<ValidationError>,
) {
    let mut seen = BTreeSet::new();
    for (id, v) in list {
        if !seen.insert(id.as_str()) {
            bad_colony(site, format!("duplicate resource {id:?} in {what}"), out);
        }
        if !res.contains(id) {
            bad_colony(site, format!("unknown resource {id:?} in {what}"), out);
        }
        if !v.is_finite() {
            bad_colony(site, format!("non-finite {what} rate for {id:?}"), out);
        } else if *v < 0.0 {
            bad_colony(site, format!("negative {what} rate for {id:?}"), out);
        }
    }
}

/// The cross-field check `check_colony_pairs` cannot express: an
/// `electric_charge` entry in a `consumes`/`consumes_per_crew` list
/// plus a nonzero `power_ec_per_s` is ONE EC draw authored twice —
/// the sim's `(module, resource)` request rows would collide and the
/// grant would silently drop half the demand.
fn check_power_collision(
    site: &str,
    module: usize,
    what: &str,
    list: &[(crate::ids::ResourceId, f64)],
    power_ec_per_s: f64,
    out: &mut Vec<ValidationError>,
) {
    if power_ec_per_s > 0.0
        && list
            .iter()
            .any(|(id, q)| *q > 0.0 && id.as_str() == crate::resource::ELECTRIC_CHARGE)
    {
        bad_colony(
            site,
            format!("module {module}: electric_charge in {what} collides with power_ec_per_s"),
            out,
        );
    }
}

/// §3.5.1's `colonies.ron` checks: `schema_version`, every colony
/// founded on a declared `sites.ron` site, every resource ref known
/// to `resources.ron` and unique within its list, every
/// rate/capacity/level field finite and non-negative (a zero rate is
/// legal — an inert module; negative is not a flow), and the
/// site↔colony cross-fields (C32-F4): a `Drill` module's resource
/// must carry a positive `resource_yields` entry at the founding site
/// (otherwise the drill reads concentration 0 forever — authored dead
/// weight), and each founding `stores` row must have aggregate
/// `Store`/`Battery` capacity it fits inside (a store-less resource
/// clamps its founding level to 0; an over-capacity level clamps
/// down — both silent at apply, so they are caught here).
#[allow(clippy::too_many_lines)] // lint-allow (§8.6): the per-module-field check list reads as a list
#[allow(clippy::cognitive_complexity)] // lint-allow (§8.6): ditto: it is one flat per-module switch
pub fn validate_colonies(
    file: &crate::colony::ColonyFile,
    sites: &SiteFile,
    res: &ResourceFile,
    recipes: &crate::load::LoadedRecipes,
) -> Vec<ValidationError> {
    let mut out = Vec::new();
    if file.schema_version != crate::colony::COLONY_SCHEMA_VERSION {
        out.push(ValidationError::SchemaVersion {
            file: "colonies.ron".to_string(),
            found: file.schema_version,
            expected: crate::colony::COLONY_SCHEMA_VERSION,
        });
    }
    let site_ids: BTreeSet<&str> = sites.sites.iter().map(|s| s.id.as_str()).collect();
    // First site def wins on a duplicate id — the `by_id` convention
    // (the duplicate itself is already flagged by `validate_sites`);
    // the drill-yield and any future site cross-check reads the def
    // the sim would resolve.
    let site_defs: BTreeMap<&str, &SiteDef> =
        sites
            .sites
            .iter()
            .map(|s| (s.id.as_str(), s))
            .fold(BTreeMap::new(), |mut m, (k, v)| {
                m.entry(k).or_insert(v);
                m
            });
    let recipe_ids: BTreeSet<&str> = recipes.recipes.iter().map(|r| r.def.id.as_str()).collect();
    // First def wins on a duplicate id — the same convention
    // `LoadedRecipes::by_id` gives the sim (`BTreeMap::collect` would
    // keep the LAST def; `validate_recipes` flags the duplicate
    // loudly either way, but the collision check below must inspect
    // the def the sim would actually install).
    let mut recipe_defs: BTreeMap<&str, &crate::recipe::RecipeDef> = BTreeMap::new();
    for r in &recipes.recipes {
        recipe_defs.entry(r.def.id.as_str()).or_insert(&r.def);
    }
    for def in &file.colonies {
        let site = def.site.as_str();
        if !site_ids.contains(site) {
            bad_colony(site, format!("site {site:?} not in sites.ron"), &mut out);
        }
        for (i, m) in def.modules.iter().enumerate() {
            use crate::colony::ColonyModuleDef as M;
            match m {
                M::Solar {
                    area_m2,
                    efficiency,
                } => {
                    if !(area_m2.is_finite() && *area_m2 >= 0.0) {
                        bad_colony(site, format!("module {i}: bad solar area"), &mut out);
                    }
                    if !(efficiency.is_finite() && (0.0..=1.0).contains(efficiency)) {
                        bad_colony(
                            site,
                            format!("module {i}: efficiency outside [0,1]"),
                            &mut out,
                        );
                    }
                }
                M::Generator {
                    fuel,
                    fuel_rate_per_s,
                    rate_per_s,
                    ..
                } => {
                    if !res.contains(fuel) {
                        bad_colony(
                            site,
                            format!("module {i}: unknown generator fuel"),
                            &mut out,
                        );
                    }
                    for (f, v) in [("fuel rate", fuel_rate_per_s), ("rate", rate_per_s)] {
                        if !(v.is_finite() && *v >= 0.0) {
                            bad_colony(site, format!("module {i}: bad generator {f}"), &mut out);
                        }
                    }
                }
                M::Drill {
                    resource,
                    base_rate_per_s,
                    min_concentration,
                    power_ec_per_s,
                } => {
                    if !res.contains(resource) {
                        bad_colony(
                            site,
                            format!("module {i}: unknown drill resource"),
                            &mut out,
                        );
                    }
                    for (f, v) in [("rate", base_rate_per_s), ("power", power_ec_per_s)] {
                        if !(v.is_finite() && *v >= 0.0) {
                            bad_colony(site, format!("module {i}: bad drill {f}"), &mut out);
                        }
                    }
                    // The part-side `ModuleDef::Drill` bound
                    // (`check_drill_module`) — the field's doc claims
                    // parity and the sim's `conc / min_concentration`
                    // scaling makes a `> 1` author a permanent output
                    // cap, which is a trap, not an intent.
                    if !(min_concentration.is_finite() && (0.0..=1.0).contains(min_concentration)) {
                        bad_colony(
                            site,
                            format!("module {i}: drill min_concentration outside [0,1]"),
                            &mut out,
                        );
                    }
                    // C32-F4's barren-ground check: the drill's
                    // concentration comes from the FOUNDING site's
                    // `resource_yields` — absent (or zero) it produces
                    // nothing forever while still drawing its EC.
                    // `None` on the site lookup means the site itself
                    // is unresolvable (already flagged above).
                    if let Some(site_def) = site_defs.get(site)
                        && !site_def
                            .resource_yields
                            .iter()
                            .any(|(r, y)| r == resource && *y > 0.0)
                    {
                        bad_colony(
                            site,
                            format!(
                                "module {i}: drill resource {resource:?} has no positive \
                                 yield at founding site {site:?} — it produces nothing"
                            ),
                            &mut out,
                        );
                    }
                }
                M::Converter {
                    recipe,
                    power_ec_per_s,
                    ..
                } => {
                    check_id(recipe.as_str(), &mut out);
                    if !recipe_ids.contains(recipe.as_str()) {
                        bad_colony(
                            site,
                            format!("module {i}: unknown recipe {recipe:?}"),
                            &mut out,
                        );
                    }
                    if !(power_ec_per_s.is_finite() && *power_ec_per_s >= 0.0) {
                        bad_colony(site, format!("module {i}: bad converter power"), &mut out);
                    }
                    if let Some(rdef) = recipe_defs.get(recipe.as_str()) {
                        check_power_collision(
                            site,
                            i,
                            "recipe inputs",
                            &rdef.inputs,
                            *power_ec_per_s,
                            &mut out,
                        );
                    }
                }
                M::Store { resource, capacity } => {
                    if !res.contains(resource) {
                        bad_colony(
                            site,
                            format!("module {i}: unknown store resource"),
                            &mut out,
                        );
                    }
                    if !(capacity.is_finite() && *capacity >= 0.0) {
                        bad_colony(site, format!("module {i}: bad store capacity"), &mut out);
                    }
                }
                M::Battery { capacity } => {
                    if !(capacity.is_finite() && *capacity >= 0.0) {
                        bad_colony(site, format!("module {i}: bad battery capacity"), &mut out);
                    }
                }
                M::Radiator {
                    area_m2,
                    emissivity,
                } => {
                    if !(area_m2.is_finite() && *area_m2 >= 0.0) {
                        bad_colony(site, format!("module {i}: bad radiator area"), &mut out);
                    }
                    if !(emissivity.is_finite() && (0.0..=1.0).contains(emissivity)) {
                        bad_colony(
                            site,
                            format!("module {i}: emissivity outside [0,1]"),
                            &mut out,
                        );
                    }
                }
                M::LifeSupport {
                    consumes_per_crew,
                    power_ec_per_s,
                } => {
                    check_colony_pairs(
                        site,
                        "life-support consumes_per_crew",
                        consumes_per_crew,
                        res,
                        &mut out,
                    );
                    if !(power_ec_per_s.is_finite() && *power_ec_per_s >= 0.0) {
                        bad_colony(
                            site,
                            format!("module {i}: bad life-support power"),
                            &mut out,
                        );
                    }
                    check_power_collision(
                        site,
                        i,
                        "consumes_per_crew",
                        consumes_per_crew,
                        *power_ec_per_s,
                        &mut out,
                    );
                }
            }
        }
        // Aggregate capacity per resource, mirroring
        // `apply_found_colony`'s Store/Battery fold (the `or_insert`
        // sum — the sim depends on this crate, never the reverse, so
        // the fold is re-derived rather than shared). Battery rows
        // hold `electric_charge` only.
        let mut capacity: BTreeMap<&str, f64> = BTreeMap::new();
        for m in &def.modules {
            match m {
                crate::colony::ColonyModuleDef::Store {
                    resource,
                    capacity: c,
                } if c.is_finite() && *c >= 0.0 => {
                    *capacity.entry(resource.as_str()).or_insert(0.0) += *c;
                }
                crate::colony::ColonyModuleDef::Battery { capacity: c }
                    if c.is_finite() && *c >= 0.0 =>
                {
                    *capacity
                        .entry(crate::resource::ELECTRIC_CHARGE)
                        .or_insert(0.0) += *c;
                }
                _ => {}
            }
        }
        let mut seen = BTreeSet::new();
        for s in &def.stores {
            if !seen.insert(s.resource.as_str()) {
                bad_colony(
                    site,
                    format!("duplicate store resource {:?}", s.resource),
                    &mut out,
                );
            }
            if !res.contains(&s.resource) {
                bad_colony(
                    site,
                    format!("unknown store resource {:?}", s.resource),
                    &mut out,
                );
            }
            if !(s.level.is_finite() && s.level >= 0.0) {
                bad_colony(
                    site,
                    format!("bad store level for {:?}", s.resource),
                    &mut out,
                );
            }
            // C32-F4's store↔capacity cross-check: founding levels
            // clamp to the aggregate capacities at apply — a store
            // row naming a resource NO module holds anchors at 0 (the
            // founding stock silently vanishes), and a level over
            // capacity clamps down. Both are authored mistakes, not
            // intent, so both fail validation.
            if s.level.is_finite() && s.level >= 0.0 {
                match capacity.get(s.resource.as_str()) {
                    None => bad_colony(
                        site,
                        format!(
                            "founding store {:?} has no Store/Battery capacity — \
                             clamps to 0 at founding",
                            s.resource
                        ),
                        &mut out,
                    ),
                    Some(&cap) if s.level > cap => bad_colony(
                        site,
                        format!(
                            "founding store {:?} level {} exceeds aggregate capacity {} — \
                             clamps at founding",
                            s.resource, s.level, cap
                        ),
                        &mut out,
                    ),
                    _ => {}
                }
            }
        }
    }
    out
}

/// Loc resolution for the colony set (the [`crate::validate::validate_site_loc_keys`]
/// pattern): every colony's `display_name` must resolve in `en.ron`.
/// Separate from [`validate_colonies`] because the loc file lives
/// outside the system directory.
pub fn validate_colony_loc_keys(
    file: &crate::colony::ColonyFile,
    loc: &LocFile,
) -> Vec<ValidationError> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for c in &file.colonies {
        let key = &c.display_name;
        if seen.insert(key.as_str().to_string()) && !loc.contains(key) {
            out.push(ValidationError::UnresolvedLocKey {
                context: format!("colony at site {}", c.site),
                key: key.as_str().to_string(),
            });
        }
    }
    out
}
