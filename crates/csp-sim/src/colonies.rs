//! M8-A: the §3.5.1 colony model — a piecewise-linear flow network
//! recomputed only at events. This is the load-bearing math of the RTS
//! layer: a colony is a module list plus per-resource linear stores on a
//! body's surface, and every quantity it produces is `rate · dt` over an
//! event-bounded interval — never integrated.
//!
//! # The five rules, verbatim and where they live
//!
//! 1. **Power is a two-state step.** `Daylight::{Day,Night}` toggles only
//!    at [`EventKind::TerminatorCrossing`] (`Silent`) — a closed form
//!    over body rotation and orbital phase at tilt 0° (`body_light` /
//!    `next_terminator_tick`). Day production uses the tilt-0 daylit
//!    *average* insolation `(2/π)·S·cos φ`, the closed form standing in
//!    for the plan's authored per-latitude average (the tilt-0 closed
//!    form stands; no latitude table is authored to date). `electric_charge` is a resource like any other — Solar and
//!    Generator produce it, every `power_ec_per_s` consumes it, Battery
//!    is its store — so "batteries drain" and "PowerLost" are the same
//!    linear-store machinery as ore and oxygen, not a second model.
//! 2. **Thermal is algebraic, never integrated.** `T_eq` solves
//!    `Q_in = εσA·T⁴` once per recompute (`solve_network`) and is held
//!    constant over the interval — T⁴ has no closed-form integral, so
//!    no integration exists to break it. `t_eq_k: None` records "no
//!    radiating area to equilibrate against" (a radiator-less colony's
//!    steady state is unbounded — recorded as `None`, never ∞, so no
//!    non-finite value can enter hashed state).
//! 3. **Converters run at a fixed interval rate** from the deterministic
//!    allocation pass: resources ascend `ResourceId`; consumers sort
//!    `(priority, ColonyId, ModuleIndex)` — per-colony today, so the key
//!    reduces to `(priority, ModuleIndex)`; `ColonyId` stays in the key
//!    so multi-owner feedstock pools (post-M8-A) keep the same
//!    ordering. Grants are full-rate until the resource's available
//!    rate is exhausted, then the remainder is split *pro-rata by
//!    request* over the consumers still unfunded — the "pro-rate the
//!    remainder" clause. Before any grant is computed the pass finds
//!    the *least fixpoint* of "every requested resource is stocked or
//!    produced by ANOTHER grounded module" — an ungrounded module (a
//!    self-feeding converter, a source-less cycle) pins at fraction 0:
//!    it registers no requests — it may not steal grants — and produces
//!    nothing. Grounded modules then iterate the grant equations to
//!    their greatest fixpoint (the operational "run as much as the
//!    network supports" reading): fractions start at 1.0 and only
//!    decrease (`min(prev, computed)`), capped at `modules + 1` sweeps —
//!    bounded and deterministic even for cyclic chains. Consumption is
//!    charged at `fraction · request` — a starved converter draws
//!    nothing, not its full request.
//! 4. **Next event** = `min` over resources of `level/(−net)` emptying
//!    and `(cap−level)/net` filling, plus the next `TerminatorCrossing`,
//!    plus the next scheduled inspection. Levels are epoch-anchored
//!    (`ColonyStore::{anchor_level, anchor_tick}`) and evaluated
//!    `level(t) = anchor + net·(t−anchor)/TICK_HZ` — the same
//!    partition-independence argument the rails epoch anchor makes:
//!    stored state mutates only at absolute-tick events and commands,
//!    so `amount = rate·dt` is literally true and bitwise identical in
//!    every §2.6.1 partition.
//! 5. **States are explicit, transitions are events.** `PowerLost`
//!    (`Clamping`) → `Degraded{PowerLost}`; `PowerRestored` → `Nominal`;
//!    `LifeSupportDepleted` (`Clamping`, §2.6.5 member) → crew `Lost` +
//!    `Dormant`; `AbandonColony` → `Abandoned` at `abandon_recovery`
//!    salvage — and every terminal transition vacates the joined
//!    vessels' `joined_colony` opt-ins with it (C09-F8). `Dormant`/
//!    `Abandoned` **consume, produce, and schedule nothing** — the
//!    mechanism that keeps 20 half-dead colonies out of the event
//!    budget. `DegradeReason::ImpactDamage` is the crash-damage class —
//!    M8-C's `apply_surface_impacts` drives it in play (debris landing
//!    inside `transfer_radius_m`), `SimCommand::DamageColonyModule` is
//!    the test stand-in, and a `ColonyDamaged` notification records the
//!    hit in the log ring (C09-F6).
//!
//! # What "Degraded" means
//!
//! In any `Degraded` state, drills and converters go inert (production
//! halt is the honest reading of "converters stop"); solar/generator
//! output continues — that is precisely how a `PowerLost` colony
//! re-charges its battery by day and earns `PowerRestored`. Life
//! support keeps drawing from its resource stores ("switches to
//! reserves"): when a required store is empty and the grant cannot
//! cover the request with crew aboard, the recompute schedules
//! `LifeSupportDepleted`. The depletion scan covers EVERY `LifeSupport`
//! module — damaged and ungrounded ones included: such a module's
//! grant is zero, so crew aboard a colony whose life support can no
//! longer serve a required resource die on schedule ("damage should
//! kill"). The scan walks the module's whole request set — the
//! `power_ec_per_s` row included — so an unpowered module whose EC
//! request can be served by neither stock nor production reads as
//! depleted even while a consumable reserve stays stocked (a
//! power-pinned module cannot run at all; unsatisfied requests are
//! depletion evidence).
//! `PowerRestored` exits `Degraded` only for `reason: PowerLost`;
//! `Degraded{ImpactDamage}`'s exit is **`SimCommand::
//! RepairColonyModule`** (C09-F6 — the doc-promised post-M8-A
//! follow-up): for the authored `colony_module_repair_cost_funds`
//! debit it clears one module's `damaged` flag, and lifts the
//! `ImpactDamage` reason to `Nominal` once no damage stands. Every
//! hit logs a `ColonyDamaged` notification and every repair a
//! `ColonyModuleRepaired` one. If damage lands while a colony is already
//! `Degraded{PowerLost}`, the `PowerLost` reason wins (the apply only
//! records `ImpactDamage` from `Nominal`) — `PowerRestored` can still
//! return it to `Nominal`, damaged modules and all (and repairing a
//! module under `PowerLost` clears the flag without touching the
//! reason — power starvation is the network's business).
//!
//! # Stale events
//!
//! Every recompute scrubs the colony's pending *boundary* events
//! (terminator, the one store-bound event, inspections) and republishes
//! the fresh set, so the heap holds only live deadlines — a stale
//! firing is just an early recompute (idempotent), never a wrong
//! level. Notification events (`PowerLost`, `PowerRestored`,
//! `LifeSupportDepleted`) are never scrubbed BY THE RECOMPUTE — they
//! are committed transitions one tick out (the life-support toggle's
//! whole-heap scrub excepted: `LifeSupportDepleted` is committed only
//! while the subsystem lives, and its firing re-checks the flag).
//! Committed is the event, never its predicate: each transition arm
//! re-solves the network at its own tick (`colony_net_solve`), so a
//! same-tick `TransferComplete` resupply — or any landing that lifts
//! `ls_depleted`/`power_starved` first — cancels the transition it
//! would have refuted —
//! and never duplicated: a recompute whose
//! predicate already has that notification pending in the heap skips
//! re-scheduling it (two recomputes at one tick — two commands in one
//! drain, a boundary firing ahead of the notification's own tick —
//! would otherwise queue a second copy the restore pairing walk
//! rejects). The `Colonies` slot's interval advance is
//! publish-only (`publish_colonies`): it re-derives each live colony's
//! earliest needed boundary and repairs a missing deadline — the
//! §2.6.5 "publish your next event time" guarantee against a
//! dropped/degraded firing. It never mutates colony records, so the
//! publish is a pure function of `(state, heap)` — identical in every
//! partition.

use std::collections::{BTreeMap, BTreeSet};

use csp_core::{SimError, StableDiscriminant, StateHasher, TICK_HZ};
use csp_data::{
    BodyDef, BodyId, ColonyDef, ColonyModuleDef, RecipeDef, RecipeId, ResourceId, SiteId,
};
use serde::{Deserialize, Serialize};

use crate::economy::{ColonyId, CrewStatus};
use crate::orbit::PatchedConics;
use crate::transfers::CrewId;
use crate::{EventKind, TickShadow, World};

/// The `ResourceId` spelling of charge — `resources.ron`'s
/// `electric_charge`. Power is a resource (module doc rule 1); every
/// place the network treats EC specially keys on this id. The
/// canonical spelling lives in `csp_data::resource` — the colony
/// validator's one-EC-draw check keys on the same constant.
pub const ELECTRIC_CHARGE: &str = csp_data::resource::ELECTRIC_CHARGE;

/// Stefan–Boltzmann σ, W/(m²·K⁴) — the §3.5.1 rule-2 solve constant.
const STEFAN_BOLTZMANN: f64 = 5.670374419e-8;

/// Rate/level epsilon for the starvation, depletion, and bound-crossing
/// predicates — a documented comparison floor, not a physical quantity.
const EPS: f64 = 1e-9;

/// |cos φ| below this is "at the pole": a tilt-0 pole sits on the
/// terminator permanently — the model reports `Night` (grazing sun is
/// ~zero insolation anyway, and the authored daylit average yields 0)
/// and schedules no `TerminatorCrossing`s.
const POLE_COS_EPS: f64 = 1e-9;

/// `LifeSupport` consumers' allocation priority — the lowest number
/// wins, and crew survival outranks every production draw (the
/// authored `Converter::priority` space starts wherever the data
/// authors it; 0 is the sim's reserved "life first" slot).
const PRIORITY_LIFE_SUPPORT: u8 = 0;
/// Default allocation priority for consumers the data does not rank —
/// drills' EC draws and any future unranked consumer.
const PRIORITY_DEFAULT: u8 = u8::MAX;

const TWO_PI: f64 = 2.0 * core::f64::consts::PI;
const PI: f64 = core::f64::consts::PI;

// ---------------------------------------------------------------------------
// State types — §3.5.1's explicit model, serialized + hashed
// ---------------------------------------------------------------------------

/// §3.5.1's two-state power source: insolation is `Day` or `Night` and
/// nothing in between (rule 1). Discriminants append-only.
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Daylight {
    /// Sunlit — solar modules produce at the daylit-average flux.
    Day = 0,
    /// Dark — solar modules produce nothing.
    Night = 1,
}

impl StableDiscriminant for Daylight {
    fn discriminant(&self) -> u16 {
        *self as u16
    }
}

/// §3.5.1's `DegradeReason` — why the colony is `Degraded`.
/// Discriminants append-only.
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DegradeReason {
    /// `PowerLost` — EC consumers starved (converters stop, batteries
    /// drain, life support on reserves).
    PowerLost = 0,
    /// A crashing vessel's impact damaged modules (§3.5.1 rule 5's
    /// "a `Degraded` reason"); driven in play by M8-C's
    /// `apply_surface_impacts`, marked in tests via
    /// `SimCommand::DamageColonyModule`, and lifted by
    /// `SimCommand::RepairColonyModule` once no damage stands.
    ImpactDamage = 1,
}

impl StableDiscriminant for DegradeReason {
    fn discriminant(&self) -> u16 {
        *self as u16
    }
}

/// §3.5.1's explicit colony states, verbatim:
/// `enum ColonyState { Nominal, Degraded { reason: DegradeReason },
/// Dormant, Abandoned }`. Discriminants append-only.
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColonyState {
    /// Running — the network produces, consumes, and schedules.
    Nominal = 0,
    /// Production halted (`reason`); solar/generator output and the
    /// life-support reserve drain continue (module doc).
    Degraded {
        /// Why the colony degraded.
        reason: DegradeReason,
    },
    /// Dead-crewed: consumes, produces, and schedules NOTHING — the
    /// event-budget mechanism for half-dead colonies (rule 5).
    Dormant = 2,
    /// Salvaged by `AbandonColony`; a terminal record.
    Abandoned = 3,
}

impl StableDiscriminant for ColonyState {
    fn discriminant(&self) -> u16 {
        match self {
            Self::Nominal => 0,
            Self::Degraded { .. } => 1,
            Self::Dormant => 2,
            Self::Abandoned => 3,
        }
    }
}

/// One installed module — the authored spec plus its damage flag.
/// Index in `Colony::modules` IS the `ModuleIndex` the allocation
/// pass's tie-break names.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColonyModule {
    /// The authored module — rates and capacities the network reads.
    pub def: ColonyModuleDef,
    /// `true` = inert (§3.5.1's crash damage). A damaged module
    /// produces, consumes, and stores nothing.
    pub damaged: bool,
}

/// One resource's linear store — the epoch-anchored reservoir (module
/// doc rule 4). `level(t)` is derived, never incremented.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColonyStore {
    /// The resource's level at `anchor_tick` (units), in `[0, cap]`.
    pub anchor_level: f64,
    /// The tick `anchor_level` is stamped at.
    pub anchor_tick: u64,
    /// Aggregate capacity — Σ of the colony's UNDAMAGED
    /// `Store`/`Battery` module capacities for this resource (units).
    /// Re-derived at every recompute ([`live_capacity`]): a damaged
    /// store holds nothing — its contents clamp away at the damage
    /// boundary — so `level_at`'s cap, `credit_end`'s headroom, and
    /// the `AbandonColony` salvage never read wreckage.
    pub capacity: f64,
    /// The interval's fixed net rate (units/s): granted production
    /// minus granted consumption. Negative drains, positive fills.
    pub net_rate_per_s: f64,
}

impl ColonyStore {
    /// `level(t)` — the verbatim piecewise-linear level (rule 4):
    /// `anchor + net·(t−anchor)/TICK_HZ`, clamped to `[0, cap]` (the
    /// bound event fires at `ceil(dt·HZ)`, a hair past the true
    /// crossing — the clamp is the linear model's own edge).
    pub fn level_at(&self, tick: u64) -> f64 {
        let dt_s = tick.saturating_sub(self.anchor_tick) as f64 / TICK_HZ as f64;
        (self.anchor_level + self.net_rate_per_s * dt_s).clamp(0.0, self.capacity)
    }
}

/// The colony arena record (§7.2's "colony stores, `ColonyState`,
/// flow-network levels"). Modules and stores are the authored network;
/// `daylight`, `module_fractions`, the store `net_rate`s, and `t_eq_k`
/// are the last solve's output — held constant over the interval and
/// rewritten only at boundaries (events/commands).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Colony {
    /// Its arena key — inline per §2.2's idiom.
    pub id: ColonyId,
    /// The site it occupies (`SiteDef` is construction config; the
    /// resolved body/pose below is the sim record).
    pub site: SiteId,
    /// The body it sits on — resolved at founding.
    pub body: BodyId,
    /// Geocentric latitude (rad).
    pub latitude_rad: f64,
    /// Longitude eastward from the frame +x (rad).
    pub longitude_rad: f64,
    /// Altitude above the body's datum radius (m) — the FOUNDING
    /// site's authored altitude, latched onto the record like
    /// `latitude_rad`/`longitude_rad`: the colony's surface point is
    /// one convention (`colony_env[body].radius_m + altitude_m`) read
    /// identically by the impact seam and `vessel_colony_linked`,
    /// never re-resolved through the (mutable) site-def table.
    /// `serde(default)` keeps pre-field saves loading at the old
    /// surface-altitude convention (0).
    #[serde(default)]
    pub altitude_m: f64,
    /// The §3.5.1 state machine slot.
    pub state: ColonyState,
    /// The module list — index = `ModuleIndex`.
    pub modules: Vec<ColonyModule>,
    /// Per-resource aggregate stores.
    pub stores: BTreeMap<ResourceId, ColonyStore>,
    /// The rule-1 insolation step.
    pub daylight: Daylight,
    /// Crew aboard — roster ids with `CrewLocation::Colony(id)`.
    pub crew: BTreeSet<CrewId>,
    /// Rule-2's held steady-state temperature — `None` = no radiating
    /// area (unbounded soak; never a non-finite record).
    pub t_eq_k: Option<f64>,
    /// The allocation pass's granted operating fraction per module —
    /// "a rate fixed for the interval" (rule 3). Parallel to `modules`.
    pub module_fractions: Vec<f64>,
    /// The tick the held solve was stamped at.
    pub solved_tick: u64,
    /// The authored next-inspection deadline (rule 4's "+ the next
    /// scheduled inspection"), set by
    /// `SimCommand::ScheduleColonyInspection`; consumed at firing.
    pub inspection_tick: Option<u64>,
}

// ---------------------------------------------------------------------------
// The light environment — construction config + the closed form
// ---------------------------------------------------------------------------

/// §3.5.1's per-body colony environment — data-derived construction
/// config (the `site_defs` precedent): handed in via
/// [`World::set_colony_env`], carried on `WorldConfig` so replays
/// rebuild it, never serialized or hashed as state.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColonyBodyEnv {
    /// Sidereal rotation period (s); `None` = tidally locked — the
    /// spin resolves to the body's own orbital mean motion (the
    /// `body.ron` convention: "the rotation period is the orbital
    /// period"). A moon locked to its PLANET still sees a solar day —
    /// the parent carries it around the star — so the synodic rate is
    /// `ω − n` of the star-orbiting ancestor (`body_light`), not the
    /// identically-0 fixed sun a star-locked planet gets.
    pub rotation_period_s: Option<f64>,
    /// Authored insolation at this body's orbit (W/m²) — the `S` in
    /// the tilt-0 daylit average `(2/π)·S·cos φ`.
    pub insolation_w_m2: Option<f64>,
    /// The body's datum radius (m) — M8-B: the `latched_pos` chord
    /// metric a `JoinColonyInventory` proximity check needs (the
    /// physics `Environment` only knows the dominant body's; colonies
    /// may sit on any body).
    pub radius_m: f64,
}

/// The `BodyDef` → env table conversion — the `from_balance`-style
/// helper callers building worlds from loaded systems use.
pub fn colony_env_from_body_defs(defs: &[BodyDef]) -> BTreeMap<BodyId, ColonyBodyEnv> {
    defs.iter()
        .map(|d| {
            (
                d.id.clone(),
                ColonyBodyEnv {
                    rotation_period_s: d.rotation_period_s,
                    insolation_w_m2: d.insolation_w_m2,
                    radius_m: d.radius_m,
                },
            )
        })
        .collect()
}

/// The colony's light field — re-derived per solve from the orbit
/// model and env table (all construction config, never state).
///
/// `H(t)` is the local solar hour angle: with tilt 0 the sun's
/// body-fixed longitude is `L(t) + π − ω·t` (the sun-direction
/// source's orbital phase minus the rotation angle — the
/// prime-meridian convention anchors the rotation angle at epoch 0),
/// so `H(t) = λ − (L0 + n·t) − π + ω·t = (λ − L0 − π) + (ω−n)·t`,
/// where `(n, L0)` name the orbit the sun direction tracks — the
/// body's own, or the star-orbiting ancestor's when the body is
/// tidally locked ([`sun_sweep`]).
/// Daylight is `cos H · cos φ > 0` — terminator at `H ≡ π/2 (mod π)`,
/// which is what [`next_terminator_tick`] solves.
#[derive(Clone, Copy, Debug)]
pub struct BodyLight {
    /// `ω − n_sun`: sub-solar longitude drift (rad/s) — the synodic
    /// rate. `0` = star-locked / rotationless: a fixed sun, no
    /// crossings.
    pub synodic_rate: f64,
    /// `λ_colony − L0 − π`: the hour-angle offset at epoch 0.
    pub h0: f64,
    /// Daylit-average insolation `(2/π)·S·cos φ` (W/m²) — rule 1's
    /// authored-average stand-in (tilt 0 closed form).
    pub day_insolation_w_m2: f64,
    /// `cos φ` — the pole test and the day predicate's second factor.
    pub cos_lat: f64,
}

/// Derive the light field for a colony pose — pure over the orbit
/// model's elements (`L0 = M0 + ω_arg + Ω_node`, `n = 2π/T`) and the
/// env table's rotation/insolation. Missing pieces default honestly:
/// no elements → `n = 0, L0 = 0` (the star itself — a fixed
/// inertial sun direction); `rotation_period_s: None` → tidally
/// locked to the PARENT (`ω` = the body's own orbital mean motion,
/// and the sun-direction sweep is the star-orbiting ancestor's — see
/// [`sun_sweep`]); no insolation → dark solar.
pub fn body_light(
    body: &BodyId,
    latitude_rad: f64,
    longitude_rad: f64,
    orbits: &PatchedConics,
    env: &ColonyBodyEnv,
) -> BodyLight {
    let (n, l0) = orbits
        .body_orbit(body)
        .map_or((0.0, 0.0), |o| orbit_motion(o.elements.as_ref()));
    let omega = env
        .rotation_period_s
        .map_or(n, |p| if p > 0.0 { TWO_PI / p } else { n });
    // A body locked to its parent is not locked to the STAR: the
    // sun's body-fixed longitude still sweeps at the parent's
    // heliocentric rate — Rask's solar day is the synodic month
    // (~141 ks, night ~70.5 ks), not the fixed sun the `omega − n`
    // self-difference used to give (synodic ≡ 0 ⇒ the authored
    // outpost read as permanently daylit, C09-F3/C34-F3). The scope
    // is the locked branch only: a rotating body's `n_sun` stays its
    // own orbital rate — a rotating MOON's sweep is equally the
    // parent's, but none is authored and the pre-fix path is kept.
    let locked = !env.rotation_period_s.is_some_and(|p| p > 0.0);
    let (n_sun, l0_sun) = if locked {
        sun_sweep(body, orbits)
    } else {
        (n, l0)
    };
    let cos_lat = libm::cos(latitude_rad);
    BodyLight {
        synodic_rate: omega - n_sun,
        h0: longitude_rad - l0_sun - PI,
        day_insolation_w_m2: env.insolation_w_m2.unwrap_or(0.0) * cos_lat * (2.0 / PI),
        cos_lat,
    }
}

/// `(n, L0)` of one orbit's elements — mean motion `2π/T` and epoch
/// longitude `M0 + ω_arg + Ω_node`. Missing or degenerate elements
/// read `(0, 0)` — a fixed inertial sun direction.
fn orbit_motion(e: Option<&csp_data::KeplerElements>) -> (f64, f64) {
    e.map_or((0.0, 0.0), |e| {
        let n = if e.period_s.is_finite() && e.period_s > 0.0 {
            TWO_PI / e.period_s
        } else {
            0.0
        };
        (
            n,
            e.mean_anomaly_epoch_rad + e.arg_periapsis_rad + e.ascending_node_rad,
        )
    })
}

/// The sun-direction sweep for a tidally-locked body: the `(n, L0)`
/// of the ancestor that orbits the star directly — the `parent`
/// chain walked while parents still carry orbit elements. A planet
/// locked to the star resolves to its own orbit (`n_sun = n` — the
/// genuinely fixed sun); a moon resolves to its planet's orbit, so
/// `synodic = n_moon − n_planet`, the synodic month. The walk is
/// bounded by the body count — parent chains are validator-proven
/// acyclic, and a table that is not resolves to the last ancestor
/// the walk reached rather than hanging.
fn sun_sweep(body: &BodyId, orbits: &PatchedConics) -> (f64, f64) {
    let mut current = body.clone();
    let mut motion = (0.0, 0.0);
    for _ in 0..orbits.bodies().len() {
        let Ok(b) = orbits.body_orbit(&current) else {
            return (0.0, 0.0);
        };
        motion = orbit_motion(b.elements.as_ref());
        match &b.parent {
            Some(parent)
                if orbits
                    .body_orbit(parent)
                    .is_ok_and(|p| p.elements.is_some()) =>
            {
                current = parent.clone();
            }
            _ => return motion,
        }
    }
    motion
}

/// `H(t)` wrapped to `(-π, π]`.
fn hour_angle(light: &BodyLight, tick: u64) -> f64 {
    let t_s = tick as f64 / TICK_HZ as f64;
    wrap_pi(light.h0 + light.synodic_rate * t_s)
}

/// Wrap to `(-π, π]` — `rem_euclid` into `[0, 2π)` shifted.
fn wrap_pi(a: f64) -> f64 {
    let w = a.rem_euclid(TWO_PI);
    if w > PI { w - TWO_PI } else { w }
}

/// The closed-form daylight at `tick` — `cos H · cos φ > 0` (rule 1's
/// two-state step). Poles read `Night` permanently (see
/// [`POLE_COS_EPS`]).
pub fn daylight_at(light: &BodyLight, tick: u64) -> Daylight {
    if libm::cos(hour_angle(light, tick)) * light.cos_lat > 0.0 {
        Daylight::Day
    } else {
        Daylight::Night
    }
}

/// The next `H ≡ π/2 (mod π)` crossing after `now` — the closed-form
/// terminator time (rule 1). Crossings are spaced `π/|synodic|`
/// (half a solar day); the tick is `now + ceil(dt·TICK_HZ)` — the
/// whole-tick bound, strictly ahead. `None` when the sun is fixed
/// (a star-locked or non-rotating body — a planet-locked moon still
/// crosses) and at the poles (a permanent terminator —
/// [`POLE_COS_EPS`]).
pub fn next_terminator_tick(light: &BodyLight, now: u64) -> Option<u64> {
    let syn = light.synodic_rate.abs();
    if syn < EPS || light.cos_lat.abs() < POLE_COS_EPS {
        return None;
    }
    let h_now = hour_angle(light, now);
    // x·dt ≡ (π/2 − h) mod π — one solution family spaced π/|x|; the
    // remainder is the least non-negative one; sitting exactly on the
    // boundary means the next is half a solar day out.
    let spacing = PI / syn;
    let dt = ((PI / 2.0 - h_now) / light.synodic_rate).rem_euclid(spacing);
    // Only an exact 0 reads "sitting on the boundary" — a positive dt
    // however small is a real crossing this interval (the `ceil` below
    // still lands it strictly ahead at the next tick); an EPS floor
    // here would skip a genuine crossing inside 1e-9 s.
    let dt = if dt > 0.0 { dt } else { spacing };
    Some(now.saturating_add((dt * TICK_HZ as f64).ceil() as u64))
}

// ---------------------------------------------------------------------------
// The network solve — §3.5.1 rules 1–4 in one pure function
// ---------------------------------------------------------------------------

/// One consumer's request on one resource — the allocation pass's
/// row. `priority` and `module` (the `ModuleIndex`) are the sort key.
#[derive(Clone, Copy, Debug)]
struct Consumer {
    /// ModuleIndex into `colony.modules`.
    module: usize,
    /// Rule-3 tie-break head — lower wins.
    priority: u8,
    /// Requested rate (units/s).
    request: f64,
}

/// Is this module inert under `state`/`damaged`? Rule 5: drills and
/// converters stop in every `Degraded` (and a damaged module is
/// inert in any state). Producers, stores, radiators, and life
/// support keep working — that is how a `PowerLost` colony re-charges
/// and how reserves drain.
fn module_inert(m: &ColonyModule, state: &ColonyState) -> bool {
    if m.damaged {
        return true;
    }
    matches!(state, ColonyState::Degraded { .. })
        && matches!(
            m.def,
            ColonyModuleDef::Drill { .. } | ColonyModuleDef::Converter { .. }
        )
}

/// The resource's live store capacity — Σ over UNDAMAGED
/// `Store`/`Battery` modules only (the `active`-filtered
/// `eps_sigma_a` discipline: a damaged module contributes nothing).
/// Re-derived at every recompute — never cached past a damage
/// boundary — so `credit_end`, the `level_at` cap, and the
/// `AbandonColony` salvage all read capacity net of wreckage: a
/// destroyed store's contents are gone with it (the clamp at the
/// damage recompute is the honest disposal), and a destroyed battery
/// accepts no further charge.
fn live_capacity(modules: &[ColonyModule], resource: &ResourceId) -> f64 {
    modules
        .iter()
        .filter(|m| !m.damaged)
        .map(|m| match &m.def {
            ColonyModuleDef::Store {
                resource: r,
                capacity,
            } if *r == *resource => *capacity,
            ColonyModuleDef::Battery { capacity } if resource.as_str() == ELECTRIC_CHARGE => {
                *capacity
            }
            _ => 0.0,
        })
        .sum()
}

/// One module's consumer registrations — `(resource, request)` pairs
/// plus the consumer's priority. Crewed `LifeSupport` scales per crew;
/// an uncrewed colony draws nothing from it. `recipes` resolves the
/// converter's `RecipeId` (M8-B); a missing recipe registers nothing —
/// `apply_found_colony` rejects unknown ids upstream.
///
/// Rows fold per resource: the grant key is `(module, resource)`, so
/// two raw requests on the same resource (a converter whose recipe
/// lists `electric_charge` in `inputs` AND a nonzero `power_ec_per_s`,
/// say) must merge into ONE summed row — a second row would overwrite
/// the first's grant and silently lose demand. Priorities are uniform
/// within a module; the `min` keeps the fold honest regardless.
/// `validate_colonies`/`colony_def_fields_finite` reject the authored
/// collision outright; the fold is the in-sim belt.
fn module_requests(
    m: &ColonyModule,
    crew: usize,
    recipes: &BTreeMap<RecipeId, RecipeDef>,
) -> Vec<(u8, ResourceId, f64)> {
    let mut rows: BTreeMap<ResourceId, (u8, f64)> = BTreeMap::new();
    {
        let mut push = |priority: u8, r: ResourceId, q: f64| {
            if q > 0.0 {
                let row = rows.entry(r).or_insert((priority, 0.0));
                row.0 = row.0.min(priority);
                row.1 += q;
            }
        };
        match &m.def {
            ColonyModuleDef::Drill { power_ec_per_s, .. } => {
                push(
                    PRIORITY_DEFAULT,
                    ResourceId::from(ELECTRIC_CHARGE),
                    *power_ec_per_s,
                );
            }
            ColonyModuleDef::Generator {
                fuel,
                fuel_rate_per_s,
                priority,
                ..
            } => {
                // M8-B: the fuel burn is a consumer row like any other
                // — a generator starved of fuel throttles to its
                // grant's fraction; `fuel_rate_per_s: 0` is the RTG
                // reading (no row at all).
                push(*priority, fuel.clone(), *fuel_rate_per_s);
            }
            ColonyModuleDef::Converter {
                recipe,
                priority,
                power_ec_per_s,
            } => {
                if let Some(rdef) = recipes.get(recipe) {
                    for (r, q) in &rdef.inputs {
                        push(*priority, r.clone(), *q);
                    }
                }
                push(
                    *priority,
                    ResourceId::from(ELECTRIC_CHARGE),
                    *power_ec_per_s,
                );
            }
            ColonyModuleDef::LifeSupport {
                consumes_per_crew,
                power_ec_per_s,
            } if crew > 0 => {
                for (r, q) in consumes_per_crew {
                    push(PRIORITY_LIFE_SUPPORT, r.clone(), q * crew as f64);
                }
                push(
                    PRIORITY_LIFE_SUPPORT,
                    ResourceId::from(ELECTRIC_CHARGE),
                    *power_ec_per_s,
                );
            }
            _ => {}
        }
    }
    rows.into_iter()
        .map(|(r, (priority, q))| (priority, r, q))
        .collect()
}

