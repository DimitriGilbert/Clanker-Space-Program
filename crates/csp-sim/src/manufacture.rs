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
//! `SimCommand::LaunchCraft { site, craft, build_resources }` is the
//! VAB's launch verb — the same gates and the same atomic spend, but
//! the craft berths `SurfaceLatched` inside the apply itself: no
//! `ActiveBuild`, no `ConstructionComplete`. A claimed berth is a
//! `Recoverable` rejection, not a retry (a launch has no record to
//! requeue — the launch path's pad-clearing `RecoverVessel` drains
//! ahead of the command). The reservation shape stays for
//! colony/agent/inventory manufacture — csp-bench orders, the MCP
//! `sim.command` passthrough, and foreign `ManufactureCraft`s.
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
use csp_data::{ResourceId, SiteDef, SiteId};
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
    /// The `ConstructionComplete`/`ConstructionRetry` deadline.
    pub complete_tick: u64,
    /// The aggregated BOM the apply actually spent (C11-F3) — the
    /// `CancelBuild` refund returns exactly this, so it rides the
    /// record (the declared `(resource, units)` pairs the order
    /// verified and aggregated, deduplicated like the spend).
    /// `#[serde(default)]` is the §7.7 field-addition convention:
    /// pre-field stamped-3 bodies still refuse at `decode_body`'s
    /// byte-exact re-encode gate rather than silently defaulting.
    #[serde(default)]
    pub build_resources: BTreeMap<ResourceId, f64>,
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
            h.btree_map(
                &b.build_resources,
                |h, r| {
                    h.str(r.as_str());
                },
                |h, q| {
                    h.f64(*q);
                },
            );
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

/// The craft's live-graph mass — `WeldPart::mass_kg` over nodes
/// only. The resolver's figure is `mass_dry + Σ capacity·mpu` over
/// EVERY tank slot (`resolve_built_part`/`propellant_mass`), so the
/// order's mass clock runs on the authored wet-full number: tanks
/// deliver propellant-full and the §3.5 cargo slots `spawn_contents`
/// strips at delivery still paid build time on their wet mass — a
/// slightly conservative ETA, accepted.
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

/// The order's build duration in ticks — `ceil(mass_kg / rate ·
/// TICK_HZ)` over the live graph (min 1), the same arithmetic the
/// apply's `complete_tick` lands on. `pub` so the launch toast's ETA
/// reads the order's deadline arithmetic instead of re-summing part
/// masses (C11-F7 — two mass bases drifted).
pub fn build_ticks(craft: &crate::CraftDef, rate_kg_per_s: f64) -> u64 {
    ticks_for(craft_mass_kg(craft), rate_kg_per_s)
}

/// The stack's belly depth — the lowest collider vertex along the
/// local up axis (+Z, the VAB's build direction), floored at 0 so an
/// origin-below-parts craft can't push its own origin under the
/// datum. The ONE rule the delivery berth, its occupancy probe, and
/// the launch pre-flight share (C12-F1): `altitude_m − belly` rests
/// the craft ON the site's datum.
pub fn collider_belly(welded: &csp_phys::weld::WeldedBody) -> f64 {
    welded
        .collider
        .hulls
        .iter()
        .flat_map(|h| h.vertices.iter().map(|v| v.z))
        .fold(f64::INFINITY, f64::min)
        .min(0.0)
}

/// The berth pose the pad delivers a craft at — upright on its belly
/// at the site, the `launch_craft` spawn convention (C12-F1). Shared
/// so the occupancy probe and the launch path pose their candidates
/// exactly as the delivery materializes them: a probe posed any
/// other way tests a volume the berth never occupies.
pub fn berth_pose(def: &SiteDef, belly: f64) -> crate::SurfacePose {
    crate::SurfacePose {
        body: def.body.clone(),
        latitude_rad: def.latitude_rad,
        longitude_rad: def.longitude_rad,
        altitude_m: def.altitude_m - belly,
        orientation: crate::bubble::surface_upright(def.latitude_rad, def.longitude_rad),
        afloat: false,
    }
}

impl crate::World {
    /// The live build-order arena — observation only (the `economy()`
    /// precedent): the UI's "build order in progress" rows and tests
    /// read it; mutation is the apply/event pair's job.
    pub fn builds(&self) -> &BTreeMap<BuildId, ActiveBuild> {
        &self.builds
    }

