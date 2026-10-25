# Cluster 8 Review — reliability, joints, save, life support, scripts host, events, physics bubble

Reviewer: ruthless-code-reviewer-8
Date: 2026-10-25
Method: static review only (no cargo runs). Findings appended as confirmed.

---

### [SEVERITY: MEDIUM] Finding 1: Burn time is silently discarded when a burn ends outside the per-tick shutdown path (no flight-qualification credit)
**File**: crates/csp-sim/src/reliability.rs:[773-782,1398-1404]
**Problem**: Flight qualification units are credited only in `bubble_part_tick`'s shutdown branch (`engine && rec.burning && throttle <= 0.0`, lines 1398-1404). Every other path that ends a burn drops the accrued burn seconds without credit: `reliability_residence_change(to_dormant=true)` (lines 773-782, called on rails exit / surface latch at lib.rs:5397 and manufacture.rs:335) sets `burning=false` via `restamp` but never emits a `burns` entry; an in-bubble `IgnitionFailure` crossing mid-burn (line 1261-1264) does the same.
**Evidence**: `rec.restamp(tick, spec.dormant_hazard_per_s(...), true, false); rec.misfire = false;` — `out.burns` does not exist in the residence-change signature; the accrued `tick - burn_start_tick` seconds vanish.
**Impact**: A vessel whose engine is still burning when it leaves the bubble (rail transition at lib.rs:5397, surface latch in `transition_latched`) or whose burn is cut mid-flight by an `IgnitionFailure` crossing loses the entire burn's qualification credit (in-bubble staging/undock do NOT lose it — the records travel and the next tick's shutdown branch credits them) — contradicting the module doc "flight units on burn completion (burn duration x a regime-weighted rate)" and under-crediting families vs. identical missions that shut down a tick earlier. Deterministic but unfair; hard to notice in tests.
**Suggestion**: Have `reliability_residence_change` (and the undock/breakup split in `split_records`) return the interrupted burns `(family, burn_s, in_atmosphere)` the same way `TickEffects::burns` does, and route them through the same `credit_qualification` write in lib.rs's post-pass.

### [SEVERITY: MEDIUM] Finding 2: The `theta = +inf` poison from `uniform() == 0.0` also strikes the renewal draws mid-game (builds on the known line-565 hash finding)
**File**: crates/csp-sim/src/reliability.rs:[1046,1260]
**Problem**: Known context covers `fresh_record`'s `theta = -log(u)` (line 414) producing `+inf` when `uniform()` returns exactly `0.0` (probability 2^-53 per draw), which `hash_record`'s `h.f64(r.theta)` (line 565) turns into a permanent `Fatal` state-hash fault (`StateHasher::write_f64` rejects non-finite, verified in crates/csp-core/src/hash.rs). The same unguarded `-libm::log(u)` exists at the two renewal sites: `apply_failure_at` line 1046 (`rec.theta += -libm::log(u)`) and `apply_mode_to_record` line 1260. Because `theta` is serialized in the save, a poisoned record bricks the save permanently — every future `state_hash` fails and every tick rolls back.
**Evidence**: `rec.theta += -libm::log(u);` with `u` from `uniform(...)` — no `u == 0.0` guard, unlike `dormant_failure_tick` which at least treats `!theta.is_finite()` as "never crosses".
**Impact**: Same permanent-Fatal/bricked-save outcome as the known finding, but reachable from a fresh draw after any failure (the renewal stream has as many draws as the part has failures, so exposure grows with mission time), not only at part creation.
**Suggestion**: Clamp at every draw site (e.g. `let theta = -libm::log(u.max(f64::MIN_POSITIVE));` or draw `u` in `(0,1)` via `1.0 - uniform(...)` reinterpreted) so `theta` is always finite; the `u == 0` draw then yields a very large but finite threshold. One shared helper fixes all three sites including line 414.

