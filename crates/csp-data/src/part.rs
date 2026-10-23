//! The §3.1 `PartDef` schema — `data/parts/<id>.ron`.
//!
//! This is the authored half of §3.1's
//! `EffectivePart = resolve(BuiltPart) ∘ wear(PartUsage)`: every number
//! the effective-stat resolver needs is declared here, so `resolve` is a
//! pure lookup over (def, variant, scale, frozen upgrade levels) and
//! never a data-file read. The resolver lives in `csp-phys`
//! (`csp_phys::resolve`) — the §2.1 dep direction lets it consume these
//! defs and emit the weld's `WeldPart` view in one place.
//!
//! Conventions shared with `system.rs` (§3.1): `schema_version` is the
//! first field of every file; IDs are stable strings; all fields are
//! named. Geometry fields are part-local: +z is the part's stack axis
//! ("up", away from the engine nozzle — the same convention the M2
//! flight fixture uses, where a `Thrust` `ForceSite` sits at
//! `z = −h/2` and points along +z).

use csp_core::DVec3;
use serde::{Deserialize, Serialize};

use crate::ids::{
    ExperimentId, FamilyId, LocKey, PartId, RecipeId, ResourceId, TechNodeId, VariantId,
};

/// The `schema_version` this loader understands for `data/parts/*.ron`
/// — its own namespace, like `balance.ron` (§3.1 first-field
/// convention).
pub const PART_SCHEMA_VERSION: u32 = 1;

/// Standard gravity for `isp` ↔ mass-flow conversion (m/s²). Terrin's
/// surface g is authored at exactly 9.810 (§3.0 table), so the
/// conventional `g₀ = 9.81` is *also* the in-fiction home surface g —
/// no separate constant is needed or wanted.
pub const G0: f64 = 9.81;

/// Editor taxonomy for the part browser. Category also carries a
/// validator obligation (§3.1 "category-consistent stats"): an `Engine`
/// part must actually carry an `Engine` module, a `FuelTank` a `Tank`
/// module, and so on — the mapping is [`PartCategory::required_module`]
/// and the rule lives in `crate::validate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PartCategory {
    /// Liquid engines — must carry a [`ModuleDef::Engine`].
    Engine,
    /// Propellant and cargo tanks — must carry a [`ModuleDef::Tank`]
    /// (§3.5: "a Cargo bay is just a Tank").
    FuelTank,
    /// Adapters, trusses, nose cones — passive structure; no module
    /// requirement beyond well-formed attach nodes.
    Structure,
    /// Pods, probe cores, reaction wheels — must carry a
    /// [`ModuleDef::CrewSeat`] or [`ModuleDef::Control`].
    Control,
    /// Payload-carrying parts (cargo bays, service compartments) —
    /// must carry a [`ModuleDef::Tank`] or [`ModuleDef::DockPort`].
    Payload,
    /// Experiment packages — must carry a [`ModuleDef::Experiment`].
    Science,
    /// Everything else: batteries, decouplers, drills, converters,
    /// labs. No module requirement — the category is a browser bucket.
    Utility,
}

impl PartCategory {
    /// Stable name for diagnostics.
    pub fn name(self) -> &'static str {
        match self {
            Self::Engine => "engine",
            Self::FuelTank => "fuel-tank",
            Self::Structure => "structure",
            Self::Control => "control",
            Self::Payload => "payload",
            Self::Science => "science",
            Self::Utility => "utility",
        }
    }
}

/// One attachment point for the editor's snap graph (§3.1
/// `attach_nodes`, M3 "Part snap graph"). Two parts snap when a node on
/// each coincide in position with opposing `direction`s; `diameter_m`
/// is the size class the editor matches (a 1.25 m node does not snap a
/// 0.625 m node without an adapter — the check is editor policy, the
/// number is data).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttachNode {
    /// Node name, unique within the part (`"top"`, `"bottom"`).
    /// Craft files reference `(part_node, node_id)` pairs.
    pub id: String,
    /// Node position in part-local frame (m), typically on the
    /// bounding-box face.
    pub position_m: DVec3,
    /// Outward normal the node accepts a mate *against*: a bottom node
    /// has `direction = (0,0,−1)` and mates a top node's `(0,0,+1)`.
    /// Must be unit length (validator enforces ‖d‖ = 1 ± 1e-6).
    pub direction: DVec3,
    /// Snap size class (m). Matched against the mate's `diameter_m`.
    pub diameter_m: f64,
}

