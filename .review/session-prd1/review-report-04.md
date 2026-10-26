# PRD-04 Review — csp-sim/lib.rs §D: restore/breakup/hash + test suite

**Scope:** `crates/csp-sim/src/lib.rs` L8869–22489 — `shadow_events_*_split`, `event_names_vessel`,
`restore_*` per-arena coherence checks, `transition_latched`, `apply_breakup`, `hash_craft`,
`craft_graph_hash`, `aero_tables_stale`, `bubble_vessel_contact`, `hash_vessel`, and `mod tests`
(~11.6k lines, ~190 `#[test]`s). Cross-referenced PLAN §7.2/§7.4/§7.5/§7.6/§8.1-8.2, plus the host
save paths the sibling reviewer flagged (`csp-sim/src/save.rs`, `csp-server/src/backend.rs`,
`csp-bench/src/bin/csp-mcp.rs`).

**Verdict:** The in-crate three-way (snapshot ↔ hash ↔ restore) is coherent and unusually well
hardened — restore mirrors the apply gates field-by-field and re-derives every derived cache rather
than trusting it. The real divergence risk lives *outside* the crate: the MCP host writes a second,
incompatible `*.cspsave` format and re-installs only 3 of ~10 §7.3 construction-config channels on
`session.load` — a genuine "save that restores to a different hash" path. Test coverage is strong for
sim mechanics and restore-honesty but the lib.rs mod exercises **zero** colony/contract/manufacture/
tech/life-support commands; player-shaped coverage exists only piecewise in the submodule test mods.

---

## Defects

### D1 — HIGH — Two divergent `*.cspsave` save formats (sibling-reviewer's flag confirmed)

- **`crates/csp-sim/src/save.rs:7-16, 85-97, 292-394`** — the §7.2 CSPSAVE codec:
  `CSPSAVE\0 | header_len:u32 | UTF-8 JSON header | DEFLATE(CBOR body)`, with
  `save_version`/`SAVE_VERSION_CURRENT`/`SAVE_VERSION_MIN`, `game_version`, `data_set_hash`, and
  `hash_schema_version`. Module doc (save.rs:29-35) says bincode was *rejected* for saves deliberately.
  Only consumer: `csp-app`'s **bug-report bundles** (`csp-app/src/lib.rs:619, 2495, 2732-2816`) —
  there is no player-facing save/load path anywhere in the UI.
- **`crates/csp-server/src/backend.rs:256-291, 502-568`** — `SaveFile { schema:u32, tick:u64,
  hash:String, snapshot:WorldSnapshot }`, raw `bincode::standard()`, `SAVE_SCHEMA = 1`,
  `SAVE_EXT = "cspsave"`. Written by MCP `session.save`/`session.load`/`list_saves`.
- **Problem:** same file extension, incompatible byte formats, no mutual recognition. `decode_save`
  on an MCP file → `BadMagic`; `bincode decode` on a real CSPSAVE → garbage schema → the
  `header.schema != SAVE_SCHEMA` skip/error path. The MCP format carries no `save_version`,
  `game_version`, `data_set_hash`, or `hash_schema_version`, so §7.7's "loader refuses naming the
  versions" and the data-set drift warning can never fire on that path. The doc comment
  (backend.rs:256-258) cites "§7.6/§7.7" as precedent — but §7.6 is the *replay artifact*, which
  PLAN.md:1685 says explicitly is "**Not a save format**," and §7.2 picked CBOR+deflate for saves.
- **Impact:** whichever convention survives, half the `.cspsave` files on disk are unreadable by the
  other tool; an MCP-authored save can never become a game save, and a bug-report `save.cspsave`
  can't be fed to `session.load`. Versioning/migration machinery exists only on the side with no
  load path.
- **Suggestion:** pick one. Either route `session.save/load` through `csp_sim::save::encode_save`/
  `decode_save` (recommended — the CSPSAVE header already carries everything `SaveInfo` needs), or
  rename the MCP extension (`*.mcpsave`) and document the split. Either way, add a cross-format
  negative test.

