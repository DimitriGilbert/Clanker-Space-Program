//! Canonical state hashing (PLAN §7.1): BLAKE3-256 over a hand-written byte
//! stream — never serde, never derive order.
//!
//! The byte stream is produced by an explicit field walk written by hand for
//! each hashed type; the walk *is* the schema. Reordering fields in a walk is
//! a deliberate, golden-breaking change — which is the point: a derive-based
//! hash would let a stray field move silently red every golden and teach the
//! team to regenerate blindly.
//!
//! **Structural unambiguity.** Every variable-length item is preceded by its
//! `u32` element/byte count, so concatenations can never collide (`a|bc` vs
//! `ab|c` are distinct streams). Container *types* carry no magic tags:
//! domain separation between fields comes from the fixed field walk (each
//! position is written by exactly one writer) plus rule 7's `u16`
//! discriminants for sum types. The discipline this imposes on callers: one
//! canonical writer per logical field, a fixed walk per type, and never two
//! meanings behind one walk.

use std::collections::{BTreeMap, BTreeSet};

use crate::SimError;

/// Version of the hash canonicalization scheme (PLAN §7.1 rule 1). Hashed
/// first by every [`StateHasher`] and recorded next to every golden file in
/// the determinism fingerprint (§7.1). Must be bumped **in the same commit**
/// that regenerates goldens for a non-behavioural hash change, with the
/// reason in the commit message — `xtask bless-goldens` refuses otherwise.
///
/// **Version 2** (M1-B): the `World` walk gained the §7.2-serialized
/// event heap (in `(tick, seq)` order), `next_event_seq`, the vessel
/// arena + `next_vessel_id`, the tracked-vessel opt-in set, and the
/// event budget. Bumped in the same change as the trivial-golden rebless.
///
/// **Version 3** (M2-C): the per-vessel walk gained the physical-
/// residence payloads — each vessel's stored `craft` weld inputs (part
/// graph + effective parts, hashed in full because the weld's mass
/// properties are a pure function of them) and the `VesselState`
/// residence payloads: `InBubble`'s welded-body kinematic/control/settle
/// fields (`pos`, `rot`, `vel`, `ang_vel`, `throttle`, `settle`) and
/// `SurfaceLatched`'s body-fixed pose record (`body`, lat/lon/alt,
/// orientation). Bumped in the same change as the trivial and
/// flight-launch golden reblesses.
///
/// **Version 4** (M4a): the per-part walk gained the resolved `aero`
/// block (`cd_curve`/`cl_curve` knots + `ref_len_m` — craft state the
/// Stage-2 tables derive from), and the per-vessel walk gained
/// `aero_mask` (§2.4.1's active `ConfigMask`). Bumped for the
/// flight-launch golden rebless; the trivial golden is vessel-free
/// and unaffected.
///
/// **Version 5** (M4b): the per-part walk gained the resolved
/// `thermal` block (`heat_capacity_j_per_k`, `max_temp_k`,
/// `emissivity`, `conduction_w_per_k`), and the per-vessel walk
/// gained the authoritative per-part temperature record
/// (`Vessel::thermal` — serialized state, §7.1 rule 5). Bumped for
/// the flight-launch golden rebless; the trivial golden is
/// vessel-free but its stream still starts with the new version.
///
/// **Version 6** (M4c): the per-part walk gained the resolved
/// `structural` block (`max_axial_n`, `max_shear_n`, `max_torque_nm`
/// — the breakup thresholds, weld input like `aero`/`thermal`), the
/// per-vessel walk gained `debris_since` (§3.5's debris flag — the
/// `max_debris` eviction order is derived from it), the `World` walk
/// gained `vessel_caps` (`caps.max_vessels`/`caps.max_debris`, hashed
/// like `event_budget` because they change state evolution), and the
/// `EventKind` table gained the `DebrisDecay` kind (discriminant 3 —
/// heap entries of it now appear in the §7.2 walk). Bumped for the
/// golden rebless.
///
/// **Version 7** (M5-A/M5-B): the per-vessel walk gained `maneuver`
/// (the scheduled-Δv node — forward-looking intent, hashed like the
/// event heap it schedules into), and the `EventKind` table gained
/// `ManeuverNodeStart` (discriminant 4 — §2.6.5's `Clamping` member)
/// and `TransferWindow` (discriminant 5 — M5's transfer-window event,
/// scheduled by `SimCommand::WatchTransferWindow`) — appended, never
/// renumbered. Bumped for the golden rebless.
///
/// **Version 8** (M6-B): the walk gained the §2.4 docking config
/// (construction config that changes state evolution), the live
/// transient-joint list + refused-pair set (hashed-never-serialized —
/// §7.4 refuses the boundary while a joint island is live), and the
/// per-part `dock_ports` face specs inside `hash_craft`. Bumped for
/// the golden rebless.
///
/// **Version 9** (M6-C): `hash_vessel` gained the §2.4 `DockSeam`
/// records (`port_a`/`port_b`/`seam_node`/`local_a`/`local_b`/
/// `latch_tick`) — serialized state that outlives the transient and
/// must hash like the rest of the vessel — plus the latch settle
/// tunables on the docking config. `SimCommand::Undock` lands as an
/// append-only enum variant; `Vessel.seams` is a save-schema change,
/// so the goldens rebless.
///
/// **Version 10** (M6-D): `hash_vessel` gained tank contents +
/// crew seat records, `hash_craft` gained the resolved `tanks`/
/// `crew_seats` specs, and the world walk gained the transfer config,
/// `next_crew_id`/`next_transfer_id` counters, and the in-flight
/// transfer arena. `SimCommand::{TransferResource, TransferCrew,
/// BoardCrew}` and `EventKind::TransferComplete` land as append-only
/// enum variants; the goldens rebless.
///
/// **Version 11** (M7-A): `hash_vessel` gained the §3.2 science records
/// (keyed data pool + spent marks), `hash_craft` gained the resolved
/// `experiments`/`lab` specs, and the world walk appended the science
/// config and the science book (banked, ledger, project arena).
/// `EventKind::{ResearchDataExhausted, ResearchProjectComplete}` land
/// as append-only discriminants 7–8; the goldens rebless.
///
/// **Version 12** (M7-B): the world walk appended the §3.3/§3.4 tech
/// record — the researched node set, purchased upgrade-track levels,
/// and family qualification records (`tech::hash_tech`). No variant or
/// discriminant changes; the goldens rebless.
///
/// **Version 13** (M7-C): the world walk appended the §3.5 economy
/// book — funds micro-units, the insolvency flag, the crew roster,
/// the site inventory arena, and the serialized economy tunables
/// (the ledger inside `EconomyState` is recorded history, never
/// hashed — the `command_log` precedent); `EventKind` gained
/// `SalaryDue` (9) and `Insolvent` (10) — new heap entries; and
/// `hash_craft` walks the frozen `part_costs` table.
///
/// **Version 14** (M8-A): the world walk appended the §3.5.1 colony
/// book — `next_colony_id` and the colony arena
/// (`colonies::hash_colonies`: sites/poses, `ColonyState`, modules,
/// epoch-anchored linear stores, held solve outputs).
/// `EventKind` gained `TerminatorCrossing` (12),
/// `ColonyStoreFull`/`Empty` (13/14), `ColonyInspection` (15),
/// `PowerLost`/`PowerRestored` (16/17), `LifeSupportDepleted` (18) —
/// appended, never renumbered; `SimCommand` gained `FoundColony`,
/// `AbandonColony`, `AssignCrewToColony`, `EvacuateColony`,
/// `DamageColonyModule`, `ScheduleColonyInspection` — append-only;
/// `WorldSnapshot`/`WorldConfig` gained the colony fields. The
/// goldens rebless.
///
/// **Version 14** (M8-B continuation): the same bump also covered
/// the M8-B additions — the `builds`/`next_build_id` manufacture
/// arena, `Vessel.joined_colony`, and `EventKind::ConstructionComplete`
/// (19, `Clamping`) — appended under the same schema bump.
///
/// **Version 15** (M8-C): the world walk appended the §3.5
/// life-support pair — `life_support_enabled` (the per-save toggle)
/// and `life_support_cfg` (cadence + rates, the `economy_cfg`
/// precedent); the contract walk appended `facts.colony_deliveries`
/// (the resupply fact record) and the `ContractParams` table gained
/// the `ColonyResupply` tag (4). `EventKind` gained
/// `LifeSupportCheck` (20 — `Silent`, the `SalaryDue` precedent) and
/// `VesselLifeSupportDepleted` (21 — `Clamping`); `SimCommand`
/// gained `SetLifeSupportEnabled` — all appended, never renumbered.
/// `ContractKind` gained `ColonyResupply` (4). The goldens rebless.
///
/// **Version 16** (M9a): the world walk appended §2.10's script book —
/// `script_op_budget`, the script arena (`next_script_id` + each
/// record's source/enabled/strikes/poll/wake/`persist`), and the
/// engaged-autopilot arena. `EventKind` gained `ScriptWake` (22 —
/// `Clamping`); `SimCommand` gained `LoadScript`, `UnloadScript`,
/// `SetScriptEnabled`, `EngageAutopilot`, `DisengageAutopilot` — all
/// appended, never renumbered. `WorldSnapshot` gained the four script
/// fields (save format change). The goldens rebless.
///
/// **Version 17** (M9b): §3.4's reliability model — the craft walk
/// appended the frozen `ReliabilitySpec` table (parallel to
/// `parts`/`part_costs`), the vessel walk appended the per-node
/// `PartReliability` records (pre-drawn `θ`, usage, flags, and the
/// record-tail `renewal_count` — the per-failure draw ordinal,
/// distinct from the dedup'd `failures` list), and the world walk
/// appended `next_part_instance_id`, the reliability config, and the
/// bench arena + counter. `EventKind` gained
/// `PartFailure` (23 — Silent) and `BenchTestComplete` (24 — Silent);
/// `SimCommand` gained `RunBenchTest`; `FundsReason` gained
/// `BenchTest` (9); `FailureMode`/`StressKind` gained explicit stable
/// `u16` discriminants. `WorldSnapshot`/`TickShadow` gained the
/// reliability fields (save format change). The goldens rebless.
///
/// **Version 18** (M9c): §2.2.1's control lease — the vessel walk
/// appended `control_lease` (`Lease`, a new `repr(u16)` sum type:
/// `Player`/`Script`/`Agent`), `SimCommand` gained `TakeControl`
/// (appended, never renumbered), and `Vessel`/`WorldSnapshot` carry the
/// field (save format change). `UnloadScript` now also releases a
/// `Script(_)` lease on its bound vessel. The goldens rebless.
///
/// **Version 19** (deep-review Phase 9.2): the colony walk appended
/// `Colony::altitude_m` — the founding site's authored altitude,
/// latched so the impact seam and `vessel_colony_linked` read one
/// surface convention. The field itself landed in Phase 6 with
/// `#[serde(default)]` (pre-field saves decode at the old `0.0`
/// convention, so `SAVE_VERSION` is *not* bumped — `SaveBodyV2` wraps
/// the live `WorldSnapshot`); this version bump is the schema-
/// coordination that was deferred to 9.2. Lands with
/// `csp_data::DATA_HASH_VERSION` 2 (the §3.1 framing fix — a separate
/// constant, fingerprint-recorded). The goldens rebless.
///
/// **Version 20** (pass-1 propellant consumption): the craft walk
/// appended `WeldPart::engine` — the resolved `EngineSpec` carrying
/// the isp and propellant mass fractions the per-tick propellant
/// bookkeeping debits against, craft identity like the tanks it
/// drains. `WeldPart` gained the field (save format change — the
/// `SAVE_VERSION` 3 bump that should have landed here came later:
/// stamped-2 bodies now prove the current schema byte-exact, and a
/// body that fails the proof is refused `UnsupportedVersion` rather
/// than silently decoding `engine: None`). The goldens rebless.
///
/// **Version 21** (§3.1 `Parachute` deployable decelerators): the
/// craft walk appended `WeldPart::parachute` — the resolved
/// `ParachuteSpec` a `DeployPart` apply reads — and the vessel walk
/// appended `Vessel::deployed` — the per-node deploy record
/// (node → deploy tick) that decides the body's per-tick
/// `DecelSite`s. Both are hashed state; the goldens rebless.
///
/// **Version 22** (§2.2 `active_vessel`): the world walk appends the
/// player's focus record (`Option<VesselId>`) after `warp_level` —
/// §7.2's serialized set names it, so a lost record must not pass the
/// hash silently. The goldens rebless.
///
/// **Version 23** (§3.1 `Parachute` gate-anchored inflation, C14-F1):
/// `Vessel::deployed`'s value widens from the bare deploy tick to
/// `DeployRecord` — `deployed_tick` plus `opened_tick`, the first
/// tick the catch gates held, which anchors the inflation ramp the
/// body's `DecelSite`s read. Hashed state deciding future forces;
/// the goldens rebless.
///
/// **Version 24** (§3.5 insolvency consequences, C08-F2/F3): the
/// `economy_cfg` walk appended `crew_salary_per_s`,
/// `site_upkeep_funds_per_s`, and `attrition_grace_periods` (the
/// authored hire default, the per-site facility upkeep, and the
/// attrition grace — construction config that changes evolution),
/// `hash_economy` appended `EconomyState::unpaid_salary_streak`,
/// `EventKind` gained `StaffAttrition` (25 — `Clamping`) and
/// `FacilityDormant` (26 — `Clamping`), `FundsReason` gained
/// `SiteUpkeep` (10), and `NotificationKind` gained
/// `StaffAttrition`/`FacilityDormant` — all appended, never
/// renumbered. The goldens rebless.
///
/// **Version 25** (§3.5 manufacture hardening, C11-F3/C21-F2):
/// `ActiveBuild` gained `build_resources` — the order's spent BOM,
/// serialized so `CancelBuild` refunds exactly and hashed as part of
/// the build walk — `EventKind` gained `ConstructionRetry` (27 —
/// `Silent`), `FundsReason` gained `BuildRefund` (11), and
/// `NotificationKind` gained `BuildBerthBlocked`/`BuildDelivered` —
/// all appended, never renumbered. The goldens rebless.
///
/// **Version 26** (§3.5 colony consequence model, C09/C08/C21):
/// `EconomyConfig` gained `colony_founding_cost_micros` (hashed with
/// the config walk — it moves the funds trajectory), `HireCrew`
/// gained `skill` (a command-shape change — old logs lacking the
/// field won't decode, and none is recorded), `FundsReason` gained
/// `ColonyFounding` (12) and `NotificationKind` gained
/// `ColonyDamaged` — both appended, never renumbered. Crewed
/// colonies without a `LifeSupport` module now consume the baseline
/// set in the network solve. The goldens rebless.
///
/// **Version 27** (C14-F3/F4 + C12 physics): the `CraftDef` node
/// walk appended `PartNode::joint` — the snapped attach point in
/// the parent's local frame, hashed as weld input because the §2.4
/// joint-load torque arm now measures about it instead of the part
/// origin. `Environment` gained `convective_heat_fraction` (the
/// authored thermal η from `balance.ron`, construction config) and
/// the aero-table binding key became a geometry-only walk
/// (`hash_craft_geometry`) — mass is no longer key material.
/// `PartNode`'s serialized layout changed, so pre-field replay logs
/// do not decode; the goldens re-record via `record --force`.
pub const HASH_SCHEMA_VERSION: u32 = 27;

