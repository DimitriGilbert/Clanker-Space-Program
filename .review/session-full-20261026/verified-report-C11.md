# Verified report C11 — session-full-20261026

Verification agent pass over `review-report-C11.md`. Every finding re-derived from source at the
cited lines plus surrounding context; Finding 1 additionally re-proven empirically with a fresh
/tmp-only harness (built, run, deleted — repo tree untouched by this verification).

Score: **6 confirmed, 1 dismissed** (Finding 2 dismissed as stated — trigger unreachable; its
structural observation survives as hardening).

---

### Finding 1: Disabling a script strands its pending Clamping `ScriptWake` — warp demotes at a dead script's tick, then every later save becomes permanently unloadable — CONFIRMED

**Original**: [HIGH] both disable paths break the `wake_tick` ↔ pending-event pairing; the stale
wake fires as Clamping for a disabled script; `fire_script_wake` early-returns without clearing
`wake_tick`; snapshot writes but restore Fatals.

**Verification** — every leg re-derived in code, then reproduced empirically:

1. Pairing contract: `ScriptState::wake_tick` doc (scripts.rs:225-229) and the restore check
   (lib.rs:3332-3342): `(Some(t), None)` → `Fatal("snapshot: script wake / pending event mismatch")`.
   `wake_events` is built from the snapshot's event heap (lib.rs:3016, 3265).
2. Disable path A — `apply_set_script_enabled` (lib.rs:5429-5449): sets `enabled=false`, clears
   `disabled_reason` on re-enable, vacates the lease via `vacate_script_lease` (lib.rs:5671-5685 —
   flips control leases only, touches neither the heap nor `wake_tick`). No scrub, no clear.
3. Disable path B — strike-disable (lib.rs:7878-7891): `script.enabled = false` + reason; same
   absence (vacate runs at 7926-7928, heap untouched).
4. Firing: `fire_script_wake` (lib.rs:8120-8147): `live = enabled && wake_tick == Some(tick)`
   (8127-8130); `!live` → `return Ok(())` at 8131-8133 **without** clearing `wake_tick`
   (`s.wake_tick = None` at 8141 only runs in the live branch). `fire_due_events` pops the event
   at 8206 before firing (8224) and `Ok(())` keeps the pop (8230-8233) — the pairing is now
   `Some(200)` with no event.
5. Save gate gap: `check_serializable` (lib.rs:3497-3549) checks only save-blocked, pending
   commands, joints, physics contacts — no wake pairing. `snapshot()` (3556-3600) clones the
   broken state; the app writes the file to disk with temp+rename and shows "SAVED — {slot}"
   (csp-app/src/lib.rs:1133-1143). Only `do_load` → `load_game` → `World::restore` hits the
   Fatal, surfaced as "load failed: snapshot: script wake / pending event mismatch"
   (csp-app/src/lib.rs:1190-1207). Dead-file production confirmed end-to-end.
6. Warp impact: `fired.push(event.clone())` at lib.rs:8212 precedes the firing, so the stale wake
   lands in `AdvanceReport::fired_events` with `class == Clamping` (events.rs:489-492);
   csp-app demotes on any fired Clamping event (csp-app/src/lib.rs:1257-1260). The module doc's
   own rule "A script with no pending wake and no throttle blocks nothing" (scripts.rs:71) is
   violated in its clearest form.
7. Contrast (the invariant IS maintained elsewhere, so this is an oversight, not a policy):
   `apply_unload_script` scrubs the heap (lib.rs:8089-8096); `request_wake` scrubs-then-schedules
   (lib.rs:9728-9746); and — the closest precedent — `apply_set_life_support(false)` scrubs every
   pending event of its subsystem on disable (life_support.rs:225-255). `SetScriptEnabled` is the
   one disable that doesn't.

**Empirical re-probe** (fresh /tmp harness: `World::with_system` two-body → `SpawnVessel` (rails,
MCP/test verb) → `LoadScript` (bootstrap wake, lib.rs:8019-8038) → stub engine arms
`request_wake(AtTick(200))` at tick 2 → `SetScriptEnabled{false}` → step past 200 → snapshot →
restore):
```
script tick 2: armed AtTick -> 200
PROBE: wake_tick after arm: Some(200)
PROBE: after disable: enabled=false, wake_tick=Some(200)
PROBE: ScriptWake@200 fired for a DISABLED script: true
PROBE: wake_tick after firing: Some(200)
PROBE: snapshot ok: true
PROBE: restore err: Fatal("snapshot: script wake / pending event mismatch")
```
Matches the reviewer's probe output exactly. The suggested fix (scrub + clear on both disable
paths, plus clear-on-not-live in `fire_script_wake`) matches the codebase's own
`apply_set_life_support` / `apply_unload_script` conventions.

