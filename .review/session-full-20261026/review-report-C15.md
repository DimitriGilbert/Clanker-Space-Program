# Review Report C15 — Physics: CraftSession + merge + tests

Session `session-full-20261026`. Scope: `crates/csp-phys/src/session.rs` (3,584),
`crates/csp-phys/src/merge.rs` (866), `crates/csp-phys/tests/resolve_parts.rs` (384),
`crates/csp-phys/tests/session.rs` (244). Reviewed against PLAN.md R3 §2.2-C, §2.4 (M6 merge),
§3.1, §7.2/§7.7, M3 row (L1497), §6.4 cut-list. All 33 in-module unit tests + 5 integration
tests pass; `cargo clippy -p csp-phys --lib` clean. Adversarial sequences (move → single-part
delete with reparent → reroot → rescale → validate → save/load; occupancy-exempt reattach;
multi-child promotion) were exercised empirically against the real `data/parts` catalog — the
attach-graph invariants hold throughout. No CRITICAL, no HIGH.

### [MEDIUM] Finding 1: `preview_attach` omits the occupancy gate `add_part` enforces — ghost/commit divergence
**File**: `crates/csp-phys/src/session.rs:1377-1384` (vs `add_part` at `:1249-1260`)
**Problem**: `add_part` refuses a second edge onto an already-bound parent attach node
(`SessionError::AttachNodeOccupied`) *before* `compute_attach`; `preview_attach` calls
`compute_attach` only. The doc on `preview_attach` claims "the same checks, no session
mutation" and "the drop lands exactly what was previewed" — both false for occupied nodes.
**Evidence**: empirically confirmed — `preview_attach` on `pod.bottom` already mated by a tank
returns `Ok(transform)` while the subsequent `add_part` with the identical edge returns
`Err(AttachNodeOccupied)`. The VAB UI pre-filters via `bound_attach_nodes` (`csp-ui/src/vab.rs:1507,1557,2018`)
so players don't hit it, but the model-level API is the oracle every other writer uses
(session.rs:330-334 names "MCP `craft.build`, the design solver, a crafted file" as the audience
of exactly this invariant).
**Impact**: any non-UI caller (session-class MCP tools, the §2.11 design solver in `csp-ai`)
that plans placements through `preview_attach` will preview snaps the commit refuses — a
plan/execute mismatch, the precise class of inconsistency the occupancy check was added to close.
**Suggestion**: factor the parent-side bound check (and the child-side variant `reattach`
applies) into a helper both `add_part`/`reattach` and `preview_attach` call, so preview returns
`AttachNodeOccupied` exactly when commit would.

### [MEDIUM] Finding 2: `BuiltPart.data_set_hash` is zeroed in the only production path — the §3.1/§7.7 per-part drift key is dead-on-arrival data
**File**: `crates/csp-ui/src/vab.rs:1913` (`BuiltPart::stock`), `crates/csp-phys/src/resolve.rs:261-268`
**Problem**: §3.1's `BuiltPart` carries `data_set_hash: Hash32` — "what the numbers meant when it
was built" — and resolve.rs documents it as plumbed so "the §7.2 record is already complete when
the check lands", with `BuiltPart::stock`'s own doc claiming "the manufacture path fills every
field". No path fills it: the VAB creates every part via `BuiltPart::stock` (zeroed hash), the
craft file serializes the record verbatim, and `to_part_graph` → `ManufactureCraft` → `CraftDef`
carries the zeros into the sim save. Grep confirms zero non-test writers of the field.
**Evidence**: `grep -rn data_set_hash` across crates — only tests, the struct definition, and the
save-*header*-level hash (which IS wired: `csp-app/src/lib.rs:1120`) touch it.
**Impact**: every part in every craft file and save freezes 32 zero bytes. When the "tracked
future item" consumer lands, it cannot distinguish stock-zeroed records from genuinely-matching
ones — the per-part drift warning will either false-fire on all legacy parts or have to
special-case zeros, defeating the field's purpose. The module doc's "manufacture path fills
every field" is false today.
**Suggestion**: stamp `built.data_set_hash = csp_data::data_set_hash()` once at the VAB→session
boundary (or at `launch_craft` before `to_part_graph`), one line plus a round-trip test; fix the
`stock` doc. Cross-cluster note for C13/C17.

