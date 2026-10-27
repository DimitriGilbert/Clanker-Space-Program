//! M8-C: the §3.5 vessel-side life support — "optional
//! `Oxygen`/`ElectricCharge`/`Food` consumption for crewed ops,
//! toggleable per save, advanced by interval like every other
//! subsystem. Publishes `LifeSupportDepleted` (`Clamping`)."
//! This module fills the `LifeSupport` slot (index 4) of the
//! §2.6.5 subsystem order; the colony side's `LifeSupport` module
//! lives in [`crate::colonies`].
//!
//! # The model — the `SalaryDue` precedent, verbatim
//!
//! Vessel-side consumption is a **recurring scheduled event**, never
//! an interval integration: `LifeSupportCheck` (`Silent`) fires once
//! per `check_period_ticks` on the absolute period grid — lazily
//! published by the slot's advance exactly like `publish_economy` —
//! and debits `rate · period` per crew member aboard every crewed
//! vessel from their `tank_contents`. Mutation happens only at the
//! absolute-tick event boundary, so the debit is `rate·period`
//! exactly — identical in every §2.6.1 partition, with no
//! `fl(fl(x − r·dt₁) − r·dt₂)` last-ulp drift to document.
//!
//! # Depletion — transitions are events
//!
//! A shortfall at the check does not kill inside the firing (the
//! colony precedent: mutations land at firings, never inside a
//! solve). The check schedules `VesselLifeSupportDepleted { vessel }`
//! (`Clamping`) one tick out — once per vessel (a pending depletion
//! is not re-scheduled). Its firing marks every crew member aboard
//! `Lost`: roster members take `CrewStatus::Lost` +
//! `CrewLocation::Unassigned`, and the vessel's seat record clears —
//! dead crew hold no seats. The event names a vessel that may no
//! longer exist (removed between schedule and firing): the firing is
//! a no-op for a missing vessel, and removal paths never scrub it —
//! the `TransferComplete` precedent.
//!
//! # The toggle — `life_support_enabled`
//!
//! A per-save serialized flag (§3.5's "toggleable per save"), flipped
//! by `SimCommand::SetLifeSupportEnabled` so a mid-save toggle is
//! §7.6-logged and replayable. **The default is OFF** — §3.5's
//! "optional" reads as opt-in: the authored part set ships no
//! `oxygen`/`food` tanks, so a default-on world would deplete every
//! crewed vessel at the first check and make all crewed play lethal.
//! Enabling the flag (construction-time `set_life_support_enabled`,
//! or the logged command mid-save) only makes sense once vessels and
//! colonies actually carry the LS resources — see "Resources" below.
//! Disabled means: the slot publishes no
//! checks, a check's firing is a no-op, a pending depletion's firing
//! is a no-op, and the colony `LifeSupport` module draws nothing and
//! never earns a `LifeSupportDepleted` (the colonies' solve reads an
//! effective crew of zero — see `recompute_colony`). Toggling
//! *scrubs* the pending vessel-side events and re-solves every live
//! colony, so neither direction leaves a stale demand or a stale
//! transition in the heap.
//!
//! # Resources
//!
//! The required set is the plan's three: `oxygen`, `food`,
//! `electric_charge` — drawn from the vessel's tank stores. A vessel
//! with crew and no life-support tanks depletes on the first check
//! after boarding (the honest minimal reading: nothing aboard means
//! nothing to breathe).

use serde::{Deserialize, Serialize};

use csp_core::{SimError, StateHasher, TICK_HZ};
use csp_data::{BalanceDef, ResourceId};

use crate::economy::{CrewLocation, CrewStatus};
use crate::transfers::{debit, reweld_if_mass_changed};
use crate::{EventKind, TickShadow, VesselId, World};

/// `balance.ron: life_support_check_period_s` mirror for data-less
/// worlds — 3600 s = one sim-hour between vessel checks.
pub const DEFAULT_CHECK_PERIOD_S: f64 = 3_600.0;
/// `balance.ron: life_support_oxygen_per_crew_s` mirror — authored
/// fiction at the TAC order of magnitude.
pub const DEFAULT_OXYGEN_PER_CREW_S: f64 = 0.02;
/// `balance.ron: life_support_food_per_crew_s` mirror.
pub const DEFAULT_FOOD_PER_CREW_S: f64 = 0.005;
/// `balance.ron: life_support_ec_per_crew_s` mirror — the powered
/// scrubber's draw per crew member.
pub const DEFAULT_EC_PER_CREW_S: f64 = 0.05;

