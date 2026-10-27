# Review Report C17 — Editor/VAB (session-full-20261026)

Files: `crates/csp-ui/src/vab.rs`, `crates/csp-view/src/vab.rs`, `crates/csp-render/src/{vab.rs,vab.wgsl}`, `crates/csp-view/tests/vab_latency.rs`. Working tree == HEAD for all five (verified: `git diff HEAD` empty on them). Test suites run green: csp-ui vab 25/25, csp-view vab 4/4, `vab_latency` pass (debug band). Findings below were verified by code trace plus an empirical software-rasterization probe of the real GPU-pick inputs (described in F1), since the pick pass itself needs a GPU.

### [HIGH] Finding 1: The GPU pick pass rasterizes pick-0 decoration (ghost, selection shell, markers), so the advertised click gestures decode as background and silently no-op
**File**: crates/csp-render/src/vab.rs:419-451 (`encode_pick` draws ALL `packet.vertices`), crates/csp-view/src/vab.rs:793-823 (selection shell + ghosts + markers emitted into `vertices` with `pick = 0`), crates/csp-app/src/lib.rs:2466-2489 (every unconsumed un-dragged viewport click requests a pick — no carry check).
**Problem**: `fs_pick` writes the vertex's pick index with full depth writes. The snapped placement ghost sits exactly where the player is told to click, and the inflated selection shell covers the entire selected part. Both write 0 (= background). The readback then resolves `VabEvent::Pick(None)` → `Select(None)`.
**Evidence** (empirical, real `data/` catalog through `View::with_sim` + `take_vab_packet`, software-rasterized at the cursor pixel exactly as `fs_pick` would): deselected craft, no ghost → pick id 1 (correct, validates the method); root selected → id 0 at every probed pixel (shell); ghost armed (selection cleared first) → id 0 at all 4 probed pixels including ±100 px off the hover point (ghost). The in-tree test `bottom_cap_pick_attaches_below_and_env_never_picks` (csp-view/src/vab.rs:1401) injects `VabEvent::Pick(Some(root))` directly, bypassing the rasterization — which is why the suites stay green.
**Impact**: (a) "carrying X — **click a craft part to place it**" (banner at csp-ui/src/vab.rs:2952, 3691) never places — the click pixel is structurally always under the ghost (the ghost renders at the hover point; the click is at the hover point). (b) "**click/drag selected: move**" (hint bar, csp-ui/src/vab.rs:3656-3657) — the click half never lifts; clicking the selected part deselects it instead (shell). Players can still build via palette-drag-release and craft-tree clicks (both use the CPU hover, not the GPU pick), so the game is playable, but its two advertised primary gestures are dead and fail with no visible feedback beyond a repeated "carrying…" line. This also degrades the M3 "GPU picking" exit row (picking works only for plain select of an unselected, ghost-free part).
**Suggestion**: keep decoration out of the pick stream — either a third packet stream (`deco_vertices`, drawn only by the color pass, like `env_vertices`), or record `packet.pickable_vertex_count` (parts only, emitted before ghosts/markers; shells would need to move into the decoration stream) and have `encode_pick` draw `0..count`. Add a regression test that software-picks through a carried ghost at the hover pixel (the probe I used is ~40 lines and needs no GPU).

### [MEDIUM] Finding 2: A click-armed part + viewport drag is swallowed whole — no orbit, no drop, no pick
**File**: crates/csp-ui/src/vab.rs:2382-2425.
**Problem**: with `pending_part` armed by a palette *click*, an LMB press on the viewport sets `orbit_block` (carry is `Some`, csp-ui/src/vab.rs:2382-2386) so camera orbit deltas are dropped (csp-view/src/vab.rs:165); the drag-lift branch is skipped (`carried().is_none()` fails, :2395); and the release branch only handles `move_node`/`drag_part` (:2406-2423) — a click-armed `pending_part` matches neither, so the release does nothing. The app-side click candidate also refuses (dragged > threshold), so no pick fires either.
**Impact**: the player who clicks a palette row and then drags across the viewport gets zero response of any kind — a dead gesture with no notice. It is the natural thing to try after the click-place in F1 fails.
**Suggestion**: in the release branch, when `over_viewport && drag_part.is_none() && move_node.is_none() && pending_part.is_some() && the press was a drag`, resolve `VabAction::Drop` (the CPU hover is authoritative) or at least cancel the orbit-block so the drag orbits instead of freezing the camera.

