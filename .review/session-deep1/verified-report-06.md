# Verified Report — Cluster C06: Save/Restore Contract (csp-sim `World::restore` + `save.rs`)

Verification of `.review/session-deep1/review-report-06.md` against source in
`crates/csp-sim/src/lib.rs`, `crates/csp-sim/src/save.rs`, and the cited
cross-checks (`csp-sim/src/{colonies,economy,manufacture,reliability,transfers,scripts,orbit,bubble}.rs`,
`csp-core/src/hash.rs`, `csp-phys/src/weld.rs`). Method: line-by-line code
trace; no reproduction was needed because every finding is a static
check-absence verifiable by reading.

**Scorecard: 9/9 findings verified — 8 fully CONFIRMED, 1 CONFIRMED with an
impact correction (Finding 3).** No finding is fabricated. Line citations in
the report are essentially exact (±a few lines; the file is 17,547 lines and
all cited ranges land where claimed).

**Contract answer (the verification question):** restore's documented
contract *does* cover these inputs. `lib.rs:2258–2262` states verbatim that
"the serialized set is validated against the world's own invariants before
anything is built — a crafted or corrupt snapshot is refused with `Fatal` …
never half-loaded into a world that would diverge or panic later." So the
crafted-snapshot coherence gaps below are real violations of the function's
own documented job, not merely robustness nits — even though every one of
them is foreign-input-only (legit snapshots cannot produce any of these
states, so no legit-save round-trip bug exists).

---

## [MEDIUM] Finding 1 — Unvalidated hashed-f64/structure payloads (OnRails, non-InBubble crafts, `ActiveBuild.craft`, colony floats)

**Verdict: CONFIRMED — every sub-claim verified.**

- `VesselState::OnRails(_) => {}` at `lib.rs:8245` — the arm is literally
  empty. `OrbitState` (`orbit.rs:207–213`) carries `parent: BodyId`,
  `epoch: SimTime`, and six element f64s, all hashed by `hash_vessel`
  (`lib.rs:9060–9069`). No finiteness check, no `parent ∈ orbits`, no
  element-range check. `apply_spawn_vessel` enforces both
  `body_orbit(&orbit.parent)` and `state_at(&orbit, self.clock)`
  (`lib.rs:4464–4465`); restore enforces neither. Notably, `restore`'s own
  signature takes `orbits: PatchedConics` (`lib.rs:2299`) but never threads
  it to `restore_vessel_state(vessel, physics.is_some())` — the parent
  check is trivially available yet absent.
- `csp_phys::weld` runs only for `InBubble` (`lib.rs:8211`). `SurfaceLatched`
  is required to have a craft (`lib.rs:8234`) and its pose floats are checked
  (`lib.rs:8237–8243`), but `pose.body` is never resolved against the orbit
  model and the craft payload is never structurally validated. `OnRails`
  vessels legitimately carry `Some(craft)` after bubble departure
  (`lib.rs:4762`), and restore does run the *reference* checks
  (thermal/seams/reliability/contents/crew/science node keys,
  `lib.rs:8093–8192`) — but never `weld`'s `validate_structure`/
  `validate_effective` (`csp-phys/src/weld.rs:338–451`), which is what
  rejects non-finite `mass_kg`/CoM/inertia/transforms/thermal/structural.
  `hash_craft` (`lib.rs:8259–8385`) hashes all of those fields.
- `ActiveBuild.craft` is a full `CraftDef` (`manufacture.rs:59`), hashed
  via `hash_builds → crate::hash_craft` (`manufacture.rs:74`).
  `restore_build_state` (`lib.rs:7588–7605`) checks id/site/event-pairing
  only. Confirmed.
- Colony floats: `latitude_rad`, `longitude_rad`, `t_eq_k`, each
  `module_fractions` entry, and `ColonyModuleDef` resource quantities are
  all `h.f64`'d (`colonies.rs:1135–1198`); the colony restore loop
  (`lib.rs:2672–2698`) checks `id == key`, `module_fractions.len()`,
  store coherence, and roster↔crew-set pairing — no float finiteness on
  the four hashed scalar families, no `body`/`site` resolution.
- The NaN→permanent-Fatal chain is real: `StateHasher::f64` records a
  `Fatal` fault and `debug_assert!(false)` on non-finite input
  (`csp-core/src/hash.rs:275–304`); `finish` surfaces `Err(Fatal)`
  (`hash.rs:426–430`). One crafted NaN → every `state_hash` call returns
  `Err` in release and panics in debug, forever.

---

## [MEDIUM] Finding 2 — `next_*_id` "ahead of live ids" checks overflow on `id.0 + 1`

**Verdict: CONFIRMED.**