/// Rule-7 sum types: the source of an enum's *declared* stable `u16`
/// discriminant (PLAN §7.1 rule 7).
///
/// Implement on enums with explicit, append-only `#[repr(u16)]`
/// discriminants — never renumbered, see [`crate::StreamTag`] for the
/// pattern. Hash them via [`StateHasher::stable_enum`] so the value that
/// enters the stream is the type's declared constant rather than a `u16`
/// literal at the walk site: the type *is* the declaration, so a walk cannot
/// drift from the enum's own definition.
pub trait StableDiscriminant {
    /// The enum's explicit, stable `u16` discriminant.
    fn discriminant(&self) -> u16;
}

/// Canonical byte-stream hasher over BLAKE3-256 (PLAN §7.1).
///
/// `StateHasher::new()` writes [`HASH_SCHEMA_VERSION`] itself (rule 1) —
/// callers never emit it.
///
/// Rule-4 fault handling: NaN/±inf must never enter sim state. A non-finite
/// [`f64`](Self::f64) write traps in debug builds (`debug_assert!`); in
/// release it writes nothing, records the first fault, and [`finish`](Self::finish)
/// returns `Err(SimError::Fatal)` — which is how the §2.13 tick boundary in
/// `csp-sim` learns to abort the tick and roll back to the pre-tick snapshot.
/// Duplicate arena IDs and over-`u32` lengths are recorded the same way.
pub struct StateHasher {
    inner: blake3::Hasher,
    /// First recorded hashing fault, surfaced by `finish` as `Fatal`.
    fault: Option<SimError>,
}

