# Review C01 — Determinism primitives (csp-core) — session-full-20261026

Scope: `crates/csp-core/src/{lib,error,hash,quat,rng,terrain,time,vec3}.rs` (1,940 LOC read in full)
+ determinism-lint wiring (`crates/{csp-core,csp-sim,csp-phys,csp-data}/clippy.toml`; the map's
"workspace-root clippy.toml" does not exist — the gates are four per-crate files). Verified against
PLAN §7.1, §2.2/§2.3, §2.7, §8.2, §10, §6.4. HEAD 7dd4b86 (past 6cc2cb4); csp-core tree clean.

## Findings

### [MEDIUM] Finding 1: DVec3/DQuat have no typed hash writer — 17 hand-rolled component walks downstream are the §7.1 silent-escape hatch
**File**: crates/csp-core/src/hash.rs:240 (API surface) vs crates/csp-sim/src/lib.rs:10927–10933, 10952, 10959, 10965, 10968, 10977, 10980, 11746, 11749, 11851–11861, 11873 — **Problem**: csp-core owns both `StateHasher` and `DVec3`/`DQuat`, yet offers no `dvec3`/`dquat` method, so every sim-layer walk expands vectors by hand (`h.f64(v.x); h.f64(v.y); h.f64(v.z);`). **Evidence**: 17 component-run sites in csp-sim/src/lib.rs alone (113 total `h.f64(` writes). I scanned every run in csp-sim/csp-phys: all are complete and x,y,z(,w)-ordered today — no live bug. **Impact**: a dropped or duplicated component compiles, hashes cleanly, and permanently excludes a state component from the determinism gate — exactly the failure class C04 just found at the sim layer (`part_ids`). New Phase1–4 pose fields (seams, surface-latch orientation, dock-port locals) all crossed this seam. **Suggestion**: add `pub fn dvec3(&mut self, v: DVec3)` / `pub fn dquat(&mut self, q: DQuat)` to `StateHasher` (fixed x,y,z(,w) order, `try_f64` semantics) and collapse the 17 sites. One canonical writer per primitive is the module's own stated discipline ("one canonical writer per logical field", hash.rs:16) — the crate doesn't apply it to its own types.

### [MEDIUM] Finding 2: libm-only gate is not exhaustive — `f64::{exp2, exp_m1, ln_1p, sinh, cosh, tanh, asinh, acosh, atanh}` (and a position on `mul_add`) are missing from `disallowed-methods`
**File**: crates/csp-core/clippy.toml:3–13 (identical path lists verified in csp-sim/csp-phys/csp-data) — **Problem**: PLAN §1.1/§2.7 state the policy as libm *exclusively*; the banned list stops at the §8.2 enumeration. The omitted methods are all platform-libm-backed and — unlike `fma` — **not** correctly-rounded-guaranteed, so they are exactly the hazard the gate exists for. **Evidence**: `rg` for every omitted method in the four gated crates returns nothing today (one deliberate `mul_add` at crates/csp-sim/src/orbit.rs:524 — value-exact per IEEE, not itself a hazard, but also ungated). I empirically verified the gate is live (probe `x.sin()` in csp-core fired from a workspace-root `cargo clippy`, clippy 1.97, no `allow(disallowed…)` suppressions anywhere in crates/). **Impact**: a dev adds `f64::tanh` to a thermal or aero path in csp-sim/csp-phys and no gate fires; T1 holds only until the binary is rebuilt on a platform whose libm differs. **Suggestion**: extend all four clippy.tomls with the nine omitted methods; either ban `f64::mul_add` or document the exactly-rounded exemption next to the vec3/quat no-FMA notes.

### [MEDIUM] Finding 3: `HeightField` content key allows a flat-stand-in ↔ real-bake collision; the "can never collide" comment is mathematically false
**File**: crates/csp-core/src/terrain.rs:165 (`key: mix64(seed) ^ (w<<32|h)`) with crates/csp-view/src/terrain.rs:307–320 (stand-in key `mix64(!seed) ^ dims`, comment: "a content key that can never collide with a real bake (`mix64` is bijective and `!seed != seed`)") — **Problem**: bijection prevents a body colliding with *itself*; it does not prevent cross-family collision: flat(seed s′) == real(seed s) iff `!s′ == s`. **Evidence**: the key is load-bearing — crates/csp-render/src/terrain.rs:1233 skips the heightmap GPU upload when `uploaded_height_key == Some(hf.key)`. **Impact**: two bodies whose seeds are bitwise complements, visited in one session (stand-in shown while the second bake is pending): the real field arrives with the stand-in's key, the upload is skipped, and the body renders with the wrong heightmap. Authored seeds today (1, 601, 602, 603) can't trigger it — latent, but the guard comment future devs will rely on is wrong. **Suggestion**: fold a stand-in discriminator into the key (e.g. `mix64(!seed) ^ dims ^ 1`, or a dedicated `STAND_IN_KEY_TAG`) and fix the comment to claim only what holds.

