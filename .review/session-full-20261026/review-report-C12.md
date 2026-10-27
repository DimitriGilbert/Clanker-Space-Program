# Review C12 — Physics: integration + contact (csp-phys simple/backend/tensor/lib)

Scope: `crates/csp-phys/src/{simple 4254, backend 199, tensor 324, lib 147}.rs`, plus the
sim-side wiring that is this cluster's focus (settle latch semantics, b698eb2 unlatch territory,
W2.6 delivery-berth upright mechanics in `csp-sim/src/{bubble,manufacture}.rs`, `csp-sim/src/lib.rs`
bubble tick). All 157 csp-phys lib tests and the targeted sim tests (`lander_20t_latches_and_hash_freezes`,
`unlatch_restores_pose_exactly`, `unlatch_stamps_com_surface_velocity`, manufacture delivery set)
were run and pass on the current tree.

Verified correct (no findings needed): PGS math (inverse effective mass identities checked
symbolically — ground body-frame and two-body world-frame forms are both `d·I⁻¹(r×d)`), CoM-frame
position/rotation discipline (`pos = x̄ − q·com_local` everywhere, position pass included),
speculative anti-tunneling margins (ground + pair), Coulomb-disc projection beyond §2.9's
per-direction clamps, restitution 0 (no bounce path exists — "bounce forever" is structurally
impossible), fixed iteration counts everywhere (8 solver, 96 ray steps, 40 bisections — no
iterate-until-converged), no HashMaps, libm-only transcendentals, no serialized solver state
(cold start each tick), hash walk covers `settle.{settled_ticks,latched}` (lib.rs:11866-11867),
batch order = ascending VesselId = `BodyHandle` order, `surface_upright` verified by hand at
lat/lon 0, poles, and general (ENU mapping correct; pole degenerates finite), unlatch restore
stamps `vel` at the CoM co-rotation (bubble.rs:322-348), the b698eb2/dd3612a relaunch chain
(`SetThrottle` → `unlatched_body` → `InBubble` + integrate same tick) is solid end to end.

---

### [MEDIUM] Finding 1: The delivery-berth occupancy probe tests a different volume than the delivery materializes
**File**: crates/csp-sim/src/manufacture.rs:295-310 (probe) vs manufacture.rs:377-401 (berth) vs crates/csp-view/src/lib.rs:2924-2941 (launch pre-flight)

