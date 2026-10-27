# Verified report C12 — physics integration + contact (second-pass verification)

Verification agent pass over `review-report-C12.md` against the current tree
(2026-10-26). Every finding was re-traced through the actual sources; the
heightfield claims were tested empirically against the real `csp_core`
height function; the clean-verdict test claims were re-run.

**Empirical work done in this pass (nothing outside /tmp touched the repo):**
- Scratch crate at /tmp/slopecheck linking `csp-core` by path: baked the real
  `HeightField` (1024x512, seeds 601/602/603/1) and measured max adjacent-texel
  gradients, which bound the bilinear field's continuous gradient.
- Re-ran `cargo test -p csp-phys` (157/157 pass), the three targeted sim tests
  (`lander_20t_latches_and_hash_freezes`, `unlatch_restores_pose_exactly`,
  `unlatch_stamps_com_surface_velocity`), `cargo test -p csp-view --test
  pad_spawn` (3/3) and `launch_deliver_throttle_flies` (pass) — the b698eb2
  relaunch chain is empirically green on the current tree.

---

### Finding 1: Delivery-berth occupancy probe tests a different volume than the delivery materializes — CONFIRMED

**Original summary**: `delivery_blocked` poses its candidate at
`DQuat::IDENTITY` / `altitude_m: def.altitude_m`, while `deliver_built_vessel`
berths upright (`surface_upright`) at `altitude_m − belly`; the launch path
poses its candidate exactly as the spawn berths it.

**Verification**: All three sites read and confirmed.
- Probe: manufacture.rs:295-310 — `orientation: DQuat::IDENTITY`,
  `altitude_m: def.altitude_m` (no belly), then `surface_pose_restore` +
  `spawn_site_occupied(&candidate)`.