### [MEDIUM] Finding 4: `StreamTag::Generation` (discriminant 3) is dead KDF surface — declared, golden-pinned, zero production draw sites
**File**: crates/csp-core/src/rng.rs:33 — **Problem**: workspace-wide `rg -ln StreamTag` shows exactly two consumers, crates/csp-sim/src/{reliability.rs, contracts.rs}; neither uses `Generation` (contract generation uses `Contracts`, per PLAN §2.7's fixed spelling). The variant's only appearances are its own golden tests (rng.rs:220, 265, 311). **Impact**: dead discriminant in an append-only, never-renumber namespace whose additions nominally require a `KDF_VERSION` bump; it advertises a stream that doesn't exist and pins goldens implying it does. **Suggestion**: remove it now while removal is free (no preimage consumer exists), or leave a comment stating it is reserved-but-unused so the next variant lands at 4 knowingly.

### [MEDIUM] Finding 5: `TerrainDef.heightmap` is schema'd and validated but never consumed by the height function — inert authored knob
**File**: crates/csp-core/src/terrain.rs:110 (`height_normalized` is procedural-only) vs crates/csp-data/src/system.rs:122 (`pub heightmap: Option<String>`, "§3.0 schema: noise seed + authored heightmap") and crates/csp-data/src/validate.rs:1361 (the ref *is* validated) — **Problem**: the data layer promises authored heightmaps and checks their paths, but `HeightField::bake(seed, w, h)` derives everything from the noise seed; all four bodies author `heightmap: None`. **Impact**: an authored heightmap would validate green and then be silently ignored — terrain would render procedural noise. PLAN §10 says "Terrain source: … decide at R2"; R2 shipped procedural-only and the decision was never recorded, so the knob is neither wired nor cut. **Suggestion**: wire the ref into `bake` (seed → authored-map load-or-noise), or strip the schema field until the §10 decision is recorded — not a §6.4-sanctioned deferral as written.

## Assessed and clean (verified, no flag)

- **§7.1 visitor conformance**: rules 1–8 implemented exactly (schema version first; LE fixed-width; `-0.0→+0.0` before `to_bits`; NaN/±inf → recorded fault surfaced by `finish` as `Fatal` — the §2.13 rollback channel; length-prefixed containers; ascending-ID arenas with duplicate-ID fault; unambiguity tests pin the byte format against independent encodings). `finish` consumes `self`, no Clone/reset — no hasher state-reuse hazard.
- **Visitor coverage mechanism** (the map's question): nothing forces a new struct field into the hash — no derive, no coverage lint in xtask, no field-mutation test. That is the PRD-sanctioned design (PLAN §7.1 explicitly rejects derive-based hashing); the hand-written walk *is* the schema. Field-level audit of what Phase1–4 added is C04's (their `part_ids` find).
- **KDF**: preimage is the literal §2.7 spec (`u8 || u64 || u16 || u64 || u64`, 27 bytes, plain BLAKE3), provenance-pinned goldens (keys, 256 B ChaCha8 stream, exact `uniform` bits), `seed_from_u64` double-banned (clippy.toml:12 + xtask/src/lint_deps.rs:857). Every draw site in the workspace uses full 3-field arity; reliability's mode stream is domain-separated by `MODE_COUNTER_BIT` (1<<63, reliability.rs:131) — no collision with renewal/ignition counters; contracts use `{Contracts, 0, offer_index}` verbatim (contracts.rs:687).
- **SimTime at extreme warp**: sole mutator `try_advance` is `checked_add` → `Fatal`, clock untouched on failure; u64 @ 64 Hz ≈ 9.2 Gyr so overflow is unreachable at 10⁵ × years; no Sub impl exists, and every `ticks() ± n` site found is additive; `step_frame` breaks on `clock >= target` and uses `saturating_add` — no retreat/underflow path. `seconds()` loses exactness only past 2⁵³ ticks (~4.5 Myr) — derived readout, not state.
- **Terrain render/physics seam**: genuinely one source — csp-phys (simple.rs:342) and csp-view (terrain.rs:231+) bake `HeightField` from the same `BAKE_WIDTH/BAKE_HEIGHT/AMPLITUDE_FRAC` consts, WGSL `height_at` (terrain.wgsl:48–73) mirrors `sample_dir` structurally (texel-centre coords, wrap-u/clamp-v, identical bilinear form); the interpolated surface is continuous across texel-index flips, so asin/atan2 f32-vs-f64 drift cannot open a seam. Biomes decoupled per §3.0.1.
- **DVec3/DQuat**: libm-only, component ops exact IEEE, no-FMA reliance documented and canaried by `to_bits` tests; `from_axis_angle`/`normalize` debug-trap with documented release behavior.
- **Lint wiring**: per-crate clippy.toml discovery empirically verified live from workspace root (probe reverted); configs identical across the four PLAN §8.2 crates; CI's `--workspace --all-targets -D warnings` promotes warn-level `disallowed_methods`. §8.2's "applied to csp-core/csp-sim/csp-phys/csp-data only" is honored (csp-script ungated is PLAN-scoped).
- §6.4 cut list contains nothing in C01 scope.

## PRD coverage table

| PRD item (§) | Status | Note |
|---|---|---|
| §7.1 StateHasher visitor + rules 1–8 | **WIRED** | exact conformance; exclusion list is sim-side (C04) |
| §2.2/§2.3 SimTime (integer clock, 64 Hz) | **WIRED** | checked overflow, no underflow paths |
| §2.7 RNG KDF (ChaCha8/BLAKE3, arity, goldens) | **WIRED** | one dead tag variant (Finding 4) |
| §2.1/§2.7 math types, libm-only | **WIRED** | gate list incomplete (Finding 2) |
| §3.0 terrain height fn (render+physics shared) | **WIRED** procedural; **UNWIRED** authored-heightmap knob (Finding 5) |
| §8.2 determinism lints | **WIRED** | empirically verified active in CI shape |

## Top 3 issues

1. Finding 1 — no typed `dvec3`/`dquat` hash writers; 17 hand-rolled walks are the demonstrated silent-escape seam (MEDIUM, cheap fix, eliminates a whole bug class).
2. Finding 3 — `HeightField` flat/real key collision is possible and the guard comment falsely claims otherwise; upload-skip makes it render-visible if triggered (MEDIUM, latent).
3. Finding 2 — libm-only enforcement omits nine platform-varying transcendentals; the gate can be bypassed without any CI signal (MEDIUM, latent).

Counts: 0 CRITICAL, 0 HIGH, 5 MEDIUM.