/// One module's full-rate productions — `(resource, units/s)` at
/// fraction 1.0. Solar keys on the daylight step (rule 1); the drill
/// scales its `base_rate_per_s` by the site's authored concentration
/// (`input.concentrations` — M8-B's `SiteDef::resource_yields`); the
/// converter's outputs come out of its recipe def. A damaged or
/// degraded-inert module produces nothing (the caller gates).
fn module_productions(m: &ColonyModule, input: &SolveInput) -> Vec<(ResourceId, f64)> {
    let ec = ResourceId::from(ELECTRIC_CHARGE);
    match &m.def {
        ColonyModuleDef::Solar {
            area_m2,
            efficiency,
        } => {
            let rate = match input.daylight {
                Daylight::Day => input.light.day_insolation_w_m2 * area_m2 * efficiency,
                Daylight::Night => 0.0,
            };
            if rate > 0.0 { vec![(ec, rate)] } else { vec![] }
        }
        ColonyModuleDef::Generator { rate_per_s, .. } => {
            if *rate_per_s > 0.0 {
                vec![(ec, *rate_per_s)]
            } else {
                vec![]
            }
        }
        ColonyModuleDef::Drill {
            resource,
            base_rate_per_s,
            min_concentration,
            ..
        } => {
            // M8-B: `base · min(1, c/m)` — the yield the site authors
            // for the resource, 0 when unlisted (barren ground drills
            // nothing). `min_concentration ≤ 0` degrades gracefully:
            // any nonzero yield runs at full rate.
            let conc = input.concentrations.get(resource).copied().unwrap_or(0.0);
            let frac = if *min_concentration > 0.0 {
                (conc / *min_concentration).clamp(0.0, 1.0)
            } else if conc > 0.0 {
                1.0
            } else {
                0.0
            };
            let rate = base_rate_per_s * frac;
            if rate > 0.0 {
                vec![(resource.clone(), rate)]
            } else {
                vec![]
            }
        }
        ColonyModuleDef::Converter { recipe, .. } => input
            .recipes
            .get(recipe)
            .map(|r| {
                r.outputs
                    .iter()
                    .filter(|(_, q)| *q > 0.0)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default(),
        _ => vec![],
    }
}

/// `Nominal`/`Degraded` — the states whose network still runs (the
/// transfer system's "is this colony reachable" gate; `Dormant` and
/// `Abandoned` are the terminal/frozen pair whose stores freeze).
pub(crate) fn colony_live(state: &ColonyState) -> bool {
    matches!(state, ColonyState::Nominal | ColonyState::Degraded { .. })
}

/// The terminal-store freeze the `Dormant`/`Abandoned` transitions
/// run (C09-F10 — was two hand-kept copies): "consumes, produces,
/// and schedules NOTHING" means the derived levels stop integrating
/// too — re-anchor every store at `now` and zero the net rates, or
/// `level_at` keeps integrating the last live rates and a later read
/// (the `AbandonColony` salvage scan) sees production that accrued
/// while the colony was dead.
fn freeze_colony_stores(colony: &mut Colony, now: u64) {
    for s in colony.stores.values_mut() {
        let level = s.level_at(now);
        s.anchor_level = level;
        s.anchor_tick = now;
        s.net_rate_per_s = 0.0;
    }
}

/// The solve's inputs — everything the network reads, snapshotted at
/// the boundary tick so the solve is a pure function of boundary
/// state.
struct SolveInput<'a> {
    /// The modules (index = ModuleIndex); inert ones contribute nothing.
    modules: &'a [ColonyModule],
    /// The state machine slot (degraded gating lives in `module_inert`).
    state: ColonyState,
    /// Store levels at the solve tick, per resource (already
    /// re-anchored by the caller).
    levels: &'a BTreeMap<ResourceId, f64>,
    /// Live store capacities, per resource — the sink half of
    /// `levels`: a resource has absorbable output headroom iff its
    /// store exists AND `level < capacity` (the caller keys `caps`
    /// on the same set `levels` keys on).
    caps: &'a BTreeMap<ResourceId, f64>,
    /// The light field + daylight step.
    light: &'a BodyLight,
    /// Crew aboard — scales `LifeSupport` requests.
    crew: usize,
    /// The daylight step for solar gating.
    daylight: Daylight,
    /// M8-B: the recipe table the `Converter` modules' `RecipeId`s
    /// resolve against (construction config — `set_recipes`).
    recipes: &'a BTreeMap<RecipeId, RecipeDef>,
    /// M8-B: the colony site's `SiteDef::resource_yields` resolved to
    /// a per-resource concentration map (construction config).
    concentrations: &'a BTreeMap<ResourceId, f64>,
    /// C09-F4's baseline crew demand — `(resource, units/s per crew
    /// member)`, the vessel-side `LifeSupportConfig::demands` set.
    /// Crew at a colony with NO `LifeSupport` module consume these
    /// from the colony's stores regardless of module presence — the
    /// consequence model that ends LS-less colonist free-riding.
    /// When a `LifeSupport` module exists (even a wrecked one) the
    /// module's own `consumes_per_crew` authors the whole demand and
    /// this set registers nothing — no double-counting.
    crew_demands: &'a [(ResourceId, f64)],
}

/// The solve-input fields [`World::colony_net_solve`] and
/// [`World::recompute_colony`] assemble identically (C09-F10 — was
/// two hand-kept copies; the firing-time re-check's correctness
/// depends on them staying identical). `levels`/`caps` stay with the
/// caller: the pure path reads them while the recompute re-derives
/// capacity and re-anchors first.
struct SolveFrame {
    /// The env-derived light field for the colony's body/site.
    light: BodyLight,
    /// The site's `resource_yields` resolved to concentrations.
    concentrations: BTreeMap<ResourceId, f64>,
    /// The life-support demand table (`LifeSupportConfig::demands`).
    crew_demands: [(ResourceId, f64); 3],
    /// Toggle-gated crew count — see [`SolveInput::crew`].
    crew: usize,
}

/// The solve's outputs — everything a recompute writes or schedules
/// from.
struct NetSolve {
    /// Granted operating fraction per module (0 for inert/damaged).
    fractions: Vec<f64>,
    /// Net store rate per resource (units/s, granted).
    net_rates: BTreeMap<ResourceId, f64>,
    /// Rule-2's algebraic `T_eq` — `None` with no radiating area.
    t_eq_k: Option<f64>,
    /// EC consumers demanded > 0 and were granted ~nothing — the
    /// `PowerLost` predicate.
    power_starved: bool,
    /// A life-support-required resource is empty and under-granted
    /// with crew aboard — the `LifeSupportDepleted` predicate.
    ls_depleted: bool,
}

/// The allocation pass's converged result — the grants the rule-3
/// sweep settled on, the module operating fractions they imply, and
/// the per-resource production at those fractions.
struct Allocation {
    /// `grant[(module, resource)]` — units/s actually served.
    grants: BTreeMap<(usize, ResourceId), f64>,
    /// Per-module operating fraction (0 for inert/damaged).
    fractions: Vec<f64>,
    /// `produced[resource]` — units/s at the converged fractions.
    produced: BTreeMap<ResourceId, f64>,
    /// `active[module]` — false for inert/damaged (the callers' gating
    /// checks reuse it).
    active: Vec<bool>,
    /// `pins[module]` — C09-F1: conservation-pinned by
    /// [`unanchored_mint_pins`] (a strict subset of the `!active`
    /// set: pinned modules are excluded both from `active` and from
    /// the latent-demand sets, unlike merely ungrounded ones).
    pins: Vec<bool>,
    /// C09-F4's baseline crew rows — `(priority, resource, request)`
    /// at the pseudo-module index `modules.len()` — non-empty only
    /// when crew stand at a `LifeSupport`-less colony AND the demand
    /// is grounded (stocked or produced like any module's request;
    /// an ungrounded baseline registers no consumer rows — the
    /// module precedent — while `solve_network`'s depletion scan
    /// still sees the would-be demand and kills the crew).
    baseline_reqs: Vec<(u8, ResourceId, f64)>,
    /// The baseline's granted fraction — `min(grant/request)` over
    /// `baseline_reqs` at the converged solve (0 when ungrounded).
    baseline_fraction: f64,
    /// The baseline demand set when ANY `LifeSupport` module is
    /// absent — populated even ungrounded, so `sinked` and
    /// `latent_ec_request` count crew needs like every other
    /// would-be consumer.
    baseline_demand: BTreeSet<ResourceId>,
}

/// The grounding closure (module doc rule 3): the least fixpoint of
/// "every requested resource is stocked (`level > eps`) or produced by
/// ANOTHER grounded module" — `j != i`, a module may never ground
/// itself. Monotone growth (each pass only grows the set) bounds the
/// closure at `n` passes; the least fixpoint is unique, so discovery
/// order affects only convergence speed — never the result.
fn grounded_modules(
    input: &SolveInput,
    viable: &[bool],
    requests: &[Vec<(u8, ResourceId, f64)>],
    outputs: &[BTreeSet<ResourceId>],
) -> Vec<bool> {
    let n = input.modules.len();
    let mut grounded = vec![false; n];
    for _ in 0..n.saturating_add(1) {
        let mut changed = false;
        for i in 0..n {
            if !viable[i] || grounded[i] {
                continue;
            }
            let fed = requests[i].iter().all(|(_, r, _)| {
                input.levels.get(r).copied().unwrap_or(0.0) > EPS
                    || (0..n).any(|j| j != i && grounded[j] && outputs[j].contains(r))
            });
            if fed {
                grounded[i] = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    grounded
}

/// The static per-module tables `allocation_pass` solves over —
/// built once from `input`; inert modules register nothing in any
/// field.
struct ModuleTables {
    /// `(priority, resource, rate)` consumer rows per module.
    requests: Vec<Vec<(u8, ResourceId, f64)>>,
    /// Full-rate `(resource, rate)` production rows per module.
    prods: Vec<Vec<(ResourceId, f64)>>,
    /// The production resource sets `prods` keys on.
    outputs: Vec<BTreeSet<ResourceId>>,
}

/// The tables' build — one pass over the module list per solve.
fn module_tables(input: &SolveInput, viable: &[bool]) -> ModuleTables {
    let requests: Vec<Vec<(u8, ResourceId, f64)>> = input
        .modules
        .iter()
        .enumerate()
        .map(|(i, m)| {
            if viable[i] {
                module_requests(m, input.crew, input.recipes)
            } else {
                Vec::new()
            }
        })
        .collect();
    let prods: Vec<Vec<(ResourceId, f64)>> = input
        .modules
        .iter()
        .enumerate()
        .map(|(i, m)| {
            if viable[i] {
                module_productions(m, input)
            } else {
                Vec::new()
            }
        })
        .collect();
    let outputs: Vec<BTreeSet<ResourceId>> = prods
        .iter()
        .map(|ps| ps.iter().map(|(r, _)| r.clone()).collect())
        .collect();
    ModuleTables {
        requests,
        prods,
        outputs,
    }
}

/// Rule-3's allocation pass — resources ascend `ResourceId`; within a
/// resource, consumers sort `(priority, ModuleIndex)` (the `ColonyId`
/// key is constant inside a colony); grants are full-rate until the
/// available rate — production plus stock — is exhausted, then the
/// remainder splits pro-rata by request over every consumer still
/// unfunded. Iterated to a monotone fixpoint: fractions start at 1.0
/// and only decrease (`min(prev, computed)`), capped at
/// `modules + 1` sweeps so cyclic chains still terminate
/// deterministically.
///
/// The grant iteration alone converges to the GREATEST fixpoint —
/// fraction 1 is self-consistent for a converter that eats its own
/// output, which would bootstrap production from nothing. So the pass
/// first computes the LEAST fixpoint of grounding
/// ([`grounded_modules`]). An ungrounded module pins at fraction 0 and
/// — just as important — registers no requests, so it cannot steal
/// grants from grounded consumers. Grounded modules then run the
/// greatest-fixpoint grant solve unchanged: the "run as much as the
/// network supports" reading, honest because every input traces back
/// to stock or a real source.
///
/// Between the two fixpoints sits the conservation pin
/// ([`unanchored_mint_pins`]): grounding's "stocked OR produced"
/// test lets a cyclic web anchor on founding stock alone, and a
/// sourceless cycle whose aggregate net rate is positive mints the
/// surplus forever at fraction 1. Pinning those SCC members out of
/// `active` restores "every input traces back to stock or a real
/// source" to also mean "no free surplus" (C09-F1).
#[allow(clippy::too_many_lines)] // the grounding → sweep → fixpoint → grant pipeline reads as one pass
fn allocation_pass(input: &SolveInput) -> Allocation {
    let n = input.modules.len();
    // `viable` = not inert under state/damage — a viable module may
    // operate at all.
    let viable: Vec<bool> = input
        .modules
        .iter()
        .map(|m| !module_inert(m, &input.state))
        .collect();
    // Static per-module request/production tables — inert modules
    // register neither. The grounding closure keys on the output
    // resource sets, the conservation pin sums the rates, and the
    // fixpoint sweeps scale `prods` by fraction.
    let ModuleTables {
        requests,
        prods,
        outputs,
    } = module_tables(input, &viable);
    // Only grounded modules run at all: they register the consumers
    // and produce in the grant solve; the rest pin at fraction 0.
    let grounded = grounded_modules(input, &viable, &requests, &outputs);
    let mut active: Vec<bool> = (0..n).map(|i| viable[i] && grounded[i]).collect();
    // C09-F1's conservation pin: grounding accepts "stocked OR
    // produced by a grounded module", so a cyclic converter web with
    // a positive aggregate net rate grounds on nothing but founding
    // stock and mints the surplus forever (the verifier's
    // `a 1.0→b 0.5` / `b 0.5→a 1.2` probe grew 10 units of `a` into
    // 130 in 600 s). Pin every member of an unanchored minting SCC
    // out of `active` before the grant sweep.
    let pins = unanchored_mint_pins(input, &active, &requests, &prods, &outputs);
    for (i, pinned) in pins.iter().enumerate() {
        if *pinned {
            active[i] = false;
        }
    }
    // Rule-3's sink side: a producing module whose EVERY output has
    // nowhere to go — no store with headroom (its entry exists and
    // `level < capacity − ε`) and no *other viable* module requesting
    // it — pins at fraction 0. Without this a converter whose recipe
    // outputs have no store runs full-tilt, drains its inputs, and
    // the product ceases to exist: `net_rates` computes the produced
    // rate but the write-back touches only `stores` entries, so a
    // storeless resource is silently voided. Refusing (fraction 0,
    // like a starved input) is the honest answer.
    //
    // The demand side reads LATENT demand — undamaged modules'
    // would-be request rows, the same set `latent_ec_request` counts
    // for `power_starved`: a consumer that is currently ungrounded,
    // or inert under a Degraded state, is still a legitimate sink
    // (once it can run it will take the output). Gating on `active`
    // would deadlock the restore path: in a Degraded colony every
    // consumer is inert, so a power producer would see "no sink" and
    // never produce the charge that earns `PowerRestored`. A
    // conservation-pinned module is the exception: it cannot run
    // under this web topology at all — not "once it can run" — so it
    // claims no sink and no producer stays sinked for it.
    let latent_requests: Vec<BTreeSet<ResourceId>> = input
        .modules
        .iter()
        .enumerate()
        .map(|(i, m)| {
            if m.damaged || pins[i] {
                BTreeSet::new()
            } else {
                module_requests(m, input.crew, input.recipes)
                    .into_iter()
                    .map(|(_, r, _)| r)
                    .collect()
            }
        })
        .collect();
    // C09-F4: the baseline crew demand — a `LifeSupport`-less colony's
    // crew still breathe, eat, and draw power from colony stores at
    // the vessel-side `life_support` rates. [`crew_baseline`] reads it
    // as LATENT demand (a sink for real producers and a
    // `power_starved` claim) even when ungrounded — the crew's need
    // does not vanish because it cannot currently be served.
    let (baseline_demand, baseline_reqs) = crew_baseline(input, &grounded, &outputs);
    let sinked: Vec<bool> = (0..n)
        .map(|i| {
            !active[i]
                || outputs[i].is_empty()
                || outputs[i].iter().any(|r| {
                    let store_headroom = input.caps.get(r).is_some_and(|cap| {
                        *cap > EPS && input.levels.get(r).copied().unwrap_or(0.0) < *cap - EPS
                    });
                    store_headroom
                        || baseline_demand.contains(r)
                        || (0..n).any(|j| j != i && latent_requests[j].contains(r))
                })
        })
        .collect();
    let mut consumers = consumer_map(&requests, &active);
    // Crew at PRIORITY_LIFE_SUPPORT sort WITH the module rows at the
    // same key — pseudo index `n` always loses the tie-break to a
    // real module (crew never outrank an installed LS module's own
    // rows, matching the depletion scan's module-first reading).
    for (priority, r, q) in &baseline_reqs {
        consumers.entry(r.clone()).or_default().push(Consumer {
            module: n,
            priority: *priority,
            request: *q,
        });
    }
    for list in consumers.values_mut() {
        list.sort_by_key(|c| (c.priority, c.module));
    }
    let mut fractions = vec![1.0f64; n];
    let mut grants: BTreeMap<(usize, ResourceId), f64>;
    let mut produced: BTreeMap<ResourceId, f64>;
    // Monotone fixpoint — each sweep's fractions are min(prev, computed).
    for _ in 0..n.saturating_add(1) {
        produced = production_at(&prods, &active, &fractions);
        grants = grant_map(&consumers, &produced, input.levels);
        // New fractions — min over the module's input grants; inert
        // modules pin at 0.
        let mut changed = false;
        for (i, reqs) in requests.iter().enumerate() {
            let f = if active[i] && sinked[i] {
                reqs.iter().fold(1.0f64, |f, (_, r, q)| {
                    let g = grants.get(&(i, r.clone())).copied().unwrap_or(0.0);
                    f.min(g / q)
                })
            } else {
                0.0
            };
            if f < fractions[i] {
                fractions[i] = f;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // The capped final sweep can exit mid-change: `grants`/`produced`
    // then hold the PRE-update iteration's table. Recompute both under
    // the converged fractions so the returned `Allocation` is always
    // one consistent solve (a clean break recomputes bitwise the same
    // table — the extra pass is idempotent).
    produced = production_at(&prods, &active, &fractions);
    grants = grant_map(&consumers, &produced, input.levels);
    // The baseline's fraction is the converged grant over its rows —
    // a pure read of the final table (it produces nothing, so nothing
    // about it feeds back into the fixpoint).
    let baseline_fraction = baseline_reqs.iter().fold(1.0f64, |f, (_, r, q)| {
        let g = grants.get(&(n, r.clone())).copied().unwrap_or(0.0);
        f.min(g / q)
    });
    Allocation {
        grants,
        fractions,
        produced,
        active,
        pins,
        baseline_reqs,
        baseline_fraction,
        baseline_demand,
    }
}

/// C09-F4's baseline crew demand — a `LifeSupport`-less colony's
/// crew consume `input.crew_demands` (the vessel-side rates, × crew)
/// from colony stores regardless of module presence. Returns
/// `(demand, reqs)` where `demand` is the LATENT resource set — the
/// sink/`power_starved` read — and `reqs` are the consumer rows
/// actually registered at pseudo-module index `modules.len()`,
/// non-empty only when the demand grounds on the modules' own rule
/// (every wanted resource stocked OR produced by a grounded module;
/// crew produce nothing, so the baseline can never feed a cycle and
/// needs no SCC pin).
fn crew_baseline(
    input: &SolveInput,
    grounded: &[bool],
    outputs: &[BTreeSet<ResourceId>],
) -> (BTreeSet<ResourceId>, Vec<(u8, ResourceId, f64)>) {
    let has_ls_module = input
        .modules
        .iter()
        .any(|m| matches!(m.def, ColonyModuleDef::LifeSupport { .. }));
    let mut demand = BTreeSet::new();
    let mut reqs = Vec::new();
    if input.crew == 0 || has_ls_module {
        return (demand, reqs);
    }
    let wants: Vec<(u8, ResourceId, f64)> = input
        .crew_demands
        .iter()
        .filter(|(_, q)| *q > 0.0)
        .map(|(r, q)| (PRIORITY_LIFE_SUPPORT, r.clone(), q * input.crew as f64))
        .collect();
    let grounded_baseline = !wants.is_empty()
        && wants.iter().all(|(_, r, _)| {
            input.levels.get(r).copied().unwrap_or(0.0) > EPS
                || (0..input.modules.len()).any(|j| grounded[j] && outputs[j].contains(r))
        });
    demand = wants.iter().map(|(_, r, _)| r.clone()).collect();
    if grounded_baseline {
        reqs = wants;
    }
    (demand, reqs)
}

/// The `LifeSupportDepleted` predicate — `true` when any required
/// life-support resource is both unstocked and under-granted. The
/// scan covers EVERY `LifeSupport` module — NOT just `active` ones:
/// a damaged or ungrounded module has no grant row (`g` reads 0),
/// so crew aboard deplete the moment a required store is empty —
/// damage kills. Stocked reserves still cover the request (the
/// "switches to reserves" reading). The scanned rows are the
/// module's WHOLE request set — `module_requests` again — so
/// `power_ec_per_s` counts as a required resource too: an unpowered
/// (ungrounded) module whose EC request cannot be served is
/// unsatisfied life support, and a stocked O2 reserve does not keep
/// its crew alive (the request rows are identical to what the
/// allocation pass registered — one source of truth for "what life
/// support needs"). C09-F4's baseline arm is the `LifeSupport`-less
/// twin: the demand rows the crew baseline *would* register,
/// grounded or not — the need is evidence either way.
fn ls_depletion_scan(
    input: &SolveInput,
    grants: &BTreeMap<(usize, ResourceId), f64>,
    baseline_demand: &BTreeSet<ResourceId>,
) -> bool {
    if input.crew == 0 {
        return false;
    }
    for (i, m) in input.modules.iter().enumerate() {
        if let ColonyModuleDef::LifeSupport { .. } = &m.def {
            for (_, r, req) in module_requests(m, input.crew, input.recipes) {
                let g = grants.get(&(i, r.clone())).copied().unwrap_or(0.0);
                let stock = input.levels.get(&r).copied().unwrap_or(0.0);
                if stock <= EPS && g < req - EPS {
                    return true;
                }
            }
        }
    }
    // The baseline twin — stock covers the request until the store
    // empties at its own boundary event; unstocked and under-granted
    // is starvation.
    for r in baseline_demand {
        let req = input
            .crew_demands
            .iter()
            .find(|(d, _)| d == r)
            .map(|(_, q)| *q)
            .unwrap_or(0.0)
            * input.crew as f64;
        let g = grants
            .get(&(input.modules.len(), r.clone()))
            .copied()
            .unwrap_or(0.0);
        let stock = input.levels.get(r).copied().unwrap_or(0.0);
        if stock <= EPS && g < req - EPS {
            return true;
        }
    }
    false
}

/// One sweep's grant table over `consumers` — the rule-3 fill: full
/// requests in `(priority, ModuleIndex)` order until the available
/// rate (production plus the stocked flag's infinite remainder)
/// exhausts, then the rest splits pro-rata by request. Factored out
/// of `allocation_pass`'s loop so the post-fixpoint grants recompute
/// under the same `produced` the caller returns (a capped final sweep
/// would otherwise leave `grants` one iteration stale).
fn grant_map(
    consumers: &BTreeMap<ResourceId, Vec<Consumer>>,
    produced: &BTreeMap<ResourceId, f64>,
    levels: &BTreeMap<ResourceId, f64>,
) -> BTreeMap<(usize, ResourceId), f64> {
    let mut grants: BTreeMap<(usize, ResourceId), f64> = BTreeMap::new();
    for (r, list) in consumers {
        let stock = levels.get(r).copied().unwrap_or(0.0);
        let mut remaining =
            produced.get(r).copied().unwrap_or(0.0) + if stock > EPS { f64::INFINITY } else { 0.0 };
        for (j, c) in list.iter().enumerate() {
            if remaining >= c.request {
                grants.insert((c.module, r.clone()), c.request);
                remaining -= c.request;
            } else {
                let total: f64 = list[j..].iter().map(|c| c.request).sum();
                if total > 0.0 {
                    let share = remaining / total;
                    for c in &list[j..] {
                        grants.insert((c.module, r.clone()), c.request * share);
                    }
                }
                break;
            }
        }
    }
    grants
}

/// C09-F1's conservation pin — the SCC check the grounding closure
/// cannot express. Grounding admits "stocked OR produced by a
/// grounded module", so a strongly connected component of producers
/// — ≥2 modules each feeding another's request, or a single module
/// requesting its own output — grounds whenever ANY member's
/// external input is merely stocked, and the grant solve then runs
/// the web at fraction 1 forever. When the SCC's aggregate net rate
/// (Σ full-rate outputs − Σ full-rate requests over its members)
/// is positive and no member is a Drill/Solar/Generator — the
/// sanctioned exogenous sources — the web mints its surplus out of
/// nothing: the verifier's `a 1.0→b 0.5` + `b 0.5→a 1.2` pair netted
/// +0.2 `a`/s from a 10-unit founding store (10 → 130 in 600 s).
/// Every member of such an SCC pins out of `active`.
///
/// Balanced or net-negative cycles are real conversion — their
/// surplus traces to stock or an external producer — and run. So do
/// source-membered webs: a Drill/Solar/Generator inside the SCC is
/// exactly where its positive net comes from (the
/// `grounded_cycle_still_runs` precedent).
///
/// The graph spans `active` modules only: a pinned/inert/ungrounded
/// module carries no flow, so no edge can route through it. Colony
/// module counts are small; the O(n²) adjacency plus per-source BFS
/// is trivial, and every scan iterates ascending `ModuleIndex` — the
/// SCC partition is exactly the deterministic order state-mutating
/// iteration requires.
fn unanchored_mint_pins(
    input: &SolveInput,
    active: &[bool],
    requests: &[Vec<(u8, ResourceId, f64)>],
    prods: &[Vec<(ResourceId, f64)>],
    outputs: &[BTreeSet<ResourceId>],
) -> Vec<bool> {
    let n = input.modules.len();
    // The request resource sets — an edge's demand endpoint.
    let wants: Vec<BTreeSet<ResourceId>> = requests
        .iter()
        .map(|rs| rs.iter().map(|(_, r, _)| r.clone()).collect())
        .collect();
    // `adjacent[i]` = the modules i feeds directly (i produces a
    // resource j requests), ascending — a self-edge never enters
    // here; singleton self-feeders are detected off `outputs`∩`wants`
    // at the SCC check below.
    let adjacent: Vec<Vec<usize>> = (0..n)
        .map(|i| {
            if !active[i] {
                Vec::new()
            } else {
                (0..n)
                    .filter(|&j| {
                        j != i && active[j] && outputs[i].iter().any(|r| wants[j].contains(r))
                    })
                    .collect()
            }
        })
        .collect();
    // `reach[i]` — everything i feeds transitively.
    let reach: Vec<BTreeSet<usize>> = (0..n)
        .map(|i| {
            let mut seen = BTreeSet::new();
            let mut stack: Vec<usize> = adjacent[i].clone();
            while let Some(j) = stack.pop() {
                if seen.insert(j) {
                    stack.extend(adjacent[j].iter().copied());
                }
            }
            seen
        })
        .collect();
    let mut pins = vec![false; n];
    let mut assigned = vec![false; n];
    for i in 0..n {
        if !active[i] || assigned[i] {
            continue;
        }
        // i's SCC: the mutually reachable members — `j` reaches `i`
        // AND `i` reaches `j` — in ascending module order.
        let members: Vec<usize> = (0..n)
            .filter(|&j| j == i || (reach[i].contains(&j) && reach[j].contains(&i)))
            .collect();
        for &j in &members {
            assigned[j] = true;
        }
        let self_loop = outputs[i].iter().any(|r| wants[i].contains(r));
        if members.len() < 2 && !self_loop {
            continue;
        }
        let sourced = members.iter().any(|&j| {
            matches!(
                input.modules[j].def,
                ColonyModuleDef::Drill { .. }
                    | ColonyModuleDef::Solar { .. }
                    | ColonyModuleDef::Generator { .. }
            )
        });
        if sourced {
            continue;
        }
        let net: f64 = members
            .iter()
            .map(|&j| {
                let produced: f64 = prods[j].iter().map(|(_, q)| *q).sum();
                let consumed: f64 = requests[j].iter().map(|(_, _, q)| *q).sum();
                produced - consumed
            })
            .sum();
        if net > EPS {
            for j in members {
                pins[j] = true;
            }
        }
    }
    pins
}

/// The per-resource consumer tables — rows only from `active`
/// modules, sorted `(priority, ModuleIndex)` (rule 3's ordering).
fn consumer_map(
    requests: &[Vec<(u8, ResourceId, f64)>],
    active: &[bool],
) -> BTreeMap<ResourceId, Vec<Consumer>> {
    let mut consumers: BTreeMap<ResourceId, Vec<Consumer>> = BTreeMap::new();
    for (i, reqs) in requests.iter().enumerate() {
        if !active[i] {
            continue;
        }
        for (priority, r, q) in reqs {
            consumers.entry(r.clone()).or_default().push(Consumer {
                module: i,
                priority: *priority,
                request: *q,
            });
        }
    }
    for list in consumers.values_mut() {
        list.sort_by_key(|c| (c.priority, c.module));
    }
    consumers
}

/// Aggregate production rates at a given fraction vector — one
/// accumulator shared by the fixpoint sweeps and the final recompute,
/// over the `prods` table `allocation_pass` already built.
fn production_at(
    prods: &[Vec<(ResourceId, f64)>],
    active: &[bool],
    fractions: &[f64],
) -> BTreeMap<ResourceId, f64> {
    let mut produced: BTreeMap<ResourceId, f64> = BTreeMap::new();
    for (i, ps) in prods.iter().enumerate() {
        if !active[i] {
            continue;
        }
        for (r, q) in ps {
            *produced.entry(r.clone()).or_insert(0.0) += q * fractions[i];
        }
    }
    produced
}

/// The §3.5.1 network solve — pure over `input` (module doc rules
/// 1–4). The allocation pass grounds the consumer set (least
/// fixpoint), then iterates the grants to their greatest fixpoint:
/// fractions start at 1.0 and only decrease, capped at `modules + 1`
/// sweeps so cyclic chains still terminate deterministically.
fn solve_network(input: &SolveInput) -> NetSolve {
    let alloc = allocation_pass(input);
    let Allocation {
        grants,
        fractions,
        produced,
        active,
        pins,
        baseline_reqs,
        baseline_fraction,
        baseline_demand,
    } = alloc;
    let mut net_rates: BTreeMap<ResourceId, f64> = BTreeMap::new();
    // Consumption is charged at `fraction · request`, NOT the raw
    // grant: a converter starved on one input runs at its binding
    // fraction and draws the other inputs at that fraction — the grant
    // alone would burn full power while the module sits idle.
    let mut consumed: BTreeMap<ResourceId, f64> = BTreeMap::new();
    for (i, m) in input.modules.iter().enumerate() {
        if !active[i] {
            continue;
        }
        for (_, r, q) in module_requests(m, input.crew, input.recipes) {
            *consumed.entry(r).or_insert(0.0) += q * fractions[i];
        }
    }
    // C09-F4: the baseline crew draw — charged at its granted
    // fraction like every module's `fraction · request` (a starved
    // crew draw consumes nothing, not its full request).
    for (_, r, q) in &baseline_reqs {
        *consumed.entry(r.clone()).or_insert(0.0) += q * baseline_fraction;
    }
    for r in produced.keys().chain(consumed.keys()) {
        net_rates.insert(
            r.clone(),
            produced.get(r).copied().unwrap_or(0.0) - consumed.get(r).copied().unwrap_or(0.0),
        );
    }
    // Rule 2: T_eq = (Q/(εσA))^¼ — sqrt twice, libm-only, never
    // integrated. Q_in is the interval's granted EC draw at the
    // converged fractions (1 EC/s = 1 W of waste heat — the
    // abstract-module convention).
    let q_in = consumed
        .get(&ResourceId::from(ELECTRIC_CHARGE))
        .copied()
        .unwrap_or(0.0);
    let eps_sigma_a: f64 = input
        .modules
        .iter()
        .enumerate()
        .filter(|(i, _)| active[*i])
        .map(|(_, m)| match &m.def {
            ColonyModuleDef::Radiator {
                area_m2,
                emissivity,
            } => emissivity * area_m2,
            _ => 0.0,
        })
        .sum();
    let t_eq_k = if eps_sigma_a > 0.0 {
        Some(libm::sqrt(libm::sqrt(
            q_in / (STEFAN_BOLTZMANN * eps_sigma_a),
        )))
    } else {
        None
    };
    // The transition predicates.
    let ec = ResourceId::from(ELECTRIC_CHARGE);
    // The *latent* EC demand — what the modules would draw ungated:
    // the Degraded inert gating must not feed back into the
    // starvation predicate, or `PowerLost` oscillates every tick
    // (its own remedy — converters stop — would read as the cure).
    // Damaged modules count nothing; conservation-pinned modules
    // likewise claim nothing — a web pinned dead by
    // `unanchored_mint_pins` cannot condemn the colony to a phantom
    // `PowerLost`. Everything else counts its would-be draw.
    let latent_ec_request: f64 = input
        .modules
        .iter()
        .enumerate()
        .filter(|(i, m)| !m.damaged && !pins[*i])
        .flat_map(|(_, m)| module_requests(m, input.crew, input.recipes))
        .filter(|(_, r, _)| *r == ec)
        .map(|(_, _, q)| q)
        .sum::<f64>()
        // C09-F4: the LS-less crew baseline's EC row is latent demand
        // too — an unpowered colony's crew still need charge, and a
        // dark battery-less base must earn `PowerLost`, not free-ride.
        + baseline_demand
            .iter()
            .filter(|r| **r == ec)
            .map(|r| {
                input
                    .crew_demands
                    .iter()
                    .find(|(d, _)| d == r)
                    .map(|(_, q)| *q)
                    .unwrap_or(0.0)
                    * input.crew as f64
            })
            .sum::<f64>();
    // `power_starved` compares a DEMAND RATE (units/s) against the
    // supply side in matching units — the produced rate AND the
    // buffered level are separate terms, never summed (a rate plus a
    // level is dimensionally meaningless even when the >0 test reads
    // the same).
    let ec_produced = produced.get(&ec).copied().unwrap_or(0.0);
    let ec_stock = input.levels.get(&ec).copied().unwrap_or(0.0);
    let power_starved = latent_ec_request > EPS && ec_produced <= EPS && ec_stock <= EPS;
    // The depletion predicate — [`ls_depletion_scan`] scans EVERY
    // LifeSupport module's whole request set (a damaged or ungrounded
    // module has no grant row, so a missing store kills) AND the
    // C09-F4 baseline arm: the demand rows an LS-less colony's crew
    // would register, grounded or not — starvation is evidence either
    // way, landing `LifeSupportDepleted` one tick out.
    let ls_depleted = ls_depletion_scan(input, &grants, &baseline_demand);
    NetSolve {
        fractions,
        net_rates,
        t_eq_k,
        power_starved,
        ls_depleted,
    }
}

// ---------------------------------------------------------------------------
// Next-boundary computation — §3.5.1 rule 4
// ---------------------------------------------------------------------------

/// The earliest boundary tick a live colony must publish — the rule-4
/// `min` (terminator, the earliest store bound, the authored
/// inspection), or `None` for a fully static colony (net-flat stores,
/// a fixed sun, no inspection): it honestly has no next event.
fn next_boundary(colony: &Colony, light: &BodyLight, now: u64) -> Option<u64> {
    let mut best: Option<u64> = None;
    let mut offer = |tick: u64| {
        if tick > now && best.is_none_or(|b| tick < b) {
            best = Some(tick);
        }
    };
    if let Some(t) = next_terminator_tick(light, now) {
        offer(t);
    }
    if let Some((tick, _, _)) = earliest_store_bound(colony, now) {
        offer(tick);
    }
    if let Some(t) = colony.inspection_tick {
        offer(t);
    }
    best
}

/// The single earliest store-bound — rule 4's `min` over resources of
/// `level/(−net)` emptying and `(cap−level)/net` filling. Ties break to
/// the lower `ResourceId` (the `BTreeMap` scan order IS that order).
///
/// The crossing tick is computed ANCHOR-RELATIVE — `anchor_tick +
/// ceil((bound_level − anchor_level)/net · HZ)` — never `anchor` plus
/// a from-`now` dt (that mixes origins and drifts the bound by
/// `now − anchor_tick`) and never `now` plus a recomputed dt (the
/// same crossing, but through the now-relative level it re-quantizes
/// at the float edge). Anchor-relative is *exactly* the crossing the
/// linear model names, so the published tick is bitwise-identical for
/// every query tick in `[anchor, bound)` — the partition-independence
/// the `Colonies` slot's publish relies on.
fn earliest_store_bound(colony: &Colony, now: u64) -> Option<(u64, ResourceId, bool)> {
    let mut best: Option<(u64, ResourceId, bool)> = None;
    for (r, s) in &colony.stores {
        let level = s.level_at(now);
        let bound = if s.net_rate_per_s < -EPS && level > EPS {
            let dt_s = s.anchor_level / -s.net_rate_per_s;
            Some((
                s.anchor_tick
                    .saturating_add((dt_s * TICK_HZ as f64).ceil() as u64),
                false,
            ))
        } else if s.net_rate_per_s > EPS && level < s.capacity - EPS {
            let dt_s = (s.capacity - s.anchor_level) / s.net_rate_per_s;
            Some((
                s.anchor_tick
                    .saturating_add((dt_s * TICK_HZ as f64).ceil() as u64),
                true,
            ))
        } else {
            None
        };
        if let Some((tick, full)) = bound
            && tick > now
            && best.as_ref().is_none_or(|(t, _, _)| tick < *t)
        {
            best = Some((tick, r.clone(), full));
        }
    }
    best
}

/// Does this event kind carry a colony boundary (the scrub set —
/// terminator, store-bound, inspection)? Notifications are
/// deliberately NOT boundary events: they are committed transitions.
fn is_colony_boundary(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::TerminatorCrossing { .. }
            | EventKind::ColonyStoreFull { .. }
            | EventKind::ColonyStoreEmpty { .. }
            | EventKind::ColonyInspection { .. }
    )
}

/// The colony any colony event names (`None` for non-colony kinds).
fn event_colony(kind: &EventKind) -> Option<ColonyId> {
    match kind {
        EventKind::TerminatorCrossing { colony }
        | EventKind::ColonyStoreFull { colony, .. }
        | EventKind::ColonyStoreEmpty { colony, .. }
        | EventKind::ColonyInspection { colony }
        | EventKind::PowerLost { colony }
        | EventKind::PowerRestored { colony }
        | EventKind::LifeSupportDepleted { colony } => Some(*colony),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Hash + shadow (§7.1, §2.13)
// ---------------------------------------------------------------------------

/// The world-level colony-arena walk — appended to `state_hash`
/// (the `hash_economy` precedent). Ordering is the schema.
pub fn hash_colonies(h: &mut StateHasher, colonies: &BTreeMap<ColonyId, Colony>) {
    h.arena(colonies.iter().map(|(id, c)| (id.0, c)), |h, c| {
        h.str(c.site.as_str());
        h.str(c.body.as_str());
        h.f64(c.latitude_rad);
        h.f64(c.longitude_rad);
        h.f64(c.altitude_m);
        h.stable_enum(&c.state);
        if let ColonyState::Degraded { reason } = &c.state {
            h.stable_enum(reason);
        }
        h.stable_enum(&c.daylight);
        h.u32(c.modules.len() as u32);
        for m in &c.modules {
            h.stable_enum(&m.def);
            hash_module_def(h, &m.def);
            h.bool(m.damaged);
        }
        h.btree_map(
            &c.stores,
            |h, r| {
                h.str(r.as_str());
            },
            |h, s| {
                h.f64(s.anchor_level);
                h.u64(s.anchor_tick);
                h.f64(s.capacity);
                h.f64(s.net_rate_per_s);
            },
        );
        h.u32(c.crew.len() as u32);
        for id in &c.crew {
            h.u64(id.0);
        }
        match c.t_eq_k {
            Some(t) => {
                h.bool(true);
                h.f64(t);
            }
            None => {
                h.bool(false);
            }
        }
        h.u32(c.module_fractions.len() as u32);
        for f in &c.module_fractions {
            h.f64(*f);
        }
        h.u64(c.solved_tick);
        match c.inspection_tick {
            Some(t) => {
                h.bool(true);
                h.u64(t);
            }
            None => {
                h.bool(false);
            }
        }
    });
}

/// The per-variant field walk for a `ColonyModuleDef` — the
/// discriminant is already written by the caller.
fn hash_module_def(h: &mut StateHasher, def: &ColonyModuleDef) {
    let pair = |h: &mut StateHasher, list: &Vec<(ResourceId, f64)>| {
        h.u32(list.len() as u32);
        for (r, q) in list {
            h.str(r.as_str());
            h.f64(*q);
        }
    };
    match def {
        ColonyModuleDef::Solar {
            area_m2,
            efficiency,
        } => {
            h.f64(*area_m2);
            h.f64(*efficiency);
        }
        ColonyModuleDef::Generator {
            fuel,
            fuel_rate_per_s,
            rate_per_s,
            priority,
        } => {
            h.str(fuel.as_str());
            h.f64(*fuel_rate_per_s);
            h.f64(*rate_per_s);
            h.u8(*priority);
        }
        ColonyModuleDef::Drill {
            resource,
            base_rate_per_s,
            min_concentration,
            power_ec_per_s,
        } => {
            h.str(resource.as_str());
            h.f64(*base_rate_per_s);
            h.f64(*min_concentration);
            h.f64(*power_ec_per_s);
        }
        ColonyModuleDef::Converter {
            recipe,
            priority,
            power_ec_per_s,
        } => {
            h.str(recipe.as_str());
            h.u8(*priority);
            h.f64(*power_ec_per_s);
        }
        ColonyModuleDef::Store { resource, capacity } => {
            h.str(resource.as_str());
            h.f64(*capacity);
        }
        ColonyModuleDef::Battery { capacity } => {
            h.f64(*capacity);
        }
        ColonyModuleDef::Radiator {
            area_m2,
            emissivity,
        } => {
            h.f64(*area_m2);
            h.f64(*emissivity);
        }
        ColonyModuleDef::LifeSupport {
            consumes_per_crew,
            power_ec_per_s,
        } => {
            pair(h, consumes_per_crew);
            h.f64(*power_ec_per_s);
        }
    }
}

/// Lazy-shadow capture for the colony arena — the
/// `shadow_economy`/`shadow_science` precedent: one clone per tick
/// that mutates it; `next_colony_id` travels with it.
pub(crate) fn shadow_colonies(
    colonies: &BTreeMap<ColonyId, Colony>,
    next_id: u64,
    slot: &mut Option<BTreeMap<ColonyId, Colony>>,
    next_slot: &mut Option<u64>,
) {
    if slot.is_none() {
        *slot = Some(colonies.clone());
        *next_slot = Some(next_id);
    }
}

/// The per-module numeric rules shared by the command-side
/// [`colony_def_fields_finite`] gate and `World::restore`'s crafted-
/// snapshot gate — every authored rate, capacity, and efficiency/
/// emissivity band finite and non-negative. A `Converter`'s recipe is
/// deliberately NOT resolved here: the recipe table is construction
/// config (re-installed post-restore), so only `power_ec_per_s` is
/// checkable at the serialization boundary — the recipe resolution
/// stays with the command-side gate.
pub(crate) fn colony_module_def_fields_finite(m: &ColonyModuleDef) -> bool {
    let nonneg = |v: f64| v.is_finite() && v >= 0.0;
    let pairs = |l: &[(ResourceId, f64)]| l.iter().all(|(_, q)| nonneg(*q));
    match m {
        ColonyModuleDef::Solar {
            area_m2,
            efficiency,
        } => nonneg(*area_m2) && (0.0..=1.0).contains(efficiency),
        ColonyModuleDef::Generator {
            fuel_rate_per_s,
            rate_per_s,
            ..
        } => nonneg(*fuel_rate_per_s) && nonneg(*rate_per_s),
        ColonyModuleDef::Drill {
            base_rate_per_s,
            min_concentration,
            power_ec_per_s,
            ..
        } => nonneg(*base_rate_per_s) && nonneg(*min_concentration) && nonneg(*power_ec_per_s),
        ColonyModuleDef::Converter { power_ec_per_s, .. } => nonneg(*power_ec_per_s),
        ColonyModuleDef::Store { capacity, .. } | ColonyModuleDef::Battery { capacity } => {
            nonneg(*capacity)
        }
        ColonyModuleDef::Radiator {
            area_m2,
            emissivity,
        } => nonneg(*area_m2) && (0.0..=1.0).contains(emissivity),
        ColonyModuleDef::LifeSupport {
            consumes_per_crew,
            power_ec_per_s,
        } => pairs(consumes_per_crew) && nonneg(*power_ec_per_s),
    }
}

/// Command-side sanity for a `ColonyDef` — every authored rate,
/// capacity, efficiency band, and store level finite and
/// non-negative (data-side rules live in `validate_colonies`; a
/// `FoundColony` carries the def inside the command, so the apply
/// re-checks before it can write NaN into hashed state).
fn colony_def_fields_finite(def: &ColonyDef, recipes: &BTreeMap<RecipeId, RecipeDef>) -> bool {
    let nonneg = |v: f64| v.is_finite() && v >= 0.0;
    let pairs = |l: &[(ResourceId, f64)]| l.iter().all(|(_, q)| nonneg(*q));
    // An `electric_charge` entry in a `consumes_per_crew` list or a
    // recipe's `inputs` plus a nonzero `power_ec_per_s` is ONE EC draw
    // authored twice — the (module, resource) request collision
    // `validate_colonies` rejects data-side.
    let ec = ResourceId::from(ELECTRIC_CHARGE);
    let ec_collision = |list: &[(ResourceId, f64)], power: f64| {
        power > 0.0 && list.iter().any(|(r, q)| *q > 0.0 && *r == ec)
    };
    def.modules.iter().all(|m| {
        colony_module_def_fields_finite(m)
            && match m {
                ColonyModuleDef::Converter {
                    recipe,
                    power_ec_per_s,
                    ..
                } => {
                    // M8-B: the recipe must resolve, and its own rates
                    // must be finite non-negative — a missing recipe is
                    // a command-side rejection, not a silent inert
                    // module.
                    recipes.get(recipe).is_some_and(|r| {
                        pairs(&r.inputs)
                            && pairs(&r.outputs)
                            && !ec_collision(&r.inputs, *power_ec_per_s)
                    })
                }
                ColonyModuleDef::LifeSupport {
                    consumes_per_crew,
                    power_ec_per_s,
                } => !ec_collision(consumes_per_crew, *power_ec_per_s),
                _ => true,
            }
    }) && def.stores.iter().all(|s| nonneg(s.level))
}

// ---------------------------------------------------------------------------
// World impl — commands, events, the recompute, the publish
// ---------------------------------------------------------------------------

impl World {
    /// The colony arena — observation only (tests, the harness, UI).
    pub fn colonies(&self) -> &BTreeMap<ColonyId, Colony> {
        &self.colonies
    }

    /// The installed colony body environments (construction config).
    pub fn colony_env(&self) -> &BTreeMap<BodyId, ColonyBodyEnv> {
        &self.colony_env
    }

    /// Install the per-body colony environments — construction config
    /// (the `set_sites` precedent: data-derived, handed in, never
    /// serialized or hashed; `WorldConfig` carries it so §7.6 replays
    /// rebuild it).
    pub fn set_colony_env(&mut self, env: BTreeMap<BodyId, ColonyBodyEnv>) {
        self.colony_env = env;
    }

    /// The recipe table — construction config (the `set_experiments`
    /// precedent: data-derived, handed in, never serialized or hashed;
    /// `WorldConfig` carries it so replays rebuild it). `Converter`
    /// modules' `RecipeId`s resolve against it at every solve.
    pub fn set_recipes(&mut self, recipes: BTreeMap<RecipeId, RecipeDef>) {
        self.recipes = recipes;
    }

    /// The installed recipe table (construction config).
    pub fn recipes(&self) -> &BTreeMap<RecipeId, RecipeDef> {
        &self.recipes
    }

    /// Found `data/systems/<sys>/colonies.ron`'s authored starting
    /// colonies — a pre-run write like `set_save_seed`: each def goes
    /// through the same `apply_found_colony` path the `FoundColony`
    /// command drains to, at the current clock (tick 0 in every real
    /// caller), so the colony records and their published deadlines
    /// are part of the baseline snapshot and never enter the command
    /// log. Atomic: a rejected def restores the pre-call world.
    /// Requires `set_sites`, `set_colony_env`, and `set_recipes` to
    /// have run first — the founding apply resolves all three.
    pub fn found_authored_colonies(&mut self, defs: &[ColonyDef]) -> Result<(), SimError> {
        let mut shadow = TickShadow::capture(self);
        for def in defs {
            if let Err(e) = self.apply_found_colony_impl(def, false, &mut shadow) {
                shadow.restore(self);
                return Err(e);
            }
        }
        Ok(())
    }

    /// The lazy-shadow twin for the colony arena.
    pub(crate) fn shadow_colonies(&mut self, shadow: &mut TickShadow) {
        shadow_colonies(
            &self.colonies,
            self.next_colony_id,
            &mut shadow.colonies,
            &mut shadow.next_colony_id,
        );
    }

    /// Drop every pending event naming `colony` in `boundary_only`'s
    /// set — or all of them when `boundary_only` is false (the
    /// `DeleteManeuver` re-heapify precedent). Boundary events are
    /// republished each recompute; notifications are dropped only
    /// when the colony leaves the live states entirely.
    fn drop_colony_events(
        &mut self,
        colony: ColonyId,
        boundary_only: bool,
        shadow: &mut TickShadow,
    ) {
        self.scrub_event_heap(shadow, |e| {
            event_colony(&e.kind) != Some(colony) || (boundary_only && !is_colony_boundary(&e.kind))
        });
    }

    /// `FoundColony` — the command path: the authored founding cost
    /// is charged, then the shared build runs (see
    /// [`Self::apply_found_colony_impl`]).
    pub(crate) fn apply_found_colony(
        &mut self,
        def: &ColonyDef,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        self.apply_found_colony_impl(def, true, shadow)
    }

    /// `FoundColony` — place `def` at its site: resolve the site's
    /// body/pose, build the aggregate stores (Store/Battery caps +
    /// founding levels), install the module list, and run the
    /// founding solve — daylight, allocation, terminator, and the
    /// first published deadlines. `Recoverable` rejections: unknown
    /// site, no colony env for the site body, non-finite or negative
    /// rates/levels, a live colony already occupying the site
    /// (C09-F9), or — on the command path — funds short of the
    /// authored `colony_founding_cost_funds` (C21-F8: §3.5's
    /// "funds ─► facilities" arrow — `AbandonColony` already salvages
    /// store contents, so a free found would mint site inventory).
    /// `charge_cost` is `false` only for `found_authored_colonies`'s
    /// world-bootstrap writes — authored starting colonies are world
    /// state, never a purchase.
    #[allow(clippy::too_many_lines)] // a sequential validate → build → solve pipeline
    fn apply_found_colony_impl(
        &mut self,
        def: &ColonyDef,
        charge_cost: bool,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let Some(site) = self.site_defs.get(&def.site) else {
            return Err(SimError::Recoverable("found colony: unknown site"));
        };
        // Snapshot the resolved pose — `site` borrows the config
        // table, which the arena write below must not hold.
        let (site_body, site_lat, site_lon, site_alt) = (
            site.body.clone(),
            site.latitude_rad,
            site.longitude_rad,
            site.altitude_m,
        );
        let Some(env) = self.colony_env.get(&site_body).copied() else {
            return Err(SimError::Recoverable(
                "found colony: no colony environment for site body",
            ));
        };
        // C09-F9: one live colony per site — the apply-side occupancy
        // check the UI's `has_colony` gate mirrors (the raw command
        // path double-founded freely before this). `Abandoned` frees
        // the site for re-founding — the abandon-recovery economics
        // already price the wreck's salvage into the site inventory;
        // `Dormant`/`Degraded`/`Nominal` all still occupy it.
        if self
            .colonies
            .values()
            .any(|c| c.site == def.site && !matches!(c.state, ColonyState::Abandoned))
        {
            return Err(SimError::Recoverable(
                "found colony: site already hosts a live colony",
            ));
        }
        if !(site_lat.is_finite()
            && site_lon.is_finite()
            && site_alt.is_finite()
            && site_alt >= 0.0)
        {
            return Err(SimError::Recoverable("found colony: non-finite site pose"));
        }
        if !colony_def_fields_finite(def, &self.recipes) {
            return Err(SimError::Recoverable(
                "found colony: non-finite field or unknown recipe",
            ));
        }
        let modules = def
            .modules
            .iter()
            .map(|d| ColonyModule {
                def: d.clone(),
                damaged: false,
            })
            .collect::<Vec<_>>();
        // Aggregate capacities; founding levels clamp to them.
        let mut stores: BTreeMap<ResourceId, ColonyStore> = BTreeMap::new();
        for m in def.modules.iter() {
            match m {
                ColonyModuleDef::Store { resource, capacity } => {
                    stores
                        .entry(resource.clone())
                        .or_insert(ColonyStore {
                            anchor_level: 0.0,
                            anchor_tick: self.clock.ticks(),
                            capacity: 0.0,
                            net_rate_per_s: 0.0,
                        })
                        .capacity += capacity;
                }
                ColonyModuleDef::Battery { capacity } => {
                    stores
                        .entry(ResourceId::from(ELECTRIC_CHARGE))
                        .or_insert(ColonyStore {
                            anchor_level: 0.0,
                            anchor_tick: self.clock.ticks(),
                            capacity: 0.0,
                            net_rate_per_s: 0.0,
                        })
                        .capacity += capacity;
                }
                _ => {}
            }
        }
        for s in &def.stores {
            if !(s.level.is_finite() && s.level >= 0.0) {
                return Err(SimError::Recoverable("found colony: bad store level"));
            }
            let store = stores.entry(s.resource.clone()).or_insert(ColonyStore {
                anchor_level: 0.0,
                anchor_tick: self.clock.ticks(),
                capacity: 0.0,
                net_rate_per_s: 0.0,
            });
            store.anchor_level = s.level.min(store.capacity);
        }
        for s in stores.values() {
            if !(s.capacity.is_finite() && s.capacity >= 0.0) {
                return Err(SimError::Recoverable("found colony: bad store capacity"));
            }
        }
        if charge_cost {
            let cost = self.economy_cfg.colony_founding_cost_micros;
            if cost > self.economy.funds_micros {
                return Err(SimError::Recoverable("found colony: insufficient funds"));
            }
            self.shadow_economy(shadow);
            crate::economy::charge(
                &mut self.economy,
                self.clock.ticks(),
                cost,
                crate::economy::FundsReason::ColonyFounding,
            );
        }
        self.shadow_colonies(shadow);
        let id = ColonyId(self.next_colony_id);
        self.next_colony_id = self
            .next_colony_id
            .checked_add(1)
            .ok_or(SimError::Fatal("colony id counter overflow"))?;
        let light = body_light(&site_body, site_lat, site_lon, &self.orbits, &env);
        let colony = Colony {
            id,
            site: def.site.clone(),
            body: site_body,
            latitude_rad: site_lat,
            longitude_rad: site_lon,
            altitude_m: site_alt,
            state: ColonyState::Nominal,
            modules,
            stores,
            daylight: daylight_at(&light, self.clock.ticks()),
            crew: BTreeSet::new(),
            t_eq_k: None,
            module_fractions: Vec::new(),
            solved_tick: self.clock.ticks(),
            inspection_tick: None,
        };
        self.colonies.insert(id, colony);
        self.recompute_colony(id, self.clock.ticks(), shadow)
    }

    /// `AbandonColony` — §3.5.1 rule 5: salvage the store contents at
    /// `abandon_recovery` into the site's inventory, bring crew home
    /// `Unassigned`, mark `Abandoned`, and drop every pending event —
    /// the terminal record.
    pub(crate) fn apply_abandon_colony(
        &mut self,
        id: ColonyId,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let Some(colony) = self.colonies.get(&id) else {
            return Err(SimError::Recoverable("abandon: unknown colony"));
        };
        if matches!(colony.state, ColonyState::Abandoned) {
            return Err(SimError::Recoverable("abandon: colony already abandoned"));
        }
        let salvage: Vec<(ResourceId, f64)> = colony
            .stores
            .iter()
            .map(|(r, s)| {
                (
                    r.clone(),
                    s.level_at(self.clock.ticks()) * self.economy_cfg.abandon_recovery,
                )
            })
            .filter(|(_, u)| *u > 0.0)
            .collect();
        let site = colony.site.clone();
        let crew: Vec<CrewId> = colony.crew.iter().copied().collect();
        self.shadow_colonies(shadow);
        self.shadow_economy(shadow);
        if let Some(st) = self.economy.sites.get_mut(&site) {
            for (r, u) in salvage {
                *st.inventory.entry(r).or_insert(0.0) += u;
            }
        }
        self.unhook_colony_crew(id, &crew, false);
        // C09-F8: the terminal transition vacates every joined
        // vessel's inventory opt-in — a `joined_colony` naming a dead
        // colony is a stale tag (serialized, and it reads as "COLONY"
        // on a recovered vessel's crew row) that only the manual
        // `LeaveColonyInventory` could clear before.
        self.shadow_vessels(shadow);
        for v in self.vessels.values_mut() {
            if v.joined_colony == Some(id) {
                v.joined_colony = None;
            }
        }
        let colony = self
            .colonies
            .get_mut(&id)
            .ok_or(SimError::Fatal("abandon: colony vanished"))?;
        colony.state = ColonyState::Abandoned;
        colony.crew.clear();
        // Terminal records freeze too — the salvage was already
        // computed off the pre-freeze levels.
        freeze_colony_stores(colony, self.clock.ticks());
        self.drop_colony_events(id, false, shadow);
        Ok(())
    }

    /// C09-F4's posting precondition — a live colony can host crew
    /// iff the subsystem is off (no demand runs at all) or SOME
    /// `LifeSupport` module stands undamaged. `apply_assign_crew` and
    /// `apply_transfer_colony_crew`'s `to_colony` arm share it.
    pub(crate) fn colony_accepts_crew(&self, colony: &Colony) -> bool {
        !self.life_support_enabled
            || colony
                .modules
                .iter()
                .any(|m| !m.damaged && matches!(m.def, ColonyModuleDef::LifeSupport { .. }))
    }

    /// `AssignCrewToColony` — post a roster member at the colony. The
    /// member must be `Unassigned` (boarding a colony off a vessel
    /// runs through `SimCommand::TransferColonyCrew`, M8-B's
    /// proximity transfer); the colony
    /// must be live (`Nominal`/`Degraded` — dormant and abandoned
    /// colonies take no crew). The recompute follows: life-support
    /// demand scales with the headcount.
    pub(crate) fn apply_assign_crew(
        &mut self,
        id: ColonyId,
        crew: CrewId,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let Some(colony) = self.colonies.get(&id) else {
            return Err(SimError::Recoverable("assign crew: unknown colony"));
        };
        if !matches!(
            colony.state,
            ColonyState::Nominal | ColonyState::Degraded { .. }
        ) {
            return Err(SimError::Recoverable("assign crew: colony not live"));
        }
        let Some(member) = self.economy.crew.get(&crew) else {
            return Err(SimError::Recoverable("assign crew: unknown crew member"));
        };
        if member.location != crate::economy::CrewLocation::Unassigned {
            return Err(SimError::Recoverable("assign crew: member not unassigned"));
        }
        if member.status != CrewStatus::Active {
            return Err(SimError::Recoverable("assign crew: member not active"));
        }
        // C09-F4: posting to an LS-less colony is refused while life
        // support is enabled — the baseline consumption in the solve
        // would starve the member on a timer (the F4 probe's
        // immortal-crew reading was the bug; refusing is the honest
        // gate — `TransferColonyCrew` checks the same). A colony with
        // a working module takes crew; a disabled subsystem takes no
        // demand and the gate lifts with it.
        if !self.colony_accepts_crew(colony) {
            return Err(SimError::Recoverable(
                "assign crew: colony has no working life-support module",
            ));
        }
        self.shadow_colonies(shadow);
        self.shadow_economy(shadow);
        self.economy
            .crew
            .get_mut(&crew)
            .ok_or(SimError::Fatal("assign crew: member vanished"))?
            .location = crate::economy::CrewLocation::Colony(id);
        self.colonies
            .get_mut(&id)
            .ok_or(SimError::Fatal("assign crew: colony vanished"))?
            .crew
            .insert(crew);
        self.recompute_colony(id, self.clock.ticks(), shadow)?;
        // 7.3 §3.6: a direct posting is the `CrewTransport`
        // predicate's trigger — same fact the `land_crew` landing
        // fires.
        self.contract_notify(&crate::contracts::ContractTrigger::CrewPosted, shadow)
    }

    /// `EvacuateColony` — every crew member aboard goes `Unassigned`;
    /// the recompute drops the life-support demand. Live colonies
    /// only — a dormant colony holds no crew by construction.
    pub(crate) fn apply_evacuate_colony(
        &mut self,
        id: ColonyId,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let Some(colony) = self.colonies.get(&id) else {
            return Err(SimError::Recoverable("evacuate: unknown colony"));
        };
        if matches!(colony.state, ColonyState::Abandoned) {
            return Err(SimError::Recoverable("evacuate: colony abandoned"));
        }
        let crew: Vec<CrewId> = colony.crew.iter().copied().collect();
        self.shadow_colonies(shadow);
        self.shadow_economy(shadow);
        self.unhook_colony_crew(id, &crew, false);
        self.colonies
            .get_mut(&id)
            .ok_or(SimError::Fatal("evacuate: colony vanished"))?
            .crew
            .clear();
        if matches!(
            self.colonies.get(&id).map(|c| &c.state),
            Some(ColonyState::Dormant)
        ) {
            return Ok(());
        }
        self.recompute_colony(id, self.clock.ticks(), shadow)
    }

    /// `DamageColonyModule` — §3.5.1's crash-damage stand-in: mark a
    /// module inert and drop the colony to `Degraded{ImpactDamage}`
    /// (M8-C's `apply_surface_impacts` is the real in-play driver;
    /// this command exercises the same bookkeeping in tests). A
    /// dormant or abandoned colony takes no new damage —
    /// it is already inert by state. The repair path is
    /// [`Self::apply_repair_module`] (C09-F6); a
    /// `Degraded{PowerLost}` colony keeps its existing reason
    /// rather than switching. Every landed hit posts a
    /// `ColonyDamaged` notification.
    /// `tick` is the tick the damage lands at — the recompute anchors
    /// there. The command path passes the drain boundary's clock; the
    /// impact seam passes the impact's own tick so the damage is not
    /// retroactive over the range it was collected in.
    pub(crate) fn apply_damage_module(
        &mut self,
        id: ColonyId,
        module: usize,
        tick: u64,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let Some(colony) = self.colonies.get(&id) else {
            return Err(SimError::Recoverable("damage: unknown colony"));
        };
        if matches!(colony.state, ColonyState::Dormant | ColonyState::Abandoned) {
            return Err(SimError::Recoverable("damage: colony not live"));
        }
        if module >= colony.modules.len() {
            return Err(SimError::Recoverable("damage: no such module"));
        }
        if colony.modules[module].damaged {
            return Err(SimError::Recoverable("damage: module already damaged"));
        }
        self.shadow_colonies(shadow);
        let colony = self
            .colonies
            .get_mut(&id)
            .ok_or(SimError::Fatal("damage: colony vanished"))?;
        colony.modules[module].damaged = true;
        if matches!(colony.state, ColonyState::Nominal) {
            colony.state = ColonyState::Degraded {
                reason: DegradeReason::ImpactDamage,
            };
        }
        // C09-F6: the damage was terminal-but-silent — the LOG ring
        // now carries it (program-level entry, the contract-kind
        // convention).
        self.notify(
            crate::reliability::Notification {
                tick: self.clock.ticks(),
                vessel: crate::reliability::PROGRAM_VESSEL,
                node: None,
                kind: crate::reliability::NotificationKind::ColonyDamaged {
                    colony: id.0,
                    module: module as u64,
                },
            },
            shadow,
        );
        self.recompute_colony(id, tick, shadow)
    }

    /// `RepairColonyModule` — C09-F6's doc-promised repair path: for
    /// the authored `colony_module_repair_cost_funds` debit, clear
    /// one module's `damaged` flag and, when that flag was the last
    /// damage standing, lift `Degraded{ImpactDamage}` back to
    /// `Nominal`. A `Degraded{PowerLost}` colony keeps its reason —
    /// the repair restores the module, not the network's power
    /// balance (the `PowerRestored` event remains that reason's only
    /// exit); repairing under `Nominal` — legal, the state can hold
    /// damaged flags after a `PowerRestored` lift — changes the flag
    /// and nothing else. `Dormant`/`Abandoned` refuse like the
    /// damage apply does. The debit precedes the flag write so a
    /// clamped charge can never deliver a free repair — and the
    /// charge is a hard gate, not `charge`'s clamped drain: the
    /// player either pays the authored cost or the module stays
    /// broken. `tick` anchors the recompute like the damage path's.
    pub(crate) fn apply_repair_module(
        &mut self,
        id: ColonyId,
        module: usize,
        tick: u64,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let Some(colony) = self.colonies.get(&id) else {
            return Err(SimError::Recoverable("repair: unknown colony"));
        };
        if matches!(colony.state, ColonyState::Dormant | ColonyState::Abandoned) {
            return Err(SimError::Recoverable("repair: colony not live"));
        }
        if module >= colony.modules.len() {
            return Err(SimError::Recoverable("repair: no such module"));
        }
        if !colony.modules[module].damaged {
            return Err(SimError::Recoverable("repair: module not damaged"));
        }
        let cost = self.economy_cfg.colony_module_repair_cost_micros;
        if cost > self.economy.funds_micros {
            return Err(SimError::Recoverable("repair: insufficient funds"));
        }
        self.shadow_economy(shadow);
        crate::economy::charge(
            &mut self.economy,
            self.clock.ticks(),
            cost,
            crate::economy::FundsReason::ColonyModuleRepair,
        );
        self.shadow_colonies(shadow);
        let colony = self
            .colonies
            .get_mut(&id)
            .ok_or(SimError::Fatal("repair: colony vanished"))?;
        colony.modules[module].damaged = false;
        // The damage→repaired transition: `ImpactDamage` degrades with
        // nothing left broken → `Nominal`. `PowerLost` and `Nominal`
        // are untouched — the former's exit stays `PowerRestored`,
        // the latter was never lifted.
        if matches!(
            colony.state,
            ColonyState::Degraded {
                reason: DegradeReason::ImpactDamage
            }
        ) && !colony.modules.iter().any(|m| m.damaged)
        {
            colony.state = ColonyState::Nominal;
        }
        self.notify(
            crate::reliability::Notification {
                tick: self.clock.ticks(),
                vessel: crate::reliability::PROGRAM_VESSEL,
                node: None,
                kind: crate::reliability::NotificationKind::ColonyModuleRepaired {
                    colony: id.0,
                    module: module as u64,
                },
            },
            shadow,
        );
        self.recompute_colony(id, tick, shadow)
    }

    /// M8-C §3.5: the deferred surface-impact seam — every debris
    /// body that touched the surface inside `step_bubble_range`
    /// records its environment-frame impact point; here each point
    /// damages the lowest-index undamaged module of every live colony
    /// within `transfer_cfg.radius_m` (the colony's "radius" — the
    /// `JoinColonyInventory` reach) on the bubble's dominant body,
    /// landing `Degraded { reason: ImpactDamage }` through the
    /// `DamageColonyModule` bookkeeping. Colonies on other bodies are
    /// unreachable — the bubble only ever sits on `dominant`.
    /// Iteration is ascending `ColonyId` and selection is
    /// first-undamaged: the outcome is a deterministic function of
    /// `(impacts, colonies)` with no draw needed.
    /// Each entry pairs the impact point with the completed tick it
    /// landed at (`t + 1` inside the range): the damage's
    /// `recompute_colony` anchors there, not at the range's `now` —
    /// anchoring at the range start would treat the damaged module as
    /// retroactively inert over the whole `(now, t_next]` interval.
    pub(crate) fn apply_surface_impacts(
        &mut self,
        impacts: &[(csp_core::DVec3, u64)],
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        if impacts.is_empty() {
            return Ok(());
        }
        let Some(physics) = &self.physics else {
            return Ok(());
        };
        let env = physics.backend.environment();
        let center = env.body_center;
        let dominant = physics.dominant.clone();
        let range = self.transfer_cfg.radius_m;
        for &(pos, impact_tick) in impacts {
            if !pos.is_finite() {
                continue;
            }
            // The point's targets — collected so the damage calls
            // (which mutate the arena) never run under the scan's
            // borrow.
            let mut hits: Vec<(ColonyId, usize)> = Vec::new();
            for (id, colony) in self.colonies.iter() {
                // Non-live colonies are not targets: `apply_damage_module`
                // refuses `Dormant`/`Abandoned` with a `Recoverable`,
                // and `step_frame`'s contract is `Err` only for `Fatal`
                // — a dormant colony in the radius would otherwise
                // wedge every advance past the impact.
                // A colony whose body no longer has a `colony_env`
                // entry (a mid-run `set_colony_env` swap) cannot be
                // solved — `recompute_colony` fails `Recoverable` on
                // the miss, and `step_frame` may carry only `Fatal`:
                // like the dormant filter, it is simply not a target.
                if colony.body != dominant
                    || matches!(colony.state, ColonyState::Dormant | ColonyState::Abandoned)
                {
                    continue;
                }
                // The colony's surface point — the ONE convention the
                // proximity gate shares (`transfers::colony_surface_
                // point`): `colony_env` radius plus the stored founding
                // altitude. `None` is the envless-colony skip above.
                let Some(site) = self.colony_surface_point(colony) else {
                    continue;
                };
                if (pos - center - site).norm() > range {
                    continue;
                }
                if let Some(idx) = colony.modules.iter().position(|m| !m.damaged) {
                    hits.push((*id, idx));
                }
            }
            for (id, module) in hits {
                self.apply_damage_module(id, module, impact_tick, shadow)?;
            }
        }
        Ok(())
    }

    /// `ScheduleColonyInspection` — the rule-4 "next scheduled
    /// inspection": set the colony's inspection deadline and
    /// republish the boundary set so it lands in the `min`. A tick at
    /// or before the drain boundary clamps to the next tick (the
    /// `SetAlarm` rule).
    pub(crate) fn apply_schedule_inspection(
        &mut self,
        id: ColonyId,
        tick: u64,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let Some(colony) = self.colonies.get(&id) else {
            return Err(SimError::Recoverable("inspection: unknown colony"));
        };
        if matches!(colony.state, ColonyState::Dormant | ColonyState::Abandoned) {
            return Err(SimError::Recoverable("inspection: colony not live"));
        }
        self.shadow_colonies(shadow);
        let t = tick.max(self.clock.ticks().saturating_add(1));
        self.colonies
            .get_mut(&id)
            .ok_or(SimError::Fatal("inspection: colony vanished"))?
            .inspection_tick = Some(t);
        self.recompute_colony(id, self.clock.ticks(), shadow)
    }

    /// The shared solve-input scaffold behind [`Self::colony_net_solve`]
    /// and [`Self::recompute_colony`] (C09-F10). `None` on the missing
    /// `colony_env` case — `colony_net_solve` propagates it as `None`,
    /// `recompute_colony` maps it to its `Recoverable`.
    fn colony_solve_frame(&self, colony: &Colony) -> Option<SolveFrame> {
        let env = self.colony_env.get(&colony.body).copied()?;
        Some(SolveFrame {
            light: body_light(
                &colony.body,
                colony.latitude_rad,
                colony.longitude_rad,
                &self.orbits,
                &env,
            ),
            concentrations: self
                .site_defs
                .get(&colony.site)
                .map(|d| d.resource_yields.iter().cloned().collect())
                .unwrap_or_default(),
            crew_demands: self.life_support_cfg.demands(),
            // M8-C §3.5's per-save toggle reads as zero crew for the
            // `LifeSupport` rows — the `crew > 0` gates in
            // `module_requests` and the depletion scan then close the
            // whole arm (no consumable draw, no EC draw, no
            // depletion). C09-F4: crewed colonies WITHOUT a
            // `LifeSupport` module consume the vessel-side baseline
            // set from colony stores (the toggle's zero-crew reading
            // closes this arm too).
            crew: if self.life_support_enabled {
                colony.crew.len()
            } else {
                0
            },
        })
    }

    /// The firing-time predicate re-check the transition arms run:
    /// re-solves the colony's network over exactly the inputs
    /// `recompute_colony` would read at `tick` — pure (levels via
    /// `level_at`, no anchoring, no writes), so the answer is
    /// identical in every §2.6.1 partition. A committed notification
    /// is scheduled one tick out; a same-tick event firing ahead of it
    /// (a `TransferComplete` resupply, a crew landing) can already
    /// have lifted the predicate — the firing re-checks rather than
    /// trusting the stale commit. `None` when the colony cannot be
    /// solved at all (a missing `colony_env` entry — the input
    /// `recompute_colony` fails `Recoverable` on).
    fn colony_net_solve(&self, id: ColonyId, tick: u64) -> Option<NetSolve> {
        let colony = self.colonies.get(&id)?;
        let frame = self.colony_solve_frame(colony)?;
        let levels: BTreeMap<ResourceId, f64> = colony
            .stores
            .iter()
            .map(|(r, s)| (r.clone(), s.level_at(tick)))
            .collect();
        let caps: BTreeMap<ResourceId, f64> = colony
            .stores
            .iter()
            .map(|(r, s)| (r.clone(), s.capacity))
            .collect();
        Some(solve_network(&SolveInput {
            modules: &colony.modules,
            state: colony.state,
            levels: &levels,
            caps: &caps,
            light: &frame.light,
            crew: frame.crew,
            daylight: colony.daylight,
            recipes: &self.recipes,
            concentrations: &frame.concentrations,
            crew_demands: &frame.crew_demands,
        }))
    }

    /// The roster half of vacating a colony's crew (C09-F10 — was
    /// three hand-kept copies at abandon / evacuate /
    /// LS-depleted): every member in `crew` still posted
    /// `Colony(id)` goes `Unassigned`; `lost` marks them
    /// [`CrewStatus::Lost`] first (the depletion path's dead). The
    /// caller shadows `economy` and clears `colony.crew` itself —
    /// that write sits beside the state write it belongs to.
    fn unhook_colony_crew(&mut self, id: ColonyId, crew: &[CrewId], lost: bool) {
        for c in crew {
            if let Some(m) = self.economy.crew.get_mut(c)
                && m.location == crate::economy::CrewLocation::Colony(id)
            {
                if lost {
                    m.status = CrewStatus::Lost;
                }
                m.location = crate::economy::CrewLocation::Unassigned;
            }
        }
    }

    /// One colony event's application — the event-side counterpart of
    /// the applies. Boundary kinds funnel into `recompute_colony`
    /// (each stamps its own reason first); transitions apply their
    /// state change, then recompute where the state stays live.
    #[allow(clippy::too_many_lines)] // a dispatch table, arm-per-kind
    pub(crate) fn fire_colony_event(
        &mut self,
        kind: &EventKind,
        event_tick: u64,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let Some(id) = event_colony(kind) else {
            return Ok(());
        };
        let Some(colony) = self.colonies.get(&id) else {
            // Stale event for a removed colony — a no-op, the
            // `DebrisDecay` precedent.
            return Ok(());
        };
        let live = matches!(
            colony.state,
            ColonyState::Nominal | ColonyState::Degraded { .. }
        );
        // The re-solve anchors at the EVENT's tick — `fire_due_events`
        // runs every firing with `self.clock == event.tick`, so `now`
        // IS the clock here; naming `event_tick` just keeps the anchor
        // honest if the firing context ever changes. A lazily
        // re-published event can legitimately fire mid-interval with
        // `event.tick < t_next`; anchoring at the range-end clock
        // there would make `anchor_tick` (hashed state) and the
        // `tick + 1` transition schedules diverge across §2.6.1
        // partitions — the value at the event's own tick is the one
        // every partition agrees on.
        let now = event_tick;
        match kind {
            EventKind::TerminatorCrossing { .. } => {
                if !live {
                    return Ok(());
                }
                // The crossing is computed, never toggled blindly —
                // the closed form stamps the post-boundary state.
                let env = self.colony_env.get(&colony.body).copied();
                let light = env.map(|e| {
                    body_light(
                        &colony.body,
                        colony.latitude_rad,
                        colony.longitude_rad,
                        &self.orbits,
                        &e,
                    )
                });
                self.shadow_colonies(shadow);
                if let Some(light) = light {
                    self.colonies
                        .get_mut(&id)
                        .ok_or(SimError::Fatal("terminator: colony vanished"))?
                        .daylight = daylight_at(&light, now);
                }
                self.recompute_colony(id, now, shadow)
            }
            EventKind::ColonyStoreFull { .. }
            | EventKind::ColonyStoreEmpty { .. }
            | EventKind::ColonyInspection { .. } => {
                if !live {
                    return Ok(());
                }
                if matches!(kind, EventKind::ColonyInspection { .. }) {
                    self.shadow_colonies(shadow);
                    let colony = self
                        .colonies
                        .get_mut(&id)
                        .ok_or(SimError::Fatal("inspection: colony vanished"))?;
                    // The deadline is consumed once it passes —
                    // event_tick may sit behind the clock in a flush.
                    if colony.inspection_tick.is_some_and(|t| t <= now) {
                        colony.inspection_tick = None;
                    }
                }
                self.recompute_colony(id, now, shadow)
            }
            EventKind::PowerLost { .. } => {
                if !matches!(colony.state, ColonyState::Nominal) {
                    return Ok(());
                }
                // Committed is the event, not the predicate: a
                // same-tick landing (an EC resupply credited ahead of
                // this firing) can lift the starvation the scheduling
                // recompute saw — re-check at the firing tick or the
                // transition lands a lie.
                if !self
                    .colony_net_solve(id, now)
                    .is_some_and(|s| s.power_starved)
                {
                    return Ok(());
                }
                self.shadow_colonies(shadow);
                self.colonies
                    .get_mut(&id)
                    .ok_or(SimError::Fatal("power lost: colony vanished"))?
                    .state = ColonyState::Degraded {
                    reason: DegradeReason::PowerLost,
                };
                self.recompute_colony(id, now, shadow)
            }
            EventKind::PowerRestored { .. } => {
                if !matches!(
                    colony.state,
                    ColonyState::Degraded {
                        reason: DegradeReason::PowerLost
                    }
                ) {
                    return Ok(());
                }
                // The inverse re-check: restoration is earned by the
                // predicate at firing time — a same-tick demand or
                // production change that re-starved the colony leaves
                // it `Degraded`, the event a stale no-op.
                if self
                    .colony_net_solve(id, now)
                    .is_none_or(|s| s.power_starved)
                {
                    return Ok(());
                }
                self.shadow_colonies(shadow);
                self.colonies
                    .get_mut(&id)
                    .ok_or(SimError::Fatal("power restored: colony vanished"))?
                    .state = ColonyState::Nominal;
                self.recompute_colony(id, now, shadow)
            }
            EventKind::LifeSupportDepleted { .. } => {
                // A disabled subsystem never kills — the
                // `fire_vessel_ls_depleted` gate's colony twin: the
                // toggle-off scrub drops the pending transition,
                // and this check is the belt to its suspenders (a
                // straggler that survives — a pre-fix save's —
                // fires as a no-op).
                if !live || !self.life_support_enabled {
                    return Ok(());
                }
                // The depletion re-check — the same belt-and-suspenders
                // reading: a same-tick `TransferComplete` resupply (or
                // a crew landing) ahead of this firing can restore a
                // grant the scheduling recompute could not yet see;
                // the commit is the event's, the predicate is
                // re-derived at its own tick.
                if !self
                    .colony_net_solve(id, now)
                    .is_some_and(|s| s.ls_depleted)
                {
                    return Ok(());
                }
                // Crew aboard are Lost (the deaths setting's
                // `off | respawn` branch is post-M8-A; permanent is
                // the honest default); the colony goes Dormant and
                // schedules nothing ever again (rule 5).
                let crew: Vec<CrewId> = colony.crew.iter().copied().collect();
                self.shadow_colonies(shadow);
                self.shadow_economy(shadow);
                self.unhook_colony_crew(id, &crew, true);
                // C09-F8: Dormant is terminal like Abandoned — clear
                // the joined-vessel opt-ins so a latched hull beside
                // the wreck doesn't carry a stale `joined_colony`.
                self.shadow_vessels(shadow);
                for v in self.vessels.values_mut() {
                    if v.joined_colony == Some(id) {
                        v.joined_colony = None;
                    }
                }
                let colony = self
                    .colonies
                    .get_mut(&id)
                    .ok_or(SimError::Fatal("life support: colony vanished"))?;
                colony.crew.clear();
                colony.state = ColonyState::Dormant;
                // Dormant consumes, produces, and schedules NOTHING —
                // the stores freeze at the firing tick.
                freeze_colony_stores(colony, now);
                self.drop_colony_events(id, false, shadow);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// The recompute — the §3.5.1 interval boundary, verbatim: re-anchor
    /// the stores at `tick`, solve the network, write the held rates,
    /// drop-and-republish the boundary set, then schedule any transition
    /// the solve predicates earned (one tick out — mutations land at
    /// event firings, never inside a solve).
    #[allow(clippy::too_many_lines)] // a sequential pipeline: anchor → solve → publish → transitions
    pub(crate) fn recompute_colony(
        &mut self,
        id: ColonyId,
        tick: u64,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let Some(colony) = self.colonies.get(&id) else {
            return Ok(());
        };
        if matches!(colony.state, ColonyState::Dormant | ColonyState::Abandoned) {
            return Ok(());
        }
        let Some(frame) = self.colony_solve_frame(colony) else {
            return Err(SimError::Recoverable(
                "colony: no colony environment for body",
            ));
        };
        let light = frame.light;
        self.shadow_colonies(shadow);
        let levels: BTreeMap<ResourceId, f64> = {
            let colony = self
                .colonies
                .get_mut(&id)
                .ok_or(SimError::Fatal("colony: vanished"))?;
            let modules = &colony.modules;
            colony
                .stores
                .iter_mut()
                .map(|(r, s)| {
                    // Capacity rides the re-anchor: `live_capacity`
                    // re-derives it from the undamaged module set
                    // BEFORE `level_at` clamps, so a damaged
                    // Store/Battery sheds its contents — and its
                    // headroom — at this boundary, not retroactively.
                    s.capacity = live_capacity(modules, r);
                    let level = s.level_at(tick);
                    s.anchor_level = level;
                    s.anchor_tick = tick;
                    (r.clone(), level)
                })
                .collect()
        };
        let colony = self
            .colonies
            .get(&id)
            .ok_or(SimError::Fatal("colony: vanished"))?;
        let caps: BTreeMap<ResourceId, f64> = colony
            .stores
            .iter()
            .map(|(r, s)| (r.clone(), s.capacity))
            .collect();
        let solve = solve_network(&SolveInput {
            modules: &colony.modules,
            state: colony.state,
            levels: &levels,
            caps: &caps,
            light: &light,
            crew: frame.crew,
            daylight: colony.daylight,
            recipes: &self.recipes,
            concentrations: &frame.concentrations,
            crew_demands: &frame.crew_demands,
        });
        let colony = self
            .colonies
            .get_mut(&id)
            .ok_or(SimError::Fatal("colony: vanished"))?;
        colony.module_fractions = solve.fractions;
        colony.t_eq_k = solve.t_eq_k;
        for (r, s) in colony.stores.iter_mut() {
            s.net_rate_per_s = solve.net_rates.get(r).copied().unwrap_or(0.0);
        }
        colony.solved_tick = tick;
        let state = colony.state;
        // A deadline at-or-behind this boundary is consumed BY this
        // recompute — the re-solve it asked for is happening now. The
        // old `filter(|t| *t > tick)` merely dropped the republish
        // while `inspection_tick` stayed `Some(past)` — the pending
        // event scrubbed below meant it could never fire, and the
        // slot could never be re-offered: a silent cancel plus a
        // leaked deadline.
        if colony.inspection_tick.is_some_and(|t| t <= tick) {
            colony.inspection_tick = None;
        }
        let store_bound = earliest_store_bound(colony, tick);
        let inspection = colony.inspection_tick;
        // Drop-and-republish the boundary set — the heap holds only
        // live deadlines (module doc). Terminator, the earliest store
        // bound, and the inspection are independent deadlines and each
        // stays pending (the heap's min orders them).
        self.drop_colony_events(id, true, shadow);
        if let Some(t) = next_terminator_tick(&light, tick) {
            self.schedule_event(EventKind::TerminatorCrossing { colony: id }, t, shadow)?;
        }
        if let Some((t, resource, full)) = store_bound {
            let kind = if full {
                EventKind::ColonyStoreFull {
                    colony: id,
                    resource,
                }
            } else {
                EventKind::ColonyStoreEmpty {
                    colony: id,
                    resource,
                }
            };
            self.schedule_event(kind, t, shadow)?;
        }
        if let Some(t) = inspection {
            self.schedule_event(EventKind::ColonyInspection { colony: id }, t, shadow)?;
        }
        // Transitions — scheduled one tick out, never inside the
        // solve (module doc rule 5). A notification of the same kind
        // already pending for this colony is NOT re-scheduled: two
        // recomputes at one tick (two commands in a drain, a boundary
        // event firing ahead of the notification's own tick) would
        // otherwise queue duplicates — and the restore walk's
        // per-kind pairing Fatals on the second.
        if matches!(state, ColonyState::Nominal) && solve.power_starved {
            let kind = EventKind::PowerLost { colony: id };
            if !self.event_heap.iter().any(|e| e.kind == kind) {
                self.schedule_event(kind, tick.saturating_add(1), shadow)?;
            }
        }
        if matches!(
            state,
            ColonyState::Degraded {
                reason: DegradeReason::PowerLost
            }
        ) && !solve.power_starved
        {
            let kind = EventKind::PowerRestored { colony: id };
            if !self.event_heap.iter().any(|e| e.kind == kind) {
                self.schedule_event(kind, tick.saturating_add(1), shadow)?;
            }
        }
        if solve.ls_depleted {
            let kind = EventKind::LifeSupportDepleted { colony: id };
            if !self.event_heap.iter().any(|e| e.kind == kind) {
                self.schedule_event(kind, tick.saturating_add(1), shadow)?;
            }
        }
        Ok(())
    }

    /// The `Colonies` slot (§2.6.5 index 3) — publish-only, like the
    /// economy's `SalaryDue` cadence: for every live colony, ensure a
    /// pending boundary event exists at or before its earliest needed
    /// boundary; a missing deadline (a degraded firing's drop) is
    /// repaired with a `ColonyInspection` at the needed tick. A pure
    /// function of `(state, heap)` — it never touches colony records,
    /// so the publish lands identically in every §2.6.1 partition.
    /// Dormant/abandoned colonies publish nothing — rule 5's "costs
    /// nothing under warp".
    pub(crate) fn publish_colonies(&mut self, shadow: &mut TickShadow) -> Result<(), SimError> {
        let now = self.clock.ticks();
        // Collect first — the publish writes the heap, which the
        // colony scan borrows.
        let mut repairs = Vec::new();
        for (id, colony) in &self.colonies {
            if matches!(colony.state, ColonyState::Dormant | ColonyState::Abandoned) {
                continue;
            }
            let Some(env) = self.colony_env.get(&colony.body).copied() else {
                continue;
            };
            let light = body_light(
                &colony.body,
                colony.latitude_rad,
                colony.longitude_rad,
                &self.orbits,
                &env,
            );
            let Some(needed) = next_boundary(colony, &light, now) else {
                continue;
            };
            let covered = self.event_heap.iter().any(|e| {
                is_colony_boundary(&e.kind)
                    && event_colony(&e.kind) == Some(*id)
                    && e.tick <= needed
            });
            if !covered {
                repairs.push((*id, needed));
            }
        }
        for (id, t) in repairs {
            self.schedule_event(EventKind::ColonyInspection { colony: id }, t, shadow)?;
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
    use crate::{MAX_TICKS_PER_FRAME, SimCommand, SourceClass, VesselId, VesselState};
    use csp_data::{KeplerElements, SiteDef};
    use std::path::Path;

    /// A two-body test system: a sun and a 6 h day planet — the
    /// terminator math's toy.
    fn test_system() -> PatchedConics {
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
        PatchedConics::new(bodies)
    }

    fn test_env() -> BTreeMap<BodyId, ColonyBodyEnv> {
        let mut env = BTreeMap::new();
        env.insert(
            BodyId::from("terra"),
            ColonyBodyEnv {
                rotation_period_s: Some(21600.0),
                insolation_w_m2: Some(1361.0),
                radius_m: 6.0e6,
            },
        );
        env
    }

    fn test_site() -> SiteDef {
        SiteDef {
            schema_version: 1,
            id: SiteId::from("pad"),
            display_name: csp_data::LocKey::from("site.pad"),
            body: BodyId::from("terra"),
            latitude_rad: 0.0,
            longitude_rad: 0.0,
            altitude_m: 0.0,
            launch: false,
            resource_yields: vec![
                (ResourceId::from("ore"), 1.0),
                (ResourceId::from("metal"), 1.0),
                (ResourceId::from("water"), 1.0),
                (ResourceId::from("oxygen"), 1.0),
                (ResourceId::from("food"), 1.0),
            ],
            starting_stock: vec![],
        }
    }

    /// A world with the site + env installed — and life support
    /// opted in (the default is OFF; these tests exercise the
    /// `LifeSupport` module's demand/depletion path).
    fn colony_world() -> World {
        let mut w = World::with_system(test_system());
        w.set_colony_env(test_env());
        w.set_sites(BTreeMap::from([(SiteId::from("pad"), test_site())]));
        w.set_life_support_enabled(true);
        w
    }

    fn solar_def(power: f64) -> ColonyModuleDef {
        ColonyModuleDef::Solar {
            area_m2: 1.0,
            efficiency: power / (1361.0 * 2.0 / PI),
        }
    }

    fn found(
        w: &mut World,
        modules: Vec<ColonyModuleDef>,
        stores: Vec<csp_data::ColonyStoreInit>,
    ) -> crate::AdvanceReport {
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::FoundColony {
                def: ColonyDef {
                    site: SiteId::from("pad"),
                    display_name: csp_data::LocKey::from("colony.x"),
                    modules,
                    stores,
                },
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap()
    }

    fn ec() -> ResourceId {
        ResourceId::from(ELECTRIC_CHARGE)
    }

    /// M8-B: an RTG generator — `fuel_rate_per_s: 0` reads as the
    /// fuel-free constant producer the tests lean on.
    fn gen_module(rate_per_s: f64) -> ColonyModuleDef {
        ColonyModuleDef::Generator {
            fuel: ResourceId::from("ore"),
            fuel_rate_per_s: 0.0,
            rate_per_s,
            priority: 0,
        }
    }

    /// M8-B: a fuel-burning generator with an authored priority.
    fn fuel_gen(
        fuel: &str,
        fuel_rate_per_s: f64,
        rate_per_s: f64,
        priority: u8,
    ) -> ColonyModuleDef {
        ColonyModuleDef::Generator {
            fuel: ResourceId::from(fuel),
            fuel_rate_per_s,
            rate_per_s,
            priority,
        }
    }

    /// M8-B: register `id` with the given input/output rate rows and
    /// return the `Converter` def naming it — the recipe-indirection
    /// twin of the M8-A inline-pair module.
    fn conv(
        recipes: &mut BTreeMap<RecipeId, RecipeDef>,
        id: &str,
        inputs: Vec<(ResourceId, f64)>,
        outputs: Vec<(ResourceId, f64)>,
        priority: u8,
        power_ec_per_s: f64,
    ) -> ColonyModuleDef {
        recipes.insert(
            RecipeId::from(id),
            RecipeDef {
                schema_version: csp_data::RECIPE_SCHEMA_VERSION,
                id: RecipeId::from(id),
                display_name: csp_data::LocKey::from("recipe.t.name"),
                inputs,
                outputs,
            },
        );
        ColonyModuleDef::Converter {
            recipe: RecipeId::from(id),
            priority,
            power_ec_per_s,
        }
    }

    /// The `SolveInput` literal's mechanical tail — every test's
    /// construction names the pieces it varies; recipes/concentrations
    /// ride the locals each test binds.
    /// The `caps` a test author means — the store table covers every
    /// `levels`/`out` resource the fixture exercises, at effectively
    /// unbounded capacity (the headroom check's "a store exists, not
    /// full" reading).
    fn test_caps(resources: &[&str]) -> BTreeMap<ResourceId, f64> {
        resources
            .iter()
            .map(|r| (ResourceId::from(*r), f64::MAX))
            .collect()
    }

    /// The fixtures' blanket store table — every resource the tests
    /// produce, unbounded.
    fn all_caps() -> BTreeMap<ResourceId, f64> {
        test_caps(&[
            "electric_charge",
            "ore",
            "metal",
            "x",
            "y",
            "gold",
            "food",
            "water",
            "oxygen",
        ])
    }

    /// The baseline demand set the solve reads for an LS-less
    /// crewed colony — the production `life_support_cfg.demands()`.
    /// Tests bind it to a local and pass `&demands` into `inp`.
    fn crew_demands() -> [(ResourceId, f64); 3] {
        crate::life_support::default_config().demands()
    }

    #[allow(clippy::too_many_arguments)] // a test-side SolveInput builder
    fn inp<'a>(
        modules: &'a [ColonyModule],
        state: ColonyState,
        levels: &'a BTreeMap<ResourceId, f64>,
        caps: &'a BTreeMap<ResourceId, f64>,
        light: &'a BodyLight,
        crew: usize,
        daylight: Daylight,
        recipes: &'a BTreeMap<RecipeId, RecipeDef>,
        concentrations: &'a BTreeMap<ResourceId, f64>,
        crew_demands: &'a [(ResourceId, f64)],
    ) -> SolveInput<'a> {
        SolveInput {
            modules,
            state,
            levels,
            caps,
            light,
            crew,
            daylight,
            recipes,
            concentrations,
            crew_demands,
        }
    }

    /// Daylit insolation average — (2/π)·S·cos φ.
    #[test]
    fn body_light_closed_form() {
        let orbits = test_system();
        let env = test_env();
        let light = body_light(
            &BodyId::from("terra"),
            0.0,
            0.0,
            &orbits,
            env.get(&BodyId::from("terra")).unwrap(),
        );
        // ω = 2π/21600; n = 2π/(86400·365.25) — synodic ≈ ω.
        let n = TWO_PI / (86400.0 * 365.25);
        let omega = TWO_PI / 21600.0;
        assert!((light.synodic_rate - (omega - n)).abs() < 1e-12);
        // h0 = 0 − 0 − π = −π (antipodal to the body's +x at epoch).
        assert!((light.h0 + PI).abs() < 1e-12);
        assert!((light.day_insolation_w_m2 - 1361.0 * 2.0 / PI).abs() < 1e-9);
    }

    /// Terminator crossings land on the closed-form half-solar-day
    /// grid and daylight flips across each one.
    #[test]
    fn terminator_times_are_closed_form() {
        let orbits = test_system();
        let env = test_env();
        let light = body_light(
            &BodyId::from("terra"),
            0.0,
            0.0,
            &orbits,
            env.get(&BodyId::from("terra")).unwrap(),
        );
        // At lon 0 the sub-solar longitude starts at π (h0 = −π):
        // the point is at midnight — Night; sunrise at H=−π/2, i.e.
        // a quarter solar day out.
        assert_eq!(daylight_at(&light, 0), Daylight::Night);
        let t = next_terminator_tick(&light, 0).unwrap();
        let solar_half = PI / light.synodic_rate;
        // First crossing within [quarter, half] solar-day spacing.
        let dt_s = t as f64 / TICK_HZ as f64;
        assert!(dt_s > 0.0 && dt_s <= solar_half + 1.0);
        assert_eq!(daylight_at(&light, t), Daylight::Day);
        let t2 = next_terminator_tick(&light, t).unwrap();
        assert_eq!(daylight_at(&light, t2), Daylight::Night);
        // Spacing is the half solar day, ±1 tick of the ceil bound.
        let spacing_ticks = (solar_half * TICK_HZ as f64) as u64;
        assert!((t2 - t).abs_diff(spacing_ticks) <= 1);
        // Tidally locked → no crossings ever.
        let locked = BodyLight {
            synodic_rate: 0.0,
            day_insolation_w_m2: 0.0,
            ..light
        };
        assert_eq!(next_terminator_tick(&locked, 0), None);
    }

    /// Fix-plan 3.6's tidal model (C09-F3/C34-F3): a body with
    /// `rotation_period_s: None` is locked to its PARENT — ω is its
    /// own orbital mean motion — while the sun direction sweeps at
    /// the star-orbiting ancestor's rate, so
    /// `synodic = n_moon − n_planet` (the synodic month), NOT the
    /// identically-0 the old `omega − n` self-difference produced.
    #[test]
    fn locked_moon_synodic_rate_uses_parent_mean_motion() {
        const T_PLANET: f64 = 9_203_835.5;
        const T_MOON: f64 = 138_984.4;
        const PLANET_M0: f64 = 0.5;
        let mut bodies = BTreeMap::new();
        bodies.insert(
            BodyId::from("sol"),
            crate::orbit::BodyOrbit {
                mu_m3_per_s2: 1.0e20,
                soi_m: None,
                parent: None,
                elements: None,
            },
        );
        bodies.insert(
            BodyId::from("planet"),
            crate::orbit::BodyOrbit {
                mu_m3_per_s2: 4.0e14,
                soi_m: Some(1.0e9),
                parent: Some(BodyId::from("sol")),
                elements: Some(KeplerElements {
                    semi_major_axis_m: 1.5e11,
                    eccentricity: 0.0,
                    inclination_rad: 0.0,
                    ascending_node_rad: 0.0,
                    arg_periapsis_rad: 0.0,
                    mean_anomaly_epoch_rad: PLANET_M0,
                    period_s: T_PLANET,
                }),
            },
        );
        bodies.insert(
            BodyId::from("moon"),
            crate::orbit::BodyOrbit {
                mu_m3_per_s2: 4.0e10,
                soi_m: Some(1.0e7),
                parent: Some(BodyId::from("planet")),
                elements: Some(KeplerElements {
                    semi_major_axis_m: 1.0e7,
                    eccentricity: 0.0,
                    inclination_rad: 0.0,
                    ascending_node_rad: 0.0,
                    arg_periapsis_rad: 0.0,
                    mean_anomaly_epoch_rad: 0.0,
                    period_s: T_MOON,
                }),
            },
        );
        let orbits = PatchedConics::new(bodies);
        let locked_env = ColonyBodyEnv {
            rotation_period_s: None,
            insolation_w_m2: Some(1.0),
            radius_m: 1.0,
        };
        let light = body_light(&BodyId::from("moon"), 0.0, 0.3, &orbits, &locked_env);
        // ω = n_moon (locked = the moon's own orbital period); the
        // sun sweep is the planet's heliocentric n — the synodic
        // month, exactly the parent-derived value.
        let n_moon = TWO_PI / T_MOON;
        let n_planet = TWO_PI / T_PLANET;
        assert_eq!(light.synodic_rate, n_moon - n_planet);
        assert!(light.synodic_rate > 0.0);
        // h0 keys off the PARENT's epoch longitude (0.5), not the
        // moon's own (0): h0 = λ − L0_planet − π.
        assert!((light.h0 - (0.3 - PLANET_M0 - PI)).abs() < 1e-12);
        // Crossings exist again — the terminator clock is live.
        assert!(next_terminator_tick(&light, 0).is_some());
        // A planet locked to the STAR keeps the genuinely fixed sun:
        // synodic ≡ 0, no crossings ever.
        let pl = body_light(&BodyId::from("planet"), 0.0, 0.0, &orbits, &locked_env);
        assert_eq!(pl.synodic_rate, 0.0);
        assert_eq!(next_terminator_tick(&pl, 0), None);
    }

    /// The authored `data/systems/home` column — the real ground
    /// truth the 3.6 day/night pins hold (the lib.rs suite's
    /// `authored_home_defs` precedent).
    fn home_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("data/systems/home")
    }

    /// The authored-data fixture both `authored_rask_*` pins load:
    /// the real home system as patched conics + colony env, the
    /// authored rask-mines site (a clone — callers re-pose its
    /// longitude for the founding hour angle they need), the
    /// authored rask-mines colony def, and the shipped recipe table.
    struct HomeRask {
        orbits: PatchedConics,
        envs: BTreeMap<BodyId, ColonyBodyEnv>,
        site: SiteDef,
        colony: csp_data::ColonyDef,
        recipes: csp_data::LoadedRecipes,
    }

    impl HomeRask {
        /// `body_light` for the (possibly re-posed) site — the pin
        /// both tests anchor their day/night claims on.
        fn light(&self) -> BodyLight {
            let rask = BodyId::from("rask");
            body_light(
                &rask,
                self.site.latitude_rad,
                self.site.longitude_rad,
                &self.orbits,
                &self.envs[&rask],
            )
        }
    }

    /// Load the [`HomeRask`] fixture from `data/systems/home` +
    /// `data/recipes` — one preamble serving both authored-data
    /// tests.
    fn home_rask() -> HomeRask {
        let dir = home_dir();
        let defs: Vec<BodyDef> = csp_data::load_system(&dir)
            .unwrap()
            .bodies
            .iter()
            .map(|b| b.def.clone())
            .collect();
        let orbits = PatchedConics::from_body_defs(&defs).unwrap();
        let envs = colony_env_from_body_defs(&defs);
        let sites = csp_data::load_sites(&dir.join("sites.ron")).unwrap();
        let site = sites
            .sites
            .iter()
            .find(|s| s.id.as_str() == "rask-mines")
            .unwrap()
            .clone();
        let colonies = csp_data::load_colonies(&dir.join("colonies.ron")).unwrap();
        let colony = colonies
            .colonies
            .iter()
            .find(|c| c.site.as_str() == "rask-mines")
            .unwrap()
            .clone();
        let recipes = csp_data::load_recipes(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join("data/recipes"),
        )
        .unwrap();
        HomeRask {
            orbits,
            envs,
            site,
            colony,
            recipes,
        }
    }

    /// `body_light` over the REAL system + the shipped rask-mines
    /// pose: synodic = n_rask − n_terrin ≈ 4.4525e-5 rad/s → a
    /// ~141 115 s solar day, and the 2.4218 rad spawn longitude sits
    /// ~0.72 rad into the day side (|cos H0| = 0.751 — the real
    /// margin the terminator verifier recommends, not the +6.1e-17
    /// residue π/2 left).
    #[test]
    fn authored_rask_light_is_parent_locked() {
        let fx = home_rask();
        let light = fx.light();
        // The C09-F3/C34-F3 verified numbers: synodic 4.4525e-5 →
        // 141 115 s solar day, 70 558 s night.
        let n_rask = TWO_PI / 138984.37657447575;
        let n_terrin = TWO_PI / 9203835.510438265;
        assert_eq!(light.synodic_rate, n_rask - n_terrin);
        let solar_day = TWO_PI / light.synodic_rate;
        assert!(
            (solar_day - 141_115.0).abs() < 100.0,
            "solar day {solar_day} s vs the verified 141 115 s"
        );
        // Daylit at epoch with a real terminator margin — and the
        // crossing clock runs.
        assert_eq!(daylight_at(&light, 0), Daylight::Day);
        assert!(libm::cos(light.h0) * light.cos_lat > 0.5);
        assert!(next_terminator_tick(&light, 0).is_some());
    }

    /// The 3.6 rebalance, end to end (C34-F3): the REAL rask-mines
    /// def — the 340 kEC battery over the 4.5 EC/s × 70 558 s =
    /// 317 511 EC bound — founded at a dusk longitude so its modules
    /// actually run through a full night. It must ride the whole
    /// night Nominal (no `Degraded{PowerLost}` window is authored),
    /// drain but never empty the battery, and cross a real
    /// `TerminatorCrossing` into a recharging dawn.
    #[test]
    fn authored_rask_outpost_rides_the_night() {
        let mut fx = home_rask();
        // A dusk-anchored clone of the authored site: h0 ≈ π/2 + 0.09
        // at lon 4.8 → Night at founding with the whole ~70 500 s
        // night ahead (the authored 2.42 rad site saturates its stores
        // in the first ~2 ks of a 51.9 ks day and would idle the
        // night away — this pose forces the battery to carry real
        // production through it).
        fx.site.longitude_rad = 4.8;
        let light = fx.light();
        let mut w = World::with_system(fx.orbits);
        w.set_colony_env(fx.envs);
        w.set_sites(BTreeMap::from([(fx.site.id.clone(), fx.site)]));
        w.set_recipes(
            fx.recipes
                .recipes
                .iter()
                .map(|r| (r.def.id.clone(), r.def.clone()))
                .collect(),
        );
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::FoundColony { def: fx.colony },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        let id = *w.colonies().keys().next().unwrap();
        assert!(
            r.rejected.is_empty() && r.degraded.is_empty(),
            "founding errors: {:?} {:?}",
            r.rejected,
            r.degraded
        );
        let colony = &w.colonies()[&id];
        assert_eq!(colony.daylight, Daylight::Night);
        assert_eq!(colony.state, ColonyState::Nominal);
        // The battery IS the authored capacity — the rebalance landed.
        let ec_id = ec();
        let cap = colony.stores[&ec_id].capacity;
        assert_eq!(cap, 340_000.0);
        // Dawn = the next terminator crossing (~68.6 ks out); step to
        // one tick before it and read the battery mid-flight.
        let dawn = next_terminator_tick(&light, 0).unwrap();
        let pre_dawn = dawn - 1;
        let mut fired = Vec::new();
        while w.clock().ticks() < pre_dawn {
            let r = w.step_frame(pre_dawn).unwrap();
            fired.extend(r.fired_events);
        }
        let colony = &w.colonies()[&id];
        let level = colony.stores[&ec_id].level_at(pre_dawn);
        // The drill alone draws 2.0 EC/s for the whole night (ore
        // never saturates — the smelter drains it as fast as it
        // lands) ⇒ ≥137 kEC must have left the battery; the authored
        // bound (4.5 EC/s) caps it at 317 511 — well under capacity.
        assert!(level < cap - 100_000.0, "battery never drained: {level}");
        assert!(level > 0.0, "battery emptied at {level}");
        assert_eq!(colony.daylight, Daylight::Night);
        assert_eq!(colony.state, ColonyState::Nominal);
        // Past dawn: Day, still Nominal, charging again — and the
        // whole run fired no PowerLost (a Degraded window would have
        // left one in the log).
        let target = dawn + 500 * TICK_HZ;
        while w.clock().ticks() < target {
            let r = w.step_frame(target).unwrap();
            fired.extend(r.fired_events);
        }
        let colony = &w.colonies()[&id];
        assert_eq!(colony.daylight, Daylight::Day);
        assert_eq!(colony.state, ColonyState::Nominal);
        assert!(
            colony.stores[&ec_id].net_rate_per_s > 0.0,
            "dawn recharge: net {}",
            colony.stores[&ec_id].net_rate_per_s
        );
        assert!(
            fired.iter().any(
                |e| matches!(e.kind, EventKind::TerminatorCrossing { colony } if colony == id)
            ),
            "no terminator crossing fired"
        );
        assert!(
            !fired
                .iter()
                .any(|e| matches!(e.kind, EventKind::PowerLost { colony } if colony == id)),
            "the authored outpost must not degrade: {fired:?}"
        );
    }

    /// Rule 3's allocation: scarce supply pro-rates the remainder by
    /// request, sorted (priority, ModuleIndex) — full grants first.
    #[test]
    fn allocation_prorates_the_remainder() {
        let ec_id = ec();
        // One generator at 10 EC/s; two converters drawing 8 + 4.
        let mut recipes = BTreeMap::new();
        let conc: BTreeMap<ResourceId, f64> = BTreeMap::new();
        let modules = vec![
            ColonyModule {
                def: gen_module(10.0),
                damaged: false,
            },
            ColonyModule {
                def: conv(
                    &mut recipes,
                    "recipe.t7",
                    vec![(ec_id.clone(), 8.0)],
                    vec![(ResourceId::from("ore"), 1.0)],
                    5,
                    0.0,
                ),
                damaged: false,
            },
            ColonyModule {
                def: conv(
                    &mut recipes,
                    "recipe.t8",
                    vec![(ec_id.clone(), 4.0)],
                    vec![(ResourceId::from("metal"), 1.0)],
                    5,
                    0.0,
                ),
                damaged: false,
            },
        ];
        let light = BodyLight {
            synodic_rate: 0.0,
            h0: 0.0,
            day_insolation_w_m2: 0.0,
            cos_lat: 1.0,
        };
        let levels = BTreeMap::new();
        let solve = solve_network(&inp(
            &modules,
            ColonyState::Nominal,
            &levels,
            &all_caps(),
            &light,
            0,
            Daylight::Day,
            &recipes,
            &conc,
            &crew_demands(),
        ));
        // Demand 12, supply 10 — grants run in (priority,
        // ModuleIndex) order: module 1 fully funded (8), module 2 is
        // the one unfunded consumer and takes the pro-rata remainder
        // (2/4 → fraction 0.5).
        assert!((solve.fractions[1] - 1.0).abs() < 1e-12);
        assert!((solve.fractions[2] - 0.5).abs() < 1e-12);
        assert!((solve.net_rates[&ec_id]).abs() < 1e-9);
        assert!(!solve.power_starved);
    }

    /// Full grants come first in (priority, ModuleIndex) order; the
    /// resource binding lands on the LAST consumer in the order.
    #[test]
    fn allocation_honours_priority_order() {
        let ec_id = ec();
        let mut recipes = BTreeMap::new();
        let conc: BTreeMap<ResourceId, f64> = BTreeMap::new();
        let modules = vec![
            ColonyModule {
                def: gen_module(10.0),
                damaged: false,
            },
            ColonyModule {
                def: conv(
                    &mut recipes,
                    "recipe.t9",
                    vec![(ec_id.clone(), 6.0)],
                    vec![],
                    9,
                    0.0,
                ),
                damaged: false,
            },
            ColonyModule {
                def: conv(
                    &mut recipes,
                    "recipe.t10",
                    vec![(ec_id.clone(), 6.0)],
                    vec![],
                    1,
                    0.0,
                ),
                damaged: false,
            },
        ];
        let light = BodyLight {
            synodic_rate: 0.0,
            h0: 0.0,
            day_insolation_w_m2: 0.0,
            cos_lat: 1.0,
        };
        let solve = solve_network(&inp(
            &modules,
            ColonyState::Nominal,
            &BTreeMap::new(),
            &all_caps(),
            &light,
            0,
            Daylight::Day,
            &recipes,
            &conc,
            &crew_demands(),
        ));
        // Priority 1 (module 2) fully funded first — 6 of 10; module 1
        // (priority 9) shares the remainder pro-rata by request —
        // it's the only unfunded consumer, so it gets 4.
        assert!((solve.fractions[2] - 1.0).abs() < 1e-12);
        assert!((solve.fractions[1] - 4.0 / 6.0).abs() < 1e-12);
    }

    /// Rule 2's algebraic T_eq — solved, held, never integrated.
    #[test]
    fn thermal_steady_state_is_algebraic() {
        let mut recipes = BTreeMap::new();
        let conc: BTreeMap<ResourceId, f64> = BTreeMap::new();
        let modules = vec![
            ColonyModule {
                def: gen_module(0.0),
                damaged: false,
            },
            ColonyModule {
                def: conv(
                    &mut recipes,
                    "recipe.t11",
                    vec![(ec(), 100.0)],
                    vec![],
                    0,
                    0.0,
                ),
                damaged: false,
            },
            ColonyModule {
                def: ColonyModuleDef::Radiator {
                    area_m2: 10.0,
                    emissivity: 0.9,
                },
                damaged: false,
            },
        ];
        // Feed the converter from a stocked store so it runs at 1.0.
        let mut levels = BTreeMap::new();
        levels.insert(ec(), 1.0);
        let light = BodyLight {
            synodic_rate: 0.0,
            h0: 0.0,
            day_insolation_w_m2: 0.0,
            cos_lat: 1.0,
        };
        let solve = solve_network(&inp(
            &modules,
            ColonyState::Nominal,
            &levels,
            &all_caps(),
            &light,
            0,
            Daylight::Day,
            &recipes,
            &conc,
            &crew_demands(),
        ));
        let expected = libm::sqrt(libm::sqrt(100.0 / (STEFAN_BOLTZMANN * 9.0)));
        assert!((solve.t_eq_k.unwrap() - expected).abs() < 1e-9);
        // No radiator → None (never a non-finite record).
        let bare = solve_network(&inp(
            &modules[..2],
            ColonyState::Nominal,
            &levels,
            &all_caps(),
            &light,
            0,
            Daylight::Day,
            &recipes,
            &conc,
            &crew_demands(),
        ));
        assert_eq!(bare.t_eq_k, None);
    }

    /// Rule 4's next event is the min over store bounds — emptying
    /// and filling — computed from the epoch-anchored linear rates.
    #[test]
    fn next_event_is_min_over_bounds() {
        let mut w = colony_world();
        // Generator 1 EC/s, battery 100, drill draws 0.4 EC/s → net
        // +0.6/s → fills at (100−L)/0.6 s.
        found(
            &mut w,
            vec![
                gen_module(1.0),
                ColonyModuleDef::Battery { capacity: 100.0 },
                ColonyModuleDef::Drill {
                    resource: ResourceId::from("ore"),
                    base_rate_per_s: 0.0,
                    min_concentration: 1.0,
                    power_ec_per_s: 0.4,
                },
            ],
            vec![csp_data::ColonyStoreInit {
                resource: ec(),
                level: 40.0,
            }],
        );
        let colony = w.colonies().values().next().unwrap();
        let store = &colony.stores[&ec()];
        assert!((store.net_rate_per_s - 0.6).abs() < 1e-9);
        // Fill at 40 + 0.6·dt = 100 → dt = 100 s → 6400 ticks.
        let full_tick = store.anchor_tick + 6400;
        let ev = w
            .event_heap
            .iter()
            .filter(|e| matches!(e.kind, EventKind::ColonyStoreFull { .. }))
            .map(|e| e.tick)
            .next();
        assert_eq!(ev, Some(full_tick));
    }

    /// amount = rate·dt literally: store levels read linearly between
    /// anchors across arbitrary partitions.
    #[test]
    fn levels_are_literal_linear() {
        let store = ColonyStore {
            anchor_level: 40.0,
            anchor_tick: 100,
            capacity: 100.0,
            net_rate_per_s: 0.6,
        };
        let l1 = store.level_at(100 + 640);
        assert!((l1 - 46.0).abs() < 1e-9);
        let l2 = store.level_at(100 + 6400);
        assert_eq!(l2, 100.0); // clamped at cap
    }

    /// The PowerLost → Degraded → PowerRestored → Nominal cycle:
    /// night starves the EC consumers, day restores.
    #[test]
    fn power_loss_cycle_transitions() {
        let mut w = colony_world();
        // Solar-only colony founded at lon 0 (midnight at epoch) with
        // a drill drawing EC — founded into Night → power-starved.
        let mut r = found(
            &mut w,
            vec![
                solar_def(1.0),
                ColonyModuleDef::Drill {
                    resource: ResourceId::from("ore"),
                    base_rate_per_s: 1.0,
                    min_concentration: 1.0,
                    power_ec_per_s: 0.5,
                },
            ],
            vec![],
        );
        let id = *w.colonies().keys().next().unwrap();
        // Founded at night with no EC production → PowerLost lands a
        // tick out (it may already have fired inside `found`'s own
        // step — check both reports).
        r.fired_events
            .extend(w.step_frame(w.clock().ticks() + 64).unwrap().fired_events);
        assert!(
            r.fired_events
                .iter()
                .any(|e| matches!(e.kind, EventKind::PowerLost { colony } if colony == id))
        );
        assert!(matches!(
            w.colonies()[&id].state,
            ColonyState::Degraded {
                reason: DegradeReason::PowerLost
            }
        ));
        // Advance past sunrise — the terminator toggles, the drill
        // stays off (Degraded), solar recharges, PowerRestored lands.
        let light = body_light(
            &BodyId::from("terra"),
            0.0,
            0.0,
            &test_system(),
            &test_env()[&BodyId::from("terra")],
        );
        let sunrise = next_terminator_tick(&light, w.clock().ticks()).unwrap();
        // Loop over the frame tick budget — sunrise sits ~350k ticks
        // out, beyond one frame's 69,120-tick cap.
        let mut events = Vec::new();
        while w.clock().ticks() < sunrise + 64 {
            let r = w.step_frame(sunrise + 64).unwrap();
            events.extend(r.fired_events);
            if !r.partial {
                break;
            }
        }
        let r = &events;
        assert!(
            r.iter().any(
                |e| matches!(e.kind, EventKind::TerminatorCrossing { colony } if colony == id)
            )
        );
        assert!(
            r.iter()
                .any(|e| matches!(e.kind, EventKind::PowerRestored { colony } if colony == id))
        );
        assert_eq!(w.colonies()[&id].state, ColonyState::Nominal);
        assert_eq!(w.colonies()[&id].daylight, Daylight::Day);
    }

    /// LifeSupportDepleted with crew aboard → crew Lost → Dormant →
    /// the colony schedules nothing ever again (rule 5).
    #[test]
    fn ls_depletion_kills_and_dormants() {
        let mut w = colony_world();
        // Hire one crew; assign to the colony; LS needs oxygen with
        // no store → depleted at founding solve.
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::HireCrew {
                name: "jeb".into(),
                kind: crate::CrewKind::Engineer,
                skill: 1,
                salary_per_s: 1.0,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::LifeSupport {
                    consumes_per_crew: vec![(ResourceId::from("oxygen"), 0.1)],
                    power_ec_per_s: 0.0,
                },
            ],
            vec![],
        );
        let id = *w.colonies().keys().next().unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::AssignCrewToColony {
                colony: id,
                crew: CrewId(0),
            },
        );
        let r = w.step_frame(w.clock().ticks() + 64).unwrap();
        assert!(
            r.fired_events.iter().any(
                |e| matches!(e.kind, EventKind::LifeSupportDepleted { colony } if colony == id)
            )
        );
        assert_eq!(w.colonies()[&id].state, ColonyState::Dormant);
        assert_eq!(w.economy().crew[&CrewId(0)].status, CrewStatus::Lost);
        // Dormant schedules NOTHING — the heap holds no event naming
        // the colony, and none appears across a long warp.
        assert!(
            !w.event_heap
                .iter()
                .any(|e| event_colony(&e.kind) == Some(id))
        );
        let r = w.step_frame(w.clock().ticks() + 2_000_000).unwrap();
        assert!(
            !r.fired_events
                .iter()
                .any(|e| event_colony(&e.kind) == Some(id))
        );
        assert_eq!(w.colonies()[&id].crew.len(), 0);
    }

    /// AbandonColony → Abandoned, salvage at abandon_recovery, crew
    /// home, no events.
    #[test]
    fn abandon_salvages_and_silences() {
        let mut w = colony_world();
        found(
            &mut w,
            vec![
                gen_module(1.0),
                ColonyModuleDef::Store {
                    resource: ResourceId::from("ore"),
                    capacity: 100.0,
                },
            ],
            vec![csp_data::ColonyStoreInit {
                resource: ResourceId::from("ore"),
                level: 40.0,
            }],
        );
        let id = *w.colonies().keys().next().unwrap();
        w.enqueue(SourceClass::Ui, 0, SimCommand::AbandonColony { colony: id });
        w.step_frame(w.clock().ticks() + 1).unwrap();
        assert_eq!(w.colonies()[&id].state, ColonyState::Abandoned);
        // 40 × 0.25 = 10 credited to the pad's inventory.
        let inv = &w.economy().sites[&SiteId::from("pad")].inventory;
        assert!((inv[&ResourceId::from("ore")] - 10.0).abs() < 1e-9);
        assert!(
            !w.event_heap
                .iter()
                .any(|e| event_colony(&e.kind) == Some(id))
        );
    }

    /// The §2.6.5 pathological-density check at M8-A scale: 20
    /// colonies × 6 converters × small buffers must not drown the
    /// 2000-event frame budget — event counts stay a handful per
    /// colony per day.
    #[test]
    fn twenty_colonies_stay_inside_the_event_budget() {
        let mut w = colony_world();
        // 20 sites → 20 pads: clone the site def under distinct ids.
        let mut sites = BTreeMap::new();
        for i in 0..20 {
            let mut s = test_site();
            s.id = SiteId::from(format!("pad{i}"));
            s.longitude_rad = i as f64 * 0.1;
            sites.insert(s.id.clone(), s);
        }
        w.set_sites(sites);
        let mut recipes = BTreeMap::new();
        for i in 0..20 {
            let mut modules = vec![
                ColonyModuleDef::Solar {
                    area_m2: 5.0,
                    efficiency: 0.2,
                },
                ColonyModuleDef::Battery { capacity: 50.0 },
            ];
            for j in 0..6 {
                modules.push(conv(
                    &mut recipes,
                    "recipe.t12",
                    vec![(ResourceId::from("ore"), 0.01)],
                    vec![(ResourceId::from("metal"), 0.005)],
                    j as u8,
                    0.05,
                ));
            }
            modules.push(ColonyModuleDef::Store {
                resource: ResourceId::from("ore"),
                capacity: 10.0,
            });
            modules.push(ColonyModuleDef::Store {
                resource: ResourceId::from("metal"),
                capacity: 10.0,
            });
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::FoundColony {
                    def: ColonyDef {
                        site: SiteId::from(format!("pad{i}")),
                        display_name: csp_data::LocKey::from("colony.x"),
                        modules,
                        stores: vec![csp_data::ColonyStoreInit {
                            resource: ResourceId::from("ore"),
                            level: 5.0,
                        }],
                    },
                },
            );
        }
        w.set_recipes(recipes);
        w.step_frame(w.clock().ticks() + 1).unwrap();
        assert_eq!(w.colonies().len(), 20);
        // One sim-week: 7 × 21600 s × 64 ticks. The event load is the
        // terminator pairs (2/colony/day ≈ 280) + sparse store bounds —
        // far under the 2000/frame budget with zero partials.
        let target = w.clock().ticks() + (7 * 21600 * 64) as u64;
        let mut total_events = 0u64;
        let mut clock = w.clock().ticks();
        while clock < target {
            // The stride is the per-frame TICK budget — the event
            // budget bounds firings, not advance.
            let r = w
                .step_frame((clock + MAX_TICKS_PER_FRAME).min(target))
                .unwrap();
            total_events += r.events_processed;
            assert!(!r.partial);
            clock = r.reached_tick;
        }
        // ≤ a few hundred events for the whole week across 20
        // colonies — the budget is 2000 PER FRAME.
        assert!(total_events < 1_500, "events: {total_events}");
        // No colony event is ever Clamping.
        assert!(!w.event_heap.iter().any(|e| event_colony(&e.kind).is_some()
            && e.class == crate::EventClass::Clamping
            && !matches!(
                e.kind,
                EventKind::PowerLost { .. }
                    | EventKind::PowerRestored { .. }
                    | EventKind::LifeSupportDepleted { .. }
            )));
    }

    /// Snapshot → restore round-trips the colony arena and keeps the
    /// hash; two identical runs hash identically (determinism).
    #[test]
    fn serialization_hash_determinism() {
        let run = || {
            let mut w = colony_world();
            found(
                &mut w,
                vec![
                    ColonyModuleDef::Solar {
                        area_m2: 2.0,
                        efficiency: 0.5,
                    },
                    ColonyModuleDef::Battery { capacity: 200.0 },
                    ColonyModuleDef::Drill {
                        resource: ResourceId::from("ore"),
                        base_rate_per_s: 0.5,
                        min_concentration: 1.0,
                        power_ec_per_s: 0.3,
                    },
                    ColonyModuleDef::Store {
                        resource: ResourceId::from("ore"),
                        capacity: 500.0,
                    },
                ],
                vec![csp_data::ColonyStoreInit {
                    resource: ec(),
                    level: 100.0,
                }],
            );
            w
        };
        let mut a = run();
        let b = run();
        let h1 = a.state_hash().unwrap();
        let h2 = b.state_hash().unwrap();
        assert_eq!(h1, h2);
        a.step_frame(20_000).unwrap();
        let snap = a.snapshot().unwrap();
        let mut r = World::restore(test_system(), None, snap).unwrap();
        r.set_sites(BTreeMap::from([(SiteId::from("pad"), test_site())]));
        r.set_colony_env(test_env());
        r.step_frame(30_000).unwrap();
        a.step_frame(30_000).unwrap();
        assert_eq!(a.state_hash().unwrap(), r.state_hash().unwrap());
    }

    /// A drained battery powering the drill — the modules most of the
    /// M8-A fix tests build on.
    fn draining_world(level: f64, power: f64) -> World {
        let mut w = colony_world();
        found(
            &mut w,
            vec![
                gen_module(1.0),
                ColonyModuleDef::Battery { capacity: 100.0 },
                ColonyModuleDef::Drill {
                    resource: ResourceId::from("ore"),
                    base_rate_per_s: 0.5,
                    min_concentration: 1.0,
                    power_ec_per_s: power,
                },
                ColonyModuleDef::Store {
                    resource: ResourceId::from("ore"),
                    capacity: 500.0,
                },
            ],
            vec![csp_data::ColonyStoreInit {
                resource: ec(),
                level,
            }],
        );
        w
    }

    /// C1: the published store-bound tick is computed from the anchor —
    /// identical for EVERY query tick inside `[anchor, bound)` (the
    /// partition independence the `Colonies` publish relies on; the
    /// `anchor + dt_from_now` form drifted the bound back by
    /// `now − anchor`).
    #[test]
    fn store_bound_is_query_tick_independent() {
        // Net +0.7 EC/s — the (100−40)/0.7 crossing sits at a
        // non-integral tick so the ceil bound is unambiguous.
        let w = draining_world(40.0, 0.3);
        let colony = w.colonies().values().next().unwrap();
        let bound = earliest_store_bound(colony, 0).unwrap();
        for t in [1, 100, 1_000, 3_000, 5_000] {
            assert_eq!(
                earliest_store_bound(colony, t),
                Some(bound.clone()),
                "t={t}"
            );
        }
        // The same invariance through `next_boundary` — the store
        // bound is the min here (the terminator sits ~691k out).
        let light = body_light(
            &BodyId::from("terra"),
            0.0,
            0.0,
            &test_system(),
            &test_env()[&BodyId::from("terra")],
        );
        let b0 = next_boundary(colony, &light, 0);
        for t in [1, 100, 1_000, 3_000, 5_000] {
            assert_eq!(next_boundary(colony, &light, t), b0, "t={t}");
        }
    }

    /// §2.6.1: the same advance split differently hashes identically —
    /// one continuous run vs 10k-tick frame-steps. C1's drifted bound
    /// made the publish repair fire junk `ColonyInspection`s at
    /// partition-dependent ticks — a hash divergence under warp.
    #[test]
    fn colony_advance_is_partition_independent() {
        let mut a = draining_world(40.0, 0.3);
        let mut b = draining_world(40.0, 0.3);
        let target = 800_000u64; // past the first terminator (~691k)
        while a.clock().ticks() < target {
            a.step_frame(target).unwrap();
        }
        while b.clock().ticks() < target {
            let next = (b.clock().ticks() + 10_000).min(target);
            b.step_frame(next).unwrap();
        }
        assert_eq!(a.clock().ticks(), b.clock().ticks());
        assert_eq!(a.state_hash().unwrap(), b.state_hash().unwrap());
    }

    /// Steady-state publish emits ZERO repairs: a correctly-published
    /// colony's earliest needed boundary is always already covered, so
    /// no `ColonyInspection` (the repair kind) ever appears unless the
    /// player authored one.
    #[test]
    fn publish_needs_no_repairs_in_steady_state() {
        let mut w = draining_world(40.0, 0.3);
        let target = 800_000u64;
        while w.clock().ticks() < target {
            let next = (w.clock().ticks() + 10_000).min(target);
            let r = w.step_frame(next).unwrap();
            assert!(
                !r.fired_events
                    .iter()
                    .any(|e| matches!(e.kind, EventKind::ColonyInspection { .. })),
                "repair inspection fired by {}",
                w.clock().ticks()
            );
            assert!(
                !w.event_heap
                    .iter()
                    .any(|e| matches!(e.kind, EventKind::ColonyInspection { .. })),
                "repair inspection pending at {}",
                w.clock().ticks()
            );
        }
    }

    /// H2: a converter eating its own output cannot bootstrap — the
    /// grounding least-fixpoint pins it at fraction 0 (the old
    /// greatest-fixpoint held `f = 1` forever, minting `gold` from
    /// nothing).
    #[test]
    fn self_feeding_converter_pins_at_zero() {
        let mut recipes = BTreeMap::new();
        let conc: BTreeMap<ResourceId, f64> = BTreeMap::new();
        let modules = vec![ColonyModule {
            def: conv(
                &mut recipes,
                "recipe.t13",
                vec![(ResourceId::from("x"), 1.0)],
                vec![
                    (ResourceId::from("x"), 1.0),
                    (ResourceId::from("gold"), 1.0),
                ],
                5,
                0.0,
            ),
            damaged: false,
        }];
        let light = BodyLight {
            synodic_rate: 0.0,
            h0: 0.0,
            day_insolation_w_m2: 0.0,
            cos_lat: 1.0,
        };
        let solve = solve_network(&inp(
            &modules,
            ColonyState::Nominal,
            &BTreeMap::new(),
            &all_caps(),
            &light,
            0,
            Daylight::Day,
            &recipes,
            &conc,
            &crew_demands(),
        ));
        assert_eq!(solve.fractions, vec![0.0]);
        // Nothing produces or consumes → no net-rate entries at all.
        assert_eq!(
            solve
                .net_rates
                .get(&ResourceId::from("gold"))
                .copied()
                .unwrap_or(0.0),
            0.0
        );
        assert_eq!(
            solve
                .net_rates
                .get(&ResourceId::from("x"))
                .copied()
                .unwrap_or(0.0),
            0.0
        );
        // Nor can a source-less two-module cycle.
        let cyclic = vec![
            ColonyModule {
                def: conv(
                    &mut recipes,
                    "recipe.t14",
                    vec![(ResourceId::from("x"), 1.0)],
                    vec![(ResourceId::from("y"), 1.0)],
                    5,
                    0.0,
                ),
                damaged: false,
            },
            ColonyModule {
                def: conv(
                    &mut recipes,
                    "recipe.t15",
                    vec![(ResourceId::from("y"), 1.0)],
                    vec![(ResourceId::from("x"), 1.0)],
                    5,
                    0.0,
                ),
                damaged: false,
            },
        ];
        let solve = solve_network(&inp(
            &cyclic,
            ColonyState::Nominal,
            &BTreeMap::new(),
            &all_caps(),
            &light,
            0,
            Daylight::Day,
            &recipes,
            &conc,
            &crew_demands(),
        ));
        assert_eq!(solve.fractions, vec![0.0, 0.0]);
    }

    /// The grounding gate is not "no cycles" — a cycle ANCHORED by a
    /// real source (the generator's EC feeds the loop) still runs at
    /// rate; only ungrounded self-feeding pins at 0.
    #[test]
    fn grounded_cycle_still_runs() {
        let ec_id = ec();
        let x = ResourceId::from("x");
        let mut recipes = BTreeMap::new();
        let conc: BTreeMap<ResourceId, f64> = BTreeMap::new();
        let modules = vec![
            ColonyModule {
                def: gen_module(2.0),
                damaged: false,
            },
            ColonyModule {
                def: conv(
                    &mut recipes,
                    "recipe.t16",
                    vec![(ec_id.clone(), 1.0)],
                    vec![(x.clone(), 1.0)],
                    5,
                    0.0,
                ),
                damaged: false,
            },
            ColonyModule {
                def: conv(
                    &mut recipes,
                    "recipe.t17",
                    vec![(x.clone(), 1.0)],
                    vec![(ec_id.clone(), 1.0)],
                    5,
                    0.0,
                ),
                damaged: false,
            },
        ];
        let light = BodyLight {
            synodic_rate: 0.0,
            h0: 0.0,
            day_insolation_w_m2: 0.0,
            cos_lat: 1.0,
        };
        let solve = solve_network(&inp(
            &modules,
            ColonyState::Nominal,
            &BTreeMap::new(),
            &all_caps(),
            &light,
            0,
            Daylight::Day,
            &recipes,
            &conc,
            &crew_demands(),
        ));
        assert_eq!(solve.fractions, vec![1.0, 1.0, 1.0]);
    }

    /// C09-F1's verifier probe, end to end through `FoundColony`:
    /// `a 1.0→b 0.5` + `b 0.5→a 1.2` founding on `a = 10` is a
    /// stock-anchored net-positive cycle — grounding's "stocked OR
    /// produced" test used to run it at fraction 1 forever, minting
    /// +0.2 `a`/s (10 → 130 units in 600 s, bounded only by the
    /// store cap). `unanchored_mint_pins` now holds both converters
    /// at fraction 0 and the store never moves.
    #[test]
    fn unanchored_net_positive_cycle_pins_at_zero() {
        let a = ResourceId::from("a");
        let b = ResourceId::from("b");
        let mut w = colony_world();
        let mut recipes = BTreeMap::new();
        let modules = vec![
            ColonyModuleDef::Store {
                resource: a.clone(),
                capacity: 1000.0,
            },
            conv(
                &mut recipes,
                "recipe.ab",
                vec![(a.clone(), 1.0)],
                vec![(b.clone(), 0.5)],
                5,
                0.0,
            ),
            conv(
                &mut recipes,
                "recipe.ba",
                vec![(b.clone(), 0.5)],
                vec![(a.clone(), 1.2)],
                5,
                0.0,
            ),
        ];
        w.set_recipes(recipes);
        found(
            &mut w,
            modules,
            vec![csp_data::ColonyStoreInit {
                resource: a.clone(),
                level: 10.0,
            }],
        );
        let colony = w.colonies().values().next().unwrap();
        // The Store runs; both converters pin at fraction 0.
        assert_eq!(colony.module_fractions, vec![1.0, 0.0, 0.0]);
        assert!(colony.stores[&a].net_rate_per_s.abs() < EPS);
        // The probe's horizon — 600 s must not mint a single unit.
        w.step_frame(w.clock().ticks() + 600 * TICK_HZ).unwrap();
        let now = w.clock().ticks();
        let colony = w.colonies().values().next().unwrap();
        assert!(
            (colony.stores[&a].level_at(now) - 10.0).abs() < EPS,
            "store a minted to {}",
            colony.stores[&a].level_at(now)
        );
    }

    /// The pin names POSITIVE nets only: the same two-converter web
    /// at a loss (`a` back at 0.8 for 1.0 in) is ordinary conversion
    /// — stock-anchored, it runs at fraction 1 and drains the store
    /// it was founded on.
    #[test]
    fn net_negative_cycle_still_runs() {
        let a = ResourceId::from("a");
        let b = ResourceId::from("b");
        let mut recipes = BTreeMap::new();
        let conc: BTreeMap<ResourceId, f64> = BTreeMap::new();
        let modules = vec![
            ColonyModule {
                def: conv(
                    &mut recipes,
                    "recipe.t19",
                    vec![(a.clone(), 1.0)],
                    vec![(b.clone(), 0.5)],
                    5,
                    0.0,
                ),
                damaged: false,
            },
            ColonyModule {
                def: conv(
                    &mut recipes,
                    "recipe.t20",
                    vec![(b.clone(), 0.5)],
                    vec![(a.clone(), 0.8)],
                    5,
                    0.0,
                ),
                damaged: false,
            },
        ];
        let light = BodyLight {
            synodic_rate: 0.0,
            h0: 0.0,
            day_insolation_w_m2: 0.0,
            cos_lat: 1.0,
        };
        let levels = BTreeMap::from([(a.clone(), 10.0)]);
        let solve = solve_network(&inp(
            &modules,
            ColonyState::Nominal,
            &levels,
            &all_caps(),
            &light,
            0,
            Daylight::Day,
            &recipes,
            &conc,
            &crew_demands(),
        ));
        assert_eq!(solve.fractions, vec![1.0, 1.0]);
        // Net `a` = 0.8 out − 1.0 in = −0.2/s: the store drains.
        assert!((solve.net_rates[&a] + 0.2).abs() < EPS);
    }

    /// The exemption clause: a Drill/Solar/Generator member IS the
    /// web's sanctioned source. A drill→converter→EC cycle with a
    /// positive aggregate net still runs — the surplus is real ore
    /// pulled out of the ground, not minted stock.
    #[test]
    fn drill_membered_cycle_is_sanctioned() {
        let ore = ResourceId::from("ore");
        let ec_id = ec();
        let mut recipes = BTreeMap::new();
        let mut conc: BTreeMap<ResourceId, f64> = BTreeMap::new();
        conc.insert(ore.clone(), 1.0);
        let modules = vec![
            ColonyModule {
                def: ColonyModuleDef::Drill {
                    resource: ore.clone(),
                    base_rate_per_s: 1.0,
                    min_concentration: 1.0,
                    power_ec_per_s: 0.5,
                },
                damaged: false,
            },
            ColonyModule {
                def: conv(
                    &mut recipes,
                    "recipe.t21",
                    vec![(ore.clone(), 1.0)],
                    vec![(ec_id.clone(), 1.0)],
                    5,
                    0.0,
                ),
                damaged: false,
            },
        ];
        let light = BodyLight {
            synodic_rate: 0.0,
            h0: 0.0,
            day_insolation_w_m2: 0.0,
            cos_lat: 1.0,
        };
        // Founding ore stock anchors the web: the converter grounds
        // on the store, the drill grounds on the converter's EC.
        let levels = BTreeMap::from([(ore.clone(), 10.0)]);
        let solve = solve_network(&inp(
            &modules,
            ColonyState::Nominal,
            &levels,
            &all_caps(),
            &light,
            0,
            Daylight::Day,
            &recipes,
            &conc,
            &crew_demands(),
        ));
        assert_eq!(solve.fractions, vec![1.0, 1.0]);
    }

    /// H1: a converter whose input is absent runs at fraction 0 and
    /// draws NO power — consumption is charged at `fraction ·
    /// request`, not the raw grant that used to bill the full EC
    /// request anyway.
    #[test]
    fn starved_converter_draws_no_power() {
        let ec_id = ec();
        let mut recipes = BTreeMap::new();
        let conc: BTreeMap<ResourceId, f64> = BTreeMap::new();
        let modules = vec![
            ColonyModule {
                def: gen_module(2.0),
                damaged: false,
            },
            ColonyModule {
                def: conv(
                    &mut recipes,
                    "recipe.t18",
                    vec![(ResourceId::from("ore"), 1.0)],
                    vec![(ResourceId::from("metal"), 0.5)],
                    5,
                    0.5,
                ),
                damaged: false,
            },
        ];
        let light = BodyLight {
            synodic_rate: 0.0,
            h0: 0.0,
            day_insolation_w_m2: 0.0,
            cos_lat: 1.0,
        };
        // No ore store, no ore producer → the converter pins at 0 and
        // the EC store sees only the generator's +2/s.
        let solve = solve_network(&inp(
            &modules,
            ColonyState::Nominal,
            &BTreeMap::new(),
            &all_caps(),
            &light,
            0,
            Daylight::Day,
            &recipes,
            &conc,
            &crew_demands(),
        ));
        assert_eq!(solve.fractions[1], 0.0);
        assert!((solve.net_rates[&ec_id] - 2.0).abs() < 1e-9);
    }

    /// M1: `consumes: [(ec, 8)]` + `power_ec_per_s: 4` are ONE 12 EC/s
    /// request — the `(module, resource)` grant key would otherwise
    /// collide and silently drop the consumes half.
    #[test]
    fn duplicate_requests_fold_into_one_row() {
        let ec_id = ec();
        let mut recipes = BTreeMap::new();
        let conc: BTreeMap<ResourceId, f64> = BTreeMap::new();
        let modules = vec![
            ColonyModule {
                def: gen_module(12.0),
                damaged: false,
            },
            ColonyModule {
                def: conv(
                    &mut recipes,
                    "recipe.t19",
                    vec![(ec_id.clone(), 8.0)],
                    vec![],
                    5,
                    4.0,
                ),
                damaged: false,
            },
        ];
        let light = BodyLight {
            synodic_rate: 0.0,
            h0: 0.0,
            day_insolation_w_m2: 0.0,
            cos_lat: 1.0,
        };
        let solve = solve_network(&inp(
            &modules,
            ColonyState::Nominal,
            &BTreeMap::new(),
            &all_caps(),
            &light,
            0,
            Daylight::Day,
            &recipes,
            &conc,
            &crew_demands(),
        ));
        // Folded request = 12 EC/s = supply → the module runs full
        // (the colliding pair used to read fraction 0.5).
        assert_eq!(solve.fractions[1], 1.0);
        assert!(solve.net_rates[&ec_id].abs() < 1e-9);
    }

    /// M1's command-side belt: a `FoundColony` def authoring the same
    /// EC draw twice (consumes + power field) is refused before it can
    /// enter hashed state.
    #[test]
    fn founding_rejects_power_consumes_collision() {
        let mut w = colony_world();
        let mut recipes = BTreeMap::new();
        let r = found(
            &mut w,
            vec![conv(
                &mut recipes,
                "recipe.t20",
                vec![(ec(), 1.0)],
                vec![],
                5,
                0.5,
            )],
            vec![],
        );
        assert!(
            r.rejected
                .iter()
                .any(|r| matches!(r.error, SimError::Recoverable(_))),
            "{:?}",
            r.rejected
        );
        assert!(w.colonies().is_empty());
    }

    /// C2: two recomputes at ONE tick must not queue two notifications
    /// — the second solve sees the first's `LifeSupportDepleted`
    /// already pending. The heap keeps exactly one, the snapshot
    /// round-trips, and the transition fires once.
    #[test]
    fn duplicate_notifications_do_not_accumulate() {
        let mut w = colony_world();
        for (name, _) in [("a", 0), ("b", 1)] {
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::HireCrew {
                    name: name.into(),
                    kind: crate::CrewKind::Engineer,
                    skill: 1,
                    salary_per_s: 1.0,
                },
            );
        }
        w.step_frame(w.clock().ticks() + 1).unwrap();
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::LifeSupport {
                    consumes_per_crew: vec![(ResourceId::from("oxygen"), 0.1)],
                    power_ec_per_s: 0.0,
                },
            ],
            vec![],
        );
        let id = *w.colonies().keys().next().unwrap();
        // Two same-tick applies → two recomputes at `now`. Applied
        // directly (not via the queue) so the pending notification is
        // inspectable before its tick.
        let mut shadow = TickShadow::capture(&w);
        w.apply_assign_crew(id, CrewId(0), &mut shadow).unwrap();
        w.apply_assign_crew(id, CrewId(1), &mut shadow).unwrap();
        let pending = w
            .event_heap
            .iter()
            .filter(|e| matches!(e.kind, EventKind::LifeSupportDepleted { colony } if colony == id))
            .count();
        assert_eq!(pending, 1);
        // The pairing walk accepts the snapshot — and the hash survives.
        let snap = w.snapshot().unwrap();
        let mut r = World::restore(test_system(), None, snap).unwrap();
        r.set_sites(BTreeMap::from([(SiteId::from("pad"), test_site())]));
        r.set_colony_env(test_env());
        assert_eq!(w.state_hash().unwrap(), r.state_hash().unwrap());
        // It fires exactly once → both crew Lost → Dormant.
        let rep = w.step_frame(w.clock().ticks() + 4).unwrap();
        assert_eq!(
            rep.fired_events
                .iter()
                .filter(
                    |e| matches!(e.kind, EventKind::LifeSupportDepleted { colony } if colony == id)
                )
                .count(),
            1
        );
        assert_eq!(w.colonies()[&id].state, ColonyState::Dormant);
        assert_eq!(w.economy().crew[&CrewId(0)].status, CrewStatus::Lost);
        assert_eq!(w.economy().crew[&CrewId(1)].status, CrewStatus::Lost);
    }

    /// M2: Dormant freezes the stores — re-anchored with zero net
    /// rates at the transition, so a later `AbandonColony` prices the
    /// dormant-tick levels, not phantom fill accrued while dead.
    #[test]
    fn dormant_freezes_stores() {
        let mut w = colony_world();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::HireCrew {
                name: "a".into(),
                kind: crate::CrewKind::Engineer,
                skill: 1,
                salary_per_s: 1.0,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::Drill {
                    resource: ResourceId::from("ore"),
                    base_rate_per_s: 1.0,
                    min_concentration: 1.0,
                    power_ec_per_s: 1.0,
                },
                ColonyModuleDef::Store {
                    resource: ResourceId::from("ore"),
                    capacity: 1_000.0,
                },
                ColonyModuleDef::Store {
                    resource: ResourceId::from("oxygen"),
                    capacity: 100.0,
                },
                ColonyModuleDef::LifeSupport {
                    consumes_per_crew: vec![(ResourceId::from("oxygen"), 0.1)],
                    power_ec_per_s: 0.0,
                },
            ],
            vec![csp_data::ColonyStoreInit {
                resource: ResourceId::from("oxygen"),
                level: 5.0,
            }],
        );
        let id = *w.colonies().keys().next().unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::AssignCrewToColony {
                colony: id,
                crew: CrewId(0),
            },
        );
        // O2 drains 0.1/s → empty ≈ tick 3200 → dormant shortly after.
        w.step_frame(w.clock().ticks() + 10_000).unwrap();
        assert_eq!(w.colonies()[&id].state, ColonyState::Dormant);
        let ore = ResourceId::from("ore");
        let frozen = w.colonies()[&id].stores[&ore].level_at(w.clock().ticks());
        assert!(frozen > 1.0, "the ore fill was mid-flight: {frozen}");
        // A long warp changes nothing — the level is frozen exactly.
        w.step_frame(w.clock().ticks() + 200_000).unwrap();
        assert_eq!(
            w.colonies()[&id].stores[&ore].level_at(w.clock().ticks()),
            frozen
        );
        w.enqueue(SourceClass::Ui, 0, SimCommand::AbandonColony { colony: id });
        w.step_frame(w.clock().ticks() + 1).unwrap();
        let inv = w.economy().sites[&SiteId::from("pad")].inventory[&ore];
        let recovery = w.economy_cfg.abandon_recovery;
        assert!((inv - frozen * recovery).abs() < 1e-9, "{inv} vs {frozen}");
    }

    /// M3: an inspection deadline landing on the same tick as an
    /// earlier boundary is CONSUMED by that boundary's recompute — the
    /// pending inspection event is scrubbed and the slot does not
    /// linger as `Some(past)`.
    #[test]
    fn past_inspection_deadline_is_consumed() {
        let mut w = colony_world();
        // Drill drains the battery at 1 EC/s: 64 units →
        // ColonyStoreEmpty at tick 4096 — the tick the inspection is
        // authored for below.
        found(
            &mut w,
            vec![
                ColonyModuleDef::Battery { capacity: 10_000.0 },
                ColonyModuleDef::Drill {
                    resource: ResourceId::from("ore"),
                    base_rate_per_s: 0.5,
                    min_concentration: 1.0,
                    power_ec_per_s: 1.0,
                },
                ColonyModuleDef::Store {
                    resource: ResourceId::from("ore"),
                    capacity: 500.0,
                },
            ],
            vec![csp_data::ColonyStoreInit {
                resource: ec(),
                level: 64.0,
            }],
        );
        let id = *w.colonies().keys().next().unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::ScheduleColonyInspection {
                colony: id,
                tick: 4096,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        assert_eq!(w.colonies()[&id].inspection_tick, Some(4096));
        // At 4096 the store-empty event (lower seq) fires first; its
        // recompute consumes the deadline — it never leaks.
        while w.clock().ticks() < 4096 + 64 {
            w.step_frame(4096 + 64).unwrap();
        }
        assert_eq!(w.colonies()[&id].inspection_tick, None);
        assert!(
            !w.event_heap
                .iter()
                .any(|e| matches!(e.kind, EventKind::ColonyInspection { .. }))
        );
    }

    /// L1: a damaged LifeSupport module serves nothing — its grant is
    /// 0, so crew aboard deplete on schedule. The `active[i]` skip
    /// used to leave them immortal.
    #[test]
    fn damaged_life_support_kills() {
        let mut w = colony_world();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::HireCrew {
                name: "a".into(),
                kind: crate::CrewKind::Engineer,
                skill: 1,
                salary_per_s: 1.0,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        // O2 comes from a drill (no O2 store): while the LS is intact
        // its grant covers the request — the colony is NOT depleted.
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::Drill {
                    resource: ResourceId::from("oxygen"),
                    base_rate_per_s: 1.0,
                    min_concentration: 1.0,
                    power_ec_per_s: 1.0,
                },
                ColonyModuleDef::LifeSupport {
                    consumes_per_crew: vec![(ResourceId::from("oxygen"), 0.1)],
                    power_ec_per_s: 0.0,
                },
            ],
            vec![],
        );
        let id = *w.colonies().keys().next().unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::AssignCrewToColony {
                colony: id,
                crew: CrewId(0),
            },
        );
        w.step_frame(w.clock().ticks() + 4).unwrap();
        assert_eq!(w.economy().crew[&CrewId(0)].status, CrewStatus::Active);
        // Damage the LifeSupport module (ModuleIndex 2) — grant 0 on
        // an empty store → LifeSupportDepleted → crew Lost → Dormant.
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::DamageColonyModule {
                colony: id,
                module: 2,
            },
        );
        let r = w.step_frame(w.clock().ticks() + 4).unwrap();
        assert!(
            r.fired_events.iter().any(
                |e| matches!(e.kind, EventKind::LifeSupportDepleted { colony } if colony == id)
            )
        );
        assert_eq!(w.colonies()[&id].state, ColonyState::Dormant);
        assert_eq!(w.economy().crew[&CrewId(0)].status, CrewStatus::Lost);
    }

    /// R9-F1: a crewed `LifeSupport` module whose `power_ec_per_s`
    /// request can be served by neither stock nor production is
    /// unsatisfied life support — a stocked O2 reserve must NOT
    /// freeze its crew alive (the power-pinned module runs at
    /// nothing, and an unserved request is depletion evidence). The
    /// scan walks the module's whole request set now — the EC row
    /// included — so the crew depletes on schedule.
    #[test]
    fn unpowered_life_support_kills_crew() {
        let mut w = colony_world();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::HireCrew {
                name: "a".into(),
                kind: crate::CrewKind::Engineer,
                skill: 1,
                salary_per_s: 1.0,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        // O2 stocked at 50, but no EC anywhere — the LS module's
        // power request is unservable, pinning it ungrounded. Under
        // the old scan the stocked O2 read as "reserves cover it"
        // and the crew lived forever on a module that never ran.
        found(
            &mut w,
            vec![
                ColonyModuleDef::Store {
                    resource: ResourceId::from("oxygen"),
                    capacity: 100.0,
                },
                ColonyModuleDef::LifeSupport {
                    consumes_per_crew: vec![(ResourceId::from("oxygen"), 0.1)],
                    power_ec_per_s: 0.5,
                },
            ],
            vec![csp_data::ColonyStoreInit {
                resource: ResourceId::from("oxygen"),
                level: 50.0,
            }],
        );
        let id = *w.colonies().keys().next().unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::AssignCrewToColony {
                colony: id,
                crew: CrewId(0),
            },
        );
        let r = w.step_frame(w.clock().ticks() + 4).unwrap();
        assert!(
            r.fired_events.iter().any(
                |e| matches!(e.kind, EventKind::LifeSupportDepleted { colony } if colony == id)
            )
        );
        assert_eq!(w.colonies()[&id].state, ColonyState::Dormant);
        assert_eq!(w.economy().crew[&CrewId(0)].status, CrewStatus::Lost);
        // The module never ran — the O2 reserve is untouched (the
        // crew died of a dead scrubber, not suffocation).
        let o2 = ResourceId::from("oxygen");
        assert!((w.colonies()[&id].stores[&o2].level_at(w.clock().ticks()) - 50.0).abs() < 1e-9);
    }

    /// R9-F2: a committed `LifeSupportDepleted` re-checks its
    /// predicate at firing — a `TransferComplete` landing O2 on the
    /// same tick (scheduled earlier, so it fires first) restores the
    /// grant and cancels the kill. The event still fires; it is the
    /// no-op its own re-check earns.
    #[test]
    fn same_tick_resupply_cancels_ls_depletion() {
        let mut w = colony_world();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::HireCrew {
                name: "a".into(),
                kind: crate::CrewKind::Engineer,
                skill: 1,
                salary_per_s: 1.0,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        let o2 = ResourceId::from("oxygen");
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::Store {
                    resource: o2.clone(),
                    capacity: 100.0,
                },
                ColonyModuleDef::LifeSupport {
                    consumes_per_crew: vec![(o2.clone(), 0.1)],
                    power_ec_per_s: 0.0,
                },
            ],
            vec![csp_data::ColonyStoreInit {
                resource: o2.clone(),
                level: 6.4,
            }],
        );
        let id = *w.colonies().keys().next().unwrap();
        let vessel = latched_vessel(&mut w, "oxygen", 100.0, 50.0);
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::JoinColonyInventory { vessel, colony: id },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::AssignCrewToColony {
                colony: id,
                crew: CrewId(0),
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        // The drain begins: the heap holds the store's empty bound.
        // `ColonyStoreEmpty`'s firing will commit `LifeSupportDepleted`
        // one tick out — the tick the resupply is aimed at.
        let empty_tick = w
            .event_heap
            .iter()
            .find(|e| {
                matches!(
                    &e.kind,
                    EventKind::ColonyStoreEmpty { colony, resource }
                        if *colony == id && *resource == o2
                )
            })
            .map(|e| e.tick)
            .unwrap();
        // 10 units at 10 u/s = 64 ticks: applied at empty_tick−63 it
        // completes at empty_tick+1 — with an earlier `event_seq`
        // than the depletion the empty's recompute commits, so the
        // credit lands first at their shared tick.
        let apply_tick = empty_tick - 63;
        w.step_frame(apply_tick).unwrap();
        assert_eq!(w.clock().ticks(), apply_tick);
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::TransferColonyResource {
                vessel,
                colony: id,
                resource: o2.clone(),
                amount: 10.0,
                to_colony: true,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        // Through empty_tick+1: the empty commits the depletion, the
        // completion credits O2, and the transition's own re-check
        // finds the store stocked — the crew survives.
        let r = w.step_frame(empty_tick + 3).unwrap();
        assert!(
            r.fired_events
                .iter()
                .any(|e| matches!(e.kind, EventKind::TransferComplete { .. }))
        );
        assert!(
            r.fired_events.iter().any(
                |e| matches!(e.kind, EventKind::LifeSupportDepleted { colony } if colony == id)
            )
        );
        assert_eq!(w.economy().crew[&CrewId(0)].status, CrewStatus::Active);
        assert_eq!(w.colonies()[&id].state, ColonyState::Nominal);
        assert_eq!(w.colonies()[&id].crew.len(), 1);
        // 10 landed at empty_tick+1, then the reserve keeps draining.
        let level = w.colonies()[&id].stores[&o2].level_at(empty_tick + 3);
        assert!(level > 9.9, "level {level}");
    }

    /// R9-F9: a damaged `Battery`'s capacity is gone with the
    /// module — the recompute re-derives aggregate capacity over
    /// undamaged stores, so an in-flight `TransferComplete` credits
    /// nothing into a wrecked battery (the source fallback returns
    /// the units to the vessel's tanks).
    #[test]
    fn damaged_battery_stops_accepting_charge() {
        let mut w = colony_world();
        let ec_id = ec();
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::Battery { capacity: 100.0 },
            ],
            vec![csp_data::ColonyStoreInit {
                resource: ec_id.clone(),
                level: 50.0,
            }],
        );
        let id = *w.colonies().keys().next().unwrap();
        let vessel = latched_vessel(&mut w, "electric_charge", 100.0, 50.0);
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::JoinColonyInventory { vessel, colony: id },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::TransferColonyResource {
                vessel,
                colony: id,
                resource: ec_id.clone(),
                amount: 10.0,
                to_colony: true,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        // In flight now (64 ticks); the vessel holds 40.
        assert!(
            (crate::transfers::vessel_resource(&w.vessels()[&vessel], &ec_id) - 40.0).abs() < 1e-9
        );
        // Wreck the battery mid-flight (ModuleIndex 1).
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::DamageColonyModule {
                colony: id,
                module: 1,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        // Capacity re-derived to 0 — the held 50 went with the module.
        let store = &w.colonies()[&id].stores[&ec_id];
        assert_eq!(store.capacity, 0.0);
        assert_eq!(store.level_at(w.clock().ticks()), 0.0);
        // The completion credits nothing into the wrecked store; the
        // source fallback returns the 10 units to the vessel's tanks.
        w.step_frame(w.clock().ticks() + 128).unwrap();
        assert_eq!(
            w.colonies()[&id].stores[&ec_id].level_at(w.clock().ticks()),
            0.0
        );
        assert!(
            (crate::transfers::vessel_resource(&w.vessels()[&vessel], &ec_id) - 50.0).abs() < 1e-9
        );
    }

    // ---- M8-B: drills / converters / generators / transfers ----

    /// A one-node craft carrying a `resource` tank of `capacity` —
    /// the vessel fixture for the colony transfer tests.
    fn tank_craft(resource: &str, capacity: f64) -> crate::CraftDef {
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
                    resource: ResourceId::from(resource),
                    capacity,
                    mass_per_unit_kg: 1.0,
                }],
                crew_seats: 2,
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

    /// A surface-latched vessel at the pad's pose — 0.05 rad east so
    /// it sits inside the 100 m radius of the colony (the site and
    /// vessel both read the same 6.0e6-radius env sphere... wait —
    /// same lat/lon reads distance 0; give it a hair of longitude so
    /// the chord is nonzero but under the radius).
    fn latched_vessel(w: &mut World, resource: &str, capacity: f64, fill: f64) -> VesselId {
        let id = VesselId(w.next_vessel_id);
        w.next_vessel_id += 1;
        let mut craft = tank_craft(resource, capacity);
        let mut tank_contents = crate::transfers::spawn_contents(&mut craft);
        // The spawn map reads `fill` after this override — reconcile
        // the part's mass through the same bucketed delta the strip
        // (build-material slots spawn empty) or a full-slot rewrite
        // would apply, keeping `mass_kg = dry + Σ bucketed wet` true.
        for stores in tank_contents.values_mut() {
            for (r, c) in stores.iter_mut() {
                if r.as_str() == resource {
                    let spawned = *c;
                    *c = fill;
                    let part = &mut craft.parts[0];
                    let (cap, mpu) = part
                        .tanks
                        .iter()
                        .find(|t| t.resource == *r)
                        .map(|t| (t.capacity, t.mass_per_unit_kg))
                        .unwrap_or((0.0, 0.0));
                    crate::transfers::apply_bucket_delta(part, cap, mpu, spawned, fill);
                }
            }
        }
        let thermal = crate::VesselThermal::at_ambient(&craft.graph, 288.0);
        w.vessels.insert(
            id,
            crate::Vessel {
                id,
                craft: Some(craft),
                state: VesselState::SurfaceLatched(crate::SurfacePose {
                    body: BodyId::from("terra"),
                    latitude_rad: 0.0,
                    longitude_rad: 0.000001,
                    altitude_m: 0.0,
                    orientation: csp_core::DQuat::IDENTITY,
                    afloat: false,
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
                reliability: BTreeMap::new(),
                control_lease: crate::Lease::Player,
                deployed: BTreeMap::new(),
            },
        );
        id
    }

    /// The multi-cargo variant of `latched_vessel`: one part carrying
    /// every `(resource, capacity, fill)` tank row — the
    /// `csp.tank.ore.s` shape the §3.5 supply loop needs (a real ore
    /// carrier holds several stock resources at once).
    fn latched_vessel_multi(w: &mut World, tanks: &[(&str, f64, f64)]) -> VesselId {
        let id = VesselId(w.next_vessel_id);
        w.next_vessel_id += 1;
        let mut craft = tank_craft(tanks[0].0, tanks[0].1);
        let part = &mut craft.parts[0];
        for (res, cap, _) in &tanks[1..] {
            part.tanks.push(csp_phys::TankSpec {
                resource: ResourceId::from(*res),
                capacity: *cap,
                mass_per_unit_kg: 1.0,
            });
        }
        // Keep the `check_tank_masses` convention honest: the part's
        // mass covers a full wet load (the resolver's wet-mass rule).
        let full_wet: f64 = part
            .tanks
            .iter()
            .map(|t| t.capacity * t.mass_per_unit_kg)
            .sum();
        part.mass_kg = 100.0 + full_wet;
        part.inertia =
            csp_phys::SymMat3::solid_box(part.mass_kg, csp_core::DVec3::new(0.5, 0.5, 0.5));
        let mut tank_contents = crate::transfers::spawn_contents(&mut craft);
        // As `latched_vessel`: rewrite the spawn map to the authored
        // fills, then reconcile each tank's bucketed wet mass (the
        // build-material slots spawned empty — their strip already ran).
        for (res, _, fill) in tanks {
            for stores in tank_contents.values_mut() {
                if let Some(c) = stores.get_mut(&ResourceId::from(*res)) {
                    let spawned = *c;
                    *c = *fill;
                    let part = &mut craft.parts[0];
                    let (cap, mpu) = part
                        .tanks
                        .iter()
                        .find(|t| t.resource.as_str() == *res)
                        .map(|t| (t.capacity, t.mass_per_unit_kg))
                        .unwrap_or((0.0, 0.0));
                    crate::transfers::apply_bucket_delta(part, cap, mpu, spawned, *fill);
                }
            }
        }
        let thermal = crate::VesselThermal::at_ambient(&craft.graph, 288.0);
        w.vessels.insert(
            id,
            crate::Vessel {
                id,
                craft: Some(craft),
                state: VesselState::SurfaceLatched(crate::SurfacePose {
                    body: BodyId::from("terra"),
                    latitude_rad: 0.0,
                    longitude_rad: 0.000001,
                    altitude_m: 0.0,
                    orientation: csp_core::DQuat::IDENTITY,
                    afloat: false,
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
                reliability: BTreeMap::new(),
                control_lease: crate::Lease::Player,
                deployed: BTreeMap::new(),
            },
        );
        id
    }

    /// Drill rate = base·min(1, yield/min_concentration): a site
    /// yielding 0.5 against min_concentration 1.0 runs the drill at
    /// half its base rate — ore fills at 0.25 u/s while the EC draw
    /// runs at full power.
    #[test]
    fn drill_extracts_at_concentration_rate() {
        let mut w = colony_world();
        let mut sites = BTreeMap::new();
        let mut s = test_site();
        s.resource_yields = vec![(ResourceId::from("ore"), 0.5)];
        sites.insert(s.id.clone(), s);
        w.set_sites(sites);
        let ore = ResourceId::from("ore");
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::Drill {
                    resource: ore.clone(),
                    base_rate_per_s: 0.5,
                    min_concentration: 1.0,
                    power_ec_per_s: 0.5,
                },
                ColonyModuleDef::Store {
                    resource: ore.clone(),
                    capacity: 100.0,
                },
            ],
            vec![],
        );
        let colony = w.colonies().values().next().unwrap();
        // 0.5 base × min(1, 0.5/1.0) = 0.25 u/s.
        assert!((colony.stores[&ore].net_rate_per_s - 0.25).abs() < 1e-9);
    }

    /// A barren site — no yield entry for the resource — drills
    /// nothing while still drawing its power request.
    #[test]
    fn drill_on_barren_site_produces_nothing() {
        let mut w = colony_world();
        let mut sites = BTreeMap::new();
        let mut s = test_site();
        s.resource_yields = vec![(ResourceId::from("water"), 0.5)];
        sites.insert(s.id.clone(), s);
        w.set_sites(sites);
        let ore = ResourceId::from("ore");
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::Drill {
                    resource: ore.clone(),
                    base_rate_per_s: 0.5,
                    min_concentration: 1.0,
                    power_ec_per_s: 0.5,
                },
                ColonyModuleDef::Store {
                    resource: ore.clone(),
                    capacity: 100.0,
                },
            ],
            vec![],
        );
        let colony = w.colonies().values().next().unwrap();
        assert_eq!(colony.stores[&ore].net_rate_per_s, 0.0);
    }

    /// A fuel-burning generator burns its fuel through the same
    /// allocation pass as everything else: the fuel store drains at
    /// fuel_rate_per_s while EC fills — and when the fuel runs out
    /// the generator's grant drops, EC production stops with it.
    #[test]
    fn generator_burns_fuel_for_ec() {
        let mut w = colony_world();
        let fuel = ResourceId::from("ore");
        let ec = ec();
        found(
            &mut w,
            vec![
                ColonyModuleDef::Battery { capacity: 1000.0 },
                fuel_gen("ore", 0.5, 4.0, 0),
                ColonyModuleDef::Store {
                    resource: fuel.clone(),
                    capacity: 100.0,
                },
            ],
            vec![
                csp_data::ColonyStoreInit {
                    resource: fuel.clone(),
                    level: 100.0,
                },
                csp_data::ColonyStoreInit {
                    resource: ec.clone(),
                    level: 0.0,
                },
            ],
        );
        let colony = w.colonies().values().next().unwrap();
        // ore −0.5/s (the burn), EC +4/s.
        assert!((colony.stores[&fuel].net_rate_per_s + 0.5).abs() < 1e-9);
        assert!((colony.stores[&ec].net_rate_per_s - 4.0).abs() < 1e-9);
        // After 200 s the fuel is gone: the EC store's net drops to 0.
        let t = colony.stores[&fuel].anchor_tick + 200 * TICK_HZ;
        w.step_frame(t).unwrap();
        let colony = w.colonies().values().next().unwrap();
        assert!(colony.stores[&fuel].level_at(t) <= 1e-9);
        assert!(colony.stores[&ec].net_rate_per_s <= 1e-9);
    }

    /// A converter runs its recipe at the granted fraction: inputs
    /// drain, outputs fill — recipe rates ×1.0 here (full grant).
    #[test]
    fn converter_runs_recipe() {
        let mut w = colony_world();
        let ore = ResourceId::from("ore");
        let metal = ResourceId::from("metal");
        let mut recipes = BTreeMap::new();
        let mut modules = vec![
            gen_module(10.0),
            conv(
                &mut recipes,
                "recipe.smelt",
                vec![(ore.clone(), 1.0)],
                vec![(metal.clone(), 0.6)],
                5,
                0.5,
            ),
            ColonyModuleDef::Store {
                resource: ore.clone(),
                capacity: 100.0,
            },
            ColonyModuleDef::Store {
                resource: metal.clone(),
                capacity: 100.0,
            },
        ];
        w.set_recipes(recipes);
        // A feeder drill keeps ore stocked so the converter stays
        // grounded: ore 1.0 in = ore 1.0 out, metal +0.6.
        modules.push(ColonyModuleDef::Drill {
            resource: ore.clone(),
            base_rate_per_s: 1.0,
            min_concentration: 1.0,
            power_ec_per_s: 0.0,
        });
        found(&mut w, modules, vec![]);
        let colony = w.colonies().values().next().unwrap();
        assert!((colony.stores[&ore].net_rate_per_s).abs() < 1e-9);
        assert!((colony.stores[&metal].net_rate_per_s - 0.6).abs() < 1e-9);
    }

    /// Founding a colony whose converter names a recipe the world
    /// never installed rejects cleanly — the recipe table is
    /// construction config, not a silent inert.
    #[test]
    fn found_rejects_unknown_recipe() {
        let mut w = colony_world();
        let r = found(
            &mut w,
            vec![ColonyModuleDef::Converter {
                recipe: csp_data::RecipeId::from("recipe.missing"),
                priority: 5,
                power_ec_per_s: 0.5,
            }],
            vec![],
        );
        assert!(
            r.rejected
                .iter()
                .any(|r| matches!(r.error, SimError::Recoverable(_))),
            "{:?}",
            r.rejected
        );
        assert!(w.colonies().is_empty());
    }

    /// The join opt-in: a latched vessel beside the colony cannot
    /// transfer until `JoinColonyInventory` names it — then the
    /// resource transfer moves units from its tanks into the
    /// colony's store on the event grid.
    #[test]
    fn colony_transfer_requires_and_runs_after_join() {
        let mut w = colony_world();
        let ore = ResourceId::from("ore");
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::Store {
                    resource: ore.clone(),
                    capacity: 100.0,
                },
            ],
            vec![],
        );
        let colony = *w.colonies().keys().next().unwrap();
        let vessel = latched_vessel(&mut w, "ore", 100.0, 50.0);
        // No join → the transfer rejects.
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::TransferColonyResource {
                vessel,
                colony,
                resource: ore.clone(),
                amount: 10.0,
                to_colony: true,
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(!r.rejected.is_empty());
        assert_eq!(w.transfers().len(), 0);
        // Join — then the same transfer applies and completes.
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::JoinColonyInventory { vessel, colony },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        assert_eq!(w.vessels()[&vessel].joined_colony, Some(colony));
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::TransferColonyResource {
                vessel,
                colony,
                resource: ore.clone(),
                amount: 10.0,
                to_colony: true,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        // Debited at apply: the vessel holds 40; 10 units ride the pipe.
        assert!(
            (crate::transfers::vessel_resource(&w.vessels()[&vessel], &ore) - 40.0).abs() < 1e-9
        );
        // 10 units at 10 u/s = 1 s = 64 ticks to completion.
        w.step_frame(w.clock().ticks() + 64 + 1).unwrap();
        let level = w.colonies()[&colony].stores[&ore].level_at(w.clock().ticks());
        assert!((level - 10.0).abs() < 1e-9, "level {level}");
        assert!(
            (crate::transfers::vessel_resource(&w.vessels()[&vessel], &ore) - 40.0).abs() < 1e-9
        );
        assert!(w.transfers().is_empty());
    }

    /// The reverse direction: the colony store debits now (the
    /// re-solve sees the drop) and the vessel's tank fills at the
    /// completion event.
    #[test]
    fn colony_to_vessel_transfer_credits_at_completion() {
        let mut w = colony_world();
        let ore = ResourceId::from("ore");
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::Store {
                    resource: ore.clone(),
                    capacity: 100.0,
                },
            ],
            vec![csp_data::ColonyStoreInit {
                resource: ore.clone(),
                level: 80.0,
            }],
        );
        let colony = *w.colonies().keys().next().unwrap();
        let vessel = latched_vessel(&mut w, "ore", 100.0, 0.0);
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::JoinColonyInventory { vessel, colony },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::TransferColonyResource {
                vessel,
                colony,
                resource: ore.clone(),
                amount: 20.0,
                to_colony: false,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        // The colony's anchor dropped by 20 at apply.
        let level = w.colonies()[&colony].stores[&ore].level_at(w.clock().ticks());
        assert!((level - 60.0).abs() < 1e-9, "level {level}");
        // 20 units at 10 u/s = 2 s = 128 ticks → tank fills.
        w.step_frame(w.clock().ticks() + 128 + 1).unwrap();
        assert!(
            (crate::transfers::vessel_resource(&w.vessels()[&vessel], &ore) - 20.0).abs() < 1e-9
        );
    }

    /// §3.5's "resources ◄── colonies/ISRU" edge, end to end
    /// (fix-plan 3.1, route (b)): a cargo vessel delivers ore
    /// vessel→colony (the `colony_resupply` direction), loads colony
    /// metal colony→vessel, and `RecoverVessel` at the site credits
    /// the tank CONTENTS to the site's inventory at the recovery
    /// efficiency — the run that refills `starting_stock`. Every leg
    /// rides shipped commands; the only thing the sim ever lacked was
    /// a tank that could hold the cargo.
    #[test]
    fn colony_cargo_loop_credits_site_inventory() {
        let mut w = colony_world();
        let ore = ResourceId::from("ore");
        let metal = ResourceId::from("metal");
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::Store {
                    resource: ore.clone(),
                    capacity: 100.0,
                },
                ColonyModuleDef::Store {
                    resource: metal.clone(),
                    capacity: 100.0,
                },
            ],
            vec![csp_data::ColonyStoreInit {
                resource: metal.clone(),
                level: 80.0,
            }],
        );
        let colony = *w.colonies().keys().next().unwrap();
        // The cargo carrier: 40 ore aboard for delivery, an empty
        // metal tank for the ride home.
        let vessel = latched_vessel_multi(&mut w, &[("ore", 100.0, 40.0), ("metal", 100.0, 0.0)]);
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::JoinColonyInventory { vessel, colony },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        // Leg 1 — resupply: 20 ore vessel→colony (2 s pipe).
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::TransferColonyResource {
                vessel,
                colony,
                resource: ore.clone(),
                amount: 20.0,
                to_colony: true,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        // Leg 2 — the export: 60 metal colony→vessel (6 s pipe).
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::TransferColonyResource {
                vessel,
                colony,
                resource: metal.clone(),
                amount: 60.0,
                to_colony: false,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        // Past both pipes (the metal leg is the long one): the vessel
        // rides home with 20 ore + 60 metal.
        w.step_frame(w.clock().ticks() + 384 + 1).unwrap();
        assert!(
            (crate::transfers::vessel_resource(&w.vessels()[&vessel], &ore) - 20.0).abs() < 1e-9
        );
        assert!(
            (crate::transfers::vessel_resource(&w.vessels()[&vessel], &metal) - 60.0).abs() < 1e-9
        );
        // The colony's books balance: +20 ore in, −60 metal out.
        let co = &w.colonies()[&colony];
        let now = w.clock().ticks();
        assert!((co.stores[&ore].level_at(now) - 20.0).abs() < 1e-9);
        assert!((co.stores[&metal].level_at(now) - 20.0).abs() < 1e-9);
        // Leg 3 — recovery at the site lands the contents at the
        // recovery efficiency (the test site is not a launch site:
        // `recovery_efficiency` 0.9 applies).
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::RecoverVessel {
                vessel,
                site: Some(SiteId::from("pad")),
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(!w.vessels().contains_key(&vessel));
        let eff = w.economy_config().recovery_efficiency;
        let inv = &w.economy().sites[&SiteId::from("pad")].inventory;
        assert!((inv[&ore] - 20.0 * eff).abs() < 1e-9, "ore {inv:?}");
        assert!((inv[&metal] - 60.0 * eff).abs() < 1e-9, "metal {inv:?}");
    }

    /// Out of range rejects the join — a vessel latched a radian
    /// away is nowhere near the colony's 100 m radius.
    #[test]
    fn join_out_of_range_rejects() {
        let mut w = colony_world();
        found(&mut w, vec![gen_module(10.0)], vec![]);
        let colony = *w.colonies().keys().next().unwrap();
        let vessel = latched_vessel(&mut w, "ore", 100.0, 0.0);
        // Move the vessel half a radian away — kilometres off.
        w.vessels.get_mut(&vessel).unwrap().state =
            VesselState::SurfaceLatched(crate::SurfacePose {
                body: BodyId::from("terra"),
                latitude_rad: 0.0,
                longitude_rad: 0.5,
                altitude_m: 0.0,
                orientation: csp_core::DQuat::IDENTITY,
                afloat: false,
            });
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::JoinColonyInventory { vessel, colony },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(!r.rejected.is_empty());
        assert_eq!(w.vessels()[&vessel].joined_colony, None);
    }

    /// Crew moves vessel → colony through the same event-driven
    /// process: the member keeps its seat until completion, then the
    /// colony's crew set + roster location update together.
    #[test]
    fn crew_transfer_vessel_to_colony() {
        let mut w = colony_world();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::HireCrew {
                name: "a".into(),
                kind: crate::CrewKind::Pilot,
                skill: 1,
                salary_per_s: 0.0,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        // C09-F4: the landing gate shares `colony_accepts_crew` — the
        // fixture needs an undamaged `LifeSupport` module to take the
        // member (consumes nothing, the test is about the seat move).
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::LifeSupport {
                    consumes_per_crew: vec![],
                    power_ec_per_s: 0.0,
                },
            ],
            vec![],
        );
        let colony = *w.colonies().keys().next().unwrap();
        let vessel = latched_vessel(&mut w, "ore", 100.0, 0.0);
        // Seat the roster member aboard — roster location Vessel.
        {
            let v = w.vessels.get_mut(&vessel).unwrap();
            v.crew.push(crate::transfers::VesselCrew {
                id: CrewId(0),
                seat: csp_phys::PartNodeId(0),
            });
        }
        w.economy.crew.get_mut(&CrewId(0)).unwrap().location =
            crate::economy::CrewLocation::Vessel(vessel);
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::JoinColonyInventory { vessel, colony },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::TransferColonyCrew {
                vessel,
                colony,
                crew: CrewId(0),
                to_colony: true,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        // Still aboard until the crew-traversal delay completes.
        assert_eq!(w.vessels()[&vessel].crew.len(), 1);
        w.step_frame(w.clock().ticks() + 640 + 1).unwrap();
        assert!(w.vessels()[&vessel].crew.is_empty());
        assert!(w.colonies()[&colony].crew.contains(&CrewId(0)));
        assert_eq!(
            w.economy().crew[&CrewId(0)].location,
            crate::economy::CrewLocation::Colony(colony)
        );
    }

    /// 7.3 §3.6: the vessel→colony crew landing is the
    /// `CrewTransport` predicate's other producer — a member landed
    /// by `TransferColonyCrew` fires `CrewPosted` and completes the
    /// posted-count ask exactly like the direct
    /// `AssignCrewToColony` posting.
    #[test]
    fn crew_landing_completes_transport_contract() {
        let mut w = colony_world();
        w.contracts.active.insert(
            crate::contracts::ContractId(0),
            crate::contracts::ActiveContract {
                id: crate::contracts::ContractId(0),
                template: csp_data::ContractTypeId::from("contract.crew_transport"),
                params: csp_data::ContractParams::CrewTransport {
                    site: SiteId::from("pad"),
                    count: 1,
                },
                deadline_tick: None,
                progress: None,
            },
        );
        w.contracts.next_contract_id = 1;
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::HireCrew {
                name: "a".into(),
                kind: crate::CrewKind::Pilot,
                skill: 1,
                salary_per_s: 0.0,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::LifeSupport {
                    consumes_per_crew: vec![],
                    power_ec_per_s: 0.0,
                },
            ],
            vec![],
        );
        let colony = *w.colonies().keys().next().unwrap();
        let vessel = latched_vessel(&mut w, "ore", 100.0, 0.0);
        {
            let v = w.vessels.get_mut(&vessel).unwrap();
            v.crew.push(crate::transfers::VesselCrew {
                id: CrewId(0),
                seat: csp_phys::PartNodeId(0),
            });
        }
        w.economy.crew.get_mut(&CrewId(0)).unwrap().location =
            crate::economy::CrewLocation::Vessel(vessel);
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::JoinColonyInventory { vessel, colony },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::TransferColonyCrew {
                vessel,
                colony,
                crew: CrewId(0),
                to_colony: true,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        // The crew-traversal delay lands the member — and the
        // `CrewPosted` trigger completes the contract in the same
        // boundary.
        w.step_frame(w.clock().ticks() + 640 + 1).unwrap();
        assert!(w.colonies()[&colony].crew.contains(&CrewId(0)));
        assert!(
            w.contracts().active.is_empty(),
            "the landing completed the transport contract"
        );
        assert_eq!(
            w.contracts().facts.completed[&csp_data::ContractKind::CrewTransport],
            1
        );
    }

    // ---- M8-C: the toggle, the impact seam, the resupply fact ----

    /// The dominant-body physics env the impact/resupply tests
    /// restore against — `terra` at the test system's 6.0e6 radius.
    fn test_physics() -> crate::PhysicsConfig {
        crate::PhysicsConfig {
            environment: csp_phys::Environment {
                body_center: csp_core::DVec3::ZERO,
                radius_m: 6.0e6,
                mu_m3_per_s2: 4.0e14,
                body_spin: csp_core::DVec3::ZERO,
                atmosphere: None,
                settle_v: 0.05,
                settle_w: 0.05,
                settle_ticks: 128,
                friction_mu: 0.5,
                drag_cd: 1.0,
                convective_heat_fraction: csp_phys::DEFAULT_CONVECTIVE_HEAT_FRACTION,
                touchdown_crush_m: csp_phys::DEFAULT_TOUCHDOWN_CRUSH_M,
                terrain: None,
                ocean: None,
            },
            dominant: BodyId::from("terra"),
            bubble_radius_m: crate::DEFAULT_BUBBLE_RADIUS_M,
        }
    }

    /// A physics world for the impact seam — `colony_world` plus the
    /// dominant-body bubble `apply_surface_impacts` reads.
    fn impact_world() -> World {
        let mut w = World::with_physics(
            test_system(),
            test_physics(),
            crate::DEFAULT_MAX_EVENTS_PER_FRAME,
        );
        w.set_colony_env(test_env());
        w.set_sites(BTreeMap::from([(SiteId::from("pad"), test_site())]));
        w
    }

    /// The toggle gates the colony side too: disabled, a crewed
    /// colony's `LifeSupport` module draws nothing and never earns a
    /// `LifeSupportDepleted`; re-enabled, the draw and the depletion
    /// transition return on the same recompute.
    #[test]
    fn toggle_gates_colony_life_support() {
        let mut w = colony_world();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::HireCrew {
                name: "a".into(),
                kind: crate::CrewKind::Engineer,
                skill: 1,
                salary_per_s: 0.0,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        // A generator + an LS module that can never breathe (no O2
        // store, no producer) — the `damaged_life_support_kills`
        // fixture.
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::LifeSupport {
                    consumes_per_crew: vec![(ResourceId::from("oxygen"), 0.1)],
                    power_ec_per_s: 0.0,
                },
            ],
            vec![],
        );
        let id = *w.colonies().keys().next().unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SetLifeSupportEnabled { enabled: false },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::AssignCrewToColony {
                colony: id,
                crew: CrewId(0),
            },
        );
        w.step_frame(w.clock().ticks() + 8).unwrap();
        assert_eq!(w.economy().crew[&CrewId(0)].status, CrewStatus::Active);
        assert!(
            !w.event_heap.iter().any(
                |e| matches!(e.kind, EventKind::LifeSupportDepleted { colony } if colony == id)
            )
        );
        // Re-enable — the recompute the toggle runs restores the
        // demand and the depletion lands.
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SetLifeSupportEnabled { enabled: true },
        );
        let r = w.step_frame(w.clock().ticks() + 2).unwrap();
        assert!(
            r.fired_events.iter().any(
                |e| matches!(e.kind, EventKind::LifeSupportDepleted { colony } if colony == id)
            )
        );
        assert_eq!(w.economy().crew[&CrewId(0)].status, CrewStatus::Lost);
        assert!(matches!(w.colonies()[&id].state, ColonyState::Dormant));
    }

    /// Regression (M8-C revalidation): a colony `LifeSupportDepleted`
    /// pending at `SetLifeSupportEnabled{false}` dies with the flag —
    /// the toggle-off scrub covers the colony-scoped kind like the
    /// vessel-side pair — and a straggler that survives the scrub
    /// anyway (a pre-fix save's) fires as a no-op: a disabled
    /// subsystem never kills.
    #[test]
    fn toggle_off_scrubs_and_gates_colony_depletion() {
        let mut w = colony_world();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::HireCrew {
                name: "jeb".into(),
                kind: crate::CrewKind::Engineer,
                skill: 1,
                salary_per_s: 0.0,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        // A generator + an LS module that can never breathe — the
        // `ls_depletion_kills_and_dormants` fixture.
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::LifeSupport {
                    consumes_per_crew: vec![(ResourceId::from("oxygen"), 0.1)],
                    power_ec_per_s: 0.0,
                },
            ],
            vec![],
        );
        let id = *w.colonies().keys().next().unwrap();
        // Assign + toggle-off in ONE boundary drain: the assign's
        // recompute schedules the depletion one tick out, then the
        // toggle's scrub drops it before any event tick can land.
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::AssignCrewToColony {
                colony: id,
                crew: CrewId(0),
            },
        );
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SetLifeSupportEnabled { enabled: false },
        );
        let r = w.step_frame(w.clock().ticks() + 8).unwrap();
        assert!(
            !r.fired_events.iter().any(
                |e| matches!(e.kind, EventKind::LifeSupportDepleted { colony } if colony == id)
            )
        );
        assert!(
            !w.event_heap.iter().any(
                |e| matches!(e.kind, EventKind::LifeSupportDepleted { colony } if colony == id)
            )
        );
        assert_eq!(w.economy().crew[&CrewId(0)].status, CrewStatus::Active);
        assert_eq!(w.colonies()[&id].state, ColonyState::Nominal);
        assert_eq!(w.colonies()[&id].crew.len(), 1);
        // A straggler that survives the scrub — a save recorded
        // pre-fix — still cannot kill: the firing arm re-checks the
        // flag (the `fire_vessel_ls_depleted` precedent).
        let mut shadow = TickShadow::capture(&w);
        w.schedule_event(
            EventKind::LifeSupportDepleted { colony: id },
            w.clock().ticks() + 1,
            &mut shadow,
        )
        .unwrap();
        let r = w.step_frame(w.clock().ticks() + 2).unwrap();
        assert!(
            r.fired_events.iter().any(
                |e| matches!(e.kind, EventKind::LifeSupportDepleted { colony } if colony == id)
            )
        );
        assert_eq!(w.economy().crew[&CrewId(0)].status, CrewStatus::Active);
        assert_eq!(w.colonies()[&id].state, ColonyState::Nominal);
        assert_eq!(w.colonies()[&id].crew.len(), 1);
    }

    /// The M8-B-deferred seam: a debris impact inside the colony's
    /// radius damages its lowest-index undamaged module and lands
    /// `Degraded { reason: ImpactDamage }`; an impact outside the
    /// radius touches nothing.
    #[test]
    fn surface_impact_damages_colony_in_radius() {
        let mut w = impact_world();
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::Store {
                    resource: ResourceId::from("ore"),
                    capacity: 100.0,
                },
            ],
            vec![],
        );
        let id = *w.colonies().keys().next().unwrap();
        let mut shadow = TickShadow::capture(&w);
        // The pad sits at (lat 0, lon 0) — surface point (R, 0, 0);
        // 5 m radially out is inside the 100 m radius, 200 m east is
        // outside it.
        w.apply_surface_impacts(
            &[(
                csp_core::DVec3::new(6.0e6 + 200.0, 0.0, 0.0),
                w.clock().ticks(),
            )],
            &mut shadow,
        )
        .unwrap();
        assert!(w.colonies()[&id].modules.iter().all(|m| !m.damaged));
        assert!(matches!(w.colonies()[&id].state, ColonyState::Nominal));
        w.apply_surface_impacts(
            &[(
                csp_core::DVec3::new(6.0e6 + 5.0, 0.0, 0.0),
                w.clock().ticks(),
            )],
            &mut shadow,
        )
        .unwrap();
        assert!(w.colonies()[&id].modules[0].damaged);
        assert!(!w.colonies()[&id].modules[1].damaged);
        assert!(matches!(
            w.colonies()[&id].state,
            ColonyState::Degraded {
                reason: DegradeReason::ImpactDamage
            }
        ));
        // A second impact takes the next undamaged module.
        w.apply_surface_impacts(
            &[(
                csp_core::DVec3::new(6.0e6 + 5.0, 0.0, 0.0),
                w.clock().ticks(),
            )],
            &mut shadow,
        )
        .unwrap();
        assert!(w.colonies()[&id].modules.iter().all(|m| m.damaged));
        // The damage rides the snapshot — hashed state.
        let h = w.state_hash().unwrap();
        let snap = w.snapshot().unwrap();
        let mut r = World::restore(test_system(), Some(test_physics()), snap).unwrap();
        r.set_colony_env(test_env());
        r.set_sites(BTreeMap::from([(SiteId::from("pad"), test_site())]));
        assert_eq!(r.state_hash().unwrap(), h);
    }

    /// R8-F4/R9-F8 regression: a colony whose body lost its
    /// `colony_env` entry (a `set_colony_env` swap post-install) is
    /// not an impact target — the seam must return `Ok`, never a
    /// `Recoverable` that `step_frame` would wrongly surface as
    /// `Err`.
    #[test]
    fn surface_impact_skips_envless_colony() {
        let mut w = impact_world();
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::Store {
                    resource: ResourceId::from("ore"),
                    capacity: 100.0,
                },
            ],
            vec![],
        );
        let id = *w.colonies().keys().next().unwrap();
        // Drop the whole env table — the colony's body now misses.
        w.set_colony_env(BTreeMap::new());
        let mut shadow = TickShadow::capture(&w);
        w.apply_surface_impacts(
            &[(
                csp_core::DVec3::new(6.0e6 + 5.0, 0.0, 0.0),
                w.clock().ticks(),
            )],
            &mut shadow,
        )
        .unwrap();
        assert!(w.colonies()[&id].modules.iter().all(|m| !m.damaged));
        assert!(matches!(w.colonies()[&id].state, ColonyState::Nominal));
    }

    /// The resupply contract's predicate rides the real transfer: a
    /// `TransferComplete` landing into the colony's stores stamps the
    /// cumulative fact and completes the contract.
    #[test]
    fn resupply_contract_completes_on_delivery() {
        let mut w = colony_world();
        let ore = ResourceId::from("ore");
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::Store {
                    resource: ore.clone(),
                    capacity: 100.0,
                },
            ],
            vec![],
        );
        let colony = *w.colonies().keys().next().unwrap();
        // The arena record the predicate evaluates — generation is
        // covered by `contracts::tests`; this test exercises the
        // delivery → fact → completion seam.
        w.contracts.active.insert(
            crate::contracts::ContractId(0),
            crate::contracts::ActiveContract {
                id: crate::contracts::ContractId(0),
                template: csp_data::ContractTypeId::from("contract.colony_resupply"),
                params: csp_data::ContractParams::ColonyResupply {
                    body: BodyId::from("terra"),
                    resource: ore.clone(),
                    amount: 10.0,
                },
                deadline_tick: None,
                progress: None,
            },
        );
        w.contracts.next_contract_id = 1;
        let vessel = latched_vessel(&mut w, "ore", 100.0, 50.0);
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::JoinColonyInventory { vessel, colony },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::TransferColonyResource {
                vessel,
                colony,
                resource: ore.clone(),
                amount: 10.0,
                to_colony: true,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(
            !w.contracts
                .facts
                .colony_deliveries
                .contains_key(&(colony.0, ore.clone()))
        );
        // 10 units at 10 u/s = 64 ticks — the completion lands the
        // fact and the contract in the same firing.
        w.step_frame(w.clock().ticks() + 64 + 1).unwrap();
        assert_eq!(
            w.contracts
                .facts
                .colony_deliveries
                .get(&(colony.0, ore.clone()))
                .copied()
                .unwrap_or(0.0),
            10.0
        );
        assert!(w.contracts.active.is_empty());
        assert_eq!(
            w.contracts
                .facts
                .completed
                .get(&csp_data::ContractKind::ColonyResupply)
                .copied()
                .unwrap_or(0),
            1
        );
        // The fact survives save/load — the §7.2 record.
        let h = w.state_hash().unwrap();
        let snap = w.snapshot().unwrap();
        let mut r = World::restore(test_system(), Some(test_physics()), snap).unwrap();
        r.set_sites(BTreeMap::from([(SiteId::from("pad"), test_site())]));
        r.set_colony_env(test_env());
        assert_eq!(r.state_hash().unwrap(), h);
    }

    /// C6-F3 institutionalized: a converter whose EVERY output has
    /// nowhere to go — no store holding it, no other viable module
    /// requesting it — pins at fraction 0 instead of draining its
    /// input into product that ceases to exist (the write-back only
    /// touches `stores` entries, so a storeless output silently
    /// voided: ore drained, metal gone).
    #[test]
    fn sinkless_output_refuses_to_run() {
        let mut levels = BTreeMap::new();
        levels.insert(ResourceId::from("ore"), 10.0);
        let mut recipes = BTreeMap::new();
        let light = BodyLight {
            synodic_rate: 0.0,
            h0: 0.0,
            day_insolation_w_m2: 0.0,
            cos_lat: 1.0,
        };
        // A converter with power but an output nobody can take: no
        // "metal" store, no consumer. `test_caps` names only what a
        // colony stocks — metal is deliberately absent.
        let modules = vec![
            ColonyModule {
                def: gen_module(10.0),
                damaged: false,
            },
            ColonyModule {
                def: conv(
                    &mut recipes,
                    "smelt",
                    vec![(ec(), 0.5), (ResourceId::from("ore"), 1.0)],
                    vec![(ResourceId::from("metal"), 0.5)],
                    5,
                    0.0,
                ),
                damaged: false,
            },
        ];
        let conc = BTreeMap::new();
        let solve = solve_network(&inp(
            &modules,
            ColonyState::Nominal,
            &levels,
            // ore store exists; metal store does NOT.
            &test_caps(&["ore", "electric_charge"]),
            &light,
            0,
            Daylight::Day,
            &recipes,
            &conc,
            &crew_demands(),
        ));
        assert_eq!(solve.fractions[1], 0.0, "no sink ⇒ no run");
        assert_eq!(
            solve
                .net_rates
                .get(&ResourceId::from("ore"))
                .copied()
                .unwrap_or(0.0),
            0.0,
            "ore is not drained for voided product"
        );
    }

    /// C5-transfers-F2 institutionalized: a resource landing on a
    /// DEAD colony still credits the frozen store (terminal
    /// bookkeeping) but must NOT record the resupply fact nor
    /// complete its contract — `colony_live` gates the predicate.
    #[test]
    fn dead_colony_delivery_does_not_complete_contract() {
        let mut w = colony_world();
        let ore = ResourceId::from("ore");
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::Store {
                    resource: ore.clone(),
                    capacity: 100.0,
                },
            ],
            vec![],
        );
        let colony = *w.colonies().keys().next().unwrap();
        w.contracts.active.insert(
            crate::contracts::ContractId(0),
            crate::contracts::ActiveContract {
                id: crate::contracts::ContractId(0),
                template: csp_data::ContractTypeId::from("contract.colony_resupply"),
                params: csp_data::ContractParams::ColonyResupply {
                    body: BodyId::from("terra"),
                    resource: ore.clone(),
                    amount: 10.0,
                },
                deadline_tick: None,
                progress: None,
            },
        );
        w.contracts.next_contract_id = 1;
        let vessel = latched_vessel(&mut w, "ore", 100.0, 50.0);
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::JoinColonyInventory { vessel, colony },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        // The colony dies after the transfer launches.
        w.colonies.get_mut(&colony).unwrap().state = ColonyState::Abandoned;
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::TransferColonyResource {
                vessel,
                colony,
                resource: ore.clone(),
                amount: 10.0,
                to_colony: true,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        w.step_frame(w.clock().ticks() + 64 + 1).unwrap();
        // The fact never recorded; the contract still waits.
        assert!(
            !w.contracts
                .facts
                .colony_deliveries
                .contains_key(&(colony.0, ore.clone())),
            "a landing on a dead colony is not a resupply fact"
        );
        assert_eq!(w.contracts.active.len(), 1, "the contract stays open");
    }

    /// A resupply template for the generation/lifecycle tests —
    /// bodies span the colony's body plus one that can never host a
    /// store, resources span the colony's store plus one it cannot
    /// hold. The draw gate must narrow both to the viable set.
    fn resupply_template() -> csp_data::ContractTemplate {
        csp_data::ContractTemplate {
            schema_version: 1,
            id: csp_data::ContractTypeId::from("contract.resupply"),
            display_name: csp_data::LocKey::from("x"),
            description: csp_data::LocKey::from("x"),
            kind: csp_data::ContractKind::ColonyResupply,
            generate: csp_data::ContractGenerator {
                weight: 1.0,
                space: csp_data::ContractParamSpace::ColonyResupply {
                    bodies: vec![BodyId::from("terra"), BodyId::from("nowhere")],
                    resources: vec![ResourceId::from("ore"), ResourceId::from("noware")],
                    amount: (10.0, 10.0),
                },
            },
            prerequisites: vec![],
            rewards: csp_data::ContractRewards {
                advance_funds: 2.0,
                completion_funds: 10.0,
                failure_penalty_funds: 4.0,
            },
            deadline_days: None,
        }
    }

    /// C09-F5/C34-F1's generation gate: candidate bodies and — on
    /// the drawn body — candidate resources are filtered by live
    /// colony store capacity, so every published offer names a
    /// delivery the transfer gate can actually land.
    #[test]
    fn resupply_draws_filter_to_viable_stores() {
        let mut w = colony_world();
        let ore = ResourceId::from("ore");
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::Store {
                    resource: ore.clone(),
                    capacity: 100.0,
                },
            ],
            vec![],
        );
        let t = resupply_template();
        w.set_contracts(BTreeMap::from([(t.id.clone(), t)]));
        w.step_frame(w.clock().ticks() + TICK_HZ).unwrap();
        assert_eq!(w.contracts.offers.len(), 6);
        for o in w.contracts.offers.values() {
            let csp_data::ContractParams::ColonyResupply { body, resource, .. } = &o.params else {
                panic!("wrong params")
            };
            // "nowhere"/"noware" can never draw: no live colony holds
            // store capacity for either.
            assert_eq!(*body, BodyId::from("terra"));
            assert_eq!(*resource, ore);
        }
    }

    /// The full §3.6 resupply arc C34-F1 said could never close:
    /// publish → accept (advance + baseline re-base) → ore aboard
    /// the csp.tank.ore.s-shaped hauler → `TransferColonyResource`
    /// delivery → `TransferComplete` lands the fact → the contract
    /// completes and pays.
    #[test]
    fn resupply_full_lifecycle_accept_deliver_complete() {
        let mut w = colony_world();
        let ore = ResourceId::from("ore");
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::Store {
                    resource: ore.clone(),
                    capacity: 100.0,
                },
            ],
            vec![],
        );
        let colony = *w.colonies().keys().next().unwrap();
        let t = resupply_template();
        w.set_contracts(BTreeMap::from([(t.id.clone(), t)]));
        w.economy.funds_micros = 0;
        w.step_frame(w.clock().ticks() + TICK_HZ).unwrap();
        let offer = *w.contracts.offers.keys().next().unwrap();
        w.enqueue(SourceClass::Ui, 0, SimCommand::AcceptContract { offer });
        w.step_frame(w.clock().ticks() + 1).unwrap();
        assert_eq!(w.contracts.active.len(), 1);
        // The advance credited; the ask re-based to baseline + 10
        // (baseline 0 on a fresh colony).
        assert_eq!(w.economy.funds_micros, 2_000_000);
        // The hauler: ore/metal/composite cargo slots — the
        // csp.tank.ore.s shape 3.1 shipped — loaded with ore.
        let vessel = latched_vessel_multi(
            &mut w,
            &[
                ("ore", 200.0, 50.0),
                ("metal", 60.0, 0.0),
                ("composite", 30.0, 0.0),
            ],
        );
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::JoinColonyInventory { vessel, colony },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::TransferColonyResource {
                vessel,
                colony,
                resource: ore.clone(),
                amount: 10.0,
                to_colony: true,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        // 10 units at the authored 10 u/s flow rate = 64 ticks to
        // `TransferComplete`, which lands the fact and completes
        // the contract in the same firing.
        w.step_frame(w.clock().ticks() + 64 + 1).unwrap();
        assert_eq!(
            w.contracts
                .facts
                .colony_deliveries
                .get(&(colony.0, ore.clone()))
                .copied()
                .unwrap_or(0.0),
            10.0
        );
        assert!(w.contracts.active.is_empty(), "the resupply completed");
        assert_eq!(
            w.contracts
                .facts
                .completed
                .get(&csp_data::ContractKind::ColonyResupply)
                .copied()
                .unwrap_or(0),
            1
        );
        // Advance 2.0 + completion 10.0.
        assert_eq!(w.economy.funds_micros, 12_000_000);
    }

    /// C09-F4's consumption half, solve-side: crew standing at a
    /// `LifeSupport`-less colony draw the vessel-baseline set from
    /// the colony's stores — `0.02 · crew` oxygen/s, `0.05 · crew`
    /// EC/s — and an unstocked, unproduced demand flags
    /// `ls_depleted` (the F4 probe's immortal colonists are
    /// impossible again). The module table is a lone free EC
    /// producer so the baseline's grounding has something to sit on.
    #[allow(clippy::too_many_lines)] // the stocked/starved/module-arm sweep reads as one pass
    #[test]
    fn ls_less_colony_crew_consumes_baseline() {
        let mut recipes = BTreeMap::new();
        let _ = &mut recipes;
        let recipes = recipes;
        let conc = BTreeMap::new();
        let demands = crew_demands();
        let modules = vec![
            ColonyModule {
                def: gen_module(10.0),
                damaged: false,
            },
            ColonyModule {
                def: ColonyModuleDef::Store {
                    resource: ResourceId::from("oxygen"),
                    capacity: 500.0,
                },
                damaged: false,
            },
        ];
        let light = BodyLight {
            synodic_rate: 0.0,
            h0: 0.0,
            day_insolation_w_m2: 0.0,
            cos_lat: 1.0,
        };
        // 2 crew; every demanded resource stocked — the baseline
        // grounds (the modules' own "every request stocked or
        // produced" rule, all-or-nothing) and its rows register.
        let levels: BTreeMap<ResourceId, f64> = [
            (ResourceId::from("oxygen"), 100.0),
            (ResourceId::from("food"), 100.0),
            (ec(), 50.0),
        ]
        .into_iter()
        .collect();
        let solve = solve_network(&inp(
            &modules,
            ColonyState::Nominal,
            &levels,
            &all_caps(),
            &light,
            2,
            Daylight::Day,
            &recipes,
            &conc,
            &demands,
        ));
        // The baseline draws O2 at 0.02·2 = 0.04 and EC at 0.05·2 =
        // 0.1 against the 10/s generator.
        assert!(
            (solve.net_rates[&ResourceId::from("oxygen")] + 0.04).abs() < 1e-12,
            "O2 net {}",
            solve.net_rates[&ResourceId::from("oxygen")]
        );
        assert!((solve.net_rates[&ec()] - 9.9).abs() < 1e-9);
        assert!(!solve.ls_depleted);
        // Food unstocked → the whole baseline stands ungrounded (the
        // module precedent: a partially stocked need registers
        // nothing and the depletion scan still sees the demand — the
        // crew starves rather than half-eating).
        let short: BTreeMap<ResourceId, f64> = [(ResourceId::from("oxygen"), 100.0), (ec(), 50.0)]
            .into_iter()
            .collect();
        let solve = solve_network(&inp(
            &modules,
            ColonyState::Nominal,
            &short,
            &all_caps(),
            &light,
            2,
            Daylight::Day,
            &recipes,
            &conc,
            &demands,
        ));
        assert!(solve.ls_depleted, "unstocked crew need must deplete");
        assert!(
            !solve.net_rates.contains_key(&ResourceId::from("oxygen")),
            "an ungrounded baseline registers no rows: {:?}",
            solve.net_rates
        );
        // Stock everything — the same crew is satisfied, no
        // depletion, and the store drains at the baseline rate.
        let full: BTreeMap<ResourceId, f64> = [
            (ResourceId::from("oxygen"), 100.0),
            (ResourceId::from("food"), 100.0),
            (ec(), 50.0),
        ]
        .into_iter()
        .collect();
        let solve = solve_network(&inp(
            &modules,
            ColonyState::Nominal,
            &full,
            &all_caps(),
            &light,
            2,
            Daylight::Day,
            &recipes,
            &conc,
            &demands,
        ));
        assert!(!solve.ls_depleted);
        assert!(
            (solve.net_rates[&ResourceId::from("food")] + 0.005 * 2.0).abs() < 1e-12,
            "food net {}",
            solve.net_rates[&ResourceId::from("food")]
        );
        // And the twin gate: a `LifeSupport` module — even a bare
        // one — owns the demand itself; the baseline registers
        // nothing (no double-counting).
        let modules_ls = vec![
            ColonyModule {
                def: gen_module(10.0),
                damaged: false,
            },
            ColonyModule {
                def: ColonyModuleDef::LifeSupport {
                    consumes_per_crew: vec![],
                    power_ec_per_s: 0.0,
                },
                damaged: false,
            },
        ];
        let solve = solve_network(&inp(
            &modules_ls,
            ColonyState::Nominal,
            &levels,
            &all_caps(),
            &light,
            2,
            Daylight::Day,
            &recipes,
            &conc,
            &demands,
        ));
        assert!(
            !solve.net_rates.contains_key(&ResourceId::from("oxygen")),
            "the LS module's table owns the crew draw: {:?}",
            solve.net_rates
        );
        assert!(!solve.ls_depleted);
    }

    /// C09-F4's gate half, command-side: while life support is
    /// enabled, `AssignCrewToColony` refuses a colony with no
    /// undamaged `LifeSupport` module — the posting would starve the
    /// member on the baseline draw. A colony that stands one takes
    /// the member; a world with the subsystem off posts anywhere.
    #[test]
    fn assign_crew_refuses_ls_less_colony() {
        let mut w = colony_world(); // life support enabled
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::HireCrew {
                name: "a".into(),
                kind: crate::CrewKind::Pilot,
                skill: 1,
                salary_per_s: 0.0,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        found(&mut w, vec![gen_module(10.0)], vec![]);
        let colony = *w.colonies().keys().next().unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::AssignCrewToColony {
                colony,
                crew: CrewId(0),
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(
            r.rejected.iter().any(
                |x| matches!(&x.error, SimError::Recoverable(m) if m.contains("life-support"))
            ),
            "{:?}",
            r.rejected
        );
        assert!(!w.colonies()[&colony].crew.contains(&CrewId(0)));
        // A colony standing a working LS module takes the member.
        let mut w = colony_world();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::HireCrew {
                name: "a".into(),
                kind: crate::CrewKind::Pilot,
                skill: 1,
                salary_per_s: 0.0,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::LifeSupport {
                    consumes_per_crew: vec![],
                    power_ec_per_s: 0.0,
                },
            ],
            vec![],
        );
        let colony = *w.colonies().keys().next().unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::AssignCrewToColony {
                colony,
                crew: CrewId(0),
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        assert!(w.colonies()[&colony].crew.contains(&CrewId(0)));
    }

    /// C09-F6: a landed impact is no longer silent — the damage
    /// apply posts a program-level `ColonyDamaged` notification with
    /// the colony and the broken `ModuleIndex`.
    #[test]
    fn damage_posts_colony_damaged_notification() {
        let mut w = colony_world();
        found(&mut w, vec![gen_module(10.0)], vec![]);
        let colony = *w.colonies().keys().next().unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::DamageColonyModule { colony, module: 0 },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(
            w.notifications().iter().any(|n| matches!(
                n.kind,
                crate::reliability::NotificationKind::ColonyDamaged { colony: c, module: 0 }
                    if c == colony.0
            )),
            "no ColonyDamaged entry: {:?}",
            w.notifications()
        );
        assert_eq!(
            w.colonies()[&colony].state,
            ColonyState::Degraded {
                reason: DegradeReason::ImpactDamage
            }
        );
    }

    /// C09-F6's repair verb: the paid damage→repaired transition —
    /// funds debit at the authored cost, the module's `damaged` flag
    /// clears, the `ImpactDamage` reason lifts to `Nominal` once no
    /// damage stands, and the log ring gets its `ColonyModuleRepaired`
    /// record (the `ColonyDamaged` entry's inverse).
    #[test]
    fn repair_restores_module_and_lifts_impact_damage() {
        let mut w = colony_world();
        found(&mut w, vec![gen_module(10.0)], vec![]);
        let colony = *w.colonies().keys().next().unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::DamageColonyModule { colony, module: 0 },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        let funds0 = w.funds_micros();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::RepairColonyModule { colony, module: 0 },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        let c = &w.colonies()[&colony];
        assert!(!c.modules[0].damaged, "module stayed damaged");
        assert_eq!(
            c.state,
            ColonyState::Nominal,
            "the last damage clearing must lift the reason"
        );
        assert_eq!(
            w.funds_micros(),
            funds0 - w.economy_config().colony_module_repair_cost_micros,
            "repair must debit the authored cost"
        );
        assert!(
            w.notifications().iter().any(|n| matches!(
                n.kind,
                crate::reliability::NotificationKind::ColonyModuleRepaired {
                    colony: c,
                    module: 0
                } if c == colony.0
            )),
            "no ColonyModuleRepaired entry: {:?}",
            w.notifications()
        );
    }

    /// The transition is per-module: repairing one of two broken
    /// modules leaves the `ImpactDamage` reason standing — the
    /// colony is still a wreck, just a smaller one.
    #[test]
    fn repair_partial_damage_keeps_reason() {
        let mut w = colony_world();
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::Radiator {
                    area_m2: 1.0,
                    emissivity: 0.8,
                },
            ],
            vec![],
        );
        let colony = *w.colonies().keys().next().unwrap();
        for module in 0..2 {
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::DamageColonyModule { colony, module },
            );
        }
        w.step_frame(w.clock().ticks() + 1).unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::RepairColonyModule { colony, module: 0 },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        let c = &w.colonies()[&colony];
        assert!(!c.modules[0].damaged);
        assert!(c.modules[1].damaged);
        assert_eq!(
            c.state,
            ColonyState::Degraded {
                reason: DegradeReason::ImpactDamage
            },
            "one damaged module left must hold the reason"
        );
    }

    /// Repair under `Degraded{PowerLost}` clears the flag and keeps
    /// the reason — the damage apply's "PowerLost wins" rule runs in
    /// both directions: the repair restores the module, not the
    /// network's power balance, and `PowerRestored` stays that
    /// reason's only exit (the day crossing earns it back here).
    #[test]
    fn repair_under_power_lost_keeps_reason_then_restores() {
        let mut w = colony_world();
        // Solar + EC-drawing drill founded at midnight — the
        // `power_loss_cycle_transitions` fixture: night starves the
        // consumers, day restores.
        found(
            &mut w,
            vec![
                solar_def(1.0),
                ColonyModuleDef::Drill {
                    resource: ResourceId::from("ore"),
                    base_rate_per_s: 1.0,
                    min_concentration: 1.0,
                    power_ec_per_s: 0.5,
                },
            ],
            vec![],
        );
        let id = *w.colonies().keys().next().unwrap();
        w.step_frame(w.clock().ticks() + 64).unwrap();
        assert!(matches!(
            w.colonies()[&id].state,
            ColonyState::Degraded {
                reason: DegradeReason::PowerLost
            }
        ));
        // Damage lands under PowerLost — the reason holds (the
        // `apply_damage_module` rule), the flag still sets.
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::DamageColonyModule {
                colony: id,
                module: 0,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(w.colonies()[&id].modules[0].damaged);
        assert!(matches!(
            w.colonies()[&id].state,
            ColonyState::Degraded {
                reason: DegradeReason::PowerLost
            }
        ));
        // Repair clears the flag; `PowerLost` stands — the module is
        // whole again, the network is still dark.
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::RepairColonyModule {
                colony: id,
                module: 0,
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        assert!(!w.colonies()[&id].modules[0].damaged);
        assert!(
            matches!(
                w.colonies()[&id].state,
                ColonyState::Degraded {
                    reason: DegradeReason::PowerLost
                }
            ),
            "repair must not lift PowerLost: {:?}",
            w.colonies()[&id].state
        );
        // And the reason's own exit still works: sunrise recharges
        // the (undamaged) solar and `PowerRestored` lands.
        let light = body_light(
            &BodyId::from("terra"),
            0.0,
            0.0,
            &test_system(),
            &test_env()[&BodyId::from("terra")],
        );
        let sunrise = next_terminator_tick(&light, w.clock().ticks()).unwrap();
        let mut events = Vec::new();
        while w.clock().ticks() < sunrise + 64 {
            let r = w.step_frame(sunrise + 64).unwrap();
            events.extend(r.fired_events);
            if !r.partial {
                break;
            }
        }
        assert!(
            events
                .iter()
                .any(|e| matches!(e.kind, EventKind::PowerRestored { colony } if colony == id))
        );
        assert_eq!(w.colonies()[&id].state, ColonyState::Nominal);
    }

    /// The rejections — each `Recoverable`, each through the real
    /// command channel: unknown colony, out-of-range module, an
    /// undamaged module, insufficient funds, and a non-live colony
    /// (the damage apply's `Dormant`/`Abandoned` convention).
    #[test]
    fn repair_rejects_cleanly() {
        let mut w = colony_world();
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::Radiator {
                    area_m2: 1.0,
                    emissivity: 0.8,
                },
            ],
            vec![],
        );
        let colony = *w.colonies().keys().next().unwrap();
        // Damage module 0 so the undamaged-module and funds arms are
        // both reachable on the same fixture.
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::DamageColonyModule { colony, module: 0 },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        let funds0 = w.funds_micros();
        // Undamaged module → rejection, no charge.
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::RepairColonyModule { colony, module: 1 },
        );
        // Out-of-range module.
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::RepairColonyModule { colony, module: 9 },
        );
        // Unknown colony.
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::RepairColonyModule {
                colony: ColonyId(99),
                module: 0,
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert_eq!(r.rejected.len(), 3, "{:?}", r.rejected);
        assert!(
            r.rejected
                .iter()
                .all(|x| matches!(&x.error, SimError::Recoverable(_)))
        );
        assert_eq!(w.funds_micros(), funds0, "rejections must not charge");
        // Insufficient funds → the damaged module stays broken and
        // the debit never lands.
        w.economy.funds_micros = 0;
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::RepairColonyModule { colony, module: 0 },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert_eq!(r.rejected.len(), 1, "{:?}", r.rejected);
        assert!(matches!(
            &r.rejected[0].error,
            SimError::Recoverable(m) if m.contains("insufficient funds")
        ));
        assert!(w.colonies()[&colony].modules[0].damaged);
        // A non-live colony: abandon it, then the verb refuses like
        // the damage apply does.
        w.economy.funds_micros = funds0;
        w.enqueue(SourceClass::Ui, 0, SimCommand::AbandonColony { colony });
        w.step_frame(w.clock().ticks() + 1).unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::RepairColonyModule { colony, module: 0 },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert_eq!(r.rejected.len(), 1, "{:?}", r.rejected);
        assert!(matches!(
            &r.rejected[0].error,
            SimError::Recoverable(m) if m.contains("not live")
        ));
    }

    /// C09-F8's terminal-transition cleanup: `AbandonColony` vacates
    /// every vessel's `joined_colony` — a stale tag would freeze the
    /// member's transfer surface (the gate refuses non-live
    /// colonies) and serialize a lie onto a recovered hull.
    #[test]
    fn abandon_clears_joined_colony() {
        let mut w = colony_world();
        let ore = ResourceId::from("ore");
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::Store {
                    resource: ore.clone(),
                    capacity: 100.0,
                },
            ],
            vec![],
        );
        let colony = *w.colonies().keys().next().unwrap();
        let vessel = latched_vessel(&mut w, "ore", 100.0, 0.0);
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::JoinColonyInventory { vessel, colony },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        assert_eq!(w.vessels()[&vessel].joined_colony, Some(colony));
        w.enqueue(SourceClass::Ui, 0, SimCommand::AbandonColony { colony });
        w.step_frame(w.clock().ticks() + 1).unwrap();
        assert_eq!(w.vessels()[&vessel].joined_colony, None);
        // And the freed site takes a re-found (the C09-F9 half):
        // `Abandoned` does not occupy.
        let r = found(&mut w, vec![gen_module(10.0)], vec![]);
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        assert_eq!(w.colonies().len(), 2);
    }

    /// The same cleanup on the depletion path: a starved colony
    /// goes `Dormant` and its joined hulls lose the opt-in in the
    /// same firing.
    #[test]
    fn ls_depletion_clears_joined_colony() {
        let mut w = colony_world();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::HireCrew {
                name: "a".into(),
                kind: crate::CrewKind::Pilot,
                skill: 1,
                salary_per_s: 0.0,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        // An LS module demanding oxygen nothing stocks — crewed, it
        // depletes on the first recompute.
        found(
            &mut w,
            vec![
                gen_module(10.0),
                ColonyModuleDef::LifeSupport {
                    consumes_per_crew: vec![(ResourceId::from("oxygen"), 0.1)],
                    power_ec_per_s: 0.0,
                },
            ],
            vec![],
        );
        let colony = *w.colonies().keys().next().unwrap();
        let vessel = latched_vessel(&mut w, "ore", 100.0, 0.0);
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::JoinColonyInventory { vessel, colony },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        assert_eq!(w.vessels()[&vessel].joined_colony, Some(colony));
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::AssignCrewToColony {
                colony,
                crew: CrewId(0),
            },
        );
        // The assign's recompute finds no oxygen to serve the module
        // — `LifeSupportDepleted` lands a tick out; its firing
        // dormants the colony AND vacates the join (the C09-F8
        // half: the tag dies with the colony, not a manual leave).
        w.step_frame(w.clock().ticks() + 8).unwrap();
        assert_eq!(w.colonies()[&colony].state, ColonyState::Dormant);
        assert_eq!(w.vessels()[&vessel].joined_colony, None);
    }

    /// C09-F9: a second `FoundColony` at an occupied site refuses —
    /// `Nominal`/`Degraded`/`Dormant` all occupy; only `Abandoned`
    /// frees the slot for re-founding.
    #[test]
    fn double_found_rejected_and_refound_after_abandon() {
        let mut w = colony_world();
        let r = found(&mut w, vec![gen_module(10.0)], vec![]);
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        let r = found(&mut w, vec![gen_module(10.0)], vec![]);
        assert!(
            r.rejected
                .iter()
                .any(|x| matches!(&x.error, SimError::Recoverable(m) if m.contains("hosts a live colony"))),
            "{:?}",
            r.rejected
        );
        assert_eq!(w.colonies().len(), 1);
        // Abandon the wreck — the site re-founds (the salvage pays
        // out separately; occupancy is the live-state check).
        let colony = *w.colonies().keys().next().unwrap();
        w.enqueue(SourceClass::Ui, 0, SimCommand::AbandonColony { colony });
        w.step_frame(w.clock().ticks() + 1).unwrap();
        let r = found(&mut w, vec![gen_module(10.0)], vec![]);
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        assert_eq!(w.colonies().len(), 2);
    }

    /// C21-F8: the command path debits the authored
    /// `colony_founding_cost_funds` — the "funds ─► facilities"
    /// arrow `AbandonColony`'s salvage made a mint without it —
    /// and refuses outright when the balance can't pay (an action
    /// reject, never a clamp).
    #[test]
    fn founding_charges_funds_and_refuses_when_short() {
        let mut w = colony_world();
        let cost = w.economy_config().colony_founding_cost_micros;
        let before = w.funds_micros();
        let r = found(&mut w, vec![gen_module(10.0)], vec![]);
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        assert_eq!(w.funds_micros(), before - cost);
        assert!(
            w.economy()
                .ledger
                .iter()
                .any(|e| e.reason == crate::economy::FundsReason::ColonyFounding),
            "no ColonyFounding ledger row"
        );
        // Broke → the next founding refuses and nothing founds.
        w.economy.funds_micros = 0;
        let colony = *w.colonies().keys().next().unwrap();
        w.enqueue(SourceClass::Ui, 0, SimCommand::AbandonColony { colony });
        w.step_frame(w.clock().ticks() + 1).unwrap();
        let r = found(&mut w, vec![gen_module(10.0)], vec![]);
        assert!(
            r.rejected.iter().any(
                |x| matches!(&x.error, SimError::Recoverable(m) if m.contains("insufficient funds"))
            ),
            "{:?}",
            r.rejected
        );
        assert_eq!(w.colonies().len(), 1);
    }

    /// The fix-plan's phase-3-level validation: ONE coherence pass
    /// over the whole §3.5 economy loop, every leg a real
    /// `SimCommand` a shipped game can issue — no leg may dead-end:
    ///
    ///   found (funds debited, C21-F8) → crew (LS-gated, C09-F4) →
    ///   earn (resupply accept → ore delivery → complete, C34-F1) →
    ///   produce (drill + converter ore→metal, C09-F1) → haul back
    ///   (the ore-tank hauler, C33-F1) → recover (site inventory +
    ///   part value credited) → spend (ManufactureCraft BOM +
    ///   funds, C11-F4).
    ///
    /// The loop's honesty is the assertion: the colony's own
    /// hauled-back metal is literally the build's BOM — recover →
    /// site inventory → manufacture is the resource edge §3.5's
    /// diagram demands. The craft shapes are the test harness's
    /// sandbox convention (no parts catalog → `verify_bill`'s
    /// envelope no-ops; the catalog-verified path is covered by
    /// `manufacture::tests`), matching `latched_vessel_multi`'s
    /// `csp.tank.ore.s` hauler shape.
    #[allow(clippy::too_many_lines, clippy::cognitive_complexity)] // the loop's legs read as one pass
    #[test]
    fn economy_loop_end_to_end() {
        let ore = ResourceId::from("ore");
        let metal = ResourceId::from("metal");
        // A launch pad (recovery credit at 1.0 + the manufacture
        // bill site) on terra, and a bare outpost site for the
        // LS-gate leg.
        let mut w = World::with_system(test_system());
        w.set_colony_env(test_env());
        let mut pad = test_site();
        pad.launch = true;
        let mut mine = test_site();
        mine.id = SiteId::from("mine");
        w.set_sites(BTreeMap::from([
            (SiteId::from("pad"), pad),
            (SiteId::from("mine"), mine),
        ]));
        w.set_life_support_enabled(true);
        // The ore→metal smelter — the converter leg's recipe.
        let mut recipes = BTreeMap::new();
        let smelt = conv(
            &mut recipes,
            "smelt",
            vec![(ore.clone(), 1.0)],
            vec![(metal.clone(), 0.5)],
            5,
            0.5,
        );
        w.set_recipes(recipes);
        let t = resupply_template();
        w.set_contracts(BTreeMap::from([(t.id.clone(), t)]));

        // ── Found: the cost debits before the colony exists. ──
        let founding = w.economy_config().colony_founding_cost_micros;
        let funds0 = w.funds_micros();
        let r = found(
            &mut w,
            vec![
                gen_module(10.0), // RTG — 10 EC/s through day and night
                ColonyModuleDef::Battery { capacity: 500.0 },
                ColonyModuleDef::LifeSupport {
                    consumes_per_crew: vec![
                        (ResourceId::from("oxygen"), 0.02),
                        (ResourceId::from("food"), 0.005),
                    ],
                    power_ec_per_s: 0.05,
                },
                ColonyModuleDef::Drill {
                    resource: ore.clone(),
                    base_rate_per_s: 1.0,
                    min_concentration: 1.0,
                    power_ec_per_s: 0.5,
                },
                smelt,
                ColonyModuleDef::Store {
                    resource: ore.clone(),
                    capacity: 500.0,
                },
                ColonyModuleDef::Store {
                    resource: metal.clone(),
                    capacity: 500.0,
                },
                ColonyModuleDef::Store {
                    resource: ResourceId::from("oxygen"),
                    capacity: 500.0,
                },
                ColonyModuleDef::Store {
                    resource: ResourceId::from("food"),
                    capacity: 500.0,
                },
            ],
            vec![
                csp_data::ColonyStoreInit {
                    resource: ResourceId::from("oxygen"),
                    level: 500.0,
                },
                csp_data::ColonyStoreInit {
                    resource: ResourceId::from("food"),
                    level: 500.0,
                },
                csp_data::ColonyStoreInit {
                    resource: ec(),
                    level: 100.0,
                },
            ],
        );
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        assert_eq!(w.funds_micros(), funds0 - founding);
        let colony = *w.colonies().keys().next().unwrap();
        // The outpost stands a bare generator — deliberately LS-less.
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::FoundColony {
                def: ColonyDef {
                    site: SiteId::from("mine"),
                    display_name: csp_data::LocKey::from("colony.mine"),
                    modules: vec![gen_module(10.0)],
                    stores: vec![],
                },
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        assert_eq!(w.funds_micros(), funds0 - 2 * founding);
        let outpost = *w.colonies().keys().nth(1).unwrap();

        // ── Crew: the LS-equipped colony takes the posting; the
        // LS-less outpost refuses it. ──
        let salary = w.economy_config().crew_salary_per_s;
        let hire = w.economy_config().hire_cost_micros;
        for (i, name) in [(0u64, "eng"), (1, "sci")] {
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::HireCrew {
                    name: name.into(),
                    kind: crate::CrewKind::Engineer,
                    skill: 2,
                    salary_per_s: salary,
                },
            );
            let r = w.step_frame(w.clock().ticks() + 1).unwrap();
            assert!(r.rejected.is_empty(), "{:?}", r.rejected);
            assert_eq!(w.funds_micros(), funds0 - 2 * founding - (i + 1) * 2 * hire);
        }
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::AssignCrewToColony {
                colony,
                crew: CrewId(0),
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        assert!(w.colonies()[&colony].crew.contains(&CrewId(0)));
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::AssignCrewToColony {
                colony: outpost,
                crew: CrewId(1),
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(
            r.rejected.iter().any(
                |x| matches!(&x.error, SimError::Recoverable(m) if m.contains("life-support"))
            ),
            "the LS-less outpost must refuse the posting: {:?}",
            r.rejected
        );

        // ── Earn: publish → accept → deliver ore → the contract
        // completes and pays. ──
        w.step_frame(w.clock().ticks() + TICK_HZ).unwrap();
        assert!(
            !w.contracts.offers.is_empty(),
            "the board published nothing"
        );
        let offer = *w.contracts.offers.keys().next().unwrap();
        w.enqueue(SourceClass::Ui, 0, SimCommand::AcceptContract { offer });
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        let funds_after_accept = w.funds_micros();
        let hauler = latched_vessel_multi(
            &mut w,
            &[
                ("ore", 200.0, 50.0),
                ("metal", 60.0, 0.0),
                ("composite", 30.0, 0.0),
            ],
        );
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::JoinColonyInventory {
                vessel: hauler,
                colony,
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::TransferColonyResource {
                vessel: hauler,
                colony,
                resource: ore.clone(),
                amount: 10.0,
                to_colony: true,
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        w.step_frame(w.clock().ticks() + 64).unwrap();
        assert!(
            w.contracts.active.is_empty(),
            "the resupply did not complete on delivery"
        );
        // Advance 2.0 + completion 10.0.
        assert_eq!(w.funds_micros(), funds_after_accept + 10_000_000);

        // ── Produce: drill ore at the authored yield, smelt it to
        // metal — the ISRU half of the loop. ──
        w.step_frame(w.clock().ticks() + 20 * TICK_HZ).unwrap();
        let now = w.clock().ticks();
        let stock = w.colonies()[&colony].stores[&metal].level_at(now);
        assert!(
            stock >= 5.0,
            "the smelter produced {stock} metal in ~25 s — the converter leg is broken"
        );

        // ── Haul back + recover: metal into the ore carrier's tank,
        // recovery credits the pad's site inventory at 1.0. ──
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::TransferColonyResource {
                vessel: hauler,
                colony,
                resource: metal.clone(),
                amount: 5.0,
                to_colony: false,
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        w.step_frame(w.clock().ticks() + 32).unwrap();
        assert_eq!(
            crate::transfers::vessel_resource(&w.vessels()[&hauler], &metal),
            5.0
        );
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::RecoverVessel {
                vessel: hauler,
                site: Some(SiteId::from("pad")),
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        assert!(!w.vessels().contains_key(&hauler));
        let inv = &w.economy().sites[&SiteId::from("pad")].inventory;
        assert_eq!(inv[&ore], 40.0, "the 50−10 ore aboard credits at 1.0");
        assert_eq!(inv[&metal], 5.0, "the hauled-back metal credits at 1.0");

        // ── Spend: the recovered metal IS the build's BOM — the
        // §3.5 "resources ◄── colonies/ISRU" edge funding a build. ──
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::ManufactureCraft {
                site: SiteId::from("pad"),
                craft: tank_craft("metal", 20.0),
                build_resources: vec![(metal.clone(), 5.0)],
            },
        );
        let r = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        assert_eq!(w.builds.len(), 1);
        assert_eq!(
            w.economy().sites[&SiteId::from("pad")].inventory[&metal],
            0.0
        );
        // 100 kg at 50 kg/s → 128 ticks to ConstructionComplete.
        w.step_frame(w.clock().ticks() + 128).unwrap();
        assert!(w.builds.is_empty(), "the order did not deliver");
        assert_eq!(w.vessels().len(), 1, "the manufactured craft materialized");

        // The honest ledger: 2 foundings + 2 hires out, advance +
        // completion + recovery part value − build cost in.
        assert_eq!(
            w.funds_micros(),
            funds0 - 2 * founding - 2 * 2 * hire + 2_000_000 + 10_000_000 + 10_000_000 - 10_000_000,
            "the loop's cumulative funds ledger is dishonest"
        );
        assert!(
            w.economy()
                .ledger
                .iter()
                .any(|e| e.reason == crate::economy::FundsReason::ColonyFounding)
        );
        assert!(
            w.economy()
                .ledger
                .iter()
                .any(|e| e.reason == crate::economy::FundsReason::Recovery)
        );
    }
}
