# Verified Report C15 — Physics: CraftSession + merge + tests

Verification agent pass over `review-report-C15.md`, session `session-full-20261026`.
Method: every finding re-traced at source; three empirical probes run out-of-tree
(`/tmp/c15probe`, path-deps on the workspace crates, real `data/parts` catalog);
`cargo test -p csp-phys` re-run green (157 tests incl. 6 merge tests); merge probe run
in both debug and release profiles. No project file was edited.

**Score: 6 confirmed (2 with material corrections), 0 dismissed.**

---

### Finding 1: `preview_attach` omits the occupancy gate `add_part` enforces — CONFIRMED

**Original**: preview returns Ok where `add_part` refuses (`AttachNodeOccupied`); the doc
"the same checks / the drop lands exactly what was previewed" is false; MCP/design-solver
audience gets a plan/execute mismatch.

**Verification**:
- Source confirmed: `add_part` gates `bound_attach_nodes(edge.parent).contains(&edge.parent_node)`
  at `crates/csp-phys/src/session.rs:1251-1260` *before* `compute_attach`; `preview_attach`
  (`session.rs:1377-1384`) calls only `compute_attach` (`:1277-1372`), which checks def/node
  existence, direction/position finiteness, size class, and transform validity — never occupancy.
- Empirically reproduced against the real catalog: `preview_attach` on `pod.bottom` mated by a
  tank → `Ok(transform)`; `add_part` with the identical edge → `Err(AttachNodeOccupied)`.
  Control on a free node: both Ok, transforms equal. Divergence survives save/load round-trip.
- Consumer trace (`rg preview_attach`): only `crates/csp-ui/src/vab.rs:1711` (`plan_axial`) and
  `:1842` (`plan_radial`). Both pre-filter — authored nodes via `bound.contains(&pn.id)`
  (`vab.rs:1685`), synthesized surf nodes via `bound.contains(&parent_id)` (`vab.rs:1808`) — so
  the VAB never calls preview with an occupied parent node.
- The "MCP `craft.build` / §2.11 design solver" audience is entirely prospective today: no such
  caller exists. `csp-ai` is a 222-line stub (no CraftSession/BuiltPart reference), `csp-script`
  exposes no craft-session API, `csp-server`'s "attach" hits are network-attach, not attach nodes.
  The citation is the doc's own stated audience (`session.rs:327-334`).

**Verdict**: real API-contract defect on a `pub` oracle whose doc makes a false claim;
MEDIUM stands, with the scope note that no live non-UI caller exists yet — the report's own
impact framing ("any non-UI caller … will") is honest about this. The suggested fix (shared
parent-side bound check, plus the child-side variant `reattach` applies at `session.rs:1536-1559`)
is correct and cheap.

### Finding 2: `BuiltPart.data_set_hash` zeroed in the only live production path — CONFIRMED (one claim corrected)

**Original**: §3.1 per-part drift key is dead-on-arrival; `stock` zeroes it, VAB never fills it,
craft files and **saves** freeze zeros; `stock` doc "the manufacture path fills every field" false.

**Verification**:
- Confirmed: `BuiltPart::stock` zeroes `data_set_hash: [0; 32]` (`crates/csp-phys/src/resolve.rs:274-283`);
  VAB `stock_build` (`crates/csp-ui/src/vab.rs:1913`) fills `upgrade_levels_at_build` but never the hash;
  craft files serialize `built: BuiltPart` verbatim (`CraftFileNode`, `session.rs:750-756`), and edits
  re-save the zeros; `resolve.rs:2369` copies the zeros onto `EffectivePart.data_set_hash` in memory;
  nothing reads the field anywhere (`rg data_set_hash` — struct defs, the copy, tests, dormant code).
