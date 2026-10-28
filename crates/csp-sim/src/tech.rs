//! M7-B: the §3.3 tech tree — the world's progression record and the
//! two research commands that spend the M7-A banked science.
//!
//! # The state (`TechState`, §7.2 "tech state")
//!
//! - `researched` — the bought [`TechNodeId`] set. A node buys with
//!   `ResearchTech` after every `requires` edge is itself researched
//!   and `science.banked` covers `science_cost`.
//! - `track_levels` — purchased [`UpgradeTrack`] levels. Each level is
//!   bought individually by `ResearchUpgradeLevel`, spending that
//!   level's authored `science_cost`, up to the cap the researched
//!   nodes grant ([`TechTree::level_cap`]) — the schema doc's
//!   "nodes unlock the right to purchase" reading.
//! - `qualifications` — the §3.4 `FamilyQualification` per-family
//!   records (§7.2's "per family, per save" bullet). The record is
//!   serialized+hashed now so the save schema is complete; the
//!   *accrual* writers (test stands, regime-weighted flight units)
//!   are M9b scope — [`TechState::credit_qualification`] is the
//!   mutation entry point they will land on.
//!
//! # The build-time wiring (§3.3 "upgrades apply at manufacture")
//!
//! The M3 resolver freezes `BuiltPart.upgrade_levels_at_build`; this
//! module is where the frozen map comes from:
//! [`TechState::frozen_upgrades`] composes the family's purchased
//! levels through `csp_phys::compose_upgrade_levels`, and
//! [`TechState::build_part`] is the whole manufacture step —
//! `tech_requirement` gate + frozen tracks + the §3.1 record fields.
//! A `BuiltPart` produced before a level is bought keeps its frozen
//! numbers forever: the record is self-contained and never reads
//! live tech state on re-derivation (§3.1's two-functions note).
//!
//! Science spend rules: only `science.banked` is spendable — a running
//! lab project's accrued-but-unbanked points bank at its exhaustion
//! event, and research commands do not touch project escrow.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use csp_core::{SimError, StateHasher};
use csp_data::{PartDef, PartId, StatPath, TechNodeId, TechTree, UpgradeTrackId, VariantId};
use csp_phys::{BuiltPart, FamilyQualifications, ResolveError, TrackLevels};

use crate::{CraftDef, TickShadow, World};

/// The program's §3.3/§3.4 progression record — serialized on the
/// snapshot and walked into `state_hash` (§7.2 "tech state";
/// "`FamilyQualification` per family"). `BTree*` containers only —
/// the determinism-domain rule.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TechState {
    /// The researched tech nodes.
    pub researched: BTreeSet<TechNodeId>,
    /// Purchased upgrade levels per track (`0` entries are never
    /// written — absence means level 0).
    pub track_levels: BTreeMap<UpgradeTrackId, u8>,
    /// §3.4 `FamilyQualification` records — live resolver input at
    /// manufacture, M9b's accrual target (module doc).
    pub qualifications: FamilyQualifications,
}

impl TechState {
    /// Whether `node` is researched.
    pub fn is_researched(&self, node: &TechNodeId) -> bool {
        self.researched.contains(node)
    }

    /// Whether `def`'s `tech_requirement` is satisfied — the part
    /// availability gate the editor and manufacture share.
    pub fn is_part_unlocked(&self, def: &PartDef) -> bool {
        self.researched.contains(&def.tech_requirement)
    }

    /// Levels bought on `track` (0 when none).
    pub fn track_level(&self, track: &UpgradeTrackId) -> u8 {
        self.track_levels.get(track).copied().unwrap_or(0)
    }

    /// The level cap the researched nodes grant on `track`.
    pub fn level_cap(&self, tree: &TechTree, track: &UpgradeTrackId) -> u8 {
        tree.level_cap(track, &self.researched)
    }