---

### Finding 2: `fire_due_events` has no zombie-drop arm for `ConstructionComplete` — a Recoverable delivery failure strands a paid build order forever — DISMISSED (as stated)

**Original**: [MEDIUM] the four sibling zombie-drop arms (ManeuverNodeStart / TransferComplete /
ResearchDataExhausted+ProjectComplete / ContractDeadline) have no `ConstructionComplete` member;
`deliver_built_vessel` → `reliability_residence_change` → `credit_qualification` is claimed as a
reachable `Recoverable` escape when a flight-unit rate is degenerate.

**Verification** — the structural gap is real, but the claimed trigger is unreachable, so the
"strands a paid order" impact cannot occur in the current code:

- The missing arm is real: `fire_due_events`' failed-firing cleanup (lib.rs:8248-8277) covers the
  four sibling kinds; `ConstructionComplete` appears only in the vessel-name match (8291) as
  `None`. The rollback mechanics are also real: `fire_construction_complete` shadows the build
  arena before `builds.remove` (manufacture.rs:267-268), so a `Recoverable` would restore the
  record while the pop stands.
- But the error surface of `fire_construction_complete` contains **no reachable Recoverable**:
  - `deliver_built_vessel` (manufacture.rs:322-422) has three `?` sites:
    `allocate_records` (Fatal-only: counter overflow, reliability.rs:490-494), the berth-retry
    `schedule_event` (Fatal-only, and `retry_tick = clock + TICK_HZ` is always strictly ahead,
    lib.rs:5125-5132), and `reliability_residence_change`.
  - `reliability_residence_change`'s only error source is
    `credit_qualification(burn_s * rate, …)` (reliability.rs:894-904), and `burns` is populated
    only from records with `rec.burning == true` (reliability.rs:832-836). On the delivery path
    the vessel was inserted a few lines earlier with records from `allocate_records` →
    `fresh_record`, which hard-codes `burning: false` (reliability.rs:466). `burns` is therefore
    always empty on delivery; `credit_qualification` is never called — a negative
    `flight_units_per_s_vacuum` config changes nothing.
  - `deliver_built_vessel` has exactly one caller (`fire_construction_complete`, manufacture.rs:276),
    and `fire_event` dispatches it bare (lib.rs:8435-8437) — no wrapper adds a Recoverable.
- This matches the function's own contract: "TOTAL like every event firing" (manufacture.rs:258-260).
  The four sibling arms exist because those firings genuinely have Recoverable paths.

**Disposition**: the impact scenario ("a Recoverable delivery failure strands a paid build order",
"latent — needs a degenerate reliability config") is wrong — no config makes it reachable today.
What survives is a defensive-hardening observation: a future Recoverable added to the delivery
path would silently strand the order (and, per the restore pairing checks, poison saves the same
way Finding 1 does). That is a [nit]-grade forward-compat note, not a live MEDIUM.

---

### Finding 3: No cancel/refund for build orders — a mis-order irreversibly consumes funds + full BOM — CONFIRMED

**Original**: [MEDIUM] `ManufactureCraft` spends site inventory + funds atomically at apply; the
only exits are delivery, unbounded berth-retry, or the site-vanish drop with no refund; no player
cancel verb exists.

**Verification**:
- Atomic spend at apply confirmed (manufacture.rs:192-249); site-vanish drop returns `Ok(())`
  with the record already removed and no refund (manufacture.rs:268-275); berth-retry re-queues
  every `TICK_HZ` with no cap or notification (manufacture.rs:327-341).
