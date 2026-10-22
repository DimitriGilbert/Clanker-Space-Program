//! The `data/recipes/<id>.ron` schema — the §3.5.1 converter recipe
//! registry (M8-B).
//!
//! A `Converter` *module* on a colony (§3.1 names `Converter {
//! recipe: RecipeId, priority, power_w }` for parts; the colony's
//! `ColonyModuleDef::Converter` carries the same `RecipeId`
//! indirection) says WHICH transformation it runs and how hard it
//! pulls; the [`RecipeDef`] says what the transformation IS — the
//! `(resource, units/s)` input/output rate pairs the colony flow
//! network consumes at the module's allocated operating fraction.
//!
//! Recipes are deliberately rate-shaped, not batch-shaped: the
//! §3.5.1 network is a piecewise-linear flow model where every
//! quantity is `rate · dt` over an event-bounded interval, so a
//! recipe is just the stoichiometric row the allocation pass grants
//! or starves. `electric_charge` is deliberately not special here —
//! a recipe may list it as an input (an electrolytic cell running on
//! charge) or an output (a fuel cell) like any other resource; the
//! *module's* `power_ec_per_s` field is the separate standing draw
//! the validator/sim keep from colliding with an EC input (one EC
//! demand must not be authored twice).

use serde::{Deserialize, Serialize};

use crate::ids::{LocKey, RecipeId, ResourceId};

/// The `schema_version` this loader understands for
/// `data/recipes/*.ron` — its own namespace, like `balance.ron`
/// (§3.1 first-field convention).
pub const RECIPE_SCHEMA_VERSION: u32 = 1;

/// `data/recipes/<id>.ron` — one converter recipe (§3.5.1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecipeDef {
    /// First field of every data file (§3.1). Must equal
    /// [`RECIPE_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Stable recipe ID (`recipe.smelt_ore`) — also the file's stem,
    /// so the validator can check `id` ↔ filename agreement.
    /// `ColonyModuleDef::Converter::recipe` (and the part-side
    /// `ModuleDef::Converter`) references this.
    pub id: RecipeId,
    /// Loc key for the display name.
    pub display_name: LocKey,
    /// `(resource, units/s)` the converter requests at full
    /// operation — the allocation pass's consumer rows. Resource IDs
    /// unique within the list (validator rule); rates finite and
    /// non-negative. Empty is legal: a converter with no inputs runs
    /// ungrounded-free (its outputs are pure production, which the
    /// grounding closure treats as a source).
    pub inputs: Vec<(ResourceId, f64)>,
    /// `(resource, units/s)` produced at full operation — scaled by
    /// the module's allocated fraction like the inputs. Must contain
    /// at least one positive-rate entry (validator rule: a recipe
    /// with no product is dead data).
    pub outputs: Vec<(ResourceId, f64)>,
}
