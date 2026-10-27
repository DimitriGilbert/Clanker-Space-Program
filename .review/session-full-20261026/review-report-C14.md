# Review Report C14 — Physics: environment (aero / thermal / structural / weld / hull)

Session `session-full-20261026`, working tree at HEAD past `6cc2cb4`. Scope:
`crates/csp-phys/src/{aero,thermal,structural,weld,hull}.rs` read in full; cross-checked
against the consumers that exercise them (`csp-sim/src/lib.rs` tick loop, `csp-sim/src/transfers.rs`
mass retally, `csp-phys/src/simple.rs` force attribution) because every finding below was traced
end-to-end and, where possible, reproduced empirically in the real sim. All five files' shipped
tests pass (`cargo test -p csp-phys`: 157+9+5 green). Temporary probes used for measurement were
removed; the tree is clean of my edits.

---

### [CRITICAL] Finding 1: The shipped parachute tears off its own mount at canopy catch — the re-entry recovery chain cannot complete

**File**: `data/parts/csp.chute.s125.ron:42-56` + `crates/csp-phys/src/simple.rs:2651-2664` + `crates/csp-phys/src/simple.rs:1428-1437` + `crates/csp-phys/src/structural.rs:299-303`

**Problem**: The chute's canopy catch gates (`deploy_alt_m: 6_000`, `deploy_speed_mps: 260`,
`deployed_drag_area_m2: 120`) and its mount limits (`max_axial_n: 150_000`, `max_shear_n: 60_000`)
are mutually incompatible by roughly 20x. The first tick both gates pass, the full 120 m² of Cd·A
applies (`decel_area_m2` ramps from the *deploy stamp*, not from gate-crossing — a chute deployed
high, the natural player action, is fully ramped long before the gate), the canopy load
`0.5·ρ·v²·120` at v=260 m/s and ρ(4-5 km) ≈ 0.7 kg/m³ is **~2.8 MN**, and `external_loads`
attributes that force to the chute node, so `failed_joints` sees an axial/shear load ~19x over
`max_axial_n` → structural breakup: the chute rips off its mount.

**Evidence** (reproduced in the real sim with the shipped RON values — pod.mk1 + science.canister +
battery.z1k + chute.s125, real structural limits, real thermal defs, dropped from 45 km at
1.9 km/s, chute deployed at t=0):

```
t=30s alt=5.54km v=313 m/s nodes=4   (gates: alt ok, v still > 260)
t=40s alt=3.06km v=212 m/s nodes=3   <-- chute torn off at the v<=260 crossing
t=50s alt=1.18km v=169 m/s nodes=3   (packed drag only, ~150 m/s terminal)
t=60s alt=0.00km v=  3 m/s nodes=1   <-- canister+battery also torn off at touchdown
```

Even the steady state rips: at 100 m/s near sea level the canopy pulls 0.66 MN = 4.4x the axial
limit; the mount can only hold its own canopy below ~47 m/s (`0.5·ρ·v²·120 ≤ 150 kN`), which no
pod-class craft ever reaches packed (terminal ~130-150 m/s at β = m/CdA ≈ 1 000 kg/m²). After the
tear, the remaining stack impacts at ~150 m/s and the touchdown contact loads strip the canister
and battery — the science-bearing leg. Note the sim's own e2e chute test
(`csp-sim/src/lib.rs:14721` `deployed_chute_decelerates_and_lands`) passes only because its fixture
uses `StructuralDef::unlimited()` (`lib.rs:14694`); the shipped part's limits were never exercised
against the canopy force.

**Impact**: The recovery path — Phase1's headline ("recovery reachable (parachute + auto-rails
re-entry)") and the chute part's own design comment ("make `RecoverData` a real path") — is
unreachable with shipped parts. Every nominal LEO entry ends with the chute torn off and the
science leg destroyed on impact. This is a whole-game-playability break, not a tuning nuisance.

