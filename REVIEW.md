# PLAN.md Review — adversarial pass

Reviewed as four specialist passes: (A) architecture/determinism, (B) rendering/GPU stack,
(C) game-design economy, (D) scope/roadmap feasibility. Findings below, each with what changed.

## A. Architecture & determinism

1. **State ownership was never stated.** The plan had `csp-sim` ("headless, deterministic") *and*
   `csp-ecs` ("bevy_ecs schedules") with no answer to "where does the authoritative vessel live?"
   If it's in a bevy `World`, you inherit the parallel executor's nondeterministic mutation order,
   `bevy_reflect`, and a save format that is "serialize a bevy World".
   → **Changed**: new §2.2. Authoritative state is plain data in `csp-sim` in index arenas;
   `bevy_ecs` (renamed crate `csp-view`) is presentation-only and read-only w.r.t. sim. All
   mutations flow through a tick-stamped `SimCommand` queue (which also gives replay, undo, and a
   non-privileged LLM path for free).

2. **bevy_ecs + custom renderer coherence was asserted, not specified.** No schedule list, no
   ordering, no statement that `bevy_app`/`bevy_render` are forbidden, no executor policy.
   → **Changed**: new §2.5 with six named schedules, explicit ordering, single-threaded executor for
   `SimStep`, `default-features = false`, and a CI-enforced dependency rule (§2.1). Also noted that
   because sim state isn't in the ECS, dropping bevy_ecs later is cheap — that's the point of the split.

3. **Timewarp silently destroyed the determinism claim.** "Fixed timestep" + "physics warp ×2–×8
   substepped" means dt varies with the player's warp key, so trajectories aren't reproducible.
   → **Changed**: §2.6 makes physics dt immutable at 1/64 s forever. Warp = ticks-per-frame (physics)
   or an analytic clock jump (rails). Declared the single most important rule in the document.

4. **"Physics warp substepped" is not a speedup.** Substepping is *more* work; it can't make the sim
   run faster than real time.
   → **Changed**: physics warp is explicitly CPU-bound (×2–×4, measured, auto-demoting), and the
   plan says so instead of advertising ×8.

5. **Rails warp ×10⁷ is unimplementable and bad play.** 10⁷ sim-s/wall-s means 166 sim-days per
   frame; no SOI entry, deadline, or colony event resolves usefully.
   → **Changed**: capped at ×10⁵, with `advance_to()` + a deterministic event heap (§2.6.1) and the
   hard rule that any thresholded subsystem must publish a next-event time or it's a bug.

6. **SOI transitions were implicitly sampled.** Under warp, sampling misses encounters.
   → **Changed**: `OrbitModel::next_soi_event` is part of the trait, solved by root-find to 1 tick.
   Shaped so the post-v1 N-body implementation can satisfy the same contract.

7. **Determinism hazards not enumerated.** No mention of `HashMap` iteration order, libm
   transcendental divergence, Rapier's lack of cross-version determinism, `parallel` being unusable
   on wasm, or SIMD-width-dependent paths.
   → **Changed**: §1.1 consequences + §2.7 defines three determinism tiers (T1 replay = v1 target,
   T2 cross-platform, T3 cross-version = explicitly not promised), with CI gates and a nightly
   cross-platform *divergence report* that costs nothing and tells you if T2 is still reachable.

8. **RNG was "seeded" with no scheme → save-scumming and replay holes.**
   → **Changed**: no global RNG; `Rng::for_(stream, entity_id, counter)`. Reliability outcomes are
   pre-drawn per part instance and serialized, so reloading cannot reroll a failure.

9. **Rapier is likely the wrong dependency for a welded-body design, and the stated escape hatch
   was non-viable** — Jolt is C++, which breaks the wasm-clean, pure-Rust premise.
   → **Changed**: §2.9. `SimpleBackend` (own integrator + parry3d-f64 collision) is the v1 default;
   Rapier is confined to jointed islands (docking/robotics) and used as a test oracle. This removes
   the biggest third-party determinism risk and the risk table's bogus mitigation.

10. **Save-game implications of determinism were an "open question".** They're load-bearing.
    → **Changed**: new §7. Snapshot authoritative; physics-solver internals and all derived caches
    are never serialized but re-derived on load; a save→load→N-ticks state-hash test; explicit
    migration chain; permanent string `PartId`s; data-set hashing to warn on part-stat drift;
    replays as a separate artifact.

11. **Scripting had two backends = two languages, two semantics, guaranteed drift**, and no
    determinism story (an unbounded script stalls or desyncs the tick).
    → **Changed**: §2.10. One guest language (Rhai) on all platforms; wasmtime demoted to a
    native-only post-v1 mod host; built-in autopilots written in Rust against `ControlApi`;
    per-tick op budgets whose suspension points are part of deterministic state.

