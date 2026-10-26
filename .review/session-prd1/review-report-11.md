# PRD-11 — VAB/Editor UX review

**Cluster files:** `crates/csp-ui/src/vab.rs` (4418), `crates/csp-view/src/vab.rs` (1319), `crates/csp-render/src/{vab.rs,vab.wgsl}` (451/107), `crates/csp-view/tests/vab_latency.rs` (165). Supporting: `crates/csp-phys/src/session.rs`, `crates/csp-phys/src/readout.rs`, `crates/csp-app/src/lib.rs`, `data/parts/*.ron`.

**PRD baseline (PLAN M3 row):** part snap graph · staging · scaling + variants · `BuiltPart`/`EffectivePart` resolver · live Δv/TWR/burn-time · `MissionProfile` stub · craft save/load with stable `PartId`s · GPU picking · latency ≤16 ms at 300 parts.

**Verdict:** the M3 checklist items all exist and are wired — snap graph, staging, per-part scale+stretch+variant, live per-stage Δv/TWR/burn (honest Tsiolkovsky with fuel drawdown and decoupler severing — `readout.rs:143-382`), `MissionProfile` check, CSPCRAFT files, GPU object-id pick, and the 16 ms test. **But both verified user complaints are real and traceable to concrete code**, and several KSP-table-stakes editor features are absent (undo, free reposition, part search, action groups, subassemblies, auto-staging).

---

## The two complaints, decoded

### (a) "Can't move the rocket however I want" — CONFIRMED

Three compounding facts:

1. **The root refuses Grab** — `VabSession::grab` returns "the craft root can't be picked up — it defines the build frame" (`csp-ui/src/vab.rs:1087-1092`). The root is the *first part the player placed* — usually the most prominent part of the rocket — so the most natural "grab the rocket" gesture is refused by design (encoded in test `move_cancel_and_self_drop_put_it_back`, vab.rs:3373+).

2. **Dragging the root is a fully dead gesture.** `vab_pointer_input` sets `orbit_block` whenever the press began on *any* part (`press_node.is_some()` → `orbit_block = primary_down && over_viewport && (carried || press_node)`, `csp-ui/src/vab.rs:2101-2103`), which makes `apply_vab_input` discard Orbit deltas (`csp-view/src/vab.rs:165`). So LMB-drag on the root: not a part move (grab refused), not a camera orbit (blocked), and — because `carried()` stays `None` — `Grab(root)` is re-applied and refused *every frame* of the drag (`vab.rs:2108-2114`), spamming the notice. The gesture does literally nothing visible.

3. **There is no whole-craft move op at all.** The `VabAction` vocabulary (`vab.rs:152-259`) has no translate/offset action; `CraftSession::set_transform` (the only API that could move the root — `csp-phys/src/session.rs:1441-1455`) is unreachable from the editor (test-only callers). The sanctioned workaround — place a sacrificial part on top, `Reroot` to it, then grab the old root — requires discovering "make root" buried in a tree context menu (`vab.rs:2691-2694`) / inspector button (`vab.rs:3128-3130`), and `reroot` intentionally preserves all body-frame poses (`session.rs:1364-1436`), so the craft never *moves*, only its tree direction flips. Moves are snap-graph reattachments only; there is no free offset/rotate/nudge tool for placed parts either.

### (b) "Removing a piece glues the whole stack to the ground so I can't put shit below" — CONFIRMED (two distinct mechanisms)

1. **`remove_part` deletes the whole subtree — and there is no undo.** `CraftSession::remove_part` removes `subtree_ids(id)` (node + all descendants; `session.rs:1304-1316`); removing the root empties the session entirely (`root = None`, session.rs:1312-1314). There is no "detach children" option and **no undo/redo anywhere in the codebase** (grep `undo|redo` — zero hits in csp-ui/csp-view). Right-click over the viewport deletes the hovered part *instantly* (`vab_pointer_input`, `vab.rs:2143-2148`), `Del`/`Backspace` deletes the selected subtree (`vab.rs:2156-2158`), and dragging a moved part over the palette deletes it (`vab.rs:2120-2125`). Removing a mid-stack part silently destroys everything mounted through it — with no recovery path. "I removed one piece and the whole bottom of the rocket vanished" is exactly this.

