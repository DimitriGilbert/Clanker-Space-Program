//! The M3 VAB readout — per-stage Δv / TWR / burn-time over the working
//! craft (PLAN M3 exit row "live Δv/TWR/burn-time readout", §4's
//! Kerbal-Engineer-equivalent row "Per-stage Δv/TWR/burn-time readouts").
//!
//! # Model (documented, honest at this tier)
//!
//! [`analyze`] resolves the session to [`EffectivePart`]s — tanks resolve
//! **full** (resolve.rs's rule: `mass_kg` is dry + propellant) — then
//! walks the firing sequence ([`CraftSession::stage_order`], strictly
//! descending stage index) once:
//!
//! - **Attached set.** Everything not yet separated. When stage `s`
//!   runs, parts under decouplers assigned to *higher* stages are
//!   already gone — the same supply-region ordering
//!   [`CraftSession::validate`] applies, reached here by *removing*
//!   severed subtrees as stages fire.
//! - **Engine set — the attached-set model (C13-F1).** The sim has no
//!   per-stage ignition: `propellant_plans` plans every engine-spec
//!   node regardless of stage and thrust applies at every attached
//!   `Thrust` site at the vessel's single throttle, so a stage
//!   boundary *jettisons*, it never ignites. Each row therefore
//!   models the segment flight's `delta_v_walk_from` walks over the
//!   same attached set: **every still-attached engine** burns, not
//!   just the parts assigned to the stage. (The rejected alternative
//!   — per-engine activation in the sim — is 7.x feature work; the
//!   readout aligns to the shipped model, one-directional today.)
//! - **Burn — pooled, in depletion passes.** Per-engine mass flow is
//!   `ṁᵢ = Fᵢ / (Ispᵢ · g₀)`; per-resource demand is `Σᵢ ṁᵢ·fracᵢ,r`
//!   over the burning engines' authored propellant mixes, pooled
//!   against the propellant remaining in every attached tank — the
//!   same vessel-wide pool `propellant_plans` and flight's
//!   `burn_step` resolve. A pass ends when its limiting resource
//!   empties (`available_kg / demand_kg_s`); engines it fed starve
//!   out — flight's zero-coverage rule — and the survivors burn on
//!   in the next pass, so a mixed-propellant fleet drains exactly the
//!   way the flight walk does. The same-stage decoupler case is
//!   fire-and-separate (the departing half feeds this segment's
//!   engines — session.rs's module doc), so separation applies
//!   **after** the stage's burn is accounted.
//! - **Δv** accumulates per pass: `dv += Isp_eff·g₀·ln(m_wet /
//!   (m_wet − m_burned))` with `Isp_eff = ΣF / (g₀·Σṁ)` over the
//!   engines still burning — the mass-flow-weighted value for
//!   mixed-engine segments. A sea-level figure substitutes
//!   `isp_sl_s` on the same ratios (only `thrust_vac_n` is authored,
//!   so thrust does not change).
//! - **Segment-start figures.** `thrust_vac_n`, `isp_*`,
//!   `mass_flow_kg_s`, and `twr_surface = F / (m_wet · g₀)` report
//!   the first burnable evaluation — what flight's
//!   `burn_step(θ = 1)` delivers at the top of the segment (starved
//!   engines contribute nothing, the coverage-zeroed convention).
//!   `csp_data::G0` *is* Terrin's surface gravity by authoring
//!   convention (part.rs), so the launchpad number needs no body
//!   lookup; [`crate::profile`] carries the body-parameterized form
//!   for the profile comparison.
//! - **Drain order (C13-F2).** Each pass debits its draw ascending
//!   `PartNodeId` — the fixed order `transfers::debit` and flight's
//!   `consume_drawn` share. Under the v1 pooled feed there is no
//!   tank priority: in the canonical top-down build the upper
//!   stage's tank sits at the lower id and drains *through the
//!   decoupler* before the booster's own, which is what
//!   [`crate::session::SessionWarning::CrossStagePropellantDrain`]
//!   warns about — a burn can strand propellant in the departing
//!   half and leave the next stage dry. A real crossfeed/feed-
//!   priority model is 7.x feature work.
//!
//! Stages with no engines get a row anyway — their decouplers still
//! drop mass that later stages' totals must not carry.
//!
//! Determinism domain: `BTreeMap`/`BTreeSet` iteration, `libm` for
//! `ln`, no I/O, no clock.

use std::collections::{BTreeMap, BTreeSet};

use csp_data::{G0, LoadedParts, ModuleDef, ResourceFile, ResourceId};

use crate::graph::PartNodeId;
use crate::resolve::{EffectivePart, FamilyQualifications, ResolveError, resolve_parts};
use crate::session::CraftSession;