### [MEDIUM] Finding 3: Every undo/redo resets the camera zoom and pan
**File**: crates/csp-ui/src/vab.rs:1104-1118 (`after_history_restore` bumps `craft_epoch`), crates/csp-view/src/vab.rs:278-282 (epoch change → `focused_once = false; pan_offset = ZERO` → distance re-derived as `extent*3`).
**Problem**: the epoch bump exists so a late GPU pick can't decode against a restored craft (P10-F3) and so New/load refits the camera — but undo/redo reuse it, so every Ctrl+Z (advertised after every removal: "Ctrl+Z restores") yanks the camera back to the default framing distance and drops the pan.
**Impact**: undo is a high-frequency verb in an editor; the camera jump makes rapid undo-redo comparisons of a craft edit actively harder and reads as a glitch.
**Suggestion**: split the stamp — a monotonic `pick_generation` (bumped on every session replacement incl. undo) for the readback guard, and keep `craft_epoch` (bumped only by `New`/`LoadBytes`) for the one-shot camera refit.

### [MEDIUM] Finding 4: M3 "variants" is wired but content-zero — all 16 parts author exactly one "stock" variant
**File**: data/parts/*.ron (16/16 have a single `stock` variant); UI at crates/csp-ui/src/vab.rs:3366-3380; resolver path csp-phys.
**Problem**: the M3 exit row requires "scaling + variants" (PLAN.md:1497). The mechanism is real end-to-end (variant combo, `SetVariant`, `BuiltPart.variant` serialization, stat multipliers in the resolver), but every catalog part ships one variant, so no player can ever exercise it. §6.4's cut list does not sanction dropping variant content.
**Impact**: PRD coverage PARTIAL — a TweakScale-equivalent half. Scaling itself is genuinely wired (16/16 parts author `Continuous` rules and the inspector slider is live).
**Suggestion**: author at least one multi-variant part (e.g. the tank's `stock`/`unpressurized` or a nosecone `pointed/blunt`) so the pipeline is player-reachable, or record the content gap as a deliberate deferral in STATUS.

### [MEDIUM] Finding 5: The 16 ms M3 latency criterion is never asserted at release — CI runs the test only in debug (200 ms band)
**File**: crates/csp-view/tests/vab_latency.rs:159-164; .github/workflows/ci.yml:40 (`cargo test --workspace`, debug profile; the release job at :89-95 runs `csp-perf`, which contains no VAB probe — verified by grep of `csp-bench/src/perf.rs` and `docs/perf/perf-log-devbox-release.ron`).
**Problem**: the test's 16 ms branch is dead in CI; the release-mode number the PRD exit row names ("VAB latency ≤16 ms at 300 parts on REF-A", PLAN.md:1497) is only produced if a developer happens to run release tests locally.
**Impact**: a regression that pushes release latency past 16 ms would not be caught by any automated gate.
**Suggestion**: run this one test in the existing release job (`cargo test --release -p csp-view --test vab_latency`) — it is ~1 s — or fold a VAB interaction probe into `csp-perf` so the committed perf log carries the number.

### [LOW] Finding 6: `VabAction::Translate` has no production issuer
**File**: crates/csp-ui/src/vab.rs:269, 1056-1075.
**Problem**: the free-reposition verb is dispatched and tested but nothing in csp-ui/csp-view/csp-app ever issues it (grep: only the test at :4886). The player capability exists via root-grab + empty-space drop, so this is dead surface, not a missing feature — but it is exactly the "coded but not wired" shape the project's own `lint-reach` discipline exists to catch (that lint covers SimCommands, not VabActions).
**Suggestion**: either expose it (e.g. arrow-key nudge in the VAB) or mark it test-only and note the grab-drop path as the issuer.

### [LOW] Finding 7: `notice_overlay` is not in the pointer mask — a right-click on a cue chip can still delete the part behind it
**File**: crates/csp-view/src/lib.rs:808-818 (mask collection happens in `m10_panels`/editor passes; `notice_overlay` at csp-ui/src/lib.rs:702-729 is drawn afterwards, `interactable(false)`, and its rect never joins `ui_masks`).
**Problem**: the C14-F2 fix masks every *window*, but the amber cue stack floats over the viewport unmasked; `secondary_clicked` has no consumption path, so a right-click landing on a cue resolves as a viewport gesture and removes the hovered part's subtree.
**Impact**: same bug class the fix closed (settings-window right-click deleting a craft part), at much lower frequency — the cues are transient and center-top.
**Suggestion**: have `notice_overlay` return its rect and push it into `ui_masks`, same as the hint bar's chip area ideally.

### [LOW] Finding 8: A failed craft save is swallowed silently (`to_bytes().ok()`)
**File**: crates/csp-view/src/lib.rs:846-850.
**Problem**: `vab.0.to_bytes().ok().map(...)` — if serialization fails, `craft_save` stays `None`: no file, no notice. The module contract says "every call lands a line in notice … so no op is ever silent" (csp-ui/src/vab.rs:866-871). Practically unreachable (bincode over a valid session), but it is the one op path with a silent failure mode.
**Suggestion**: map the error to `launch_refused`-style feedback (`set_notice`) instead of `.ok()`.

### [LOW] Finding 9: DRY/CRAP — duplicated planner/emitter code and a 10-field gesture state
**File**: crates/csp-ui/src/vab.rs, crates/csp-view/src/vab.rs.
**Problem/Concrete refactors**:
- `plan_axial` (csp-ui:1642-1723) and `attach_auto` (csp-ui:1998-2078) are two copies of the same candidate-enumeration idea (facing metric + least-twist sort + try-until-accepted), and both contain a byte-identical ancestor-chain fold block (1659-1679 vs 2029-2042). Extract `fn body_transform_of(&self, node) -> PartTransform` and make `attach_auto` = "plan, then commit first edge" on top of `plan_axial` — the commit path is already `preview_attach`-shaped, so parity is structural today but must be maintained by hand.
- `emit_part`'s model branch (csp-view:1191-1218) duplicates `emit_triangles`' model branch (1070-1103) almost line-for-line. `emit_part` should be `emit_triangles(..., p.model.as_deref(), p.transform, p.color, pick)` (it already exists as the shared core — the kitbash half deduplicates, the model half didn't).
- `VabSession` carries ten pointer/carry fields with cross-invariants (`pending_part`, `drag_part`, `move_node`, `press_node`, `orbit_block`, `press_viewport`, `ghost_status`, `hover`, `cursor_px`, `drop_anchor`, csp-ui:393-482) whose consistency rules ("one carry at a time", "press anchors the gesture") are re-stated in five setters and three dispatch arms. Group them into a `Carry`/`Pointer` struct with the one-carry rule in one mutator.
- `select()` (1242-1278) and `drop_carried()` (1317-1346) decode the same carry×target matrix in parallel — a third copy of "root drops on empty, part keeps carrying" lives in `drop_whole_craft`. One `resolve_carry(target: Option<PartNodeId>) -> CarryResolution` would collapse them.

## Phase3 fixes — do they hold? YES
- **Bottom attach**: holds. `PITCH_LIMIT = π` pole-to-pole orbit (csp-view:47, test `orbit_reaches_below_the_craft`), the hover ray classifies `BottomCap` via the entry face (csp-view:678-687, test `hover_from_below_hits_the_bottom_cap`), the hangar floor is depth-write-free AND excluded from the pick stream (csp-render/src/vab.rs:177-199, 413-451; test `bottom_cap_pick_attaches_below_and_env_never_picks`), and `plan_axial` honors the hit side (`want_down`, csp-ui:1682, test `bottom_cap_hover_plans_the_bottom_node`).
- **Undo**: holds. Whole-session snapshots, pushed only when `session != prev` (no-op drops push nothing), bounded 64, redo cleared on new mutation, bit-identical restore incl. ids and `next_id` (test `undo_restores_a_deleted_subtree`); `preview_attach` == `add_part`'s `compute_attach` (session.rs:1377-1381) so a planned fan can't half-commit in practice.
- **Move**: holds. Drag-lift via `press_node` + `is_decidedly_dragging` with orbit suppressed for the press's whole life; reattach keeps id/build/subtree; own-subtree drop = put back; root lifts the whole craft and lands at the view-computed anchor (`root_grab_moves_the_whole_craft`).
- **Remove-gluing**: holds. The floor is compositional only (0.30·extent gap, no occlusion, no pick), and remove/RemoveOnly/reroot are all undoable session ops.
- **Occupancy**: holds. `bound_attach_nodes` + `AttachNodeOccupied` + `validate_occupancy`; `plan_radial` synthesizes def-local `surf:` ids that `scaled_attach_node` re-derives on resize (stretch-follows test at csp-ui:4384).
Caveat: the click-channel gestures *around* these flows are broken by F1/F2 — the fixes' session semantics are correct; the input path to two of them is not.

## Playability walkthrough (3-stage + chute + canister, documented flows only)
Root: palette click → click build area — works (empty-craft click places root; no parts to occlude the pick). Stack parts: palette **drag**-release on caps works; craft-tree click works (auto axial); palette **click** + click on the craft — the advertised gesture — dead (F1). Side mounts (chute, canister): drag-drop + X symmetry works; `End` mode mounts by an authored node. Staging: click parts (unselected ones pick fine), per-part stage DragValue, +/−/compact; Space deploys chutes per 7dd4b86. Δv/TWR/burn/cost/profile readouts update live after every edit (dirty-gated `recompute` in `vab_editor`, both `Presentation` and `UiBuild` call it). Save/load round-trips with stable ids (CraftFile serializes per-node ids; name validation blocks traversal; demo build refuses IO at panel+session+app layers). Dead-end states: every carry has Esc/right-click/banner/put-back exits (test `held_part_exits`); refused ops keep the arm (`failed_pending_attach_keeps_the_arm`); no unrecoverable state found. The only playability scar is F1/F2: a player who follows the banner's instruction gets a silent nothing.

## PRD coverage table (M3, PLAN.md:1497; §3.7 editor presentation)
| Requirement | Status | Evidence |
|---|---|---|
| Part snap graph | WIRED | authored nodes, size-class gate, occupancy, synthesized surface nodes (csp-phys/session.rs + csp-ui planner) |
| Staging | WIRED | per-part stages, insert/remove/compact, severing via analysis, firing-order rows |
| Scaling | WIRED | 16/16 parts `Continuous`, live inspector slider + stretch, resolver exponents |
| Variants | PARTIAL | mechanism WIRED end-to-end; zero multi-variant content (Finding 4) |
| `BuiltPart`+`EffectivePart` resolver, stubbed wear | WIRED | csp-phys resolve (depth in C13) |
| Live Δv/TWR/burn-time | WIRED | dirty-gated `recompute`; per-stage + totals + SL; profile check |
| `MissionProfile` stub | WIRED | editable targets, evaluate, confidence line |
| Craft save/load, stable `PartId`s | WIRED | CSPCRAFT bincode, per-node ids, atomic store, demo gates |
| GPU picking | DEGRADED | pipeline + epoch stamp real; decoration in the pick stream breaks click-commit gestures (Finding 1) |
| VAB ≤16 ms @300 parts REF-A | PARTIAL | dedicated test with a real 16 ms release assert, but never run at release in CI (Finding 5) |
| §3.7 editor presentation | EXCEEDED | placeholder-primitives floor was the M3 bar; kitbash + studio env + baked models shipped |

## Top 3 issues
1. **[HIGH] F1** — pick-0 decoration in the GPU pick stream kills the advertised click-to-place and click-selected-to-lift gestures (empirically verified).
2. **[MEDIUM] F2** — click-armed part + viewport drag is a fully swallowed gesture (no orbit, no drop, no pick).
3. **[MEDIUM] F3** — every undo/redo resets camera zoom/pan via the shared `craft_epoch` bump.
