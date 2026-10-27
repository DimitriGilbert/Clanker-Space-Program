//! [`Dataset`] + [`validate_dataset`] — the whole-tree sweep that
//! runs every content kind's validator plus the cross-file checks no
//! single file's signature can express.

use crate::load::LoadedParts;

use crate::loc::LocFile;
use crate::resource::ResourceFile;
use crate::site::SiteFile;

use super::ValidationError;
use super::{
    validate_balance, validate_colonies, validate_colony_loc_keys, validate_contract_loc_keys,
    validate_contracts, validate_experiment_loc_keys, validate_experiments, validate_id_stability,
    validate_loc, validate_loc_keys, validate_part_loc_keys, validate_parts,
    validate_recipe_loc_keys, validate_recipes, validate_resources, validate_site_loc_keys,
    validate_sites, validate_system, validate_techtree, validate_techtree_loc_keys,
};

/// The whole `data/` tree as the loaders hand it back — one field per
/// file kind, the [`validate_dataset`] input. Borrowed, never owned:
/// the caller loads once and hands the sweep views.
#[derive(Clone, Copy, Debug)]
pub struct Dataset<'a> {
    /// `data/systems/home/` (manifest + bodies + biomes).
    pub system: &'a crate::load::LoadedSystem,
    /// `data/balance.ron`.
    pub balance: &'a crate::balance::BalanceDef,
    /// `data/resources.ron`.
    pub resources: &'a ResourceFile,
    /// `data/systems/home/sites.ron`.
    pub sites: &'a SiteFile,
    /// `data/systems/home/colonies.ron`.
    pub colonies: &'a crate::colony::ColonyFile,
    /// `data/recipes/`.
    pub recipes: &'a crate::load::LoadedRecipes,
    /// `data/experiments/`.
    pub experiments: &'a crate::load::LoadedExperiments,
    /// `data/techtree/`.
    pub techtree: &'a crate::load::LoadedTechTree,
    /// `data/contracts/`.
    pub contracts: &'a crate::load::LoadedContracts,
    /// `data/parts/`.
    pub parts: &'a LoadedParts,
    /// `data/loc/en.ron`.
    pub loc: &'a LocFile,
    /// `data/id-baseline.ron` when the tree authors one — `None`
    /// disables the §7.7 tripwire for that data set (the demo's
    /// reduced tree ships no baseline).
    pub id_baseline: Option<&'a crate::ids::IdBaseline>,
}

/// The §8.2 semantic gate over a fully loaded `data/` tree — the same
/// sweep the data tests run, callable from production startup so a
/// bad tree is refused loudly instead of shipping silently. Returns
/// every failure across every file kind, never just the first.
pub fn validate_dataset(d: &Dataset<'_>) -> Vec<ValidationError> {
    let mut out = Vec::new();
    out.extend(validate_system(d.system));
    out.extend(validate_resources(d.resources));
    out.extend(validate_parts(
        d.parts,
        d.resources,
        d.recipes,
        d.experiments,
    ));
    out.extend(validate_sites(d.sites, d.system, d.resources));
    out.extend(validate_experiments(d.experiments, d.system));
    out.extend(validate_techtree(d.techtree, d.parts));
    out.extend(validate_contracts(
        d.contracts,
        d.system,
        d.experiments,
        d.techtree,
        d.resources,
        d.sites,
    ));
    out.extend(validate_recipes(d.recipes, d.resources));
    out.extend(validate_balance(d.balance, d.resources));
    out.extend(validate_colonies(
        d.colonies,
        d.sites,
        d.resources,
        d.recipes,
    ));
    if let Some(baseline) = d.id_baseline {
        out.extend(validate_id_stability(d.parts, baseline));
    }
    out.extend(validate_loc(d.loc));
    out.extend(validate_loc_keys(d.system, d.loc));
    out.extend(validate_site_loc_keys(d.sites, d.loc));
    out.extend(validate_part_loc_keys(d.parts, d.resources, d.loc));
    out.extend(validate_experiment_loc_keys(d.experiments, d.loc));
    out.extend(validate_techtree_loc_keys(d.techtree, d.loc));
    out.extend(validate_contract_loc_keys(d.contracts, d.loc));
    out.extend(validate_recipe_loc_keys(d.recipes, d.loc));
    out.extend(validate_colony_loc_keys(d.colonies, d.loc));
    check_surface_resources(d.system, d.resources, &mut out);
    out
}

/// `surface_resources` `ResourceId` keys must exist in `resources.ron`.
/// Lives here rather than in [`crate::validate::validate_system`] for the same reason
/// the loc gates live outside it: the resource file is not part of the
/// system directory, so `validate_system`'s signature cannot express
/// the check (the per-map `png` name refs are checked there — the
/// keys were the skipped half).
pub(crate) fn check_surface_resources(
    sys: &crate::load::LoadedSystem,
    res: &ResourceFile,
    out: &mut Vec<ValidationError>,
) {
    for b in &sys.bodies {
        for (r, _) in &b.def.surface_resources {
            if !res.contains(r) {
                out.push(ValidationError::UnknownSurfaceResource {
                    body: b.def.id.as_str().to_string(),
                    resource: r.as_str().to_string(),
                });
            }
        }
    }
}