/// The `ResourceId` spellings of the three required consumables —
/// the plan's `Oxygen`/`ElectricCharge`/`Food`.
pub const OXYGEN: &str = "oxygen";
/// See [`OXYGEN`].
pub const FOOD: &str = "food";
/// See [`OXYGEN`]. (`electric_charge` is
/// [`crate::colonies::ELECTRIC_CHARGE`] — reused, never re-spelled
/// in the solve.)
const EC: &str = crate::colonies::ELECTRIC_CHARGE;

/// §3.5's vessel-side life-support tunables — construction config
/// (loaded from `balance.ron` in real worlds; the `DEFAULT_*`
/// mirrors otherwise), **serialized and hashed** like `economy_cfg`:
/// every field changes state evolution (the check cadence and the
/// debited amounts).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LifeSupportConfig {
    /// The check cadence, ticks (`check_period_s · TICK_HZ` at
    /// load). `LifeSupportCheck` fires on this absolute grid.
    pub check_period_ticks: u64,
    /// `oxygen` units per crew member per sim-second.
    pub oxygen_per_crew_s: f64,
    /// `food` units per crew member per sim-second.
    pub food_per_crew_s: f64,
    /// `electric_charge` units per crew member per sim-second.
    pub ec_per_crew_s: f64,
}

/// The `DEFAULT_*` config — `World::new()`'s no-data fallback.
pub fn default_config() -> LifeSupportConfig {
    LifeSupportConfig {
        check_period_ticks: (DEFAULT_CHECK_PERIOD_S * TICK_HZ as f64).ceil() as u64,
        oxygen_per_crew_s: DEFAULT_OXYGEN_PER_CREW_S,
        food_per_crew_s: DEFAULT_FOOD_PER_CREW_S,
        ec_per_crew_s: DEFAULT_EC_PER_CREW_S,
    }
}

impl LifeSupportConfig {
    /// The `balance.ron` → config conversion (§8.3).
    pub fn from_balance(b: &BalanceDef) -> Self {
        Self {
            check_period_ticks: (b.life_support_check_period_s * TICK_HZ as f64)
                .ceil()
                .max(1.0) as u64,
            oxygen_per_crew_s: b.life_support_oxygen_per_crew_s,
            food_per_crew_s: b.life_support_food_per_crew_s,
            ec_per_crew_s: b.life_support_ec_per_crew_s,
        }
    }

    /// The snapshot-restore gate — a zero period would re-arm the
    /// check grid at the clock; a negative/non-finite rate would
    /// mint or eat resources.
    pub fn is_valid(&self) -> bool {
        self.check_period_ticks > 0
            && self.oxygen_per_crew_s.is_finite()
            && self.oxygen_per_crew_s >= 0.0
            && self.food_per_crew_s.is_finite()
            && self.food_per_crew_s >= 0.0
            && self.ec_per_crew_s.is_finite()
            && self.ec_per_crew_s >= 0.0
    }

    /// `(resource, units/s)` per crew member — the required set in
    /// the fixed order the check's debits share (`electric_charge`,
    /// `food`, `oxygen` — ascending `ResourceId`).
    pub fn demands(&self) -> [(ResourceId, f64); 3] {
        [
            (ResourceId::from(EC), self.ec_per_crew_s),
            (ResourceId::from(FOOD), self.food_per_crew_s),
            (ResourceId::from(OXYGEN), self.oxygen_per_crew_s),
        ]
    }
}

/// The world-level config walk — appended to `state_hash` under the
/// flag (the `economy_cfg` precedent).
pub fn hash_life_support(h: &mut StateHasher, enabled: bool, cfg: &LifeSupportConfig) {
    h.bool(enabled);
    h.u64(cfg.check_period_ticks);
    h.f64(cfg.oxygen_per_crew_s);
    h.f64(cfg.food_per_crew_s);
    h.f64(cfg.ec_per_crew_s);
}

/// Lazy-shadow capture for the enable flag — the flag flips inside a
/// tick via `SetLifeSupportEnabled`, so it rides the shadow like
/// `tracked_vessels` (the config never does — it is a pre-run write).
pub(crate) fn shadow_life_support(enabled: bool, slot: &mut Option<bool>) {
    if slot.is_none() {
        *slot = Some(enabled);
    }
}

