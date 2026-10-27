//! The §3.1 effective-stat pipeline: `BuiltPart` → `EffectivePart`.
//!
//! §3.1 defines it as **two functions, not one** —
//! `EffectivePart = resolve(BuiltPart) ∘ wear(PartUsage)` — because the
//! inputs have different lifetimes. `part_id`/`scale`/`variant`/upgrade
//! levels are **frozen at manufacture** and serialized in the
//! [`BuiltPart`] record (§7.2). Two inputs are **live**, handed to the
//! resolver like [`ResourceFile`] is — resolver inputs, never
//! `BuiltPart` fields: wear is live per-instance state ([`PartUsage`],
//! also §7.2-serialized), and qualification is live *per-family* state
//! ([`FamilyQualification`], §7.2's "per family, per save") — a
//! bench-tested or flight-proven family must improve the hazard of
//! units built *before* the test (§3.4: `qual_factor(family)` reads the
//! family's current units, so a flying craft's reliability re-resolves
//! when its family's record moves). [`resolve_built_part`] is the
//! frozen half, [`apply_wear`] the live half, and [`resolve_part`] the
//! composition every call site uses — one resolver for the editor, the
//! flight sim, the design AI, and contract validation (§3.1: "one
//! resolver, one place").
//!
//! # Naming reconciliation (M3-A validator's note)
//!
//! The plan uses one name for two things: §3.1's `EffectivePart` is the
//! resolved craft part; §2.4's `weld(part_graph, &[EffectivePart])`
//! names the same word for the physical subset the weld consumes. The
//! code keeps the §3.1 meaning on [`EffectivePart`] (this module — the
//! domain record the resolver produces) and names the weld-consumed
//! projection [`crate::WeldPart`]: `weld(graph, &[WeldPart])`. The
//! resolved part embeds its weld view verbatim at
//! [`EffectivePart::weld_input`] — no conversion math, no second
//! source of truth: the weld input is a field of the resolved record.
//!
//! # Resolution order (deterministic, fixed)
//!
//! 1. **Def identity** — `def.id` must equal `built.part_id`.
//! 2. **Scale legality** — `built.scale` must satisfy
//!    [`ScaleRule`] (`1.0` for `None`, a listed factor for `Discrete`,
//!    inside `[min, max]` for `Continuous`).
//! 3. **Variant** — `built.variant` must exist on the def.
//! 4. **Stat-path reachability** — every [`StatPath`] a variant
//!    multiplier or frozen upgrade entry touches must resolve on this
//!    def, else [`ResolveError::UnresolvableStatPath`].
//! 5. **Stat resolution, in order**: linear scale `s` on geometry;
//!    `s^e` per [`ScaleExponents`] on mass/cost/thrust/capacity/
//!    structural; variant stat multipliers; frozen upgrade
//!    multipliers (`upgrade_levels_at_build`). Multiplicative terms
//!    commute, but the evaluation order is fixed so the audit trail
//!    matches the doc: **scale → variant → upgrades**.
//! 6. **Weld view** — total mass = scaled·multiplied dry mass plus
//!    propellant (`Σ capacity · mass_per_unit_kg` via `resources.ron`);
//!    CoM and inertia derive from the resolved collider; one
//!    `ForceSiteKind::Thrust` site per resolved `Engine` module at the
//!    −z face pointing +z, `rated_n` = resolved `thrust_vac_n`.
//! 7. **Qualification** — `qual_factor` is the def's
//!    `qualification_curve` evaluated at the family's **live**
//!    `flight_units + bench_units` from the caller's
//!    [`FamilyQualification`] record. This is deliberately *not*
//!    frozen: §3.4 stores `FamilyQualification` per family per save,
//!    and a bench test or flight hour banked after manufacture must
//!    reach already-built units — re-resolve and the part's hazard
//!    improves. (What §3.3 freezes is *upgrades*: a later tech-tree
//!    level never retro-actively applies to a built part.)
//! 8. **Wear** — `hazard × (1 + operating_s / base_mtbf_s)`, the
//!    documented v1 stub. §3.4's real accumulation — `Π(stress_i)`
//!    over live regime, infant-mortality decay, dormant accrual, the
//!    `Λ` integral and threshold draw — lands with the reliability
//!    loop (a later milestone); the [`PartUsage`] input is plumbed now
//!    so that signature does not move.
//!
//! # Conventions the numbers rely on
//!
//! - **`Discrete` scale scales geometry only.** `ScaleRule::Discrete`
//!   authors a menu of sizes but carries no [`ScaleExponents`], so v1
//!   applies `s` to collider, attach nodes, `size_m`, and `ref_len_m`
//!   and leaves authored stats untouched. Parts whose stats must scale
//!   author `Continuous` instead. This is a schema property, not a
//!   resolver choice — flagged so nobody "fixes" it in code. To keep
//!   the silent non-scaling out of authored data, the `csp-data`
//!   validator forbids `Tank`/`Engine` modules on `Discrete` parts —
//!   those are the modules whose stats a size menu would obviously be
//!   expected to scale.
//! - **`build_resources` scales with `s^mass`, not `s^cost`.** The
//!   bill of materials is a mass-like quantity — a scaled-up tank's
//!   BOM is its scaled-up dry mass in feedstock terms, and `s^cost`
//!   would couple material consumption to the *funds* exponent
//!   authored for pricing. `ScaleExponents::mass` is the closest
//!   honest channel the v1 schema has; a dedicated `bom` exponent is
//!   a schema question deferred with the rest of the gap list below.
//! - **No exponent covers `Control.torque_nm` or
//!   `thermal.conduction_w_per_k`.** [`ScaleExponents`] has exactly
//!   five fields and neither stat has a channel — so in v1 both are
//!   deliberately unscaled (torque still takes its [`StatPath`]
//!   multiplier). Physical answers differ (`torque ∝ wheel mass`?
//!   `conduction ∝ contact area`?); which exponent — or new exponent
//!   field — each wants is a schema-version decision, deferred to the
//!   module-model milestone. Documented, not silently arbitrary.
//! - **The wear stub double-counts an MTBF upgrade, deliberately.**
//!   `wear_multiplier` normalizes `operating_s` by the *resolved*
//!   `base_mtbf_s` (post-upgrade), so a `BaseMtbfS` upgrade both
//!   lowers `λ_base` and slows wear accrual — the upgrade's benefit
//!   is applied twice. Normalizing by the resolved value is the
//!   self-consistent choice ("operating time as a fraction of the
//!   part's actual design life"), and the §3.4 accumulation loop
//!   replaces the whole stub before it can mislead balance work —
//!   flagged here so nobody "fixes" the apparent dip by reverting to
//!   an extra serialized field.
//! - **`built[i]` binds to the graph node with `effective_index == i`.**
//!   Placement lives in the [`crate::PartGraph`] (transform + parent
//!   edge); the `BuiltPart` slice's *position* is the single binding
//!   channel — the same convention `weld(graph, &[WeldPart])` uses,
//!   so a `resolve_parts` output feeds `weld` verbatim. An earlier
//!   draft carried a redundant `node` field on [`BuiltPart`]; it was
//!   dropped because a third, unchecked binding channel can only
//!   disagree with the graph, never help it.
//! - **`data_set_hash` has no consumer at M3.** It is plumbed —
//!   recorded on [`BuiltPart`], carried onto [`EffectivePart`] — so
//!   the serialized record is complete, but nothing yet compares it:
//!   the §7.7 drift warning (load-time `data_set_hash()` vs the
//!   frozen value) is a **tracked future item**, not forgotten.
//! - **`scale` matches authored factors exactly.** A `Discrete`
//!   membership test is `f64` equality: the editor picks from the
//!   authored menu, so the built record stores a listed factor
//!   verbatim and bit-equality is by construction, not luck.
//! - **Tanks resolve full.** `BuiltPart` has no contents field: a
//!   manufactured craft launches with full tanks, so `mass_kg` is
//!   dry + full propellant. Runtime contents (the §2.4 1/1024
//!   quantization and its re-weld trigger) are per-instance
//!   inventory state that lands with tank draining — callers will
//!   rebuild `EffectivePart`s from updated inputs then, keeping this
//!   signature stable.
//! - **`Cylinder` colliders resolve to inscribed octagonal prisms**
//!   (16 vertices: 8 directions on the authored circle at ±h). The
//!   compound collider is convex-hulled at weld anyway; 8 facets keep
//!   the parry materialization cheap. The prism's vertices lie *on*
//!   the cylinder — the hull under-covers by <8% of area at facet
//!   midpoints, which is the documented approximation.
//! - **`Convex` inertia is a point-cloud approximation**: `m/n`
//!   concentrated at each vertex about their centroid. Exact for
//!   vertex-symmetric hulls, honest elsewhere; a polyhedral integrator
//!   can replace it without touching the pipeline.
//! - **CoM is the collider centroid.** Uniform density: primitives
//!   resolve exactly `0` (they are authored centered on the origin);
//!   `Convex` takes the vertex mean.
//! - **Thermal scaling is partial by physics**: `heat_capacity_j_per_k`
//!   is `mass·cp` and scales with `s^mass`; `max_temp_k`/`emissivity`
//!   are intensive and do not scale; `conduction_w_per_k` is
//!   unscaled in v1 (the contact-area model is the thermal
//!   milestone's call).
//!
//! # Determinism
//!
//! Pure function: no I/O, no clock, no RNG, `BTreeMap`-ordered
//! multiplier application, `libm::pow` for `s^e`, fixed summation
//! order. Bit-identical for bit-identical inputs; resolving a
//! permuted `&[BuiltPart]` yields the same per-part results
//! (resolution is independent per part) — the tests assert both.
//!
//! # Errors
//!
//! Every failure is a typed [`ResolveError`] mapping to
//! `SimError::SaveBlocking` (§2.13): a craft whose parts cannot
//! resolve is unlaunchable and the save naming it is not persistable.
//! Two variants — [`ResolveError::PartIdMismatch`] and
//! [`ResolveError::UsageCountMismatch`] — are strictly *caller*
//! invariants (a def-lookup bug, a parallel-array bug), which is
//! `SimError::Fatal` territory on paper. They stay `SaveBlocking`
//! deliberately: at this boundary the resolver cannot distinguish a
//! caller bug from corrupt serialized state, and `SaveBlocking` is
//! the strictly-safer failure (the save is refused, the sim keeps
//! running) — a documented conservative mapping, not an oversight.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use csp_core::{DVec3, SimError};
use csp_data::{
    AttachNode, ColliderDef, FailureMode, FamilyId, LoadedParts, ModuleDef, PartDef, PartId,
    ResourceFile, ResourceId, ScaleRule, StatPath, StressKind, StressRule, VariantId,
};

use crate::part::{ColliderShape, ForceSite, ForceSiteKind, WeldPart};
use crate::tensor::{Axis, SymMat3};

/// Slope of the v1 wear→hazard stub: a part's baseline hazard grows by
/// `operating_s / base_mtbf_s` — one full MTBF of operating time
/// doubles the unstressed hazard.
///
/// **M9b status — superseded for failure decisions.** The §3.4 model
/// landed in `csp-sim`'s reliability module: the authoritative λ(t)
/// is evaluated per tick (in-bubble) or per closed-form publish
/// (dormant) from the live `PartReliability` record — pre-drawn
/// `θ`, regime-stress products, infant decay, dormant accrual — and
/// this stub never feeds it. `wear_multiplier`/`apply_wear`/`hazard_
/// per_s` remain only as resolution-time display projections (what
/// a readout can show for a fresh/aged instance); no sim path reads
/// them. Removing them is a follow-up cleanup once the readouts
/// switch to the `ReliabilitySpec` table.
pub const WEAR_HAZARD_STUB: f64 = 1.0;

/// The §3.1 serialized craft-instance record — "SERIALIZED per part
/// instance (§7.2)". Everything the resolver needs to reproduce a
/// part's *manufacture-time* numbers lives here: nothing in this
/// record may read live tech state, which is the mechanism behind
/// §3.3's "upgrades never apply retroactively to craft already in
/// flight, including across a reload".
///
/// `PartUsage` (the live wear half) and `FamilyQualification` (the
/// live qualification half) are deliberately separate records: this
/// struct is write-once at manufacture while usage mutates every tick
/// and the family's qualification record mutates per test/flight —
/// §7.2 serializes all three as distinct per-instance / per-family
/// fields. The record carries **no graph binding**: `built[i]` is the
/// instance of the node whose `effective_index == i` — the same
/// positional convention `weld(graph, &[WeldPart])` uses — so the
/// `PartGraph` remains the single source of placement truth and no
/// second channel can disagree with it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BuiltPart {
    /// Which `PartDef` it was built from (§3.1 stable string ID).
    pub part_id: PartId,
    /// Linear scale factor frozen at manufacture (must satisfy the
    /// def's [`ScaleRule`] at resolve time).
    pub scale: f64,
    /// Stack-axis ("length") multiplier frozen at manufacture —
    /// `1.0` is unstretched. Geometry along the part's local +z axis
    /// is `scale · stretch`; cross-sections are `scale` alone (the
    /// VAB's per-part length control — the difference between a
    /// bigger tank and a *longer* tank). Stat scaling treats the
    /// authored [`ScaleExponents`] as volume-order exponents: the
    /// uniform `s^e` covers the two cross-section powers, and stretch
    /// contributes the remainder `t^max(e−2, 0)` — a capacity/mass
    /// exponent of 3 (volume) makes a `Continuous` tank's contents
    /// scale linearly with length. Gated by the same [`ScaleRule`]
    /// bounds as `scale` at resolve time (`None` forces `1.0`,
    /// `Discrete` bounds to the authored menu's min/max, `Continuous`
    /// to its authored range).
    pub stretch: f64,
    /// Which authored variant it was built as.
    pub variant: VariantId,
    /// §3.3 upgrade tracks frozen at manufacture:
    /// `stat_path → TrackLevels { levels, multiplier }`.
    ///
    /// Deviation from the §3.1 sketch (`BTreeMap<FamilyId,
    /// TrackLevels>`), documented: the sketch keys on family, but the
    /// `UpgradeTrack` schema that landed with the tree keys on
    /// `stat_path` — and the map stores the *composed* effect, not a
    /// lookup reference. [`compose_upgrade_levels`] folds the
    /// family's purchased track levels into this map at manufacture
    /// **keeping both the level count and the composed effect**:
    /// `levels` preserves the track position for UI/save-migration,
    /// and `multiplier` folds whatever §3.3's `multiplier_or_delta`
    /// authored — including *additive* deltas a bare
    /// `Π`-of-multipliers map could not represent. Storing the
    /// composed effect makes the record **self-contained** —
    /// re-derivation on load needs the part def only, never the tree,
    /// so a rebalanced or deleted track can never alter an in-flight
    /// part.
    pub upgrade_levels_at_build: BTreeMap<StatPath, TrackLevels>,
    /// `data_set_hash` (§3.1) at manufacture — "what the numbers meant
    /// when it was built", the §7.7 drift-warning key. Nothing consumes
    /// it at M3 — the load-time comparison against the current
    /// `csp_data::data_set_hash()` is a tracked future item (the module
    /// doc's convention list), plumbed now so the §7.2 record is
    /// already complete when the check lands.
    pub data_set_hash: [u8; 32],
}

impl BuiltPart {
    /// A stock build: scale 1, stretch 1, the `variant` named, no
    /// upgrades, **zeroed data-set hash**. Convenience for tests and
    /// early call sites — the caller stamps the hash at the session
    /// boundary: the VAB's `stock_build` fills it from the installed
    /// catalog hash, and `csp_sim`'s `SimState::build_part` takes the
    /// stamp as an argument. A `stock()` part placed without a
    /// stamping wrapper keeps the zero sentinel (nothing consumes the
    /// field yet — see the field doc).
    pub fn stock(part_id: PartId, variant: VariantId) -> Self {
        Self {
            part_id,
            scale: 1.0,
            stretch: 1.0,
            variant,
            upgrade_levels_at_build: BTreeMap::new(),
            data_set_hash: [0; 32],
        }
    }
}

/// One frozen upgrade-track entry in [`BuiltPart::upgrade_levels_at_build`]
/// — the level count *and* the composed effect it produced, so the
/// record keeps §3.3's level information while staying resolvable
/// without the tech tree (see the field's doc).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TrackLevels {
    /// Track levels purchased at manufacture — informational; the
    /// resolver reads only `multiplier` (the track position is kept
    /// for the editor's "what did I buy" view and future migration).
    pub levels: u8,
    /// The stat multiplier those levels composed to at manufacture.
    /// §3.3 authors `multiplier_or_delta` per level: for multiplier
    /// tracks this is the product of level multipliers; for delta
    /// tracks it is `(base + Σ deltas) / base` evaluated against the
    /// manufacture-time base — either way the resolver multiplies it
    /// on like any variant multiplier. Must be finite and `> 0`.
    pub multiplier: f64,
}

/// §3.4 `FamilyQualification` verbatim — the per-family, per-save
/// qualification record (§7.2-serialized). It is **live** state, a
/// resolver *input* like [`ResourceFile`], never a `BuiltPart` field:
/// bench tests and flight accrual raise `flight_units`/`bench_units`
/// while a craft is in flight, and §3.4 intends that improvement to
/// reach units built earlier — `qual_factor(family)` is evaluated on
/// the family's *current* units at each resolve. A record absent from
/// the map a caller hands [`resolve_parts`] is the unqualified state
/// ([`FamilyQualification::FRESH`], first knot of the curve).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FamilyQualification {
    /// Accumulated flight-test units (§3.4 — weighted by regime
    /// coverage when the qualification program lands).
    pub flight_units: f64,
    /// Accumulated test-stand units (§3.4 bench testing).
    pub bench_units: f64,
    /// Failures observed in this family — §3.4 "failures add
    /// qualification data" (counted, not yet consumed by the curve).
    pub observed_failures: u32,
}

impl FamilyQualification {
    /// No qualification data — resolves `qual_factor` at zero units.
    pub const FRESH: Self = Self {
        flight_units: 0.0,
        bench_units: 0.0,
        observed_failures: 0,
    };

    /// `flight_units + bench_units` — the abscissa
    /// `ReliabilityDef::qualification_curve` is evaluated at.
    pub fn units(&self) -> f64 {
        self.flight_units + self.bench_units
    }

    /// Both unit accumulators finite and non-negative — the
    /// resolver's input gate on the live record.
    pub fn is_valid(&self) -> bool {
        self.flight_units >= 0.0
            && self.bench_units >= 0.0
            && self.flight_units.is_finite()
            && self.bench_units.is_finite()
    }
}

