# Doc verification — docs/DATA-AND-SCRIPTING.md vs working tree

Verifier: independent (session-full-20261026). Every factual claim in the doc was
checked against the tree (`data/`, `crates/csp-data|script|ai|server|sim|view|ui|bench`,
`csp-app`, `xtask`, `apps/telemetry`, `.github/workflows/ci.yml`,
`.review/session-prd1/STATUS.md`). No files were edited outside this report.

## Summary

- **Verdict: PASS-WITH-FIXES** (overall doc rating: MINOR-INACCURATE)
- Claim clusters checked: **95** · verified **91** · inaccurate **1** ·
  minor-inaccurate **3** (nits folded in below)
- The doc is exceptionally faithful. Notably: the tech-tree count (4 nodes + 5
  tracks — landmine (a)), the 17/25 ControlApi split (landmine (b)), the `--serve`
  model (c), the flight-launch golden failure (d), the authoring recipes (e), and
  scripting reachability (f) all verify exactly. The §7 table's nine
  `ui-unreachable:` opt-out line numbers (L552, 564, 631, 644, 674, 692, 778,
  939, 997) match `csp-sim/src/lib.rs` byte-for-byte.
- Worst finding: §2's claim that `gen_biome_maps --check` runs **in CI** — it
  does not; `ci.yml` never invokes it (wrong substance, one claim).
- Completeness: a new content/automation dev can work from this doc + siblings;
  8 gaps listed at the end (craft/system/scenario authoring, MCP resources, …).

## Claims table