### D2 — HIGH — `session.load` re-installs only 3 of ~10 §7.3 channels → save→load→N-ticks diverges

- **`crates/csp-bench/src/bin/csp-mcp.rs:80-93`** — `with_reconfigure` re-installs
  `set_script_op_budget`, `set_script_engine`, `set_autopilot_driver` — **only**.
- **`crates/csp-sim/src/lib.rs:3169-3195`** — `World::restore` leaves `colony_env`, `experiments`,
  `parts`, `recipes`, `tech_tree`, `site_defs`, `contract_defs` empty/`None`, plus `script_engine`/
  `autopilot_driver` `None` (the latter two covered by the hook).
- The world the fixture built had all of them (`csp-bench/src/scenarios.rs:184-246`;
  `csp-view/src/lib.rs:1138-1211` — ~10 `set_*` calls).
- **Problem:** the restored world runs with empty data-derived tables. `publish_contracts`
  (lib.rs:7016) lazily refills the offer board as a pure function of `(contract_defs, facts,
  offer_index, seed)` — an empty `contract_defs` produces a different board (or a `degraded` error),
  and `contracts.offers` is hashed state → the §7.5 "save → load → N ticks, hash vs uninterrupted"
  test would diverge on the next interval boundary for any save whose board is below `board_size`.
  Same class: `RunExperiment`/`StartResearch`/`TransmitData` resolve `experiments`; `RecoverVessel`/
  `ManufactureCraft`/`FoundColony` resolve `site_defs`/`recipes`; colony re-solves read `colony_env`;
  the `CraftDef::part_ids` tech gate resolves `parts`. All fail or diverge *silently* — no error says
  the channel was never re-installed.
- **Impact:** an MCP `session.save` → `session.load` round-trip is a different world with the same
  initial hash — the divergence only surfaces when a subsystem reads a missing table. This is the
  §7.5 hazard realized in production plumbing.
- **Suggestion:** centralize the re-install list (the `csp-bench::replay()` block at
  `replay.rs:783-819` is the correct superset — ~8 channels + engines) into one host-callable
  function, or carry the channel in `WorldConfig` the artifact pattern already uses. Better still:
  have `World::restore` record which channels the snapshot's live state *needs* (non-empty
  `science.projects` ⇒ experiments; non-empty `colonies` ⇒ colony_env; non-empty `contracts` ⇒
  contract_defs) and expose a `restore_warnings()`/`needs_config` checklist hosts must satisfy —
  the current contract is documentation-only.

### D3 — MEDIUM — `DebrisDecay` restores without a debris/residence check and fires `remove_vessel` unconditionally

- **`lib.rs:7623-7625`** — `EventKind::DebrisDecay { vessel } => self.remove_vessel(*vessel, shadow)`
  — no `debris_since` check, no residence check.
- **`lib.rs:2962-2966`** — restore only checks `vessels.contains_key(&v)` for vessel-named events.
- **Problem:** every other vessel-named event either pairs with a live record both directions
  (`ManeuverNodeStart`↔`Scheduled` node, `PartFailure`↔`pending_failure_tick`) or is a documented
  tolerated stale no-op. `DebrisDecay` is neither: the producer only schedules it for **on-rails
  debris** (`schedule_debris_decay`, lib.rs:4598-4604; scrubbed at `ToBubble` via
  `event_names_vessel`, lib.rs:8930), and `remove_vessel` unconditionally deletes the vessel —
  including roster kill via `lose_aboard_crew` (lib.rs:4572-4577). A crafted/corrupt snapshot plants
  a decay on a crewed `InBubble` vessel and restore accepts it.
- **Impact:** a corrupt save can schedule the deletion of any vessel — crewed, non-debris,
  bubble-resident — at an arbitrary future tick. Also undermines the `debug_assert` invariant the
  in-bubble removal sites rely on (lib.rs:4203-4209).
