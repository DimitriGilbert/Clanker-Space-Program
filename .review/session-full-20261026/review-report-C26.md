# Review report C26 — Scripting + AI (csp-script, api_freeze, csp-ai)

Session session-full-20261026 · reviewed working tree (HEAD past 6cc2cb4 + uncommitted W2.6/UX).
Scope: `crates/csp-script/src/lib.rs` (1,773), `crates/csp-script/tests/api_freeze.rs` (182),
`crates/csp-ai/src/lib.rs` (222), against PLAN §2.10 (scripting), §2.11 (AI), §6.4 (cut list).
Sim-side seam (`csp-sim/src/scripts.rs`, `lib.rs` control pass/applies/wake resolution) read for
context; cross-cluster findings referenced, not re-derived.

Verified green before findings: `cargo test -p csp-script -p csp-ai` (13 + 3 pass),
`cargo test -p csp-bench --lib scenarios::tests::autopilot_ascent_reaches_orbit` (passes, 3.1 s —
the real AscentAutopilot flies the fixture craft from the pad to a closed orbit above 70 km).

---

### [SEVERITY: CRITICAL] Finding 1: A one-line player script panics the host — any property assignment into `ctx` hits rhai's `unreachable!()` on the shared persist cell

**File**: `crates/csp-script/src/lib.rs:1183` (plant), `:1200-1202` (panic site), `:1206-1209` (the false doc claim)

**Problem**: `run_tick` builds `ctx.persist` as a *shared* Rhai cell
(`Dynamic::from_map(...).into_shared()`). Rhai 1.27's data-size checker (`eval/data_check.rs:86`,
`calc_map_sizes`) treats a `Union::Shared` **inside a map** as an internal invariant violation and
panics with `unreachable!("shared values discovered within data")` — and the size check runs on
**every property assignment into the containing map**. There is no `catch_unwind` anywhere in the
workspace (grep verified), so the panic unwinds through `run_script_tick` → `control_tick` →
`step_frame` → the game thread: crash to desktop (and a poisoned host thread under `--serve`).

**Evidence** (empirically reproduced with a scratch test against the real `RhaiHost`, then removed):

| guest line | result |
|---|---|
| `ctx.persist = #{ n: 1 };` | **PANIC** |
| `ctx.persist["n"] = 1; ctx.persist = #{};` (the natural "reset my state" idiom) | **PANIC** |
| `ctx.extra = 1;` — merely *adding* a property to `ctx`, persist untouched | **PANIC** |
| `let c = ctx; c.persist = #{ n: 1 };` | **PANIC** |
| `ctx.persist["n"] = 1;` (indexed write, the documented idiom) | ok |
| `ctx.persist.remove("n");` / `let a = [ctx.persist];` | ok |

`prepare` accepts all of these (no syntax fault), so a script pasted into the new AUTOMATION
editor (or delivered via MCP `LoadScript`) is stored, ticks once, and kills the process. The
lib.rs:1206-1209 doc claim — "a script reassigned `ctx.persist` wholesale — also legal, read
whatever it left" — is doubly wrong: the reassignment is neither read back (the write-back reads
the original shared cell, so the `try_cast::<Map>() → None` branch is dead) nor graceful; it
aborts the process. The inline test suite covers only the indexed-write idiom, which is why this
survived.

**Impact**: any player can crash the game with `ctx.foo = 1;`; the same script in a golden replay
aborts the replay. This defeats §2.10's sandbox story (budget/strikes are supposed to make guest
faults non-fatal) — the memory ceilings added as the C16-F2 hardening are precisely the code path
that panics.

**Suggestion**: (a) wrap the `engine.call_fn` in `std::panic::catch_unwind(AssertUnwindSafe(..))`
and classify a caught panic as `ScriptFault::Runtime` (strike path, discard the tick's effects —
the existing atomic-drain rule already requires that); (b) stop planting shared values inside
guest-visible maps for the assignment path — e.g. register an explicit `persist` accessor pair,
or vendor a one-line fix to rhai's `calc_map_sizes` (treat `Shared` as one entry instead of
`unreachable!`); (c) fix the lib.rs:1206-1209 comment to match actual semantics; (d) add the
panicking variants above as regression tests.

