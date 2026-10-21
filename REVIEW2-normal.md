# PLAN.md Review — Round 2, adversarial pass

Scope: hunting for *things left in the air* — contradictions that only surface at implementation
time, numbers that don't survive arithmetic, and requirements with no owner. Round-1 findings
(REVIEW.md) are treated as settled and are not relitigated; several of them are, however,
*incompletely discharged* in the current text, and those are called out as new findings.

Also checked PLAN against the committed scaffold (`crates/`, `.github/workflows/ci.yml`,
`Cargo.toml`, `data/`) at `01083d4`.

Findings are numbered and ordered by severity. Each has a one-line fix/decision proposal.

---

## CRITICAL

**C1 — Docking-as-joint contradicts the station pillar and the 16-joint cap.**
§2.4: "Joints only where physically required: docking ports". §2.4 also caps v1 at **16 joints per
bubble**. §5.3 budgets a **1000-part station**; pillar 1 says "build … stations". A 17-module
station is therefore unbuildable, and a permanently-jointed station runs the constraint solver
forever — the exact KSP1 wobble failure the welded-body decision was taken to avoid.
→ *Fix*: docking is a two-phase operation — joint only during the capture transient (≤N ticks),
then the two part graphs **merge and re-weld into one body**; undock splits them. The 16-joint cap
then applies only to robotics/transients. State this in §2.4 and add a weld-merge test to M6.

**C2 — The inertia-tensor cache makes the save/load determinism test fail by construction.**
§2.4: inertia tensor "recomputed … at most every N ticks or on >0.5% mass delta". §7: welded bodies
and inertia tensors are **not serialized, re-derived on load**, and "re-derivation must be exact".
A threshold-and-interval cache is a *function of history*, not of current state: the live run holds
a stale tensor from the last trigger; the reloaded run derives a fresh exact one. The §7
save→load→10k-tick hash test diverges by design, and it will be misdiagnosed for a week.
→ *Fix*: make every derived cache a pure function of current serialized state — quantize resource
mass to a fixed integer grid (e.g. 1/1024 of capacity) and recompute when the *quantized* value
changes; delete the "every N ticks" trigger. Same rule applies to any other time-triggered cache.

**C3 — Automatic warp demotion is a wall-clock input to sim state.**
§2.6: physics warp "drops automatically if the frame budget is exceeded"; the warp controller
"measures and demotes". Warp level must live in sim state (rails advance moves the clock, and
§2.6 clamps warp on events). Therefore measured frame time → sim state → T1 replay is broken for
any session that ever demoted. §1.1's own rule ("never wall-clock in the sim") is violated by §2.6.
→ *Fix*: the warp controller lives in `csp-app` (outside the sim) and demotion is emitted as an
explicit `SetWarp` **command** into the tick-stamped queue/log. The sim never measures anything.
Add a CI assert: no sim field is written except from a command or a scheduled event.

**C4 — "Colonies integrate analytically" is only true for a model the plan doesn't have.**
§3.5 asserts `amount = rate * dt`, piecewise-constant between events, while simultaneously saying
rate depends on "local concentration, power, crew, and **thermal**". Concretely:
- *Power*: solar insolation varies continuously with sun angle and body rotation — not
  piecewise-constant. Battery SoC is a reservoir (fine, linear), but the source isn't.
- *Thermal*: radiator equilibrium is T⁴ — nonlinear, no closed form.
- *Converter chains*: drill → buffer → converter → buffer couples rates to levels. Only linear if
  every converter runs at a level-independent rate until a store crosses a bound.
- *Contested inputs*: two converters sharing a scarce feedstock need a deterministic allocation
  rule. None exists. Tie-breaks by `BTreeMap` order is an accident, not a design.
→ *Fix*: write the colony model as an explicitly **piecewise-linear flow network**: (a) insolation
is a two-state day/night step with events at terminator crossings plus an authored per-orbit
average; (b) colony thermal is solved **algebraically** to steady state per interval, never
integrated; (c) converters run at a rate determined by a fixed-priority, fixed-order allocation
pass over resources in `ResourceId` order, recomputed only at events; (d) next event =
`min over resources of level/(-net_rate) or (cap-level)/net_rate`. Then §3.5's claim is true.
Add this as a named subsection; it is the load-bearing math of the whole RTS layer.

**C5 — Aero voxel tables: §2.4.1 says serialize them, §7 says never serialize them.**
§2.4.1: "Voxelization result is cached by part-graph hash **and serialized with the craft**."
§7: "What is NOT serialized: … any derived cache (welded bodies, inertia tensors, **aero voxel
tables**, trajectory splines)." An implementer picks one and breaks the other test.
→ *Fix*: never serialize them in the *save*; optionally cache them in a separate, deletable,
content-addressed cache directory keyed by `(part_graph_hash, voxelizer_version)`. A craft file
stores the hash only. Fix §2.4.1's wording.