    /// The frozen `upgrade_levels_at_build` for `def` at this tech
    /// state — the family's tracks paired with their purchased levels,
    /// composed by the resolver's manufacture helper (§3.3).
    pub fn frozen_upgrades(
        &self,
        tree: &TechTree,
        def: &PartDef,
    ) -> Result<BTreeMap<StatPath, TrackLevels>, ResolveError> {
        csp_phys::compose_upgrade_levels(
            def,
            tree.tracks_for_family(&def.family)
                .map(|t| (t, self.track_level(&t.id))),
        )
    }

    /// The manufacture step (§3.3): the whole `BuiltPart` a craft
    /// records at build — `tech_requirement` gated, upgrade levels
    /// composed and frozen, `data_set_hash` stamped by the caller.
    /// `scale`/`variant` freeze verbatim; the resolver gates their
    /// legality at resolve time as usual.
    ///
    /// `Recoverable` when the part is tech-locked — a locked part is a
    /// rejected build, never silently downgraded.
    pub fn build_part(
        &self,
        tree: &TechTree,
        def: &PartDef,
        variant: VariantId,
        scale: f64,
        data_set_hash: [u8; 32],
    ) -> Result<BuiltPart, SimError> {
        if !self.is_part_unlocked(def) {
            return Err(SimError::Recoverable("tech: part is tech-locked"));
        }
        Ok(BuiltPart {
            part_id: def.id.clone(),
            scale,
            // Sim-side manufacture has no length input — the VAB's
            // per-part stretch is an editor dimension; manufacture
            // builds unstretched unless the record says otherwise.
            stretch: 1.0,
            variant,
            upgrade_levels_at_build: self.frozen_upgrades(tree, def)?,
            data_set_hash,
        })
    }

