# Review Report 15 — lib.rs front half (lines 1–1700)

Reviewer: ruthless-15-lib1 | Date: 2026-10-25 | Scope: crates/csp-app/src/lib.rs:1-1700 | STATIC ONLY

---

### [SEVERITY: MEDIUM] Finding 1: wasm deferred GPU init permanently drops a resize that lands during the init window — stale surface, stretched scene, misaligned UI clicks
**File**: crates/csp-app/src/lib.rs:714-745 (with 673-682, 876-886)
**Problem**: On wasm, `init_graphics` (lib.rs:673-682) spawns `Gpu::new` with `window.inner_size()` captured at spawn time. A few event turns later `poll_gpu_init` → `finish_gpu_init` sizes *everything* from the spawn-time value: `VabRenderer::new`/`FlightRenderer::new` at `(gpu.config.width, gpu.config.height)` (lib.rs:736-745). A `WindowEvent::Resized` that arrives in that window finds `self.gpu == None`, so the handler's `if let Some(gpu)` guard skips both `gpu.resize` and the renderer resizes — the new size is recorded only into `settings.graphics.window_size`. Nothing replays it: `finish_gpu_init` never re-reads `window.inner_size()`, and no deferred-size field exists.
**Evidence**: The code itself acknowledges the event class — lib.rs:876-886: "re-fed every frame from `gpu.config` so a `Resized` swallowed by the deferred wasm GPU init (or a backend that never sends one) can't wedge an aspect." But re-feeding the viewport from a *stale* `gpu.config` only keeps the view internally consistent with the stale surface; it does not restore the true window size. The surface keeps presenting at the spawn-time extent, the browser CSS-stretches the canvas, egui hit-testing runs in the new logical size while the image is stretched, and the only recovery is the user manually resizing the window again (which finally finds `gpu` set).
**Impact**: Web demo: a canvas reflow during load (CSS/fonts landing, scrollbar appearing, mobile rotation in the first seconds) leaves the scene permanently distorted and egui widgets visibly offset from their click targets until the player resizes the page.
**Suggestion**: In `finish_gpu_init` (in range), reconcile once before first use: read `window.inner_size()` and, when it differs from `gpu.config`, call `gpu.resize(w, h)`, `flight_renderer.resize(&gpu.device, w, h)`, `vab_renderer.resize(&gpu.device, w, h)`. (Equivalently: store the latest non-zero Resized in a `pending_resize` field set by the handler when `gpu` is None, and apply it here.) This is a one-shot, wasm-only fix; the native path is synchronous and unaffected.

### [SEVERITY: MEDIUM] Finding 2: unthrottled redraw loop while occluded/minimized — full sim + egui frame cost burned every vsync for zero presented pixels
**File**: crates/csp-app/src/lib.rs:774-791 (with 1235-1247, and about_to_wait at 2338-2347 out of half-scope)
**Problem**: `render()` requests a redraw unconditionally (lib.rs:790, and again at lib.rs:1430 inside `paint`, plus `about_to_wait`), so the loop never idles. When the surface is occluded or the window minimized, `gpu.begin_frame()` returns `FrameResult::Skip` (lib.rs:1246 — "Transient (timeout/occluded)") and `paint` returns — but only *after* `run_frame` already ran the entire frame: warp decide + `AdvanceTo`, the full `sim_step`, egui `end_frame` (UiBuild + tessellation inputs), and texture delta application (lib.rs:1202-1224). `WindowEvent::Occluded` is not handled anywhere, so the app cannot distinguish "present" from "present-and-discard".
**Evidence**: lib.rs:788-790: `self.run_frame(&window, dt_wall); self.paint(&window); window.request_redraw();` — no occlusion gate; lib.rs:1245-1246 returns `Skip` without touching the redraw schedule.
**Impact**: With the window fully covered or minimized, the app pegs one core (sim step + full egui rebuild each vsync) while presenting nothing — fan noise / battery drain on laptops; on wasm the RAF loop likewise burns the tab. The sim continuing while hidden is defensible design; burning a full present-rate frame for pixels nobody sees is not.
**Suggestion**: Track `WindowEvent::Occluded(bool)` in `App`. While occluded, either (a) stop requesting redraws and accept a paused sim (KSP-style pause-when-hidden), or (b) keep stepping via `about_to_wait` on a throttled timer (e.g. 10 Hz) without `request_redraw`, skipping the egui/paint half. Minimum honest fix: skip `run_frame`'s egui-pass work when the last `begin_frame` reported occluded.

### [SEVERITY: LOW] Finding 3: Reconfigure recovery reuses stale `gpu.config` dims though the live `window` is in scope
**File**: crates/csp-app/src/lib.rs:1237-1244
**Problem**: On `FrameResult::Reconfigure` (surface `Lost`/`Outdated`), the handler reconfigures with the *old* config size: `let (w, h) = (gpu.config.width, gpu.config.height); gpu.resize(w, h);` — `csp_render::Gpu::resize` (csp-render/src/lib.rs:187-192) just re-configures with the passed dims. For a surface lost *because the window changed size* and whose `Resized` event is still queued behind this redraw, the frame is skipped and recovery only happens when that `Resized` lands; for backends that report `Outdated` without a size change the same-dims reconfigure is correct. `paint(&mut self, window: &Window)` has the authoritative size in hand and doesn't use it.
**Evidence**: lib.rs:1238-1243 vs the signature at lib.rs:1184.
**Impact**: One-or-more-frame black window after resize races on some backends (Linux/Wayland mode switches, Windows sleep/resume); in the worst ordering the surface stays `Outdated` until the next resize event, since configuring with stale dims cannot satisfy a size-changed swapchain.
**Suggestion**: Use `window.inner_size()` for the reconfigure: `let size = window.inner_size(); gpu.resize(size.width, size.height);` and resize the flight/vab depth targets alongside (as the `Resized` handler does), so this path is self-sufficient instead of depending on a `Resized` event to follow.

---
## Out-of-scope follow-ups (max 3)
- lib.rs:790 + 1430 + 2344-2346: three `request_redraw` sites per cycle — coalesced by winit, harmless, but only one is needed.
- lib.rs:321/175: `craft_dir` is CWD-relative (`crafts/`) while settings use `$XDG_CONFIG_HOME` — installed/launched-from-elsewhere binaries scatter or fail craft writes (documented as intentional; revisit before packaging).
- lib.rs:606 + 619: `collect_bug_report` computes `data_set_hash` twice on the fatal path (full data/ re-walk) — crash-path only, cosmetic cost.

## Verdict (front half, lines 1-1700)
The frame loop, warp plumbing, fatal/§2.13 path, pick readback epoch discipline, rebind capture, and held-key axes are unusually defensive and consistent with the PLAN shape they cite; no blockers or fake features found in range. Two medium robustness gaps remain at the lifecycle seams — the wasm deferred-init resize drop and the occlusion present-loop burn — plus one low-severity reconfigure nit. Fix Finding 1 before the next web-demo build; 2 and 3 can batch.
**Half-verdict: APPROVE WITH COMMENTS (fix F1 on the web milestone; F2/F3 follow-up).**
