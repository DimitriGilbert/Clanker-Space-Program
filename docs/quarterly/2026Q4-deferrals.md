# 2026 Q4 — §6.5 cut/deferral record

Quarterly review per PLAN §6.5 ("the cut trigger"): five Phase-7 items from
`.review/session-full-20261026/fix-plan.md` (§7.1, §7.2, §7.4, §7.5, §7.6) were each decided
**DEFER** — none is a confirmed defect fix; all are feature-roadmap, pillar, or architectural
work whose absence was previously unsanctioned. This file is the recorded decision §6.5
requires. Working tree at HEAD `6d7fd80`.

Format per entry: item · decision · rationale · what it defers (milestone/spec) · re-open
trigger.

---

## 7.1 §2.11 design assistant + `MissionProfile` emission [C26-F3]

- **Decision**: DEFER to post-v1.
- **Rationale**: §2.11's v1-classical list (PLAN.md:774-778) promises a *design assistant* —
  a greedy + local-search staging proposer over the unlocked part set given payload mass,
  target Δv, min TWR, and body — plus the `MissionProfile` emission that §3.4's confidence
  number consumes. Neither exists, and §6.4's five-item cut list does not cover them.
  Building the proposer + its VAB exposure is a new capability (feature-roadmap work), not a
  confirmed-finding fix. `csp-phys/src/profile.rs` (`:9`, `:193`) carries the M3-stub
  `MissionProfile` and documents coast time as the design assistant's to add "if it ever
  authors"; nothing shipped depends on the producer. `csp-ai` carries only the ascent
  autopilot.
- **Defers**: §2.11 *Design assistant* + *Mission profile* bullets; feeds §3.4's confidence
  number (M9b "real `MissionProfile` confidence UI" row covers the UI, not the producer).
- **Re-open trigger**: scheduling the feature — the plan's recommendation stands: implement
  into `csp-ai`, exposed in the VAB. Until then §3.4's confidence number runs on the M3-stub
  profile.

## 7.2 Planes/rovers pillar [C33-F2]

- **Decision**: DEFER to post-v1 — recorded as a **v1 cut** of the planes/rovers half of
  pillar 1.
- **Rationale**: PLAN.md:12 promises "rockets, planes, rovers, stations, from a data-driven
  part graph." The gap is **schema-level**, not content-level: `ModuleDef`
  (`crates/csp-data/src/part.rs:506`) enumerates Engine, Tank, Control, DockPort, CrewSeat,
  Experiment, Drill, Converter, Lab, Parachute — no Wing/LiftingSurface/Wheel/GroundDrive
  variant exists, so wing/wheel parts cannot be authored in data at all. Closing it
  requires a schema extension + a part family + aero ground-contact physics — a v2 pillar
  commitment, not a v1 fix. §6.4's cut list does not mention planes/rovers; this record
  sanctions the cut. Stations are **partial-sanctioned**: dockport (tech.rocketry.3) and
  lab (tech.science.1) exist; the docking UI is already cut per §6.4 item 4 with proximity
  transfer retained.