/// One firing stage's readout row — the segment between its firing
/// and the next stage's, evaluated the way flight's propulsion walk
/// evaluates the same attached set (module doc). Fields are `0`/empty
/// on a stage with nothing burning or burning nothing; `separated_ids`
/// still reports what its decouplers drop.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StageStats {
    /// The stage index (the `stages` vec is in firing order —
    /// descending index).
    pub stage: u32,
    /// Parts assigned to this stage that are still attached when it
    /// fires, ascending id.
    pub part_ids: Vec<PartNodeId>,
    /// Engines burning through this stage's segment — **every engine
    /// still attached** when it fires, not a subset of `part_ids`
    /// (the attached-set model: the sim fires all attached engines at
    /// the one vessel throttle; the boundary jettisons rather than
    /// ignites).
    pub engine_ids: Vec<PartNodeId>,
    /// Parts this stage's decouplers drop, ascending id — their
    /// subtrees leave after the burn is accounted.
    pub separated_ids: Vec<PartNodeId>,
    /// Σ deliverable vacuum thrust at segment start (N) — starved
    /// engines contribute `0`, matching flight's coverage-zeroed
    /// `burn_step(θ = 1)`.
    pub thrust_vac_n: f64,
    /// Effective vacuum Isp at segment start, mass-flow weighted
    /// over the metered engines: `ΣF / (g₀·Σṁ)` (s). `0` when nothing
    /// metered burns.
    pub isp_vac_s: f64,
    /// Effective sea-level Isp on the same weighting (s).
    pub isp_sl_s: f64,
    /// Σ propellant mass flow at segment start (kg/s).
    pub mass_flow_kg_s: f64,
    /// Mass still attached while this stage burns — dry mass plus the
    /// propellant remaining in every attached tank (kg).
    pub wet_mass_kg: f64,
    /// Propellant this segment's engines draw before the pooled
    /// supply is spent (kg) — summed over the depletion passes.
    pub propellant_burned_kg: f64,
    /// Σ depletion-pass durations — the segment's burn time (s).
    pub burn_time_s: f64,
    /// `Σ isp_vac·g₀·ln(wet / (wet − burned))` over the passes (m/s).
    pub delta_v_m_per_s: f64,
    /// The same at `isp_sl` — the launchpad figure (m/s).
    pub delta_v_sl_m_per_s: f64,
    /// `thrust_vac_n / (wet · g₀)` at segment start — TWR vs Terrin
    /// surface gravity.
    pub twr_surface: f64,
}

/// The whole-craft readout [`analyze`] produces — consumed by the VAB's
/// staging/readout panel and by the [`crate::profile::MissionProfile`]
/// comparison.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CraftAnalysis {
    /// The resolved parts, in [`CraftSession::iter`] order (ascending
    /// `PartNodeId` — the `effective_index` convention `to_part_graph`
    /// sets).
    pub effective: Vec<EffectivePart>,
    /// Per-stage rows in firing order — highest stage index first.
    pub stages: Vec<StageStats>,
    /// Σ `delta_v_m_per_s` over the firing sequence (m/s).
    pub total_delta_v_m_per_s: f64,
    /// Σ `delta_v_sl_m_per_s` (m/s).
    pub total_delta_v_sl_m_per_s: f64,
    /// Total mass at T−0: dry + full propellant over every part (kg).
    pub launch_mass_kg: f64,
    /// Σ resolved `cost_funds`.
    pub cost_funds: f64,
}

/// Per-node bookkeeping for the firing walk: the dry mass and the
/// resolved tank contents (units — converted to kg via `res`).
struct NodeMass {
    /// `eff.mass_kg` minus full propellant mass — constant across the
    /// walk.
    dry_kg: f64,
    /// `resource → units remaining`. Initialized to resolved (scaled)
    /// capacity; consumed by firing stages.
    tanks: BTreeMap<ResourceId, f64>,
}

impl NodeMass {
    /// Current total mass of the node: dry + propellant remaining (kg).
    fn total(&self, res: &ResourceFile) -> f64 {
        self.dry_kg
            + self
                .tanks
                .iter()
                .map(|(r, u)| *u * res.mass_per_unit(r).unwrap_or(0.0))
                .sum::<f64>()
    }
}