| # | Doc claim (§) | Anchor checked | Verdict |
|---|---|---|---|
| 1 | 16 part files; `csp.chute.s125.ron` / `csp.engine.sparc.ron` exist (§1.1) | `ls data/parts/` | VERIFIED |
| 2 | 10 resources; 3 recipes; 1 experiment; 6 contracts; 5 eval scenarios (§1.1) | `ls data/{recipes,experiments,contracts,eval-scenarios}`; `data/resources.ron` (10 ids) | VERIFIED |
| 3 | techtree `home.ron` = **4 nodes + 5 tracks**; costs 0/15/30/60; "review map's 9 counts both" (§1.1, §1.3) | `data/techtree/home.ron` — 4 `TechNode`s, 5 `UpgradeTrack`s, costs exactly 0/15/30/60; 4+5=9 | VERIFIED (landmine a) |
| 4 | Schema versions: parts v1, sites **v3 with band [2,3]**, colonies v2-only, balance **v11**, others v1 (§1.1–1.2) | `part.rs:28`, `site.rs:38,45` (`[VERSION-1, VERSION]` = [2,3]), `colony.rs:34`, `balance.rs:55`, `load.rs` acceptance lists | VERIFIED — note: `load.rs:120`'s own prose comment ("v1 and v2") is stale in *code*; the doc correctly reports the const |
| 5 | `schema_version` first field; version probed pre-parse (`read_versioned_ron` + `SchemaProbe`) (§1.2) | `load.rs:327–397` | VERIFIED |
| 6 | id = file stem for registries; missing registry dir loads empty; required files error Io (§1.1–1.2) | validator "id↔stem" rule (`validate.rs` parts sweep); `load.rs` loaders | VERIFIED |
| 7 | Every struct `#[serde(deny_unknown_fields)]` (§1.2) | 63 occurrences across csp-data structs | VERIFIED |
| 8 | Path-traversal guard: tame names, symlink refusal (`join_authored`, `UnsafeName`/`Symlink`) (§1.2) | `load.rs:139–197, 389–397` | VERIFIED |
| 9 | LocKey indirection + `validate_*_loc_keys` family (§1.2) | 7 sweeps: site/recipe/colony/part/experiment/techtree/contract loc-keys | VERIFIED |
| 10 | Determinism lints in csp-data via crate `clippy.toml` (§1.2) | `crates/csp-data/clippy.toml` (libm-only disallowed-methods) | VERIFIED |
| 11 | PartDef field order & full walkthrough incl. `+z` stack axis (§1.3) | `part.rs::PartDef` (805–854) + module doc | VERIFIED |
| 12 | `ScaleRule` shapes; exponents bounded [0,4]; Discrete parts may not carry Tank/Engine (§1.3) | `part.rs:108–157`; `validate.rs:3201–3210` rejection | VERIFIED |
| 13 | Module table: all 11 `ModuleDef` variants + exact key fields (Engine…Parachute) (§1.3) | `part.rs:504–652` — every field name matches | VERIFIED |
| 14 | Validator: category↔module, `isp_sl ≤ isp_vac`, propellant fractions sum 1, parachute deployed > packed, thermal conduction ≤ capacity (§1.5) | `validate.rs:3022, 3076+, 934, part.rs` docs | VERIFIED |
| 15 | ReliabilityDef: stress kinds (6), failure modes (8, Explosion only fatal) (§1.3) | `part.rs:688–771` | VERIFIED |
| 16 | "Every part ships a unit `stock` variant" (§1.3) — stated as convention | all 16 part files carry a `stock` variant; validator only requires non-empty/unique/positive (`part.rs:314–321`) | VERIFIED |
| 17 | `ELECTRIC_CHARGE` pinned in code; EC mass 0 (§1.3) | `resource.rs:30`; `data/resources.ron` | VERIFIED |
| 18 | Experiments: `transmit_ratio` band 0.2–1.0; caps per (body, SituationMask); ledger keys (exp, body, biome, situation) (§1.3) | `validate.rs:3540`; `exp.materials_study.ron`; `research.rs::DataKey` | VERIFIED |
| 19 | System/Body walkthrough: parent/SOI/period `PERIOD_REL_TOL 1e-9`, tidally-locked Rask, atmosphere `p0·exp(−h/H)` + temp table, biome PNG ≤2048×1024 (§1.3) | `system.rs` (67–98); `validate.rs:59`; `rask/body.ron`; PNGs are real 64×32 8-bit colormap; solen has none | VERIFIED |
| 20 | sites.ron v3: one launch site per body, `terrin-launch` starting_stock metal 2000 / composite 400 (§1.3) | `validate.rs:2096`; `data/systems/home/sites.ron` | VERIFIED |
| 21 | colonies.ron: module set (Solar/Generator/Drill/Converter/Store/Battery/Radiator/LifeSupport), RTG = `fuel_rate_per_s: 0`, Rask outpost values (40 m²/0.20, battery 5000, ore drill, smelt_ore, ore/metal stores) (§1.3) | `colony.rs`; `data/systems/home/colonies.ron` | VERIFIED |
| 22 | Contracts: 5 kinds, fact-based prereqs (TechResearched/ContractsCompleted/BodyVisited/CrewLostAtMost), 6 templates, crew-transport/tourist deferred in module doc (§1.3, §8.6) | `contract.rs:63–75, 340–355, 13–34`; 6 files in `data/contracts/` | VERIFIED |
| 23 | balance.ron ~45 tunables incl. `script_op_budget: 20_000`, bubble 20 km, events/frame 2000, recovery 0.9, salary/hire/manufacture (§1.3) | `BalanceDef` = 46 pub fields; all named values present in `data/balance.ron` | VERIFIED |
| 24 | Pipeline: loaders → `validate_dataset` → `View::from_source` installs (PatchedConics, RhaiHost, AscentAutopilot, op budget); any validation error refuses startup (§1.4) | `validate.rs:4305`; `csp-view/src/lib.rs:1568–1797` (`ViewError::Validation`) | VERIFIED |
| 25 | "LoadedDataset (11 file kinds)" (§1.4) | type is `validate::Dataset` (`validate.rs:4276`) with exactly 11 fields — **no `LoadedDataset` symbol exists** | **MINOR-INACCURATE** (name drift; count right) |
| 26 | `data_root()`: CWD first, then repo-relative from crate manifest; `ron_sort_key` byte-wise order (§1.4) | `csp-view/src/lib.rs:4055–4065`; `load.rs:321` | VERIFIED |
| 27 | datahash: BLAKE3 over `path‖0x00‖u64-LE len‖bytes`, ascending; excludes `test-fixtures/**` and art-ext files **only under `data/art/`** (biome PNGs hashed); `DATA_HASH_VERSION = 2` (v1 framing non-injective) (§1.6) | `datahash.rs:104–214`; `lib.rs:53` | VERIFIED |
| 28 | Hash consumers: save header `data_set_hash`, frozen into `BuiltPart`, fixture_hash mirror (§1.6) | `save.rs:124–137`; `tech.rs:102–128`; `replay.rs` | VERIFIED |
| 29 | validate.rs ~6.6k lines; sweep list; csp-data test files (8 named) run same sweep (§1.5) | 6580 lines; all `validate_*` fns; `crates/csp-data/tests/` matches the 8 names | VERIFIED |
| 30 | Authoring recipes: add-a-part steps (id=stem, family/tracks, category↔modules, resource refs, loc keys `part.<id>.name` / `part.variant.<vid>`, `tech_requirement`, stockpile payability, wasm demo parity 16 parts) (§1.7) | loc key spellings present in `data/loc/en.ron`; `apps/demo/data/parts` = 16; validator rules as above | VERIFIED (landmine e) |
| 31 | bless-goldens refusals: dirty tree, orphan goldens, partial artifact sets, hash change w/o `HASH_SCHEMA_VERSION` bump unless `--reason` (§2.1) | `xtask/src/main.rs:57–139` | VERIFIED |
| 32 | Golden artifacts `{fingerprint, save_seed, data_fixture_hash, snapshot, final_tick, command log}` bincode; sidecar names rustc/triple/parry/rapier/libm/rand_chacha/blake3/rhai/HASH_SCHEMA/KDF/DATA_HASH/fixture hash; `csp-replay verify` exit 1 = hash, 2 = harness (§2.2) | `replay.rs:41–83`; `csp-replay.rs:29–42` | VERIFIED |
| 33 | Known issue: flight-launch golden fails at **bincode decode** vs uncommitted W2.6 churn, passes at HEAD, noted in `.review/session-prd1/STATUS.md` (§2.2) | STATUS.md:22 states exactly this | VERIFIED (landmine d) |
| 34 | **"`gen_biome_maps --check` in CI"** proves committed PNGs match generator (§2 table) | `--check` mode exists (`gen_biome_maps.rs:267–298`) but **ci.yml never runs it** (steps: fmt/clippy/test/wasm-check/lint-deps+attribution+reach/cargo-deny/goldens/perf); no test invokes it | **INACCURATE** — the claimed CI gate does not exist |
| 35 | bake_art: `.csar` packs, procedural parts (`xtask/src/bin/parts`), excluded from data hash (§2.4) | `data/art/{parts,terrain}.csar`; `xtask/src/bin/parts/` exists | VERIFIED |
| 36 | Script lifecycle: `LoadScript` at `csp-sim/src/lib.rs:1106`; prepare gate (bare wait/until compile error, `on_tick` arity 1, compile fault → `Recoverable`) (§3.1) | `lib.rs:1106`; `csp-script lib.rs:360, 1097–1119`; `csp-sim lib.rs:7983–7997` | VERIFIED |
| 37 | Tick model: inside `step_bubble_tick` **before force integration**, ascending ScriptId, autopilots after; atomic effects (fault drains nothing) (§3.1) | `csp-sim lib.rs:7775–7812` (`control_tick`); host Err-path returns without draining | VERIFIED |
| 38 | Off-bubble scripts run only on `ScriptWake`; in-bubble wakes vacuous; strike disable = 5 consecutive, visible `disabled_reason`; engine-less world stores scripts (§3.1) | `scripts.rs:30–63, 98`; `lib.rs:2396` | VERIFIED |
| 39 | `ctx` map field list incl. `rot {w,x,y,z}`, `resources`, `parts`, `node`/`node_tick`; missing → NaN (§3.2) | `telemetry_to_map` (`csp-script lib.rs:738–822`) — every key present | VERIFIED |
| 40 | ControlApi **17 trait methods**; guest surface **25 entries**; frozen by `tests/api_freeze.rs` reading trait source (§3.3) | trait at `csp-script lib.rs:89–136` = 17 methods; `CONTROL_API_SURFACE` = 25 (const + `assert_eq!(…,25)`); `api_freeze.rs` exists with both freeze tests | VERIFIED (landmine b, part 1) |
| 41 | Guest verb semantics: steer→+Z align/zero=hold; `steer_quat(w,x,y,z)` scalar-first; stage/jettison→JettisonPart; add_node(tick,pro,norm,rad)→SetManeuver; transfer same-bubble; add_alarm absolute; deploy; engage/disengage; wait_* registration; cancel_wait (§3.3) | `bind_fns` (828–1064) matches row-for-row; quat order trap NOT misstated — doc and code agree scalar-first | VERIFIED (landmine b, part 2) |
| 42 | Guest determinism: pinned Rhai features, no `BasicTimePackage`, explicit libm overrides (`register_libm_math`) (§3.3) | `csp-script lib.rs:22–34, 637–735` + `no_wall_clock_reaches_the_guest` test | VERIFIED |
| 43 | Wake conditions: 7 variants; closed-form only; refusals (open conic, target outside [r_p, r_a), circular conic, no SOI event) at registration; `ScriptWake` class `Clamping` (§3.4) | `scripts.rs:304–330, 288` | VERIFIED |
| 44 | Budgets: op budget 20 000 from balance via `set_script_op_budget`; memory ceilings 1 MiB / 10 000 / 10 000; `BudgetExceeded` classified structurally; poll = aborted or ≥9/10 budget; 10 heavy ticks → `poll_demoted` + `rails_warp_refusal` (§3.5, §8.5) | `balance.ron:81`; `MAX_SCRIPT_*` (611–615); `POLL_OPS_NUM/DEN = 9/10`, `POLL_DEMOTE_TICKS = 10`, `STRIKE_DISABLE_LIMIT = 5` (`scripts.rs:98–108`) | VERIFIED |
| 45 | Persistence: `PersistValue` primitives only, non-finite/rich refuse, `persist` in save + state hash (`hash_scripts`), AST scratch recompiles (§3.6) | `dynamic_to_persist` (550–565); `hash_scripts` (`scripts.rs:600`, called `lib.rs:8880`) | VERIFIED |
| 46 | Four stock scripts; two quoted (§3.7) | `STOCK_SCRIPTS` = 4 (`program.rs:770–834`); "telemetry logger" quoted verbatim; **"circularize at apoapsis" quoted abridged** (drops the two `print` branches) without saying so | **MINOR-INACCURATE** (cosmetic) |
| 47 | script-ascent eval scenario: embedded script, goals over telemetry paths, tick/event budgets, stop_when (§3.7, §5.8 ref) | `data/eval-scenarios/script-ascent.ron` | VERIFIED |
| 48 | Scripting reachability: AUTOMATION section = stock-script row, paste-in buffer binding focused vessel, pause/resume/unload + strike/wake status, log tail; notices in csp-app; `ProgramCommand::LoadScript`→`SimCommand` in `csp-view` (§3.8, §7) | `program.rs:1602–1834` (`automation_section`, `sect("AUTOMATION")`); `csp-app lib.rs:1347–1350`; `csp-view lib.rs:1236` | VERIFIED (landmine f) |
| 49 | csp-ai: **222 lines**, one program, phases 0–4 with 1.5 km/60 km/12 s/overshoot constants, `IoControlApi` plumbing, bubble-only engage, three issuers (§4) | `csp-ai/src/lib.rs` (222 lines, constants at 39–54) | VERIFIED |
| 50 | No landing/rendezvous/design-assistant/LlmAdapter; transfer machinery exists in `transfer.rs` + map planner (§4) | `transfer.rs` (2556 lines, Lambert/porkchop); `map.rs:769+` alarm book/AlarmEntry | VERIFIED |
| 51 | Hosts: `LocalGame` @ backend.rs:633, `GameHost` @ :826, `serve_cli` @ csp-app lib.rs:2972, csp-mcp flags `--empty/--fixture/--save-dir/--dashboard-dir/--client` (§5.1) | all anchors exact; csp-mcp uses LocalGame; `--empty` is the *default* (doc lists it as an option without saying default — nit) | VERIFIED (nit: `--empty` default unstated) |
| 52 | `--serve` = fresh headless `View::new()` world, engines installed, **no starter craft**, shared saves dir, SPA served; windowed app hosts no server; backend.rs sketches app attach as M9d (§5.1, §8.2) | `serve_cli` (2965–3001): `View::new()`, installs RhaiHost+AscentAutopilot, no `seed_starter_craft` (only app path :412); only serve_cli touches csp_server; `backend.rs:820–825` M9d sketch | VERIFIED (landmine c) |
| 53 | HTTP endpoint table incl. `/mcp` POST-only (GET/DELETE → 405), token-free `/` + `/assets` (§5.2) | `http.rs:198–210, 672–673` | VERIFIED |
| 54 | Auth: 32-byte CSPRNG hex token, `<pid>.token` 0600/0700 atomic, `<token>\n<port>\n`, removed on shutdown, symlink refused, `?token=` ws-only, port 47900, 64 KiB cap, ≤32 cmds/client/drain, ≤16 ws (503), ping 30 s / idle 90 s, loopback-only default, **no `--bind-lan` flag** (§5.3) | `serve.rs:244–320`; `DEFAULT_PORT=47900`; `ingress.rs:30,36`; `http.rs:104–113` | VERIFIED |
| 55 | MCP revision 2026-07-28 (+2025-11-25, never 2024-11-05); sessionless; `X-CSP-Client`/`?client=`; initialize-first gate `-32600` (ping excepted); SSE on Accept (§5.4) | `mcp.rs:43–48, 55, 164–184` | VERIFIED |
| 56 | Tool surface `csp.mcp.tools.v1`: 9 sim-class + 7 session-class with the stated arg shapes (`vessel.control` throttle/steer_dir[3]/attitude{rot[4],ang_vel[3]}/hold_attitude, lease required; take/release; mission.nodes set/delete; timewarp.set/advance_to; sim.command raw passthrough) (§5.5, §8.1) | `tools_v1` (740–860) matches table exactly; `TOOL_SCHEMA_VERSION = "csp.mcp.tools.v1"` | VERIFIED |
| 57 | Driving model: world advances only on submit; `drain_and_step` steps ≥1 boundary, chases AdvanceTo up to **64 frames/call**, re-issues on partial; report carries rejected/partial/reached/target/fired events (§5.6) | `backend.rs:387–439` (`for _ in 0..64`); GameHost loop `rx.recv()`-driven only | VERIFIED |
| 58 | WS protocol: envelope `{proto:1, kind, seq, tick, payload}`, `fields` dotted paths incl. `focus.*`/`program.*`, ≤64 paths, `rate_hz` ∈ {64…1} default 16, `unknown_fields`, drop-to-latest, command channel + own-seq acks; MessagePack deferred with byte-budget tripwire e2e (§5.7, §8.7) | `http.rs:85–92`; `telemetry.rs:33–52` (focus alias, ProgramSnapshot funds/science/contracts/crew/colonies/scripts); `ws_subscription_stays_within_byte_budget` e2e | VERIFIED |
| 59 | Worked example: `Initialize` handshake JSON; fresh world has no vessels; `session.load`; `ManufactureCraft` (costed, SurfaceLatched at ConstructionComplete) / `SpawnVessel {parent, elements}`; `vessel.take_control` + `vessel.control {"throttle":1.0}`; `EngageAutopilot` via sim.command; `RunExperiment` fields per lib.rs:836; `TransmitData`/`RecoverData` with `DataKey {experiment,body,biome,situation}` (research.rs:122); save/load (§5.8) | all command variants + field names verified in `csp-sim/src/lib.rs` (RunExperiment at exactly :836, DataKey at exactly :122); quat-order trap avoided (example uses throttle, not a quat) | VERIFIED (landmine b, part 3) |
| 60 | e2e suite coverage list (§5.8) | all named tests exist (`mcp_round_trip_and_external_ordering`, `bounded_ingress_returns_backpressure`, `lease_enforcement_over_the_wire`, `commands_vs_local_world_hash` [= "local-vs-server hash parity"], ws ×5 incl. cap/envelope/byte-budget, `dashboards_sidecar_api_and_tools`, `spa_serving_is_token_free_but_allowlisted`, `backend_calls_leave_workers_free`, token lifecycle/symlink) — **but "incl. LAN binds"**: e2e binds are all 127.0.0.1; wildcard/LAN allowlist coverage is http.rs *unit* tests (`for_bind("0.0.0.0:47900")`, http.rs:986+) | **MINOR-INACCURATE** (LAN-bind coverage attributed to e2e; §5.3 "e2e-tested" same) |
| 61 | Telemetry SPA: TanStack/Vite, ~1.1k TS (1037), versions pinned exactly, widgets gauge/graph/orbit-map/table in `registry.tsx`, `dashboards/default.json`, `?csp_token` → sessionStorage + URL strip, first-boot gate, restart rotates token (§6) | `apps/telemetry` — all verified | VERIFIED |
| 62 | `http.rs` "no binary wiring yet" spa_dir comment is stale; `serve_cli` sets it; `packaged_spa_dir` resolves `apps/telemetry/dist/client` / `share/csp/telemetry` (§6, §8.8) | comment exists (`http.rs:52`); `csp-app lib.rs:2995, 3006–3024` | VERIFIED |
| 63 | §7 status table rows (wired/mcp-only/absent) incl. exact `ui-unreachable` line list, `lint_reach` CI gate, alarm book `map.rs:769+` | `csp-sim/src/lib.rs` opt-outs at exactly 552/564/631/644/674/692/778/939/997; ci.yml runs lint-reach | VERIFIED |
| 64 | §8 divergences 1–8 | each cross-checked above (tools 9+7, serve-only, csp-ai scope, engage shape, poll-heavy semantics, 5 contract kinds, MessagePack deferred, stale comment) | VERIFIED |

