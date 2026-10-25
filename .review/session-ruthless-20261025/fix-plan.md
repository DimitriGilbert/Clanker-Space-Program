# Fix Plan — Clanker-Space-Program Ruthless Audit (session-ruthless-20261025)

> Orchestrator-written (both dispatched fix-plan agents were killed by provider network failures).
> Synthesized from 22 verified reports: **145 CONFIRMED findings / 1 DISMISSED / 9 STALE-BY-CONCURRENT-FIX**.
> Severity mix among confirmed: 2 CRITICAL, ~20 HIGH, ~70 MEDIUM, remainder LOW/nits.

## Overview

The codebase has a genuinely rigorous determinism core (hashing, RNG, command log, rollback, orbit
math, server security) that survived adversarial re-derivation — and, around it, dead headline
features, world-bricking edge paths, decorative quality gates, broken editor verbs, and
integration-failure graphics. This plan attacks by risk: save/world integrity first, then dead
features, UX criticals, graphics, determinism gates, then the MEDIUM/LOW tail.

## Prerequisites
- Pinned toolchain (rust-toolchain.toml 1.97.x), `magick` installed for visual checks, GPU or lavapipe for render gates.

## Gatekeeping Commands
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`  (budget ≥15 min; `warp_x1_vs_x1e5_equivalence_10_sim_days` alone ~7 min in debug)
- `cargo check --workspace --exclude csp-server --exclude csp-bench --exclude xtask --target wasm32-unknown-unknown`
- `cargo run -p xtask -- lint-deps && cargo run -p xtask -- lint-attribution`

---

## Phase 1: Save/world-integrity & crash blockers
**Type**: Parallel (3 tracks, disjoint files)
**Dependencies**: None

**Requirements**:
- [C4a-F1, empirically reproduced] `fire_event` SoiTransition arm (csp-sim/src/lib.rs:7305-7318): guard `schedule_next_soi` behind vessel-still-exists (mirror apply_to_rails_at's re-check). Add the verifier's repro (root body `soi_m: Some(8.4e7)`, debris on escape hyperbola, assert clock advances past the event) as a permanent regression test.
- [C16-F1 CRITICAL] csp-script/src/lib.rs:404: depth-cap `preprocess` recursion (~64); [C16-F2 HIGH] `RhaiHost::new`: add `set_max_string_size`/`set_max_array_size`/`set_max_map_size` (rhai 1.27.0 defaults are None → OOM in ~34 ops).
- [C2-F2] csp-data/src/biome.rs:235: `decompress_to_vec_zlib_with_limit(&idat, stride*h)` (pinned miniz_oxide 0.9.1 has it); [C2-F3] load.rs gather: refuse symlinks like datahash::collect does.
- [C12-F1 + C19bake-F4] art.rs image branch: cross-check `data.len()` vs w/h/layers/mips before slicing (mesh branch already does); consider v3 header w/ raw_len+CRC32. Closes the corrupt-pack startup panic.
- [C15-rest-F2] csp-app/src/settings_io.rs:408: tmp-file + rename for settings writes.

**Inputs**: verified-report-4a.md, -16.md, -2.md, -12.md, -19bake.md, -15.md
**Outputs**: crates/csp-sim/src/lib.rs, crates/csp-script/src/lib.rs, crates/csp-data/src/{biome.rs,load.rs}, crates/csp-view/src/art.rs, xtask/src/bin/bake_art.rs, crates/csp-app/src/settings_io.rs
**Validation**: new regression tests above; full `cargo test --workspace`; clippy/fmt gates.

## Phase 2: Dead headline features — wire or remove (decision required per item)
**Type**: Sequential (design decisions first, then implementation)
**Dependencies**: Phase 1

**Requirements**:
- [C15-rest-F1 HIGH, independently re-derived] csp-app/src/warp.rs:143-146: fix the 60× unit error (compare ms-per-second against the per-second share of frame budget); fix warp tests that feed 5 ns mislabeled as 5 µs (they only pass because inputs are 1000× off); amend PLAN §2.6.3. Without this, warp >×1 is unreachable for any real vessel.
- [C7-F1 HIGH, probe-confirmed] Tech tree gates nothing (zero production callers of `is_part_unlocked`; no client-side gating either; upgrades cannot reach a craft). DECISION: wire part-identity into CraftDef + enforce at SpawnPhysicalVessel/Launch + filter VAB palette, or strip the module and its PLAN claims. No middle ground.
- [C2-F1 HIGH] Wire semantic validation into production startup (View::from_source) or a startup `validate` xtask step invoked by CI + app boot; fix the two divergent duplicate-ID tie-breaks (by_id first-wins vs set_sites last-wins).

**Inputs**: verified-report-15.md, -7.md, -2.md
**Outputs**: crates/csp-app/src/warp.rs, csp-sim tech wiring (per decision), csp-view/src/lib.rs or xtask
**Validation**: warp test with honest inputs passes and 200-part scene reaches ×2..×10 on reference log numbers; a manufactured lock-check test for tech; invalid data set refused loudly at startup.

## Phase 3: UX criticals (VAB verbs + input)
**Type**: Parallel (2 tracks)
**Dependencies**: None (independent of 1-2)

**Requirements**:
- [C14-F1 CRITICAL, egui-source-verified] csp-ui/src/vab.rs:1937: `let typing = ctx.text_edit_focused();` (matches csp-app lib.rs:2269) — resurrects dead Del/Esc/X hotkeys.
- [C14-F2 HIGH] Raw-pointer masking: hosts must return their window rects into `vab_pointer_input` (top bar, tutorial card, help, settings, crash dialog are currently unmasked → click-through subtree deletes). DO NOT use `ctx.is_pointer_over_area(pos)` — it does not exist in egui 0.37 (the original author's comment cites it; that's likely why masking is incomplete).
- [C14-F4] Esc default binding collision with cancel: rebind OpenSettings (e.g. F2) or route Esc contextually.
- [C15-lib2-F1/F2] focus_lost clears map_dragging/lmb_press; throttle hold rebases (or drops with notice) when flight focus changes.
- [C15-lib2-F3] Text-edit gate exempts non-`Key::Character` keys so Esc works while typing.

**Inputs**: verified-report-14.md, -15.md
**Outputs**: crates/csp-ui/src/{vab.rs,settings.rs}, crates/csp-app/src/lib.rs
**Validation**: new UI tests: hotkey paths reachable with `text_edit_focused` true/false; pointer-mask rect test enumerating all overlaid windows; focus-loss state-clearing test.

## Phase 4: Graphics integration quality
**Type**: Parallel (renderer / app-web tracks)
**Dependencies**: None

**Requirements**:
- [C11-F1 HIGH] Anti-aliasing: FXAA post pass or 4×MSAA across all four pipelines (terrain.rs:918, flight.rs:134, vab.rs:166, map.rs:119) — cite verified-report-11.
- [C11-F2] Craft lighting atmosphere coupling: tint PartLight.sun_col by terrain transmittance per frame (flight.rs:2407 hard-coded [0.95,0.92,0.85]).
- [C11-F3] Ocean after flight stream, depth-test-only (dir0_kind.w discriminator already exists).
- [C11-F4/F5] Aerial-perspective jitter+steps; on_uncaptured_error/device-lost handler (log-once + surface reinit, not panic/hang).
- [C19bake-F1/F2] Normal-map mips renormalized + rounding (not floor); delete or wire the dead `means` field and fix both lying doc comments (terrain.rs:534-535, bake_art.rs:10-11).
- [Vision live findings] VAB editor environment (floor/grid/lighting — 97.8% black void + placeholder mint disc); contact shadow/grounding for the pad scene; wasm demo: ship egui `default_fonts` and add a no-WebGPU fallback page (currently a silent black page — capture-log.md evidence).
- [C19bake-F3] bake_art: assemble-then-atomic-write both packs (no mixed-generation embeds).

**Inputs**: verified-report-11.md, -19bake.md, -vision.md, capture-log.md
**Outputs**: crates/csp-render/src/*, crates/csp-view/src/flight.rs, xtask bake_art, csp-app wasm entry
**Validation**: render gate extended with a distinguishing assertion (washed-out control frame must FAIL — verified-report-13-F1 proved the current gate passes the historical bug shape); fresh screenshots of flight/VAB/map/web reviewed before/after.

## Phase 5: Determinism & quality gates that currently gate nothing
**Type**: Sequential
**Dependencies**: Phase 1

**Requirements**:
- [C18-F1 HIGH] Verify the flight-launch golden: workspace test iterating scenarios × verify_golden (or CI loop over `csp-replay list`); pin the count. It was re-blessed for schema v20 and still ships unverified.
- [C18-F2 HIGH] Perf budgets: release-mode CI job; implement the >10% regression diff vs last committed log; in debug assert finiteness + print (never a 400× bound that reads like the budget).
- [C18-F7 / C19-lints-F1] Refusal inside `replay::write_golden` (both writers inherit: xtask bless + csp-replay record).
- [C18-F4] Add rhai to TRACKED_DEPS fingerprint.
- [C18-F6] Reject zero-goal scenarios in validate().
- [C13-F1/F2] Render gate: distinguishing pixel/packet assertions + required-mode or loud-skip signal (CI currently has no lavapipe install step — the gate has likely never run in CI).
- [C13-F3] Replace the `reason().starts_with("to-bubble")` branch with a typed reject kind (also fix the live second site lib.rs:5621 found by the verifier).

**Inputs**: verified-report-18.md, -13.md, -16.md
**Outputs**: crates/csp-bench/**, .github/workflows/ci.yml, crates/csp-view/tests/render_gate.rs, csp-sim/src/lib.rs, csp-core/src/error.rs
**Validation**: red-pill tests (mutate a golden hash → CI fails; remove lavapipe → gate loud-skips; zero-goal ron rejected).

## Phase 6: Sim/economy correctness MEDIUMs (batched by file)
**Type**: Parallel by file group
**Dependencies**: Phase 1 (share csp-sim/src/lib.rs)

**Requirements** (each cites its verified report):
- csp-sim/src/lib.rs: aero staleness re-check per tick (C4a-F2 — partition-dependent physics); autopilot driver check at apply (C4a-F3); spawn SOI residence (C4a-OO1); StagePlan per-vessel keying (C13-F4).
- reliability.rs: θ clamp `u.max(f64::MIN_POSITIVE)` at all three `-log` sites + delete the false "handled everywhere" comment (C1-F1; C8-F2 confirms both renewal sites); burn-credit emission on all burn-exit paths (C8-F1 — every standard executor burn completion loses credit); qualification-curve full gate in resolve (C9-resolve-F1).
- maneuver/latch: stop destroying Burning nodes on unlatch or make docs/restore honest (C5-transfers-F1 HIGH).
- contracts.rs: advance offer_index past failing draws (C6-F1 wedge — institutionalize the verifier's repro); cap/faucet decision (C6-F2 + C7-F4); colony_live gate on ColonyDelivery (C5-transfers-F2).
- colonies.rs: converter output headroom check or explicit refusal event (C6-F3).
- save.rs: honest v1 arm (real frozen V1 types or refuse with a true message) (C8-F3).
- joints.rs: dock-merge reliability preservation (C8-F4).
- orbit.rs/lib.rs: root-exit/grandparent asymmetry + §2.9 dead-code decision (C5a-F1, cross-confirmed C4a).
- transfer.rs: per-cell Lambert error → "no leg" not scan abort; max_revs cap ≤15; long-way pocket fix at z < −5.04e5 (C5-transfer; 24% of a broad grid fails Long today — add the sweep as a test).
- transfers.rs: duplicate-tank-spec rejection in check_tank_masses (C5-transfers-F3).
- csp-phys/session.rs: surf-node def-local storage + scale application (C9-session-F1 HIGH); from_file single-pass cycle check (C9-session-F2).
- csp-phys/simple.rs: midpoint gyroscopic term (C9-simple1-F1 HIGH — energy pump ×e^11/hour); angular clamp σ by ‖I⁻¹(r×nσ)‖ + axis-angle step (C9-simple2-F1, downgraded but real); speculative contact margin for tunneling (C9-simple1-F2); hoist per-body facet planes (C9-simple1-F3).

**Inputs**: verified reports 4a, 5, 6, 7, 8, 9, 13
**Outputs**: crates/csp-sim/src/*, crates/csp-phys/src/*
**Validation**: per-fix unit tests from the verified repros; `cargo test --workspace` (note: any hash-affecting change requires HASH_SCHEMA_VERSION bump + bless-goldens on a clean tree — sequence AFTER Phase 5's flight-launch verification lands so the golden actually protects it).

## Phase 7: Perf landmines & resource caps
**Type**: Parallel
**Dependencies**: None

**Requirements**: map polyline epsilon-quantized cache key (C12-F2 — epoch makes hits structurally impossible, ~8.4k Kepler solves/frame at debris cap); live-vessel per-part cache reuse (C12-F3); shadow anchor vs camera clamp (C12-F4); heightfield bake off the presentation thread (C12-F5); art.rs Box::leak → owned String (C12-F6); rollback per-item undo instead of full-arena clones (C4a-F5); aero staleness hash caching (C4a-F4); csp-server session.save cap + list_saves metadata-only index (C17-F1); telemetry graph.tsx loop-based min/max + cap (C21-F1 HIGH) + rAF-coalesced renders (C21-F2).

**Inputs**: verified-report-12.md, -4a.md, -17.md, -21.md
**Outputs**: csp-view/src/{map.rs,flight.rs,terrain.rs,art.rs}, csp-sim/src/lib.rs, csp-server/src/backend.rs, apps/telemetry/src/lib/widgets/graph.tsx, apps/telemetry/src/routes/index.tsx
**Validation**: perf rows before/after on the committed log scale; a breakup-scenario map-frame budget test; long-running telemetry soak ≥1 h without blank/freeze.

## Phase 8: LOWs, doc lies, and hygiene
**Type**: Parallel (batch by file)
**Dependencies**: Phases 1-7

**Requirements**: every remaining confirmed LOW/nit from verified reports 1,2,3,4b,7,8,10,11,13,14,15,16,17,19bake,21 — notably: false comments (reliability "handled everywhere", vab "mirrored by", terrain means doc, bake format doc, C20 stale docs), VAB tone-map parity, sRGB fallback warning, csp-replay docs, PROTOCOL.md tool list, VALID_RATES dead export, bugreport double-hash, logring filter gate, Uniform01 dedup, changelog order, propellant +Inf inversion, orbit geometry validator gaps (C3-F3), variant stat-path validation (C3-F1), loc-key validators (C3-F2), datahash case-folding (C3-F7).

## Phase 9: Stale-visual backlog (verify-then-close)
**Type**: Sequential
**Dependencies**: Phase 4

**Requirements**: the 9 STALE-BY-CONCURRENT-FIX vision findings (daylight stars, pole pinch ×2, terrain washout, VAB part shading ×2, etc.) were real at capture but the fix agent's commits 915e356/1e8c6da claim to address them. Take FRESH captures (`cargo test -p csp-view --test render_gate dump|dump_sky -- --ignored --nocapture` + the new render_dump_extra map/vab dumpers + web demo), compare against vision-report.md expectations, then close each item or re-file it as live with pixel evidence.

**Inputs**: verified-report-vision.md, capture-log.md
**Outputs**: fresh PNGs under .review/*/screenshots-verify2/, closure notes
**Validation**: per-image verdicts recorded; any re-filed item enters Phase 4 scope.

---

## Success Criteria
- All 145 confirmed findings addressed or explicitly deferred with written rationale.
- All gatekeeping commands green; flight-launch golden verified in CI; perf budgets numerically enforced in a release job.
- New regression tests present for every empirically reproduced bug (SOI loop, Lambert sweep, warp inputs, contract wedge, zero-goal eval, V8-spread-equivalent).
- Fresh visual verification pass with no plausibility-breaking defects remaining in flight/VAB/map/web scenes.

## Known coverage gaps (disclosed)
- `xtask/src/lint_deps.rs` internals, `lint_attr.rs`, `gen_biome_maps.rs`, `main.rs` build-web atomicity: 8 dispatched agents died to provider network failures; only the record-laundering finding (de-facto verified via C18-F7) is covered. Recommend a follow-up review when the provider stabilizes.
- egui UI panels could not be screenshotted headless (no Xvfb); UI judgments are code-level, plus the wasm-demo capture.
