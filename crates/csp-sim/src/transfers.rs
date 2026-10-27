//! M6-D: the §3.5 `ResourceTransfer` **process** and the crew
//! transfer — the last two items of M6's "multi-vessel & docking"
//! row (`PLAN.md`: *"`ResourceTransfer` process; crew transfer"*).
//!
//! §3.5's spec, verbatim: *"`ResourceTransfer` is a sim process, not
//! an instant action: precondition = docked **or** both vessels
//! surface-latched within `transfer_radius_m` (default 100 m) of each
//! other; rate = `min(source_outflow, dest_inflow)` from the
//! connected ports; publishes `TransferComplete` (so it survives
//! warp). Instantaneous transfer would be invisible to the event
//! queue."*
//!
//! # The link precondition ("docked or in range")
//!
//! After M6-C's latch merge, two *docked* vessels are **one** vessel —
//! the absorbed craft's tanks and seats are pooled into the merged
//! body automatically (KSP-style crossfeed falls out of the merge,
//! no transfer needed). A `from == to` command therefore rejects as
//! a no-op input error. What the precondition gates between *two
//! live* vessels, in [`World::transfer_linked`]'s order:
//!
//! 1. **Jointed**: a live §2.4 soft-capture joint connects the pair —
//!   the transient window where the two bodies are physically
//!   linked, before the latch collapses them into one.
//! 2. **In-bubble proximity**: both `InBubble` and their body origins
//!   within `transfer_radius_m` — the M8-row "proximity transfer
//!   works without orbital docking" mechanism, shared early because
//!   §6.4's cut rules it must survive independent of docking.
//! 3. **Surface-latched proximity**: both `SurfaceLatched` on the same
//!   body and within `transfer_radius_m` — §3.5's verbatim clause
//!   (the colony supply path; positions come out of the stored
//!   body-fixed pose record).
//!
//! The link is checked once, at command-apply time — the in-flight
//! stream is not re-gated (a hose already connected is not
//! disconnected by later drift; the debit has already happened).
//!
//! # The process model (pacing)
//!
//! `SimCommand::TransferResource` is *validated and debited* at its
//! drain boundary — the amount leaves the source tanks immediately
//! (reservation by construction: a second transfer cannot overdraw
//! the same contents) — and the destination's free capacity is
//! checked *net of other inbound in-flight transfers*. The record
//! ([`ActiveTransfer`]) then rides the world as hashed+serialized
//! state and a [`EventKind::TransferComplete`] event — `Clamping`,
//! so warp demotes for it, §3.5's "survives warp" — lands the
//! contents at `complete_tick`, credited into the destination's
//! tanks in ascending-node order.
//!
//! Mutation happens only at the command boundary and the event tick:
//! identical in every §2.6.1 partition — there is no per-interval
//! accumulation to last-ulp-drift.
//!
//! Completion is *best-effort against the live arena*: the
//! destination may have changed between apply and completion —
//! undocked (its tanks left with a fragment), broken up, or been
//! absorbed (see the latch rewrite below). The rule is total and
//! deterministic: credit the destination's tanks; whatever does not
//! fit returns to the source's tanks; whatever still does not fit
//! left the simulation with the tanks that would have held it.
//! `TransferCrew` runs the same machinery with a seat-slot check and
//! a fixed `crew_transfer_ticks` duration.
//!
//! # The contents and crew records
//!
//! [`Vessel::tank_contents`] is `BTreeMap<PartNodeId,
//! BTreeMap<ResourceId, f64>>` — units per (node, resource), keyed to
//! the graph so merge/split partition them with the parts that carry
//! them (the same discipline `VesselThermal` and `DockSeam`
//! follow). [`Vessel::crew`] is the seat-occupancy record: crew are
//! sim-side `CrewId`s (the M7 arena grows the names/kinds/salaries —
//! these ids persist), each bound to a `CrewSeat` part node. Crew
//! ride their seat part: a seat destroyed by jettison/breakup takes
//! its occupant with it (M7's `Lost` status replaces the removal).
//!
//! # Mass — the §2.4 quantized-inertia rule, applied
//!
//! `WeldPart::mass_kg` is *dry + quantized contents*: the maintained
//! invariant is `mass_kg = dry + Σ bucketed wet mass` per tanked
//! part. [`debit`]/[`credit`] apply it **at write time** — each
//! contents slot they move `old → new` shifts the part's `mass_kg`
//! by exactly `(bucket(new) − bucket(old))·capacity·mpu` (inertia
//! scaled by the mass ratio), so the tally is idempotent by
//! construction: there is no recovered dry mass to drift, a
//! bucket-straddling write moves mass once, and a second tally of
//! the same contents is a bitwise no-op. [`reweld_if_mass_changed`]
//! then re-welds an `InBubble` body when the write reports a moved
//! mass, preserving pose/velocity/control state — a pure function
//! of serialized state, no time trigger. `OnRails`/`SurfaceLatched`
//! vessels update the stored `WeldPart`s only; the next
//! materialization welds the new mass fresh. A side effect the
//! reader should know: `craft_graph_hash` covers `mass_kg`, so a
//! quantum flip invalidates the vessel's derived aero-table binding
//! and `ensure_aero` re-derives the (mass-independent, so identical)
//! table at the next range — conservative invalidation, accepted for
//! v1 over a second hash definition.
//!
//! # State class (§7.2/§7.4)
//!
//! `transfers`, `next_transfer_id`, `next_crew_id`, and
//! `transfer_cfg` are **hashed AND serialized** — an in-flight
//! transfer is forward-looking state like a scheduled node, and
//! §7.4 does not name it among the save-blocking conditions. Restore
//! enforces the transfer↔`TransferComplete` pairing like the
//! `Scheduled`-node↔`ManeuverNodeStart` coherence check.
//!
//! # v1 simplifications (documented)
//!
//! - **One global flow rate.** §3.5's `min(source_outflow,
//!   dest_inflow)` names per-port rates the data model does not carry
//!   yet; `transfer_rate_units_per_s` stands in for the pair until
//!   port flow stats land (M8's flow network).
//! - **Crew transfer duration is a fixed delay** (`crew_transfer_
//!   ticks`, default 10 s) rather than a traversed path — the plan
//!   names a rate for resources only.
//! - **Transfers are between two vessel ids.** Per-tank addressing
//!   (picking WHICH tanks to drain/fill first beyond the fixed
//!   ascending-node rule) is post-v1 UI work.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use csp_core::{DVec3, SimError, StableDiscriminant, StateHasher};
use csp_data::{BalanceDef, ResourceId};
use csp_phys::PartNodeId;

use crate::joints::LiveJoint;
use crate::{TickShadow, Vessel, VesselId, VesselState, World};

/// §2.2 sim-side transfer-process identity — monotonic `u64`, never
/// reused (`next_transfer_id` is serialized per §7.2). A transfer's id
/// is what its [`crate::EventKind::TransferComplete`] event names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TransferId(pub u64);

/// §2.2 sim-side crew identity — monotonic `u64`, never reused
/// (`next_crew_id` is serialized per §7.2). M7's named-individual
/// `CrewMember` arena grows on top of these ids; at M6 a crew member
/// is exactly its seat assignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CrewId(pub u64);

/// One crew member aboard a vessel — the seat assignment. `seat`
/// names the graph node whose [`csp_phys::WeldPart::crew_seats`]
/// capacity the member occupies; merge/split reparent it with the
/// part (see the [`crate::joints`] and `apply_breakup` bookkeeping).
/// A crew member whose seat node leaves the vessel leaves with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VesselCrew {
    /// The crew member's arena id.
    pub id: CrewId,
    /// The `CrewSeat`-carrying node they sit in.
    pub seat: PartNodeId,
}

/// What one in-flight transfer carries — the `TransferComplete`
/// event's dispatch payload. Discriminants are the §7.1 rule-7
/// encoding: explicit, stable, append-only.
#[repr(u16)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TransferCargo {
    /// `amount` units of `resource` — debited from the source's tanks
    /// at the command boundary, credited to the destination's at
    /// `complete_tick`.
    Resource {
        /// The resource moving.
        resource: ResourceId,
        /// Units in flight — exact, not quantum-adjusted (the quantum
        /// applies to *mass*, never to the bookkeeping).
        amount: f64,
    } = 0,
    /// Crew member `crew` — still aboard the source (occupying its
    /// seat) until `complete_tick`, then reseated on the destination.
    Crew {
        /// The crew member moving.
        crew: CrewId,
    } = 1,
}

