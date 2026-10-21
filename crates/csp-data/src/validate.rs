//! The §3.0/§8.2 data validator. Runs over a [`LoadedSystem`] (already
//! parsed by [`crate::load`]) and returns a **typed error list** — never
//! asserts, never exits: CI and the editor both consume the same list.
//!
//! Rules implemented (§3.0 "Validator rules" + §8.2's gate row):
//!
//! - `schema_version` present and equal to [`SCHEMA_VERSION`] on every
//!   file (`system.ron`, each `body.ron`, each `biomes.ron`);
//! - the manifest's body list and the loaded defs agree exactly;
//! - every body's `parent` exists; parent chains are acyclic and
//!   terminate at `system.star`;
//! - a parent's SOI exceeds every child's apoapsis (`a·(1+e)`);
//! - an orbiting body's `period_s` is consistent with the parent's μ and
//!   `a` to 1e-9 relative: `T = 2π√(a³/μ_parent)`;
//! - an atmosphere's `top_m` exceeds its `scale_height_m`, its
//!   temperature table is a usable ≥2-knot piecewise-linear function;
//! - biome legends have unique indices/IDs that fit inside the PNG's
//!   palette, and the map is a real 8-bit palette PNG within
//!   §3.0.1's ≤2048×1024 bound;
//! - field sanity: finite, positive where physics requires — and the
//!   **body**-orbit invariants `a > 0`, `0 ≤ e < 1`. Note the scope:
//!   `KeplerElements` also carries open *vessel* orbits sim-side
//!   (`a < 0` hyperbolic, `a := −p` parabolic, `period_s = +∞` — see the
//!   type's doc); this validator only ever sees authored bodies, which
//!   are bound by construction, so the elliptic range is the right rule
//!   here, not a restriction of the type.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::biome::{BIOME_MAP_MAX_H, BIOME_MAP_MAX_W, inspect_indexed_png};
use crate::ids::LocKey;
use crate::load::LoadedSystem;
use crate::loc::LocFile;
use crate::system::SCHEMA_VERSION;

/// Relative tolerance for the §3.0 period-consistency rule:
/// `|T_authored − 2π√(a³/μ_parent)| / T_computed ≤ PERIOD_REL_TOL`.
pub const PERIOD_REL_TOL: f64 = 1e-9;

