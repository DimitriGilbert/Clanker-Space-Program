# Verified Report C23 — verification agent pass

Report under review: `review-report-C23.md` · tree at HEAD `7dd4b86` (clean — see
Tree cleanliness below). Verification method: source read of every cited
file:line, egui 0.37.0 registry source read (`area.rs`, `label.rs`, `style.rs`,
`progress_bar.rs`, `id.rs`), plus two throwaway empirical harnesses in `/tmp`
(never in the project tree): a headless egui 0.37 pass-runner replicating the
`flight_hud`/navball Areas with press→drag→release event scripts and an
A/B `.selectable(false)` run, and a standalone replication of
`TutorialState::observe`. All 43 `csp-ui` tests pass; `hud_dump` contrast test
run with `--ignored --nocapture`.

Score: **6 confirmed / 0 dismissed.** Both complaint verdicts upheld.

---

### Finding 1: Readout block not draggable by its own text (HIGH) — CONFIRMED
**Original**: egui 0.37 default-selectable `ui.label` widgets sense
click+drag and win the press over the Area's move widget; the readout block
is a wall of mono labels, so grabbing it where the text is does nothing.

**Verification**:
- Source: `crates/csp-ui/src/lib.rs:753-756` — `Area::new(Id::unique("flight_hud")).movable(true)`; the row labels at `lib.rs:785-810` (`pair`/`row`/`sect`), `:813-819` (ON PAD), `:762-775` (no-vessel), `:979-1009` (controls hint) are all plain `ui.label(RichText…)` with no `.selectable(false)`. The notice labels already do it correctly at `lib.rs:723`.
- egui 0.37 source: `style.rs:1599` `selectable_labels: true` by default; `label.rs:154-166` — a selectable label gets `Sense::click_and_drag()` (non-touch); `area.rs:553-596` — the move widget is a whole-area-rect `Sense::DRAG` widget created in `begin()` *before* the content widgets, so later-created labels are hit-tested on top and claim the drag. `progress_bar.rs:117` — ProgressBar allocates with `Sense::hover()` (inert), which is why the bar rows still pass the drag to the move widget, exactly as the report says.
- Empirical (headless, 960×600, press→+70/−30→+140/−60→release): with default-selectable labels, drags starting on all four label-row points yield delta `(0,0)` and `interaction_snapshot.dragged` reports a *label* id, not `flight_hud.with("move")`; the same script with `.selectable(false)` on the row labels drags from every in-panel point (delta `(+140,−60)`, dragged id = the move id). The navball Area (painter-only, `Sense::hover()` content) drags fine from its parked rect. This reproduces the reviewer's A/B evidence exactly (their `(500,550)` point sits in the real panel's empty right margin next to the 280-px bars; my simplified content is narrower so that point missed the panel — same mechanism, no material discrepancy).
- The suggested fix is valid: `egui::Label::new(...).selectable(false)` is the pattern `notice_overlay` already uses.

### Finding 2: Navball first-open parks 242 px too high (HIGH) — CONFIRMED
**Original**: the sizing pass constrains `default_pos` against egui's nominal
600×400 `default_area_size`, clamping the ball to `y = H−400` forever.

**Verification**:
- Source: `crates/csp-ui/src/lib.rs:1018-1024` — `default_pos(ball_center.x − 68, rect.bottom() − 152 − 6)`, no `.default_size`. `nav_size = (136, 152)`.
- egui 0.37 source: `area.rs:482-530` — first show (`state.size == None`) forces a sizing pass whose size is `default_size` (NaN) → `style.spacing.default_area_size` = `vec2(600.0, 400.0)` (`style.rs:1579`); `area.rs:604-613` then constrains the position via `constrain_window_rect_to_area` (`context.rs:2907-2915` — pure push-to-fit: a rect that already fits is never moved). Since the real 136×152 content fits under the clamped `y = H−400`, later frames keep it there permanently.
- Empirical: on a 960×600 headless screen the navball parks at `[360,200]` (f1=f2=f3, stable) instead of the intended `[412,442]` — 242 px high, `x = min(W/2−68, W−600)` = 360, exactly the report's numbers. The readout-block replica with a ≥400-tall label stack self-corrects to `[10,124]–[…,600]` (real height re-clamps it to the bottom), matching the report's `[10,124]–[607,600]`; the navball never does. Ball rect `[360,200]–[496,352]` overlaps the readout rect horizontally (360 < 607) — the narrow-window overlap claim holds.
- Fix suggestion valid: `default_size` is a real `Area` builder (`area.rs:281-290`) and is what the sizing pass uses (`area.rs:517-519`).

### Finding 3: Tutorial rewinds on its own coached recovery path (MEDIUM) — CONFIRMED
**Original**: `observe()` rewinds (`step > STEP_LAUNCH && !has_vessel`) before
the advance loop, unscoped; `RecoverVessel` banks science and removes the
vessel in one tick, so the post-recovery frame rewinds steps 8/9 to step 2 with
the "vessel lost" banner.