### Details of non-VERIFIED rows

- **#34 (INACCURATE)** — §2 blast-radius table, biome-map row: "`cargo run -p
  xtask --bin gen_biome_maps --check` in CI". `.github/workflows/ci.yml` runs:
  fmt, clippy, `cargo test --workspace`, wasm check, `lint-deps` +
  `lint-attribution` + `lint-reach`, cargo-deny, golden replays, perf — no
  `gen_biome_maps` anywhere; no Rust test invokes the check either (csp-data's
  biome test only proves the PNGs decode as indexed palette). Fix: either wire
  the check into ci.yml or reword the doc to "run it manually; not CI-gated".
- **#25 (MINOR)** — the type is `csp_data::validate::Dataset` (11 fields — count
  correct); "LoadedDataset" is not a symbol in the tree.
- **#46 (MINOR)** — the circularize stock script is quoted with the `print`
  branches removed; harmless but the doc presents it as the shipped script.
- **#60 (MINOR)** — LAN/wildcard-bind allowlist coverage lives in
  `http.rs`'s unit tests (`BindAllowlist::for_bind("0.0.0.0:47900")`, and the
  "wildcard bind (the --bind-lan shape)" comment), not in `tests/e2e.rs`.
- **#51 (nit)** — `--empty` is the documented default of `csp-mcp`
  (`csp-mcp.rs:10`); the doc's flag list doesn't say so.

## Missing topics (completeness — could a new dev work from this alone?)

Mostly yes for parts/contracts/tech-tree/colony/site authoring and for
MCP/scripting. Gaps, roughly in order of impact:

1. **CraftDef / craft files** — the manufacture path (`ManufactureCraft`,
   VAB build) needs a `CraftDef`; its shape and where authored craft live are
   never documented (the MCP example punts with `{"ManufactureCraft": {...}}`).
2. **Add-a-body / add-a-system recipe** — §1.7 covers sites/colonies per system
   but not authoring `system.ron` + `body.ron` + orbits end-to-end.
3. **Eval-scenario authoring** — `data/eval-scenarios/` is referenced but the
   scenario schema (setup verbs, agent variants, goal ops, budgets, settle) has
   no walkthrough.
4. **`SpawnVessel`'s `KeplerElements`** — the example's `elements: {...}` is
   undocumented; an agent cannot fill it from this doc.
5. **MCP `resources` surface** — `mcp.rs` serves `resources/list`
   (`csp://telemetry/snapshot`); §5 omits it entirely.
6. **Dashboard document shape** — §6 says sidecar JSON `schema_version` 1 but
   never shows a document (widget kinds × `fields` × layout) or points at the
   schema definition in `dashboard.rs`.
7. **Localization story** — only `loc/en.ron` exists; the doc doesn't state
   whether/how other language tables would load.
8. **Embedded demo set maintenance** — parity with `apps/demo/data/parts` is
   called "manual", but how the wasm `EmbeddedSource` table is generated/kept
   in sync is unexplained.

(Items beyond these — save-format internals, per-widget field vocabulary — are
reasonably deferred to the sibling docs the doc already links.)
