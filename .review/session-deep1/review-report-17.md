# Review Report — Cluster C17 (csp-app)

Scope: `crates/csp-app/src/{lib,main,warp,settings_io,bugreport,logring,audio,synth}.rs`,
read in full. Cross-checked against `csp-render/src/vab.rs` (pick protocol),
`csp-view/src/lib.rs`/`flight.rs` (sim_step, viewports, FlightSfx decay),
`csp-ui/src/vab.rs` (save-name origin), `csp-sim` (`MAX_TICKS_PER_FRAME`,
`rails_warp_refusal`, stale-AdvanceTo absorption), and wgpu 30.0.1's wasm
`Device::poll` (a no-op `Ok` — the unconditional poll in `paint` is safe).

Findings are ordered by severity.

---

### [HIGH] Finding 1: rails-refusal / Clamping demotion is defeated by re-promotion every ~30 frames

**File:** `crates/csp-app/src/lib.rs:731-744` + `crates/csp-app/src/warp.rs:142-181, 217-226`

**Problem:** `WarpController::demote(0)` drops `effective` to ×1 but deliberately
leaves `requested` at the player's rung (e.g. ×10⁵). The comment at warp.rs:211
claims "a persistent refusal re-demotes every frame it is checked, so it cannot
oscillate" — but `decide()` runs *before* the post-step refusal check every frame
and knows nothing about the refusal. With `effective = 0 < requested`, the
§2.6.3 promotion streak rebuilds immediately (`margined > next`), and after
`PROMOTE_FRAMES` (30) the controller promotes straight to the margined ceiling —
up to rung 6 — emitting `SetWarp(100_000)`. Because `decide()` runs before
`target_tick`, that same frame accumulates `dt × 100_000` into `scaled_nanos`
and enqueues an `AdvanceTo` ~28 sim-minutes ahead. Only *after* that step does
the refusal check fire and re-demote.

**Evidence:**
- `decide()` (warp.rs:142) recomputes `ladder_ceiling`/`promote_streak` from
  `requested` with no awareness of the refusal that demoted `effective`.
- The demote path (lib.rs:731-744) only runs post-step; the promote path
  (lib.rs:649-674) runs pre-step, so the promoted `SetWarp` + inflated
  `AdvanceTo` are already logged and drained before the refusal re-check.
- Per cycle: ~1.67 sim-ks injected per 0.5 s wall (at ×10⁵) ≈ sustained ~×3300
  effective warp while `rails_warp_refusal` (thrust / unsettled contact /
  atmosphere) is reporting `Some` — exactly the conditions §2.6.2 wrote the gate
  for. Same mechanics apply to the §2.6.5 `Clamping` demotion.

**Impact:** The §2.6.2 warp-safety gate is a one-frame pause, not a gate. A
vessel under thrust at requested ×10⁵ visibly sits at ×1, yet the sim keeps
leaping ~28 sim-minutes every half-second, forever, with a `SetWarp` toggle
pair logged each cycle.

**Suggestion:** Suppress promotion while a demotion cause is active — e.g. the
app holds a `refusal_active` latch: on `clamping || refusal.is_some()`, call a
new `warp.suppress()` that clears `promote_streak`/`promote_candidate` *and*
prevents `decide()` from promoting until the refusal clears (or simply demote
`requested` to 0 as well on refusal-driven demotions; a hysteresis rebuild
cannot happen with `requested == 0`). Rebase the accumulator at the same time
(see Finding 2).

---

### [HIGH] Finding 2: `scaled_nanos` backlog makes demotion/step-down non-immediate — the sim keeps advancing at max per-frame rate after "stop"

**File:** `crates/csp-app/src/warp.rs:196-204, 217-226`

**Problem:** `target_tick` accumulates `Σ dt_ns × multiplier` and returns
`scaled_ticks.max(current_tick)`. `AdvanceTo` targets are absolute and the sim
must honor every logged one (`step_frame` advances `min(target,
clock + MAX_TICKS_PER_FRAME)` each call). Nothing ever rebases `scaled_nanos`:
`demote`, `set_cap_multiplier`, and manual step-downs only reduce the *growth
rate*, never the outstanding lead.

At ×10⁵ the accumulator grows ~1667 sim-s per 60 fps frame while the sim can
reach at most `MAX_TICKS_PER_FRAME` = 69120 ticks = 1080 sim-s per frame — a
backlog growing ~590 sim-s/frame (~2250 sim-s/frame at 30 fps). One wall-minute
at ×10⁵ leaves ~2.1 Ms (~24 sim-days) of outstanding targets; stepping down to
×1 then keeps the sim stepping 69120 ticks *every frame* (~×1000 wall-effective
at 60 fps) for thousands of frames — while the HUD shows ×1.

**Evidence:**
- warp.rs:194's comment claims "`MAX_TICKS_PER_FRAME` back-pressure bounds any
  real overshoot" — it bounds ticks *per call*, not the outstanding absolute
  target; the overshoot is unbounded.
- warp.rs:203 `target.max(current_tick)` — the sim keeps chasing the stale
  accumulated target regardless of the new `effective`.