impl StableDiscriminant for TransferCargo {
    fn discriminant(&self) -> u16 {
        match self {
            Self::Resource { .. } => 0,
            Self::Crew { .. } => 1,
        }
    }
}

/// One end of a transfer process (M8-B): `Vessel` covers the M6-D
/// vessel↔vessel link; `Colony` is the §3.5 vessel↔colony side —
/// reachable only for a vessel that has explicitly
/// `JoinColonyInventory`'d (the opt-in is never automatic).
/// Discriminants append-only, the `TransferCargo` rule.
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TransferEnd {
    /// A vessel arena id.
    Vessel(VesselId) = 0,
    /// A colony arena id — the colony's aggregate stores, not its
    /// site inventory.
    Colony(crate::economy::ColonyId) = 1,
}

impl StableDiscriminant for TransferEnd {
    fn discriminant(&self) -> u16 {
        match self {
            Self::Vessel(_) => 0,
            Self::Colony(_) => 1,
        }
    }
}

/// One in-flight transfer — the hashed+serialized process record.
/// Every live record pairs with exactly one pending
/// `TransferComplete` event at `complete_tick` (the restore coherence
/// check enforces both directions, like the maneuver node's).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActiveTransfer {
    /// Its arena key — kept inline per §2.2's arena-of-structs idiom.
    pub id: TransferId,
    /// The end the cargo left.
    pub from: TransferEnd,
    /// The end it lands at.
    pub to: TransferEnd,
    /// What it carries.
    pub cargo: TransferCargo,
    /// The tick its `TransferComplete` event fires at.
    pub complete_tick: u64,
}

/// The §3.5 transfer tunables — construction config (loaded from
/// `balance.ron` in real worlds; the `DEFAULT_*` mirrors otherwise),
/// **serialized and hashed** like `caps`/`docking`: every field
/// changes state evolution (the link precondition and the completion
/// scheduling), so an undocumented divergence here would red the
/// wrong tests.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TransferConfig {
    /// `transfer_radius_m` — the undocked link precondition (m).
    pub radius_m: f64,
    /// `transfer_rate_units_per_s` — the single flow rate standing in
    /// for §3.5's `min(source_outflow, dest_inflow)` (units/s). Must
    /// be `> 0` — a zero rate would schedule completion at eternity.
    pub rate_units_per_s: f64,
    /// `crew_transfer_ticks` — the crew-traversal delay (ticks).
    /// `0` is clamped to `1` at apply: a completion must sit strictly
    /// ahead of the clock.
    pub crew_transfer_ticks: u64,
}

/// `balance.ron: transfer_radius_m` mirror for data-less worlds (unit
/// tests, the wasm slice) — §3.5's default 100 m.
pub const DEFAULT_TRANSFER_RADIUS_M: f64 = 100.0;
/// `balance.ron: transfer_rate_units_per_s` mirror — 10 units/s.
pub const DEFAULT_TRANSFER_RATE_PER_S: f64 = 10.0;
/// `balance.ron: crew_transfer_ticks` mirror — 640 = 10 s at 64 Hz.
pub const DEFAULT_CREW_TRANSFER_TICKS: u64 = 640;

/// The `DEFAULT_*` config — `World::new()`'s no-data fallback.
pub fn default_config() -> TransferConfig {
    TransferConfig {
        radius_m: DEFAULT_TRANSFER_RADIUS_M,
        rate_units_per_s: DEFAULT_TRANSFER_RATE_PER_S,
        crew_transfer_ticks: DEFAULT_CREW_TRANSFER_TICKS,
    }
}

impl TransferConfig {
    /// The `balance.ron` → config conversion (§8.3 — the file is the
    /// tunable source of truth).
    pub fn from_balance(b: &BalanceDef) -> Self {
        Self {
            radius_m: b.transfer_radius_m,
            rate_units_per_s: b.transfer_rate_units_per_s,
            crew_transfer_ticks: b.crew_transfer_ticks,
        }
    }

    /// The snapshot-restore gate: corrupt config fails typed rather
    /// than silently diverging the run (a NaN radius would make every
    /// link check false — a silent transfer-off; a non-positive rate
    /// would schedule nonsense).
    pub fn is_valid(&self) -> bool {
        self.radius_m.is_finite()
            && self.radius_m >= 0.0
            && self.rate_units_per_s.is_finite()
            && self.rate_units_per_s > 0.0
    }
}

/// §2.4's contents quantum — the bucket a tank's contents count
/// toward for *mass* purposes: `floor(contents / capacity · 1024) /
/// 1024` of capacity, clamped to `[0, 1]` (a full tank buckets at
/// exactly 1.0; a `capacity ≤ 0` tank holds nothing). Bookkeeping
/// tracks exact units; only the inertia-cache rule sees the bucket —
/// that is the plan's "no time trigger" mechanism: the re-weld fires
/// on bucket boundary crossings, never on a schedule.
pub(crate) fn contents_bucket(contents: f64, capacity: f64) -> f64 {
    if capacity <= 0.0 || contents <= 0.0 {
        return 0.0;
    }
    ((contents / capacity * 1024.0).floor() / 1024.0).min(1.0)
}

/// The vessel's exact stored units of `resource` — `Σ` over every
/// tank node's contents slot. In-flight debits are already gone from
/// this sum: the number is "what is aboard now".
pub fn vessel_resource(vessel: &Vessel, resource: &ResourceId) -> f64 {
    vessel
        .tank_contents
        .values()
        .map(|stores| stores.get(resource).copied().unwrap_or(0.0))
        .sum()
}

/// The vessel's total capacity for `resource` — `Σ` over tank specs.
/// Reads the *declared* tanks (the craft), never the contents map, so
/// a fully-drained vessel still reports its capacity. Iterates
/// `graph.nodes → parts[effective_index]` like [`vessel_seats`]:
/// `craft.parts` is the *shared* parts table — a post-split vessel's
/// graph names only its own nodes, and reading the whole table would
/// count the departed fragment's tanks as phantom capacity.
pub fn vessel_capacity(vessel: &Vessel, resource: &ResourceId) -> f64 {
    let Some(craft) = &vessel.craft else {
        return 0.0;
    };
    craft
        .graph
        .nodes
        .values()
        .flat_map(|n| {
            craft
                .parts
                .get(n.effective_index as usize)
                .into_iter()
                .flat_map(|p| p.tanks.iter())
        })
        .filter(|t| t.resource == *resource)
        .map(|t| t.capacity)
        .sum()
}

/// The vessel's total crew slots — `Σ crew_seats` over the graph.
pub fn vessel_seats(vessel: &Vessel) -> u64 {
    let Some(craft) = &vessel.craft else {
        return 0;
    };
    craft
        .graph
        .nodes
        .values()
        .map(|n| {
            craft
                .parts
                .get(n.effective_index as usize)
                .map_or(0, |p| u64::from(p.crew_seats))
        })
        .sum()
}

/// The lowest-id graph node with a free `CrewSeat` slot — the fixed
/// deterministic seat-choice rule every crew write shares.
fn choose_seat(vessel: &Vessel) -> Option<PartNodeId> {
    let craft = vessel.craft.as_ref()?;
    let mut occupancy: BTreeMap<PartNodeId, u32> = BTreeMap::new();
    for c in &vessel.crew {
        *occupancy.entry(c.seat).or_default() += 1;
    }
    for (id, node) in &craft.graph.nodes {
        let seats = craft
            .parts
            .get(node.effective_index as usize)
            .map_or(0, |p| p.crew_seats);
        if seats > occupancy.get(id).copied().unwrap_or(0) {
            return Some(*id);
        }
    }
    None
}