/// The §7.2 qualification table: `FamilyId` → its live
/// [`FamilyQualification`] record ("per family, per save", §3.4).
/// `BTreeMap`, never `HashMap` — deterministic lookup order (§2.2).
pub type FamilyQualifications = BTreeMap<FamilyId, FamilyQualification>;

/// §3.4 `PartUsage` verbatim — the per-instance wear record,
/// serialized (§7.2). Every field is a monotone accumulator the §3.4
/// runtime loop owns; a fresh part is all zeros
/// ([`PartUsage::FRESH`]). The resolver reads `operating_s` for the
/// wear stub and validates the rest so corrupt serialized state fails
/// typed instead of silently feeding the hazard math.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PartUsage {
    /// Accumulated operating (powered/stressed) time, s.
    pub operating_s: f64,
    /// Accumulated dormant time, s.
    pub dormant_s: f64,
    /// Lifetime ignition count.
    pub ignitions: u32,
    /// Lifetime gimbal actuation cycles.
    pub gimbal_cycles: u32,
    /// Peak temperature seen, K.
    pub max_temp_k: f64,
    /// Peak axial load seen, N.
    pub max_axial_n: f64,
    /// Peak acceleration load seen, g.
    pub max_g: f64,
    /// `Λ` — the integrated hazard the §3.4 threshold `θ` trips on.
    pub hazard_integral: f64,
}

impl PartUsage {
    /// A fresh part's record — all accumulators zero.
    pub const FRESH: Self = Self {
        operating_s: 0.0,
        dormant_s: 0.0,
        ignitions: 0,
        gimbal_cycles: 0,
        max_temp_k: 0.0,
        max_axial_n: 0.0,
        max_g: 0.0,
        hazard_integral: 0.0,
    };

    /// All fields finite and non-negative — the resolver's input gate,
    /// public so callers can uphold [`wear_multiplier`]'s stated
    /// precondition before handing a record in.
    pub fn is_valid(&self) -> bool {
        self.operating_s >= 0.0
            && self.dormant_s >= 0.0
            && self.max_temp_k >= 0.0
            && self.max_axial_n >= 0.0
            && self.max_g >= 0.0
            && self.hazard_integral >= 0.0
            && self.operating_s.is_finite()
            && self.dormant_s.is_finite()
            && self.max_temp_k.is_finite()
            && self.max_axial_n.is_finite()
            && self.max_g.is_finite()
            && self.hazard_integral.is_finite()
    }
}

/// The reliability block after resolution — §3.4's
/// `λ(t) = λ_base · Π(stress_i) · qual_factor(family)` with everything
/// the resolver owns evaluated: `λ_base` post-variant, `qual_factor`
/// at the family's *live* qualification units, and the wear stub
/// applied. The `Π(stress_i)` product is *runtime* regime state — the
/// authored rules pass through verbatim for the sim and the editor to
/// display (§3.4: "hidden stress multipliers are bad design").
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResolvedReliability {
    /// `base_mtbf_s` after [`StatPath::BaseMtbfS`] multipliers (s).
    pub base_mtbf_s: f64,
    /// `λ_base = 1 / base_mtbf_s` (1/s).
    pub base_hazard_per_s: f64,
    /// Authored infant-mortality fraction, carried for the §3.4
    /// runtime model (fresh-part decay is *not* applied by the wear
    /// stub — documented there) and for display.
    pub infant_mortality: f64,
    /// `qual_factor(family)` at the live
    /// [`FamilyQualification::units`] handed in — ∈ (0, 1].
    pub qual_factor: f64,
    /// The `wear(PartUsage)` multiplier — `1.0` on the fresh path.
    pub wear_multiplier: f64,
    /// Resolved baseline hazard: `λ_base · qual_factor ·
    /// wear_multiplier` — the unstressed rate the editor shows and
    /// the §3.4 loop stresses further.
    pub hazard_per_s: f64,
    /// `λ_base · dormant_stress` — the §3.4 dormant accrual rate
    /// (verbatim formula: dormant hazard keys on `λ_base`, not the
    /// stressed rate).
    pub dormant_hazard_per_s: f64,
    /// Authored stress rules (resolved passthrough).
    pub regime_stress: Vec<StressRule>,
    /// Authored dormant multiplier.
    pub dormant_stress: f64,
    /// Authored failure-mode distribution.
    pub failure_modes: Vec<(FailureMode, f64)>,
    /// Authored `qual_factor` curve (resolved passthrough) — the
    /// point-evaluated [`Self::qual_factor`] is the live-units
    /// reading at resolve time; the curve itself travels with the
    /// resolved part so §3.4's `qual_factor(family)` can re-evaluate
    /// on the family's *live* units (the module doc's step 7).
    pub qualification_curve: csp_data::QualCurve,
}

/// §3.1's `EffectivePart` — the resolved craft part the whole game
/// runs on: manufacture-frozen stats with the weld input embedded.
///
/// Produced only by [`resolve_part`]/[`resolve_parts`] (which is why
/// construction guarantees hold: every emitted numeric family is
/// gated at the stage that produces it — `resolve_stats`,
/// `resolve_envelopes`, `resolve_weld_input`, `resolve_reliability` —
/// so every number is finite, `mass_kg > 0`, and stat paths
/// resolved). Serialized for craft-file and tooling
/// symmetry with [`WeldPart`]; the §7.2 record of truth remains the
/// [`BuiltPart`] this was resolved from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectivePart {
    /// Provenance: which def produced it.
    pub part_id: PartId,
    /// The upgrade/qualification family (§3.3/§3.4 key).
    pub family: FamilyId,
    /// The variant applied.
    pub variant: VariantId,
    /// The scale factor applied.
    pub scale: f64,
    /// The stack-axis stretch factor applied ([`BuiltPart::stretch`]).
    pub stretch: f64,
    /// `data_set_hash` carried from the [`BuiltPart`] — the §7.7
    /// drift-warning key, so a resolved part still reports what data
    /// produced it.
    pub data_set_hash: [u8; 32],
    /// The weld-consumed physical view — handed to
    /// `weld(graph, &[WeldPart])` verbatim.
    pub weld_input: WeldPart,
    /// Resolved funds cost (`s^cost · variant.cost_multiplier ·
    /// upgrades`).
    pub cost_funds: f64,
    /// Resolved build bill of materials (`amount · s^mass` — BOM is
    /// mass-like; see the module doc's convention note).
    pub build_resources: Vec<(ResourceId, f64)>,
    /// Resolved bounding box (`size · s`), part-local.
    pub size_m: DVec3,
    /// Resolved attach nodes (position and diameter scaled by `s`;
    /// direction unchanged).
    pub attach_nodes: Vec<AttachNode>,
    /// Functional modules with resolution baked into the numbers —
    /// same enum as the def side, post-scale/post-multiplier values.
    /// (`Engine.thrust_vac_n` here is the resolved rating the force
    /// site's `rated_n` mirrors.)
    pub modules: Vec<ModuleDef>,
    /// Resolved structural limits (`× s^structural`).
    pub structural: csp_data::StructuralDef,
    /// Resolved thermal lump (see the module doc's scaling note).
    pub thermal: csp_data::ThermalDef,
    /// Resolved aero (`ref_len_m · s`; coefficient curves verbatim —
    /// they are dimensionless).
    pub aero: csp_data::AeroDef,
    /// Resolved reliability block.
    pub reliability: ResolvedReliability,
}

/// Why a `BuiltPart` cannot resolve — the typed boundary between bad
/// craft/save data and the pure function (§2.13). Every variant maps
/// to `SimError::SaveBlocking`: a part that cannot resolve makes its
/// craft unlaunchable and the save naming it unpersistable.
#[derive(Clone, Debug, PartialEq)]
pub enum ResolveError {
    /// `resolve_parts` found no def for `built.part_id` — the save or
    /// craft file names a part the data set does not contain.
    UnknownPartId {
        /// The missing ID.
        part_id: PartId,
        /// The record's position in the `built` slice — which is its
        /// `effective_index` in the graph convention (module doc), so
        /// the graph node it failed on is recoverable.
        index: usize,
    },
    /// Two defs in the `LoadedParts` set share one `PartId` — the
    /// `BTreeMap` lookup index would silently keep the last, so a
    /// `built` record naming it resolves against whichever def
    /// happened to sort later: ambiguous authority is a data-set
    /// defect, refused. The set-level validator forbids duplicate
    /// ids; this is the resolver's defence-in-depth mirror for a set
    /// that never passed it.
    DuplicatePartId {
        /// The id claimed by two defs.
        part_id: PartId,
    },
    /// `resolve_part` was handed a def whose `id` disagrees with
    /// `built.part_id` — caller-side lookup bug, reported typed
    /// rather than silently resolving the wrong def. (A caller
    /// invariant, mapped `SaveBlocking` anyway — see the module doc's
    /// errors note.)
    PartIdMismatch {
        /// The def handed in.
        def: PartId,
        /// The ID the record names.
        built: PartId,
    },
    /// `built.variant` is not among `def.variants`.
    UnknownVariant {
        /// The part being resolved.
        part_id: PartId,
        /// The unknown variant ID.
        variant: VariantId,
    },
    /// `built.scale` is non-finite, non-positive, violates the def's
    /// `ScaleRule`, or is not `1.0` on an unscalable part.
    InvalidScale {
        /// The part being resolved.
        part_id: PartId,
        /// The offending factor.
        scale: f64,
        /// Which constraint failed (static diagnostic text).
        rule: &'static str,
    },
    /// A variant or frozen upgrade entry touches a [`StatPath`] this
    /// def does not carry (e.g. `IspVacS` on a part with no `Engine`
    /// module). Every path must resolve — §3.1's resolver contract.
    UnresolvableStatPath {
        /// The part being resolved.
        part_id: PartId,
        /// The unreachable path.
        stat: StatPath,
        /// Where the multiplier came from (`"variant"` or
        /// `"upgrade_levels_at_build"`).
        source: &'static str,
    },
    /// One variant names the same [`StatPath`] twice — the entries
    /// would silently multiply. The `csp-data` validator rejects this
    /// at authoring time; the resolver re-checks as defence in depth
    /// because a `PartDef` can reach it unvalidated (hand-built,
    /// serialized).
    DuplicateStatPath {
        /// The part being resolved.
        part_id: PartId,
        /// The duplicated path.
        stat: StatPath,
    },
    /// A `Tank` capacity or build-material names a resource absent
    /// from `resources.ron`.
    UnknownResource {
        /// The part being resolved.
        part_id: PartId,
        /// The missing resource.
        resource: ResourceId,
    },
    /// A resolved quantity came out non-finite or outside its
    /// physical bound (zero/negative mass or MTBF, negative cost or
    /// capacity, non-positive multiplier, degenerate collider).
    InvalidStat {
        /// The part being resolved.
        part_id: PartId,
        /// Which resolved field failed (static diagnostic text).
        field: &'static str,
    },
    /// A `PartUsage` input was non-finite or negative — corrupt
    /// serialized wear, not a def problem.
    InvalidUsage {
        /// The part being resolved.
        part_id: PartId,
        /// Which field failed.
        field: &'static str,
    },
    /// A [`FamilyQualification`] input was non-finite or negative —
    /// corrupt serialized qualification state, not a def problem.
    InvalidQualification {
        /// The part being resolved.
        part_id: PartId,
        /// Which field failed.
        field: &'static str,
    },
    /// `resolve_parts` was given a `usages` slice whose length differs
    /// from `built` — the parallel-array contract. (A caller
    /// invariant, mapped `SaveBlocking` anyway — see the module doc's
    /// errors note.)
    UsageCountMismatch {
        /// `built.len()`.
        built: usize,
        /// `usages.len()`.
        usages: usize,
    },
}

impl ResolveError {
    /// Static reason text for the `SimError` mapping — logs and bug
    /// bundles only, never hashed (§2.13).
    pub fn reason(&self) -> &'static str {
        match self {
            Self::UnknownPartId { .. } => "resolve: unknown part id",
            Self::DuplicatePartId { .. } => "resolve: two defs share one part id",
            Self::PartIdMismatch { .. } => "resolve: def id does not match built part id",
            Self::UnknownVariant { .. } => "resolve: unknown variant id",
            Self::InvalidScale { rule, .. } => rule,
            Self::UnresolvableStatPath { .. } => "resolve: stat path does not exist on this part",
            Self::DuplicateStatPath { .. } => "resolve: variant names the same stat path twice",
            Self::UnknownResource { .. } => "resolve: unknown resource id",
            Self::InvalidStat { field, .. } => field,
            Self::InvalidUsage { field, .. } => field,
            Self::InvalidQualification { field, .. } => field,
            Self::UsageCountMismatch { .. } => {
                "resolve: usage slice length does not match built parts"
            }
        }
    }
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownPartId { part_id, index } => {
                write!(
                    f,
                    "resolve: no def for part id {part_id:?} (built[{index}])"
                )
            }
            Self::DuplicatePartId { part_id } => {
                write!(f, "resolve: two defs claim part id {part_id:?}")
            }
            Self::PartIdMismatch { def, built } => {
                write!(f, "resolve: def {def:?} handed for built part {built:?}")
            }
            Self::UnknownVariant { part_id, variant } => {
                write!(f, "resolve: part {part_id:?} has no variant {variant:?}")
            }
            Self::InvalidScale {
                part_id,
                scale,
                rule,
            } => write!(f, "resolve: {part_id:?} scale {scale} — {rule}"),
            Self::UnresolvableStatPath {
                part_id,
                stat,
                source,
            } => write!(
                f,
                "resolve: {part_id:?} {source} touches {} which the def lacks",
                stat.name()
            ),
            Self::DuplicateStatPath { part_id, stat } => {
                write!(
                    f,
                    "resolve: {part_id:?} variant multiplies {} twice",
                    stat.name()
                )
            }
            Self::UnknownResource { part_id, resource } => {
                write!(
                    f,
                    "resolve: {part_id:?} references unknown resource {resource:?}"
                )
            }
            Self::InvalidStat { part_id, field } => write!(f, "resolve: {part_id:?}: {field}"),
            Self::InvalidUsage { part_id, field } => {
                write!(f, "resolve: {part_id:?} usage: {field}")
            }
            Self::InvalidQualification { part_id, field } => {
                write!(f, "resolve: {part_id:?} family qualification: {field}")
            }
            Self::UsageCountMismatch { built, usages } => {
                write!(f, "resolve: {built} built parts vs {usages} usages")
            }
        }
    }
}

impl std::error::Error for ResolveError {}

impl From<ResolveError> for SimError {
    fn from(e: ResolveError) -> Self {
        SimError::SaveBlocking(e.reason())
    }
}

/// Where a multiplier term came from — diagnostic channel for
/// [`ResolveError::UnresolvableStatPath`].
const SOURCE_VARIANT: &str = "variant";
const SOURCE_UPGRADE: &str = "upgrade_levels_at_build";

/// The resolved `s^e` factors for one scale step. `linear` is `s`
/// itself (cross-section geometry); `linear_z` is `s · t` — the
/// stack-axis geometry factor (`t` = [`BuiltPart::stretch`]). The
/// rest are per-[`ScaleExponents`] `s^e · t^max(e−2, 0)` evaluated
/// through `libm::pow` — the pinned `powf` stand-in, so
/// cross-platform bit-identity holds by construction.
struct ScaleFactors {
    linear: f64,
    linear_z: f64,
    mass: f64,
    cost: f64,
    thrust: f64,
    capacity: f64,
    structural: f64,
}

/// The stretch bounds a [`ScaleRule`] admits: `None` pins `1.0`
/// (a part that does not scale does not stretch either), `Discrete`
/// bounds to the authored menu's min/max (its stats stay fixed —
/// geometry-only rules stretch geometry-only, same convention as
/// `scale`), `Continuous` to its authored range.
fn stretch_bounds(rule: &ScaleRule) -> (f64, f64) {
    match rule {
        ScaleRule::None => (1.0, 1.0),
        ScaleRule::Discrete(set) => (
            set.first().copied().unwrap_or(1.0),
            set.last().copied().unwrap_or(1.0),
        ),
        ScaleRule::Continuous { min, max, .. } => (*min, *max),
    }
}

/// The per-stat exponent a stretch step applies: `t^max(e−2, 0)`.
/// `s^e` for a uniform scale covers volume as `e` powers of `s`;
/// the cross-section contributes two of them (scaled by `s`), so the
/// remaining `e−2` powers are the length axis's share (scaled by `t`
/// instead). An exponent under 2 is cross-section-dominated and sees
/// no length term — `t^0 = 1` exactly.
fn stretch_exp(e: Option<f64>) -> Option<f64> {
    e.map(|e| (e - 2.0).max(0.0))
}

/// Gate `built.scale`/`built.stretch` against the def's [`ScaleRule`]
/// and produce the per-stat factors. `ScaleRule::Discrete` authors no
/// exponents (see the module doc): stat factors are 1 there, only
/// `linear`/`linear_z` apply.
fn scale_factors(def: &PartDef, scale: f64, stretch: f64) -> Result<ScaleFactors, ResolveError> {
    let bad = |rule: &'static str| ResolveError::InvalidScale {
        part_id: def.id.clone(),
        scale,
        rule,
    };
    if !scale.is_finite() || scale <= 0.0 {
        return Err(bad("resolve: scale must be finite and positive"));
    }
    if !stretch.is_finite() || stretch <= 0.0 {
        return Err(bad("resolve: stretch must be finite and positive"));
    }
    let (t_lo, t_hi) = stretch_bounds(&def.scale);
    if stretch < t_lo || stretch > t_hi {
        return Err(bad("resolve: stretch outside the authored range"));
    }
    let exp = match &def.scale {
        ScaleRule::None => {
            if scale != 1.0 {
                return Err(bad("resolve: part does not scale (scale must be 1.0)"));
            }
            None
        }
        ScaleRule::Discrete(set) => {
            if !set.contains(&scale) {
                return Err(bad("resolve: scale is not one of the authored sizes"));
            }
            None
        }
        ScaleRule::Continuous {
            min,
            max,
            exponents,
        } => {
            if scale < *min || scale > *max {
                return Err(bad("resolve: scale outside the authored range"));
            }
            Some(exponents)
        }
    };
    Ok(ScaleFactors {
        linear: scale,
        linear_z: scale * stretch,
        mass: pow2(scale, exp.map(|e| e.mass)) * pow2(stretch, stretch_exp(exp.map(|e| e.mass))),
        cost: pow2(scale, exp.map(|e| e.cost)) * pow2(stretch, stretch_exp(exp.map(|e| e.cost))),
        thrust: pow2(scale, exp.map(|e| e.thrust))
            * pow2(stretch, stretch_exp(exp.map(|e| e.thrust))),
        capacity: pow2(scale, exp.map(|e| e.capacity))
            * pow2(stretch, stretch_exp(exp.map(|e| e.capacity))),
        structural: pow2(scale, exp.map(|e| e.structural))
            * pow2(stretch, stretch_exp(exp.map(|e| e.structural))),
    })
}

