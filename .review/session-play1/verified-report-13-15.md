# Verified Report — P13, P14, P15 (session-play1)

Verification method: every finding re-read against the cited code (git history where
claimed; the ron-0.12.2 registry source for the NaN-literal claim). Verdicts:
**CONFIRMED** = the code does what the report claims and the impact reasoning holds;
**CONFIRMED-TRIVIAL** = code matches but the impact is cosmetic/unreachable (nit-grade);
**DISMISSED** = the code contradicts the claim.

---

## P13 — Audio

### F1 — MEDIUM — `MAX_PIPE_LEAD_S` is a wall-clock proxy; pipe saturation is unrecoverable — **CONFIRMED**
`crates/csp-app/src/audio.rs:296-321` (`writer_loop`): `lead = written_frames/SR −
started.elapsed()` measures *written-vs-wall*, not pipe occupancy (`W − drained`);
there is no `fcntl(F_GETPIPE_SZ)`/`poll` readback anywhere. Once the blocking
`write_all` (L318) meets the default 64 KiB pipe cap (~185 ms at 44.1 kHz stereo
f32), writes pace at the player's drain rate and `lead` freezes below the 0.05 cap
— the sleep branch (L304-307) never re-engages, so occupancy stays pinned at cap.
Entry condition verified: post-stall `lead ≈ 0.05 − S`; catch-up writes fill the
drained pipe before `lead` reaches the cap, and saturation holds once
`S ≥ cap − 0.05 ≈ 135 ms`. The suggestion (re-baseline on `lead < −cap` debt, or
nonblocking + drop-oldest) is apt — the latency fix silently reverts to ~185 ms +
server buffer with no log.

### F2 — MEDIUM — `cue_shot` covers ~1/3 of the pushable vocabulary — **CONFIRMED**
`audio.rs:105-130` maps the 10 `sfx_edges` texts + 5 exact refusal strings. Grep of
every `flight_notice(` call site finds ≥15 unmapped texts: `VESSEL NOT RECOVERABLE —
NOT ON RAILS HERE` (view lib.rs:1511), `NO VESSEL TO RECOVER` (:1523), `ALL STAGES
FIRED — NOTHING LEFT` / `SINGLE-STAGE CRAFT — NOTHING TO STAGE` (:1798-1800),
`STAGING NOT READY — LAUNCHED CRAFT NOT SPAWNED` (:1814), `NOT STAGED — PARKED ON
THE SURFACE…` (:1820), `NOT STAGED — VESSEL IS ON RAILS` (:1826), `NOTHING TO STAGE
— LAUNCHED CRAFT GONE` (:1838), the `format!` notices `STAGED — {n} PART(S)
JETTISONED` (:1857) and `SETTINGS — {n} KEY BINDING(S) MIGRATED…` (app lib.rs:356)
that can never literal-match, and read-only refusals `WARP DEMOTION` (:450),
`LAUNCH` (:1041), `MANEUVER` (:1055), `STAGING` (:1619), `RECOVER-CONTROL` (:1819)
— only the attitude/throttle pair is mapped. Every successful staging logs a
fresh "cue vocabulary drifted" warn via `unmapped.insert` (audio.rs:498-503).

### F3 — LOW — continuous voices track `vessels.last()`, not the pin-aware focus — **CONFIRMED**
`audio.rs:509` `let focus = snap.vessels.last()` vs the HUD rule
`focus_override.target.and_then(find).or_else(focus_vessel)` (flight.rs:1128-1131).
Already confirmed twice upstream (P04-F4, P07-F1/P11-F1).

### F4 — LOW — a single NaN frame kills a continuous voice — **CONFIRMED**
`audio.rs:510-516`: `f64::clamp` passes NaN through (`self` NaN fails both
comparisons), `as f32` keeps it, `level += (NaN − level)·0.25` poisons the smoother
permanently; `NaN > LEVEL_EPS` is false at L518-527 → gain pinned 0 forever.
Reachability requires a NaN `throttle`/`plasma_w_m2` in the snapshot — an edge
case, but the dead-forever mechanism is exactly as described.

### F5 — LOW — settle check commits to players that die after 60 ms — **CONFIRMED**
`audio.rs:385-394`: after `PLAYER_SETTLE_MS` the loop commits to the first live
child; `update` (L479-486) flips `alive=false` permanently on later exit — the
remaining `PLAYERS` candidates are never re-tried. The unconditional main-thread
settle sleep (60–180 ms) also checks out.

### F6 — LOW — pad launch stacks two identical `EngineIgnition` shots — **CONFIRMED**
`SetThrottle` unlatches + raises throttle the same frame; `sfx_edges` pushes both
`LIFTOFF` (latch edge, flight.rs:1251-1252) and `ENGINE IGNITION` (throttle edge,
L1236-1237) in one pass; both map to `OneShot::EngineIgnition` (audio.rs:107,114)
→ two coherent copies of the same buffer, +6 dB.