/// The spawn-time contents map — every declared tank slot gets an
/// entry, and the craft's `mass_kg`/`inertia` are rewritten to match
/// (call ONCE per craft, before it is shared or welded — the mass
/// strip is not idempotent).
///
/// Two fill rules, split by resource class:
///
/// - **Propellant/consumable slots spawn FULL** — the resolver's
///   wet-mass convention (`mass_kg` already includes
///   `Σ capacity·mass_per_unit_kg`), value-neutral under
///   `RecoverVessel`: a recovered unit of propellant credits site
///   stock nothing spends.
/// - **§3.5 build-material/cargo slots
///   ([`csp_data::resource::is_build_material`] — ore/metal/composite)
///   spawn EMPTY.** `RecoverVessel` credits contents to the recovery
///   site's inventory at its efficiency (1.0 at a launch pad), so a
///   cargo tank delivered full would be a manufacture→recover faucet
///   minting the exact resources build orders spend — and a full tank
///   could never haul (`free = 0` at delivery). An empty slot lands
///   `free = capacity` for the colony→vessel supply loop instead.
///
/// The strip is physical, not just bookkeeping: the maintained
/// invariant is `mass_kg = dry + Σ bucketed wet` (module doc), so each
/// empty-spawned slot returns its `capacity·mass_per_unit_kg` through
/// the same [`apply_bucket_delta`] a full→empty debit would apply —
/// a later colony→vessel fill then adds the mass back exactly.
pub(crate) fn spawn_contents(
    craft: &mut crate::CraftDef,
) -> BTreeMap<PartNodeId, BTreeMap<ResourceId, f64>> {
    let mut out = BTreeMap::new();
    for (id, node) in &craft.graph.nodes {
        let Some(part) = craft.parts.get_mut(node.effective_index as usize) else {
            continue;
        };
        if part.tanks.is_empty() {
            continue;
        }
        // Snapshot the spec list so the strip can mutate the part.
        let tanks: Vec<(ResourceId, f64, f64)> = part
            .tanks
            .iter()
            .map(|t| (t.resource.clone(), t.capacity, t.mass_per_unit_kg))
            .collect();
        let mut stores = BTreeMap::new();
        for (resource, capacity, mpu) in tanks {
            if csp_data::resource::is_build_material(&resource) {
                // Cargo slot: empty contents, authored wet mass handed
                // back — the `debit` path's own full→empty move.
                apply_bucket_delta(part, capacity, mpu, capacity, 0.0);
                stores.insert(resource, 0.0);
            } else {
                stores.insert(resource, capacity);
            }
        }
        out.insert(*id, stores);
    }
    out
}

/// The `SpawnPhysicalVessel` gate for the [`TankSpec`](csp_phys::TankSpec)
/// mass convention: a tanked part must arrive with `mass_kg` covering
/// its full tanks (`mass_kg ≥ Σ capacity·mass_per_unit_kg`) — anything
/// less is corrupt authored input the dry-mass recovery could not make
/// sense of. Small negative slack absorbs authored rounding.
pub(crate) fn check_tank_masses(craft: &crate::CraftDef) -> Result<(), SimError> {
    for node in craft.graph.nodes.values() {
        let Some(part) = craft.parts.get(node.effective_index as usize) else {
            continue;
        };
        // Duplicate-resource tank specs are incoherent under the
        // rest of the module's conventions: `vessel_capacity` sums
        // every matching spec while `spawn_contents` keeps only the
        // last and `debit`/`credit` address only the first — the
        // resolver rejects them for authored crafts; a programmatic
        // `CraftDef` must meet the same gate here at the spawn
        // boundary.
        {
            let mut seen = std::collections::BTreeSet::new();
            if part.tanks.iter().any(|t| !seen.insert(t.resource.as_str())) {
                return Err(SimError::Recoverable(
                    "physical spawn: duplicate tank resource on one part",
                ));
            }
        }
        let full_wet: f64 = part
            .tanks
            .iter()
            .map(|t| t.capacity * t.mass_per_unit_kg)
            .sum();
        if !(full_wet.is_finite() && full_wet <= part.mass_kg + part.mass_kg.abs() * 1e-9 + 1e-9) {
            return Err(SimError::Recoverable(
                "physical spawn: tank wet mass exceeds part mass",
            ));
        }
    }
    Ok(())
}

/// Apply one contents slot's §2.4 bucketed-mass move to its part —
/// `mass_kg` is `dry + Σ bucketed wet`, so a write `old → new` shifts
/// the part mass by exactly `(bucket(new) − bucket(old))·cap·mpu`
/// and scales inertia by the mass ratio (contents ride at the part's
/// CoM — the resolved lump does not model slosh). This is what makes
/// the tally idempotent: the writer knows both contents values, so
/// the delta is exact — there is no `mass − wet_exact` dry-mass
/// recovery to drift, and rewriting the same contents is a bitwise
/// no-op (`dq == 0.0`). Returns whether the part's mass moved.
pub(crate) fn apply_bucket_delta(
    part: &mut csp_phys::WeldPart,
    capacity: f64,
    mass_per_unit_kg: f64,
    old: f64,
    new: f64,
) -> bool {
    let dq = (contents_bucket(new, capacity) - contents_bucket(old, capacity))
        * capacity
        * mass_per_unit_kg;
    if dq == 0.0 {
        return false;
    }
    let new_mass = part.mass_kg + dq;
    let ratio = if part.mass_kg > 0.0 {
        new_mass / part.mass_kg
    } else {
        1.0
    };
    let i = &mut part.inertia;
    i.xx *= ratio;
    i.yy *= ratio;
    i.zz *= ratio;
    i.xy *= ratio;
    i.xz *= ratio;
    i.yz *= ratio;
    part.mass_kg = new_mass;
    true
}

/// Drain `amount` units of `resource` from the vessel's tanks,
/// ascending `PartNodeId` — the fixed drain order. Returns `(moved,
/// mass_changed)`: the amount actually removed (≤ `amount`; callers
/// check sufficiency first, so it is `amount` in every committed
/// path) and whether any part's `mass_kg` moved (the caller re-welds
/// `InBubble` bodies on true via [`reweld_if_mass_changed`]).
/// Contents are exact units — the §2.4 quantum applies to mass only.
pub(crate) fn debit(vessel: &mut Vessel, resource: &ResourceId, amount: f64) -> (f64, bool) {
    let Some(craft) = &mut vessel.craft else {
        return (0.0, false);
    };
    let mut remaining = amount;
    let mut mass_changed = false;
    for (id, node) in &craft.graph.nodes {
        if remaining <= 0.0 {
            break;
        }
        let Some(part) = craft.parts.get_mut(node.effective_index as usize) else {
            continue;
        };
        let Some((cap, mpu)) = part
            .tanks
            .iter()
            .find(|t| t.resource == *resource)
            .map(|t| (t.capacity, t.mass_per_unit_kg))
        else {
            continue;
        };
        let stores = vessel.tank_contents.entry(*id).or_default();
        let held = stores.entry(resource.clone()).or_insert(0.0);
        let old = *held;
        let take = old.min(remaining);
        *held -= take;
        remaining -= take;
        mass_changed |= apply_bucket_delta(part, cap, mpu, old, *held);
    }
    (amount - remaining, mass_changed)
}

/// Fill `amount` units of `resource` into the vessel's tanks,
/// ascending `PartNodeId` — the fixed fill order. Returns `(moved,
/// mass_changed)`: the amount actually stored (≤ `amount`; the caller
/// owns the remainder's fallback) and whether any part's `mass_kg`
/// moved. Exact units.
pub(crate) fn credit(vessel: &mut Vessel, resource: &ResourceId, amount: f64) -> (f64, bool) {
    let Some(craft) = &mut vessel.craft else {
        return (0.0, false);
    };
    let mut remaining = amount;
    let mut mass_changed = false;
    for (id, node) in &craft.graph.nodes {
        if remaining <= 0.0 {
            break;
        }
        let Some(part) = craft.parts.get_mut(node.effective_index as usize) else {
            continue;
        };
        let Some((cap, mpu)) = part
            .tanks
            .iter()
            .find(|t| t.resource == *resource)
            .map(|t| (t.capacity, t.mass_per_unit_kg))
        else {
            continue;
        };
        let stores = vessel.tank_contents.entry(*id).or_default();
        let held = stores.entry(resource.clone()).or_insert(0.0);
        let old = *held;
        let add = (cap - old).max(0.0).min(remaining);
        *held += add;
        remaining -= add;
        mass_changed |= apply_bucket_delta(part, cap, mpu, old, *held);
    }
    (amount - remaining, mass_changed)
}