/// `s^e` when an exponent set exists, `1.0` otherwise (None/Discrete
/// rules scale geometry only). A `1.0` factor multiplies exactly —
/// unscaled stats keep their authored bits.
fn pow2(s: f64, e: Option<f64>) -> f64 {
    match e {
        Some(e) => libm::pow(s, e),
        None => 1.0,
    }
}

/// Does `path` name a scalar this def actually carries? Delegates to
/// [`StatPath::base_value`] — the reachability predicate and the
/// delta-track base read are the same def knowledge, kept on the data
/// side so the tech-tree validator checks against identical rules.
/// This is the §3.1 "every path must resolve or it's a typed error"
/// rule — the *check* half; the *apply* half is in
/// [`resolve_stats`]/[`resolve_module`].
fn stat_path_resolves(def: &PartDef, path: StatPath) -> bool {
    path.base_value(def).is_some()
}

/// The manufacture half of §3.3's "upgrades apply at manufacture":
/// compose a family's purchased [`UpgradeTrack`] levels into the
/// frozen `BuiltPart::upgrade_levels_at_build` map.
///
/// `track_levels` is `(track, purchased_level_count)` — what the
/// sim's `TechState` hands over for `def.family` (0-level tracks and
/// tracks of other families are skipped, so a caller may hand the
/// family's whole track list). For each applicable track the composed
/// effect freezes as one [`TrackLevels`] entry:
///
/// - `Multiplier` levels fold by product;
/// - `Delta` levels fold as `(base + Σ deltas) / base` evaluated
///   against the **authored** base ([`StatPath::base_value`] — the
///   level plays out multiplicatively on top of scale and variant
///   afterwards, so an additive upgrade scales with the part);
/// - deltas compose before multipliers, a fixed order.
///
/// A `level` beyond the track's authored length, a `stat_path` the
/// def cannot resolve, or a composed effect that is not finite and
/// positive is a typed error — the validator's track rules make all
/// three unreachable for validated data, and this gate is the
/// defence-in-depth mirror the resolver applies everywhere else. Two
/// tracks reaching the same `stat_path` merge multiplicatively (the
/// validator forbids the authoring, but the fold is harmless).
pub fn compose_upgrade_levels<'a>(
    def: &PartDef,
    track_levels: impl IntoIterator<Item = (&'a csp_data::UpgradeTrack, u8)>,
) -> Result<BTreeMap<StatPath, TrackLevels>, ResolveError> {
    let mut out: BTreeMap<StatPath, TrackLevels> = BTreeMap::new();
    for (track, level) in track_levels {
        if level == 0 || track.family != def.family {
            continue;
        }
        let n = usize::from(level);
        if n > track.levels.len() {
            return Err(ResolveError::InvalidStat {
                part_id: def.id.clone(),
                field: "compose: track level exceeds the authored level count",
            });
        }
        let Some(base) = track.stat_path.base_value(def) else {
            return Err(ResolveError::UnresolvableStatPath {
                part_id: def.id.clone(),
                stat: track.stat_path,
                source: "upgrade_track",
            });
        };
        let mut mult = 1.0f64;
        let mut delta = 0.0f64;
        for l in &track.levels[..n] {
            match l.multiplier_or_delta {
                csp_data::UpgradeEffect::Multiplier(m) => mult *= m,
                csp_data::UpgradeEffect::Delta(d) => delta += d,
            }
        }
        if delta != 0.0 {
            if !(base.is_finite() && base > 0.0) {
                return Err(ResolveError::InvalidStat {
                    part_id: def.id.clone(),
                    field: "compose: delta track base is not positive and finite",
                });
            }
            mult *= (base + delta) / base;
        }
        if !(mult.is_finite() && mult > 0.0) {
            return Err(ResolveError::InvalidStat {
                part_id: def.id.clone(),
                field: "compose: composed upgrade multiplier is not positive and finite",
            });
        }
        out.entry(track.stat_path)
            .and_modify(|tl| {
                tl.levels = tl.levels.saturating_add(level);
                tl.multiplier *= mult;
            })
            .or_insert(TrackLevels {
                levels: level,
                multiplier: mult,
            });
    }
    Ok(out)
}

/// Fold variant then `upgrade_levels_at_build` multipliers into one
/// `StatPath → product` map (BTreeMap: the application's multiplication
/// order is then ascending `StatPath`, fixed regardless of authored
/// order — and multiplicative terms commute anyway). Every referenced
/// path is reachability-checked first, reporting its source; a variant
/// naming the same path twice fails [`ResolveError::DuplicateStatPath`]
/// (the validator's rule, re-checked here — a `Vec` can carry the
/// duplicate the `BTreeMap` upgrade map structurally cannot).
fn fold_multipliers(
    def: &PartDef,
    variant_mults: &[(StatPath, f64)],
    upgrade_mults: &BTreeMap<StatPath, TrackLevels>,
) -> Result<BTreeMap<StatPath, f64>, ResolveError> {
    // Reachability + positivity gate first, each source named — a bad
    // multiplier never reaches the fold. The variant loop also watches
    // for a repeated StatPath (see the fn doc).
    let mut variant_paths = BTreeSet::new();
    for (path, m, source) in variant_mults
        .iter()
        .map(|(p, m)| (p, m, SOURCE_VARIANT))
        .chain(
            upgrade_mults
                .iter()
                .map(|(p, tl)| (p, &tl.multiplier, SOURCE_UPGRADE)),
        )
    {
        if !stat_path_resolves(def, *path) {
            return Err(ResolveError::UnresolvableStatPath {
                part_id: def.id.clone(),
                stat: *path,
                source,
            });
        }
        if !m.is_finite() || *m <= 0.0 {
            return Err(ResolveError::InvalidStat {
                part_id: def.id.clone(),
                field: "resolve: stat multiplier is not positive and finite",
            });
        }
        if source == SOURCE_VARIANT && !variant_paths.insert(*path) {
            return Err(ResolveError::DuplicateStatPath {
                part_id: def.id.clone(),
                stat: *path,
            });
        }
    }
    let mut out = BTreeMap::new();
    for (path, m) in variant_mults.iter() {
        *out.entry(*path).or_insert(1.0) *= *m;
    }
    for (path, tl) in upgrade_mults {
        *out.entry(*path).or_insert(1.0) *= tl.multiplier;
    }
    Ok(out)
}

/// The composed multiplier for `path` (1.0 when untouched).
fn mult(mults: &BTreeMap<StatPath, f64>, path: StatPath) -> f64 {
    mults.get(&path).copied().unwrap_or(1.0)
}

/// The resolved collider — `s` scales cross-sections, `sz` the stack
/// axis (`s · stretch`). `Cylinder` becomes the inscribed octagonal
/// prism documented in the module header; the 8 ring directions are
/// exact constants (`FRAC_1_SQRT_2` pairs) so no libm call sits on the
/// geometry path.
fn scale_collider(collider: &ColliderDef, s: f64, sz: f64) -> ColliderShape {
    const R: f64 = core::f64::consts::FRAC_1_SQRT_2;
    const RING: [(f64, f64); 8] = [
        (1.0, 0.0),
        (R, R),
        (0.0, 1.0),
        (-R, R),
        (-1.0, 0.0),
        (-R, -R),
        (0.0, -1.0),
        (R, -R),
    ];
    let st = |v: DVec3| DVec3::new(v.x * s, v.y * s, v.z * sz);
    match collider {
        ColliderDef::Box { half_extents_m } => ColliderShape::Box {
            half_extents: st(*half_extents_m),
        },
        ColliderDef::Cylinder {
            radius_m,
            half_height_m,
        } => {
            let (r, h) = (radius_m * s, half_height_m * sz);
            let mut pts = Vec::with_capacity(16);
            for &(x, y) in &RING {
                pts.push(DVec3::new(r * x, r * y, h));
            }
            for &(x, y) in &RING {
                pts.push(DVec3::new(r * x, r * y, -h));
            }
            ColliderShape::Points(pts)
        }
        ColliderDef::Convex { points } => {
            ColliderShape::Points(points.iter().map(|p| st(*p)).collect())
        }
    }
}

/// Lowest z over the collider's vertices — the −z face the thrust site
/// sits on (module doc: engines nozzle at `z_min`, thrust along +z,
/// the M2 fixture's convention).
fn collider_z_min(collider: &ColliderShape) -> Option<f64> {
    collider.vertices().iter().map(|v| v.z).reduce(f64::min)
}

/// Part-local CoM: the collider centroid under uniform density.
/// Primitives are authored centered on the origin (`DVec3::ZERO`
/// exactly); `Convex` takes the vertex mean in fixed order.
fn collider_com(collider: &ColliderDef) -> DVec3 {
    match collider {
        ColliderDef::Box { .. } | ColliderDef::Cylinder { .. } => DVec3::ZERO,
        ColliderDef::Convex { points } => {
            let mut c = DVec3::ZERO;
            for p in points {
                c = c + *p;
            }
            c / points.len().max(1) as f64
        }
    }
}

/// Inertia of the resolved part mass distributed over the resolved
/// collider: exact primitive formulas for `Box`/`Cylinder`, the
/// documented point-cloud approximation for `Convex` (equal `m/n`
/// point masses at the scaled vertices, about `com`).
fn collider_inertia(collider: &ColliderDef, s: f64, sz: f64, mass: f64, com: DVec3) -> SymMat3 {
    match collider {
        ColliderDef::Box { half_extents_m } => SymMat3::solid_box(
            mass,
            DVec3::new(
                half_extents_m.x * s,
                half_extents_m.y * s,
                half_extents_m.z * sz,
            ),
        ),
        ColliderDef::Cylinder {
            radius_m,
            half_height_m,
        } => SymMat3::solid_cylinder(mass, radius_m * s, half_height_m * sz, Axis::Z),
        ColliderDef::Convex { points } => {
            if points.is_empty() {
                return SymMat3::ZERO;
            }
            let m = mass / points.len() as f64;
            let mut i = SymMat3::ZERO;
            for p in points {
                i = i + SymMat3::ZERO.shift(m, DVec3::new(p.x * s, p.y * s, p.z * sz) - com);
            }
            i
        }
    }
}

/// The `resolve_stats` product — `(mass_dry_kg, cost_funds,
/// build_resources, modules)` — aliased so the signature stays
/// readable.
type ResolvedStats = (f64, f64, Vec<(ResourceId, f64)>, Vec<ModuleDef>);

/// The scalar/module half of resolution (everything except geometry
/// and reliability). Returns `(mass_dry, cost, build_resources,
/// modules)` with scale and multipliers applied.
///
/// Every emitted numeric family is gated — the resolver's output
/// contract (`EffectivePart`: "every number is finite") is enforced
/// here, at emission, so a def that skipped the `csp-data` validator
/// fails typed rather than leaking NaN/∞ or out-of-range stats into
/// the resolved record. The gates mirror the validator's rules; the
/// `res` argument resolves module `ResourceId`s (engine propellants,
/// drill outputs) — the validator's `check_resource_list` layer,
/// re-checked here because the resolver cannot assume validation ran.
fn resolve_stats(
    def: &PartDef,
    f: &ScaleFactors,
    mults: &BTreeMap<StatPath, f64>,
    cost_multiplier: f64,
    res: &ResourceFile,
) -> Result<ResolvedStats, ResolveError> {
    let bad = |field: &'static str| ResolveError::InvalidStat {
        part_id: def.id.clone(),
        field,
    };
    let mass_dry = def.mass_dry_kg * f.mass * mult(mults, StatPath::MassDryKg);
    let cost = def.cost_funds * f.cost * cost_multiplier * mult(mults, StatPath::CostFunds);
    // BOM scales with `s^mass`, not `s^cost`: a bill of materials is
    // mass-like (a scaled tank's BOM is its scaled dry mass in
    // feedstock terms). See the module doc's convention note — a
    // dedicated BOM exponent is a deferred schema question.
    let build_resources: Vec<(ResourceId, f64)> = def
        .build_resources
        .iter()
        .map(|(r, u)| (r.clone(), u * f.mass))
        .collect();
    // Resolved BOM amounts are positive and finite — the validator's
    // `check_resource_list` rule applied to the *resolved* amount.
    for (_, u) in &build_resources {
        if !(u.is_finite() && *u > 0.0) {
            return Err(bad(
                "resolve: build resource amount is not positive and finite",
            ));
        }
    }
    let modules = def
        .modules
        .iter()
        .map(|m| resolve_module(def, m, f, mults, res))
        .collect::<Result<_, _>>()?;
    Ok((mass_dry, cost, build_resources, modules))
}

/// The `Engine.propellants` contract: `ResourceId`s known to
/// `resources.ron`, unique, with positive finite mass fractions that
/// total 1 — the validator's `check_engine_module` rules, mirrored so
/// an unvalidated def cannot resolve a mix the readout's mass-flow
/// bookkeeping cannot drain (an unknown id there resolves
/// `mass_per_unit` to `None`, silently starving the stage).
fn check_engine_propellants(
    def: &PartDef,
    propellants: &[(ResourceId, f64)],
    res: &ResourceFile,
) -> Result<(), ResolveError> {
    /// Mirror of csp-data's `PROPELLANT_SUM_TOL` (private to the
    /// validator) — fractions must total 1 within authoring noise.
    const PROPELLANT_SUM_TOL: f64 = 1e-6;
    let bad = |field: &'static str| ResolveError::InvalidStat {
        part_id: def.id.clone(),
        field,
    };
    if propellants.is_empty() {
        return Err(bad("resolve: engine declares no propellants"));
    }
    let mut seen = BTreeSet::new();
    let mut sum = 0.0;
    for (r, frac) in propellants {
        if !seen.insert(r.as_str()) {
            return Err(bad("resolve: duplicate propellant resource id"));
        }
        if !res.contains(r) {
            return Err(ResolveError::UnknownResource {
                part_id: def.id.clone(),
                resource: r.clone(),
            });
        }
        if !(frac.is_finite() && *frac > 0.0) {
            return Err(bad(
                "resolve: propellant fraction is not positive and finite",
            ));
        }
        sum += frac;
    }
    if !(sum.is_finite() && (sum - 1.0).abs() <= PROPELLANT_SUM_TOL) {
        return Err(bad("resolve: propellant mass fractions do not sum to 1"));
    }
    Ok(())
}