**C6 — No object-count or debris policy; the save and the sim grow without bound.**
100 sim-years of launches leaves every spent stage in orbit. The vessel/part arenas, `PartUsage`
per instance, and the event queue grow monotonically. §5.3's "map view, 500 on-rails objects" is a
*render* budget masquerading as a cap, and §9's "100 sim-year economy harness" would run into it.
There is no debris lifetime, no object limit, no recovery/decay rule, no "terminate vessel".
→ *Fix*: hard caps in `balance.ron` (e.g. 512 tracked vessels, 64 debris) + automatic rules:
debris below atmosphere decays on an event, debris outside SOI is deleted, suborbital debris is
removed on impact, and oldest-debris eviction with a notification when the cap is hit. Add a
"save size after 100 sim-years ≤ X MB" assertion to the economy harness.

**C7 — The home system's physics is undefined, yet M1 and all balance depend on it.**
§2.3 gives only radius (600 km) and SOI (8.4 Mm). Nowhere are: μ/surface g, rotation period,
atmosphere model and scale height, sea-level pressure, ocean presence, axial tilt, the number of
bodies, or their orbits. These fix orbital velocity, Δv budgets, engine stats, part costs, and
therefore the entire economy — they cannot be deferred past M1. Worse: `PartDef` is fully
specified in the plan while `SystemDef`/`BodyDef`/biome-map formats **are not specified at all**,
and `data/systems/home/` is empty.
→ *Fix*: add §3.0 "Home system reference sheet" with the full parameter set for 3 bodies, plus a
`SystemDef`/`BodyDef` RON schema and validator, as an **M1 deliverable with an exit criterion**
(e.g. "home LEO circular velocity = 2.3 km/s ±1%, verified by test").

---

## HIGH

**H8 — Rails warp is forbidden on "contact", so you can never warp while landed.**
§2.6: rails warp allowed only when "no vessel in bubble under thrust/**contact**/atmosphere". A
landed vessel is in contact and (on an atmospheric body) in atmosphere. So the player cannot warp
on the pad, cannot warp while a rover sits on a colony, cannot warp through a construction timer
at a surface base — i.e. the RTS layer is unreachable from the place you spend most of your time.
→ *Fix*: introduce **surface rails**: a settled vessel (velocity below a threshold for K ticks)
is latched to a body-fixed frame, leaves the bubble, and does not block warp. Unlatch on
command/contact-breaking event. This also fixes H13.

**H9 — The state hash — the entire T1 gate — is undefined.**
M0's exit criterion is "state-hash harness exists", but nothing says *what* is hashed, in what
order, how floats are canonicalized (`-0.0` vs `0.0`, NaN payloads), or that the hash is stable
across struct refactors. With `bincode` + `derive(Serialize)` the hash changes when a field is
reordered, so every golden hash reds on cosmetic refactors and the team learns to regenerate them
blindly — which silently voids the gate.
→ *Fix*: define a `StateHash` with an explicit, versioned field walk (not derive-order), f64
hashed as `to_bits()` after normalizing `-0.0 → 0.0`, a debug assert that no state float is
NaN/inf, and a `hash_schema_version` that must be bumped deliberately in the same commit that
regenerates goldens.

**H10 — Golden replays will be red after every balance tweak.**
§9 puts all constants in `data/balance.ron`; §7 hashes the data set; §8 runs golden replays on
every commit. Editing one number in `balance.ron` changes every replay hash. Nothing says the
replays use frozen fixture data.
→ *Fix*: golden replays load `data/test-fixtures/<scenario>/` — a frozen, versioned data set never
touched by balance work. Shipping data gets its own, cheaper "does it still validate + run 1k
ticks without NaN" gate.

**H11 — Arena key reuse undermines both iteration determinism and the anti-save-scum claim.**
§2.2: "slotmap-style" arenas, "iteration order is always the arena's dense index order". Dense
index order *changes when slots are removed* (last-element swap), so iteration order is a function
of the removal history — deterministic within one run, but only reproducible on load if the free
list / generation counters / dense ordering are serialized bit-exactly. §7 says "sim arenas" and
does not mention free lists or generations. Separately, §3.4 draws reliability θ from
`Rng::for_(RELIABILITY, part_instance_id, 0)`: if instance ids are reused, a new part inherits the
old part's failure threshold.
→ *Fix*: (a) `PartInstanceId`/`VesselId` carry a **monotonic u64 counter that is never reused**,
and the counter is serialized; (b) sim iteration is over a stable sorted key order, not the dense
order; (c) §7's serialization list explicitly names free lists, generations and id counters.

**H12 — The RNG derivation collapses the stream tuple into 64 bits (and the plan's arity is
inconsistent).**
Scaffold `csp-core::stream()` mixes `(save_seed, tag, entity_id, counter)` into one `u64` and calls
`ChaCha8Rng::seed_from_u64` — 64 of 256 seed bits, hand-rolled mixer, collisions correlated across
the whole key space. Also §2.7 specifies a 4-tuple API while §3.6 specifies contract generation as
the 3-tuple `(save_seed, CONTRACTS, offer_index)` — is `offer_index` the entity or the counter?
→ *Fix*: build a 32-byte seed by writing the four `u64`s little-endian into `[u8; 32]` and using
`ChaCha8Rng::from_seed`; make the API a single struct `StreamKey { tag, entity, counter }` so every
call site is 4-valued; document `CONTRACTS` as `entity = 0, counter = offer_index`.