**Verification**:
- Source: `crates/csp-ui/src/tutorial.rs:264-267` (rewind before the `while` advance loop, no tier scoping); step-8 hint `tutorial.rs:116-121` recommends "land and 'recover' it for the full yield".
- Sim side: `csp-sim/src/economy.rs:970` banks pooled science and `economy.rs:1000` `remove_vessel` — the same atomic apply. Mirror side: `csp-view/src/tutorial.rs:117` `science_banked: mirror.input.banked` (staged from `sim.science().banked`, `csp-view/src/lib.rs:3587`) and `:103` `has_vessel: live.0 > 0` (non-debris count across residences, `flight.rs:1503-1508`). The post-recovery frame `{has_vessel: false, science_banked > 0}` is real, not synthetic.
- Empirical (standalone replication of `observe`/`step_complete` verbatim): `{step:8, science_pooled:10, has_vessel:true}` → stays 8; feeding the post-recovery snapshot `{science_banked:4.0, has_vessel:false}` → `step=2, vessel_lost=true` (banner text confirmed at `tutorial.rs:384-389`); at step 9 with `research_spent:true` and no vessel → `step=2, vessel_lost=true, finished=false`. Counter-check: with another vessel alive there is no rewind (`step=9`) — the defect is specific to the single-vessel coached path, as the report frames it.
- The advance loop starting at the rewound step 2 stalls on `has_vessel`, never examining the banked-science predicates — the research step indeed cannot complete until a new launch. Fix suggestion (tier-scope the rewind and/or advance before rewind) is sound.

### Finding 4: `fmt_clock` renders `1:60` (nit) — CONFIRMED
`lib.rs:643-652`: `sec = a − m*60` printed `{sec:02.0}` rounds 59.5–59.99 up.
Empirically: 119.7 s → `"1:60"`; 3599.9 s → `"59:60"`. Used by T-AP/T-PE
(`lib.rs:836-838`), DEP (`:948`), BURN (`:954`). Real, cosmetic.

### Finding 5: chip-rule drift + duplicated binding-join (nit) — CONFIRMED
- `navball.rs:337-343`: `heading_tape` fills `Color32::from_black_alpha(160)` + gray-120 stroke instead of the shared `HUD_CHIP_FILL`/`paint_hud_chip` (`lib.rs:62-101`) — the only painter-drawn HUD strip off the rule documented at `lib.rs:54-59` (and `paint_hud_chip` is `pub(crate)` in the same crate, so reuse is feasible).
- Duplicated join: `lib.rs:1066-1075` (`help_panel`) and `settings.rs:866-877` (`keymap_grid`) both do `bindings.get(action).map(|v| v.iter().map(|k| k.display_label(labels)).collect::<Vec<_>>().join(sep))` + the `"—"` fallback — copy-paste with only the separator differing (`" / "` vs `"  "`).

### Finding 6: tutorial card parks over the map orbit readout (nit) — CONFIRMED
`tutorial.rs:329` parks the card at `(screen.right() − 298, top + 30)` with
`default_width(290)`; `paint_orbit_readout` draws its chip right-aligned to
`rect.right() − 12` starting at `top + 34` (`lib.rs:476-486`). In csp-view the
map overlay draws at `lib.rs:873` and `tutorial_overlay` at `:1491` — later,
same/higher layer → card on top. Overlap real during "Close the orbit" (step 6)
whenever the card is open in map mode. Card is a draggable `egui::Window`
(recoverable), and the Planning window parks at `(12, 60)` (`lib.rs:1494`) so
left-top is free — suggestion valid.

---

## Complaint verdicts (verified)

- **Complaint #5 (notices unreadable) — FIXED: upheld.** `notice_overlay`
  (`lib.rs:702-729`): `NOTICE_AMBER` on the shared `hud_frame()` chip,
  `TextWrapMode::Extend` single-line, labels `.selectable(false)` at `:723`.
  Ran `cargo test -p csp-ui --test hud_dump -- --ignored --nocapture`: passes,
  `notice band: dark(chip)=21846 bright(text)=3355 mid(AA)=1199` — the exact
  figures the report cites, `bright > 400` and `dark > bright` asserted
  (`tests/hud_dump.rs:259-266`). Warp-refusal path confirmed end-to-end:
  `flight_notice` (`csp-view/src/lib.rs:3311-3317`) pushes into
  `FlightSfx.cues` with `SFX_TTL_FRAMES=90` (`flight.rs:590`), TTL- aged and
  popped at `flight.rs:1948-1953` — bounded, no unbounded growth.
- **Complaint #2 (movable panels) — PARTIALLY FIXED: upheld.** Windows drag by
  title bar; navball Area drags from anywhere (empirically confirmed); the
  readout block cannot be dragged over its text (Finding 1, empirically
  confirmed) and the navball's default parking is wrong (Finding 2,
  empirically confirmed).

## Other report claims spot-checked (all accurate)

- QuickSave/QuickLoad/RevertToLaunch dispatched at `csp-app/src/lib.rs:2641-2652`
  (revert key arms the disclosure prompt, matching the report).
- `quality` honestly hidden (`settings.rs:777` "No `quality` row: the field has
  zero consumers"); round-trip/rebind tests present and passing.
- Full `cargo test -p csp-ui`: 43/43 pass.

## Tree cleanliness

`git status`: no source modifications anywhere under `crates/`. Untracked files
are session artifacts only (`.review/session-full-20261026/*`, including this
and the sibling reports) plus two reference docs (`docs/UI-AND-RENDERING.md`,
`docs/DATA-AND-SCRIPTING.md` — session documentation written against `ff897d4`,
not probe code); the one modified tracked file is
`.review/session-full-20261026/STATUS.md` (session bookkeeping). The reviewer's
throwaway harness is indeed gone; no probe code remains. This verification
agent's harnesses were built in `/tmp` only.
