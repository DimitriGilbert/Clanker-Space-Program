# Verified Report — review-report-01.md (Cluster C01: `csp-core`)

Verifier: read every cited line against current source; cross-checked downstream usage in
`csp-sim`, `csp-phys`, `csp-view`; checked workspace `Cargo.toml`, `rust-toolchain.toml`,
`clippy.toml`, and PLAN.md §7.1/§8.4 fingerprint provisions.

**Result: 3/3 findings CONFIRMED.** All claims are mechanically accurate. Two findings carry
minor framing corrections; Finding 2 is actually *stronger* than reported (a reachable
deserialization path was found that the report did not cite). Test-count cross-check: 35
`#[test]` attributes, 2 of which live in mutually exclusive `#[cfg(debug_assertions)]` /
`#[cfg(not(debug_assertions))]` pairs (hash.rs:562-572, 605-618) → 33 run in debug, matching
the report's "33 tests pass".

---

## Finding 1 — `SimTime`'s public `.0` field bypasses the checked-overflow contract

**Verdict: CONFIRMED** (with one framing correction)

**Verified evidence:**
- `crates/csp-core/src/time.rs:17` — `pub struct SimTime(pub u64)` exactly as cited.
- `time.rs:30-40` — `try_advance`/`checked` is the only guarded mutator; `checked_add` →
  `SimError::Fatal("sim tick counter overflow")`, clock untouched on failure. Correct.
- Doc claim "`try_advance` is the only mutator" — `time.rs:22` literally says "the only
  mutator, and it is fallible". The `pub` field contradicts that statement in fact, though
  not in intent.
- Overflow mechanics — `clock.0 += n` on the `pub u64`: workspace `Cargo.toml` sets no
  `overflow-checks` override (`[profile.release]` at Cargo.toml:105 is the only profile
  section), so debug builds panic on overflow and release wraps silently `u64::MAX → 0`.
  Both bullets are technically correct.
- Downstream construction — `SimTime(x)` sites confirmed at `csp-sim/src/orbit.rs:1629`,
  `csp-sim/src/lib.rs` (47 sites), `csp-sim/src/transfer.rs` (26), `csp-view/src/map.rs`
  (3), plus tests; the grep hit its 100-match cap, so "~100 downstream sites" is accurate.
- **No direct `.0` writes on `SimTime` downstream — verified independently:**
  `csp-view/src/lib.rs:255` `clock.0 = input.clock` writes `Clock.0`, where
  `Clock(pub SimTime)` (lib.rs:101) — it assigns a whole `SimTime`, not the inner `u64`.
  `csp-sim/src/colonies.rs:516` `row.0 = row.0.min(priority)` mutates a `(u8, f64)` tuple
  element, not a `SimTime`. `csp-sim/src/lib.rs:4272` is a read. Latent hole, not active
  bug — the report's own characterization.

**Correction to the report:** the claim that a debug panic "violates the crate's own
'sim never panics' rule (lib.rs:11)" misquotes the rule — `lib.rs:11` reads "The sim never
panics **in release**". A debug-build panic is actually the crate's *intended* trap
convention (cf. `hash.rs:201` "traps in debug builds (`debug_assert!`)", `hash.rs:277`
`debug_assert!(false, "{e}")`). The real defect is only the release path: silent wrap +
arbitrary non-monotonic rewind bypassing the §2.13 `Fatal` boundary.

**Determinism relevance:** real. A wrapped/rewound clock silently corrupts every
`Ord`-dependent structure (event heap, leases, epochs) and diverges any replay; the
contract wants `Fatal` + tick abort instead. Severity MEDIUM is fair given zero current
abuse; the suggested `from_ticks` constructor + private field is a mechanical fix.

---

## Finding 2 — `DQuat::normalize` and `from_axis_angle` produce silent NaN

**Verdict: CONFIRMED** (stronger than reported — an unguarded ingest path exists)

**Verified evidence:**
- `quat.rs:43-49` — `from_axis_angle` divides each component by `axis.norm()` with no
  zero/finite guard. Zero axis → NaN on all four components. The doc (lines 40-42) does
  warn "a zero axis yields NaN — callers validate geometry", as reported.
- `quat.rs:64-67` — `normalize` divides by `self.norm()` with no guard and, as claimed,
  the doc (61-63) does **not** warn about zero input.
- Current callers validate — confirmed: `csp-sim/src/maneuver.rs:239-257` `align_rotation`
  feeds `to.cross(ortho)` (guaranteed nonzero for unit `to` by construction) and
  `from.cross(to)` (nonzero because `cos` is strictly inside ±0.999999);
  `csp-sim/src/bubble.rs:161-165` `phase_quat` normalizes `body_spin` only when
  `omega > 0.0` (line 184); `csp-phys/src/session.rs:674-676` mirrors the same
  guarded convention.
- Huge-axis case — mechanically correct: `|axis| ≳ ~1.02e154` ⇒ `norm_squared() = +inf`
  ⇒ `x/inf = 0` ⇒ returns finite `(0,0,0,cos(θ/2))`. In `DQuat::rotate` (quat.rs:83-87)
  `qv = 0` ⇒ `t = 0` ⇒ `v' = v`, i.e. it acts as **identity regardless of angle** — a
  finite wrong result that evades the `hash.rs:297-300` non-finite gate. Confirmed.