/// One module with resolution baked in: engine stats get `s^thrust`
/// and their [`StatPath`] multipliers, tank capacities `s^capacity`,
/// control torque its multiplier. All other modules pass through —
/// nothing in their field set is a scale or variant target in v1.
/// `Control.torque_nm` is **unscaled by `s` on purpose**: the schema
/// has no exponent channel for it (module doc's gap note) — torque
/// still takes its [`StatPath::ControlTorqueNm`] multiplier.
///
/// The emitted fields are gated per family (see [`resolve_stats`]'s
/// doc): the engine numbers the readout divides by and compares, tank
/// capacities, control authority, drill/lab/converter rates — each
/// re-checked against the same bounds the `csp-data` validator
/// enforces. `Converter.recipe` and `Experiment.kind` are
/// cross-references into *other* data files (recipes, experiment
/// kinds) this signature cannot see — resolving them is the
/// validator's job alone, documented rather than half-checked.
#[allow(clippy::too_many_lines)] // the per-module gate list is inherently an arm table
fn resolve_module(
    def: &PartDef,
    m: &ModuleDef,
    f: &ScaleFactors,
    mults: &BTreeMap<StatPath, f64>,
    res: &ResourceFile,
) -> Result<ModuleDef, ResolveError> {
    let bad = |field: &'static str| ResolveError::InvalidStat {
        part_id: def.id.clone(),
        field,
    };
    Ok(match m {
        ModuleDef::Engine {
            thrust_vac_n,
            isp_vac_s,
            isp_sl_s,
            throttle_range,
            gimbal_deg,
            ignitions,
            rated_burn_s,
            propellants,
            ullage_required,
        } => {
            let thrust = thrust_vac_n * f.thrust * mult(mults, StatPath::ThrustVacN);
            let isp_vac = isp_vac_s * mult(mults, StatPath::IspVacS);
            let isp_sl = isp_sl_s * mult(mults, StatPath::IspSlS);
            let burn = rated_burn_s * mult(mults, StatPath::RatedBurnS);
            if !(thrust.is_finite() && thrust > 0.0) {
                return Err(bad(
                    "resolve: engine thrust_vac_n is not positive and finite",
                ));
            }
            if !(isp_vac.is_finite() && isp_vac > 0.0) {
                return Err(bad("resolve: engine isp_vac_s is not positive and finite"));
            }
            if !(isp_sl.is_finite() && isp_sl > 0.0) {
                return Err(bad("resolve: engine isp_sl_s is not positive and finite"));
            }
            if !(burn.is_finite() && burn > 0.0) {
                return Err(bad(
                    "resolve: engine rated_burn_s is not positive and finite",
                ));
            }
            if !(throttle_range.min.is_finite()
                && throttle_range.max.is_finite()
                && throttle_range.min >= 0.0
                && throttle_range.min < throttle_range.max
                && throttle_range.max <= 1.0)
            {
                return Err(bad(
                    "resolve: engine throttle_range outside 0 <= min < max <= 1",
                ));
            }
            if !(gimbal_deg.is_finite() && *gimbal_deg >= 0.0) {
                return Err(bad("resolve: engine gimbal_deg is negative or non-finite"));
            }
            if let Some(n) = ignitions
                && *n == 0
            {
                return Err(bad(
                    "resolve: engine ignitions is Some(0) — use None for unlimited",
                ));
            }
            check_engine_propellants(def, propellants, res)?;
            ModuleDef::Engine {
                thrust_vac_n: thrust,
                isp_vac_s: isp_vac,
                isp_sl_s: isp_sl,
                throttle_range: *throttle_range,
                gimbal_deg: *gimbal_deg,
                ignitions: *ignitions,
                rated_burn_s: burn,
                propellants: propellants.clone(),
                ullage_required: *ullage_required,
            }
        }
        ModuleDef::Tank { capacity } => {
            let mut seen = BTreeSet::new();
            let mut resolved = Vec::with_capacity(capacity.len());
            for (r, u) in capacity {
                if !seen.insert(r.as_str()) {
                    return Err(bad("resolve: duplicate tank capacity resource id"));
                }
                if !res.contains(r) {
                    return Err(ResolveError::UnknownResource {
                        part_id: def.id.clone(),
                        resource: r.clone(),
                    });
                }
                let u = u * f.capacity;
                if !(u.is_finite() && u > 0.0) {
                    return Err(bad("resolve: tank capacity is not positive and finite"));
                }
                resolved.push((r.clone(), u));
            }
            ModuleDef::Tank { capacity: resolved }
        }
        ModuleDef::Control { torque_nm, power_w } => {
            let torque = torque_nm * mult(mults, StatPath::ControlTorqueNm);
            if !(torque.is_finite() && torque >= 0.0) {
                return Err(bad("resolve: control torque_nm is negative or non-finite"));
            }
            if !(power_w.is_finite() && *power_w >= 0.0) {
                return Err(bad("resolve: control power_w is negative or non-finite"));
            }
            ModuleDef::Control {
                torque_nm: torque,
                power_w: *power_w,
            }
        }
        ModuleDef::CrewSeat { slots } => {
            if *slots == 0 {
                return Err(bad("resolve: crew seat with zero slots"));
            }
            ModuleDef::CrewSeat { slots: *slots }
        }
        ModuleDef::Experiment {
            kind,
            data_units,
            reusable,
            situations,
        } => {
            if !(data_units.is_finite() && *data_units > 0.0) {
                return Err(bad("resolve: experiment data_units not positive/finite"));
            }
            if situations.is_empty() || situations.0 & !csp_data::SituationMask::ALL.0 != 0 {
                return Err(bad("resolve: experiment situation mask is invalid"));
            }
            ModuleDef::Experiment {
                kind: kind.clone(),
                data_units: *data_units,
                reusable: *reusable,
                situations: *situations,
            }
        }
        ModuleDef::Drill {
            resource,
            base_rate_per_s,
            min_concentration,
            power_w,
        } => {
            if !res.contains(resource) {
                return Err(ResolveError::UnknownResource {
                    part_id: def.id.clone(),
                    resource: resource.clone(),
                });
            }
            if !(base_rate_per_s.is_finite() && *base_rate_per_s > 0.0) {
                return Err(bad("resolve: drill base_rate is not positive and finite"));
            }
            if !(min_concentration.is_finite() && (0.0..=1.0).contains(min_concentration)) {
                return Err(bad("resolve: drill min_concentration outside [0, 1]"));
            }
            if !(power_w.is_finite() && *power_w >= 0.0) {
                return Err(bad("resolve: drill power_w is negative or non-finite"));
            }
            m.clone()
        }
        ModuleDef::Converter { power_w, .. } => {
            if !(power_w.is_finite() && *power_w >= 0.0) {
                return Err(bad("resolve: converter power_w is negative or non-finite"));
            }
            m.clone()
        }
        ModuleDef::Lab {
            throughput_data_per_s,
            multiplier,
            researcher_slots,
        } => {
            if !(throughput_data_per_s.is_finite() && *throughput_data_per_s > 0.0)
                || !(multiplier.is_finite() && *multiplier > 0.0)
            {
                return Err(bad(
                    "resolve: lab throughput/multiplier not positive/finite",
                ));
            }
            if *researcher_slots == 0 {
                return Err(bad("resolve: lab with zero researcher slots"));
            }
            m.clone()
        }
        ModuleDef::Parachute {
            deploy_alt_m,
            deploy_speed_mps,
            packed_drag_area_m2,
            deployed_drag_area_m2,
            deploy_time_s,
        } => {
            // The validator's bounds, mirrored: positive catch
            // ceilings, non-negative areas, deployed > packed (an
            // equal-or-smaller canopy is a no-op deploy, not a
            // decelerator), non-negative ramp. The drag areas scale
            // `s²` in [`resolve_weld_input`] — geometry, like the
            // collider and `ref_len_m`, not a stat-path family.
            if !(deploy_alt_m.is_finite() && *deploy_alt_m > 0.0) {
                return Err(bad(
                    "resolve: parachute deploy_alt_m is not positive and finite",
                ));
            }
            if !(deploy_speed_mps.is_finite() && *deploy_speed_mps > 0.0) {
                return Err(bad(
                    "resolve: parachute deploy_speed_mps is not positive and finite",
                ));
            }
            if !(packed_drag_area_m2.is_finite() && *packed_drag_area_m2 >= 0.0) {
                return Err(bad(
                    "resolve: parachute packed_drag_area_m2 is negative or non-finite",
                ));
            }
            if !(deployed_drag_area_m2.is_finite() && *deployed_drag_area_m2 > *packed_drag_area_m2)
            {
                return Err(bad(
                    "resolve: parachute deployed_drag_area_m2 does not exceed packed",
                ));
            }
            if !(deploy_time_s.is_finite() && *deploy_time_s >= 0.0) {
                return Err(bad(
                    "resolve: parachute deploy_time_s is negative or non-finite",
                ));
            }
            m.clone()
        }
        // `DockPort`/`Decoupler` carry no numeric families — the port's
        // node-name check lives in [`resolve_weld_input`] where the
        // face spec is produced.
        other => other.clone(),
    })
}

/// The whole-curve well-formedness gate the validator applies
/// (validate.rs `check_reliability`): non-empty, every knot finite,
/// `qualification_units` non-negative and strictly increasing, and
/// every `hazard_multiplier` in `(0, 1]`. `qual_factor` evaluated at
/// the resolve-time units alone cannot see a malformed knot further
/// along the curve — and the sim re-evaluates the same curve at the
/// family's *live* units — so the curve itself is gated here, where
/// the resolver's own contract ("cannot assume validation ran") says
/// the check belongs.
pub fn qual_curve_valid(curve: &csp_data::QualCurve) -> bool {
    let knots = &curve.knots;
    if knots.is_empty() {
        return false;
    }
    // Units are non-decreasing, not strictly increasing: equal-knot
    // pairs are a supported pattern — the earlier window claims every
    // `units` that could reach the 0/0 division, so the curve
    // degenerates gracefully to the first knot's value.
    let mut prev = f64::NEG_INFINITY;
    for k in knots {
        if !k.qualification_units.is_finite()
            || !k.hazard_multiplier.is_finite()
            || k.qualification_units < 0.0
            || k.qualification_units < prev
            || !(0.0..=1.0).contains(&k.hazard_multiplier)
            || k.hazard_multiplier <= 0.0
        {
            return false;
        }
        prev = k.qualification_units;
    }
    true
}

/// Piecewise-linear `qual_factor` at `units` over the def's
/// qualification curve, clamped to the end knots (the §3.0
/// temperature-table convention `AeroKnot` docs reuse). An empty
/// curve — which the validator forbids but a raw def could carry —
/// resolves to `1.0`, "no qualification credit". A **non-finite**
/// `units` also resolves to the first knot: the comparisons below all
/// fail on NaN/±∞, which would otherwise fall through to the last
/// (best) knot — corrupt qualification state degrades to the
/// unqualified reading, never the qualified one.
///
/// Public for the M9b reliability loop: §3.4's `qual_factor(family)`
/// reads the family's *live* qualification units, so the sim
/// re-evaluates the same curve the resolver does — one function, one
/// answer.
pub fn qual_factor(curve: &csp_data::QualCurve, units: f64) -> f64 {
    let knots = &curve.knots;
    if !units.is_finite() {
        return knots.first().map_or(1.0, |k| k.hazard_multiplier);
    }
    if knots.is_empty() || units <= knots[0].qualification_units {
        return knots.first().map_or(1.0, |k| k.hazard_multiplier);
    }
    for w in knots.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if units <= b.qualification_units {
            let t =
                (units - a.qualification_units) / (b.qualification_units - a.qualification_units);
            return a.hazard_multiplier + t * (b.hazard_multiplier - a.hazard_multiplier);
        }
    }
    knots[knots.len() - 1].hazard_multiplier
}

/// `wear(PartUsage)` — the second of §3.1's two functions, as a pure
/// multiplier: `1 + WEAR_HAZARD_STUB · operating_s / base_mtbf_s`.
/// `PartUsage::FRESH` returns exactly `1.0`. See the module doc for
/// what replaces this stub (§3.4 accumulation loop, later milestone).
///
/// **Contract (unchecked by design — this helper is infallible):**
/// `base_mtbf_s` must be finite and `> 0`, and `usage` must satisfy
/// [`PartUsage::is_valid`]. [`resolve_part`]/[`apply_wear`] guarantee
/// both before calling; direct callers must uphold them or the
/// quotient is garbage (the function cannot fail, so it cannot say
/// so). Note `base_mtbf_s` is the *resolved* MTBF — normalizing by
/// the post-upgrade value is deliberate and double-counts an MTBF
/// upgrade's benefit; the module doc's wear note explains why that
/// is the documented v1 choice rather than a bug. Even under the
/// contract the quotient is unbounded — [`apply_wear`] gates the
/// *result* for finiteness before storing it.
pub fn wear_multiplier(base_mtbf_s: f64, usage: &PartUsage) -> f64 {
    1.0 + WEAR_HAZARD_STUB * (usage.operating_s / base_mtbf_s)
}

/// Apply the wear half to a resolved part: recompute
/// `wear_multiplier` and `hazard_per_s` in place. Exposed so the
/// future runtime path can re-wear a resolved part without
/// re-resolving the frozen half — `resolve ∘ wear` is associative per
/// instance.
///
/// Errors on invalid usage input (non-finite or negative fields) so a
/// corrupt serialized record fails typed rather than poisoning hazard.
/// Also errors when the *output* overflows: `operating_s /
/// base_mtbf_s` is unbounded for valid inputs (a huge accumulator
/// against a small-but-legal MTBF), and an infinite multiplier must
/// fail typed instead of writing `∞` into `hazard_per_s`.
pub fn apply_wear(eff: &mut EffectivePart, usage: &PartUsage) -> Result<(), ResolveError> {
    if !usage.is_valid() {
        return Err(ResolveError::InvalidUsage {
            part_id: eff.part_id.clone(),
            field: "resolve: usage accumulator is negative or non-finite",
        });
    }
    let r = &mut eff.reliability;
    let wear = wear_multiplier(r.base_mtbf_s, usage);
    let hazard = r.base_hazard_per_s * r.qual_factor * wear;
    if !wear.is_finite() || !hazard.is_finite() {
        return Err(ResolveError::InvalidStat {
            part_id: eff.part_id.clone(),
            field: "resolve: wear multiplier/hazard overflowed to non-finite",
        });
    }
    r.wear_multiplier = wear;
    r.hazard_per_s = hazard;
    Ok(())
}

/// The engine-specific half of a [`ReliabilitySpec`] — the resolved
/// fields §3.4's stress rules key on (`BurnBeyondRated`,
/// `ExcessRestarts`, `UllageNegativeIgnition`). `None` on a spec
/// means the part carries no `Engine` module: engine-only stresses
/// never apply and the part never "burns" for the operating/dormant
/// split.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct EngineReliability {
    /// Rated continuous burn (s) — the `BurnBeyondRated` metric's
    /// reference.
    pub rated_burn_s: f64,
    /// Rated lifetime ignitions; `None` = unlimited — the
    /// `ExcessRestarts` count metric compares against the authored
    /// rule threshold either way.
    pub ignitions: Option<u32>,
    /// Whether ignition requires settled propellant — gates the
    /// `UllageNegativeIgnition` stress.
    pub ullage_required: bool,
}

/// The reliability half of a resolved part, packaged for the sim's
/// `CraftDef` — §3.4's def-side numbers with resolution baked in,
/// parallel to `parts`/`part_costs` (frozen at build like both; the
/// LIVE inputs — `PartUsage`, `FamilyQualification` — stay sim-side
/// per §3.1's two-lifetimes rule).
///
/// Unlike [`ResolvedReliability`] this record carries the *curve*,
/// not the point-evaluated `qual_factor`: §3.4 re-evaluates
/// `qual_factor(family)` on the family's live units, so the curve
/// must travel with the craft (a bench test mid-flight must reach
/// already-built units — the resolve doc's step 7).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReliabilitySpec {
    /// The upgrade/qualification family (§3.3/§3.4 key).
    pub family: FamilyId,
    /// `base_mtbf_s` after [`StatPath::BaseMtbfS`] multipliers (s).
    pub base_mtbf_s: f64,
    /// Authored infant-mortality fraction — extra hazard for a fresh
    /// part, decaying with early operating time.
    pub infant_mortality: f64,
    /// Authored dormant-hazard multiplier — `λ_dormant =
    /// λ_base · dormant_stress` (§3.4 verbatim: no `qual_factor`,
    /// no infant decay, no regime stresses).
    pub dormant_stress: f64,
    /// Authored stress rules, verbatim (§3.4: displayed to the
    /// player, never hidden).
    pub regime_stress: Vec<StressRule>,
    /// Authored `(mode, weight)` failure-mode distribution.
    pub failure_modes: Vec<(FailureMode, f64)>,
    /// Authored `qual_factor` curve — evaluated on the family's
    /// *live* [`FamilyQualification::units`] at each λ evaluation
    /// (the type doc's live-qualification rule).
    pub qualification_curve: csp_data::QualCurve,
    /// The engine half, when the part carries an `Engine` module.
    pub engine: Option<EngineReliability>,
}

impl ReliabilitySpec {
    /// Project one [`EffectivePart`] to its reliability spec —
    /// the `CraftDef::reliability` producer every craft builder
    /// shares (`part_costs` precedent: the caller assembles the
    /// parallel table from resolved parts).
    pub fn from_effective(eff: &EffectivePart) -> Self {
        let engine = eff.modules.iter().find_map(|m| match m {
            ModuleDef::Engine {
                rated_burn_s,
                ignitions,
                ullage_required,
                ..
            } => Some(EngineReliability {
                rated_burn_s: *rated_burn_s,
                ignitions: *ignitions,
                ullage_required: *ullage_required,
            }),
            _ => None,
        });
        Self {
            family: eff.family.clone(),
            base_mtbf_s: eff.reliability.base_mtbf_s,
            infant_mortality: eff.reliability.infant_mortality,
            dormant_stress: eff.reliability.dormant_stress,
            regime_stress: eff.reliability.regime_stress.clone(),
            failure_modes: eff.reliability.failure_modes.clone(),
            qualification_curve: eff.reliability.qualification_curve.clone(),
            engine,
        }
    }

    /// Whether the record is well-formed — the sim's authored-input
    /// gate (the spawn checks): every rate finite and non-negative,
    /// MTBF positive, rules sane, at least one weighted mode.
    pub fn is_valid(&self) -> bool {
        self.base_mtbf_s.is_finite()
            && self.base_mtbf_s > 0.0
            && self.infant_mortality.is_finite()
            && (0.0..1.0).contains(&self.infant_mortality)
            && self.dormant_stress.is_finite()
            && (0.0..=1.0).contains(&self.dormant_stress)
            && !self.failure_modes.is_empty()
            && self
                .failure_modes
                .iter()
                .all(|(_, w)| w.is_finite() && *w > 0.0)
            && self.regime_stress.iter().all(|r| {
                r.threshold.is_finite()
                    && r.threshold >= 0.0
                    && r.multiplier.is_finite()
                    && r.multiplier >= 1.0
            })
            && qual_curve_valid(&self.qualification_curve)
            && self
                .engine
                .is_none_or(|e| e.rated_burn_s.is_finite() && e.rated_burn_s >= 0.0)
    }

    /// `λ_base = 1 / base_mtbf_s` (hazard per second).
    pub fn base_hazard_per_s(&self) -> f64 {
        1.0 / self.base_mtbf_s
    }

    /// The dormant accrual rate — §3.4 verbatim `λ_base ·
    /// dormant_stress`, scaled by the save's global dormant switch
    /// (`dormant_stress_scale = 0` disables the mechanic entirely).
    /// Constant per part per save — which is what makes the dormant
    /// `PartFailure` publish tick stable once computed.
    pub fn dormant_hazard_per_s(&self, dormant_stress_scale: f64) -> f64 {
        self.base_hazard_per_s() * self.dormant_stress * dormant_stress_scale
    }
}

/// The regime inputs one stress evaluation reads — what the part saw
/// this tick. §3.4's `Π(stress_i)` terms each key on one field; the
/// sim's per-part tick pass fills them from live physics state, the
/// editor fills them from a user-set probe so the same evaluation the
/// flight model runs is the one displayed (§3.4: "hidden stress
/// multipliers are bad design" — the display IS this function's
/// output, never a re-derived table).
///
/// The type lives next to [`ReliabilitySpec`] — not in the sim —
/// because it is pure spec math both the sim tick and the `csp-ui`
/// VAB consume (`csp-ui` never sees `csp-sim`, §2.1).
#[derive(Clone, Copy, Debug, Default)]
pub struct RegimeMetrics {
    /// Vessel throttle fraction this tick.
    pub throttle: f64,
    /// Continuous burn duration at this tick (s) — engine only.
    pub burn_s: f64,
    /// `temp_k / max_temp_k` — the `Overtemperature` metric.
    pub temp_fraction: f64,
    /// Sustained acceleration in g — the `ExcessG` metric.
    pub g_load: f64,
    /// `UllageNegativeIgnition` latched this burn.
    pub ullage_negative: bool,
    /// Lifetime restart count (`ignitions − 1`, floored at 0) — the
    /// `ExcessRestarts` metric.
    pub restarts: f64,
}

/// Whether `rule` is active under `m` — the single evaluation both
/// [`stress_product`] and [`stress_breakdown`] read, so the displayed
/// table can never drift from the applied product.
fn stress_rule_active(spec: &ReliabilitySpec, rule: &StressRule, m: &RegimeMetrics) -> bool {
    match rule.kind {
        StressKind::OverThrottle => m.throttle > rule.threshold,
        StressKind::BurnBeyondRated => {
            spec.engine.is_some()
                && m.burn_s > rule.threshold + spec.engine.map_or(0.0, |e| e.rated_burn_s)
        }
        StressKind::Overtemperature => m.temp_fraction > rule.threshold,
        StressKind::ExcessG => m.g_load > rule.threshold,
        StressKind::UllageNegativeIgnition => m.ullage_negative,
        StressKind::ExcessRestarts => m.restarts > rule.threshold,
    }
}

