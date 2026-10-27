# Review report C11 — Manufacture + life support + save codec + events + script host

Session `session-full-20261026`, working tree (HEAD past 6cc2cb4). All findings verified by
reading the full data flow; finding 1 additionally verified with a throwaway integration test
(written, run, deleted — tree left clean).

Files: `crates/csp-sim/src/{manufacture,life_support,save,events,scripts}.rs` + the lib.rs glue
they contract with (`schedule_event`, `fire_due_events`, `check_serializable`, `restore`
coherence, `apply_set_script_enabled`, `fire_script_wake`) and the player save path in
`csp-app/src/lib.rs`.

---

### [HIGH] Finding 1: Disabling a script strands its pending Clamping `ScriptWake` — warp demotes at a dead script's tick, then every later save becomes permanently unloadable

**File**: `crates/csp-sim/src/scripts.rs:226-229` (pairing contract) + `crates/csp-sim/src/lib.rs:5429-5449`, `lib.rs:7899-7906`, `lib.rs:8127-8133`, `lib.rs:3332-3342`

**Problem**: `ScriptState::wake_tick` "pairs with exactly one pending `ScriptWake`" — enforced at
restore (`lib.rs:3332-3342`, hard `Fatal("snapshot: script wake / pending event mismatch")`) and
maintained by `UnloadScript` (scrubs the event, `lib.rs:8089-8096`) and by re-registration
(scrub-then-schedule, `lib.rs:9722-9746`). But the two DISABLE paths break the pairing from the
other side:

1. `apply_set_script_enabled` (`lib.rs:5429-5449`) flips `enabled=false` and vacates the lease —
   it neither scrubs the pending `ScriptWake` nor clears `wake_tick`.
2. The strike-disable inside `run_script_tick` (`lib.rs:7899-7906`) does the same.

Then when the wake tick arrives, `fire_script_wake` (`lib.rs:8127-8133`) sees
`live = enabled && wake_tick == Some(tick)` → false → **early return without clearing
`wake_tick`**, while `fire_due_events` has already popped the event.

**Evidence (empirical)**: throwaway integration test (AUTOMATION-panel flow: LoadScript on a
rails vessel → engine arms `AtTick(200)` → `SetScriptEnabled{false}` → step past 200 → save):
```
PROBE: ScriptWake@200 fired for a DISABLED script: true     <- Clamping event fired anyway
PROBE: wake_tick after firing: Some(200)                    <- stale pairing
PROBE: snapshot ok: true                                    <- the save file gets WRITTEN
PROBE: restore err: Fatal("snapshot: script wake / pending event mismatch")
```

**Impact**: (a) `ScriptWake` is `Clamping` (`events.rs:378-392`), so rail-warping demotes to x1
at the wake tick of a script the player explicitly stopped — violating §2.10's "a script with no
wake condition and no throttle does not block warp" in its clearest form; (b) any quicksave (F5)
taken after the wake tick encodes fine but can **never be loaded** — silent production of dead
save files from ordinary UI actions.

**Suggestion**: on both disable paths, do what `UnloadScript` does — scrub pending
`ScriptWake{script}` from the heap (shadowed) and set `wake_tick = None`. Belt-and-suspenders:
in `fire_script_wake`'s not-live early return, clear a `wake_tick == Some(tick)` match so a
stray pairing can never survive a firing.

---

### [MEDIUM] Finding 2: `fire_due_events` has no zombie-drop arm for `ConstructionComplete` — a Recoverable delivery failure strands a paid build order forever

**File**: `crates/csp-sim/src/manufacture.rs:262-277` + `crates/csp-sim/src/lib.rs:8296-8312`