/// One validation failure. `body`/`parent` fields carry the string form
/// of IDs — diagnostics are never hashed, so plain `String`s are fine
/// (same convention as `csp_bench::ReplayError`).
#[derive(Clone, Debug, PartialEq)]
pub enum ValidationError {
    /// A file's `schema_version` differs from [`SCHEMA_VERSION`].
    SchemaVersion {
        /// Which file kind ("system.ron", `body.ron`/`biomes.ron` under a
        /// body dir — the message carries the context).
        file: String,
        /// What the file declared.
        found: u32,
    },
    /// Two loaded `body.ron` files declare the same `id`.
    DuplicateBodyId {
        /// The duplicated ID.
        id: String,
    },
    /// `system.bodies` lists an ID whose def failed to appear (id field
    /// in `body.ron` does not match its manifest entry/directory).
    ListedIdNotDeclared {
        /// The manifest entry with no matching def.
        id: String,
    },
    /// A loaded `body.ron` declares an ID absent from `system.bodies`.
    DeclaredIdNotListed {
        /// The unlisted ID.
        id: String,
    },
    /// `system.star` is not one of the loaded bodies.
    StarNotFound {
        /// The star ID the manifest names.
        id: String,
    },
    /// The star itself declares a parent — roots have none.
    StarHasParent {
        /// The star's ID.
        id: String,
    },
    /// A body other than the star has no parent — a second root.
    RootNotStar {
        /// The parentless non-star body.
        id: String,
    },
    /// `body.parent` names an ID that is not a loaded body.
    ParentNotFound {
        /// The body with the dangling parent ref.
        body: String,
        /// The missing parent ID.
        parent: String,
    },
    /// Following `parent` links from `body` returns to `body` — a cycle.
    ParentCycle {
        /// A body on the cycle.
        body: String,
    },
    /// A body's parent chain terminates at a root that is not the star —
    /// the body is unreachable from the system root (§8.2 "reachability").
    DisconnectedFromStar {
        /// The body whose chain misses the star.
        body: String,
    },
    /// A parented body lacks `orbit` elements.
    OrbitMissing {
        /// The body.
        body: String,
    },
    /// A parentless body carries `orbit` elements — roots do not orbit.
    OrbitOnRoot {
        /// The body.
        body: String,
    },
    /// An ID is empty or contains characters unsafe as a directory name
    /// (`/`, `\`, `.`-only segments). IDs are path components under
    /// `data/systems/`, so they must be tame.
    MalformedId {
        /// The offending ID text.
        id: String,
    },
    /// A numeric field is NaN or infinite.
    NonFinite {
        /// Owning body.
        body: String,
        /// Field name.
        field: &'static str,
    },
    /// A numeric field that must be positive is not.
    NonPositive {
        /// Owning body.
        body: String,
        /// Field name.
        field: &'static str,
        /// The offending value.
        value: f64,
    },
    /// `eccentricity` outside the `[0, 1)` bound the §3.0 rules place on
    /// *authored body* orbits (bodies are bound by construction; the
    /// open-orbit `KeplerElements` contract for vessels lives in
    /// `csp_sim::orbit` and never reaches this validator).
    EccentricityOutOfRange {
        /// The body.
        body: String,
        /// The offending eccentricity.
        e: f64,
    },
    /// §3.0 rule: a child's apoapsis must lie inside its parent's SOI.
    SoiTooSmall {
        /// The parent whose SOI is too small.
        parent: String,
        /// The child poking out of it.
        child: String,
        /// The parent's authored SOI (m).
        soi_m: f64,
        /// The child's apoapsis `a·(1+e)` (m).
        apoapsis_m: f64,
    },
    /// §3.0 rule: `period_s` must match `2π√(a³/μ_parent)` within
    /// [`PERIOD_REL_TOL`] relative.
    PeriodInconsistent {
        /// The body.
        body: String,
        /// The authored `period_s`.
        authored_s: f64,
        /// `2π√(a³/μ_parent)` from the loaded `a` and parent μ.
        computed_s: f64,
    },
    /// §3.0 rule: atmosphere top must exceed scale height.
    AtmosphereTopTooLow {
        /// The body.
        body: String,
        /// Authored `top_m`.
        top_m: f64,
        /// Authored `scale_height_m`.
        scale_height_m: f64,
    },
    /// The atmosphere's temperature table is not a usable
    /// piecewise-linear function.
    BadTemperatureTable {
        /// The body.
        body: String,
        /// Why it failed (stable reason token).
        reason: &'static str,
    },
    /// A biome legend repeats a palette index or a biome ID.
    BiomeLegendDuplicate {
        /// The body.
        body: String,
        /// What was duplicated ("index" or "id") and its value.
        detail: String,
    },
    /// A legend index has no palette entry in the PNG.
    BiomeIndexExceedsPalette {
        /// The body.
        body: String,
        /// The legend index that overflows.
        index: u8,
        /// The PNG's palette size.
        palette_len: usize,
    },
    /// The referenced PNG is not structurally a valid palette PNG.
    BiomeMapPngInvalid {
        /// The body.
        body: String,
        /// [`crate::biome::inspect_indexed_png`]'s reason.
        reason: &'static str,
    },
    /// §3.0.1's size bound: equirect, ≤[`BIOME_MAP_MAX_W`]×[`BIOME_MAP_MAX_H`].
    BiomeMapTooLarge {
        /// The body.
        body: String,
        /// Image width.
        width: u32,
        /// Image height.
        height: u32,
    },
    /// A `LocKey` referenced by the data set does not resolve in the
    /// locale file.
    UnresolvedLocKey {
        /// Where the key was referenced from.
        context: String,
        /// The unresolved key.
        key: String,
    },
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SchemaVersion { file, found } => {
                write!(
                    f,
                    "{file}: schema_version {found}, expected {SCHEMA_VERSION}"
                )
            }
            Self::DuplicateBodyId { id } => write!(f, "duplicate body id {id:?}"),
            Self::ListedIdNotDeclared { id } => {
                write!(f, "manifest lists {id:?} but no body.ron declares it")
            }
            Self::DeclaredIdNotListed { id } => {
                write!(f, "body {id:?} is declared but absent from the manifest")
            }
            Self::StarNotFound { id } => write!(f, "star {id:?} is not a loaded body"),
            Self::StarHasParent { id } => write!(f, "star {id:?} declares a parent"),
            Self::RootNotStar { id } => write!(f, "non-star body {id:?} has no parent"),
            Self::ParentNotFound { body, parent } => {
                write!(f, "{body:?}: parent {parent:?} is not a loaded body")
            }
            Self::ParentCycle { body } => write!(f, "{body:?}: parent chain cycles"),
            Self::DisconnectedFromStar { body } => {
                write!(f, "{body:?}: parent chain does not reach the star")
            }
            Self::OrbitMissing { body } => write!(f, "{body:?}: parented body has no orbit"),
            Self::OrbitOnRoot { body } => write!(f, "{body:?}: root body carries orbit elements"),
            Self::MalformedId { id } => write!(f, "malformed id {id:?}"),
            Self::NonFinite { body, field } => {
                write!(f, "{body:?}.{field}: not finite")
            }
            Self::NonPositive { body, field, value } => {
                write!(f, "{body:?}.{field}: {value} is not positive")
            }
            Self::EccentricityOutOfRange { body, e } => {
                write!(f, "{body:?}: eccentricity {e} outside [0, 1)")
            }
            Self::SoiTooSmall {
                parent,
                child,
                soi_m,
                apoapsis_m,
            } => write!(
                f,
                "{child:?} apoapsis {apoapsis_m} m exceeds parent {parent:?} SOI {soi_m} m"
            ),
            Self::PeriodInconsistent {
                body,
                authored_s,
                computed_s,
            } => write!(
                f,
                "{body:?}: period {authored_s} s vs computed {computed_s} s (rel diff exceeds {PERIOD_REL_TOL:e})"
            ),
            Self::AtmosphereTopTooLow {
                body,
                top_m,
                scale_height_m,
            } => write!(
                f,
                "{body:?}: atmosphere top {top_m} m does not exceed scale height {scale_height_m} m"
            ),
            Self::BadTemperatureTable { body, reason } => {
                write!(f, "{body:?}: temperature table: {reason}")
            }
            Self::BiomeLegendDuplicate { body, detail } => {
                write!(f, "{body:?}: biome legend duplicates {detail}")
            }
            Self::BiomeIndexExceedsPalette {
                body,
                index,
                palette_len,
            } => write!(
                f,
                "{body:?}: biome index {index} >= palette size {palette_len}"
            ),
            Self::BiomeMapPngInvalid { body, reason } => {
                write!(f, "{body:?}: biomes.png invalid: {reason}")
            }
            Self::BiomeMapTooLarge {
                body,
                width,
                height,
            } => write!(
                f,
                "{body:?}: biome map {width}x{height} exceeds {BIOME_MAP_MAX_W}x{BIOME_MAP_MAX_H}"
            ),
            Self::UnresolvedLocKey { context, key } => {
                write!(f, "{context}: loc key {key:?} has no entry in en.ron")
            }
        }
    }
}