impl World {
    /// The configured life-support tunables.
    pub fn life_support_config(&self) -> LifeSupportConfig {
        self.life_support_cfg
    }

    /// The life-support tunables — post-construction setter for
    /// worlds built from `balance.ron` (the `set_economy_config`
    /// pattern: call before the first `step_frame`).
    pub fn set_life_support_config(&mut self, cfg: LifeSupportConfig) {
        self.life_support_cfg = cfg;
    }

    /// The per-save toggle (§3.5's "toggleable per save") — sim
    /// state, hashed and serialized.
    pub fn life_support_enabled(&self) -> bool {
        self.life_support_enabled
    }

    /// The construction-time twin of `SimCommand::
    /// SetLifeSupportEnabled` — a pre-run write for harness/data
    /// setup (the `set_save_seed` precedent). Worlds start DISABLED
    /// (opt-in — see the module doc): enabling is the caller's claim
    /// that crewed craft carry `oxygen`/`food`/`electric_charge`
    /// tanks.
    pub fn set_life_support_enabled(&mut self, enabled: bool) {
        self.life_support_enabled = enabled;
    }

    /// Does any vessel carry crew? The publish gate — a crewless
    /// world gets no perpetual no-op check (the `publish_economy`
    /// gate precedent).
    fn any_crewed_vessel(&self) -> bool {
        self.vessels.values().any(|v| !v.crew.is_empty())
    }

    /// `SetLifeSupportEnabled` — the per-save toggle (module doc):
    /// flip the flag, then scrub pending life-support events on
    /// BOTH scopes — vessel-side and colony-side (a disabled
    /// subsystem's scheduled work dies with the flag — toggling off
    /// mid-check leaves no zombie, and a pending colony depletion is
    /// a committed transition only while the subsystem lives) — and
    /// re-solve every live colony so its `LifeSupport` module's
    /// demand tracks the flag in both directions (off removes the
    /// draw, on restores it — the same recompute a crew posting
    /// runs).
    pub(crate) fn apply_set_life_support(
        &mut self,
        enabled: bool,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        if enabled == self.life_support_enabled {
            return Ok(());
        }
        shadow_life_support(self.life_support_enabled, &mut shadow.life_support_enabled);
        self.life_support_enabled = enabled;
        // Scrub the pending events — every kind this subsystem
        // schedules, on both scopes: the vessel-side `LifeSupportCheck`
        // / `VesselLifeSupportDepleted` pair AND the colony-side
        // `LifeSupportDepleted` the recompute earns (a pending
        // depletion is a committed transition only while the
        // subsystem that scheduled it lives; a colony-side straggler
        // would fire one tick out and kill while disabled).
        self.shadow_events_whole(shadow);
        let heap = std::mem::take(&mut self.event_heap);
        self.event_heap = heap
            .into_iter()
            .filter(|e| {
                !matches!(
                    e.kind,
                    EventKind::LifeSupportCheck
                        | EventKind::LifeSupportDepleted { .. }
                        | EventKind::VesselLifeSupportDepleted { .. }
                )
            })
            .collect();
        // Re-solve every live colony — the flag gates the
        // `LifeSupport` module's demand inside the solve.
        let now = self.clock.ticks();
        let ids: Vec<crate::economy::ColonyId> = self.colonies.keys().copied().collect();
        for id in ids {
            self.recompute_colony(id, now, shadow)?;
        }
        Ok(())
    }

    /// The `LifeSupport` slot (§2.6.5 index 4) — publish-only, the
    /// `publish_economy` precedent: ensure exactly one
    /// `LifeSupportCheck` sits on the absolute period grid while the
    /// subsystem is enabled and any vessel carries crew. A pure
    /// function of `(clock, heap, vessels, flag)` — identical in
    /// every §2.6.1 partition.
    pub(crate) fn publish_life_support(&mut self, shadow: &mut TickShadow) -> Result<(), SimError> {
        let period = self.life_support_cfg.check_period_ticks;
        if period == 0 || !self.life_support_enabled || !self.any_crewed_vessel() {
            return Ok(());
        }
        if self
            .event_heap
            .iter()
            .any(|e| matches!(e.kind, EventKind::LifeSupportCheck))
        {
            return Ok(());
        }
        let next = (self.clock.ticks() / period + 1) * period;
        self.schedule_event(EventKind::LifeSupportCheck, next, shadow)
    }

