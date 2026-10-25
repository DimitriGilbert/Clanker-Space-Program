# Cluster 13 Review — csp-view (headless shell, VAB, demo, tests incl. render_gate)
# Reviewer: ruthless, static-only (no cargo)
# Date: 2026-10-25


### [SEVERITY: MEDIUM] Finding 1: The render gate's pixel assertions cannot catch a washed-out or mis-shaded frame — all distinguishing power is packet-level
**File**: /home/didi/workspace/Clanker-Space-Program/crates/csp-view/tests/render_gate.rs:[313-324]
**Problem**: The pixel half of `boot_scene_has_real_geometry` asserts only `non_clear/total > 0.5`, `distinct > 8`, `max_spread > 30`. `distinct` counts 4-bit-per-channel buckets; the module's own doc (lines 86-88) says a two-stop *gradient* already yields "a few dozen". The negative control proves the point: the fallback sky alone rasterizes >0.9 non-clear (line 371-377) with plenty of buckets. So a seeded scene whose terrain shader regressed to flat albedo, whose lighting blew out to near-white, or whose craft is depth-culled/off-screen still passes: sky gradient + any non-uniform terrain easily exceeds 8 buckets and a 30/255 spread.
**Evidence**: Negative control `empty_flight_scene_is_what_the_gate_rejects` passes `> 0.9 non_clear` on the *bug-shaped* scene with only a 6-vertex gradient — i.e. the pixel stats would pass on the empty scene too; only the structural asserts (`terrain.is_some()`, `vertices > 6`) distinguish. Nothing pins that the craft is visible: `packet.vertices.len() > 6` counts emitted vertices, not rasterized coverage.
**Impact**: The gate reliably catches "nothing drew" (its original bug) and "no terrain extracted", but a visual regression that keeps any variation — washed-out exposure, dead lighting, uniform biome color, invisible craft — ships green. That is exactly the regression class an owner expects a "render gate" to catch.
**Suggestion**: Add region-aware statistics: e.g. require the lower-half (terrain) region's distinct-bucket count to exceed a floor and to exceed the upper-half (sky) region's, or require a horizontal-band variance profile; and/or compute the craft's projected screen footprint from the packet's already-projected vertices and assert a minimum covered-pixel count.

### [SEVERITY: MEDIUM] Finding 2: The render gate silently self-disables on GPU-less hosts — a skip is indistinguishable from a pass
**File**: /home/didi/workspace/Clanker-Space-Program/crates/csp-view/tests/render_gate.rs:[55-74, 262-265, 336-339]
**Problem**: `headless_device()` returns `None` when no adapter exists; both gate tests then `eprintln!(...) ; return` — which is a test **pass**. There is no exit-code or CI-visible difference between "gate ran and passed" and "gate never ran".
**Evidence**: `let Some((device, queue)) = headless_device() else { eprintln!("render-gate: no headless GPU adapter (install lavapipe) — skipping"); return; };` (lines 262-265, repeated at 336-339). The module doc (lines 31-34) documents the skip as intended, but nothing anywhere detects the skip — a CI image that loses lavapipe (or a driver regression) turns the entire gate into a permanent no-op while staying green.
**Impact**: The single test guarding the shipped "uniform purple frame" class can silently stop executing; the owner has no signal. Given this gate exists precisely because packet-level tests could not see the framebuffer, an always-green-but-never-run gate recreates the original hole.
**Suggestion**: Honor an env var (e.g. `CSP_RENDER_GATE=required`) that panics on skip, and set it in CI; or fail (not skip) when `std::env::var("CI").is_ok()`; at minimum emit a machine-readable skip marker (sentinel file / mangled test name) the pipeline can count.

### [SEVERITY: MEDIUM] Finding 3: Focus-pin release branches on human-readable error text — violates SimError's own "never branch-determining" contract
**File**: /home/didi/workspace/Clanker-Space-Program/crates/csp-view/src/lib.rs:[1182-1186]
**Problem**: `View::sim_step` releases the recover-control pin by string-matching reject reasons:
```rust
let to_bubble_reject = report.rejected.iter().any(|r| {
    r.key.source_class == SourceClass::Ui
        && r.key.source_id == PLAYER_SOURCE_ID
        && r.error.reason().starts_with("to-bubble")
});
```
`SimError`'s payloads are documented in csp-core/src/error.rs:14-16 as "diagnostic text intended for logs, dialogs, and bug-report bundles only — they are never hashed, never serialized into the canonical stream, and never branch-determining." This is a control-flow branch on exactly that text, across a crate boundary, with no test pinning the prefix (grep finds "to-bubble" only in csp-sim/src/lib.rs:5601-5633 literals and this match).
**Evidence**: The five `SimError::Recoverable("to-bubble: …")` messages live in csp-sim (crates/csp-sim/src/lib.rs:5601-5616). Rewording any of them (e.g. "vessel not on rails — cannot enter bubble") compiles clean, passes every test, and silently disables the P04-F2 drain-reject release — the pin stays armed forever and hijacks focus on any later staging, the exact shipped bug this code was written to fix.
**Impact**: A cosmetic message edit in another crate silently resurrects a fixed focus-hijack bug. Fragile coupling of the worst kind: no compiler, no test, no contract.
**Suggestion**: Add a typed reject kind to the `AdvanceReport` reject record (e.g. a `RejectKind` enum or echoing the rejected `SimCommand` variant) and match on that; or at minimum hoist a shared `pub const TO_BUBBLE_PREFIX: &str` into csp-sim with a unit test asserting every ToBubble rejection reason starts with it, and use the const here.

