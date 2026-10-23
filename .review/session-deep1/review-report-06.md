# Review Report — Cluster C06: Save/Restore Contract (csp-sim World::restore + save.rs)

Scope: `crates/csp-sim/src/lib.rs` L1169–3016 (WorldSnapshot, World::restore, check_serializable, snapshot), L7431–8391 (restore_* helpers, hash_craft/hash_vessel), `crates/csp-sim/src/save.rs` (full), plus cross-checks into `state_hash_masked` (L6898–7024), `hash_vessel` (L8971–9107), `hash_colonies` (colonies.rs:1131–1188), `hash_economy` (economy.rs:435–459), `hash_builds` (manufacture.rs:65–78), `csp_core::StateHasher::f64` (csp-core/src/hash.rs:275–304), `csp_phys::weld` (csp-phys/src/weld.rs:311,338–451), and the apply/firing paths that establish each invariant.

**Positive finding up front:** every *hashed* world field is present in `WorldSnapshot` and re-installed by `World::restore` — the walk in `state_hash_masked` maps 1:1 onto snapshot fields, and the only hashed-but-never-serialized field (`joints`) is correctly excluded by the §7.4 boundary refusal in `check_serializable` (L2811). The v1→v2 migration is a compiler-enforced field-wise copy; `parse_header` is panic-free on `u32::MAX` `header_len`; the inflate bound (`MAX_INFLATED_BODY`) and the canonical-CBOR re-encode check are sound. **No legit-save round-trip divergence was found.** The real problems are coherence gaps in restore's foreign-input validation — crafted/corrupt snapshots that build worlds the sim could never produce, several of them producing a permanent `state_hash` failure (debug: panic).

---

### [MEDIUM] Finding 1: Hashed-f64/structure payloads are unvalidated for OnRails vessels, non-InBubble crafts, `ActiveBuild.craft`, and colony records — a single crafted NaN bricks `state_hash` forever

**File:** `crates/csp-sim/src/lib.rs:8245`, 2672–2698, 7588–7605; `crates/csp-sim/src/manufacture.rs:59,74`; `crates/csp-sim/src/colonies.rs:1135–1175`

**Problem:** `World::restore`'s own contract is "a crafted or corrupt snapshot is refused with `Fatal` … never half-loaded into a world that would diverge or panic later" (L2258–2262). Yet several *hashed* payloads pass restore with no finiteness or structural validation:

- **`VesselState::OnRails(_) => {}`** (L8245) — the arm does nothing. The six hashed element f64s (hash_vessel L9064–9069) and `orbit.epoch`/`orbit.parent` are unchecked: no finiteness, no `parent ∈ orbits` (the orbit model is right there in `restore`'s signature), no element-range sanity. `apply_spawn_vessel` enforces both `body_orbit(parent)` and `state_at(orbit, clock)` (L4464–4465); restore enforces neither.
- **`CraftDef` internals are only validated via `csp_phys::weld` — which runs only for `InBubble` vessels** (L8211). `OnRails` and `SurfaceLatched` vessels can carry `Some(craft)`; restore checks their *references* (thermal/seams/reliability/contents/crew/science node keys, L8093–8192) but never the craft payload: part `mass_kg`/CoM/inertia/thermal/structural/force-sites, node transforms, `effective_index` bounds, `part_costs`, `reliability` specs — all hashed by `hash_craft` (L8259–8385), all unvalidated here. `weld`'s `validate_structure`/`validate_effective` (weld.rs:338–451) exists for exactly this but is never invoked on these residences.
- **`ActiveBuild.craft`** is a full `CraftDef`, hashed via `hash_builds → hash_craft` (manufacture.rs:74). `restore_build_state` (L7588–7605) checks id/site/event-pairing but never the craft — a NaN `mass_kg` rides into `deliver_built_vessel`.
- **Colony records:** `latitude_rad`, `longitude_rad`, `t_eq_k`, `module_fractions` values, and `ColonyModuleDef` resource-quantity pairs are all `h.f64`'d (colonies.rs:1135–1175,1193–1198) but the restore loop (L2672–2698) checks only `id == key`, `module_fractions.len() == modules.len()`, store coherence, and crew pairing. `pose.body`/`site` refs are likewise unchecked (consistent with the tech-tree precedent — acceptable for def-refs, but the *floats* are the issue).

**Evidence:** `StateHasher::f64` records a `Fatal` fault on non-finite input (csp-core/src/hash.rs:297–300); `debug_assert!` traps on it in debug builds (hash.rs:277); `finish` surfaces `Err(Fatal)` in release (hash.rs:426–430). So one NaN in any of the above fields → `state_hash()` returns `Err` on **every** call in release and **panics in debug builds** — the world is permanently unhashable, which is precisely the §7.5/T1 surface restore exists to protect.

**Impact:** crafted or corrupted save → `Ok(world)` from restore → a world that either panics the first `state_hash` (debug) or can never hash again (release), and where malformed crafts additionally `Fatal` on the first `ToBubble`/unlatch/`ConstructionComplete` weld instead of being refused at load. Foreign-input only — legit snapshots can't contain non-finite state (rule 4) — but refusing exactly this is the function's documented job.

**Suggestion:**
- `OnRails`: check all six elements finite (+ `a`/`e`/`M0` ranges), `orbit.parent` resolves in `self.orbits`, `orbit.epoch.ticks() <= clock_ticks` (the spawn invariant `state_at` implies), and `pose.body` resolves for `SurfaceLatched`.
- Add a `validate_craft(craft) -> Result<(), SimError>` that runs `weld`-equivalent structural checks (or literally `csp_phys::weld(&craft.graph, &craft.parts)` and discards the body — cheap enough at load) for **every** serialized `CraftDef`: `OnRails`/`SurfaceLatched` vessels and `ActiveBuild.craft` — plus finiteness walks over `part_costs` and `craft.reliability` specs.
- Colony: finite-check `latitude_rad`, `longitude_rad`, `t_eq_k`, each `module_fractions` entry, and module-def quantities (and optionally `inspection_tick`/pose refs per Finding 3).

---

### [MEDIUM] Finding 2: `next_*_id` "ahead of live ids" checks overflow on `id.0 + 1` — a `u64::MAX` arena key panics debug builds and silently bypasses the check in release

**File:** `crates/csp-sim/src/lib.rs:2328` (vessels), 2626 (scripts), 2699–2705 (colonies)

**Problem:** Three of the seven next-id checks use the `max_key + 1` formulation:

```rust
if snapshot.next_vessel_id < snapshot.vessels.keys().next_back().map_or(0, |id| id.0 + 1) {
```

A crafted snapshot whose top arena key is `u64::MAX` makes `id.0 + 1` overflow: **panic** under `overflow-checks` (debug/test — the same foreign-input panic class the `header_len` u32::MAX comment in save.rs:568–570 explicitly avoids), and silent wrap to `0` in release — where `next_*_id < 0` is always false, so the check is bypassed entirely.

The other four arenas use the safe per-entry form — `id.0 >= next_*_id → Fatal` (transfers L7518, builds L7589, benches L8065, research L7661, contracts L7850, part instances L8044) — which cannot overflow and is strictly stronger.

**Evidence:** with `vessels = {3: …, u64::MAX: …}` and `next_vessel_id = 3`, release restore accepts a world where the counter is *not* ahead of live ids; the next `apply_spawn_vessel`/`deliver_built_vessel` allocates `VesselId(3)` and `vessels.insert(3, …)` (L4472) silently **clobbers a live hashed arena entry** before `checked_add` can save anything — the counter overflow guard only fires when `next` itself is `u64::MAX`, not on a reused live id. Same shape for `scripts` (`LoadScript` inserts at `next_script_id`) and `colonies` (`FoundColony`).

**Impact:** debug-build panic on foreign input; release-build acceptance of an id-reuse state that produces silent hashed-arena corruption on the next allocation — the exact "monotonic, never reused" invariant these checks exist to protect.

**Suggestion:** replace the `map_or(0, |id| id.0 + 1)` form with the per-entry loop the other arenas use (`for id in snapshot.vessels.keys() { if id.0 >= snapshot.next_vessel_id { Fatal } }`), or `id.0.checked_add(1)` — one uniform formulation everywhere also removes the temptation to copy the fragile one.

---

### [MEDIUM] Finding 3: `colony.inspection_tick` has no ↔ pending-`ColonyInspection` pairing check — the only stored deadline the restore doesn't cross-check both directions

**File:** `crates/csp-sim/src/lib.rs:2459–2464`, 2672–2698; invariant source `crates/csp-sim/src/colonies.rs:1824–1828`, 2080–2108

**Problem:** `inspection_tick` is a *stored, hashed* deadline (colonies.rs:1178–1186) maintained by the same drop-and-republish discipline as every other paired record: `recompute_colony` schedules exactly one `ColonyInspection` at `colony.inspection_tick` (colonies.rs:2084–2108), and `apply_schedule_inspection` writes the record then republishes (1824–1829). The sim's invariant is therefore `Some(t) ⟺ exactly one pending event at t`.

The event walk collects `colony_inspections` and enforces uniqueness + colony existence (L2459–2464, 2485–2489) — but the per-colony loop (L2672–2698) never compares `c.inspection_tick` against the collected event set. Every sibling pairing *is* enforced: `wake_tick`↔`ScriptWake` (2631–2646), `deadline_tick`↔`ContractDeadline` (7856–7870), `complete_tick`↔event for transfers/builds/benches, `exhaust_tick`↔event pair, node↔`ManeuverNodeStart`, `pending_failure_tick`↔`PartFailure`.

**Evidence:** crafted `inspection_tick = Some(t)` with no pending event → the deadline is invisible to the heap until some *unrelated* recompute republishes it — possibly never, if no other colony event/command lands; the comment at colonies.rs:2076–2079 describes exactly this "silent cancel plus a leaked deadline" failure mode. Crafted event at `t_e` with `inspection_tick = Some(t_i)`, `t_i > t_e` → the firing's consume gate (`inspection_tick.is_some_and(|t| t <= now)`, colonies.rs:1903) fails, `recompute_colony` republishes a *second* inspection at `t_i` → two inspections.

**Impact:** a crafted/corrupt snapshot restores into a world whose colony-inspection behavior diverges from any reachable state — a lost or doubled inspection — where restore's contract is to refuse.

**Suggestion:** in the colony loop, `match (c.inspection_tick, pending_inspection.get(id)) { (Some(t), Some(&e)) if t == e => {}, (None, None) => {}, _ => Fatal }` — collected from the event walk alongside `colony_inspections` (needs tick values, so `BTreeMap<ColonyId, u64>` like `wake_events`); then refuse any pending inspection naming a colony whose `inspection_tick` is `None`/mismatched. (`Some(t)` with `t <= clock` is self-healing via the `t <= tick → None` clear at colonies.rs:2080, but the strict `t == event_tick` check handles it identically to the other pairs.)

---

### [MEDIUM] Finding 4: `economy.crew` roster keys are never checked against `next_crew_id` — a crafted roster id above the counter enables silent roster overwrite / duplicate crew id

**File:** `crates/csp-sim/src/lib.rs:7743–7798` (restore_economy_state), 7566–7573; `crates/csp-sim/src/economy.rs:396–399`, 756–760

**Problem:** `next_crew_id` is hashed + serialized as the single allocator for the whole `CrewId` domain — `BoardCrew` seats (transfers.rs:985) and roster hires (economy.rs:756) draw from the same counter. Restore checks crew ids `< next_crew_id` for **vessel seats** (L7569) and **transfer cargo** (L7545), but `restore_economy_state` never checks the **roster map's own keys**: `for (id, m) in &e.crew` (L7748) validates salary/skill/location only.

**Evidence:** crafted `economy.crew` containing `CrewId(5)` with `next_crew_id = 3` passes restore. Subsequent `HireCrew`/`BoardCrew` allocations walk 3→4→5; at `CrewId(5)`, `self.economy.crew.insert(id, CrewMember{…})` (economy.rs:760) silently **overwrites the existing roster member** — hashed state lost with no error. If instead `BoardCrew` reaches it first, a `VesselCrew{id:5}` seat coexists with roster member 5, and the *next* snapshot's restore fails `"crew id not unique / issued"` — corruption that surfaces a save later.

**Impact:** silent hashed-state corruption (member loss) or duplicate crew identity, both unreachable from any legit world — the roster is the arena §7.2 names and the counter's whole point is that every live id sits below it.

**Suggestion:** in `restore_economy_state`, `if id.0 >= snapshot.next_crew_id { Fatal("snapshot: roster id not below next") }` — mirrors the seated-crew check two helpers earlier.

---

### [LOW] Finding 5: `SiteState.id != arena key` unchecked — the one arena missing the `x.id == key` gate, and `s.id` (not the key) is what's hashed

**File:** `crates/csp-sim/src/lib.rs:7790–7796`; `crates/csp-sim/src/economy.rs:376–381`, 446–458

**Problem:** `SiteState` carries its key inline "per §2.2's idiom" (economy.rs:377). Every other arena enforces `inline id == map key` at restore (vessel L2335, script L2619, colony L2673, transfer L7521, build L7592, bench L8062, project L7664, contract L7850); the site loop checks only inventory finiteness. Uniquely here, the *hash* also reads `s.id` rather than the map key (`h.str(s.id.as_str())`, economy.rs:448) while iterating `e.sites.values()` in key order — so the hashed identity and the lookup identity can disagree.

**Impact:** crafted `sites["a"].id == "b"` restores into a world whose site rows lie about their identity — lookups key on `"a"` while the hash records `"b"`. Deterministic (round-trips), but unreachable-legit and contrary to the stated idiom.

**Suggestion:** `if s.id != *key { Fatal("snapshot: site id != arena key") }` in the site loop — one line matching seven other arenas.

---

### [LOW] Finding 6: `autopilots` keys not checked ⊆ `vessels`

**File:** `crates/csp-sim/src/lib.rs:2647–2651`; removal paths L3700, 3827

**Problem:** `EngageAutopilot` requires the vessel to exist (L4234) and every removal path deletes the autopilot with its vessel ("A bound autopilot must not outlive its vessel", L3822–3827). Restore validates each record's contents (`autopilot_state_is_valid`) but never that `snapshot.autopilots.keys() ⊆ snapshot.vessels` — while `tracked_vessels ⊆ vessels` *is* enforced (L2340–2344). A crafted orphan autopilot runs `driver.run_tick` each tick against a missing vessel — deterministic no-op-ish, but a state the sim cannot produce.

**Suggestion:** one-line `contains_key` gate in the same loop — the `tracked_vessels` precedent.

---

### [LOW] Finding 7: `script_op_budget == 0` admitted — asymmetric with the `event_budget == 0` refusal and the setter's `max(1)` clamp

**File:** `crates/csp-sim/src/lib.rs:2303–2305` (the `event_budget` gate), 2780 (uncheck copy), 2122 (`set_script_op_budget` clamps `.max(1)`)

**Problem:** `event_budget == 0` is `Fatal` because a legit world clamps it (`with_config`, L2026) — a 0 in the snapshot is "invalid input, not a value to silently repair". `script_op_budget` has the identical construction story (`DEFAULT_SCRIPT_OP_BUDGET` at L2077; `set_script_op_budget` clamps `.max(1)`) yet is copied unchecked at L2780. A 0 budget is hashed (L7010) and changes evolution — every script call exhausts instantly → poll-demotion → warp refusal naming the script — a state unreachable from any writer. Same check, same one line.

---

### [LOW] Finding 8: `header.body_len as usize` truncates on 32-bit targets — a lying `body_len` can satisfy the framing check

**File:** `crates/csp-sim/src/save.rs:542`

**Problem:** `header.body_len` is `u64`; `body_len as usize` truncates on wasm32 (`u64 → u32` `as`-cast). A header declaring `body_len = 2³² + body.len()` passes `!= body.len()` on wasm32 where it should fail — the §7.2 "truncated or appended file is refused before the codec runs" guarantee is silently conditional on pointer width. No memory-safety hole (inflate still bounds at `MAX_INFLATED_BODY` and CBOR must decode+re-encode), but the same foreign-input class as the `u32::MAX header_len` the code deliberately guards two functions earlier.

**Suggestion:** `if header.body_len != body.len() as u64` — lossless on every target.

---

### [LOW] Finding 9: Minor coherence-check gaps (bundled)

**File:** `crates/csp-sim/src/lib.rs:7566–7573`, 7538–7548, 8041–8050, 8061–8067; `crates/csp-sim/src/save.rs:470–487`

- **`crew_seen` covers seated crew only.** The helper documents "crew ids are a single arena: unique across every vessel" (L7511) but never inserts `TransferCargo::Crew` ids (nor colony crew-set members). A crafted snapshot can seat crew X aboard vessel A while X is also in-flight cargo (or cargo of two transfers). `land_crew`'s precondition no-ops (transfers.rs:1118–1128) contain the damage — no duplication actually materializes — but the uniqueness check is incomplete relative to its own doc.
- **`ActiveBench.site` unchecked.** `ActiveBuild.site` must name an `economy.sites` row (L7595); `ActiveBench.site` gets no such gate though `RunBenchTest` required a launch site (reliability.rs:1120–1127). Harmless today — `fire_bench_complete` ignores `site` (reliability.rs:1183) — but asymmetric.
- **Part-instance uniqueness is per-(vessel, instance), not global.** The doc claims "every part instance is unique across the save" (L8026) but `instances` is keyed `(vid, instance)` (L8049) — a crafted cross-vessel instance-id collision passes; two parts would share the `{RELIABILITY}`/`{IGNITION}` KDF entity stream.
- **`hex_decode` accepts uppercase** (`b'A'..=b'F'`, save.rs:479) though `SaveError::BadHeader`'s doc promises refusal of anything "not 64 lowercase-hex characters" (L367–368) — trivial doc/code mismatch; either drop uppercase or relax the doc.

---

## Coverage notes (checked, no finding)

- Snapshot↔hash completeness: all hashed fields (clock, warp, source_seqs, event_budget, caps, docking, transfer_cfg, next_vessel_id, vessels, tracked, next_event_seq, heap, joint_refused, next_crew_id, next_transfer_id, transfers, science_cfg, science, tech, economy_cfg, economy, save_seed, contract_cfg, contracts, next_colony_id, colonies, next_build_id, builds, life_support, script_op_budget, next_script_id, scripts, autopilots, next_part_instance_id, reliability_cfg, bench_runs, next_bench_id) serialize and restore; `joints` excluded by boundary refusal; `pending`/`command_log`/`script_log`/orbit-model/engine/aero-cache exclusions are all documented construction-config or output artifacts.
- Validation ordering: all `*_cfg.is_valid()` gates precede the event walk's `% period` modulos (the prior life_support_cfg bug is fixed — L2321–2327 comment).
- Event-pairing checks verified present for: maneuver nodes, transfers, builds, benches, research (exhausted+complete at `exhaust_tick`, recomputed via `research::exhaust_tick`), contracts, script wakes, part failures (one direction, orphans tolerated deliberately).
- Colony event dedup is correct: `earliest_store_bound` publishes at most one store-bound per colony (colonies.rs:1045–1047, 2093–2105), so Full+Empty sharing one set key cannot refuse a legit state; transitions dedup per kind.
- `event_heap` accepts `tick == clock` — correct for the documented mid-flush deferred-batch state (L2267–2269).
- save.rs: `parse_header` is slice-safe on `u32::MAX` `header_len`; canonical-CBOR re-encode catches trailing/truncated/non-canonical bodies; `body_len` framing + `UnsupportedEncoding`/`UnsupportedVersion`/`BadMagic`/`BadHeader` refusals all reachable and correctly ordered; migration chain is structurally sound.
- `BoardCrew` id-only members legitimately lack roster entries (lib.rs:1015–1017) — the `if let Some(m)` tolerance at L7781 is required, not a bug.