**Suggestion**: (a) Reef the canopy by load, not by clock: clamp the effective area to a max
canopy force, `area_eff = min(deployed_drag_area, F_max/(0.5·ρ·v²))` with `F_max` ~ 100 kN, in
`decel_area_m2` — this preserves the 12 m/s terminal velocity, removes the snap for any entry
speed, and makes `deploy_time_s` a fine-grained ramp on top; and/or (b) author the mount limits
for what a canopy hard-point carries (~3-4 MN axial). Also ramp from gate-crossing rather than
the deploy stamp. Re-run the e2e test with the real part data (finite limits) as the regression.

---

### [HIGH] Finding 2: Stage-2 aero tables rebuild synchronously on the sim thread every fuel quantum during powered ascent (measured 6.9 ms/tick starter craft, 28.4 ms/tick 22-node stack)

**File**: `crates/csp-phys/src/aero.rs:1289` (`build_aero_tables`) + `crates/csp-sim/src/lib.rs:10938` (`hash_craft` hashes `p.mass_kg`) + `crates/csp-sim/src/lib.rs:11100-11109` (`aero_tables_stale`) + `crates/csp-sim/src/transfers.rs:484` (`apply_bucket_delta` mutates `part.mass_kg`)

**Problem**: The aero table's cache key and staleness predicate reuse `craft_graph_hash`, which
includes each part's bucketed `mass_kg`. Every tank quantum (cap/1024) mutates that mass, so
during any burn the table is invalidated essentially every tick (`s160` + `sparc`: 63.7 kg/s at
full throttle crosses the 0.79 kg LF quantum and 0.97 kg ox quantum about once per tick), and the
per-tick staleness pre-pass (`lib.rs:4086-4094`) rebuilds the entire Stage-2 output —
re-voxelization, 66 lattice sweeps, 30-knot grid — inline on the sim thread. Measured in release
on this devbox: **6.9 ms** for a Stock-Hopper-sized craft, **28.4 ms** for a 22-node stack.

**Impact**: PLAN §2.4.1 defines Stage 2 as "expensive, deferred", computed off the sim thread and
installed by command ("<500 ms off the sim thread"); §5.3 budgets ≤ 0.5 ms/tick for a 200-part
vessel. Powered ascent now spends 14-57x the entire per-tick budget on rebuilding a table whose
*geometrical* inputs (the only things the voxelizer and sweeps read) did not change — only
`com_local` did, and by at most one quantum's worth of mass movement. The starter craft burns
~44% of wall-clock on rebuilds at ×1; a mid-size stack cannot hold real-time ×1 at all, and
physics warp ×2 is unreachable during ascent. Geometry tables are being keyed on mass.

**Suggestion**: Split the keys. The aero-table binding should be a *geometry/topology* hash
(graph + colliders + effective indices, excluding `mass_kg`), with `com_local` either passed into
`force_torque_body` from the live body or patched in `rebody` (`transfers.rs:630` already has the
re-weld; today it clones the stale table). Then a fuel quantum costs a f64 update, not a
re-voxelization, and the expensive rebuild happens only on real geometry changes (staging,
breakup, jettison) — which is what §2.4.1's "once per design × configuration" says.

---

### [MEDIUM] Finding 3: Re-entry heating can never be lethal — M4b's "lethal in the right places" is unreachable with the shipped calibration

**File**: `crates/csp-phys/src/thermal.rs:120` (`CONVECTIVE_HEAT_FRACTION = 0.005`)

**Problem**: The convective model is mechanically correct (per-part lumped input
`η·½ρv³·A_eff`, shielding-aware via the aero tables, radiation + conduction, per-tick explicit
Euler at the immutable dt), but η = 0.005 against authored heat capacities (20 kJ/K battery …
450 kJ/K engine) and Terrin LEO entry speeds (≤ 2.3 km/s) yields peak lump temperatures of
**~310 K** against `max_temp_k` 800-1600 K — measured in the probe above: the chute (900 K limit,
the most fragile part, deliberately side-mounted) peaked at 247 K. The instantaneous skin
equilibrium does reach ~1600 K at peak flux, but the pulse lasts seconds against a 40 kJ/K lump:
~10 K/s for a few seconds. Even a maximum-energy home-system entry (~3 km/s from a Duva return)
only triples the flux — nowhere near the 3-4x *temperature* margin the fourth-power radiation
balance requires. No authored entry, on any shipped body, can push any part past its limit.