/// How a part may be resized in the editor (§3.1 `scale` —
/// "TweakScale native to part defs" per the §-table entry "v1 (M3)").
/// `s` is the linear scale factor: geometry, attach-node positions and
/// diameters scale by `s`; stats scale by `s^exponent` where the
/// exponents are authored (see [`ScaleExponents`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ScaleRule {
    /// Fixed size — pods and engines typically do not scale.
    None,
    /// A menu of fixed sizes (e.g. `[0.5, 1.0, 2.0]`). Must be
    /// non-empty, positive, strictly increasing, and contain `1.0` —
    /// the authored stats are the `s = 1` baseline.
    Discrete(Vec<f64>),
    /// Free-range scaling within `[min, max]` (validator:
    /// `0 < min ≤ 1 ≤ max`).
    Continuous {
        /// Smallest legal linear factor.
        min: f64,
        /// Largest legal linear factor.
        max: f64,
        /// Per-stat-family exponents for `s^exp` scaling.
        exponents: ScaleExponents,
    },
}

/// Stat exponents for [`ScaleRule::Continuous`]. Each is the `e` in
/// `stat(s) = stat(1) · s^e`. Sane authored values: `mass`/`capacity` ≈ 3
/// (volume), `thrust` ≈ 2–3 (nozzle area to mass-flow), `cost` ≈ 3,
/// `structural` ≈ 2 (cross-section). The validator bounds every
/// exponent to `[0, 4]` — exponents outside that are authoring errors,
/// not physics the sim supports.
///
/// Known coverage gaps, deliberate for v1: there is **no exponent
/// channel** for `build_resources` (the resolver uses `mass` — the BOM
/// is mass-like), `ModuleDef::Control::torque_nm`, or
/// `ThermalDef::conduction_w_per_k` (both left unscaled). Adding any of
/// them is a schema-version decision — see `csp-phys`'s resolve module
/// doc for the deferred-question list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScaleExponents {
    /// Exponent on `mass_dry_kg` — also applied to `build_resources`
    /// amounts (the BOM is mass-like; see the struct doc).
    pub mass: f64,
    /// Exponent on `cost_funds`.
    pub cost: f64,
    /// Exponent on engine `thrust_vac_n` (ignored for non-engines).
    pub thrust: f64,
    /// Exponent on `Tank` capacities (resource units) and
    /// `CrewSeat`/`Lab` counts at the designer's discretion.
    pub capacity: f64,
    /// Exponent on `StructuralDef` limits.
    pub structural: f64,
}

/// The authored collider the resolver turns into a
/// `csp_phys::ColliderShape` (§3.1 `collider`; §2.4 Stage 1 merges
/// per-part convex hulls into the compound collider). Primitives
/// convert to vertex sets; `Convex` is the escape hatch for shapes
/// that are not primitives.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ColliderDef {
    /// Axis-aligned box by half-extents (m).
    Box { half_extents_m: DVec3 },
    /// Cylinder about the local +z axis: `radius_m` and
    /// `half_height_m`.
    Cylinder { radius_m: f64, half_height_m: f64 },
    /// Extreme points of a convex polytope in part-local frame —
    /// the resolver emits them verbatim as `ColliderShape::Points`.
    /// Minimum 4 non-degenerate points (validator).
    Convex { points: Vec<DVec3> },
}

/// One knot of an authored aero coefficient curve (§3.1 `aero` —
/// "Cd/Cl curves"). Piecewise-linear in Mach; outside the table the
/// resolver clamps to the ends, matching the §3.0 temperature-table
/// convention.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AeroKnot {
    /// Mach number (≥ 0, strictly increasing within a curve).
    pub mach: f64,
    /// Coefficient value at that Mach (≥ 0).
    pub coeff: f64,
}

/// §3.1 `aero`: analytic Cd/Cl curves plus the reference length the
/// §2.4.1 voxelizer's `AeroTable` normalizes against. Exposed/shielded
/// area is *not* authored — it comes from voxelization at weld Stage 2.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AeroDef {
    /// Drag coefficient vs Mach. ≥ 1 knot.
    pub cd_curve: Vec<AeroKnot>,
    /// Lift coefficient vs Mach. ≥ 1 knot.
    pub cl_curve: Vec<AeroKnot>,
    /// Reference length (m) — the part's aero scale, before part
    /// scaling multiplies it.
    pub ref_len_m: f64,
}