2. **The decorative hangar floor makes the stack look glued down AND occludes the bottom cap.** `emit_environment` draws a lit floor disc + contact blob + grid at `floor_z = min_z - 0.02·extent` — i.e., always *just under the lowest hull point* (`csp-view/src/vab.rs:851-870`), so after any removal the "ground" rises with the new lowest part: the stack always reads as planted on the ground. Worse, the environment verts are emitted into the **same vertex stream the GPU pick pass rasterizes** (`extract_vab`, `csp-view/src/vab.rs:779`; `encode_pick` draws `packet.vertices` with depth-write `Greater`, `csp-render/src/vab.rs:320-353`, depth state at :141-146). From beneath the craft the floor wins the depth test in **both** passes: the bottom cap is *invisible* and its click decodes as `pick = 0` → `Select(None)` → placement does nothing (or deselects). The only escape is that the CPU hover ray ignores the floor (`hover_pick`, `csp-view/src/vab.rs:656-693`), so a *blind* drag-drop (palette-drag → release over the hidden cap) still attaches, and `mount: stack` mode + a side-face click also reaches the bottom node — but the canonical gesture (orbit under, click the bottom cap) is defeated by decoration. The earlier pitch-clamp fix (commit `396e12c`, `PITCH_LIMIT = ±π`, `csp-view/src/vab.rs:47`) made the camera reach underneath; the floor then re-occluded the target it freed.

   *Note:* the test-comment at `csp-view/src/vab.rs:1234-1235` literally quotes this complaint ("can't place shit below") as fixed by the orbit clamp — the fix is real but incomplete because the occluder was added afterwards.

---

## Findings

### HIGH

**H1 — Bottom-cap attachment is occluded by the decorative floor in both color and pick passes.**
`csp-view/src/vab.rs:833-1012` (`emit_environment`), `:779` (env emitted first into `packet.vertices`); `csp-render/src/vab.rs:320-356` (pick pass draws the same stream with `depth_write_enabled: true`, `Greater` reverse-Z — `:141-146`).
Problem: the floor disc (`r_in = extent·3`, clamped ≥ 6 m — `:898`) always sits `~2%` of craft extent below the lowest hull point (`:854-870`). Any eye below `floor_z` sees the floor's underside and the bottom cap is behind it — unviewable; the same depth ordering in the pick pass means a click on the occluded cap decodes as background (`VabEvent::Pick(None)` → `Select(None)`).
Evidence: camera can reach pitch ±π (`:47`, test `:1237-1264`); CPU `hover_pick` tests only part AABBs (`:665-692`) so `Drop` still works blind.
Impact: matches the verified complaint "can't put shit below" — the standard gesture for extending a stack downward fails silently.
Suggestion: mark the environment `pick = 0` *and* skip it in the pick pass (split env/part vertex streams or tag verts and discard in `fs_pick`), and either make the floor non-depth-occluding for parts (draw env with depth-write off / `Greater`-equivalent bias), shrink it, or drop `floor_z` below `min_z` by a usable margin (e.g., `extent·0.5`) — the camera then clears it.

**H2 — Remove is subtree-delete with no undo; three independent gestures delete instantly.**
`csp-phys/src/session.rs:1304-1316` (remove = subtree; root remove = wipe); `csp-ui/src/vab.rs:2143-2148` (right-click remove), `:2156-2158` (Del/Backspace), `:2120-2125` (palette drop = delete), `:2696-2699`, `:3131-3133`.
Problem: no `Undo`/`Redo` in the action vocabulary, no session snapshots, no confirmation. Right-click doubles as "cancel carry" vs "delete subtree" depending on invisible state (`vab.rs:2143-2146`). Removing the root deletes the entire craft.
Impact: one misclick can destroy a long build — the dominant "the editor ate my rocket" risk, and it amplifies complaint (b): removing a mid part deletes everything hanging from it.
Suggestion: keep a bounded undo stack of `CraftSession` clones (session ops are already centralized in `apply` — snapshot before mutating arms), wire Ctrl+Z/Ctrl+Y; consider a "remove part only" reparent option or at least a subtree-count confirmation for large subtrees.

