# Deep Review — Cluster C01: `csp-core` (determinism root)

**Files reviewed (read in full):**
- `crates/csp-core/src/lib.rs` (25 lines)
- `crates/csp-core/src/error.rs` (55 lines)
- `crates/csp-core/src/hash.rs` (760 lines)
- `crates/csp-core/src/quat.rs` (149 lines)
- `crates/csp-core/src/rng.rs` (319 lines)
- `crates/csp-core/src/time.rs` (81 lines)
- `crates/csp-core/src/vec3.rs` (162 lines)

**Cross-checked against:** `PLAN.md` §7.1 rules 1–8, §2.7 KDF spec, §2.13 error taxonomy; workspace `Cargo.toml` (exact pins `blake3 =1.8.3`, `rand_chacha =0.10.0`, `rand_core =0.10.1`, `libm =0.2.16`); `rand_core-0.10.1`/`rand_chacha-0.10.0` registry sources (`BlockRng::next_u64_from_u32` word order); `crates/csp-core/clippy.toml` disallowed-methods list. `cargo test -p csp-core`: 33 tests pass.

## Verdict summary

The core is in good shape. The canonical byte stream matches §7.1 rules 1–8 exactly (verified rule-by-rule against the spec and the byte-level tests). The KDF preimage is fixed-width and unambiguous (`u8 | u64 | u16 | u64 | u64`, 27 bytes — verified against §2.7 and the frozen goldens). No hash-walk ambiguity, no preimage collision surface, no endian/width dependence (everything `to_le_bytes`, fixed widths), no off-by-one in `arena`/`vec`/`btree_map`/`opt` walks, and `StreamTag` discriminants are explicit + pinned. Findings below are real but medium-severity: an unenforced `SimTime` invariant, silent NaN paths in `DQuat`, and a toolchain-level float determinism caveat.

---

### [MEDIUM] Finding 1: `SimTime`'s public `.0` field bypasses the checked-overflow contract
**File:** `crates/csp-core/src/time.rs:17` (`pub struct SimTime(pub u64)`), contract at `time.rs:22-29`

**Problem:** The docs state `try_advance` is "the only mutator" and that overflow is an invariant violation surfaced as `SimError::Fatal` (§2.13). But the field is `pub`, so any code can write `clock.0 += n` or `clock.0 = x`, which:
- in debug builds, **panics** on `u64` overflow (`+=` uses `checked` semantics in debug) — violating the crate's own "sim never panics" rule (`lib.rs:11`);
- in release builds, **silently wraps** `u64::MAX → 0`, making sim time jump backwards — event scheduling, leases, and every `Ord`-dependent structure downstream then misbehave silently rather than surfacing `Fatal`.

The checked API is advisory, not enforced; the invariant the type exists to protect is one stray `+=` away from silent corruption.

**Evidence:** `time.rs:17` (`pub u64`), `time.rs:30-40` (the only guarded path). Construction (`SimTime(x)`) is used at ~100 downstream sites (e.g., `csp-sim/src/orbit.rs:1629`), so `pub` currently earns its keep — but nothing distinguishes *construction* from *unchecked mutation*. Direct field writes on `SimTime` were not found downstream today (only construction and `.0` reads in `csp-view`), so this is a latent hole, not an active bug.

**Impact:** If any future mutation bypasses `try_advance`, tick overflow wraps silently in release instead of aborting the tick per §2.13; in debug it panics, contradicting the documented no-panic discipline (tests aside). Also allows arbitrary non-monotonic rewinds.

**Suggestion:** Make the field private and add `pub const fn from_ticks(u64) -> Self` (plus keep `ticks()`); serde's `transparent` derive works fine on a private field. Migration cost is mechanical (`SimTime(x)` → `SimTime::from_ticks(x)`). Alternatively, at minimum document on the field that mutation outside `try_advance` is forbidden.

---

### [MEDIUM] Finding 2: `DQuat::normalize` and `from_axis_angle` produce silent NaN instead of a `SimError` boundary
**File:** `crates/csp-core/src/quat.rs:43-49` (`from_axis_angle`), `quat.rs:64-67` (`normalize`)

**Problem:** Both divide by a norm with no zero/finite guard:
- `from_axis_angle`: `axis.norm() == 0` → all components NaN. (Documented: "callers validate" — and current callers do, e.g. `csp-sim/src/bubble.rs:161-165` `phase_quat`, `csp-sim/src/maneuver.rs:239-257` `align_rotation`.) A sharper unhandled edge: `axis` with huge components (|axis| ≳ 1e154) gives `norm_squared() = +inf` → `ax = x/inf = 0` → returns finite `(0,0,0,cos(θ/2))` — a **finite but wrong rotation** that the §7.1 NaN gate cannot catch because nothing non-finite survives.
- `normalize`: dividing by `norm() == 0` (e.g. `DQuat::new(0,0,0,0)` — constructible via `pub` fields or deserialization) → NaN quat. Unlike `from_axis_angle`, `normalize`'s doc does **not** warn about the zero-input hazard, yet it is called in hot composition chains (`bubble.rs:206,244`: `(phase.conjugate() * body.rot).normalize()`).

The failure mode is deferred: a NaN pose only surfaces at the §7.1 hash boundary as a generic `Fatal("non-finite f64 in state hash")`, losing all provenance about which subsystem injected it.

**Evidence:** `quat.rs:44-48`, `quat.rs:65-66`. Contrast with the crate's own §2.13 design (`hash.rs:297-305`) where non-finite values are meant to be caught — these helpers can *manufacture* non-finite state from finite inputs.

**Impact:** Corrupt data (all-zero quat from a missing serde field, arithmetic cancellation, or a bug upstream) converts to NaN silently; a huge-axis input converts to a finite wrong rotation that no gate catches. Diagnosis degrades to a generic rollback.

