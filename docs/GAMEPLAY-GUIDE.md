# Clanker Space Program — Gameplay Guide (working-tree truth, 2026-10-26)

This is the honest, player-facing manual of what is **actually playable in the current working
tree** — not what PLAN.md aspires to. Every claim carries a code or data anchor (`path:symbol`).
Where behavior diverges from PLAN.md, both are stated. A brand-new dev or agent should be able to
learn how the game is played from this file without reading code.

Companion docs (written in parallel): [ARCHITECTURE.md](ARCHITECTURE.md),
[SIMULATION.md](SIMULATION.md), [UI-AND-RENDERING.md](UI-AND-RENDERING.md),
[DATA-AND-SCRIPTING.md](DATA-AND-SCRIPTING.md), [BUILD-AND-DEV.md](BUILD-AND-DEV.md).

Terms used throughout (defined once here):
- **Bubble** — the 20 km radius around the focused vessel where real per-tick physics runs
  (`data/balance.ron: bubble_radius_m`, 64 Hz ticks).
- **Rails / OnRails** — a vessel integrated analytically on a Keplerian conic; no ticks, no drag.
- **Latched (SurfaceLatched)** — a settled, parked vessel frozen to the body-fixed surface frame
  (PLAN §2.6.4). Launch pad craft, landed craft.
- **Δv** — velocity change available from remaining propellant (rocket equation), in m/s.
- **TWR** — thrust-to-weight ratio. Below ~1.2 at launch you are not going to space today.
- **Ap / Pe** — apoapsis / periapsis (highest / lowest point of the orbit).
- **EC** — ElectricCharge, the resource transmits and drills consume.
- **BOM** — bill of materials; the metal/composite a launch order debits from the pad's stockpile.
- **₣ / ⚛** — funds / science, as the HUD status strip prints them.

---

## 1. The game, as shipped in this tree

A KSP-style build-and-fly sim with an RTS progression layer, one fictional solar system
(Solen star; **Terrin** home with a 70 km atmosphere; **Rask** moon; **Duva** neighbour —
`data/systems/home/`). There is **no formal win condition**; the played loop is:

```
VAB (build) → LAUNCH (a paid build order) → fly in the bubble → auto OnRails above 70 km
   → science (run canister → transmit or land+recover) → research (tech nodes/tracks)
   → contracts (accept → complete → funds) → recover the vessel (funds/science/crew home)
   → hire crew, run labs, bench-test parts, found/join colonies → repeat, warp to deadlines
```

What you start with (`csp-view/src/lib.rs: View::new` + `View::seed_starter_craft`):
- **₣ 1,000,000** funds (`data/balance.ron: starting_funds_micros`).
- A seeded starter craft, the **"Stock Hopper"** — pod + science canister + battery + s160 tank +
  Sparc engine, sitting latched on the Terrin pad (`csp-view/src/demo.rs: STARTER_CRAFT`,
  liftoff TWR ≈ 5).
- A seeded 6-slot contract offer board (`contract_offer_slots`, `csp-sim/src/contracts.rs`).
- One authored colony: the **Rask Mines** outpost (solar + battery + ore drill + smelter),
  founded at boot (`data/systems/home/colonies.ron`).
- The first-flight tutorial checklist on screen (`csp-ui/src/tutorial.rs: STEPS`).

Progression currencies: **funds** (contracts, recovery, science milestones; spent on launches,
hires, bench tests), **science** (experiments; spent on tech), **part-family qualification**
(flight + bench units that lower future failure hazard). There is deliberately **no reputation
currency** (PLAN §3.6).

Divergence from PLAN.md: PLAN §3.5's full facility/upkeep economy is only partly populated
(salaries exist but only for hired crew; facility upkeep is not authored). The loop above is
what the tree actually ships.

---

## 2. Controls

### 2.1 Keyboard (defaults — all rebindable in Settings → Key bindings)

The shipped defaults (`csp-ui/src/settings.rs: Keymap::default`). Movement/attitude keys bind
**physical key positions** (`key-w` = the QWERTY W position), so they work unchanged on AZERTY
etc.; hint bars always print the key as your layout labels it.

| Action | Default key | What it does |
|---|---|---|
| Toggle map | `M` | Map view ↔ previous view |
| Toggle VAB | `V` | Editor ↔ previous view |
| Open settings | `Esc` | Settings window |
| Warp faster / slower | `.` / `,` | Step the warp ladder |
| Throttle full / cut | `Z` / `X` | Snap throttle to 1.0 / 0.0 (Z on a latched pad craft **launches** it) |
| Throttle up / down (held) | `Shift` / `Ctrl` | Continuous throttle ramp |
| Stage | `Space` | Fire the next stage (jettison drop set) — flight/map only |
| Cycle focus | `Tab` | Map camera focus body/vessel |
| Recover control | `T` | Bring the focused (map) or newest on-rails vessel back into the bubble (`ToBubble` + player lease) — this is also the **deorbit/re-entry verb** |
| Pan/pitch | `W A S D` / arrows | Camera pan (map/VAB); pitch/yaw in flight |
| Roll left / right | `Q` / `E` | Flight roll |
| Controls help | `F1` / `/` | The controls card (lists live bindings + mouse map) |
| Debug panel | `F3` | Dev panel (adapter/tick internals) |
| Program window | `P` | Contracts / research / science / colonies / automation |
| Quicksave / Quickload | `F5` / `F9` | Write / restore the `quicksave` slot |
| Revert to launch | `F10` | Arms the revert confirm (see §8) |

### 2.2 Mouse

