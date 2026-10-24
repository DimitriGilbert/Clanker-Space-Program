//! The §3.0 `SystemDef`/`BodyDef` schema — the home system (and future
//! systems) are authored as `data/systems/<id>/system.ron` plus one
//! `<body>/body.ron` per body.
//!
//! Every file's first field is `schema_version` (§3.1): the loader accepts
//! the current version (and, once a v2 exists, the previous version with
//! explicit defaults); the validator enforces it again so a hand-rolled
//! fixture cannot smuggle a skipped check.

use serde::{Deserialize, Serialize};

use crate::ids::{BodyId, LocKey, ResourceId, SystemId};

/// The `schema_version` this loader/validator pair understands (§3.1).
/// Bump when the schema changes; old versions get explicit defaults in
/// the loader rather than bare serde errors.
pub const SCHEMA_VERSION: u32 = 1;

/// Classical Keplerian elements for a body or vessel on rails (§2.8
/// `OrbitState` consumes this; `epoch` lives there — a body's epoch is
/// sim-time 0).
///
/// **Two populations share the type, with different invariants.**
///
/// *Authored body* orbits are always bound: the §3.0 validator requires
/// `a > 0`, `0 ≤ e < 1`, and `period_s` finite and consistent with
/// `2π√(a³/μ_parent)`.
///
/// *Vessel* elements — built sim-side by `csp_sim::orbit`, never by the
/// data validator — may be **open**: a hyperbola stores the standard
/// `a < 0`, and a parabolic (`e ≈ 1`) orbit stores `a := −p` (negative
/// semi-latus rectum `p = h²/μ`, so `r_p = |a|/2` and the mean-motion
/// convention `n = √(μ/|a|³)` survives); every open orbit stores
/// `period_s = +∞`. Consumers of this type must therefore not assume
/// `a > 0` or `e < 1` — branch on `eccentricity`.
///
/// All angles are radians. `period_s` is deliberately redundant for
/// authored data: the §3.0 validator checks `T = 2π√(a³/μ_parent)` to
/// 1e-9 relative, so an authoring slip in *either* `a` or `T` is caught
/// rather than silently disagreeing with the sheet.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeplerElements {
    /// Semi-major axis `a` (m). Positive for ellipses; **negative for
    /// hyperbolic vessel orbits** (`a := −p` under the parabolic
    /// encoding — see the type doc).
    pub semi_major_axis_m: f64,
    /// Eccentricity `e`. Authored bodies are bound: `0 ≤ e < 1` (the
    /// §3.0 validator rejects otherwise). Vessel elements may reach
    /// `e ≥ 1` — `e > 1` is hyperbolic, `e ≈ 1` the parabolic encoding.
    pub eccentricity: f64,
    /// Inclination `i` (rad).
    pub inclination_rad: f64,
    /// Longitude of the ascending node `Ω` (rad).
    pub ascending_node_rad: f64,
    /// Argument of periapsis `ω` (rad).
    pub arg_periapsis_rad: f64,
    /// Mean anomaly at epoch `M0` (rad) — the anomaly-at-epoch form the
    /// §2.8 solver advances from.
    pub mean_anomaly_epoch_rad: f64,
    /// Authored orbital period (s). See the type doc — this field exists
    /// to be *checked*, not to drive the sim. `+∞` for open (vessel-side)
    /// orbits, which have no period.
    pub period_s: f64,
}

/// One knot of a body's authored piecewise-linear temperature table
/// (§3.0 atmosphere model).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemperatureKnot {
    /// Altitude above the datum (m). Knots must be strictly increasing.
    pub altitude_m: f64,
    /// Temperature at that altitude (K). Between knots the sim
    /// interpolates linearly; outside the table it clamps to the ends.
    pub temperature_k: f64,
}

/// §3.0 v1 atmosphere — closed-form so it can be evaluated inside the
/// tick: `p(h) = p0·exp(−h/H)` up to `top_m`, then a documented hard cut
/// to zero. Density from the ideal gas law with `molar_mass_kg_per_mol`;
/// speed of sound `√(γRT/M)` with `heat_capacity_ratio` as γ.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AtmosphereDef {
    /// Sea-level (datum) pressure `p0` (Pa).
    pub sea_level_pressure_pa: f64,
    /// Scale height `H` (m).
    pub scale_height_m: f64,
    /// Top of atmosphere above the datum (m) — must exceed
    /// `scale_height_m` (§3.0 validator rule).
    pub top_m: f64,
    /// Molar mass `M` (kg/mol) for ρ = pM/(RT).
    pub molar_mass_kg_per_mol: f64,
    /// Ratio of specific heats γ for `√(γRT/M)`.
    pub heat_capacity_ratio: f64,
    /// Piecewise-linear temperature table, ≥2 knots, altitudes strictly
    /// increasing and within `[0, top_m]`.
    pub temperature_knots: Vec<TemperatureKnot>,
}

