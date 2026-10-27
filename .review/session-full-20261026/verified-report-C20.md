# Verified Report C20 — session-full-20261026

Verification agent pass over `review-report-C20.md`. Every finding re-checked against
the working tree at the cited file:line (line numbers below are the verified ones).
Empirical: `cargo test -p csp-view --test pad_spawn launch_site_sits_on_rendered_ground --release` → ok (0.53 s).

Score: 4 confirmed, 0 dismissed as findings. One evidence item inside Finding 1
(the `csp.lab.mk1` example) is incorrect and is corrected below; it does not sink the finding.

---

### Finding 1: VAB and flight resolve different baked models for the same part — CONFIRMED
**Original summary**: `model_key` has two disjoint arms (def-id substring vs `PartKind`); VAB keys with `Some(part_id)`, flight with `None` + a 7-way kind that omits `WeldPart.parachute`; chute/ember/decoupler/dockport/lab render differently across the editor→flight seam.

**Verification**:
- Structure confirmed. `crates/csp-view/src/art.rs:375-435` `model_key(Option<&str>, PartKind, bool)`: an id-substring branch (11 return arms over 17 substrings — the report's "12-way" is off by one; immaterial) then a 7-way `PartKind` branch. `crates/csp-view/src/vab.rs:319-348` passes `Some(eff.part_id)` with an inline 4-way kind (Engine/CrewSeat/Tank/Structural). `crates/csp-view/src/flight.rs:1320-1346` `part_kind(&WeldPart)` is 7-way and never reads `wp.parachute`; `crates/csp-phys/src/part.rs:309` confirms the field exists. `flight.rs:1460-1464` calls `model_key(None, kind, is_pointed)`.
- Per-part traces (defs read at `data/parts/`):
  - `csp.chute.s125` (modules: Parachute only, collider Box 0.25/0.2/0.25): VAB id branch `.chute.` → `"chute"`; flight → Structural, `is_pointed` is Lathe-only (`part_mesh.rs:472-474`) → `"adapter"`. CONFIRMED swap.
  - `csp.engine.ember`: VAB `.engine.`+`ember` → `"engine_b"`; flight Engine → `"engine_a"`. CONFIRMED, and visually loud: `xtask/src/bin/parts/mod.rs:531` `engine_a` = "the big first-stage bell (sparc class)", `:623` `engine_b` = "the smaller engine (ember class)" — the 60 kN upper-stage engine renders as the big first-stage bell in flight.
  - `csp.decoupler.s125` (Decoupler module, flat-top cylinder): VAB → `"decoupler"`; flight Structural, `tip_at_top` requires last profile radius ≤ 2% of r_max (`part_mesh.rs:261-264`) → `"adapter"`. CONFIRMED.
  - `csp.dockport.s125`: id matches no substring → VAB falls through to kind branch with inline Structural → `"adapter"`; flight `dock_ports` non-empty → Port → `"decoupler"`. CONFIRMED inverted.
  - `csp.lab.mk1`: **evidence item INCORRECT**. The lab carries `CrewSeat(slots: 2)`; flight `part_kind` tests `crew_seats > 0` BEFORE `lab`/science (flight.rs:1329 vs :1339) → Crew → `"pod"`, and the VAB inline kind tests CrewSeat (vab.rs:326-331) → Crew → `"pod"`. Both views render `"pod"` — no mismatch for the lab. (The report's "Tank-if-EC-tank/Structural" gloss of the VAB check is also wrong: the VAB inline check has a CrewSeat arm.) Strike this example; the finding stands on the other four parts.
- Player visibility confirmed: when a model resolves, flight emits it exclusively — `flight.rs:2812-2817` `if let Some(m) = &p.model { emit_model_part(...); continue; }` — the kitbash silhouette is fully replaced, so the chute's canister-with-packed-canopy baked model (`"chute"` key exists, xtask mod.rs:1302) becomes a generic adapter collar in flight. All 12 model keys exist in the pack (`xtask/src/bin/parts/mod.rs:1292-1303`; `art.rs:445-455 packs_decode` gates them). The art packs are embedded (`art.rs:87-88 include_bytes`) and installed by `View::from_source` (`lib.rs:1905-1910`) for both `View::new` (native) and `View::new_demo` — and on wasm `View::new` IS `new_demo` (`lib.rs:1959-1963`), so the swap shows in the public web demo too.
- Severity MEDIUM is fair: no logic breaks, but it is a real editor↔flight parity defect on the flagship recovery part, exactly at the seam a player compares, plus a genuine three-vocabulary CRAP hazard.

### Finding 2: demo data set is a hand-maintained mirror with a names-only drift gate; buried-pad regression never runs against it — CONFIRMED
**Original summary**: `apps/demo/data` duplicates authored content; `demo_files_match_disk` compares file names not content; `launch_site_sits_on_rendered_ground` runs against desktop `data/` only, so the wasm storefront set can silently drift.

**Verification**:
- Gate confirmed names-only: `crates/csp-view/src/demo.rs:296-321` collects disk `PathBuf`s and embedded `FILES` keys, sorts both, `assert_eq!` on the path lists. No byte/hash comparison anywhere in `crates/csp-view/tests/demo.rs`.
- Mirror confirmed: `apps/demo/data/systems/home/terrin/body.ron` diffs byte-clean against `data/` (same `noise_seed: 601`, `radius_m: 6.000e5`); `sites.ron` pad entry carries identical coords (lat −0.092, lon 2.916, alt 0.0) and its header literally reads "Mirror of data/systems/home/sites.ron" (the demo copy omits the rask-mines site by design).
- Coverage gap confirmed: every test in `crates/csp-view/tests/pad_spawn.rs` builds `View::new().expect("data/ load")` (lines 27, 109, 149); the site-vs-rendered-terrain delta assertion lives at lines 107-141 of that file and is never instantiated over `demo::source()`. `tests/demo.rs::demo_dataset_loads_and_validates` (lines 28-109) runs schema + loc-key validation only — no terrain parity check. Nothing in the suite would fail if a demo-side edit to sites.ron/body.ron re-opened the buried/floating pad class in the wasm build.
- Today's parity is by convention, not enforcement — exactly as the report frames it. A risk finding, not a current defect; MEDIUM is defensible given the demo is the shipped public artifact.

### Finding 3: "test stand" facility gameplay has no corresponding rendered structure — CONFIRMED
**Original summary**: the FACILITIES panel presents a test stand at the launch site with `RunBenchTest` buttons, but `SPACE_CENTER` contains no test stand — legibility gap only.

**Verification**:
- UI side: `crates/csp-ui/src/program.rs:1500-1517` renders `"test stand @ {bench_site_name} …"` whenever a bench site exists, with `RunBenchTest` buttons at :1590; `crates/csp-view/src/lib.rs:4635-4636` sets `bench_site` from the launch site (so the copy reads "test stand @ terrin-launch"-style).
- Render side: enumerated all `SPACE_CENTER` pieces (`crates/csp-view/src/structures.rs:250-538`): launch mount apron/pedestals/trench liners, gantry (2 pieces at 14.5, 2), crawlerway (2), VAB + west annex (4 at −42/−68, 85), propellant farm (2 at 30/39, −24), support sheds (2 at −26/−30 and 24, −32). No stand/bench/blast-bucket/firing-cage piece exists (grep over structures.rs finds none).
- Confirmed as a copy-vs-world parity gap; no logic breaks. LOW is correct.

### Finding 4: structures.rs triplicates the lat/lon→direction + spin-phase math — CONFIRMED
**Original summary**: `site_frame`, `emit_launch_complex`, and `site_world_pos` each recompute `dir_fixed` and the `rem_euclid(2π)`/`from_axis_angle` spin conjugate; `emit_launch_complex` computes `dir_i` only to feed a ground radius into `site_frame`, which recomputes the identical vector internally.

**Verification**: read all three sites. `structures.rs:124-143` (`site_frame`), `structures.rs:802-812` (`emit_launch_complex` — the redundancy claim is verbatim true: `dir_i` is computed solely for `ground(dir_i, 30.0)` whose result is passed as `ground_r` into `site_frame`, which then rebuilds the same `dir_fixed`/`spin` itself), `structures.rs:856-866` (`site_world_pos`), plus the fourth inverse-sign copy at `terrain.rs:418-423` (`ground_radius`'s −phase conjugate). Pure DRY; LOW is correct.

---

## SOUND verdicts — spot-checks

1. **HeightField shared physics/render — CONFIRMED SOUND.** Single implementation and constants in `crates/csp-core/src/terrain.rs` (`BAKE_WIDTH: 1024`, `BAKE_HEIGHT: 512`, `AMPLITUDE_FRAC: 0.004` at :29-36; `HeightField::bake`/`sample_dir` :140-198, documented as the texel-exact CPU mirror of WGSL `height_at`). Physics: `csp-phys/src/simple.rs` `TerrainSurface.field()` lazily bakes `HeightField::bake(self.seed, BAKE_WIDTH, BAKE_HEIGHT)` behind `OnceCell` with amplitude `radius_m · AMPLITUDE_FRAC`. View: `csp-view/src/terrain.rs:404-448` `ground_radius` samples `cache.field(body_id, body.seed)` — same type, same seed source, same constants.
2. **Pad site authored terrain-aware — CONFIRMED SOUND.** `data/systems/home/sites.ron` places the pad at (−0.092, 2.916) with the +0.02 m / flat ±0.5 m rationale in the authoring comments; `launch_site_sits_on_rendered_ground` (tests/pad_spawn.rs:107-141, delta ≤ 5 m against the 1024×512 bake the GPU ships) passes empirically this session.
3. **60-part cap enforced, not decorative — CONFIRMED SOUND.** `DEMO_PART_LIMIT: usize = 60` (demo.rs:25), applied in `View::new_demo` (lib.rs:1934). `check_part_count` (csp-ui/src/vab.rs:837-844) is enforced at attach-planning (:1452), root/attach (:1861, :1884), and `LoadBytes` (:1220); unit tests `part_limit_refuses_past_the_cap`, `part_limit_applies_to_loaded_craft`, and `file_io_disabled_refuses_load` all present. (Note: the report cites "vab.rs:837-844,1220" — correct for `crates/csp-ui/src/vab.rs`; the crate qualifier was missing, since csp-view also has a vab.rs.)

## Corrections to the original report
- Finding 1 evidence item 5 (`csp.lab.mk1` → flight `Science`/"science") is wrong: CrewSeat/Crew precedence beats Lab/Science in BOTH classifiers, so the lab renders `"pod"` in both views. The finding survives on chute/ember/decoupler/dockport.
- Finding 1 says "12-way def-id substring branch"; the branch has 11 return arms (17 substrings). Cosmetic.