### [MEDIUM] Finding 3: `merge_graphs`' `effective_base` precondition is a `debug_assert` — silently aliases parts in release
**File**: `crates/csp-phys/src/merge.rs:235-241`
**Problem**: the doc-precondition `effective_base ≥ max(primary effective_index) + 1` is
enforced only by `debug_assert!`, with the justification "the parts slice is out of signature by
design … only the primary's used indices can prove the bound". But that justification proves the
opposite for the checkable direction: the *lower* bound on `effective_base` IS derivable from
the primary graph alone (`primary.nodes.values().all(|n| n.effective_index < effective_base)`),
which is exactly what the assert evaluates.
**Evidence**: the sole caller passes `u32::try_from(craft_p.parts.len())` (`csp-sim/src/joints.rs:886`)
— safe under the maintained invariant (fragments keep the full parent parts slice,
`apply_breakup` at lib.rs:11433), so no live bug; but a future caller computing the base from
`graph.nodes.len()` instead (the obvious mistake after a fragment slimming) silently binds
secondary `WeldPart`s onto primary mass/collider entries in release builds — "no check
downstream can see" it, per the code's own comment.
**Impact**: a one-line caller change turns into silently wrong mass properties with no error.
**Suggestion**: promote the check to a typed `MergeError::EffectiveBase` return; it costs one
O(n) pass the function already does twice and makes the precondition total.

### [LOW] Finding 4: `compact_stages` launders crafted out-of-cap stage indices into validity
**File**: `crates/csp-phys/src/session.rs:1923-1935`
**Problem**: `validate` flags stage indices above `MAX_STAGE_INDEX` as `InvalidStage` errors
(reachable only via crafted files), but `compact_stages` remaps *any* populated stage into
`0..k`, converting an unlaunchable forged craft into a clean one with no trace. Same (milder)
for repeated `remove_stage` walking indices down.
**Impact**: cosmetic/hardening — the tidy op is an explicit player action, and the laundered
craft is physically legal afterwards; the error existed only as a file-integrity signal.
**Suggestion**: acceptable as-is; if file-integrity signalling matters, have `compact_stages`
clamp-and-warn or refuse while an out-of-cap index is present.

### [LOW] Finding 5: O(n²) child scans in `subtree_ids` / `missing_propellants` — bounded, but the fix pattern already exists in this file
**File**: `crates/csp-phys/src/session.rs:1145-1154`, `:2138-2199`
**Problem**: `children_ids` is an O(n) scan; `subtree_ids` calls it per frontier node (O(n²)),
and `missing_propellants` calls it inside the region flood (O(region·n) per engine).
`body_transforms` (`:1169`), `from_file` (`:2319`) and `validate_occupancy` (`:2063`) already
build the one-pass parent→children index with a comment saying exactly this.
**Impact**: at 300–1,000 parts this is single-digit ms per `remove_part`/`validate` call —
within the M3 16 ms budget, but it is the same CRAP-shape the file already solved three times.
**Suggestion**: extract the `BTreeMap<PartNodeId, Vec<PartNodeId>>` child-index helper (one
place, four users) rather than a fourth inline copy when the next consumer arrives.

### [NIT] Finding 6: crafted-file double-bound attach nodes remain "launchable"
**File**: `crates/csp-phys/src/session.rs:2062-2093`, `:586-598`
`validate_occupancy` reports double-bound nodes as *warnings*, so `is_launchable()` is true for
a crafted file whose geometry welds coincident interpenetrating colliders. This is a
deliberately documented posture (`:586-598`) — the weld tolerates it, nothing crashes — recorded
here for the coverage table, not as a defect.

## Non-findings (verified, for the record)

