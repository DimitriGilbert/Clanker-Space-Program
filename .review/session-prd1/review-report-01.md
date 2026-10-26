# PRD-01 Review — Determinism primitives (csp-core) + the world-state/hashing machinery in csp-sim

**Cluster:** PRD-01 (exploration-map §4, line 86)
**Files reviewed:** `crates/csp-core/src/{lib,time,hash,rng,error,vec3,quat,terrain}.rs` (there is no `math.rs` — math lives in `vec3.rs`/`quat.rs`/`terrain.rs`), plus the machinery it feeds in `crates/csp-sim/src/{lib,bubble,events,joints,maneuver,transfers,research,tech,economy,contracts,colonies,manufacture,life_support,reliability,scripts,save}.rs` and `crates/csp-bench/src/replay.rs`.
**PRD refs:** PLAN §2.1, §2.2, §2.2.1, §2.3, §2.6.x, §2.7, §7.1–§7.6.
**Verdict:** The determinism core is honest and heavily hardened — clearly reworked in the ruthless review. The hash walk covers every progression-era field, the KDF uses fixed `{tag, entity, counter}` arity at every draw site with golden-pinned outputs, command ordering is total and dequeue-stamped, the shadow/rollback discipline is pervasive, and the replay harness verifies stamps + clock + hash end-to-end. One real **completeness gap vs PLAN §2.3** remains (the 20 km bubble boundary and `active_vessel` sim state do not exist), plus a handful of small honesty/convention nits.

---

## Defects

### D1 — MEDIUM: `active_vessel` sim state and the §2.3 bubble-boundary rules do not exist

**Files:** `crates/csp-sim/src/lib.rs` (World struct ~L2108–2349; residence transition machinery L5615+); `data/balance.ron:8`; `crates/csp-data/src/balance.rs:67`; `crates/csp-data/src/validate.rs:1825`.

**Problem:** PLAN §2.2 requires "`active_vessel: Option<VesselId>` is **sim state** (it gates control authority), changed only by command," and §2.3 defines the physics bubble as radius 20 km with 10% hysteresis: "vessels go on rails at 20 km, rejoin at 18 km," with membership defined by distance to `active_vessel` (or surface-latch within it). None of this exists:

- `grep active_vessel` over all of `crates/` → zero hits. The `World` has no such field; the per-vessel `Lease` (M9c) took over the "control authority" role.
- `grep bubble_radius` over all of `crates/` → hits only in `csp-data` (`balance.rs` field + validator) and `data/balance.ron` comments. **The value is authored and validated but never consumed by the sim.** There is no automatic bubble→rails transition at 20 km, no 18 km rejoin, no distance-based membership predicate anywhere.
- Residence is exclusively explicit state (`VesselState::{OnRails, InBubble, SurfaceLatched}`) changed by `ToRails`/`ToBubble`/`Unlatch` commands, the settle-latch machinery (`transition_latched`, lib.rs:10190), SOI events, and breakup consequences. Whoever decides a vessel should leave the bubble does so by issuing a command (in practice, the player/view — `csp-view/src/lib.rs:1946` issues `ToBubble` for the focus target).

**Evidence:** The only radius in the sim's contact/warp paths is `env.radius_m`/`atm.top_m` (dominant-body sphere), never the 20 km bubble radius. The exploration map's own claim ("auto bubble→rails at boundary") could not be confirmed in code.

**Impact:** Determinism is *not* harmed — residence is explicit, hashed, serialized state, arguably *more* honest than a float-distance predicate (which PLAN itself flags as a "state-dependent rounding event" hazard for recentering). But the PRD contract is unimplemented: multi-vessel-in-one-bubble can only arise from explicit commands, the documented automatic boundary behavior (and its hysteresis) is absent, and a tunable the data pipeline maintains is dead. Any PRD-conformance audit will flag §2.3. Also: `Lease::Agent`/`Script` semantics aside, there is no sim-side notion of "the vessel the player is flying," so warp refusals/telemetry must re-derive "active" from the view layer — consistent today, but a seam PLAN says should be sim state.

**Suggestion:** Either (a) implement `active_vessel` + boundary transitions per §2.2/§2.3 (set on spawn/`TakeControl`-family command; auto `ToRails` past `bubble_radius_m`, auto re-materialize at 0.9× with the latch exemption), or (b) amend PLAN §2.2/§2.3 to document the explicit-residence model — it is a defensible, arguably superior design, but the spec currently promises machinery that does not exist, and `balance.bubble_radius_m` should be deleted or consumed.

