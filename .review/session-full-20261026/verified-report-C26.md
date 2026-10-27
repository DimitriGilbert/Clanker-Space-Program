# Verified report C26 — Scripting + AI (csp-script, api_freeze, csp-ai)

Verification agent pass over `review-report-C26.md`, working tree at HEAD `7dd4b86` (+ session
untracked review files). Every finding re-derived from source; the CRITICAL was re-reproduced
empirically in a scratch harness (rhai `=1.27.0, default-features = false, features = ["no_std"]`,
the exact workspace pin), then the harness was deleted. No repo file was edited.

Score: **10 confirmed, 0 dismissed** (three findings carry minor evidence corrections that do not
change their verdicts).

---

### Finding 1: A one-line player script panics the host — property assignment into `ctx` hits rhai's `unreachable!()` on the shared persist cell — CONFIRMED

**Original summary**: `run_tick` plants `ctx.persist` as a shared Rhai cell; rhai 1.27's
`calc_map_sizes` panics `unreachable!("shared values discovered within data")` on any property
assignment into the containing map; no `catch_unwind` exists, so the panic unwinds to the game
thread — crash to desktop from any pasted `ctx.foo = 1;`.

**Verification/Reason** — all five sub-checks of the brief hold:

a. **Mechanism confirmed at source.** `crates/csp-script/src/lib.rs:1183`
   `Dynamic::from_map(...).into_shared()` and `:1191` inserts that shared cell *inside* the `ctx`
   map; `:1200-1202` calls `engine.call_fn` with no panic guard. Rhai 1.27.0
   (`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/rhai-1.27.0/src/eval/data_check.rs:86`)
   panics in `calc_map_sizes` on `Union::Shared` inside a map. Crucially, the check is **gated on
   the engine's data-size limits being set** (`Engine::check_data_size` /
   `has_data_size_limit()`), and `RhaiHost` sets exactly those limits (`lib.rs:670-676`:
   `set_max_operations/string_size/array_size/map_size`) — the report's "the C16-F2 memory
   ceilings are precisely the code path that panics" is literally true: without limits the same
   script runs clean; with the shipped limits it aborts.

   **Empirical reproduction** (scratch harness, project's exact rhai pin and the same
   `set_max_*` configuration, `catch_unwind` only in the harness):

   | guest line | result |
   |---|---|
   | `ctx.persist["n"] = 1;` (documented idiom) | ok |
   | `ctx.persist = #{ n: 1 };` | PANIC `data_check.rs:86` |
   | `ctx.persist["n"] = 1; ctx.persist = #{};` | PANIC |
   | `ctx.extra = 1;` | PANIC |
   | `let c = ctx; c.persist = #{ n: 1 };` | PANIC |
   | `ctx.persist.remove("n");` / `let a = [ctx.persist];` / read-only | ok |

   Identical to the report's evidence table, including the ok rows. Backtrace for
   `ctx.extra = 1`: `calc_map_sizes ← calc_data_sizes ← Engine::check_data_size ←
   eval_dot_index_chain_raw` — the property-assignment (dot-chain) path checks the *target map's*
   total size, which is why merely adding an unrelated property panics. A run of the same script
   with the limits removed did **not** panic, confirming the gate.

b. **No containment.** `grep -rn "catch_unwind|set_hook|panic::|std::panic" crates/` — the only
   hits are a comment (`csp-script/src/lib.rs:371`) and `install_panic_hook`
   (`csp-app/src/lib.rs:2948-2961`), which writes a §2.14 bug-report bundle and *chains to the
   previous hook* — it does not catch or absorb the panic. Nothing wraps `engine.call_fn`.