/// `Π(stress_i)` over the spec's authored rules for `metrics` — the
/// applied half of §3.4's stress model (the sim's operating λ reads
/// this every evaluation).
pub fn stress_product(spec: &ReliabilitySpec, m: &RegimeMetrics) -> f64 {
    let mut product = 1.0;
    for rule in &spec.regime_stress {
        if stress_rule_active(spec, rule, m) {
            product *= rule.multiplier;
        }
    }
    product
}

/// The `(rule, active)` display table — the §3.4 "displayed to the
/// player" half of [`stress_product`], same evaluation: each authored
/// rule paired with whether it fires under `m`. The VAB's part
/// inspector renders this verbatim (rule text from the def, active
/// marks from the probe metrics).
pub fn stress_breakdown<'a>(
    spec: &'a ReliabilitySpec,
    m: &RegimeMetrics,
) -> Vec<(&'a StressRule, bool)> {
    spec.regime_stress
        .iter()
        .map(|rule| (rule, stress_rule_active(spec, rule, m)))
        .collect()
}

/// Propellant mass of the resolved tank contents — tanks full (module
/// doc), units → kg through `resources.ron`. Capacity is a
/// resource-pair field, not a [`StatPath`] target in v1.
fn propellant_mass(
    def: &PartDef,
    modules: &[ModuleDef],
    res: &ResourceFile,
) -> Result<f64, ResolveError> {
    let mut kg = 0.0;
    for m in modules {
        if let ModuleDef::Tank { capacity } = m {
            for (r, units) in capacity {
                let mpu = res
                    .mass_per_unit(r)
                    .ok_or_else(|| ResolveError::UnknownResource {
                        part_id: def.id.clone(),
                        resource: r.clone(),
                    })?;
                if !units.is_finite() || *units < 0.0 {
                    return Err(ResolveError::InvalidStat {
                        part_id: def.id.clone(),
                        field: "resolve: tank capacity is negative or non-finite",
                    });
                }
                kg += units * mpu;
            }
        }
    }
    Ok(kg)
}

/// The geometry stage: scaled collider → CoM → inertia → thrust
/// sites, assembled into the [`WeldPart`] view. `res` supplies the
/// tank entries' resolved `mass_per_unit_kg` (M6-D — the frozen
/// wet-mass datum the sim's contents bookkeeping debits against).
#[allow(clippy::too_many_lines)] // the module-dispatch arm list grew with M7-A
#[allow(clippy::too_many_arguments)] // s/sz scale + the stat/module triples — splitting obscures the pipeline
fn resolve_weld_input(
    def: &PartDef,
    s: f64,
    sz: f64,
    mass_kg: f64,
    modules: &[ModuleDef],
    thermal: &csp_data::ThermalDef,
    structural: &csp_data::StructuralDef,
    res: &ResourceFile,
) -> Result<WeldPart, ResolveError> {
    let bad = |field: &'static str| ResolveError::InvalidStat {
        part_id: def.id.clone(),
        field,
    };
    let collider = scale_collider(&def.collider, s, sz);
    if !collider.is_valid() {
        return Err(bad("resolve: collider resolves to non-finite vertices"));
    }
    // A zero-vertex collider is a `Convex { points: [] }` passthrough
    // — `ColliderShape::is_valid` is vacuous on an empty set and the
    // weld tolerates it as a documented "no-skin" marker, but an
    // *authored* `ColliderDef` must resolve real geometry: a part
    // with no hull has no contact, no facet planes, and a `z_min`
    // the engine thrust-site lookup cannot compute. The validator
    // requires ≥ 4 convex points; the resolver rejects the empty set
    // outright.
    if collider.vertices().is_empty() {
        return Err(bad("resolve: collider resolves to zero vertices"));
    }
    let com0 = collider_com(&def.collider);
    let com = DVec3::new(com0.x * s, com0.y * s, com0.z * sz);
    let inertia = collider_inertia(&def.collider, s, sz, mass_kg, com);
    let mut force_sites = Vec::new();
    let mut dock_ports = Vec::new();
    let mut tanks: Vec<crate::part::TankSpec> = Vec::new();
    let mut crew_seats = 0u32;
    let mut experiments = Vec::new();
    let mut lab: Option<crate::part::LabSpec> = None;
    let mut engine: Option<crate::part::EngineSpec> = None;
    let mut parachute: Option<crate::part::ParachuteSpec> = None;
    for m in modules {
        match m {
            ModuleDef::Engine {
                thrust_vac_n,
                isp_vac_s,
                propellants,
                ..
            } => {
                // The consumption half rides the weld view too — the
                // sim's per-tick propellant bookkeeping debits
                // `throttle·rated/(isp_vac·g₀)` split by the resolved
                // mass fractions. At most one Engine module per part:
                // the validator enforces it (`csp_data::validate`'s
                // module-count rule), and an unvalidated def gets the
                // same typed refusal here — the alternative is two
                // disagreeing reads (this loop last-wins on `engine`
                // while `ReliabilitySpec::from_effective` first-wins
                // on `rated_burn_s`/`ignitions`).
                if engine.is_some() {
                    return Err(bad("resolve: more than one Engine module on a part"));
                }
                engine = Some(crate::part::EngineSpec {
                    isp_vac_s: *isp_vac_s,
                    propellants: propellants.clone(),
                });
                let z_min = collider_z_min(&collider)
                    .ok_or(bad("resolve: collider has no vertices for a thrust site"))?;
                if !thrust_vac_n.is_finite() || *thrust_vac_n < 0.0 {
                    return Err(bad("resolve: resolved thrust is negative or non-finite"));
                }
                force_sites.push(ForceSite::new(
                    DVec3::new(0.0, 0.0, z_min),
                    DVec3::Z,
                    ForceSiteKind::Thrust,
                    *thrust_vac_n,
                ));
            }
            // §3.1 `DockPort { size, node }` → the port face's resolved
            // spec: the named attach node's position (scaled like every
            // node position — `resolve_envelopes` applies the same
            // `s`) and unit direction. A module naming no attach node
            // is a validator failure the resolver re-checks typed.
            ModuleDef::DockPort { size, node } => {
                let face = def
                    .attach_nodes
                    .iter()
                    .find(|n| &n.id == node)
                    .ok_or(bad("resolve: dock_port node names no attach node"))?;
                if !(face.position_m.is_finite() && face.direction.is_finite()) {
                    return Err(bad("resolve: dock_port face node is non-finite"));
                }
                dock_ports.push(crate::part::DockPortSpec {
                    local_pos: DVec3::new(
                        face.position_m.x * s,
                        face.position_m.y * s,
                        face.position_m.z * sz,
                    ),
                    local_dir: face.direction,
                    size: *size,
                });
            }
            // M6-D: the `Tank` module's storage half — the resolved
            // capacity list pooled per resource into [`TankSpec`]s
            // with `mass_per_unit_kg` baked in. `propellant_mass`
            // already ran these lookups on the same resolved modules,
            // so `mass_per_unit` failures are unreachable — a missing
            // entry here is the same `UnknownResource` verdict.
            ModuleDef::Tank { capacity } => {
                for (r, units) in capacity {
                    let mpu =
                        res.mass_per_unit(r)
                            .ok_or_else(|| ResolveError::UnknownResource {
                                part_id: def.id.clone(),
                                resource: r.clone(),
                            })?;
                    if let Some(t) = tanks.iter_mut().find(|t| t.resource == *r) {
                        t.capacity += units;
                    } else {
                        tanks.push(crate::part::TankSpec {
                            resource: r.clone(),
                            capacity: *units,
                            mass_per_unit_kg: mpu,
                        });
                    }
                }
            }
            // M6-D: `CrewSeat { slots }` → the part's seat count the
            // sim's crew bookkeeping reads.
            ModuleDef::CrewSeat { slots } => {
                crew_seats = crew_seats.saturating_add(u32::from(*slots));
            }
            // M7-A: `Experiment` → the resolved instrument spec the
            // science model's `RunExperiment` reads.
            ModuleDef::Experiment {
                kind,
                data_units,
                reusable,
                situations,
            } => {
                if !(data_units.is_finite() && *data_units > 0.0) {
                    return Err(bad("resolve: experiment data_units not positive/finite"));
                }
                experiments.push(crate::part::ExperimentSpec {
                    kind: kind.clone(),
                    data_units: *data_units,
                    reusable: *reusable,
                    situations: *situations,
                });
            }
            // M7-A: `Lab` → the resolved converter spec. One per part —
            // a second Lab module is ambiguous authored data (which T /
            // multiplier would the project formula read?), refused.
            ModuleDef::Lab {
                throughput_data_per_s,
                multiplier,
                researcher_slots,
            } => {
                if lab.is_some() {
                    return Err(bad("resolve: more than one Lab module on a part"));
                }
                if !(throughput_data_per_s.is_finite() && *throughput_data_per_s > 0.0)
                    || !(multiplier.is_finite() && *multiplier > 0.0)
                {
                    return Err(bad(
                        "resolve: lab throughput/multiplier not positive/finite",
                    ));
                }
                lab = Some(crate::part::LabSpec {
                    throughput_data_per_s: *throughput_data_per_s,
                    multiplier: *multiplier,
                    researcher_slots: u32::from(*researcher_slots),
                });
            }
            // `Parachute` → the deploy spec the sim's `DeployPart`
            // reads. One per part — a second is ambiguous authored
            // data (which area/thresholds would the single per-node
            // deploy flag arm?), refused like `Lab`/`Engine`. The
            // Cd·A areas scale `s²` — canopy area is geometry like
            // the collider and `ref_len_m`, not a stat-path family
            // (the schema has no area exponent channel; same deferred
            // convention as torque/conduction).
            ModuleDef::Parachute {
                deploy_alt_m,
                deploy_speed_mps,
                packed_drag_area_m2,
                deployed_drag_area_m2,
                deploy_time_s,
            } => {
                if parachute.is_some() {
                    return Err(bad("resolve: more than one Parachute module on a part"));
                }
                let area_scale = s * s;
                let spec = crate::part::ParachuteSpec {
                    deploy_alt_m: *deploy_alt_m,
                    deploy_speed_mps: *deploy_speed_mps,
                    packed_drag_area_m2: packed_drag_area_m2 * area_scale,
                    deployed_drag_area_m2: deployed_drag_area_m2 * area_scale,
                    deploy_time_s: *deploy_time_s,
                };
                if !(spec.deploy_alt_m.is_finite() && spec.deploy_alt_m > 0.0)
                    || !(spec.deploy_speed_mps.is_finite() && spec.deploy_speed_mps > 0.0)
                    || !(spec.packed_drag_area_m2.is_finite() && spec.packed_drag_area_m2 >= 0.0)
                    || !(spec.deployed_drag_area_m2.is_finite()
                        && spec.deployed_drag_area_m2 > spec.packed_drag_area_m2)
                    || !(spec.deploy_time_s.is_finite() && spec.deploy_time_s >= 0.0)
                {
                    return Err(bad("resolve: parachute spec is invalid after scaling"));
                }
                parachute = Some(spec);
            }
            _ => {}
        }
    }
    Ok(WeldPart {
        mass_kg,
        com,
        inertia,
        collider,
        force_sites,
        aero: csp_data::AeroDef {
            cd_curve: def.aero.cd_curve.clone(),
            cl_curve: def.aero.cl_curve.clone(),
            ref_len_m: def.aero.ref_len_m * s,
        },
        thermal: thermal.clone(),
        structural: structural.clone(),
        dock_ports,
        tanks,
        crew_seats,
        experiments,
        lab,
        engine,
        parachute,
    })
}

/// The `‖d‖ = 1` band for resolved attach-node directions — a mirror
/// of the `csp-data` validator's `NODE_DIR_UNIT_TOL` (private there).
/// The session's snap machinery normalizes for its own use; the
/// resolver gates so the *resolved* record carries the authored
/// direction only when it was already unit within authoring noise.
const NODE_DIR_UNIT_TOL: f64 = 1e-6;

/// One aero coefficient curve under the output contract: every knot
/// finite, `mach ≥ 0` strictly increasing, `coeff ≥ 0` — so an
/// unvalidated def's curve cannot reach `csp-phys::aero`'s piecewise
/// reader (a non-monotone or NaN knot lerps garbage). An **empty**
/// curve is legal resolved data: `WeldPart::aero`'s doc gives it the
/// "occludes but produces no force" semantics, so the resolver gates
/// knot quality rather than presence (the validator's non-empty rule
/// is authored-data policy, not an output contract).
fn aero_curve_is_valid(curve: &[csp_data::AeroKnot]) -> bool {
    let mut prev = f64::NEG_INFINITY;
    for k in curve {
        if !(k.mach.is_finite() && k.coeff.is_finite())
            || k.mach < 0.0
            || k.mach <= prev
            || k.coeff < 0.0
        {
            return false;
        }
        prev = k.mach;
    }
    true
}

/// The envelope stage: structural limits, thermal lump, aero, and
/// attach nodes at the resolved scale — each emitted family gated
/// against the same bounds the `csp-data` validator enforces (the
/// resolver's output contract, applied to the *resolved* values).
fn resolve_envelopes(
    def: &PartDef,
    f: &ScaleFactors,
) -> Result<
    (
        csp_data::StructuralDef,
        csp_data::ThermalDef,
        csp_data::AeroDef,
        Vec<AttachNode>,
    ),
    ResolveError,
> {
    let bad = |field: &'static str| ResolveError::InvalidStat {
        part_id: def.id.clone(),
        field,
    };
    let structural = csp_data::StructuralDef {
        max_axial_n: def.structural.max_axial_n * f.structural,
        max_shear_n: def.structural.max_shear_n * f.structural,
        max_torque_nm: def.structural.max_torque_nm * f.structural,
    };
    if !(structural.max_axial_n.is_finite() && structural.max_axial_n > 0.0)
        || !(structural.max_shear_n.is_finite() && structural.max_shear_n > 0.0)
        || !(structural.max_torque_nm.is_finite() && structural.max_torque_nm > 0.0)
    {
        return Err(bad("resolve: structural limit is not positive and finite"));
    }
    let thermal = csp_data::ThermalDef {
        heat_capacity_j_per_k: def.thermal.heat_capacity_j_per_k * f.mass,
        max_temp_k: def.thermal.max_temp_k,
        emissivity: def.thermal.emissivity,
        conduction_w_per_k: def.thermal.conduction_w_per_k,
    };
    if !(thermal.heat_capacity_j_per_k.is_finite() && thermal.heat_capacity_j_per_k > 0.0)
        || !(thermal.max_temp_k.is_finite() && thermal.max_temp_k > 0.0)
        || !(thermal.emissivity.is_finite()
            && thermal.emissivity > 0.0
            && thermal.emissivity <= 1.0)
        || !(thermal.conduction_w_per_k.is_finite() && thermal.conduction_w_per_k >= 0.0)
    {
        return Err(bad("resolve: thermal field is out of range or non-finite"));
    }
    // The explicit-Euler stability bound (validator's
    // `ConductionExceedsCapacity`) re-checked at *resolved* values:
    // `heat_capacity` scales by `s^mass` while `conduction` does not
    // (v1 schema gap, module doc), so a small-enough scale can push a
    // def that passed authored-side over the bound.
    if thermal.conduction_w_per_k > thermal.heat_capacity_j_per_k {
        return Err(bad(
            "resolve: conduction exceeds resolved heat capacity — unstable thermal lump",
        ));
    }
    if !(def.aero.ref_len_m * f.linear).is_finite() || def.aero.ref_len_m * f.linear <= 0.0 {
        return Err(bad("resolve: aero ref_len_m is not positive and finite"));
    }
    if !aero_curve_is_valid(&def.aero.cd_curve) || !aero_curve_is_valid(&def.aero.cl_curve) {
        return Err(bad(
            "resolve: aero curve knot is non-finite, negative, or non-monotone",
        ));
    }
    let aero = csp_data::AeroDef {
        cd_curve: def.aero.cd_curve.clone(),
        cl_curve: def.aero.cl_curve.clone(),
        ref_len_m: def.aero.ref_len_m * f.linear,
    };
    let mut seen_ids = BTreeSet::new();
    let mut attach_nodes = Vec::with_capacity(def.attach_nodes.len());
    for n in &def.attach_nodes {
        if n.id.is_empty() || !seen_ids.insert(n.id.as_str()) {
            return Err(bad("resolve: attach node id is empty or duplicated"));
        }
        let position_m = DVec3::new(
            n.position_m.x * f.linear,
            n.position_m.y * f.linear,
            n.position_m.z * f.linear_z,
        );
        // The node diameter is a cross-section measure — the stack
        // axis's stretch does not widen it.
        let diameter_m = n.diameter_m * f.linear;
        if !position_m.is_finite() {
            return Err(bad("resolve: attach node position is non-finite"));
        }
        let len = n.direction.norm();
        if !(len.is_finite() && (len - 1.0).abs() <= NODE_DIR_UNIT_TOL) {
            return Err(bad("resolve: attach node direction is not unit length"));
        }
        if !(diameter_m.is_finite() && diameter_m > 0.0) {
            return Err(bad(
                "resolve: attach node diameter is not positive and finite",
            ));
        }
        attach_nodes.push(AttachNode {
            id: n.id.clone(),
            position_m,
            direction: n.direction,
            diameter_m,
        });
    }
    Ok((structural, thermal, aero, attach_nodes))
}