impl AeroDef {
    /// Constant-coefficient convenience — a single-knot flat `Cd`/`Cl`
    /// (Mach-independent). For tests, scenarios, and parts whose aero
    /// is genuinely flat (blunt bodies); authored part data should
    /// prefer real curves.
    pub fn constant(cd: f64, cl: f64, ref_len_m: f64) -> Self {
        Self {
            cd_curve: vec![AeroKnot {
                mach: 0.0,
                coeff: cd,
            }],
            cl_curve: vec![AeroKnot {
                mach: 0.0,
                coeff: cl,
            }],
            ref_len_m,
        }
    }
}

/// §3.1 `thermal`: lumped-capacity parameters. `heat_capacity_j_per_k`
/// is the lump (`mass·cp`); `conduction_w_per_k` is the coupling to
/// neighbour parts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThermalDef {
    /// Lumped heat capacity, `mass·cp` (J/K).
    pub heat_capacity_j_per_k: f64,
    /// Maximum service temperature (K) — §3.4's overtemperature stress
    /// is measured as a fraction of this.
    pub max_temp_k: f64,
    /// Surface emissivity ε ∈ (0, 1] for the §2.4.1/§3.5.1 radiation
    /// terms.
    pub emissivity: f64,
    /// Conductive coupling to attached neighbour parts (W/K).
    pub conduction_w_per_k: f64,
}

/// §3.1 `structural`: weld/breakup limits. §2.4 breakup splits the
/// part graph at the node whose load exceeds these.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StructuralDef {
    /// Maximum axial (along-stack) load (N).
    pub max_axial_n: f64,
    /// Maximum shear load (N).
    pub max_shear_n: f64,
    /// Maximum torque at an attachment (N·m).
    pub max_torque_nm: f64,
}

impl StructuralDef {
    /// Effectively unbreakable limits for hand-authored `WeldPart`s
    /// (fixtures, single-part test bodies) — finite, so it passes the
    /// weld's input gate, and far past any real joint load.
    pub fn unlimited() -> Self {
        Self {
            max_axial_n: 1e30,
            max_shear_n: 1e30,
            max_torque_nm: 1e30,
        }
    }
}

/// The scalar stat a variant or §3.3 `UpgradeTrack` modifies. Variants
/// (this file) and upgrade tracks (`data/techtree/*.ron`) share this
/// vocabulary — `UpgradeTrack { family, stat_path, levels }` resolves
/// `stat_path` to one of these, and `multiplier_or_delta` multiplies
/// the corresponding authored field. Coverage is deliberately the
/// scalar stats the resolver reads; resource-pair fields (tank
/// capacities) are not variant targets in v1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum StatPath {
    /// `PartDef::mass_dry_kg`.
    MassDryKg,
    /// `PartDef::cost_funds`.
    CostFunds,
    /// `ModuleDef::Engine::thrust_vac_n`.
    ThrustVacN,
    /// `ModuleDef::Engine::isp_vac_s`.
    IspVacS,
    /// `ModuleDef::Engine::isp_sl_s`.
    IspSlS,
    /// `ModuleDef::Engine::rated_burn_s`.
    RatedBurnS,
    /// `ModuleDef::Control::torque_nm`.
    ControlTorqueNm,
    /// `ReliabilityDef::base_mtbf_s`.
    BaseMtbfS,
}

impl StatPath {
    /// Stable name for diagnostics (`"mass_dry_kg"` etc.).
    pub fn name(self) -> &'static str {
        match self {
            Self::MassDryKg => "mass_dry_kg",
            Self::CostFunds => "cost_funds",
            Self::ThrustVacN => "thrust_vac_n",
            Self::IspVacS => "isp_vac_s",
            Self::IspSlS => "isp_sl_s",
            Self::RatedBurnS => "rated_burn_s",
            Self::ControlTorqueNm => "torque_nm",
            Self::BaseMtbfS => "base_mtbf_s",
        }
    }
}