impl StateHasher {
    /// New hasher with `HASH_SCHEMA_VERSION` already hashed (rule 1).
    pub fn new() -> Self {
        let mut h = Self {
            inner: blake3::Hasher::new(),
            fault: None,
        };
        h.u32(HASH_SCHEMA_VERSION);
        h
    }

    // -- rule 2: fixed-width little-endian scalars -------------------------

    /// `u8`, written verbatim.
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.inner.update(&v.to_le_bytes());
        self
    }

    /// `u16`, little-endian.
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.inner.update(&v.to_le_bytes());
        self
    }

    /// `u32`, little-endian. Also the canonical length prefix (rules 5, 6, 8).
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.inner.update(&v.to_le_bytes());
        self
    }

    /// `u64`, little-endian.
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.inner.update(&v.to_le_bytes());
        self
    }

    /// `i32`, little-endian two's complement.
    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.inner.update(&v.to_le_bytes());
        self
    }

    /// `i64`, little-endian two's complement.
    pub fn i64(&mut self, v: i64) -> &mut Self {
        self.inner.update(&v.to_le_bytes());
        self
    }

    /// `bool` as `0u8`/`1u8` (rule 2).
    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.u8(v as u8)
    }

    // -- rules 3–4: f64 ----------------------------------------------------

    /// `f64` as `to_bits()` little-endian after `-0.0 → +0.0` (rule 3).
    ///
    /// NaN and ±inf are forbidden (rule 4): debug builds trap; release builds
    /// write nothing, record a `Fatal` fault, and let `finish` surface it at
    /// the tick boundary — the tick aborts and rolls back per §2.13, so the
    /// skipped bytes are irrelevant. Use [`Self::try_f64`] to handle the
    /// error at the write site instead.
    pub fn f64(&mut self, v: f64) -> &mut Self {
        if let Err(e) = self.write_f64(v) {
            debug_assert!(false, "{e}");
            self.record_fault(e);
        }
        self
    }

    /// Checked [`Self::f64`]: returns `Err(SimError::Fatal)` on NaN/±inf
    /// instead of trapping in debug builds. The fault is still recorded, so
    /// `finish` fails even if the caller drops the `Err` — rule 4 is a sim
    /// invariant, not a local write error.
    pub fn try_f64(&mut self, v: f64) -> Result<&mut Self, SimError> {
        match self.write_f64(v) {
            Ok(()) => Ok(self),
            Err(e) => {
                self.record_fault(e);
                Err(e)
            }
        }
    }

    fn write_f64(&mut self, v: f64) -> Result<(), SimError> {
        if !v.is_finite() {
            return Err(SimError::Fatal("non-finite f64 in state hash"));
        }
        // Rule 3: -0.0 canonicalizes to +0.0 before `to_bits`.
        let v = if v == 0.0 { 0.0 } else { v };
        self.inner.update(&v.to_bits().to_le_bytes());
        Ok(())
    }

    // -- rule 3 applied to the math primitives --------------------------------
    //
    // The canonical writers the header's "one canonical writer per logical
    // field" discipline promised (C01-F1): every hand-rolled
    // `f64(x); f64(y); f64(z)` run in the walk compiled just as cleanly with a
    // dropped or duplicated line, silently excluding a component from the
    // determinism gate. The typed writers make the component count and order
    // a property of the type, not of the call site.

    /// `DVec3` as `x || y || z` (each an `f64` rule-3 write). Component order
    /// is the struct's declaration order — identical to the hand-rolled runs
    /// this replaces.
    pub fn dvec3(&mut self, v: crate::DVec3) -> &mut Self {
        self.f64(v.x).f64(v.y).f64(v.z)
    }

    /// `DQuat` as `x || y || z || w` — declaration order, `w` the scalar
    /// part, identical to the hand-rolled runs this replaces.
    pub fn dquat(&mut self, q: crate::DQuat) -> &mut Self {
        self.f64(q.x).f64(q.y).f64(q.z).f64(q.w)
    }

    // -- rule 7: option / enums ---------------------------------------------

    /// `Option`: `0u8` for `None`, `1u8 || payload` for `Some` (rule 7).
    pub fn opt<T>(&mut self, v: &Option<T>, mut f: impl FnMut(&mut Self, &T)) -> &mut Self {
        match v {
            Some(x) => {
                self.u8(1);
                f(self, x);
            }
            None => {
                self.u8(0);
            }
        }
        self
    }

    /// Sum-type discriminant: the enum's *explicit, stable* `u16`, followed
    /// by the variant payload written by the caller (rule 7). Discriminants
    /// are declared in code — append-only, never reordered. Prefer the typed
    /// [`Self::stable_enum`] for enums implementing [`StableDiscriminant`];
    /// this raw form remains for manual cases (e.g. writing a tag at a site
    /// that does not hold the enum value).
    pub fn enum_discriminant(&mut self, discriminant: u16) -> &mut Self {
        self.u16(discriminant)
    }

    /// Typed rule-7 discriminant write: hashes `v`'s declared
    /// [`StableDiscriminant`], then the caller writes the variant payload.
    /// Equivalent to `enum_discriminant(v.discriminant())` with the stable
    /// value supplied by the type itself rather than by convention.
    pub fn stable_enum<T: StableDiscriminant>(&mut self, v: &T) -> &mut Self {
        self.u16(v.discriminant())
    }

    // -- rules 6, 8: length-prefixed data ------------------------------------

    /// Raw bytes: `u32` byte length, then the bytes.
    ///
    /// Wire-identical to [`Self::str`]; if a schema must distinguish a string
    /// field from a blob at the same position, wrap one in a rule-7 enum.
    pub fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.write_len(v.len());
        self.inner.update(v);
        self
    }

    /// `String`/`str`: `u32` byte length, then UTF-8 bytes (rule 8).
    pub fn str(&mut self, v: &str) -> &mut Self {
        self.bytes(v.as_bytes())
    }

    /// `Vec`/slice: `u32` element count, then elements in index order
    /// (rule 6).
    pub fn vec<T>(&mut self, items: &[T], mut f: impl FnMut(&mut Self, &T)) -> &mut Self {
        self.write_len(items.len());
        for item in items {
            f(self, item);
        }
        self
    }

    /// `BTreeMap`: `u32` entry count, then `key || value` pairs in key order
    /// (rule 6). `BTreeMap` iteration *is* key order, which is why `HashMap`
    /// is banned in the determinism domain and this helper exists.
    pub fn btree_map<K, V>(
        &mut self,
        map: &BTreeMap<K, V>,
        mut key: impl FnMut(&mut Self, &K),
        mut value: impl FnMut(&mut Self, &V),
    ) -> &mut Self {
        self.write_len(map.len());
        for (k, v) in map {
            key(self, k);
            value(self, v);
        }
        self
    }

    /// Arenas (rule 5): `u32` count of *occupied* entries, then each entry as
    /// `id: u64 || payload` in **ascending monotonic-ID order**.
    ///
    /// `entries` is any iteration of `(id, payload)` pairs — dense slot
    /// order, whatever the arena hands out; this helper sorts by ID itself,
    /// so the wrong order is impossible to write. Empty slots are skipped by
    /// the caller; free lists and slot generations are allocator state, not
    /// world state, and are never hashed.
    ///
    /// Duplicate IDs are a corrupt-arena invariant violation (§2.13): a
    /// `Fatal` fault is recorded (surfaced by `finish`) and debug builds trap.
    pub fn arena<'a, T: 'a, I>(
        &mut self,
        entries: I,
        mut payload: impl FnMut(&mut Self, &T),
    ) -> &mut Self
    where
        I: IntoIterator<Item = (u64, &'a T)>,
    {
        let mut sorted: Vec<(u64, &'a T)> = entries.into_iter().collect();
        sorted.sort_unstable_by_key(|(id, _)| *id);
        for w in sorted.windows(2) {
            if w[0].0 == w[1].0 {
                let e = SimError::Fatal("duplicate monotonic ID in hashed arena");
                debug_assert!(false, "{e}");
                self.record_fault(e);
            }
        }
        self.write_len(sorted.len());
        for (id, entry) in sorted {
            self.u64(id);
            payload(self, entry);
        }
        self
    }

    /// Consume the hasher and finalize the 32-byte BLAKE3 digest.
    ///
    /// Returns `Err(SimError::Fatal)` if any write recorded a fault (non-finite
    /// `f64`, duplicate arena ID, over-`u32` length) — the §2.13 tick boundary
    /// propagates this to abort the tick and roll back.
    pub fn finish(self) -> Result<[u8; 32], SimError> {
        match self.fault {
            Some(e) => Err(e),
            None => Ok(*self.inner.finalize().as_bytes()),
        }
    }

    /// `u32` length prefix for container writers (rules 5, 6, 8). A count
    /// that does not fit `u32` is a structural invariant violation: the
    /// stream is already non-canonical, so `u32::MAX` is written to keep the
    /// byte position defined and the fault surfaces at `finish`.
    fn write_len(&mut self, n: usize) -> &mut Self {
        match u32::try_from(n) {
            Ok(n) => self.u32(n),
            Err(_) => {
                self.record_fault(SimError::Fatal("collection length exceeds u32"));
                self.u32(u32::MAX)
            }
        }
    }

    fn record_fault(&mut self, e: SimError) {
        self.fault.get_or_insert(e);
    }
}

impl Default for StateHasher {
    fn default() -> Self {
        Self::new()
    }
}

/// The §7.1 bubble mask — a named function, not an ad-hoc test filter.
///
/// The §2.6.1 ×1-vs-×10⁵ equivalence test must produce identical state hashes
/// *except for bubble contents*, so the equivalence hash walks each vessel
/// arena through this mask: given an arena's `(id, entry)` pairs and the set
/// of in-bubble entity IDs, it yields only the out-of-bubble (on-rails)
/// entries. Compose it with [`StateHasher::arena`], which still sorts
/// ascending by ID, so the mask cannot disturb rule-5 ordering:
///
/// ```ignore
/// h.arena(bubble_mask(vessels.iter_pairs(), &in_bubble), write_vessel);
/// ```
pub fn bubble_mask<'a, T: 'a, I>(
    entries: I,
    in_bubble: &BTreeSet<u64>,
) -> impl Iterator<Item = (u64, &'a T)>
where
    I: IntoIterator<Item = (u64, &'a T)>,
{
    entries
        .into_iter()
        .filter(move |(id, _)| !in_bubble.contains(id))
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers shipping
// fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// BLAKE3-256 of a byte stream assembled by hand in the test — every rule
    /// test compares `StateHasher::finish` against this independent encoding,
    /// so the tests pin the *byte format*, not just internal consistency.
    fn digest(bytes: &[u8]) -> [u8; 32] {
        *blake3::hash(bytes).as_bytes()
    }

    /// Every canonical stream begins with the schema version (rule 1).
    fn canonical_prefix() -> Vec<u8> {
        HASH_SCHEMA_VERSION.to_le_bytes().to_vec()
    }

    #[test]
    fn rule1_schema_version_is_emitted_first_by_the_hasher() {
        // A fresh hasher's stream is exactly the schema version, LE u32.
        let h = StateHasher::new();
        assert_eq!(h.finish().unwrap(), digest(&canonical_prefix()));
    }

    #[test]
    fn rule2_integers_fixed_width_le_and_bool_as_u8() {
        let mut expected = canonical_prefix();
        expected.extend(0xABu8.to_le_bytes());
        expected.extend(0x1234u16.to_le_bytes());
        expected.extend(0xDEAD_BEEFu32.to_le_bytes());
        expected.extend(0x0102_0304_0506_0708u64.to_le_bytes());
        expected.extend((-2i32).to_le_bytes());
        expected.extend((-3i64).to_le_bytes());
        expected.extend([1u8, 0u8]); // bool true, then false

        let mut h = StateHasher::new();
        h.u8(0xAB)
            .u16(0x1234)
            .u32(0xDEAD_BEEF)
            .u64(0x0102_0304_0506_0708)
            .i32(-2)
            .i64(-3)
            .bool(true)
            .bool(false);
        assert_eq!(h.finish().unwrap(), digest(&expected));
    }

    #[test]
    fn rule3_f64_is_bits_le_and_negative_zero_canonicalizes() {
        let mut expected = canonical_prefix();
        expected.extend(1.5f64.to_bits().to_le_bytes());
        // -0.0 must produce +0.0's bits.
        expected.extend(0.0f64.to_bits().to_le_bytes());

        let mut h = StateHasher::new();
        h.f64(1.5).f64(-0.0);
        assert_eq!(h.finish().unwrap(), digest(&expected));

        let mut a = StateHasher::new();
        let mut b = StateHasher::new();
        a.f64(-0.0);
        b.f64(0.0);
        assert_eq!(a.finish().unwrap(), b.finish().unwrap());
    }

    #[test]
    fn rule4_nan_and_infinities_are_rejected_as_fatal() {
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut h = StateHasher::new();
            assert!(matches!(h.try_f64(v), Err(SimError::Fatal(_))));
            // The recorded fault surfaces at `finish` even if the write-site
            // `Err` is dropped — this is the §2.13 tick-boundary mechanism.
            assert!(matches!(h.finish(), Err(SimError::Fatal(_))));
        }
    }

    /// Rule 4 in debug builds: a non-finite write traps immediately.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "non-finite f64")]
    fn rule4_f64_panics_in_debug() {
        StateHasher::new().f64(f64::NAN);
    }

    /// Rule 4 in release builds: the plain `f64` writer does not trap — it
    /// records a `Fatal` fault that `finish` surfaces, which is how the
    /// §2.13 tick boundary learns to abort the tick and roll back.
    #[test]
    #[cfg(not(debug_assertions))]
    fn rule4_f64_release_records_fatal() {
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut h = StateHasher::new();
            h.f64(v);
            assert!(matches!(h.finish(), Err(SimError::Fatal(_))));
        }
    }

    #[test]
    fn rule5_arena_hashes_ascending_id_order_with_id_prefixes() {
        // Fed in dense-slot (scrambled) order; the canonical stream must
        // order by ascending monotonic ID and prefix each entry with its ID.
        // Empty slots / free lists / generations simply never reach the walk.
        let (a, b, c) = (10u32, 20u32, 30u32);
        let mut h = StateHasher::new();
        h.arena([(7u64, &b), (1u64, &a), (42u64, &c)], |h, v| {
            h.u32(*v);
        });

        let mut expected = canonical_prefix();
        expected.extend(3u32.to_le_bytes()); // occupied-entry count
        for (id, v) in [(1u64, 10u32), (7u64, 20u32), (42u64, 30u32)] {
            expected.extend(id.to_le_bytes());
            expected.extend(v.to_le_bytes());
        }
        assert_eq!(h.finish().unwrap(), digest(&expected));
    }

    /// A duplicated monotonic ID is a corrupt-arena invariant violation:
    /// debug builds trap; release builds record a `Fatal` fault instead —
    /// covered by `rule5_duplicate_arena_id_release_records_fatal` below.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "duplicate monotonic ID")]
    fn rule5_duplicate_arena_id_panics_in_debug() {
        let (x, y) = (1u32, 2u32);
        StateHasher::new().arena([(9u64, &x), (9u64, &y)], |h, v| {
            h.u32(*v);
        });
    }

    /// Rule 5 in release builds: `arena()` does not trap on a duplicate ID —
    /// it records a `Fatal` fault that `finish` surfaces at the §2.13 tick
    /// boundary.
    #[test]
    #[cfg(not(debug_assertions))]
    fn rule5_duplicate_arena_id_release_records_fatal() {
        let (x, y) = (1u32, 2u32);
        let mut h = StateHasher::new();
        h.arena([(9u64, &x), (9u64, &y)], |h, v| {
            h.u32(*v);
        });
        assert!(matches!(h.finish(), Err(SimError::Fatal(_))));
    }

    #[test]
    fn finish_surfaces_any_recorded_fault_as_fatal() {
        // Same private mechanism every release-path fault uses.
        let mut h = StateHasher::new();
        h.fault = Some(SimError::Fatal("injected"));
        assert!(matches!(h.finish(), Err(SimError::Fatal(_))));
    }

    #[test]
    fn rule6_vec_is_u32_len_then_index_order() {
        let items = [10u16, 20, 30];
        let mut h = StateHasher::new();
        h.vec(&items, |h, v| {
            h.u16(*v);
        });

        let mut expected = canonical_prefix();
        expected.extend(3u32.to_le_bytes());
        for v in items {
            expected.extend(v.to_le_bytes());
        }
        assert_eq!(h.finish().unwrap(), digest(&expected));
    }

    #[test]
    fn rule6_btreemap_is_u32_len_then_key_order() {
        let mut map = BTreeMap::new();
        map.insert(9u8, "nine");
        map.insert(1u8, "one");
        let mut h = StateHasher::new();
        h.btree_map(
            &map,
            |h, k| {
                h.u8(*k);
            },
            |h, v| {
                h.str(v);
            },
        );

        let mut expected = canonical_prefix();
        expected.extend(2u32.to_le_bytes());
        for (k, v) in [(1u8, "one"), (9u8, "nine")] {
            expected.extend(k.to_le_bytes());
            expected.extend((v.len() as u32).to_le_bytes());
            expected.extend(v.as_bytes());
        }
        assert_eq!(h.finish().unwrap(), digest(&expected));
    }

    /// The math-primitive writers expand in the struct's declaration order —
    /// the exact `f64` component runs they replaced at the walk sites.
    #[test]
    fn math_primitives_expand_in_declaration_order() {
        let v = crate::DVec3::new(1.5, -2.0, 3.25);
        let q = crate::DQuat::new(0.0, 0.5, -0.5, 1.0);

        let mut expected = canonical_prefix();
        for c in [v.x, v.y, v.z, q.x, q.y, q.z, q.w] {
            expected.extend(c.to_bits().to_le_bytes());
        }

        let mut h = StateHasher::new();
        h.dvec3(v).dquat(q);
        assert_eq!(h.finish().unwrap(), digest(&expected));
    }

    #[test]
    fn rule7_option_is_u8_tag() {
        let mut h = StateHasher::new();
        h.opt(&Some(5u16), |h, v| {
            h.u16(*v);
        })
        .opt(&None::<u16>, |h, v| {
            h.u16(*v);
        });

        let mut expected = canonical_prefix();
        expected.push(1u8);
        expected.extend(5u16.to_le_bytes());
        expected.push(0u8);
        assert_eq!(h.finish().unwrap(), digest(&expected));
    }

    #[test]
    fn rule7_enum_discriminant_is_explicit_u16() {
        let mut h = StateHasher::new();
        h.enum_discriminant(7);

        let mut expected = canonical_prefix();
        expected.extend(7u16.to_le_bytes());
        assert_eq!(h.finish().unwrap(), digest(&expected));
    }

    #[test]
    fn rule8_string_is_u32_len_then_utf8() {
        let s = "véssel"; // multi-byte UTF-8 exercises byte length, not chars
        let blob = b"\x00\xff\x10";

        let mut h = StateHasher::new();
        h.str(s).bytes(blob);

        let mut expected = canonical_prefix();
        expected.extend((s.len() as u32).to_le_bytes());
        expected.extend(s.as_bytes());
        expected.extend((blob.len() as u32).to_le_bytes());
        expected.extend(blob);
        assert_eq!(h.finish().unwrap(), digest(&expected));
    }

    /// The unambiguity property: `u32` length prefixes mean different logical
    /// walks can never produce the same byte stream.
    #[test]
    fn length_prefixes_make_concatenation_unambiguous() {
        let mut h1 = StateHasher::new();
        h1.bytes(b"a").bytes(b"bc");
        let mut h2 = StateHasher::new();
        h2.bytes(b"ab").bytes(b"c");
        let mut h3 = StateHasher::new();
        h3.bytes(b"abc");

        let d1 = h1.finish().unwrap();
        let d2 = h2.finish().unwrap();
        let d3 = h3.finish().unwrap();
        assert_ne!(d1, d2);
        assert_ne!(d1, d3);
        assert_ne!(d2, d3);
    }

    #[test]
    fn bubble_mask_drops_only_in_bubble_ids() {
        let mut in_bubble = BTreeSet::new();
        in_bubble.insert(2u64);
        let (a, b, c) = (10u32, 20u32, 30u32);
        let entries = [(1u64, &a), (2u64, &b), (3u64, &c)];

        let mut h = StateHasher::new();
        h.arena(bubble_mask(entries, &in_bubble), |h, v| {
            h.u32(*v);
        });

        let mut expected = canonical_prefix();
        expected.extend(2u32.to_le_bytes()); // ID 2 is masked out
        for (id, v) in [(1u64, 10u32), (3u64, 30u32)] {
            expected.extend(id.to_le_bytes());
            expected.extend(v.to_le_bytes());
        }
        assert_eq!(h.finish().unwrap(), digest(&expected));
    }
}