/// Compute the live VAB readout for `session`: resolve every node,
/// then run the firing-order walk described in the module doc.
///
/// `defs`/`res` are the authored catalog and resource registry. An
/// empty session resolves to an empty analysis — `to_part_graph` would
/// refuse it, and the editor reads a bare "no parts" table instead.
/// Resolution failures ([`ResolveError`]) propagate — the UI surfaces
/// them alongside `validate`'s errors.
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
pub fn analyze(
    session: &CraftSession,
    defs: &LoadedParts,
    res: &ResourceFile,
) -> Result<CraftAnalysis, ResolveError> {
    if session.is_empty() {
        return Ok(CraftAnalysis::default());
    }
    // iter() is ascending-id — the `effective[i] ↔ node` pairing is the
    // effective_index convention of to_part_graph.
    let built: Vec<_> = session.iter().map(|(_, n)| n.built().clone()).collect();
    // The editor resolves as-manufactured: FRESH qualification and no
    // wear — the numbers the player designs against, matching the
    // launch state's resolve.
    let effective = resolve_parts(defs, &built, res, &FamilyQualifications::new(), None)?;

    // Node → its resolved part + module flags, one lookup map.
    let mut eff_of: BTreeMap<PartNodeId, &EffectivePart> = BTreeMap::new();
    for ((id, _node), eff) in session.iter().zip(effective.iter()) {
        eff_of.insert(id, eff);
    }
    let has_module = |id: PartNodeId, f: &dyn Fn(&ModuleDef) -> bool| {
        eff_of.get(&id).is_some_and(|e| e.modules.iter().any(f))
    };

    // Per-node dry mass + full tank contents.
    let mut mass: BTreeMap<PartNodeId, NodeMass> = BTreeMap::new();
    for (id, eff) in &eff_of {
        let mut tanks: BTreeMap<ResourceId, f64> = BTreeMap::new();
        for m in &eff.modules {
            if let ModuleDef::Tank { capacity } = m {
                for (r, units) in capacity {
                    *tanks.entry(r.clone()).or_default() += units;
                }
            }
        }
        let prop_kg: f64 = tanks
            .iter()
            .map(|(r, u)| u * res.mass_per_unit(r).unwrap_or(0.0))
            .sum();
        mass.insert(
            *id,
            NodeMass {
                dry_kg: (eff.weld_input.mass_kg - prop_kg).max(0.0),
                tanks,
            },
        );
    }

    // Children index — one pass over the edges keeps the sever walk
    // deterministic (ascending parents, ascending children).
    let mut children: BTreeMap<PartNodeId, Vec<PartNodeId>> = BTreeMap::new();
    for (id, n) in session.iter() {
        if let Some(e) = n.edge() {
            children.entry(e.parent).or_default().push(id);
        }
    }

    let mut attached: BTreeSet<PartNodeId> = session.iter().map(|(id, _)| id).collect();
    let launch_mass_kg: f64 = mass.values().map(|m| m.total(res)).sum();
    let cost_funds: f64 = effective.iter().map(|e| e.cost_funds).sum();

    let mut stages = Vec::new();
    for stage in session.stage_order() {
        let part_ids: Vec<PartNodeId> = session
            .parts_in_stage(stage)
            .into_iter()
            .filter(|id| attached.contains(id))
            .collect();
        let mut row = StageStats {
            stage,
            part_ids,
            ..StageStats::default()
        };
        // The attached-set engine model (module doc, C13-F1): every
        // engine still aboard burns through this segment — the sim
        // fires all attached engines at the one vessel throttle, and
        // a stage boundary jettisons rather than ignites.
        row.engine_ids = attached
            .iter()
            .copied()
            .filter(|id| has_module(*id, &|m| matches!(m, ModuleDef::Engine { .. })))
            .collect();
        row.wet_mass_kg = attached
            .iter()
            .filter_map(|id| mass.get(id))
            .map(|m| m.total(res))
            .sum();

        if !row.engine_ids.is_empty() {
            // An engine starves when a propellant it draws is
            // un-holdable (`mass_per_unit ≤ 0` — a massless charge, or
            // a resource no tank carries: the sim's ∞-demand →
            // 0-coverage rule) or when the pooled supply of one runs
            // out mid-segment. The survivors burn on — the depletion
            // passes below are the same loop flight's
            // `delta_v_walk_from` iterates.
            let mut starved: BTreeSet<PartNodeId> = BTreeSet::new();
            for id in &row.engine_ids {
                let Some(eff) = eff_of.get(id) else {
                    continue;
                };
                let unholdable = eff.modules.iter().any(|m| match m {
                    ModuleDef::Engine { propellants, .. } => propellants
                        .iter()
                        .any(|(r, f)| *f > 0.0 && res.mass_per_unit(r).unwrap_or(0.0) <= 0.0),
                    _ => false,
                });
                if unholdable {
                    starved.insert(*id);
                }
            }

            // Depletion passes (module doc): each pass burns the
            // engines still alight until the limiting pooled resource
            // empties, debits the draw ascending `PartNodeId` — the
            // drain order `transfers::debit` and `consume_drawn` share
            // (C13-F2) — then starves the engines it fed and repeats.
            // The bound is defence, matching the flight walk's. The
            // row's thrust/Isp/mass-flow/TWR report the *first*
            // burnable evaluation — the segment-start delivery; burn
            // time, propellant, and Δv accumulate across passes.
            //
            // Starved engines claim nothing from the pool — a stated
            // simplification of the sim's shared-denominator coverage
            // (they inflate demand there while delivering zero); at
            // rated-until-depletion granularity the totals are equal.
            let mut segment_start = true;
            for _pass in 0..64 {
                // Per-resource demand (kg/s) and ΣF over the engines
                // still burning. Engines with no positive-fraction
                // propellant (or a non-positive Isp) are unmetered —
                // real thrust, nothing drawn, excluded from the
                // effective Isp (the sim's free-thrust convention).
                let mut demand: BTreeMap<ResourceId, f64> = BTreeMap::new();
                let mut mdot_total = 0.0f64;
                let mut sl_mdot_equiv = 0.0f64;
                let mut thrust_n = 0.0f64;
                let mut metered_thrust_n = 0.0f64;
                for id in &row.engine_ids {
                    if starved.contains(id) {
                        continue;
                    }
                    let Some(eff) = eff_of.get(id) else {
                        continue;
                    };
                    for m in &eff.modules {
                        let ModuleDef::Engine {
                            thrust_vac_n,
                            isp_vac_s,
                            isp_sl_s,
                            propellants,
                            ..
                        } = m
                        else {
                            continue;
                        };
                        thrust_n += thrust_vac_n;
                        if *isp_vac_s > 0.0 && propellants.iter().any(|(_, f)| *f > 0.0) {
                            metered_thrust_n += thrust_vac_n;
                            let mdot = thrust_vac_n / (isp_vac_s * G0);
                            mdot_total += mdot;
                            sl_mdot_equiv += thrust_vac_n / (isp_sl_s * G0);
                            for (r, frac) in propellants {
                                if *frac > 0.0 {
                                    *demand.entry(r.clone()).or_default() += mdot * frac;
                                }
                            }
                        }
                    }
                }
                // Pooled availability over the attached set (kg) — the
                // same vessel-wide pool `propellant_plans` resolves.
                // Massless resources carry no burnable mass, so only
                // `mpu > 0` resources enter (un-holdable demanders are
                // already starved, above).
                let mut available: BTreeMap<ResourceId, f64> = BTreeMap::new();
                for id in &attached {
                    let Some(nm) = mass.get(id) else {
                        continue;
                    };
                    for (r, units) in &nm.tanks {
                        let mpu = res.mass_per_unit(r).unwrap_or(0.0);
                        if mpu > 0.0 {
                            *available.entry(r.clone()).or_default() += units * mpu;
                        }
                    }
                }
                // A demanded resource already empty starves every
                // engine drawing it — drop them and re-evaluate
                // without their claims.
                let mut starved_any = false;
                for id in &row.engine_ids {
                    if starved.contains(id) {
                        continue;
                    }
                    let Some(eff) = eff_of.get(id) else {
                        continue;
                    };
                    let starves = eff.modules.iter().any(|m| match m {
                        ModuleDef::Engine { propellants, .. } => {
                            propellants.iter().any(|(r, f)| {
                                *f > 0.0
                                    && demand.get(r).is_some_and(|d| *d > 0.0)
                                    && available.get(r).copied().unwrap_or(0.0) <= 0.0
                            })
                        }
                        _ => false,
                    });
                    if starves {
                        starved.insert(*id);
                        starved_any = true;
                    }
                }
                if starved_any {
                    continue;
                }
                // First burnable evaluation → the segment-start
                // figures flight's `burn_step(θ = 1)` reports.
                if segment_start {
                    segment_start = false;
                    row.thrust_vac_n = thrust_n;
                    row.mass_flow_kg_s = mdot_total;
                    if mdot_total > 0.0 {
                        row.isp_vac_s = metered_thrust_n / (G0 * mdot_total);
                    }
                    if sl_mdot_equiv > 0.0 {
                        row.isp_sl_s = metered_thrust_n / (G0 * sl_mdot_equiv);
                    }
                    if row.wet_mass_kg > 0.0 {
                        row.twr_surface = row.thrust_vac_n / (row.wet_mass_kg * G0);
                    }
                }
                if demand.is_empty() {
                    break;
                }
                // The pass ends when the limiting pooled resource
                // empties — the same first-depletion bound
                // `burn_step`'s `burn_s` reports.
                let mut burn = f64::INFINITY;
                for (r, kg_s) in &demand {
                    if *kg_s > 0.0 {
                        burn = burn.min(available.get(r).copied().unwrap_or(0.0) / kg_s);
                    }
                }
                if !burn.is_finite() || burn <= 0.0 {
                    break;
                }
                let burned: f64 = demand
                    .iter()
                    .map(|(r, kg_s)| (kg_s * burn).min(available.get(r).copied().unwrap_or(0.0)))
                    .sum();
                // Per-pass Tsiolkovsky on the live attached mass —
                // tanks shrink as earlier passes debit them.
                let wet_now: f64 = attached
                    .iter()
                    .filter_map(|id| mass.get(id))
                    .map(|m| m.total(res))
                    .sum();
                let dry_end = (wet_now - burned).max(1e-9);
                let ratio = wet_now / dry_end;
                if ratio > 1.0 && mdot_total > 0.0 {
                    row.delta_v_m_per_s +=
                        (metered_thrust_n / (G0 * mdot_total)) * G0 * libm::log(ratio);
                }
                if ratio > 1.0 && sl_mdot_equiv > 0.0 {
                    row.delta_v_sl_m_per_s +=
                        (metered_thrust_n / (G0 * sl_mdot_equiv)) * G0 * libm::log(ratio);
                }
                row.burn_time_s += burn;
                row.propellant_burned_kg += burned;
                // Debit the draw ascending `PartNodeId`, each resource
                // drawn down until the pass's burn is met — the fixed
                // drain order (module doc, C13-F2).
                for (r, kg_s) in &demand {
                    let mpu = res.mass_per_unit(r).unwrap_or(1.0).max(1e-12);
                    let mut need_units = kg_s * burn / mpu;
                    for id in &attached {
                        if need_units <= 0.0 {
                            break;
                        }
                        let Some(nm) = mass.get_mut(id) else {
                            continue;
                        };
                        let Some(have) = nm.tanks.get_mut(r) else {
                            continue;
                        };
                        let take = (*have).min(need_units);
                        *have -= take;
                        need_units -= take;
                    }
                }
            }
        }

        // This stage's decouplers sever their child edges now that the
        // burn is accounted — the fire-and-separate ordering the
        // session module doc fixes.
        let decouplers: Vec<PartNodeId> = row
            .part_ids
            .iter()
            .copied()
            .filter(|id| has_module(*id, &|m| matches!(m, ModuleDef::Decoupler)))
            .collect();
        let mut separated = BTreeSet::new();
        for d in decouplers {
            if let Some(kids) = children.get(&d) {
                for c in kids {
                    for dead in session.subtree_ids(*c) {
                        if attached.remove(&dead) {
                            separated.insert(dead);
                        }
                    }
                }
            }
        }
        row.separated_ids = separated.into_iter().collect();
        stages.push(row);
    }

    Ok(CraftAnalysis {
        effective,
        total_delta_v_m_per_s: stages.iter().map(|s| s.delta_v_m_per_s).sum(),
        total_delta_v_sl_m_per_s: stages.iter().map(|s| s.delta_v_sl_m_per_s).sum(),
        stages,
        launch_mass_kg,
        cost_funds,
    })
}

