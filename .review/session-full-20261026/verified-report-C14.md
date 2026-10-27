# Verified Report C14 — Physics: environment (aero / thermal / structural / weld / hull)

Verification agent for `review-report-C14.md`, working tree at HEAD past `7dd4b86`.
Every claim was re-derived from source and, where decisive, re-executed against the real sim:
a `/tmp` harness (`c14verify`, path-deps on `csp-sim`/`csp-phys`/`csp-data`/`csp-core`) drove the
real `World` over the real Terrin environment (`data/systems/home/terrin/body.ron` via
`Environment::for_body`) with crafts built from the shipped RON constants verbatim (pod.mk1,
science.canister, battery.z1k, chute.s125, tank.s160, engine.sparc — masses, structural limits,
thermal defs, `ParachuteSpec` all copied exactly). Harness deleted after use; no repo file edited.

**Verdict summary: 3 confirmed (one severity-adjusted CRITICAL→HIGH per the counterfactual rule),
1 dismissed with a corrected replacement finding, 1 confirmed as nit.**

---

### Finding 1: Shipped parachute tears off its mount at canopy catch — CONFIRMED
**(severity CRITICAL → HIGH per the counterfactual rule: viable ballistic profiles exist and were reproduced end-to-end)**

**Original claim**: gates (6 km / 260 m/s / 120 m²) vs mount limits (150 kN axial / 60 kN shear)
are incompatible ~20x; `decel_area_m2` ramps from the deploy stamp so a chute deployed high is
fully ramped at gate-pass; ~2.8 MN at catch → chute rips off; recovery chain unreachable; e2e test
masks it with `StructuralDef::unlimited()`.

**Verification — constants and code (all confirmed by reading):**
- `data/parts/csp.chute.s125.ron:42-56`: `max_axial_n: 150_000`, `max_shear_n: 60_000`,
  `deploy_alt_m: 6_000`, `deploy_speed_mps: 260`, `deployed_drag_area_m2: 120`, `deploy_time_s: 1.5`.
  The part's own header promises a catch "below 6 km and 260 m/s … land intact and make
  `RecoverData` a real path."
- `simple.rs:2651-2664` (`decel_area_m2`): ramp = `(at_s − deployed_at_s)/deploy_time_s`, clamped —
  anchored at the **deploy stamp**, not gate-crossing. Deployed-at flows from the deploy command's
  tick (`lib.rs:8941`). So deploy-early → ramp long complete → **full 120 m² on the first tick both
  gates pass**. Confirmed.