**H13 — No contact model, and the immutable-dt rule forbids the usual escape hatch.**
§2.9 says "semi-implicit Euler **/** RK4" — that is two different integrators with a 4× difference
in force-loop cost; §2.9 never picks one, and RK4 with discontinuous contact forces is wrong
anyway. Landing gear holding a 20 t lander at 1/64 s is a stiff problem; the single most important
rule in the document (dt never changes) means contact substepping is illegal, so contact must be
*unconditionally stable at 1/64 s*. That is an impulse/LCP-style solver, not a penalty spring, and
the plan hasn't said so.
→ *Fix*: pin the integrator (semi-implicit Euler for the body, explicit in §2.9), specify contact
as a projected-Gauss-Seidel impulse solve with Baumgarte stabilization and a resting/settled latch
(H8), and add an M2 exit criterion: "a 20 t lander rests on gear for 10k ticks with ‖v‖ < 1e-6 and
no jitter in the state hash".

**H14 — ×4 physics warp, the 2 ms step budget, and 144 fps are jointly unsatisfiable.**
§5.3: 200-part vessel → 144 fps **and** sim step <2 ms. §2.6: physics warp ×4 = 256 ticks/s. At
144 fps that is 1.8 ticks/frame × 2 ms = 3.6 ms of a 6.9 ms frame, before any rendering — so the
warp controller will demote to ×2 essentially always, and "×4" is the same kind of advertising the
plan rightly killed for ×8. Note also the step budget is stated only for ×1.
→ *Fix*: state the budget as **per-tick cost at the target warp**: ≤0.5 ms/tick for a 200-part
vessel, which makes ×4 real at 144 fps; or honestly cap physics warp at ×2 and drop ×3/×4.

**H15 — The aero table's 26 directions are too coarse to produce usable stability derivatives, and
the grid is anisotropic.**
26 directions (cube faces+edges+corners) is 45° angular resolution. Pitch/yaw aero *derivatives*
— the thing that decides whether a rocket is stable — are obtained by differencing that table, so
they are quantized at 45°, and interpolation across a 45° cell will produce discontinuities that
make the autopilot and SAS oscillate. The interpolation scheme is also unspecified (nearest?
barycentric on a spherical triangulation of those 26 points?). Separately, "64³ over the vessel
**AABB**" gives 0.03 m × 0.03 m × 0.63 m voxels for a 2 m × 2 m × 40 m rocket — anisotropic in
exactly the axis that matters for frontal area. Finally, diagonal directions require either a
rotated re-voxelization (26 × 262 k solid-fill tests) or a traversal scheme; neither is specified,
and that is where the <500 ms budget lives or dies.
→ *Fix*: (a) **isotropic** voxels with a fixed edge length and a capped total count (KSP/FAR do
this), not 64³ over the AABB; (b) voxelize once, sweep rays in rotated frames (no re-voxelization),
and document the traversal; (c) raise the direction set to a geodesic sphere of ≥66 directions and
interpolate barycentrically over its triangulation; (d) add an M4 exit criterion comparing
table-derived `Cm_α` against a direct per-direction computation within a stated tolerance.

**H16 — Weld cost at docking and at breakup has no budget and no asynchrony story.**
§2.4 says the weld happens "on launch/scene-load". But docking (M6) creates a new body mid-flight
and breakup (M4) creates two — both re-weld, including (per C1's fix) a 1000-part station weld
whose voxelization budget (§5.3 covers only 300 parts at <500 ms) extrapolates to seconds. A
reentry breakup cascade triggers several welds within a few ticks. §5.3 also says weld runs "off
the sim thread" — impossible on `wasm32-unknown-unknown` (single-threaded, no atomics), where the
budget is nevertheless stated as <2 s.
→ *Fix*: split weld into (1) mass/inertia/force-sites — cheap, synchronous, must be exact; and
(2) aero voxelization — expensive, deferred, with the child bodies **inheriting the parent's aero
table** until the recompute lands (document the approximation). On wasm, voxelization runs
incrementally across frames; say so in §5.3 instead of "off the sim thread".

**H17 — `editor.build` has no `SimCommand` path, so "no interface has privileged access" is false.**
§2.12 lists `editor.build` as an MCP tool and asserts all mutation goes through `SimCommand`. But
§2.2's arenas are `VesselId`/`PartInstanceId`/`ColonyId`/`ContractId` — the in-editor craft is
`csp-ui` state, not sim state. Either the editor's working craft becomes sim state (big, and it
would put VAB drag operations in the deterministic command log), or MCP's editor tools bypass the
queue and the headline claim collapses. The same question applies to camera, map-view focus, and
"active vessel".
→ *Fix*: declare a third category — **session state** (editor working craft, camera, selection,
dashboards) — explicitly outside the sim and outside the command log, with its own small
`SessionCommand` channel that MCP and egui both use. Then say precisely which MCP tools are
session-level and that they are *not* replay-relevant. Also move "active vessel / control
authority" into sim state, since it gates control.