impl std::error::Error for ValidationError {}

/// IDs are directory names under `data/systems/<sys>/` — keep them tame:
/// non-empty, `[a-z0-9._-]` only, never a `.`/`..` segment.
fn id_well_formed(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && id.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' || c == '-'
        })
}

fn check_id(id: &str, out: &mut Vec<ValidationError>) {
    if !id_well_formed(id) {
        out.push(ValidationError::MalformedId { id: id.to_string() });
    }
}

fn check_finite(body: &str, field: &'static str, v: f64, out: &mut Vec<ValidationError>) {
    if !v.is_finite() {
        out.push(ValidationError::NonFinite {
            body: body.to_string(),
            field,
        });
    }
}

fn check_positive(body: &str, field: &'static str, v: f64, out: &mut Vec<ValidationError>) {
    if !v.is_finite() {
        out.push(ValidationError::NonFinite {
            body: body.to_string(),
            field,
        });
    } else if v <= 0.0 {
        out.push(ValidationError::NonPositive {
            body: body.to_string(),
            field,
            value: v,
        });
    }
}

/// Body defs keyed by ID — built once by [`build_def_map`] and shared by
/// the graph and orbital rules. `BTreeMap` keeps iteration deterministic.
type DefMap<'a> = BTreeMap<&'a str, &'a crate::load::LoadedBody>;

/// The §3.0 validator over a fully loaded system. Pure: every input is
/// already in memory, so results never depend on the filesystem, the
/// clock, or iteration order of a hash container.
pub fn validate_system(sys: &LoadedSystem) -> Vec<ValidationError> {
    let mut out = Vec::new();
    check_schema_versions(sys, &mut out);
    let defs = build_def_map(sys, &mut out);
    check_body_set(sys, &defs, &mut out);
    check_graph(sys, &defs, &mut out);
    for b in &sys.bodies {
        check_body_fields(b, &mut out);
        check_orbit_rules(b, &defs, &mut out);
        if b.def.atmosphere.is_some() {
            check_atmosphere(b, &mut out);
        }
        if let Some(bm) = &b.biomes {
            check_biomes(b, bm, &mut out);
        }
    }
    out
}