    /// Credit a family's §3.4 qualification record — the M9b accrual
    /// writers' entry point (flight units from regime-weighted flight,
    /// bench units from the test stand, `observed_failures` from
    /// `PartFailure` events). A missing record is created; negative or
    /// non-finite credits reject `Recoverable` — the resolver gates
    /// the same fields at resolve time and a corrupt record must fail
    /// at write, not at read.
    pub fn credit_qualification(
        &mut self,
        family: csp_data::FamilyId,
        flight_units: f64,
        bench_units: f64,
        observed_failures: u32,
    ) -> Result<(), SimError> {
        if !(flight_units.is_finite() && flight_units >= 0.0)
            || !(bench_units.is_finite() && bench_units >= 0.0)
        {
            return Err(SimError::Recoverable(
                "tech: qualification credit is negative or non-finite",
            ));
        }
        let q = self.qualifications.entry(family).or_default();
        q.flight_units += flight_units;
        q.bench_units += bench_units;
        q.observed_failures = q.observed_failures.saturating_add(observed_failures);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Hash walk (§7.1) — hand-written, ordering is the schema.
// ---------------------------------------------------------------------------

/// The [`TechState`] walk — appended to `state_hash` after the science
/// book (M7-B). Order: researched set, purchased levels, the
/// qualification table — the §7.2 order the module doc gives.
pub fn hash_tech(h: &mut StateHasher, t: &TechState) {
    h.u32(t.researched.len() as u32);
    for n in &t.researched {
        h.str(n.as_str());
    }
    h.btree_map(
        &t.track_levels,
        |h, id| {
            h.str(id.as_str());
        },
        |h, l| {
            h.u64(u64::from(*l));
        },
    );
    h.btree_map(
        &t.qualifications,
        |h, f| {
            h.str(f.as_str());
        },
        |h, q| {
            h.f64(q.flight_units);
            h.f64(q.bench_units);
            h.u64(u64::from(q.observed_failures));
        },
    );
}

// ---------------------------------------------------------------------------
// World impl — construction config, the two commands, the manufacture
// helper, observation.
// ---------------------------------------------------------------------------

impl World {
    /// The tech book — researched nodes, purchased levels, family
    /// qualification records.
    pub fn tech(&self) -> &TechState {
        &self.tech
    }

    /// The installed tree (construction config) — `None` for a world
    /// built without `data/techtree/`.
    pub fn tech_tree(&self) -> Option<&TechTree> {
        self.tech_tree.as_ref()
    }

    /// Install the merged tech tree — construction config (the
    /// `set_experiments` precedent: data-derived, handed in, never
    /// serialized or hashed; `WorldConfig` carries it so §7.6 replays
    /// rebuild it).
    pub fn set_tech_tree(&mut self, tree: TechTree) {
        self.tech_tree = Some(tree);
    }

    /// The `data/parts/` def table — construction config like
    /// `experiments` (data-derived, §7.3): handed in via
    /// [`Self::set_parts`]/`WorldConfig`, never serialized or hashed.
    /// `CraftDef::part_ids` resolves against it at the spawn/
    /// manufacture gate — the authored `tech_requirement` lives on
    /// the def, so the gate checks *data*, never a claimed node.
    pub fn set_parts(&mut self, parts: BTreeMap<PartId, PartDef>) {
        self.parts = parts;
    }

    /// The installed part catalog (construction config).
    pub fn parts(&self) -> &BTreeMap<PartId, PartDef> {
        &self.parts
    }

    /// The authored starting capability — grant every prerequisite-
    /// free, zero-cost node the installed tree authors (the shipped
    /// home tree's `tech.start` — "free root — the launch stack").
    /// A pre-run write like [`Self::found_authored_colonies`]: the
    /// grant is part of the baseline snapshot and never enters the
    /// command log. Without it a fresh world would hold `researched
    /// = {}` and the §3.3 gate would lock the whole launch stack out
    /// of the catalog it ships as starting equipment. A no-op with
    /// no tree installed.
    pub fn grant_free_tech_roots(&mut self) {
        let Some(tree) = &self.tech_tree else {
            return;
        };
        let roots: Vec<TechNodeId> = tree
            .nodes
            .iter()
            .filter(|n| n.requires.is_empty() && n.science_cost == 0.0)
            .map(|n| n.id.clone())
            .collect();
        for id in roots {
            self.tech.researched.insert(id);
        }
    }

    /// The §3.3 facility gate — `true` when `facility` is unlocked
    /// by a researched node's `unlocks.facilities` grant, or when no
    /// tree is installed (the `check_part_gate` precedent: treeless
    /// worlds run ungated). C08-F6/C33-F3 wires the authored
    /// `facility.*` ids onto the surfaces they name — the test
    /// stand, the lab, the pad, the assembly building.
    pub(crate) fn facility_unlocked(&self, facility: &csp_data::FacilityId) -> bool {
        self.tech_tree
            .as_ref()
            .is_none_or(|t| t.facility_unlocked(facility, &self.tech.researched))
    }

    /// The §3.3 contract-type gate — `true` when the template id is
    /// granted by a researched node's `unlocks.contract_types`, or
    /// when no tree is installed. Generation and accept share it —
    /// the validator cross-check keeps it aligned with the
    /// template's own `TechResearched` prereq.
    pub(crate) fn contract_type_unlocked(&self, template: &csp_data::ContractTypeId) -> bool {
        self.tech_tree
            .as_ref()
            .is_none_or(|t| t.contract_type_unlocked(template, &self.tech.researched))
    }

    /// The §3.3 availability gate the spawn and manufacture applies
    /// share — the `part_costs`/`reliability` shape checks' sibling:
    /// [`CraftDef::part_ids`] is empty ("identity not recorded" —
    /// hand-built craft, the empty-table convention) or covers
    /// `parts` exactly, every id resolves in the installed catalog,
    /// and — when the world has a tech tree — every def's
    /// `tech_requirement` is researched. A locked or unknown part is
    /// a `Recoverable` rejection, never a silent launch. A world
    /// without a tree runs ungated (no progression data, nothing to
    /// enforce against — the research commands' own "no tech tree
    /// installed" refusal precedent).
    pub(crate) fn check_part_gate(&self, craft: &CraftDef) -> Result<(), SimError> {
        if craft.part_ids.is_empty() {
            return Ok(());
        }
        if craft.part_ids.len() != craft.parts.len() {
            return Err(SimError::Recoverable(
                "part gate: part_ids not parallel to parts",
            ));
        }
        for id in &craft.part_ids {
            let Some(def) = self.parts.get(id) else {
                return Err(SimError::Recoverable(
                    "part gate: part id not in the installed catalog",
                ));
            };
            if self.tech_tree.is_some() && !self.tech.is_part_unlocked(def) {
                return Err(SimError::Recoverable("part gate: part is tech-locked"));
            }
        }
        Ok(())
    }

    /// `ResearchTech` — spend `science.banked` for the node's
    /// `science_cost` and mark it researched. Requires an installed
    /// tree, a known unresearched node with every prerequisite met,
    /// and enough banked science (in-flight project accrual does not
    /// count — see the module doc).
    pub(crate) fn apply_research_tech(
        &mut self,
        node: &TechNodeId,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let Some(tree) = &self.tech_tree else {
            return Err(SimError::Recoverable("tech: no tech tree installed"));
        };
        let Some(def) = tree.node(node) else {
            return Err(SimError::Recoverable("tech: unknown node"));
        };
        let (cost, mut prereq_unmet) = (def.science_cost, false);
        for r in &def.requires {
            if !self.tech.researched.contains(r) {
                prereq_unmet = true;
                break;
            }
        }
        if self.tech.researched.contains(node) {
            return Err(SimError::Recoverable("tech: node already researched"));
        }
        if prereq_unmet {
            return Err(SimError::Recoverable("tech: prerequisites not met"));
        }
        if !(cost.is_finite() && cost >= 0.0) {
            return Err(SimError::Recoverable(
                "tech: node cost is not finite and >= 0",
            ));
        }
        if self.science.banked < cost {
            return Err(SimError::Recoverable("tech: insufficient banked science"));
        }
        self.shadow_tech(shadow);
        self.shadow_science(shadow);
        self.science.banked -= cost;
        self.tech.researched.insert(node.clone());
        Ok(())
    }

    /// `ResearchUpgradeLevel` — buy the next level of `track`:
    /// `level < level_cap(researched)`, `level < levels.len()`, and
    /// `science.banked` covers `levels[level].science_cost`. Each
    /// level spends its own authored cost (module doc).
    pub(crate) fn apply_research_upgrade_level(
        &mut self,
        track: &UpgradeTrackId,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let Some(tree) = &self.tech_tree else {
            return Err(SimError::Recoverable("tech: no tech tree installed"));
        };
        let Some(def) = tree.track(track) else {
            return Err(SimError::Recoverable("tech: unknown upgrade track"));
        };
        let current = self.tech.track_level(track);
        if usize::from(current) >= def.levels.len() {
            return Err(SimError::Recoverable("tech: track already maxed"));
        }
        if current >= self.tech.level_cap(tree, track) {
            return Err(SimError::Recoverable("tech: track level not unlocked"));
        }
        let cost = def.levels[usize::from(current)].science_cost;
        if !(cost.is_finite() && cost >= 0.0) {
            return Err(SimError::Recoverable(
                "tech: level cost is not finite and >= 0",
            ));
        }
        if self.science.banked < cost {
            return Err(SimError::Recoverable("tech: insufficient banked science"));
        }
        self.shadow_tech(shadow);
        self.shadow_science(shadow);
        self.science.banked -= cost;
        self.tech.track_levels.insert(track.clone(), current + 1);
        Ok(())
    }

    /// Lazy-shadow capture for the tech book (the
    /// `shadow_science` precedent — one clone per tick that mutates).
    pub(crate) fn shadow_tech(&mut self, shadow: &mut TickShadow) {
        if shadow.tech.is_none() {
            shadow.tech = Some(self.tech.clone());
        }
    }

    /// The manufacture helper a craft-build flow calls (§3.3):
    /// `tech_requirement` gate + frozen upgrade composition — see
    /// [`TechState::build_part`]. `Recoverable` when no tree is
    /// installed or the part is tech-locked.
    pub fn build_part(
        &self,
        def: &PartDef,
        variant: VariantId,
        scale: f64,
        data_set_hash: [u8; 32],
    ) -> Result<BuiltPart, SimError> {
        let Some(tree) = &self.tech_tree else {
            return Err(SimError::Recoverable("tech: no tech tree installed"));
        };
        self.tech
            .build_part(tree, def, variant, scale, data_set_hash)
    }
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers the
// shipping fallible paths.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::{SimCommand, SourceClass};
    use csp_data::{TechNode, TechUnlocks, UpgradeEffect, UpgradeLevel, UpgradeTrack};

    /// A two-node tree: root (free) → tier2 (30 science, grants
    /// `track.isp` cap 2). The part def is hand-built minimal — every
    /// field the resolver/build path touches.
    fn tree() -> TechTree {
        TechTree {
            nodes: vec![
                TechNode {
                    id: "tech.start".into(),
                    display_name: "n".into(),
                    description: "d".into(),
                    science_cost: 0.0,
                    requires: vec![],
                    unlocks: TechUnlocks::default(),
                },
                TechNode {
                    id: "tech.tier2".into(),
                    display_name: "n".into(),
                    description: "d".into(),
                    science_cost: 30.0,
                    requires: vec!["tech.start".into()],
                    unlocks: TechUnlocks {
                        track_levels: vec![("track.isp".into(), 2)],
                        facilities: vec![],
                        contract_types: vec![],
                    },
                },
            ],
            tracks: vec![UpgradeTrack {
                id: "track.isp".into(),
                display_name: "t".into(),
                family: "fam.e".into(),
                stat_path: StatPath::IspVacS,
                levels: vec![
                    UpgradeLevel {
                        science_cost: 10.0,
                        multiplier_or_delta: UpgradeEffect::Multiplier(1.1),
                    },
                    UpgradeLevel {
                        science_cost: 20.0,
                        multiplier_or_delta: UpgradeEffect::Multiplier(1.1),
                    },
                ],
            }],
        }
    }

    /// A minimal engine PartDef in family `fam.e` gated on `tech.tier2`.
    fn def(tech: &str) -> PartDef {
        use csp_data::*;
        PartDef {
            schema_version: PART_SCHEMA_VERSION,
            id: PartId::from("test.e"),
            family: FamilyId::from("fam.e"),
            display_name: "n".into(),
            category: PartCategory::Engine,
            mass_dry_kg: 100.0,
            cost_funds: 100.0,
            size_m: csp_core::DVec3::new(1.0, 1.0, 1.0),
            build_resources: vec![],
            attach_nodes: vec![],
            scale: ScaleRule::None,
            collider: ColliderDef::Box {
                half_extents_m: csp_core::DVec3::new(1.0, 1.0, 1.0),
            },
            aero: AeroDef::constant(0.5, 0.0, 1.0),
            thermal: ThermalDef {
                heat_capacity_j_per_k: 1.0,
                max_temp_k: 1.0,
                emissivity: 0.5,
                conduction_w_per_k: 0.0,
            },
            structural: StructuralDef::unlimited(),
            variants: vec![PartVariant {
                id: VariantId::from("stock"),
                display_name: "v".into(),
                cost_multiplier: 1.0,
                stat_multipliers: vec![],
            }],
            modules: vec![ModuleDef::Engine {
                thrust_vac_n: 1_000.0,
                isp_vac_s: 300.0,
                isp_sl_s: 250.0,
                throttle_range: ThrottleRange { min: 0.0, max: 1.0 },
                gimbal_deg: 0.0,
                ignitions: None,
                rated_burn_s: 100.0,
                propellants: vec![(ResourceId::from("liquid_fuel"), 1.0)],
                ullage_required: false,
            }],
            reliability: ReliabilityDef {
                base_mtbf_s: 1_000.0,
                infant_mortality: 0.0,
                qualification_curve: QualCurve {
                    knots: vec![QualKnot {
                        qualification_units: 0.0,
                        hazard_multiplier: 1.0,
                    }],
                },
                regime_stress: vec![],
                dormant_stress: 0.0,
                failure_modes: vec![(FailureMode::DegradedThrust, 1.0)],
            },
            tech_requirement: TechNodeId::from(tech),
        }
    }

    fn world_with_tree() -> World {
        let mut w = World::new();
        w.set_tech_tree(tree());
        w.science.banked = 100.0;
        w
    }

    /// Enqueue `cmd` and drain one tick — the report's `rejected`
    /// carries `Recoverable` command errors (§2.2.1), so a failed
    /// apply asserts on `rejected` non-empty, not on `step_frame`
    /// failing.
    fn drain(w: &mut World, cmd: SimCommand) -> crate::AdvanceReport {
        w.enqueue(SourceClass::Ui, 0, cmd);
        w.step_frame(w.clock().ticks() + 1).unwrap()
    }

    fn ok(w: &mut World, cmd: SimCommand) {
        let r = drain(w, cmd);
        assert!(r.rejected.is_empty(), "rejected: {:?}", r.rejected);
    }

    fn rejected(w: &mut World, cmd: SimCommand) {
        let r = drain(w, cmd);
        assert!(!r.rejected.is_empty(), "expected a rejection");
    }

    #[test]
    fn research_tech_spends_science_and_unlocks() {
        let mut w = world_with_tree();
        ok(
            &mut w,
            SimCommand::ResearchTech {
                node: "tech.start".into(),
            },
        );
        assert!(w.tech().is_researched(&"tech.start".into()));
        assert_eq!(w.science().banked, 100.0); // free node spends nothing
        ok(
            &mut w,
            SimCommand::ResearchTech {
                node: "tech.tier2".into(),
            },
        );
        assert_eq!(w.science().banked, 70.0);
        // Part gating follows the researched set.
        let d = def("tech.tier2");
        assert!(w.tech().is_part_unlocked(&d));
        // Rejects: already researched, unknown node, missing prereq,
        // insufficient science.
        rejected(
            &mut w,
            SimCommand::ResearchTech {
                node: "tech.tier2".into(),
            },
        );
        rejected(
            &mut w,
            SimCommand::ResearchTech {
                node: "tech.nope".into(),
            },
        );
        let mut w2 = world_with_tree();
        w2.science.banked = 10.0;
        rejected(
            &mut w2,
            SimCommand::ResearchTech {
                node: "tech.tier2".into(),
            },
        );
        ok(
            &mut w2,
            SimCommand::ResearchTech {
                node: "tech.start".into(),
            },
        );
        rejected(
            &mut w2,
            SimCommand::ResearchTech {
                node: "tech.tier2".into(),
            },
        );
        assert_eq!(w2.science().banked, 10.0); // rejected — no debit
        // No tree at all → typed rejection.
        let mut w3 = World::new();
        w3.science.banked = 1e6;
        rejected(
            &mut w3,
            SimCommand::ResearchTech {
                node: "tech.start".into(),
            },
        );
    }

    #[test]
    fn upgrade_levels_buy_up_to_granted_cap() {
        let mut w = world_with_tree();
        // Track locked until the granting node is researched.
        rejected(
            &mut w,
            SimCommand::ResearchUpgradeLevel {
                track: "track.isp".into(),
            },
        );
        ok(
            &mut w,
            SimCommand::ResearchTech {
                node: "tech.start".into(),
            },
        );
        ok(
            &mut w,
            SimCommand::ResearchTech {
                node: "tech.tier2".into(),
            },
        );
        ok(
            &mut w,
            SimCommand::ResearchUpgradeLevel {
                track: "track.isp".into(),
            },
        );
        ok(
            &mut w,
            SimCommand::ResearchUpgradeLevel {
                track: "track.isp".into(),
            },
        );
        assert_eq!(w.tech().track_level(&"track.isp".into()), 2);
        assert_eq!(w.science().banked, 100.0 - 30.0 - 10.0 - 20.0);
        // Cap is 2 — a third buy rejects; unknown track rejects.
        rejected(
            &mut w,
            SimCommand::ResearchUpgradeLevel {
                track: "track.isp".into(),
            },
        );
        rejected(
            &mut w,
            SimCommand::ResearchUpgradeLevel {
                track: "track.x".into(),
            },
        );
    }

    #[test]
    fn build_part_gates_and_freezes_upgrades() {
        let mut w = world_with_tree();
        let d = def("tech.tier2");
        // Locked until tier2 is researched.
        assert!(w.build_part(&d, "stock".into(), 1.0, [0; 32]).is_err());
        ok(
            &mut w,
            SimCommand::ResearchTech {
                node: "tech.start".into(),
            },
        );
        ok(
            &mut w,
            SimCommand::ResearchTech {
                node: "tech.tier2".into(),
            },
        );
        let b0 = w.build_part(&d, "stock".into(), 1.0, [0; 32]).unwrap();
        assert!(b0.upgrade_levels_at_build.is_empty());
        // Buy level 1 → the next build freezes ×1.1 on IspVacS.
        ok(
            &mut w,
            SimCommand::ResearchUpgradeLevel {
                track: "track.isp".into(),
            },
        );
        let b1 = w.build_part(&d, "stock".into(), 1.0, [0; 32]).unwrap();
        let tl = b1.upgrade_levels_at_build[&StatPath::IspVacS];
        assert_eq!(tl.levels, 1);
        assert_eq!(tl.multiplier, 1.1);
        // Frozen-at-build (§3.3): b1 keeps ×1.1 after level 2 is bought.
        ok(
            &mut w,
            SimCommand::ResearchUpgradeLevel {
                track: "track.isp".into(),
            },
        );
        let b2 = w.build_part(&d, "stock".into(), 1.0, [0; 32]).unwrap();
        assert_eq!(
            b2.upgrade_levels_at_build[&StatPath::IspVacS].multiplier,
            1.1 * 1.1
        );
        assert_eq!(
            b1.upgrade_levels_at_build[&StatPath::IspVacS].multiplier,
            1.1
        );
        // And the composed stat resolves through the M3 resolver.
        let res = csp_data::ResourceFile {
            schema_version: 1,
            resources: vec![csp_data::ResourceDef {
                id: csp_data::ResourceId::from("liquid_fuel"),
                display_name: "r.liquid_fuel".into(),
                mass_per_unit_kg: 5.0,
            }],
        };
        let eff =
            csp_phys::resolve_part(&d, &b2, &res, &csp_phys::FamilyQualification::FRESH, None)
                .unwrap();
        let csp_data::ModuleDef::Engine { isp_vac_s, .. } = &eff.modules[0] else {
            panic!()
        };
        assert!((isp_vac_s - 300.0 * 1.21).abs() < 1e-9);
    }

    #[test]
    fn tech_state_survives_snapshot_restore() {
        let mut w = world_with_tree();
        ok(
            &mut w,
            SimCommand::ResearchTech {
                node: "tech.start".into(),
            },
        );
        ok(
            &mut w,
            SimCommand::ResearchTech {
                node: "tech.tier2".into(),
            },
        );
        ok(
            &mut w,
            SimCommand::ResearchUpgradeLevel {
                track: "track.isp".into(),
            },
        );
        w.tech
            .qualifications
            .entry("fam.e".into())
            .or_default()
            .flight_units = 12.0;
        let pre = w.state_hash().unwrap();
        let snap = w.snapshot().unwrap();
        let mut r = World::restore(
            crate::orbit::PatchedConics::new(BTreeMap::new()),
            None,
            snap,
        )
        .unwrap();
        r.set_tech_tree(tree());
        assert_eq!(r.state_hash().unwrap(), pre);
        assert!(r.tech().is_researched(&"tech.tier2".into()));
        assert_eq!(r.tech().track_level(&"track.isp".into()), 1);
    }

    #[test]
    fn state_hash_covers_tech() {
        let a = world_with_tree();
        let mut b = world_with_tree();
        ok(
            &mut b,
            SimCommand::ResearchTech {
                node: "tech.start".into(),
            },
        );
        assert_ne!(a.state_hash().unwrap(), b.state_hash().unwrap());
        let mut c = world_with_tree();
        ok(
            &mut c,
            SimCommand::ResearchTech {
                node: "tech.start".into(),
            },
        );
        assert_eq!(b.state_hash().unwrap(), c.state_hash().unwrap());
    }

    /// The §3.3 spawn/manufacture gate: a craft whose `part_ids`
    /// carries a catalog part launches only when its
    /// `tech_requirement` is researched — the `def("tech.tier2")`
    /// fixture's engine is locked until both tree nodes are.
    #[test]
    fn part_gate_enforces_research() {
        let mut w = World::new();
        w.set_tech_tree(tree());
        let d = def("tech.tier2");
        w.set_parts(std::collections::BTreeMap::from([(
            d.id.clone(),
            d.clone(),
        )]));
        // The gate resolves identity against the installed catalog —
        // `parts` only supplies the parallel-table length (a zero-mass
        // lump is legal weld input).
        let lump = || csp_phys::WeldPart {
            mass_kg: 1.0,
            com: csp_core::DVec3::ZERO,
            inertia: csp_phys::SymMat3::solid_box(1.0, csp_core::DVec3::new(0.1, 0.1, 0.1)),
            collider: csp_phys::ColliderShape::Box {
                half_extents: csp_core::DVec3::new(0.1, 0.1, 0.1),
            },
            force_sites: vec![],
            aero: csp_data::AeroDef::constant(0.0, 0.0, 1.0),
            thermal: csp_data::ThermalDef {
                heat_capacity_j_per_k: 1.0,
                max_temp_k: 1_000.0,
                emissivity: 0.8,
                conduction_w_per_k: 0.0,
            },
            structural: csp_data::StructuralDef::unlimited(),
            dock_ports: vec![],
            tanks: vec![],
            crew_seats: 0,
            experiments: vec![],
            lab: None,
            engine: None,
            parachute: None,
        };
        let craft = |ids: Vec<PartId>| crate::CraftDef {
            part_build_resources: Vec::new(),
            hash_memo: Default::default(),
            graph: csp_phys::PartGraph::new(csp_phys::PartNodeId(0)),
            parts: vec![lump()],
            part_costs: vec![1.0],
            reliability: vec![],
            part_ids: ids,
        };
        // Locked: tier2 is unresearched.
        let err = w.check_part_gate(&craft(vec![d.id.clone()])).unwrap_err();
        assert!(err.reason().contains("tech-locked"), "{err}");
        // Unknown ids and shape mismatches refuse too.
        assert!(w.check_part_gate(&craft(vec!["missing".into()])).is_err(),);
        assert!(
            w.check_part_gate(&craft(vec![d.id.clone(), d.id.clone()]))
                .is_err()
        );
        // Empty identity is the ungated convention.
        let anon = crate::CraftDef {
            part_build_resources: Vec::new(),
            hash_memo: Default::default(),
            graph: csp_phys::PartGraph::new(csp_phys::PartNodeId(0)),
            parts: vec![],
            part_costs: vec![],
            reliability: vec![],
            part_ids: vec![],
        };
        assert!(w.check_part_gate(&anon).is_ok());
        // Research the chain — the part unlocks (a direct state write:
        // the gate reads `tech`, it does not care how it got there).
        w.tech.researched.insert("tech.tier2".into());
        assert!(w.check_part_gate(&craft(vec![d.id.clone()])).is_ok());
        // A tree-less world runs ungated.
        let mut bare = World::new();
        bare.set_parts(std::collections::BTreeMap::from([(d.id.clone(), d)]));
        assert!(bare.check_part_gate(&craft(vec!["test.e".into()])).is_ok());
    }

    #[test]
    fn credit_qualification_accumulates() {
        let mut t = TechState::default();
        t.credit_qualification("fam.e".into(), 5.0, 2.0, 1).unwrap();
        t.credit_qualification("fam.e".into(), 1.0, 0.0, 0).unwrap();
        let q = &t.qualifications[&"fam.e".into()];
        assert_eq!(q.flight_units, 6.0);
        assert_eq!(q.bench_units, 2.0);
        assert_eq!(q.observed_failures, 1);
        assert!(
            t.credit_qualification("fam.e".into(), -1.0, 0.0, 0)
                .is_err()
        );
    }
}