| Input | Where | Effect |
|---|---|---|
| Drag | flight | Orbit the chase camera 1:1 — it turns by exactly what the pointer travels (`csp-view/src/flight.rs: FlightEvent::Orbit`); pitch stops at ±89°. Dragged below the horizon the camera rides 0.3 m above the ground, sliding along it rather than going under, and the ground never pushes it inside the craft's minimum distance — dragged straight down, it slides along the ground toward the craft and then goes round it at exactly that distance, and zooming in never moves it back out. Over water the surface is no floor: the camera goes under it, down to 0.3 m above the seabed, and from below the water fills the view. Over a craft that is under the ground the camera orbits from above the ground; for a craft at least an eye distance down — the whole orbit above the ground — a drag moves it there as freely as in flight, while a shallower craft keeps only its depth of clearance and the ground still lifts the eye when it looks down |
| Wheel | flight | Zoom: the wheel sets a distance and the camera eases to it (80 ms half-life), between 3 m (1.5 × the craft's size for a big one) and 1,000 km (`FlightEvent::Zoom`) |
| Drag / RMB or MMB drag / wheel | map | Orbit / pan / zoom-to-cursor; **click** a marker to focus (`csp-ui/src/lib.rs: map_overlay` hint bar) |
| Click / drag / RMB | VAB | Select part / lift-to-move / remove hovered subtree; drop a lifted part on the palette to delete (`csp-ui/src/vab.rs: vab_pointer_input`) |
| **RMB on a part** | **flight** | **Part action menu** — run science, deploy parachute, transmit/recover data, recover/terminate vessel (`csp-ui/src/program.rs: part_menu`) |

### 2.3 Rebinding & settings

`Settings` (Esc): audio volume/mute, **vertical sync** and **render scale** (Graphics — vsync off
asks for an uncapped frame rate where the platform and driver allow it; the log's `render:` line
names the mode actually in use. Render scale 0.5–1.0 renders the 3D scene
smaller and upscales it for fps while the UI stays sharp), UI scale, **orbit sensitivity**
(0.25–4, default 1.0 — scales every camera drag in flight, map and VAB) and **invert vertical
orbit** (reverses the vertical half of an orbit drag in all three cameras — flight, map and VAB),
**max time warp**
(×1…×100,000 — a QoL ceiling under the ladder, `WARP_CAP_OPTIONS` at `csp-ui/src/settings.rs:227`),
new-game difficulty (life support, dormant stress — applied only when a save is created), and the
full key-binding grid with per-action rebind/clear (`Keymap::bind` evicts a stolen key so nothing
double-fires). Settings persist to `$XDG_CONFIG_HOME/csp/settings.ron`
(`csp-app/src/settings_io.rs: config_dir`).

---

## 3. The VAB (Vehicle Assembly Building) end-to-end

Entered with `V` or the top-bar button. Four surfaces: **palette** (left, catalog),
**inspector + staging + readout** (right), **action strip**, and the 3D build area
(`csp-ui/src/vab.rs: vab_editor`).

### 3.1 Building

1. **Open the catalog**: the palette panel lists every part your researched tech unlocks
   (`part_unlocked`; tech-locked rows refuse placement with `VabError::PartLocked` — never a
   silent downgrade). 16 parts ship today (§10).
2. **Place the root**: click a palette row — this *lifts* the part onto the cursor as a ghost
   (KSP's held-part gesture, `VabAction::CatalogPick`) — then click the empty build area of an
   **empty** craft. A click never places directly; the ghost commits on drop
   (`VabSession::plan_attach` previews exactly what a drop commits, green/red).
3. **Attach more parts**: pick another row and click a placed part. **Auto** attach mode reads
   the hovered face — top/bottom **caps stack axially**, the hull **side surface-mounts**
   (radial attach). You can force `Axial` / `Side` / `End` in the action strip's "mount" toggle
   (`AttachMode`, `csp-ui/src/vab.rs:70`). Parts join through size-class-compatible attach
   nodes (`VabError::NoCompatibleAttach` names a mismatch).
4. **Radial symmetry**: ×1 / ×2 / ×4 copies of a surface mount — action-strip buttons or the
   `X` key cycles (`VabAction::SetSymmetry`).
5. **Moving parts**: drag a placed part (or click the already-selected one) to lift it **with
   its subtree** and drop elsewhere (`VabAction::Grab` → `reattach`, node id preserved). The
   inspector's "Pick up (move)" button does the same. **The root still refuses to be grabbed —
   it defines the build frame** (`csp-ui/src/vab.rs:200`); instead use **Reroot**
   (`VabAction::Reroot`, moves the origin to the selected part) or **Translate** — the
   free-reposition verb that moves the *whole craft* in the build frame (`VabAction::Translate`).
6. **Removing**: `Del`/`Backspace` removes the selected part's subtree; right-click removes the
   hovered subtree; **"Remove part only"** (inspector) promotes children to the grandparent
   (`VabAction::RemoveOnly` — the fix for the old "removing a part glues the stack" complaint).
   Drop a lifted part on the palette to delete it.
7. **Undo/redo**: `Ctrl+Z` / `Ctrl+Shift+Z` (`VabAction::Undo/Redo`) — every craft mutation is
   recoverable.
8. **Tweak**: inspector offers per-part **scale** and **stack-axis stretch** (gated by the part's
   `ScaleRule` — tanks scale continuously, the adapter has discrete 0.5/1/2 steps, most parts
   none) and **variants** (e.g. engine "qualified" variant: +20–25% cost for better stats).
9. **Staging**: the Staging panel — `+ stage` / `− stage` / `compact`, and per-part stage
   assignment via the inspector (higher stage index fires earlier). Space fires one stage set
   per press in flight; the plan is captured at launch (`csp-view/src/flight.rs: StagePlan`).

### 3.2 Readouts (before you launch)

The readout panel shows, live: part count, **wet mass (t)**, **Δv total (vac and sea-level)**,
per-stage `Δv / TWR / burn time / thrust` rows, and a **mission profile check** — set a target
Δv and minimum launch TWR and it prints the margin ("profile: Δv margin +850 m/s" or
"short on Δv or launch TWR") (`csp-ui/src/vab.rs` readout section ~line 3103; the analysis is
`csp_phys::readout::analyze`). The **validation** panel lists launch-blocking errors (red) and
warnings (yellow); a clean craft reads **"clean — launchable"** (`csp-ui/src/vab.rs:3602`).

### 3.3 Craft files

Inspector bottom (native builds; hidden in the web demo): **save as** `<name>` and a **load
craft** combo — files live as `CSPCRAFT` bytes under `$XDG_CONFIG_HOME/csp/crafts/`
(`csp-app/src/lib.rs: craft_dir`, outbox `craft_save`). "New craft" clears the session.

### 3.4 LAUNCH — what it costs and what happens

The top-bar **LAUNCH** button is a **§3.5 build order, not an instant spawn** (the old
launch-bypass bug is fixed; PLAN and tree now agree):

- You stay in the VAB and get a refusal notice if: a part is **tech-locked**, the craft can't
  resolve, **funds < craft cost** (`craft_cost_micros`), the pad's **stockpile is short** on a
  BOM resource (parts' `build_resources`, e.g. Sparc = 70 metal + 10 composite), the **vessel
  cap** is hit, or the pad is blocked by an **active** (flying/rails) vessel
  (`csp-view/src/lib.rs: launch_preflight` — every refusal names its reason on the notice row).
- A **parked (latched) vessel on the pad is auto-recovered** as part of your launch — funds and
  parts come back, the pad clears (`launch_craft`, "PARKED BUILD RECOVERED" notice).
- On success: **funds + the site-inventory BOM are debited at order time** and a
  `ManufactureCraft` build order is placed (`csp-view/src/lib.rs:3015`). The craft
  **materializes latched on the pad after `mass / 50 kg/s` sim-seconds** of build time
  (`manufacture_rate_kg_per_s`) — the notice prints the ETA, the Program window's FACILITIES
  section counts it down. The view switches to Flight immediately; throttle (Z) unlatches and
  launches it.
- The world state just before the order is captured as the **revert point** (§8).

PLAN.md note: PLAN allows launch also costing *time* — implemented exactly as build time here;
there is no separate "rollout".

Known editor limitations (verified): the root part cannot be lifted/moved alone (reroot or
whole-craft translate instead); no mirror symmetry (radial only, 1/2/4); no multi-craft or
subassembly loading; no offset/rotate gizmo — positions are snap-attach only.

---

## 4. Flight

### 4.1 Pad → ascent

The delivered craft sits **latched** on the pad; the HUD prints `ON PAD — [Z] to launch`
(`csp-ui/src/lib.rs: flight_hud`). `Z` (throttle full) issues `SetThrottle`, which **unlatches**
the craft and starts the climb. Fly with `Shift/Ctrl` (throttle), `WASD` (pitch/yaw), `Q/E`
(roll), `Space` (stage). Chase camera: drag to orbit, wheel to zoom (the distance eases to the
wheel's target, 80 ms half-life). A craft is framed at four times its size whenever it takes the
camera — a launch, a recovery — and the camera eases there rather than jumping; when the new
craft is far away (a recovery on the far side of the planet) the camera makes the trip in half a
second, over the ground, and keeps your orbit angles.

### 4.2 HUD and navball (what the numbers mean)

Left telemetry block (`csp-ui/src/lib.rs: flight_hud`; the block and the navball are **movable
windows** — drag them anywhere; caveat: the readout block must be dragged by its margins, not
its text, and the navball first opens parked too high — see Known issues):

- **ORBIT**: `AP/PE` osculating apoapsis/periapsis altitudes; `T-AP/T-PE` times to apsis; `T`
  orbital period. `—` while parked or on an open (escape) trajectory.
- **SURFACE**: `ALT` altitude above terrain, `SPD` surface-relative speed, `ORB` inertial
  (orbital) speed, `V/S` vertical speed, `HDG` compass heading, `P/R` pitch/roll.
- **VESSEL**: `MASS` wet mass, `FN` thrust at current throttle, `TWR` at local gravity,
  `TWR·SL` at surface gravity (the pad figure), `ACC`/`G` acceleration, `ΔV` remaining
  including staged drops (`STG+`), `MAXF` full-throttle thrust.
- **PROP**: throttle bar, per-resource tank gauges, `FLOW` kg/s, `DEP` time to fuel depletion,
  `BURN` full-throttle burn time, `ISP` mass-flow-weighted vacuum Isp, `STG` firings left,
  parts/warp line.
- **Navball** (bottom-center by intent — its first-open parking position is mis-placed ~242 px
  high, a known issue; `csp-ui/src/navball.rs`): attitude ball with sky/ground, pitch
  ladder, compass letters, heading tape, plus **green prograde/retrograde markers** in the
  surface frame (hidden while parked).
- Top-center **notice stack** (« cues »): engine/stage/impact SFX edges, warp/refusal reasons,
  build-delivery toasts — in *every* view mode (`csp-ui/src/lib.rs: notice_overlay`).
- Top-right **status strip** `₣ funds ⚛ science ✉ contracts` — click it to open the Program
  window (`program_status`).

### 4.3 Reaching orbit (auto bubble → rails)

There is **no manual "go on rails" verb** and none is needed: when your vessel climbs out of
the atmosphere on a conic that clears the floor, the sim converts it automatically
(`csp-sim/src/lib.rs` "auto bubble→rails conversion" at the tick-range break). Conditions
(`apply_to_rails_at`, `csp-sim/src/lib.rs:6206`): altitude above **atmosphere top** (Terrin
**70 km**), no live ground/vessel contact, and the conic's periapsis above
`radius + max(atm_top, bubble_radius)` — i.e. **raise Pe above ~70 km to be captured on rails**,
which is exactly "close the orbit". On conversion the HUD's orbit numbers become live rails
readings and warp above ×2 unlocks (§7). The tutorial's step 7 coaches this
("pitch over near apoapsis, raise periapsis above the atmosphere"); the "circularize at
apoapsis" stock script or a manual maneuver node will fly the circularization burn for you
(§9).

### 4.4 Map view, maneuver nodes, transfer planning

`M` opens the map: orbit lines, apsis marks, SOI-encounter predictions, body/vessel labels,
click-to-focus, `Tab` cycles focus (`csp-view/src/map.rs`). Two planning windows:

- **Maneuver node** panel (`csp-ui/src/lib.rs: maneuver_panel`): pick vessel, set `T+` seconds
  and prograde/normal/radial Δv, **Set node / Delete node**. One node per vessel. The burn is
  flown **automatically by the executor** — at the node tick the vessel re-materializes in the
  bubble, the executor owns throttle+attitude until the Δv is delivered, then returns the
  vessel to rails (`csp-sim/src/maneuver.rs` module doc). Warp demotes before the burn
  (`ManeuverNodeStart` is a clamping event), so you cannot overshoot it.
- **Planning** panel (`csp-ui/src/lib.rs: planning_panel`): **ALARMS** (manual label+T+, quick
  *at Pe / at Ap / at node*) and **TRANSFER** — origin/destination pickers, per-vessel
  eligibility notes ("in physics bubble — coast to rails first"), **Find window** (Lambert
  planner: departure countdown, transit time, Δv depart/arrive, predicted SOI encounter with
  IMPACTS warning), then **Set maneuver node** / **Watch window** / **Alarm at window**.

### 4.5 Re-entry, parachute, recovery, terminate

- **Come back down**: with the vessel focused in map (or newest), press **`T` (Recover
  control)** — a `ToBubble` converts the rails vessel into the bubble at its current conic
  position and hands you the player lease (`csp-view/src/lib.rs: recover_control`; refusal
  notices name why). Then burn retrograde / set a retrograde node to lower Pe into the
  atmosphere and drag does the rest.
- **Parachute**: the `csp.chute.s125` part (surface-mount). Deploy via **right-click on the
  part → deploy parachute**, or the flight **Actions strip** — deployable **only while flying**
  (in-bubble; `apply_deploy_part`, `csp-sim/src/lib.rs:7211`). The canopy catches below
  **6 km** and **260 m/s** (`deploy_alt_m`/`deploy_speed_mps` in the part def) with a
  120 m² drag area.
- **Water is real** (sub-phase 4.1): where a body has an ocean (`terrin` does — everything
  from the seabed up to sea level), water holds a craft up. A part floats if it is lighter
  than the water its collider displaces (`water_density_kg_m3: 1025` kg/m³ — a full fuel tank
  floats with four fifths of it under, a loaded ore carrier sinks). What floats **settles and
  latches like a landed craft, as `Splashed`**; what sinks goes down to the seabed and latches
  `Landed`. Weight high in the stack tips it over in the water, weight low brings it back
  upright — a tall craft that splashes engine-first tips over and settles on its side.
  **Splashdown loads are real**: under a parachute (≈17 m/s) a stack on the shipped
  mount limits survives the hit (≈9 g); unbraked at 200 m/s it breaks apart (≈900 g). The
  shipped science
  canister's situation mask (49) does **not** include `Splashed` — recover a floater or
  transmit from it instead of running the canister (`csp-phys/src/water.rs`,
  `SimWorld::splashed`).
  **You can see it** (sub-phase 4.2): a craft in the water shows through the surface, fading
  with the water before it, red first — 15 m of water leaves a hundredth of the red, 92 m a
  hundredth of the green, 230 m a hundredth of the blue — and the camera follows it under the
  surface, down to 0.3 m above the seabed.
- **Recovery**: land — or come to rest on the water — the vessel becomes **latched** — then **RECOVER VESSEL**
  (Actions strip, Program → SCIENCE, or right-click menu). Payout: **part value** at site
  efficiency (1.0 at the launch pad, 0.9 at other sites, 0.25 remote salvage), **resource
  contents** credited to that site's stockpile, **all pooled science banks at the recover
  fraction**, and **crew come home** (`apply_recover_vessel`, `csp-sim/src/economy.rs:880`).
  Recovery is also what completes **rescue contracts** (the `VesselRecovered` trigger,
  `economy.rs:1006`).
- **Terminate**: same surfaces, "terminate" — deletes a latched vessel outright, no payout,
  frees the pad berth (`SimCommand::TerminateVessel`). Use it for hulks you don't want to fly
  home.

---

## 5. The science loop end-to-end

One experiment ships: **`exp.materials_study`** on the **`csp.science.canister`** (starter
craft carries one, plus the `z1k` battery to pay transmits).

1. **Run**: right-click the canister in flight → **⚛ run**, or Actions strip **"⚛ run
   science"** (runs every runnable module), or Program → SCIENCE → `run`. The canister is
   **one-shot** (10 data units) and runs only in **Landed / SpaceLow / SpaceHigh** situations
   (situation mask 49, `data/parts/csp.science.canister.ron`); a blocked button says why on
   hover.
2. **Diminishing repeats**: the sim applies the plan rule's exact integral —
   `C' = C + (cap − C)·(1 − exp(−k·u/cap))` per run of `u` units (k = 1.0;
   `research.rs:741–753`). The oft-quoted 63/86/95% sequence holds only for runs of
   `cap` units; the shipped canister runs `u = 10`, so on Terrin (cap 30) three runs
   bank ≈28% / ≈49% / ≈64% of the cap cumulative (≈8.5, ≈6.1, ≈4.4 units) — Rask (60)
   and Duva (90) fill slower still. Caps per body:
   Terrin 30, Rask 60, Duva 90, Solen 50 (space-only) (`data/experiments/`, cap table).
3. **Transmit vs recover**: `transmit_ratio = 0.4` — transmitting banks **40%** of pooled
   units' value immediately and drains **1 EC per data unit** (`transmit_ec_per_data_unit`);
   the button is disabled with "needs N EC (have M)" when short. **Recovering** (latched) banks
   **100%**, or exactly the `1 − ratio` remainder for an already-transmitted key, and closes
   the key (`csp-sim/src/research.rs: apply_transmit_data / apply_recover_data`).
4. **Banking science pays funds**: every science point banked also credits **₣10**
   (`science_funds_per_point`) at the milestone edge.
5. **Labs** (PLAN §3.2's data→science conversion): the `csp.lab.mk1` part (2 crew seats, 2
   researcher slots, tech `tech.science.1`). Program → LABS: an idle lab lists **"research
   ⟨pooled key⟩"** buttons → `StartResearch` escrows the pooled data and converts it to
   science over sim-time (crew aboard staff it); **stop** banks the accrual. Live projects
   show accrued ⚛, data left, and ETA — they advance under warp.
6. **What science buys**: the tech tree (§6 RESEARCH) and upgrade tracks.
7. **Tutorial coverage**: the first-flight checklist's second tier is exactly this loop —
   "Run a science experiment → Bank the data → Research a tech node"
   (`csp-ui/src/tutorial.rs: STEPS[7..10]`), with predicates on real state (pooled units,
   banked science, a paid node researched). Known defect: the tutorial's rewind check
   (`tutorial.rs:264–267`, `step > STEP_LAUNCH && !has_vessel`, unscoped) fires when you
   follow the coached land-and-recover path — recovering the vessel resets the tutorial to
   the launch step ("vessel lost") and blocks the banking step until you launch again
   (verified session-full-20261026, report C23 Finding 3).

---

## 6. The Program window (`P`, or click the status strip)

One scrollable window with sections (`csp-ui/src/program.rs: program_panel`). Every verb below
is wired to a real `SimCommand` — no dead buttons; disabled buttons carry the reason on hover.

- **OVERVIEW** — funds, science total, spendable banked science.
- **CONTRACTS** — active contracts with countdowns and **live progress lines**
  ("delivered 4/20 ore", "band satisfied"); the offer board with each offer's params, reward
  line (`+₣ advance · +₣ done · −₣ expiry`), deadline, and per-prerequisite ✓/✗ — the accept
  button is disabled with "prerequisites unmet: …" until they hold (`contracts_section`).
  Accept credits the advance immediately; already-satisfied state-predicate offers
  (satellite/part-test) can complete at acceptance (`apply_accept_contract`).
  The eight shipped templates and how to complete them (`data/contracts/`):
  | Template | Ask | How a player completes it |
  |---|---|---|
  | `contract.survey` | 1 unit of materials-study data from Terrin | Run the canister, transmit or recover (starter-completable) |
      | `contract.survey.data` | ≥5 units (tech.science.1 + one completed survey) | A single fresh-key run covers it (≈8.5 units on Terrin); more runs help at drained keys |
  | `contract.satellite` | Closed orbit in a drawn Pe 80–250 km / Ap ≤1500 km band | Put any non-debris vessel's orbit inside the band (`objective_met`) |
  | `contract.part_test` | A named module type in a drawn regime (orbit altitude / landed body) | Fly a craft carrying that module into the regime (bench tests do **not** satisfy it — it reads vessel state) |
  | `contract.rescue` | Recover a vessel on the drawn body | Land + latch + `RecoverVessel` there (transition-keyed) |
  | `contract.colony_resupply` | Deliver N resource units into colonies on the drawn body | Join a colony's inventory with a latched vessel and use **deliver** (`TransferColonyResource`) |
  | `contract.crew_transport` | Post N crew at the colony on the drawn site | Fly members over and transfer them vessel↔colony, or **→colony** an unassigned member (`AssignCrewToColony`); the ask re-bases at accept, so crew already stationed don't count |
  | `contract.tourist_flight` | A tourist rides through the drawn Terrin orbit band and comes home alive | Board a `CrewKind::Tourist`, fly the band, then recover them home — needs `CrewLostAtMost(0)` standing to unlock |
  Board gating is by tech prerequisites, so the deeper kinds appear only after research —
  at game start you see survey offers only. (The stale "2/6 uncompletable" finding from
  `.review/session-prd1` no longer holds: rescue and resupply both have player paths now.)
- **VESSELS** — every vessel with residence ("in flight"/"on rails"/"landed"), body, crew
  count, debris flag; linked vessels (docked or both latched within 100 m) get per-resource
  **→vN 10 / all** transfer buttons (`TransferResource`).
- **CREW** — **+pilot/engineer/scientist/tourist** hire buttons (₣5,000 each, salary shown;
  salaries drain every 30 sim-days), **board** (seat a fresh member on the focused vessel if
  it has a free `CrewSeat`), and per-member posting verbs: unassigned → **→colony**;
  colony-posted → **→vessel** (if the focused vessel joined that colony); aboard → **→colony**
  or **→vN** to a linked vessel. This is the full crew surface of PLAN §3.5 minus XP
  (deliberately absent in v1).
- **COLONIES + SITES** — per-colony card: state (nominal/degraded/dormant/abandoned), day/night,
  modules with allocation fractions, store levels `level/capacity (rate/s)`, thermal T_eq,
  crew count, inspection countdown; verbs **evacuate / inspect +1d / abandon**. A latched
  focused vessel on the colony's body within 100 m can **join** its inventory, then
  **deliver/withdraw** resources (10 or all) and move crew both ways. **SITES** rows show each
  site's stockpile and a **found outpost** button on unoccupied sites (a free starter
  solar+battery prototype colony — no player-designed colony construction yet).
- **AGENDA** — pending alarms/watched windows with countdowns; quick **alarm +1 h / +1 d**
  buttons (`SetAlarm`).
- **AUTOMATION** — see §9.
- **RESEARCH** — the 4-node tech tree (`data/techtree/home.ron`: `tech.start` free →
  `tech.science.1` 15⚛ / `tech.rocketry.2` 30⚛ → `tech.rocketry.3` 60⚛) with **research**
  buttons (prereq- and cost-gated), plus the 5 **upgrade tracks** (Sparc Isp ×1.03/level ×4,
  Sparc thrust, Ember Isp +5/+5/+10 s across its 3 levels, small-tank mass −5%, pod MTBF
  ×1.25) bought level-by-level
  with **+cost** buttons. Unlocks apply to *newly manufactured* parts only (frozen at build,
  PLAN §3.3 — implemented via `BuiltPart.upgrade_levels_at_build`).
- **SCIENCE** — the focused vessel's situation context line, experiment rows (run), pooled
  data rows (**transmit +N⚛** EC-gated / **recover +N⚛** latched-gated), **recover vessel →
  site / terminate**, and **deploy** buttons for armed parachutes.
- **LABS** — see §5.
- **FACILITIES** — in-flight **build orders** with delivery countdowns; per-part-family
  qualification rows (units pooled, live hazard multiplier, next curve knot, credited
  failures); the **test stand** line (per-unit cost: ₣50 + 1 metal + 60 s/unit at the pad's
  stockpile) with **test +5 / +25** bench buttons (`RunBenchTest`) — the way to qualify
  engines *before* trusting them (each family's hazard multiplier drops toward ×0.45 as units
  accumulate).
- **RELIABILITY** — per-part rows on the focused vessel: FAILED (mode list), ignition misfire
  (latched until throttle returns to 0), thrust degraded, or `ok (wear N%)` with dormant flag.
  Failures also flash as HUD toasts on the focused vessel.
- **LOG** — recent notification lines (part failures, misfires, ON RAILS records, …).

**In-flight Action strip** (bottom-right, `flight_action_bar`): the same science/recovery verbs
without opening the window — run science, per-record transmit/recover, deploy chute, big
**⏏ RECOVER VESSEL** row, Program button.

---

## 7. Time warp

- **Ladder**: `×1, ×2, ×10, ×100, ×1,000, ×10,000, ×100,000` (`WARP_LADDER`,
  `csp-app/src/warp.rs:33`). `.`/`,` step it; the top-bar prints `warp ×N` with −/+ buttons.
  Your settings cap can lower the ceiling.
- **×2 is physics warp** (bubble keeps ticking at the same dt); ×10+ is **rails warp** — no
  bubble ticks, analytic advance.
- **When warp refuses, and why** (complaint "warp refuses silently" is fixed — every refusal is
  spoken): pressing `.` while a rails-warp gate holds prints **`WARP REFUSED — <REASON>`** with
  the sim's own reason each press, and a persistent **`RAILS WARP ×1 — <REASON>`** HUD notice
  latches while it holds (`warp_step_request` and `on_step_report`,
  `csp-app/src/lib.rs:1398/1295`; the predicate is `rails_warp_refusal`,
  `csp-sim/src/lib.rs:3707`). Typical reasons: **a vessel is thrusting** (cut throttle —
  leaving the bubble zeroes it), **in atmosphere** (climb above 70 km or land+latch),
  **live contact**, or **a script is polling** (named). A press eaten by the settings cap
  prints **`WARP ALREADY AT ×N — SETTINGS LIMIT/LADDER TOP`**; a rung the measured frame budget
  can't hold prints **`WARP LIMITED — ×N EXCEEDS THE STEP-TIME BUDGET`** (the request stays
  queued and engages if the scene lightens).
- **Auto-demote before things you care about**: fired *clamping* events — your alarms, a
  tracked vessel's SOI transition, maneuver node start, contract deadlines, research
  completion, insolvency — demote warp so you cannot overshoot (`on_step_report`).
- **What advances under warp** (interval subsystems, `csp-sim` `advance_interval_subsystems`):
  colonies (day/night, drills, converters), **build orders** (ConstructionComplete delivery),
  **lab research projects**, contract **deadlines**, **salaries** (30-day cadence), dormant
  reliability hazard, in-flight resource/crew **transfers**, debris decay. The physics bubble
  does not tick under rails warp — engines are off by definition there.

---

## 8. Save / load / revert (the Game menu)

Top-right **Game** strip (drag it anywhere) → **Game** menu (`csp-ui/src/game.rs: game_menu`):

- **Save**: type a slot name → Save; **Quicksave [F5]** writes slot `quicksave`.
- **Load**: lists every slot with its label and tick; click to restore (also **F9** quickload).
  Files are `CSPSAVE` under `$XDG_CONFIG_HOME/csp/saves/<slot>.cspsave`
  (`csp-app/src/lib.rs:1159`). Saving is refused (with the §7.4 reason named) while commands
  are pending, contacts are live, joints are live, or the session is tainted/halted — e.g.
  *save while latched on the pad, not mid-bounce*.
- **Revert to launch [F10]**: enabled once a launch order has been placed (the pre-launch
  snapshot is captured at `launch_craft`). The confirm dialog carries the mandated honesty
  disclosure: **"Reliability thresholds are pre-drawn: the same parts will still fail at the
  same accumulated hazard. Revert is not a reroll — it is a retry of the flying."**
  (`game.rs`, PLAN §3.4's revert policy).
- **New game**: confirm-gated; abandons the current session (save first).

A fatal sim error shows a read-only crash dialog with the bug-report bundle path
(`csp_ui::crash_dialog`) — the session cannot be saved or continued from that point.

---

## 9. Alarms, autopilot, scripting — honest status

- **Alarms: PLAYABLE.** Two surfaces: the map's **Planning** panel (manual label+T+, at Pe/Ap/
  at-node quick arms, window alarms; armed rows show countdowns, fired rows await dismissal,
  "lapsed" rows report a wake that never fired) and Program → **AGENDA** (+1 h/+1 d). Fired
  alarms demote warp and toast on the HUD.
- **Autopilot: PARTIAL — ascent only.** Program → AUTOMATION: a target-apoapsis field
  (10–10,000 km) + **engage ascent** (enabled only for an in-bubble vessel — hover explains
  "unlatch first" / "on rails has no control ticks"), and **disengage**. It flies a
  gravity-turn to the target orbit (`csp-ai` `AscentAutopilot`, engaged via
  `EngageAutopilot`). No landing/rendezvous/transfer autopilots, no design assistant
  (PLAN §2.11 shortfall, still open).
- **Scripting: PLAYABLE — a Rhai console, not a full IDE.** Program → AUTOMATION: load any of
  four **stock scripts** — *telemetry logger*, *apoapsis announcer*, **circularize at
  apoapsis** (arms the apsis wake, schedules the prograde burn node on rails),
  *ascent hand-off* — or paste your own `fn on_tick(ctx) { … }` source and **load source**.
  Loaded scripts bind to the focused vessel: **pause/resume**, **unload**, strike counter
  (5 consecutive faults parks a script with its reason), armed wake tick, and a **script log**
  tail (also flashed on the HUD, bounded). Wake verbs (`wait_apoapsis()` etc.) keep scripts
  alive under rails warp (`csp-script`; budget 20,000 ops/tick). Scripts cannot do anything
  the player can't — everything goes through the same command queue.
- **For agents, not players**: the same sim is fully drivable over MCP (`csp-bench`'s
  `csp-mcp` stdio bin; `csp-server` HTTP + WS telemetry SPA, native only). That surface is
  out of scope for this guide — see the architecture doc.

---

## 10. State of the game — honest feature table

Legend: **PLAYABLE** (implemented + player-reachable) · **PARTIAL** (shipped but reduced /
one path) · **HIDDEN** (exists, no player UI) · **MISSING** (absent).

| Feature | Status | Anchor / note |
|---|---|---|
| VAB editor (stack+radial, staging, symmetry, undo, readouts, craft files) | PLAYABLE | `csp-ui/src/vab.rs`; root-move still refused (reroot/translate instead) |
| Launch as paid build order (funds+BOM+time) | PLAYABLE | `csp-view/src/lib.rs: launch_craft/launch_preflight` |
| Flight + HUD + navball + staging | PLAYABLE | `csp-ui/src/lib.rs: flight_hud`, `navball.rs` |
| Map view, focus, encounters | PLAYABLE | `csp-view/src/map.rs` |
| Maneuver nodes + auto burn executor | PLAYABLE | `csp-sim/src/maneuver.rs` |
| Transfer planner + window watch | PLAYABLE | `csp-ui/src/lib.rs: planning_panel` |
| Auto bubble→rails (reach orbit) | PLAYABLE | `csp-sim/src/lib.rs: apply_to_rails_at` + auto conversion |
| Parachute + re-entry + recovery + terminate | PLAYABLE | `csp.chute.s125`, `apply_deploy_part`, `apply_recover_vessel` |
| Science loop (run/transmit/recover, diminishing returns) | PLAYABLE | `csp-sim/src/research.rs`, `data/experiments/` |
| Tech tree + upgrade tracks | PLAYABLE (thin: 4 nodes / 5 tracks / 16 parts) | `data/techtree/home.ron` |
| Contracts (6 kinds, gated board) | PLAYABLE | `data/contracts/`, `contracts.rs: objective_met` |
| Economy (funds, salaries, milestones, insolvency) | PLAYABLE | `csp-sim/src/economy.rs`, `data/balance.ron` |
| Crew (hire/board/transfer/colony posting) | PLAYABLE (no XP by design) | Program → CREW |
| Colonies (view, logistics, inspect, abandon, found starter outpost) | PARTIAL — you cannot *design* a colony; `FoundColony` spawns a fixed solar+battery prototype | Program → COLONIES, `data/systems/home/colonies.ron` |
| Labs (data→science over time) | PLAYABLE | `csp.lab.mk1`, Program → LABS |
| Manufacture + test stand (qualification) | PLAYABLE | Program → FACILITIES, `RunBenchTest` |
| Reliability (failures, wear, misfires, dormant hazard) | PLAYABLE | Program → RELIABILITY, `csp-sim/src/reliability.rs` |
| Timewarp ladder with spoken refusals | PLAYABLE | §7 |
| Save/load/quicksave/revert | PLAYABLE | §8 |
| Alarms | PLAYABLE | Planning panel + AGENDA |
| Scripting (Rhai console + stock scripts) | PLAYABLE | AUTOMATION section |
| Autopilot | PARTIAL — ascent only | AUTOMATION section |
| Docking | PARTIAL — `csp.dockport.s125` part + soft-capture/latch physics exist (tech.rocketry.3); no docking UI/indicators; end-to-end docking not exercised by any shipped flow (unverified in play) | `csp-sim/src/joints.rs`, `data/parts/csp.dockport.s125.ron` |
| Life support | HIDDEN-by-default — implemented, off unless enabled for new games; nothing forces engagement | `DifficultySettings.life_support_enabled`, `life_support.rs` |
| Planes / rovers / stations (pillar-1 breadth) | MISSING — no wings, wheels, cargo bays; catalog is a rocket stack | `data/parts/` |
| N-body, CommNet, EVA, multiplayer, LLM assistant | MISSING (post-v1 by plan) | PLAN §1 non-goals |
| MCP server + telemetry SPA | PLAYABLE for agents (native, separate surface) | `csp-server`, `apps/telemetry` |
| Web demo | PARTIAL — 60-part cap, no craft IO/saves | `csp-view/src/demo.rs`, `apps/demo` |
| Sound | PLAYABLE — synth bank, engine/stage/impact/UI cues | `csp-app/src/{audio,synth}.rs` |

### The six LIVE USER REPORT complaints (`.review/session-prd1/STATUS.md`) — verdicts in this tree

| # | Complaint | Verdict | Evidence |
|---|---|---|---|
| 1 | Science/recovery undiscoverable — no part menus | **FIXED** | Right-click part menu (`part_menu`), always-visible Actions strip (`flight_action_bar`), clickable status strip, tutorial science tier |
| 2 | UI widgets fixed-position, can't move | **PARTIALLY FIXED** | Flight HUD block and navball are movable Areas (`flight_hud`), status strip movable, Game strip drag-anywhere, every panel a standard draggable egui window — **but** the readout block cannot be dragged by its own text (the row labels at `csp-ui/src/lib.rs:786–810` are selectable `ui.label`s that win the hit-test over the Area's move widget — drag it by its margins/edges), and the navball Area **parks ~242 px too high on first open** (no `default_size`, `csp-ui/src/lib.rs:1018–1024`; it overlaps the readout block until dragged). Both verified in session-full-20261026 |
| 3 | Sky grainy on the ground, stars bleed in early | **ADDRESSED IN CODE, visually unverified** | Per-step jittered 12-tap inscatter kills band/grain and a hard `smoothstep(6e-4, 4e-3)` star gate guarantees zero stars under any blue sky (`csp-render/src/terrain.wgsl` ~283–296 and ~1098–1105) |
| 4 | Warp refuses silently | **FIXED** | `WARP REFUSED — reason` on every gated press + latched `RAILS WARP ×1 — reason` HUD notice + cap/budget notices (`csp-app/src/lib.rs: warp_step_request/on_step_report`) |
| 5 | Yellow notices unreadable (no bg, wrap to 2 lines), flight data contrast | **FIXED** | Notice stack wears the shared dark chip and lays out single-line (`TextWrapMode::Extend`); the whole telemetry column is chip-backed (`notice_overlay`, `hud_frame`) |
| 6 | Research/science UI confusing | **PARTIALLY FIXED** | Tutorial teaches the loop; EC cost and latched gates are disclosed on hover; situation context line names body/biome/situation. Residual: it is still one dense Program window — usability beyond this is subjective and open |

### Known issues (verified session-full-20261026)

Live defects confirmed against this tree (verified reports C17/C23):

- **Readout block not draggable by its text** — the flight-HUD row labels are selectable
  `ui.label`s that win the hit-test over the Area's move widget (`csp-ui/src/lib.rs:786–810`);
  drag the block by its margins/edges instead.
- **Navball parks ~242 px too high on first open** (no `default_size`,
  `csp-ui/src/lib.rs:1018–1024`), overlapping the readout block until dragged.
- **Tutorial rewinds on its own coached path** — recovering the vessel trips the unscoped
  `vessel lost` rewind (`csp-ui/src/tutorial.rs:264–267`) and re-blocks the science tier until
  a new launch (see §5 item 7).

---

## 11. Troubleshooting / FAQ

**Why won't warp engage?**
Read the toast. `WARP REFUSED — VESSEL UNDER THRUST`: cut throttle (X) — only an in-bubble
vessel can be under thrust (leaving the bubble zeroes it; on rails there is no throttle).
`IN ATMOSPHERE`: climb above 70 km or land and
settle (latched craft never block warp). `LIVE CONTACT`: you're touching something. `SCRIPT N
IS POLLING`: pause or unload it (AUTOMATION). `WARP ALREADY AT ×N — SETTINGS LIMIT`: raise
*max time warp* in Settings. `WARP LIMITED`: your machine's step budget — it will engage if
the scene lightens.

**Why did my launch "dead-refuse"?** (you stay in the VAB with a notice)
One of: a part is **tech-locked** (research the node or swap the part); **insufficient funds**
(craft cost vs on-hand is printed); **insufficient metal/composite at terrin-launch** (the pad
starts with 2000 metal / 400 composite; recover vessels at the pad to refill at 100%
efficiency); **vessel cap reached** (recover/terminate something); **launch site occupied by an
active vessel** (a *parked* one is auto-recovered for you — only a flying/rails vessel blocks);
craft failed to resolve (is the catalog loaded?). All from `launch_preflight`, all on the
notice row.

**Where did my funds go?**
Launch debits **at the LAUNCH click** (build order), not at delivery. Bench tests debit
₣50+1 metal per unit. Hired crew draw salary every 30 sim-days. Failed contracts pay
penalties. Money comes *back* via contract advances/completions (credit at accept), science
milestones (₣10/⚛ banked), and vessel recovery (part value × site efficiency + resources).
Funds clamp at zero; insolvency demotes warp and notifies.

**I clicked LAUNCH but nothing is on the pad.**
Check the notice: "BUILD ORDER PLACED — delivery in ⟨time⟩". The craft materializes latched
after `mass/50 kg/s` sim-seconds (a ~4.5 t Hopper is ~90 s). Watch FACILITIES for the
countdown; warp (you're latched — it won't refuse) to skip it.

**The pad craft won't move.**
It's latched on purpose. Press **Z** (throttle full) — that unlatches and launches. WASD does
nothing while latched.

**Why can't I pick up the root part in the VAB?**
By design — the root defines the build frame. Use **Reroot** (inspector) to move the origin to
another part, or grab a non-root part, or use whole-craft translate. If you deleted the wrong
thing: **Ctrl+Z**.

**Transmit is greyed out.**
Not enough ElectricCharge — hover shows "needs N EC (have M)". Add a `z1k` battery (1,000 EC)
or recover instead (needs landing + settle). A "tx'd" row has already been transmitted;
recovering banks the remaining 60%.

**Recover vessel is greyed out.**
The vessel must be **latched** — landed *and settled* (a couple of seconds at rest). Chute
deploy is the opposite: only while flying. Neither works on rails: press **T** first.

**My data row says the key is closed.**
Recovery closes a science key once its full value is banked; further runs at that exact
(body, biome, situation) key yield diminishing amounts (≈28/49/64% of the Terrin cap over
three 10-unit runs). Change situation
(landed vs space) or body to open fresh caps.

**Rescue contract won't complete though I'm on the body.**
It completes on the **recovery event**, not on landing — press RECOVER VESSEL there.

**How do I deorbit / come home?**
Focus the vessel in map, press **T** (`RecoverControl`) to pull it off rails into the bubble,
then fly the entry or set a retrograde maneuver node — the executor flies it automatically.
Deploy the chute below ~6 km / 260 m/s.

**The game says the session is read-only.**
A fatal sim error halted the clock (crash dialog with a bug-report bundle path). Nothing can
be saved from that state; restart or load a save.

**Where are my files?**
`$XDG_CONFIG_HOME/csp/` (Linux; `~/Library/Application Support` on macOS, `%APPDATA%` on
Windows): `settings.ron`, `crafts/*.cspcraft`, `saves/*.cspsave`.

---

*Verification method: every section above was confirmed against the working tree on
2026-10-26 by direct read of the cited files (post-`session-prd1` phases 1–2 landed, including
the formerly-uncommitted csp-app/csp-ui/csp-view changes, now committed). No gameplay was
executed: purely visual qualities (sky, "feel") **and** interaction/feel claims (e.g. widget
drag behavior) are marked unverified or carried as known issues where direct code read cannot
settle them.*