/// One authored variant of a part (§3.3 "scaling + variants"; the
/// `variant: VariantId` `BuiltPart` freezes). A variant is a
/// **multiplier set over the base def, never a new `PartDef`** — the
/// same rule §3.3 gives upgrade tracks, one layer down. Authoring
/// convention: the plain `"stock"` variant ships in every def with
/// unit multipliers; the validator does not require that name, only
/// that the variant set is non-empty with unique IDs and positive
/// finite multipliers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PartVariant {
    /// Variant ID, unique within the part.
    pub id: VariantId,
    /// Loc key for the display name.
    pub display_name: LocKey,
    /// Multiplier on `cost_funds` — variants are the §3.3 cost/quality
    /// trade: better stats cost more.
    pub cost_multiplier: f64,
    /// `(stat, multiplier)` pairs — `stat(1) · m` per entry, applied on
    /// top of scaling at resolve time. Stat paths unique within the
    /// variant (validator).
    pub stat_multipliers: Vec<(StatPath, f64)>,
}

/// Propellant feed range as a fraction of rated thrust.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThrottleRange {
    /// Minimum throttle setting ∈ [0, 1).
    pub min: f64,
    /// Maximum throttle setting ∈ (0, 1] — `1.0` is `thrust_vac_n`.
    pub max: f64,
}

/// §3.2's six situations — the `situation` leg of the
/// `(body, biome, situation)` science key. "KSP-proven granularity; do
/// not invent more" (§3.2). Discriminants are explicit, stable,
/// append-only — the sim's `DataKey` hashes them (§7.1 rule 7), and
/// each discriminant is also its [`SituationMask`] bit index, so the
/// enum and the mask can never drift apart.
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Situation {
    /// On the solid surface.
    Landed = 0,
    /// On the ocean.
    Splashed = 1,
    /// Airborne below the flying-low/flying-high split.
    FlyingLow = 2,
    /// Airborne above it.
    FlyingHigh = 3,
    /// In the body's low-space band.
    SpaceLow = 4,
    /// In high space.
    SpaceHigh = 5,
}

impl Situation {
    /// The situation's [`SituationMask`] bit.
    pub fn mask(self) -> SituationMask {
        SituationMask(1 << (self as u8))
    }

    /// Stable name for diagnostics.
    pub fn name(self) -> &'static str {
        match self {
            Self::Landed => "landed",
            Self::Splashed => "splashed",
            Self::FlyingLow => "flying_low",
            Self::FlyingHigh => "flying_high",
            Self::SpaceLow => "space_low",
            Self::SpaceHigh => "space_high",
        }
    }

    /// The situations whose contexts are on or just above the surface —
    /// residence sanity for the sim's `RunExperiment` apply: a
    /// `Landed`/`Splashed`/`FlyingLow`/`FlyingHigh` claim needs a
    /// body-adjacent residence (in-bubble or surface-latched), never
    /// pure rails.
    pub fn is_surface_side(self) -> bool {
        matches!(
            self,
            Self::Landed | Self::Splashed | Self::FlyingLow | Self::FlyingHigh
        )
    }
}

impl csp_core::StableDiscriminant for Situation {
    fn discriminant(&self) -> u16 {
        *self as u16
    }
}

/// §3.2's six situations as a bitmask over `u8` — `Experiment`
/// modules declare where they may run. Transparent serde: files spell
/// it as an integer mask, which keeps the RON terse and the bincode
/// path trivial.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SituationMask(pub u8);

impl SituationMask {
    /// `Landed` bit (§3.2).
    pub const LANDED: Self = Self(1 << 0);
    /// `Splashed` bit.
    pub const SPLASHED: Self = Self(1 << 1);
    /// `FlyingLow` bit.
    pub const FLYING_LOW: Self = Self(1 << 2);
    /// `FlyingHigh` bit.
    pub const FLYING_HIGH: Self = Self(1 << 3);
    /// `SpaceLow` bit.
    pub const SPACE_LOW: Self = Self(1 << 4);
    /// `SpaceHigh` bit.
    pub const SPACE_HIGH: Self = Self(1 << 5);
    /// Every situation set — convenience for `ExperimentDef` cap entries
    /// that apply body-wide.
    pub const ALL: Self = Self(
        Self::LANDED.0
            | Self::SPLASHED.0
            | Self::FLYING_LOW.0
            | Self::FLYING_HIGH.0
            | Self::SPACE_LOW.0
            | Self::SPACE_HIGH.0,
    );