    /// The pure-check half of `ManufactureCraft` — every gate the
    /// order must clear BEFORE it spends: site validity, the craft's
    /// shape/coherence tables, the §3.3 part + facility gates, the
    /// weld, the declared BOM's shape and catalog envelope, funds,
    /// the vessel-cap slot, and site-inventory affordability.
    /// Returns the aggregated BOM the apply then debits atomically.
    ///
    /// `pub` for the launch pre-flight (csp-view mirrors the whole
    /// gate set so a launch that would reject at the drain refuses
    /// at the button with the real reason instead of stranding the
    /// player in an empty flight scene). Read-only — the apply
    /// re-runs it at drain time regardless, so a caller-side race
    /// can only ever refuse early, never admit a bad order.
    #[allow(clippy::too_many_lines)]
    pub fn check_manufacture_craft(
        &self,
        site: &SiteId,
        craft: &crate::CraftDef,
        build_resources: &[(ResourceId, f64)],
    ) -> Result<BTreeMap<ResourceId, f64>, SimError> {
        if !self.economy_cfg.is_valid() {
            // Input-reachable like the five transfer sites (a config
            // a caller installed pre-check or a corrupt record): a
            // `Recoverable` rejection, never a `Fatal`.
            return Err(SimError::Recoverable("manufacture: invalid economy config"));
        }
        let Some(def) = self.site_defs.get(site) else {
            return Err(SimError::Recoverable("manufacture: unknown site"));
        };
        if !def.launch {
            return Err(SimError::Recoverable(
                "manufacture: build orders consume a launch-site inventory",
            ));
        }
        if !self.economy.sites.contains_key(site) {
            return Err(SimError::Recoverable(
                "manufacture: site has no state arena row",
            ));
        }
        // Shape checks — the same gates `apply_spawn_physical` runs:
        // the weld must succeed at delivery, so it must be sound at
        // order time.
        crate::transfers::check_tank_masses(craft)?;
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
        // The embodied-BOM table — empty ("no bill recorded", the
        // recovery credit is then zero) or covering `parts` exactly
        // with finite non-negative rows — the same parallel-table
        // convention as `part_costs`/`reliability`/`part_ids`.
        if !craft.part_build_resources.is_empty()
            && (craft.part_build_resources.len() != craft.parts.len()
                || craft
                    .part_build_resources
                    .iter()
                    .flatten()
                    .any(|(_, u)| !(u.is_finite() && *u >= 0.0)))
        {
            return Err(SimError::Recoverable(
                "manufacture: incoherent build-resource table",
            ));
        }
        // M7-B §3.3: the same availability gate the spawn apply runs —
        // an un-researched part cannot be ordered, not just launched.
        self.check_part_gate(craft)?;
        // C08-F6/C33-F3 §3.3: the build order is assembled in the VAB
        // and rolled to the pad — both authored facility unlocks gate
        // the verb (a treeless world runs ungated, the
        // `check_part_gate` precedent; shipped trees author both at
        // tech.start).
        if !self.facility_unlocked(&csp_data::FacilityId::from("facility.vab"))
            || !self.facility_unlocked(&csp_data::FacilityId::from("facility.launchpad"))
        {
            return Err(SimError::Recoverable(
                "manufacture: facility not unlocked (vab/launchpad)",
            ));
        }
        csp_phys::weld::weld(&craft.graph, &craft.parts)
            .map_err(|_| SimError::Recoverable("manufacture: craft fails the weld gate"))?;
        let mut bom: BTreeMap<ResourceId, f64> = BTreeMap::new();
        for (r, q) in build_resources {
            if !(q.is_finite() && *q > 0.0) {
                return Err(SimError::Recoverable(
                    "manufacture: non-positive or non-finite build resource",
                ));
            }
            *bom.entry(r.clone()).or_insert(0.0) += q;
        }
        // C11-F4: the catalog is authoritative for what a craft costs —
        // a caller-declared bill must sit inside its resolution
        // envelope (and an anonymous craft in a catalog world is the
        // free-build bypass `check_part_gate`'s empty-`part_ids`
        // convention leaves open — closed here, at the money door only;
        // the spawn paths keep their sandbox craft rule).
        self.verify_bill(craft, &bom)?;
        // The mint-hole gate: when the craft carries a per-part bill,
        // it must sum to the declared `build_resources` the order
        // spends — recovery credits the per-part table back into a
        // site inventory, so a table that disagrees with the spent
        // bill is debit-small/recover-big forgery. The producer sums
        // both from the same `EffectivePart::build_resources` rows in
        // the same order, so equality is bitwise, not approximate.
        if !craft.part_build_resources.is_empty() {
            let mut per_part: BTreeMap<ResourceId, f64> = BTreeMap::new();
            for rows in &craft.part_build_resources {
                for (r, q) in rows {
                    *per_part.entry(r.clone()).or_insert(0.0) += q;
                }
            }
            if per_part != bom {
                return Err(SimError::Recoverable(
                    "manufacture: per-part build resources do not sum to the declared bill",
                ));
            }
        }
        let cost = crate::economy::craft_cost_micros(craft);
        if cost > self.economy.funds_micros {
            return Err(SimError::Recoverable("manufacture: insufficient funds"));
        }
        let mass = craft_mass_kg(craft);
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
                .get(site)
                .and_then(|st| st.inventory.get(r))
                .copied()
                .unwrap_or(0.0);
            if have < *q {
                return Err(SimError::Recoverable(
                    "manufacture: insufficient site inventory",
                ));
            }
        }
        Ok(bom)
    }

    /// The atomic spend both manufacture verbs share — debit every
    /// aggregated BOM row out of the ordering site's inventory and
    /// charge the craft's funds through the ledger. Runs only AFTER
    /// `check_manufacture_craft` proved every row affordable, so a
    /// missing row here is `Fatal`, never `Recoverable` (the §2.6.4
    /// rollback law: shadow before write).
    fn spend_build_bill(
        &mut self,
        site: &SiteId,
        bom: &BTreeMap<ResourceId, f64>,
        cost: u64,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        self.shadow_economy(shadow);
        for (r, q) in bom {
            let st = self
                .economy
                .sites
                .get_mut(site)
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
        Ok(())
    }

    /// `ManufactureCraft` — the reservation: validate site, craft,
    /// BOM, funds, and the vessel-cap slot, then spend everything and
    /// schedule the delivery. Nothing about the craft exists until
    /// `ConstructionComplete`.
    pub(crate) fn apply_manufacture_craft(
        &mut self,
        site: SiteId,
        craft: crate::CraftDef,
        build_resources: Vec<(ResourceId, f64)>,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let bom = self.check_manufacture_craft(&site, &craft, &build_resources)?;
        // Commit — the §2.6.4 rollback law: shadow before write.
        let cost = crate::economy::craft_cost_micros(&craft);
        let mass = craft_mass_kg(&craft);
        self.spend_build_bill(&site, &bom, cost, shadow)?;
        self.shadow_builds(shadow);
        let id = BuildId(self.next_build_id);
        self.next_build_id = self
            .next_build_id
            .checked_add(1)
            .ok_or(SimError::Fatal("build id overflow"))?;
        let now = self.clock.ticks();
        // A huge authored mass saturates `ticks_for` — input-reachable
        // like the non-finite check above, so the overflow is a
        // `Recoverable` rejection, not a `Fatal`.
        let complete_tick = now
            .checked_add(ticks_for(mass, self.economy_cfg.manufacture_rate_kg_per_s))
            .ok_or(SimError::Recoverable(
                "manufacture: build duration overflows the tick horizon",
            ))?;
        self.builds.insert(
            id,
            ActiveBuild {
                id,
                site,
                craft,
                complete_tick,
                build_resources: bom,
            },
        );
        self.schedule_event(
            EventKind::ConstructionComplete { build: id },
            complete_tick,
            shadow,
        )
    }

    /// `LaunchCraft` — the VAB's atomic order-and-berth: every
    /// `check_manufacture_craft` gate, the same BOM + funds spend,
    /// then the delivery tail's `SurfaceLatched` write — all inside
    /// this one apply. No `ActiveBuild`, no `ConstructionComplete`:
    /// the player's rocket is on the pad the moment the command
    /// drains. A claimed berth is a `Recoverable` rejection rather
    /// than the build order's `ConstructionRetry` — a launch has no
    /// record to requeue, and the launch path's pad-clearing
    /// `RecoverVessel` drains ahead of this command anyway, so a
    /// block here can only be a non-recoverable claimant or a race
    /// the pre-flight could not see.
    pub(crate) fn apply_launch_craft(
        &mut self,
        site: SiteId,
        craft: crate::CraftDef,
        build_resources: Vec<(ResourceId, f64)>,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let bom = self.check_manufacture_craft(&site, &craft, &build_resources)?;
        // The berth check BEFORE the spend — the atomicity rule the
        // order's affordability loop already sets: a refused launch
        // debits nothing.
        if self.berth_blocked(&site, &craft) {
            return Err(SimError::Recoverable("launch: pad occupied"));
        }
        let cost = crate::economy::craft_cost_micros(&craft);
        self.spend_build_bill(&site, &bom, cost, shadow)?;
        self.insert_berthed_vessel(&site, craft, shadow)?;
        Ok(())
    }

    /// `CancelBuild` — the C11-F3 exit a mis-order needed: refund the
    /// order's funds (`craft_cost_micros` — the same figure the apply
    /// charged) and its spent BOM back into the ordering site's
    /// inventory, remove the record, and scrub its pending
    /// `ConstructionComplete`/`ConstructionRetry` (the strict
    /// build↔event pairing makes the scrub mandatory — the
    /// `apply_unload_script`/`retire_script_wake` convention).
    pub(crate) fn apply_cancel_build(
        &mut self,
        build: BuildId,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let Some(b) = self.builds.get(&build) else {
            return Err(SimError::Recoverable("manufacture: unknown build order"));
        };
        let site = b.site.clone();
        let bom = b.build_resources.clone();
        let refund = crate::economy::craft_cost_micros(&b.craft);
        // §2.6.4's rollback law: shadow before the write. One economy
        // capture covers the stock credit and the funds credit.
        self.shadow_economy(shadow);
        if let Some(st) = self.economy.sites.get_mut(&site) {
            for (r, q) in &bom {
                *st.inventory.entry(r.clone()).or_insert(0.0) += q;
            }
        }
        // A site row that vanished mid-build can't take the stock
        // back — the delivery path's own drop rule, mirrored.
        crate::economy::credit(
            &mut self.economy,
            self.clock.ticks(),
            refund,
            FundsReason::BuildRefund,
        );
        self.shadow_builds(shadow);
        self.builds.remove(&build);
        self.scrub_event_heap(shadow, |e| {
            !matches!(&e.kind,
                EventKind::ConstructionComplete { build: b }
                | EventKind::ConstructionRetry { build: b }
                    if *b == build)
        });
        Ok(())
    }

    /// `ConstructionComplete`/`ConstructionRetry` — delivery: the
    /// craft becomes a surface-latched vessel at its site. TOTAL
    /// like every event firing (a mid-air failure would orphan the
    /// record under the snapshot pairing): a vanished record or site
    /// is a no-op, the slot was reserved at order time. `retried`
    /// marks a berth-blocked re-fire — it picks the re-queue kind
    /// and gates the once-per-episode `BuildBerthBlocked` notice
    /// (C21-F2).
    pub(crate) fn fire_construction_complete(
        &mut self,
        build: BuildId,
        shadow: &mut TickShadow,
        retried: bool,
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
        self.deliver_built_vessel(b, shadow, retried)
    }

    /// The berth check both delivery paths share: weld the craft,
    /// pose it where the site would berth it ([`berth_pose`] —
    /// upright, belly on the datum, the same `SurfaceLatched` record
    /// the vessel lands with and the launch path's own convention; an
    /// identity/flat probe would test a volume the delivery never
    /// materializes, C12-F1), and run the same `spawn_site_occupied`
    /// predicate the launch pre-flight and `SpawnPhysicalVessel`
    /// apply share — a parked or in-bubble hull claiming the site
    /// defers a build delivery, refuses a `LaunchCraft`. A weld
    /// failure or a world without physics cannot block: the order's
    /// weld gate already ran at apply, and a physics-less world has
    /// no contact model to overlap in.
    fn berth_blocked(&self, site: &SiteId, craft: &crate::CraftDef) -> bool {
        let (Some(def), Some(physics)) = (self.site_defs.get(site), self.physics.as_ref()) else {
            return false;
        };
        let Ok(mut candidate) = csp_phys::weld::weld(&craft.graph, &craft.parts) else {
            return false;
        };
        let pose = berth_pose(def, collider_belly(&candidate));
        let (pos, rot, _, _) = crate::bubble::surface_pose_restore(
            physics.backend.environment(),
            &pose,
            candidate.com_local,
            self.clock,
        );
        candidate.pos = pos;
        candidate.rot = rot;
        self.spawn_site_occupied(&candidate)
    }

    /// The build order's berth check for [`Self::deliver_built_vessel`]
    /// — [`Self::berth_blocked`] on the order's site and craft.
    fn delivery_blocked(&self, b: &ActiveBuild) -> bool {
        self.berth_blocked(&b.site, &b.craft)
    }

    /// The shared delivery tail — insert the latched vessel record.
    ///
    /// W2.6's berth rule: the pad is a single berth. When a vessel
    /// claims the delivery site at the deadline (parked since the
    /// order, or a crash-landed hull), the delivery waits — the
    /// record re-queues with a fresh `ConstructionRetry` one
    /// sim-second out rather than materializing inside the parked
    /// craft (coincident hulls ghost-overlap forever, the spawn
    /// occupancy gate's reason) or evaporating a paid-for build.
    /// C21-F2: the retry rides a distinct `Silent` kind — the
    /// re-queue used to re-fire `ConstructionComplete` every
    /// sim-second, so each pop re-toasted "CONSTRUCTION COMPLETE"
    /// and re-pinned `eta_s` at ~1 s. The first deferral logs one
    /// `BuildBerthBlocked`; the eventual delivery logs
    /// `BuildDelivered`; between them the chain is quiet.
    fn deliver_built_vessel(
        &mut self,
        b: ActiveBuild,
        shadow: &mut TickShadow,
        retried: bool,
    ) -> Result<(), SimError> {
        // Test-only fault injection: the delivery path is
        // deliberately TOTAL in production (every failure mode above
        // is `Fatal` or a tolerated no-op), so `fire_due_events`'s
        // `Recoverable` arm — and its zombie-drop for the
        // construction pair — is unreachable from a test without a
        // stand-in failure. One-shot: `replace(false)` consumes the
        // flag so a berth-retry chain fails only the armed firing.
        #[cfg(test)]
        if TEST_FAIL_DELIVERY.with(|f| f.replace(false)) {
            return Err(SimError::Recoverable("test: injected delivery failure"));
        }
        if self.delivery_blocked(&b) {
            let retry_tick = self.clock.ticks().saturating_add(TICK_HZ);
            if !retried {
                // The episode's one announcement — a retry firing
                // knows the block was already logged, so the chain
                // re-queues silently from here.
                self.notify(
                    crate::reliability::Notification {
                        tick: self.clock.ticks(),
                        vessel: crate::reliability::PROGRAM_VESSEL,
                        node: None,
                        kind: crate::reliability::NotificationKind::BuildBerthBlocked {
                            build: b.id.0,
                        },
                    },
                    shadow,
                );
            }
            self.builds.insert(
                b.id,
                ActiveBuild {
                    complete_tick: retry_tick,
                    ..b
                },
            );
            return self.schedule_event(
                EventKind::ConstructionRetry { build: b.id },
                retry_tick,
                shadow,
            );
        }
        self.insert_berthed_vessel(&b.site, b.craft, shadow)?;
        if retried {
            // A retry-delivered build never fired `ConstructionComplete`
            // — the log line closes the episode the `BuildBerthBlocked`
            // entry opened (C21-F2).
            self.notify(
                crate::reliability::Notification {
                    tick: self.clock.ticks(),
                    vessel: crate::reliability::PROGRAM_VESSEL,
                    node: None,
                    kind: crate::reliability::NotificationKind::BuildDelivered { build: b.id.0 },
                },
                shadow,
            );
        }
        Ok(())
    }

    /// The berth write both delivery paths share — materialize
    /// `craft` as a `SurfaceLatched` vessel at `site` and restamp its
    /// reliability residence. `deliver_built_vessel` runs it at the
    /// order's deadline; `apply_launch_craft` runs it inside the
    /// command's own apply. Callers gate the berth first (the retry
    /// re-queue / the `LaunchCraft` rejection) — this write assumes
    /// the volume is clear.
    fn insert_berthed_vessel(
        &mut self,
        site: &SiteId,
        mut craft: crate::CraftDef,
        shadow: &mut TickShadow,
    ) -> Result<VesselId, SimError> {
        self.shadow_vessels(shadow);
        let id = VesselId(self.next_vessel_id);
        self.next_vessel_id = self
            .next_vessel_id
            .checked_add(1)
            .ok_or(SimError::Fatal("vessel id counter overflow"))?;
        let def = self
            .site_defs
            .get(site)
            .cloned()
            .ok_or(SimError::Fatal("manufacture: site vanished mid-delivery"))?;
        let ambient = self
            .physics
            .as_ref()
            .map(|p| csp_phys::ambient_k(p.backend.environment(), def.altitude_m));
        // The spawn-fill write is part of delivery — propellant tanks
        // full, §3.5 build-material/cargo tanks EMPTY with their
        // authored wet mass handed back (the manufacture→recover
        // faucet `spawn_contents`'s doc names). `craft` is the
        // delivered record, so the strip lands on the vessel itself.
        let tank_contents = crate::transfers::spawn_contents(&mut craft);
        let thermal = ambient.map_or_else(crate::VesselThermal::default, |a| {
            crate::VesselThermal::at_ambient(&craft.graph, a)
        });
        // M9b §3.4: construction IS part creation — the delivered
        // craft's per-node records draw their `θ₀` here, so a revert
        // to the post-delivery snapshot preserves the pre-drawn
        // thresholds (the allocator rides `shadow_vessels`).
        let reliability = crate::reliability::allocate_records(
            self.save_seed,
            &craft,
            self.clock.ticks(),
            &mut self.next_part_instance_id,
        )?;
        // W2.6: the delivery berths the craft the way the pad spawn
        // did — [`berth_pose`]'s upright-on-its-belly convention, not
        // the identity pose buried `z_min` deep in the pad (the latch
        // record restores verbatim on unlatch; an embedded or tipped
        // delivery reads as a crash). The occupancy probe reads the
        // identical pose, so a hull the probe cleared is the volume
        // this record materializes (C12-F1).
        let pose = csp_phys::weld::weld(&craft.graph, &craft.parts)
            .ok()
            .map(|w| berth_pose(&def, collider_belly(&w)))
            .unwrap_or_else(|| berth_pose(&def, 0.0));
        self.vessels.insert(
            id,
            crate::Vessel {
                id,
                craft: Some(craft),
                state: crate::VesselState::SurfaceLatched(pose),
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
                deployed: BTreeMap::new(),
            },
        );
        // M9b: a freshly delivered vessel sits dormant — restamp the
        // records under the dormant rate now (the publish slot's
        // next pass derives the pending crossings from this stamp).
        self.reliability_residence_change(id, self.clock.ticks(), true, shadow)?;
        Ok(id)
    }

    /// §3.5's server-side bill check (C11-F4). The command declares
    /// the resolved bill (`part_costs`, the aggregated `bom`), but
    /// the catalog is authoritative — under the old positivity-only
    /// gate an external caller could order a weldable hull with an
    /// empty `part_ids`/BOM and pay nothing. What the command does
    /// NOT carry is the `BuiltPart` (scale, stretch, variant, frozen
    /// upgrade levels), so the exact resolved figure is not
    /// re-derivable here — what IS provable is the resolution
    /// ENVELOPE: every authored variant × every legal `(scale,
    /// stretch)` corner × every purchasable upgrade composition
    /// produces a per-part cost range and a per-resource BOM range,
    /// and a declared value outside it is forged. A world with no
    /// installed catalog has no authoritative bill to check against —
    /// the sandbox convention `check_part_gate` keeps.
    fn verify_bill(
        &self,
        craft: &crate::CraftDef,
        bom: &BTreeMap<ResourceId, f64>,
    ) -> Result<(), SimError> {
        if self.parts.is_empty() {
            return Ok(());
        }
        if craft.part_ids.len() != craft.parts.len() {
            return Err(SimError::Recoverable(
                "manufacture: every part needs a catalog identity to bill against",
            ));
        }
        let mut bom_lo: BTreeMap<ResourceId, f64> = BTreeMap::new();
        let mut bom_hi: BTreeMap<ResourceId, f64> = BTreeMap::new();
        for node in craft.graph.nodes.values() {
            let idx = node.effective_index as usize;
            let Some(def) = craft.part_ids.get(idx).and_then(|id| self.parts.get(id)) else {
                continue;
            };
            let (cost_lo, cost_hi) = self.part_cost_envelope(def)?;
            let declared = craft.part_costs.get(idx).copied().unwrap_or(0.0);
            // The envelope's corners are evaluated with the resolver's
            // own libm arithmetic, but `pow` is not bitwise-monotone —
            // the hair of slack keeps a legitimate interior point from
            // failing on rounding at the bound.
            if !(declared >= cost_lo * (1.0 - BILL_EPS) && declared <= cost_hi * (1.0 + BILL_EPS)) {
                return Err(SimError::Recoverable(
                    "manufacture: declared part cost outside the catalog envelope",
                ));
            }
            for (r, u) in &def.build_resources {
                // The BOM is mass-like (`resolve_stats`: `u · s^mass
                // · t^max(mass−2,0)`) — variant and upgrade multipliers
                // never touch it, only the scale corners do.
                let (lo, hi) = scale_bom_bounds(&def.scale, *u);
                *bom_lo.entry(r.clone()).or_insert(0.0) += lo;
                *bom_hi.entry(r.clone()).or_insert(0.0) += hi;
            }
        }
        for (r, lo) in &bom_lo {
            let declared = bom.get(r).copied().unwrap_or(0.0);
            let hi = bom_hi[r];
            if !(declared >= *lo * (1.0 - BILL_EPS) && declared <= hi * (1.0 + BILL_EPS)) {
                return Err(SimError::Recoverable(
                    "manufacture: declared bill of materials outside the catalog envelope",
                ));
            }
        }
        for r in bom.keys() {
            if !bom_lo.contains_key(r) {
                return Err(SimError::Recoverable(
                    "manufacture: bill names a resource no part authors",
                ));
            }
        }
        Ok(())
    }

    /// One def's resolved-cost envelope — `cost_funds · f.cost ·
    /// variant.cost_multiplier · (variant × upgrade CostFunds mults)`
    /// over every authored variant and the scale rule's legal corners
    /// (the stat factors are monotone per axis, so the corners carry
    /// the extrema), with the upgrade leg ranged over every
    /// purchasable level composition — the command freezes neither
    /// variant nor levels, so both walk the whole authored space.
    /// `Recoverable` when no variant/scale combination can resolve at
    /// all — a def that cannot legitimately produce the part.
    fn part_cost_envelope(&self, def: &csp_data::PartDef) -> Result<(f64, f64), SimError> {
        if def.variants.is_empty() {
            return Err(SimError::Recoverable(
                "manufacture: part has no authored variant to bill against",
            ));
        }
        let (u_lo, u_hi) = self.upgrade_mult_envelope(def, csp_data::StatPath::CostFunds)?;
        let mut lo = f64::INFINITY;
        let mut hi = 0.0f64;
        for v in &def.variants {
            let v_cost = v
                .stat_multipliers
                .iter()
                .find(|(p, _)| *p == csp_data::StatPath::CostFunds)
                .map_or(1.0, |(_, m)| *m);
            let base = def.cost_funds * v.cost_multiplier * v_cost;
            for f_cost in scale_corners(&def.scale, |e| e.map(|e| e.cost)) {
                lo = lo.min(base * f_cost * u_lo);
                hi = hi.max(base * f_cost * u_hi);
            }
        }
        if !(lo.is_finite() && hi.is_finite() && lo <= hi) {
            return Err(SimError::Recoverable(
                "manufacture: part has no legal scale to bill against",
            ));
        }
        Ok((lo, hi))
    }

    /// The composed upgrade multiplier's purchasable range for
    /// `path` on `def`'s family — `compose_upgrade_levels`'s fold
    /// evaluated at every level prefix (level 0 contributes 1.0, so
    /// 1 is always inside). `(1, 1)` when the world has no tree —
    /// nothing is purchasable, so no upgrade leg exists.
    fn upgrade_mult_envelope(
        &self,
        def: &csp_data::PartDef,
        path: csp_data::StatPath,
    ) -> Result<(f64, f64), SimError> {
        let Some(tree) = &self.tech_tree else {
            return Ok((1.0, 1.0));
        };
        let (mut lo, mut hi) = (1.0f64, 1.0f64);
        for track in tree.tracks_for_family(&def.family) {
            if track.stat_path != path {
                continue;
            }
            // The resolver's reachability gate: a track naming a stat
            // this def lacks can never compose a nonzero level on it,
            // so it contributes only its level-0 factor.
            let Some(base) = track.stat_path.base_value(def) else {
                continue;
            };
            let (mut t_lo, mut t_hi) = (1.0f64, 1.0f64);
            let (mut mult, mut delta) = (1.0f64, 0.0f64);
            for level in &track.levels {
                match level.multiplier_or_delta {
                    csp_data::UpgradeEffect::Multiplier(m) => mult *= m,
                    csp_data::UpgradeEffect::Delta(d) => delta += d,
                }
                let m = if delta != 0.0 {
                    mult * (base + delta) / base
                } else {
                    mult
                };
                // The compose gate admits only finite positive
                // products — a level that can't compose admits no
                // declared figure either.
                if m.is_finite() && m > 0.0 {
                    t_lo = t_lo.min(m);
                    t_hi = t_hi.max(m);
                }
            }
            lo *= t_lo;
            hi *= t_hi;
        }
        Ok((lo, hi))
    }
}