- **Defers**: pillar 1's "planes, rovers" (PLAN.md:12); any rover-presupposing plan text
  (e.g. PLAN.md:507's warp rule) reads as forward-looking for v1.
- **Re-open trigger**: a v2 milestone that funds the `ModuleDef` schema extension, a
  wing/wheel part family, and ground-contact aero. Re-opening requires a
  `DATA_HASH_VERSION`/content-set review since the schema changes part hashing.

## 7.4 ControlApi action groups + `mission.ascend` [C26-F4]

- **Decision**: DEFER — keep the frozen ControlApi surface as-is for v1.
- **Rationale**: PLAN.md:723 specces "action groups" in the trait surface and PLAN.md:765
  specces `mission.ascend(target_apo, incl)` as a callable service. The built surface has
  neither — `engage_autopilot(apo_m, pe_m)` / `disengage_autopilot()`
  (`csp-script/src/lib.rs:144-146`, module doc `:68-69` names them "the §2.11
  `mission.ascend` verbs") covers the ascend semantics under a different name and argument
  (periapsis, not inclination). The ControlApi is **frozen**:
  `crates/csp-script/tests/api_freeze.rs` pins the current surface, so post-freeze additions
  are a major-version event, not a fix-plan decision. Per-part `deploy`/`jettison` verbs
  exist; grouping is a convenience layer over them.
- **Defers**: §2.10's "action groups" trait member and the `mission.ascend(target_apo,
  incl)` service name/signature.
- **Re-open trigger**: a scheduled major-version bump of the ControlApi (new freeze-test
  baseline in the same commit). At that point decide whether `mission.ascend` is added
  beside `engage_autopilot` or the spec is re-worded to the shipped name.

## 7.5 HTTP attach-to-a-running-game [C21-F4 remainder]

- **Decision**: DEFER — recorded deviation: §2.12's "attach to a running game" is
  **headless-hosted-world only** in v1.
- **Rationale**: §2.12 (PLAN.md:799, :831) says the HTTP host attaches to a running game.
  The shipped `serve_cli` (`csp-app/src/lib.rs:3197`) instead spawns its own fresh headless
  world via `GameHost::spawn_full`; the windowed app hosts no server, no `ArcSwap` publish
  exists in any csp-app frame path, and no ingress drain mirrors the player's live session.
  `csp-server/src/backend.rs:852-857` documents the attach shape ("the shape `csp-app`
  mirrors when it attaches the same backend to its frame loop (M9d)"). The minimal half of
  C21-F4 already landed: `serve_cli` seeds the hosted world with the starter craft and
  carries an inline deviation note (`csp-app/src/lib.rs:3191-3196`;
  `docs/DATA-AND-SCRIPTING.md:634-640`).
  The remainder — frame-loop attach — is a feature, not a fix.
- **Defers**: §2.12 "attach to a running game" for the windowed session (M9d host
  bridging). Agents can still drive the hosted headless game over HTTP/MCP in v1.
- **Re-open trigger**: scheduling M9d-style host bridging — implement the frame-loop
  `ArcSwap` publish + ingress drain per the `backend.rs:852-857` sketch so `--serve`
  attaches to the player's session.

## 7.6 Thrust off the dominant body (moon insertion/return) [C06-F3]

- **Decision**: DEFER — recorded §6.5 cut of powered flight outside the construction-time
  dominant body's SOI.
- **Rationale**: `physics.dominant` is fixed at construction
  (`World::with_physics`, `csp-sim/src/lib.rs:2900-2909`) and only re-derived on restore
  from the serialized `PhysicsConfig` (`:2919-2925`; `bubble.rs:398-411`) — no
  dominant-switch command exists anywhere in the workspace. `apply_to_bubble`,
  `apply_set_maneuver`, and `fire_maneuver_start` all refuse `orbit.parent !=
  physics.dominant` (`lib.rs:6676`, `:6792`, `:6924`). Inside Rask's SOI a vessel can
  therefore never thrust, schedule a node, or latch — moon missions are ballistic flybys;
  no orbit insertion, no return burn, no landing. PLAN.md:1501's M5 note sanctions
  deferring "landable" only (it names a `dominant=duva` bubble world "tracked for
  post-v1"); this record extends the sanction to the full thrust/scheduling/latch gap at
  the moon. A dominant-switch/bubble re-parent mechanism is an architectural feature, not
  a fix. Pairs with fix-plan §4.4 route (a): `departure_node` must price the correction the
  player cannot perform in-sim.
- **Defers**: powered flight, maneuver scheduling, and surface latch inside non-dominant
  SOIs (v1: Rask). M5's "reachable" bar stands as demonstrated
  (`duva_reachable_in_sim_end_to_end`, `lib.rs:20162`).
- **Re-open trigger**: a v2 milestone funding a dominant-switch/bubble re-parent mechanism
  (multi-body bubble worlds). Until then transfer planning must keep producing plans that
  are flyable from the dominant SOI (§4.4 route (a)).

---

*Review inputs: `.review/session-full-20261026/fix-plan.md` Phase 7;
`verified-report-C26.md` Findings 3-4; `verified-report-C33.md` Finding 2;
`verified-report-C21.md` Finding 4; `verified-report-C06.md` Finding 3. Source line numbers
re-verified at HEAD `6d7fd80` (they have drifted from the verified reports). Items 7.3
(contract kinds) and 7.7 (lethality + repair calibration) are not decided here — they
remain Phase-7 work items, not deferrals.*