/// M9b §3.4 `Leak` — drain `amount` units from ONE part's tanks,
/// split proportional to current holdings (a leak drains the mix,
/// not one resource at a time). Exact-unit contents debits with the
/// §2.4 bucketed-mass move applied per resource, the `debit`
/// precedent — returns whether any `mass_kg` moved (the caller
/// re-welds on true via [`reweld_if_mass_changed`]). Field-level
/// arguments (the part + its store map) so the bubble tick's body
/// borrow can coexist.
pub(crate) fn leak_part(
    part: &mut csp_phys::WeldPart,
    stores: &mut BTreeMap<ResourceId, f64>,
    amount: f64,
) -> bool {
    let total: f64 = stores.values().copied().sum();
    if total <= 0.0 || !amount.is_finite() || amount <= 0.0 {
        return false;
    }
    let mut mass_changed = false;
    // BTreeMap order is the fixed drain order; the share is
    // proportional to holdings — deterministic given the record.
    let shares: Vec<(ResourceId, f64)> = stores
        .iter()
        .map(|(r, held)| (r.clone(), amount * held / total))
        .collect();
    for (r, share) in shares {
        let Some((cap, mpu)) = part
            .tanks
            .iter()
            .find(|t| t.resource == r)
            .map(|t| (t.capacity, t.mass_per_unit_kg))
        else {
            continue;
        };
        let held = stores.entry(r).or_insert(0.0);
        let old = *held;
        *held = (old - share).max(0.0);
        mass_changed |= apply_bucket_delta(part, cap, mpu, old, *held);
    }
    mass_changed
}

/// The re-weld half of the §2.4 inertia-cache rule — `debit`/`credit`
/// already folded the bucketed-mass delta into `mass_kg`/`inertia` at
/// write time (the only contents producers at M6), so this step only
/// re-welds an `InBubble` body when the write reports a moved mass.
/// `mass_changed` is the writers' exact bucket-flip report, which is
/// what keeps the tally idempotent: a write that crossed no bucket
/// boundary (or a repeat tally of the same contents) reports `false`
/// and nothing moves — there is no recovered dry mass to drift.
/// `OnRails`/`SurfaceLatched` vessels carry the updated `WeldPart`s
/// only; the next materialization welds the new mass fresh.
/// `Recoverable` on a weld failure: the contents write it follows is
/// rolled back with the command/event.
pub(crate) fn reweld_if_mass_changed(
    vessel: &mut Vessel,
    mass_changed: bool,
) -> Result<(), SimError> {
    if mass_changed && matches!(vessel.state, VesselState::InBubble(_)) {
        rebody(vessel)?;
    }
    Ok(())
}

/// Re-weld an `InBubble` vessel's body after a contents-driven mass
/// change — preserving kinematics, control state, settle, and the
/// attached aero table (the mass change alters no geometry).
fn rebody(vessel: &mut Vessel) -> Result<(), SimError> {
    let (Some(craft), VesselState::InBubble(old)) = (&vessel.craft, &vessel.state) else {
        return Ok(());
    };
    let mut body = csp_phys::weld(&craft.graph, &craft.parts)
        .map_err(|e| SimError::Recoverable(e.reason()))?;
    body.pos = old.pos;
    body.rot = old.rot;
    // `vel` is the CoM velocity and a mass redistribution moves the
    // welded `com_local`: apply the jettison/breakup convention —
    // the retained body's new CoM rides `v + ω_world × rot·Δcom_local`.
    // A same-mass re-weld moves `com_local` by nothing and the
    // correction vanishes, so the re-weld stays idempotent.
    let omega_world = old.rot.rotate(old.ang_vel);
    body.vel = old.vel + omega_world.cross(old.rot.rotate(body.com_local - old.com_local));
    body.ang_vel = old.ang_vel;
    body.throttle = old.throttle;
    body.settle = old.settle;
    body.aero = old.aero.clone();
    vessel.state = VesselState::InBubble(body);
    Ok(())
}

/// The M6-D transfer link — see the module doc's clause list. `a`
/// and `b` must be distinct live vessels (the callers check); a
/// self-addressed transfer is meaningless in the pooled-assembly
/// model and rejects upstream.
pub(crate) fn linked(
    joints: &[LiveJoint],
    vessels: &BTreeMap<VesselId, Vessel>,
    body_radius_m: Option<f64>,
    radius_m: f64,
    a: VesselId,
    b: VesselId,
) -> bool {
    // Clause 1 — jointed: a live §2.4 soft-capture joint physically
    // connects the pair (the pre-latch window; after the merge they
    // are one vessel and the pair is moot).
    if joints
        .iter()
        .any(|j| (j.vessel_a == a && j.vessel_b == b) || (j.vessel_a == b && j.vessel_b == a))
    {
        return true;
    }
    let (Some(va), Some(vb)) = (vessels.get(&a), vessels.get(&b)) else {
        return false;
    };
    match (&va.state, &vb.state) {
        // Clause 2 — in-bubble proximity (the M8-row mechanism):
        // body origins within `radius_m`.
        (VesselState::InBubble(ba), VesselState::InBubble(bb)) => {
            ba.pos.distance(bb.pos) <= radius_m
        }
        // Clause 3 — §3.5's verbatim rule: surface-latched on the same
        // body, within `radius_m`. The pose record is body-fixed
        // spherical; the chord distance between the two (R+alt) points
        // is the conservative measure (chord ≤ arc — in-range stays
        // in-range under any finer metric).
        (VesselState::SurfaceLatched(pa), VesselState::SurfaceLatched(pb)) => {
            if pa.body != pb.body {
                return false;
            }
            let Some(r) = body_radius_m else {
                return false;
            };
            let pa_pos = latched_pos(pa, r);
            let pb_pos = latched_pos(pb, r);
            pa_pos.distance(pb_pos) <= radius_m
        }
        _ => false,
    }
}

/// `(R + alt) · (cos φ·cos λ, cos φ·sin λ, sin φ)` — the latched
/// pose's world point on the dominant body's sphere (libm
/// transcendentals, §2.7). `pub(crate)` for
/// [`World::colony_surface_point`], the shared convention the M8-C
/// impact check's colony surface points ride.
pub(crate) fn latched_pos(pose: &crate::SurfacePose, body_radius_m: f64) -> DVec3 {
    let (slat, clat) = (libm::sin(pose.latitude_rad), libm::cos(pose.latitude_rad));
    let (slon, clon) = (libm::sin(pose.longitude_rad), libm::cos(pose.longitude_rad));
    let r = body_radius_m + pose.altitude_m;
    DVec3::new(r * clat * clon, r * clat * slon, r * slat)
}

/// The §7.1 field walk for one in-flight transfer — the world hash
/// calls it under the arena's ascending-id enumeration.
pub fn hash_transfer(h: &mut StateHasher, t: &ActiveTransfer) {
    for e in [t.from, t.to] {
        h.stable_enum(&e);
        match e {
            TransferEnd::Vessel(v) => {
                h.u64(v.0);
            }
            TransferEnd::Colony(c) => {
                h.u64(c.0);
            }
        }
    }
    h.stable_enum(&t.cargo);
    match &t.cargo {
        TransferCargo::Resource { resource, amount } => {
            h.str(resource.as_str());
            h.f64(*amount);
        }
        TransferCargo::Crew { crew } => {
            h.u64(crew.0);
        }
    }
    h.u64(t.complete_tick);
}

/// In-flight inbound resource units to `vessel` — the capacity
/// reservation half of the §3.5 precondition: a destination's free
/// space is checked *net of* transfers already in flight, so two
/// concurrent streams cannot overbook one tank.
fn inbound_resource(
    transfers: &BTreeMap<TransferId, ActiveTransfer>,
    to: TransferEnd,
    resource: &ResourceId,
) -> f64 {
    transfers
        .values()
        .filter(|t| t.to == to)
        .map(|t| match &t.cargo {
            TransferCargo::Resource {
                resource: r,
                amount,
            } if r == resource => *amount,
            _ => 0.0,
        })
        .sum()
}

/// In-flight inbound crew bound for `vessel` — seat reservations.
fn inbound_crew(transfers: &BTreeMap<TransferId, ActiveTransfer>, to: TransferEnd) -> u64 {
    transfers
        .values()
        .filter(|t| t.to == to && matches!(t.cargo, TransferCargo::Crew { .. }))
        .count() as u64
}

impl World {
    /// The live transfer arena — observation only (tests,
    /// presentation's transfer HUD). Ascending-id key order is the
    /// canonical enumeration.
    pub fn transfers(&self) -> &BTreeMap<TransferId, ActiveTransfer> {
        &self.transfers
    }

    /// `true` while `vessel` satisfies the `JoinColonyInventory`
    /// proximity gate against `colony` — `SurfaceLatched` on the
    /// colony's body within `transfer_radius_m` of the colony's site
    /// pose (the [`Self::vessel_colony_linked`] predicate the apply
    /// enforces). Observation-only read for the presentation layer's
    /// button gating — liveness (`colony_live`) is checked by the
    /// caller, exactly like the apply does.
    pub fn colony_joinable(&self, vessel: VesselId, colony: crate::economy::ColonyId) -> bool {
        self.vessel_colony_linked(vessel, colony)
    }

