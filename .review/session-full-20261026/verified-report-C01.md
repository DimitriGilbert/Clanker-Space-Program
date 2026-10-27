# Verified report C01 — review-report-C01 (session-full-20261026)

Verifier: independent re-check of every finding against the working tree at HEAD 7dd4b86.
Method: full source reads (csp-core hash/terrain/rng/time, csp-sim lib walk sites, csp-view +
csp-render terrain, csp-data system/validate/load), workspace-wide rg, PLAN.md section reads,
and an out-of-tree empirical clippy probe (no project files modified).

Summary: 4 CONFIRMED, 1 DISMISSED. Two confirmed findings carry count/evidence refinements
(F1: 16+1 sites; F2: the omission list is 10 methods, not 9 — `f64::sin_cos` was missed).

---

### Finding 1: No typed DVec3/DQuat hash writers — 17 hand-rolled component walks — CONFIRMED

**Original summary**: csp-core owns `StateHasher` and `DVec3`/`DQuat` but offers no
`dvec3`/`dquat` writer, so every sim-layer walk expands vectors by hand; a dropped/duplicated
component compiles and hashes cleanly, silently excluding state from the determinism gate
(the C04 `part_ids` class). 17 component-run sites, 113 `h.f64(` writes in csp-sim/src/lib.rs.

**Verification**:
- `impl StateHasher` (crates/csp-core/src/hash.rs:240-478) has scalar/container writers only
  (u8..i64, bool, f64/try_f64, opt, stable_enum, bytes/str/vec/btree_map/arena). No dvec3/dquat.
- `h.f64(` count in crates/csp-sim/src/lib.rs: exactly **113** (confirmed).
- Site count, enumerated by hand from the source:
  - 16 vec3/quat walks: t.offset (10927-29), t.rotation (10930-33), p.com (10939-41),
    collider Points v (10952-54), Box half_extents (10959-61), force-site local_pos (10965-67)
    and local_dir (10968-70), dock-port local_pos (10977-79) and local_dir (10980-82),
    seam local_a (11746-48), local_b (11749-51), body pos/rot/vel/ang_vel (11851-63),
    pose.orientation (11873-76).
  - +1 tensor walk: p.inertia xx,yy,zz,xy,xz,yz (10942-47).
  - Total **17 "component-run sites"** — the report's count is defensible, but note the fix
    scope: `dvec3`/`dquat` writers collapse 16 of the 17; the inertia tensor needs its own
    writer (or stays hand-rolled).
- All 17 runs are complete and canonically ordered today — no live bug (confirmed by reading
  every run; cross-checked that every `.f64(….y)` line is the +1 neighbor of a `.x` line, so
  no orphaned/odd-length walk exists).
- csp-phys has **zero** `h.f64(` sites; no other csp-sim module (colonies, reliability,
  contracts, research, …) contains a vec3 walk (rg for the `x…y` run pattern: no hits).
- The "silent escape" framing is fair, with one nuance: editing an **existing** walk (dropping
  or duplicating a component) changes the digest and reds the golden replays — not silent.
  The genuinely silent case is **newly added state**: a new vector field with an incomplete
  hand expansion reblesses cleanly under its schema bump, exactly like C04's omitted
  `part_ids`. There is no forcing function (no derive, no coverage lint — verified; PLAN §7.1
  explicitly rejects derive-based hashing, so the hand walk per *struct* is sanctioned; a
  canonical writer per *primitive* is not provided, which contradicts hash.rs's own stated
  discipline at hash.rs:15-17: "one canonical writer per logical field").
- Phase1-4 pose fields crossing the seam (seams 11746+, surface-latch orientation 11873+,
  dock-port locals 10977+) — all confirmed present at the cited lines.

**Verdict**: CONFIRMED as MEDIUM. Maintainability/determinism-gate robustness finding, no live
bug, demonstrated failure class at the adjacent seam (C04).

---

### Finding 2: libm-only gate omits platform-backed f64 methods — CONFIRMED (list is 10, not 9)

**Original summary**: the four per-crate `clippy.toml` `disallowed-methods` lists stop at the
§8.2 enumeration; `f64::{exp2, exp_m1, ln_1p, sinh, cosh, tanh, asinh, acosh, atanh}` (and a
position on `mul_add`) are missing; none used today except one deliberate `mul_add` at
csp-sim/src/orbit.rs:524.