- `demote` (warp.rs:217) touches only `effective`, `promote_*` — never
  `scaled_nanos`.
- Verified sim-side: a lower later `AdvanceTo` is absorbed harmlessly ("a stale
  `AdvanceTo` behind the clock is absorbed", csp-sim lib.rs:9550 test), so
  rebasing is legal within the §7.6 log contract.

**Impact:** "Warp to 1" — manual or Clamping/refusal-driven — does not stop the
sim. After sustained high warp the player watches the clock race at max tick
rate for seconds-to-minutes of wall time at ×1 displayed warp, and the
§2.6.3 budget derate can't recover either (frames keep paying the 69120-tick
step cost while backlog drains). Combined with Finding 1, both safety paths
are degraded in exactly the scenarios they exist for.

**Suggestion:** On any demotion (and on player step-down), rebase
`scaled_nanos = clock_ticks × NANOS_PER_SEC / TICK_HZ` (i.e. call
`demote`/`set_cap_multiplier` with the current tick and reset the accumulator
so the next `AdvanceTo` ≈ clock). The log then records a smaller absolute
target, which step_frame absorbs as stale — deterministic and immediate.

---

### [MEDIUM] Finding 3: craft-save name is raw user text → path traversal out of `crafts/` + phantom store entries

**File:** `crates/csp-app/src/lib.rs:831-832, 1471-1477` (name origin:
`crates/csp-ui/src/vab.rs:978-980`, a `TextEdit::singleline`)

