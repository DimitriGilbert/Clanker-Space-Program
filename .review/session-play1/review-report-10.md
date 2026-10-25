# P10 Review — VAB usability (palette, placement, files, notices)

Scope: `crates/csp-ui/src/vab.rs`, `crates/csp-view/src/vab.rs`, `crates/csp-app/src/lib.rs`
(craft IO + GPU pick), `crates/csp-render/src/vab.rs` + `vab.wgsl`, dep-read of
`crates/csp-phys/src/session.rs` (`add_part`, `snap_transform`, `bound_attach_nodes`,
`to_bytes`/`from_bytes`) and `crates/csp-view/src/{lib.rs,demo.rs}` call sites.

## Verdict

The core flow is genuinely usable and discoverable: the palette banner states what the
next click does in every state, `CatalogPick` arms pending placement with three cancel
paths + a banner button, every op lands a notice that is mirrored on the viewport hint
bar, and the build flow is regression-tested (`pending_placement_cancels_three_ways`,
`failed_pending_attach_keeps_the_arm`, `catalog_pick_drives_the_build_flow`,
`attach_auto_stacks_engine_nozzle_down` — all pass; `cargo test -p csp-ui vab`: 10/10;
`-p csp-app`: 31/31; `-p csp-view` `vab_latency`: 1/1).

**No un-cancellable state exists.** I traced the invariant
`pending_part.is_some() ⟹ self.selected.is_none()`: `self.selected` is only written by
`place_root`/`place_attach` (both clear the arm) and `select`'s non-consume branch
(which sets it to `None` while armed). So `catalog_pick`'s `selected()` branch can never
shadow the re-click-to-cancel branch, and the armed row's toggle is always reachable.
Exits verified: `Select(None)` (background pick or "deselect"), `CancelPlacement`
(banner button), re-click armed row, `New`, `LoadBytes`, `place_root` after the craft
empties. A failed attach keeps the arm *and* keeps `selected` `None`, so retry and
cancel both stay live.

Verified clean per the focus list:

- `attach_auto` ordering (vab.rs:729–809): `bound_attach_nodes(parent)` correctly
  excludes both the parent's own child-node (the node it hangs by) and parent-side
  nodes bound by existing children (`session.rs:941–952`); `facing =
  -parent_t.apply_dir(pn.direction).z` measured in body frame via the ancestor-chain
  fold; `twist = cn.direction.dot(-pn.direction)` is exactly the cosine of
  `snap_transform`'s `rotation_arc` argument (`session.rs:742`) — least-twist tiebreak
  is correct. Stable sort → deterministic authored-order fallback.
- `validate_craft_name` runs before *both* halves (`lib.rs:961–983`): refused names
  reach neither `save_craft_file` nor `craft_store.insert`; `load_craft_store` applies
  the same rule to hand-placed files (`lib.rs:2142`). Test `unsafe_craft_names_are_refused`.
- `file_io=false` enforced at both layers: session `load_bytes` → `FileIoDisabled`
  (vab.rs:624) and app outbox `self.demo` gates on `craft_save`/`craft_load`
  (lib.rs:947, 1062). `app.demo = view.is_demo()` (`lib.rs:340`) so the two flags can't
  desync; `to_bytes`/export additionally hidden at the panel and dropped at the outbox.
- `craft_epoch` refit (view/vab.rs:233–237): bumped by `New`/`LoadBytes`
  (ui/vab.rs:487, 632), re-arms `focused_once` and drops stale `pan_offset`; empty
  crafts keep `focused_once=false` so the first placed part still frames correctly.
- Pick → pending-placement consume: `Pick` → `VabAction::Select` → `select()` consumes
  `Some(node)` as the parent pick, `None` disarms (view/vab.rs:157–161,
  ui/vab.rs:642–654). `pick_decode_ids` is snapshotted at encode so a replaced
  `last_vab_packet` can't mis-decode (`lib.rs:1279–1284`); `pick_ids` are 1-based and
  `checked_sub(1)` keeps 0 = background (lib.rs:748–750, view/vab.rs:353–355).
- The pick pass has its own depth target and honors occlusion (render/vab.rs:261–267) —
  the pixel you click is the pixel you see.

## Findings

### F1 — LOW — A click during an in-flight pick is silently dropped, including cancel clicks

`crates/csp-app/src/lib.rs:1264–1267`

```rust
if let Some((x, y)) = self.pending_pick.take() {
    if self.pick_in_flight {
        // One outstanding pick — drop the request; the next
        // click retriggers.
    } else if x < gpu.config.width && y < gpu.config.height {
```