    /// True when no situation bit is set — the validator rejects an
    /// experiment that can run nowhere.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether `situation`'s bit is set.
    pub fn contains(self, situation: Situation) -> bool {
        self.0 & situation.mask().0 != 0
    }

    /// The set bits as a [`Situation`] list, ascending — the canonical
    /// enumeration for iteration (determinism domain).
    pub fn situations(self) -> Vec<Situation> {
        [
            Situation::Landed,
            Situation::Splashed,
            Situation::FlyingLow,
            Situation::FlyingHigh,
            Situation::SpaceLow,
            Situation::SpaceHigh,
        ]
        .into_iter()
        .filter(|s| self.contains(*s))
        .collect()
    }
}

/// Docking port size class (§3.1 `DockPort { size: PortSize }`).
/// Mismatched classes cannot latch — the check is sim policy, the
/// class is data. Discriminants are explicit, stable, append-only —
/// §7.1 rule 7 hash encodings.
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PortSize {
    /// 0.625 m class.
    Small = 0,
    /// 1.25 m class.
    Medium = 1,
    /// 2.5 m class.
    Large = 2,
}

impl csp_core::StableDiscriminant for PortSize {
    fn discriminant(&self) -> u16 {
        *self as u16
    }
}

/// §3.1 `modules` — the functional payloads a part carries. One part
/// may carry several (a crew pod is `CrewSeat + Control`); each
/// variant is the plan's verbatim field set. The resolver turns these
/// into `ForceSite`s, tank inventories, and process rates.
///
/// Two additions beyond the plan's literal list, both plan-implied:
/// `Control` is the reaction wheel / SAS module (§3.4's dormant-failure
/// example is "a probe in orbit can fail a reaction wheel", and M3's
/// live readouts need a control authority to display), and `Decoupler`
/// is the staging part (§2.4 names decoupler transients; §3.4's failure
/// list includes "stuck decoupler").
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ModuleDef {
    /// Liquid engine. §3.1 field set verbatim except `throttle_range`,
    /// spelled as a named [`ThrottleRange`].
    Engine {
        /// Rated thrust in vacuum (N).
        thrust_vac_n: f64,
        /// Specific impulse in vacuum (s).
        isp_vac_s: f64,
        /// Specific impulse at sea level (s) — must not exceed
        /// `isp_vac_s` (validator).
        isp_sl_s: f64,
        /// Allowed throttle band as a fraction of rated thrust.
        throttle_range: ThrottleRange,
        /// Gimbal authority (degrees, ≥ 0).
        gimbal_deg: f64,
        /// Maximum ignitions over the part's life; `None` = unlimited.
        /// §3.4's `ExcessRestarts` stress keys on this.
        ignitions: Option<u32>,
        /// Rated continuous burn (s) — §3.4's `BurnBeyondRated` stress
        /// threshold.
        rated_burn_s: f64,
        /// Propellant consumption as `(resource, mass fraction)` pairs:
        /// fractions are positive and sum to 1.0 (validator), and mass
        /// flow at a given thrust is `thrust / (isp·g₀)` split by these
        /// fractions. The resolver converts mass to units through
        /// `data/resources.ron`.
        propellants: Vec<(ResourceId, f64)>,
        /// Whether ignition requires settled propellant (§3.4's
        /// `UllageNegativeIgnition` stress).
        ullage_required: bool,
    },
    /// Resource storage — a fuel tank *and* a cargo bay and a battery
    /// (§3.5: "a Cargo bay is just a Tank"; `ElectricCharge` with
    /// `mass_per_unit_kg = 0` makes the same variant a battery).
    /// `capacity` is `(resource, units)`; wet mass is
    /// `Σ units · mass_per_unit_kg` via `data/resources.ron`.
    Tank {
        /// `(resource, capacity in units)` pairs — resource IDs unique
        /// within the tank (validator).
        capacity: Vec<(ResourceId, f64)>,
    },
    /// Attitude authority — reaction wheel / SAS. Torque is what the
    /// editor's control readout sums.
    Control {
        /// Maximum torque about each principal axis (N·m).
        torque_nm: f64,
        /// Electric draw while providing authority (W).
        power_w: f64,
    },
    /// Staging decoupler: on activation the attach node below this
    /// part severs (§2.4 joints are transients only — decoupling is a
    /// graph split, not a joint). Marker module; §3.4 covers its
    /// `StuckDecoupler` failure mode via `ReliabilityDef`.
    Decoupler,
    /// Docking port (§3.1). `node` names the `attach_nodes` entry that
    /// is the port *face* — the plan's literal `{ size }` alone cannot
    /// say which node captures (a port part also carries its mount
    /// node), so the designation is plan-implied schema like
    /// `Control`/`Decoupler` are plan-implied modules. The sim's
    /// capture test anchors its soft-capture joint at the named node's
    /// position and requires facing `direction`s; other attach nodes
    /// on the part are ordinary snap points.
    DockPort {
        /// Size class — ports latch only within a class.
        size: PortSize,
        /// `attach_nodes` id of the docking face (validator: must
        /// name an existing node).
        node: String,
    },
    /// Crew accommodation (§3.1). A pod with a seat is command-capable;
    /// pair with `Control` for uncrewed authority.
    CrewSeat {
        /// Number of crew slots.
        slots: u8,
    },
    /// Science experiment (§3.1/§3.2).
    Experiment {
        /// Experiment kind — data records key on it.
        kind: ExperimentId,
        /// Data units one run produces.
        data_units: f64,
        /// Whether it can run again after collection.
        reusable: bool,
        /// Where it may run (§3.2 situation mask, non-empty).
        situations: SituationMask,
    },
    /// Surface resource harvester (§3.1).
    Drill {
        /// Resource extracted.
        resource: ResourceId,
        /// Base extraction rate (units/s) at full concentration.
        base_rate_per_s: f64,
        /// Minimum concentration fraction it can work at.
        min_concentration: f64,
        /// Electric draw while running (W).
        power_w: f64,
    },
    /// ISRU converter (§3.1/§3.5.1).
    Converter {
        /// Recipe ID — recipes are colony data, not part data.
        recipe: RecipeId,
        /// Allocation priority in the colony flow network (§3.5.1 rule
        /// 3: consumers iterate in `(priority, ColonyId, ModuleIndex)`
        /// order).
        priority: u8,
        /// Electric draw while running (W).
        power_w: f64,
    },
    /// Science lab (§3.1/§3.2's exponential processing model).
    Lab {
        /// Data processed per second `T` in §3.2's rate model.
        throughput_data_per_s: f64,
        /// `lab_multiplier` in `R = Σ skill · lab_multiplier`.
        multiplier: f64,
        /// Researcher seats.
        researcher_slots: u8,
    },
}

