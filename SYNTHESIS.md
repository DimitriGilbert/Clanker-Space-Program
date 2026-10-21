
Inputs: PLAN.md (R2), REVIEW.md (round 1, applied), REVIEW2-opus.md (57 findings incl. §J owner
calls), REVIEW2-normal.md (59 findings C1–L59). Output: PLAN.md (R3).

Both round-2 reviews exist and were read in full. They overlap heavily and **converge on every
critical finding**, which is the strongest signal in this exercise: where two independent adversarial
passes agree, the finding is real.

## 1. Settled user decisions — preserved, with the required records

| Decision | Status in R3 |
|---|---|
| Rust + wgpu + `bevy_ecs` standalone + egui hybrid shell | Unchanged |
| Desktop-first, wasm32 compiling in CI | Unchanged; §6.3 now *defines* the web tier as a fixed demo and §6.4 explains honestly what cutting it does and does not refund |
| Deterministic headless sim, integer 64 Hz tick, **no background thrust in v1** | Unchanged and reinforced; the no-thrust rule is now *load-bearing* for surface rails (§2.6.4) |
| Fictional hand-authored ~1/10 system; pluggable `OrbitModel` | Unchanged; §3.0 fills in the numbers |
| Real-time + timewarp RTS layer | Unchanged |
| Classical AI first + LLM later; kOS-style scripting first-class | Unchanged |
| Welded single rigid body + per-part analytic forces | Unchanged; docking now rigidizes into it |
| **TanStack Start** telemetry SPA | **Kept against opus #25.** One-line tradeoff recorded in §1 so no future reviewer relitigates; mitigated by exact version pins and by classifying the SPA as §6.0 schedule slack |
| **MCP: both transports** | Honoured as **stdio + Streamable HTTP**; opus #19 is right that HTTP+SSE is the deprecated 2024-11-05 transport |
| `ATTRIBUTION.md` CI lint | Kept, and now has a line format (`path \| source \| license \| url`) so it is implementable |

## 2. Accepted essentially verbatim

**opus:** 1 (interval-only integration), 2 (weld as pure fn, installed by command), 3 & normal C5
(aero tables never serialized), 4 & normal H22 (frozen upgrade levels per instance), 5 & normal C1
(docking rigidizes), 7 & normal H21 (dashboards are sidecar), 8 & normal M38 (reputation folded),
9 (proximity transfer survives the M6 cut), 10 (script wake events), 11 (`CommandKey`),
12 & normal C3 (warp as logged command), 13 (Clamping/Silent), 14 (event-count budget + partial
advance), 15 (rails presentation via `state_at`), 16 (SOI scan/bracket/Brent + stated miss bound),
17 (1 km integer lattice), 18 (wgpu 30 mesh-shader restatement + dated stamps), 19 (Streamable
HTTP), 20 (parry feature pinning + T1/T2 caveat), 21 (Rhai tick-entry-point), 22 (Rhai feature
parity + CI diff), 24 & normal H12 (own KDF), 26 (Stumpff series + Laguerre + Δt reduction),
28 (dormant hazard + `PartFailure` event), 29 (location-independent advancement), 30, 31 & normal
H23 (crew model), 32 (`ConfigMask`), 33 (voxel resolution cap), 34 & normal H14 (per-tick budgets),
35 & normal H9 (state-hash spec), 36 (`schema_version` + data-set hash definition), 37 (error
classes), 38 (ring buffer + bug bundle), 39 & normal H20 (WS protocol), 40 (test pyramid +
fingerprints), 41 (server concurrency + `csp-mcp` bin), 42 (wasm exclude + positive assertion),
43 & normal M50 (clippy.toml, reconciled rand rule), 44 & normal H26 (reference machines),
45 (unmeasurables → numbers), 46 (CI runtime budgets), 47 (M9a–d), 48 (§6.0 critical path),
49 (M4a/b/c + EU markers), 50 (cut-list reorder), 51 (capacity + quarterly trigger), 52–57 and
normal H30 (all of Appendix A).

**normal:** C2 (quantized inertia trigger — the sharpest single catch in either review), C4
(piecewise-linear colony flow network — §3.5.1 exists because of this), C6 (object/debris caps),
C7 (home system reference sheet + `SystemDef`/`BodyDef`), H8 (surface rails), H10 (frozen fixture
data for goldens), H11 (monotonic never-reused IDs + sorted iteration + free lists serialized),
H13 (pinned integrator + PGS contact + settle latch + the 20 t-lander criterion), H15 (isotropic
voxels, 66-direction geodesic, voxelize-once/sweep-rays, `Cm_α` exit criterion), H16 (two-stage weld
+ aero inheritance + wasm incremental), H17 (session-state category), H18 (control lease + "
determinism is a property of the log"), H19 (external command budgets), H24 (authored biome maps
decoupled from terrain noise), H25 (revert policy), H27 (pre-1.0 no-migration policy), H28
(commercial intent + per-crate licensing), H29 (`ColonyState`), M31–M37, M39–M42, M44–M49, L51–L59.

