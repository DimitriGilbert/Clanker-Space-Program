# Verified Report C17 — Editor/VAB (session-full-20261026)

Verification agent pass over `review-report-C17.md`. Every finding was re-checked
against the working tree; the HIGH finding was re-verified empirically with a
fresh software-rasterization probe of the real GPU-pick inputs (built through
`View::with_sim` + `take_vab_packet`, rasterized exactly as `fs_pick` + the pick
pipeline would: NDC pos via w=1 passthrough, reverse-Z depth, `Greater` wins,
pixel-center sampling, no cull; probe files created under
`crates/csp-view/tests/` and deleted after the run — `git status` clean).

**Totals: 8 CONFIRMED, 1 DISMISSED (Finding 4).**

---

### Finding 1: Pick-0 decoration rasterized in the GPU pick pass kills the advertised click gestures — CONFIRMED

**Original**: The pick pass draws the ghost/selection shell (pick 0) over the click
target, so click-to-place decodes as background and click-selected-to-lift
deselects instead.

**Verification** — re-derived from pipeline state, then re-probed empirically.

Pipeline re-derivation (all in the tree):
- `VabRenderer::encode_pick` (crates/csp-render/src/vab.rs:404-455) uploads the
  ENTIRE `packet.vertices` stream (line 419) and draws `0..n` (line 451) into an
  `R32Uint` target cleared to `TRANSPARENT` (= u32 0, line 432), with its own
  `Depth32Float` cleared to 0.0 (line 440) under `depth_write_enabled: true`,
  `Greater` wins reverse-Z (lines 166-172, 271-286). `fs_pick`
  (crates/csp-render/src/vab.wgsl:104-107) returns the flat `pick` varying.