---

### [SEVERITY: HIGH] Finding 2: The ascent autopilot never terminates itself — its own success permanently blocks auto-rails, so "fly to orbit" ends with no conic

**File**: `crates/csp-ai/src/lib.rs:161-165` (phase 4 = "drift until disengaged"), cross-ref `csp-sim/src/lib.rs:4802`

**Problem**: on completion (phase 4) the driver keeps issuing `set_throttle(0.0)` every tick and
keeps its `AutopilotState` record alive until a manual `DisengageAutopilot`. The auto bubble→rails
detector explicitly skips any vessel with an enabled autopilot record (`lib.rs:4802`), so the
craft that just reached orbit stays `InBubble` forever: no conic, no orbit line/map prediction, no
SOI, physics-warp ladder only. The bench acceptance test even codifies it
(`scenarios.rs:986 assert!(w.autopilots()[&FLIGHT_VESSEL].enabled)`), so the no-conbit outcome is
the *expected* terminal state of the player-facing "engage ascent" button.

**Impact**: the §2.11 ascent path — now a first-class UI verb — delivers a terminal state that
cannot use any orbit feature; the player must know to press "disengage" to convert. This is the
csp-ai-side root cause of the C03/C19 verified finding (completed ascent AP suppresses
auto-rails); do not double-count the sim-side flag, but the fix belongs here.

**Suggestion**: in phase 4's first tick (or at the 3→4 transition) call
`api.disengage_autopilot()` — the `ControlApi` verb already exists and the command path is
identical to the player's. Update the acceptance test to assert the record is *gone* and the
vessel railed. (If hands-off-until-told is desired, at minimum auto-disengage once the vessel
meets the auto-rails coasting predicates.)

---

### [SEVERITY: HIGH] Finding 3 (PRD-coverage): §2.11 design assistant + MissionProfile emission missing with no §6.4 sanction

**File**: `crates/csp-ai/src/lib.rs` (entire crate — ascent only); PLAN.md:775-778 vs PLAN.md:1543-1559