- `hash.rs:297-305` citation — the non-finite rejection is at lines 297-300 (canonicalization
  at 301-303); citation off by a few lines, substance correct.
- Zero-quat constructible via `pub` fields / serde — `quat.rs:23-30` has `pub` fields and
  `Serialize, Deserialize` derives; `DQuat::new(0,0,0,0)` is legal.

**Stronger than reported — reachable deserialization path:** the report hypothesizes
"all-zero quat from a missing serde field" without citing a boundary. One exists:
snapshot/reweld validation at `csp-sim/src/lib.rs:8213-8220` checks `rot.is_finite()` but
**not** `rot.norm() > 0.0`, and `lib.rs:8237-8243` checks `pose.orientation.is_finite()`
but not its norm. A finite zero-norm quat in a snapshot passes both gates; at unlatch,
`surface_pose_restore` computes `(phase * pose.orientation).normalize()`
(`bubble.rs:244`) → NaN. Contrast `SetAttitude` (`lib.rs:4067`) and physical spawn
(`lib.rs:4531-4535`), which *do* check `rot.norm() > 0.0` before `normalize()` — the
convention exists but is not applied at the snapshot boundary. This converts Finding 2
from "callers validate today" into "one validation boundary is incomplete".

**Determinism relevance:** real but contained — any NaN produced is still caught at the
§7.1 `write_f64` gate as `Fatal`; the harm is lost provenance plus the huge-axis
finite-wrong case which is *not* caught. Suggestions (debug_assert + doc + optional
`try_normalize`, plus a `norm > 0` check in the snapshot gate) are appropriate. MEDIUM is
fair.

---

## Finding 3 — Bit-exactness of `DVec3`/`DQuat` arithmetic rests on unguaranteed FP codegen

**Verdict: CONFIRMED** (severity arguably LOW given the toolchain pin)

**Verified evidence:**
- `vec3.rs:39-41` — `dot` is `x*o.x + y*o.y + z*o.z`; `vec3.rs:44-50` — `cross` is
  `a*b - c*d` chains; `quat.rs:95-98` — Hamilton product is the 4-term `±` chains cited.
  All are the canonical `fmul`/`fadd` shapes contraction would alter.
- Technical claim is correct: rustc emits `fadd`/`fmul` without fast-math flags, so LLVM
  cannot legally contract to `fma` today; but Rust provides no language-level guarantee
  (the only guaranteed-fused op is `mul_add`, which is `libm`-adjacent and unused here).
  The report's "holds today, not a language guarantee" framing is accurate and fair.
- `clippy.toml` (csp-core) bans only `f64::*` libm-covered methods and
  `rand_core::SeedableRng::seed_from_u64` — confirmed, nothing pins codegen behavior.
- Module docs: `vec3.rs:8` says "exact IEEE `f64` arithmetic" — the no-contraction
  assumption is indeed implicit, not documented. Report's documentation suggestion valid.
- **Mitigations exist and are stronger than the report implies:**
  `rust-toolchain.toml` pins `channel = "1.97.1"`, and PLAN §8.4/§7.1 (PLAN.md:1591)
  requires a determinism fingerprint beside every golden recording rustc version, target
  triple, and dependency versions. So contraction could only arrive via a deliberate
  toolchain bump, which goldens would red on — "detection-without-recourse" is the right
  characterization, and the residual risk is genuinely small.
- **Minor overstatement:** "only tests these ops to `1e-15` tolerance" — `vec3.rs` tests
  `dot`, `cross`, and `norm` with `assert_eq!` (lines 134, 144-145, 151-153) and
  `quat.rs:115` uses `assert_eq!` on `rotate`. However, all exact-equality tests use
  benign inputs where contraction produces identical bits; no test pins `to_bits` on
  contraction-sensitive inputs, so the substantive point stands.

**Determinism relevance:** the property is real (cross-toolchain/backend bit-exactness is
assumed, not guaranteed) but nearly fully mitigated by the pinned toolchain + fingerprinted
goldens. The suggestion — document the reliance in module docs + add a few `to_bits`
golden tests for `dot`/`cross`/`mul`/`rotate`/`normalize` — is cheap and worth doing.

---

## Notes on the report's "checked and found clean" section

Spot-verified, all accurate: `write_f64` `-0.0 → +0.0` canonicalization and non-finite
rejection (hash.rs:297-303); length-prefixing on all variable-length writers
(hash.rs:347-349, `opt` 0/1 tag at 310-321, `u16` discriminant at 329-339); `arena`
sorted-dedup `windows(2)` check (hash.rs:407); `try_advance` semantics (time.rs:30-40).
The serde-transparent `SimTime` claim in Finding 1's suggestion is also correct —
`#[serde(transparent)]` (time.rs:16) works identically on a private field.

## Summary

| Finding | Verdict | Notes |
|---------|---------|-------|
| 1. `SimTime` pub field | CONFIRMED | Latent hole; debug-panic bullet misquotes lib.rs:11 (rule is release-scoped) |
| 2. `DQuat` silent NaN | CONFIRMED | Stronger than reported: snapshot gate (lib.rs:8213-8243) lacks `norm > 0` checks |
| 3. FP codegen assumption | CONFIRMED | Real but LOW in practice — rust-toolchain.toml pins 1.97.1 + §8.4 fingerprints |
