//! §3.5's build orders — the M8-B manufacture pipeline (the
//! `Manufacturing`/`Construction` slots of `SUBSYSTEM_ORDER`'s
//! event-driven shape, though orders need no interval advance: an
//! order is a record plus its `ConstructionComplete` deadline,
//! created together, like `transfers`'s record/event pairing).
//!
//! `SimCommand::ManufactureCraft { site, craft, build_resources }` is
//! a RESERVATION: at apply time the command checks and atomically
//! spends the launch site's `economy.sites` inventory rows plus the
//! craft's funds cost, then schedules `ConstructionComplete` at
//! `now + build_ticks`. The craft itself materializes ONLY at the
//! event — a hull that does not exist cannot fly, be seen, or hash.
//!
//! - `site` must name a `SiteDef` with `launch` — the §3.5 launch
//!   site's own inventory is the bill the order pays out of (per-site
//!   inventories are the supply loop's staging ground; a colony's
//!   stores are a different inventory).
//! - `build_resources` is the craft's aggregate BOM — the
//!   `(resource, units)` pairs the caller summed over
//!   `csp_phys::EffectivePart::build_resources` (`part_costs` rides
//!   `CraftDef` the same way: the command log is self-contained, and
//!   the sim re-checks shape, not the arithmetic).
//! - `build_ticks = ceil(mass_kg / manufacture_rate_kg_per_s ·
//!   TICK_HZ)`, mass over live graph nodes — a bigger craft simply
//!   takes longer, and every order reserves its future vessel slot
//!   (`vessels + builds < max_vessels`) so completion never races
//!   the cap.

use std::collections::BTreeMap;

use csp_core::{SimError, StableDiscriminant, StateHasher, TICK_HZ};
use csp_data::{ResourceId, SiteId};
use serde::{Deserialize, Serialize};

use crate::TickShadow;
use crate::VesselId;
use crate::economy::FundsReason;
use crate::events::EventKind;

/// §2.2 build-order identity — a monotonic counter, hashed and
/// serialized, never reused (the `TransferId` convention).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BuildId(pub u64);

impl StableDiscriminant for BuildId {
    fn discriminant(&self) -> u16 {
        0
    }
}

/// A live build order — serialized and hashed sim state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActiveBuild {
    /// Its arena key, inline per §2.2.
    pub id: BuildId,
    /// The launch site paying for it — delivery lands there.
    pub site: SiteId,
    /// The craft under construction.
    pub craft: crate::CraftDef,
    /// The `ConstructionComplete` deadline.
    pub complete_tick: u64,
}

/// §7.1's state-hash walk for the build arena.
pub fn hash_builds(h: &mut StateHasher, builds: &BTreeMap<BuildId, ActiveBuild>) {
    h.btree_map(
        builds,
        |h, id| {
            h.u64(id.0);
        },
        |h, b| {
            h.u64(b.id.0);
            h.str(b.site.as_str());
            crate::hash_craft(h, &b.craft);
            h.u64(b.complete_tick);
        },
    );
}

/// Tick ceiling for a duration — `rate·dt ≥ amount`, the transfers
/// convention (min 1: a zero-mass craft still lands on the event
/// boundary, never the same tick it was ordered).
fn ticks_for(mass_kg: f64, rate_kg_per_s: f64) -> u64 {
    let t = (mass_kg / rate_kg_per_s * TICK_HZ as f64).ceil() as u64;
    t.max(1)
}

/// The craft's live-graph mass — `WeldPart::mass_kg` (wet: tanks
/// build full, so filling them is part of the job) over nodes only.
fn craft_mass_kg(craft: &crate::CraftDef) -> f64 {
    craft
        .graph
        .nodes
        .values()
        .map(|n| {
            craft
                .parts
                .get(n.effective_index as usize)
                .map(|p| p.mass_kg)
                .unwrap_or(0.0)
        })
        .sum()
}