    /// `true` while the `TransferResource`/`TransferCrew` link
    /// precondition holds between `a` and `b` — jointed, co-bubble
    /// within `transfer_radius_m`, or co-latched on one body within
    /// the radius (the [`Self::transfer_linked`] clauses). `a == b`
    /// is never linked (the self-transfer rejection's reading).
    /// Observation-only read for presentation gating.
    pub fn vessels_linked(&self, a: VesselId, b: VesselId) -> bool {
        a != b && self.transfer_linked(a, b)
    }

    /// §3.5's transfer tunables — post-construction setter for worlds
    /// built from `balance.ron` ([`View`](csp_view)-side startup,
    /// `csp-bench` scenario setup). A construction-config write like
    /// [`World::set_docking_config`]: call before the first
    /// `step_frame`.
    pub fn set_transfer_config(&mut self, config: TransferConfig) {
        self.transfer_cfg = config;
    }

    /// The configured transfer tunables.
    pub fn transfer_config(&self) -> TransferConfig {
        self.transfer_cfg
    }

    /// The link precondition — the module doc's three clauses over
    /// this world's joints, residences, and configured radius.
    pub(crate) fn transfer_linked(&self, a: VesselId, b: VesselId) -> bool {
        let body_radius = self
            .physics
            .as_ref()
            .map(|p| p.backend.environment().radius_m);
        linked(
            &self.joints,
            &self.vessels,
            body_radius,
            self.transfer_cfg.radius_m,
            a,
            b,
        )
    }

    /// `TransferResource` — §3.5's process: validate the link and the
    /// books, debit the source now, schedule `TransferComplete`. All
    /// failures are `Recoverable` rejections on the originating
    /// channel (§2.2.1), never silent no-ops.
    pub(crate) fn apply_transfer_resource(
        &mut self,
        from: VesselId,
        to: VesselId,
        resource: &ResourceId,
        amount: f64,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        if from == to {
            // A latched assembly is ONE vessel with pooled tanks —
            // there is no second side to move units to.
            return Err(SimError::Recoverable("transfer: self-transfer is a no-op"));
        }
        if !(amount.is_finite() && amount > 0.0) {
            return Err(SimError::Recoverable("transfer: non-positive amount"));
        }
        if !self.transfer_cfg.is_valid() {
            return Err(SimError::Recoverable("transfer: invalid transfer config"));
        }
        let (Some(src), Some(dst)) = (self.vessels.get(&from), self.vessels.get(&to)) else {
            return Err(SimError::Recoverable("transfer: unknown vessel"));
        };
        if !self.transfer_linked(from, to) {
            return Err(SimError::Recoverable(
                "transfer: vessels not docked or in range",
            ));
        }
        if vessel_resource(src, resource) < amount {
            return Err(SimError::Recoverable("transfer: insufficient resource"));
        }
        let free = vessel_capacity(dst, resource)
            - vessel_resource(dst, resource)
            - inbound_resource(&self.transfers, TransferEnd::Vessel(to), resource);
        if free < amount {
            return Err(SimError::Recoverable("transfer: insufficient capacity"));
        }
        // Commit: debit now (the amount rides the in-flight record —
        // the books never show it aboard both vessels; the §2.4
        // bucketed-mass delta lands in the source's `WeldPart`s at the
        // same write), then record the process and schedule its
        // completion.
        self.shadow_vessels(shadow);
        self.shadow_transfers(shadow);
        let src = self
            .vessels
            .get_mut(&from)
            .ok_or(SimError::Fatal("transfer: source vessel vanished"))?;
        let (_, mass_changed) = debit(src, resource, amount);
        reweld_if_mass_changed(src, mass_changed)?;
        let id = TransferId(self.next_transfer_id);
        self.next_transfer_id = self
            .next_transfer_id
            .checked_add(1)
            .ok_or(SimError::Fatal("transfer id counter overflow"))?;
        let dur_ticks = (amount * csp_core::TICK_HZ as f64 / self.transfer_cfg.rate_units_per_s)
            .ceil()
            .min(u64::MAX as f64) as u64;
        let complete_tick = self
            .clock
            .ticks()
            .saturating_add(dur_ticks.max(1))
            .max(self.clock.ticks().saturating_add(1));
        self.transfers.insert(
            id,
            ActiveTransfer {
                id,
                from: TransferEnd::Vessel(from),
                to: TransferEnd::Vessel(to),
                cargo: TransferCargo::Resource {
                    resource: resource.clone(),
                    amount,
                },
                complete_tick,
            },
        );
        self.schedule_event(
            crate::EventKind::TransferComplete { transfer: id },
            complete_tick,
            shadow,
        )
    }

    /// `TransferCrew` — the same process shape for crew (module doc):
    /// the member must be aboard `from` and not already in flight;
    /// the destination must hold a free seat net of inbound
    /// reservations. The member keeps its source seat until the
    /// completion event reseats it.
    pub(crate) fn apply_transfer_crew(
        &mut self,
        from: VesselId,
        to: VesselId,
        crew: CrewId,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        if from == to {
            return Err(SimError::Recoverable(
                "crew transfer: self-transfer is a no-op",
            ));
        }
        if !self.transfer_cfg.is_valid() {
            return Err(SimError::Recoverable(
                "crew transfer: invalid transfer config",
            ));
        }
        let (Some(src), Some(dst)) = (self.vessels.get(&from), self.vessels.get(&to)) else {
            return Err(SimError::Recoverable("crew transfer: unknown vessel"));
        };
        if !src.crew.iter().any(|c| c.id == crew) {
            return Err(SimError::Recoverable(
                "crew transfer: crew not aboard source",
            ));
        }
        if self
            .transfers
            .values()
            .any(|t| matches!(&t.cargo, TransferCargo::Crew { crew: c } if *c == crew))
        {
            return Err(SimError::Recoverable(
                "crew transfer: crew already in flight",
            ));
        }
        if !self.transfer_linked(from, to) {
            return Err(SimError::Recoverable(
                "crew transfer: vessels not docked or in range",
            ));
        }
        let free = vessel_seats(dst)
            .saturating_sub(dst.crew.len() as u64)
            .saturating_sub(inbound_crew(&self.transfers, TransferEnd::Vessel(to)));
        if free == 0 {
            return Err(SimError::Recoverable("crew transfer: no free seat"));
        }
        self.shadow_transfers(shadow);
        let id = TransferId(self.next_transfer_id);
        self.next_transfer_id = self
            .next_transfer_id
            .checked_add(1)
            .ok_or(SimError::Fatal("transfer id counter overflow"))?;
        let complete_tick = self
            .clock
            .ticks()
            .saturating_add(self.transfer_cfg.crew_transfer_ticks.max(1))
            .max(self.clock.ticks().saturating_add(1));
        self.transfers.insert(
            id,
            ActiveTransfer {
                id,
                from: TransferEnd::Vessel(from),
                to: TransferEnd::Vessel(to),
                cargo: TransferCargo::Crew { crew },
                complete_tick,
            },
        );
        self.schedule_event(
            crate::EventKind::TransferComplete { transfer: id },
            complete_tick,
            shadow,
        )
    }

    /// `BoardCrew` — seat a fresh crew member on `vessel` (the M6
    /// stand-in for §3.5's "board → `Vessel` (requires a free
    /// `CrewSeat`)": M7's facility/hire flow is where real crew
    /// originate). Allocates the next `CrewId`, seats it at the
    /// lowest-id node with a free slot — the same seat-choice rule
    /// [`choose_seat`] gives completions. Free-seat accounting counts
    /// inbound in-flight crew as reserved, like the transfer path.
    pub(crate) fn apply_board_crew(
        &mut self,
        vessel: VesselId,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let Some(v) = self.vessels.get(&vessel) else {
            return Err(SimError::Recoverable("board: unknown vessel"));
        };
        let free = vessel_seats(v)
            .saturating_sub(v.crew.len() as u64)
            .saturating_sub(inbound_crew(&self.transfers, TransferEnd::Vessel(vessel)));
        if free == 0 {
            return Err(SimError::Recoverable("board: no free seat"));
        }
        let Some(seat) = choose_seat(v) else {
            return Err(SimError::Recoverable("board: no free seat"));
        };
        self.shadow_vessels(shadow);
        self.shadow_transfers(shadow);
        let id = CrewId(self.next_crew_id);
        self.next_crew_id = self
            .next_crew_id
            .checked_add(1)
            .ok_or(SimError::Fatal("crew id counter overflow"))?;
        let v = self
            .vessels
            .get_mut(&vessel)
            .ok_or(SimError::Fatal("board: vessel vanished"))?;
        v.crew.push(VesselCrew { id, seat });
        Ok(())
    }