- The three `map_or(0, |id| id.0 + 1)` formulations exist verbatim:
  vessels `lib.rs:2328`, scripts `lib.rs:2626`, colonies `lib.rs:2699–2705`.
- The safe per-entry form exists for the other arenas exactly as listed:
  transfers `lib.rs:7518`, builds `lib.rs:7589`, research `lib.rs:7661`,
  contracts `lib.rs:7850`, part instances `lib.rs:8044`, benches
  `lib.rs:8065`.
- Overflow semantics verified: `BTreeMap` `next_back()` returns the max
  key; a crafted `u64::MAX` key makes `id.0 + 1` panic under
  `overflow-checks` (debug) and wrap to `0` in release. Workspace release
  profile (`Cargo.toml:105–107`) sets no `overflow-checks`, so release
  wraps: `next < 0` is always false and the check is bypassed. This is the
  same foreign-input panic class `save.rs`'s `parse_header` comment
  deliberately avoids (`save.rs:568–575`).
- Consequence verified: with `{3, u64::MAX}` live and `next = 3`, the
  next allocation silently clobbers — `vessels.insert(id, …)`
  (`lib.rs:4472`), `scripts.insert(id, …)` (`lib.rs:6301`),
  `colonies.insert(id, colony)` (`colonies.rs:1539`). The counter's
  `checked_add` guard only fires when `next` itself is `u64::MAX`, not on
  a reused live id.

---

## [MEDIUM] Finding 3 — `colony.inspection_tick` has no ↔ pending-`ColonyInspection` pairing check

**Verdict: CONFIRMED — the missing check is real; the impact is somewhat
overstated.**

- The pairing gap is confirmed: the event walk collects
  `colony_inspections` into a `BTreeSet` — uniqueness + colony existence
  only (`lib.rs:2459–2464`, `2485–2489`), tick values are not recorded —
  and the per-colony loop (`lib.rs:2672–2698`) never compares
  `c.inspection_tick` against it. Every sibling pairing listed is
  enforced: `wake_tick`↔`ScriptWake` both directions (`lib.rs:2631–2646`),
  `deadline_tick`↔`ContractDeadline` (`lib.rs:7856–7870`),
  `complete_tick`↔events for transfers/builds/benches, node↔
  `ManeuverNodeStart` (`lib.rs:2591–2606`), `pending_failure_tick`↔
  `PartFailure` (`lib.rs:8052–8058`), `exhaust_tick`↔research pair
  (`lib.rs:7719–7726`).
- The sim-side invariant is real: `apply_schedule_inspection` writes the
  record then republishes (`colonies.rs:1824–1829`);
  `recompute_colony` drop-and-republishes one `ColonyInspection` at
  `inspection_tick` (`colonies.rs:2080–2108`); `inspection_tick` is
  hashed (`colonies.rs:1178–1186`). In a legit world
  `Some(t) ⟺ exactly one pending event at t` holds.
- **Correction to the impact:** the report's "invisible to the heap until
  some unrelated recompute republishes it — possibly never" overstates the
  missing-event direction. `publish_colonies` (`colonies.rs:2150–2188`)
  runs in every interval's subsystem order (`lib.rs:5992`) and repairs a
  missing deadline — for every *live* colony with a `colony_env` entry it
  schedules a `ColonyInspection` at `next_boundary` (which includes
  `inspection_tick`, `colonies.rs:1048`). So a crafted `Some(t)` with no
  pending event self-republishes within one interval for live colonies.
  The deadline is truly leaked only when (a) the colony's `body` has no
  `colony_env` entry — itself an unchecked crafted state — or (b) the
  colony is `Dormant`/`Abandoned` (publish skips them, `colonies.rs:2158`;
  and `apply_schedule_inspection` refuses dormant/abandoned at
  `colonies.rs:1819`, so `inspection_tick = Some` on a dormant colony is
  itself unreachable-legit and permanently hashed). Note the report's own
  suggestion's parenthetical (`t <= clock` is self-healing) is consistent
  with this repair path.
- The mismatch direction (pending event at `t_e` vs `inspection_tick =
  Some(t_i)`, `t_i > t_e`) is also real but converges rather than
  producing literally "two pending inspections": the `t_e` event fires,
  the consume gate (`colonies.rs:1903`) fails, `recompute_colony`
  republishes at `t_i` — the post-fire state is consistent again. The
  genuine divergence is the *extra firing/re-solve at the wrong tick
  `t_e`* — `recompute_colony` re-anchors stores at the fire tick (hashed
  `anchor_tick`/`solved_tick` fields), producing hashed state no legit
  trajectory reaches — plus the transient acceptance of the mismatched
  pairing itself. Net: the check is missing and the crafted state is
  unreachable-legit; "lost/doubled inspection" should be read as
  "wrong-tick re-solve or a conditionally-leaked deadline", which still
  violates the documented refuse-crafted-input contract.