## 3. Adjudicated — where the reviews differed, or where I overrode both

| # | Conflict | Call | Why (one line) |
|---|---|---|---|
| 1 | **Interval vs tick advancement** (opus 1 says interval-only) | **Interval-only, with one `step_frame` code path for ×1 and ×10⁵** | Two integrators over one interval cannot agree, so warp choice would change outcomes — violating the plan's own headline rule; the shared code path makes the equivalence test meaningful rather than aspirational. |
| 2 | **Landed vessels & warp** (normal H8: "no warp while landed" breaks RTS play; opus didn't see it) | **Surface rails latch** (§2.6.4) | A settled vessel leaves the bubble into a body-fixed frame — so warp works on the pad, the no-thrust rule is untouched (latched throttle is already zero), and no integration means no drift. |
| 3 | **Saving with live contacts** (opus 6: refuse saves during persistent contact) | **Refuse only for live joints and *unsettled* contact** | Opus's blanket contact refusal would forbid saving while landed, which is most of the game; surface rails removes the contact state entirely, so option (a) becomes cheap instead of hostile. This is the one place I narrowed an opus recommendation. |
| 4 | **Save format** (opus 23: self-describing body + bincode replays; normal H27 only asked about the migration *policy*) | **Plaintext JSON header + DEFLATE(CBOR) body for saves; bincode for replays; plus normal's pre-1.0 no-migration policy** | Both are right about different halves: bincode genuinely cannot be migrated without frozen schemas, and writing migrations pre-1.0 for one user is waste. The decisive argument is that the state hash comes from the §7.1 visitor, not the serializer — so a self-describing save costs *nothing* in determinism. |
| 5 | **RNG derivation** (opus 24 + normal H12) | **Own BLAKE3 KDF to a full 32-byte key, `KDF_VERSION` in the header, never `seed_from_u64`** | normal caught the 64-of-256-bit loss, opus caught that `seed_from_u64` is an unversioned upstream implementation detail; both are fatal and the fix is the same. I also wrote down the reseed-per-key semantics opus asked for, because `U_k` depends on it. |
| 6 | **Rhai model** (opus 21) | **Tick-entry-point `on_tick`, op budget aborts the call, strikes, explicit `persist` map, `wait` as a wake-event host service** | **Verified independently against rhai.rs/book and docs.rs 2026-07:** exceeding `max_operations` *terminates* the script, `on_progress`'s `Some(token)` terminates immediately, and the official `pause_and_resume` example requires a dedicated thread + MPSC channel — banned in the sim. The plan's old "suspend and resume at the suspension point" had no implementation. |
| 7 | **M9 split** (opus 47) | **M9a Automation / M9b Reliability / M9c Interfaces / M9d Dashboards+Eval**, with `ControlApi` frozen at end of M9c | Also surfaces the ordering constraint the old plan hid: the API cannot be frozen before the autopilots and the MCP tool surface have exercised it. |
| 8 | **State hash** (opus 35 + normal H9) | **BLAKE3-256 over a hand-written `StateHasher` visitor**, canonical rules 1–8, NaN forbidden, `HASH_SCHEMA_VERSION` + per-golden fingerprint, bless workflow refuses silent changes | normal's "the team learns to bless blindly and the gate is void" is the real risk; opus's canonicalization details are the implementation. Combined. |
| 9 | **wasm CI gate** (opus 42, normal H30) | Exclusions **plus** a *positive* assertion that `csp-server`/`csp-bench`/`tokio`/`rayon` are absent from `csp-app`'s wasm tree | Exclusions alone would let the dependency sneak in transitively; the positive check is what actually enforces §2.1. |
| 10 | **Cut-list ordering** (opus 50) | **R3 → life support → third body → orbital docking (proximity transfer stays) → web polish** | Web's cost is paid up front and continuously and is non-refundable; ranking it first put the *product* on the chopping block while still paying the premium. |
| 11 | **Scaffold violations** (opus 52–57, normal H30) | **Appendix A, 13 numbered items, and M0 is explicitly incomplete until all are closed** | Keeps the plan clean and normative while recording the debt where an implementer will actually trip over it; the clock-in-ECS item is the most expensive line in the repo precisely because it is the first pattern anyone copies. |
| 12 | **Physics warp ceiling** (normal H14 says ×2–×4 is unsatisfiable; opus 34 asked for a numeric demotion rule) | **×2 is the advertised v1 cap; ×3/×4 unlock only when measured per-tick cost allows**, with the numeric rule and 1.5× hysteresis | normal's arithmetic is right; opus's rule is the mechanism. Advertising a fixed ×4 would repeat the ×8 dishonesty round 1 killed. |
| 13 | **Direction-set size** (opus 32 said "≥ a regular parameterization"; normal H15 said ≥66 geodesic) | **66-direction geodesic + barycentric over its triangulation, isotropic voxels, voxelize-once/sweep-rays** | normal's argument is stronger and specific: stability derivatives come from *differencing* the table, so 45° resolution makes SAS oscillate — a gameplay bug, not a fidelity nicety. |

## 4. Things I corrected that neither review caught

1. **§2.3's "SOI ≈ 8.4 Mm" is arithmetically impossible** alongside a moon orbiting at 12 Mm. The
   home SOI must exceed every child's apoapsis. Corrected to **84.0 Mm** in §3.0, and the
   `SystemDef` validator now asserts the invariant so it cannot recur.
2. **Lab rate units.** §3.2 specified science/**day** inside a sim that is otherwise strictly SI.
   Normalized to per-second (normal M36 flagged the nonlinearity but not the unit).
3. **`csp-sim`'s one-tick shadow copy.** Opus 37 demanded rollback-on-`Fatal` but did not note that
   it implies a per-tick structural clone; the cost is now stated and budgeted inside §5.3's
   per-tick numbers rather than appearing as a mystery regression later.
4. **The `Dormant` colony state doubles as event-budget relief.** normal H29 proposed it for
   gameplay reasons; it is also the mechanism that stops 20 half-dead colonies from drowning §2.6.5's
   2000-event budget. Written up as both.

## 5. Residual risk — flagged for the human

1. **Schedule, honestly: 51 EU ≈ 3 570 h ≈ 6.9 years at 10 h/week; minimum v1 ≈ 4.4 years.**
   I recorded 10 h/week as the capacity because the plan needed *a* number to make §9's dominant
   risk measurable. **If that figure is wrong, every conclusion in §6 moves.** This is the single
   thing I most want you to overwrite with a real number. The quarterly cut trigger (§6.5) is the
   only instrument in the document that actually resists scope creep; if you drop it, the cut list
   reverts to a wish list.
2. **TanStack Start remains a known-suboptimal fit.** I kept it per your decision and mitigated it
   (exact pins, schedule slack). The residual risk is real: a Node toolchain with ~daily patch
   cadence across ~20 interdependent packages, inside a Rust project, delivering no SSR value. If
   M9d ever starts eating critical-path time, swapping to client-only Vite + TanStack Router is a
   contained change — the wire protocol (§2.12.2) is framework-agnostic by design, specifically so
   this escape stays cheap.
3. **T2 (cross-platform determinism) is probably unreachable with parry/rapier**, and the plan now
   says so (§2.7 caveat). The nightly divergence report will show contact-scenario drift forever.
   That is fine as long as nobody mistakes it for a regression — which is exactly why the caveat is
   written down. Do not let anyone promote that report into a gate.
4. **§2.6.6's SOI guarantee is bounded, not absolute.** Encounters with SOI residence shorter than
   the scan stride `h` can be missed. This is inherent to any non-continuous collision scheme over
   a jump-advanced clock. It is now stated as the real guarantee rather than implied away, but
   players *will* eventually find a graze the solver missed.
5. **§3.5.1's colony model is specified but unvalidated.** It is the longest chain of new reasoning
   in R3 (two-state insolation, algebraic thermal steady state, fixed-priority allocation,
   min-over-resources event times). I believe it satisfies the "publish your next event time" rule,
   but it has never been implemented. Expect M8 to find at least one case where the piecewise-linear
   assumption leaks — most likely in coupled converter chains sharing two scarce inputs.
6. **Rhai's op-count budget is a coarse proxy for CPU.** Upstream says one "operation" roughly equals
   one expression node, and a single registered Rust host call also counts as one — so a script can
   burn real CPU inside a cheap-looking op count. The 20 000-op budget bounds *script* work, not
   *host* work; if a host service turns out to be expensive, the budget will not catch it. Worth a
   per-host-call cost table if M9a's profiling shows it matters.
7. **`HASH_SCHEMA_VERSION` discipline is a social control, not a technical one.** `xtask
   bless-goldens` can refuse obvious cases, but a determined implementer (including a future you at
   11 p.m.) can always bump the version and bless. The determinism gate's real enforcement is the
   §8.1 property-test layer underneath it — which is why promoting that layer out of "nice to have"