**Suggestion:** Add `debug_assert!(n.is_finite() && n > 0.0)` in `normalize` and `from_axis_angle` (matching the crate's trap-in-debug convention), and document the zero-input contract on `normalize`. Optionally offer `try_normalize() -> Option<DQuat>` for boundaries that ingest untrusted state.

---

### [MEDIUM] Finding 3: Bit-exactness of `DVec3`/`DQuat` arithmetic rests on unguaranteed rustc FP-codegen behavior
**File:** `crates/csp-core/src/vec3.rs:39-50`, `quat.rs:52-101`

**Problem:** `dot`, `cross`, `DQuat::mul` (the 4-term `a*b ± c*d ± e*f` chains), `rotate`, and `normalize` assume LLVM will neither (a) contract `mul+add` into `fma` (different rounding, different bits) nor (b) reassociate the addition chains. rustc today emits plain `fmul`/`fadd` without fast-math flags, so this holds — but it is **not a language guarantee**, and it is exactly the shape of expression that changes under contraction (relevant for alternative backends/targets, e.g. aarch64 FMA or a future cranelift path). Every downstream vessel pose, weld frame, and orbit computation inherits the assumption. Note the determinism fingerprint (PLAN §7.1) pins rustc version and goldens would red on a change — so the risk is detection-without-recourse, not silent acceptance.

**Evidence:** `vec3.rs:40` (`x*x + y*y + z*z`), `quat.rs:95-98` (Hamilton product). The libm rule (`clippy.toml` disallowed-methods) removes the *library* source of platform drift, but nothing pins the *codegen* source for basic `+`/`*`/`/` expressions.

**Impact:** A toolchain/backend change could alter every bit of every quaternion/vector result; goldens catch it, but there is no code-level lever to fix it short of rewriting the expressions (e.g. explicit intermediate bindings don't prevent contraction either — only per-op intrinsics or a documented accepted-risk do).

**Suggestion:** Low-cost mitigation: document the reliance in the `vec3`/`quat` module docs (the assumption is currently implicit), and add bit-pinned (`to_bits`) golden tests for `dot`, `cross`, `DQuat::mul`, `rotate`, `normalize` on a few fixed inputs — the suite currently pins the RNG and hasher byte formats but only tests these ops to `1e-15` tolerance, which would not detect a 1-ulp contraction change on ops never exercised exactly.

---

## Explicitly checked and found clean

- **Hash-walk unambiguity:** every variable-length writer (`bytes`, `str`, `vec`, `btree_map`, `arena`) prefixes a `u32` count; `opt` uses `0u8|1u8`; sum types use `u16` — fixed walks make positions unambiguous. `str`/`bytes` wire-identity is documented (`hash.rs:343-356`).
- **`f64` canonical encoding:** `to_bits()` LE after `-0.0 → +0.0` (`hash.rs:301-303`); NaN/±inf rejected with fault-recording + debug trap, matching rule 4. Subnormals preserved. No NaN-payload question since NaN is forbidden outright.
- **`arena` walk:** sorts ascending by `u64` ID itself (rule 5 order cannot be written wrong), `windows(2)` adjacent-dup check on the sorted vec is a correct and complete duplicate detector; ID-prefixed entries, occupied count only. No off-by-one.
- **`write_len` overflow path:** records `Fatal`, writes `u32::MAX`, still writes elements — byte position stays defined and `finish()` always surfaces the fault, so the stream is never consumed (`hash.rs:437-445`).
- **KDF preimage:** fixed-width 27-byte layout `u8|u64|u16|u64|u64` — no field-boundary ambiguity possible; `KDF_VERSION` first byte; plain BLAKE3 finalize (32-byte XOF prefix = `blake3::hash`) matches §2.7 (`rng.rs:75-83`). Tag width (`u16`) is the rule-7 encoding — noted and consistent.
- **`uniform`:** `next_u64 >> 11` × 2⁻⁵³ — the standard top-53-bit draw, `[0,1)`, deterministic; rand_core 0.10 `BlockRng::next_u64_from_u32` word order verified against registry source; exact-bits golden test pins it.
- **`StreamTag`/`StableDiscriminant`:** explicit `#[repr(u16)]` discriminants (`0..=3`), `self as u16`, pinned by `stream_tag_discriminants_are_explicit_and_pinned`; trait impl delegates to the inherent method — no dual-source drift.
- **Endianness/width:** every scalar path uses `to_le_bytes`; `usize` only enters via `write_len`'s `u32::try_from`. BLAKE3 output is byte-identical across SIMD/portable paths.
- **`SimTime::try_advance`:** `checked_add`, leaves clock untouched on failure — correct (`time.rs:30-40`). `seconds()` is a derived `f64`, never stored — consistent with the integer-clock rule.
- **`DQuat` math:** Hamilton product coefficients verified term-by-term (`quat.rs:95-98`); `rotate` is the standard `v + w·t + qᵥ×t, t=2(qᵥ×v)` formula — correct for unit quats as documented.
- **Dependencies:** exact pins (`=`) for `blake3`, `rand_chacha`, `rand_core`, `libm` in workspace `Cargo.toml`; `Cargo.lock` confirms `csp-core` resolves to `rand_chacha 0.10.0`/`rand_core 0.10.1` (the 0.9.x entries in the lock belong to other crates — no ambiguity).
- **`bubble_mask`:** pure `BTreeSet` membership filter composed with `arena`'s own sort — cannot disturb rule-5 ordering (`hash.rs:470-480`).
- **`error.rs`/`lib.rs`:** clean; `#![forbid(unsafe_code)]`; error payloads are `&'static str`, never hashed.