**H3 — The craft root can never be moved; drag-on-root is a dead gesture; no whole-craft reposition op exists.**
`csp-ui/src/vab.rs:1081-1102` (root grab refusal), `:2101-2114` (orbit_block + per-frame refused Grab), `:152-259` (no translate in the vocabulary); `csp-phys/src/session.rs:1441-1455` (`set_transform` unused by editor).
Impact: matches "can't move the rocket however I want". Players cannot lift the whole stack to slide parts underneath, cannot nudge/offset placed parts, and the most intuitive drag target (the first placed part) produces zero feedback except a spammed notice.
Suggestion: let root Grab lift the whole craft and drop it back (a no-op reattach is already the semantic), or add a root translate op through `set_transform`; at minimum, exclude the root from `orbit_block` when Grab is refused so the drag orbits the camera instead of going dead, and rate-limit the refusal notice.

### MEDIUM

**M1 — LAUNCH on an empty session silently spawns a hidden stock craft.**
`csp-view/src/lib.rs:2069-2071` (`if session.is_empty() { return Ok(stock_flight_craft()); }`, hand-built 2-part debug craft at `:3010+`).
Impact: a player who wiped their craft (see H2) or never built one presses LAUNCH and flies a rocket they never made — reads as "the editor isn't connected to the game".
Suggestion: refuse with "craft is empty — build something first" (the `EmptySession` validation error already exists, `session.rs:1755`); gate the stock craft behind a demo/debug flag.

**M2 — An axial plan can land on the opposite face from the click.**
`csp-ui/src/vab.rs:1425-1466` — `side_match` is only a sort preference; when every node on the clicked side fails the diameter/preview gate, a pair on the *other* side is committed. A bottom-cap click can mount the part on top of the same part.
Impact: placement violates the pointed-at affordance; only the ghost preview (if noticed) reveals it.
Suggestion: restrict axial candidates to the clicked side when a cap region is hit; return the existing "no free attach node on that side" error otherwise.

**M3 — No part-overlap/intersection checking anywhere in placement.**
`plan_radial` (`vab.rs:1475-1598`) checks surface-spot occupancy by exact float-encoded `surf:` id (`:1551`) — two mounts at nearly-identical points produce different ids and both succeed; nothing bounds overlap for any mount type. `validate` (`session.rs:1748-1832`) has no geometric check.
Impact: parts can be stacked inside each other — invisible correctness hole that also lets surface-mount spam accumulate in one spot.
Suggestion: quantized surf-position occupancy (e.g., 5 cm cells) and/or an AABB overlap warning in `validate`.

### LOW

- **L1 — Stale-pick epoch guard doesn't cover edits** (`csp-app/src/lib.rs:833-852`, `csp-render/src/vab.rs:77-85`). `craft_epoch` bumps only on `New`/`LoadBytes` (`vab.rs:825, 1030`); a part add/remove between encode and readback isn't an epoch change. Safe in practice — `PartNodeId`s are never reused (`session.rs:1304` note + monotonic `next_id`) and `Select` filters dead ids (`vab.rs:1046`) — but the comments claim a guarantee the mechanism doesn't provide. Doc/claim fix.
- **L2 — Contradictory `surf:` doc** (`session.rs:698-700` says stored values are "already at the node's built scale+stretch, decode applies no further scaling"; `scaled_attach_node` at `:773-783` scales them, and `plan_radial` divides scale out at encode time — def-local). Doc bug only.
- **L3 — Symmetry limited to ×1/×2/×4, new placements only** (`vab.rs:882-888, 2188-2195`; `plan_move` hardcodes `copies=1`, `:1308`). KSP parity would be 1,2,3,4,6,8 and symmetric *moves*/copies of existing parts.
- **L4 — VAB hotkeys bypass the rebindable keymap** — `Del`/`Backspace`/`Esc`/`X` are hardcoded (`vab.rs:2054-2066`), inconsistent with M10's rebindable keys.
- **L5 — Stage DragValue caps at 64** (`vab.rs:3021`) vs `MAX_STAGE_INDEX = 999` (`session.rs:182`) — harmless ceiling mismatch.
- **L6 — `orbit_block` drops mid-drag over a panel** (`vab.rs:2101-2103` uses *current* `over_viewport`, not the press anchor) — camera orbit resumes while a carried part is dragged over the palette; press-anchor should own the flag for the gesture's life.
- **L7 — Refused root Grab re-fires every frame of a drag** (`vab.rs:2108-2114`) — notice spam; fold into H3 fix.