**Problem**: `delivery_blocked` poses its occupancy candidate with
`orientation: DQuat::IDENTITY` and `altitude_m: def.altitude_m` (no belly offset), while
`deliver_built_vessel` berths the craft **upright** (`surface_upright(lat, lon)`) at
`altitude_m: def.altitude_m − belly`. The launch path (`csp-view/src/lib.rs:2906-2941`) poses its
candidate *exactly as the spawn would berth it* — the convention the manufacture comment at
manufacture.rs:313-322 itself invokes ("the same `spawn_site_occupied` predicate the launch
pre-flight ... share").

**Evidence**: at the Terrin pad (lat −0.092, lon 2.916) local up ≈ (−0.975, 0.222, −0.092), so the
upright berth points a rocket's long axis along ~−x̂ while the probe's identity pose points it
along +ẑ — for a 12 m stack the two AABBs share almost no volume. The belly omission additionally
offsets the probe's origin by |min-z| (0.2–0.5 m typical, more for low-slung builds).

**Impact**: the probe can report "berth free" while the delivered pose overlaps a parked craft —
precisely the coincident-hull ghost-overlap the berth rule exists to prevent (both hulls then
latch statically inside each other; no solver ever separates them; launch then refuses with
"launch site occupied" and `spawn_site_occupant` returns only one of the two). Contrived geometry
is needed (parked craft offset in the direction identity-vs-upright differ), but the W2.6
retry-queue machinery shows this path is expected to be exercised.

**Suggestion**: pose the candidate exactly as `deliver_built_vessel` does —
`orientation: surface_upright(def.latitude_rad, def.longitude_rad)`,
`altitude_m: def.altitude_m − belly` (reuse the same belly computation; better, extract one
`berth_pose(def, &craft) -> SurfacePose` used by both the probe and the delivery, mirroring the
launch path's single-pose discipline).

---

### [MEDIUM] Finding 2: Terrain contact normal is always radial — the doc's µm-scale justification no longer holds, and slope sliding is suppressed
**File**: crates/csp-phys/src/simple.rs:1698 (`let n_w = rel / dist;`), :2246 (`let normal = rel / dist;` in the Baumgarte pass); doc claim at :74-78

**Problem**: since R2 the contact surface is the displaced heightfield `radius_m + h(dir)·amplitude`
(amplitude = 2.4 km on Terrin), but both the velocity solve and the position pass use the purely
radial direction as the contact normal. The module doc justifies the vertex-only/radial-normal
model with "face/edge contacts on a curved surface differ from the vertex manifold by ≪ µm at
home-planet radius" — true for the bare sphere's curvature, first-order wrong for the heightfield
gradient (slopes of tens of degrees are authorable at 1024×512 bake resolution).

**Evidence / Impact**: gravity is radial and the support impulse is radial, so their tangential
components cancel exactly — a craft that comes to rest on **any** slope never experiences a
downslope force and never slides, regardless of μ (real behavior: slides when tan α > μ; with
μ = 0.5 that is every slope ≥ ~27°). Lateral equilibrium is also artificially stable (a downhill
perturbation deepens radial depth → radial correction pushes the craft back *uphill*). Net: crater
walls and hillsides are stickier than physics says — gameplay-forgiving but wrong, and the
in-code justification is stale. The settle latch itself is *not* affected (sink and correction are
both radial, so net per-tick pose delta ~0 at equilibrium — verified reasoning; `lander_20t` and
`dropped_box_rests_on_terrain_not_datum` pass).

**Suggestion**: either (a) derive the contact normal from the heightfield gradient (central
difference over `sample_dir` — deterministic, cheap, same bake), using it for the impulse direction
while keeping radial depth measurement; or (b) keep the v1 approximation but fix the module doc to
state the real bound (normal error = slope angle; slope sliding suppressed) so the next person
doesn't inherit the µm claim.

---

### [MEDIUM] Finding 3: Duplicated force model and duplicated PGS row solve in simple.rs (~2.7k code lines)
**File**: crates/csp-phys/src/simple.rs:1344-1479 (`external_loads`) vs :1536-1616 (`accumulate_forces`); :2015-2068 vs :2069-2121 (`solve_velocity` ground vs pair blocks); :840-850 vs :1044-1054 (`ang`/`effective` closures)

**Problem**: (1) `external_loads` re-implements `accumulate_forces`' gravity/aero/thrust channels
per-part; only the decelerator term is shared (`decel_area_m2`, whose doc says it is shared "so the
two channels can never disagree" — but the other three channels are two hand-maintained copies; a
drift there makes §2.4 structural-failure loads disagree with the forces actually applied).
(2) The ground-manifold and pair-manifold blocks inside each solver iteration are structurally
identical ~55-line sequences (normal → t1 → t2 → Coulomb-disc projection) differing only in the
relative-velocity evaluation and the impulse write-back. (3) The `ang` + `effective` closures are
duplicated verbatim in `PairConstraint::new` and `JointConstraint::new`.

**Impact**: every future solver tuning (friction model, iteration experiments) must be applied in
2–3 places; the two force channels are the load-bearing pair for breakup correctness.

**Suggestion**: extract `fn solve_contact_rows(vn, vt1, vt2, kn, kt1, kt2, target, &mut λs, apply:
impl FnMut(DVec3, f64))` used by both manifolds; extract `fn two_body_effective(...) -> impl
Fn(DVec3) -> f64 + '_`; consider a per-channel force-at-point iterator consumed by both
`accumulate_forces` (summing) and `external_loads` (attributing).

---

### [LOW] Finding 4: Facet-plane reconstruction is eager for every hull every tick, even when nothing overlaps
**File**: crates/csp-phys/src/simple.rs:1781-1795 (`hull_planes` built for all bodies before the AABB pair filter at :1801)

**Problem**: `facet_planes` is the O(n⁴) sweep (C(16,3)·16 ≈ 9k inner ops for the 16-vertex
cylinder prisms most parts resolve to). It is computed for **every hull of every body** in a
multi-vessel bubble before any AABB overlap test — two vessels kilometres apart in one bubble pay
full plane detection per hull per tick (a 30+30-part pair ≈ 0.5M ops/tick of pure waste, against
the §5.3 ≤0.5 ms/tick budget, at ×2 physics warp).

**Suggestion**: make `hull_planes` lazily initialized (`Vec<Option<Vec<Plane>>>`, filled on first
overlapping pair). Poses are frozen during generation, so the memo is exact; the fixed
ascending `(i, j, hull_i, hull_j, direction, vertex)` order is unchanged.

---

### [LOW] Finding 5: A latched vessel can be left floating when its support departs
**File**: crates/csp-phys/src/simple.rs:2362 (`in_contact` counts pair contacts) + crates/csp-sim/src/lib.rs:11154-11223 (`transition_latched`), :3860-3955 (unlatch triggers)

**Problem**: settle qualification counts *any* contact, including vessel-vessel-only (correct for
stacking — `vessel_resting_on_vessel_is_stable` pins it). But when the supporter later unlatches
and flies off, nothing re-evaluates the latched record: §2.6.4's unlatch triggers (control command,
`Unlatch`, activation, nearby impact) don't include "support removed". The vessel (or non-debris
resting hull; debris is impact-removed only on *ground* contacts, lib.rs:4422-4435) stays
`SurfaceLatched` in mid-air until the player interacts with it.