---

### D2 — LOW: `CraftDef::part_ids` is serialized-but-unhashed on a claim ("nothing reads it after the spawn check") that is falsified in the current tree

**Files:** `crates/csp-sim/src/bubble.rs:102–126` (field + doc), `crates/csp-sim/src/lib.rs:9667–9671` (restore validity check), `crates/csp-sim/src/tech.rs:270–287` (`check_part_gate`), `crates/csp-sim/src/joints.rs:943–951` (merge propagation), `crates/csp-sim/src/reliability.rs:689–691` (`part_ids_parallel`), `hash_craft` at `lib.rs:9961` (deliberately omits `part_ids`).

**Problem:** The field doc asserts the table "is inert gate/build metadata — nothing reads it after the spawn check," which justified excluding it from `hash_craft`. In the current tree it is read post-apply in three places: `restore`'s crafted-snapshot gate (lib.rs:9670 — `part_ids.len() != parts.len()` is `Fatal`), the latch-merge propagation (joints.rs:943 — merged `part_ids` depend on `part_ids_parallel` of *both* halves), and `check_part_gate` (tech.rs:272 — invoked at every spawn/manufacture apply, i.e., on any future command carrying a craft, not just "the spawn check"). The merge case is the live one: two worlds differing only in `part_ids` produce different merged `part_ids` tables — same hash, different serialized state.

**Evidence:** Today every read is either (a) a logged-command gate whose verdict is replayable from `(command, construction-config catalog, hashed TechState)`, or (b) propagation of the unhashed table itself — never a branch on hashed content. So the exclusion is *defensible today* but rests on "nobody ever reads this to decide evolution," not on the documented "nobody reads this, period."

**Impact:** Not a live divergence: `state_hash` is identical across `part_ids` variants and no evolution path consumes it. The risk is latent — the inaccurate comment invites a future consumer (e.g., a contract predicate "part X flown," a recovery/insurance rule keyed on catalog id) to consume an unhashed field and silently open a real hash gap.

