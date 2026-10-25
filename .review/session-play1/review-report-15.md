# Review Report — P15: New regression-test surface

**Scope:** `crates/csp-view/tests/{stage_plan,flight_focus,pad_spawn,part_silhouettes,render_gate,tutorial,vab_latency,demo,startup_colonies}.rs`, `crates/csp-app/src/lib.rs` test module (L2209–2753), `crates/csp-sim/src/lib.rs` fuel tests (L17259–17346).

**Method:** each test audited for (a) does it fail on the pre-fix behavior, (b) does it exercise the claimed path vs. a bypass, (c) can it run at all. Git history used to date every file/test against the playability commits (`f5e8177`, `d8cd46f`, `5529f5f`, `7ca6fa4`). All `csp-view` tests were executed: everything passes; `render_gate` ran on a real (lavapipe) adapter here — `seeded scene: non_clear=76800/76800 distinct=32 spread=127`, `empty scene: 76800/76800` — so the gate is live, not skipped, on this host.

**Verdict:** the new tests are *not* tautological — each pins observable state the pre-fix code could not produce, and `render_gate` even ships a negative control proving the positive gate isn't vacuous. The problem is the other direction: several of the riskiest *code* paths the fixes added have no test at all, and much of the listed "new" surface is pre-existing tests.

---

## Findings

### F1 — HIGH: Fuel pooled-coverage (the free-impulse bug class) is untested; both sim fuel tests are single-engine / single-propellant