- No cancel verb: no `CancelBuild`/build-cancel anywhere in the workspace (grep over all crates);
  `SimCommand` has `ManufactureCraft` as the only build verb; the UI has no build-cancel path
  (csp-view's only "cancel"s are VAB editor operations, and map.rs:1261 explicitly notes "the sim
  has no unschedule verb").
- Correctly framed by the reviewer as a playability gap, not a §3.5 violation.

---

### Finding 4: Manufacture cost is caller-declared — the sim holds the part catalog and could verify, but only validates positivity — CONFIRMED

**Original**: [MEDIUM] `build_resources`/`part_costs` ride the command; apply checks only shape +
positivity; `World.parts` + `PartDef` carry authoritative `build_resources`/`cost_funds`; external
clients can build with an empty BOM and zero costs.

**Verification**:
- BOM gate is positivity/finite only (manufacture.rs:169-176); `craft_cost_micros` sums the
  caller-supplied `craft.part_costs` (economy.rs:520-530, zero entries count as 0).
- The catalog is present and authoritative: `World.parts` backs `check_part_gate`
  (tech.rs:271-291), and `PartDef` carries `cost_funds` and `build_resources`
  (csp-data/src/part.rs:822-830). The resolver even computes both per-part
  (`resolve_stats`, csp-phys/src/resolve.rs:1122-1135: `cost`, `build_resources`), so the
  authoritative numbers demonstrably exist at the seam.
- Bypass mechanics confirmed: `check_part_gate` returns `Ok` when `craft.part_ids` is empty
  (tech.rs:272-274), `part_costs` of all zeros passes the len/finite/≥0 gate
  (manufacture.rs:148-152), and an empty `build_resources` spends nothing — a weldable craft of
  real `WeldPart`s orders for free.
- Trust boundary confirmed: PLAN §2.12 "one control surface, four clients"
  (PLAN.md:788); `csp-server` accepts arbitrary `SimCommand`s from external clients
  (`Host::submit`, csp-server/src/backend.rs:162; ingress.rs:140). The MCP/test-only
  `SpawnVessel` comment (lib.rs:553) confirms external clients are a designed caller class.

---

### Finding 5: §7.7 data-drift warning does not "list what changed" — CONFIRMED

**Original**: [MEDIUM] PLAN §7.7 requires the loader to "warn and list what changed"; only an
aggregate-hash comparison + generic notice ships; `datahash.rs` cannot enumerate changes.

**Verification**:
- PLAN.md §7.7 (around lines 1702-1705): "Loading with changed data **warns and lists what
  changed** (part stats, biome maps, balance constants)." Verbatim as quoted.
- save.rs:134-137 carries `data_set_hash` correctly (v2+; `None` = v1 migrated).
- csp-app/src/lib.rs:1176-1183: hash mismatch → "DATA DRIFT — this save was written against a
  different data set" — no listing of any kind.
- csp-data/src/datahash.rs exports exactly one function, `data_set_hash` (aggregate). No diff
  facility exists. Warning WIRED, listing MISSING — PARTIAL row accurate.

---

### Finding 6: `craft_mass_kg` is dry mass — the "wet: tanks build full" rationale is false as coded — CONFIRMED

**Original**: [LOW] the doc comment claims `WeldPart::mass_kg` is wet, but the weld mass is
`Σ part.mass_kg` only and the delivered vessel spawns with full tanks — build time is computed on
dry mass.

**Verification** (traced the whole chain):
- `PartDef.mass_dry_kg` → `resolve_stats`: `mass_dry = def.mass_dry_kg * …`
  (csp-phys/src/resolve.rs:1122) → `resolve_weld_input(def, …, mass_kg, …)` (2343) →
  `WeldPart.mass_kg` is DRY by construction.
- Weld totals `Σ s.part.mass_kg` only (csp-phys/src/weld.rs assemble, ~627-633); propellant lives
  in the separate dynamic `tank_contents` map.
- `craft_mass_kg` (manufacture.rs:90-103) sums exactly that dry `mass_kg`, while
  `spawn_contents` fills every tank to `capacity` at delivery (transfers.rs:388-408) — the
  delivered mass exceeds the mass the ETA was computed on. The manufacture.rs:88-89 comment
  ("wet: tanks build full, so filling them is part of the job") states the opposite of the code.

---

### Finding 7 (DRY): build-ETA mass arithmetic duplicated in the view's launch toast — CONFIRMED

**Original**: [LOW] the "BUILD ORDER PLACED" toast re-implements `craft_mass_kg` + `mass / rate`
inline instead of reading the build arena's `complete_tick` the FACILITIES rows use.

**Verification**: csp-view/src/lib.rs:3004-3026 re-sums `craft.parts[…].mass_kg` over graph nodes
(character-for-character the same arithmetic as manufacture.rs:90-103) and divides by the rate for
the toast; the FACILITIES build rows instead read `b.complete_tick.saturating_sub(now)`
(csp-view/src/lib.rs:4624). Two mass bases, one authoritative deadline — the drift risk is as
described.

---

## PRD coverage table — spot-verified

| PRD row | Verdict |
|---|---|
| §2.6.5 constants 2000/1024 | CONFIRMED — `DEFAULT_MAX_EVENTS_PER_FRAME = 2_000` (lib.rs:224), `MAX_EVENTS_PER_BATCH = 1_024` (lib.rs:252); `step_frame` checks the budget between iterations only (lib.rs:8722-8727), batches are atomic inside `fire_due_events` with the ≤1024 ceiling, deferred tails flush at the clock (lib.rs:8636-8642), `partial` at lib.rs:8736. |
| §7.2 container | CONFIRMED — magic/JSON header/DEFLATE-CBOR/`body_len` framing/256 MiB inflate bound (save.rs:100-111), DEFLATE level pinned for byte-stability (save.rs:103-105), atomic player write temp+rename (csp-app/src/lib.rs:1133-1143). |
| §7.2 serialized set | CONFIRMED — snapshot carries builds + `next_build_id`, scripts + persist + `script_op_budget`, notification log (lib.rs:3587-3599); the §7.1 hash walk covers all of them (lib.rs:8868-8880). |
| §7.7 v2 byte-exact gate | CONFIRMED — save.rs:495-528: chain 2..=3, stamped-2 decodes under the v3 schema with a decode→re-encode byte-identity proof; pre-`engine` bodies fail the proof → `UnresolvableSchema`. Corroborated: no `skip_serializing_if` anywhere in the save tree (the only hits in the repo are csp-server wire DTOs, outside the serialized set). |
| §7.7 data drift PARTIAL | CONFIRMED (Finding 5). |
| §7.4 refusal rules | CONFIRMED — `check_serializable` rules 1/2/4 + pending-commands (lib.rs:3497-3549); surfaced "SAVE REFUSED — {reason}" (csp-app/src/lib.rs:1115). Rule 3 N/A holds: no staged-weld subsystem exists (grep over csp-sim: zero hits). |
| §7.5 load integrity | CONFIRMED — recorded `state_hash` vs restored, DIVERGED notice (csp-app/src/lib.rs:1197-1203). |
| §2.6.5 SUBSYSTEM_ORDER | CONFIRMED — events.rs:97-110 matches PLAN.md:534-535 element-for-element. |
| §2.6.5 classes | CONFIRMED — `class_of` (events.rs:449-501): tracked opt-in for SoiTransition, `PartFailure`/`BenchTestComplete` Silent, `ScriptWake`/`LifeSupportDepleted` Clamping. |
| §3.5 manufacture WIRED | CONFIRMED — atomic reservation (manufacture.rs:118-255), slot reservation via `vessel_slots_in_use = vessels + builds` (lib.rs:2632-2634), delivery at event, tests present (manufacture.rs:547-681). |
| §3.5 life support WIRED | CONFIRMED — default OFF opt-in (lib.rs:1087), `SetLifeSupportEnabled` scrubs pending events on disable (life_support.rs:225-255), depletion firing re-checks coverage before killing (life_support.rs:374-421). |
| §3.5 crew-deaths PARTIAL/MISSING | CONSISTENT — the kill site sets `CrewStatus::Lost` unconditionally (life_support.rs:413-420), no `off/respawn/permanent` setting exists; cross-cluster as the report says. |
| §2.10 budget/strikes/poll | CONFIRMED — `STRIKE_DISABLE_LIMIT = 5`, `POLL_DEMOTE_TICKS = 10` (scripts.rs:96-101); strike-disable + poll flag in `run_script_tick` (lib.rs:7870-7919). |
| §2.10 wakes WIRED "with the Finding-1 hole" | CONFIRMED — registration/firing verified (lib.rs:8019-8038, 8107-8147); the hole is Finding 1. |

---

## Summary for the coordinator

- **Finding 1 [HIGH]: CONFIRMED** — reproduced empirically from a fresh harness, independent of
  the reviewer's probe; every code leg checks out, and the app-level "file written, load
  permanently fails" path is verified through `do_save`/`do_load`. Fix precedent already exists
  in-repo (`apply_set_life_support`, `apply_unload_script`).
- **Finding 2 [MEDIUM]: DISMISSED as stated** — the Recoverable escape is unreachable (fresh
  delivery records are never `burning`, so `credit_qualification` is never called on this path;
  all other error sites are Fatal-only, matching the function's documented TOTAL contract).
  Downgrade to a hardening nit: add the zombie-drop arm only if/when a Recoverable is ever added
  to the delivery path.
- Findings 3-7: CONFIRMED as written (3 and 4 are gaps at the documented external boundary, 5 is
  a spec-shortfall vs PLAN §7.7, 6-7 verified down the full call chain).
- Tree untouched by this verification (only `.review/` session artifacts; the two untracked
  `docs/*.md` files predate this pass and were not created here). /tmp harness deleted.