- **Suggestion:** restore-side: `DebrisDecay{v}` requires `vessels[v].debris_since.is_some()`
  (the event names debris by construction). Belt-and-suspenders: `fire_event` should guard
  `debris_since.is_some()` before `remove_vessel`, making a stale firing a no-op like
  `TransferComplete`.

### D4 — MEDIUM — `control_lease` coherence not checked on restore → wedged leases

- **`lib.rs:5103-5114`** — `apply_take_control` requires `Lease::Script(id)` to name a live script
  ("a dead/absent one would wedge the vessel under a controller that cannot act") and
  `Lease::Agent(client)` to name a client in `source_seqs`.
- **`lib.rs:2779-2784` + `restore_vessel_state`** — no equivalent check on the serialized lease.
- **Problem:** a crafted `Lease::Script(99)` with no script 99 (or `Lease::Agent(7)` with no
  `(External, 7)` seq entry) restores into the exact state the apply gate exists to prevent.
  `check_control_lease` (lib.rs:5061-5067) then rejects every non-Player command forever and the
  dead lease can never be transferred to — only `Player`'s unconditional preempt escapes it, so the
  wedge is bounded, but the invariant the vacate paths (`vacate_script_lease`, lib.rs:5145-5158)
  assume ("a Script lease names a live script") is silently broken.
- **Impact:** corrupt state accepted; vessel parked under an unholdable lease until the player
  takes it back.
- **Suggestion:** after the scripts arena validates (lib.rs:3065-3098), walk vessels once:
  `Lease::Script(id)` → `snapshot.scripts.contains_key(&id)` else `Fatal` (or demote to
  `Lease::Player` — the `vacate_script_lease` semantics). Same for `Agent` vs `source_seqs`.

### D5 — MEDIUM-LOW — `body.throttle` unbounded on restore

- **`lib.rs:9887-9898`** — restore checks `throttle.is_finite()` only.
- **`lib.rs:4812-4822`** — `SetThrottle` clamps `[0,1]` and zeroes when engines can't draw.
- **Problem:** a crafted `throttle = 5.0` (or `0.5` on a dry vessel) passes restore; the fuel-plan/
  executor path then runs out-of-range thrust — deterministic but dishonest, and outside every
  producer's reachable range.
- **Suggestion:** `throttle.is_finite() && (0.0..=1.0).contains(&throttle)`.

### D6 — LOW-MEDIUM — `ScriptState.vessel` can name an unissued id → binds the *next* spawn

- **`lib.rs:3065-3098`** — restore runs `scripts::script_state_is_valid` (persist floats only —
  `scripts.rs:596-598`) and the `wake_tick`↔event pairing; `s.vessel` is unchecked.
- **`scripts.rs:200-204`** — a script bound to a *removed* vessel is documented to keep ticking
  (telemetry reads `Missing`) — but *unissued* is a different state: when the next `SpawnVessel`
  allocates that id the script silently binds a hull it was never loaded on.
- **Impact:** the precise "binds the next spawn" hazard `check_refused_pairs` explicitly rejects
  for `joint_refused` (lib.rs:9622-9637) — accepted here for scripts.
- **Suggestion:** `s.vessel.0 >= snapshot.next_vessel_id` → `Fatal`. (Removed-but-issued stays
  legal per the doc.)

### D7 — LOW — smaller restore-honesty gaps (crafted-input class)

- **`contracts.rs:155-158`** — `facts.bodies_visited` entries never resolved against
  `orbits.bodies()`; a crafted `BodyId` can satisfy prereqs/predicates a legit save couldn't
  (the colony `body` IS resolved, lib.rs:9524).
- **`lib.rs:9028-9040`** — `TransferCargo::Crew` not checked to be seated at `t.from` (the apply
  keeps the member seated at source until landing); a crafted in-flight member seated elsewhere
  double-books on `land_crew`.
- **`restore_vessel_state`** — `vessel.aero_mask` bits never validated against the live graph; a
  crafted mask marks tables stale and `ensure_aero`→`build_aero_tables` Fatals mid-tick
  (lib.rs:3749-3750) instead of being refused at load.