    /// `TransferComplete` firing — the completion half of the process:
    /// consume the record and land its cargo per the module doc's
    /// best-effort rule (destination tanks/seats first, source as the
    /// fallback, whatever still fits nowhere is gone with its tanks).
    /// A stale id (the record is consumed only here, but a degraded
    /// prior firing could leave an orphan) is a no-op, never an
    /// error — the `DebrisDecay` precedent.
    pub(crate) fn fire_transfer_complete(
        &mut self,
        transfer: TransferId,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let Some(t) = self.transfers.get(&transfer).cloned() else {
            return Ok(());
        };
        self.shadow_vessels(shadow);
        self.shadow_transfers(shadow);
        self.transfers.remove(&transfer);
        match t.cargo {
            TransferCargo::Resource { resource, amount } => {
                let mut remaining = amount;
                let landed = self.credit_end(t.to, &resource, remaining, shadow)?;
                remaining -= landed;
                // M8-C §3.6: a landing into a colony's stores is the
                // resupply fact — cumulative by `(colony, resource)`,
                // counted by what actually landed, then the
                // `ColonyDelivery` trigger evaluates the subscribed
                // contracts. Gated on `colony_live`: the cargo still
                // lands in a dead colony's frozen store (terminal
                // bookkeeping, `credit_end`'s arm), but dumping into
                // an abandoned colony must not complete a "supply a
                // functioning colony" contract — the same gate the
                // crew-landing (`land_crew`) and join paths apply.
                if let TransferEnd::Colony(c) = t.to
                    && landed > 0.0
                    && self
                        .colonies
                        .get(&c)
                        .is_some_and(|co| crate::colonies::colony_live(&co.state))
                {
                    crate::contracts::shadow_contracts(&self.contracts, &mut shadow.contracts);
                    *self
                        .contracts
                        .facts
                        .colony_deliveries
                        .entry((c.0, resource.clone()))
                        .or_insert(0.0) += landed;
                    self.contract_notify(
                        &crate::contracts::ContractTrigger::ColonyDelivery,
                        shadow,
                    )?;
                }
                if remaining > 0.0 {
                    remaining -= self.credit_end(t.from, &resource, remaining, shadow)?;
                }
                // `remaining` > 0 here: every tank that could hold it
                // is gone (undocked fragment, breakup, removal). The
                // units leave the simulation — the tanks' fate was
                // already honest; the fallback order is the
                // documented totality.
                let _ = remaining;
            }
            TransferCargo::Crew { crew } => {
                self.land_crew(t.from, t.to, crew, shadow)?;
            }
        }
        Ok(())
    }

    /// The credit half of the best-effort rule: land `amount` at one
    /// endpoint, returning the units that actually moved. `Vessel` is
    /// the tank-walk (`credit` + re-weld); `Colony` is the aggregate
    /// store's re-anchor (`min(capacity)` like the tanks) — a dead or
    /// vanished colony still holds its frozen stores, so the credit
    /// lands there too (the store records are terminal bookkeeping,
    /// not a resurrected network — no recompute runs on a dead one).
    fn credit_end(
        &mut self,
        end: TransferEnd,
        resource: &ResourceId,
        amount: f64,
        shadow: &mut TickShadow,
    ) -> Result<f64, SimError> {
        match end {
            TransferEnd::Vessel(v) => match self.vessels.get_mut(&v) {
                Some(vessel) => {
                    let (moved, mass_changed) = credit(vessel, resource, amount);
                    reweld_if_mass_changed(vessel, mass_changed)?;
                    Ok(moved)
                }
                None => Ok(0.0),
            },
            TransferEnd::Colony(c) => {
                self.shadow_colonies(shadow);
                let now = self.clock.ticks();
                let Some(colony) = self.colonies.get_mut(&c) else {
                    return Ok(0.0);
                };
                let Some(store) = colony.stores.get_mut(resource) else {
                    return Ok(0.0);
                };
                let level = store.level_at(now);
                let moved = (store.capacity - level).max(0.0).min(amount);
                store.anchor_level = level + moved;
                store.anchor_tick = now;
                if crate::colonies::colony_live(&colony.state) {
                    self.recompute_colony(c, now, shadow)?;
                }
                Ok(moved)
            }
        }
    }

    /// The crew-landing half: `Vessel` destinations reseat through
    /// `choose_seat` (the vessel-vessel rule); `Colony` destinations
    /// add the member to the colony's crew set and write the roster's
    /// `CrewLocation::Colony` (the roster↔set pairing the restore
    /// gate enforces). Any unmet precondition no-ops — the member
    /// simply stays at the source, the same totality.
    fn land_crew(
        &mut self,
        from: TransferEnd,
        to: TransferEnd,
        crew: CrewId,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        match (from, to) {
            (TransferEnd::Vessel(f), TransferEnd::Vessel(t)) => {
                if f != t
                    && let Some(seat) = self.vessels.get(&t).and_then(choose_seat)
                    && let Some(src) = self.vessels.get_mut(&f)
                    && let Some(pos) = src.crew.iter().position(|c| c.id == crew)
                {
                    src.crew.remove(pos);
                    let dst = self
                        .vessels
                        .get_mut(&t)
                        .ok_or(SimError::Fatal("transfer: destination vanished mid-fire"))?;
                    dst.crew.push(VesselCrew { id: crew, seat });
                    // The roster half of the re-seat — the `Colony`
                    // arms' pairing: the member's recorded location
                    // moves with the seat, keeping the roster↔seat
                    // invariant the restore gate checks.
                    if self
                        .economy
                        .crew
                        .get(&crew)
                        .is_some_and(|m| m.location == crate::economy::CrewLocation::Vessel(f))
                    {
                        self.shadow_economy(shadow);
                        if let Some(m) = self.economy.crew.get_mut(&crew) {
                            m.location = crate::economy::CrewLocation::Vessel(t);
                        }
                    }
                }
            }
            // vessel → colony: the member leaves its seat for the
            // colony's crew set; roster location becomes `Colony`.
            (TransferEnd::Vessel(f), TransferEnd::Colony(c)) => {
                let live = self
                    .colonies
                    .get(&c)
                    .is_some_and(|co| crate::colonies::colony_live(&co.state));
                if live
                    && let Some(src) = self.vessels.get_mut(&f)
                    && let Some(pos) = src.crew.iter().position(|c| c.id == crew)
                {
                    src.crew.remove(pos);
                    self.shadow_colonies(shadow);
                    self.shadow_economy(shadow);
                    if let Some(co) = self.colonies.get_mut(&c) {
                        co.crew.insert(crew);
                    }
                    if let Some(m) = self.economy.crew.get_mut(&crew) {
                        m.location = crate::economy::CrewLocation::Colony(c);
                    }
                    let now = self.clock.ticks();
                    self.recompute_colony(c, now, shadow)?;
                }
            }
            // colony → vessel: the member leaves the colony's crew
            // set for a seat; roster location becomes `Vessel`.
            (TransferEnd::Colony(c), TransferEnd::Vessel(t)) => {
                let active = self
                    .economy
                    .crew
                    .get(&crew)
                    .is_some_and(|m| m.status == crate::economy::CrewStatus::Active);
                if active
                    && self
                        .colonies
                        .get(&c)
                        .is_some_and(|co| co.crew.contains(&crew))
                    && let Some(seat) = self.vessels.get(&t).and_then(choose_seat)
                {
                    self.shadow_colonies(shadow);
                    self.shadow_economy(shadow);
                    if let Some(co) = self.colonies.get_mut(&c) {
                        co.crew.remove(&crew);
                    }
                    let dst = self
                        .vessels
                        .get_mut(&t)
                        .ok_or(SimError::Fatal("transfer: destination vanished mid-fire"))?;
                    dst.crew.push(VesselCrew { id: crew, seat });
                    if let Some(m) = self.economy.crew.get_mut(&crew) {
                        m.location = crate::economy::CrewLocation::Vessel(t);
                    }
                    let now = self.clock.ticks();
                    self.recompute_colony(c, now, shadow)?;
                }
            }
            (TransferEnd::Colony(_), TransferEnd::Colony(_)) => {}
        }
        Ok(())
    }
}