/// The reliability stage: `λ_base` post-variant and `qual_factor` at
/// the family's **live** qualification record. `wear_multiplier`/
/// `hazard_per_s` are set to the fresh values — [`apply_wear`]
/// recomputes them (FRESH is identity, so a never-worn part reads
/// identically).
fn resolve_reliability(
    def: &PartDef,
    qual: &FamilyQualification,
    mults: &BTreeMap<StatPath, f64>,
) -> Result<ResolvedReliability, ResolveError> {
    let base_mtbf_s = def.reliability.base_mtbf_s * mult(mults, StatPath::BaseMtbfS);
    if !base_mtbf_s.is_finite() || base_mtbf_s <= 0.0 {
        return Err(ResolveError::InvalidStat {
            part_id: def.id.clone(),
            field: "resolve: resolved base_mtbf_s is not positive and finite",
        });
    }
    if !qual.is_valid() {
        return Err(ResolveError::InvalidQualification {
            part_id: def.id.clone(),
            field: "resolve: qualification units negative or non-finite",
        });
    }
    let base_hazard_per_s = 1.0 / base_mtbf_s;
    // A denormal `base_mtbf_s` passes the finite-and-positive gate above
    // yet produces `λ_base = ∞` — gate the hazard itself (M3-C validator
    // hardening).
    if !base_hazard_per_s.is_finite() {
        return Err(ResolveError::InvalidStat {
            part_id: def.id.clone(),
            field: "resolve: base_hazard_per_s is non-finite (denormal base_mtbf_s)",
        });
    }
    // Passthrough finiteness (M3-C validator hardening): the reliability
    // fields the resolver carries verbatim — infant mortality, dormant
    // stress, regime thresholds/multipliers, failure weights — are gated
    // here so a def that skipped the `csp-data` validator cannot leak
    // NaN/∞ into `ResolvedReliability` for the sim or the editor to read.
    let rel = &def.reliability;
    if !rel.infant_mortality.is_finite() || !rel.dormant_stress.is_finite() {
        return Err(ResolveError::InvalidStat {
            part_id: def.id.clone(),
            field: "resolve: infant_mortality/dormant_stress is non-finite",
        });
    }
    if rel
        .regime_stress
        .iter()
        .any(|r| !r.threshold.is_finite() || !r.multiplier.is_finite())
    {
        return Err(ResolveError::InvalidStat {
            part_id: def.id.clone(),
            field: "resolve: regime_stress threshold/multiplier is non-finite",
        });
    }
    if rel.failure_modes.iter().any(|(_, w)| !w.is_finite()) {
        return Err(ResolveError::InvalidStat {
            part_id: def.id.clone(),
            field: "resolve: failure-mode weight is non-finite",
        });
    }
    // Whole-curve gate: the curve ships verbatim on the spec and the
    // sim re-evaluates it at the family's *live* units, so a knot the
    // resolve-time evaluation never reaches can still lerp a NaN or a
    // >1 multiplier into a live λ — gate every knot, not just the
    // point the resolve happens to sit at (validate.rs's rules).
    if !qual_curve_valid(&def.reliability.qualification_curve) {
        return Err(ResolveError::InvalidStat {
            part_id: def.id.clone(),
            field: "resolve: qualification curve is malformed",
        });
    }
    let qf = qual_factor(&def.reliability.qualification_curve, qual.units());
    // Output gate: a malformed curve (e.g. an equal-knot pair making
    // the lerp divide 0/0 → NaN, or knots outside (0, 1] on a def that
    // skipped the validator) must fail typed, not leak NaN into hazard.
    if !(qf.is_finite() && qf > 0.0 && qf <= 1.0) {
        return Err(ResolveError::InvalidStat {
            part_id: def.id.clone(),
            field: "resolve: qual_factor is not finite in (0, 1]",
        });
    }
    Ok(ResolvedReliability {
        base_mtbf_s,
        base_hazard_per_s,
        infant_mortality: def.reliability.infant_mortality,
        qual_factor: qf,
        wear_multiplier: 1.0,
        hazard_per_s: base_hazard_per_s * qf,
        dormant_hazard_per_s: base_hazard_per_s * def.reliability.dormant_stress,
        regime_stress: def.reliability.regime_stress.clone(),
        dormant_stress: def.reliability.dormant_stress,
        failure_modes: def.reliability.failure_modes.clone(),
        qualification_curve: def.reliability.qualification_curve.clone(),
    })
}

/// `resolve(BuiltPart)` — the frozen half of §3.1: `def` resolved
/// against `built` into a fresh (zero-wear) [`EffectivePart`]. Most
/// call sites want [`resolve_part`], which composes this with
/// [`apply_wear`].
///
/// The pipeline order is the module doc's: identity → scale → variant
/// → stat-path reachability → scaled stats with multipliers → weld
/// view (mass with propellant, CoM, inertia, collider, thrust sites)
/// → qualification. `qual` is the family's *live*
/// [`FamilyQualification`] record — pass [`FamilyQualification::FRESH`]
/// for an unqualified family; a `resolve_parts` caller hands the map
/// and a missing entry means the same.
pub fn resolve_built_part(
    def: &PartDef,
    built: &BuiltPart,
    res: &ResourceFile,
    qual: &FamilyQualification,
) -> Result<EffectivePart, ResolveError> {
    if def.id != built.part_id {
        return Err(ResolveError::PartIdMismatch {
            def: def.id.clone(),
            built: built.part_id.clone(),
        });
    }
    let bad = |field: &'static str| ResolveError::InvalidStat {
        part_id: def.id.clone(),
        field,
    };
    let f = scale_factors(def, built.scale, built.stretch)?;
    let variant = def
        .variants
        .iter()
        .find(|v| v.id == built.variant)
        .ok_or_else(|| ResolveError::UnknownVariant {
            part_id: def.id.clone(),
            variant: built.variant.clone(),
        })?;
    let mults = fold_multipliers(
        def,
        &variant.stat_multipliers,
        &built.upgrade_levels_at_build,
    )?;

    let (mass_dry, cost_funds, build_resources, modules) =
        resolve_stats(def, &f, &mults, variant.cost_multiplier, res)?;

    // Build-material resources must resolve too — a craft that can't
    // be costed can't be built.
    for (r, _) in &build_resources {
        if !res.contains(r) {
            return Err(ResolveError::UnknownResource {
                part_id: def.id.clone(),
                resource: r.clone(),
            });
        }
    }
    // Gate dry mass *before* propellant is added (M3-C validator
    // hardening): a negative resolved `mass_dry` padded by full tanks
    // would slip past the `mass_kg > 0` check below.
    if !mass_dry.is_finite() || mass_dry < 0.0 {
        return Err(bad("resolve: resolved dry mass is negative or non-finite"));
    }
    let mass_kg = mass_dry + propellant_mass(def, &modules, res)?;
    if !mass_kg.is_finite() || mass_kg <= 0.0 {
        return Err(bad(
            "resolve: resolved mass is zero, negative, or non-finite",
        ));
    }
    if !cost_funds.is_finite() || cost_funds < 0.0 {
        return Err(bad("resolve: resolved cost is negative or non-finite"));
    }

    let (structural, thermal, aero, attach_nodes) = resolve_envelopes(def, &f)?;
    let weld_input = resolve_weld_input(
        def,
        f.linear,
        f.linear_z,
        mass_kg,
        &modules,
        &thermal,
        &structural,
        res,
    )?;
    let reliability = resolve_reliability(def, qual, &mults)?;
    let size_m = DVec3::new(
        def.size_m.x * f.linear,
        def.size_m.y * f.linear,
        def.size_m.z * f.linear_z,
    );
    if !(size_m.is_finite() && size_m.x > 0.0 && size_m.y > 0.0 && size_m.z > 0.0) {
        return Err(bad("resolve: resolved size_m is not positive and finite"));
    }

    Ok(EffectivePart {
        part_id: def.id.clone(),
        family: def.family.clone(),
        variant: built.variant.clone(),
        scale: f.linear,
        stretch: built.stretch,
        data_set_hash: built.data_set_hash,
        weld_input,
        cost_funds,
        build_resources,
        size_m,
        attach_nodes,
        modules,
        structural,
        thermal,
        aero,
        reliability,
    })
}

/// `resolve(BuiltPart) ∘ wear(PartUsage)` — the composition every call
/// site uses. `usage` of `None` (or `Some(&PartUsage::FRESH)`) is the
/// fresh-manufacture path: the wear multiplier is exactly `1.0` and
/// the output is [`resolve_built_part`]'s. `qual` is the family's live
/// [`FamilyQualification`] — `&FamilyQualification::FRESH` when the
/// family is untested. Both live parameters exist now — while the
/// §3.4 accumulation loop is still a later milestone — so the
/// signature the editor, sim, AI, and contract validation share never
/// has to move.
pub fn resolve_part(
    def: &PartDef,
    built: &BuiltPart,
    res: &ResourceFile,
    qual: &FamilyQualification,
    usage: Option<&PartUsage>,
) -> Result<EffectivePart, ResolveError> {
    let mut eff = resolve_built_part(def, built, res, qual)?;
    apply_wear(&mut eff, usage.unwrap_or(&PartUsage::FRESH))?;
    Ok(eff)
}

