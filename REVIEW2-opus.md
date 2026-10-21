# REVIEW2-opus.md — Second adversarial pass on PLAN.md

Scope: PLAN.md as revised after REVIEW.md, plus the `crates/` scaffold, `Cargo.lock`,
and `.github/workflows/ci.yml`. Findings from REVIEW.md that are genuinely resolved are
not repeated. Where a claim was checkable, I checked it against upstream sources
(wgpu v30 changelog, MCP spec, Rhai book, parry/rapier feature tables, bincode docs,
bevy_ecs 0.20, TanStack Start 1.0, gpuweb implementation status).

Severity: **critical** = plan would mislead implementation / **high** = will cause rework /
**medium** = ambiguity / **low** = polish.

Count: 41 findings. The five that matter most: **1, 2, 6, 14, 33**.

---

## A. Internal contradictions between sections

### 1. Rails warp and real-time produce different world state for the same elapsed sim time — this violates §2.6's own headline rule. — **critical**
§2.6 declares "trajectories must not depend on the player's warp choices" the single most
important rule. But §2.6 then specifies that colonies, research conversion, and reliability
hazard are *integrated analytically* during rails advance, while during ×1/physics warp they
are (implicitly) accumulated per tick. Two integrators over the same interval do not agree
bit-for-bit, and for anything nonlinear (lab throughput `min(1, data/throughput)`, drill rate
vs stored power, hazard with stress multipliers) they do not agree at all. Consequence: warp
choice changes outcomes, the save→load→N-ticks hash test (§7) passes only if the player never
warps, and the "colony century" golden replay encodes a specific warp pattern as ground truth.
**Fix (decision):** there is exactly **one** integrator per subsystem and it is the
interval-based closed-form one. Even at ×1, colonies/research/reliability advance on
`advance_to(next_event_or_now)` semantics — never per-tick accumulation. Only the physics
bubble is tick-stepped. Write this as §2.6's corollary: *"subsystems are advanced by interval,
not by tick; the tick loop drives only the bubble."* Then add a test: run 10 sim-days at ×1 and
at ×10⁵ from the same save; the state hashes must be identical except for bubble contents.

### 2. "Weld is off the sim thread" vs "weld output is authoritative sim state and must be bit-identical" vs "csp-sim may not depend on a threadpool". — **critical**
§5.3 budgets weld/voxelization at "<500 ms, **off the sim thread**". §7 requires re-welding on
load to produce **bit-identical** mass properties. §2.1 bans rayon/threadpools in `csp-sim`,
`csp-phys`, `csp-data`. These three cannot all hold as written without saying who owns the weld
and when its result becomes visible to the sim.
**Fix:** weld is a *pure function* `weld(part_graph, effective_parts) -> WeldedBody` living in
`csp-phys` (no threads, no HashMap). `csp-app` may call it on a worker thread, but the result
enters the sim as a `SimCommand::InstallWeld { graph_hash, body }` stamped at the tick it is
*consumed*, and the sim refuses to launch/stage until the weld for the current graph hash is
installed. That keeps the latency off the tick and the data path deterministic. State explicitly
that a weld computed on a worker is byte-compared against a sim-thread recompute in debug builds.

