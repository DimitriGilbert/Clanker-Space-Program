# Review Report C23 — HUD, navball, settings, tutorial

Session `session-full-20261026` · reviewed the CURRENT tree (HEAD `7dd4b86`; the map's
"uncommitted" UX pass is now commits `ff897d4`+`7dd4b86`) · files: `crates/csp-ui/src/{lib,navball,settings,tutorial}.rs`, `crates/csp-ui/tests/hud_dump.rs`.

Method note: every movable-panel and tutorial claim below was verified with a throwaway
egui headless harness (press/move/release event scripts, `layer_id_at`/`interaction_snapshot`
probes, and a temporary `.selectable(false)` patch A/B); the harness was validated against a
baseline `egui::Window` drag (passes) and `WindowDrag::Anywhere` (passes). The scratch test was
deleted after verification; the tree is untouched.

---

### [HIGH] Finding 1: The movable readout block cannot be dragged by its own text — complaint #2 only half-fixed
**File**: `crates/csp-ui/src/lib.rs:753-756` (Area), labels at `lib.rs:785-810` (`pair`/`row`/`sect`), `:813-819` (ON PAD), `:762-775` (no-vessel), `:979-1009` (controls hint)
**Problem**: The new `.movable(true)` Area works mechanically, but egui 0.37's default-selectable
`ui.label` widgets sense click + drag-to-select, and they win the press gesture over the Area's
move widget wherever a label rect is under the pointer. The readout block is a wall of mono
labels; only the empty right margin and the ProgressBar rows accept the move-drag.
**Evidence** (headless, 960×600, full press→drag→release): dragging from 4 of 5 in-panel points
(`(100,200)`, `(200,300)`, `(308,360)`, `(60,500)` — all over row text) yields delta `(0,0)`;
`interaction_snapshot` shows the dragged id ≠ `flight_hud.with("move")` and the move widget not
even hovered. Only `(500,550)` (past the text, next to the bars) moves. Temporarily adding
`.selectable(false)` to the `pair`/`row`/`sect` labels made **every** point drag
(delta `(+140,−60)` at all starts), then the patch was reverted.
**Impact**: The player grabs the panel where the text is (the visually salient part) → nothing
moves → "still can't drag the panels". The navball Area (painter-only content) drags fine from
anywhere, and all `egui::Window` panels drag by title bar — so the flagship panel of complaint
#2 is the one that still feels broken.
**Suggestion**: Wrap the readout rows in `egui::Label::new(...).selectable(false)` (the
`notice_overlay` labels already do exactly this at `lib.rs:723`). One-line change per closure.
Optionally also give the Area `.sense(Sense::drag())` explicitly as belt-and-braces.

### [HIGH] Finding 2: Navball first-open parking is wrong at every window height — permanently floats 242 px too high (and overlaps the readout block on narrow windows)
**File**: `crates/csp-ui/src/lib.rs:1018-1024`
**Problem**: `default_pos(ball_center.x − 68, rect.bottom() − nav_size.y − 6)` is what the code
asks for, but on an Area's first show egui runs a sizing pass with the *nominal* area size
(`style.spacing.default_area_size = 600×400`) and constrains the position against it: since
`(H−158)+400 > H` is always true, the ball is clamped to `y = H−400` (and `x = min(W/2−68, W−600)`),
then stays there because its real 136×152 fits below the clamped position.
**Evidence** (headless, first 10 frames): navball ends parked at `[360,200]–[497,353]` on a
960×600 screen instead of the intended `[412,442]–[548,594]`; intermediate frames show the
sequence `default (412,442) → nominal-clamp (360,200) → stays`. Consequently
`hud/navball first-open overlap: true` at 960×600 (the readout block `[10,124]–[607,600]`
covers the ball). At 1080p the ball parks at y=680 instead of 922. The HUD readout block
self-corrects (its real height 476 re-clamps it to the bottom), the navball does not.
**Impact**: First open of flight view shows the navball floating mid-screen, detached from the
bottom edge, visually broken; on windows ≲1300 px wide it sits on top of the telemetry panel.
The player can drag it down (Finding 1 does not affect it), but the default layout is wrong.
**Suggestion**: Add `.default_size(nav_size)` to the navball Area (and, for symmetry,
`.default_size(egui::vec2(600.0, 476.0))`-ish to the readout block) so the sizing pass
constrains against the real size; or first show via `.current_pos()` after a warm-up frame.

