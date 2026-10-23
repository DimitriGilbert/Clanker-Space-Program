# Verified Report — Cluster C05: `orbit.rs` / `transfer.rs` / `maneuver.rs`

Verification of `.review/session-deep1/review-report-05.md` against source in
`crates/csp-sim/src/{orbit,transfer,maneuver}.rs` and call sites in
`crates/csp-sim/src/lib.rs`. Methods: line-by-line code trace plus one live
reproduction (temporary integration test, since removed).

**Scorecard: 6/6 findings verified — 4 fully CONFIRMED, 2 CONFIRMED with
corrections/reachability caveats.** No finding is fabricated. Two line-number
citations in the report are slightly off (noted inline).

---

## [MEDIUM] Finding 1 — `predict_encounter` chaining drops post-rebase `suppress` → wrong-side `Err`

**Verdict: CONFIRMED — byte-exact live reproduction.**

Mechanism, verified end-to-end:

- `predict_encounter_excluding` hard-codes `suppress: None` in its
  `next_soi_event_within` call — `transfer.rs:1162-1170` (`None` literal at
  line 1168).
- The suppression machinery exists exactly as described:
  `next_soi_event_within(…, suppress, exclude)` at `orbit.rs:1135-1181`
  forwards a per-candidate `suppress_dir`; `scan_candidate` drops a
  first-stride detection matching that direction at `orbit.rs:1335`.