---

## [MEDIUM] Finding 4 — `economy.crew` roster keys never checked against `next_crew_id`

**Verdict: CONFIRMED.**

- `restore_economy_state` (`lib.rs:7748–7775`) validates
  salary/skill/location for `for (id, m) in &e.crew` — there is no
  `id.0 >= snapshot.next_crew_id` gate on roster keys. Vessel seats are
  checked (`lib.rs:7569`) and transfer cargo is checked (`lib.rs:7545`);
  the roster is not.
- Both consequences verified: `HireCrew` allocates `CrewId(next_crew_id)`
  and `economy.crew.insert(id, …)` (`economy.rs:756–760`) — a silent
  overwrite of the crafted member; `BoardCrew` draws from the same
  counter (`transfers.rs:985`). The report's predicted follow-on error
  string is slightly off in one path — a seated `VesselCrew{id:5}`
  coexisting with roster member 5 trips the location-pairing `Fatal`
  ("seated crew's location is not its vessel", `lib.rs:7784`, or the
  roster-side `snapshot: crew location/seat record disagree`,
  `lib.rs:7758`) rather than "crew id not unique / issued" — but the
  substance (silent member loss, or duplicate crew identity that a later
  restore refuses) is exactly right.

---

## [LOW] Finding 5 — `SiteState.id != arena key` unchecked; hash reads `s.id`

**Verdict: CONFIRMED.**

- `SiteState` carries `id` inline "per §2.2's idiom" (`economy.rs:376–381`).
  The restore site loop iterates `e.sites.values()` and checks only
  inventory finiteness (`lib.rs:7790–7796`) — no `s.id == key`.
- `hash_economy` iterates `e.sites.values()` and hashes `s.id`
  (`economy.rs:446–458`), so lookup identity (map key) and hashed identity
  (inline id) can disagree. All seven sibling arenas enforce `x.id == key`
  (vessel `lib.rs:2335`, script `lib.rs:2619`, colony `lib.rs:2673`,
  transfer `lib.rs:7521`, build `lib.rs:7592`, bench `lib.rs:8062`,
  contract `lib.rs:7850`, project `lib.rs:7664`). Confirmed.

---

## [LOW] Finding 6 — `autopilots` keys not checked ⊆ `vessels`

**Verdict: CONFIRMED.**

- `lib.rs:2647–2651` iterates `snapshot.autopilots.values()` and validates
  records via `autopilot_state_is_valid` (`scripts.rs:583–585`) — the
  `BTreeMap<VesselId, AutopilotState>` keys (`lib.rs:1383`) are never
  checked against `vessels`.