- **Occupancy across move/reparent/reroot**: `reattach`'s parent- and child-side exemptions
  (freeing exactly the moving edge's own old ends) are correct in both directions; `reroot`'s
  edge reversal preserves the mate set (roles swap, no new bindings); `remove_and_reparent`
  plans all promotions against a running `bound` set before mutating (atomic, deterministic
  ascending-id order). Adversarially exercised — no drift.
- **Undo exactness**: the session has no derived caches (occupancy/transforms recomputed per
  call); the VAB's undo stack stores whole `CraftSession` clones (`csp-ui/src/vab.rs:465-470`),
  so undo restores occupancy, ids and `next_id` bit-exactly by construction.
- **Craft file round-trip**: byte-canonical (save→load→save fixed point), id-stable with holes,
  `next_id > max live id` enforced, every structural corruption class (dup id, missing/multiple
  root, root edge, dangling parent, cycle, bad transform, schema version) refused with a typed
  error — tested both at unit level and against the authored catalog. `bincode`-not-CBOR is a
  documented, defensible deviation (§7.2 governs *saves*; craft files are §2.2-C session
  artifacts; bincode is the §2.1-pinned codec on this crate's allowlist).
- **Empty-stage OOM hardening** (`MAX_EMPTY_STAGE_WARNINGS`, `u32::MAX` stage): correct
  arithmetic, tested at the boundary.
- **Merge correctness**: id remap is collision-free by construction (`base = max(primary)+1`,
  `checked_add`); the seam edge composes the secondary root's stored transform (the
  undock-fragment case); `check_secondary_structure` rejects both dangling parents and detached
  cycles up front; the M6 exit-criterion test (`merge_weld_is_bit_identical_to_from_scratch_weld`)
  derives the seam the way `apply_latch` does and proves `to_bits()`-level mass/CoM/inertia
  equality plus the mass-union property; the 1000-part station folds to one body. Merge-time id
  remapping of science projects, crew, tank contents, reliability records, parachute deploys,
  temperatures, seam records and the aero mask is fully wired at the sim latch
  (`csp-sim/src/joints.rs:1010-1080`) — primary ids pass through unchanged, secondary ids map.

## PRD coverage table

| PRD requirement (PLAN §) | Status | Evidence |
|---|---|---|
| M3 "part snap graph" (L1497) | **WIRED** | session.rs attach ops, `snap_transform`, size-class gate, `surf:` radial mounts; integration tests over authored defs |
| M3 "staging" (L1497) | **WIRED** (editor model) | stage indices w/ `MAX_STAGE_INDEX` cap, insert/remove/compact, descending firing order, supply-region propellant + empty-stage warnings; flight mapping via `fire_stage` → drop-set `JettisonPart`s (`csp-view/src/lib.rs:3164+`, documented no-debris v1 limitation) |
| M3 "scaling + variants" (L1497) | **WIRED** | `set_scale`/`set_stretch`/`set_variant` gated by `ScaleRule`, resnap of self+children, post-scale diameter drift → warning |
| M3 "craft save/load with stable `PartId`s" (L1497) | **WIRED** | byte-canonical bincode file, ids never reused, `next_id` round-trip, full structural refusal set (tested) |
| M3 occupancy ("one attach node one neighbour", §2.4 editor-time) | **WIRED** at op level | `AttachNodeOccupied` on add/reattach both sides; crafted files → warning only (documented, Finding 6) |
| §3.1 `BuiltPart.data_set_hash` | **DEGRADED (plumbed, never filled)** | Finding 2; save-*header*-level drift hash IS wired (`csp-app/src/lib.rs:1120,1177`) |
| §2.4 M6 latch merge (L314-322) | **WIRED** (sim-side) | `merge_graphs` + bit-identical merge test (the M6 exit criterion) + 1000-part station; full per-arena id remap at the latch |
| §6.4 item 4: docking *UI* post-v1 (L1551) | **SANCTIONED cut** | merge exercised via sim joints only; no unsanctioned degradation found |
| §7.7 pre-1.0 version refusal (L1689-1698) | **WIRED** | `UnsupportedSchemaVersion` refusal, no migration; v1 bodies refused (documented) |

## Top 3 issues

1. **[MEDIUM]** `preview_attach` omits the occupancy gate — preview says Ok where commit
   refuses; the model-level oracle MCP/scripts/the design solver rely on is inconsistent with
   `add_part` (session.rs:1377 vs :1249).
2. **[MEDIUM]** `BuiltPart.data_set_hash` is zeroed in the only production path (VAB stock
   builds); the per-part §3.1/§7.7 drift key serializes dead zeros into every craft file and
   save, and the `stock` doc's "manufacture path fills every field" is false
   (csp-ui/src/vab.rs:1913, resolve.rs:261-268).
3. **[MEDIUM]** `merge_graphs`' `effective_base` precondition is a release-stripped
   `debug_assert` over a bound derivable from the primary graph alone — a future caller
   arithmetic slip silently aliases mass properties instead of erroring (merge.rs:235-241).