12. **"LLM sandboxed by construction" was too glib** — it can still drain funds, and network latency
    inside a tick breaks replay.
    → **Changed**: network I/O never inside a tick; responses enter as tick-stamped commands;
    irreversible actions require player confirmation.

13. **Missing: units/epoch conventions, depth-precision strategy, bubble-recentering threshold.**
    → **Changed**: §2.3 (SI units, radians, integer tick clock, 1 km recentre threshold, reverse-Z
    with infinite far plane, per-patch f64 terrain origins, scale/precision arithmetic shown).

## B. Rendering / GPU stack

14. **"WebGPU is the floor, features degrade gracefully" is false for the stated goals.** Bindless
    binding arrays, `multi_draw_indirect`, indirect-count, mesh shaders, and 64-bit atomics are
    native-only in wgpu; WebGPU has none. A GPU-driven pipeline isn't a flag away from the web path —
    it's a different renderer.
    → **Changed**: §5.1 declares an explicit two-tier contract (Baseline / Advanced) with a
    capability table, a rule that both tiers must render every scene correctly, per-tier screenshot
    tests, and single-source shaders with binding-model branches.

15. **`wgpu::hal` raw-Vulkan passthrough listed as the escape hatch** — using it forfeits Metal,
    DX12, and web, i.e. it contradicts the two headline requirements.
    → **Changed**: removed; replaced with "contribute upstream, or Advanced-tier-only".

16. **Mesh shader status was vague ("wgpu experimental → stable").** They're real but experimental:
    `Features::EXPERIMENTAL_MESH_SHADER`, full on Vulkan, Metal/DX12 via passthrough shaders as of
    wgpu 28.
    → **Changed**: pinned to R4, Advanced-tier, opt-in, never on the critical path.

17. **Render phases were labelled M0/M5/M7, colliding with the milestone table** (render "M5" =
    terrain, milestone M5 = second body).
    → **Changed**: renamed R0–R4 and mapped to milestones.

18. **Virtual texturing on web ignores the wasm memory ceiling** (32-bit, ~2 GB practical,
    single-threaded).
    → **Changed**: VT is native-tier, post-v1; web runs a reduced dataset, measured in CI.

19. **"TAA/FSR" — FSR is not available through wgpu.**
    → **Changed**: TAA + a hand-rolled upscaler; no vendor-SDK promises.

20. **No performance budgets anywhere**, so "it's slow" could never be a build failure.
    → **Changed**: §5.3 budget table (per-scene fps, sim-step ms, rails-advance ms, weld time) with
    a nightly CI regression gate.

## C. Game design / economy

21. **`value(n) = v0 / 2^n` caps lifetime science per key at 2·v0** and makes the 4th run worth 6%.
    → **Changed**: §3.2 uses an authored per-key cap with exponential approach to it
    (`yield = cap·exp(-k·collected/cap)·units`), so run 1 ≈ 63%, run 2 ≈ 86%, run 3 ≈ 95% of cap.
    Added transmit-vs-recover ratios and the KSP-proven six-situation granularity instead of
    "biomes are granular" hand-waving.

22. **"Researchers convert data over time" has no closed form** → it cannot survive rails warp.
    → **Changed**: lab conversion rate is an explicit formula, integrated analytically over warp
    intervals, publishing completion/exhaustion events.

23. **"Parts evolve along upgrade tracks" had no mechanism**, and upgrades-as-new-PartDefs would
    cause combinatorial part spam plus a save-migration nightmare.
    → **Changed**: §3.1/§3.3. Concrete `PartDef`/`ModuleDef`, an `UpgradeTrack` of multiplicative
    deltas, and a single `EffectivePart` resolver (`def ∘ upgrades ∘ scale ∘ variant ∘ wear`) shared
    by editor, sim, design AI, and contract validation. Upgrades apply at build time, never
    retroactively to craft already in flight.

24. **Reliability model was prose, not a model** — no hazard function, no RNG scheme, no warp
    integration, and random failures were save-scummable.
    → **Changed**: §3.4. Weibull/Poisson hazard with `Λ` accumulated (closed-form under warp),
    threshold `θ = -ln(U)` pre-drawn per part instance and serialized, separate per-ignition
    Bernoulli draws, data-driven stress multipliers that are **shown to the player**, family
    qualification from bench + regime-weighted flight units, failures adding qualification data, and
    a player-facing "mission confidence" number the design AI can optimize.

25. **"Per-part voxelized aero" (mod table) contradicted "per-part analytical forces"
    (architecture).**
    → **Changed**: §2.4.1 splits it — weld-time voxelization produces per-part exposed-area/shielding
    tables for 26 flow directions; the runtime tick is analytic and cheap. Cached by part-graph hash.

