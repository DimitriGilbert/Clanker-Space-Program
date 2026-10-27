# Review Report C20 — Scene: structures, meshes, art, terrain cache, demo

Session `session-full-20261026`, working tree @ HEAD+uncommitted. Scope: `crates/csp-view/src/{structures,part_mesh,terrain,art,demo,tutorial}.rs`, `crates/csp-view/tests/{demo,tutorial}.rs`. PRD anchors: §3.7 (presentation floor/kitbash), R2 terrain (§5.2), §6.3 web demo, M4c. All 69 csp-view tests (lib + demo + pad_spawn + tutorial) pass in `--release` (verified this session).

## Terrain-vs-contact datum parity — verdict: SOUND

The "buried pad" class is closed by construction, not luck:
- One shared height source: `csp_core::terrain::HeightField` (`crates/csp-core/src/terrain.rs:129-198`) — physics (`csp-phys/src/simple.rs:297-342`, `TerrainSurface` bakes `BAKE_WIDTH×BAKE_HEIGHT` behind `OnceCell`, amplitude `radius_m·AMPLITUDE_FRAC`) and view (`csp-view/src/terrain.rs:34-36,404-448` `ground_radius`) sample the identical bake with the same `sample_dir` (claimed and structured as the texel-exact CPU mirror of WGSL `height_at`).
- The pad site is authored terrain-aware: `data/systems/home/sites.ron:34-46` places (−0.092, 2.916) where the baked field is +0.02 m and flat ±0.5 m; `tests/pad_spawn.rs::launch_site_sits_on_rendered_ground` pins the site-vs-rendered-terrain delta ≤5 m.
- Structures anchor to the same surface: `structures.rs:781-848` `emit_launch_complex` re-samples `ground_radius` under every piece (ring-max over the piece footprint), re-radialized per piece, so the complex sits on the drawn ground; the craft spawns at `altitude_m: 0` ≈ the sampled surface (+0.02 m), in the flame-trench slot between the pedestals (`structures.rs:250-296`) — it reads as parked ON the pad, not buried/floating.
- `TerrainCache` (`terrain.rs:194-329`) key-swap/done-map/pending logic is correct across body switches: a visited body swaps in for free, a dropped in-flight bake is re-spawned (no stale-`pending` reuse), the flat stand-in has a content key that can't collide so the renderer re-uploads on the swap. The flat window is sub-second, only on a newly-dominant body (SOI transitions happen at altitude where the delta is invisible), and self-heals — not a defect.

## Findings