- `simple.rs:1428-1437` (`external_loads`): the decel force `0.5·ρ·A·v²` is attributed to
  `site.node` at `site.local_pos`. `structural.rs:219` (`f_j = a.force·−1 + point_accel·a.mass`) and
  `structural.rs:299-303` (`failed_joints` vs the child's limits) confirmed.
- Re-derived atmosphere numbers (Terrin: p₀=101 325 Pa, H=5 600 m, ρ₀=1.225): ρ(6 km)=0.48,
  ρ(4.5 km)=0.61, ρ(3 km)=0.76. Worst-case gate-pass load (v=260 @ 6 km): `0.5·0.48·260²·120` =
  **1.94 MN ≈ 13x axial / 32x shear**; at the reviewer's catch point (v≈212 @ 3 km, ρ=0.76):
  ≈ 2.04 MN ≈ 14x/34x. The report's "roughly 20x / ~2.8 MN" used ρ≈0.7 (actual 0.61-0.76) — same
  conclusion, slightly optimistic constant. Mount holds its own canopy only below
  `v ≤ √(2500/ρ)` = 45-65 m/s; packed terminal velocity of pod-class stacks is ~125-150 m/s
  (measured in-sim: 146 → 131 m/s between 3-5 km), so **no ballistic free fall ever reaches the
  hold speed before catch**. Confirmed.
- e2e masking confirmed: `lib.rs:14694` — `chute_craft()` fixture sets `StructuralDef::unlimited()`;
  test `deployed_chute_decelerates_and_lands` at `lib.rs:14721`. Note the fixture is even gentler
  than the report implies (1.5→0.5 s ramp, 80 m/s from 1 km) — with real limits its own catch
  profile pulls ~419 kN at full ramp (2.8x axial). The shipped part's limits were never exercised.

**Verification — in-sim reproduction (natural profile):** pod+canister+battery+chute
(995 kg, canister on the pod top node per the pod RON's own snap convention, chute radial),
dropped from 45 km @ 1 900 m/s, `DeployPart` at t=0:

```
t=30.0s alt=6083 v=280 nodes[4]   (both gates not yet true)
t=31.0s CHUTE NODE GONE           <- torn at the FIRST tick alt<=6000 AND v<=260
t=69.0s SURFACE LATCHED nodes[1] pod   (science-bearing canister+battery lost ~t=15s, see below)
```

Tear at first gate-pass reproduced exactly (mine at ~5.9 km, reviewer's at 3 km — craft-dependent
deceleration, identical mechanism). The mount-load joint evaluation was also replicated per-tick
via the public `external_loads`/`joint_loads`/`failed_joints` on the live body: on a gate-pass at
MN scale the joint load is ~the full canopy force (chute subtree 55 kg ≪ force) → immediate fail.

**Counterfactual (the coordinator's decisive question)** — "is there ANY flight profile where the
stock chute survives to latch with a realistic craft?" **YES — three profiles reproduced intact to
`SurfaceLatched` with terminal 11-13 m/s:**
- Deploy **under ~2.5 km while falling packed** (4-node craft, canister+battery already lost to
  entry aero): deploy @ 2 500 m / 131 m/s → survived, latched `pod+chute`.
- Same, lighter profile: deploy @ 2 000 m / 125 m/s → survived, latched at t=236 s.
- Deploy **already slow**: 3 km @ 60 m/s → survived trivially.
- Mechanism (per-tick probe): on late deploy the 1.5 s ramp races the deceleration; canopy force
  peaks ≈ 109 kN < 150 kN axial, and the craft's canopy-torque tumble (`ω_y` ±14 rad/s) dithers the
  load between the axial/shear channels — measured channel peaks **axial 113 kN (<150 kN),
  shear 49 kN (<60 kN)**. It holds, legitimately, in the shipped sim.
- **Fragility documented**: deploy @ 1 200 m / 117 m/s — near-identical peak dynamic pressure —
  **tore within 0.4 s**. Survival at the margin is tumble-phase luck (deterministic per seed, not
  design margin), and nothing in the game signposts the window; the part doc and the 6 km/260 gate
  semantics actively instruct the opposite (deploy-and-forget), which reliably destroys the chute.
- Additional break found while reproducing: with the editor-default top-mounted science.canister +
  battery.z1k, an orbital-speed entry (1.9-2.3 km/s) strips the science leg at ~15-25 km under aero
  loads **regardless of the chute** (limits 200/80 and 150/60 kN vs aero torques at q≈0.05-0.1 MPa).
  So the *orbital science-return* chain is broken by more than the chute in the default mount; a
  gentle-entry (suborbital hop) recovery does complete via the late-deploy window.

**Per the standing rule (viable profile exists → downgrade), final severity: HIGH.** The report's
core physics, its ~MN arithmetic, the ramp-from-deploy-stamp mechanism, the tear-at-gate-pass
reproduction, and the e2e-test masking all stand; "unreachable with shipped parts" is overstated
because undocumented knife-edge profiles complete the chain.

**Fix directions from the original report remain sound** (reef by load `area_eff =
min(A, F_max/(0.5ρv²))`, and/or author the mount for canopy hard-point loads; ramp from
gate-crossing; re-run the e2e with finite limits).

---

### Finding 2: Stage-2 aero tables rebuild synchronously every fuel quantum (6.9-28.4 ms/tick during ascent) — DISMISSED (mechanism inverted in the current tree; replacement finding below)

**Original claim**: `hash_craft` includes `p.mass_kg` → every fuel quantum (cap/1024) invalidates
the aero table → per-tick pre-pass rebuilds Stage 2 inline on the sim thread; measured 6.9 ms/tick
starter, 28.4 ms/tick 22-node; PLAN §2.4.1's "<500 ms off the sim thread" / §5.3's 0.5 ms/tick
violated 14-57x during powered ascent.

**Verification — the static chain is real, but the trigger never fires:**
- `lib.rs:10938` (`hash_craft` hashes `p.mass_kg`) — confirmed. `contents_bucket`
  (`transfers.rs:302-307`) = cap/1024 — confirmed. Sparc mdot = 200 000/(320·9.81) = **63.7 kg/s** —
  confirmed; s160 quanta: LF 162·5/1024 = **0.791 kg**, OX 198·5/1024 = **0.967 kg** — confirmed;
  at 64 Hz a bucket flips ~1.1x/tick. `apply_bucket_delta` mutates `part.mass_kg`
  (`transfers.rs:484`) — confirmed. The per-tick pre-pass (`lib.rs:4086-4095`) calls `ensure_aero`
  inline, which calls `build_aero_tables` synchronously (`lib.rs:6883`) — confirmed.
- **But**: `craft_graph_hash` (`lib.rs:11077-11089`) memoizes on `CraftDef::hash_memo`, and the
  memo's own contract (`bubble.rs:130-134`) says: *"mutate a `CraftDef` only before it is shared,
  or by replacing it wholesale — an in-place write to a memoized def leaves the memo stale (wrong
  direction)."* `transfers::debit`/`credit`/`leak_part` mutate `craft.parts[*].mass_kg` **in place**
  through `vessel.craft.as_mut()` — the documented forbidden write. The stored def's memo therefore
  keeps returning the **pre-burn** hash; `aero_tables_stale` (`lib.rs:11100-11110`) compares the
  attached table's hash (also pre-burn) against the stale memo → **equal → never stale → no
  rebuild, for the entire burn.**
- **Measured end-to-end** (4-node engine+tank+pod+chute over real Terrin, full throttle from full
  tanks to dry): coast **0.010-0.011 ms/tick**, burn **0.012-0.013 ms/tick**. No storm. Proof the
  table never rebuilt: after the burn (tank 2 020 → 220 kg) the attached table still carries the
  spawn `com_local` — `(−0.013, 0, 2.330)` vs fresh weld `(−0.024, 0, 2.723)`, **stale by 0.39 m**.
- The reviewer's standalone cost is reproducible — `build_aero_tables` directly: **7.60 ms** for a
  4-6-node starter-class stack, **65.9 ms** for a 22-node stack (mine: 19 tanks, 40 m tall; the
  reviewer's 28.4 ms presumably a squatter stack) — i.e. their measurement was the build cost, and
  the "every fuel quantum" attribution was inference that the memo defeats. All per-tick craft
  replacements are event paths (jettison `lib.rs:7163`, breakup `7414`, restore `11657`), so the
  storm cannot materialize during a nominal ascent.
- PLAN cites verified: §2.4 "Stage 2 — aero tables (expensive, deferred)" (:293), "<500 ms off the
  sim thread" (:299), "once per design × configuration" (:335), ≤0.5 ms/tick (:1441).

**Corrected finding (real, different)**: (a) the debit/credit/leak path violates the
`CraftHashMemo` write rule, making the aero staleness predicate **blind to mass changes** — the
tables (and their `com_local`, used for force torque arms) stay pinned at the spawn configuration
for the whole flight (0.39 m CoM error after one tank; grows with burn extent). (b) The aero-table
binding keyed on a mass-inclusive hash remains a **landmine**: any fix that makes the memo honest
(including the write-rule fix itself) instantly converts every burn into a per-quantum
7-66 ms synchronous rebuild storm — the exact design the report describes, one memo-fix away.
(c) When staleness *does* fire today (spawn, staging, jettison, breakup), the rebuild is
synchronous on the sim thread: a 7.6-66 ms one-tick hitch, still off §2.4.1's intent that Stage 2
install by command off-thread (`InstallAeroTables` is defined but never issued — parallel finding).
The suggested fix (geometry-only table key + `com_local` patched in `rebody`, which already
re-welds on mass change and already clones the table "the mass change alters no geometry",
`transfers.rs:629-648`) is exactly right and fixes both directions.

---

### Finding 3: Re-entry heating can never be lethal (η=0.005 hardcoded) — CONFIRMED

**Verification**:
- `thermal.rs:120`: `pub const CONVECTIVE_HEAT_FRACTION: f64 = 0.005;` — hardcoded; the docstring
  itself calls it "the pinned v1 value pending a `balance.ron`-level tunable" and its own
  arithmetic (η·½ρv³·A ≈ 1.8 MW on a 400 kJ/K lump ≈ 4.5 K/s) supports the non-lethality reading.
- **Measured in-sim** (real env, real shipped thermal defs): pod+canister+battery+chute dropped
  from 69 km @ **2 300 m/s straight down** — steeper and faster than any real Terrin LEO entry
  (circular 75 km = 2 287 m/s): peak lumps **pod 259 K** (limit 1 200 K), canister 208 K (1 000 K),
  battery 249 K (800 K), chute 214 K (900 K). Nothing approached even the 0.9·Tmax reliability
  band (720-1 080 K). The shipped test rig pins the same story: `reentry_heating_survivable`
  (`lib.rs:16129-16167`) asserts a peak of **273.98 K (+11 K over ambient)** as a deterministic
  regression pin. Lethality is exercised only by a hand-authored 350 K-ceiling craft
  (`overheat_destroys_vessel`, `lib.rs:16102`) — no shipped part is reachable.
- M4b's exit criterion "reentry survivable **and lethal in the right places**" (`PLAN.md:1499`)
  is therefore half-met. Report's numbers (~310 K peak, chute 247 K) are profile-dependent but the
  conclusion is exactly right; severity MEDIUM appropriate (PRD commitment became decoration;
  not a playability break).

---

### Finding 4: Joint-load torque arm uses part origin, not attach point — CONFIRMED

**Verification**: `structural.rs:210-217` — `let p_j = transforms[id].offset;` under the verbatim
`KNOWN-DEFERRED (PRD-10 M3 / Phase-3 W3.4h)` note; all three torque terms (`i_pj`, the
`(c_S−p_j)×a` transport, the `τ_J` reference) derive from `p_j` (`structural.rs:218-223`).
`snap_transform` exists with the real snapped point (`session.rs:913`, used at `1359/1710`),
so the note's claim that the joint point is known at attach time is accurate. PLAN §2.4 specifies
breakup with momentum tolerances only and **no accuracy carve-out** for the joint point; §6.4's
cut list (read in full) does not sanction the approximation — "unsanctioned" holds. The
axial/shear channels are moment-independent (confirmed from the code), so ascents are unaffected —
consistent with the M4c WIRED row. My chute runs incidentally showed the torque channel doing real
work on protruding parts (canister/battery torn at ~15-25 km under entry aero torque), i.e. this
channel governs real breakups while carrying the wrong lever arm. MEDIUM appropriate.

---

### Finding 5: Bit-equality walkover duplicated; comment cites a private prompt — CONFIRMED (nit)

**Verification**: `aero.rs:818-882` (`bit_eq`/`dirs_bit_eq`, shipping) and `weld.rs:1196-1236`
(`assert_bit_identical`, test-only) — both hand-rolled per-field `to_bits` walks, ~104 lines total
(report said ~120; close enough for a nit). One softening caveat: the two sites compare **different
types** (`CraftAeroTables` vs `WeldedBody`), so "must grow in lockstep" is about the shared
technique, not literally shared fields — a `BitsEq` trait suggestion remains reasonable.
`aero.rs:123-124` cites `the prompt's "4–6 Mach knots"` — confirmed; no "Mach knots" language
exists anywhere in PLAN.md (the related spec text is M4a's `Cm_α` ≤5% exit criterion, `PLAN.md:1498`),
so the comment references a conversation, not a repo spec. NIT appropriate.

---

## PRD coverage table — verification notes

| Row | Verdict | Note |
|---|---|---|
| Weld Stage 1 / no time-triggered caches / never-serialized tables / mask cap + Hamming / voxel constants / 66-vs-42 divergence / barycentric interpolation | **CONFIRMED** | all spot-checked at the cited sites (`transfers.rs:617-625` re-weld on quantized mass only; `#[serde(skip)] aero` `weld.rs:158-161`; `ensure_aero` nearest-mask `lib.rs:6878-6897`; `aero.rs:109-121` constants + module-doc divergence) |
| Breakup children inherit parent tables; rebuilt next tick | **CONFIRMED** | fragments get fresh `CraftDef`s (memo empty → hash recomputed → staleness fires), so this row is unaffected by the Finding-2 memo issue |
| §2.4.1 Stage 2 deferred/off-thread — DEGRADED | **CONFIRMED DEGRADED, wrong justification** | `InstallAeroTables` is indeed never issued (only defined/rejected-on in csp-sim; no producer anywhere); Stage 2 is computed synchronously (spawn `lib.rs:6098`, `ensure_aero` `6883`). But "Finding 2 quantifies the sim-thread cost during ascent" is wrong — burns cost nothing extra today; the real costs are spawn/staging/jettison hitches (7.6-66 ms) and the stale-`com_local` blindness |
| M4b thermal DEGRADED (lethality unreachable) | **CONFIRMED** | Finding 3, measured |
| M4c structural thresholds vs playability | **PLAUSIBLE, not re-derived** | ascent margins not re-measured; consistent with my liftoff-free scenes; the playability break is the chute interaction (Finding 1) |
| §3.7 re-entry heat as presentation driver | **CONFIRMED** | `FlightBody::plasma_w_m2` (`csp-view/src/flight.rs:272`, onset/full constants at :46-48) exposes the convective W/m² input to the view |

## Adjusted top 3

1. **[HIGH, was CRITICAL] Finding 1** — chute tears off at canopy catch on the designed/documented
   profile (reproduced: tear on the first tick both gates pass, ~1.9-2.8 MN vs 150/60 kN);
   downgraded because reproducible ballistic profiles complete the recovery chain (deploy
   < ~2.5 km falling packed, or deploy already slow — all latched intact at 11-13 m/s), though
   they are un-signposted, contradict the part's own doc, and are knife-edge (1 200 m tore where
   2 500 m held at the same peak load).
2. **[MEDIUM, corrected from HIGH] Finding 2-replacement** — aero staleness is blind to mass
   changes (memo write-rule violation by the debit path): tables never rebuild during burns,
   `com_local` goes stale (0.39 m after one tank); the mass-inclusive table key is a landmine that
   turns into the report's per-quantum rebuild storm the moment the memo is fixed; standalone
   rebuild cost 7.6-66 ms confirmed; off-thread install path still unused.
3. **[MEDIUM] Finding 3** — re-entry heating cannot kill anything with shipped parts
   (peak 208-259 K vs 800-1 600 K on a 2.3 km/s steep entry); η hardcoded against its own
   balance.ron intent.