- The `stock` doc's "the manufacture path fills every field" (`resolve.rs:271-273`) is false for the
  only live manufacture path. The `resolve.rs:261-267` field doc ("plumbed now so the §7.2 record is
  already complete") describes intent, not behavior.
- **Corrected claim**: "`to_part_graph` → `ManufactureCraft` → `CraftDef` carries the zeros into the
  sim save" is **false**. `CraftDef` (`crates/csp-sim/src/bubble.rs:79-137`) carries graph +
  `WeldPart`s + `part_costs` + `reliability` + `part_ids` — no `BuiltPart`. The launch path drops
  the `BuiltPart` slice, keeping only `part_id`s (`crates/csp-view/src/lib.rs:2834-2849`). The sim
  save's only `data_set_hash` is the header-level one, which IS stamped (`csp-app/src/lib.rs:1120`)
  and compared at load with the §7.7 drift notice (`lib.rs:1174-1188`). Blast radius: **craft files
  only**, not saves.
- Minor imprecision: "zero non-test writers" — `SimState::build_part` (`crates/csp-sim/src/tech.rs:108-130`)
  does write a caller-stamped hash, but it has **zero production callers** (dormant API), so the
  operative claim (no live path stamps a real hash) holds.

**Verdict / severity reconciliation**: the defect is real but narrower than stated. The user-facing
§7.7 protection is live at save level; the per-part key is a by-design unconsumed future item
(`resolve.rs:115-118`) whose only pollution surface is editor craft files. Recommend **MEDIUM → LOW**
(borderline MEDIUM if you weight the accumulating-zeroed-legacy-files argument when the tracked
consumer lands). The one-line fix (stamp at the VAB→session boundary) and the `stock` doc fix stand.

### Finding 3: `merge_graphs` `effective_base` precondition is release-stripped `debug_assert` — CONFIRMED

**Original**: the lower bound on `effective_base` IS derivable from the primary graph alone; the
"parts slice out of signature" justification proves the wrong direction; release builds silently
alias mass properties.

**Verification**:
- Source confirmed (`crates/csp-phys/src/merge.rs:230-241`): the assert evaluates exactly
  `primary.nodes.values().all(|n| n.effective_index < effective_base)` — primary-only, so a typed
  `MergeError` return is implementable exactly as suggested.
- Empirically confirmed in **both** profiles: with a two-node primary (used indices {0,1}) and
  `effective_base = 1` (the miscomputed-caller case), the debug build panics at `merge.rs:235`;
  the **release** build returns `Ok` with the secondary root remapped to `effective_index = 1` —
  identical to primary node 1's index, i.e. two nodes aliasing one `WeldPart` slot with no error.
  In-range aliasing cannot trip the weld's index-range checks — "no check downstream can see it"
  is accurate.
- Caller safety confirmed: sole production caller passes `u32::try_from(craft_p.parts.len())`
  (`crates/csp-sim/src/joints.rs:889`), and breakup fragments keep the full parent `parts` slice
  (`crates/csp-sim/src/lib.rs:11441` — `parts: craft.parts.clone()`), so `parts.len()` ≥ every
  used index. No live bug, exactly as the report says.

**Verdict**: CONFIRMED as a latent pub-API hardening gap (`merge_graphs` re-exported at
`csp-phys/src/lib.rs:108`); MEDIUM is defensible-borderline given no live caller — the report
states this itself.

### Finding 4: `compact_stages` launders crafted out-of-cap stage indices — CONFIRMED (LOW, as filed)

**Verification**: `validate` flags `stage > MAX_STAGE_INDEX` (999) as `InvalidStage` errors
(`session.rs:1983-1989`, reachable only via crafted files since `set_stage`/`insert_stage` gate at
`:1822`/`:1883`); `compact_stages` (`:1923-1935`) remaps *every* populated stage — including an
out-of-cap 5000 or `u32::MAX` — to its rank in `0..k`, erasing the error; `remove_stage`
(`:1911-1919`) walks indices down one per call (milder, same direction). Pure 12-line function;
code-level confirmation is sufficient. LOW/hardening posture ("acceptable as-is") is sound.

### Finding 5: O(n²) child scans in `subtree_ids` / `missing_propellants` — CONFIRMED (LOW, as filed)

**Verification**: `children_ids` is a full O(n) scan (`session.rs:1115-1121`); `subtree_ids`
(`:1145-1154`) calls it per frontier node; `missing_propellants` (`:2138-2199`) calls it inside
the region flood (`:2167`), once per engine via `validate_staging` (`:2120`); the decoupler check
(`:2106`) is another per-node O(n) call. The one-pass `BTreeMap<PartNodeId, Vec<PartNodeId>>`
pattern already exists three times with the cited comments: `body_transforms` (`:1169`, comment at
`:1161`), `validate_occupancy` (`:2063-2068`), `from_file` (`:2319-2324`, comment at `:2316-2318`).
Bounded and within budget at editor scales; the "extract the helper" suggestion is the right shape.

### Finding 6: crafted-file double-bound nodes remain "launchable" — CONFIRMED (NIT/record, as filed)

**Verification**: `validate_occupancy` tallies duplicate bound ids and pushes them to **warnings**
(`session.rs:2086-2091`); `SessionWarning::AttachNodeOccupied`'s doc (`:588-598`) documents the
deliberate posture (craft welds, geometry flagged); `Validation` (`:601-609`) gates
`is_launchable` on errors only. Accurate as a coverage-table record, not a defect.

---

## Clean verdicts — spot-checks (3 of the requested 2)

1. **Attach invariants under adversarial sequences — HOLDS (empirical)**. Reproduced out-of-tree
   against the real catalog: move onto an occupied parent refused (`reattach` engine → tank.bottom
   bound by dec ⇒ `AttachNodeOccupied`); own-slot reattach exemption works (chute → its current
   pod.top/mount slot ⇒ Ok); multi-child promotion atomic — `remove_and_reparent(tank)` with
   children {dec, radial surf chute} refused (`ReparentFailed`, tank still present, occupancy
   clean), and succeeds after freeing pod.top, with zero occupancy warnings and zero errors after;
   occupancy clean at every step including after reroot.
2. **Craft-file byte-canonical round-trip — HOLDS (empirical + existing test)**.
   `save_load_preserves_ids_and_bytes_are_canonical` (`crates/csp-phys/tests/session.rs:129-175`)
   green in the suite; the probe additionally confirmed save→load→save byte fixed point and full
   session equality (`PartialEq`) through the promotion/reroot sequence, with id holes preserved.
3. **M6 bit-identity + id remapping — HOLDS (test + source)**.
   `merge_weld_is_bit_identical_to_from_scratch_weld` (the M6 exit criterion, PLAN L314-322) and
   `thousand_part_station_merges_into_one_body` re-run green. Merge-time per-arena remap verified
   at `crates/csp-sim/src/joints.rs:998-1082`: tank contents + seated crew (`:998-1003`,
   `merge_vessel_records` through `map`), reliability (`:1009-1014`), parachute deploys
   (`:1015-1021`), science (`:1027-1032`, primary ids preserved per `research.rs:75`), seam records
   (`:1033-1042`), temperatures (`:1043-1049`), aero mask positional fold (`:1050-1082`).

Undo-exactness and empty-stage hardening verdicts accepted on source reading (VAB undo stores
whole `CraftSession` clones, `csp-ui/src/vab.rs:465-470`; no derived session caches — every op
recomputes occupancy/transforms).

## PRD coverage table — verified

| Row | Verification |
|---|---|
| M3 snap graph WIRED | Ops + `snap_transform` + size-class gate (`compute_attach` `:1349-1358`) + `surf:` radial mounts confirmed (probe mounted a radial chute via `surface_node_id`); authored-catalog integration tests green. |
| M3 staging WIRED (editor model) | Cap/insert/remove/compact at `:182-:1935`; descending `stage_order` (`:1863`); supply-region warnings (`missing_propellants`); flight mapping `fire_stage` → `SimCommand::JettisonPart` at `csp-view/src/lib.rs:3189/3290` with the documented `InBubble`-only v1 limitation (`:3177`). |
| M3 scaling/variants WIRED | `set_scale`/`set_stretch`/`set_variant` gated (ScaleRule refusal verified live — probe's `set_scale(pod, 2.0)` correctly refused; drift warning covered by `session_gates_editor_edits`, green). |
| M3 save/load WIRED | Empirical fixed point + equality (above); refusal set in `from_file` (`:2290-2343`) and unit-tested. |
| M3 occupancy WIRED at op level | Add/reattach both sides verified empirically; crafted-file warning posture = Finding 6. |
| §3.1 `data_set_hash` DEGRADED | Accurate **with the correction from Finding 2**: zeros freeze in *craft files* only, not sim saves; the row's own note that the save-header hash IS wired (`app lib.rs:1120,1174-1188`) is correct. |
| §2.4 M6 merge WIRED (sim-side) | Merge tests green (above); full per-arena remap at the latch confirmed by reading `joints.rs:998-1082`. |
| §6.4 item 4 SANCTIONED | PLAN L1549-1553: "Orbital docking (M6) → post-v1, but proximity transfer stays" — the sim-joints merge exceeds the sanctioned cut; no unsanctioned degradation. |
| §7.7 pre-1.0 refusal WIRED | Craft file: `UnsupportedSchemaVersion` refusal, no migration (`session.rs:2245-2246`); save: version chain with refusal of unresolvable versions (`csp-sim/src/save.rs:35-93`), matching PLAN L1687-1699. |

## Net effect on the report

Findings 1, 3, 4, 5, 6 stand as filed. Finding 2 stands **with its sim-save propagation claim
struck** (craft files only; save-header §7.7 warning live) and severity reconciled MEDIUM → LOW
(borderline MEDIUM). No finding is dismissed; no false positive found. Top-3 ordering should
swap accordingly: Finding 1, then Finding 3, then Finding 2 (downgraded).