- Berth: manufacture.rs:377-401 — `belly` = min(0, min collider z) (identical
  arithmetic to the launch path's `z_min`), state written as
  `altitude_m: def.altitude_m − belly`, `orientation: surface_upright(lat, lon)`.
- Launch pre-flight: csp-view/src/lib.rs:2906-2941 — candidate posed with
  `surface_upright(lat, lon)` and `site_alt + PAD_CLEARANCE_M − z_min`
  (`PAD_CLEARANCE_M = 0.0`, csp-view lib.rs:70 — so the launch pose and the
  delivery pose are the *same* convention; only the probe diverges).
- `spawn_site_occupied` is AABB-based (`volumes_overlap`, simple.rs:1527-1532),
  so pose (position AND rotation) determines the tested volume. The probe's
  comment (manufacture.rs:279-286) claims predicate-sharing with the launch
  pre-flight — the predicate is shared, the pose is not.
- Numbers check: Terrin pad lat −0.092 / lon 2.916 (data/systems/home/sites.ron:19,33)
  gives local up = (−0.970, 0.223, −0.092) (report said −0.975 — trivially off,
  immaterial): up is ~5° off the horizontal plane, so an upright stack's long
  axis lies near-horizontal in world terms while the identity probe's stack
  axis is world +Z. For the same craft delivered at the same site the two AABBs
  still intersect near the shared origin, but for a parked craft *offset* from
  the berth center the probe's thin vertical AABB misses volumes the delivered
  upright AABB covers. Belly omission additionally sinks the probe's origin by
  |belly| relative to the delivery.
- Downstream consequence verified: both hulls would be `SurfaceLatched` (no
  solver); on later unlatch, coincident/deeply-overlapping hulls ghost through
  (documented at simple.rs:1517-1526 and pinned by
  `spawn_physical_refuses_occupied_site`, csp-sim lib.rs:19984). Retry machinery
  exists (manufacture.rs:327-341) but re-runs the same wrong-pose probe.

**Verdict**: Real predicate/delivery divergence breaking the berth rule's
stated purpose; contrived-geometry trigger as the report itself notes. MEDIUM
stands. The suggested `berth_pose(def, &craft)` extraction is exactly right and
trivially shareable with the launch path since both altitude formulas are
already identical.

---

### Finding 2: Terrain contact normal radial-only; µm doc justification stale; slope sliding suppressed — PARTIALLY CONFIRMED (impact claim refuted; downgrade to LOW/doc fix)

**Original summary**: contact normal is purely radial (simple.rs:1698, :2246)
while the contact surface is the displaced heightfield; the module doc's
"≪ µm" justification predates the heightfield; a craft on any slope never
slides because gravity and the support impulse share the radial line; slopes
of "tens of degrees" are authorable at 1024×512.

**Verification**:
- Code facts CONFIRMED. simple.rs:1698 `let n_w = rel / dist;` (velocity-pass
  constraint normal) and :2246 `let normal = rel / dist;` (Baumgarte position
  pass), while depth is measured against `env.surface_radius(dir, t)` — the
  heightfield `radius_m + h(dir)·amplitude` (amplitude = 0.004·R = 2400 m on
  Terrin; BAKE 1024×512; csp-core terrain.rs:29-36). Gravity is exactly radial
  (`accumulate_forces`, simple.rs:1542-1545), so in solver coordinates gravity
  injects zero tangential velocity on any slope — the suppression mechanism is
  real.
- Doc staleness CONFIRMED in part: the :74-78 sentence bundles the radial
  "up" normal with a µm-scale claim that is only true for the bare sphere; with
  the heightfield the normal error equals the local slope angle (empirically
  ≤ 5.8°, see below), not µm. (The *manifold* half of the µm claim — vertex vs
  face/edge — survives: at hull-footprint scale the bilinear field is
  near-planar.)
- Impact claim REFUTED empirically. Baking the actual field with the real
  `csp_core::terrain` code: max adjacent-texel slope (which bounds the bilinear
  gradient) is **5.03° on Terrin (seed 601)**, 5.35° Rask (602), 5.18° Duva
  (603), 5.82° Solen (1); >87% of Terrin is under 1°. μ = 0.5
  (DEFAULT_FRICTION_MU, simple.rs:272) gives a 26.6° friction angle: under a
  *correct* slope normal, no shipped slope (tan ≤ 0.10 < 0.5) would slide
  either — static friction holds everywhere the field can express. So "craft
  sticks instead of sliding" produces no behavior difference on any authored
  content. "Slopes of tens of degrees are authorable" is doubly false: the
  height function is hardcoded seeded fbm (terrain.rs:110-120), and authored
  `heightmap:` files are explicitly rejected by R2 (csp-view/src/terrain.rs:105-107
  "R2 uses noise_seed instead"; all shipped bodies are `heightmap: None`).
- Settle-latch non-impact confirmed (radial sink vs radial correction cancel;
  `lander_20t` and `dropped_box_rests_on_terrain_not_datum` pass — re-run).
- Pad corroboration: the real bake gives +0.020 m at (−0.092, 2.916), exactly
  the sites.ron comment, and `launch_site_sits_on_rendered_ground` (|Δ| ≤ 5 m)
  passes.

**Verdict**: The fix-the-doc option (b) is the correct scope today — the
normal-error bound should read "slope angle, ≤ ~6° on shipped fields," and the
finding's residual engineering value is forward-looking (if heightmaps or
sharper fbm ever land). MEDIUM overstates it; recommend LOW.

---

### Finding 3: Duplicated force model and duplicated PGS row solve — CONFIRMED

**Original summary**: `external_loads` re-implements `accumulate_forces`'
channels with only `decel_area_m2` shared; ground vs pair PGS blocks are
~55-line structural twins; `ang`/`effective` closures duplicated in
`PairConstraint::new` and `JointConstraint::new`.

**Verification** (sampled all three):
- external_loads (simple.rs:1344-1479) vs accumulate_forces (:1536-1616):
  gravity formula (per-part vs total — two hand-maintained copies of
  `−μ·m·r̂/r²`), placeholder-drag formula `v_rel·(−0.5·ρ·Cd·A·speed)`
  (:1407-1409 vs :1577-1578), and the thrust-site loop (:1443-1458 vs
  :1604-1614) are genuinely duplicated arithmetic. `decel_area_m2`'s doc
  ("Shared by ... so the two channels can never disagree") is at :2648-2650
  as quoted.