**Impact**: M4b's exit criterion "reentry survivable **and lethal in the right places**" is half
met: survivable everywhere, lethal nowhere. Overheat (`Explosion`, `Overtemperature` reliability
stress at 0.9·max_temp, §3.4) is dead code in practice for re-entry; the whole thermal chain
reduces to a presentation driver for the plasma sheath. Not a playability break (the game is
merely forgiving), but a PRD commitment that silently became decoration, and η is a hardcoded
constant the docstring itself defers to `balance.ron`.

**Suggestion**: Either move `CONVECTIVE_HEAT_FRACTION` to `balance.ron` and calibrate it so a
steep Terrin entry pushes unshielded low-C parts (battery 20 kJ/K @ 800 K) into the 0.9·Tmax
reliability-stress band while shielded/pod parts survive (η ~ 0.02-0.05 by the docstring's own
calibration arithmetic), or record the non-lethality as a deliberate v1 cut in PLAN §6.4 terms.
Shielding asymmetry already exists in the model — it is currently unexploited by the tuning.

---

### [MEDIUM] Finding 4: Joint-load torque arm uses the part origin, not the attach point — breakup torques systematically wrong (unsanctioned deferred approximation)

**File**: `crates/csp-phys/src/structural.rs:210-218`

**Problem**: `p_j = transforms[id].offset` is the child part's *origin*, not the snapped attach
point; the in-code `KNOWN-DEFERRED (PRD-10 M3 / Phase-3 W3.4h)` note says the real joint point
lives in `snap_transform` and was deferred pending a `CraftDef` schema bump. All three reported
loads derive from `p_j`: `i_pj` (inertia about the joint), the `(c_S − p_j)×a` transport term, and
the `τ_J` external-moment reference — so `torque_nm` is off by the (attach-point − origin) arm
whenever parts do not mount origin-to-origin (every radial mount and every offset stack joint,
e.g. the chute at x=0.9 from the pod axis).

**Impact**: `max_torque_nm` comparisons (8-30 kN·m on shipped parts) are evaluated against a
torque that can carry the wrong lever arm by fractions of a metre — both false positives (spurious
breakups on radially-mounted parts under g-loads + angular accel) and false negatives. PLAN §2.4
specifies structural breakup with no accuracy carve-out, and §6.4's cut list does not sanction
this approximation. The axial/shear channels are unaffected (moment-independent), which is why
ascents still pass.

**Suggestion**: Land the schema carry-through the note already describes: serialize the snapped
attach point on `PartNode` (or `WeldPart`), rebless goldens once, and use it for `p_j` and the
thermal/structural attach-axis convention. If that cannot land this cycle, at minimum bound the
error in a test (radial-mount fixture with the arm at its authored maximum) so the deferred state
is measured, not folklore.

---

### [NIT] Finding 5: Bit-level equality comparison hand-written twice; one doc comment cites a private prompt

**File**: `crates/csp-phys/src/aero.rs:818-882` (`bit_eq`/`dirs_bit_eq`) + `crates/csp-phys/src/weld.rs:1196-1236` (`assert_bit_identical`)