- **`lib.rs:9923-9930`** — `SurfaceLatched` `pose.altitude_m` checked finite but not `>= 0`
  (the colony pose gate requires `>= 0`, lib.rs:9529-9533).
- **`lib.rs:2848-2889`** — `SoiTransition.body`, and `SoiTransition`/`SoiRescan`/`DebrisDecay`/
  `ManeuverNodeStart` naming non-rails vessels: no body/residence check. Mostly degrades
  harmlessly (per-event `Recoverable` → `degraded`, lib.rs:7483-7498) — D3 covers the one
  destructive case.

---

## Coverage gaps

### G1 — MEDIUM — the 11.6k-line test mod exercises no colony/contract/progression commands

`grep SimCommand::` over lib.rs:10894-22489 shows **zero** instances of: `FoundColony`,
`AbandonColony`, `EvacuateColony`, `DamageColonyModule`, `ScheduleColonyInspection`,
`AssignCrewToColony`, `TransferColonyCrew`, `TransferColonyResource`, `JoinColonyInventory`,
`LeaveColonyInventory`, `AcceptContract`, `ContractReward`, `ManufactureCraft`, `RunBenchTest`,
`ResearchTech`, `ResearchUpgradeLevel`, `SetLifeSupportEnabled`; `RecoverVessel` appears once — as a
lease-gate rejection (lib.rs:21710) — and `HireCrew` once. The mod is mechanics-and-machinery:
command ordering, rollback, warp equivalence, SOI/bubble physics, breakup, fuel, transfers,
science applies, reliability dormant publish, scripts, leases, and ~25 restore/snapshot/hash tests.

Mitigating fact: the *applies themselves* are tested in their own modules — colonies.rs has 43
World-level tests (incl. the real resupply arc `resupply_contract_completes_on_delivery`,
colonies.rs:4952), contracts.rs 16 (incl. `satellite_completes_on_orbit_achieved`, contracts.rs:1431),
economy.rs 11, tech.rs 7, manufacture.rs 3, life_support.rs 5. So "unreachable apply" isn't the risk.

**What is missing:** a single *player-shaped cross-subsystem arc*. Nobody tests the career loop a
player actually drives — e.g. `RunExperiment` → lab research → bank → `ResearchTech` → part-gate
unlock → `ManufactureCraft` → `ConstructionComplete` → fly the delivered hull; or found colony →
join inventory → resupply → contract completes → funds → `HireCrew` → `BoardCrew` → crew transfer.
Each leg is verified in a different file's harness with different fixtures; the seams between
progression systems (science→tech→economy→manufacture→launch) are unexercised. There is also no
`crates/csp-sim/tests/` integration dir — every test runs in-module against hand-built worlds, so
nothing exercises the API the way a host does (see G3).

### G2 — MEDIUM — the §7.5 per-commit gate (N=10 000, scenario set) doesn't exist as such

§7.5/§8.2 ask: "save → load → N ticks, hash-compared against uninterrupted N ticks, N = 10 000,
across the scenario set, every commit." The *pattern* is institutionalized — many tests tick
post-restore and hash-compare: lib.rs:12075-12079 (2×20 000 ticks), 14507-14520, 15089-15092,
15271-15275, 18958-18963, 19010-19015, 20279-20281, 20473-20478, 20533-20539;
colonies.rs:3282-3289 (30 000 ticks); economy.rs salary partition test; manufacture/tech/contracts/
life-support snapshot tests. But:

- every round-trip scenario is **single-subsystem** — none carries a rich world (live colony +
  active contract + in-flight build + bench run + transfer + script + debris simultaneously), so
  cross-subsystem interference at the boundary is untested;
- no dedicated N=10 000 scenario-set harness exists — `ci.yml:37-75` runs `cargo test --workspace`
  and the golden replays; no §7.5 gate entry;
