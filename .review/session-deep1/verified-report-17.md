# Verified Report — Cluster C17 (csp-app)

Verification of `.review/session-deep1/review-report-17.md` against actual
source. Scope checked: `crates/csp-app/src/{lib,warp,settings_io,audio}.rs`,
`crates/csp-sim/src/lib.rs` (`step_frame`, `drain_at`, `AdvanceTo`,
`MAX_TICKS_PER_FRAME`, `rails_warp_refusal`, tests at 9539–9566),
`crates/csp-view/src/{lib,flight}.rs`, `crates/csp-ui/src/vab.rs`,
`crates/csp-render/src/lib.rs`, `crates/csp-core/src/time.rs`, `Cargo.toml`.

**Verdict summary: 8/8 findings CONFIRMED.** Findings 1 and 2 are in fact
*mildly understated* — see notes below.

---

## [HIGH] Finding 1 — CONFIRMED (mechanism is worse than reported)

**Claim:** `demote(0)` leaves `requested` intact; `decide()` runs pre-step and
rebuilds the promotion streak under a persistent refusal, so the §2.6.2 gate
is defeated every ~30 frames.

**Verified:**
- `warp.rs:217-226` — `demote()` touches only `effective`, `promote_streak`,
  `promote_candidate`; `requested` and `scaled_nanos` untouched. The
  "cannot oscillate" comment is at `warp.rs:210-213` as quoted.
- `lib.rs:649-660` — `observe()` → `decide()` → `target_tick()` all run
  *before* `view.sim_step(target)` (lib.rs:700). A `decide()` promotion
  emits `SetWarp` (652-656) and inflates this frame's `AdvanceTo` (666-670)
  before the post-step refusal check at `lib.rs:731-744` runs.
- `warp.rs:142-180` — `decide()` computes `ladder_ceiling` from `requested`
  with no refusal awareness; with `effective=0 < requested=6`,
  `margined > next` holds (per-tick cost is unchanged by the refusal), so
  `promote_streak` rebuilds and promotes after `PROMOTE_FRAMES=30`
  (warp.rs:165-169).
- Sim side confirmed: `step_frame` (csp-sim `lib.rs:6760`) drains the
  enqueued `SetWarp` + `AdvanceTo` at the same call's first boundary —
  `AdvanceTo(t)` raises `effective_target` via `.max()` (`lib.rs:4044`) —
  so the promoted target is honored the same frame, bounded only by
  `MAX_TICKS_PER_FRAME` (69120 ticks = 1080 sim-s at `TICK_HZ=64`,
  csp-core `time.rs:9`).
- `rails_warp_refusal` (csp-sim `lib.rs:2968-3013`) returns `Some` for
  under-thrust / unsettled-contact / in-atmosphere as described and
  persists while the condition persists.

**Stronger than reported — the streak is never even reset:** `demote(0)`
early-returns at `warp.rs:219` (`if rung >= self.effective { return None }`)
*without* clearing `promote_streak`/`promote_candidate`. While a refusal
holds `effective=0`, the per-frame `demote(0)` call at `lib.rs:732-734` is a
complete no-op — it does not even touch the streak the comment claims it
suppresses. So the 30-frame streak builds *uninterrupted* across refused
frames; the cycle is exactly 30 frames (0.5 s at 60 fps) per injection, not
30 + 1. The "persistent refusal re-demotes every frame it is checked, so it
cannot oscillate" rationale is wrong on both counts: the re-demote is a
no-op during build-up, and `decide()` never sees the refusal.

**Math check:** promotion frame accumulates `dt × 10⁵` = 16.67 ms × 10⁵ ≈
1667 sim-s ≈ 1.67 sim-ks (report correct). Injection ≈ 1667 sim-s / 0.5 s
wall ≈ sustained ×3300 (report correct). A `SetWarp(100_000)`/`SetWarp(1)`
pair is logged per cycle (confirmed — both go through `enqueue_command` →
`drain_boundary` → `command_log`).

---

## [HIGH] Finding 2 — CONFIRMED (impact figure understated ~65×)

**Claim:** `scaled_nanos` is never rebased; after sustained high warp the
sim keeps stepping `MAX_TICKS_PER_FRAME` every frame at ×1 displayed warp.

**Verified:**
- `warp.rs:196-204` — `scaled_nanos` is a monotonic `u128` accumulator;
  `target = scaled_ticks.max(current_tick)`. Nothing anywhere resets it.
  `demote` (217-226) and `set_cap_multiplier` (235-242, which delegates to
  `demote`) don't touch it; manual step-downs change only `requested` →
  `effective` via `decide`.
- `step_frame` (csp-sim `lib.rs:6760-6801`) advances up to
  `MAX_TICKS_PER_FRAME = 69_120` (line 214) ticks per call toward the
  absolute target; per-call split confirmed by test
  `the_per_call_budget_splits_advances` (9539-9548).