/// One knot of a [`QualCurve`]: at `qualification_units` accumulated
/// (§3.4 `FamilyQualification.flight_units + bench_units`), the
/// family's hazard multiplier is `hazard_multiplier`. Piecewise-linear
/// in units; the resolver clamps to the ends. First knot is at 0 units
/// by convention — its multiplier is the unqualified hazard factor.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct QualKnot {
    /// Accumulated qualification units (≥ 0, strictly increasing).
    pub qualification_units: f64,
    /// `qual_factor` — multiplier on `λ_base` ∈ (0, 1]. A value of 1 is
    /// "no qualification credit"; lower is a better-proven family.
    pub hazard_multiplier: f64,
}

/// §3.4 `qualification_curve` as a knot table. `λ(t) = λ_base ·
/// Π(stress_i) · qual_factor(family)`; this table is `qual_factor`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QualCurve {
    /// Knots of the piecewise-linear curve (validator: ≥ 1, units
    /// strictly increasing, multipliers ∈ (0, 1]).
    pub knots: Vec<QualKnot>,
}

/// §3.4 stress kinds — "throttle above rated, burn beyond
/// `rated_burn_s`, temperature fraction of `max_temp_k`, g-load,
/// ullage-negative ignition, restarts beyond `ignitions`". Each names
/// which regime metric `StressRule::threshold` is measured against.
/// The §3.4 stress kinds — explicit, stable, append-only `u16`
/// discriminants (the sim's `ReliabilitySpec` hash walk reads them).
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StressKind {
    /// Throttle fraction above the rated point.
    OverThrottle = 0,
    /// Continuous burn beyond `rated_burn_s` (seconds over).
    BurnBeyondRated = 1,
    /// Temperature as a fraction of `ThermalDef::max_temp_k`.
    Overtemperature = 2,
    /// Sustained acceleration load (g).
    ExcessG = 3,
    /// Ignition attempted ullage-negative (requires
    /// `ullage_required` on the engine; threshold unused — the rule
    /// fires per event, `threshold` authors the multiplier's ramp-in
    /// and is conventionally 0).
    UllageNegativeIgnition = 4,
    /// Ignitions beyond the `ignitions` count.
    ExcessRestarts = 5,
}