- §8.1(a)'s property-test tier is absent workspace-wide (no `proptest` dep anywhere) — the
  `advance_to` equivalence exists only as the scenario test
  `warp_x1_vs_x1e5_equivalence_10_sim_days` (lib.rs:12143).

**Suggestion:** one table-driven "kitchen-sink" world (seeded colony + accepted contract + build +
bench + transfer + script-bound vessel + debris) with snapshot→restore→10 000-tick hash compare —
it would have caught D2's missing-channel class and gives the CI gate a real entry.

### G3 — LOW — no host-shaped restore test

Every test calls `World::restore` then hand-installs config ad hoc (lib.rs:20277
`r.set_experiments(...)`, colonies.rs:3285-3286 `set_sites`+`set_colony_env`). Nothing tests the
channel as a *list* — the csp-mcp `reconfigure` hole (D2) is exactly the bug a "restore → assert
required channels present" test catches. A `World::restore` + required-config parity test would
pin the seam.

---

## What the restore/hash/three-way gets right (audit trail)

- **Field-by-field audit:** every `state_hash` field is serialized and restored; every serialized
  field is either hashed or documented history (`command_log`, `notifications`, `script_log`,
  `part_ids`, aero caches, `joints` — the last save-refused at lib.rs:3219). No hashed-not-serialized
  or serialized-not-restored divergence found in-crate.
- **Coherence checks mirror the apply gates** in both directions: arena `x.id == key`, counters
  ahead of live ids (overflow-safe `next_back()` form, lib.rs:2768-2777), event↔record pairings
  for maneuver nodes / transfers / builds / research pairs / contract deadlines / colony
  inspections / bench runs / part failures / script wakes — plus `class` re-derivation
  (lib.rs:2845-2847) and event-grid checks (salary, life-support).
- **Re-derivation honored per §7.5:** `InBubble` bodies are re-welded from `(graph, parts)` and only
  kinematic/control/settle stamped back (lib.rs:9869-9904) — the serialized weld outputs are
  discarded and recomputed, never trusted. `body.aero` is `#[serde(skip)]` (weld.rs:160-161);
  `TerrainSurface.field` likewise (simple.rs:305); `physics.aero`/`thermal_rates` rebuilt lazily
  via `aero_tables_stale` (lib.rs:10136-10146).
- **No false-rejections found** — the checks were already hardened once: the bucketed-vs-exact
  wet-mass bound (lib.rs:9404-9414 + N1 regression test at 18977-19016) shows a real restore-Fatal
  on a legitimate snapshot was found and fixed.
- **`hash_memo` is airtight** (bubble.rs:127-183): `Clone` clears, `serde` skips, `PartialEq`
  ignores, `hash_craft` never walks it — a stale hit is structurally impossible.
- **`EventsShadow` Ops→Whole upgrade** (lib.rs:8890-8918) reconstructs the boundary heap under
  wholesale scrubs; `event_heap_ops_restore_rebuilds_boundary` (lib.rs:22445) pins both modes.
- **`apply_breakup` claim-ordering** (lib.rs:10355-10440): ancestors-first sort + deepest-first
  member assignment correctly prevents double-claimed subtrees; `nested_breakup_does_not_duplicate_
  subtrees` tests it; momentum stamps and per-node partition of tanks/crew/science/reliability/
  thermal are consistent with `fragment_graph_of`'s id preservation (structural.rs:349-377).
- **`transition_latched`** correctly keeps a latched body as a static collider while a *moving*
  neighbor rests on it (lib.rs:10176-10208) and returns only real transitions so contract triggers
  can't double-fire — the subtle case is documented and handled.
- **Rollback discipline** inside `apply_breakup`/removals: `shadow_economy`/`shadow_science`/
  `shadow_events_whole_split` captured before every book/heap write (lib.rs:10567, 10698-10710);
  `vessels`+`next_vessel_id` ride the `Whole` arena shadow taken at `step_bubble_range` entry
  (lib.rs:3674-3677).