- Stale `AdvanceTo` is absorbed harmlessly — test
  `advance_to_never_retreats_the_clock` (9550-9566) confirms the report's
  claim that rebasing is legal within the §7.6 contract.
- The warp.rs:194-195 comment ("`MAX_TICKS_PER_FRAME` back-pressure bounds
  any real overshoot") bounds ticks *per call*, not the outstanding
  absolute target — quoted accurately.

**Math check:** accumulate 1667 sim-s/frame vs drain 1080 sim-s/frame →
net ~587 sim-s/frame backlog at 60 fps, ~2253 at 30 fps (report's ~590/~2250
correct). One wall-minute outstanding ≈ 587 × 3600 ≈ 2.1 Ms ≈ 24 sim-days
(correct). Drain time ≈ 2.1 Ms / 1080 s·frame⁻¹ ≈ 1944 frames ≈ 32 s wall.

**Correction (understatement):** the report says the post-step-down drain
runs at "~×1000 wall-effective at 60 fps". The actual rate is 1080 sim-s
per frame × 60 frames/s ≈ **×64,800 wall-effective** — the mechanism and
impact direction are confirmed; if anything the user-visible symptom (clock
racing at near-max-ladder rate for ~30 s while HUD shows ×1) is *worse*
than the figure given. The conclusion stands.

---

## [MEDIUM] Finding 3 — CONFIRMED

**Claim:** craft name is raw `TextEdit` text; `dir.join("{name}.cspcraft")`
allows `..`/absolute-path escape and separator names produce phantom store
entries.

**Verified:**
- `csp-ui/src/vab.rs:978-980` — `TextEdit::singleline(&mut vab.save_name)`;
  only gate is `!save_name.is_empty()`. No sanitization anywhere in the
  pipeline: `vab.rs:980` → `csp-view/src/lib.rs:403-407`
  (`outbox.craft_save = (name, bytes)` verbatim) → `csp-app/src/lib.rs:815`.
- `lib.rs:831-832` — `craft_store.insert(name, bytes)` runs *before*
  `save_craft_file`, and `set_craft_files` + `Notice("saved {name}")`
  (834-836) run unconditionally — confirming the phantom-entry claim.
- `save_craft_file` (lib.rs:1471-1477) — `dir.join(format!("{name}.cspcraft"))`,
  only `dir` itself is `create_dir_all`'d. On Unix, `name="/tmp/x"` produces
  `dir.join("/tmp/x.cspcraft")` = `/tmp/x.cspcraft` (absolute join replaces
  base); `name="../../x"` escapes `crafts/`; `name="a/b"` fails the write
  (missing subdir) while the store insert already committed — a phantom
  entry that `load_craft_store` (1443-1459, non-recursive `read_dir` keyed
  by `file_stem`) never re-lists next launch. All confirmed.
- wasm path is a no-op (`save_craft_file` stub at 1480-1481) and demo gate
  blocks saves (lib.rs:816-822), so this is native-only — consistent with
  the report's framing.

---

## [MEDIUM] Finding 4 — CONFIRMED

**Claim:** `set_flight_viewport` is only called in the `Resized` handler
behind a `gpu && flight_renderer` guard; never in `finish_gpu_init` or
`run_frame` → stuck at (1.0, 1.0) on wasm and on platforms without an
initial `Resized`.

**Verified:**
- `lib.rs:1248-1252` — sole call site, inside `WindowEvent::Resized`,
  guarded by `(Some(g), Some(f))`.
- `lib.rs:688-693` — `set_map_viewport`/`set_vab_viewport` refreshed
  per-frame from `gpu.config` in `run_frame`; flight has no equivalent.
- `finish_gpu_init` (lib.rs:545-599) — read in full; creates the renderers
  but never calls `set_flight_viewport`.
- wasm path (lib.rs:504-538): `gpu`/`flight_renderer` are `None` until
  `poll_gpu_init` lands `finish_gpu_init`; any `Resized` during that window
  is dropped by the guard with no later re-set except manual resize.
- Default `viewport_px: (1.0, 1.0)` at `csp-view/src/flight.rs:84`;
  consumers confirmed — `solve_orbit_camera` (flight.rs:652, aspect) and
  `terrain::build_packet` (flight.rs:785, LOD).

---

## [LOW] Finding 5 — CONFIRMED

- `lib.rs:1244` — `settings.graphics.window_size = [size.width, size.height]`
  written unconditionally for every `Resized`, including 0×0.
- `csp-render/src/lib.rs:146-152` — `Gpu::resize` correctly ignores 0×0
  (`if width > 0 && height > 0`), so the settings write is the only leak.
- `persist_settings` on `CloseRequested` (lib.rs:1233-1239 → 357-365)
  writes the polluted value; `create_window` (lib.rs:466-469) floors with
  `.max(1)` → next launch restores a 1×1-px window. Confirmed.

---

## [LOW] Finding 6 — CONFIRMED

- `lib.rs:867-869` — `outbox.launch` → `launch_craft()` with no
  `read_only`/`demo`/`fatal` gate; `csp-view/src/lib.rs:1357-1368` enqueues
  `SpawnPhysicalVessel` on source_id `1` and calls
  `set_mode(ViewMode::Flight)` unconditionally — the misleading mode switch
  is real.
- `lib.rs:871-874` — `outbox.maneuver` enqueues on `MANEUVER_SOURCE_ID`
  with no gate.
- While `fatal.is_some()` the whole controller/step block is skipped
  (`target = None`, lib.rs:649-674) → `sim_step` never runs → the queue
  never drains → the enqueued commands are silently inert. Confirmed.
- Contrast verified: `craft_save` has both `demo` and `read_only` gates
  with honest `Notice`s (lib.rs:816-830). The asymmetry is real.

---

## [LOW] Finding 7 — CONFIRMED

- `lib.rs:1260-1263` — `key_press` (which hosts the rebind capture at
  1119-1131) is only invoked under `!consumed`. A press aimed at a focused
  `TextEdit` is consumed by egui → capture misses it, key lands as text.
  Confirmed.
- Second part also confirmed: `bind_key` (`csp-view/src/lib.rs:1069-1077`)
  writes into the *staged* `SettingsPanelRes.0.settings.keys`; `dispatch`
  reads the live `self.settings.keys` (lib.rs:1135) until Apply — held-key
  repeats after a successful capture fall through to normal dispatch under
  the old keymap, and `accepts_repeat` actions can fire.

---

## [LOW] Finding 8 — CONFIRMED (all four items)

1. `lib.rs:806-814` plays `ui_confirm` for `outbox.mode`; `dispatch`
   (1148-1206) performs `ToggleMap`/`ToggleVab`/`OpenSettings`/`ToggleHelp`
   with no blip. Inconsistency confirmed.
2. `settings_io.rs:124-126` — wasm `save_settings` is `Ok(())` no-op →
   "settings saved" notice (lib.rs:860) on nothing persisted. Confirmed.
3. `audio.rs:389-413` — `update()` runs `set_master`, drains cues and calls
   `play()` (pushing up to `MAX_ACTIVE_SHOTS=16`, line 52/358) *before* the
   `!self.alive` early return at 411. Confirmed, harmless as stated.
4. `lib.rs:1172` — throttle dispatch uses bare `1`; named constants
   `WARP_SOURCE_ID=0`, `MANEUVER_SOURCE_ID=2`, `SETTINGS_SOURCE_ID=3` at
   lib.rs:72-78; `View::launch_craft` also emits on `1`
   (`csp-view/src/lib.rs:1359`). Confirmed.

---

## Spot-check of "came back clean" items — all hold

- **Pick protocol:** `pick_in_flight` one-in-flight gate (lib.rs:1075),
  decode-table snapshot at encode (1085→1090), `checked_sub(1)` +
  bounds-checked decode (641-642). Consistent.
- **Command ordering:** `SetWarp` then `AdvanceTo` both on
  `WARP_SOURCE_ID=0`, FIFO drain in production order (`drain_at`,
  csp-sim `lib.rs:5911-5916`). Consistent.
- **Fatal path:** `on_sim_fatal` (lib.rs:399-420) sets `fatal` +
  `read_only` + bundle; `begin_frame`/`end_frame` still run (697/769) —
  only the step block is skipped. Consistent.
- **`life_support_enabled`:** sim default `false` (csp-sim `lib.rs:2073`) —
  enqueue-only-when-true asymmetry is consistent, not a bug. Confirmed.
- **wgpu:** `Cargo.toml:77` — `wgpu = "30"`. `device.poll(PollType::Poll)`
  unconditional at lib.rs:1101 with warn-only error path; the wasm no-op
  claim is plausible for wgpu 30 (not independently executable here).
- **Audio subprocess/cue tracker:** `drain_new` multiset matching by
  (text, ttl) at audio.rs:111-128 is correct under per-frame decay.

---

## Net assessment

No finding is dismissed. The two HIGH findings are real defects with the
exact causal chain described; Finding 1's mechanism is slightly *more*
broken than reported (the refusal-path `demote(0)` is a no-op at
`effective=0` and never even clears the promotion streak), and Finding 2's
"~×1000" understates the post-stop drain rate (~×64,800). Findings 3–8
match source line-for-line, including the phantom-store-entry and
staged-vs-live-keymap subtleties.