### Missing vs the PRD editor spec (gaps, not bugs)

- **Action groups** — §2.10's `ControlApi` spec lists "action groups" among the mutation surface; no `ActionGroup` concept exists anywhere (grep zero). Not strictly an M3 line item, but a listed spec feature — absent.
- **Subassemblies** — no save-a-subtree/reuse-a-subassembly path; `CatalogPick`/`PlaceAtHover` handle single parts only; the tree can't even hold a second root, so building a booster offline is impossible.
- **Part search/filter** — palette is category-grouped only (`vab.rs:2432-2464`); no text search with 11 parts it's tolerable, but the catalog will grow.
- **Auto-staging** — every part lands in stage 0 (`session.rs:1129-1131`); no "engines+their decoupler group by default" assist; staging is fully manual via DragValue.
- **No rotate/offset/gizmo tool** for placed parts — repositioning is attach-node-only; `set_transform` exists but is unexposed (H3).
- **Undo/redo** — see H2.
- **No symmetry-on-existing-part, no copy/paste, no duplicate-subtree.**

## What's verified good (no action)

- Radial/side attach is implemented: `AttachMode::{Auto,Axial,Side,End}` (`vab.rs:64-79`), synthesized `surf:` nodes with azimuth/height from the live hit (`:1475-1598`, `session.rs:701-798`), action-bar toggles + `X` cycle (`:2179-2229`). The play1 "no radial attach" complaint is fixed.
- Δv readout is honest: per-stage Isp-weighted Tsiolkovsky with propellant drawdown, decoupler severing, SL + vac numbers, burn time, launch TWR, mission-profile margin, unqualified-family confidence (`readout.rs:143-382`, shown at `vab.rs:2761-2815, 2960-2965`). The only documented gap is the massless-propellant latent edge (`readout.rs:270-281`) — no authored engine triggers it.
- Move keeps id/build/subtree, self-drop and subtree-drop put back safely, cycle refusal is typed (`vab.rs:1159-1185`, `session.rs:1323-1351`).
- `reroot` is geometry-preserving by construction (`session.rs:1364-1436`, tested).
- Latency criterion test exists and measures the real path (`tests/vab_latency.rs`, 300 parts, 16 ms release / 200 ms debug gate).
- Tech-lock gating is consistent across palette arm, drop, and stock build (`vab.rs:962-972, 1140-1150, 1641-1650`); loc fallback honest (`:517-526`).
- Kitbash+model rendering, selection outline, attach-node markers, ghost green/red/live-refusal read are all coherent.

## Suggested fix order

1. **H1** — floor out of the pick pass + depth-bias/lower it (unbreaks "put below" for ~1 line of vertex tagging + one pipeline tweak).
2. **H2** — undo stack on `VabSession::apply` (cheap: `CraftSession` is `Clone`).
3. **H3** — root move/orbit unblock + root translate (kills "can't move the rocket").
4. **M1** — refuse empty launches.
5. M2/M3, then the L-list and spec gaps (undo-adjacent: confirmation for subtree deletes).