/// Craft-level resolution: every `BuiltPart` against the loaded part
/// set, in input order. `usages`, when `Some`, is parallel to `built`
/// (one wear record per part — the §3.4 runtime loop's input shape);
/// `None` resolves the whole craft fresh. `quals` is the save's
/// family-qualification table; a family absent from it resolves as
/// [`FamilyQualification::FRESH`] — an untested family is the legal
/// unqualified state, not an error.
///
/// Def lookup is a `BTreeMap` built once per call — a def set naming
/// one `PartId` twice fails [`ResolveError::DuplicatePartId`] up front
/// rather than silently keeping the last; unknown-id errors report the
/// lowest-positioned failing record in `built` order (deterministic
/// first-error).
pub fn resolve_parts(
    defs: &LoadedParts,
    built: &[BuiltPart],
    res: &ResourceFile,
    quals: &FamilyQualifications,
    usages: Option<&[PartUsage]>,
) -> Result<Vec<EffectivePart>, ResolveError> {
    if let Some(u) = usages
        && u.len() != built.len()
    {
        return Err(ResolveError::UsageCountMismatch {
            built: built.len(),
            usages: u.len(),
        });
    }
    let mut by_id: BTreeMap<&str, &PartDef> = BTreeMap::new();
    for p in &defs.parts {
        if by_id.insert(p.def.id.as_str(), &p.def).is_some() {
            return Err(ResolveError::DuplicatePartId {
                part_id: p.def.id.clone(),
            });
        }
    }
    let mut out = Vec::with_capacity(built.len());
    for (i, b) in built.iter().enumerate() {
        let def =
            by_id
                .get(b.part_id.as_str())
                .copied()
                .ok_or_else(|| ResolveError::UnknownPartId {
                    part_id: b.part_id.clone(),
                    index: i,
                })?;
        let qual = quals
            .get(&def.family)
            .unwrap_or(&FamilyQualification::FRESH);
        out.push(resolve_part(def, b, res, qual, usages.map(|u| &u[i]))?);
    }
    Ok(out)
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers shipping
// fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use csp_data::{
        AeroDef, ColliderDef, ModuleDef, PART_SCHEMA_VERSION, PartCategory, PartVariant, QualCurve,
        QualKnot, ReliabilityDef, ScaleExponents, StructuralDef, ThermalDef, ThrottleRange,
    };

    /// A minimal engine+control def every knob of the resolver can
    /// act on: scalable, two variants (stock + hot), all module stats
    /// present so every [`StatPath`] resolves.
    // A complete PartDef literal is inherently long — the length cap
    // exists to catch runaway logic, not exhaustive fixtures.
    #[allow(clippy::too_many_lines)]
    fn test_def() -> PartDef {
        PartDef {
            schema_version: PART_SCHEMA_VERSION,
            id: PartId::from("test.engine.unit"),
            family: FamilyId::from("test.family.engine"),
            display_name: "test.engine.unit.name".into(),
            category: PartCategory::Engine,
            mass_dry_kg: 100.0,
            cost_funds: 1_000.0,
            size_m: DVec3::new(1.0, 1.0, 2.0),
            build_resources: vec![(ResourceId::from("metal"), 10.0)],
            attach_nodes: vec![AttachNode {
                id: "top".into(),
                position_m: DVec3::new(0.0, 0.0, 1.0),
                direction: DVec3::Z,
                diameter_m: 1.0,
            }],
            scale: ScaleRule::Continuous {
                min: 0.5,
                max: 2.0,
                exponents: ScaleExponents {
                    mass: 3.0,
                    cost: 2.0,
                    thrust: 2.0,
                    capacity: 3.0,
                    structural: 2.0,
                },
            },
            collider: ColliderDef::Cylinder {
                radius_m: 0.5,
                half_height_m: 1.0,
            },
            aero: AeroDef {
                cd_curve: vec![csp_data::AeroKnot {
                    mach: 0.0,
                    coeff: 0.5,
                }],
                cl_curve: vec![csp_data::AeroKnot {
                    mach: 0.0,
                    coeff: 0.0,
                }],
                ref_len_m: 2.0,
            },
            thermal: ThermalDef {
                heat_capacity_j_per_k: 100_000.0,
                max_temp_k: 1_500.0,
                emissivity: 0.8,
                conduction_w_per_k: 50.0,
            },
            structural: StructuralDef {
                max_axial_n: 500_000.0,
                max_shear_n: 100_000.0,
                max_torque_nm: 20_000.0,
            },
            variants: vec![
                PartVariant {
                    id: VariantId::from("stock"),
                    display_name: "test.variant.stock".into(),
                    cost_multiplier: 1.0,
                    stat_multipliers: Vec::new(),
                },
                PartVariant {
                    id: VariantId::from("hot"),
                    display_name: "test.variant.hot".into(),
                    cost_multiplier: 1.5,
                    stat_multipliers: vec![
                        (StatPath::ThrustVacN, 1.2),
                        (StatPath::IspVacS, 1.1),
                        (StatPath::MassDryKg, 0.9),
                    ],
                },
            ],
            modules: vec![
                ModuleDef::Engine {
                    thrust_vac_n: 50_000.0,
                    isp_vac_s: 300.0,
                    isp_sl_s: 250.0,
                    throttle_range: ThrottleRange { min: 0.2, max: 1.0 },
                    gimbal_deg: 2.0,
                    ignitions: Some(4),
                    rated_burn_s: 180.0,
                    propellants: vec![(ResourceId::from("liquid_fuel"), 1.0)],
                    ullage_required: false,
                },
                ModuleDef::Control {
                    torque_nm: 500.0,
                    power_w: 10.0,
                },
            ],
            reliability: ReliabilityDef {
                base_mtbf_s: 200_000.0,
                infant_mortality: 0.2,
                qualification_curve: QualCurve {
                    knots: vec![
                        QualKnot {
                            qualification_units: 0.0,
                            hazard_multiplier: 1.0,
                        },
                        QualKnot {
                            qualification_units: 100.0,
                            hazard_multiplier: 0.5,
                        },
                    ],
                },
                regime_stress: vec![StressRule {
                    kind: csp_data::StressKind::OverThrottle,
                    threshold: 1.0,
                    multiplier: 3.0,
                }],
                dormant_stress: 0.02,
                failure_modes: vec![(FailureMode::DegradedThrust, 1.0)],
            },
            tech_requirement: csp_data::TechNodeId::from("tech.test"),
        }
    }

    fn resources() -> ResourceFile {
        ResourceFile {
            schema_version: csp_data::RESOURCE_SCHEMA_VERSION,
            resources: vec![
                csp_data::ResourceDef {
                    id: ResourceId::from("metal"),
                    display_name: "r.metal".into(),
                    mass_per_unit_kg: 10.0,
                },
                // The engine def burns these — `resolve_module`
                // resolves propellant `ResourceId`s against the set.
                csp_data::ResourceDef {
                    id: ResourceId::from("liquid_fuel"),
                    display_name: "r.liquid_fuel".into(),
                    mass_per_unit_kg: 5.0,
                },
                csp_data::ResourceDef {
                    id: ResourceId::from("oxidizer"),
                    display_name: "r.oxidizer".into(),
                    mass_per_unit_kg: 5.0,
                },
            ],
        }
    }

    fn built(variant: &str) -> BuiltPart {
        BuiltPart::stock(PartId::from("test.engine.unit"), VariantId::from(variant))
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-12 * b.abs().max(1.0)
    }

    /// Resolve the stock build of `def` fresh — every output-contract
    /// refusal test starts from the authored-good def and breaks one
    /// field, so this is the one-line common path.
    fn resolve_stock(def: &PartDef) -> Result<EffectivePart, ResolveError> {
        resolve_part(
            def,
            &built("stock"),
            &resources(),
            &FamilyQualification::FRESH,
            None,
        )
    }

    /// Assert `result` is `Err(InvalidStat)` naming `field` — most
    /// output-contract gates share the one variant, so the field text
    /// is what pins *which* gate fired (asserting the variant alone
    /// would pass if a different, earlier gate tripped first).
    #[track_caller]
    fn assert_invalid_stat<T: std::fmt::Debug>(
        result: Result<T, ResolveError>,
        field: &'static str,
    ) {
        assert_eq!(
            result.unwrap_err(),
            ResolveError::InvalidStat {
                part_id: PartId::from("test.engine.unit"),
                field,
            }
        );
    }

    /// A second `Engine` module is ambiguous authored data — this loop
    /// used to last-win on `engine` while `from_effective` first-wins
    /// on `rated_burn_s`/`ignitions`; now it is a typed refusal (the
    /// `Lab` rule's shape).
    #[test]
    fn second_engine_module_is_refused() {
        let mut def = test_def();
        def.modules.push(def.modules[0].clone());
        assert_invalid_stat(
            resolve_stock(&def),
            "resolve: more than one Engine module on a part",
        );
    }

    #[test]
    fn stock_variant_resolves_authored_stats() {
        let eff = resolve_part(
            &test_def(),
            &built("stock"),
            &resources(),
            &FamilyQualification::FRESH,
            None,
        )
        .unwrap();
        assert_eq!(eff.weld_input.mass_kg, 100.0);
        assert_eq!(eff.cost_funds, 1_000.0);
        assert_eq!(eff.weld_input.com, DVec3::ZERO);
        // Cylinder inertia: axial ½mr², transverse m(3r²+4h²)/12.
        let i = eff.weld_input.inertia;
        assert!(close(i.zz, 0.5 * 100.0 * 0.25));
        assert!(close(i.xx, 100.0 * (3.0 * 0.25 + 4.0) / 12.0));
        // One thrust site at the −z face, +z, rated at resolved thrust.
        assert_eq!(eff.weld_input.force_sites.len(), 1);
        let site = eff.weld_input.force_sites[0];
        assert_eq!(site.kind, ForceSiteKind::Thrust);
        assert_eq!(site.local_pos, DVec3::new(0.0, 0.0, -1.0));
        assert_eq!(site.local_dir, DVec3::Z);
        assert_eq!(site.rated_n, 50_000.0);
        // Reliability: λ = 1/2e5, qual 1.0 at zero units.
        assert!(close(eff.reliability.base_hazard_per_s, 1.0 / 200_000.0));
        assert_eq!(eff.reliability.qual_factor, 1.0);
        assert_eq!(eff.reliability.wear_multiplier, 1.0);
    }

    #[test]
    fn variant_multipliers_apply_to_named_stats() {
        let eff = resolve_part(
            &test_def(),
            &built("hot"),
            &resources(),
            &FamilyQualification::FRESH,
            None,
        )
        .unwrap();
        // hot: thrust ×1.2, isp_vac ×1.1, mass ×0.9, cost ×1.5.
        let ModuleDef::Engine {
            thrust_vac_n,
            isp_vac_s,
            isp_sl_s,
            rated_burn_s,
            ..
        } = &eff.modules[0]
        else {
            panic!("engine module expected");
        };
        assert_eq!(*thrust_vac_n, 60_000.0);
        assert_eq!(*isp_vac_s, 330.0);
        assert_eq!(*isp_sl_s, 250.0); // untouched
        assert_eq!(*rated_burn_s, 180.0); // untouched
        assert_eq!(eff.weld_input.mass_kg, 90.0);
        assert_eq!(eff.cost_funds, 1_500.0);
        assert_eq!(eff.weld_input.force_sites[0].rated_n, 60_000.0);
    }

    #[test]
    fn scale_exponents_apply_per_stat() {
        let mut b = built("stock");
        b.scale = 2.0;
        let eff = resolve_part(
            &test_def(),
            &b,
            &resources(),
            &FamilyQualification::FRESH,
            None,
        )
        .unwrap();
        // s=2: mass 2³=8, cost 2²=4, thrust 2²=4, structural 2²=4.
        assert_eq!(eff.weld_input.mass_kg, 800.0);
        assert_eq!(eff.cost_funds, 4_000.0);
        assert_eq!(eff.structural.max_axial_n, 2_000_000.0);
        let ModuleDef::Engine { thrust_vac_n, .. } = &eff.modules[0] else {
            panic!();
        };
        assert_eq!(*thrust_vac_n, 200_000.0);
        // Geometry scaled linearly.
        assert_eq!(eff.size_m, DVec3::new(2.0, 2.0, 4.0));
        assert_eq!(eff.attach_nodes[0].position_m.z, 2.0);
        assert_eq!(eff.aero.ref_len_m, 4.0);
        assert_eq!(eff.thermal.heat_capacity_j_per_k, 800_000.0); // s^mass
        assert_eq!(eff.thermal.max_temp_k, 1_500.0); // intensive
        // Site follows the scaled −z face.
        assert_eq!(eff.weld_input.force_sites[0].local_pos.z, -2.0);
        // Cylinder collider → 16-vertex prism at doubled radius.
        let ColliderShape::Points(pts) = &eff.weld_input.collider else {
            panic!("points expected");
        };
        assert_eq!(pts.len(), 16);
        assert_eq!(pts[0], DVec3::new(1.0, 0.0, 2.0));
    }

    #[test]
    fn upgrade_multipliers_stack_on_variants() {
        let mut b = built("hot");
        b.upgrade_levels_at_build.insert(
            StatPath::ThrustVacN,
            TrackLevels {
                levels: 2,
                multiplier: 1.1,
            },
        );
        b.upgrade_levels_at_build.insert(
            StatPath::BaseMtbfS,
            TrackLevels {
                levels: 4,
                multiplier: 2.0,
            },
        );
        let eff = resolve_part(
            &test_def(),
            &b,
            &resources(),
            &FamilyQualification::FRESH,
            None,
        )
        .unwrap();
        let ModuleDef::Engine { thrust_vac_n, .. } = &eff.modules[0] else {
            panic!();
        };
        // 50_000 · 1.2 (variant) · 1.1 (upgrade).
        assert_eq!(*thrust_vac_n, 66_000.0);
        assert_eq!(eff.reliability.base_mtbf_s, 400_000.0);
    }

    /// §3.4: qualification is LIVE. The same frozen `BuiltPart` —
    /// byte-identical — resolves a better `qual_factor` when the
    /// family's record moves, which is exactly what a bench test
    /// improving already-built units looks like.
    #[test]
    fn live_qualification_evaluates_curve_per_resolve() {
        let def = test_def();
        let b = built("stock");
        // 50 units: midpoint of the authored 0→100 lerp → 0.75.
        let q = FamilyQualification {
            flight_units: 50.0,
            ..FamilyQualification::FRESH
        };
        let eff = resolve_part(&def, &b, &resources(), &q, None).unwrap();
        assert_eq!(eff.reliability.qual_factor, 0.75);
        assert!(close(
            eff.reliability.hazard_per_s,
            (1.0 / 200_000.0) * 0.75
        ));
        // Bench-test the family AFTER manufacture: the same built
        // record now resolves the clamped 0.5 — hazard improved on an
        // already-built unit. (bench_units adds to flight_units.)
        let q = FamilyQualification {
            flight_units: 50.0,
            bench_units: 950.0,
            observed_failures: 1,
        };
        let eff = resolve_part(&def, &b, &resources(), &q, None).unwrap();
        assert_eq!(eff.reliability.qual_factor, 0.5);
        assert!(eff.reliability.hazard_per_s < 1.0 / 200_000.0);
        // Corrupt serialized qualification state fails typed.
        let q = FamilyQualification {
            flight_units: f64::NAN,
            ..FamilyQualification::FRESH
        };
        assert!(matches!(
            resolve_part(&def, &b, &resources(), &q, None),
            Err(ResolveError::InvalidQualification { .. })
        ));
        let q = FamilyQualification {
            bench_units: -1.0,
            ..FamilyQualification::FRESH
        };
        assert!(matches!(
            resolve_part(&def, &b, &resources(), &q, None),
            Err(ResolveError::InvalidQualification { .. })
        ));
    }

    /// The qual-factor output gate: whatever a malformed curve
    /// evaluates to — NaN or outside (0, 1] — fails `InvalidStat`
    /// instead of leaking into `hazard_per_s`. (An equal-knot pair's
    /// 0/0 lerp division is unreachable — every `units ≤` the shared
    /// value is claimed by the earlier window — but a NaN authored
    /// `hazard_multiplier` passes straight through the early-return
    /// and end-knot paths, so the gate guards the *output*, not any
    /// one curve defect. The validator forbids all of these; the
    /// resolver cannot assume validation ran.)
    #[test]
    fn degenerate_qualification_curve_fails_typed() {
        // A NaN authored knot multiplier — returned verbatim at every
        // units ≤ first knot — must not reach hazard.
        let mut def = test_def();
        def.reliability.qualification_curve = QualCurve {
            knots: vec![QualKnot {
                qualification_units: 0.0,
                hazard_multiplier: f64::NAN,
            }],
        };
        assert!(matches!(
            resolve_part(
                &def,
                &built("stock"),
                &resources(),
                &FamilyQualification::FRESH,
                None
            ),
            Err(ResolveError::InvalidStat { .. })
        ));
        // Multipliers outside (0, 1] on an unvalidated def: above 1
        // (a hazard penalty) and 0 (infinite hazard suppression).
        for bad_mult in [1.5, 0.0] {
            let mut def = test_def();
            def.reliability.qualification_curve.knots[1].hazard_multiplier = bad_mult;
            let q = FamilyQualification {
                flight_units: 1_000.0,
                ..FamilyQualification::FRESH
            };
            assert!(
                matches!(
                    resolve_part(&def, &built("stock"), &resources(), &q, None),
                    Err(ResolveError::InvalidStat { .. })
                ),
                "multiplier {bad_mult} must fail"
            );
        }
        // Equal-knot pairs resolve without error: the earlier window
        // claims every `units` that could reach the 0/0 division, so
        // the curve degenerates gracefully to the first knot's value.
        let mut def = test_def();
        def.reliability.qualification_curve = QualCurve {
            knots: vec![
                QualKnot {
                    qualification_units: 0.0,
                    hazard_multiplier: 1.0,
                },
                QualKnot {
                    qualification_units: 50.0,
                    hazard_multiplier: 0.8,
                },
                QualKnot {
                    qualification_units: 50.0,
                    hazard_multiplier: 0.5,
                },
            ],
        };
        let q = FamilyQualification {
            flight_units: 50.0,
            ..FamilyQualification::FRESH
        };
        let eff = resolve_part(&def, &built("stock"), &resources(), &q, None).unwrap();
        assert_eq!(eff.reliability.qual_factor, 0.8);
    }

    #[test]
    fn wear_stub_scales_hazard_and_validates() {
        let mut usage = PartUsage::FRESH;
        usage.operating_s = 50_000.0; // quarter of the 200k MTBF
        let eff = resolve_part(
            &test_def(),
            &built("stock"),
            &resources(),
            &FamilyQualification::FRESH,
            Some(&usage),
        )
        .unwrap();
        assert_eq!(eff.reliability.wear_multiplier, 1.25);
        assert!(close(
            eff.reliability.hazard_per_s,
            (1.0 / 200_000.0) * 1.25
        ));
        // Invalid usage fails typed.
        let bad_usage = PartUsage {
            operating_s: f64::NAN,
            ..PartUsage::FRESH
        };
        assert!(matches!(
            resolve_part(
                &test_def(),
                &built("stock"),
                &resources(),
                &FamilyQualification::FRESH,
                Some(&bad_usage)
            ),
            Err(ResolveError::InvalidUsage { .. })
        ));
    }

    /// `dormant_hazard_per_s` keys on `λ_base` verbatim (§3.4), the
    /// authored infant-mortality fraction passes through unapplied
    /// (the stub does not model infant decay — `hazard_per_s` is
    /// `λ_base · qual · wear` only), and `apply_wear` *recomputes*
    /// rather than compounds: applying it twice with the same usage
    /// is idempotent.
    #[test]
    fn reliability_passthroughs_and_wear_idempotence() {
        let usage = PartUsage {
            operating_s: 40_000.0,
            ..PartUsage::FRESH
        };
        let mut eff = resolve_part(
            &test_def(),
            &built("stock"),
            &resources(),
            &FamilyQualification::FRESH,
            Some(&usage),
        )
        .unwrap();
        let r = &eff.reliability;
        assert_eq!(r.infant_mortality, 0.2); // authored passthrough
        assert!(close(r.dormant_hazard_per_s, r.base_hazard_per_s * 0.02));
        assert!(close(
            r.hazard_per_s,
            r.base_hazard_per_s * r.qual_factor * r.wear_multiplier
        ));
        // Re-wearing the resolved part is recompute, not accumulate:
        // the second apply lands byte-identical.
        let once = r.clone();
        apply_wear(&mut eff, &usage).unwrap();
        assert_eq!(eff.reliability, once);
        // And FRESH wear restores the baseline exactly.
        apply_wear(&mut eff, &PartUsage::FRESH).unwrap();
        assert_eq!(eff.reliability.wear_multiplier, 1.0);
        assert!(close(
            eff.reliability.hazard_per_s,
            eff.reliability.base_hazard_per_s
        ));
    }

    #[test]
    fn every_error_variant_is_typed() {
        let def = test_def();
        let no_qual = &FamilyQualification::FRESH;
        // Unknown variant.
        assert!(matches!(
            resolve_part(&def, &built("nonexistent"), &resources(), no_qual, None),
            Err(ResolveError::UnknownVariant { .. })
        ));
        // Scale violations.
        for s in [0.0, -1.0, f64::NAN, f64::INFINITY, 3.0] {
            let mut b = built("stock");
            b.scale = s;
            assert!(
                matches!(
                    resolve_part(&def, &b, &resources(), no_qual, None),
                    Err(ResolveError::InvalidScale { .. })
                ),
                "scale {s} must fail"
            );
        }
        // A variant naming one StatPath twice fails typed — defence in
        // depth for a def that skipped the validator.
        let mut dup = test_def();
        dup.variants[0].stat_multipliers =
            vec![(StatPath::ThrustVacN, 1.1), (StatPath::ThrustVacN, 1.2)];
        assert!(matches!(
            resolve_part(&dup, &built("stock"), &resources(), no_qual, None),
            Err(ResolveError::DuplicateStatPath {
                stat: StatPath::ThrustVacN,
                ..
            })
        ));
        // Unresolvable upgrade path: a tank-less def is fine, but
        // ControlTorqueNm exists — use a def without Control for a real
        // failure. IspVacS on a def *with* an engine resolves, so craft
        // one without.
        let mut no_engine = test_def();
        no_engine
            .modules
            .retain(|m| !matches!(m, ModuleDef::Engine { .. }));
        let mut b = built("stock");
        b.upgrade_levels_at_build.insert(
            StatPath::IspVacS,
            TrackLevels {
                levels: 1,
                multiplier: 1.1,
            },
        );
        assert!(matches!(
            resolve_part(&no_engine, &b, &resources(), no_qual, None),
            Err(ResolveError::UnresolvableStatPath {
                stat: StatPath::IspVacS,
                ..
            })
        ));
        // Def/built id mismatch.
        let mut other = built("stock");
        other.part_id = PartId::from("test.other");
        assert!(matches!(
            resolve_part(&def, &other, &resources(), no_qual, None),
            Err(ResolveError::PartIdMismatch { .. })
        ));
        // Zero resolved mass.
        let mut light = test_def();
        light.mass_dry_kg = 0.0;
        assert!(matches!(
            resolve_part(&light, &built("stock"), &resources(), no_qual, None),
            Err(ResolveError::InvalidStat { .. })
        ));
        // Unknown resource in a tank.
        let mut tanky = test_def();
        tanky.modules.push(ModuleDef::Tank {
            capacity: vec![(ResourceId::from("unobtainium"), 5.0)],
        });
        assert!(matches!(
            resolve_part(&tanky, &built("stock"), &resources(), no_qual, None),
            Err(ResolveError::UnknownResource { .. })
        ));
        // Every error maps SaveBlocking.
        let e = resolve_part(&def, &other, &resources(), no_qual, None).unwrap_err();
        assert!(matches!(SimError::from(e), SimError::SaveBlocking(_)));
    }

    /// Every `StatPath` in the authored vocabulary resolves on a def
    /// carrying the matching modules — the "every reachable path"
    /// half of §3.1's contract (the unreachable half is tested above).
    #[test]
    fn every_stat_path_resolves() {
        let def = test_def();
        for path in [
            StatPath::MassDryKg,
            StatPath::CostFunds,
            StatPath::ThrustVacN,
            StatPath::IspVacS,
            StatPath::IspSlS,
            StatPath::RatedBurnS,
            StatPath::ControlTorqueNm,
            StatPath::BaseMtbfS,
        ] {
            let mut b = built("stock");
            b.upgrade_levels_at_build.insert(
                path,
                TrackLevels {
                    levels: 1,
                    multiplier: 1.25,
                },
            );
            let eff = resolve_part(&def, &b, &resources(), &FamilyQualification::FRESH, None)
                .unwrap_or_else(|e| panic!("{} must resolve: {e}", path.name()));
            let ModuleDef::Engine {
                thrust_vac_n,
                isp_vac_s,
                isp_sl_s,
                rated_burn_s,
                ..
            } = &eff.modules[0]
            else {
                panic!();
            };
            let ModuleDef::Control { torque_nm, .. } = &eff.modules[1] else {
                panic!();
            };
            match path {
                StatPath::MassDryKg => assert_eq!(eff.weld_input.mass_kg, 125.0),
                StatPath::CostFunds => assert_eq!(eff.cost_funds, 1_250.0),
                StatPath::ThrustVacN => assert_eq!(*thrust_vac_n, 62_500.0),
                StatPath::IspVacS => assert_eq!(*isp_vac_s, 375.0),
                StatPath::IspSlS => assert_eq!(*isp_sl_s, 312.5),
                StatPath::RatedBurnS => assert_eq!(*rated_burn_s, 225.0),
                StatPath::ControlTorqueNm => assert_eq!(*torque_nm, 625.0),
                StatPath::BaseMtbfS => assert_eq!(eff.reliability.base_mtbf_s, 250_000.0),
            }
        }
    }

    /// The `Convex` collider path: an asymmetric authored point set
    /// resolves a nonzero vertex-mean CoM, the point-cloud inertia
    /// about it (hand-computed), the scaled vertex set verbatim, and
    /// the thrust site at the scaled min z.
    #[test]
    fn convex_collider_resolves_offcentre_com_and_point_inertia() {
        let mut def = test_def();
        // Asymmetric wedge — the authored centroid is not the origin.
        def.collider = ColliderDef::Convex {
            points: vec![
                DVec3::ZERO,
                DVec3::new(1.0, 0.0, 0.0),
                DVec3::new(0.0, 1.0, 0.0),
                DVec3::new(0.0, 0.0, 1.0),
                DVec3::new(1.0, 1.0, 1.0),
            ],
        };
        let mut b = built("stock");
        b.scale = 2.0;
        let eff = resolve_part(&def, &b, &resources(), &FamilyQualification::FRESH, None).unwrap();
        // CoM = vertex mean · s: authored (0.4, 0.4, 0.4) → (0.8, 0.8, 0.8).
        assert_eq!(eff.weld_input.com, DVec3::new(0.8, 0.8, 0.8));
        // The resolved collider is the scaled authored set, verbatim.
        let ColliderShape::Points(pts) = &eff.weld_input.collider else {
            panic!("points expected");
        };
        assert_eq!(pts.len(), 5);
        assert_eq!(pts[4], DVec3::new(2.0, 2.0, 2.0));
        // Point-cloud inertia about com, hand-computed: mass 800,
        // m/n = 160 per vertex; r_i = 2·p_i − (0.8, 0.8, 0.8) gives
        // xx = yy = zz = 160·9.6 = 1536 and xy = xz = yz = −160·0.8 = −128.
        let i = eff.weld_input.inertia;
        assert!(close(i.xx, 1_536.0));
        assert!(close(i.yy, 1_536.0));
        assert!(close(i.zz, 1_536.0));
        assert!(close(i.xy, -128.0));
        assert!(close(i.xz, -128.0));
        assert!(close(i.yz, -128.0));
        assert!(i.is_positive_semidefinite());
        // The thrust site sits at the scaled set's min z — 0 here.
        assert_eq!(eff.weld_input.force_sites[0].local_pos, DVec3::ZERO);
        assert_eq!(eff.weld_input.force_sites[0].rated_n, 200_000.0);
    }

    /// Two distinct built parts resolve independently — repeat
    /// resolution is bit-identical, and a permuted `built` slice
    /// yields the same per-part outputs (each part's numbers depend
    /// only on its own record). Output order follows `built` order.
    #[test]
    fn resolution_is_bit_identical_and_permutation_safe() {
        use std::path::PathBuf;

        let mut second = test_def();
        second.id = PartId::from("test.engine.unit.b");
        let defs = LoadedParts {
            parts: vec![
                csp_data::LoadedPart {
                    def: test_def(),
                    path: PathBuf::from("test.engine.unit.ron"),
                },
                csp_data::LoadedPart {
                    def: second,
                    path: PathBuf::from("test.engine.unit.b.ron"),
                },
            ],
        };
        let res = resources();
        let quals = FamilyQualifications::new();
        let mut b_scaled =
            BuiltPart::stock(PartId::from("test.engine.unit.b"), VariantId::from("hot"));
        b_scaled.scale = 1.5;
        let list = [built("stock"), b_scaled];
        let a = resolve_parts(&defs, &list, &res, &quals, None).unwrap();
        let c = resolve_parts(&defs, &list, &res, &quals, None).unwrap();
        assert_eq!(a, c); // PartialEq on f64s: bitwise-identical resolve.
        // The actual permutation: reversed input → reversed output,
        // identical per-part results.
        let permuted = [list[1].clone(), list[0].clone()];
        let p = resolve_parts(&defs, &permuted, &res, &quals, None).unwrap();
        assert_eq!(p[0], a[1]);
        assert_eq!(p[1], a[0]);
    }

    // ---------- M7-B: compose_upgrade_levels (§3.3) ----------

    use csp_data::{UpgradeEffect, UpgradeLevel, UpgradeTrack};

    fn track(family: &str, stat: StatPath, levels: &[(f64, UpgradeEffect)]) -> UpgradeTrack {
        UpgradeTrack {
            id: csp_data::UpgradeTrackId::from(format!("track.{family}")),
            display_name: "track.name".into(),
            family: csp_data::FamilyId::from(family),
            stat_path: stat,
            levels: levels
                .iter()
                .map(|(cost, e)| UpgradeLevel {
                    science_cost: *cost,
                    multiplier_or_delta: *e,
                })
                .collect(),
        }
    }

    /// Multiplier levels fold by product; the level count freezes.
    #[test]
    fn compose_multiplier_track() {
        let t = track(
            "test.family.engine",
            StatPath::IspVacS,
            &[
                (10.0, UpgradeEffect::Multiplier(1.03)),
                (10.0, UpgradeEffect::Multiplier(1.03)),
                (10.0, UpgradeEffect::Multiplier(1.03)),
            ],
        );
        let m = compose_upgrade_levels(&test_def(), [(&t, 2)]).unwrap();
        let tl = m[&StatPath::IspVacS];
        assert_eq!(tl.levels, 2);
        assert_eq!(tl.multiplier, 1.03 * 1.03);
    }

    /// Delta levels fold as `(base + ΣΔ)/base` on the authored stat —
    /// test_def's authored `isp_vac_s` is 300, so +15 → ×1.05.
    #[test]
    fn compose_delta_track_uses_authored_base() {
        let t = track(
            "test.family.engine",
            StatPath::IspVacS,
            &[
                (10.0, UpgradeEffect::Delta(10.0)),
                (10.0, UpgradeEffect::Delta(5.0)),
            ],
        );
        let m = compose_upgrade_levels(&test_def(), [(&t, 2)]).unwrap();
        let tl = m[&StatPath::IspVacS];
        assert!((tl.multiplier - 315.0 / 300.0).abs() < 1e-12);
    }

    /// Other families' tracks and zero levels are skipped; a level
    /// beyond the authored count and an unresolvable stat fail typed.
    #[test]
    fn compose_skips_and_rejects() {
        let foreign = track(
            "other.family",
            StatPath::MassDryKg,
            &[(1.0, UpgradeEffect::Multiplier(2.0))],
        );
        let own = track(
            "test.family.engine",
            StatPath::MassDryKg,
            &[(1.0, UpgradeEffect::Multiplier(1.5))],
        );
        let m = compose_upgrade_levels(&test_def(), [(&foreign, 1), (&own, 0)]).unwrap();
        assert!(m.is_empty());

        let m = compose_upgrade_levels(&test_def(), [(&own, 5)]);
        assert!(matches!(m, Err(ResolveError::InvalidStat { .. })));

        // A stat the def lacks fails `UnresolvableStatPath` — drop the
        // Control module so `ControlTorqueNm` cannot resolve.
        let bad = track(
            "test.family.engine",
            StatPath::ControlTorqueNm,
            &[(1.0, UpgradeEffect::Multiplier(1.1))],
        );
        let mut d = test_def();
        d.modules
            .retain(|m| !matches!(m, ModuleDef::Control { .. }));
        assert!(matches!(
            compose_upgrade_levels(&d, [(&bad, 1)]),
            Err(ResolveError::UnresolvableStatPath { .. })
        ));
    }

    /// A frozen `BuiltPart` keeps the manufacture-time stats — the
    /// §3.3 never-retroactive contract expressed through compose.
    #[test]
    fn composed_upgrades_freeze_into_built_part() {
        let t = track(
            "test.family.engine",
            StatPath::ThrustVacN,
            &[(10.0, UpgradeEffect::Multiplier(1.1))],
        );
        let mut b = built("stock");
        b.upgrade_levels_at_build = compose_upgrade_levels(&test_def(), [(&t, 1)]).unwrap();
        let eff = resolve_part(
            &test_def(),
            &b,
            &resources(),
            &FamilyQualification::FRESH,
            None,
        )
        .unwrap();
        let ModuleDef::Engine { thrust_vac_n, .. } = &eff.modules[0] else {
            panic!()
        };
        assert!(close(*thrust_vac_n, 55_000.0));
    }

    // ---------- R14-F2/F5: output-contract refusal gates ----------
    //
    // Every gate below mirrors a `csp-data` validator rule on the
    // *resolved* record — the resolver cannot assume validation ran,
    // so a def reaching it unvalidated must fail typed rather than
    // leak NaN/∞/out-of-range stats into `EffectivePart`. Each test
    // asserts the typed variant (and, for the `InvalidStat` family,
    // the exact field text pinning which gate fired).

    /// `resolve_parts` refuses a def set claiming one `PartId` twice:
    /// the `BTreeMap` lookup index would silently keep the last def,
    /// so the ambiguity is a data-set defect refused up front.
    #[test]
    fn resolve_parts_rejects_duplicate_part_ids() {
        use std::path::PathBuf;

        let defs = LoadedParts {
            parts: vec![
                csp_data::LoadedPart {
                    def: test_def(),
                    path: PathBuf::from("test.engine.unit.ron"),
                },
                // Same id in a second file — the dup the index build
                // must catch regardless of `built` contents.
                csp_data::LoadedPart {
                    def: test_def(),
                    path: PathBuf::from("test.engine.unit.copy.ron"),
                },
            ],
        };
        let err = resolve_parts(
            &defs,
            &[built("stock")],
            &resources(),
            &FamilyQualifications::new(),
            None,
        )
        .unwrap_err();
        assert_eq!(
            err,
            ResolveError::DuplicatePartId {
                part_id: PartId::from("test.engine.unit"),
            }
        );
    }

    /// An authored `Convex` with no points would resolve a zero-vertex
    /// collider — no hull, no facet planes, no `z_min` for the engine
    /// thrust-site lookup. `ColliderShape::is_valid` is vacuous on the
    /// empty set, so this is its own gate.
    #[test]
    fn empty_convex_collider_is_refused() {
        let mut def = test_def();
        def.collider = ColliderDef::Convex { points: Vec::new() };
        assert_invalid_stat(
            resolve_stock(&def),
            "resolve: collider resolves to zero vertices",
        );
    }

    /// `throttle_range` must satisfy `0 <= min < max <= 1` on the
    /// resolved module — an inverted or empty band, or one escaping
    /// [0, 1], is refused before a throttle readout can consume it.
    #[test]
    fn engine_throttle_range_out_of_band_is_refused() {
        for range in [
            ThrottleRange { min: 0.8, max: 0.5 }, // inverted
            ThrottleRange { min: 0.5, max: 0.5 }, // empty (min == max)
            ThrottleRange {
                min: -0.1,
                max: 1.0,
            }, // below 0
            ThrottleRange { min: 0.0, max: 1.5 }, // above 1
            ThrottleRange {
                min: f64::NAN,
                max: 1.0,
            }, // non-finite
        ] {
            let mut def = test_def();
            let ModuleDef::Engine { throttle_range, .. } = &mut def.modules[0] else {
                panic!("engine module expected");
            };
            *throttle_range = range;
            assert_invalid_stat(
                resolve_stock(&def),
                "resolve: engine throttle_range outside 0 <= min < max <= 1",
            );
        }
    }

    /// `gimbal_deg` is degrees of authority: negative or non-finite is
    /// refused (zero stays legal — a fixed nozzle).
    #[test]
    fn engine_gimbal_deg_negative_or_nonfinite_is_refused() {
        for g in [-1.0, f64::NAN, f64::INFINITY] {
            let mut def = test_def();
            let ModuleDef::Engine { gimbal_deg, .. } = &mut def.modules[0] else {
                panic!("engine module expected");
            };
            *gimbal_deg = g;
            assert_invalid_stat(
                resolve_stock(&def),
                "resolve: engine gimbal_deg is negative or non-finite",
            );
        }
    }

    /// `ignitions: Some(0)` — "zero lifetime ignitions" — is refused;
    /// `None` is the authored spelling of unlimited.
    #[test]
    fn engine_zero_ignitions_is_refused() {
        let mut def = test_def();
        let ModuleDef::Engine { ignitions, .. } = &mut def.modules[0] else {
            panic!("engine module expected");
        };
        *ignitions = Some(0);
        assert_invalid_stat(
            resolve_stock(&def),
            "resolve: engine ignitions is Some(0) — use None for unlimited",
        );
    }

    /// The `Engine.propellants` contract on an unvalidated def: ids
    /// known to `resources.ron`, unique, positive finite fractions
    /// summing to 1. Each defect fails typed — `UnknownResource` for
    /// the missing id, `InvalidStat` naming the rule for the rest.
    #[test]
    fn engine_propellant_gates_refuse_each_defect() {
        let set_propellants = |def: &mut PartDef, p: Vec<(ResourceId, f64)>| {
            let ModuleDef::Engine { propellants, .. } = &mut def.modules[0] else {
                panic!("engine module expected");
            };
            *propellants = p;
        };
        // Duplicate ResourceId (dedup runs before the sum check).
        let mut def = test_def();
        set_propellants(
            &mut def,
            vec![
                (ResourceId::from("liquid_fuel"), 0.5),
                (ResourceId::from("liquid_fuel"), 0.5),
            ],
        );
        assert_invalid_stat(
            resolve_stock(&def),
            "resolve: duplicate propellant resource id",
        );
        // Fractions not summing to 1 (0.9 here).
        let mut def = test_def();
        set_propellants(
            &mut def,
            vec![
                (ResourceId::from("liquid_fuel"), 0.5),
                (ResourceId::from("oxidizer"), 0.4),
            ],
        );
        assert_invalid_stat(
            resolve_stock(&def),
            "resolve: propellant mass fractions do not sum to 1",
        );
        // Unknown ResourceId — the dedicated variant, not InvalidStat.
        let mut def = test_def();
        set_propellants(&mut def, vec![(ResourceId::from("unobtainium"), 1.0)]);
        let err = resolve_stock(&def).unwrap_err();
        assert_eq!(
            err,
            ResolveError::UnknownResource {
                part_id: PartId::from("test.engine.unit"),
                resource: ResourceId::from("unobtainium"),
            }
        );
        // Non-positive / non-finite fractions (the per-entry check
        // fires before the sum check, so these never reach it).
        for frac in [0.0, -0.5, f64::NAN] {
            let mut def = test_def();
            set_propellants(&mut def, vec![(ResourceId::from("liquid_fuel"), frac)]);
            assert_invalid_stat(
                resolve_stock(&def),
                "resolve: propellant fraction is not positive and finite",
            );
        }
        // An empty propellant list is its own message.
        let mut def = test_def();
        set_propellants(&mut def, Vec::new());
        assert_invalid_stat(
            resolve_stock(&def),
            "resolve: engine declares no propellants",
        );
    }

    /// Tank capacity entries re-check the resource list at resolve:
    /// unknown ids → `UnknownResource`, duplicate ids and
    /// non-positive resolved units → `InvalidStat` naming the rule.
    #[test]
    fn tank_capacity_gates_refuse_each_defect() {
        // Unknown resource id.
        let mut def = test_def();
        def.modules.push(ModuleDef::Tank {
            capacity: vec![(ResourceId::from("unobtainium"), 5.0)],
        });
        let err = resolve_stock(&def).unwrap_err();
        assert_eq!(
            err,
            ResolveError::UnknownResource {
                part_id: PartId::from("test.engine.unit"),
                resource: ResourceId::from("unobtainium"),
            }
        );
        // Non-positive resolved units (zero, negative, non-finite).
        for units in [0.0, -5.0, f64::NAN] {
            let mut def = test_def();
            def.modules.push(ModuleDef::Tank {
                capacity: vec![(ResourceId::from("liquid_fuel"), units)],
            });
            assert_invalid_stat(
                resolve_stock(&def),
                "resolve: tank capacity is not positive and finite",
            );
        }
        // Duplicate resource ids within one tank.
        let mut def = test_def();
        def.modules.push(ModuleDef::Tank {
            capacity: vec![
                (ResourceId::from("liquid_fuel"), 5.0),
                (ResourceId::from("liquid_fuel"), 5.0),
            ],
        });
        assert_invalid_stat(
            resolve_stock(&def),
            "resolve: duplicate tank capacity resource id",
        );
    }

    /// An `Experiment` whose `situations` mask is empty or sets the
    /// reserved bits (6–7) can run nowhere meaningful — the same
    /// refusal the validator gives.
    #[test]
    fn experiment_bad_situation_mask_is_refused() {
        for mask in [
            csp_data::SituationMask(0),           // runs nowhere
            csp_data::SituationMask(0b0100_0000), // reserved bit 6
            csp_data::SituationMask(0xFF),        // valid + reserved bits
        ] {
            let mut def = test_def();
            def.modules.push(ModuleDef::Experiment {
                kind: csp_data::ExperimentId::from("exp.test"),
                data_units: 10.0,
                reusable: true,
                situations: mask,
            });
            assert_invalid_stat(
                resolve_stock(&def),
                "resolve: experiment situation mask is invalid",
            );
        }
    }

    /// A cd/cl curve with non-increasing mach knots would feed
    /// `csp-phys::aero`'s piecewise reader garbage — refused on either
    /// curve (strictly-increasing means equal machs fail too).
    #[test]
    fn aero_curve_non_monotonic_mach_is_refused() {
        const GATE: &str = "resolve: aero curve knot is non-finite, negative, or non-monotone";
        let decreasing = vec![
            csp_data::AeroKnot {
                mach: 1.0,
                coeff: 0.5,
            },
            csp_data::AeroKnot {
                mach: 0.5,
                coeff: 0.6,
            },
        ];
        // cd side.
        let mut def = test_def();
        def.aero.cd_curve = decreasing.clone();
        assert_invalid_stat(resolve_stock(&def), GATE);
        // cl side.
        let mut def = test_def();
        def.aero.cl_curve = decreasing;
        assert_invalid_stat(resolve_stock(&def), GATE);
        // Equal machs are non-monotone (not strictly increasing).
        let mut def = test_def();
        def.aero.cd_curve = vec![
            csp_data::AeroKnot {
                mach: 0.5,
                coeff: 0.5,
            },
            csp_data::AeroKnot {
                mach: 0.5,
                coeff: 0.6,
            },
        ];
        assert_invalid_stat(resolve_stock(&def), GATE);
    }

    /// `conduction_w_per_k > resolved heat_capacity` violates the
    /// explicit-Euler stability bound (the validator's
    /// `ConductionExceedsCapacity` re-checked at resolved values).
    /// `heat_capacity` scales by `s^mass` while conduction is unscaled
    /// (the v1 schema gap), so s = 0.5 pushes a def that passes the
    /// authored-side bound over the resolved one: 100_000 · 0.5³ =
    /// 12_500 < 20_000.
    #[test]
    fn conduction_exceeding_resolved_heat_capacity_is_refused() {
        let mut def = test_def();
        def.thermal.conduction_w_per_k = 20_000.0; // < authored 100_000 heat cap
        let mut b = built("stock");
        b.scale = 0.5;
        assert_invalid_stat(
            resolve_part(&def, &b, &resources(), &FamilyQualification::FRESH, None),
            "resolve: conduction exceeds resolved heat capacity — unstable thermal lump",
        );
    }

    /// Attach-node directions must resolve unit length (±1e-6): the
    /// snap graph's opposing-direction test assumes normalized
    /// normals. Non-unit and non-finite both fail the same gate.
    #[test]
    fn non_unit_attach_direction_is_refused() {
        const GATE: &str = "resolve: attach node direction is not unit length";
        let mut def = test_def();
        def.attach_nodes[0].direction = DVec3::new(1.0, 1.0, 0.0); // ‖d‖ = √2
        assert_invalid_stat(resolve_stock(&def), GATE);
        let mut def = test_def();
        def.attach_nodes[0].direction = DVec3::new(f64::NAN, 0.0, 0.0);
        assert_invalid_stat(resolve_stock(&def), GATE);
    }

    /// `apply_wear`'s overflow gate: `base_mtbf_s` is only contracted
    /// "finite and positive", so a tiny-but-legal MTBF makes
    /// `operating_s / base_mtbf_s` overflow to ∞ for a valid usage
    /// record — the non-finite multiplier/hazard must fail typed,
    /// never be stored. `f64::MAX / 1e-300` overflows.
    #[test]
    fn apply_wear_overflow_on_tiny_mtbf_is_refused() {
        let mut def = test_def();
        def.reliability.base_mtbf_s = 1e-300; // λ_base = 1e300, still finite → resolves
        let mut eff = resolve_stock(&def).unwrap();
        let usage = PartUsage {
            operating_s: f64::MAX,
            ..PartUsage::FRESH
        };
        assert_invalid_stat(
            apply_wear(&mut eff, &usage),
            "resolve: wear multiplier/hazard overflowed to non-finite",
        );
        // The composed `resolve_part` path refuses identically.
        assert_invalid_stat(
            resolve_part(
                &def,
                &built("stock"),
                &resources(),
                &FamilyQualification::FRESH,
                Some(&usage),
            ),
            "resolve: wear multiplier/hazard overflowed to non-finite",
        );
    }
}