/// A surface ocean (§3.0: Terrin has "ocean — yes (datum r = R)").
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OceanDef {
    /// Radius of the ocean surface above the body's centre (m) — the
    /// altimetry datum for "splashed" vs "landed".
    pub datum_radius_m: f64,
}

/// Terrain authoring refs (§3.0 schema: "noise seed + authored heightmap
/// refs"). The seed is consumed by the R2 terrain generator; biomes are
/// deliberately NOT derived from it (§3.0.1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerrainDef {
    /// Seed for the procedural terrain generator (R2). Authored so a
    /// generated world is reproducible across saves and replays.
    pub noise_seed: u64,
    /// Authored heightmap, path relative to the body directory —
    /// `None` for pure noise (the §3.7/R2 "hybrid" question).
    pub heightmap: Option<String>,
}

/// Reference to a body's §3.0.1 biome map: a palette-indexed equirect
/// PNG plus its RON legend.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BiomeMapRef {
    /// Palette-indexed PNG, relative to the body directory. Equirect,
    /// ≤2048×1024, 8-bit palette (colour type 3) — the validator parses
    /// the header and enforces this.
    pub png: String,
    /// RON legend (`schema_version` + `Vec<{index, id, name}>`),
    /// relative to the body directory.
    pub legend: String,
}

/// Reference to a per-resource surface concentration map (§3.1
/// `surface_resources`). A grayscale raster where each pixel is the
/// local concentration fraction; the sim reads it via the biome-map-style
/// equirect projection. Schema-only at M1 — no maps are authored yet.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConcentrationMapRef {
    /// Map file, relative to the body directory.
    pub png: String,
}

/// `data/systems/<id>/system.ron` — the system's identity and manifest.
/// Bodies are *listed* here and *defined* by their own `body.ron` files,
/// so the manifest is the complete body set (the validator checks the
/// two agree exactly).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemDef {
    /// First field of every data file (§3.1). Must equal
    /// [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// System identifier — also its directory name under `data/systems/`.
    pub id: SystemId,
    /// The system's root body: every other body's parent chain must
    /// terminate here.
    pub star: BodyId,
    /// Every body in the system, as `BodyId`s — each names a
    /// `<id>/body.ron` file under the system directory.
    pub bodies: Vec<BodyId>,
    /// Loc key for the note explaining the epoch convention.
    pub epoch_note: LocKey,
}

/// `data/systems/<sys>/<body>/body.ron` — one celestial body (§3.0).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BodyDef {
    /// First field of every data file (§3.1). Must equal
    /// [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Body identifier — must equal its entry in [`SystemDef::bodies`]
    /// (and therefore its directory name).
    pub id: BodyId,
    /// Loc key for the display name.
    pub display_name: LocKey,
    /// The body this one orbits — `None` for the star only.
    pub parent: Option<BodyId>,
    /// Equatorial radius (m).
    pub radius_m: f64,
    /// Gravitational parameter μ (m³/s²). Authored directly — mass is
    /// derived (§3.0).
    pub mu_m3_per_s2: f64,
    /// Sphere-of-influence radius (m); `None` = infinite (the star).
    /// The validator requires a parent's SOI to exceed every child's
    /// apoapsis.
    pub soi_m: Option<f64>,
    /// Sidereal rotation period (s); `None` = tidally locked to the
    /// parent (rotation period = orbital period — §3.0, Rask).
    pub rotation_period_s: Option<f64>,
    /// Axial tilt (rad). 0 across the v1 home system by design (§3.0:
    /// it makes insolation a function of latitude and local hour only).
    pub tilt_rad: f64,
    /// Keplerian elements; `None` only for the star.
    pub orbit: Option<KeplerElements>,
    /// Atmosphere, if any (§3.0 model above).
    pub atmosphere: Option<AtmosphereDef>,
    /// Ocean, if any.
    pub ocean: Option<OceanDef>,
    /// Terrain authoring refs — required even for a flat body.
    pub terrain: TerrainDef,
    /// §3.0.1 biome map ref — `None` for bodies without surface science
    /// contexts (the star).
    pub biome_map: Option<BiomeMapRef>,
    /// Surface resource deposits as `(resource, concentration map)`
    /// pairs (§3.1). Empty in the M1 home system.
    pub surface_resources: Vec<(ResourceId, ConcentrationMapRef)>,
    /// Authored insolation at this body's orbit (W/m², §3.0). For the
    /// star it is the *reference* flux at the home orbit distance
    /// ("1361 @ 1.36e10 m").
    pub insolation_w_m2: Option<f64>,
}