**Problem:** while one GPU readback is outstanding (`pick_in_flight`), a second click's
`pending_pick` is taken and discarded — no encode, no `Pick(None)` fallback either
(`resolve_background` is only set in the `else` arm).

**Impact:** the armed placement's documented "click empty space to cancel" (and any
select) can be eaten if the player clicks twice inside the ~1–2 frame readback window.
The gesture vanishes with zero feedback — the exact "dead click" class this cluster was
fixing, just rarer. Note the window is small and the comment acknowledges the trade-off.

**Suggestion:** keep the *latest* click queued instead of dropping (`self.pending_pick =
Some((x,y))` and re-take next frame — the field is already `Option`), or resolve the
dropped click as background (`Pick(None)`) so at minimum a cancel gesture still lands.

### F2 — LOW — Out-of-bounds / unre-encodable picks produce no event at all

`crates/csp-app/src/lib.rs:1268, 1289–1291`

**Problem:** `else if x < gpu.config.width && y < gpu.config.height` — a click whose
release position falls outside the (possibly just-resized) surface takes
`pending_pick` and emits nothing: no encode, and `resolve_background` is never set
because the whole `if` body is skipped.

**Impact:** rare (resize between press and release), but the click is a dead drop —
no select, and no cancel of an armed placement. Inconsistent with the file's own
"click empty space cancels — even when nothing can rasterize" contract at L1254–1260,
which is otherwise implemented correctly (empty packet / missing renderer →
`Pick(None)` at L1302–1307).

**Suggestion:** treat the out-of-bounds arm like `resolve_background = true` —
the pixel is definitionally background to the pick pass.

### F3 — LOW — A stale pick id collapses into "background click": deselects and disarms a pending placement

`crates/csp-view/src/vab.rs:157–161` + `crates/csp-ui/src/vab.rs:642–654`

**Problem:** `Pick(Some(id))` → `Select(Some(PartNodeId(id)))` → `select()` filters
`sel` by `session.contains`. A node that no longer exists filters to `None`, which is
then indistinguishable from a genuine background click: `self.selected = None` and
`pending_part.take()` disarms with "placement cancelled". Two stale cases:

1. The node is removed between rasterize and readback (`remove_part` never recycles
   ids, so it becomes a dead id) — a parent-pick that would have attached instead
   cancels the arm.
2. `New`/`LoadBytes` replaces the session between encode and readback; a loaded craft
   can legitimately contain the *same* `PartNodeId` for a different part — the stale
   pick then *selects/attaches under the wrong node* rather than being dropped.

**Impact:** narrow (1–2 frame window crossed by an op), but case 2 is a wrong-part
attach with no error path.

**Suggestion:** stamp the pick request with `craft_epoch` (or a packet/scene
generation) at `pending_pick` time and drop the decode when it no longer matches —
`VabInput.craft_epoch` already exists for exactly this "the craft under the camera
changed" class.

### F4 — LOW — `lmb_press` seeds from `last_cursor`; a click with no preceding `CursorMoved` is silently lost

`crates/csp-app/src/lib.rs:1966–1981`

```rust
self.lmb_press = if consumed { None } else { self.last_cursor };
```

**Problem:** winit's `MouseInput` carries no position, so the press position is read
from `last_cursor` — which is `None` until the first `CursorMoved`. A press+release
with no intervening move (first-ever click, touch/tap input that synthesizes button
events without a move) leaves `lmb_press = None` → release takes nothing → no pick.
The drag check also measures from the last move, not the press point — same value in
practice, but the press position is what's intended.

**Impact:** narrow on desktop (a move almost always precedes a click), more plausible
on wasm touch. The click does nothing at all — no select, no cancel.

**Suggestion:** low-priority hardening: if `lmb_press` is `None` at release, still
treat an unconsumed in-VAB release as a background pick (`pending_pick` at
`last_cursor` when available) so the cancel path isn't gated on cursor-move history.

### F5 — LOW — `to_bytes` failure silently drops the save request

`crates/csp-view/src/lib.rs:452–456`

```rust
outbox.craft_save = if let Some(name) = vab_out.save_name {
    vab.0.to_bytes().ok().map(|bytes| (name, bytes))
} else {
    None
};
```

**Problem:** a `to_bytes` error maps `craft_save` to `None` — the outbox carries no
request, the app sees nothing, and no notice is produced. The Save click is a dead
drop, violating the "every action produces a visible result or a clear notice" rule
the rest of the cluster implements carefully.