/// §8.2's "schema_version present" row — presence is enforced by serde at
/// parse time; this re-checks the *value* on every loaded file.
fn check_schema_versions(sys: &LoadedSystem, out: &mut Vec<ValidationError>) {
    if sys.system.schema_version != SCHEMA_VERSION {
        out.push(ValidationError::SchemaVersion {
            file: "system.ron".to_string(),
            found: sys.system.schema_version,
        });
    }
    for b in &sys.bodies {
        if b.def.schema_version != SCHEMA_VERSION {
            out.push(ValidationError::SchemaVersion {
                file: format!("{}/body.ron", b.def.id),
                found: b.def.schema_version,
            });
        }
        if let Some(bm) = &b.biomes
            && bm.legend.schema_version != SCHEMA_VERSION
        {
            out.push(ValidationError::SchemaVersion {
                file: format!(
                    "{}/{}",
                    b.def.id,
                    b.def
                        .biome_map
                        .as_ref()
                        .map_or("biomes.ron", |r| r.legend.as_str())
                ),
                found: bm.legend.schema_version,
            });
        }
    }
}

/// Index loaded defs by ID, reporting duplicates.
fn build_def_map<'a>(sys: &'a LoadedSystem, out: &mut Vec<ValidationError>) -> DefMap<'a> {
    let mut defs = DefMap::new();
    for b in &sys.bodies {
        let id = b.def.id.as_str();
        if defs.insert(id, b).is_some() {
            out.push(ValidationError::DuplicateBodyId { id: id.to_string() });
        }
    }
    defs
}

/// Manifest ↔ loaded-set agreement, plus ID hygiene on both sides.
fn check_body_set(sys: &LoadedSystem, defs: &DefMap, out: &mut Vec<ValidationError>) {
    check_id(sys.system.id.as_str(), out);
    check_id(sys.system.star.as_str(), out);
    let mut listed: BTreeSet<&str> = BTreeSet::new();
    for id in &sys.system.bodies {
        listed.insert(id.as_str());
        check_id(id.as_str(), out);
        if !defs.contains_key(id.as_str()) {
            out.push(ValidationError::ListedIdNotDeclared {
                id: id.as_str().to_string(),
            });
        }
    }
    for &id in defs.keys() {
        check_id(id, out);
        if !listed.contains(id) {
            out.push(ValidationError::DeclaredIdNotListed { id: id.to_string() });
        }
    }
}

/// Star shape, parent-exists / orbit-shape checks, acyclicity, and
/// reachability of every body from `system.star`.
fn check_graph(sys: &LoadedSystem, defs: &DefMap, out: &mut Vec<ValidationError>) {
    let star_id = sys.system.star.as_str();
    let star_known = if let Some(star) = defs.get(star_id) {
        if star.def.parent.is_some() {
            out.push(ValidationError::StarHasParent {
                id: star_id.to_string(),
            });
        }
        if star.def.orbit.is_some() {
            out.push(ValidationError::OrbitOnRoot {
                body: star_id.to_string(),
            });
        }
        true
    } else {
        out.push(ValidationError::StarNotFound {
            id: star_id.to_string(),
        });
        false
    };
    for b in &sys.bodies {
        let id = b.def.id.as_str();
        match &b.def.parent {
            None => {
                if id != star_id {
                    out.push(ValidationError::RootNotStar { id: id.to_string() });
                }
            }
            Some(p) => {
                if !defs.contains_key(p.as_str()) {
                    out.push(ValidationError::ParentNotFound {
                        body: id.to_string(),
                        parent: p.as_str().to_string(),
                    });
                }
                if b.def.orbit.is_none() {
                    out.push(ValidationError::OrbitMissing {
                        body: id.to_string(),
                    });
                }
            }
        }
    }
    if star_known {
        check_parent_chains(sys, defs, star_id, out);
    }
}