- The stream carries decoration tagged pick 0 by construction: the selection
  shell (crates/csp-view/src/vab.rs:793-805), ghosts (812-823), and markers
  (`emit_octa` → `unlit_v(..., 0)`, 1128-1158). This is even documented at
  crates/csp-render/src/vab.rs:87-89 ("ghosts, markers, and selection shells
  ride it too, tagged pick 0").
- Shell half is structural, not incidental: `part_mesh::inflated`
  (crates/csp-view/src/part_mesh.rs:584-624) grows the shape outward in ALL
  directions (box faces on all three axes; lathe rings radially + end stations
  along z), so the shell strictly encloses the part. In a `Greater`-wins pass
  the enclosing shell's near face beats the part's surface at EVERY part pixel,
  independent of triangle order.
- Request path: any unconsumed, un-dragged LMB release in VAB mode requests a
  pick at the cursor with no carry check (crates/csp-app/src/lib.rs:2456-2501);
  decode → `VabEvent::Pick` (csp-app:874-899, with the epoch guard) →
  `VabAction::Select` (crates/csp-view/src/vab.rs:170-173). `select(None)`
  while carrying only re-prints "carrying X — click a craft part to place it"
  (crates/csp-ui/src/vab.rs:1259-1267); with a selection it deselects, never
  reaches `grab` (1271-1277).
- The in-tree test injects `VabEvent::Pick(Some(root))` directly
  (crates/csp-view/src/vab.rs:1401), bypassing rasterization — which is why the
  suites stay green.

Empirical re-probe (real `data/` catalog, tank root at viewport 800x600, cursor
at the hover point):
- Plain state (selection cleared — note `SetRoot` auto-selects the root, so the
  reviewer's "deselected" premise required the explicit clear): 96 part
  triangles, 0 decoration triangles, click pixel decodes pick 1. Validates the
  rasterization method end-to-end.
- Root selected: click pixel decodes 0; **1440/1440** grid-sampled part pixels
  decode 0 — the shell shadows the entire part, exactly as the enclosure
  argument predicts.
- Carrying (`CatalogPick` of a second tank, selection cleared, ghost at the
  snap pose): the click/hover pixel decodes 0 with the decoration decisively
  nearer (deco z 0.0142 vs part z 0.0100); 883/1440 part pixels shadowed.
  Robustness matrix over 72-89 hover positions on two cameras (default and
  top-down): the hover pixel decodes 0 at 69/72 (default) and 89/89 (top-down).

One precision adjustment: the report's "the click pixel is structurally always
under the ghost" is slightly overstated — 3 of 72 hover pixels escaped the ghost
on the default camera (0 on top-down). The shell half IS structurally total,
and the ghost half fails ~96-100% of the time, so the HIGH verdict and impact
stand unchanged: both advertised gestures ("Carrying X — click a craft part to
place it", banner csp-ui:2952/3691; "click/drag selected: move", hint bar
csp-ui:3656-3657) are dead in the common case, with no feedback beyond the
repeated carry notice, and the fallbacks (palette drag-release → `Drop` via the
CPU hover; craft-tree clicks → `AttachAuto`) keep the game playable.

---

### Finding 2: Click-armed part + viewport drag swallowed whole — CONFIRMED

**Original**: With `pending_part` armed by a palette click, an LMB viewport drag
produces no orbit, no drop, no pick — a dead gesture.

**Verification** — traced through the gesture state machine
(crates/csp-ui/src/vab.rs:2304-2465) and the app click candidate:
- Palette click arms `pending_part` via `catalog_pick` (1127-1161); `drag_part`
  stays `None`.
- Press over the viewport: `orbit_block` set (2382-2386 — `carried().is_some()`
  is true), so `apply_vab_input` drops every `Orbit` delta for the press's
  whole life (crates/csp-view/src/vab.rs:165-167). No camera motion.
- Drag: the drag-lift branch requires `vab.carried().is_none()` (2395) — false,
  skipped. No grab.
- Release over the viewport: the `move_node` branch (2406) and `drag_part`
  branch (2416) both match `None` — nothing runs; no `CancelPlacement` either.
- App side: release with drag > `CLICK_MAX_DRAG_PX` (4 px,
  csp-app/src/lib.rs:129, 2475-2479) requests no pick.

Net: no orbit, no drop, no pick, no cancel — exactly as reported. It is the
natural gesture to try after F1's click-place fails, which compounds the HIGH.

---

### Finding 3: Every undo/redo resets camera zoom/pan via the shared `craft_epoch` — CONFIRMED

**Original**: `after_history_restore` bumps `craft_epoch`, which the view reads
as "new craft — refit the camera", so Ctrl+Z yanks zoom back to `extent*3` and
drops the pan.

**Verification**:
- `after_history_restore` bumps `craft_epoch` on every undo and redo
  (crates/csp-ui/src/vab.rs:1104-1118, bump at 1115; called at 1087 and 1097).
- `vab_present` treats an epoch change as a craft replacement:
  `focused_once = false; pan_offset = ZERO` (crates/csp-view/src/vab.rs:278-282),
  then re-derives `distance_m = extent * 3` (387-390). Zoom and pan reset;
  yaw/pitch survive.
- Undo is high-frequency and advertised: every removal notice ends "Ctrl+Z
  restores" (csp-ui:1180-1187, 1205-1209) and Ctrl+Z/Ctrl+Shift+Z dispatch
  directly (2442-2447).
- The bump itself is deliberate (the stale-pick guard, comment at 1100-1103;
  the readback guard it feeds is real — csp-app:881-893), but the field's own
  docs (csp-ui:460-464, 580-583) say only `New`/`LoadBytes` bump it. The
  suggested split (`pick_generation` vs `craft_epoch`) is the right shape.
Confirmed as a real UX regression, not a correctness bug.

---

### Finding 4: Variants wired but content-zero ("all 16 parts single stock variant") — DISMISSED

**Original**: The mechanism is real end-to-end but every catalog part ships one
variant, so no player can exercise it; PRD row PARTIAL.

**Verification** — the factual core is wrong:
- 14/16 parts do author a single `stock` variant, but BOTH engines author a
  second variant: `csp.engine.ember` (data/parts/csp.engine.ember.ron:40-44)
  and `csp.engine.sparc` (data/parts/csp.engine.sparc.ron:44-48) each carry
  `stock` + `qualified` with real multipliers (cost x1.25/x1.2;
  `BaseMtbfS` 1.3, `IspVacS` 1.03 / `RatedBurnS` 1.5).
- The path is player-reachable: the inspector's variant combo iterates the
  def's variants and issues `SetVariant` (crates/csp-ui/src/vab.rs:3364-3380),
  and the resolver genuinely applies variant stat/cost multipliers in its
  `scale → variant → upgrades` chain (crates/csp-phys/src/resolve.rs —
  doc lines 39-48, 240-241, 415, 474).

So "all 16 parts author exactly one stock variant" and "no player can ever
exercise it" are false; the PRD table's parenthetical for the variants row is
likewise wrong (the row is arguably WIRED, not PARTIAL, with 2 parts x 2
variants of real content). Whether two multi-variant parts fully satisfy the
M3 "scaling + variants" exit row is a coverage judgment call, not a defect —
at most a nit-level content wish (more multi-variant parts), which does not
survive as a MEDIUM.

---

### Finding 5: 16 ms latency criterion never asserted at release in CI — CONFIRMED

**Original**: The test's 16 ms branch is debug-gated to a 200 ms band; CI runs
tests only in debug and the release job runs `csp-perf`, which has no VAB probe.

**Verification**:
- crates/csp-view/tests/vab_latency.rs:159:
  `let budget_ms = if cfg!(debug_assertions) { 200.0 } else { 16.0 };` — the
  16 ms assert can only fire in a release-profile test run.
- .github/workflows/ci.yml:37-40 — the only test step is
  `cargo test --workspace` (debug profile). The release job (ci.yml:89-95)
  builds and runs `csp-perf --baseline` only.
- Grep confirms no VAB probe in crates/csp-bench/src/perf.rs and no VAB row in
  docs/perf/perf-log-devbox-release.ron (zero matches), and no
  `--release` test invocation anywhere in .github/ or xtask.

The PRD row's PARTIAL status ("never run at release in CI") is accurate; a
release-only regression past 16 ms passes every automated gate. Confirmed.

---

### Finding 6: `VabAction::Translate` has no production issuer — CONFIRMED

**Original**: Dispatched and tested but never issued outside the test; dead
surface, capability exists via root-grab + empty-space drop.

**Verification**: dispatch at crates/csp-ui/src/vab.rs:1056-1075; workspace-wide
grep finds exactly one issuer — the test at csp-ui:4886. The player-equivalent
path is real: `grab(root)` lifts the whole craft (1287-1311) and
`drop_whole_craft` lands it via `set_transform` (1353-1374). LOW as reported.

---

### Finding 7: `notice_overlay` not in the pointer mask — right-click on a cue chip can delete the part behind it — CONFIRMED

**Original**: The amber cue stack floats over the viewport unmasked; a
right-click on it resolves as a viewport gesture and removes the hovered part.

**Verification**:
- `notice_overlay` (crates/csp-ui/src/lib.rs:702-729) returns `()`; it is drawn
  at crates/csp-view/src/lib.rs:810, AFTER mask collection (editor rects 623,
  `m10_panels` 794) and BEFORE `vab_pointer_input` receives `ui_masks`
  (817-818). Its rect never joins the masks.
- The area is `interactable(false)` (csp-ui:709), so egui never consumes the
  click; `over_viewport` (csp-ui:2350-2351) then evaluates true through the
  chip, and the `secondary && over_viewport` arm (2429-2434) removes
  `vab.hover()`'s subtree. Same bug class the C14-F2 window masking closed.
Confirmed, LOW (transient, center-top cues only).

---

### Finding 8: Failed craft save swallowed silently (`to_bytes().ok()`) — CONFIRMED

**Original**: A serialization failure leaves `craft_save` `None` — no file, no
notice — violating the "no op is ever silent" contract.

**Verification**: crates/csp-view/src/lib.rs:846-850 —
`vab.0.to_bytes().ok().map(|bytes| (name, bytes))`; the `Err` path produces no
notice anywhere. The contract text is at crates/csp-ui/src/vab.rs:866-871
("every call lands a line in notice ... so no op is ever silent"). Practically
near-unreachable (bincode over a valid session), so LOW is the right weight.
Confirmed.

---

### Finding 9: DRY/CRAP — duplicated planner/emitter code, 10-field gesture state — CONFIRMED (one fix caveat)

**Verification** of each concrete refactor:
- `plan_axial` (csp-ui:1642-1723) vs `attach_auto` (1998-2078): same
  candidate-enumeration idea with the same facing metric (1690 / 2050), same
  twist metric (1702 / 2057), same sort shape (1707 / 2062), same
  try-until-accepted loop (1709-1719 / 2064-2070), and the ancestor-chain fold
  duplicated (1659-1679 vs 2029-2042 — same statements, plan_axial's as a
  fallback closure, attach_auto's inline). Confirmed; "byte-identical" is
  fair modulo the wrapper and a variable name.
- `emit_part` model branch (csp-view:1191-1218) vs `emit_triangles` model
  branch (1070-1103): near line-for-line. CAVEAT on the suggested fix: they
  are NOT behavior-identical — `emit_part` writes raw baked albedo, while
  `emit_triangles` multiplies albedo by the passed color. Delegating with
  `p.color` would category-tint every baked model (a visible change);
  delegation needs a white tint for the model path or an explicit decision to
  adopt the tint. The duplication finding stands.
- Ten pointer/carry fields on `VabSession` confirmed (csp-ui:393-482:
  `pending_part`, `drag_part`, `move_node`, `press_node`, `orbit_block`,
  `ghost_status`, `hover`, `cursor_px`, `drop_anchor`, `press_viewport`), with
  the one-carry rule restated across `set_drag_part` (632-638),
  `catalog_pick` (1147-1155), `grab` (1293-1306).
- `select` (1242-1278) vs `drop_carried` (1317-1346): parallel decode of the
  same carry x target matrix; `drop_whole_craft` (1353-1374) carries the third
  copy of the root-on-empty rule. Confirmed.

---

## PRD coverage rows re-checked

- **GPU picking DEGRADED — stands.** The pipeline, id encoding, and epoch
  guard are real and work in the plain state (probe: unselected, ghost-free
  part decodes correctly); the decoration-in-stream defect (Finding 1) is what
  degrades it. Confirmed by probe.
- **Variants PARTIAL — corrected.** The row's evidence ("all 16 parts single
  stock variant") is false (both engines ship `stock` + `qualified`); with the
  mechanism wired end-to-end and player-reachable, the row is arguably WIRED.
  See Finding 4.
- **VAB <=16 ms @300 parts PARTIAL — stands.** The release assert exists but
  no CI job ever executes a release-profile test; the committed perf log has
  no VAB row. See Finding 5.

## Phase3-fix and walkthrough sections

Spot-checked against the tree; no contradictions with the verified findings.
One detail worth noting that reconciles the walkthrough with Finding 1: an
empty-craft click still places the root even though the float ghost decodes 0,
because `select(None)` on an empty session routes to `place_root`
(csp-ui:1262) — which is why "palette click -> click build area" works while
"click a craft part to place it" does not.