- **File:** `crates/csp-sim/src/lib.rs` L17263 (`engine_burn_drains_propellant`), L17301 (`dry_engine_produces_no_thrust`); implementation at L7699–7872.
- **Problem:** `propellant_plans` exists specifically to pool demand: `coverage = min(1, available / Σ needs)` across *every claimant* (L7794–7802), because per-claimant coverage let two engines each read 100% of the same units — full impulse while the pooled debit clamped at `available` (the comment at L7711–7717 names this exact bug class). Both tests build `fueled_engine_craft` (L17244): one part, one `EngineSpec`, one propellant (`liquid_fuel`). With a single claimant, per-claimant and pooled coverage are **numerically identical** — a regression reverting to per-engine `avail/need` passes both tests.
- **Evidence:** `grep` confirms no test anywhere constructs a craft with two `engine` specs, an un-holdable propellant (`units = f64::INFINITY`, L7773–7775), or a multi-propellant engine. The exploration map's claim that the fuel tests "cover pooled-coverage + starvation (two co-claimant engines, un-holdable propellant, jettisoned tank mid-burn)" is not accurate — none of those three cases exists.
- **What IS covered (genuinely):** per-tick debit rate within ±2 ticks (L17290–17294 — tight); tank lands exactly 0.0, so no over-debit on the dry-out tick (L17318–17322); the `engine_can_draw` all-starved `SetThrottle` clamp (L17337); ballistic velocity bound after flameout (sane — circular orbit ⇒ d|v| ≈ 0 in 1 s). The `!any_spec` unmetered-craft convention is *incidentally* pinned by pre-existing `throttle_drives_thrust_and_clamps` (L12980) — `box_craft` has `engine: None` (L12323) yet throttle applies.
- **Impact:** the only hash-bearing, determinism-load-bearing cluster (P03) has its most subtle invariant — the shared denominator — unguarded. Untested paths: co-claimant split, un-holdable/massless propellant starvation, one-of-two-propellants starvation (min-over scale), jettisoned tank mid-burn (`apply_jettison_part` shrinks `tank_contents` at L6104–6111; next tick's plan should starve the engine), partial-coverage thrust taper, and post-debit `apply_fuel_scales` re-scale idempotency after the re-weld (L3635–3642).
- **Suggestion:** add a two-engine one-tank test asserting total debit == `available` on the dry-out tick (and each engine's scaled `rated_n`); a `fueled_engine_craft` variant spec'ing a resource no tank declares (expects thrust 0 / `SetThrottle` clamps); and a jettison-the-tank-during-burn test asserting the burn stalls.
- **Minor:** L17339 comment says "~800 km orbit" — the spawn is `TERRIN_R + 200_000` = 800 km *radius*, 200 km altitude.

### F2 — HIGH: Dead-stick recovery (`RecoverControl` → `ToBubble` + focus pin) has zero test coverage

- **File:** `crates/csp-view/src/lib.rs` L1500 (`recover_control`), L1552 (`to_bubble_eligible`), `crates/csp-sim/src/lib.rs` L5477 (`apply_to_bubble`), `crates/csp-view/src/flight.rs` L236–268 (`FlightFocusOverride`), `crates/csp-app/src/lib.rs` L1813–1823 (dispatch arm + read-only gate).
- **Problem:** `d8cd46f`'s commit message headlines "dead-stick fix", and `recover_control`'s only callers are production code — `grep` finds no test invoking `recover_control`, dispatching `KeyAction::RecoverControl`, or enqueueing `SimCommand::ToBubble` outside bench scenarios. `flight_focus.rs` pins only the newest-spawn focus rule (written pre-pin, Phase 14) — none of the pin's three release paths (staged-then-left, gone-from-arena, fresh launch — `flight.rs` L1093–1131, `lib.rs` end_frame pin release) is exercised, nor is the eligibility mirror that prevents pin-and-silent-reject.
- **Evidence:** `git log -S recover_control` → only `d8cd46f` (the feature commit itself).
- **Impact:** a headline shipped fix can regress silently — e.g., a stale pin retargeting focus/throttle, or `TakeControl{Player}` preempting a lease the sim meant to keep.
- **Suggestion:** an integration test in `tests/flight_focus.rs` (or new `recover_control.rs`): spawn → rails via natural drain or a scripted transfer → `view.recover_control()` → assert `ToBubble`+`TakeControl` land, focus pins the recovered vessel, pin releases on the next `launch_craft`, and an ineligible target yields a HUD cue without a queued command.

### F3 — MEDIUM-HIGH: AZERTY / physical-first key resolution is untested at the layer that implements it

- **File:** `crates/csp-app/src/lib.rs` L1337–1390 (`key_press`, `action_of`), `crates/csp-app/src/settings_io.rs` L228 (`keycode_token`), L330–428 (`event_binding`, `char_to_physical_token`, `migrate_char_bindings`, `layout_dependent_binding_count`, `event_label`), `crates/csp-app/src/lib.rs` L1633 (`drop_rebound_holds`), L1702+ (`intercept_rebind_key`/`captured_key`).
- **Problem:** the AZERTY fix's entire mechanism — physical `key-*` token tried before the logical token, on *both* press and release — has no test at the app layer. Every input test calls `app.dispatch(KeyAction)` directly, bypassing resolution: they prove "the action does the right thing", never "the right physical key produces the action". `keycode_token`'s KeyCode→`key-*` table, `migrate_char_bindings`' char→physical migration (the actual AZERTY fix), `intercept_rebind_key`'s swallow-until-release via `captured_key`, and `drop_rebound_holds` all have zero coverage.
- **Evidence:** `grep` of the test module (L2209–2753) for `captured_key|intercept|keyboard_event|key_press|action_of|physical_key` → nothing. `rebound_key_dispatches_its_new_action` (updated in `f5e8177`) asserts `action_for("key-m")` — a string lookup, not a `KeyEvent` resolution — and its rebind uses the *logical* `"m"`, so physical-vs-logical precedence is never exercised. `csp-ui` settings tests (L887–1025) do cover `Keymap` mechanics — `key-*` defaults for every action, `action_for("z") → None` vs `action_for("key-z") → ThrottleFull`, `sanitize`, eviction — so the data structure is tested; the *event→token* path is not.
- **Impact:** the symmetric press/release physical-first contract can break (e.g., release resolves logically first → wedged held verb) with no failing test — exactly the bug class `drop_rebound_holds` and the `!`-release note in P01 guard.
- **Suggestion:** construct synthetic `winit::event::KeyEvent`s (constructible in tests) or factor resolution into a testable `(physical_key, logical_key) -> Option<KeyAction>` fn; add a `migrate_char_bindings` unit test (a legacy `"z"`-bound ThrottleFull file → `key-z`, plus a collision case) and a press-physical/release-shifted-glyph symmetry test.

### F4 — MEDIUM: `StagePlan`'s lazy binding and `for_launch`/`stage_plan_of` capture path are never exercised; 4 of 6 refusal branches uncovered

- **File:** `crates/csp-view/tests/stage_plan.rs` L70–73, L151–152; machinery at `crates/csp-view/src/flight.rs` L464–516 (`for_launch`, `vessel_for`), `crates/csp-view/src/lib.rs` L1741, L1948 (`stage_plan_of`), L1788–1859 (`fire_stage`).
- **Problem:** both tests install `StagePlan::bound_to`, whose doc says "for tests and scripted producers" (L478) — it bypasses the entire new binding mechanism: `min_id` (`vessels().next_back()+1` at capture), `craft_fingerprint` matching, `.rev()` newest-match-first, and never-cache-`None`. The "plan fires its own vessel not the focus" test proves the *bound* vessel wins — but nothing proves the binding lands on the right vessel, which is where a real bug would live (e.g., an off-by-one `min_id`, or fingerprint drift after jettison mutating the bound craft). `stage_plan_of`'s decoupler→drop-set walk is also never exercised — the starter craft is single-stage, so `for_launch` capture runs in every test only to be immediately overwritten.
- **Evidence:** `grep "for_launch|stage_plan_of" crates/csp-view/tests/` → no hits. Uncovered `fire_stage` arms: `OnRails` → "NOT STAGED — VESSEL IS ON RAILS" (L1825–1827), unspawned (L1811–1815), gone-vessel plan retirement (L1829–1839), and the `total` honesty distinguishing expended vs single-stage (L1797–1801). Sim-side `apply_jettison_part` itself *is* covered (L12569, L18031, L18663, L19272) — the gap is the view layer only.
- **Impact:** the test proves the *policy* (bound ≠ focus, refuse-don't-eat) but not the *mechanism* that implements it; a binding bug (e.g., the second launch's plan binding the still-live first copy of the same craft — identical fingerprints) would pass.
- **Not tautological, to be clear:** the latched-refusal test fails on the old pop-unconditionally behavior three ways (return ≠ 0, `stages.len()` drops to 1, no "NOT STAGED" cue); the bound-vessel test fails on fire-at-focus because `JettisonPart` rejects a foreign node (`"jettison: unknown part node"`, sim L6028) so the bound vessel keeps its node.
- **Suggestion:** a test that launches a real multi-stage VAB craft (decoupler + drop set), lets the plan bind lazily, then fires — plus a two-launches-same-craft case asserting `min_id` keeps plan #2 bound to vessel #2.

### F5 — MEDIUM: `render_gate` passes vacuously on GPU-less CI (documented, still a coverage hole)

- **File:** `crates/csp-view/tests/render_gate.rs` L55–74 (`headless_device` → `None`), L251–254, L325–327 (early return + eprintln).
- **Problem:** when `request_adapter` yields nothing, both tests print a note to stderr and return → `cargo test` reports `ok` — indistinguishable from a real pass unless run with `--nocapture`. Verified live here (lavapipe adapter exists: `distinct=32 spread=127` seeded, full-frame gradient empty), so the gate genuinely works where a driver exists — but a GPU-less runner gets zero render coverage with no signal.
- **Quality notes otherwise:** the test design is strong — `analyze` computes the expected clear byte through the sRGB conversion rather than assuming it (L101–113), `empty_flight_scene_is_what_the_gate_rejects` is a real negative control pinning both the empty-packet shape *and* the `GreaterEqual` rasterization fix, and the draw order/depth-clear mirrors the app's scene pass.
- **Impact:** the only pixel-level regression gate in the suite can silently downgrade to a no-op — the exact condition (no observable framebuffer) that let the "uniform purple" build ship.
- **Suggestion:** mark the pair `#[ignore]` and run them via an explicit `cargo test -- --ignored` GPU job, or gate on an env var (`CSP_RENDER_GATE=require` → fail rather than skip when no adapter); at minimum emit the skip via a mechanism CI can count.

### F6 — LOW: Most of the listed "new" surface predates the playability track

- **Evidence (`git log`/`git show`):** `flight_focus.rs` — last touched `b06e8c2` (deep-review Phase 14); `part_silhouettes.rs` — `fd2158d` (Phase 13); `tutorial.rs` — `b06e8c2`; `vab_latency.rs` — `c55e276` (M3); `demo.rs` — `f5d21eb` (Phase 9); `startup_colonies.rs` — `2ec4128` (M8); `pad_spawn.rs::starter_craft_rests_on_the_pad` and `render_gate.rs`'s two tests — `4bc10bd`/`4eab706` (pre-track playability-adjacent fixes). Of the 13 app-module tests, only `held_throttle_ramps_then_holds_on_release` and `pad_attitude_press_notices_and_defers_until_liftoff` are new (`5529f5f`); `read_only_refuses_launch_and_maneuver` was *modified* (assertion moved from the VAB-only notice row to the `FlightSfx` "READ-ONLY" cue — a genuine improvement: the old assertion passed even when the refusal was invisible outside the editor).
- **Actually new this track:** `stage_plan.rs` (both tests, `f5e8177`), `launch_site_sits_on_rendered_ground` (`f5e8177`, +45), the render-gate camera-distance change (`f5e8177`, +9), the two app tests above (`5529f5f`), the two sim fuel tests (`d8cd46f`), plus the in-crate `in_bubble_vessel_appears_in_map_sync` (`d8cd46f`, map.rs L2102 — real vessel-on-map coverage the file list didn't credit).
- **Impact:** none of this makes the tests wrong — they're valid regression nets, and `tutorial.rs`'s ascent plus `demo.rs::demo_seed_craft_ascends` now *incidentally* provide the only end-to-end coverage of the fuel-burn path through real data (the craft cannot reach 70 km / +50 m if engines don't burn propellant). It does mean the "new regression surface" is thinner than the cluster list implies — useful when weighing the missing coverage in F1–F3.

### F7 — LOW: `part_silhouettes` covers shape class only — none of the pass-2 visual changes

- **File:** `crates/csp-view/tests/part_silhouettes.rs` (whole file); new code at `crates/csp-view/src/part_mesh.rs` + `flight.rs` livery/`kind_tint`/plume/sheath (`5529f5f`, +261 lines in part_mesh alone).
- **Problem:** the test predates `5529f5f` and asserts `PartShape::Lathe` classification, profile monotonicity, and triangle-count distinctness. The commit's actual visual work — smooth normals, per-part-kind livery tint, two-shell plume, sheath per-vertex facing alpha — has no assertion: a regression to flat single-color facets passes everything.
- **Impact:** low — the test does prove what it claims (silhouettes, not boxes); the gap is scope, not correctness. A vertex-normal-variance or per-part tint assertion would extend it to the shipped change.

### F8 — LOW: `launch_site_sits_on_rendered_ground` duplicates the bake constants and checks only the dominant body

- **File:** `crates/csp-view/tests/pad_spawn.rs` L127 (`HeightField::bake(body.seed, 1024, 512)`); constants at `crates/csp-view/src/terrain.rs` L30–31 (`HEIGHTFIELD_W/H`, private).
- **Problem:** the literals are correct today but not referenced — a heightfield-resolution change desyncs the test's mirror silently (it would measure a different surface than the pass draws). `sample_dir` ↔ WGSL `height_at` mirroring was verified faithful (terrain.rs L199–213 ≡ terrain.wgsl L44–58: same texel-center bilinear, wrap-u/clamp-v — body-fixed `dir` is correct because the shader rotates inertial→body-fixed before sampling). Only the dominant body's launch site is pinned.
- **Impact:** the test is genuine — it would have caught the shipped ~+57 m buried pad and still guards against site re-authoring or noise-seed changes — but it's a data-consistency pin more than a code regression test.
- **Suggestion:** export/use the shared constants; optionally iterate *all* `launch: true` sites.

### F9 — LOW: `vab_latency` release-mode hard assert is a REF-A number on arbitrary hardware

- **File:** `crates/csp-view/tests/vab_latency.rs` L159–164.
- **Problem:** `budget_ms` = 200 ms debug (passed: total run 0.65 s incl. 300-op build) / 16 ms release — a machine-specific kill criterion asserted on whatever CI runs `cargo test --release`. Worst-of-5 helps, and the comment documents the caveat honestly.
- **Suggestion:** make the release arm env-gated (`CSP_PERF_REF_A=1`) or comparative (regression ratio vs a baseline op), not absolute.

---

## Coverage map vs. the shipped bugs (the focus question)

| Shipped bug | Test that pins it | Fails on pre-fix? |
|---|---|---|
| Fuel drain / dry engine | `engine_burn_drains_propellant`, `dry_engine_produces_no_thrust` (sim L17263/17301); e2e incidentally via `tutorial.rs`, `demo_seed_craft_ascends` | **Yes** (zero-drain / non-empty tank fails) — but see F1: pooling, starvation shapes, jettison-mid-burn all untested |
| Vessel invisible on map | `map.rs::in_bubble_vessel_appears_in_map_sync` (L2102, in-crate) | Yes |
| Dead-stick recover | **None** — F2 | — |
| AZERTY physical keys | `keymap_default_covers_every_action`/`action_for_resolves_first_match` (csp-ui) cover the *map*; **nothing covers event→token resolution or migration** — F3 | Partially |
| Eaten stage on latched/rails; plan-at-focus | `stage_plan.rs` both tests — latched only; binding mechanism bypassed — F4 | Yes (for the two covered cases) |
| Pad spawn (12 m fall / buried pad) | `starter_craft_rests_on_the_pad` + `launch_site_sits_on_rendered_ground` | Yes |
| Purple screen (empty scene + Greater depth) | `render_gate.rs` both tests — real pixel evidence + negative control | Yes (where an adapter exists — F5) |
| Throttle on/off-only | `held_throttle_ramps_then_holds_on_release` | Yes (pre-fix logs 1 command, not 61) |
| Silent pad attitude | `pad_attitude_press_notices_and_defers_until_liftoff` | Yes (no cue, no deferral, no post-liftoff rate) |
| Read-only eaten launch/maneuver | `read_only_refuses_launch_and_maneuver` (cue assertion new in `5529f5f`) | Yes |

## Bottom line

No tautological tests found — every assertion audited pins state the old code couldn't produce, and the strongest pieces (render-gate negative control, pad-spawn sim/render split, throttle command-log assertion) are genuinely well-aimed. The deficit is *coverage of the fix mechanics*: the two most subtle new mechanisms — pooled fuel coverage and the stage-plan lazy bind — plus two headline features (dead-stick recover, AZERTY event resolution) ship with zero direct tests. Recommend F1 and F2 additions before this is called a regression net rather than a smoke suite.