- One overstatement: the aero *table* channel is NOT fully hand-duplicated —
  `CraftAeroTables::force_torque_body` and `part_forces_body` both call the
  shared `Self::part_force` (aero.rs:753, "identical arithmetic ... same
  arithmetic so the vector sum is bitwise the total force"). The channel
  *glue* (ρ, Mach, Re, `w_body` derivation) is still two copies, so drift
  exposure remains, but the core per-part aero math cannot diverge.
- PGS blocks: ground (:2015-2068) vs pair (:2069-2121) confirmed as structural
  twins — normal row → t1 → t2 → Coulomb-disc projection, differing only in
  the relative-velocity expression and write-back helper, exactly as reported.
- Closures: `ang` + `effective` verbatim-duplicated at :840-850 and :1044-1054
  (the latter's own comment admits "identical to `PairConstraint::new`'s
  `ang`"); the latched inv_m/inv_i setup boilerplate is also duplicated
  (:821-834 vs :1021-1041).

**Verdict**: Confirmed as written minus the aero nuance. Maintainability-only
(no behavior bug); the breakup-load/force-channel pairing is the load-bearing
duplication, so the severity is defensible.

---

### Finding 4: `facet_planes` eager for every hull every tick — CONFIRMED

**Original summary**: the O(n⁴) plane sweep runs for all hulls of all bodies
before the AABB pair filter.

**Verification**: simple.rs:1781-1795 builds `hull_planes` unconditionally for
every hull of every body; the body-AABB pair reject is at :1798-1803 and the
hull-AABB reject at :1817-1821 — both after the planes exist. `facet_planes`
is C(n,3) triples × O(n) support scan (hull.rs:49+), n = 16 for the cylinder
prisms parts resolve to (part.rs:164-165) → ~7-9k inner ops per hull; a
30+30-part pair pays ~0.5M ops/tick as estimated. Notably part.rs:160-162's
own doc says the sweep runs "every tick a *pair candidate overlaps*" — the
implementation is broader than the doc's stated expectation. The suggestion is
sound: `bodies` is `&[WeldedBody]` (immutable) during generation, so a
lazily-filled `Vec<Option<Vec<Plane>>>` memo is exact within the call and
changes no iteration order. Perf framing checks out (PLAN :470 ×2 warp cap,
:1441 ≤0.5 ms/tick row).

**Verdict**: Confirmed; LOW/performance is the right weight.

---

### Finding 5: Latched vessel left floating when its support departs — CONFIRMED

**Original summary**: settle qualifies on *any* contact including
vessel-vessel-only; no unlatch trigger re-evaluates a latched record when the
supporter leaves.

**Verification**:
- simple.rs:2362 `in_contact = !self.contacts[i].is_empty()`; pair contacts are
  pushed into both bodies' `self.contacts` slots (:1899-1900), so pair-only
  rest qualifies for latch.
- The latch-departure veto (csp-sim lib.rs:11154-11223) only holds a body back
  while a *still-moving* body touches it (lib.rs:11170
  `bubble_vessel_contact(..., true)`); once both sides are latched, both
  depart ("the top one leaves first", :11146-11147). Afterwards nothing
  re-evaluates either record: the only SurfaceLatched→InBubble paths are
  control commands (`SetThrottle`/`SetAttitude` → `unlatched_body`,
  lib.rs:5270/5282), `SimCommand::Unlatch` (:5286→5792-5801), and `ToBubble`.
  Grep for support/floating/ground-below re-evaluation: none exists.
- Reachable scenario confirmed: craft B lands on latched craft A → B latches
  (static supporter, veto excludes latched) → A unlatches and flies → B hangs
  `SurfaceLatched` mid-air until the player addresses it (unlatch restores the
  exact pose and it falls — self-healing, deterministic).
- Two report nuances, both in the finding's favor rather than against: (i)
  debris that settles via pair-only contact is *also* removed (the
  `settle.latched` branch at lib.rs:4422-4429), so the debris parenthetical is
  slightly off, but non-debris vessels exhibit the gap; (ii) the spec's
  "activation" and "nearby impact" unlatch triggers are themselves unwired
  (simple.rs:211-213: "M2-C World wiring") — the support-removal gap sits in
  known unwired-trigger territory, consistent with LOW.

**Verdict**: Confirmed as a real, deterministic, player-visible oddity; LOW.

---

### Finding 6: `cast_ray` exit-root normal contradicts doc; 96-step scan can miss grazed peaks — CONFIRMED

**Original summary**: `Hit`'s doc promises a normal "pointing back toward the
ray origin side," but an origin below the surface gets the exit root with the
outward (away-from-origin) normal; the fixed 96-step scan over the whole
entry/exit window can step over a double-crossed terrain peak.

**Verification**: backend.rs:126-128 doc read verbatim. Sphere branch
(simple.rs:2542-2552): origin inside → `-b - sq < 0` → exit root `-b + sq`,
normal `rel/‖rel‖` = outward at the exit point, pointing away from the
origin — contradicts the doc (the "(outward for the terrain sphere)"
parenthetical only covers the outside-origin case). The terrain branch's
comment (:2557-2558) explicitly handles "exit for an origin already under the
surface" and returns the same outward radial normal (:2617-2623). Scan: 96
uniform steps over a window that can span the whole shell (up to ~1205 km on
Terrin → ~12.5 km/step vs 3.7 km texels), 40-step bisection — a ridge crossed
twice inside one interval leaves both endpoint signs equal and goes unnoticed;
deterministic and lossy as stated. No production caller: the only `cast_ray`
call sites are simple.rs tests (:3167, :3183, :3195, :3969).

**Verdict**: Confirmed; LOW is right (trait-surface polish, not a gameplay bug).

---

## Clean verdicts — spot-checked, no false negatives found

- PGS determinism: SOLVER_ITERATIONS = 8 (simple.rs:237), BAUMGARTE_BETA = 0.2
  (:241); restitution 0 (target_vn is 0 or `depth/dt ≤ 0`; no restitution
  constant exists anywhere); fixed orders throughout (ground-then-pair-then-
  joints per iteration, ascending generation orders); BTreeMap-only discipline
  confirmed (all "HashMap" hits in csp-phys are doc comments forbidding them);
  ground body-frame K (:637-639) and two-body world-frame `ang` (:840-845)
  are the same `d·I⁻¹(r×d)` identity as claimed.
- Settle-latch hysteresis: pose-delta gates beyond the spec confirmed in
  `update_settle` (:2354-2440); `position_correction_is_capped_and_blocks_settle`
  (:3817) pins it; latch zeroes throttle/vel/ang_vel and clears contacts
  (:2430-2434).
- b698eb2 relaunch chain: commit exists with the described message;
  `SetThrottle` → `unlatched_body` → InBubble confirmed at lib.rs:5262-5273 +
  :3867-3955; the full chain (launch → delivery → throttle unlatch → climb) is
  pinned by `launch_deliver_throttle_flies` (csp-view lib.rs:5879) — re-run,
  passes.
- Hash walk covers `settle.{settled_ticks,latched}` at exactly csp-sim
  lib.rs:11866-11867. Unlatch CoM-slot velocity stamping confirmed
  (bubble.rs:341-346). `surface_upright` hand-checked at (0,0) (ẑ→x̂ tilt) and
  poles (finite, degenerate east handled).
- Test claims re-run: 157/157 csp-phys lib tests, the three targeted sim
  tests, and pad_spawn 3/3 all pass on the current tree.

## PRD coverage table — verified

Every row's citations check out: trait verbatim (backend.rs:183-199); pinned
semi-implicit Euler with the documented midpoint gyroscopic stage
(simple.rs:1621-1655, rationale :1626-1636); PGS constants/order/Coulomb-disc
(:237-241, :2050-2067); settle latch wiring (:2354-2440) and body-fixed record
(bubble.rs:264-348); M2 exit tests (`lander_20t_latches_and_hash_freezes`,
  csp-sim lib.rs:15403; 20 t box at csp-phys :2932); M6 pair manifold + joints
  with momentum-conservation tests at :3709/:4078.
The two deviations are accurately characterized:
- **No RapierBackend**: documented in-code (backend.rs:49-58 JointDef backend
  note; simple.rs:121-124); `rapier3d-f64 = "=0.36.1"` pinned at workspace
  Cargo.toml:65 with no crate depending on it; PLAN.md:315/698-699 confirm the
  spec names RapierBackend for jointed islands — an unsanctioned-but-documented
  v1 reading, correctly flagged DEGRADED-not-blocker.
- **No nightly oracle**: the four analytic unit tests exist exactly as cited
  (free fall :2779, terminal velocity :2817, precession :3485, tumble :4211);
  the PLAN.md:1769 nightly `SimpleBackend` vs `RapierBackend` harness does not
  exist. PARTIAL is correct.
- W2.6 delivery row: manufacture.rs:371-401 confirmed (upright + belly, launch
  convention mirrored; retry queue :327-341), with Finding 1's predicate
  divergence accurately noted.