**Impact**: deterministic and self-healing on interaction (unlatch restores it mid-air and it
falls), but a visible oddity: launch a lander out from under parked debris and the debris hangs in
the sky.

**Suggestion**: when `unlatched_body` restores a vessel, unlatch-and-drop any `SurfaceLatched`
vessel whose berth volume overlapped it (the `volumes_overlap` predicate already exists), as a
"nearby impact event"-class trigger.

---

### [LOW] Finding 6: `cast_ray` normal contradicts its own doc for inside-surface origins; fixed 96-step scan can miss grazed terrain
**File**: crates/csp-phys/src/backend.rs:126-128 (doc) vs crates/csp-phys/src/simple.rs:2542-2552, :2596-2628

**Problem**: `Hit`'s doc says the normal points "back toward the ray origin side", but for an
origin below the surface the sphere branch returns the *exit* root with the outward normal
(pointing away from the origin). Separately, the terrain branch's fixed 96-step sign-change scan
over the whole entry/exit window can step over a terrain peak crossed twice within one interval at
long range / grazing angles (deterministic, but lossy). No production caller exists yet — the
trait method is the §2.9 contract surface only.

**Suggestion**: when the fixpoint is wanted, negate the normal on the exit-root branch (or document
the exit case); leave the scan as-is until a consumer imposes a real accuracy requirement.

---

## PRD coverage table