    /// `LifeSupportCheck` firing — the recurring debit (module
    /// doc): each crewed vessel, ascending `VesselId`, pays
    /// `rate · period · crew` per required resource from its
    /// `tank_contents`. A resource the tanks cannot fully cover
    /// debits what exists (the reserve drains honestly) and earns a
    /// `VesselLifeSupportDepleted` one tick out — scheduled at most
    /// once per vessel. Re-arms one period further along the grid
    /// while the predicate still holds, like `fire_salary_due`.
    pub(crate) fn fire_life_support_check(
        &mut self,
        event_tick: u64,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        if self.life_support_enabled && self.life_support_cfg.is_valid() {
            let period_s = self.life_support_cfg.check_period_ticks as f64 / TICK_HZ as f64;
            let demands = self.life_support_cfg.demands();
            let crewed: Vec<(VesselId, u64)> = self
                .vessels
                .iter()
                .filter(|(_, v)| !v.crew.is_empty())
                .map(|(id, v)| (*id, v.crew.len() as u64))
                .collect();
            for (vid, crew) in crewed {
                let mut short = false;
                for (resource, per_crew_s) in &demands {
                    let required = per_crew_s * period_s * crew as f64;
                    if required <= 0.0 {
                        continue;
                    }
                    self.shadow_vessels(shadow);
                    let v = self
                        .vessels
                        .get_mut(&vid)
                        .ok_or(SimError::Fatal("life support: vessel vanished"))?;
                    let (moved, mass_changed) = debit(v, resource, required);
                    reweld_if_mass_changed(v, mass_changed)?;
                    if moved < required {
                        short = true;
                    }
                }
                if short {
                    let kind = EventKind::VesselLifeSupportDepleted { vessel: vid };
                    // One pending depletion per vessel — the colony
                    // transition dedup precedent. The transition lands
                    // one tick past the EVENT's tick — `fire_due_events`
                    // runs this firing with `clock == event_tick`, so
                    // the anchor is identical in every §2.6.1 partition
                    // even when the check fires mid-interval.
                    if !self.event_heap.iter().any(|e| e.kind == kind) {
                        self.schedule_event(kind, event_tick.saturating_add(1), shadow)?;
                    }
                }
            }
        }
        // Re-arm at the next grid tick while the predicate still
        // holds — the `fire_salary_due` cadence. The publish repairs
        // a dropped one anyway; the in-firing re-arm keeps the
        // common case off the slot's repair path.
        let period = self.life_support_cfg.check_period_ticks;
        if period > 0 && self.life_support_enabled && self.any_crewed_vessel() {
            let next = event_tick
                .saturating_add(period)
                .max(self.clock.ticks().saturating_add(1));
            if !self
                .event_heap
                .iter()
                .any(|e| matches!(e.kind, EventKind::LifeSupportCheck))
            {
                self.schedule_event(EventKind::LifeSupportCheck, next, shadow)?;
            }
        }
        Ok(())
    }