impl crate::World {
    /// `ManufactureCraft` — the reservation: validate site, craft,
    /// BOM, funds, and the vessel-cap slot, then spend everything and
    /// schedule the delivery. Nothing about the craft exists until
    /// `ConstructionComplete`.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn apply_manufacture_craft(
        &mut self,
        site: SiteId,
        craft: crate::CraftDef,
        build_resources: Vec<(ResourceId, f64)>,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        if !self.economy_cfg.is_valid() {
            return Err(SimError::Fatal("economy: invalid config"));
        }
        let Some(def) = self.site_defs.get(&site) else {
            return Err(SimError::Recoverable("manufacture: unknown site"));
        };
        if !def.launch {
            return Err(SimError::Recoverable(
                "manufacture: build orders consume a launch-site inventory",
            ));
        }
        if !self.economy.sites.contains_key(&site) {
            return Err(SimError::Recoverable(
                "manufacture: site has no state arena row",
            ));
        }
        // Shape checks — the same gates `apply_spawn_physical` runs:
        // the weld must succeed at delivery, so it must be sound at
        // order time.
        crate::transfers::check_tank_masses(&craft)?;
        if craft.part_costs.len() != craft.parts.len()
            || !craft.part_costs.iter().all(|c| c.is_finite() && *c >= 0.0)
        {
            return Err(SimError::Recoverable("manufacture: incoherent part costs"));
        }
        // M9b: the reliability table is empty (no model) or covers
        // `parts` exactly with valid specs — the `part_costs` gate.
        if !craft.reliability.is_empty()
            && (craft.reliability.len() != craft.parts.len()
                || !craft.reliability.iter().all(|s| s.is_valid()))
        {
            return Err(SimError::Recoverable(
                "manufacture: incoherent reliability table",
            ));
        }
        csp_phys::weld::weld(&craft.graph, &craft.parts)
            .map_err(|_| SimError::Recoverable("manufacture: craft fails the weld gate"))?;
        let mut bom: BTreeMap<ResourceId, f64> = BTreeMap::new();
        for (r, q) in &build_resources {
            if !(q.is_finite() && *q > 0.0) {
                return Err(SimError::Recoverable(
                    "manufacture: non-positive or non-finite build resource",
                ));
            }
            *bom.entry(r.clone()).or_insert(0.0) += q;
        }
        let cost = crate::economy::craft_cost_micros(&craft);
        if cost > self.economy.funds_micros {
            return Err(SimError::Recoverable("manufacture: insufficient funds"));
        }
        let mass = craft_mass_kg(&craft);
        if !mass.is_finite() {
            return Err(SimError::Recoverable("manufacture: non-finite craft mass"));
        }
        // Every live order reserves the slot its delivery needs —
        // the cap race is an apply-time reject, never a mid-air
        // completion failure (`vessel_slots_in_use` counts the build
        // arena the same way every other growth path now does).
        if self.vessel_slots_in_use() >= self.caps.max_vessels {
            return Err(SimError::Recoverable("manufacture: vessel cap"));
        }
        // Affordability BEFORE the shadow — the spend is atomic, so
        // a short BOM entry rejects without touching state.
        for (r, q) in &bom {
            let have = self
                .economy
                .sites
                .get(&site)
                .and_then(|st| st.inventory.get(r))
                .copied()
                .unwrap_or(0.0);
            if have < *q {
                return Err(SimError::Recoverable(
                    "manufacture: insufficient site inventory",
                ));
            }
        }
        // Commit — the §2.6.4 rollback law: shadow before write.
        self.shadow_economy(shadow);
        for (r, q) in &bom {
            let st = self
                .economy
                .sites
                .get_mut(&site)
                .ok_or(SimError::Fatal("manufacture: site row vanished"))?;
            *st.inventory
                .get_mut(r)
                .ok_or(SimError::Fatal("manufacture: inventory row vanished"))? -= q;
        }
        crate::economy::charge(
            &mut self.economy,
            self.clock.ticks(),
            cost,
            FundsReason::Manufacture,
        );
        self.shadow_builds(shadow);
        let id = BuildId(self.next_build_id);
        self.next_build_id = self
            .next_build_id
            .checked_add(1)
            .ok_or(SimError::Fatal("build id overflow"))?;
        let now = self.clock.ticks();
        let complete_tick = now
            .checked_add(ticks_for(mass, self.economy_cfg.manufacture_rate_kg_per_s))
            .ok_or(SimError::Fatal("manufacture: tick overflow"))?;
        self.builds.insert(
            id,
            ActiveBuild {
                id,
                site,
                craft,
                complete_tick,
            },
        );
        self.schedule_event(
            EventKind::ConstructionComplete { build: id },
            complete_tick,
            shadow,
        )
    }

    /// `ConstructionComplete` — delivery: the craft becomes a
    /// surface-latched vessel at its site. TOTAL like every event
    /// firing (a mid-air failure would orphan the record under the
    /// snapshot pairing): a vanished record or site is a no-op, the
    /// slot was reserved at order time.
    pub(crate) fn fire_construction_complete(
        &mut self,
        build: BuildId,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        self.shadow_builds(shadow);
        let Some(b) = self.builds.remove(&build) else {
            return Ok(());
        };
        if !self.site_defs.contains_key(&b.site) {
            // The site vanished mid-build — the delivery drops with
            // the record (total: an event never fails).
            return Ok(());
        }
        self.deliver_built_vessel(b, shadow)
    }

    /// The shared delivery tail — insert the latched vessel record.
    fn deliver_built_vessel(
        &mut self,
        b: ActiveBuild,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        self.shadow_vessels(shadow);
        let id = VesselId(self.next_vessel_id);
        self.next_vessel_id = self
            .next_vessel_id
            .checked_add(1)
            .ok_or(SimError::Fatal("vessel id counter overflow"))?;
        let def = self
            .site_defs
            .get(&b.site)
            .cloned()
            .ok_or(SimError::Fatal("manufacture: site vanished mid-delivery"))?;
        let ambient = self
            .physics
            .as_ref()
            .map(|p| csp_phys::ambient_k(p.backend.environment(), def.altitude_m));
        let tank_contents = crate::transfers::spawn_contents(&b.craft);
        let thermal = ambient.map_or_else(crate::VesselThermal::default, |a| {
            crate::VesselThermal::at_ambient(&b.craft.graph, a)
        });
        // M9b §3.4: construction IS part creation — the delivered
        // craft's per-node records draw their `θ₀` here, so a revert
        // to the post-delivery snapshot preserves the pre-drawn
        // thresholds (the allocator rides `shadow_vessels`).
        let reliability = crate::reliability::allocate_records(
            self.save_seed,
            &b.craft,
            self.clock.ticks(),
            &mut self.next_part_instance_id,
        )?;
        self.vessels.insert(
            id,
            crate::Vessel {
                id,
                craft: Some(b.craft),
                state: crate::VesselState::SurfaceLatched(crate::SurfacePose {
                    body: def.body,
                    latitude_rad: def.latitude_rad,
                    longitude_rad: def.longitude_rad,
                    altitude_m: def.altitude_m,
                    // Identity is the "parked at epoch orientation"
                    // pose — the body-fixed frame the latched
                    // convention captures anyway.
                    orientation: csp_core::DQuat::IDENTITY,
                }),
                aero_mask: csp_phys::ConfigMask::ALL,
                thermal,
                debris_since: None,
                maneuver: None,
                seams: Vec::new(),
                tank_contents,
                crew: Vec::new(),
                science: crate::VesselScience::default(),
                joined_colony: None,
                reliability,
                control_lease: crate::Lease::Player,
            },
        );
        // M9b: a freshly delivered vessel sits dormant — restamp the
        // records under the dormant rate now (the publish slot's
        // next pass derives the pending crossings from this stamp).
        self.reliability_residence_change(id, self.clock.ticks(), true);
        Ok(())
    }
}