impl World {
    /// The colony's surface point in its body's fixed frame — the ONE
    /// convention both the join gate below and the impact seam
    /// (`colonies::apply_surface_impacts`) read:
    /// `colony_env[body].radius_m + colony.altitude_m` at the colony's
    /// latched lat/lon. `None` when the body carries no `colony_env`
    /// entry (the impact seam's "not a target" gate).
    pub(crate) fn colony_surface_point(&self, co: &crate::colonies::Colony) -> Option<DVec3> {
        let r = self.colony_env.get(&co.body)?.radius_m;
        let pose = crate::SurfacePose {
            body: co.body.clone(),
            latitude_rad: co.latitude_rad,
            longitude_rad: co.longitude_rad,
            altitude_m: co.altitude_m,
            orientation: csp_core::DQuat::IDENTITY,
        };
        Some(latched_pos(&pose, r))
    }

    /// §3.5's colony-inventory opt-in gate: a `SurfaceLatched` vessel
    /// on the colony's body whose pose chord distance to the colony's
    /// site pose is within `transfer_radius_m` — the same-body,
    /// same-radius rule `linked` uses for two latched vessels, with
    /// the colony env's `radius_m` covering non-dominant bodies.
    fn vessel_colony_linked(&self, vessel: VesselId, colony: crate::economy::ColonyId) -> bool {
        let (Some(v), Some(co)) = (self.vessels.get(&vessel), self.colonies.get(&colony)) else {
            return false;
        };
        let VesselState::SurfaceLatched(pose) = &v.state else {
            return false;
        };
        if pose.body != co.body {
            return false;
        }
        let Some(r) = self.colony_env.get(&co.body).map(|e| e.radius_m) else {
            return false;
        };
        let Some(co_pos) = self.colony_surface_point(co) else {
            return false;
        };
        latched_pos(pose, r).distance(co_pos) <= self.transfer_cfg.radius_m
    }

    /// `JoinColonyInventory` — §3.5's "a surface-latched vessel within
    /// a colony's radius MAY join the colony's inventory" — the
    /// opt-in record the vessel↔colony transfers read. NEVER
    /// automatic: an unjoined latched vessel beside a colony
    /// transfers nothing. The vessel must sit within
    /// `transfer_radius_m` of the colony on the colony's body, and
    /// the colony must be live (a frozen dead colony's stores take
    /// no new joiners).
    pub(crate) fn apply_join_colony(
        &mut self,
        vessel: VesselId,
        colony: crate::economy::ColonyId,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let Some(co) = self.colonies.get(&colony) else {
            return Err(SimError::Recoverable("join colony: unknown colony"));
        };
        if !crate::colonies::colony_live(&co.state) {
            return Err(SimError::Recoverable(
                "join colony: colony is dormant or abandoned",
            ));
        }
        if !self.transfer_cfg.is_valid() {
            return Err(SimError::Recoverable(
                "join colony: invalid transfer config",
            ));
        }
        let Some(v) = self.vessels.get(&vessel) else {
            return Err(SimError::Recoverable("join colony: unknown vessel"));
        };
        if v.joined_colony == Some(colony) {
            return Ok(());
        }
        if !self.vessel_colony_linked(vessel, colony) {
            return Err(SimError::Recoverable(
                "join colony: vessel not latched in range of the colony",
            ));
        }
        self.shadow_vessels(shadow);
        self.vessels
            .get_mut(&vessel)
            .ok_or(SimError::Fatal("join colony: vessel vanished"))?
            .joined_colony = Some(colony);
        Ok(())
    }

    /// `LeaveColonyInventory` — clear the opt-in record (the join
    /// was per-vessel; clearing while a transfer is in flight does
    /// not recall the pipe — the completion's endpoint dispatch is
    /// total).
    pub(crate) fn apply_leave_colony(
        &mut self,
        vessel: VesselId,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        if !self.vessels.contains_key(&vessel) {
            return Err(SimError::Recoverable("leave colony: unknown vessel"));
        }
        self.shadow_vessels(shadow);
        self.vessels
            .get_mut(&vessel)
            .ok_or(SimError::Fatal("leave colony: vessel vanished"))?
            .joined_colony = None;
        Ok(())
    }

    /// The colony gate every vessel↔colony command shares: the
    /// vessel's `joined_colony` must name THIS colony — proximity is
    /// not re-checked (the member joined where it sits; it leaves
    /// only by command or by leaving the surface, which clears the
    /// record).
    fn joined_colony_gate(
        &self,
        vessel: VesselId,
        colony: crate::economy::ColonyId,
    ) -> Result<(), SimError> {
        let Some(v) = self.vessels.get(&vessel) else {
            return Err(SimError::Recoverable("colony transfer: unknown vessel"));
        };
        if v.joined_colony != Some(colony) {
            return Err(SimError::Recoverable(
                "colony transfer: vessel has not joined this colony's inventory",
            ));
        }
        let Some(co) = self.colonies.get(&colony) else {
            return Err(SimError::Recoverable("colony transfer: unknown colony"));
        };
        if !crate::colonies::colony_live(&co.state) {
            return Err(SimError::Recoverable(
                "colony transfer: colony is dormant or abandoned",
            ));
        }
        Ok(())
    }

    /// `TransferColonyResource { to_colony }` — the vessel↔colony
    /// half of the §3.5 process, same reservation semantics as the
    /// vessel-vessel path: debit the source at the command boundary
    /// (the colony side's debit is a store re-anchor plus a re-solve
    /// — the dropped stock can unground consumers), reserve the
    /// destination's capacity net of inbound streams, and schedule
    /// `TransferComplete` on the same flow-rate grid.
    #[allow(clippy::too_many_lines)] // the two directions mirror as one list
    pub(crate) fn apply_transfer_colony_resource(
        &mut self,
        vessel: VesselId,
        colony: crate::economy::ColonyId,
        resource: &ResourceId,
        amount: f64,
        to_colony: bool,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        if !(amount.is_finite() && amount > 0.0) {
            return Err(SimError::Recoverable(
                "colony transfer: non-positive amount",
            ));
        }
        if !self.transfer_cfg.is_valid() {
            return Err(SimError::Recoverable(
                "colony transfer: invalid transfer config",
            ));
        }
        self.joined_colony_gate(vessel, colony)?;
        let now = self.clock.ticks();
        if to_colony {
            let src = self
                .vessels
                .get(&vessel)
                .ok_or(SimError::Fatal("colony transfer: vessel vanished"))?;
            if vessel_resource(src, resource) < amount {
                return Err(SimError::Recoverable(
                    "colony transfer: insufficient resource aboard",
                ));
            }
            let co = self
                .colonies
                .get(&colony)
                .ok_or(SimError::Fatal("colony transfer: colony vanished"))?;
            let capacity = co.stores.get(resource).map(|st| st.capacity).unwrap_or(0.0);
            let level = co
                .stores
                .get(resource)
                .map(|st| st.level_at(now))
                .unwrap_or(0.0);
            let free = capacity
                - level
                - inbound_resource(&self.transfers, TransferEnd::Colony(colony), resource);
            if free < amount {
                return Err(SimError::Recoverable(
                    "colony transfer: insufficient colony store capacity",
                ));
            }
            // Commit — debit the vessel's tanks now.
            self.shadow_vessels(shadow);
            let src = self
                .vessels
                .get_mut(&vessel)
                .ok_or(SimError::Fatal("colony transfer: vessel vanished"))?;
            let (_, mass_changed) = debit(src, resource, amount);
            reweld_if_mass_changed(src, mass_changed)?;
        } else {
            let co = self
                .colonies
                .get(&colony)
                .ok_or(SimError::Fatal("colony transfer: colony vanished"))?;
            let level = co
                .stores
                .get(resource)
                .map(|st| st.level_at(now))
                .unwrap_or(0.0);
            if level < amount {
                return Err(SimError::Recoverable(
                    "colony transfer: insufficient colony store level",
                ));
            }
            let dst = self
                .vessels
                .get(&vessel)
                .ok_or(SimError::Fatal("colony transfer: vessel vanished"))?;
            let free = vessel_capacity(dst, resource)
                - vessel_resource(dst, resource)
                - inbound_resource(&self.transfers, TransferEnd::Vessel(vessel), resource);
            if free < amount {
                return Err(SimError::Recoverable(
                    "colony transfer: insufficient vessel capacity",
                ));
            }
            // Commit — the store's anchor drops now, then a re-solve
            // (the stock dip can change every grant and bound).
            self.shadow_colonies(shadow);
            let co = self
                .colonies
                .get_mut(&colony)
                .ok_or(SimError::Fatal("colony transfer: colony vanished"))?;
            if let Some(store) = co.stores.get_mut(resource) {
                store.anchor_level = store.level_at(now) - amount;
                store.anchor_tick = now;
            }
            self.recompute_colony(colony, now, shadow)?;
        }
        self.shadow_transfers(shadow);
        let id = TransferId(self.next_transfer_id);
        self.next_transfer_id = self
            .next_transfer_id
            .checked_add(1)
            .ok_or(SimError::Fatal("transfer id counter overflow"))?;
        let dur_ticks = (amount * csp_core::TICK_HZ as f64 / self.transfer_cfg.rate_units_per_s)
            .ceil()
            .min(u64::MAX as f64) as u64;
        let complete_tick = self
            .clock
            .ticks()
            .saturating_add(dur_ticks.max(1))
            .max(self.clock.ticks().saturating_add(1));
        let (from, to) = if to_colony {
            (TransferEnd::Vessel(vessel), TransferEnd::Colony(colony))
        } else {
            (TransferEnd::Colony(colony), TransferEnd::Vessel(vessel))
        };
        self.transfers.insert(
            id,
            ActiveTransfer {
                id,
                from,
                to,
                cargo: TransferCargo::Resource {
                    resource: resource.clone(),
                    amount,
                },
                complete_tick,
            },
        );
        self.schedule_event(
            crate::EventKind::TransferComplete { transfer: id },
            complete_tick,
            shadow,
        )
    }