/// Walk every parent chain: a revisit is a cycle; a resolved chain that
/// terminates anywhere but the star is unreachable (§8.2 "reachability").
fn check_parent_chains(
    sys: &LoadedSystem,
    defs: &DefMap,
    star_id: &str,
    out: &mut Vec<ValidationError>,
) {
    for b in &sys.bodies {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut cur = b.def.id.as_str();
        seen.insert(cur);
        let mut parent = b.def.parent.as_ref();
        while let Some(p) = parent {
            let pid = p.as_str();
            if !seen.insert(pid) {
                out.push(ValidationError::ParentCycle {
                    body: b.def.id.as_str().to_string(),
                });
                break;
            }
            match defs.get(pid) {
                None => break, // ParentNotFound already reported
                Some(pd) => {
                    cur = pid;
                    parent = pd.def.parent.as_ref();
                }
            }
        }
        // A chain ending on `parent.is_none()` landed on the terminal
        // root `cur`; it should be the star. (Parentless non-star bodies
        // already reported as RootNotStar.)
        if b.def.parent.is_some() && parent.is_none() && cur != star_id {
            out.push(ValidationError::DisconnectedFromStar {
                body: b.def.id.as_str().to_string(),
            });
        }
    }
}

/// Field sanity on one body: finite where required, positive where
/// physics demands it.
fn check_body_fields(b: &crate::load::LoadedBody, out: &mut Vec<ValidationError>) {
    let body = b.def.id.as_str();
    check_positive(body, "radius_m", b.def.radius_m, out);
    check_positive(body, "mu_m3_per_s2", b.def.mu_m3_per_s2, out);
    check_finite(body, "tilt_rad", b.def.tilt_rad, out);
    if let Some(soi) = b.def.soi_m {
        check_positive(body, "soi_m", soi, out);
    }
    if let Some(rot) = b.def.rotation_period_s {
        check_positive(body, "rotation_period_s", rot, out);
    }
    if let Some(ins) = b.def.insolation_w_m2 {
        check_positive(body, "insolation_w_m2", ins, out);
    }
    if let Some(oc) = &b.def.ocean {
        check_positive(body, "ocean.datum_radius_m", oc.datum_radius_m, out);
    }
}

/// Orbit-element sanity plus the two §3.0 orbital rules: child apoapsis
/// inside parent SOI, and `period_s` consistent with `2π√(a³/μ_parent)`
/// to [`PERIOD_REL_TOL`] relative.
fn check_orbit_rules(b: &crate::load::LoadedBody, defs: &DefMap, out: &mut Vec<ValidationError>) {
    let body = b.def.id.as_str();
    let Some(o) = &b.def.orbit else { return };
    check_positive(body, "orbit.semi_major_axis_m", o.semi_major_axis_m, out);
    check_finite(body, "orbit.inclination_rad", o.inclination_rad, out);
    check_finite(body, "orbit.ascending_node_rad", o.ascending_node_rad, out);
    check_finite(body, "orbit.arg_periapsis_rad", o.arg_periapsis_rad, out);
    check_finite(
        body,
        "orbit.mean_anomaly_epoch_rad",
        o.mean_anomaly_epoch_rad,
        out,
    );
    check_positive(body, "orbit.period_s", o.period_s, out);
    if !(0.0..1.0).contains(&o.eccentricity) {
        out.push(ValidationError::EccentricityOutOfRange {
            body: body.to_string(),
            e: o.eccentricity,
        });
    }

    // §3.0 rules need a resolvable, loaded parent.
    let Some(p) = b.def.parent.as_ref().and_then(|p| defs.get(p.as_str())) else {
        return;
    };
    let apoapsis = o.semi_major_axis_m * (1.0 + o.eccentricity);
    if let Some(soi) = p.def.soi_m
        && apoapsis >= soi
    {
        out.push(ValidationError::SoiTooSmall {
            parent: p.def.id.as_str().to_string(),
            child: body.to_string(),
            soi_m: soi,
            apoapsis_m: apoapsis,
        });
    }
    // T = 2π√(a³/μ_parent). libm::sqrt: f64::sqrt is banned by this
    // crate's clippy.toml (§8.2 determinism lints).
    let a = o.semi_major_axis_m;
    let computed = 2.0 * core::f64::consts::PI * libm::sqrt(a * a * a / p.def.mu_m3_per_s2);
    let rel = ((o.period_s - computed) / computed).abs();
    if rel > PERIOD_REL_TOL {
        out.push(ValidationError::PeriodInconsistent {
            body: body.to_string(),
            authored_s: o.period_s,
            computed_s: computed,
        });
    }
}