#[cfg(test)]
// Tests signal failure by panicking (§2.13's no-panic rule is a shipping
// contract, not a test contract).
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::resolve::BuiltPart;
    use crate::session::AttachEdge;
    use csp_core::DVec3;
    use csp_data::{
        AeroDef, AttachNode, ColliderDef, FamilyId, LoadedPart, LoadedParts, PART_SCHEMA_VERSION,
        PartCategory, PartDef, PartId, PartVariant, QualCurve, QualKnot, ReliabilityDef,
        ResourceDef, ScaleRule, StructuralDef, TechNodeId, ThermalDef, ThrottleRange, VariantId,
    };

    /// Minimal resources: fuel at 5 kg/u (the authored mix's density)
    /// plus a massless charge.
    fn resources() -> ResourceFile {
        ResourceFile {
            schema_version: 1,
            resources: vec![
                ResourceDef {
                    id: ResourceId::from("fuel"),
                    display_name: "r.fuel".into(),
                    mass_per_unit_kg: 5.0,
                },
                ResourceDef {
                    id: ResourceId::from("charge"),
                    display_name: "r.charge".into(),
                    mass_per_unit_kg: 0.0,
                },
            ],
        }
    }

    fn base_def(id: &str, category: PartCategory, mass: f64) -> PartDef {
        PartDef {
            schema_version: PART_SCHEMA_VERSION,
            id: PartId::from(id),
            family: FamilyId::from("f"),
            display_name: "n".into(),
            category,
            mass_dry_kg: mass,
            cost_funds: 1.0,
            size_m: DVec3::new(1.0, 1.0, 1.0),
            build_resources: Vec::new(),
            attach_nodes: vec![
                AttachNode {
                    id: "top".into(),
                    position_m: DVec3::new(0.0, 0.0, 0.5),
                    direction: DVec3::Z,
                    diameter_m: 1.0,
                },
                AttachNode {
                    id: "bottom".into(),
                    position_m: DVec3::new(0.0, 0.0, -0.5),
                    direction: DVec3::new(0.0, 0.0, -1.0),
                    diameter_m: 1.0,
                },
            ],
            scale: ScaleRule::None,
            collider: ColliderDef::Box {
                half_extents_m: DVec3::new(0.5, 0.5, 0.5),
            },
            aero: AeroDef {
                cd_curve: Vec::new(),
                cl_curve: Vec::new(),
                ref_len_m: 1.0,
            },
            thermal: ThermalDef {
                heat_capacity_j_per_k: 1.0,
                max_temp_k: 1.0,
                emissivity: 0.5,
                conduction_w_per_k: 1.0,
            },
            structural: StructuralDef {
                max_axial_n: 1.0,
                max_shear_n: 1.0,
                max_torque_nm: 1.0,
            },
            variants: vec![PartVariant {
                id: VariantId::from("stock"),
                display_name: "v".into(),
                cost_multiplier: 1.0,
                stat_multipliers: Vec::new(),
            }],
            modules: Vec::new(),
            reliability: ReliabilityDef {
                base_mtbf_s: 1.0e9,
                infant_mortality: 0.0,
                qualification_curve: QualCurve {
                    knots: vec![QualKnot {
                        qualification_units: 0.0,
                        hazard_multiplier: 1.0,
                    }],
                },
                regime_stress: Vec::new(),
                dormant_stress: 0.02,
                failure_modes: vec![(csp_data::FailureMode::SensorFault, 1.0)],
            },
            tech_requirement: TechNodeId::from("t"),
        }
    }

    fn catalog(defs: Vec<PartDef>) -> LoadedParts {
        LoadedParts {
            parts: defs
                .into_iter()
                .map(|def| LoadedPart {
                    def,
                    path: "x".into(),
                })
                .collect(),
        }
    }

    /// pod(100 dry) → tank(50 dry + 200 u fuel = 1000 kg) →
    /// engine(80 dry, 10 kN, 300 s vac, fuel-only). One stage 0.
    fn single_stage() -> (CraftSession, LoadedParts, ResourceFile) {
        let mut pod = base_def("t.pod", PartCategory::Control, 100.0);
        pod.modules = vec![ModuleDef::Control {
            torque_nm: 10.0,
            power_w: 1.0,
        }];
        let mut tank = base_def("t.tank", PartCategory::FuelTank, 50.0);
        tank.modules = vec![ModuleDef::Tank {
            capacity: vec![(ResourceId::from("fuel"), 200.0)],
        }];
        let mut eng = base_def("t.eng", PartCategory::Engine, 80.0);
        eng.modules = vec![ModuleDef::Engine {
            thrust_vac_n: 10_000.0,
            isp_vac_s: 300.0,
            isp_sl_s: 250.0,
            throttle_range: ThrottleRange { min: 0.0, max: 1.0 },
            gimbal_deg: 0.0,
            ignitions: None,
            rated_burn_s: 600.0,
            propellants: vec![(ResourceId::from("fuel"), 1.0)],
            ullage_required: false,
        }];
        let defs = catalog(vec![pod, tank, eng]);
        let res = resources();
        let stock = |id: &str| BuiltPart::stock(PartId::from(id), VariantId::from("stock"));
        let mut s = CraftSession::new();
        let pod_id = s.add_root(&defs, stock("t.pod")).unwrap();
        let tank_id = s
            .add_part(
                &defs,
                stock("t.tank"),
                AttachEdge::new(pod_id, "bottom", "top"),
            )
            .unwrap();
        let _eng_id = s
            .add_part(
                &defs,
                stock("t.eng"),
                AttachEdge::new(tank_id, "bottom", "top"),
            )
            .unwrap();
        (s, defs, res)
    }

    /// The single-stage numbers must match a hand Tsiolkovsky: wet =
    /// 100+50+80+1000 = 1230, dry = 230, dv = 300·9.81·ln(1230/230).
    #[test]
    fn single_stage_readout_matches_tsiolkovsky() {
        let (s, defs, res) = single_stage();
        let a = analyze(&s, &defs, &res).unwrap();
        assert_eq!(a.stages.len(), 1);
        let st = &a.stages[0];
        assert_eq!(st.stage, 0);
        assert_eq!(st.engine_ids.len(), 1);
        assert!((st.wet_mass_kg - 1230.0).abs() < 1e-9, "{st:?}");
        assert!((st.propellant_burned_kg - 1000.0).abs() < 1e-9);
        let expect = 300.0 * G0 * libm::log(1230.0 / 230.0);
        assert!((st.delta_v_m_per_s - expect).abs() < 1e-6, "{st:?}");
        let expect_sl = 250.0 * G0 * libm::log(1230.0 / 230.0);
        assert!((st.delta_v_sl_m_per_s - expect_sl).abs() < 1e-6);
        // mdot = 10000/(300·9.81); burn = 1000/mdot.
        let burn = 1000.0 / (10_000.0 / (300.0 * G0));
        assert!((st.burn_time_s - burn).abs() < 1e-6);
        assert!((st.twr_surface - 10_000.0 / (1230.0 * G0)).abs() < 1e-9);
        assert!((a.launch_mass_kg - 1230.0).abs() < 1e-9);
        assert!((a.total_delta_v_m_per_s - st.delta_v_m_per_s).abs() < 1e-9);
    }

    /// Two stages: booster decoupler+engine+tank staged at 1 under the
    /// sustainer engine, sustainer stage 0. Under the attached-set
    /// model (C13-F1) BOTH engines fire from launch — the stage-1 row
    /// is the pooled burn of the whole stack and matches flight's
    /// `burn_step` numbers; the sustainer then sees post-separation
    /// mass and its own (drained) tank.
    #[test]
    fn two_stage_readout_separates_mass_and_propellant() {
        let (mut s, defs, res) = single_stage();
        let stock = |id: &str| BuiltPart::stock(PartId::from(id), VariantId::from("stock"));
        let mut dec = base_def("t.dec", PartCategory::Utility, 20.0);
        dec.modules = vec![ModuleDef::Decoupler];
        let mut btank = base_def("t.btank", PartCategory::FuelTank, 50.0);
        btank.modules = vec![ModuleDef::Tank {
            capacity: vec![(ResourceId::from("fuel"), 200.0)],
        }];
        let mut beng = base_def("t.beng", PartCategory::Engine, 80.0);
        beng.modules = vec![ModuleDef::Engine {
            thrust_vac_n: 20_000.0,
            isp_vac_s: 280.0,
            isp_sl_s: 240.0,
            throttle_range: ThrottleRange { min: 0.0, max: 1.0 },
            gimbal_deg: 0.0,
            ignitions: None,
            rated_burn_s: 300.0,
            propellants: vec![(ResourceId::from("fuel"), 1.0)],
            ullage_required: false,
        }];
        let mut defs2 = defs.clone();
        for d in [dec, btank, beng] {
            defs2.parts.push(LoadedPart {
                def: d,
                path: "x".into(),
            });
        }
        let defs = defs2;
        let eng_id = s.iter().map(|(id, _)| id).last().unwrap();
        let dec_id = s
            .add_part(
                &defs,
                stock("t.dec"),
                AttachEdge::new(eng_id, "bottom", "top"),
            )
            .unwrap();
        let btank_id = s
            .add_part(
                &defs,
                stock("t.btank"),
                AttachEdge::new(dec_id, "bottom", "top"),
            )
            .unwrap();
        let beng_id = s
            .add_part(
                &defs,
                stock("t.beng"),
                AttachEdge::new(btank_id, "bottom", "top"),
            )
            .unwrap();
        s.set_stage(dec_id, 1).unwrap();
        s.set_stage(beng_id, 1).unwrap();

        let a = analyze(&s, &defs, &res).unwrap();
        // Firing order: stage 1 then stage 0.
        assert_eq!(a.stages.len(), 2);
        let (s1, s0) = (&a.stages[0], &a.stages[1]);
        assert_eq!(s1.stage, 1);
        // C13-F1: the row's engine set is every attached engine —
        // sustainer AND booster — matching the sim's vessel-wide
        // fire (the boundary jettisons, it does not ignite).
        assert_eq!(s1.engine_ids, vec![eng_id, beng_id]);
        // Stage 1 burns with EVERYTHING attached: wet = 1230 + 20+50+80+1000
        // = 2380. The supply pool is the whole attached set, so the
        // engines drain BOTH tanks — 2000 kg — before any separation.
        assert!((s1.wet_mass_kg - 2380.0).abs() < 1e-9, "{s1:?}");
        assert!((s1.propellant_burned_kg - 2000.0).abs() < 1e-9);
        // Flight-identical figures: ΣF = 30 kN, pooled ṁ =
        // 10k/(300·g₀) + 20k/(280·g₀), Isp_eff mass-flow weighted.
        assert!((s1.thrust_vac_n - 30_000.0).abs() < 1e-9, "{s1:?}");
        let mdot = 10_000.0 / (300.0 * G0) + 20_000.0 / (280.0 * G0);
        assert!((s1.mass_flow_kg_s - mdot).abs() < 1e-9);
        let isp_eff = 30_000.0 / (10_000.0 / 300.0 + 20_000.0 / 280.0);
        assert!((s1.isp_vac_s - isp_eff).abs() < 1e-9);
        assert!((s1.burn_time_s - 2000.0 / mdot).abs() < 1e-6);
        assert!((s1.twr_surface - 30_000.0 / (2380.0 * G0)).abs() < 1e-9);
        let dv1 = isp_eff * G0 * libm::log(2380.0 / 380.0);
        assert!((s1.delta_v_m_per_s - dv1).abs() < 1e-6);
        // The decoupler drops btank+beng (the dec stays parent-side).
        assert_eq!(s1.separated_ids, vec![btank_id, beng_id]);
        // Stage 0: pod+sustainer tank+engine+the spent decoupler —
        // wet is dry-only 250 (the dec stays parent-side; the tank is
        // empty), so the sustainer delivers nothing: an honest starved
        // stage — the coverage-zeroed figures flight's burn_step
        // reports on a dry pool (thrust 0, TWR 0, "0:00").
        assert!((s0.wet_mass_kg - 250.0).abs() < 1e-9, "{s0:?}");
        assert!((s0.propellant_burned_kg).abs() < 1e-9);
        assert_eq!(s0.delta_v_m_per_s, 0.0);
        assert_eq!(s0.thrust_vac_n, 0.0);
        assert_eq!(s0.twr_surface, 0.0);
        assert!((a.total_delta_v_m_per_s - dv1).abs() < 1e-6);
    }

    /// Disjoint propellant sets exercise the depletion passes: when
    /// the oxidizer runs out the ox-fed engine starves while the
    /// fuel-only engine burns on — the same pass boundary flight's
    /// `delta_v_walk_from` iterates, so burn time and Δv are the
    /// two-segment sums, not a single first-depletion figure.
    #[test]
    #[allow(clippy::too_many_lines)] // the fixture and the two-pass math read as one narrative
    fn mixed_propellant_stage_burns_in_depletion_passes() {
        let res = ResourceFile {
            schema_version: 1,
            resources: vec![
                ResourceDef {
                    id: ResourceId::from("fuel"),
                    display_name: "r.fuel".into(),
                    mass_per_unit_kg: 5.0,
                },
                ResourceDef {
                    id: ResourceId::from("ox"),
                    display_name: "r.ox".into(),
                    mass_per_unit_kg: 5.0,
                },
            ],
        };
        let mut pod = base_def("t.pod", PartCategory::Control, 100.0);
        pod.modules = vec![ModuleDef::Control {
            torque_nm: 10.0,
            power_w: 1.0,
        }];
        let mut tank = base_def("t.tank2", PartCategory::FuelTank, 50.0);
        tank.modules = vec![ModuleDef::Tank {
            capacity: vec![
                (ResourceId::from("fuel"), 200.0),
                (ResourceId::from("ox"), 200.0),
            ],
        }];
        let engine = |id: &str, thrust: f64, isp: f64, mix: Vec<(ResourceId, f64)>| {
            let mut d = base_def(id, PartCategory::Engine, 80.0);
            d.modules = vec![ModuleDef::Engine {
                thrust_vac_n: thrust,
                isp_vac_s: isp,
                isp_sl_s: 250.0,
                throttle_range: ThrottleRange { min: 0.0, max: 1.0 },
                gimbal_deg: 0.0,
                ignitions: None,
                rated_burn_s: 600.0,
                propellants: mix,
                ullage_required: false,
            }];
            d
        };
        // A: ox-heavy biprop; B: fuel-only — the ox depletion starves
        // A alone.
        let defs = catalog(vec![
            pod,
            tank,
            engine(
                "t.engA",
                10_000.0,
                300.0,
                vec![
                    (ResourceId::from("fuel"), 0.1),
                    (ResourceId::from("ox"), 0.9),
                ],
            ),
            engine(
                "t.engB",
                5_000.0,
                200.0,
                vec![(ResourceId::from("fuel"), 1.0)],
            ),
        ]);
        let stock = |id: &str| BuiltPart::stock(PartId::from(id), VariantId::from("stock"));
        let mut s = CraftSession::new();
        let pod_id = s.add_root(&defs, stock("t.pod")).unwrap();
        let tank_id = s
            .add_part(
                &defs,
                stock("t.tank2"),
                AttachEdge::new(pod_id, "bottom", "top"),
            )
            .unwrap();
        let eng_a = s
            .add_part(
                &defs,
                stock("t.engA"),
                AttachEdge::new(tank_id, "bottom", "top"),
            )
            .unwrap();
        let eng_b = s
            .add_part(
                &defs,
                stock("t.engB"),
                AttachEdge::new(eng_a, "bottom", "top"),
            )
            .unwrap();

        let a = analyze(&s, &defs, &res).unwrap();
        assert_eq!(a.stages.len(), 1);
        let st = &a.stages[0];
        assert_eq!(st.engine_ids, vec![eng_a, eng_b]);
        // Segment-start: both engines deliver — ΣF 15 kN.
        assert!((st.thrust_vac_n - 15_000.0).abs() < 1e-9, "{st:?}");
        let mdot_a = 10_000.0 / (300.0 * G0);
        let mdot_b = 5_000.0 / (200.0 * G0);
        assert!((st.mass_flow_kg_s - (mdot_a + mdot_b)).abs() < 1e-9);
        let fuel_d = mdot_a * 0.1 + mdot_b;
        let ox_d = mdot_a * 0.9;
        // Pass 1 ends on the ox pool (1000 kg each side): burn1 =
        // 1000/ox_d < 1000/fuel_d.
        let burn1 = 1000.0 / ox_d;
        assert!(burn1 < 1000.0 / fuel_d);
        let fuel_left = 1000.0 - fuel_d * burn1;
        // Pass 2: A starved, B alone drains the fuel remnant.
        let burn2 = fuel_left / mdot_b;
        assert!((st.burn_time_s - (burn1 + burn2)).abs() < 1e-6, "{st:?}");
        // Every tank ends dry — Σ burned is the whole 2000 kg.
        assert!((st.propellant_burned_kg - 2000.0).abs() < 1e-6);
        // Two Tsiolkovsky terms: the mixed-Isp pass, then B alone.
        let wet = 2310.0;
        let isp1 = 15_000.0 / (G0 * (mdot_a + mdot_b));
        let wet2 = wet - (fuel_d + ox_d) * burn1;
        let dv1 = isp1 * G0 * libm::log(wet / wet2);
        let dv2 = 200.0 * G0 * libm::log(wet2 / (wet2 - fuel_left));
        assert!((st.delta_v_m_per_s - (dv1 + dv2)).abs() < 1e-6, "{st:?}");
        assert!((st.twr_surface - 15_000.0 / (wet * G0)).abs() < 1e-9);
    }

    /// An engine with no propellant in its attached set produces a
    /// zero-Δv row, not NaN/∞ — `validate`'s EngineWithoutPropellant is
    /// the surfaced diagnostic; the readout reports physics.
    #[test]
    fn starved_stage_reports_zero() {
        let (_, defs, res) = single_stage();
        let defs2 = catalog(
            defs.parts
                .iter()
                .filter(|p| p.def.id.as_str() != "t.tank")
                .map(|p| p.def.clone())
                .collect(),
        );
        let stock = |id: &str| BuiltPart::stock(PartId::from(id), VariantId::from("stock"));
        let mut s2 = CraftSession::new();
        let pod_id = s2.add_root(&defs2, stock("t.pod")).unwrap();
        s2.add_part(
            &defs2,
            stock("t.eng"),
            AttachEdge::new(pod_id, "bottom", "top"),
        )
        .unwrap();
        let a = analyze(&s2, &defs2, &res).unwrap();
        let st = &a.stages[0];
        assert_eq!(st.delta_v_m_per_s, 0.0);
        assert_eq!(st.burn_time_s, 0.0);
        assert_eq!(st.propellant_burned_kg, 0.0);
        // Delivered thrust is coverage-zeroed — the same 0 flight's
        // `burn_step` reports on a dry pool, so the TWR row reads 0
        // rather than an unachievable rated figure.
        assert_eq!(st.thrust_vac_n, 0.0);
        assert_eq!(st.twr_surface, 0.0);
    }

    /// An empty session analyzes to empty — no error.
    #[test]
    fn empty_session_is_empty_analysis() {
        let defs = catalog(Vec::new());
        let res = resources();
        let a = analyze(&CraftSession::new(), &defs, &res).unwrap();
        assert!(a.stages.is_empty() && a.effective.is_empty());
        assert_eq!(a.launch_mass_kg, 0.0);
    }
}