- The invariant is real: `EngageAutopilot` requires the vessel
  (`lib.rs:4234`) and every removal path deletes the autopilot
  (`lib.rs:3700`, `lib.rs:3822–3827` — comment verbatim "A bound
  autopilot must not outlive its vessel"). The `tracked_vessels ⊆
  vessels` precedent exists at `lib.rs:2340–2344`. Confirmed.

---

## [LOW] Finding 7 — `script_op_budget == 0` admitted

**Verdict: CONFIRMED.**

- `event_budget == 0` → `Fatal` at `lib.rs:2303–2305`, justified by the
  `with_config` `event_budget.max(1)` clamp (`lib.rs:2026`).
- `script_op_budget` has the identical construction story — default
  `DEFAULT_SCRIPT_OP_BUDGET` at `lib.rs:2076`, the only setter clamps
  `ops.max(1)` (`lib.rs:2122`) — yet is copied unchecked at
  `lib.rs:2780`. It is hashed at `lib.rs:7010`. A crafted 0 is
  unreachable-legit and changes evolution. Confirmed.

---

## [LOW] Finding 8 — `header.body_len as usize` truncates on 32-bit targets

**Verdict: CONFIRMED.**

- `save.rs:542`: `if header.body_len as usize != body.len()` — `body_len`
  is `u64`; the `as usize` cast truncates on wasm32. A header declaring
  `2³² + body.len()` passes the framing check there. The same file's
  `parse_header` comment (`save.rs:568–575`) explicitly cites wasm32
  `usize` overflow as the foreign-input class to avoid — an internal
  inconsistency. No memory-safety hole: the inflate bound
  (`MAX_INFLATED_BODY`, `save.rs:93`) and canonical-CBOR decode+re-encode
  still run. Confirmed.

---

## [LOW] Finding 9 — Minor coherence-check gaps (bundled)

**Verdict: CONFIRMED — all four sub-items verified.**

- **`crew_seen` covers seated crew only** — `lib.rs:7566–7573` inserts
  only `vessel.crew` ids; `TransferCargo::Crew` ids get only the
  `< next_crew_id` gate (`lib.rs:7544–7548`), and colony crew-set members
  are checked only against roster location (`lib.rs:2692–2697`), not
  seat/cargo uniqueness. `land_crew`'s preconditions no-op on failure
  (`transfers.rs:1108–1140`), so the damage is contained as claimed —
  the check is incomplete relative to its own "single arena" doc
  (`lib.rs:7511–7512`).
- **`ActiveBench.site` unchecked** — `ActiveBench.site` exists and is
  hashed (`reliability.rs:357–358`, `630`); the restore loop
  (`lib.rs:8061–8076`) has no `economy.sites` gate though `RunBenchTest`
  requires a launch-site def + state row (`reliability.rs:1087–1096`).
  `fire_bench_complete` indeed ignores `site` (`reliability.rs:1170–1183`)
  — asymmetric but harmless today, as stated.
- **Part-instance uniqueness is per-(vessel, instance), not global** —
  `lib.rs:8049` keys `instances` on `(vid, rec.instance)`; the KDF
  `entity` is `instance.0` alone (`reliability.rs:407`, `491`, `550`,
  `1024`, `1230`), so a crafted cross-vessel `PartInstanceId` collision
  passes restore and two parts share the `{RELIABILITY}`/`{IGNITION}`
  stream. The doc at `lib.rs:8026` does claim "every part instance is
  unique across the save" — code/doc mismatch confirmed.
- **`hex_decode` accepts uppercase** — `save.rs:479` has the
  `b'A'..=b'F'` arm while `SaveError::BadHeader`'s doc promises "not 64
  lowercase-hex characters" (`save.rs:367–368`). Confirmed trivial
  doc/code mismatch.

---

## Coverage notes — re-verified

- Snapshot↔hash completeness confirmed by walking both sides:
  `state_hash_masked` (`lib.rs:6898–7024`) maps 1:1 onto `WorldSnapshot`
  fields (`lib.rs:1241–1402`); `joints` is the only hashed-never-serialized
  field and `check_serializable`'s boundary refusal covers it
  (`lib.rs:2811–2815`). The "no legit-save round-trip divergence" claim is
  consistent with the code.
- Validation ordering fix is present (`lib.rs:2321–2327` — `life_support_cfg`
  proven valid before the heap walk's `% check_period_ticks` modulo at
  `lib.rs:2504`).
- `tick == clock` events accepted at `lib.rs:2387` — matches the documented
  mid-flush deferred-batch carve-out (`lib.rs:2267–2269`).
- `parse_header` is panic-free on `u32::MAX` `header_len` (`save.rs:562–578`
  — `get()` slicing, no arithmetic). The canonical-CBOR re-encode check
  (`save.rs:586–598`) catches trailing/truncated/non-canonical bodies.
  `migrate_v1_to_v2` is a field-wise copy (`save.rs:316–356`).
- `BoardCrew` id-only members lacking roster entries: the `if let Some(m)`
  tolerance at `lib.rs:7781` is required; the doc basis is the
  `TransferColonyCrew` command doc at `lib.rs:1015–1017`.
- `orbit.parent`/`pose.body`/`colony.body`/`colony.site` non-resolution is
  additionally confirmed beyond the report: a crafted unknown `orbit.parent`
  makes every `next_soi_event`/`state_at` error (wedged, not panicking — it
  propagates `Err` through each interval), and a colony `body` with no
  `colony_env` silently skips the `publish_colonies` repair pass — the one
  case where Finding 3's leaked deadline is truly permanent.

## Summary

All nine findings are real and accurately located. They are uniformly
foreign-input-only — no legit-writer path can produce any of them — but
`World::restore`'s own doc comment (`lib.rs:2258–2262`) makes refusing
crafted/corrupt snapshots part of the function's contract, so each is a
contract violation, not a stylistic gap. The two highest-consequence items:

1. **Finding 1** — one crafted NaN in an `OnRails` element, a non-InBubble
   `CraftDef`, `ActiveBuild.craft`, or a colony float yields a world whose
   `state_hash` returns `Err(Fatal)` on every call (release) or panics
   (debug). Permanent, unrecoverable.
2. **Finding 2** — the `id.0 + 1` overflow both panics debug builds on
   foreign input and silently bypasses the monotonic-id invariant in
   release, enabling hashed-arena clobber on the next allocation.

Finding 3's missing pairing check is confirmed, but the "lost forever /
doubled pending inspection" impact is softened by the `publish_colonies`
repair pass — the accurate residual is a wrong-tick re-solve divergence or
a conditionally-leaked deadline (dormant colony / missing `colony_env`),
still unreachable-legit.