**Problem**: on a non-Fatal event-firing failure, `fire_due_events` rolls the per-event state
back (restoring the `ActiveBuild` record that `fire_construction_complete` removed) while the
pop stands — the event is gone. It then zombie-drops the orphaned RECORD for
`ManeuverNodeStart`, `TransferComplete`, `ResearchDataExhausted/ProjectComplete`, and
`ContractDeadline` — but not for `ConstructionComplete`. `deliver_built_vessel` does have a
`Recoverable` escape: `reliability_residence_change` → `credit_qualification` returns
`Recoverable("tech: qualification credit is negative or non-finite")` (`tech.rs:146-151`) when a
flight-unit rate is degenerate.

**Impact**: on that path the build record is restored with no `ConstructionComplete` event — a
zombie order that never delivers, never refunds, permanently holds a reserved vessel slot
(`vessel_slots_in_use` counts `builds`, so the slot leak is permanent), and never notifies.
Latent (needs a degenerate reliability config), but it is exactly the corruption class the four
sibling arms exist to prevent; manufacture is now the launch hot path.

**Suggestion**: mirror the `TransferComplete` arm — on a failed `ConstructionComplete` firing,
drop the record with the event (and push a notification: "build lost — contact / refund" is a
product decision, but silent strand is not an option).

---

### [MEDIUM] Finding 3: No cancel/refund for build orders — a mis-order irreversibly consumes funds + full BOM

**File**: `crates/csp-sim/src/manufacture.rs:118-255` (no cancel path); no `CancelBuild` variant anywhere in `SimCommand` (`crates/csp-sim/src/lib.rs:1025-1041` is the only build verb)

**Problem**: `ManufactureCraft` spends the site inventory + funds atomically at apply, then the
only exits are delivery, berth-retry (unbounded), or the site-vanish drop (no refund,
`manufacture.rs:271-275`). There is no player verb to cancel an in-flight order.

**Impact**: playability on the new launch hot path — a mis-clicked launch (wrong site, wrong
craft, wrong moment) commits the entire cost with no recourse; the berth-retry case (pad
occupied) can hold the paid order indefinitely with the player unable to back out. §3.5 does not
mandate cancel, so this is a gap, not a violation.

**Suggestion**: `CancelBuild { build }` → refund the BOM to the site inventory + funds via the
economy ledger, scrub the paired event (the restore coherence check already requires exact
pairing, so the scrub is mandatory, not optional).

---

### [MEDIUM] Finding 4: Manufacture cost is caller-declared — the sim holds the part catalog and could verify, but only validates positivity

**File**: `crates/csp-sim/src/manufacture.rs:168-183` (BOM gate), `:177` (`craft_cost_micros` over caller-supplied `part_costs`)