impl csp_core::StableDiscriminant for StressKind {
    fn discriminant(&self) -> u16 {
        *self as u16
    }
}

/// §3.4 `regime_stress` entry: when the kind's regime metric exceeds
/// `threshold`, `λ` picks up `multiplier`. Multipliers are ≥ 1 and
/// **displayed to the player** (§3.4: "hidden stress multipliers are
/// bad design") — the editor lists every authored rule.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StressRule {
    /// Which regime metric this rule watches.
    pub kind: StressKind,
    /// Threshold in the kind's units (validator: finite, ≥ 0).
    pub threshold: f64,
    /// Hazard multiplier while the stress applies (validator: ≥ 1).
    pub multiplier: f64,
}

/// §3.4 `failure_modes` — "mostly non-fatal: degraded thrust, stuck
/// gimbal, failure to ignite, leak, sensor fault, stuck decoupler,
/// panel jam. Explosions are reserved for thermal/structural."
/// [`FailureMode::is_fatal`] is the §3.4 dormant-hazard filter —
/// "dormant failures are restricted to non-fatal modes".
///
/// Discriminants are explicit, stable, append-only `u16`s — the sim's
/// `PartFailure` event hashes them (§7.1 rule 7).
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureMode {
    /// Thrust degraded — engine runs at reduced output.
    DegradedThrust = 0,
    /// Gimbal frozen in place.
    StuckGimbal = 1,
    /// Engine fails to ignite on the next ignition draw.
    IgnitionFailure = 2,
    /// Tank contents leak over time.
    Leak = 3,
    /// Telemetry/control sensor fault.
    SensorFault = 4,
    /// Decoupler does not sever on the staging signal.
    StuckDecoupler = 5,
    /// Deployable (panel/antenna) does not deploy.
    PanelJam = 6,
    /// Catastrophic loss of the part — thermal/structural only (§3.4).
    /// The only fatal mode; never produced by dormant hazard.
    Explosion = 7,
}

impl csp_core::StableDiscriminant for FailureMode {
    fn discriminant(&self) -> u16 {
        *self as u16
    }
}

impl FailureMode {
    /// Whether the mode destroys the part. §3.4 restricts dormant
    /// (unobserved) failures to non-fatal modes — this is the filter
    /// the reliability resolver applies.
    pub fn is_fatal(self) -> bool {
        matches!(self, Self::Explosion)
    }
}

/// §3.4 `ReliabilityDef` verbatim: the def-side half of the hazard
/// model (`PartUsage` is the live half and serializes per instance —
/// it is not part data).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReliabilityDef {
    /// Base mean-time-between-failures (s) — `λ_base = 1/base_mtbf_s`
    /// in `λ(t) = λ_base · Π(stress_i) · qual_factor(family)`.
    pub base_mtbf_s: f64,
    /// Extra hazard fraction for a fresh part, decaying with early
    /// operating time ∈ [0, 1) (validator).
    pub infant_mortality: f64,
    /// `qual_factor` curve over the family's accumulated qualification
    /// units.
    pub qualification_curve: QualCurve,
    /// Stress rules (§3.4): regime metric → `λ` multiplier.
    pub regime_stress: Vec<StressRule>,
    /// Hazard multiplier while dormant (§3.4 default 0.02 — "nonzero,
    /// small"). `0` in the def is legal but unusual; the balance-file
    /// `dormant_stress = 0` switch disables dormant hazard globally.
    /// Validator bounds to [0, 1].
    pub dormant_stress: f64,
    /// `(mode, weight)` — the draw distribution over failure modes when
    /// `Λ` crosses the part's threshold. Weights positive, at least
    /// one entry (validator).
    pub failure_modes: Vec<(FailureMode, f64)>,
}

