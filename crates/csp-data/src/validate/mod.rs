//! The §3.0/§8.2 data validator. Runs over a [`crate::load::LoadedSystem`] (already
//! parsed by [`crate::load`]) and returns a **typed error list** — never
//! asserts, never exits: CI and the editor both consume the same list.
//!
//! Rules implemented (§3.0 "Validator rules" + §8.2's gate row):
//!
//! - `schema_version` present and equal to [`crate::system::SCHEMA_VERSION`] on every
//!   file (`system.ron`, each `body.ron`, each `biomes.ron`);
//! - the manifest's body list and the loaded defs agree exactly;
//! - every body's `parent` exists; parent chains are acyclic and
//!   terminate at `system.star`;
//! - a parent's SOI exceeds every child's apoapsis (`a·(1+e)`);
//! - an orbiting body's `period_s` is consistent with the parent's μ and
//!   `a` to 1e-9 relative: `T = 2π√(a³/μ_parent)`;
//! - an atmosphere's `top_m` exceeds its `scale_height_m`, its
//!   temperature table is a usable ≥2-knot piecewise-linear function;
//! - biome legends have unique well-formed indices/IDs that fit inside
//!   the PNG's palette, the map decodes as a real 8-bit palette PNG
//!   within §3.0.1's ≤2048×1024 bound, and every pixel index the map
//!   paints is legended;
//! - field sanity: finite, positive where physics requires — and the
//!   **body**-orbit invariants `a > 0`, `0 ≤ e < 1`. Note the scope:
//!   `KeplerElements` also carries open *vessel* orbits sim-side
//!   (`a < 0` hyperbolic, `a := −p` parabolic, `period_s = +∞` — see the
//!   type's doc); this validator only ever sees authored bodies, which
//!   are bound by construction, so the elliptic range is the right rule
//!   here, not a restriction of the type.
//!
//! §3.1/§3.4 add a second rule set over `data/parts/` and
//! `data/resources.ron` — [`validate_parts`], [`validate_resources`],
//! [`validate_part_loc_keys`]: id↔filename agreement, duplicate IDs,
//! positive masses/sizes, variant consistency (non-empty, unique IDs,
//! positive multipliers, unique stat paths), `ScaleRule` sanity
//! (discrete lists contain the 1.0 baseline; continuous exponents in
//! `[0, 4]`), attach-node well-formedness (unique IDs, unit normals),
//! category-consistent modules (an `Engine` part carries an `Engine`
//! module, …), no `Tank`/`Engine` modules on `Discrete`-scaled parts
//! (a size menu cannot scale their stats), resource references
//! resolved against `resources.ron`, the §3.4 reliability-field
//! ranges, and the M4b thermal Euler-stability bound
//! (`conduction_w_per_k ≤ heat_capacity_j_per_k` — see
//! [`ValidationError::ConductionExceedsCapacity`]).
//!
//! Layout (C32-F7): `error` owns [`ValidationError`], `common`
//! the shared check helpers, `tests` the unit-test suite; every
//! other submodule carries one content kind's rules and its
//! `*_loc_keys` sweep.

mod balance;
mod colonies;
mod common;
mod contracts;
mod dataset;
mod error;
mod experiments;
mod ids;
mod loc;
mod parts;
mod recipes;
mod resources;
mod sites;
mod system;
mod techtree;

#[cfg(test)]
// §2.13's no-panic rule covers shipping fallible paths; tests signal
// failure by panicking anyway, so unwrap/expect are allowed here.
#[allow(clippy::unwrap_used, clippy::expect_used)] // lint-allow (§8.6): test-only unwraps
mod tests;

pub use balance::validate_balance;
pub use colonies::{validate_colonies, validate_colony_loc_keys};
pub use contracts::{validate_contract_loc_keys, validate_contracts};
pub use dataset::{Dataset, validate_dataset};
pub use error::ValidationError;
pub use experiments::{validate_experiment_loc_keys, validate_experiments};
pub use ids::validate_id_stability;
pub use loc::validate_loc;
pub use parts::{validate_part_loc_keys, validate_parts};
pub use recipes::{validate_recipe_loc_keys, validate_recipes};
pub use resources::validate_resources;
pub use sites::{FIXED_SUN_TERMINATOR_MIN_COS, validate_site_loc_keys, validate_sites};
pub use system::{PERIOD_REL_TOL, validate_loc_keys, validate_system};
pub use techtree::{validate_techtree, validate_techtree_loc_keys};