| PRD item (§) | Status | Evidence |
|---|---|---|
| §2.9 `PhysicsBackend` trait (step/cast_ray/contacts) | **WIRED** | backend.rs:183-199 verbatim; `cast_ray` has no production consumer yet (trait completeness only, no gameplay gap) |
| §2.9 SimpleBackend, pinned semi-implicit Euler | **WIRED** (documented refinement) | simple.rs:1621-1655; gyroscopic term uses a midpoint stage (module doc :1626-1636 quantifies the ×e¹¹ energy pump it kills) — a strictly better, still-deterministic reading of the pin |
| §2.9 PGS contact: 8 iters, β=0.2, restitution 0, two-direction Coulomb, fixed `(body_id, contact_index)` order | **WIRED — better than spec** | SOLVER_ITERATIONS=8, BAUMGARTE_BETA=0.2 (simple.rs:237-241); Coulomb *disc* projection beyond the spec's per-direction clamps (:2050-2067); Baumgarte at position level, spec-consistent reading justified in-module (:166-189) |
| §2.6.4 settle latch (v/w gates, 128 ticks, terminal state) | **WIRED** | simple.rs:2354-2440; pose-delta gates beyond spec prevent mid-slide latching (test :3817-3862); latch zeroes throttle per §2.6.2's leave-the-bubble rule, any throttle command unlatches first (:2463-2474) |
| §2.6.4 body-fixed pose record, exact restore, no drift | **WIRED** | bubble.rs:264-348; spinning-world phase convention consistent between `surface_radius`, capture, restore, and the renderer's unspin; CoM-slot velocity stamping correct |
| §2.9 M2 exit criterion (20 t lander, 10k ticks, ‖v‖<1e-6, constant hash) | **WIRED** | csp-sim lib.rs:15403 `lander_20t_latches_and_hash_freezes` (full 10k-tick observation, spinning world, restore-hash determinism) + csp-phys :2932 (20 t box, bitwise fixed point) |
| §2.9 `RapierBackend` for jointed islands | **DEGRADED — documented v1 deviation, not §6.4-sanctioned** | backend.rs:49-58: soft-capture solved inside SimpleBackend's PGS instead; `rapier3d-f64` still workspace-pinned, no dependent crate. Defensible (compliant spherical joint = 3 bilateral PGS rows; ≤3 s transient) but it is an unsanctioned spec change — note, not a blocker, since cap-16 transients keep the surface tiny |
| §2.9/§8 oracle cross-check (SimpleBackend vs Rapier, ≤2 s contact-free, nightly) | **PARTIAL** | The four named analytic scenarios exist as in-crate unit tests (free fall :2779, terminal velocity :2817, torque-free precession :3484, tumble energy :4211) with stated tolerances; the Rapier-side nightly harness does not exist (no crate depends on rapier) |
| M2 ground contact + pre-voxel drag/thrust/gravity | **WIRED** | simple.rs:1536-1616; terrain-aware contact surface (R2) closes the sink-below-visible-ground defect (test :3874) |
| M6 vessel-vessel contact + transient joints (one bubble = one constraint problem) | **WIRED** | pair manifold :1757-1978, joints :1097-1130/:2134-2169; no island decomposition needed (batch rebuilt per tick, ascending VesselId); momentum conservation pinned by tests :3709/:4078 |
| W2.6 delivery-berth upright mechanics | **WIRED** (predicate divergence — Finding 1) | manufacture.rs:371-401 (upright + belly, mirrors launch convention); retry-queue on occupied berth |
| §2.4/§2.9 nothing on the §6.4 cut list | — | nothing in this cluster was cut; the two deviations above are documented in-code, not in PLAN |

## Top 3 issues
1. **[MEDIUM] Finding 1** — delivery-berth occupancy probe (IDENTITY pose, no belly) tests a
   different volume than the upright+belly berth it guards; ghost-overlap is the designed-against
   failure. One-function fix (`berth_pose` shared by probe + delivery).
2. **[MEDIUM] Finding 2** — terrain contact normal is radial-only; the µm-scale doc justification
   is stale and slope sliding is suppressed on all gradients (support impulse and gravity share the
   radial line). Fix the normal from the heightfield gradient or fix the doc.
3. **[MEDIUM] Finding 3** — `external_loads` vs `accumulate_forces` and the ground/pair PGS blocks
   are hand-duplicated; the force-channel pair directly gates breakup correctness. Extract the
   shared row solver and two-body effective-mass helper.