**Verification**:
- All four crates/csp-{core,sim,phys,data}/clippy.toml are byte-identical; workspace-root
  clippy.toml does not exist (confirmed — PLAN.md:113's tree shows one, but reality is 4 files).
- Banned list = sin, cos, tan, asin, acos, atan, atan2, exp, ln, log, log2, log10, powf, powi,
  sqrt, cbrt, hypot + seed_from_u64. (Minor report nit: the list is §8.2's enumeration *plus*
  log2/log10, not exactly "stops at the §8.2 enumeration" — immaterial.)
- Omitted platform-libm-backed methods: the nine listed **plus `f64::sin_cos`** (std delegates
  to `cmath::sincos`; the report missed it — the omission is larger than stated). `sin_cos`
  is used today only in crates/csp-view (map.rs, tests) — an ungated crate, so no live
  violation, but the gate hole is real for the four gated crates.
- Usage in the four gated crates: rg for exp2|exp_m1|ln_1p|sinh|cosh|tanh|asinh|acosh|atanh|
  sin_cos → zero hits; mul_add → exactly one, crates/csp-sim/src/orbit.rs:524
  (`(hi - lo) <= next.abs().mul_add(1e-15, f64::MIN_POSITIVE)`) — deliberate, and IEEE `fma`
  is correctly rounded, so the report's fma characterization is correct.
- Policy is exclusive: PLAN.md:68-69 "must use the `libm` crate **exclusively**, enforced by
  `clippy.toml` `disallowed-methods` (§8.2)". The gate under-implements the stated policy.
- Gate liveness re-verified empirically without touching the project: a /tmp two-crate
  workspace with `clippy.toml` (banning `f64::sin`) in crate a only, `x.sin()` in both crates,
  `cargo clippy -p a -p b` from the workspace root (clippy 1.97): warning fires in a only,
  none in b. Per-crate discovery from workspace root confirmed; the "applies to THIS crate
  only" comment is accurate. CI (.github/workflows/ci.yml) runs
  `cargo clippy --workspace --all-targets -- -D warnings`, promoting the warn-level lint to
  an error. No `allow(clippy::disallowed…)` suppressions anywhere in crates/.

**Verdict**: CONFIRMED as MEDIUM (latent gate gap). Refinement: extend the ban by ten methods,
not nine (`sin_cos` included).

---

### Finding 3: HeightField flat-stand-in ↔ real-bake key collision; "can never collide" comment is false — CONFIRMED

**Original summary**: real key `mix64(seed) ^ (w<<32|h)` (csp-core/terrain.rs:165) vs stand-in
key `mix64(!seed) ^ dims` (csp-view/terrain.rs:307-320) with a comment claiming collision
impossibility; the renderer skips GPU upload on key match (csp-render/terrain.rs:1233);
complement-seeded bodies collide.

**Verification**:
- Key derivations confirmed verbatim: crates/csp-core/src/terrain.rs:165
  `key: mix64(seed) ^ (u64::from(width) << 32 | u64::from(height))`;
  crates/csp-view/src/terrain.rs:320 `key: csp_core::terrain::mix64(!seed) ^ (u64::from(w) << 32 | u64::from(h))`.
  Both sides use the same dims (csp-view aliases `BAKE_WIDTH/BAKE_HEIGHT` as
  `HEIGHTFIELD_W/H`, terrain.rs:35), so the dims term cancels in any equality.
- `mix64` (terrain.rs:42-46) is the splitmix64 finalizer — a composition of invertible ops
  (xor-right-shift, odd multiply mod 2^64, xor-right-shift), hence bijective. Therefore
  `stand_in(s′).key == real(s).key ⟺ mix64(!s′) == mix64(s) ⟺ !s′ == s`.
  The comment's two facts (`mix64` bijective; `!seed != seed`) prove only *same-seed*
  non-collision; two bodies with complementary seeds collide exactly. The comment
  "a content key that can never collide with a real bake" (csp-view/terrain.rs:306-308) is
  mathematically false as a general claim. (Note `s == !s` is impossible for any u64, so the
  comment's own arithmetic is fine — its *scope* is wrong.)
- Load-bearing skip confirmed: crates/csp-render/src/terrain.rs:1232-1248 uploads the heightmap
  only when `self.uploaded_height_key != Some(hf.key)`; one `TerrainRenderer` instance lives
  for the whole app (crates/csp-app/src/lib.rs:813), so the uploaded key persists across body
  switches.
- Impact trace: with `s_B = !s_A`, `flat(B).key == real(A).key`. Visit A, leave before its
  bake finishes is not required — the simple order (A's flat, then A's real uploaded, then
  visit B) makes B's incoming `flat(B)` match the already-uploaded `real(A)` key → upload
  skipped → **B renders with A's real heightmap while B's bake pends**. A permanently-stuck
  flat render is reachable when A's real field is the next heightmap packet after an uploaded
  `flat(B)` (e.g. A's bake completes inline via the spawn-failure fallback,
  csp-view/terrain.rs:238-247, before any flat(A) frame uploads).
- Practical reachability today: seeds come from body.ron `noise_seed` — solen 1, terrin 601,
  rask 602, duva 603 (data/systems/home/*/body.ron) — no pair is complementary, so the
  collision is latent, exactly as the report states. Synthesized fallback bodies hash body
  names (`seed_of`), making an accidental complement astronomically unlikely.

**Verdict**: CONFIRMED as MEDIUM (latent, but the guard comment future devs will rely on is
false and the skip makes it render-visible if ever triggered).

---

### Finding 4: StreamTag::Generation (discriminant 3) is dead KDF surface — CONFIRMED

**Original summary**: `Generation` is declared and golden-pinned but has zero production draw
sites; dead discriminant in an append-only namespace; either remove or mark reserved.

**Verification**:
- crates/csp-core/src/rng.rs:32 `Generation = 3`; the variant's only references are its own
  goldens at rng.rs:220, 265, 311 — the exact lines the report cites.
- Workspace-wide `rg StreamTag`: consumers are csp-core itself (decl/re-export/doc mention)
  and exactly two sim files: csp-sim/src/reliability.rs (tags `Reliability` at 445, 532,
  1117, 1350; `Ignition` at 591) and csp-sim/src/contracts.rs (`Contracts` at 688). No
  `StreamTag::Generation` anywhere outside rng.rs.
- PLAN §2.7's fixed spelling — "contract generation is `{tag: CONTRACTS, entity: 0,
  counter: offer_index}`" — confirmed in the PLAN text; §2.7 names no `Generation` stream.
  contracts.rs:687-692 matches the spec verbatim (`{Contracts, 0, index}`).
- The enum doc (rng.rs:21-25) does say new variants require a `KDF_VERSION` bump and are
  append-only, so the variant both advertises a nonexistent stream and pins goldens implying
  it exists. Removal changes no preimage (no consumer draws tag 3), so it is currently free.

**Verdict**: CONFIRMED. Severity note: this is closer to LOW than MEDIUM (cleanliness +
future-variant numbering), but every factual claim in the finding holds.

---

### Finding 5: TerrainDef.heightmap validated-but-unconsumed, silently ignored — DISMISSED

**Original summary**: the data layer schemas and validates `heightmap` but `HeightField::bake`
derives everything from the noise seed; an authored heightmap would "validate green and then
be silently ignored"; "R2 shipped procedural-only and the decision was never recorded"; wire
or strip the field.

**Verification** (kernel facts true, framing false):
- Confirmed true: `pub heightmap: Option<String>` (crates/csp-data/src/system.rs:122);
  validate.rs:1361-1362 checks the ref (name-safety only — rejects `..`/absolute/separator
  refs, passes a legitimate name green); `HeightField::bake(seed, w, h)`
  (crates/csp-core/src/terrain.rs:146-167) is procedural-only; all four bodies author
  `heightmap: None`.
- Confirmed FALSE — "silently ignored": crates/csp-view/src/terrain.rs:98-110 logs
  `terrain: {body} authors a heightmap — R2 uses noise_seed instead` once per authored body
  at config build, with the doc comment "A `heightmap:` ref is the R4/native-tier hook — at
  R2 it logs once and falls back to the authored noise seed."
- Confirmed FALSE — "the decision was never recorded": docs/UI-AND-RENDERING.md:622-623
  records it explicitly: "A `heightmap: Some(_)` ref is the unimplemented R4/native hook."
  docs/DATA-AND-SCRIPTING.md likewise documents `terrain: {noise_seed, heightmap}` as schema.
- Context: `surface_resources` is documented the same way ("empty in v1 — schema-only") —
  schema'd-but-unconsumed hooks with documented fallbacks are an established, deliberate
  convention in this codebase, not an accidental inert knob.

**Verdict**: DISMISSED as filed. The MEDIUM severity rests on the two disproven premises
("silent", "unrecorded"); what remains — a documented, warned R4 hook whose validator checks
name safety rather than rejecting presence — is at most a nit (e.g. have the data validator
warn like the view layer does). Re-fileable as LOW if desired; not a §6.4-scoping violation.

---

## WIRED-claim spot-checks (four performed; all hold)

1. **§7.1 rules 1-8 vs PLAN §7.1** — CONFIRMED. hash.rs implements each rule exactly:
   version-first in `new()` (241-249, rule 1); LE fixed-width ints + bool-as-u8 (254-292,
   rule 2); `-0.0 → +0.0` before `to_bits` (325-333, rule 3); NaN/±inf → recorded fault,
   `debug_assert` in debug, `finish` → `Fatal` (303-323, 454-459, rule 4); arena sorts
   ascending by ID itself + duplicate-ID fault (424-447, rule 5); BTreeMap key order / Vec
   index order with u32 lens (388-411, rule 6); Option 0u8/1u8 tag + explicit stable u16
   discriminants via `StableDiscriminant` (337-367, rule 7); str u32-len + UTF-8 (375-384,
   rule 8). `finish` consumes `self`; tests pin the byte format against independently
   assembled streams. PLAN §7.1 text matches all eight rules, and explicitly rejects
   derive-based hashing — "no forcing function" is the sanctioned design, as the report says.
2. **SimTime no-underflow at 10^5 warp** — CONFIRMED. time.rs: sole mutator `try_advance`
   (42-52) is `checked_add` → `Fatal`, clock untouched on failure (test at 88-92); no `Sub`
   impl; u64 @ 64 Hz ≈ 9.2 Gyr of sim time. csp-sim side: `step_frame` breaks on
   `clock.ticks() >= effective_target` (lib.rs:8631); `AdvanceTo` grows the target via `.max`
   (lib.rs:5251); scheduling uses `saturating_add` (lib.rs:4892, 4975, 5104). No retreat path.
3. **KDF arity at draw sites** — CONFIRMED. Every draw constructs the full 3-field
   `StreamKey`: reliability.rs 445/532/591/1117/1350, contracts.rs 687-692 (exactly §2.7's
   `{Contracts, 0, offer_index}`). `MODE_COUNTER_BIT = 1 << 63` (reliability.rs:131) cannot
   overlap the small threshold/renewal/ignition counters. Preimage layout and 27-byte width
   pinned by `kdf_preimage_layout_is_the_literal_spec` (rng.rs:228-246).
4. **Per-crate clippy firing from workspace root** — CONFIRMED empirically (out-of-tree
   probe, clippy 1.97): gate fires in the crate owning the clippy.toml, not in its ungated
   workspace sibling, when invoked from the workspace root; CI promotes it via
   `cargo clippy --workspace --all-targets -- -D warnings` (.github/workflows/ci.yml).

## Side observations

- `StreamTag::Generation` zero production draws: CONFIRMED (see Finding 4).
- `TerrainDef.heightmap` validated-but-unconsumed: kernel confirmed, but it is a documented
  R4 hook with a runtime warn (see Finding 5) — not a defect as characterized.

## Corrections to the original report

1. Finding 1: the 17 "component-run sites" are 16 vec3/quat walks + 1 inertia-tensor walk;
   `dvec3`/`dquat` writers collapse 16 of them. Also, edits to *existing* walks are caught by
   goldens (hash changes); the silent escape is specifically *new* fields.
2. Finding 2: the omitted set is 10 platform-backed methods, not 9 — `f64::sin_cos` is also
   missing from the ban (used today only in ungated csp-view). The gate list is §8.2 plus
   log2/log10, not exactly the §8.2 enumeration.
3. Finding 5: dismissed — "silently ignored" and "decision never recorded" are contradicted by
   csp-view/src/terrain.rs:98-110 and docs/UI-AND-RENDERING.md:622-623.