**H18 — No conflict model for concurrent player + agent control.**
Two command sources can set throttle on the same vessel in the same tick. Last-writer-wins by
arrival order is wall-clock dependent, which means the *live* session is nondeterministic even
though the *log* replays — a distinction the plan never draws, and one that will be reported as a
determinism bug.
→ *Fix*: one **control lease per vessel** (`Player | Script(id) | Agent(client_id)`), explicit
takeover command, player preempts unconditionally; conflicting commands from a non-leaseholder are
rejected with an error on the originating channel. Plus a stated line: "determinism is a property
of the command log, not of a live multi-client session."

**H19 — No rate limiting or resource budget on the MCP/WS command channel.**
Scripts get a 20 k-op/tick budget (§2.10); external agents get nothing. An agent in a loop can
enqueue unbounded commands per tick, inflating the log, the save, and the frame time.
→ *Fix*: per-client command budget per tick (e.g. 32), a bounded queue with explicit backpressure
errors, and a max message size; mirror the script budget's "part of deterministic state" property
by making the drop/defer decision a queue rule, not a timing accident.

**H20 — Telemetry WS has no subscription model, rate, encoding, backpressure, or bandwidth budget.**
§2.12.2 implies arbitrary field selection ("widget registry mapping type → component + telemetry
fields") against a 1000-part station. Per-part payload at 60 Hz is ~3.4 MB/s binary and ~30 MB/s as
JSON — fine on loopback, not fine to a tablet over WiFi, and catastrophic if the send is synchronous
in the frame loop (a slow client then stalls the sim). The plan also never says which thread the
server runs on or where it gets a snapshot from — §2.2's diagram is single-threaded.
→ *Fix*: specify (a) explicit per-client subscriptions with a field allowlist; (b) rates as
divisors of 64 Hz (default 16 Hz; per-part detail capped and opt-in); (c) binary framing
(MessagePack/CBOR) for arrays; (d) a double-buffered snapshot the server thread reads without
blocking the sim, drop-oldest on slow clients; (e) a §5.3 row: "telemetry ≤ 256 KB/s at 16 Hz for a
1000-part vessel".

**H21 — Dashboards in the save contradicts §7 and pollutes the command log.**
§2.12.2: dashboards "stored as JSON in the save (`dashboards` section)" and "persist via the game's
save API". §7's serialization list does not contain them. If they are in the save, dragging a
widget mutates sim-adjacent state, changes the save, and (if routed through `SimCommand` per
§2.12's own rule) enters the deterministic command log.
→ *Fix*: dashboards are per-profile config files (`config/dashboards/*.json`), never in the save,
never in the command log, excluded from the state hash. Delete the save-API wording.

**H22 — `EffectivePart` mixes build-time-frozen and live inputs; the frozen half isn't serialized.**
§3.1's resolver is `def ∘ upgrades ∘ scale ∘ variant ∘ wear`. §3.3 says upgrades apply at build
time and **never retroactively to craft in flight** — so `upgrades`/`scale`/`variant` are frozen
per part instance at manufacture, while `wear` is live. That is two functions, not one, and §7's
serialization list contains `PartUsage` but not the frozen build-time stat snapshot or the tech
levels at build time. An implementer delivers M3's "one function" and discovers in M7 that it
needs splitting — the exact rework M3 was resequenced to avoid.
→ *Fix*: define `BuiltPart { part_id, scale, variant, upgrade_levels_at_build }`, serialized per
part instance; `EffectivePart = resolve(BuiltPart) ∘ wear(PartUsage)`. Add `BuiltPart` to §7.

**H23 — The crew model doesn't exist, but four systems depend on it.**
§3.5 hires crew with funds and pays salaries; §3.2 gives researchers skill levels; §6/M6 does
"crew transfer"; §3.5 has life support and a deaths setting. Nothing says whether crew are named
persistent individuals with skill/XP and a location, or a fungible headcount per facility/vessel.
This decides UI, save size, the transfer command shape, and whether death is meaningful.
→ *Fix*: decide now. Recommend **named individuals** with `CrewId`, a `location: Facility | Vessel |
Colony`, a skill level, and no XP in v1 (XP is a post-v1 tuning knob). Add `CrewId` to §2.2's arena
list and crew roster to §7.

**H24 — Biomes have no format and conflict with procedural-first terrain.**
§3.2 keys science on `(body, biome, situation)` and §9 lists biomes as content, but there is no
biome map format, no authoring tool, no crate, and `data/` has no slot for them. §3.7 says terrain
is procedural-first — if biomes derive from the noise function, then any terrain tuning silently
invalidates every saved science key and every contract that references a biome.
→ *Fix*: biomes are **authored, versioned, low-resolution index maps** (equirectangular, ≤2048×1024,
palette-indexed PNG) in `data/systems/home/<body>/biomes.png` + a RON legend, explicitly decoupled
from terrain noise; the data-set hash covers them, and a changed biome map triggers §7's warning.

**H25 — No revert/quickload policy, and reliability's design makes it load-bearing.**
Pre-drawn failure thresholds mean reloading cannot reroll a failure (good), but that also means
KSP's "revert to launch" either doesn't exist or is pointless. Reverting is one of the top-three
quality-of-life features of this genre. The plan never mentions it.
→ *Fix*: state the policy explicitly. Recommend: revert-to-launch exists, restores the pre-launch
snapshot, and *keeps* pre-drawn thresholds (so the engine still fails, at the same Λ) — and say so
in the UI so players don't call it a bug.

**H26 — No system requirements / reference hardware, so §5.3 is untestable.**
"144 fps at 1080p" on which GPU? The nightly "fail on >10% regression" gate implies a fixed runner,
but GitHub-hosted runners have no usable GPU, so the render budgets cannot run in CI as described.
→ *Fix*: name a reference machine for each budget row (e.g. "RTX 3060 / Ryzen 5600, 1080p"), state
min/recommended specs for v1, and split §8's perf gate into (a) CPU-only sim/rails/weld budgets
that run on hosted CI, and (b) GPU budgets that run on a self-hosted runner or are measured
manually at milestone exits.

**H27 — No save-compatibility policy for the development period, so §7 taxes every commit.**
§7 mandates an explicit `migrate_vN_to_vN+1` chain and "a save that cannot be migrated is refused".
Taken literally from M0, that is years of migrations for a schema that changes weekly, written for
a user base of one.
→ *Fix*: declare a policy — "before v1.0, `save_version` bumps may invalidate saves; the loader
refuses with a clear message and no migration is written. Migration chains begin at the first
public build." Add it to §7 and to the M10 definition.

**H28 — Commercial intent is undecided, yet §3.7 and the workspace license already depend on it.**
§3.7 rejects CC-BY-NC "if the game is ever sold"; `Cargo.toml` already declares
`MIT OR Apache-2.0` for the whole workspace, which — if it covers the game, not just the engine
crates — permits anyone to sell your game. No LICENSE files exist at the repo root.
→ *Fix*: decide now (recommend: engine crates MIT/Apache-2.0, `data/` and the game binary
proprietary-but-source-available), add the LICENSE files, and set `license` per-crate rather than
workspace-wide. This also locks §3.7's asset rules instead of leaving them conditional.

**H29 — Colony failure states are undefined.**
What happens when a colony loses power, runs out of Food with crew aboard, or is struck by a
crashing vessel? Does production stop and resume, does hardware degrade, do crew die, can a colony
be abandoned or demolished to recover resources? (Note: there are no hostile entities in this
design, so "colony defense" is correctly a non-requirement — but *failure* is not.)
→ *Fix*: add a `ColonyState { Nominal, Degraded{reason}, Dormant, Abandoned }` with explicit
transition events (`PowerLost`, `LifeSupportDepleted`, `CrewLost`), a dormant state that consumes
and produces nothing (and so costs nothing under warp), and an explicit "abandon colony" command.

**H30 — M0's exit criteria are not met by the committed M0 scaffold, and two of them are violated.**
Against `01083d4`:
- `SimTime` is stored in a **`bevy_ecs` `Resource`** (`csp_view::Clock`) and advanced by a bevy
  system (`advance_clock` in the `sim_step` schedule). That is authoritative sim state living in the
  ECS — the precise thing §2.2 forbids and finding #1 of round 1 fixed.
- The frame loop runs `view.sim_ticks()` **before** `view.frame()`, and `frame()` runs
  `input` first — so `Input` executes *after* `SimStep`, inverting §2.5's stated order. egui is also
  run inline in `csp-app::render`, not in the `ui_build` schedule, reading `view.world` directly.
- No state-hash harness exists (an M0 exit criterion).
- The dependency-rule lint is a vacuous grep: `cargo tree -p csp-sim 2>/dev/null | grep -E "… rand …"`
  — `2>/dev/null` means a failed `cargo tree` passes silently; the pattern `"rand "` does not match
  `rand_chacha`, which `csp-core` **does** pull in, putting `rand_chacha` in `csp-sim`'s future tree
  and outside §2.1's allowed dependency list. Nothing checks `HashMap`, `std::time`, `std`
  transcendentals, or "only `csp-view` depends on both".
→ *Fix*: move the clock into `csp-sim` (`csp_sim::World` owning `SimTime`), reorder the frame loop,
move egui into `ui_build`, write the state-hash harness, and replace the grep with `cargo-deny`
bans + a `clippy.toml` `disallowed-methods`/`disallowed-types` list, failing loudly on tool error.

---

## MEDIUM

**M31 — Subsystem query order for `advance_to` is unspecified, and `event_seq` isn't in §7.**
§2.6 step 1 says "query every subsystem for its next scheduled event time"; §2.6.1 makes ordering
total via `(tick, event_seq)`. But `event_seq` is assigned at insertion, so it depends on the order
subsystems are queried/stepped — which the plan never enumerates. `event_seq` itself is absent from
§7's serialization list.
→ *Fix*: enumerate the subsystem order as a constant array in `csp-sim` with a comment that
reordering it is a save-breaking change; add `event_seq` to §7.

**M32 — Input is frame-rate dependent unless commands are absolute.**
At 144 fps, ~55% of frames produce zero ticks; at 20 fps a frame is 3.2 ticks. If a held key emits
`AdjustThrottle(+Δ)` once per frame, throttle rate scales with fps; egui sliders/drags emit
per-frame commands likewise. The log replays faithfully, but gameplay differs by machine — and
that *will* be reported as a determinism bug.
→ *Fix*: all control commands are **absolute** (`SetThrottle(f64)`, `SetPitchInput(f64)`); any
rate-based input is integrated against `ticks_consumed`, in `csp-app`, before becoming a command.
Add a §2.5 rule and a lint-by-review note on the `SimCommand` enum.

**M33 — "Warp refused while any vessel has nonzero throttle" creates an unrecoverable stuck state.**
A decoupled debris stage left with throttle set, or a bugged script, blocks all warp forever with
no diagnosis path.
→ *Fix*: throttle is forced to zero when a vessel leaves the bubble; the warp-refusal UI names the
offending vessel and offers "zero its throttle".

**M34 — The physics bubble radius is never stated.**
§2.3 gives the recentring threshold (1 km) but not the bubble's extent, i.e. when a vessel goes
on-rails. This is load-bearing for M6 (two vessels in one bubble), debris, landed vessels, and
the "500 on-rails objects" budget.
→ *Fix*: state it (e.g. 20 km, in `balance.ron`) along with the on-rails/off-rails hysteresis.

**M35 — Runtime joint creation can exceed the 16-joint cap.**
§2.4 enforces the cap in the *editor*, but joints also arise from "decoupler failure cases" at
runtime. Behaviour when the runtime cap is exceeded is undefined.
→ *Fix*: runtime joint creation beyond the cap degrades to an immediate clean separation (no joint)
and logs; the editor cap stays as a design-time guard.

**M36 — Lab conversion is not linear, and its exhaustion event is asymptotic.**
§3.2's `min(1, data_available/throughput)` makes the rate proportional to `data_available` when
data is scarce — an exponential decay, not a constant. §3.2 nonetheless says "integrated in closed
form" without acknowledging it, and "data exhausted" never occurs exactly.
→ *Fix*: say the closed form is exponential in the scarce regime, and define exhaustion as crossing
an authored epsilon (e.g. 1e-6 data units) so the event has a finite time. Also normalize the
rate's units to per-second like the rest of the sim.

**M37 — "Mission confidence" requires a planned-mission object that doesn't exist.**
§3.4's player-facing number is `exp(-Λ_expected_for_planned_mission)`, and the design AI optimizes
against it. There is no mission/flight-plan data model anywhere in §3.
→ *Fix*: define a minimal `MissionProfile { burn_segments: Vec<(duration_s, throttle, regime)> }`
produced by the design assistant (§2.11) and editable in the editor; confidence is a pure function
of it. Assign it to M3 (stub) / M9 (real).

**M38 — Reputation is simultaneously a contract prerequisite and an undecided feature.**
§3.6 lists reputation in prerequisites *and* rewards; §10 says it may be folded into contract
availability. An implementer building §3.6 must invent it.
→ *Fix*: decide in §3.6 (recommend: no reputation currency; prerequisites use concrete facts —
contracts completed by type, bodies visited, failures in the last N — and "reputation" is purely a
displayed aggregate).

**M39 — No bankruptcy / failure state for the economy.**
Salaries and upkeep drain continuously (§3.5) and warp is clamped by events (§2.6), so a player
warping to a transfer window can be drained to negative funds. There is no defined behaviour.
→ *Fix*: funds clamp at zero; unpaid salaries cause staff attrition and facility dormancy via
events; a "insolvent" state that auto-demotes warp and notifies.

**M40 — Logistics: how colony output reaches the launch-site inventory is unspecified.**
§3.5 says manufacture consumes resources "from the launch-site inventory" and `Composite` is
import-only early, pulling you into the colony game — but the return path ("you fly it") has no
mechanics: cargo capacity, resource transfer on recovery, crediting recovered cargo to the launch
site, or separate inventories per launch site.
→ *Fix*: one sentence per rule — recovery credits a vessel's resource contents to the recovery
site's inventory at a stated efficiency; each launch site has its own inventory; a `Cargo` module
is just a `Tank`. Assign to M8.

**M41 — The demo/web-tier boundary, and save portability, are undefined.**
§6's cut list says "web tier → playable demo only"; M10 says "web tier playable at reduced scope".
Which is it, what content is in a demo, and can a save move between web and desktop (different
datasets per §1.1, so probably not)? Also: `csp-server` is native-only, so the web build has **no
telemetry dashboards** — never stated.
→ *Fix*: define the web tier as a **fixed demo scenario** (one body, capped part count, no save
import/export, no dashboards) and say so in §1.1 and M10, or cut it from v1 per §6 and stop paying
for it in CI beyond `cargo check`.

**M42 — Localization exists as a type and nothing else.**
`LocKey` appears in `PartDef`, but there is no catalog format, no locale selection, no plural/RTL
story, no CJK font-atlas plan for egui (a real binary-size and memory cost, especially on wasm),
and no localization line item in M10.
→ *Fix*: declare "v1 is English-only; `LocKey` is indirection to a single `data/loc/en.ron`;
translation and non-Latin fonts are post-v1". One sentence, removes an invisible quarter of work.

**M43 — The M4 fun gate is judged on placeholder art, which is the same false-negative mistake the
plan corrects elsewhere.**
§6 moved the fun gate from M2 to M4 precisely because a point-mass rocket gives a false negative.
§3.7 then puts the first real art pass at M10 — so the gate is run on grey primitives, with no
defined quality bar for "generated placeholder art", no audio (M10), and no VFX budget.
→ *Fix*: define a minimum presentation bar as part of M4's exit criteria (silhouette-correct
kitbashed parts, engine plume, reentry plasma, basic SFX) and state that art *fidelity* is not
judged at the gate — only legibility and feel.

**M44 — RON debug dump must never be a load path.**
§7 offers "a RON debug-dump mode" alongside the authoritative bincode save. RON cannot portably
represent NaN/inf, and nothing says the dump is one-way.
→ *Fix*: state that the RON dump is write-only/diagnostic, and that the only load path is bincode.
(Authored RON *input* data is fine — `f64::from_str` is correctly rounded.)

**M45 — Bench/eval agent loop: lockstep vs free-running is undefined, and "reproducible" oversells.**
§2.12.1 calls deterministic replays "the grading ground truth" and says "a failing run is
reproducible down to the command log". The *sim* is reproducible; the *agent* is not, because its
commands depend on when it happened to read telemetry. Whether `csp-bench` blocks the sim until the
agent yields is unspecified, and it changes every score.
→ *Fix*: `csp-bench` runs **lockstep**: the sim does not advance until every registered agent has
returned for the current tick (with a tick-count timeout counted as a no-op). State that only
logged runs are byte-reproducible; agent runs are reproducible only as logs.

**M46 — The `csp-server` auth model is thin for a browser-reachable port.**
"Read-only telemetry may be unauthenticated on localhost" means any web page the player visits can
`fetch`/`WebSocket` to `127.0.0.1:<port>` and read their game state (and enumerate ports). No
`Origin`/`Host` allowlist, no TLS for LAN (token in cleartext), no token distribution/rotation
story for a local MCP client, no port/instance discovery for two running games.
→ *Fix*: require an `Origin`/`Host` allowlist on all endpoints including read-only; put the token in
`<save_dir>/server-token` (0600) and document reading it; LAN binds require an explicit CLI flag and
print a warning; rotate on save creation; a fixed default port with `--port` and a lockfile for
multi-instance.

**M47 — T2's "software transcendentals everywhere" is unreachable while using parry/rapier.**
§2.7's T2 row requires software transcendentals everywhere, but `parry3d-f64` and `rapier3d-f64`
call `std` float math internally; the `libm`-only clippy lint cannot reach them. T1 is unaffected
(same binary, same platform), so this is only a mislabelled promise — but the nightly divergence
report will show drift and nobody will know whether it's expected.
→ *Fix*: add a sentence: "T2 additionally requires a `libm`-only fork or a float-math-free collision
path; until then the nightly divergence report is expected to show contact-scenario drift and is
informational for non-contact scenarios only."

**M48 — The physics cross-check gate compares backends that cover disjoint cases, with no tolerance.**
§8 runs `SimpleBackend` vs `RapierBackend` "within tolerance" — but §2.9 scopes `SimpleBackend` to
single welded bodies and Rapier to jointed islands, so the only overlap is single-body-with-contact,
where a hand-rolled integrator and Rapier will diverge rapidly. "Tolerance" is undefined.
→ *Fix*: scope the oracle test to short (≤2 s), contact-free, analytically-checkable scenarios
(free fall, drag terminal velocity, circular orbit) with stated numeric tolerances, and drop the
implication that it validates contact.

**M49 — "Active vessel" / control focus ownership is undefined, and the dual UI needs it.**
Both the egui client and any number of web dashboards and agents need to agree on which vessel is
being controlled; bubble membership is sim state but "active" isn't defined anywhere.
→ *Fix*: `active_vessel: Option<VesselId>` is sim state, changed only by command (pairs with H18's
control lease); dashboards display it and may change it if token-gated.

**M50 — No workspace dependency/lint table despite the plan saying there is one.**
§2.1 annotates `Cargo.toml` as "workspace; shared lint + dep policy". The real file has neither
`[workspace.dependencies]` nor `[workspace.lints]`; `wgpu = "30"` is already duplicated in two
crates, and the plan's own text references wgpu 28.
→ *Fix*: add `[workspace.dependencies]` + `[workspace.lints.clippy]` (incl. `disallowed_methods`)
now, while there are 8 crates and not 14; reconcile the wgpu version reference in §5.1.

---

## LOW

**L51 — §2.6's ×10⁷ arithmetic is wrong.** 10⁷ sim-s/wall-s at 60 fps is ~1.9 sim-days *per frame*
(~116 days per wall-second), not "166 sim-days" per frame. The conclusion is right; fix the number
so nobody re-derives the cap from a bad premise.

**L52 — Telemetry at 60 Hz against a 64 Hz tick** means duplicated/dropped samples forever.
→ Use divisors of 64 (64/32/16/8 Hz).

**L53 — "At high warp, interpolation is disabled" is vacuous.** High warp is rails warp, where
there is no bubble to interpolate. The real case is physics warp ×2–×4, where interpolation *is*
wanted. Reword.

**L54 — §2.12's MCP milestone is "M9/post"** — ambiguous in a table whose purpose is to remove
ambiguity. Pick M9 (stdio, for `csp-bench`) and post-v1 (HTTP+SSE attach), or both in M9.

**L55 — The `ATTRIBUTION.md` CI check described in §3.7 does not exist** in `ci.yml`, and `data/`
is empty. Harmless now; add the check with the first asset so it can't be forgotten.

**L56 — Crash reporting / bug-report flow is implied but not scoped.** §7 says replays are used for
bug reports; M10 has no "attach replay to a bug report" or crash handler line item.

**L57 — Audio asset sourcing is absent from §3.7**, which covers only art. M10 ships audio.
→ Add a one-line policy (CC0 freesound/sonniss packs, attribution in the same file).

**L58 — `data/` has no slot for `balance.ron`, `loc/`, `contracts/`, or biome maps** even though
§3.7/§9/§3.2/§3.6 require them. Create the directories with the schema stubs so the validator has
something to validate.

**L59 — The RNG API shape in the scaffold (`stream()`/`uniform()` free functions) differs from the
plan's `Rng::for_(...)`.** Cosmetic, but the plan is quoted in doc comments; keep them in sync or
the doc comments become lies.

---

## What I could not break — load-bearing decisions that must not regress

These survived deliberate attack. Treat changes to any of them as needing a written ADR.

1. **Authoritative state outside the ECS, in plain arenas, mutated only by an ordered step
   function.** Every determinism problem I found is a *leak* across this boundary (C3, H30), never a
   flaw in the boundary itself. It is the single most valuable decision in the document.
2. **One immutable physics dt of 1/64 s, forever.** It is the constraint that generated the rails
   model, the event queue, and the honest warp table. It also *forces* good answers elsewhere
   (H13's unconditionally stable contact solver). Do not add substepping "just for contact".
3. **Tick-stamped `SimCommand` queue as the single mutation path.** Replay, undo, MCP, scripts, and
   the LLM all fall out of it. H17's `editor.build` hole is a gap in *coverage*, not evidence
   against the design — the fix is to name a session-state category, not to weaken the queue.
4. **Rails advance as an event-driven jump with a mandatory "publish your next event time" rule.**
   This is what makes the RTS layer tractable at all. C4 is a demand that the colony model actually
   obey the rule, not an argument against it.
5. **Three explicit determinism tiers with T3 disclaimed.** Refusing to promise cross-version
   determinism is the thing that lets derived caches be re-derived and backends be swapped.
6. **Derived caches are never serialized.** C2 and C5 are violations of this rule by other parts of
   the plan; the rule is right and should win both arguments.
7. **`SimpleBackend` (own integrator + parry) as default, Rapier confined to joints.** Removing the
   largest third-party determinism dependency from the hot path is correct, and the Jolt rejection
   reasoning holds.
8. **Weld-time-expensive / runtime-cheap aero split.** H15 attacks the *parameters* (26 directions,
   64³ AABB) and never the structure. The structure is right.
9. **Two declared render tiers with a "both must be correct" rule and per-tier screenshot tests.**
   The honest alternative to "degrades gracefully", and it makes the web cut cheap.
10. **One guest language (Rhai) with a per-tick op budget that is part of deterministic state, and
    autopilots in Rust.** Resists every variation I tried to break it with.
11. **Pre-drawn, serialized reliability thresholds (`θ = -ln U`) with Λ integrated in closed form.**
    Simultaneously warp-safe and save-scum-proof. H11/H12 are about the *key derivation*, not the
    model; H25 is about telling the player.
12. **Stable string `PartId`s, multiplicative upgrade tracks, no upgrade-as-new-PartDef.** The
    combinatorics argument is airtight and the migration rule follows from it.
13. **Data-set hashing in the save with a "what changed" warning.** Cheap, and it converts a class
    of support nightmares into a dialog.
14. **Milestone resequencing and the ordered cut list, with the fun gate at M4.** The reasoning
    (M2's force model is the final one; reliability last) is sound. M43 only asks that the *gate* be
    given a presentation floor.
15. **Rejecting "core features implemented as mods" and cutting the mod marketplace.** Correct
    solo-dev scope discipline; the data formats as the modding surface is the right trade.