### [SEVERITY: MEDIUM] Finding 1: VAB and flight resolve different baked models for the same part — three classification vocabularies, and `WeldPart.parachute` is never consulted
**File**: `crates/csp-view/src/flight.rs:1320-1346,1460-1464`; `crates/csp-view/src/vab.rs:320-347`; `crates/csp-view/src/art.rs:375-435`
**Problem**: `model_key` has two disjoint arms — a 12-way def-id substring branch and a 7-way `PartKind` branch. The VAB keys with `Some(eff.part_id)`; flight passes `None` and a `PartKind` derived from `WeldPart` fields that omits `parachute` (`csp-phys/src/part.rs:304-309` carries it; `part_kind` never reads it).
**Evidence** (all stock parts, verified against `model_key`'s branches):
- `csp.chute.s125`: VAB → `"chute"`; flight → `Structural` → `"adapter"`.
- `csp.engine.ember`: VAB → `"engine_b"` ("the smaller engine (ember class)", `xtask/src/bin/parts/mod.rs:626`); flight → `Engine` → `"engine_a"` (the big sparc bell).
- `csp.decoupler.s125`: VAB → `"decoupler"`; flight (no dock_ports) → `"adapter"`.
- `csp.dockport.s125`: `.dockport.` matches no id branch → VAB `Structural` → `"adapter"`; flight `Port` → `"decoupler"` (inverted).
- `csp.lab.mk1`: no id branch → VAB kind (Tank-if-EC-tank/Structural) vs flight `Science` → `"science"`.
**Impact**: a craft visibly changes part appearance at the exact editor→flight seam the player compares; the chute is the worst case because it sits on the recommended recovery flow — in flight it renders as a generic structural collar (`emit_model_part` fully replaces the kitbash silhouette, `flight.rs:2814-2817`). PRD §3.7's floor promise is "silhouette-correct kitbashed parts for the stock ascent craft" — the collider fit keeps the silhouette class right, but the VAB-vs-flight inconsistency is a real parity defect, and the duplicated three-way classification is a CRAP magnet.
**Suggestion**: thread the part identity into the flight view — add `part_id: PartId` (or a family enum) to `WeldPart`/`CraftDef` graph nodes — and key both views through one `model_key(Some(id), …)` call, deleting the vab.rs inline kind block (`vab.rs:320-340`) in favor of a shared classifier. Minimal fix for the worst case: read `wp.parachute` in `part_kind` and add a `Chute` kind mapping to `"chute"`.

### [SEVERITY: MEDIUM] Finding 2: the §6.3 demo data set is a hand-maintained content mirror of `data/` with only a filename gate — the buried-pad regression does not cover it
**File**: `crates/csp-view/src/demo.rs:71-204,296-321`; `apps/demo/data/systems/home/sites.ron`
**Problem**: `apps/demo/data` duplicates the authored Terrin content (sites.ron lat/lon and the whole `terrin/body.ron` are byte/content copies of `data/`; the header literally says "Mirror of data/systems/home/sites.ron"). The only drift gate, `demo_files_match_disk` (`demo.rs:296-321`), compares file NAMES, not content. The terrain-parity regression (`tests/pad_spawn.rs::launch_site_sits_on_rendered_ground`) runs against `View::new()`'s desktop `data/` only.
**Evidence**: today `apps/demo/data/systems/home/terrin/body.ron` diffs clean against `data/` (same `noise_seed 601`) and the demo sites.ron carries the same (−0.092, 2.916) — parity holds by convention. Nothing fails if a demo-side edit (site coords, seed, radius) re-opens the buried/floating pad class in the wasm build, which is the public storefront and the one target the desktop test suite never re-checks.
**Impact**: data-integrity risk on a shipped artifact, in exactly the class this review was asked to probe. (The mandate's "baked-art staleness vs regen" cousin: content mirrors need the same loud-drift gate the csar packs got via `art::tests::packs_decode`.)
**Suggestion**: in `tests/demo.rs`, run the same site-vs-terrain delta assertion over `demo::source()` (every input it needs — seed, amplitude, site coords — is already loaded there in `demo_dataset_loads_and_validates`); longer term, generate `apps/demo/data` from `data/` with an xtask exclusion list instead of hand-copying.

### [SEVERITY: LOW] Finding 3: the test-stand facility gameplay has no corresponding structure at the rendered space center
**File**: `crates/csp-view/src/structures.rs:250-538` (`SPACE_CENTER`); `crates/csp-ui/src/program.rs:1500-1512`
**Problem**: the FACILITIES panel presents "test stand @ terrin-launch" with bench buttons (`RunBenchTest`), and the techtree gates facility unlocks — but the rendered complex (pad mount, gantry, VAB, crawlerway, propellant farm, sheds) contains no test stand. The review focus calls this out: structures now imply REAL facilities; the stand exists only as panel copy.
**Impact**: legibility/parity gap only — no logic breaks. The complex otherwise reads correctly as a space center (pad straddling a flame trench with the craft resting in the slot at terrain level, gantry service arms, VAB with crawler door).
**Suggestion**: one more `Piece` (~60 m west of the mount: a blast bucket + firing cage + propellant skid) reusing the existing `Prim::Bbox`/`Lathe` emit path; ~25 lines of static table.

### [SEVERITY: LOW] Finding 4: structures.rs triplicates the lat/lon→direction + spin-phase math
**File**: `crates/csp-view/src/structures.rs:124-143,802-812,856-866`
**Problem**: `site_frame`, `emit_launch_complex`, and `site_world_pos` each recompute `dir_fixed` from `sin_cos` of the site's lat/lon plus the `rem_euclid(2π)`/`from_axis_angle` spin conjugate — and `emit_launch_complex` computes `dir_i` only to pass its ground radius into `site_frame`, which immediately recomputes the identical vector internally.
**Impact**: pure DRY; a future spin-convention change must be edited in three places (four counting `terrain.rs:418-423`'s own copy).
**Suggestion**: a `fn site_dir(env, site) -> (DVec3 fixed, DVec3 inertial)` helper used by all three; `site_frame` can then take the precomputed direction.

## PRD coverage table

| PRD item | Status | Evidence |
|---|---|---|
| §3.7 kitbash silhouettes (M4c floor) | **WIRED — exceeded** | `part_mesh.rs` classifier + `emit_smooth` (pure, deterministic, tested); real-art tier on top: baked `parts.csar` models via `emit_model`, fitted to collider AABBs with inverse-transpose normals |
| §3.7 art sourcing/licensing | **WIRED — cleaner than spec** | Procedural generator (`xtask parts`) replaced the CC0 kit — license-free by construction; `art.rs` decode is bounded + graceful-fallback (C12-F1/F6 hardening in place) |
| R2 terrain: quadtree, f64 patch origins, pre-baked tiles, LUT scattering, ocean, planet LOD | **WIRED** | `csp-view/terrain.rs` packet build + `TerrainCache` (async bake native, sync wasm, instant re-visits); `patch_might_hold_sea` shoreline test with correct datum math; camera floor / shadow / ocean all sample the drawn field |
| Rendered surface == contact surface | **WIRED** | Shared `csp_core::terrain` source; same dims + `AMPLITUDE_FRAC` both sides; pad authored flat + regression-tested (see parity verdict) |
| §6.3 fixed demo scenario | **WIRED** | `View::new_demo`: embedded set, fixed seed, Terrin-only manifest asserted, starter craft seeded pad-resting (W2.6 manufacture path), ascent test proves flyability |
| §6.3 part cap 60 | **WIRED — enforced, not decorative** | `set_part_limit(Some(60))` → `check_part_count` refuses place AND `LoadBytes` (`vab.rs:837-844,1220`); unit tests `part_limit_refuses_past_the_cap` / `..._applies_to_loaded_craft` |
| §6.3 no save import/export / no dashboards / no MCP | **WIRED** | `set_file_io(false)` refuses `LoadBytes` (`FileIoDisabled`, tested); server is native-only |
| §6.3 saves not portable | **WIRED** | Reduced data set ⇒ data-set hash can never match (documented in demo.rs header) |
| M10 tutorial/first-flight | **WIRED** | `tutorial_drive` reads staged mirrors only; pin-aware focus, debris filtering, cross-residence live count; end-to-end tests (progression, dismiss/resume, debris rewind, science honesty) |
| Structures/space center render | **WIRED (beyond-PRD presentation)** | Static complex anchored to rendered terrain with per-piece re-radialization, AO + mottle; F3 legibility gap noted |

No CRITICAL/HIGH findings. No "coded but not wired" items in this cluster: the 60-part cap, file-IO gate, and terrain cache were all verified enforced, not just settable.

## Top 3 issues
1. **[MEDIUM]** VAB↔flight baked-model mismatch (chute→adapter, ember→engine_a/engine_b, decoupler/dockport inverted) — three classification vocabularies, `WeldPart.parachute` unused (`flight.rs:1320,1460`; `vab.rs:320`; `art.rs:375`).
2. **[MEDIUM]** Demo data set is a hand-maintained mirror with a names-only gate; the buried-pad regression never runs against it (`demo.rs:296`; `apps/demo/data/systems/home/sites.ron`).
3. **[LOW]** No test-stand structure at the rendered space center despite "test stand @ site" gameplay/copy (`structures.rs:250`; `program.rs:1500`).