### [SEVERITY: MEDIUM] Finding 4: A second launch silently discards the active stage plan — unfired stages become permanently unfireable
**File**: /home/didi/workspace/Clanker-Space-Program/crates/csp-view/src/lib.rs:[1868, 1886]
**Problem**: `launch_craft` ends with an unconditional `self.world.insert_resource(plan);`. `StagePlan` is a single resource; launching craft B while craft A's plan still holds unfired drop-sets replaces it outright. A's stages can never be fired afterwards, and the HUD's `stages_left`/`stages_total` now describe B.
**Evidence**: lib.rs:1868 `let plan = flight::StagePlan::for_launch(stage_plan, &self.sim, &craft);` … 1886 `self.world.insert_resource(plan);` — no check of the existing plan. This contradicts the design the same resource documents: flight.rs:600-610 ("The plan belongs to the vessel its launch spawned — NOT the flight focus, which a newer spawn … can take over without retargeting this craft's stages") plus the entire fingerprint/`min_id`/`ambiguous` binding machinery built so a plan never fires at the wrong vessel. That machinery is moot when the plan is simply deleted on the next launch. Note tests/stage_plan.rs:140-166 papers over this: it launches twice and then re-`insert_resource`s a plan for the *first* vessel — the production path never restores it.
**Impact**: Player launches a multi-stage craft, parks it in orbit (plan half-spent), builds and launches a second craft: the first craft's remaining stages are gone with no notice, and `fire_stage` presses now target only the second craft. Silent loss of player capability.
**Suggestion**: Keep the incumbent plan when it still has stages and its bound vessel is alive (replace only expended/gone/ambiguous plans), or store per-vessel plans; if single-plan is a deliberate v1 constraint, surface a notice ("staging plan superseded — N firings of <vessel> abandoned") instead of discarding silently, and document the constraint on `StagePlan`.

### [SEVERITY: LOW] Finding 5: `render_flight_scene_sized` half-uses its `w`/`h` parameters — latent readback corruption or validation error
**File**: /home/didi/workspace/Clanker-Space-Program/crates/csp-view/tests/render_gate.rs:[214-242]
**Problem**: The function takes `w`/`h` and sizes the row pitch from `w` (`row_bytes = w * 4`), but the buffer's height uses the constant `H` (line 218: `size: (row_bytes * H as usize)`), and `copy_texture_to_buffer`'s extent uses the constants `W`/`H` (lines 237-241: `width: W, height: H`).
**Evidence**: Only `render_flight_scene` calls it, always with `(W, H)` — so it works today. Any future caller passing other dims (the function exists precisely to be sized) gets: `w < W` → rows pitched at `w*4` but a `W`-wide copy (wgpu validation error at best, interleaved garbage at worst); `h != H` → buffer over/under-allocation.
**Impact**: Latent trap in the very harness meant to be extended for multi-resolution gate checks.
**Suggestion**: Use `w`/`h` consistently in the buffer size (`row_bytes * h as usize`) and the copy extent (`width: w, height: h`), and drop the constants from those two spots.

### [SEVERITY: LOW] Finding 6: A failed craft-save serialization is dropped with no feedback — the only silent-swallow on a user persistence action
**File**: /home/didi/workspace/Clanker-Space-Program/crates/csp-view/src/lib.rs:[477-481]
**Problem**: `outbox.craft_save = if let Some(name) = vab_out.save_name { vab.0.to_bytes().ok().map(|bytes| (name, bytes)) } else { None };` — `to_bytes()` failing yields `None`, byte-identical to "no save was requested". Nothing is written, no notice lands anywhere. `CraftSession::to_bytes` (csp-phys/src/session.rs:1929-1950) can return `CraftFileError::Encode`.
**Evidence**: Every neighboring refusal path surfaces its reason: `VabSession::apply` writes `self.notice` on every Err (csp-ui/src/vab.rs:746-757), `launch_refused` posts a VabEvent::Notice *and* returns Err (lib.rs:2076-2080). This is the one user-initiated action whose failure is invisible. Likelihood is low (bincode over plain data is near-infallible), which is also why it will never be noticed when it does happen.
**Impact**: A Save click that silently does nothing — the player cannot tell "saved" from "failed", and the developer gets no diagnostic.
**Suggestion**: On `Err(e)`, keep the save request out of the outbox but push the reason onto the session's notice row (`vab.0.set_notice(Some(format!("save failed: {e}")))`) or add an error variant to the outbox field so `csp-app` can show it.

## Verdict (5 lines)
1. lib.rs's frame loop, schedule wiring, and §2.5 conformance are genuinely solid — staging-before-schedules, single-threaded SimStep/Sync, and the focus-pin lifecycle (P04-F2/F3, P05-F1) are carefully reasoned and well tested.
2. The VAB bridge is honest: session ops always land a notice, pick readbacks are epoch-guarded end-to-end, and `vab_present`'s closed-mode gating leaks nothing renderable; PartTransform being rigid makes the hover-pick's cross-part t-comparison valid.
3. The render gate is real but narrower than its reputation: it catches "nothing rasterized" and "no terrain extracted", NOT washed-out/mis-shaded/invisible-craft frames — and on GPU-less hosts it silently turns into an always-green no-op.
4. The sharpest code smell is control flow keyed on another crate's diagnostic strings (SimError prefixes), explicitly forbidden by that type's own docs, with zero tests pinning the contract.
5. Request changes: the two gate findings (1, 2) and the string-prefix branch (3) should land before this is trusted as the anti-"purple frame" guarantee; 4-6 are real but small.