    /// `VesselLifeSupportDepleted` firing — the crew consequence
    /// (module doc): every member aboard goes `Lost`, roster-side and
    /// seat-side. A missing vessel is a no-op (the event survives
    /// removal like `TransferComplete`); a disabled subsystem never
    /// kills (the toggle scrubs pending events, and this gate is the
    /// belt to their suspenders).
    ///
    /// The event is committed, the PREDICATE is not: the scheduling
    /// check found a resource the tanks could not fully cover, but a
    /// same-tick `TransferComplete` landing ahead of this firing can
    /// have topped them up — the firing re-checks coverage (the
    /// colony-side `LifeSupportDepleted` arm's `colony_net_solve`
    /// re-check) and a covered vessel's crew survives.
    pub(crate) fn fire_vessel_ls_depleted(
        &mut self,
        vessel: VesselId,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        if !self.life_support_enabled {
            return Ok(());
        }
        let Some(v) = self.vessels.get(&vessel) else {
            return Ok(());
        };
        if v.crew.is_empty() {
            return Ok(());
        }
        // The scheduling predicate, re-evaluated at firing: a resource
        // is unserved iff the tanks hold less than one period's debit
        // (the check's `moved < required` — `debit` moves min(held,
        // required), so a short debit means a short tank).
        let period_s = self.life_support_cfg.check_period_ticks as f64 / TICK_HZ as f64;
        let crew_n = v.crew.len() as f64;
        let still_short = self
            .life_support_cfg
            .demands()
            .iter()
            .any(|(resource, per_crew_s)| {
                let required = per_crew_s * period_s * crew_n;
                required > 0.0 && crate::transfers::vessel_resource(v, resource) < required
            });
        if !still_short {
            return Ok(());
        }
        self.shadow_vessels(shadow);
        self.shadow_economy(shadow);
        let v = self
            .vessels
            .get_mut(&vessel)
            .ok_or(SimError::Fatal("life support: vessel vanished"))?;
        let crew: Vec<crate::transfers::CrewId> = v.crew.iter().map(|c| c.id).collect();
        v.crew.clear();
        for c in &crew {
            if let Some(m) = self.economy.crew.get_mut(c)
                && m.location == CrewLocation::Vessel(vessel)
            {
                m.status = CrewStatus::Lost;
                m.location = CrewLocation::Unassigned;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
// §2.13's no-panic rule covers shipping fallible paths; tests signal
// failure by panicking, so unwrap/expect stays legal inside test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::transfers::vessel_resource;
    use crate::{Lease, SimCommand, SourceClass, Vessel, VesselState, VesselThermal};
    use csp_data::BodyId;
    use csp_phys::{PartGraph, PartNode, PartNodeId, PartTransform, TankSpec};
    use std::collections::BTreeMap;

    /// A test-sized config — a 100-tick cadence keeps the run
    /// inside one `step_frame` call's tick budget (the production
    /// hour-scale period spans many calls).
    fn test_cfg() -> LifeSupportConfig {
        LifeSupportConfig {
            check_period_ticks: 100,
            oxygen_per_crew_s: 0.02,
            food_per_crew_s: 0.005,
            ec_per_crew_s: 0.05,
        }
    }

    /// A one-node craft with a crew seat and `cap` of each LS
    /// resource — the check's debit target.
    fn crew_craft(cap: f64) -> crate::CraftDef {
        let mut graph = PartGraph::new(PartNodeId(0));
        graph.nodes.insert(
            PartNodeId(0),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        crate::CraftDef {
            hash_memo: Default::default(),
            part_ids: vec![],
            graph,
            parts: vec![csp_phys::WeldPart {
                experiments: Vec::new(),
                lab: None,
                engine: None,
                parachute: None,
                dock_ports: vec![],
                tanks: vec![
                    TankSpec {
                        resource: ResourceId::from(OXYGEN),
                        capacity: cap,
                        mass_per_unit_kg: 0.5,
                    },
                    TankSpec {
                        resource: ResourceId::from(FOOD),
                        capacity: cap,
                        mass_per_unit_kg: 0.5,
                    },
                    TankSpec {
                        resource: ResourceId::from(EC),
                        capacity: cap,
                        mass_per_unit_kg: 0.0,
                    },
                ],
                crew_seats: 2,
                structural: csp_data::StructuralDef::unlimited(),
                mass_kg: 10_000.0,
                com: csp_core::DVec3::ZERO,
                inertia: csp_phys::SymMat3::solid_box(
                    10_000.0,
                    csp_core::DVec3::new(1.0, 1.0, 1.0),
                ),
                collider: csp_phys::ColliderShape::Box {
                    half_extents: csp_core::DVec3::new(1.0, 1.0, 1.0),
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
            part_costs: vec![1_000.0],
            reliability: vec![],
        }
    }

    /// A crewed, tanked vessel straight into the arena (the
    /// colonies.rs test pattern — direct insertion for the
    /// command-free world).
    fn crewed_vessel(w: &mut World, cap: f64, seats: u32) -> VesselId {
        let mut craft = crew_craft(cap);
        let id = VesselId(w.next_vessel_id);
        w.next_vessel_id += 1;
        let mut crew = Vec::new();
        for _ in 0..seats {
            let cid = crate::transfers::CrewId(w.next_crew_id);
            w.next_crew_id += 1;
            crew.push(crate::transfers::VesselCrew {
                id: cid,
                seat: PartNodeId(0),
            });
        }
        // `spawn_contents` mutates the craft (the build-material mass
        // strip — none of these tanks qualify, but the stored record
        // must still be the post-spawn craft, not a pre-strip clone).
        let tank_contents = crate::transfers::spawn_contents(&mut craft);
        let thermal = VesselThermal::at_ambient(&craft.graph, 288.0);
        w.vessels.insert(
            id,
            Vessel {
                id,
                craft: Some(craft),
                state: VesselState::SurfaceLatched(crate::SurfacePose {
                    body: BodyId::from("terra"),
                    latitude_rad: 0.0,
                    longitude_rad: 0.0,
                    altitude_m: 0.0,
                    orientation: csp_core::DQuat::IDENTITY,
                }),
                aero_mask: csp_phys::ConfigMask::ALL,
                thermal,
                debris_since: None,
                maneuver: None,
                seams: Vec::new(),
                tank_contents,
                crew,
                science: crate::VesselScience::default(),
                joined_colony: None,
                reliability: BTreeMap::new(),
                control_lease: Lease::Player,
                deployed: BTreeMap::new(),
            },
        );
        id
    }

    /// §2.6.1 regression: a lazily published `LifeSupportCheck` that
    /// lands INSIDE the interval must fire identically under every
    /// partition. The coarse run publishes and fires the check
    /// mid-interval (`event.tick = 100 < t_next`); the fine run fires
    /// it at `clock == event.tick`. The depletion transition's
    /// `event_tick + 1` schedule is hashed either way.
    #[test]
    fn mid_interval_check_fires_identically_across_partitions() {
        let build = || {
            let mut w = World::new();
            w.set_life_support_config(test_cfg()); // 100-tick grid
            w.set_life_support_enabled(true);
            let vid = crewed_vessel(&mut w, 10.0, 1);
            // No oxygen aboard — the check shorts and schedules the
            // depletion transition.
            w.vessels
                .get_mut(&vid)
                .unwrap()
                .tank_contents
                .get_mut(&PartNodeId(0))
                .unwrap()
                .insert(ResourceId::from(OXYGEN), 0.0);
            w
        };
        let mut coarse = build();
        let mut fine = build();
        // One 300-tick frame: the publish lands `LifeSupportCheck` at
        // 100 inside `(0, 300]`; the event batch fires it mid-interval,
        // and its `event_tick + 1` depletion cascades in-batch.
        coarse.step_frame(300).unwrap();
        // Fine partitions: the check fires at its own tick, the
        // depletion one tick later, then the run steps on.
        for t in [100, 101, 200, 300] {
            fine.step_frame(t).unwrap();
        }
        assert_eq!(coarse.clock().ticks(), 300);
        assert_eq!(fine.clock().ticks(), 300);
        // Non-vacuous: the depletion really fired in both runs.
        assert!(coarse.vessels[&VesselId(0)].crew.is_empty());
        assert!(fine.vessels[&VesselId(0)].crew.is_empty());
        assert_eq!(
            coarse.state_hash().unwrap(),
            fine.state_hash().unwrap(),
            "mid-interval event firing must be partition-independent"
        );
    }

    /// LS check: a crewed, stocked vessel debits rate·period·crew per
    /// resource on the absolute grid — and the cadence re-arms.
    #[test]
    fn check_debits_on_the_grid() {
        let mut w = World::new();
        let cfg = test_cfg();
        w.set_life_support_config(cfg);
        w.set_life_support_enabled(true); // opt-in: tests enable it
        let vid = crewed_vessel(&mut w, 1000.0, 2);
        // First publish lands on the grid.
        w.step_frame(1).unwrap();
        let check = w
            .event_heap
            .iter()
            .find(|e| matches!(e.kind, EventKind::LifeSupportCheck))
            .cloned()
            .expect("a crewed vessel publishes the check");
        let period = cfg.check_period_ticks;
        assert_eq!(check.tick, period);
        let before: BTreeMap<_, _> = [
            ResourceId::from(OXYGEN),
            ResourceId::from(FOOD),
            ResourceId::from(EC),
        ]
        .iter()
        .map(|r| (r.clone(), vessel_resource(&w.vessels[&vid], r)))
        .collect();
        w.step_frame(period).unwrap();
        let period_s = period as f64 / TICK_HZ as f64;
        for (r, rate) in cfg.demands() {
            let want = before[&r] - rate * period_s * 2.0;
            let got = vessel_resource(&w.vessels[&vid], &r);
            assert!((got - want).abs() < 1e-9, "{r:?}: got {got}, want {want}");
        }
        // The cadence re-armed on the grid.
        let check2 = w
            .event_heap
            .iter()
            .find(|e| matches!(e.kind, EventKind::LifeSupportCheck))
            .unwrap();
        assert_eq!(check2.tick, 2 * period);
    }

    /// Depletion: a crewed vessel that cannot cover the debit earns
    /// the `Clamping` transition, whose firing marks the crew Lost.
    #[test]
    fn depletion_kills_the_crew() {
        let mut w = World::new();
        w.set_life_support_config(test_cfg());
        w.set_life_support_enabled(true);
        // Stocked tanks but no oxygen aboard — the check shorts it.
        let vid = crewed_vessel(&mut w, 10.0, 0);
        // `tank_contents` is per-tank (`PartNodeId` → contents) —
        // empty the node's oxygen store so the check shorts it.
        w.vessels
            .get_mut(&vid)
            .unwrap()
            .tank_contents
            .get_mut(&PartNodeId(0))
            .unwrap()
            .insert(ResourceId::from(OXYGEN), 0.0);
        w.vessels
            .get_mut(&vid)
            .unwrap()
            .crew
            .push(crate::transfers::VesselCrew {
                id: crate::transfers::CrewId(0),
                seat: PartNodeId(0),
            });
        // Hire a roster member and sit them aboard — the Lost status
        // needs a roster record (id-only BoardCrew members just
        // vanish).
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::HireCrew {
                name: "pilot".into(),
                kind: crate::economy::CrewKind::Pilot,
                skill: 1,
                salary_per_s: 0.0,
            },
        );
        w.step_frame(1).unwrap();
        let roster_id = crate::transfers::CrewId(0);
        w.economy.crew.get_mut(&roster_id).unwrap().location = CrewLocation::Vessel(vid);
        let period = w.life_support_config().check_period_ticks;
        let r = w.step_frame(period + 1).unwrap();
        assert!(r.fired_events.iter().any(
            |e| matches!(e.kind, EventKind::VesselLifeSupportDepleted { vessel } if vessel == vid)
        ));
        assert!(w.vessels[&vid].crew.is_empty());
        let m = &w.economy.crew[&roster_id];
        assert_eq!(m.status, CrewStatus::Lost);
        assert_eq!(m.location, CrewLocation::Unassigned);
    }

    /// The toggle: disabled → no check publishes, no debit, no
    /// depletion; re-enable → the cadence returns (and the flag
    /// itself flips through the logged command).
    #[test]
    fn toggle_disables_and_reenables() {
        let mut w = World::new();
        w.set_life_support_config(test_cfg());
        // Enabled at build so the queued `false` is a real transition.
        w.set_life_support_enabled(true);
        let vid = crewed_vessel(&mut w, 10.0, 1);
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SetLifeSupportEnabled { enabled: false },
        );
        w.step_frame(1).unwrap();
        let period = w.life_support_config().check_period_ticks;
        let before = vessel_resource(&w.vessels[&vid], &ResourceId::from(OXYGEN));
        w.step_frame(3 * period).unwrap();
        assert_eq!(
            vessel_resource(&w.vessels[&vid], &ResourceId::from(OXYGEN)),
            before
        );
        assert!(
            !w.event_heap
                .iter()
                .any(|e| matches!(e.kind, EventKind::LifeSupportCheck))
        );
        assert!(w.vessels[&vid].crew.len() == 1);
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SetLifeSupportEnabled { enabled: true },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(
            w.event_heap
                .iter()
                .any(|e| matches!(e.kind, EventKind::LifeSupportCheck))
        );
    }

    /// Serialization: the flag and config ride the snapshot — a
    /// restored world keeps consuming on the same grid.
    #[test]
    fn snapshot_carries_the_flag_and_config() {
        let mut w = World::new();
        w.set_life_support_config(test_cfg());
        w.set_life_support_enabled(true); // the carried flag under test
        w.step_frame(1).unwrap();
        let snap = w.snapshot().unwrap();
        assert!(snap.life_support_enabled);
        assert_eq!(snap.life_support_cfg, test_cfg());
        let r = World::restore(
            crate::orbit::PatchedConics::new(BTreeMap::new()),
            None,
            snap,
        )
        .unwrap();
        assert!(r.life_support_enabled());
        assert_eq!(r.state_hash().unwrap(), w.state_hash().unwrap());
    }
}
