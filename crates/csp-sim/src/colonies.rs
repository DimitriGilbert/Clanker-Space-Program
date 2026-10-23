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
//!    salvage. `Dormant`/`Abandoned` **consume, produce, and schedule
//!    nothing** — the mechanism that keeps 20 half-dead colonies out of
//!    the event budget. `DegradeReason::ImpactDamage` is the
//!    crash-damage class — M8-A exercises it through
//!    `SimCommand::DamageColonyModule`; the bubble-impact detection
//!    hook remains outstanding (it is not part of M8-B's resource-
//!    flow scope — manufacture/transfers are).
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
//! kill"). `PowerRestored` is the ONLY way out of `Degraded`, and only
//! for `reason: PowerLost`: **`Degraded{ImpactDamage}` is terminal in
//! M8-A** — there is no repair command (one lands post-M8-A with the
//! impact-detection hook), so impact damage marks a colony dead
//! without killing its crew. If damage lands while a colony is already
//! `Degraded{PowerLost}`, the `PowerLost` reason wins (the apply only
//! records `ImpactDamage` from `Nominal`) — `PowerRestored` can still
//! return it to `Nominal`, damaged modules and all.
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
//! while the subsystem lives, and its firing re-checks the flag) —
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
/// place the network treats EC specially keys on this id.
pub const ELECTRIC_CHARGE: &str = "electric_charge";

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
    /// "a `Degraded` reason"); marked via
    /// `SimCommand::DamageColonyModule` — the bubble-impact detection
    /// that would drive it in play is not part of M8-B's scope.
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
    /// Aggregate capacity — Σ of the colony's `Store`/`Battery`
    /// module capacities for this resource (units).
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
    /// rotation rate resolves to the orbital mean motion and the
    /// synodic rate is 0 (a fixed sun: no terminators ever).
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
/// body-fixed longitude is `L(t) + π − ω·t` (orbital phase minus the
/// rotation angle — the prime-meridian convention anchors the
/// rotation angle at epoch 0), so
/// `H(t) = λ − (L0 + n·t) − π + ω·t = (λ − L0 − π) + (ω−n)·t`.
/// Daylight is `cos H · cos φ > 0` — terminator at `H ≡ π/2 (mod π)`,
/// which is what [`next_terminator_tick`] solves.
#[derive(Clone, Copy, Debug)]
pub struct BodyLight {
    /// `ω − n`: sub-solar longitude drift (rad/s) — the synodic rate.
    /// `0` = tidally locked / rotationless: a fixed sun, no crossings.
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
/// locked (`ω = n`); no insolation → dark solar.
pub fn body_light(
    body: &BodyId,
    latitude_rad: f64,
    longitude_rad: f64,
    orbits: &PatchedConics,
    env: &ColonyBodyEnv,
) -> BodyLight {
    let (n, l0) = orbits
        .body_orbit(body)
        .ok()
        .and_then(|o| o.elements.as_ref())
        .map_or((0.0, 0.0), |e| {
            let n = if e.period_s.is_finite() && e.period_s > 0.0 {
                TWO_PI / e.period_s
            } else {
                0.0
            };
            (
                n,
                e.mean_anomaly_epoch_rad + e.arg_periapsis_rad + e.ascending_node_rad,
            )
        });
    let omega = env
        .rotation_period_s
        .map_or(n, |p| if p > 0.0 { TWO_PI / p } else { n });
    let cos_lat = libm::cos(latitude_rad);
    BodyLight {
        synodic_rate: omega - n,
        h0: longitude_rad - l0 - PI,
        day_insolation_w_m2: env.insolation_w_m2.unwrap_or(0.0) * cos_lat * (2.0 / PI),
        cos_lat,
    }
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
/// whole-tick bound, strictly ahead. `None` for a tidally-locked or
/// non-rotating body (fixed sun) and at the poles (a permanent
/// terminator — [`POLE_COS_EPS`]).
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
    let dt = if dt > EPS { dt } else { spacing };
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
    // register neither.
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
    let outputs: Vec<BTreeSet<ResourceId>> = input
        .modules
        .iter()
        .enumerate()
        .map(|(i, m)| {
            if viable[i] {
                module_productions(m, input)
                    .into_iter()
                    .map(|(r, _)| r)
                    .collect()
            } else {
                BTreeSet::new()
            }
        })
        .collect();
    // Only grounded modules run at all: they register the consumers
    // and produce in the grant solve; the rest pin at fraction 0.
    let grounded = grounded_modules(input, &viable, &requests, &outputs);
    let active: Vec<bool> = (0..n).map(|i| viable[i] && grounded[i]).collect();
    let consumers = consumer_map(&requests, &active);
    let mut fractions = vec![1.0f64; n];
    let mut grants: BTreeMap<(usize, ResourceId), f64> = BTreeMap::new();
    let mut produced: BTreeMap<ResourceId, f64>;
    // Monotone fixpoint — each sweep's fractions are min(prev, computed).
    for _ in 0..n.saturating_add(1) {
        produced = production_at(input, &active, &fractions);
        grants.clear();
        for (r, list) in &consumers {
            let stock = input.levels.get(r).copied().unwrap_or(0.0);
            let mut remaining = produced.get(r).copied().unwrap_or(0.0)
                + if stock > EPS { f64::INFINITY } else { 0.0 };
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
        // New fractions — min over the module's input grants; inert
        // modules pin at 0.
        let mut changed = false;
        for (i, reqs) in requests.iter().enumerate() {
            let f = if active[i] {
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
    produced = production_at(input, &active, &fractions);
    Allocation {
        grants,
        fractions,
        produced,
        active,
    }
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
/// accumulator shared by the fixpoint sweeps and the final recompute.
fn production_at(
    input: &SolveInput,
    active: &[bool],
    fractions: &[f64],
) -> BTreeMap<ResourceId, f64> {
    let mut produced: BTreeMap<ResourceId, f64> = BTreeMap::new();
    for (i, m) in input.modules.iter().enumerate() {
        if !active[i] {
            continue;
        }
        for (r, q) in module_productions(m, input) {
            *produced.entry(r).or_insert(0.0) += q * fractions[i];
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
    // Damaged modules count nothing; everything else counts its
    // would-be draw.
    let latent_ec_request: f64 = input
        .modules
        .iter()
        .filter(|m| !m.damaged)
        .flat_map(|m| module_requests(m, input.crew, input.recipes))
        .filter(|(_, r, _)| *r == ec)
        .map(|(_, _, q)| q)
        .sum();
    // `power_starved` compares a DEMAND RATE (units/s) against the
    // supply side in matching units — the produced rate AND the
    // buffered level are separate terms, never summed (a rate plus a
    // level is dimensionally meaningless even when the >0 test reads
    // the same).
    let ec_produced = produced.get(&ec).copied().unwrap_or(0.0);
    let ec_stock = input.levels.get(&ec).copied().unwrap_or(0.0);
    let power_starved = latent_ec_request > EPS && ec_produced <= EPS && ec_stock <= EPS;
    // The depletion predicate scans EVERY LifeSupport module — NOT
    // just `active` ones: a damaged or ungrounded module has no grant
    // row (`g` reads 0), so crew aboard deplete the moment a required
    // store is empty — damage kills. Stocked reserves still cover the
    // request (the "switches to reserves" reading).
    let mut ls_depleted = false;
    if input.crew > 0 {
        for (i, m) in input.modules.iter().enumerate() {
            if let ColonyModuleDef::LifeSupport {
                consumes_per_crew, ..
            } = &m.def
            {
                for (r, q) in consumes_per_crew {
                    let req = q * input.crew as f64;
                    if req <= 0.0 {
                        continue;
                    }
                    let g = grants.get(&(i, r.clone())).copied().unwrap_or(0.0);
                    let stock = input.levels.get(r).copied().unwrap_or(0.0);
                    if stock <= EPS && g < req - EPS {
                        ls_depleted = true;
                    }
                }
            }
        }
    }
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
    def.modules.iter().all(|m| match m {
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
        ColonyModuleDef::Converter {
            recipe,
            power_ec_per_s,
            ..
        } => {
            // M8-B: the recipe must resolve, and its own rates must be
            // finite non-negative — a missing recipe is a command-side
            // rejection, not a silent inert module.
            nonneg(*power_ec_per_s)
                && recipes.get(recipe).is_some_and(|r| {
                    pairs(&r.inputs)
                        && pairs(&r.outputs)
                        && !ec_collision(&r.inputs, *power_ec_per_s)
                })
        }
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
        } => {
            pairs(consumes_per_crew)
                && nonneg(*power_ec_per_s)
                && !ec_collision(consumes_per_crew, *power_ec_per_s)
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
            if let Err(e) = self.apply_found_colony(def, &mut shadow) {
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
        self.shadow_events(shadow);
        let heap = std::mem::take(&mut self.event_heap);
        self.event_heap = heap
            .into_iter()
            .filter(|e| {
                event_colony(&e.kind) != Some(colony)
                    || (boundary_only && !is_colony_boundary(&e.kind))
            })
            .collect();
    }

    /// `FoundColony` — place `def` at its site: resolve the site's
    /// body/pose, build the aggregate stores (Store/Battery caps +
    /// founding levels), install the module list, and run the
    /// founding solve — daylight, allocation, terminator, and the
    /// first published deadlines. `Recoverable` rejections: unknown
    /// site, no colony env for the site body, non-finite or negative
    /// rates/levels.
    pub(crate) fn apply_found_colony(
        &mut self,
        def: &ColonyDef,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let Some(site) = self.site_defs.get(&def.site) else {
            return Err(SimError::Recoverable("found colony: unknown site"));
        };
        // Snapshot the resolved pose — `site` borrows the config
        // table, which the arena write below must not hold.
        let (site_body, site_lat, site_lon) =
            (site.body.clone(), site.latitude_rad, site.longitude_rad);
        let Some(env) = self.colony_env.get(&site_body).copied() else {
            return Err(SimError::Recoverable(
                "found colony: no colony environment for site body",
            ));
        };
        if !(site_lat.is_finite() && site_lon.is_finite()) {
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
        for c in &crew {
            if let Some(m) = self.economy.crew.get_mut(c)
                && m.location == crate::economy::CrewLocation::Colony(id)
            {
                m.location = crate::economy::CrewLocation::Unassigned;
            }
        }
        let colony = self
            .colonies
            .get_mut(&id)
            .ok_or(SimError::Fatal("abandon: colony vanished"))?;
        colony.state = ColonyState::Abandoned;
        colony.crew.clear();
        // Terminal records freeze too — "consumes, produces, and
        // schedules NOTHING" means the derived levels stop integrating:
        // re-anchor at the abandon tick and zero every net rate (the
        // salvage was already computed off the pre-freeze levels).
        let now = self.clock.ticks();
        for s in colony.stores.values_mut() {
            let level = s.level_at(now);
            s.anchor_level = level;
            s.anchor_tick = now;
            s.net_rate_per_s = 0.0;
        }
        self.drop_colony_events(id, false, shadow);
        Ok(())
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
        self.recompute_colony(id, self.clock.ticks(), shadow)
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
        for c in &crew {
            if let Some(m) = self.economy.crew.get_mut(c)
                && m.location == crate::economy::CrewLocation::Colony(id)
            {
                m.location = crate::economy::CrewLocation::Unassigned;
            }
        }
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
    /// module inert and drop the colony to `Degraded{ImpactDamage}`.
    /// (The bubble-impact detection that drives this in play is not
    /// part of M8-B's resource-flow scope.) A dormant or abandoned
    /// colony takes no new damage —
    /// it is already inert by state. M8-A has no repair path —
    /// `ImpactDamage` is a terminal `Degraded` reason (only a
    /// `PowerLost` reason can leave `Degraded`, via `PowerRestored`),
    /// and a `Degraded{PowerLost}` colony keeps its existing reason
    /// rather than switching.
    pub(crate) fn apply_damage_module(
        &mut self,
        id: ColonyId,
        module: usize,
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
        self.recompute_colony(id, self.clock.ticks(), shadow)
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
    pub(crate) fn apply_surface_impacts(
        &mut self,
        impacts: &[csp_core::DVec3],
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
        let radius_m = env.radius_m;
        let dominant = physics.dominant.clone();
        let range = self.transfer_cfg.radius_m;
        for pos in impacts {
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
                if colony.body != dominant
                    || matches!(colony.state, ColonyState::Dormant | ColonyState::Abandoned)
                {
                    continue;
                }
                // The colony's surface point — the latched-pose form
                // (`transfers::latched_pos`) at surface altitude.
                let site = crate::transfers::latched_pos(
                    &crate::SurfacePose {
                        body: colony.body.clone(),
                        latitude_rad: colony.latitude_rad,
                        longitude_rad: colony.longitude_rad,
                        altitude_m: 0.0,
                        orientation: csp_core::DQuat::IDENTITY,
                    },
                    radius_m,
                );
                if (*pos - center - site).norm() > range {
                    continue;
                }
                if let Some(idx) = colony.modules.iter().position(|m| !m.damaged) {
                    hits.push((*id, idx));
                }
            }
            for (id, module) in hits {
                self.apply_damage_module(id, module, shadow)?;
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

    /// One colony event's application — the event-side counterpart of
    /// the applies. Boundary kinds funnel into `recompute_colony`
    /// (each stamps its own reason first); transitions apply their
    /// state change, then recompute where the state stays live.
    #[allow(clippy::too_many_lines)] // a dispatch table, arm-per-kind
    pub(crate) fn fire_colony_event(
        &mut self,
        kind: &EventKind,
        _event_tick: u64,
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
        // The re-solve runs at the CLOCK, never at `event_tick`: in a
        // deferred-batch flush the event's tick sits behind the clock
        // — re-anchoring a store there would walk a level backward,
        // and `schedule_event` rightly Fatals on a non-forward tick.
        // Levels are linear, so re-anchoring at the landing tick is
        // the same value — and the clock is an absolute tick, so the
        // publish is identical in every §2.6.1 partition.
        let now = self.clock.ticks();
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
                // Crew aboard are Lost (the deaths setting's
                // `off | respawn` branch is post-M8-A; permanent is
                // the honest default); the colony goes Dormant and
                // schedules nothing ever again (rule 5).
                let crew: Vec<CrewId> = colony.crew.iter().copied().collect();
                self.shadow_colonies(shadow);
                self.shadow_economy(shadow);
                for c in &crew {
                    if let Some(m) = self.economy.crew.get_mut(c)
                        && m.location == crate::economy::CrewLocation::Colony(id)
                    {
                        m.status = CrewStatus::Lost;
                        m.location = crate::economy::CrewLocation::Unassigned;
                    }
                }
                let colony = self
                    .colonies
                    .get_mut(&id)
                    .ok_or(SimError::Fatal("life support: colony vanished"))?;
                colony.crew.clear();
                colony.state = ColonyState::Dormant;
                // Dormant consumes, produces, and schedules NOTHING —
                // the stores must stop evolving too: re-anchor at the
                // firing tick and zero every net rate, or `level_at`
                // keeps integrating the last live rates and a later
                // `AbandonColony` salvages production that accrued
                // while the colony was dead.
                for s in colony.stores.values_mut() {
                    let level = s.level_at(now);
                    s.anchor_level = level;
                    s.anchor_tick = now;
                    s.net_rate_per_s = 0.0;
                }
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
        let Some(env) = self.colony_env.get(&colony.body).copied() else {
            return Err(SimError::Recoverable(
                "colony: no colony environment for body",
            ));
        };
        let light = body_light(
            &colony.body,
            colony.latitude_rad,
            colony.longitude_rad,
            &self.orbits,
            &env,
        );
        self.shadow_colonies(shadow);
        let levels: BTreeMap<ResourceId, f64> = {
            let colony = self
                .colonies
                .get_mut(&id)
                .ok_or(SimError::Fatal("colony: vanished"))?;
            colony
                .stores
                .iter_mut()
                .map(|(r, s)| {
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
        let concentrations: BTreeMap<ResourceId, f64> = self
            .site_defs
            .get(&colony.site)
            .map(|d| d.resource_yields.iter().cloned().collect())
            .unwrap_or_default();
        let solve = solve_network(&SolveInput {
            modules: &colony.modules,
            state: colony.state,
            levels: &levels,
            light: &light,
            // M8-C §3.5's per-save toggle reads as zero crew for the
            // `LifeSupport` rows — the `crew > 0` gates in
            // `module_requests` and the depletion scan then close the
            // whole arm (no consumable draw, no EC draw, no depletion).
            crew: if self.life_support_enabled {
                colony.crew.len()
            } else {
                0
            },
            daylight: colony.daylight,
            recipes: &self.recipes,
            concentrations: &concentrations,
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
    #[allow(clippy::too_many_arguments)] // a test-side SolveInput builder
    fn inp<'a>(
        modules: &'a [ColonyModule],
        state: ColonyState,
        levels: &'a BTreeMap<ResourceId, f64>,
        light: &'a BodyLight,
        crew: usize,
        daylight: Daylight,
        recipes: &'a BTreeMap<RecipeId, RecipeDef>,
        concentrations: &'a BTreeMap<ResourceId, f64>,
    ) -> SolveInput<'a> {
        SolveInput {
            modules,
            state,
            levels,
            light,
            crew,
            daylight,
            recipes,
            concentrations,
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
            &light,
            0,
            Daylight::Day,
            &recipes,
            &conc,
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
            &light,
            0,
            Daylight::Day,
            &recipes,
            &conc,
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
            &light,
            0,
            Daylight::Day,
            &recipes,
            &conc,
        ));
        let expected = libm::sqrt(libm::sqrt(100.0 / (STEFAN_BOLTZMANN * 9.0)));
        assert!((solve.t_eq_k.unwrap() - expected).abs() < 1e-9);
        // No radiator → None (never a non-finite record).
        let bare = solve_network(&inp(
            &modules[..2],
            ColonyState::Nominal,
            &levels,
            &light,
            0,
            Daylight::Day,
            &recipes,
            &conc,
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
            &light,
            0,
            Daylight::Day,
            &recipes,
            &conc,
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
            &light,
            0,
            Daylight::Day,
            &recipes,
            &conc,
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
            &light,
            0,
            Daylight::Day,
            &recipes,
            &conc,
        ));
        assert_eq!(solve.fractions, vec![1.0, 1.0, 1.0]);
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
            &light,
            0,
            Daylight::Day,
            &recipes,
            &conc,
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
            &light,
            0,
            Daylight::Day,
            &recipes,
            &conc,
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
                .any(|(_, e)| matches!(e, SimError::Recoverable(_))),
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
            graph,
            parts: vec![csp_phys::WeldPart {
                experiments: Vec::new(),
                lab: None,
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
        let craft = tank_craft(resource, capacity);
        let mut tank_contents = crate::transfers::spawn_contents(&craft);
        for stores in tank_contents.values_mut() {
            for (r, c) in stores.iter_mut() {
                if r.as_str() == resource {
                    *c = fill;
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
                .any(|(_, e)| matches!(e, SimError::Recoverable(_))),
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
                salary_per_s: 0.0,
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        found(&mut w, vec![gen_module(10.0)], vec![]);
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
            },
            dominant: BodyId::from("terra"),
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
            &[csp_core::DVec3::new(6.0e6 + 200.0, 0.0, 0.0)],
            &mut shadow,
        )
        .unwrap();
        assert!(w.colonies()[&id].modules.iter().all(|m| !m.damaged));
        assert!(matches!(w.colonies()[&id].state, ColonyState::Nominal));
        w.apply_surface_impacts(&[csp_core::DVec3::new(6.0e6 + 5.0, 0.0, 0.0)], &mut shadow)
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
        w.apply_surface_impacts(&[csp_core::DVec3::new(6.0e6 + 5.0, 0.0, 0.0)], &mut shadow)
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
}