c. **Player path fully real, no sandbox.** The AUTOMATION editor has a paste-in source field
   (`csp-ui/src/program.rs:1740` `script_source` text edit → `:1758` `ProgramCommand::LoadScript`),
   mapped 1:1 to `SimCommand::LoadScript` (`csp-view/src/lib.rs:1236-1237`), applied in-tick by
   `apply_load_script` (`csp-sim/src/lib.rs:7985-7999`) which stores the script `enabled: true`
   (`:8010`). `prepare` (`csp-script/src/lib.rs:1097-1119`) only preprocesses blocking waits and
   compiles — `ctx.extra = 1` compiles fine. The next `control_tick` (`csp-sim/src/lib.rs:7781`)
   calls `run_script_tick` → `RhaiHost::run_tick` → `call_fn` inline on the calling thread — the
   game thread (or the `--serve` dedicated game thread, `csp-app/src/lib.rs:2965+`). No
   `thread::spawn`, `JoinHandle`, or child process exists anywhere in `csp-sim`/`csp-script`.
   Panic = process death (profile is default unwind; the hook reports and chains).

d. **Doc claim at `lib.rs:1205-1209` is doubly false — confirmed.** "a non-map result means the
   script reassigned `ctx.persist` wholesale — also legal, read whatever it left": (1) the
   reassignment is not legal — it panics (reproduced); (2) even hypothetically, the write-back at
   `:1209` reads `persist_dyn` — the *original* shared cell handle — and rhai's
   `Dynamic::flatten_clone` (`types/dynamic.rs:1705-1713`) reads through a shared cell and returns
   the inner map, so `try_cast::<Map>()` is `Some` and the `None` branch is dead: whatever a
   reassigned `ctx.persist` held would never be read.

e. **Test-gap claim confirmed.** `cargo test -p csp-script -p csp-ai` → 13 + 3 pass (matches the
   report); every persist-touching inline test uses only the indexed-write idiom
   (`ctx.persist["k"] = …`, lib.rs:1536/1609/1628/1655-1658/1690/1750) — no reassignment or
   property-add variant exists.

Minor evidence corrections (do not affect the verdict):
- "or delivered via MCP `LoadScript`" — the current MCP surface (`csp-server/src/mcp.rs`) has no
  LoadScript tool; that delivery path is forward-looking. The AUTOMATION-editor path is fully
  present and sufficient.
- The panic site the report calls "`:1200-1202` (panic site)" is the *call* site; the panic
  itself is inside rhai (`data_check.rs:86`). Trivial.

**Verdict**: CONFIRMED — severity justified (any player crash; golden replays abort identically
by construction, since replay drives the same command through the same sim).

---

### Finding 2: The ascent autopilot never terminates itself — success permanently blocks auto-rails — CONFIRMED

**Original summary**: phase 4 issues `set_throttle(0.0)` forever and keeps its record alive until
manual `DisengageAutopilot`; the auto bubble→rails detector skips any vessel with an enabled
autopilot record, so the orbiting craft stays `InBubble` with no conic/SOI/map prediction.

**Verification/Reason**:
- `crates/csp-ai/src/lib.rs:162-165` — `_ => { api.set_throttle(0.0); }`; doc at `:24-25` "Phase
  4 — done. Throttle 0; the record stays until `DisengageAutopilot`". The driver never calls any
  disengage verb (verified across all 222 lines).
- `crates/csp-sim/src/lib.rs:4802-4804` — `if self.autopilots.get(vid).is_some_and(|a| a.enabled)
  { continue; }` in the auto-rails candidate loop; the code comment explicitly explains AP
  exclusion (silencing a live control producer), which makes the never-ending phase 4 the
  permanent suppressor.
- Bench codifies it: `crates/csp-bench/src/scenarios.rs:986`
  `assert!(w.autopilots()[&FLIGHT_VESSEL].enabled);` — the accepted terminal state has the
  record enabled, i.e. unrailable.
- One-call-fix claim verified: `ControlApi::disengage_autopilot` exists
  (`csp-script/src/lib.rs:131-132`, `IoControlApi` impl `:295-301` issuing the same
  `SimCommand::DisengageAutopilot` the UI button issues) and is the very api object the ascent
  driver holds (`csp-ai/lib.rs:198-208`). Matches the C03 verifier's conclusion
  (`verified-report-C03.md` Finding 2) — same defect from the csp-ai side; not double-counted.

