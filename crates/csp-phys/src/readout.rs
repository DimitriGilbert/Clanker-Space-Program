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
//! - **Burn.** A stage's engines burn against the propellant remaining
//!   in the attached tanks. Per-engine mass flow is `ṁᵢ = Fᵢ / (Ispᵢ ·
//!   g₀)`; per-resource demand is `Σᵢ ṁᵢ·fracᵢ,r` over the engines'
//!   authored propellant mixes. Burn time is the limiting resource's
//!   `available_kg / demand_kg_s`; the same-stage decoupler case is
//!   fire-and-separate (the departing half feeds this stage's engines —
//!   session.rs's module doc), so separation applies **after** the
//!   stage's burn is accounted.
//! - **Δv.** Tsiolkovsky on the attached mass: `dv = Isp_eff·g₀·
//!   ln(m_wet / (m_wet − m_burned))` with `Isp_eff = ΣF / (g₀·Σṁ)` —
//!   the mass-flow-weighted value for mixed-engine stages. A sea-level
//!   figure substitutes `isp_sl_s` on the same mass ratio (only
//!   `thrust_vac_n` is authored, so thrust does not change).
//! - **TWR** is `ΣF / (m_wet · g₀)`. `csp_data::G0` *is* Terrin's
//!   surface gravity by authoring convention (part.rs), so the
//!   launchpad number needs no body lookup; [`crate::profile`] carries
//!   the body-parameterized form for the profile comparison.
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