#[cfg(test)]
// §2.13's no-panic rule covers shipping fallible paths; tests signal
// failure by panicking, so unwrap/expect stays legal inside test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::{SimCommand, SourceClass, World};
    use csp_data::{BodyId, KeplerElements, LocKey, SiteDef};
    use std::collections::BTreeMap;

    /// A two-body test system (the colonies.rs convention): a sun and
    /// a 6 h day planet.
    fn test_system() -> crate::orbit::PatchedConics {
        let mut bodies = BTreeMap::new();
        bodies.insert(
            BodyId::from("sun"),
            crate::orbit::BodyOrbit {
                mu_m3_per_s2: 1.0e20,
                soi_m: None,
                parent: None,
                elements: None,
            },
        );
        bodies.insert(
            BodyId::from("terra"),
            crate::orbit::BodyOrbit {
                mu_m3_per_s2: 4.0e14,
                soi_m: Some(1.0e9),
                parent: Some(BodyId::from("sun")),
                elements: Some(KeplerElements {
                    semi_major_axis_m: 1.5e11,
                    eccentricity: 0.0,
                    inclination_rad: 0.0,
                    ascending_node_rad: 0.0,
                    arg_periapsis_rad: 0.0,
                    mean_anomaly_epoch_rad: 0.0,
                    period_s: 86400.0 * 365.25,
                }),
            },
        );
        crate::orbit::PatchedConics::new(bodies)
    }

    /// One launch site + its state row holding an ore stockpile; the
    /// colony env the join/transfer paths read.
    fn world() -> World {
        let mut w = World::with_system(test_system());
        let mut sites = BTreeMap::new();
        sites.insert(
            SiteId::from("pad"),
            SiteDef {
                schema_version: csp_data::SITE_SCHEMA_VERSION,
                id: SiteId::from("pad"),
                display_name: LocKey::from("site.pad"),
                body: BodyId::from("terra"),
                latitude_rad: 0.0,
                longitude_rad: 0.0,
                altitude_m: 0.0,
                launch: true,
                resource_yields: vec![],
            },
        );
        w.set_sites(sites);
        w.economy.sites.insert(
            SiteId::from("pad"),
            crate::economy::SiteState {
                id: SiteId::from("pad"),
                inventory: BTreeMap::from([(ResourceId::from("ore"), 100.0)]),
            },
        );
        w
    }

    /// A one-node 100 kg craft costing 10.0 funds.
    fn craft() -> crate::CraftDef {
        let mut graph = csp_phys::PartGraph::new(csp_phys::PartNodeId(0));
        graph.nodes.insert(
            csp_phys::PartNodeId(0),
            csp_phys::PartNode::new(None, csp_phys::PartTransform::IDENTITY, 0),
        );
        crate::CraftDef {
            graph,
            parts: vec![csp_phys::WeldPart {
                experiments: Vec::new(),
                lab: None,
                dock_ports: vec![],
                tanks: vec![csp_phys::TankSpec {
                    resource: ResourceId::from("ore"),
                    capacity: 10.0,
                    mass_per_unit_kg: 1.0,
                }],
                crew_seats: 0,
                structural: csp_data::StructuralDef::unlimited(),
                mass_kg: 100.0,
                com: csp_core::DVec3::ZERO,
                inertia: csp_phys::SymMat3::solid_box(100.0, csp_core::DVec3::new(0.5, 0.5, 0.5)),
                collider: csp_phys::ColliderShape::Box {
                    half_extents: csp_core::DVec3::new(0.5, 0.5, 0.5),
                },
                force_sites: vec![],
                aero: csp_data::AeroDef::constant(0.0, 0.0, 1.0),
                thermal: csp_data::ThermalDef {
                    heat_capacity_j_per_k: 100_000.0,
                    max_temp_k: 1_000.0,
                    emissivity: 0.8,
                    conduction_w_per_k: 10.0,
                },
            }],
            part_costs: vec![10.0],
            reliability: vec![],
        }
    }

    /// The §3.5 build order: inventory + funds spend at the command
    /// boundary, the craft materializes latched at the site only when
    /// `ConstructionComplete` fires — 100 kg at 50 kg/s = 2 s = 128
    /// ticks.
    #[test]
    fn manufacture_spends_at_order_delivers_at_event() {
        let mut w = world();
        let funds0 = w.economy().funds_micros;
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::ManufactureCraft {
                site: SiteId::from("pad"),
                craft: craft(),
                build_resources: vec![(ResourceId::from("ore"), 20.0)],
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        // Spent at the boundary: 20 ore + 10 funds — no vessel yet.
        let site = &w.economy().sites[&SiteId::from("pad")];
        assert!((site.inventory[&ResourceId::from("ore")] - 80.0).abs() < 1e-9);
        assert_eq!(funds0 - w.economy().funds_micros, 10_000_000);
        assert_eq!(w.vessels().len(), 0);
        assert_eq!(w.builds.len(), 1);
        // Completion: the craft arrives surface-latched at the pad.
        w.step_frame(w.clock().ticks() + 128).unwrap();
        assert_eq!(w.vessels().len(), 1);
        assert!(w.builds.is_empty());
        let v = w.vessels().values().next().unwrap();
        assert!(matches!(v.state, crate::VesselState::SurfaceLatched(_)));
        // Tanks build full (the spawn convention): 10 ore aboard.
        assert!(
            (crate::transfers::vessel_resource(v, &ResourceId::from("ore")) - 10.0).abs() < 1e-9
        );
    }

    /// Affordability gates: a BOM entry over the stockpile, a missing
    /// launch site, and a non-launch site all reject without a spend.
    #[test]
    fn manufacture_rejects_the_unfunded() {
        let mut w = world();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::ManufactureCraft {
                site: SiteId::from("pad"),
                craft: craft(),
                build_resources: vec![(ResourceId::from("ore"), 200.0)],
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(!r.rejected.is_empty());
        assert_eq!(
            w.economy().sites[&SiteId::from("pad")].inventory[&ResourceId::from("ore")],
            100.0
        );
        // A non-launch site: clone the pad minus the flag.
        let mut sites = BTreeMap::new();
        sites.insert(
            SiteId::from("field"),
            SiteDef {
                schema_version: csp_data::SITE_SCHEMA_VERSION,
                id: SiteId::from("field"),
                display_name: LocKey::from("site.field"),
                body: BodyId::from("terra"),
                latitude_rad: 0.0,
                longitude_rad: 0.0,
                altitude_m: 0.0,
                launch: false,
                resource_yields: vec![],
            },
        );
        w.set_sites(sites);
        w.economy.sites.insert(
            SiteId::from("field"),
            crate::economy::SiteState {
                id: SiteId::from("field"),
                inventory: BTreeMap::from([(ResourceId::from("ore"), 100.0)]),
            },
        );
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::ManufactureCraft {
                site: SiteId::from("field"),
                craft: craft(),
                build_resources: vec![(ResourceId::from("ore"), 1.0)],
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(!r.rejected.is_empty());
        assert!(w.builds.is_empty());
    }

    /// Snapshot mid-build → restore → the order still pairs with its
    /// event and delivers identically; two identical runs hash equal.
    #[test]
    fn manufacture_snapshot_restore_and_determinism() {
        let run = || {
            let mut w = world();
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::ManufactureCraft {
                    site: SiteId::from("pad"),
                    craft: craft(),
                    build_resources: vec![(ResourceId::from("ore"), 20.0)],
                },
            );
            w.step_frame(w.clock().ticks() + 1).unwrap();
            w
        };
        let mut a = run();
        let b = run();
        assert_eq!(a.state_hash().unwrap(), b.state_hash().unwrap());
        let snap = a.snapshot().unwrap();
        let mut r = World::restore(test_system(), None, snap).unwrap();
        r.set_sites(BTreeMap::from([(
            SiteId::from("pad"),
            SiteDef {
                schema_version: csp_data::SITE_SCHEMA_VERSION,
                id: SiteId::from("pad"),
                display_name: LocKey::from("site.pad"),
                body: BodyId::from("terra"),
                latitude_rad: 0.0,
                longitude_rad: 0.0,
                altitude_m: 0.0,
                launch: true,
                resource_yields: vec![],
            },
        )]));
        r.step_frame(300).unwrap();
        a.step_frame(300).unwrap();
        assert_eq!(a.state_hash().unwrap(), r.state_hash().unwrap());
        assert_eq!(r.vessels().len(), 1);
    }
}
