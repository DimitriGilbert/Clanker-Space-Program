# Verified Report 1 — csp-core determinism primitives

Verification agent: report-1 verifier | Date: 2026-10-25 | Verified against worktree at HEAD 1e8c6da (only .review/ files dirty; no concurrent source fixes — nothing qualifies for FIXED-BY-CONCURRENT-EDIT).

All cited line numbers in the original report were re-checked and match HEAD exactly.

### Finding 1: uniform() can draw 0.0; -ln U consumers store +inf; hash walk turns it into permanent Fatal — CONFIRMED
**Original**: `csp_core::uniform` includes exactly 0.0 (p = 2^-53/draw); reliability's three `-log(u)` sites are unguarded; `hash_record` feeds raw theta into `write_f64`, which faults Fatal on +inf, permanently breaking `state_hash()` for that world.
**Verification**:
- Primitive: rng.rs:93-96 `(rng.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)` — result is exactly 0.0 whenever the word's top 53 bits are zero (word < 2^11), p = 2^-53. The crate's own test at rng.rs:202 asserts `(0.0..1.0).contains(&a)` — the range includes 0.0; the API contract admits the draw.
- Consumers: reliability.rs:414 `let theta = -libm::log(u);` (libm::log(0.0) = -inf, so theta = +inf), and :1046, :1260 `rec.theta += -libm::log(u);` (finite + inf = inf). All three unguarded — no `MIN_POSITIVE`/clamp anywhere near them (grep for `MIN_POSITIVE|max(` in reliability.rs finds only unrelated sites). The in-code safety claim at reliability.rs:411-413 ("handled everywhere as 'no crossing inside any horizon'") is false for the hash path.
- Hash trap: reliability.rs:565 `h.f64(r.theta);` → hash.rs:318-321 `write_f64` returns `Err(SimError::Fatal("non-finite f64 in state hash"))` on any non-finite. The fault is sticky (hash.rs:468-470 `fault.get_or_insert`), so `finish()` at :447-451 returns Fatal for the whole world hash. `hash_vessel_reliability` (reliability.rs:593-600) runs `hash_record` on every record in the table — no skip.
- Production exposure: backend.rs:460-466 (save: snapshot then `state_hash()`, save refused on Err) and :489-491 (summary). Permanence: nothing ever clamps theta back to finite (renewals only add; physics guards at :523 `!rec.theta.is_finite() → None` and :1458 `hazard_integral >= +inf` is false correctly tolerate it), and the draw is a pure function of (save_seed, instance, counter), so reload/rollback reproduce it identically.
- Nuances vs the report: (a) in debug builds it is worse than reported — hash.rs:298 `debug_assert!(false, "{e}")` panics outright at the write site, before finish(); the "returns Fatal" behavior is release-only. (b) "unplayable" overstates slightly: the in-memory world keeps ticking (physics tolerates +inf); the accurate impact is permanently unsaveable/unsummarizable (and any §2.13 hash-gated boundary refuses). (c) Other uniform consumers are fine: :487 (weighted mode pick) and :546 (ignition `u >= p_fail`) never produce inf from u=0.
- Suggested fix verified sound: `u.max(f64::MIN_POSITIVE)` is identity for every u > 0 (and for u=0 gives ~745 as theta — finite, "never crosses any horizon" semantics preserved), so zero golden churn.

### Finding 2: no quaternion hemisphere canonicalization; q and -q hash differently — CONFIRMED (hardening gap, as the report itself labels it)
**Original**: DQuat has no canonical form; the vessel walk hashes raw rot components (lib.rs:10432-10435), so identical orientations reached through different sign algebra hash differently.
**Verification**:
- quat.rs: no canonicalization exists. Grep for canonical/hemisphere/`w < 0`/neg finds only `conjugate` (quat.rs:104-107), which negates the vector part only; there is not even a full-negation `Neg` impl on DQuat. `normalize` (:95-102) only rescales.
- Consumer: lib.rs:10432-10435 `h.f64(body.rot.x); ... h.f64(body.rot.w);` — raw components. `write_f64` canonicalizes only -0.0 (hash.rs:322-323), so every sign difference changes the digest: q vs -q hash differently, trivially.
- Algebra: from_axis_angle (quat.rs:62-72) computes `(axis·sin(θ/2), cos(θ/2))`; θ vs θ-2π flips the sign of both sin and cos — all four components negate. Reachable only through future refactors, as the report says.
- Nuance on the report's fallback evidence: the two anti-parallel fallbacks were verified to exist (maneuver.rs:262-274 picks `to.cross(least-aligned basis)`; session.rs:807-816 picks `orthogonal_axis(f)`), but they do not produce a clean q vs -q: distinct perpendicular axes that are not ± of each other give *different rotations* (both mapping f to -f — task underdetermination, broader than double-cover), and a half-turn quat has w = cos(π/2_f64) ≈ +6.1e-17 ≠ 0, so axis negation yields (-v, w), not exactly -q. This weakens that one piece of evidence but not the finding: the sign-sensitivity of the hash plus the absence of any canonical form is fully verified, and the report honestly scoped the impact as "not reachable in current code."
- Fix suggestion is reasonable and correctly costed (one HASH_SCHEMA_VERSION bump + rebless now vs later).