- The chaining recipe is documented at `transfer.rs:1111-1114`
  ("Feed it back into [`predict_encounter`] at `now = event.tick` to chain
  multi-encounter predictions") — the report cites lines 1131-1133; the
  correct citation is **1111-1114**.
- The Brent contract is a distance bound, not a side claim — `orbit.rs:1016-1018`,
  `brent_root` returns `b.ceil()` (`orbit.rs:1452`), so an early-side tick is
  contractual.
- The degenerate `rebase` errors exist verbatim: Enter →
  `orbit.rs:935-937` ("soi enter: target is not a child of the vessel's
  parent"), Exit → `orbit.rs:960-962` ("soi exit: event body is not the
  vessel's parent"), and `predict_encounter_excluding` propagates the error
  except for the parentless-Exit case (`transfer.rs:1179-1192` — the
  `parentless_exit` guard checks `direction == Exit`, so the Enter artifact
  always propagates).
- `exclude` cannot cover the Enter artifact: the skip requires
  `signed_distance < 0.0` at `now` (`orbit.rs:1156-1159`); the wrong-side
  Enter conic starts outside (`d > 0`). Also the artifact fires on the
  *parent* boundary candidate — `exclude` semantics don't reach it either
  way. Confirmed.
- The live sim does pass the just-fired `(body, direction)`:
  `lib.rs:6571-6576` (post-`SoiTransition` rescan) →
  `schedule_next_soi(…, suppress)` at `lib.rs:3988-3994`. Confirmed.

**Live reproduction** (temporary test `crates/csp-sim/tests/verify_c05.rs`,
run and then deleted): the suppression test's own fixture — vessel parented
to `m` at `r = 2.44e6` just outside `r_soi = 2.43e6`, `v = (−80, 150, 0)`
inbound — passed to the public `predict_encounter` returned, verbatim:

```
Err(Recoverable("soi enter: target is not a child of the vessel's parent"))
```

matching the report's claimed output exactly. The artifact is real, the
detection is first-stride (proven by `first_stride_suppression_drops_wrong_
side_redetection`, `orbit.rs:2337-2408`), and the public API gives the caller
no way to suppress it.

**Impact caveat (verification addition):** all in-tree callers of
`predict_encounter` outside `transfer.rs` are inside `lib.rs`'s `mod tests`
(`lib.rs:13542, 13589, 13761`, all after `#[cfg(test)] mod tests` at
`lib.rs:9109-9113`). No production caller exercises the documented chaining
recipe yet — the bug is latent in the public API contract, exactly as the
finding's "documented composition" framing implies. MEDIUM is fair.

---

## [MEDIUM] Finding 2 — `departure_node` can return `tick ≤ clock` → silent clamp → wrong-longitude burn

**Verdict: CONFIRMED.**

- `burn_s = dep_s + dm/n_v − period_s` at `transfer.rs:1359-1363` places the
  burn up to one parking period before `departure_tick`; the only refusal is
  `burn_s < 0`/non-finite (`transfer.rs:1364-1366`, "ejection: burn precedes
  epoch"). The function has **no `now`/clock parameter** — signature at
  `transfer.rs:1296-1300`. Confirmed.
- `apply_set_maneuver` clamps silently: `let at = tick.max(self.clock.ticks()
  .saturating_add(1))` at `lib.rs:5088` (report cited ~5077 — actual is
  **5088**). The stored `ManeuverNode.tick = at` (`lib.rs:5089-5095`).
- The components are resolved at the *clamped* tick, not the designed one:
  the apply-time feasibility check calls `state_at(orbit, SimTime(at))` and
  `node_dv_world(r, v, …)` at `lib.rs:5100-5106`, and — decisively — the
  executor re-resolves the stored components against the state at
  `event.tick` in `fire_maneuver_start`, `lib.rs:5211-5219`. A clamped node
  burns the designed component vector against a different orbital longitude:
  same |Δv|, wrong inertial direction — "garbage ejection" is an accurate
  description of the delivered burn.
- The `departure_node` doc (`transfer.rs:1280-1295`) lists the
  instantaneous-egress and up-to-one-period-early limits and the required
  correction loop, but never the check-`node.tick`-against-clock trap.
  Confirmed; the `DepartureNode.tick` doc (`transfer.rs:1242-1245`) claims
  only "at-or-before the plan's departure tick".

**Impact caveat (verification addition):** `departure_node` has no non-test
callers in-tree (only test code references it, `lib.rs:13364-13365` is a doc
mention inside `mod tests`). The trap is in the query's contract, reachable
the moment a planner/AI feature consumes it — which the doc presents as its
intended use ("ready to hand to `SimCommand::SetManeuver` verbatim",
`transfer.rs:1237-1239`). MEDIUM is fair.

---

## [LOW] Finding 3 — degenerate-frame checks test exact `h ≤ 0` while docs claim `r×v ≈ 0`

**Verdict: CONFIRMED — with a correction: only one of the two docstrings
says "≈".**

- `burn_frame` (`maneuver.rs:146-157`): gate is `hn <= 0.0` plus
  `rn <= 0.0 || vn <= 0.0` and non-finite checks (line 150); doc says
  "`r × v ≈ 0`" (line 145). Code tests exact zero, doc says approximate —
  the mismatch the finding claims holds here.
- `cartesian_to_elements` (`orbit.rs:670-675`): gate is `h <= 0.0`
  (line 672) — but the docstring at `orbit.rs:653-654` says "`h = 0`",
  i.e. **exact**, matching the code. The report's claim that "both
  docstrings describe the rejection domain as `r × v ≈ 0`" is half-wrong;
  there is no doc dishonesty on the `cartesian_to_elements` side.
- The underlying mechanism is real in both places: a near-radial state
  (tiny positive `h`) passes both gates → `burn_frame` returns a normal
  dominated by rounding error; `cartesian_to_elements` produces
  degenerate-but-finite elements (including the `a := −p` parabolic
  encoding with `p ≈ 0`) instead of `Recoverable`. Confirmed.

So: real near-rectilinear gap, real doc/code mismatch in `burn_frame` only;
the suggestion (relative gate in both places, or tighten `maneuver.rs`'s
doc) stands. LOW is fair.

---

## [LOW] Finding 4 — `brent_root` machine term degrades the unconditional "≤ 1 tick" bound

**Verdict: CONFIRMED.**

- `orbit.rs:1402`: `tol1 = 2.0 * f64::EPSILON * b.abs() + 0.5`, exit when
  `|xm| = |c−b|/2 ≤ tol1` (lines 1403-1405) → bracket width bound
  `2·tol1 = 1 + 4·eps·|b|` raw ticks.
- Arithmetic checks: at `|b| = 4e14`, `4·2.22e-16·4e14 ≈ 0.36` → ~1.36
  ticks; at `|b| = 1e16`, `≈ 8.9` → ~10 ticks. Matches the report.
- Docs are unconditional: module doc `orbit.rs:64-66` ("Brent-refines each
  bracket … to `≤ 1 tick`"), `SoiEvent` doc `orbit.rs:161-162` ("within 1
  tick of the refined crossing"), `brent_root` doc `orbit.rs:1362-1364`.
- Timescale claim: 4e14 ticks at 64 Hz ≈ 6.25e12 s ≈ 198k sim-years —
  "practically unreachable" is accurate. LOW is fair.

---

## [LOW] Finding 5 — `lambert_multi` swallows `lambert_velocities` failures that `lambert` propagates

**Verdict: CONFIRMED.**

- `lambert`: `let sol = lambert_velocities(&g, mu, z, 0)?;` — propagates
  `Recoverable` (`transfer.rs:526`).
- `lambert_multi`: `if let Ok(sol) = lambert_velocities(...) && lands_on(...)`
  — a solved `z` that fails velocity recovery (`"lambert: degenerate g
  coefficient"`, `"lambert: non-finite velocities"`,
  `"lambert: infeasible solution point"` — `transfer.rs:476-493`) is
  silently dropped from the result set (`transfer.rs:561-567`).
- No wrong answer is returned, but the zero-rev and multi-rev paths treat
  the identical solver defect inconsistently, and a silent empty `Vec` is
  indistinguishable from "branch had no roots". Accurate as stated. LOW is
  fair.

---

## [LOW] Finding 6 — NaN `soi_m` silently produces "no events" / burns the sample budget

**Verdict: CONFIRMED — with a reachability caveat.**

- `stride_ticks` (`orbit.rs:772-774`): `(0.1·NaN/v·HZ).ceil().max(1.0)` —
  `f64::max` discards NaN → stride 1. Confirmed.
- `signed_distance` (`orbit.rs:1263-1278`): `sep − NaN = NaN`; every sign
  comparison in `scan_candidate` (`orbit.rs:1311-1323`) is false → "no
  crossing" on a 1-tick grid → `Ok(None)` on short horizons, full-budget
  burn → `Recoverable` on long ones. Confirmed.
- `soi_candidates` admits any `Some(r_soi)` including NaN
  (`orbit.rs:1190-1211`); `candidate_v_rel_max` never inspects `r_soi`.
  Confirmed.
- The loud-NaN precedent is real: `nan_angle_orbit_state_fails_next_soi_event`
  (`orbit.rs:2265-2288`) asserts NaN orientation angles → `Recoverable`.

**Caveat (verification addition):** the normal data path is already guarded
— `csp-data`'s validator rejects non-finite `soi_m`
(`crates/csp-data/src/validate.rs:1018-1019` via `check_positive`,
lines 812-817, `NonFinite` error). The silent path is reachable only through
`PatchedConics::new` with hand-built `BodyOrbit`s or a corrupt record that
bypassed validation — defense-in-depth, not a live data bug. LOW (arguably
lower) is fair.

---

## Spot-check of the report's "checked and clear" claims

- **Epoch anchoring:** `tick_delta_seconds` differences the integer lattice
  in `i128` (`orbit.rs:764-766`); `elements_state_at` re-derives from the
  fixed epoch (`orbit.rs:612-637`); rebase stamps `epoch = t`
  (`orbit.rs:953-957, 984-989`). No per-tick re-stamp exists. Holds.
- **SOI stale-clock:** `schedule_next_soi` clamps `now ≥ clock`
  (`lib.rs:3971`); rescan origin `max(tick, clock)` (`lib.rs:6573`); the
  open-left `(now, horizon]` contract at `orbit.rs:1327-1329`. Holds.
- **Executor bookkeeping:** `fire_maneuver_start` resolves components at
  `event.tick` via `node_dv_world` (`lib.rs:5211-5219`); `SetManeuver`
  clamps at `lib.rs:5088` as documented in `maneuver.rs:68-70`. Holds (and
  is precisely what powers Finding 2).
- **Determinism:** libm-only transcendentals in the reviewed span, fixed
  iteration caps (`BRENT_MAX_ITERS`, `SOLVER_MAX_ITERS`, `EJECTION_MAX_ITERS`),
  `BTreeMap` body order. Holds.

## Corrections to the report's citations

| Report cites | Actual |
|---|---|
| `transfer.rs:1131-1133` (chaining doc) | `transfer.rs:1111-1114` |
| `lib.rs:~5077` (SetManeuver clamp) | `lib.rs:5088` |
| "both docstrings" claim `r×v ≈ 0` (F3) | only `maneuver.rs:145`; `orbit.rs:654` says exact `h = 0` |

## Verification actions performed

- Read `orbit.rs` (lines 1-330, 600-999, 1000-1454, 2240-2409), `transfer.rs`
  (470-629, 1100-1399, 1800-1959), `maneuver.rs` (full), `lib.rs` (3940-4009,
  5029-5233, 6520-6679), `csp-data/src/validate.rs` (soi_m check).
- Wrote and ran temporary integration test `crates/csp-sim/tests/verify_c05.rs`
  reproducing Finding 1's claimed error byte-exactly (`cargo test -p csp-sim
  --test verify_c05` — passed); **file deleted afterward**, no source changes
  remain.