### [MEDIUM] Finding 3: Tutorial rewinds and shows "vessel lost" on its own coached recovery path; the science tier stalls
**File**: `crates/csp-ui/src/tutorial.rs:264-267` (`observe` rewind before advance), step-8 coaching at `:117-121`
**Problem**: `observe()` applies the vessel-lost rewind (`step > STEP_LAUNCH && !has_vessel →
step = STEP_LAUNCH`) *before* the advance loop, and the rewind is not scoped to the flight tier.
Step 8's hint says: "…or land and 'recover' it for the full yield." `RecoverVessel` banks science
and removes the vessel in the same tick, so the next frame's snapshot is
`{has_vessel: false, science_banked: >0}` — the rewind fires, the banked-science signal in that
same snapshot is never examined, and the checklist drops to step 2 with the
"vessel lost — build or load a craft and LAUNCH again" banner. Step 9 (research) then cannot
complete until the player launches *another* vessel, after which steps 2–9 complete in one pass.
**Evidence** (unit-level, via the real `TutorialState::observe`): `{step: 8, science_pooled: 10}`
advanced normally to 8; feeding the post-recovery snapshot `{science_banked: 4.0, has_vessel: false}`
yields `step = 2, vessel_lost = true`. Same at step 9 (`research_spent: true` with no vessel →
not finished, step 2 + banner). `science_banked` is the program-wide mirror
(`csp-view/src/tutorial.rs:117`) and `has_vessel` is the live non-debris count (`:103`), so this
is the real post-recovery frame, not a synthetic.
**Impact**: The tutorial's flagship recovery path — the one its own coaching recommends —
punishes the player with a false "vessel lost" error state and stalls the final step.
**Suggestion**: Scope the rewind to the flight tier (only rewind when `self.step` is in
`STEP_LAUNCH+1..=6`), and/or run the advance loop before the rewind check; the science-tier
predicates (7–9) are vessel-independent by design and should never be rewound.

### [nit] Finding 4: `fmt_clock` can render `1:60`
**File**: `crates/csp-ui/src/lib.rs:643-652`
`sec = a − m*60` printed with `{:.0}` rounds 59.5–59.99 up: 119.7 s → `"1:60"` on the T-AP/T-PE/DEP/BURN countdowns. Use `a.div_euclid(60)`/floor the seconds (and round the minutes up from there if a "round up the wait" behavior is wanted).

### [nit] Finding 5: chip-rule drift + duplicated binding-join logic
**Files**: `crates/csp-ui/src/navball.rs:337-343`, `crates/csp-ui/src/settings.rs:869-877` vs `lib.rs:1066-1082`
- `heading_tape` draws its own `from_black_alpha(160)` backing instead of the shared
  `HUD_CHIP_FILL`/edge chip (`paint_hud_chip`) — the one overlay strip exempted from the
  §"readable-overlay rule" comment at `lib.rs:54-59`.
- The "join a row's `display_label`s" expression is copy-pasted in `help_panel` and
  `keymap_grid`. Extract `Keymap::bindings_label(action, labels) -> String` next to the
  existing `hint_labeled`.

### [nit] Finding 6: tutorial card's map-mode parking covers the map orbit readout
**File**: `crates/csp-ui/src/tutorial.rs:329` (right-top parking) vs `lib.rs:455-486` (`paint_orbit_readout` top-right)
In map mode with the tutorial active, the card parks exactly where the focused orbit's
Ap/Pe/period chip draws (and, being drawn later, on top). The "Close the orbit" step is when a
new player most wants that readout. The card is a draggable Window so it is recoverable;
consider parking map-mode at left-top (the Planning window is at `12,60` but narrower).

---