### 3. §2.4.1 says the voxelization result "is serialized with the craft"; §7 says aero voxel tables are **never** serialized and are re-derived on load. — **critical**
Direct contradiction, and it decides whether craft files are portable across data-set changes.
**Fix:** pick re-derivation (consistent with §7's whole argument) and delete the serialization
sentence; keep only the *in-memory* cache keyed by part-graph hash. If you want ship-time load
speed, add an explicit, deletable, hash-keyed side cache file that is never authoritative and
never enters the state hash. Say which.

### 4. Tech upgrades "never apply to craft already in flight" (§3.3) is unimplementable given §7's "re-derive the welded body from the part graph on load". — **critical**
On load you re-run `EffectivePart = def ∘ tech_upgrades(researched_tracks) ∘ …`. With current
researched tracks, an in-flight craft silently gains upgrades after a reload — exactly the
"unexplainable saves" §3.3 is trying to prevent.
**Fix:** the resolved upgrade level set is **frozen into the vessel at build time** and
serialized per vessel (`built_with: BTreeMap<FamilyId, TrackLevels>`), plus the data-set hash.
Re-derivation on load uses the frozen set, not the live tech state. Add this to §7's "what is
serialized" list and to §3.3 as the mechanism, not just the intent.

### 5. 16-joint-per-bubble cap (§2.4) vs "1000-part station in orbit" budget (§5.3) and M6 "station assembly". — **critical**
A station assembled from docked modules exceeds 16 joints quickly, and the plan never says what
docking does to the body graph.
**Fix (decision):** docking **rigidizes**. On latch, the two `WeldedBody`s are recompiled into a
single `WeldedBody` with a recorded seam (`DockSeam { port_a, port_b, latch_tick }`); the
constraint solver is used only during the *approach/soft-capture* window (a few seconds) and
then the island collapses to one body. Undock splits the graph using the §2.4 breakup path. The
16-joint cap then applies only to live hinges/pistons/soft-captures, not to station size. Write
this into §2.4 and M6's exit criteria.

### 6. Rapier-for-jointed-islands + "never serialize physics internals" = the save-determinism test cannot pass whenever anything is jointed. — **critical**
§2.9 confines Rapier to jointed islands; §7 forbids serializing solver caches, warm-start
impulses, and contact manifolds; §7 also demands save→load→10k ticks hash-equals uninterrupted
10k ticks. Rapier's sequential-impulse solver is warm-started: discarding impulses changes the
next step. So the test is guaranteed to fail for any save taken during soft-capture, with
landing gear in contact, or with a robotic arm loaded.
**Fix:** state the exception explicitly. Either (a) saving is **refused** while a jointed island
or persistent contact set is active (KSP-style "cannot save while in contact"), or (b) the
determinism test's scope is narrowed to free-flight saves and the plan says so. Recommended: (a)
for v1 plus "settle to rest or free flight before save" UI rule; it is one check and removes a
whole class of unfixable test failures. Also define the hand-off semantics when a body
transitions `SimpleBackend ↔ RapierBackend` (dock/undock, touchdown/liftoff) — that transition
is currently unspecified and is a state discontinuity.

### 7. Dashboards stored "as JSON in the save" contradicts §2.2 (nothing mutates sim state except via `SimCommand`) and §7 (save is the authoritative deterministic snapshot). — **high**
A tablet resizing a widget would mutate the save and, if the dashboard section is in the state
hash, change replay ground truth. If it isn't in the hash, then §7's "the save is the snapshot"
is no longer true without qualification.
**Fix:** dashboards live in a **sidecar** file (`<save>.dashboards.json`) or in global app
config, versioned independently, explicitly excluded from the state hash, and explicitly not a
`SimCommand` path. Add a "non-authoritative sidecar data" subsection to §7 listing it alongside
settings and keybinds.

### 8. Reputation is both a shipped mechanic (§3.6 prerequisites and rewards) and an open question (§10). — **medium**
**Fix:** make the call now. Fold it in: reputation is a derived scalar from contract
success/failure history, used only for offer-board gating; no UI currency, no spending. Delete
the §10 bullet.

### 9. Cut list ordering breaks M8. — **high**
§6 cut list: "M6 docking → post-v1". M8 ships "supply missions" and colony resupply, and §3.5
says logistics is "you fly it". Without docking or a landed-cargo transfer mechanism, M8's
supply loop has no mechanism, and the `Composite` import-only pull (§3.5) has no delivery path.
**Fix:** either (a) move "surface/landed resource transfer within N m without joints" out of M6
into M8 as a separate, cheap feature (no constraint solver, proximity + confirm), or (b) state
that cutting M6 also cuts M8 resupply and reduces colonies to "launch-site-only ISRU". (a) is
the right call; make the cut list say "M6 docking (orbital) → post-v1; proximity transfer stays".

### 10. `csp-script` runs scripts "in `SimStep`" (§2.10) while rails warp has no `SimStep`. — **high**
Under rails (×10…×10⁵) no ticks execute, so scripts are frozen for up to 1.1 sim-days per wall
second — including scripts whose entire job is "wait until Ap, then burn". Whether warp is
refused while a script is loaded is never stated, and contradicts the "automation is first-class"
pillar if the answer is "scripts just stop".
**Fix (decision):** scripts may register **wake conditions** that are published as
`ScheduledEvent`s (`ScriptWake { script_id, tick }`, and sim-state predicates compiled to an
event time where closed-form — otherwise refused). Rails warp clamps on `ScriptWake`. A script
holding no wake condition and no throttle does not block warp; a script that *polls* is detected
(N consecutive budget-exhausting ticks) and demotes warp with a visible warning. Also state: an
active script with nonzero throttle already blocks warp under §2.6's no-background-thrust rule.

### 11. §2.12 "every tool/command maps to `SimCommand`s stamped at the tick they're consumed" has no answer for commands that arrive for an already-executed tick, and no intra-tick ordering. — **critical**
Replay requires a **total order** over commands within a tick. Four producers (UI, Rhai scripts,
Rust autopilots, MCP/WS) with an in-process queue and a network path cannot be ordered by
arrival time without breaking determinism (and arrival time is wall clock, which is banned in
sim).
**Fix:** define `CommandKey = (tick, source_class, source_id, source_seq)` with a fixed
`source_class` order (e.g. Script < Autopilot < UI < External), a per-source monotonic `seq`,
and the rule "commands are stamped at the tick they are *dequeued*, never at the tick they were
produced". Late/stale stamps are impossible by construction. The command log stores `CommandKey`
and is the replay artifact. Write this as §2.2.1.

### 12. Warp level changes are not stated to be `SimCommand`s, yet they determine how the clock advances and are produced by a wall-clock-driven controller. — **high**
§2.6's warp controller "measures and demotes" based on frame budget — a wall-clock input that
changes the sim's evolution (how far rails jumps). A replay that doesn't record the resulting
warp decisions diverges on a slower machine.
**Fix:** `SimCommand::SetWarp(level)` and `SimCommand::AdvanceTo(target_tick)` are explicit,
logged commands; the measuring/demoting controller lives entirely in `csp-app` and only ever
emits commands. The replay harness replays the commands, never re-derives them. One sentence in
§2.6, but it is load-bearing for every golden replay.

---

## B. Timewarp and event-queue semantics (the real warp limiter)

### 13. No classification of events into "clamps warp" vs "fires silently". — **high**
§2.6 says "warp is clamped by pending events" and §2.6.1 says every thresholded subsystem must
publish a next-event time. Taken together, with 20 colonies and 500 on-rails objects, warp will
be perpetually clamped by `ColonyStoreFull`, `ReliabilityInspectionDue`, and every SOI event of
every uninteresting debris object. The feature becomes unusable at exactly the scale it exists
for.
**Fix:** two event classes. `Clamping` (focused vessel's SOI/maneuver, player alarms, contract
deadlines, research completion, life-support depletion) demotes warp; `Silent` (any other
vessel's SOI, store full/empty, inspection due) is processed inside `advance_to` without
touching warp. Per-object opt-in ("track this vessel") promotes Silent→Clamping. Put the class
in the `ScheduledEvent` enum so it's not a policy sprinkled across subsystems.

### 14. The ≤4 ms rails-advance budget is asserted with no event-density model, and the event count per frame is unbounded. — **high**
At ×10⁵, one frame = ~18 sim-minutes. Any subsystem with a sub-18-minute characteristic period
(crew rotation, drill cycle, inspection interval, store fill with a small buffer) generates ≥1
event per frame per object. 20 colonies × a few converters is already hundreds of
heap operations and closed-form integrations per frame, and `advance_to` is a serial loop.
**Fix:** state the budget as *events processed per frame* (e.g. ≤2000) and add the back-pressure
rule: if `advance_to` exceeds its budget, it returns early at the tick reached and the frame
reports partial progress — warp becomes self-limiting rather than frame-stalling. Also require
periodic subsystems to publish *coarse* events (next store-full, not next cycle). Add one golden
scenario at the pathological density, not just "colony century".

### 15. Presentation under rails warp is unspecified. — **medium**
§2.5 says `Presentation` interpolates between tick N-1 and N and "at high warp interpolation is
disabled (positions snap)". Under rails there are no ticks at all, so there is nothing to
interpolate between, and the map view — the main thing you look at during warp — would snap by
18 sim-minutes per frame.
**Fix:** map-view presentation evaluates `OrbitModel::state_at(presentation_tick)` directly, at
a presentation time that may be *between* the pre- and post-advance clock. Say that `state_at`
must be cheap and pure (it is, for conics) and that the presentation clock is a separate,
non-authoritative value.

### 16. SOI root-find "to within 1 tick" is stated but the solver, bracketing, and failure mode are not. — **medium**
`|r_vessel - r_body| - r_soi` is not monotonic; a grazing pass can enter and exit within one
interval, and bisection on an unbracketed interval misses it. "Solved to 1 tick" is also a
*precision* claim that needs a measurement definition.
**Fix:** specify: coarse scan at a fixed stride derived from the minimum encounter geometry
(e.g. stride such that relative motion < 0.1·r_soi), sign-change bracketing, then Brent to 1
tick; document the missed-encounter bound as a function of stride and state it as the real
guarantee. Add a test with a deliberate graze.

### 17. Bubble recentering at 1 km has no lattice rule, so it is a state-dependent rounding event. — **medium**
"Snapped to the active vessel's position whenever it exceeds 1 km" means the new origin is an
arbitrary f64 triple, and every recentre re-rounds every position in the bubble. Over a long
ascent that is hundreds of recentres of accumulated, unmodelled error — and the threshold
crossing itself is float-compare-sensitive.
**Fix:** origins snap to a **1 km integer lattice** (`origin = floor(pos/1000)*1000` as i64
kilometres), so the shift is exactly representable and the transform is exact for the mantissa
range in play. Threshold test on the integer lattice cell, not on a float distance.

---

## C. Claims that are wrong, stale, or unverifiable (checked upstream)

### 18. Mesh-shader status is stale relative to the pinned wgpu. — **medium**
§5.1 and REVIEW.md #16 say "Metal/DX12 via passthrough shaders as of wgpu 28". The workspace
pins **wgpu 30.0.1** (`Cargo.lock`), and wgpu v30 release notes record *full* Metal mesh-shader
support including WGSL, and HLSL-writer support completing DX12 — i.e. no longer
passthrough-only. Still `Features::EXPERIMENTAL_MESH_SHADER`, still documented as
subject to breaking change.
**Fix:** restate as: "experimental in wgpu 30; native paths on Vulkan/Metal/DX12, WGSL
supported; API explicitly unstable." Keep it R4/Advanced-only. Add a dated version stamp to every
upstream-status claim in §5 so staleness is visible (`as of wgpu 30.0, 2026-07`).

### 19. MCP transport choice is stale — "HTTP+SSE" is the deprecated 2024-11-05 transport. — **high**
§2.12's table says MCP over "stdio **and** HTTP+SSE". The spec replaced HTTP+SSE with
**Streamable HTTP** in 2025-03-26, and the 2026-07-28 revision further removed the GET stream
endpoint and protocol-level sessions. Building HTTP+SSE now means building a deprecated
transport that modern clients won't speak.
**Fix:** "stdio + **Streamable HTTP** (single POST endpoint, per-request SSE upgrade)". Also
record the **protocol revision you target** and the `MCP-Protocol-Version` header handling;
add the spec-mandated `Origin` validation and localhost-binding requirement (DNS-rebinding
protection) to §2.12's rules — currently only a token is mentioned.

### 20. `parry3d-f64` exists and is fine, but the plan under-states the dependency-churn and determinism-feature coupling. — **medium**
Verified: `parry3d-f64` 0.31.1, Apache-2.0, actively maintained — but it moved 0.23→0.31 in
roughly a year, including a `glamx` math-backend swap. It also has an `enhanced-determinism`
feature that pulls `indexmap` + `simba/libm_force` — i.e. upstream agrees that its *default*
build has iteration-order and transcendental hazards. §2.7's clippy `disallowed_methods` lint
only covers *your* code, not parry's or nalgebra's `sin`/`cos`.
**Fix:** state the feature set explicitly now (`parry3d-f64` with `enhanced-determinism`,
without `parallel`/`simd8`, `default-features = false`), pin exact versions in
`[workspace.dependencies]`, and amend §2.7 T1 to admit that third-party transcendentals are a
*same-binary* guarantee only. Note the opportunity: `parry3d_f64::transformation::voxelization`
already gives you a voxelizer for §2.4.1 — but check it for `HashMap`/`HashablePartialEq` use
before letting its output feed sim state.

### 21. Rhai cannot suspend and resume a script — §2.10's "exceeding the op budget suspends the script until next tick and the suspension point is part of deterministic state" is not implementable. — **critical**
Verified against the Rhai book: the budget mechanism is `Engine::set_max_operations` /
`Engine::on_progress`; exceeding the limit **terminates the script with an error**, and
`on_progress` returning `Some(token)` terminates immediately. There is no continuation,
coroutine, or resumable-VM API, so "suspend and resume at the suspension point" has no
implementation. This is the single most load-bearing wrong claim in the plan, because it
underwrites both "automation is first-class" and "scripts can't stall the tick".
**Fix (decision):** scripts are **tick-entry-point** programs, not long-running coroutines:
the host calls `fn on_tick(state)` once per tick with a hard op budget; exceeding the budget
aborts *that call*, logs, and increments a strike counter (N strikes → script disabled, visible
in UI). Cross-tick continuation is the script's job via explicit persistent state
(`persist` map, serialized into the save). kOS-style blocking `wait`/`until` becomes a host
service that registers a wake event (see finding 10) and returns. Document that `wait` inside a
tick is a compile-time error.

### 22. Rhai's recommended wasm feature set breaks native/web parity, which the plan sells as a benefit. — **high**
Rhai's own wasm guidance recommends `unchecked` (which **disables max_operations and progress
tracking entirely**), plus `only_i32` and `f32_float`. Any of those silently makes the web build
a different language with different numerics and no budget enforcement — while §2.10 claims
"player scripts are portable between native and web" and the budget is part of deterministic
state.
**Fix:** pin one feature set for both targets: **not** `unchecked`, **not** `only_i32`, **not**
`f32_float`; accept the ~30% wasm slowdown and the panic-on-limit behaviour, and add a CI check
that the resolved Rhai feature set is identical on both targets (`cargo tree -f "{p} {f}"` diff).

### 23. `bincode` + "explicit `migrate_vN_to_vN+1` chain" does not work as written, because bincode is not self-describing. — **high**
Verified: bincode stores no metadata; upstream states backward-compatible struct changes are
"impossible", and `#[serde(skip*)]`, `flatten`, `tag`, `untagged` are documented to lose data.
A migration function can only run *after* you have decoded the old bytes — which requires the
**old struct definitions**, frozen, forever, one module per version.
**Fix:** write the policy: (a) an uncompressed, self-describing **header** (magic, `save_version`,
`game_version`, data-set hash, compression id) read before anything else; (b) `csp-data` contains
`schema/v1.rs`, `schema/v2.rs`, … each a frozen, never-edited copy of the save types, with
`migrate_v1_to_v2(v1::Save) -> v2::Save`; (c) current code works only on the newest schema;
(d) a CI test that every historical fixture save in `tests/fixtures/saves/` loads and migrates.
Also decide now whether you accept the frozen-schema maintenance cost or switch the save to a
self-describing format (postcard+field ids, or CBOR/MessagePack) and keep bincode only for
replays. My call: self-describing for saves, bincode for replay logs — replays are already
version-scoped by T3.

### 24. `SeedableRng::seed_from_u64` is not a value-stability contract. — **high** (plan §2.7 + `csp-core`)
The scaffold derives every stream via a 4-word splitmix-ish fold into
`ChaCha8Rng::seed_from_u64`. `seed_from_u64`'s output is a `rand_core` *implementation detail*;
if it changes in a future `rand_core`, every pre-drawn reliability threshold in every existing
save changes. That is a silent, world-altering dependency bump.
**Fix:** own the key derivation. Build the 32-byte ChaCha key yourself from
`(save_seed, stream_tag, entity_id, counter)` with a documented, tested, version-stamped mixing
function (`KDF_VERSION: u8` in the save header), and `ChaCha8Rng::from_seed(key)`. Add a test
with hard-coded expected outputs so a dependency bump fails loudly. Also reconsider the naming:
the current scheme *reseeds* per counter rather than advancing a stream, so "counters are part of
the save" means "we store an index", not "we store stream position" — say which, because the
reliability model's `U_k = Rng::for_(IGNITION, part, k)` depends on it.

### 25. TanStack Start is mature enough — but it is the wrong tool here, and the plan doesn't justify it. — **medium**
Verified: TanStack Start 1.0 shipped stable 2026-10-13 (`@tanstack/react-start` 1.169.0), so
"immature" is not the objection. The objection is fit: Start's value is SSR + server functions +
file-based server routes. `csp-server` serves *static* assets and a WebSocket; there is no
server-side JS tier, no SSR benefit for a localhost dashboard, and the release cadence is
~daily patch releases across ~20 interdependent packages — a standing maintenance tax on a
solo Rust project, plus a Node/pnpm toolchain in a Rust CI.
**Fix (decision):** build `apps/telemetry` as a **plain Vite + React SPA** with TanStack Router
(client-only) + TanStack Query, Tailwind, shadcn. You keep the routing/data-fetching ergonomics
and the component vocabulary you already know from `workspace-welcome`, and you drop the SSR
framework you cannot use. If you keep Start anyway, say *why* in one line (so the next reviewer
doesn't re-litigate) and pin exact versions.

### 26. "Universal-variable Kepler solver handles e≈1 and hyperbolic without branching bugs" overstates it. — **medium**
Universal variables remove *branching*, not *conditioning*. Known failure modes: Stumpff
`C(z)/S(z)` cancellation near `z≈0` (requires series near zero), Newton divergence for large
`Δt` and near-parabolic orbits (needs Laguerre–Conway with `n=5` and bracketing), and
sensitivity for very high eccentricity at periapsis. Under rails warp you will call it with
enormous `Δt`.
**Fix:** specify the solver concretely — Stumpff with series fallback for `|z| < ε`, Laguerre
iteration, a fixed iteration cap with a documented fallback, and reduction of `Δt` modulo the
period for elliptic orbits *before* solving. Add a property test sweeping
`e ∈ {0, 0.5, 0.9, 0.999, 1.0, 1.001, 3}` × `Δt` over 12 decades, asserting the
elements↔cartesian round-trip budget. Also: §2.8's "<1e-6 relative over one orbit" is undefined
for parabolic/hyperbolic (no period) — restate as "over a fixed `Δt` of 10⁷ s or one period,
whichever applies".

### 27. WebGPU tier claims verified correct and should be left alone. — **informational**
Checked: bindless in WebGPU is still at proposal/design stage with no browser preview as of
mid-2026; `chromium-experimental-multi-draw-indirect` exists only behind the unsafe-WebGPU flag
with ~0.1% field availability. §5.1's two-renderer position is correct and conservative. Keep it.
Add the dated stamp per finding 18 and a one-line "revisit when two browsers ship bindless".

---

## D. Unspecified system interactions

### 28. Reliability on unloaded, on-rails vessels is undefined, and there is no failure event kind. — **high**
§3.4 says `Λ` integrates in closed form over rails intervals and the threshold `θ` is pre-drawn.
Unanswered: do non-operating parts accrue hazard (storage/thermal-cycling hazard)? Does a probe
that has been in orbit for 10 sim-years fail its reaction wheel while unobserved? §2.6.1's event
list contains `ReliabilityInspectionDue` but **no `PartFailure`** — yet failure is precisely a
thresholded nonlinearity that must publish a next-event time (`t` where `Λ(t) = θ`, closed-form
for piecewise-constant `λ`).
**Fix:** add `PartFailure { vessel, part_instance }` as a `Silent`-class event (finding 13);
define a `dormant_stress` multiplier (default small but nonzero) in `ReliabilityDef`; and decide
the UX: failures on unloaded vessels are *recorded*, surfaced in a notification log, and applied
on next load. State that dormant failure modes are restricted to non-fatal ones.

### 29. Science conversion, colony production, and life support under physics warp / in-bubble are unspecified. — **high**
Covered structurally by finding 1, but there are concrete sub-cases the plan must answer:
a lab aboard a vessel in the physics bubble while the bubble steps at 256 ticks/s; crew
consuming O₂ on a landed vessel that is simultaneously a colony module; a drill running on a
vessel inside the bubble vs the same vessel on rails.
**Fix:** state the rule: subsystem advancement is **location-independent** — a lab/drill/life-support
module advances by interval regardless of whether its vessel is in the bubble, and the bubble's
tick loop only drives forces, contacts, and control. One sentence, kills a whole class of bugs.

### 30. Colony logistics vs M6 docking and the `Composite` pull. — **medium**
See finding 9 for the cut-list consequence. Separately: §3.5 says logistics is "you fly it", but
nothing specifies the *transfer act* — proximity threshold, whether a landed vessel auto-joins a
colony's inventory, transfer rate, whether transfer is instantaneous or an event-publishing
process (it must publish events to survive warp).
**Fix:** specify `ResourceTransfer` as a sim process with a rate and a completion event, a
docked-or-within-`N` m precondition, and an explicit rule for "landed vessel within colony
radius joins the colony inventory (opt-in, toggled by a `SimCommand`)".

### 31. Crew model is missing and is shared by three systems. — **high**
Researchers (§3.2, hired, skill, salary), crew (§3.5, life support, transfer in M6), tourists
(§3.6 contract type), and deaths-as-a-setting all imply a roster entity that has no data model,
no ID scheme, no assignment rules, and no save entry in §7's list.
**Fix:** define `CrewId` + `CrewMember { name, kind: {Pilot, Engineer, Scientist, Tourist},
skill, assignment: Facility|Vessel|Unassigned, salary, status }`, serialized, with the
assignment transitions that M6 transfer and §3.2 lab staffing need. Add to §7.

### 32. Configuration-dependent aero tables. — **high**
§2.4.1 computes one `AeroTable` per part per weld, keyed by part-graph hash. But fairing
deployment, cargo-bay doors, extended landing gear, and deployed solar panels all change
shielding *at runtime* without changing the part graph. With one table, a jettisoned fairing's
payload stays shielded (or never was).
**Fix:** weld produces a table **per discrete configuration** (`ConfigMask` over
deployable/jettisonable parts), computed lazily and capped (e.g. ≤8 configurations; editor warns
beyond). State the fallback: unknown configuration falls back to the nearest computed mask with
a logged warning. Also specify the 26-direction **interpolation scheme** — 26 scattered
directions is not a grid; use barycentric interpolation on the triangulated direction hull, or
switch to a regular spherical parameterization. Unspecified interpolation here means unspecified
forces.

### 33. Voxel grid resolution is fixed at 64³ over the vessel AABB — this degrades to nonsense at station scale. — **high**
A 100 m station gives ~1.5 m voxels; a 0.3 m RCS thruster is sub-voxel and its shielding is
meaningless. The budget table (§5.3) also only covers a 300-part craft.
**Fix:** resolution is `max(64, ceil(longest_axis / target_voxel_m))` clamped by a memory cap,
with `target_voxel_m` a balance constant; or voxelize per-stack rather than per-vessel. State
the cap and the behaviour at the cap (coarser voxels + a warning), and note that aero shielding
only matters in atmosphere so an orbital-only assembly can skip voxelization entirely — which is
also the performance answer.

### 34. Physics-warp CPU budget is not actually closed. — **medium**
§2.6 allows ×4 (256 ticks/s); §5.3 budgets sim step <2 ms for a 200-part vessel → 512 ms/s of
CPU, while also demanding 144 fps rendering. For the 1000-part station scene there is **no**
sim-step budget at all, only an fps number.
**Fix:** add sim-step budgets per scene, and state the demotion rule numerically: warp level is
the largest `k` such that `measured_step_ms * 64 * k < budget_fraction * frame_ms`, with
`budget_fraction` a setting (default 0.5), hysteresis to prevent oscillation, and the resulting
level emitted as a logged command (finding 12).

---

## E. Missing specs an implementer would have to invent

### 35. The state hash is referenced ~8 times and specified nowhere. — **high**
M0's exit criterion is "state-hash harness exists"; §2.7, §7, §8 all depend on it. Undefined:
what is included, field/arena traversal order, float encoding (`to_bits`? what about `-0.0` and
NaN?), whether `Option`/empty-arena-slot representation is canonical, and what the hash is
(BLAKE3? fixed output width?).
**Fix:** write §7.1: hash = BLAKE3 over a canonical byte stream produced by a hand-written
`StateHasher` visitor (not serde), dense arena index order, all floats via `to_bits()` with
`-0.0 → 0.0` and NaN forbidden (debug assert: no NaN may enter sim state), explicit exclusion
list mirroring §7's "not serialized" list, and a `HASH_SCHEMA_VERSION` recorded next to every
golden file.

### 36. Data-file format versioning is absent. — **medium**
§7 handles *save* versioning carefully and says nothing about *data* versioning. `PartDef` RON
files have no `schema_version`; `data/balance.ron`, tech trees, and system defs likewise. The
first time you add a required field, every community part file breaks with a serde error.
**Fix:** every data file gets `schema_version: u32` as its first field; the loader supports the
current plus previous version with explicit defaults; the validator (§8) enforces presence.
Also specify the data-set hash's inputs precisely — it's load-bearing for the §7 drift warning
and for the frozen-upgrade snapshot (finding 4): hash over sorted canonical-form file contents
of `data/`, excluding art, with the algorithm pinned.

### 37. Error-handling policy. — **high**
`csp-core` is said to hold "error types" and that's it. Nothing says: may `csp-sim` panic? (It
must not — it runs inside a server and a bench harness.) What happens on a mid-tick invariant
violation, a failed weld, a script that panics in a host function, a data file that fails
validation at runtime, a corrupt save, a disconnected GPU?
**Fix:** write §2.13: `csp-sim` returns `Result` at step granularity and **never panics** in
release (debug asserts allowed); a `SimError` aborts the tick, rolls back to the pre-tick
snapshot (requires a one-tick shadow copy — state that cost), and surfaces to the UI; loader
errors are collected not fail-fast (report all bad parts, load the rest, mark the save
unsavable); renderer device loss reconfigures or exits cleanly; `csp-server` errors never reach
the sim. Define the four classes: `Fatal`, `SaveBlocking`, `Recoverable`, `Cosmetic`.

### 38. Logging and observability — and the fact that §2.1's rules forbid both inside the sim. — **medium**
`csp-sim` may not use `std::time` and (per the spirit of the rules) shouldn't emit host-side
side effects, which means you cannot instrument the one thing whose performance is budgeted. No
tracing crate is chosen; no policy on log levels, log sinks, or how a player's bug report
captures the command log + save + data hash.
**Fix:** `tracing` at the `csp-app`/`csp-server` boundary; `csp-sim` emits structured
`SimEvent`s into a ring buffer it owns (deterministic, part of no hash, drained per frame);
timing instrumentation lives in the *caller* (`csp-app` measures `step()`), never inside. Add a
"bug report bundle" artifact (save + command log + data-set hash + versions + last N log lines),
which also makes §2.12's eval harness reproducible as claimed.

### 39. WS/MCP protocol versioning and the telemetry wire format. — **high**
§2.12 mentions "a WebSocket feed of snapshot telemetry" with no schema, no versioning, no rate
policy, and no subscription model. At 64 Hz, a 1000-part vessel's full snapshot is megabytes per
second to a browser — and a dashboard that asks for "all parts" cannot be served naively.
**Fix:** specify: a versioned envelope (`{ proto: 1, kind, seq, tick, payload }`), a
**subscription** model (client declares the telemetry field paths its widgets need; the server
sends only those), server-side decimation to a configurable rate (default 10 Hz; never tied to
the tick rate), delta encoding optional, and a documented backpressure rule (drop to latest, never
queue). Version the MCP tool schema independently (`csp.mcp.tools.v1`) and state the
compatibility promise: additive-only within a major, tools never silently change semantics.

### 40. The testing pyramid below golden replays is empty. — **high**
§8 is six integration/nightly gates and `cargo test --workspace`. Nothing names the unit and
property tests that would make a failing golden replay *diagnosable* — right now a red golden
hash tells you "something in the universe changed" and nothing more.
**Fix:** name the tier explicitly in §8: (a) **property tests** (`proptest`) for Kepler
round-trip, universal-variable solver across the e/Δt sweep, weld mass-property invariance under
part reordering, momentum conservation on breakup, voxelizer determinism under AABB
perturbation, hazard-integral closed-form vs fine-stepped reference within tolerance,
`advance_to` equivalence vs tick-stepping (finding 1's test), event-heap total-order under ties;
(b) **unit tests** per subsystem's closed-form integrator against a naive stepped oracle;
(c) **fixture tests** for every historical save version (finding 23); (d) golden replays last.
Also add a `--bless` workflow and store a **determinism fingerprint** (rustc version, target,
exact versions of `parry`/`rapier`/`libm`, `HASH_SCHEMA_VERSION`) alongside each golden — without
it, every dependency bump is an unexplained red build, which is how determinism gates get
switched off.

### 41. `csp-server` has no threading/concurrency design, and no stated host for the MCP stdio transport. — **high**
It is "embedded HTTP+WS", native-only, and will bring tokio + an HTTP stack into a project whose
core forbids threadpools. Unspecified: how the server reads snapshots without blocking the frame
loop (double-buffer? `Arc<Snapshot>` swap? what's the publish point?), how external commands
enter the queue across the thread boundary, where the per-save token is stored and rotated, how
the browser obtains it, and — for the stdio transport — **which binary** an eval harness spawns
(`csp` the windowed app? `csp-bench`? a third `csp-mcp` bin?).
**Fix:** decide: axum + tokio in `csp-server` only; the sim publishes `Arc<SimSnapshot>` once per
frame into an `ArcSwap`, readers never block the frame; ingress is a bounded MPSC drained in
`Input` and ordered by finding 11's `CommandKey` with `source_class = External`; the token is
written to `$XDG_RUNTIME_DIR/csp/<pid>.token` (0600) and injected into the SPA's served HTML, not
stored in the save; and the stdio MCP server is a **separate `csp-mcp` bin in `csp-bench`** that
owns its own headless sim — attach-to-running-game is the HTTP path only. Write this as §2.12.3;
it is currently the vaguest high-visibility area of the plan.

---

## F. CI and measurability

### 42. `cargo check --workspace --target wasm32-unknown-unknown` contradicts "`csp-server` is native-only and out of the wasm gate's reach". — **high** (plan-vs-CI, already live)
The current workflow checks the whole workspace on wasm. It passes only because `csp-server` and
`csp-bench` are empty; the day axum/tokio lands, the wasm gate breaks and the reflex will be to
weaken the gate.
**Fix now:** `cargo check --workspace --exclude csp-server --exclude csp-bench --target
wasm32-unknown-unknown`, plus a *positive* check that the wasm tree does not contain them
(`cargo tree --target wasm32-unknown-unknown -p csp-app | grep -q csp-server && exit 1`).

### 43. The "CI-enforced dependency rules" and determinism lints claimed by §2.1/§2.7/§8 and by M0's exit criteria do not exist. — **medium** (plan-vs-code)
The workflow has one `cargo tree | grep` line. There is no `clippy.toml`, so
`disallowed_methods` (the stated libm enforcement) is not configured; there is no `HashMap` ban;
`csp-data`/`csp-phys` are not checked (they don't exist); and `grep -E "rand "` would not match
`rand_chacha`/`rand_core` — which `csp-sim` *will* pull transitively through `csp-core`.
**Fix:** add `clippy.toml` with `disallowed-methods` for `f64::{sin,cos,tan,asin,acos,atan,atan2,
exp,ln,powf,powi,sqrt,cbrt,hypot}` and `disallowed-types` for `std::collections::{HashMap,HashSet}`
+ `std::time::*`, applied via `[lints]` in `csp-sim`/`csp-core`/`csp-phys`/`csp-data` only. Also
reconcile §2.1's allowlist with reality: `csp-sim → csp-core → rand_chacha` means the rule should
read "no `rand` facade, no `getrandom`, no thread-local or global RNG", not "no rand".

### 44. Nightly render-screenshot and fps-budget gates cannot run on hosted GitHub runners. — **high**
§8 schedules per-tier screenshot comparison and §5.3 fps budgets in nightly CI. Hosted runners
have no GPU; the Advanced tier (bindless, `multi_draw_indirect`, mesh shaders) cannot even be
*instantiated* on lavapipe, and 144 fps is not a software-rasterizer number. As written, these
gates are unrunnable, which means they will be quietly skipped.
**Fix:** split the claim. CI-runnable: naga/WGSL compile validation for both binding models,
headless Baseline-tier screenshots on lavapipe with a loose tolerance, and *sim-step / rails-advance
ms* budgets (CPU-only, runnable anywhere). Not CI-runnable: fps budgets and Advanced-tier
screenshots — these run on a **named reference machine** (specify CPU/GPU/driver/resolution in
§5.3) with results committed to a perf log. A performance budget without a reference machine is
unmeasurable; §5.3 currently has none.

### 45. Unmeasurable or unfalsifiable statements that should be numbers. — **medium**
- "tune it so idling at warp is net-negative without being punishing" (§3.5) — no target.
  **Fix:** "a fully-staffed base with no missions loses 2–5% of liquid funds per sim-month at
  default difficulty; the economy harness asserts the band."
- "both tiers must produce a correct image for every scene" (§5.1) — "correct" undefined.
  **Fix:** per-tier tolerance (e.g. ≤2% of pixels beyond ΔE 3, no geometry/visibility
  differences), measured by the screenshot harness.
- "round-trip error budget <1e-6 relative in position over one orbit" (§2.8) — see finding 26.
- "momentum conserved to within 1e-9 relative" (§2.4) — relative to *what* (initial momentum
  magnitude? total mass·velocity scale?). Define the denominator or the test is untestable for
  near-zero-momentum cases.
- "egui is fine for panels, wrong for a 3D canvas — evaluate in M3 with a real 100-part craft"
  (§9) — no criterion.
  **Fix:** state the kill criterion: "if VAB interaction latency exceeds 16 ms at 300 parts, the
  panel layer is replaced."
- "frame budget exceeded" (§2.6) — see finding 34.

### 46. "Golden replays on every commit" includes a colony-century scenario; runtime is unbudgeted. — **medium**
**Fix:** state a CI wall-clock budget (e.g. the per-commit replay suite must finish in <5 min);
put the century-scale and pathological-event-density scenarios in nightly; keep a fast smoke
subset per commit.

---

## G. Scope and timeline honesty

### 47. M9 is four milestones wearing one hat. — **critical**
M9 ships: `ControlApi` freeze, the Rhai host with budgets and persistence, three Rust autopilots
(ascent/rendezvous/landing are each a real control-engineering problem), `csp-server` (HTTP + WS
+ auth + snapshot publishing), the entire TanStack dashboard app with a widget registry and
drag/resize persistence, the MCP server (transport + tool schema + resources), the `csp-bench`
scenario format, the first eval suite, **and** the full reliability model (hazard, failure modes,
qualification programs, test-stand facility, part-test contracts, confidence UI). Its neighbours
M6 and M10 are each a fraction of that. This is the plan's biggest honesty problem now that the
determinism story is sound.
**Fix:** split into M9a *Automation* (`ControlApi` + Rhai host + ascent autopilot), M9b
*Reliability* (the whole of §3.4 — it is a milestone, not a bullet), M9c *Interfaces*
(`csp-server` + WS telemetry + MCP), M9d *Eval* (dashboards + bench scenarios + eval suite). Also
note the ordering constraint the plan currently hides: `ControlApi` cannot be frozen *before*
the autopilots and the MCP tool surface exercise it, so "freeze at M9" (§9) should read "freeze
at the end of M9c".

### 48. The real critical path is not stated anywhere. — **high**
§6 lists milestones and §9 names "solo scope" as the dominant risk, but nothing identifies the
chain that actually gates ship. From the architecture it is:
`csp-core clock/RNG → event queue + advance_to → OrbitModel → weld + force model → EffectivePart
resolver → the save/state-hash/replay harness → everything else`.
The harness in particular is on the critical path of *every* subsequent milestone (it's how you
know you broke something), yet it appears only as a parenthetical M0 exit criterion.
**Fix:** add a §6.0 "critical path" paragraph naming that chain, and promote the
state-hash/replay harness to a first-class M0 deliverable with its own exit criteria (finding 35).
Also name the three things that can be built in any order and are therefore schedule slack
(render tiers beyond R0, dashboards, audio).

### 49. Secretly-bigger-than-their-neighbours milestones, ranked. — **high**
- **M9** — see finding 47.
- **M4** — "flight fidelity" contains the voxelizer (+ configurations, finding 32, + resolution,
  finding 33), Mach/Reynolds aero curves, a per-part thermal network with conduction *and*
  reentry heating, and structural/thermal breakup with momentum conservation. That is four
  subsystems and it is also the declared fun gate, i.e. the point of maximum "one more tweak"
  risk. Split: M4a aero, M4b thermal, M4c breakup.
- **M8** — colonies, drills/converters, a power model, closed-form production, a manufacturing
  queue consuming resources *and* time, supply missions, and life support. Each needs event
  publication and a UI.
- **M3** — now carries the `EffectivePart` resolver (correctly pulled forward) *plus* the part
  snap graph, staging, scaling, variants, craft save/load, and GPU picking. The editor is
  historically where solo projects lose a quarter; it is listed as one row.
**Fix:** re-letter these and state expected effort in relative units (not dates) so the cut list
has something to cut against.

### 50. The cut list's first item saves almost nothing. — **high**
"Web tier → playable demo only" is first to be cut, but the web tier's cost is paid *up front and
continuously*: wasm-clean dependency selection for every crate, no-threadpool constraints, the
reduced-dataset work, the Baseline renderer, the wasm CI gate, and the Rhai feature-parity
constraint (finding 22). Cutting it at month 30 refunds none of that. Meanwhile the thing the web
tier buys (the pillar-5 claim, and a shareable demo) is a marketing asset, not a mechanic.
**Fix:** be honest about what cutting web actually recovers (the Baseline-tier polish, the
reduced-dataset authoring, and the nightly wasm perf work — not the architectural constraints),
and reorder the cut list accordingly: R3 Advanced tier → life support → third body → orbital
docking (keeping proximity transfer, finding 9) → web tier polish. Alternatively, make the
opposite decision cleanly: if web is first to be cut, drop the wasm CI gate *now* and keep only
"don't gratuitously break wasm" as a convention. Pick one; the current plan pays the premium and
ranks the product first for the chopping block.

### 51. No calendar, no capacity, no definition of "done enough". — **medium**
"M0–M10 is multiple years" is the only estimate in the document, and §9 calls scope the dominant
risk while giving the risk no instrument. There is no statement of hours/week available.
**Fix:** one line of capacity (e.g. "10 h/week"), relative effort units per milestone (finding
49), and a quarterly review ritual whose only job is to execute the next cut-list item if the
previous quarter missed its milestone. A cut list with no trigger is a wish list.

---

## H. Plan vs scaffold (M0 is not as done as it looks)

### 52. The authoritative clock lives in the bevy ECS as a `Resource`. — **high**
`csp-view::Clock(SimTime)` is advanced by a bevy system (`advance_clock`) inside the `SimStep`
schedule, and `csp-app` reads sim time from the ECS. That is precisely the arrangement §2.2
forbids ("authoritative sim state does not live in the ECS"), and it is the shape REVIEW.md
finding #1 was written to eliminate. Right now it's harmless; as the first real pattern an
implementer copies, it is the most expensive line in the repo.
**Fix:** the clock moves into `csp-sim::World`; `csp-view`'s `Clock` becomes a mirrored,
presentation-only copy written by `Sync`, and `SimStep` becomes the single system that calls
`csp_sim::World::step()` as §2.5 says. Do this before M1.

### 53. Five of the twelve planned crates don't exist; two exist as doc-comment stubs; `apps/telemetry/` and `data/` (beyond `ATTRIBUTION.md`) don't exist. — **medium**
Missing: `csp-data`, `csp-phys`, `csp-ui`, `csp-script`, `csp-ai`. Stubs: `csp-sim` (4 lines),
`csp-server` (2 lines), `csp-bench`, `csp-render` (a `Gpu` wrapper). Not a defect per se — but
M0's exit criteria claim "dependency-rule lint active" and "state-hash harness exists", and
neither is true (findings 43, 35). The scaffold reads as "M0 done"; it is "M0 windowing done".
**Fix:** either create the empty crates now (so the dependency lint has something to enforce and
the layout stops being aspirational) or annotate the crate-layout tree in §2.1 with
`(not yet created)` markers. And don't mark M0 complete until the lint and the hash harness exist
— they're the two things M1 onward depend on.

### 54. No `[workspace.dependencies]`, no `[workspace.lints]`, no pinned versions, no MSRV, no `rust-toolchain.toml`. — **medium**
Six crates independently declare `wgpu = "30"`, `egui = "0.37"` etc. with caret ranges. For a
project whose golden replays are version-sensitive by design (T3 is explicitly not promised), a
floating dependency graph plus `dtolnay/rust-toolchain@stable` in CI means golden hashes can
change on an unrelated Tuesday. `bevy_ecs` 0.20 also sets MSRV 1.95, which nothing records.
**Fix:** `[workspace.dependencies]` with exact `=` versions for anything in the determinism
domain (`libm`, `parry3d-f64`, `rapier3d-f64`, `rand_chacha`, `rand_core`, `bincode`), caret for
the rest; `[workspace.lints]` for the shared lint policy; a committed `rust-toolchain.toml`;
`Cargo.lock` committed (it is — good); and the determinism fingerprint from finding 40.

### 55. `bevy_ecs` features: `["std"]` only, while §2.5 promises a multi-threaded executor on native. — **low**
`multi_threaded` requires `bevy_tasks/multi_threaded`, and the parallel executor also wants
`async_executor` (which bevy_ecs lists in its defaults and which is `std`-only). The scaffold
can't run anything in parallel today.
**Fix:** declare the intended feature sets per target now (`multi_threaded` + `async_executor` on
native, neither on wasm) so the wasm/native divergence is explicit rather than discovered during
R3. Note also that §2.5's parallel `Presentation`/`RenderExtract` is the only remaining use for
bevy_ecs's executor — if you never enable it, §2.5's own escape hatch ("dropping bevy_ecs is a
cheap change") becomes the obvious call.

### 56. The dependency-rule CI line is a brittle shell one-liner. — **low**
`! cargo tree -p X | grep -E ... && ! cargo tree -p Y | grep -E ...` with `2>/dev/null` happens
to behave correctly, but it silently passes if `cargo tree` itself fails (output empty → grep
fails → `!` succeeds), and the patterns are substring-fragile (`"bevy"` also matches
`bevy_platform`, which is arguably fine; `"rand "` matches nothing).
**Fix:** a small `xtask` (or `cargo-deny` with `[bans]`) that parses `cargo tree --format "{p}"`
and asserts an explicit allow/deny list per crate, failing on tooling error. `cargo-deny` also
gets you the license audit §3.7 wants.

### 57. §3.7's "CI check fails on files lacking attribution entries" doesn't exist and has no format. — **low**
`data/ATTRIBUTION.md` exists with no specified schema.
**Fix:** define the line format (`path | source | license | url`), and implement the check in the
same xtask as finding 56. Low stakes until the first borrowed asset — but that's also when it's
cheapest to add.

---

## I. Things I tried to break and couldn't — load-bearing, leave them alone

1. **Authoritative state outside the ECS, commands as the only mutation path (§2.2).** Every
   other finding in this review got easier to fix *because* of this decision. It is the spine.
2. **Immutable 1/64 s physics dt (§2.6).** Finding 1 attacks the *subsystem* integrators, not
   this rule — the rule itself is what made finding 1 visible. Keep it, and keep it stated as the
   most important line in the document.
3. **Rails warp as an analytic jump over an event heap, with the "publish your next event time or
   it's a bug" rule (§2.6.1).** Findings 13/14 add classification and back-pressure; the
   architecture is right. The `(tick, event_seq)` total order is correct and necessary.
4. **The three-tier determinism promise (§2.7), especially the nightly cross-platform
   *divergence report that doesn't fail the build*.** This is the cheapest high-value idea in the
   plan. Don't let anyone turn it into a gate.
5. **Welded single rigid body + analytic per-part forces; joints only where physically required
   (§2.4).** Finding 5 extends it (docking rigidizes); the core call kills the KSP1 kraken class
   of bugs and is correct.
6. **`SimpleBackend` default with Rapier confined to jointed islands and used as a test oracle
   (§2.9).** Verified the surrounding facts: `rapier3d-f64` exists, `enhanced-determinism` is
   real and documented as IEEE-754-strict cross-platform, mutually exclusive with `simd8`, and
   `parallel` is rayon-based (unusable on wasm). The plan's reading of this is accurate and the
   risk inversion is the right trade.
7. **Two-tier renderer with an explicit capability contract (§5.1).** Independently verified:
   WebGPU bindless is still pre-preview, multi-draw-indirect is flag-only with ~0.1% field
   availability. This is not pessimism, it's current fact. Also correct to have deleted the
   `wgpu::hal` escape hatch.
8. **`OrbitModel::next_soi_event` in the trait rather than a step-and-check loop (§2.8).** The
   right shape, and it's the reason the post-v1 N-body swap is credible.
9. **Pre-drawn, serialized reliability thresholds `θ = -ln(U)` with a closed-form hazard integral
   (§3.4).** Anti-save-scum, warp-safe, and testable. Findings 24 and 28 harden the plumbing; the
   model is sound.
10. **Soft-capped cumulative science per key (§3.2) and multiplicative `UpgradeTrack` deltas
    rather than new `PartDefs` (§3.3).** Both are correct replacements for the originals.
11. **One `EffectivePart` resolver shared by editor, sim, design AI, and contract validation,
    pulled forward to M3 (§3.1).** The highest-leverage sequencing decision in §6.
12. **Rejecting "core features as mods against the public API" (§4).** Correct, and worth keeping
    the rejection *in writing* so it doesn't come back.
13. **`bevy_ecs` 0.20 standalone with `default-features = false`.** Verified: upstream explicitly
    supports standalone use; `World::new()` + `Schedule::run(&mut world)` is the documented
    pattern; `bevy_reflect` is an optional default that can be turned off. No hidden `bevy_app`
    requirement. The §2.5 plan is implementable as written.
14. **ChaCha8 + explicit stream derivation as the RNG architecture.** ChaCha8 is integer-only and
    bit-identical across platforms — the right primitive. Finding 24 is about the *seeding* path,
    not the choice.
15. **"No background thrust in v1" (§2.6).** I looked for a way to argue this is too restrictive
    and couldn't — it's what keeps rails analytic, it's KSP-consistent, and the post-v1 escape
    (Edelbaum/averaged elements) is correctly scoped with its caveats.
16. **Network I/O never inside a tick; LLM responses enter as tick-stamped commands; confirmation
    required for irreversible actions (§2.11).** Correct and sufficient.

---

## J. Decisions I'd make right now (owner calls, so nobody has to ask)

1. **Subsystems advance by interval, never by tick.** One integrator per subsystem. (Finding 1.)
2. **Docking rigidizes into a single welded body after soft-capture.** (Finding 5.)
3. **Saving is refused while a jointed island or persistent contact set is live.** (Finding 6.)
4. **Rhai scripts are tick-entry-point programs with explicit persistent state and host-registered
   wake events. No suspension, no resumption.** (Findings 10, 21.)
5. **Saves use a self-describing body with a plaintext header; bincode stays for replay logs.**
   (Finding 23.)
6. **Own the RNG key derivation; never `seed_from_u64`.** (Finding 24.)
7. **`apps/telemetry` is a client-only Vite + TanStack Router SPA, not TanStack Start.**
   (Finding 25.)
8. **MCP: stdio + Streamable HTTP, with a recorded target protocol revision and Origin
   validation. Attach-to-running-game is HTTP only; stdio is a separate headless `csp-mcp` bin.**
   (Findings 19, 41.)
9. **Dashboards are a non-authoritative sidecar file, excluded from the state hash.** (Finding 7.)
10. **M9 splits into M9a–M9d; reliability is its own milestone.** (Finding 47.)
11. **Reputation folds into contract gating. Delete the open question.** (Finding 8.)
12. **Fix the wasm CI gate and add `clippy.toml` + `[workspace.dependencies]` before M1 starts.**
    (Findings 42, 43, 54.)