/// One firing stage's readout row. Fields are `0`/empty on a stage
/// with no engines; `separated_ids` still reports what its decouplers
/// drop.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StageStats {
    /// The stage index (the `stages` vec is in firing order —
    /// descending index).
    pub stage: u32,
    /// Parts assigned to this stage that are still attached when it
    /// fires, ascending id.
    pub part_ids: Vec<PartNodeId>,
    /// Engines this stage ignites (`engine_ids ⊆ part_ids`).
    pub engine_ids: Vec<PartNodeId>,
    /// Parts this stage's decouplers drop, ascending id — their
    /// subtrees leave after the burn is accounted.
    pub separated_ids: Vec<PartNodeId>,
    /// Σ resolved `thrust_vac_n` of `engine_ids` (N).
    pub thrust_vac_n: f64,
    /// Effective vacuum Isp, mass-flow weighted: `ΣF / (g₀·Σṁ)` (s).
    /// `0` when the stage has no engines.
    pub isp_vac_s: f64,
    /// Effective sea-level Isp on the same weighting (s).
    pub isp_sl_s: f64,
    /// Σ propellant mass flow at rated vacuum thrust (kg/s).
    pub mass_flow_kg_s: f64,
    /// Mass still attached while this stage burns — dry mass plus the
    /// propellant remaining in every attached tank (kg).
    pub wet_mass_kg: f64,
    /// Propellant this stage's engines draw before the limiting
    /// resource runs out (kg).
    pub propellant_burned_kg: f64,
    /// `propellant_burned / mass_flow` — seconds at full rated thrust.
    pub burn_time_s: f64,
    /// `isp_vac · g₀ · ln(wet / (wet − burned))` (m/s).
    pub delta_v_m_per_s: f64,
    /// The same ratio at `isp_sl` — the launchpad figure (m/s).
    pub delta_v_sl_m_per_s: f64,
    /// `thrust / (wet · g₀)` — TWR vs Terrin surface gravity.
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
        row.engine_ids = row
            .part_ids
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
            row.thrust_vac_n = row
                .engine_ids
                .iter()
                .filter_map(|id| eff_of.get(id))
                .map(|e| engine_thrust(e))
                .sum();
            // Per-engine mass flow, then per-resource demand (kg/s).
            let mut demand: BTreeMap<ResourceId, f64> = BTreeMap::new();
            let mut mdot_total = 0.0f64;
            let mut sl_mdot_equiv = 0.0f64;
            for id in &row.engine_ids {
                let Some(eff) = eff_of.get(id) else { continue };
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
                    let mdot = thrust_vac_n / (isp_vac_s * G0);
                    mdot_total += mdot;
                    sl_mdot_equiv += thrust_vac_n / (isp_sl_s * G0);
                    for (r, frac) in propellants {
                        *demand.entry(r.clone()).or_default() += mdot * frac;
                    }
                }
            }
            row.mass_flow_kg_s = mdot_total;
            if mdot_total > 0.0 {
                row.isp_vac_s = row.thrust_vac_n / (G0 * mdot_total);
                row.isp_sl_s = row.thrust_vac_n / (G0 * sl_mdot_equiv);
            }

            // Propellant on the attached set — the supply region (module
            // doc: `attached` already encodes the severed-edge cuts).
            //
            // LATENT EDGE (documented, accepted at this tier): massless
            // resources (`mass_per_unit_kg == 0`, e.g. electric charge)
            // are skipped here — they carry no burnable mass, so
            // `available` never gains an entry for them. An engine whose
            // authored propellant mix demands one then reads
            // `available == 0` below and burns for 0 s — a massless-
            // propellant engine silently contributes thrust but no Δv.
            // No authored engine does this today (the validator gates
            // propellant mixes); the honest fix when one lands is
            // demand/supply accounting in *units*, not kg — the
            // consumption loop's `mpu.max(1e-12)` fallback is the same
            // approximation's other half.
            let mut available: BTreeMap<ResourceId, f64> = BTreeMap::new();
            for id in &attached {
                let Some(nm) = mass.get(id) else { continue };
                for (r, units) in &nm.tanks {
                    let mpu = res.mass_per_unit(r).unwrap_or(0.0);
                    if mpu > 0.0 {
                        *available.entry(r.clone()).or_default() += units * mpu;
                    }
                }
            }

            // Burn time is limited by the first propellant to run out.
            let mut burn = f64::INFINITY;
            for (r, kg_s) in &demand {
                if *kg_s > 0.0 {
                    burn = burn.min(available.get(r).copied().unwrap_or(0.0) / kg_s);
                }
            }
            if !burn.is_finite() {
                burn = 0.0;
            }
            row.burn_time_s = burn;
            row.propellant_burned_kg = demand
                .iter()
                .map(|(r, kg_s)| (kg_s * burn).min(available.get(r).copied().unwrap_or(0.0)))
                .sum();
            let dry_end = (row.wet_mass_kg - row.propellant_burned_kg).max(1e-9);
            let ratio = row.wet_mass_kg / dry_end;
            if ratio > 1.0 && row.isp_vac_s > 0.0 {
                row.delta_v_m_per_s = row.isp_vac_s * G0 * libm::log(ratio);
                row.delta_v_sl_m_per_s = row.isp_sl_s * G0 * libm::log(ratio);
            }
            if row.wet_mass_kg > 0.0 {
                row.twr_surface = row.thrust_vac_n / (row.wet_mass_kg * G0);
            }

            // Consume the propellant — ascending `PartNodeId`, each
            // resource drawn down until the stage's burn is met.
            //
            // DRAW ORDER (documented, R14-F6): `attached` is a
            // `BTreeSet`, so tanks drain in *allocation* order — the
            // order parts were added to the session, not any authored
            // priority. The order is stable and deterministic for a
            // given session (ids are never reused), but it is not a
            // game rule: which tank empties first only matters when a
            // stage's burn cannot empty them all — a partial draw
            // drains low-id tanks first, and a decoupler dropping the
            // drained half leaves later stages more propellant than
            // dropping the full half would. A stable *authored* rule
            // (tank feed priority, bottom-up draining) is a schema
            // question, deferred like the other v1 simplifications.
            for (r, kg_s) in &demand {
                let mpu = res.mass_per_unit(r).unwrap_or(1.0).max(1e-12);
                let mut need_units = kg_s * burn / mpu;
                for id in &attached {
                    if need_units <= 0.0 {
                        break;
                    }
                    let Some(nm) = mass.get_mut(id) else { continue };
                    let Some(have) = nm.tanks.get_mut(r) else {
                        continue;
                    };
                    let take = (*have).min(need_units);
                    *have -= take;
                    need_units -= take;
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

/// Σ `thrust_vac_n` across the part's `Engine` modules.
fn engine_thrust(eff: &EffectivePart) -> f64 {
    eff.modules
        .iter()
        .map(|m| match m {
            ModuleDef::Engine { thrust_vac_n, .. } => *thrust_vac_n,
            _ => 0.0,
        })
        .sum()
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
    /// sustainer engine, sustainer stage 0. The booster burn lifts
    /// everything; the sustainer sees only post-separation mass and its
    /// own (full) tank.
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
        assert_eq!(s1.engine_ids, vec![beng_id]);
        // Stage 1 burns with EVERYTHING attached: wet = 1230 + 20+50+80+1000
        // = 2380. The supply region is the whole attached set, so the
        // booster drains BOTH tanks — 2000 kg — before any separation.
        assert!((s1.wet_mass_kg - 2380.0).abs() < 1e-9, "{s1:?}");
        assert!((s1.propellant_burned_kg - 2000.0).abs() < 1e-9);
        let dv1 = 280.0 * G0 * libm::log(2380.0 / 380.0);
        assert!((s1.delta_v_m_per_s - dv1).abs() < 1e-6);
        // The decoupler drops btank+beng (the dec stays parent-side).
        assert_eq!(s1.separated_ids, vec![btank_id, beng_id]);
        // Stage 0: pod+sustainer tank+engine+the spent decoupler —
        // wet is dry-only 250 (the dec stays parent-side; the tank is
        // empty), so the sustainer gets zero Δv: an honest starved
        // stage, matching EngineWithoutPropellant's advisory.
        assert!((s0.wet_mass_kg - 250.0).abs() < 1e-9, "{s0:?}");
        assert!((s0.propellant_burned_kg).abs() < 1e-9);
        assert_eq!(s0.delta_v_m_per_s, 0.0);
        assert!((a.total_delta_v_m_per_s - dv1).abs() < 1e-6);
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
        assert!(st.twr_surface > 0.0); // thrust/mass still honest
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