**Problem**: The same `to_bits`-walkover exists as shipping code in `CraftAeroTables::bit_eq`
(§2.4's debug byte-compare) and as test code in weld — ~120 duplicated lines of per-field bit
comparisons that must grow in lockstep with the structs. Related: `aero.rs:123-124` justifies the
Mach knots as `the prompt's "4–6 Mach knots"` — a reference to a conversation, not to any spec in
the repo, which will mislead the next reader auditing constants.

**Suggestion**: One `BitsEq` trait (`f64`, `DVec3`, `Vec<T: BitsEq>` impls) in csp-core next to
`StateHasher`, used by both sites; rewrite the knot comment to cite the actual source of the
requirement (the Cm_α exit criterion span).

---

## PRD coverage table (PLAN §2.4 / §2.4.1 / M4b / M4c, environment files)

| PRD requirement | Status | Evidence |
|---|---|---|
| §2.4 Weld Stage 1: pure fn, mass/CoM/inertia, force sites, compound collider, bit-identical, perm test | **WIRED** | `weld.rs:365`; typed errors incl. non-unit rotations; bit-identity + hand-computed tensor tests green |
| §2.4 Weld "runs on sim thread, always"; no time-triggered caches | **WIRED** | every sim path calls `weld` synchronously; re-weld driven by quantized mass only (`transfers.rs:617`) |
| §2.4 Breakup split + momentum conservation to defined tolerances; children inherit parent aero tables | **WIRED** | `structural.rs:333-394`; tolerance test passes; fragments inherit the parent table at split, hash staleness rebuilds it next tick (window: 1 tick — better than the plan's "until a Stage-2 recompute lands") |
| §2.4.1 Isotropic 0.15 m voxels, 4M budget, edge doubling + warning | **WIRED** | `aero.rs:986-1042` (`VOXEL_TARGET_EDGE_M`, u128-safe budget loop) |
| §2.4.1 Voxelize once, sweep rotated frames; deterministic fixed-order traversal | **WIRED** | single body-frame grid; binning sweep `aero.rs:1158`; BTreeMap/fixed orders; `tables_are_bit_deterministic` green |
| §2.4.1 66-direction geodesic set | **WIRED (documented divergence)** | frequency-4 octahedron (66) vs PLAN's "(icosahedron, 1 subdivision)" = 42 — PLAN is self-contradictory; module doc resolves toward the pinned count (parallel finding, not re-derived) |
| §2.4.1 Barycentric interpolation over triangulated hull | **WIRED** | `DirectionSet::weights`, shared-edge determinism, `cm_alpha_table_vs_direct` checks 5% vs non-tautological direct re-sweep |
| §2.4.1 Per-ConfigMask tables, 8-mask cap, Hamming fallback, editor warning | **WIRED** | `aero.rs:121`, `ensure_aero` nearest-mask path (`lib.rs:6890`); mask remap on jettison (`lib.rs:7343/7423`) |
| §2.4.1 Stage 2 deferred off-thread, `InstallAeroTables`, "<500 ms off sim thread", refuse-to-launch | **DEGRADED** | `InstallAeroTables` never issued (parallel finding); Stage 2 computed synchronously by `ensure_aero`; Finding 2 quantifies the sim-thread cost during ascent — the requirement is materially violated, not just re-homed |
| §2.4.1 Tables never serialized / re-derived on load; deletable cache | **WIRED** | `#[serde(skip)] aero` on `WeldedBody`; rebuild on restore |
| M4b thermal: per-part, conduction, reentry heating, "survivable **and lethal in the right places**", pure-function-of-state | **DEGRADED** | model + determinism + NaN guard + input-enforced stability WIRED (`thermal.rs`, `UnstableThermalCoupling` gate); lethality unreachable — Finding 3 |
| M4c structural breakup thresholds vs playability (starter craft on normal ascent) | **WIRED** | ascent margins ≥ 2x (joint loads at liftoff TWR≈5 and burnout ~14 g stay under all authored limits — traced); the playability break is the chute data interaction, Finding 1 |
| §3.7 re-entry heat as presentation driver | **WIRED (sim side)** | flux/A_eff exposed to view (plasma display is C19's, parallel) |

## Top 3 issues

1. **[CRITICAL] Finding 1** — chute rips off its mount at canopy catch (~2.8 MN vs 150 kN);
   recovery/science-return cannot complete with shipped parts. Verified in-sim with shipped values.
2. **[HIGH] Finding 2** — Stage-2 aero rebuild storm on the sim thread every fuel quantum
   (6.9-28.4 ms/tick measured); tables keyed on mass when only geometry matters.
3. **[MEDIUM] Finding 3** — re-entry heating cannot kill anything (peak ~310 K vs 800-1600 K
   limits); M4b's "lethal in the right places" is dead code, η hardcoded instead of a tunable.