**Problem**: §2.11 v1-classical list: *Ascent* (WIRED), *Transfer* (WIRED — `csp-sim/src/transfer.rs`
Lambert/porkchop + map planning surface, M5 row), *Design assistant* — "constraint solver over
part stats — given payload mass, target Δv, min TWR, body, propose staging. Greedy + local search
over the unlocked part set" — and *Mission profile* — "the design assistant emits the
`MissionProfile` that §3.4's confidence number requires". Neither exists anywhere in the tree
(grep: no design-assistant/staging-proposal code; `csp-ai` is 222 lines, ascent only). §6.4's cut
list sanctions exactly five cuts (advanced render, life support, Duva, docking-with-proximity,
web polish) and "minimum shippable v1 = M0…M9a, M9b, M10" — the design assistant appears in no
milestone row and in no cut item. The PRD is internally inconsistent (§2.11 says v1-classical;
§6.0's table never assigns it an EU), but per the owner's mandate an unsanctioned v1 shortfall is
flagged, not excused.

**Impact**: §2.11 shortfall beyond the known scope gap the exploration map notes; §3.4's
confidence number has no producer chain for player-designed craft (MissionProfile exists only as
a readout stub lineage from M3/M9b).

**Suggestion**: either build the greedy+local-search staging proposer over the unlocked part set
(small: it composes `csp-phys` readouts) or record an explicit §6.4 cut-line entry + PLAN edit so
the PRD stops promising it.

---

### [SEVERITY: MEDIUM] Finding 4 (PRD-coverage): §2.10 ControlApi verb list short — no action groups; `mission.ascend(target_apo, incl)` not offered as specced

**File**: `crates/csp-script/src/lib.rs:89-136` (trait), `:317-343` (`CONTROL_API_SURFACE`); PLAN.md:723, 764-766

**Problem**: PLAN §2.10 line 723 enumerates the trait surface: "telemetry read, throttle/steer/SAS,
staging, maneuver nodes, **action groups**, resource transfer, part actions, alarms, log/print."
Grep across `crates/` finds no `ActionGroup`/action-group concept at all — the frozen surface has
per-part `deploy(node)`/`jettison(node)` but no grouping/toggle verb, and §6.4 does not sanction
its absence. Likewise §2.10 line 764-766 specces the script-callable service as
`mission.ascend(target_apo, incl)`; built is `engage_autopilot(apo_m, pe_m)` (different name, no
inclination parameter). The freeze at M9c (correctly pinned by `tests/api_freeze.rs`) means
adding any of these is now a `csp.mcp.tools` major-version event — the shortfall gets more
expensive the longer it sits.

**Impact**: DEGRADED §2.10 coverage (individual part actions cover the practical v1 subset; no
parts currently need grouped actions — legs/chutes deploy per node), plus a service-signature
deviation from the normative text.

**Suggestion**: add an explicit §6.4 note or the verbs. If action groups stay out of v1, one PLAN
line ("action groups → post-v1; per-part verbs cover v1 parts") closes the gap honestly.

---

### [SEVERITY: MEDIUM] Finding 5 (data integrity): the `ctx` schema — the actual script-facing API — is not pinned by any freeze test

**File**: `crates/csp-script/src/lib.rs:738-822` (`telemetry_to_map`), `crates/csp-script/tests/api_freeze.rs:15-75`

**Problem**: api_freeze pins the trait method set and the guest function names/arities (verified
current — no drift behind the new UI verbs; the new `LoadScript`/`UnloadScript`/
`SetScriptEnabled`/`EngageAutopilot`/`DisengageAutopilot` verbs ride `SimCommand` directly and
correctly bypass the frozen trait). But nothing pins the **`ctx` map contract**: the top-level
keys (`tick`, `script`, `vessel`, `persist`) and every `ctx.vessel` field name
(`altitude`/`apoapsis`/`time_to_apoapsis`/`residence`/`resources`/`parts`/`rot{w,x,y,z}`/…)
live only in `telemetry_to_map` and in a hand-written doc comment (lib.rs:36-60). A rename or
removal silently breaks every saved player script at runtime (missing-key reads misbehave /
fault into strikes) with zero test signal — exactly the drift class the freeze exists to make
"a test failure, not a diff reviewer's judgment call". Also: absent numeric telemetry is encoded
as `NaN` floats (lib.rs:740) — a real trap for the documented primitive-persist rule
(`ctx.persist["apo"] = ctx.vessel.apoapsis` faults when apoapsis is unknown) and undocumented.

**Impact**: player-script portability across versions is unprotected; the NaN-for-missing
convention is undocumented at the point of use.

**Suggestion**: add a freeze test asserting the exact key sets of `ctx` and the vessel telemetry
map (construct a `VesselTelemetry`, run the map builder, compare key lists); document the
NaN-for-missing convention in the `ctx` doc block (the stock script already teaches the
`apo != apo` idiom).

---

### [SEVERITY: LOW] Finding 6: ascent AP's phase-2 recovery branch is dead code; open-conic coast strands the autopilot

**File**: `crates/csp-ai/src/lib.rs:135-146`

**Problem**: the "decaying apoapsis (never made it) drops back to the burn phase" guard requires
`apoapsis_altitude_m.is_some() && apoapsis < target - 5 km && time_to_apoapsis_s.is_none()`. Per
`csp-sim` `osculating` (lib.rs:9347-9375), apo Some + time-to-apo None co-occurs only at the
exact apsis tick or on degenerate elements: a genuine failed ascent (closed conic, apo below
target) always has `time_to_apoapsis = Some`, so the guard never fires and recovery happens only
accidentally via phase 3's prograde-over-burn spiraling past `apo > target + 5 km`. Symmetrically,
if the conic opens (apo → None, e.g. a hot engagement on an already-escaping vessel), phase 2's
`lead = unwrap_or(INFINITY)` never elapses and the safety branch is false (`is_some_and` on
None) — the AP dead-sticks at throttle 0 forever with its record alive (and per Finding 2, also
blocking rails). No unit tests exist for the phase machine at all (csp-ai has zero tests; only
the bench end-to-end covers the happy path).

**Impact**: guidance edge cases silently degrade; dead safety code misleads future maintenance.

**Suggestion**: drop the `time_to_apoapsis_s.is_none()` conjunct (fall back to phase 1 whenever
`apo < target - APO_OVERSHOOT_M` while coasting), handle `apo == None` (open conic → circularize
prograde or terminate), and add csp-ai unit tests driving `fly()` with stubbed telemetry across
the phase boundaries.

---

### [SEVERITY: LOW] Finding 7: `IoControlApi::log` misattributes autopilot log lines to a bogus script id

**File**: `crates/csp-script/src/lib.rs:268-271`

**Problem**: `let id = self.script.unwrap_or(ScriptId(self.source_id as u64))` — for an
autopilot caller (`script: None`), `source_id` is the *vessel* id channel key, so a future driver
that logs attributes its lines to `script <vessel-id>` in `script_log`, which no console row
renders. Harmless today only because `AscentAutopilot` never calls `log`.

**Suggestion**: return `Err`/no-op for script-less callers (mirroring `request_wake`'s honest
refusal) or stamp a dedicated autopilot log channel.

---

### [SEVERITY: LOW] Finding 8: `LoadScript` compiles unbounded source inside the sim tick and serializes it forever

**File**: `crates/csp-script/src/lib.rs:1097-1099` (prepare), `csp-sim/src/lib.rs:7985-7999` (apply)

**Problem**: `apply_load_script` hands the verbatim source to `engine.prepare` during the command
drain with no length cap. The sandbox pins guest *runtime* memory (`MAX_SCRIPT_STRING_SIZE` etc.)
but not compile cost or stored size: a multi-megabyte paste (or an MCP `LoadScript`) stalls the
tick compiling, then rides in every save and every `state_hash` walk (`hash_script` hashes the
source string).

**Suggestion**: reject `source.len()` over a generous cap (e.g. 256 KiB) at the apply — same
spirit as `MAX_SCRIPT_STRING_SIZE`, one `Recoverable`.

---

### [SEVERITY: NIT] Finding 9: ~30 host closures re-registered on the shared engine every script tick

**File**: `crates/csp-script/src/lib.rs:1166-1175` (`bind_fns` called from `run_tick`)

**Problem**: each `run_tick` re-registers the full guest surface on `self.engine` to capture the
tick's `Bound`. Correct (same-hash registrations replace, no accumulation) but wasteful and the
pattern invites the next contributor to *append* registrations. **Suggestion**: register once in
`new()` with closures over a `Rc<RefCell<Bound>>` swapped per tick.

### [SEVERITY: NIT] Finding 10: a script cannot discover its own vessel id

**File**: `crates/csp-script/src/lib.rs:1162` vs `:1189` — `telemetry_of`/`vessels` maps carry an
`id` key; `ctx.vessel` does not, so `transfer(to, …)` callers must guess which `vessels()` entry
is themselves. Add `id` to `ctx.vessel` (ctx-key freeze per Finding 5 covers it).

---

## Verified-correct (no findings — recorded so the parent doesn't re-audit)

- **§2.10 semantics under the new player paths**: budget (20k default, per-save `script_op_budget`
  forwarded to the engine), strikes (5 consecutive → visible disable + lease vacate + reason in
  the automation panel), poll demotion (10 heavy ticks, ≥9/10 budget or abort, named in
  `rails_warp_refusal`), atomic tick drain (fault ⇒ no effects/persist/wake), persist
  round-trip + hash, script/autopilot persistence across save/load/revert
  (`WorldSnapshot` scripts/autopilots/next_script_id; engines re-installed post-restore and
  flagged by `config_channels`). An infinite-loop paste strikes out in 5 ticks — no freeze, visible
  feedback (verified by `host_budget_abort` + strike machinery).
- **Budget escapes checked and closed**: wake chains still pay the per-call op budget; memory
  ceilings turn doubling loops into faults (`host_memory_limits_fault_not_oom`); template-depth
  cap kills the pre-parser stack overflow; NaN/non-finite guest values are rejected at the apply
  (`SetThrottle` lib.rs:5263, `SetAttitude` :5279) — no NaN enters sim state via scripts.
- **Rhai parity pin**: `rhai = "=1.27.0", default-features = false, features = ["no_std"]` —
  no `unchecked`/`only_i32`/`f32_float`, `sync` off; no wall clock/fs/net reachable (tested);
  transcendentals shadowed with libm for cross-target bit-parity.
- **Ascent guidance vs the real atmosphere**: Terrin top 70 km / H 5.6 km — pitch program
  (1.5→60 km) and the 12 s circularization lead are sane against it; acceptance test flies the
  fixture craft to orbit; UI engage (10–10,000 km, gated to `bubble` residence) matches the
  apply's residence/driver gates.
- **LlmAdapter absence is sanctioned**: §2.11 "Later LLM" + §6.0 "post" milestone row — correctly
  post-v1, not a gap.

## PRD coverage table

| PLAN item | Status | Evidence |
|---|---|---|
| §2.10 ControlApi trait surface | **DEGRADED** — action groups absent, no §6.4 sanction (Finding 4) | lib.rs:89-136 vs PLAN:723 |
| §2.10 tick-entry + budget + strikes + persist | **WIRED** (but see Finding 1 crash) | lib.rs:1096-1312; scripts.rs strikes/poll |
| §2.10 wake events (closed-form, refusal, Clamping) | **WIRED** | scripts.rs:661-813, resolve_wake lib.rs:9381 |
| §2.10 poll detection | **WIRED** | lib.rs:3707-3720, 7897-7919 |
| §2.10 Rhai parity / no wall clock | **WIRED** | Cargo pin; `no_wall_clock_reaches_the_guest` |
| §2.10 autopilots-as-services | **WIRED** (name/params deviate, Finding 4) | engage_autopilot guest verb |
| §2.10 api_freeze vs new UI verbs | **WIRED — no drift**; ctx schema unpinned (Finding 5) | api_freeze 3/3 green |
| §2.11 Ascent | **WIRED** with terminal-state defect (Finding 2) | csp-ai; bench acceptance green |
| §2.11 Transfer optimizer | **WIRED** (M5 path, not an AP service) | csp-sim/transfer.rs; map.rs planning |
| §2.11 Design assistant + MissionProfile emission | **MISSING — no §6.4 sanction** (Finding 3) | none; PLAN:775-778 vs §6.4 |
| §2.11 LlmAdapter | **MISSING — sanctioned post-v1** | PLAN:779-786, §6.0 "post" row |

## Top 3 issues

1. **CRITICAL** — shared persist cell in `ctx`: any guest property assignment into `ctx`
   (`ctx.persist = #{}`, `ctx.extra = 1`) hits rhai's `unreachable!()` and crashes the process;
   no catch_unwind exists (Finding 1).
2. **HIGH** — ascent AP never auto-disengages; its live record permanently blocks auto-rails, so
   the AP→orbit terminal state has no conic/orbit features (Finding 2, root cause side of the
   C03/C19 verified finding).
3. **HIGH** — §2.11 design assistant + MissionProfile emission missing with no §6.4 cut-list
   sanction (Finding 3).