**Problem:** `out.craft_save = (name, bytes)` with `name` taken verbatim from a
user-editable egui text field. `save_craft_file` does
`dir.join(format!("{name}.cspcraft"))` with no sanitization. A name containing
`..` segments or an absolute path escapes `crafts/` (e.g. `../../x` →
`x.cspcraft` beside the config dir; `/tmp/x` on Unix ignores `dir` entirely).
A name containing `/` writes into a non-existent subdirectory — the write
fails (only `dir` itself is `create_dir_all`'d), yet `craft_store` has already
been updated (lib.rs:831 inserts before the disk write) and `set_craft_files`
lists the name as a saved craft: a phantom entry that "loads" this session but
silently vanishes on next launch (`load_craft_store` keys by file stem and
never sees it).

**Impact:** The app's own UI lets a craft name write `.cspcraft` files outside
`crafts/` (limited blast radius — extension is appended — but real), and names
with separators produce save confirmations (`Notice("saved {name}")`) for files
that can never be re-read. Same trust boundary the read_only/demo gates police.

**Suggestion:** Validate `name` in `handle_ui_outbox` (or a `sanitize_craft_name`
helper): reject empty/`..`/path separators/control chars and overlong names
with a `VabEvent::Notice` refusal, mirroring the demo/read-only notices already
there.

---

### [MEDIUM] Finding 4: flight viewport is only set from `Resized` — stuck at (1,1) default on wasm and on any platform without an initial `Resized`

**File:** `crates/csp-app/src/lib.rs:688-693` (map/vab set per-frame) vs
`lib.rs:1248-1252` (flight set only in `Resized`, and only when
`self.gpu.is_some() && self.flight_renderer.is_some()`); default `(1.0, 1.0)`
at `crates/csp-view/src/flight.rs:84`.

**Problem:** `set_map_viewport`/`set_vab_viewport` are refreshed every frame in
`run_frame` from `gpu.config`; `set_flight_viewport` is instead only written in
the `Resized` handler. `finish_gpu_init` never calls it. Two loss modes:
- wasm: `Resized` events that arrive while `gpu_rx` is still pending hit the
  `(&self.gpu, &mut self.flight_renderer)` guard and are dropped — the aspect
  stays 1:1 until the user manually resizes (the only re-set path).
- native: correctness relies on winit delivering an initial `Resized` after
  `init_graphics`; platforms that don't leave `viewport_px` at the `(1.0, 1.0)`
  default forever.

`viewport_px` feeds `solve_orbit_camera` (aspect) and `build_packet` (terrain
LOD) — a wrong value renders the flight scene stretched/compressed, exactly the
class of runtime-only bug this review targets (headless tests never see it).

**Suggestion:** Update `set_flight_viewport` in `run_frame` alongside the map
and vab calls (identical plumbing precedent), or call it in `finish_gpu_init`
with the live `gpu.config` size — both fix wasm and remove the platform
dependence.

---

### [LOW] Finding 5: a 0×0 `Resized` persists `window_size = [0,0]` → next launch opens a 1×1-px window

**File:** `crates/csp-app/src/lib.rs:1241-1247, 466-469`

**Problem:** `Resized` writes `size.width/height` straight into
`settings.graphics.window_size`; `persist_settings` on close then saves it.
`gpu.resize` correctly ignores 0×0 (minimize), but the settings write doesn't.
`create_window` floors to `max(1)` — so after a platform emits `Resized(0,0)`
(minimize/hide paths on some compositors), the next launch restores a
1×1-pixel window.

**Suggestion:** Skip `window_size` updates when either dimension is below a
sanity floor (e.g. `< 64`), or clamp at the `Resized` write site the same way
`create_window` clamps at read.

---

### [LOW] Finding 6: post-`Fatal`, `launch`/`maneuver` outbox requests queue silently forever — inconsistent with the read-only notices

**File:** `crates/csp-app/src/lib.rs:867-874` vs `823-830`; `View::launch_craft`
also calls `set_mode(ViewMode::Flight)` unconditionally (csp-view lib.rs:1354).

**Problem:** The read-only gate refuses `craft_save` with an honest
`VabEvent::Notice`, but `outbox.launch` still runs `launch_craft()` (enqueues a
`SpawnPhysicalVessel` on the never-drained queue *and* flips the view to Flight,
implying success), and `outbox.maneuver` still enqueues. On a halted session
these commands are stamped/logged at dequeue — which never happens — so the
player gets a silent no-op or a misleading mode switch.

**Suggestion:** Gate `launch` and `maneuver` on `!read_only` with the same
notice pattern (`"session is read-only"`); at minimum skip `launch_craft`'s
`set_mode` when halted.

---

### [LOW] Finding 7: rebind capture is behind the `!consumed` gate — a focused egui widget eats the key instead of binding it

**File:** `crates/csp-app/src/lib.rs:1260-1263, 1117-1131`

**Problem:** `key_press` (which hosts rebind capture) only runs when egui did
not consume the `KeyboardInput`. While the rebind target is armed, a press
aimed at a still-focused `TextEdit` (e.g. the craft-name field) goes to the
widget's text instead of becoming a binding — the capture stays armed and the
typed character lands in the field. After a successful bind, subsequent held-key
repeats fall through to normal dispatch under the *old* live keymap (the rebind
only stages), firing whatever action the key used to own.

**Suggestion:** Give `rebind_target.is_some()` priority over the `consumed`
guard (check it in `window_event` before `key_press`'s normal path), or accept
it as a documented quirk — but the "armed capture loses to a text field"
interaction is a plausible report from a user rebinding mid-session.

---

### [LOW] Finding 8: minor consistency items

- `lib.rs:806-814`: `outbox.mode` plays `ui_confirm`, but `dispatch`'s
  `ToggleMap`/`ToggleVab`/`OpenSettings`/`ToggleHelp` (lib.rs:1148-1206) perform
  the same class of session action silently — the acknowledge blip is
  inconsistent between panel clicks and key presses.
- `lib.rs:857-865` + `settings_io.rs:124`: on wasm `save_settings` is a no-op
  `Ok(())`, so Apply reports "settings saved" though nothing persists. Either
  return a distinguishable result or show "applied (session only)".
- `audio.rs:399-413`: `update()` drains cues and calls `play()` *before* the
  `!alive` early-return — a dead backend still enqueues up to
  `MAX_ACTIVE_SHOTS` into a mix nobody reads. Harmless but trivially reorderable.
- `lib.rs:1170-1182`: throttle commands use a bare `1` for `source_id` while
  the other producers use named constants (`WARP_SOURCE_ID`, `MANEUVER_SOURCE_ID`,
  `SETTINGS_SOURCE_ID`); `View::launch_craft` also emits on `1`. A named
  `FLIGHT_SOURCE_ID` would document the shared FIFO.

---

## Checks that came back clean

- Pick encode/decode is consistent: `R32Uint` pixel = 1-based `pick_ids` index,
  `checked_sub(1)` decode against the encode-time `pick_decode_ids` snapshot —
  the stale-table race is correctly handled, bounds-checked in `encode_pick`,
  and one-in-flight is enforced.
- Command ordering: `SetWarp` then `AdvanceTo` on `WARP_SOURCE_ID` lands
  level-first; demotion SetWarps enqueue post-step for next-frame drain as
  documented; `MANEUVER`/`SETTINGS`/`1` FIFOs are distinct producers.
- The just-fixed depth split is structurally right: scene pass attaches
  flight/vab depth only in those modes (Map `None`), egui renders in a second
  no-depth `LoadOp::Load` pass — and the renderer options are all `Some` or all
  `None` together, so no depth-mismatch path exists.
- Fatal path: halt stops the controller/commands but keeps `begin_frame`/
  `end_frame` alive for the crash dialog; `read_only` does gate craft saves;
  bundle collection is best-effort and records snapshot refusals.
- Audio: subprocess lifecycle is clean (spawn→writer→try_wait→kill/join/reap;
  inert-on-death), the cue multiset tracker is correct under per-frame ttl
  decay (which `flight_present` does every frame regardless of mode), the bank
  is bounded/deterministic, and `paplay`'s no-file stdin convention is honored.
- `device.poll(PollType::Poll)` on wasm returns `Ok` (no-op) in wgpu 30 — no
  divergence/spam.
- `life_support_enabled`: sim default is `false`, so the enqueue-only-when-true
  asymmetry in `apply_startup_difficulty` is consistent, not a bug.
- logring: tee init is idempotent-safe, `tail()` degrades to empty, panic-hook
  bundle is sim-free by contract. `stamp()`'s nanos+seq naming is collision-free.