### [SEVERITY: MEDIUM] Finding 3: The v1 migration arm is decorative — `WorldSnapshotV1` aliases the live tree, so no genuine first-public-build v1 save can ever load
**File**: crates/csp-sim/src/save.rs:[186-330,625-645]
**Problem**: The module advertises a real §7.7 migration chain ("[`decode_save`] folds the `migrate_vN_to_vN+1` chain until the current schema falls out"), and `SAVE_VERSION_MIN = 1` claims v1 is readable. But `WorldSnapshotV1`'s fields are the LIVE shared types (`pub vessels: BTreeMap<VesselId, Vessel>`, live `WeldPart` inside, live M9b fields like `bench_runs`/`reliability_cfg` at the top level). The module's own version table documents that the live serialized tree changed under the v2 stamp (`WeldPart::engine` grew it) — which means the "frozen" v1 struct drifted with the live tree instead of being frozen. `migrate_v1_to_v2` (lines 336-378) is a pure field-wise copy that migrates nothing.
**Evidence**: A genuine first-public-build v1 body (pre-`engine`, pre-M9b fields) decoded under `WorldSnapshotV1` fails serde (no `#[serde(default)]` on any `WeldPart` field — verified in crates/csp-phys/src/part.rs:220-303) or fails the byte-exact re-encode gate; `decode_body`'s v1 arm maps every failure to `UnsupportedVersion { found: 1 }` (line 635). The only v1 bodies that load are current-schema bytes stamped `1` — the test `v1_save_migrates_to_current` builds exactly that circular fixture.
**Impact**: (1) Real old saves are refused, which is §7.7-legal, but with the message "save version 1 is outside this build's chain (1..=3)" — false: 1 is inside the chain; the schema is unresolvable. Users will conclude their save version is unsupported rather than understanding the real failure. (2) The migration chain provides zero actual migration capability — the next genuine schema change (v4) repeats the same trap unless the freeze rule is followed manually, and nothing in the code enforces it.
**Suggestion**: Either genuinely freeze v1 (copy the nested types, as the doc's parenthetical promises) with a real v1→current transform that synthesizes `engine`/M9b defaults explicitly, or drop the facade: set `SAVE_VERSION_MIN = SAVE_VERSION_CURRENT` and delete the dead `migrate_v1_to_v2`, with the UnsupportedVersion message naming "schema unresolvable under this build" for in-range stamps that fail the decode proof. If refusal is the intended behavior, at least fix the error message for in-range-but-unresolvable stamps (it currently claims the version is outside the chain for `found: 1` and `found: 2`).

### [SEVERITY: MEDIUM] Finding 4: Docking a spec'd vessel to a specless craft permanently kills the reliability model for BOTH halves — and undock never restores it
**File**: crates/csp-sim/src/joints.rs:[917-925] with crates/csp-sim/src/lib.rs:[6412-6418,6478-6481]
**Problem**: `apply_latch`'s merged reliability table is `Vec::new()` unless BOTH halves were `specs_parallel` (lines 917-925). `specs_parallel` is `reliability.len() == parts.len()`, so an EMPTY table (the legal "no model" state the spawn gate at lib.rs:5170-5179 accepts for hand-built/test crafts) is non-parallel — docking a real, fully-spec'd vessel to any specless craft empties the merged table. The undock/breakup split (lib.rs:6412-6418 and 6478-6481) then hands BOTH halves `reliability: craft.reliability.clone()` — the empty table — verbatim.
**Evidence**: joints.rs:917-925 `let reliability_table_m = if specs_parallel(&craft_p) && specs_parallel(&craft_s) { concat } else { Vec::new() };`; lib.rs:6480 `reliability: craft.reliability.clone()` on the fragment, lib.rs:6416 same on the retained side. The comment covers only the merged state ("its records go inert, never wrong") — not that the loss is permanent through undock.
**Impact**: One dock/undock cycle with a test craft silently and permanently disables the entire §3.4 failure model (hazard accrual, ignition draws, dormant publishes) for the previously healthy vessel and everything later docked to it: `specs_parallel` gates every reader, records keep hashing forever, and the restore gate accepts the empty table, so nothing ever flags it. Deterministic, silent, and unrecoverable in-session — the opposite of the module's "honest absence" framing, because the spec'd half had a model and lost it.
**Suggestion**: On the breakup split, rebuild each half's table slice from the parent's table when the parent's is parallel (it always is when both halves were parallel at merge) — or, simpler, store the reliability table per-half at merge time (e.g. remember each half's covering length) instead of concatenating/degrading positionally. At minimum, undock should re-derive `reliability` for the retained and fragment graphs from the surviving records' nodes rather than cloning the degraded empty table.

---

## Verdict

Four MEDIUM findings, no fresh CRITICAL/HIGH. This cluster is the most
defensively-engineered code I reviewed: the event heap's budget degrades
to `partial` (never drops), the snapshot enumerates the heap canonically,
the record↔event pairing invariant survives every residence/latch/split
transition I traced, and the scripts seam guards NaN persists at both the
host and restore boundaries. The real defects are quiet modeling/state
losses, not crashes: qualification credit vanishing on bubble exit, the
renewal draws sharing the known `theta=+inf` bricked-save path, a
migration chain that cannot migrate, and a dock/undock cycle that
permanently and silently switches off the reliability model. None are
save-corrupting; all are deterministic and easy to fix at the named sites.
events.rs, life_support.rs, bubble.rs, and scripts.rs: no findings.