    /// `TransferColonyCrew { to_colony }` — the crew half: the member
    /// keeps the source seat/set slot until `TransferComplete`
    /// reseats it, roster `CrewLocation` updated at the landing (the
    /// `AssignCrewToColony`-side pairing the restore gate enforces).
    /// Members must be roster-tracked and `Active` — a `BoardCrew`
    /// id-only member has no roster record to pair with the colony's
    /// crew set.
    pub(crate) fn apply_transfer_colony_crew(
        &mut self,
        vessel: VesselId,
        colony: crate::economy::ColonyId,
        crew: CrewId,
        to_colony: bool,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        if !self.transfer_cfg.is_valid() {
            return Err(SimError::Recoverable(
                "colony crew transfer: invalid transfer config",
            ));
        }
        self.joined_colony_gate(vessel, colony)?;
        if self
            .transfers
            .values()
            .any(|t| matches!(&t.cargo, TransferCargo::Crew { crew: c } if *c == crew))
        {
            return Err(SimError::Recoverable(
                "colony crew transfer: crew already in flight",
            ));
        }
        let member = self
            .economy
            .crew
            .get(&crew)
            .filter(|m| m.status == crate::economy::CrewStatus::Active)
            .ok_or(SimError::Recoverable(
                "colony crew transfer: no active roster member",
            ))?;
        if to_colony {
            let aboard = self
                .vessels
                .get(&vessel)
                .is_some_and(|v| v.crew.iter().any(|c| c.id == crew));
            if !aboard {
                return Err(SimError::Recoverable(
                    "colony crew transfer: crew not aboard vessel",
                ));
            }
            // C09-F4: the vessel→colony half shares
            // `AssignCrewToColony`'s posting precondition — landing
            // crew at an LS-less colony would starve them on the
            // baseline draw the solve now models.
            let col = self.colonies.get(&colony).ok_or(SimError::Recoverable(
                "colony crew transfer: unknown colony",
            ))?;
            if !self.colony_accepts_crew(col) {
                return Err(SimError::Recoverable(
                    "colony crew transfer: colony has no working life-support module",
                ));
            }
        } else {
            if member.location != crate::economy::CrewLocation::Colony(colony) {
                return Err(SimError::Recoverable(
                    "colony crew transfer: crew not at this colony",
                ));
            }
            let dst = self
                .vessels
                .get(&vessel)
                .ok_or(SimError::Fatal("colony crew transfer: vessel vanished"))?;
            let free = vessel_seats(dst)
                .saturating_sub(dst.crew.len() as u64)
                .saturating_sub(inbound_crew(&self.transfers, TransferEnd::Vessel(vessel)));
            if free == 0 {
                return Err(SimError::Recoverable("colony crew transfer: no free seat"));
            }
        }
        self.shadow_transfers(shadow);
        let id = TransferId(self.next_transfer_id);
        self.next_transfer_id = self
            .next_transfer_id
            .checked_add(1)
            .ok_or(SimError::Fatal("transfer id counter overflow"))?;
        let complete_tick = self
            .clock
            .ticks()
            .saturating_add(self.transfer_cfg.crew_transfer_ticks.max(1))
            .max(self.clock.ticks().saturating_add(1));
        let (from, to) = if to_colony {
            (TransferEnd::Vessel(vessel), TransferEnd::Colony(colony))
        } else {
            (TransferEnd::Colony(colony), TransferEnd::Vessel(vessel))
        };
        self.transfers.insert(
            id,
            ActiveTransfer {
                id,
                from,
                to,
                cargo: TransferCargo::Crew { crew },
                complete_tick,
            },
        );
        self.schedule_event(
            crate::EventKind::TransferComplete { transfer: id },
            complete_tick,
            shadow,
        )
    }
}

/// Merge an absorbed vessel's contents/crew into the primary's
/// through `map` (the latch merge's node remap) — called by
/// [`crate::joints::apply_latch`]; returns the merged records so the
/// latch's build-locals-then-commit pattern is preserved. `map` must
/// already preserve primary ids (the merge remap covers only the
/// secondary's renumbered nodes — remapped ids are fresh and cannot
/// collide with them).
pub(crate) fn merge_vessel_records(
    primary: &Vessel,
    secondary: &Vessel,
    map: &impl Fn(PartNodeId) -> PartNodeId,
) -> (
    BTreeMap<PartNodeId, BTreeMap<ResourceId, f64>>,
    Vec<VesselCrew>,
) {
    let mut tank_contents = primary.tank_contents.clone();
    for (node, stores) in &secondary.tank_contents {
        tank_contents
            .entry(map(*node))
            .or_default()
            .extend(stores.iter().map(|(r, c)| (r.clone(), *c)));
    }
    let mut crew = primary.crew.clone();
    crew.extend(secondary.crew.iter().map(|c| VesselCrew {
        id: c.id,
        seat: map(c.seat),
    }));
    (tank_contents, crew)
}

/// Redirect in-flight transfers naming `sec` (the absorbed vessel)
/// to `prim` (the merged survivor) — the latch rewrite that lets a
/// stream already in the pipe land in the merged vessel's tanks
/// (the absorbed vessel's capacity traveled with it).
pub(crate) fn remap_transfer_vessels(
    transfers: &mut BTreeMap<TransferId, ActiveTransfer>,
    sec: VesselId,
    prim: VesselId,
) {
    for t in transfers.values_mut() {
        if t.from == TransferEnd::Vessel(sec) {
            t.from = TransferEnd::Vessel(prim);
        }
        if t.to == TransferEnd::Vessel(sec) {
            t.to = TransferEnd::Vessel(prim);
        }
    }
}

/// Partition a splitting vessel's records for the fragment: contents
/// and crew bound to nodes in `frag_nodes` leave with it. Returns the
/// fragment's `(tank_contents, crew)`; the caller retains the rest.
pub(crate) fn split_vessel_records(
    vessel: &mut Vessel,
    frag_nodes: &BTreeSet<PartNodeId>,
) -> (
    BTreeMap<PartNodeId, BTreeMap<ResourceId, f64>>,
    Vec<VesselCrew>,
) {
    let frag_tanks: BTreeMap<PartNodeId, BTreeMap<ResourceId, f64>> = vessel
        .tank_contents
        .iter()
        .filter(|(id, _)| frag_nodes.contains(id))
        .map(|(id, stores)| (*id, stores.clone()))
        .collect();
    vessel
        .tank_contents
        .retain(|id, _| !frag_nodes.contains(id));
    let (frag_crew, kept_crew): (Vec<VesselCrew>, Vec<VesselCrew>) = vessel
        .crew
        .iter()
        .partition(|c| frag_nodes.contains(&c.seat));
    vessel.crew = kept_crew;
    (frag_tanks, frag_crew)
}