/// ±1e-9 — the bill envelope's slack band (see `verify_bill`).
const BILL_EPS: f64 = 1.0e-9;

// Test-only fault injection for `World::deliver_built_vessel` — see
// the comment at the use site. `cfg(test)` keeps it out of every
// shipping build like the stand-in failure it produces; a
// `thread_local` (not a shared atomic) so parallel test threads each
// arm their own copy — a global flag would let any concurrent
// delivery consume the arm mid-test.
#[cfg(test)]
std::thread_local! {
    pub(crate) static TEST_FAIL_DELIVERY: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// The scale rule's stat-factor corners — `s^e · t^max(e−2,0)` at
/// every legal `(scale, stretch)` extreme. Stat factors are monotone
/// per axis (the stretch exponent is `max(e−2, 0) ≥ 0`), so the
/// corner box bounds every interior point — `resolve_stats`'s own
/// `pow2`/`stretch_exp` arithmetic, evaluated only where extrema can
/// live. `None`/`Discrete` rules scale geometry only: stat factors
/// are identically 1.
fn scale_corners(
    rule: &csp_data::ScaleRule,
    exp: impl Fn(Option<&csp_data::ScaleExponents>) -> Option<f64>,
) -> Vec<f64> {
    let corners: &[(f64, f64)] = match rule {
        csp_data::ScaleRule::None => &[(1.0, 1.0)],
        // Discrete menus move geometry only — every stat factor is
        // `pow2(1.0)` regardless of the picked size.
        csp_data::ScaleRule::Discrete(_) => &[(1.0, 1.0)],
        csp_data::ScaleRule::Continuous { min, max, .. } => {
            &[(*min, *min), (*min, *max), (*max, *min), (*max, *max)]
        }
    };
    let e = match rule {
        csp_data::ScaleRule::Continuous { exponents, .. } => exp(Some(exponents)),
        _ => None,
    };
    corners
        .iter()
        .map(|&(s, t)| {
            let (se, te) = (e.unwrap_or(0.0), e.map_or(0.0, |e| (e - 2.0).max(0.0)));
            if e.is_none() {
                1.0
            } else {
                libm::pow(s, se) * libm::pow(t, te)
            }
        })
        .collect()
}

/// One BOM line's `(lo, hi)` over the scale corners — `u · f.mass`,
/// the resolver's own factor (see `verify_bill`).
fn scale_bom_bounds(rule: &csp_data::ScaleRule, units: f64) -> (f64, f64) {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for f in scale_corners(rule, |e| e.map(|e| e.mass)) {
        lo = lo.min(units * f);
        hi = hi.max(units * f);
    }
    (lo, hi)
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
                starting_stock: vec![],
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
            part_build_resources: Vec::new(),
            hash_memo: Default::default(),
            part_ids: vec![],
            graph,
            parts: vec![csp_phys::WeldPart {
                experiments: Vec::new(),
                lab: None,
                engine: None,
                parachute: None,
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
        // `ore` is a §3.5 build material — the cargo tank delivers
        // EMPTY (the manufacture→recover faucet fix): nothing aboard,
        // `free = capacity`, and the 10 kg of authored wet mass went
        // back with the contents (100 kg authored − 10 u · 1 kg/u).
        let ore = ResourceId::from("ore");
        assert_eq!(crate::transfers::vessel_resource(v, &ore), 0.0);
        assert_eq!(
            crate::transfers::vessel_capacity(v, &ore) - crate::transfers::vessel_resource(v, &ore),
            10.0,
            "a delivered cargo tank must offer its full capacity"
        );
        let part = &v.craft.as_ref().unwrap().parts[0];
        assert!(
            (part.mass_kg - 90.0).abs() < 1e-9,
            "cargo wet mass strips at delivery: {}",
            part.mass_kg
        );
    }

    /// `LaunchCraft` — the VAB's verb: the build order's reservation
    /// arithmetic with NO build wait. One drain runs every
    /// `check_manufacture_craft` gate, spends the aggregated BOM out
    /// of the site's inventory plus the funds cost, and berths the
    /// craft `SurfaceLatched` — no `ActiveBuild`, no
    /// `ConstructionComplete`.
    #[test]
    fn launch_craft_spends_and_berths_atomically() {
        let mut w = world();
        let funds0 = w.economy().funds_micros;
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::LaunchCraft {
                site: SiteId::from("pad"),
                craft: craft(),
                build_resources: vec![(ResourceId::from("ore"), 20.0)],
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        // The same atomic spend the order path debits: 20 ore + 10
        // funds — and the craft is already berthed, no build record.
        let site = &w.economy().sites[&SiteId::from("pad")];
        assert!((site.inventory[&ResourceId::from("ore")] - 80.0).abs() < 1e-9);
        assert_eq!(funds0 - w.economy().funds_micros, 10_000_000);
        assert!(
            w.builds.is_empty(),
            "a launch mints no build order: {:?}",
            w.builds.keys().collect::<Vec<_>>()
        );
        assert_eq!(w.vessels().len(), 1, "the launch berthed in one drain");
        let v = w.vessels().values().next().unwrap();
        assert!(matches!(v.state, crate::VesselState::SurfaceLatched(_)));
        // The delivery tail is verbatim the build's: cargo tanks
        // deliver EMPTY (the manufacture→recover faucet) with the
        // authored wet mass stripped.
        let ore = ResourceId::from("ore");
        assert_eq!(crate::transfers::vessel_resource(v, &ore), 0.0);
        let part = &v.craft.as_ref().unwrap().parts[0];
        assert!(
            (part.mass_kg - 90.0).abs() < 1e-9,
            "cargo wet mass strips at berth: {}",
            part.mass_kg
        );
    }

    /// A claimed berth refuses the launch `Recoverable` — a launch
    /// has no `ActiveBuild` to requeue the way `ConstructionRetry`
    /// re-fires a deferred delivery, so the atomicity rule applies:
    /// the refused command debits nothing.
    #[test]
    fn launch_craft_rejects_on_a_claimed_berth() {
        let mut w = physics_world();
        let ore = ResourceId::from("ore");
        let funds0 = w.economy().funds_micros;
        // A parked hull holds the pad's single berth.
        w.vessels.insert(VesselId(0), parked_vessel(&w, craft()));
        w.next_vessel_id = 1;
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::LaunchCraft {
                site: SiteId::from("pad"),
                craft: craft(),
                build_resources: vec![(ore.clone(), 20.0)],
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert_eq!(r.rejected.len(), 1, "{:?}", r.rejected);
        assert_eq!(r.rejected[0].kind, csp_core::RejectKind::LaunchCraft);
        assert!(matches!(r.rejected[0].error, SimError::Recoverable(_)));
        // Nothing spent, nothing berthed, no record left behind.
        assert_eq!(
            w.economy().sites[&SiteId::from("pad")].inventory[&ore],
            100.0
        );
        assert_eq!(w.economy().funds_micros, funds0);
        assert_eq!(w.vessels().len(), 1, "only the occupant remains");
        assert!(w.builds.is_empty());
    }

    /// `LaunchCraft` runs the `check_manufacture_craft` gate set
    /// verbatim — the order's refusals are the launch's refusals:
    /// insufficient inventory, insufficient funds, an unknown site,
    /// a non-launch site, and the insolvency-dormant facility gate.
    #[test]
    fn launch_craft_runs_the_same_gates() {
        let mut w = world();
        let ore = ResourceId::from("ore");
        let launch = |w: &mut World, site: &str, bom: Vec<(ResourceId, f64)>| {
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::LaunchCraft {
                    site: SiteId::from(site),
                    craft: craft(),
                    build_resources: bom,
                },
            );
            w.step_frame(w.clock().ticks() + 1).unwrap()
        };
        // Over-stockpile BOM — the affordability gate.
        let r = launch(&mut w, "pad", vec![(ore.clone(), 200.0)]);
        assert_eq!(r.rejected.len(), 1);
        assert!(matches!(r.rejected[0].error, SimError::Recoverable(_)));
        // Unknown site.
        let r = launch(&mut w, "no-such-site", vec![(ore.clone(), 1.0)]);
        assert_eq!(r.rejected.len(), 1);
        assert_eq!(r.rejected[0].kind, csp_core::RejectKind::LaunchCraft);
        // Non-launch site — clone the pad minus the flag (keep the
        // pad in the def set: `set_sites` retires unlisted rows).
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
                starting_stock: vec![],
            },
        );
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
                starting_stock: vec![],
            },
        );
        w.set_sites(sites);
        w.economy.sites.insert(
            SiteId::from("field"),
            crate::economy::SiteState {
                id: SiteId::from("field"),
                inventory: BTreeMap::from([(ore.clone(), 100.0)]),
            },
        );
        let r = launch(&mut w, "field", vec![(ore.clone(), 1.0)]);
        assert_eq!(r.rejected.len(), 1, "a non-launch site must refuse");
        // Insolvency: the facility-dormant gate names the launch the
        // same as the order.
        w.economy.insolvent = true;
        let r = launch(&mut w, "pad", vec![(ore.clone(), 1.0)]);
        assert_eq!(r.rejected.len(), 1);
        assert!(
            matches!(&r.rejected[0].error, SimError::Recoverable(m) if m.contains("insolvent")),
            "{:?}",
            r.rejected
        );
        w.economy.insolvent = false;
        // And the honest launch still applies after the refusals —
        // the stockpile proves nothing debited along the way.
        assert_eq!(
            w.economy().sites[&SiteId::from("pad")].inventory[&ore],
            100.0
        );
        let r = launch(&mut w, "pad", vec![(ore.clone(), 20.0)]);
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        assert_eq!(w.vessels().len(), 1);
        assert!(w.builds.is_empty());
    }

    /// The faucet regression, end to end (fix-plan 3.1): build a bare
    /// ore tank, recover it at the launch site — the cycle must move
    /// ONLY the paid BOM out of the site inventory. Pre-fix the
    /// delivered tank arrived full and `RecoverVessel` credited the
    /// contents back at efficiency 1.0: −20 ore + 10 ore netted the
    /// build 10 ore (and `metal`/`composite` slots would mint the
    /// build currencies outright). Post-fix the recovery credits zero
    /// contents — the inventory stays at its post-BOM level.
    #[test]
    fn manufactured_cargo_tank_recovers_empty() {
        let mut w = world();
        let ore = ResourceId::from("ore");
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::ManufactureCraft {
                site: SiteId::from("pad"),
                craft: craft(),
                build_resources: vec![(ore.clone(), 20.0)],
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        // Deliver (100 kg at 50 kg/s → 128 ticks) and recover at the
        // pad it landed on — `launch: true`, so contents would credit
        // at 1.0 if there were any.
        w.step_frame(w.clock().ticks() + 128).unwrap();
        let vessel = *w.vessels().keys().next().unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::RecoverVessel {
                vessel,
                site: Some(SiteId::from("pad")),
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        // A rejected recovery would leave the same 80 — prove the
        // vessel is actually gone before crediting the assertion.
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        assert!(!w.vessels().contains_key(&vessel));
        let inv = &w.economy().sites[&SiteId::from("pad")].inventory;
        assert_eq!(
            inv[&ore], 80.0,
            "build−recover cycle must net only the BOM spend ({inv:?})"
        );
    }

    /// The embodied-BOM return leg (the stockpile soft-lock): a craft
    /// whose `part_build_resources` records the bill the order spent
    /// carries it through delivery, and `RecoverVessel` credits it
    /// back into the site's inventory at the recovery efficiency —
    /// here the pad's 1.0, so the build→deliver→recover cycle is
    /// stockpile-neutral. Without the return leg every cycle
    /// permanently sank the BOM (the ~11-cycle "insufficient metal"
    /// wall the drain report found).
    #[test]
    fn embodied_bom_rides_delivery_and_refunds_at_recovery() {
        let mut w = world();
        let ore = ResourceId::from("ore");
        let mut c = craft();
        c.part_build_resources = vec![vec![(ore.clone(), 20.0)]];
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::ManufactureCraft {
                site: SiteId::from("pad"),
                craft: c,
                build_resources: vec![(ore.clone(), 20.0)],
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        w.step_frame(w.clock().ticks() + 128).unwrap();
        let vessel = *w.vessels().keys().next().unwrap();
        // The bill survives delivery on the craft record.
        assert_eq!(
            w.vessels()[&vessel]
                .craft
                .as_ref()
                .unwrap()
                .part_build_resources,
            vec![vec![(ore.clone(), 20.0)]]
        );
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::RecoverVessel {
                vessel,
                site: Some(SiteId::from("pad")),
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        let inv = &w.economy().sites[&SiteId::from("pad")].inventory;
        assert_eq!(
            inv[&ore], 100.0,
            "pad recovery refunds the whole embodied BOM: {inv:?}"
        );
    }

    /// The mint-hole gate: a per-part bill that does not sum to the
    /// declared `build_resources` refuses — the order spends the
    /// declared figure but recovery would credit the bigger table.
    /// A non-parallel table rejects the same way (the
    /// `reliability`-gate convention).
    #[test]
    fn manufacture_rejects_a_bom_table_that_disagrees_with_the_bill() {
        let mut w = world();
        let ore = ResourceId::from("ore");
        let mut c = craft();
        // Table claims 25, the declared bill spends 20 — forgery.
        c.part_build_resources = vec![vec![(ore.clone(), 25.0)]];
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::ManufactureCraft {
                site: SiteId::from("pad"),
                craft: c,
                build_resources: vec![(ore.clone(), 20.0)],
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert_eq!(r.rejected.len(), 1);
        assert!(matches!(r.rejected[0].error, SimError::Recoverable(_)));
        assert_eq!(
            w.economy().sites[&SiteId::from("pad")].inventory[&ore],
            100.0,
            "a rejected order spends nothing"
        );
        let mut c2 = craft();
        c2.part_build_resources = vec![vec![(ore.clone(), 20.0)], Vec::new()];
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::ManufactureCraft {
                site: SiteId::from("pad"),
                craft: c2,
                build_resources: vec![(ore.clone(), 20.0)],
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert_eq!(r.rejected.len(), 1, "non-parallel table must reject");
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
                starting_stock: vec![],
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
                starting_stock: vec![],
            },
        )]));
        r.step_frame(300).unwrap();
        a.step_frame(300).unwrap();
        assert_eq!(a.state_hash().unwrap(), r.state_hash().unwrap());
        assert_eq!(r.vessels().len(), 1);
    }

    /// C11-F3: `CancelBuild` is the order's exit — the funds spend
    /// and every stock unit return exactly, the record and its
    /// pending `ConstructionComplete` die together (the strict
    /// build↔event pairing), and a second cancel rejects unknown-id.
    #[test]
    fn cancel_build_refunds_the_full_spend() {
        let mut w = world();
        let ore = ResourceId::from("ore");
        let funds0 = w.economy().funds_micros;
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::ManufactureCraft {
                site: SiteId::from("pad"),
                craft: craft(),
                build_resources: vec![(ore.clone(), 20.0)],
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        let build = *w.builds.keys().next().unwrap();
        assert!(
            w.pending_events().iter().any(
                |e| matches!(e.kind, EventKind::ConstructionComplete { build: b } if b == build)
            ),
            "the order's completion event is pending"
        );
        w.enqueue(SourceClass::Ui, 0, SimCommand::CancelBuild { build });
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        assert!(w.builds.is_empty(), "the record is gone");
        assert!(
            !w.pending_events().iter().any(
                |e| matches!(&e.kind, EventKind::ConstructionComplete { build: b }
                    | EventKind::ConstructionRetry { build: b } if *b == build)
            ),
            "the paired event was scrubbed"
        );
        assert_eq!(
            w.economy().sites[&SiteId::from("pad")].inventory[&ore],
            100.0,
            "every BOM unit returned"
        );
        assert_eq!(
            w.economy().funds_micros,
            funds0,
            "the funds spend came back to the micros"
        );
        assert!(
            w.economy()
                .ledger
                .iter()
                .any(|e| matches!(e.reason, FundsReason::BuildRefund)),
            "the refund rode its own ledger reason"
        );
        // A second cancel names no live record — Recoverable, never
        // a silent no-op.
        w.enqueue(SourceClass::Ui, 0, SimCommand::CancelBuild { build });
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(!r.rejected.is_empty());
    }

    /// A launch-site world holding a physics bubble plus a parked
    /// hull — the berth-blocked delivery needs both (`delivery_blocked`
    /// is vacuous without the contact model).
    fn physics_world() -> World {
        let mut w = World::with_physics(
            test_system(),
            crate::PhysicsConfig {
                environment: csp_phys::Environment {
                    body_center: csp_core::DVec3::ZERO,
                    radius_m: 600_000.0,
                    mu_m3_per_s2: 4.0e14,
                    body_spin: csp_core::DVec3::ZERO,
                    atmosphere: None,
                    settle_v: 0.05,
                    settle_w: 0.05,
                    settle_ticks: 128,
                    friction_mu: 0.5,
                    drag_cd: 1.0,
                    convective_heat_fraction: csp_phys::DEFAULT_CONVECTIVE_HEAT_FRACTION,
                    terrain: None,
                    ocean: None,
                },
                dominant: BodyId::from("terra"),
                bubble_radius_m: crate::DEFAULT_BUBBLE_RADIUS_M,
            },
            crate::DEFAULT_MAX_EVENTS_PER_FRAME,
        );
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
                starting_stock: vec![],
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

    /// A vessel record parked at the delivery berth — the pose the
    /// delivery would take, so `spawn_site_occupied` reads the claim.
    fn parked_vessel(w: &World, craft: crate::CraftDef) -> crate::Vessel {
        let def = &w.site_defs[&SiteId::from("pad")];
        let belly = csp_phys::weld::weld(&craft.graph, &craft.parts)
            .map(|wb| collider_belly(&wb))
            .unwrap_or(0.0);
        crate::Vessel {
            id: VesselId(0),
            craft: Some(craft),
            state: crate::VesselState::SurfaceLatched(berth_pose(def, belly)),
            aero_mask: csp_phys::ConfigMask::ALL,
            thermal: crate::VesselThermal::default(),
            debris_since: None,
            maneuver: None,
            seams: Vec::new(),
            tank_contents: BTreeMap::new(),
            crew: Vec::new(),
            science: crate::VesselScience::default(),
            joined_colony: None,
            reliability: BTreeMap::new(),
            control_lease: crate::Lease::Player,
            deployed: BTreeMap::new(),
        }
    }

    /// C12-F1 + C21-F2: a claimed berth defers the delivery — the
    /// deadline fires once as `ConstructionComplete`, then the order
    /// re-queues on the `Silent` `ConstructionRetry` kind (one
    /// `BuildBerthBlocked` log entry, not a per-second toast loop).
    /// Clearing the berth lands the craft and logs `BuildDelivered`.
    #[test]
    fn blocked_berth_retries_silently_then_delivers() {
        let mut w = physics_world();
        let ore = ResourceId::from("ore");
        // The parked hull claims the pad before the order completes.
        w.vessels.insert(VesselId(0), parked_vessel(&w, craft()));
        w.next_vessel_id = 1;
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::ManufactureCraft {
                site: SiteId::from("pad"),
                craft: craft(),
                build_resources: vec![(ore.clone(), 20.0)],
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        let build = *w.builds.keys().next().unwrap();
        // Past the deadline: the completion fired once, then retries.
        let mut complete_fires = 0u32;
        let mut retry_fires = 0u32;
        for _ in 0..4 {
            let r = w.step_frame(w.clock().ticks() + TICK_HZ).unwrap();
            for e in &r.fired_events {
                match &e.kind {
                    EventKind::ConstructionComplete { build: b } if *b == build => {
                        complete_fires += 1
                    }
                    EventKind::ConstructionRetry { build: b } if *b == build => retry_fires += 1,
                    _ => {}
                }
            }
        }
        assert_eq!(complete_fires, 1, "the deadline fires exactly once");
        assert!(
            retry_fires >= 2,
            "the Silent retry re-queues: {retry_fires}"
        );
        assert_eq!(w.vessels().len(), 1, "the berth still blocks delivery");
        assert_eq!(w.builds.len(), 1, "the paid order waits, not drops");
        let blocked_notes = w
            .notifications()
            .iter()
            .filter(|n| matches!(
                n.kind,
                crate::reliability::NotificationKind::BuildBerthBlocked { build: b } if b == build.0
            ))
            .count();
        assert_eq!(blocked_notes, 1, "one notice per blockage episode");
        // The berth clears — the pending retry lands the craft.
        w.vessels.remove(&VesselId(0));
        let r = w.step_frame(w.clock().ticks() + TICK_HZ).unwrap();
        assert!(
            r.fired_events.iter().any(|e| matches!(&e.kind,
                EventKind::ConstructionRetry { build: b } if *b == build)),
            "delivery lands through the retry kind"
        );
        assert_eq!(w.vessels().len(), 1);
        assert!(w.builds.is_empty());
        assert!(w.notifications().iter().any(|n| matches!(
            n.kind,
            crate::reliability::NotificationKind::BuildDelivered { build: b } if b == build.0
        )));
    }

    /// C11-F4: with a catalog installed the bill is verified — the
    /// empty-`part_ids` free build and an understated declared cost or
    /// BOM all reject, while the honest order applies.
    #[test]
    fn catalog_bill_verification_refuses_forged_orders() {
        use csp_data::*;
        let mut w = world();
        let d = PartDef {
            schema_version: PART_SCHEMA_VERSION,
            id: PartId::from("test.tank"),
            family: FamilyId::from("fam.t"),
            display_name: "n".into(),
            category: PartCategory::Utility,
            mass_dry_kg: 90.0,
            cost_funds: 10.0,
            size_m: csp_core::DVec3::new(1.0, 1.0, 1.0),
            build_resources: vec![(ResourceId::from("ore"), 20.0)],
            attach_nodes: vec![],
            scale: ScaleRule::None,
            collider: ColliderDef::Box {
                half_extents_m: csp_core::DVec3::new(0.5, 0.5, 0.5),
            },
            aero: AeroDef::constant(0.0, 0.0, 1.0),
            thermal: ThermalDef {
                heat_capacity_j_per_k: 1.0,
                max_temp_k: 1_000.0,
                emissivity: 0.8,
                conduction_w_per_k: 0.0,
            },
            structural: StructuralDef::unlimited(),
            variants: vec![PartVariant {
                id: VariantId::from("stock"),
                display_name: "v".into(),
                cost_multiplier: 1.0,
                stat_multipliers: vec![],
            }],
            modules: vec![],
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
                failure_modes: vec![],
            },
            tech_requirement: TechNodeId::from("tech.root"),
        };
        w.set_parts(BTreeMap::from([(d.id.clone(), d)]));
        let ore = ResourceId::from("ore");
        let mut order = |craft, bom: Vec<(ResourceId, f64)>| {
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::ManufactureCraft {
                    site: SiteId::from("pad"),
                    craft,
                    build_resources: bom,
                },
            );
            w.step_frame(w.clock().ticks() + 1).unwrap()
        };
        // The bypass: an anonymous craft in a catalog world.
        let r = order(craft(), vec![(ore.clone(), 20.0)]);
        assert!(!r.rejected.is_empty(), "anonymous part_ids must refuse");
        // Catalog-identified but forged bills.
        let mut honest = craft();
        honest.part_ids = vec![PartId::from("test.tank")];
        let mut cheap = honest.clone();
        cheap.part_costs = vec![1.0];
        let r = order(cheap, vec![(ore.clone(), 20.0)]);
        assert!(!r.rejected.is_empty(), "declared cost below envelope");
        let r = order(honest.clone(), vec![(ore.clone(), 1.0)]);
        assert!(!r.rejected.is_empty(), "declared BOM below envelope");
        let r = order(honest.clone(), vec![]);
        assert!(!r.rejected.is_empty(), "the empty BOM must refuse");
        let r = order(honest.clone(), vec![(ResourceId::from("unobtanium"), 5.0)]);
        assert!(!r.rejected.is_empty(), "unauthored resource must refuse");
        // The honest bill (def cost + authored BOM) applies.
        let r = order(honest, vec![(ore.clone(), 20.0)]);
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        assert_eq!(w.builds.len(), 1);
    }

    /// C08-F6/C33-F3's facility gates — `unlocks.facilities` on
    /// researched nodes now gates the facility verbs: the test
    /// stand refuses while `facility.test_stand` is unresearched
    /// and opens on the research, and the build order itself checks
    /// `facility.vab` + `facility.launchpad`. A treeless world runs
    /// ungated — the `check_part_gate` precedent.
    #[test]
    fn facility_unlocks_gate_bench_and_manufacture() {
        let mut w = world();
        // The stand consumes `reliability_bench_resource` ("metal")
        // — stock it so the gate is the only thing standing.
        w.economy
            .sites
            .get_mut(&SiteId::from("pad"))
            .unwrap()
            .inventory
            .insert(ResourceId::from("metal"), 10.0);
        w.set_tech_tree(csp_data::TechTree {
            nodes: vec![csp_data::TechNode {
                id: csp_data::TechNodeId::from("tech.n"),
                display_name: "n".into(),
                description: "d".into(),
                science_cost: 0.0,
                requires: vec![],
                unlocks: csp_data::TechUnlocks {
                    track_levels: vec![],
                    facilities: vec![
                        csp_data::FacilityId::from("facility.test_stand"),
                        csp_data::FacilityId::from("facility.vab"),
                        csp_data::FacilityId::from("facility.launchpad"),
                    ],
                    contract_types: vec![],
                },
            }],
            tracks: vec![],
        });
        let bench = |w: &mut World| {
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::RunBenchTest {
                    site: SiteId::from("pad"),
                    family: csp_data::FamilyId::from("f"),
                    units: 1.0,
                },
            );
            w.step_frame(w.clock().ticks() + 1).unwrap()
        };
        let build = |w: &mut World| {
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::ManufactureCraft {
                    site: SiteId::from("pad"),
                    craft: craft(),
                    build_resources: vec![],
                },
            );
            w.step_frame(w.clock().ticks() + 1).unwrap()
        };
        // Unresearched — both verbs refuse on the facility gate.
        let r = bench(&mut w);
        assert!(
            r.rejected
                .iter()
                .any(|x| matches!(&x.error, SimError::Recoverable(m) if m.contains("test stand"))),
            "{:?}",
            r.rejected
        );
        let r = build(&mut w);
        assert!(
            r.rejected.iter().any(
                |x| matches!(&x.error, SimError::Recoverable(m) if m.contains("vab/launchpad"))
            ),
            "{:?}",
            r.rejected
        );
        // Researched — the bench reserves; the build's BOM is empty
        // here so it rejects LATER, on the bill, not the gate.
        w.tech
            .researched
            .insert(csp_data::TechNodeId::from("tech.n"));
        let r = bench(&mut w);
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        let r = build(&mut w);
        assert!(
            !r.rejected.iter().any(
                |x| matches!(&x.error, SimError::Recoverable(m) if m.contains("vab/launchpad"))
            ),
            "the facility gate must not fire post-research: {:?}",
            r.rejected
        );
        // And the treeless world's ungated bench applies outright.
        let mut w = world();
        w.economy
            .sites
            .get_mut(&SiteId::from("pad"))
            .unwrap()
            .inventory
            .insert(ResourceId::from("metal"), 10.0);
        let r = bench(&mut w);
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
    }

    /// The zombie-drop: a `Recoverable`-failed construction firing
    /// consumes the popped event, and the per-event shadow rolls the
    /// record back into `builds()` — under §3.7's strict
    /// build↔event pairing that unpaired record is a zombie: nothing
    /// reschedules it, `builds()` stays non-empty (suppressing the
    /// view's dead-scene bounce — the stranded player), and the next
    /// save trips `restore_build_state`'s pairing gate. The firing's
    /// failure arm drops the record with the event, the
    /// `ManeuverNodeStart`/`TransferComplete`/contract arms'
    /// convention.
    ///
    /// The delivery path is deliberately TOTAL in production (see
    /// `fire_construction_complete`'s doc), so the arm is reachable
    /// only through `TEST_FAIL_DELIVERY`'s stand-in failure.
    #[test]
    fn failed_construction_delivery_drops_the_build() {
        let mut w = world();
        let ore = ResourceId::from("ore");
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::ManufactureCraft {
                site: SiteId::from("pad"),
                craft: craft(),
                build_resources: vec![(ore.clone(), 20.0)],
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        let build = *w.builds.keys().next().unwrap();

        // Arm the one-shot failure, then step past the deadline —
        // 100 kg at 50 kg/s is 128 ticks (the delivery test above).
        TEST_FAIL_DELIVERY.with(|f| f.set(true));
        let r = w.step_frame(w.clock().ticks() + 200).unwrap();
        assert!(
            r.degraded
                .iter()
                .any(|e| matches!(e, SimError::Recoverable(_))),
            "the injected failure must ride the degraded channel: {:?}",
            r.degraded
        );
        assert!(
            w.builds.is_empty(),
            "a failed firing consumes the record with its event: {:?}",
            w.builds.keys().collect::<Vec<_>>()
        );
        assert!(
            !w.pending_events().iter().any(|e| matches!(&e.kind,
                EventKind::ConstructionComplete { build: b }
                | EventKind::ConstructionRetry { build: b }
                    if *b == build)),
            "no construction event may survive for the dropped record"
        );
        assert!(w.vessels().is_empty(), "a failed delivery lands no vessel");
        // And the world is not wedged: the clock keeps advancing and
        // the pairing gate has no zombie to fault on the next save.
        w.step_frame(w.clock().ticks() + 64).unwrap();
        w.snapshot().expect("a paired arena snapshots cleanly");
    }

    /// The retry arm of the same drop: a berth-blocked delivery
    /// re-queues on `ConstructionRetry`, and a `Recoverable` failure
    /// on the RE-fire consumes the record identically — the pairing
    /// covers both kinds.
    #[test]
    fn failed_construction_retry_drops_the_build() {
        let mut w = physics_world();
        let ore = ResourceId::from("ore");
        w.vessels.insert(VesselId(0), parked_vessel(&w, craft()));
        w.next_vessel_id = 1;
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::ManufactureCraft {
                site: SiteId::from("pad"),
                craft: craft(),
                build_resources: vec![(ore.clone(), 20.0)],
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        let build = *w.builds.keys().next().unwrap();
        // Step to the deadline: the occupied berth re-queues the
        // order onto `ConstructionRetry` one sim-second out.
        w.step_frame(w.clock().ticks() + 200).unwrap();
        assert_eq!(w.builds.len(), 1, "the berth-blocked order waits");
        assert!(
            w.pending_events().iter().any(
                |e| matches!(&e.kind, EventKind::ConstructionRetry { build: b } if *b == build)
            ),
            "a retry event must be pending"
        );
        TEST_FAIL_DELIVERY.with(|f| f.set(true));
        let r = w.step_frame(w.clock().ticks() + TICK_HZ + 8).unwrap();
        assert!(
            r.degraded
                .iter()
                .any(|e| matches!(e, SimError::Recoverable(_))),
            "the retry's injected failure must ride degraded: {:?}",
            r.degraded
        );
        assert!(
            w.builds.is_empty(),
            "the retry arm drops the record with its event too"
        );
        assert!(
            !w.pending_events().iter().any(|e| matches!(&e.kind,
                EventKind::ConstructionComplete { build: b }
                | EventKind::ConstructionRetry { build: b }
                    if *b == build)),
            "no construction event may survive for the dropped record"
        );
    }
}