**Problem**: `build_resources` and `craft.part_costs` ride the command; the apply checks only
`q > 0 && finite` and `costs ≥ 0 && finite`. The module doc frames this as necessity ("the sim
re-checks shape, not the arithmetic") — but `World` DOES hold the installed part catalog
(`self.parts`, used by `check_part_gate` at `tech.rs:271-292`), and `PartDef` carries
`build_resources` (`csp-data/src/part.rs:830`). The funds side is likewise derived from
caller-supplied `part_costs`, not the catalog.

**Impact**: via the §2.12 one-control-surface (MCP `sim.command`, `--serve` hosted clients), an
external client can build any craft with an empty BOM and zero `part_costs` — free launches that
still pass every gate. Single-player self-cheat is low-impact, but the trust boundary is the
documented external interface, and the doc's "cannot verify" framing is inaccurate.

**Suggestion**: when `self.parts` covers the craft's `part_ids`, recompute the BOM and cost
server-side and reject a mismatch (`Recoverable`); keep the caller-supplied values only for
catalog-less sandbox worlds.

---

### [MEDIUM] Finding 5: §7.7 data-drift warning does not "list what changed"

**File**: `crates/csp-sim/src/save.rs:135-137` (carries the hash correctly) + `crates/csp-app/src/lib.rs:1141-1152` (the warning), `crates/csp-data/src/datahash.rs` (no diff facility)

**Problem**: PLAN §7.7 (`PLAN.md:1702-1705`): "Loading with changed data warns **and lists what
changed** (part stats, biome maps, balance constants)". The loader compares the recorded
`data_set_hash` to the live one and prints "DATA DRIFT — this save was written against a
different data set"; `datahash.rs` exposes only the aggregate hash, nothing that can enumerate
the changed files.

**Impact**: the player learns a save mismatches but not whether it matters (a biome-map tweak vs
a balance.ron overhaul) — the exact support-nightmare the "list what changed" clause exists to
prevent. Warning WIRED, listing MISSING.

**Suggestion**: record per-file digests (or per-domain sub-hashes: parts/biomes/balance) beside
the aggregate in the header and diff them at load; render the changed-file list in the notice.

---

### [LOW] Finding 6: `craft_mass_kg` is dry mass — the "wet: tanks build full" rationale is false as coded

**File**: `crates/csp-sim/src/manufacture.rs:88-103`

**Problem**: the doc claims `WeldPart::mass_kg` is wet ("tanks build full, so filling them is
part of the job"), but the welder's mass sum is `Σ part.mass_kg` only (`csp-phys/src/weld.rs:628-655`);
tank-contents mass is dynamic (transfers `reweld_if_mass_changed`). The delivered vessel is
spawned full (`spawn_contents`), so build time is computed on the dry mass.

**Impact**: ETA under-counts for tank-heavy craft (a 2 t hull with 8 t of propellant builds in
~20% of the "honest" time); minor, but the doc-comment justifies the formula with a claim the
code contradicts. Suggestion: add `Σ capacity · mass_per_unit_kg` over tank specs, or fix the
comment.

---

### [LOW] Finding 7 (DRY): build-ETA mass arithmetic duplicated in the view's launch toast

**File**: `crates/csp-view/src/lib.rs:3005-3016` vs `crates/csp-sim/src/manufacture.rs:83-103`

**Problem**: the "BUILD ORDER PLACED — delivery in ..." toast re-implements `craft_mass_kg` +
`mass / rate` inline instead of reading the authoritative `complete_tick` the FACILITIES rows
use (`csp-view/src/lib.rs:4624`, `eta_s = complete_tick - now`). Two copies of the mass basis —
if finding 6 is fixed (or the rate config semantics change), the toast and the real delivery
drift apart silently.

**Suggestion**: after the order lands, render the toast from the build arena's `complete_tick`
(one frame later via the mirror), or expose a `World::build_eta(build)` helper both surfaces read.

---

### Follow-ups (cross-cluster, not counted)

- §3.5 "Deaths are a per-save setting (`off | respawn | permanent`)" is unimplemented — comments
  reference it (`economy.rs:249`, `colonies.rs:2205`) but both kill sites (`life_support.rs:413-420`,
  `colonies.rs` colony depletion) are permanently fatal with no setting. Crew arena is C08's file.
- `events.rs:413-443`: `StableDiscriminant for EventKind` hand-duplicates the declared `u16`
  discriminants (0..=24) with no consistency check; a future edit that moves one silently forks
  the hash schema. One assert-each-discriminant test would pin it (left out per no-add-tests rule).
- C21 (confirmed, not re-derived here): the berth-retry loop in `deliver_built_vessel`
  (`manufacture.rs:327-341`) re-queues every 64 ticks with no cap/no notification — Clamping
  toast spam + warp demotion while the pad stays occupied; also keeps the FACILITIES ETA pinned
  at "1 s" forever.

---

## PRD coverage table

| PRD rule | Status | Evidence |
|---|---|---|
| §3.5 manufacture: order consumes site inventory + funds + time, publishes `ConstructionComplete` | **WIRED** | `manufacture.rs:118-255` (atomic reservation), delivery `:322-422`; tests `manufacture_spends_at_order_delivers_at_event`, cap reservation via `vessel_slots_in_use` (`lib.rs:2632`) |
| §3.5 manufacture: cancel/refund | **MISSING** (not PRD-mandated) | Finding 3 |
| §3.5 life support: optional O2/EC/Food, per-save toggle, `LifeSupportDepleted` Clamping, interval-advanced | **WIRED** (recurring-event model, documented deviation that is determinism-sound) | `life_support.rs:271-422`; default OFF is opt-in and §6.4 puts life support on the sanctioned cut list ("Life support → post-v1") — shipping it opt-in exceeds the sanctioned minimum, no flag |
| §3.5 crew deaths setting | **PARTIAL/MISSING** (cross-cluster) | Follow-ups |
| §2.6.5 `SUBSYSTEM_ORDER` verbatim + reorder = hash bump | **WIRED** | `events.rs:97-110`, matches PLAN order exactly |
| §2.6.5 event classes incl. tracked opt-in; Clamping/Silent member tables | **WIRED** (extensions `TransferWindow`, `PowerLost/Restored` etc. chosen consistently; `PartFailure` Silent per table) | `events.rs:449-501` |
| §2.6.5 budget: 2000/frame between batches, 1024-chunk atomic batch, deferred tails, postponed drain, `partial` | **WIRED** | `lib.rs:224/252`, `step_frame` loop `lib.rs:8620-8735` + `fire_due_events:8189` — constants and loop shape match the spec verbatim |
| §7.2 container (magic, JSON header, DEFLATE-CBOR, byte-stable, `body_len` framing, 256 MiB bomb bound) | **WIRED** | `save.rs:95-111, 353-425`; atomic player write (temp+rename) at `csp-app/src/lib.rs:1133-1138` |
| §7.2 serialized set (heap + `next_event_seq`, scripts + persist + strike counters + `script_op_budget`, builds + `next_build_id`, notification log) | **WIRED** | `lib.rs:1410-1415, 2385-2389, 3446-3481`; hashed at `lib.rs:8870-8879` |
| §7.4 refusal rules 1/2/4 (+ pending-commands) with reason in UI | **WIRED** | `check_serializable` `lib.rs:3491-3549`; surfaced "SAVE REFUSED — {reason}" in `do_save` |
| §7.4 rule 3 (Stage-2 weld pending) | **N/A** | no staged-weld subsystem exists in the tree — vacuous, not a violation |
| §7.7 version gate: chain 2..=3, refuse-with-message, never half-load; stamped-2 byte-exact proof | **WIRED** | `save.rs:495-528`; sound — no `skip_serializing_if` anywhere in the serialized tree (verified), so a pre-`engine` body can never re-encode byte-identically |
| §7.7 data drift "warns AND LISTS what changed" | **PARTIAL** | Finding 5 |
| §7.5 load integrity (recorded state hash vs restored) | **WIRED** | header `state_hash` + DIVERGED notice `csp-app/src/lib.rs:1173-1179` |
| §2.10 budget/strikes (5 consecutive, visible reason)/poll (10 heavy ticks, named demotion)/persist/one-tick-latency | **WIRED** | `scripts.rs:94-108`, `lib.rs:7812-7920`, `rails_warp_refusal` consumes `poll_demoted`; `TickIo::issue` derives the source stamp in release (no guest impersonation) |
| §2.10 wakes: closed-form conditions or refusal at registration; Clamping; survives rails; double-tick rule | **WIRED** with the Finding-1 hole | `scripts.rs:661-813` (apsis/altitude math verified incl. hyperbolic branches), `lib.rs:8107-8147` |

## Top 3 issues

1. **[HIGH] Finding 1** — disable a script → stale `wake_tick` pairing → Clamping wake of a dead
   script demotes warp, and every quicksave after the wake tick is written but can never load
   (empirically confirmed end-to-end).
2. **[MEDIUM] Finding 2** — `ConstructionComplete` missing from `fire_due_events`' zombie-drop
   discipline: a Recoverable delivery failure strands a paid build order + a vessel slot forever.
3. **[MEDIUM] Finding 3** — no cancel/refund verb for build orders on the new launch hot path
   (with Finding 4's unverified caller-declared BOM/cost as the adjacent trust-boundary gap).