/// §3.0 atmosphere model: positive model constants, γ > 1, the
/// `top_m > scale_height_m` rule, and a usable temperature table.
fn check_atmosphere(b: &crate::load::LoadedBody, out: &mut Vec<ValidationError>) {
    let Some(atm) = &b.def.atmosphere else { return };
    let body = b.def.id.as_str();
    check_positive(
        body,
        "atmosphere.sea_level_pressure_pa",
        atm.sea_level_pressure_pa,
        out,
    );
    check_positive(body, "atmosphere.scale_height_m", atm.scale_height_m, out);
    check_positive(body, "atmosphere.top_m", atm.top_m, out);
    check_positive(
        body,
        "atmosphere.molar_mass_kg_per_mol",
        atm.molar_mass_kg_per_mol,
        out,
    );
    if !(atm.heat_capacity_ratio.is_finite() && atm.heat_capacity_ratio > 1.0) {
        out.push(ValidationError::NonPositive {
            body: body.to_string(),
            field: "atmosphere.heat_capacity_ratio",
            value: atm.heat_capacity_ratio,
        });
    }
    if atm.top_m <= atm.scale_height_m {
        out.push(ValidationError::AtmosphereTopTooLow {
            body: body.to_string(),
            top_m: atm.top_m,
            scale_height_m: atm.scale_height_m,
        });
    }
    check_temperature_table(b, out);
}

fn check_temperature_table(b: &crate::load::LoadedBody, out: &mut Vec<ValidationError>) {
    let Some(atm) = &b.def.atmosphere else { return };
    let body = b.def.id.as_str();
    let mut fail = |reason: &'static str| {
        out.push(ValidationError::BadTemperatureTable {
            body: body.to_string(),
            reason,
        })
    };
    let knots = &atm.temperature_knots;
    if knots.len() < 2 {
        fail("needs at least 2 knots");
        return;
    }
    let mut prev = f64::NEG_INFINITY;
    for k in knots {
        if !k.altitude_m.is_finite() || !k.temperature_k.is_finite() {
            fail("non-finite knot");
            return;
        }
        if k.altitude_m <= prev {
            fail("altitudes not strictly increasing");
            return;
        }
        if k.altitude_m < 0.0 || k.altitude_m > atm.top_m {
            fail("knot outside [0, top_m]");
            return;
        }
        if k.temperature_k <= 0.0 {
            fail("non-positive temperature");
            return;
        }
        prev = k.altitude_m;
    }
}

fn check_biomes(
    b: &crate::load::LoadedBody,
    bm: &crate::load::LoadedBiomeMap,
    out: &mut Vec<ValidationError>,
) {
    let body = b.def.id.as_str();
    let mut indices = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for e in &bm.legend.entries {
        if !indices.insert(e.index) {
            out.push(ValidationError::BiomeLegendDuplicate {
                body: body.to_string(),
                detail: format!("index {}", e.index),
            });
        }
        if !ids.insert(e.id.as_str()) {
            out.push(ValidationError::BiomeLegendDuplicate {
                body: body.to_string(),
                detail: format!("id {:?}", e.id),
            });
        }
    }
    match inspect_indexed_png(&bm.png) {
        Err(reason) => out.push(ValidationError::BiomeMapPngInvalid {
            body: body.to_string(),
            reason,
        }),
        Ok(info) => {
            if info.width > BIOME_MAP_MAX_W || info.height > BIOME_MAP_MAX_H {
                out.push(ValidationError::BiomeMapTooLarge {
                    body: body.to_string(),
                    width: info.width,
                    height: info.height,
                });
            }
            for e in &bm.legend.entries {
                if usize::from(e.index) >= info.palette_len {
                    out.push(ValidationError::BiomeIndexExceedsPalette {
                        body: body.to_string(),
                        index: e.index,
                        palette_len: info.palette_len,
                    });
                }
            }
        }
    }
}

/// §8.2's "ID stability"/loc-resolution check: every `LocKey` the system
/// references (`epoch_note`, display names, biome names) must resolve in
/// the locale file. Separate from [`validate_system`] because the loc
/// file lives outside the system directory.
pub fn validate_loc_keys(sys: &LoadedSystem, loc: &LocFile) -> Vec<ValidationError> {
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
    check("system.ron".to_string(), &sys.system.epoch_note);
    for b in &sys.bodies {
        check(format!("{}/body.ron", b.def.id), &b.def.display_name);
        if let Some(bm) = &b.biomes {
            for e in &bm.legend.entries {
                check(format!("{}/biomes.ron", b.def.id), &e.name);
            }
        }
    }
    out
}