### Finding 3: clippy disallowed-methods list incomplete (no f32, several f64 missing), 4 byte-identical copies — CONFIRMED
**Original**: The ban list covers only 15 f64 methods; missing sin_cos/exp2/exp_m1/ln_1p/sinh-family and all f32 intrinsics; four identical clippy.toml copies with no sync check; zero current violations.
**Verification**:
- All four files byte-identical (md5 263869b4c8c7ec2eef4b0ea95017a327 for csp-core/csp-sim/csp-phys/csp-data clippy.toml). Each bans sin, cos, tan, asin, acos, atan, atan2, exp, ln, log, log2, log10, powf, powi, sqrt, cbrt, hypot — that is **17** f64 methods, not 15 as the report states (minor count slip; does not change the finding).
- Missing as claimed: `f64::{sin_cos, exp2, exp_m1, ln_1p, sinh, cosh, tanh, asinh, acosh, atanh}` — none appear in any list. Zero `f32::` entries in any file.
- Zero-violation claim verified: grep over csp-core/csp-sim/csp-phys/csp-data src for `.sin_cos(|.exp2(|.exp_m1(|.ln_1p(|.sinh(|.cosh(|.tanh(|.asinh(|.acosh(|.atanh(` and `f32::<intrinsic>` returns zero hits; the only f32 mention in the four crates is a doc comment (scripts.rs:132) noting Rhai's f32_float is banned.
- Additional hole found during verification (nuance, not scored): `f64::mul_add` is also unbanned and has one live use — csp-sim/src/orbit.rs:518 `next.abs().mul_add(1e-15, f64::MIN_POSITIVE)`. IEEE-correctly-rounded fma is bit-deterministic across platforms, so this use is defensible, but given the crate's explicit no-FMA-contraction stance (quat.rs:13-22) the list should either ban it or document the exemption.

### Finding 4: the [0,1) scaling expression duplicated in contracts::next01 — CONFIRMED
**Original**: `contracts.rs:231-233 next01` is character-for-character the body of `rng::uniform` minus the KDF; determinism-critical constants (shift 11, 53-bit, 2^-53) pinned in csp-core only.
**Verification**: contracts.rs:232 `(rng.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)` is identical to rng.rs:95 modulo surrounding code. Repo-wide grep finds exactly two code copies (rng.rs:95, contracts.rs:232; rng.rs:293 is a doc comment). The next01 doc comment itself says "the `csp_core::uniform` scaling applied to a stream position" — the borrowing is acknowledged, which makes the drift risk real (a change to `uniform`'s scaling won't propagate here; only goldens would catch it, and only on exercised paths). `pick`/weighted-choice consumers tolerate 0.0 — no active bug, LOW is the right severity.

### Finding 5: HASH_SCHEMA_VERSION changelog documents v13 before v12 — CONFIRMED
**Original**: The version-history ledger lists Version 13 (line 108) above Version 12 (line 116), the only inversion; constant = 20 is correct.
**Verification**: hash.rs:108 "**Version 13** (M7-C)" (economy) precedes hash.rs:116 "**Version 12** (M7-B)" (tech record); every other entry ascends (v2 ... v11, then v13, v12, v14 ... v20). `pub const HASH_SCHEMA_VERSION: u32 = 20;` at hash.rs:200. Documentation-only; suggestion (swap with the next bump) is sound.

---

## Clean-claim spot-check (report Verdict line 1): hash.rs implements PLAN §7.1 rules 1-8 — CONFIRMED ACCURATE
Verified each rule against PLAN.md §7.1 text:
- Rule 1: `StateHasher::new()` writes `HASH_SCHEMA_VERSION` u32 first (hash.rs:234-241).
- Rule 2: all scalar writers LE fixed-width via to_le_bytes; `bool` as `u8` (:270-272).
- Rule 3: `write_f64` canonicalizes -0.0 → +0.0 then `to_bits()` LE (:322-324).
- Rule 4: non-finite → Fatal fault, sticky, surfaced by `finish()` (:318-321, :444-451).
- Rule 5: `arena` sorts by ID itself (wrong order impossible to write), duplicate-ID fault, occupied-count u32 then `id: u64 || payload` (:379-411).
- Rule 6: `vec`/`btree_map` write u32 counts; BTreeMap iteration is key order (:415-442).
- Rule 7: `opt` writes 0u8 | 1u8||payload (:330-336); enums via stable declared u16 discriminants (`StableDiscriminant`, :204-217, :351-357).
- Rule 8: `str`/`bytes` write u32 byte length then UTF-8 bytes (:363-375).
No deviation found.

---

## Summary

5 confirmed, 0 dismissed, 0 fixed-by-concurrent-edit.

Finding 1 is the actionable one: one-line clamp at reliability.rs:414/:1046/:1260, zero golden churn, and it falsifies an in-code safety comment (reliability.rs:411-413) — that comment should be rewritten when the clamp lands. Findings 2-5 are correctly scoped hardening/maintainability gaps with accurate impact statements; Finding 3's "15 methods" is actually 17, and the list also misses mul_add (one live, defensible use at orbit.rs:518).