**Suggestion:** Either hash `part_ids` (cheap: `h.vec` of strings — but that's a `HASH_SCHEMA_VERSION` bump + golden rebless), or correct the comment to state the actual invariant — "never read to decide state evolution; gate verdicts on it are reproducible from the logged command" — and add a `// HASH-EXCLUDED` marker mirroring the `notifications`/`ledger` precedent so future reviewers see the deliberate choice.

---

### D3 — LOW (convention violation): `deadline_tick` Option is hashed with a `u16` tag, not rule 7's `u8` tag

**File:** `crates/csp-sim/src/contracts.rs:546–552` (`hash_contracts`).

**Problem:** §7.1 rule 7 defines `Option` as `0u8` | `1u8 || payload` — `StateHasher::opt` implements exactly that, and every other Option in the walk uses `opt` (or the byte-identical `h.bool` + payload seen in `hash_colonies`). `hash_contracts` writes `h.u16(1)`/`h.u16(0)` for `ActiveContract.deadline_tick` — a two-byte tag, off-spec.

**Impact:** Zero for determinism — the walk is the schema and is self-consistent — but it is exactly the kind of hand-walk drift §7.1's "explicit field walk" is meant to make *visible*, and it silently became part of the frozen schema at some version bump. Worth a comment marking it intentional (or fixing under the next schema bump).

**Suggestion:** Note it in the `HASH_SCHEMA_VERSION` changelog next bump, or convert to `h.opt` when the schema next moves.

---

### D4 — LOW (redundancy): `hash_script` writes `s.id` a second time inside the arena payload

**File:** `crates/csp-sim/src/scripts.rs:539–548`.

**Problem:** `hash_scripts` wraps records in `StateHasher::arena`, which already prefixes each entry `id: u64` (hash.rs:436). `hash_script` then writes `h.u64(s.id.0)` again inside the payload. Compare `hash_vessel`, which correctly relies on the arena prefix for `vessel.id`.

**Impact:** None — deterministic either way; just burns 8 bytes and a consistency nit for anyone diffing walks against structs. The `ScriptState.id` field itself is also validated `== key` at restore, so it's belt-and-suspenders, not a hazard.

**Suggestion:** Drop the inner `h.u64(s.id.0)` at the next schema bump, or leave and document — either way, harmless.

---

## Verified-clean areas (audit trail)

- **Canonical byte stream (csp-core/hash.rs):** every §7.1 rule implemented and pinned by independent byte-format tests — LE fixed-width ints, `bool`→u8, `to_bits` f64 after `-0.0→+0.0`, NaN/±inf→`Fatal` (debug trap, release records fault surfaced by `finish`), u32 length prefixes everywhere (unambiguity test at L742), arena = `len` + `id||payload` in ascending-ID order with duplicate-ID `Fatal`, `BTreeMap`/`Vec`/`Option`/explicit-`u16` enum walks, `bubble_mask` is the named §7.1 function. `HASH_SCHEMA_VERSION = 20` with a per-bump changelog naming what each version added — discipline PLAN §7.1 demands.
- **Hash coverage vs `World` fields:** cross-checked field-by-field (`state_hash_masked`, lib.rs:7993–8119). Every evolution-affecting field is walked: clock, warp, `source_seqs`, `event_budget`, `caps`, `docking`, `transfer_cfg`, `next_*` counters (vessel/crew/transfer/colony/build/script/part-instance/bench), vessel arena (via `hash_vessel`, covering craft/aero_mask/debris_since/seams/maneuver/tank_contents/crew/science/joined_colony/thermal/reliability/control_lease/state — all `Vessel` fields), joints + `joint_refused`, transfers, `tracked_vessels`, `next_event_seq`, event heap in `(tick,seq)` order, all subsystem books (science, tech, economy incl. crew roster + sites, contracts incl. `offer_index` + facts, colonies incl. stores/states/module fractions, builds, life-support flag+cfg, scripts incl. `persist` maps + wake/strike/poll state, autopilots, reliability cfg + benches). Exclusions are all documented: `command_log`/`ledger`/`notifications`/`script_log` = recorded history; `pending` = never-state (see below); physics/orbits/def-tables/engines = construction config (§7.3); `part_ids` = D2.
- **KDF/RNG (rng.rs):** exact §2.7 preimage `KDF_VERSION(u8) || seed(u64) || tag(u16) || entity(u64) || counter(u64)` → BLAKE3 → `ChaCha8Rng::from_seed` — never `seed_from_u64`. Frozen golden keys + 256-byte stream golden + `uniform` `to_bits` goldens pin it against dependency bumps. `uniform01` = `(next_u64 >> 11) · 2⁻⁵³`, single implementation shared by `uniform` and contracts' `next01`. Every production draw site uses fixed `{tag, entity, counter}` arity: `{RELIABILITY, instance, k}` incl. `MODE_COUNTER_BIT = 1<<63` disjoint mode draws (reliability.rs:131,403,490,1075,1292), `{IGNITION, instance, k}` (reliability.rs:549), `{CONTRACTS, 0, offer_index}` (contracts.rs:685). `U=0` clamped to `MIN_POSITIVE` before `−ln U` so θ can never go `+∞` and poison the hash permanently — a real corner caught.
- **Command ordering (§2.2.1):** `pending` is `BTreeMap<(SourceClass, u32), VecDeque>` — key order IS `(class, source_id)` order; per-FIFO production order supplies `source_seq`; stamps assigned at dequeue inside `drain_at`, never at enqueue; `not_before` enforces the one-tick latency for `issue`d commands (`producing tick + 1`); `source_seq` is hashed+serialized so restores continue stamping; rejected commands roll back their seq consumption and leave no log entry (`drain_at` L6871–6960). `command_log` is append-only in drain order ⇒ strictly sorted. `SourceClass`/`Lease`/`EventKind`/etc. all carry explicit `#[repr(u16)]` append-only discriminants hashed via `StableDiscriminant`.
- **Rollback (TickShadow):** per-iteration capture + lazy per-field capture, `merge_committed` keeps earliest captures, `drained`/`issued`/`seq_restore` rebuild the queue/counters exactly, `EventsShadow` Ops-mode reconstructs the boundary heap from the `next_event_seq` watermark. Per-command and per-event sub-shadows isolate non-`Fatal` failures onto `rejected`/`degraded` while `Fatal` aborts the whole iteration.
- **`step_frame` (§2.6.2):** boundary drain once per tick value, `t_next = min(effective_target, event_tick, now+budget)`, per-integer-tick bubble steps, constant `SUBSYSTEM_ORDER` interval advance, clock advance via fallible `try_advance`, same-tick event batch atomic with `MAX_EVENTS_PER_BATCH` deferred-tail handling that correctly postpones the next drain (flush rule survives snapshot/restore — restore explicitly admits `tick == clock` events for exactly this state).
- **Replay (`csp-bench/src/replay.rs`):** self-checking recorder, artifact = `{fingerprint, save_seed, data_fixture_hash, initial_snapshot, final_tick, command_log, config}`; replay restores, re-enqueues only external classes (Script/Autopilot re-derive), compares produced log entry-for-entry (`StampMismatch`), pins terminal tick (`ClockMismatch`), then hashes — with `AdvanceStalled` for zero-progress partials, fingerprint sidecar + advisory drift diffs, fixture-freeze hard check, and `GoldenOccupied`/`PartialGolden` anti-laundering refusals. The two golden scenarios (`trivial`, `flight-launch`) exist under `data/test-fixtures/`.
- **Ban-list compliance:** no `HashMap`/`HashSet`/`std::time`/`Instant`/`seed_from_u64`/`thread_rng`/`getrandom` in `csp-{core,sim,phys,data}/src`; no bare-`f64` transcendentals (`.sqrt/.sin/.cos/.exp/.ln/.asin/.atan/.atan2/...`) outside `libm::` in the determinism domain; ingress rate limits (32/tick, 64 KiB, bounded queue) implemented in `csp-server/src/ingress.rs` per §2.2.1.
- **`check_serializable` vs §7.4:** refuses on non-empty `pending` (undrained commands), live joints, and bubble bodies with live contacts not surface-latched (incl. vessel-vessel contact — beyond the plan's letter, correctly); rules 3–4 are vacuous here (Stage-2 tables compute synchronously sim-side; session `SaveBlocking` is view-side).
- **Math types:** `DVec3`/`DQuat` are exact-IEEE + `libm` only, with `to_bits` contraction-canary tests documenting the no-FMA reliance honestly (codegen observation + pinned toolchain + fingerprint, not a language guarantee); `SimTime` is a private-field `u64` whose only mutator is checked `try_advance` (Fatal on overflow, clock untouched); `terrain.rs` heightfield is integer-hash + f64 arithmetic with the sim/render sharing one baked field.
- **Restore validation:** `World::restore` refuses crafted snapshots across a long coherence list (event heap ordering/uniqueness/class consistency, event↔record pairing for maneuver/transfer/build/bench/wake/deadline/research/inspection, monotonic-counter bounds, quat finiteness+nonzero-norm, colony/roster cross-refs, global part-instance uniqueness for KDF entity safety).

## Informational (not defects)

- `StreamTag::Generation = 3` is declared but has no production draw site (rng.rs:32; only test usage) — a reserved tag; fine under append-only rules, but worth a comment saying so.
- `advance_orbits` (lib.rs:7041) calls `state_at` and discards the result — it's a non-convergence canary feeding `degraded`, correct per §2.8's "Recoverable + fall back to previous valid state" (epoch-anchored elements are the state; nothing to fall back *from*).
- `advance_interval_subsystems`'s colony/LS/reliability/economy slots are publish-only by design (closed-form lazy republish) — documented honest no-ops, matching PRD-03's "no-op-by-design" expectation.
- `hash_colonies`/`hash_builds` use manual `u32 len` + loops / `btree_map` instead of `vec`/`arena` — byte-identical output, just style drift.
- `CommandKey::source_id` u32 clamp (`id.min(u32::MAX)`) collapses ≥2³² ids onto one channel — documented; unreachable in practice.
- Save format (save.rs): `CSPSAVE\0` + JSON header + DEFLATE-over-CBOR body with `SAVE_VERSION_CURRENT=3`, `MIN=2`, and the stamped-2-body byte-exactness proof — matches §7.2/§7.7 (pre-1.0 may-invalidate policy implemented as refusal, not silent half-load).

## Cluster status vs map

The map's completeness verdict (**WIRED**) holds for the determinism primitives themselves. The one substantive PRD gap inside this cluster is **D1** (`active_vessel`/bubble-boundary machinery absent — a §2.2/§2.3 spec deviation, documented nowhere); everything else is correctness-verified or low-severity convention debt. The core is in good shape — the foundation is honest.