**Impact:** practically unreachable — `to_bytes` only fails on a bincode encode error
(`session.rs:1749`), which an in-memory `Vec` writer can't produce for this schema.
Code-level honesty gap, not a live bug.

**Suggestion:** on `Err`, push a `VabEvent::Notice`/session `set_notice` naming the
failure instead of `.ok()`-ing it away.

### F6 — LOW — The palette flow has no upward-attachment path: every `attach_auto` mate prefers the parent's lowest free node

`crates/csp-ui/src/vab.rs:774–793`

**Problem:** `facing` sorts parent nodes most-downward-first unconditionally — correct
and tested for the pod→tank→engine stack — but it means a cap-type part (nosecone, a
pod you want *on top of* a tank root) can never attach above the parent while any
lower node is free. The session vocabulary offers no "attach here/up" op, and
`Reroot` preserves geometry, so an under-placed part can't be flipped to the top
without a rebuild.

**Impact:** the dominant root-is-the-top flow is unaffected, and the banner/notice
makes the resulting placement obvious. But "click part → it goes somewhere sane" only
holds downward; for top-fitting parts the honest result is geometrically wrong and
unfixable inside the gesture set. This is the residual gap behind "is the flow
genuinely usable".

**Suggestion:** design-level, not a blocker — when true per-node picking lands (or a
"fit above/below" modifier), the candidate filter already has the machinery to honor
it. Until then the `NoCompatibleAttach`-vs-"wrong side" distinction is worth keeping
in mind if more cap/aero parts ship.

### F7 — NIT — "Save" with an empty `save_name` is a silent no-op

`crates/csp-ui/src/vab.rs:1326–1328`

**Problem:** `if ui.button("Save").clicked() && !vab.save_name.is_empty()` — the button
is enabled, consumes the click, and produces no request and no cue.

**Suggestion:** `ui.add_enabled(!vab.save_name.is_empty(), egui::Button::new("Save"))`
or surface a "name the craft first" notice.

### F8 — NIT — Whitespace-only save names pass `validate_craft_name`

`crates/csp-app/src/lib.rs:2170–2184`

**Problem:** `" "` is non-empty and contains no separator/control char → writes
` .cspcraft`. Round-trips consistently (stem → same store key), so no divergence, but
a space-named craft file is a smell; on Windows trailing-space stems are also
filesystem-mangled. Similarly, Windows reserved stems (`con`, `nul`) aren't rejected —
they fail honestly in `save_craft_file` with a notice, so it's cosmetic.

**Suggestion:** `name.trim().is_empty()` → refuse as empty.

### F9 — NIT — Armed-placement banner text goes stale when the craft empties

`crates/csp-ui/src/vab.rs:1120–1131` + `catalog_pick` ordering (575–601)

**Problem:** `Remove` doesn't clear `pending_part` (intended — arm survives edits),
so removing the whole craft leaves "placing X — click the craft part to attach it
under" displayed while the next catalog click actually does `place_root`. Works
correctly (root is placed, arm cleared), just mislabeled for one step.

### F10 — NIT — Armed row's hover verb doesn't admit re-click cancels

`crates/csp-ui/src/vab.rs:1054–1063`

**Problem:** the armed row is highlighted but its hover text still reads "click to
start placing…" — clicking it cancels. Discoverability nit for the third cancel path.

### F11 — NIT — `NoCompatibleAttach`/session errors print `PartNodeId` Debug

`crates/csp-ui/src/vab.rs:181–184`

**Problem:** the notice shows `under PartNodeId(3)` — honest but the raw wrapper leaks
into player-facing text; `node_label` exists for this (`#3`).

## Coverage gaps worth noting (no defect found)

- `pending_part` armed + `LoadBytes`/`New` mid-flight clears the arm via session
  replacement — correct; `pending_part` is never serialized into the craft file
  (`to_bytes` serializes `CraftSession` only). ✓
- `craft_load` is deliberately *not* `read_only`-gated (`lib.rs:1061–1077`) — loading
  edits §2.2-C session state, not the halted sim; consistent with the §2.13 policy
  that gates writes (`craft_save`) and commands (`launch`, `maneuver`). ✓
- `craft_files` list is seeded in `finish_gpu_init` (`lib.rs:706`) and refreshed after
  every save — disk-loaded files appear in the load ComboBox at startup. ✓
- `apply_vab_input` runs unconditionally in `Input`, so `Notice`/`LoadCraft`/stale
  `Pick` events drain even outside VAB mode. ✓
- Demo seed path uses `SetRoot`/`AttachAuto` through the same `apply` channel —
  honors `part_limit`, doesn't touch `file_io` (`demo.rs:179–212`). ✓