---

## P14 — Settings & session persistence

### F1 — MEDIUM — `graphics.quality` is a dead control — **CONFIRMED**
Workspace-wide grep for `quality`/`GraphicsQuality`: references exist only in
`crates/csp-ui/src/settings.rs` (field L88-89, enum L116-126, combo box L748-762,
tests L891/913/921) plus the `csp-ui/src/lib.rs:37` re-export. `apply_live_settings`
(csp-app/lib.rs:431-459) applies only `ui_scale` and `max_warp_multiplier`; no
render crate reads the value. The field doc ("selects the render tier the M10
render work consumes", settings.rs:37-38) names a consumer that never landed — a
user-facing control that reports "settings saved" and changes nothing.

### F2 — MEDIUM — keymap forward-compat gaps — **CONFIRMED**
(a) Upgraders get dead verbs: `bindings` is a `BTreeMap<KeyAction, Vec<KeyBinding>>`
(settings.rs:515-521) — serde fills only present keys; `#[serde(default)]`
backfills a *missing section*, never missing map entries. `Keymap::sanitize`
(L617-640) iterates `KeyAction::ALL` via `get_mut` + `continue` — absent actions
are skipped, never filled from `Keymap::default()`. `git show HEAD~4` verifies the
premise: 14 variants then, 20 now (`ThrottleUp/Down`, `Stage`, `RecoverControl`,
`RollLeft/Right` added) — a previous-milestone `settings.ron` leaves all six
unbound forever (persisted back on the next Apply).
(b) All-or-nothing parse: `load_settings_from` (settings_io.rs:87-109) runs one
`ron::from_str::<Settings>` — an unknown `KeyAction`/`GraphicsQuality` variant
(e.g. a downgrade, a hand-edit typo) fails the whole file → `Settings::default()`
with only `log::warn!`, no in-app notice. The `unknown_keys_ignored` rule covers
struct fields (test L908) but not map keys/enum variants — the claim is exact.

### F3 — LOW-MEDIUM — `NaN` literals bypass every clamp — **CONFIRMED**
ron 0.12.2 accepts `NaN`/`inf`/`NaNf32`/`NaNf64` (registry `parse.rs:468-475`).
`f32::clamp` returns NaN unchanged, so `ui_scale: NaN` survives the apply-time
clamp and lands `set_zoom_factor(NaN)` (csp-app/lib.rs:432-437); `master_volume:
NaN` → NaN gain → NaN samples into the pipe (audio.rs:549-555). `inf` clamps fine
— only NaN slips; no finiteness check exists at load or apply.

### F4 — LOW — a captured rebind key never records its glyph label — **CONFIRMED**
`window_event` returns early at csp-app/lib.rs:1882-1886 whenever
`intercept_rebind_key` swallows the event — `note_key_label` at L1890 never runs
for the capture press (nor its swallowed repeats/release, L1702-1724). The capture
arm (L1344-1356) calls `bind_key` but never `set_key_label`, so the fresh binding
prints the QWERTY `physical_fallback` until the key is pressed in normal dispatch.

### F5 — LOW — AZERTY migration maps glyphs to QWERTY positions — **CONFIRMED**
`char_to_physical_token` (settings_io.rs:346-370) maps `"z"` → `"key-z"` — the
QWERTY-named position — regardless of the layout the file was authored under; an
AZERTY-authored `"z"` (typed at the W-position) relocates to the Z-position key.
The notice (csp-app/lib.rs:356-358) reports only a count, not which bindings
moved. Digit migration (AZERTY shifted-row `"1"` → unshifted `key-1`) also
verified in the table. Disclosed, idempotent, self-healing — LOW is fair.

### F6 — LOW — `schema_version` round-trips the file's value — **CONFIRMED**
`schema_version` deserializes verbatim (settings.rs:33); `save_settings_to`
(settings_io.rs:122-130) writes the struct back, so `schema_version: 99` in →
`99` out — the writer stamps the file's value, not `SETTINGS_SCHEMA_VERSION` as
its own doc requires. Harmless today; dishonest by contract.

---

## P15 — New regression-test surface

### F1 — HIGH — fuel pooled-coverage untested — **CONFIRMED**
Both sim fuel tests build `fueled_engine_craft` (csp-sim/lib.rs:17244-17257): one
part, one `EngineSpec`, one propellant (`liquid_fuel`, frac 1.0) — per-claimant
and pooled coverage are numerically identical under a single claimant, so a
revert of the shared-denominator fix (`coverage = min(1, avail/Σ needs)`,
L7794-7802) passes both tests. Workspace grep finds no test constructing two
engine parts, a multi-propellant spec, or an un-holdable propellant
(`units = f64::INFINITY`, L7773-7775). The exploration map's claim (line 158:
"cover pooled-coverage + starvation — two co-claimant engines, un-holdable
propellant, jettisoned tank mid-burn") is inaccurate — none of the three exists.
Nit verified: `TERRIN_R = 600_000` (L12394), so `TERRIN_R + 200_000` is 800 km
*radius* / 200 km altitude — the L17339 "~800 km orbit" comment is wrong.

### F2 — HIGH — dead-stick recovery has zero test coverage — **CONFIRMED (with one factual correction)**
Core claim holds: `git log -S recover_control` → only `d8cd46f`; no test invokes
`View::recover_control`, dispatches `KeyAction::RecoverControl`, or touches
`FlightFocusOverride`/`to_bubble_eligible` — `flight_focus.rs` (whole file: 43
lines) pins only the newest-spawn rule. None of the pin's release paths
(staged-then-left flight.rs:1121-1127, arena-gone lib.rs:1161-1163, fresh launch
lib.rs:1758) nor the pin-and-silent-reject eligibility mirror is exercised.
**Correction:** the supporting claim "no test … enqueueing `SimCommand::ToBubble`
outside bench scenarios" is wrong — `bubble_rails_bubble_roundtrip`
(csp-sim/lib.rs:13489-13540) enqueues `SimCommand::ToBubble` and asserts
re-materialization at the advanced conic + dead-SOI-event drop. The sim
*primitive* is tested; the view/app *feature path* (verb → eligibility → pin →
lease) has zero coverage — severity stands.

### F3 — MEDIUM-HIGH — AZERTY event→token resolution untested — **CONFIRMED**
`settings_io.rs` has no `mod tests`; `keycode_token`, `event_binding`,
`event_label`, `migrate_char_bindings`, `layout_dependent_binding_count` appear
only in production call sites (csp-app/lib.rs:350,363,1344,1365,1384,1666). The
app test module (L2209-2753) contains no `key_press`/`action_of`/
`intercept_rebind_key`/`captured_key`/`drop_rebound_holds` coverage;
`rebound_key_dispatches_its_new_action` (L2399-2428) resolves through logical
`key_token` + `action_for` string lookup — physical-first precedence and
press/release symmetry are never exercised. The csp-ui `Keymap` tests cover the
map, not the event path.

### F4 — MEDIUM — `StagePlan` lazy binding never exercised — **CONFIRMED**
Both tests install `StagePlan::bound_to` (stage_plan.rs:70,152 — doc at
flight.rs:478-480 says "for tests and scripted producers"), bypassing
`for_launch`'s `min_id` capture (flight.rs:473), `craft_fingerprint` matching and
`.rev()` newest-match in `vessel_for` (L497-516). No test references `for_launch`
or `stage_plan_of` (lib.rs:1629/1741/1948). Uncovered `fire_stage` arms: both
empty-plan texts (lib.rs:1797-1801), unspawned (:1811-1815), OnRails (:1825-1827),
gone-vessel retirement (:1829-1839) — only latched-refusal and the success path
run. Not tautological (the two covered cases genuinely fail pre-fix), but the
binding *mechanism* — where a real off-by-one/`min_id` bug would live — is
untested.

### F5 — MEDIUM — `render_gate` passes vacuously on GPU-less CI — **CONFIRMED**
`headless_device` returns `None` with no adapter (render_gate.rs:55-74); both
tests `eprintln!` + `return` (L251-254, L325-327) → `cargo test` reports `ok`
with zero render coverage and no countable signal.

### F6 — LOW — most of the listed "new" surface predates the playability track — **CONFIRMED**
`git log -1`/`git log -S` verified: `flight_focus.rs` b06e8c2 (Phase 14),
`part_silhouettes.rs` fd2158d (Phase 13), `tutorial.rs` b06e8c2, `vab_latency.rs`
c55e276 (M3), `demo.rs` f5d21eb (Phase 9), `startup_colonies.rs` 2ec4128 (M8),
`starter_craft_rests_on_the_pad` 4bc10bd, render-gate tests 4eab706 — all
pre-track. Actually new: `stage_plan.rs` + `launch_site_sits_on_rendered_ground`
(f5e8177), the two fuel tests + `in_bubble_vessel_appears_in_map_sync` (d8cd46f),
`held_throttle_ramps_then_holds_on_release` +
`pad_attitude_press_notices_and_defers_until_liftoff` (5529f5f).

### F7 — LOW — `part_silhouettes` covers shape class only — **CONFIRMED**
The file (135 lines) asserts `PartShape::Lathe` classification, profile
monotonicity/flare, and triangle-count distinctness — no assertions on the
5529f5f visual work (smooth normals, `kind_tint` livery, two-shell plume, sheath
alpha). Last touched fd2158d, predating the commit it would guard.

### F8 — LOW — bake constants duplicated as literals — **CONFIRMED**
`HEIGHTFIELD_W/H = 1024/512` are private `const`s (terrain.rs:30-31);
pad_spawn.rs:127 hardcodes `HeightField::bake(body.seed, 1024, 512)` — a
resolution change desyncs the mirror silently.

### F9 — LOW — `vab_latency` release hard assert is a REF-A number — **CONFIRMED**
vab_latency.rs:159-164: `budget_ms = if cfg!(debug_assertions) { 200.0 } else
{ 16.0 }` — an absolute machine-specific kill criterion asserted on arbitrary CI
hardware; the comment documents the caveat honestly.

---

## Summary table

| Finding | Severity | Verdict | One-line basis |
|---|---|---|---|
| P13-F1 | MEDIUM | **CONFIRMED** | `lead` is W−T not W−drained; blocking `write_all` + 64 KiB pipe → saturation sticky below the 0.05 cap forever (audio.rs:296-321) |
| P13-F2 | MEDIUM | **CONFIRMED** | ≥15 `flight_notice` texts unmapped incl. two `format!` families that can never literal-match (audio.rs:105-130 vs view lib.rs:1798-1857, app lib.rs:356-1819) |
| P13-F3 | LOW | **CONFIRMED** | `snap.vessels.last()` (audio.rs:509) vs pin-aware focus (flight.rs:1128-1131) |
| P13-F4 | LOW | **CONFIRMED** | NaN survives `clamp` → smoother poisoned → `NaN > LEVEL_EPS` false → voice dead permanently (audio.rs:510-527) |
| P13-F5 | LOW | **CONFIRMED** | 60 ms settle commits; later death → `alive=false`, no fallback (audio.rs:385-394, 479-486) |
| P13-F6 | LOW | **CONFIRMED** | LIFTOFF+IGNITION same frame → 2× `EngineIgnition` coherent sum (flight.rs:1236-1253, audio.rs:107/114) |
| P14-F1 | MEDIUM | **CONFIRMED** | `GraphicsQuality` referenced only in csp-ui model/panel; `apply_live_settings` has no quality branch |
| P14-F2 | MEDIUM | **CONFIRMED** | sanitize skips absent actions (never backfills; HEAD~4=14 variants verified); one bad variant → whole-file default, log-only |
| P14-F3 | LOW-MEDIUM | **CONFIRMED** | ron 0.12.2 parses `NaN`; `clamp` passes it → `set_zoom_factor(NaN)` / NaN master gain |
| P14-F4 | LOW | **CONFIRMED** | intercept returns before `note_key_label` (lib.rs:1882-1890) → QWERTY fallback shown post-rebind |
| P14-F5 | LOW | **CONFIRMED** | glyph→QWERTY-position migration relocates AZERTY-authored bindings; notice names no keys |
| P14-F6 | LOW | **CONFIRMED** | file's `schema_version` round-trips; writer never stamps its own |
| P15-F1 | HIGH | **CONFIRMED** | both fuel tests single-engine/single-propellant; pooled denominator untested; exploration-map coverage claim inaccurate |
| P15-F2 | HIGH | **CONFIRMED** | zero tests for `recover_control`/pin/eligibility — but correct the report: `SimCommand::ToBubble` IS tested (sim lib.rs:13489) |
| P15-F3 | MEDIUM-HIGH | **CONFIRMED** | `settings_io` has no test module; physical-first resolution/migration/capture untested |
| P15-F4 | MEDIUM | **CONFIRMED** | both tests use `bound_to`; `for_launch`/`vessel_for`/`stage_plan_of` + 4 refusal arms uncovered |
| P15-F5 | MEDIUM | **CONFIRMED** | no adapter → `eprintln!` + early return → silent `ok` |
| P15-F6 | LOW | **CONFIRMED** | git history verified — most listed files predate the track |
| P15-F7 | LOW | **CONFIRMED** | shape-class assertions only; no normals/tint/plume/sheath coverage |
| P15-F8 | LOW | **CONFIRMED** | private 1024/512 consts duplicated as literals in the test |
| P15-F9 | LOW | **CONFIRMED** | absolute 16 ms release-mode budget asserted on arbitrary hardware |

## Notes for the fixer

- **P13-F1** is the subtlest real defect in this batch: the fix is not "tune the
  constant" — the metric cannot see occupancy. The report's re-baseline sketch
  (treat `lead < −MAX_PIPE_LEAD_S` as unrecoverable debt and reset the clock) is
  the cheapest correct patch; nonblocking write + drop-oldest bounds occupancy
  directly.
- **P15-F2** severity stands but write the fix notes accurately: add view-level
  tests for `recover_control`/pin lifecycle; do not add a redundant sim-level
  `ToBubble` test — `bubble_rails_bubble_roundtrip` already covers it.
- No findings dismissed outright; P15-F2 carries the factual correction above.