## Verified-fixed / clean (no findings)

- **Complaint #5 (notices unreadable) — FIXED.** `NOTICE_AMBER` (255,202,84) on the shared
  `hud_frame()` dark chip, `TextWrapMode::Extend` (single line — the bar widens, never folds;
  `lib.rs:702-729`), telemetry grid on the same chip (`lib.rs:761`). The new
  `tests/hud_dump.rs` rasterizes the real overlay over a bright backdrop and asserts
  `dark(chip)=21846 > bright(text)=3355 > 400` — passes; the ASCII luminance dump shows
  readable chips over the bright sky. Warp-refusal notices land on this same channel
  (`csp-app` `flight_notice` → `FlightSfx.cues` → `notice_overlay` in every view mode).
- **Notice queue**: TTL-bounded FIFO (`SFX_TTL_FRAMES=90`, popped at ttl 0,
  `csp-view/src/flight.rs:1948-1953`); producers are edge events + per-press refusals (key
  repeats are dropped for non-pan actions) — no realistic unbounded growth. OK.
- **Navball math** (`navball.rs`): ground-cap decomposition vs analytic horizon, seam-split
  ladder runs, dead-ahead/astern letter skipping, heading-tape shortest-angle wrap — all
  covered by strong unit tests; no defects found.
- **settings.rs**: rebind/evict/sanitize round-trips, one-action-per-key invariant, forward-compat
  parse, and the F5/F9/F10 (QuickSave/QuickLoad/RevertToLaunch) vocabulary are present and
  dispatched (`csp-app/src/lib.rs:2641-2643`). The `quality` field is honestly hidden (no lying
  control). Panel positions live in egui session memory only (not persisted across restarts) —
  matches KSP stock behavior, not a PRD requirement.
- **HUD readout block first-open** parks fully on-screen (bottom-left, self-corrects after the
  sizing pass); position survives mode switches (verified across 5 hidden frames).

## PRD coverage table

| Requirement (PLAN) | Status | Evidence |
|---|---|---|
| §3.7 presentation floor — legible flight HUD/instruments | **WIRED** | chip rule `lib.rs:54-119`; navball; `hud_dump.rs` contrast assertions |
| §4 KAC-equivalent alarms (UX context) | **WIRED** | `planning_panel` alarms/watch/window verbs `lib.rs:1465-1720` |
| M10 settings + input rebinding | **WIRED** | `settings.rs` (panel, keymap, sanitize, persistence via `csp-app` settings_io) |
| M10 rebindable keys incl. save/revert | **WIRED** | `KeyAction::{QuickSave,QuickLoad,RevertToLaunch}` `settings.rs:241-246`, dispatched `csp-app/src/lib.rs:2641-2643` |
| M10 tutorial/first-flight (now incl. science) | **WIRED w/ defect** | `tutorial.rs` steps 0-9; recovery-path rewind = Finding 3 |
| M10 crash handler + bug-report dialog | **WIRED** | `crash_dialog` `lib.rs:219-249` (bundle write is csp-app's) |
| §2.6/2.7 warp UX feedback (notices) | **WIRED** | global `notice_overlay` `lib.rs:702-729`, refusal reasons via `flight_notice` (producer side C24/C25) |
| Complaint #2 movable panels | **PARTIAL** | Windows + navball drag; readout block not draggable over text (Finding 1); navball mis-parked (Finding 2) |
| Complaint #5 notice/HUD contrast | **FIXED** | amber-on-chip + Extend wrap + `hud_dump` assertion |

## Top 3 issues
1. [HIGH] Readout block not draggable by its own text — selectable labels steal the gesture; fix is `.selectable(false)` on the row labels (Finding 1).
2. [HIGH] Navball first-open parking clamped against egui's nominal 600×400 sizing size — parks 242 px too high at every resolution, overlaps the telemetry panel on narrow windows; fix is `.default_size(nav_size)` (Finding 2).
3. [MEDIUM] Tutorial rewinds to "vessel lost" on its own coached land-and-recover path and stalls the research step; scope the rewind to the flight tier (Finding 3).
