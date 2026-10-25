# Review Report 9: CraftSession state machine (attach/detach/staging)

File: crates/csp-phys/src/session.rs (3,143 lines)
Focus: state-machine correctness, staging, decouple, rollback, determinism, dangling ids
Status: COMPLETE

### [SEVERITY: HIGH] Finding 1: Surface-mount (`surf:`) snap points are frozen at mount-time scale — resize never carries radial attachments, silently

**File**: crates/csp-phys/src/session.rs:762-784 (passthrough at 768-770), 1449-1495

**Problem**: `scaled_attach_node` returns a decoded `surf:` node verbatim — "decode applies no further scaling" — because the VAB bakes the mounting part's *current* scale into the stored id (`crates/csp-ui/src/vab.rs:1470-1472`: `let r_p = collider_support_xy(&parent_def.collider, u) * p_scale;` → `surface_node_id(parent_pos, u, mount_dia)`; child side likewise `* built.scale` at 1482-1487). When `set_scale`/`set_stretch` later resize either side, `resnap_self_and_children` re-evaluates the snap against that frozen point, so the mount neither moves nor resizes.

**Evidence**: session.rs:768-770:
```rust
if let Some(n) = decode_surface_node(node_id) {
    return Some(n);
}
```
resnap_node (1467-1481) feeds this straight into `snap_transform` with no knowledge of the scale at which the `surf:` id was authored. This contradicts the method's own contract at 1484-1489: "a part that grows or stretches carries its attachments' snap points with it" — true for authored axial nodes (position/diameter scale via `scaled_attach_node`), false for every `surf:` edge. There is no compensating re-derivation anywhere in the VAB (`VabAction::SetScale` at vab.rs:813-821 calls `session.set_scale` directly; `collider_support_xy`/`collider_z_extent` are used only at plan time).

**Impact**: Player-visible geometry corruption through a normal inspector edit: grow a core stage 2x with radially-mounted boosters → the boosters re-snap bit-identically to the old radius and end up buried inside the hull (or hanging off a gap after a shrink); `set_stretch` similarly leaves the frozen `z` behind. The craft still saves and launches — the weld consumes topology + transform only — so the corruption ships into flight. Worse, it is *silent* for `Side` mounts: both synthesized diameters are frozen equal, so `validate`'s `AttachDiameterMismatch` never fires (only `End` mounts, whose authored child node scales, get the warning).

**Suggestion**: Change the convention so `surf:` ids store def-local (unscaled) coordinates and have `scaled_attach_node` apply the same `(s, s, s·t)` position / `·s` diameter scaling it applies to authored nodes — it already receives the right per-side scale/stretch for both the parent and child ends. That requires a semantics/version bump (`CRAFT_SCHEMA_VERSION`, per the §7.7 refuse-don't-migrate rule) since old files carry pre-scaled points. Interim honesty fix: emit a warning (or refuse the resize) when a resized node has `surf:`-mounted children, so the editor stops claiming attachments follow resizes they don't follow.

### [SEVERITY: MEDIUM] Finding 2: `from_file` reachability scan is O(n^2) and runs before any part-count cap — a crafted large file wedges the editor

**File**: crates/csp-phys/src/session.rs:2042-2053

**Problem**: Cycle detection re-scans the entire node map once per processed node.

**Evidence**: session.rs:2044-2050:
```rust
while let Some(p) = frontier.pop() {
    for (id, n) in &nodes {
        if n.edge.as_ref().is_some_and(|e| e.parent == p) && seen.insert(*id) {
            frontier.push(*id);
        }
    }
}
```
Each pop is a full `BTreeMap` iteration, so a well-formed-but-large file costs n^2 node visits. The consumer's only cap arrives too late: `crates/csp-ui/src/vab.rs:962-964` runs `CraftSession::from_bytes(bytes)` to completion and only then calls `check_part_count(session.len())`, so the quadratic walk always executes first. (The rest of `from_file` — duplicate/edge/root checks — is linear or O(n log n); only this loop is quadratic.)

**Impact**: A crafted craft file (only reachable in `file_io`-enabled builds) with, say, 200k nodes decodes in milliseconds but then performs ~4x10^10 ordered-map iterations — tens of seconds to minutes of frozen UI before the file is rejected for the part cap. Normal editing is unaffected (the op-built tree is already acyclic and the demo cap is ~60), which is why this is MEDIUM and not higher.

**Suggestion**: Build the parent->children index in one pass (the same `BTreeMap<PartNodeId, Vec<PartNodeId>>` pattern `body_transforms` at 1081-1086 and `readout::analyze` already use) and BFS over it — O(n log n) total. Optionally have `from_bytes`/`from_file` reject node counts above `IndexOverflow`-adjacent sanity bounds so structural validation never runs on absurd inputs.

---

## Checked and clean (so the coordinator doesn't re-spend)

- Ops atomicity: `add_part`/`add_root`/`reattach` validate via `&self` before any mutation; `next_id` advances only on success (test-verified). No half-attached states.
- Cycle safety: `is_ancestor_of` + tree invariant; `from_file` refuses dangling parents, multiple roots, root-with-edge, and cycles with deterministic first-error.
- `reroot`: snapshot-then-reverse is correct; inverse transforms valid by construction; body poses preserved (hand-verified the compose fold order matches `body_transforms`).
- Staging: `stage_order` is a deduped descending set — double-fire impossible; `insert_stage` cap check is airtight (no saturation merge); `report_stage_gaps` breaks at the 8-warning cap, no u32 overflow (`pair[0] < pair[1]` always); `compact_stages` can only lower indices.
- `missing_propellants` sever model matches the documented fire-and-separate semantics (same-stage decoupler feeds the engine, decoupler body stays parent-side).
- Id lifetime: ids never recycled; `next_id` round-trips and collides-refuses on load; VAB clears selected/move_node/press_node against the removed set.
- Determinism: BTree everywhere, ascending-id canonical order, `libm` transcendentals, Rust f64 Display for `surf:` ids is shortest-round-trip and platform-stable.
- No undo/rollback path exists anywhere (grep-confirmed), so nothing to review there.

## Verdict

The state machine core is unusually solid — atomic ops, typed errors, airtight load validation, real cycle/cap discipline. Two real defects: one HIGH doc-contradicting geometry bug (surface mounts frozen at mount scale through resizes, silent for Side mounts) and one MEDIUM quadratic load path a crafted file can trigger ahead of the cap check. REQUEST CHANGES on the HIGH; the MEDIUM can follow.