26. **Resource list unbounded ("propellant + structural materials").**
    → **Changed**: fixed v1 set of ten resources, and an explicit mechanism (`Composite` is
    import-only early) that *pulls* players into the colony game rather than bolting it on.

27. **No recurring costs** → funds were a one-way ratchet and the RTS layer had no pressure.
    → **Changed**: salaries + facility upkeep as a recurring drain, tuned so idling at warp is
    net-negative.

28. **Contracts had no data model and were re-rollable by reload.**
    → **Changed**: `ContractTemplate` with prerequisites/rewards/penalties, offer board seeded from
    `(save_seed, CONTRACTS, offer_index)`, completion evaluated by the sim on events rather than
    polled per tick.

29. **No balance tooling** for a design with three interacting currencies.
    → **Changed**: all constants in `data/balance.ron` + a headless 100-sim-year economy harness.

## D. Scope & roadmap feasibility

30. **M1 and M5 overlapped.** M1 already delivered "planet + moon on conics, SOI transitions"; M5 was
    titled "second body".
    → **Changed**: M1 is the full orbit/rails/event layer; M5 is explicitly *interplanetary*
    (maneuver nodes, Lambert/porkchop, encounter prediction).

31. **M2's point-mass flight model was thrown away by M4** — pure rework, and a point-mass rocket
    gives a false negative on the "am I having fun" gate.
    → **Changed**: M2 builds the final welded-body force model at lower fidelity; M4 raises fidelity.
    Fun gate moved to M4, vertical slice = M2+M3+M4.

32. **M3 (editor) secretly depended on M6/M7/M8.** Δv/TWR readouts and part costs need resources
    (was M7) and upgrade/wear resolution (M6/M8).
    → **Changed**: the `EffectivePart` resolver is an M3 deliverable with stubbed upgrade/wear inputs,
    so M6/M7/M9 wire into an existing seam instead of rewriting the editor.

33. **Reliability (M8) was listed before the usage tracking it needs**, and would amplify frustration
    before the flight model and economy are solid.
    → **Changed**: moved to M9, after modules and mission-length content exist. Rationale stated.

34. **Docking / multi-vessel was missing entirely**, despite "stations" being a pillar and joints
    being the one place the constraint solver runs.
    → **Changed**: new M6 (multi-vessel bubble, docking as the first jointed island, resource/crew
    transfer), with a 16-joint-per-bubble cap enforced by the editor.

35. **The mod-equivalent table was an unscoped wishlist** (CommNet, life support, KIS/KAS, Parallax,
    VT, marketplace) with no milestone — classic solo-dev death.
    → **Changed**: §4 assigns every row v1 / post-v1 / cut. Marketplace cut. CommNet post-v1 with the
    reason (it gates *control*, so it can break the scripting UX if half-done).

36. **"Built-in features implemented as mods against the public API — dogfooding" is a trap.** It
    forces a complete stable public API before you know what the game needs, and roughly doubles the
    cost of every core feature for a solo dev.
    → **Changed**: rejected explicitly. Core features are first-class Rust; the script API covers
    control/telemetry; the data formats are the modding surface; dogfooding only where it's cheap.

37. **Exit criteria were unmeasurable** ("map view", "parts break off").
    → **Changed**: numeric/testable criteria (1-tick SOI precision, <1e-6 orbit round-trip, momentum
    conservation to 1e-9, golden replay green, perf budgets).

38. **No cut list and no v1 definition** for a multi-year solo project.
    → **Changed**: §6 adds an ordered cut list (web tier → docking → life support → third body →
    Advanced render tier) and a minimum shippable v1 (home planet + moon + one neighbour).

39. **Missing non-feature work that always eats a quarter**: audio, input rebinding, settings,
    localization keys, save migration, tutorial/onboarding, CI matrix, test strategy beyond golden
    replays.
    → **Changed**: M10 "Ship v1" exists; §8 CI/test table added; `LocKey` in the data model.

40. **Unresolved gameplay question that blocks the warp design**: do unloaded vessels thrust under
    warp?
    → **Decision made** (defensible, KSP-consistent, and it keeps rails analytic): no background
    thrust in v1; warp is refused while any vessel has throttle. Low-thrust-on-rails is post-v1 with
    its own accuracy caveats.

## Things I left alone

- Every row of the settled-decisions table is unchanged. Consequences are now written down in §1.1.
- Pillars, fictional ~1/10 system, pluggable `OrbitModel`, welded bodies, desktop-first + wasm CI,
  classical-AI-first, and "scripting is a pillar not a mod" all survive review. They're the right calls.
- Crate names changed (`csp-ecs` → `csp-view`, plus `csp-core`/`csp-phys`/`csp-ui` split out) because
  the old names encoded the state-ownership ambiguity that caused finding #1.