// §2.13's no-panic rule covers shipping fallible paths; tests signal
// failure by panicking anyway, so unwrap/expect are allowed here.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::biome::BiomeLegendEntry;
    use crate::ids::{BiomeId, SystemId};
    use crate::load::{LoadedBiomeMap, LoadedBody};
    use crate::system::{
        AtmosphereDef, BodyDef, KeplerElements, OceanDef, SystemDef, TemperatureKnot, TerrainDef,
    };

    /// A minimal but *valid* two-body system (star + one planet) — every
    /// negative test mutates this fixture so exactly one rule fires.
    fn fixture() -> LoadedSystem {
        let mu_star = 1.0e14;
        let a = 1.0e9;
        let period = 2.0 * core::f64::consts::PI * libm::sqrt(a * a * a / mu_star);
        let star = LoadedBody {
            def: BodyDef {
                schema_version: SCHEMA_VERSION,
                id: "star".into(),
                display_name: "body.star.name".into(),
                parent: None,
                radius_m: 1.0e6,
                mu_m3_per_s2: mu_star,
                soi_m: None,
                rotation_period_s: Some(100_000.0),
                tilt_rad: 0.0,
                orbit: None,
                atmosphere: None,
                ocean: None,
                terrain: TerrainDef {
                    noise_seed: 1,
                    heightmap: None,
                },
                biome_map: None,
                surface_resources: Vec::new(),
                insolation_w_m2: Some(1361.0),
            },
            biomes: None,
        };
        let planet = LoadedBody {
            def: BodyDef {
                schema_version: SCHEMA_VERSION,
                id: "planet".into(),
                display_name: "body.planet.name".into(),
                parent: Some("star".into()),
                radius_m: 1.0e5,
                mu_m3_per_s2: 1.0e11,
                soi_m: Some(1.0e8),
                rotation_period_s: None, // tidally locked is legal
                tilt_rad: 0.0,
                orbit: Some(KeplerElements {
                    semi_major_axis_m: a,
                    eccentricity: 0.0,
                    inclination_rad: 0.0,
                    ascending_node_rad: 0.0,
                    arg_periapsis_rad: 0.0,
                    mean_anomaly_epoch_rad: 0.0,
                    period_s: period,
                }),
                atmosphere: None,
                ocean: Some(OceanDef {
                    datum_radius_m: 1.0e5,
                }),
                terrain: TerrainDef {
                    noise_seed: 2,
                    heightmap: None,
                },
                biome_map: None,
                surface_resources: Vec::new(),
                insolation_w_m2: Some(100.0),
            },
            biomes: None,
        };
        LoadedSystem {
            system: SystemDef {
                schema_version: SCHEMA_VERSION,
                id: SystemId("test".into()),
                star: "star".into(),
                bodies: vec!["star".into(), "planet".into()],
                epoch_note: "system.test.epoch_note".into(),
            },
            bodies: vec![star, planet],
        }
    }

    fn planet(sys: &mut LoadedSystem) -> &mut BodyDef {
        &mut sys.bodies[1].def
    }

    /// The fixture itself must be clean — otherwise the negative tests
    /// below attribute nothing.
    #[test]
    fn fixture_is_valid() {
        assert!(validate_system(&fixture()).is_empty());
    }

    #[test]
    fn soi_below_child_apoapsis_is_rejected() {
        let mut sys = fixture();
        // Star's SOI smaller than the planet's 1e9 m apoapsis.
        sys.bodies[0].def.soi_m = Some(5.0e8);
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::SoiTooSmall { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn parent_cycle_is_rejected() {
        let mut sys = fixture();
        // Add a moon pair that orbit each other.
        let mut a = sys.bodies[1].clone();
        a.def.id = "a".into();
        a.def.parent = Some("b".into());
        let mut b = sys.bodies[1].clone();
        b.def.id = "b".into();
        b.def.parent = Some("a".into());
        sys.bodies.push(a);
        sys.bodies.push(b);
        sys.system.bodies.push("a".into());
        sys.system.bodies.push("b".into());
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .filter(|e| matches!(e, ValidationError::ParentCycle { .. }))
                .count()
                >= 2,
            "both cycle members report: {errs:?}"
        );
    }

    #[test]
    fn period_inconsistent_with_parent_mu_is_rejected() {
        let mut sys = fixture();
        let o = planet(&mut sys).orbit.as_mut().unwrap();
        o.period_s *= 1.000_001;
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::PeriodInconsistent { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn atmosphere_top_below_scale_height_is_rejected() {
        let mut sys = fixture();
        planet(&mut sys).atmosphere = Some(AtmosphereDef {
            sea_level_pressure_pa: 101_325.0,
            scale_height_m: 5_600.0,
            top_m: 5_000.0, // below H — §3.0 violation
            molar_mass_kg_per_mol: 0.029,
            heat_capacity_ratio: 1.4,
            temperature_knots: vec![
                TemperatureKnot {
                    altitude_m: 0.0,
                    temperature_k: 288.0,
                },
                TemperatureKnot {
                    altitude_m: 5_000.0,
                    temperature_k: 200.0,
                },
            ],
        });
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::AtmosphereTopTooLow { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn dangling_parent_is_rejected() {
        let mut sys = fixture();
        planet(&mut sys).parent = Some("ghost".into());
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::ParentNotFound { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn second_root_is_rejected() {
        let mut sys = fixture();
        let mut stray = sys.bodies[1].clone();
        stray.def.id = "stray".into();
        stray.def.parent = None;
        stray.def.orbit = None;
        sys.bodies.push(stray);
        sys.system.bodies.push("stray".into());
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::RootNotStar { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn bad_schema_version_is_rejected() {
        let mut sys = fixture();
        planet(&mut sys).schema_version = 99;
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::SchemaVersion { found: 99, .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn hyperbolic_and_negative_eccentricity_are_rejected() {
        for e in [1.0, 1.5, -0.1] {
            let mut sys = fixture();
            planet(&mut sys).orbit.as_mut().unwrap().eccentricity = e;
            let errs = validate_system(&sys);
            assert!(
                errs.iter()
                    .any(|x| matches!(x, ValidationError::EccentricityOutOfRange { .. })),
                "e={e}: {errs:?}"
            );
        }
    }

    #[test]
    fn non_finite_radius_is_rejected() {
        let mut sys = fixture();
        planet(&mut sys).radius_m = f64::NAN;
        let errs = validate_system(&sys);
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::NonFinite {
                    field: "radius_m",
                    ..
                }
            )),
            "{errs:?}"
        );
    }

    /// A minimal valid palette-PNG byte stream for validator tests — the
    /// inspector never checks CRCs, so chunks can be hand-assembled.
    fn tiny_png(w: u32, h: u32, palette_entries: usize) -> Vec<u8> {
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 3, 0, 0, 0]);
        for (kind, data) in [
            (*b"IHDR", ihdr),
            (*b"PLTE", vec![0u8; palette_entries * 3]),
            (*b"IEND", Vec::new()),
        ] {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(&kind);
            out.extend_from_slice(&data);
            out.extend_from_slice(&[0, 0, 0, 0]); // unchecked CRC
        }
        out
    }

    fn with_biomes(sys: &mut LoadedSystem, legend_idx: u8, png: Vec<u8>) {
        let b = &mut sys.bodies[1];
        b.def.biome_map = Some(crate::system::BiomeMapRef {
            png: "biomes.png".into(),
            legend: "biomes.ron".into(),
        });
        b.biomes = Some(LoadedBiomeMap {
            legend: crate::biome::BiomeLegend {
                schema_version: SCHEMA_VERSION,
                entries: vec![
                    BiomeLegendEntry {
                        index: 0,
                        id: BiomeId("planet.one".into()),
                        name: "biome.planet.one".into(),
                    },
                    BiomeLegendEntry {
                        index: legend_idx,
                        id: BiomeId("planet.two".into()),
                        name: "biome.planet.two".into(),
                    },
                ],
            },
            png,
        });
    }

    #[test]
    fn biome_legend_index_beyond_palette_is_rejected() {
        let mut sys = fixture();
        with_biomes(&mut sys, 5, tiny_png(4, 4, 2)); // palette of 2, index 5
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BiomeIndexExceedsPalette { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn non_png_biome_map_is_rejected() {
        let mut sys = fixture();
        with_biomes(&mut sys, 1, b"not a png".to_vec());
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BiomeMapPngInvalid { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn oversized_biome_map_is_rejected() {
        let mut sys = fixture();
        with_biomes(&mut sys, 1, tiny_png(4096, 32, 2));
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BiomeMapTooLarge { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn duplicate_legend_index_is_rejected() {
        let mut sys = fixture();
        with_biomes(&mut sys, 0, tiny_png(4, 4, 2)); // two entries at index 0
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BiomeLegendDuplicate { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn unresolved_loc_key_is_rejected() {
        let mut loc = std::collections::BTreeMap::new();
        loc.insert("system.test.epoch_note".to_string(), "x".to_string());
        loc.insert("body.star.name".to_string(), "x".to_string());
        // Deliberately omit body.planet.name.
        let loc = LocFile {
            schema_version: SCHEMA_VERSION,
            strings: loc,
        };
        let errs = validate_loc_keys(&fixture(), &loc);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::UnresolvedLocKey { key, .. } if key == "body.planet.name")),
            "{errs:?}"
        );
    }
}