**Verdict**: CONFIRMED.

---

### Finding 3 (PRD-coverage): §2.11 design assistant + MissionProfile emission missing with no §6.4 sanction — CONFIRMED

**Original summary**: §2.11's v1-classical list promises a design assistant (staging proposer)
and MissionProfile emission; neither exists; §6.4's cut list sanctions none of it.

**Verification/Reason** — quoted text checked verbatim:
- PLAN.md:771-778 (§2.11 "**v1 classical**"): "*Design assistant*: constraint solver over part
  stats — given payload mass, target Δv, min TWR, body, propose staging. Greedy + local search
  over the unlocked part set. No ML." and "*Mission profile*: the design assistant emits the
  `MissionProfile` that §3.4's confidence number requires".
- §6.4 (PLAN.md:1543-1562) cuts exactly five items: (1) R3 advanced render, (2) life support,
  (3) Duva, (4) orbital docking M6 (proximity transfer stays), (5) web tier polish. "Minimum
  shippable v1 = M0…M9a, M9b, M10". No design-assistant cut item, and no milestone row in §6.0's
  table assigns it an EU (M3 has "`MissionProfile` stub", M9b has "real `MissionProfile`
  confidence UI" — the UI, not the producer).
- Code: `grep -rniE "design.?assistant|staging.?propos" crates/` finds only two comments in
  `csp-phys/src/profile.rs` (`:9` "produced by the §2.11 design assistant", `:193` "coast time is
  the design assistant's to add **if it ever authors**"). `csp-ai` is 222 lines, ascent only.
  MissionProfile exists as the M3 stub lineage (`profile.rs` doc: "stub in M3, real in M9b") —
  matching the report's characterization.

**Verdict**: CONFIRMED (PRD internally inconsistent as stated; unsanctioned v1 shortfall).

---

### Finding 4 (PRD-coverage): §2.10 ControlApi verb list short — no action groups; `mission.ascend(target_apo, incl)` not offered as specced — CONFIRMED

**Original summary**: PLAN:723 specces "action groups" in the trait surface and PLAN:765 specces
`mission.ascend(target_apo, incl)`; built surface has neither; §6.4 does not sanction.

**Verification/Reason**:
- PLAN.md:723: "telemetry read, throttle/steer/SAS, staging, maneuver nodes, **action groups**,
  resource transfer, part actions, alarms, log/print." — verified verbatim.
- `grep -rniE "action.?group" crates/ --include="*.rs"` → zero hits. The trait
  (`csp-script/src/lib.rs:89-136`) and the frozen guest surface (`:317-343`) have per-part
  `deploy`/`jettison` but no grouping verb. §6.4's five items (above) do not cover it.
- PLAN.md:765: "callable services (`mission.ascend(target_apo, incl)`)"; built is
  `engage_autopilot(apo_m, pe_m)` (trait `:127-131`, guest binding `:1011-1033`, freeze list) —
  different name, and the second parameter is periapsis, not inclination. The freeze test
  (`tests/api_freeze.rs`) pins the current surface, so post-freeze additions are the major-version
  event the report describes.

**Verdict**: CONFIRMED (DEGRADED, as graded; practical v1 coverage argument accepted).

---

### Finding 5 (data integrity): the `ctx` schema is not pinned by any freeze test — CONFIRMED

**Original summary**: api_freeze pins methods + guest function names/arities but nothing pins the
`ctx` map contract; NaN-for-missing numerics is undocumented and interacts dangerously with the
primitive-persist rule.

**Verification/Reason**:
- `crates/csp-script/tests/` contains only `api_freeze.rs`; it pins `FROZEN_METHODS` and
  `FROZEN_GUEST_SURFACE` — no ctx/vessel key sets. The ctx contract lives only in the doc block
  (`lib.rs:37-60`) and `telemetry_to_map` (`:738+`). A key rename would produce zero test signal.
- NaN encoding confirmed at `lib.rs:740`:
  `let f = |v: Option<f64>| v.map_or(Dynamic::from_float(f64::NAN), Dynamic::from_float);` and
  `:773` for another optional field.
- Persist trap confirmed: `dynamic_to_persist` (`:550-565`, `:557` `f.is_finite().then_some(..)`)
  rejects NaN, so `ctx.persist["apo"] = ctx.vessel.apoapsis` faults ("unsupported or non-finite
  value", `:591`) when apoapsis is unknown → strike path. The convention is documented nowhere at
  the point of use (grep for "NaN" in csp-script finds only the encoder lines and one test).

Minor evidence correction: the `apo != apo` idiom the report attributes to "the stock script"
lives in the *bench* scenario script (`csp-bench/src/scenarios.rs:1001`), not the player-facing
`STOCK_SCRIPTS` (`csp-ui/src/program.rs:770-866`, which never mentions NaN). This strengthens
rather than weakens the point: the player-facing stock library does not teach the workaround.

**Verdict**: CONFIRMED.

---

### Finding 6: ascent AP's phase-2 recovery branch is dead code; open-conic engagement strands the autopilot — CONFIRMED (core), with evidence corrections on the secondary scenario

**Original summary**: the phase-2 safety guard requires apo Some + below target + `t_apo None`,
which (per `osculating`) never co-occurs on a genuine failed ascent; open conics leave `lead =
INFINITY` forever; zero csp-ai tests.

**Verification/Reason**:
- Guard at `csp-ai/lib.rs:141-146`: `apoapsis.is_some_and(|a| a < target - APO_OVERSHOOT_M) &&
  time_to_apoapsis_s.is_none()` — as quoted.
- `osculating` (`csp-sim/src/lib.rs:9347-9375`): apo = `Some` iff `e < 1.0`; `t_apo` from
  `scripts::resolve_apsis_ticks` (`scripts.rs:661-712`), which for an elliptic conic *always*
  returns `Ok(Some(tick))` (the `d <= 0 → d + 2π` wrap yields a full period even exactly at
  apoapsis) and errs only on degenerate elements (`e`/`mu` non-finite, `a <= 0`). In-bubble
  telemetry uses this same function (`lib.rs:9594-9598`). Therefore apo-Some + t_apo-None requires
  degenerate-yet-convertible elements — the guard is dead for every real failed ascent (which has
  `t_apo = Some`). Confirmed; if anything *deader* than claimed — the report's "at the exact
  apsis tick" case actually yields `t_apo = Some(full period)`, not None.
- Zero tests confirmed: `csp-ai/src/` contains only `lib.rs`, no `#[test]`, no tests dir.
- Correction on the open-conic scenario: reaching phase 2 requires the phase 1→2 transition,
  which needs `apo = Some ≥ target` (`:121-127`); during unpowered coast `e` cannot grow into
  ≥1, so *phase 2 with apo = None* is effectively unreachable. The real open-conic strand is one
  phase earlier: engage on an already-escaping vessel (apo = None from tick one) and phase 1's
  match arm never fires — the AP burns at throttle 1 forever, record alive, rails blocked. Same
  family of defect, arguably worse; the report's "hot engagement on an already-escaping vessel"
  example lands there, not in phase 2.

**Verdict**: CONFIRMED — dead safety code + no open-conic handling + zero unit tests are all
real; note the strand manifests in phase 1 (full-throttle burn) rather than phase 2 coast.

---

### Finding 7: `IoControlApi::log` misattributes autopilot log lines to a bogus script id — CONFIRMED

**Verification/Reason**: `csp-script/src/lib.rs:268-271`:
`let id = self.script.unwrap_or(ScriptId(self.source_id as u64));` — for autopilot callers
`script: None` (`csp-ai/lib.rs:207`) and `source_id` is the vessel-derived u32 channel key, so an
autopilot `log()` would write `script_log` under a fabricated id. Contrast the honest refusals in
`request_wake`/`cancel_wake` (`:255-266`). Harmless today: `AscentAutopilot::fly` never calls
`log` (verified across the file). LOW as graded.

---

### Finding 8: `LoadScript` compiles unbounded source inside the sim tick and hashes it forever — CONFIRMED

**Verification/Reason**: `apply_load_script` (`csp-sim/src/lib.rs:7985-7999`) passes the verbatim
source to `engine.prepare` during the command drain (apply at `:5425`), with no length cap
anywhere on the command path (UI `csp-ui/program.rs:1758`, view `csp-view/lib.rs:1236` both
forward the string untouched; grep found no `source.len()` gate). Runtime memory ceilings
(`lib.rs:611-615`, `:670-676`) do not bound compile cost or stored size. `hash_script`
(`scripts.rs:581-598`) writes `h.str(&s.source)` and is called from the `state_hash` walk
(`lib.rs:8880`) — every hash re-walks the full source; the source also serializes in every save
(ScriptState.source). LOW as graded.

---

### Finding 9 (NIT): host closures re-registered on the shared engine every script tick — CONFIRMED

**Verification/Reason**: `run_tick` calls `self.bind_fns(..)` (`lib.rs:1167-1175`) each tick;
~45 `register_fn` registrations in the crate. Same-name registration replaces rather than
accumulates, so cost is per-tick re-registration only. NIT as graded; the suggested
register-once-with-`Rc<RefCell<Bound>>` shape is sound.

---

### Finding 10 (NIT): a script cannot discover its own vessel id — CONFIRMED

**Verification/Reason**: `telemetry_by_id`/`telemetry_list` entries get an `"id"` key
(`lib.rs:1162-1164`), but `ctx.vessel` is built from `telemetry_to_map` alone (`:1187-1190`),
which has no `id`. A `transfer(to, …)` caller must guess which `vessels()` entry is itself. NIT
as graded.

---

## PRD coverage rows re-verified

| Report row | Verdict | Evidence |
|---|---|---|
| Action groups DEGRADED unsanctioned | CONFIRMED | Finding 4 above |
| ctx schema unpinned | CONFIRMED | Finding 5 above |
| LlmAdapter sanctioned post-v1 | CONFIRMED | §2.11 "**Later LLM**: `LlmAdapter` trait…" (PLAN:779-786) + §6.0 "post" row "…LLM assistant…" (PLAN:1512) |
| Transfer optimizer WIRED (M5 path) | CONFIRMED present | `csp-sim/src/transfer.rs` (lambert/lambert_multi, porkchop_scan header docs) |
| §2.10 wake/poll/budget rows WIRED | consistent with source seen | scripts.rs wake resolution, `control_tick`/`run_script_tick` structure as described |
| §2.11 Ascent WIRED with terminal-state defect | CONFIRMED | Finding 2 |

## Verified-correct section spot checks

- Test counts reproduce: `cargo test -p csp-script -p csp-ai` → 13 + 3 pass.
- No panic containment exists (see Finding 1b) — the §2.14 hook is report-only.
- Feature pin verified: workspace `rhai = "=1.27.0", default-features = false, features =
  ["no_std"]` (root Cargo.toml:45; Cargo.lock 1.27.0) — `unchecked`/`no_closure` off, which is
  precisely why the data-size check and the Shared arm are both compiled in.

## Bottom line

The CRITICAL is real, reproduced, and player-reachable today through the shipped AUTOMATION
editor with no sandbox of any kind. Findings 2 and 3 stand as HIGH. All MEDIUM/LOW/NIT findings
stand; the three evidence corrections (MCP delivery path not yet in tree; NaN idiom's location;
Finding 6's stranded phase) narrow details, not verdicts.