/// `data/parts/<id>.ron` — one part definition (§3.1). Field order
/// matches the plan's sketch so the file reads top-down: identity,
/// economics, shape, scaling, physics, functionality, reliability,
/// gating.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PartDef {
    /// First field of every data file (§3.1). Must equal
    /// [`PART_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Stable part ID (`csp.engine.sparc`) — also the file's stem, so
    /// the validator can check `id` ↔ filename agreement.
    pub id: PartId,
    /// Upgrade/qualification family (§3.3 `UpgradeTrack.family`,
    /// §3.4 `FamilyQualification`). Parts in a family share tracks.
    pub family: FamilyId,
    /// Loc key for the display name.
    pub display_name: LocKey,
    /// Browser category — also the validator's module-consistency key.
    pub category: PartCategory,
    /// Dry mass at `scale = 1` (kg).
    pub mass_dry_kg: f64,
    /// Funds cost at `scale = 1`, stock variant.
    pub cost_funds: f64,
    /// Bounding box at `scale = 1` (m) — the editor's placement and the
    /// voxelizer's seed extent. `+z` is the stack axis.
    pub size_m: DVec3,
    /// Manufacture bill of materials `(resource, units)` — §3.1
    /// `build_resources`; consumed from the launch-site inventory (§3.5).
    pub build_resources: Vec<(ResourceId, f64)>,
    /// Snap-graph attachment points (see [`AttachNode`]).
    pub attach_nodes: Vec<AttachNode>,
    /// Editor scaling rule.
    pub scale: ScaleRule,
    /// Collider source geometry.
    pub collider: ColliderDef,
    /// Aero curves + reference length.
    pub aero: AeroDef,
    /// Thermal lump parameters.
    pub thermal: ThermalDef,
    /// Structural limits.
    pub structural: StructuralDef,
    /// Authored variants (see [`PartVariant`] — non-empty, unique IDs).
    pub variants: Vec<PartVariant>,
    /// Functional modules (see [`ModuleDef`]).
    pub modules: Vec<ModuleDef>,
    /// §3.4 reliability fields.
    pub reliability: ReliabilityDef,
    /// Tech node that unlocks the part (§3.3). The id must be
    /// well-formed; *resolution* against `data/techtree/*.ron` lands
    /// with the tree loader — the §3.3 rule "every part reachable" runs
    /// there, where the full set exists.
    pub tech_requirement: TechNodeId,
}

impl StatPath {
    /// The authored base value this path reads on `def` — `None` when
    /// the def does not carry the stat (a `ThrustVacN` on a part with
    /// no `Engine` module), which is also the §3.1 "the path resolves
    /// on this def" predicate: `base_value(def).is_some()`.
    ///
    /// Consumers: the resolver's stat-path reachability gate
    /// (`csp_phys::resolve`), the tech-tree validator ("every part in a
    /// tracked family resolves the track's `stat_path`"), and the
    /// manufacture-time composition of `Delta` upgrade levels —
    /// `(base + Σ deltas) / base` is evaluated against exactly this
    /// authored base.
    ///
    /// Convention for module stats: the *first* matching module's
    /// authored field. The part validator forbids multiple `Engine`
    /// modules, so engine paths are unambiguous; a part with several
    /// `Control` modules takes the first's `torque_nm` as its delta
    /// base (documented — deltas are an authored convention, and the
    /// family-stat validator's coverage rule is what keeps them sane).
    pub fn base_value(self, def: &PartDef) -> Option<f64> {
        match self {
            Self::MassDryKg => Some(def.mass_dry_kg),
            Self::CostFunds => Some(def.cost_funds),
            Self::BaseMtbfS => Some(def.reliability.base_mtbf_s),
            Self::ThrustVacN | Self::IspVacS | Self::IspSlS | Self::RatedBurnS => {
                def.modules.iter().find_map(|m| match m {
                    ModuleDef::Engine {
                        thrust_vac_n,
                        isp_vac_s,
                        isp_sl_s,
                        rated_burn_s,
                        ..
                    } => Some(match self {
                        Self::ThrustVacN => *thrust_vac_n,
                        Self::IspVacS => *isp_vac_s,
                        Self::IspSlS => *isp_sl_s,
                        _ => *rated_burn_s,
                    }),
                    _ => None,
                })
            }
            Self::ControlTorqueNm => def.modules.iter().find_map(|m| match m {
                ModuleDef::Control { torque_nm, .. } => Some(*torque_nm),
                _ => None,
            }),
        }
    }
}
