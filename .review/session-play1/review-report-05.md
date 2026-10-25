# Review Report — P05: Stage plan (launch-captured firing sets)

Scope: `csp-view/src/flight.rs` (`StagePlan` L408–517, `craft_fingerprint` FNV L547–585,
HUD mirrors L350–357/1138/1152–1153), `csp-view/src/lib.rs` (`stage_plan_of` L1941–1974,
`fire_stage` L1764–1859, plan capture in `launch_craft` L1734–1759), `csp-app/src/lib.rs`
(`stage_press` L1614–1623, `Stage` dispatch L1809), `csp-sim/src/lib.rs`
(`apply_jettison_part` L6006–6142, dispatch L4552–4553), `csp-view/tests/stage_plan.rs`.
Cross-checked: `csp-phys/src/session.rs` (`stage_order` L1424–1430, `parts_in_stage`
L1406–1412, `children_ids` L925–931, `to_part_graph` L1508–1525), `csp-sim` vessel-id
allocator (`next_vessel_id` L1308, mint L4971–4975, L5135+, L6286+), `data/parts/
csp.decoupler.s125.ron:60`.

## Verified correct (no action needed)

- **Firing order is right.** `session.stage_order()` returns populated stages *descending*
  (highest fires first — KSP rule, session.rs:1424–1430); `stage_plan_of` pushes drop sets
  in that order (lib.rs:1951–1972) and `fire_stage` drains front-first via `remove(0)`
  (lib.rs:1844–1848). The `remove(0)`-vs-`pop()` comment is accurate.
- **The refusal matrix is complete and none of it pops.** Empty/expended
  (lib.rs:1791–1802), unspawned (`vessel_for → None`, :1811–1816), latched (:1819–1824),
  rails (:1825–1828), and gone-from-arena (:1829–1840, which also retires the plan) each
  land a `FlightSfx` cue before any mutation of `plan.stages`. The InBubble gate sits
  exactly where it must — *before* the pop — so `JettisonPart`'s InBubble-only contract
  (sim:6021) correctly defers the firing to a bubble-resident bound vessel; the
  `stage_plan.rs` test proves latched → throttle-unlatch → real fire.
- **`min_id` does its documented job against still-live older copies.** `vessel_for`
  binds the newest `id >= min_id` with matching fingerprint (flight.rs:501–511); any
  live vessel at capture has `id <= next_back`, so an older same-craft copy can never
  bind. Fingerprint drift after jettison is safe: the bound id is returned verbatim and
  never re-resolved (:497–500), and a *second* plan of the same session craft keys on
  the fresh craft's fingerprint while `min_id` excludes the mutated old vessel anyway.
- **Command ordering can't invert.** Spawn and every `JettisonPart` ride the same
  `(Ui, PLAYER_SOURCE_ID)` FIFO (lib.rs:1742–1752, :1851–1855), so the launch's spawn
  always applies before any jettison issued later — the bind scan only ever sees
  already-drained vessels.
- **Session→graph id mapping is faithful.** `to_part_graph` preserves session node ids
  and parent links verbatim (session.rs:1508–1525), so the session-tree drop sets are
  valid `PartNodeId`s on the spawned vessel. Root can never enter a drop set (children
  of a decoupler; the sim's root-refusal at sim:6030 is unreachable from this path).
- **Per-node jettison order within a drop set doesn't matter.** `apply_jettison_part`
  reparents children to the removed node's parent (sim:6046–6062); ascending-id issue
  order still removes the whole subtree regardless of parent/child id ordering.
- **Read-only gate is present** (`stage_press`, app:1617–1622) with its own cue.

## Findings

### F1 — MEDIUM: `min_id` can't disambiguate same-craft vessels minted at/after it — a queued or scripted identical spawn steals the binding permanently

- **File:line:** `crates/csp-view/src/flight.rs:473` (`min_id` capture), `:501–514`
  (`vessel_for` — `.iter().rev().find(id >= min_id && fingerprint match)`); capture site
  `crates/csp-view/src/lib.rs:1741`.
- **Problem:** `min_id` is `vessels().next_back() + 1` — the max *live* id + 1. It only
  filters the live set; it says nothing about *which post-capture mint* is this launch's
  own. A `SpawnPhysicalVessel` enqueued *before* the plan's capture but still undrained
  mints `next_vessel_id >= min_id` and, for a relaunch of the unchanged session craft,
  matches `craft_fingerprint` exactly — so the newest plan binds the *previous* launch's
  vessel. Symmetrically, `.rev()` prefers the *newest* match, so any identical-craft
  vessel minted after this launch's own (a scripted producer) wins the binding over the
  real target. The doc at flight.rs:446–455 claims ids at/above `min_id` "were
  necessarily allocated after the launch was enqueued" — true of allocation *time*,
  false of *ownership*.
- **Evidence:** Two `View::launch_craft` calls inside one command-drain window (the API
  is `pub`; seeds/tests/scripted callers invoke it directly — the app can't produce it
  since `outbox.launch` is a bool drained once per frame and `sim_step` runs every
  frame): spawn A still queued at plan-B capture → `min_id_B = max_live+1`, spawn A
  mints exactly that id or higher, fingerprint matches → plan B binds vessel A
  permanently (`vessel` is never re-pointed, flight.rs:443–444). Worst case the older
  copy is pad-latched — every Stage press then refuses "NOT STAGED — PARKED ON THE
  SURFACE" while the just-launched craft can never be staged. `sim.next_vessel_id`
  (sim:1308, `pub`) would state the bound's intent correctly but cannot close the hole:
  pre-queued mints still satisfy it.
- **Impact:** Staging permanently wedged on, or fired at, the wrong same-craft vessel;
  the plan is consumed by jettisons off the wrong graph. Trigger is narrow in the app
  shell (needs two same-craft launches inside one `step_frame` drain, or a scripted
  identical spawn), but the failure is silent and sticky once hit.
- **Suggestion:** Since ambiguity between identical-fingerprint vessels is
  unresolvable by `(min_id, fingerprint)` alone, either (a) bind only on a *unique*
  candidate — stay unbound (retry next press) while >1 vessel matches, so the
  unresolvable case degrades to the "not spawned" cue rather than a wrong sticky
  binding; or (b) plumb the minted `VesselId` back from the spawn drain (the spawn and
  plan ride the same FIFO, so the binding is knowable at apply time).

### F2 — MEDIUM: `StuckDecoupler` can never gate a stage firing — the checked node is never the decoupler

- **File:line:** `crates/csp-view/src/lib.rs:1952–1968` (drop set = decoupler's
  *children*; the decoupler stays parent-side) vs `crates/csp-sim/src/lib.rs:6036–6043`
  (`v.reliability.get(&node).has_mode(StuckDecoupler)` keyed on the *dropped* node).
- **Problem:** `fire_stage` issues `JettisonPart` for every node *below* the decoupler;
  the decoupler node itself — which is what carries the `StuckDecoupler` record — is
  never in a drop set (unless nested under another decoupler, where it's payload, not
  the separating mechanism). So the sim's stuck-decoupler refusal is unreachable from
  the only player-facing separation path: a decoupler that drew the failure mode still
  lets its stage fire.
- **Evidence:** `data/parts/csp.decoupler.s125.ron:60` authors `(StuckDecoupler, 0.8)`
  on the decoupler part — the mode exists in shipped content and rolls onto the
  decoupler's `PartNodeId` via `reliability` at spawn. Nothing in `fire_stage` or
  `stage_plan_of` consults `v.reliability`, and the apply-side check looks at the
  dropped child (a tank/engine — whose authored modes won't include StuckDecoupler),
  so it never fires either.
- **Impact:** The M9b failure mode is dead weight for staging — players see a recorded
  stuck decoupler separate cleanly. The reverse edge also exists: a stacked decoupler
  *inside* a drop set that drew StuckDecoupler rejects its own removal → silent partial
  drop (see F3).
- **Suggestion:** Carry each firing's *decoupler* node ids in the plan entry alongside
  the drop set (`stage_plan_of` already visits them), and in `fire_stage` pre-check
  `v.reliability.get(&dec).has_mode(StuckDecoupler)` before popping — refuse with a
  `FlightSfx` cue instead of issuing doomed commands.

### F3 — LOW: The success cue counts issued commands, not applied removals — drain-time rejects leave a lying "STAGED" and a spent stage

- **File:line:** `crates/csp-view/src/lib.rs:1844–1858` — `fired = nodes.len()` then
  `STAGED — {fired} PART(S) JETTISONED` is emitted at *enqueue* time.
- **Problem:** `fire_stage` cannot observe drain outcomes, so any `JettisonPart` that
  rejects at apply (`Recoverable`, log-only) is counted as delivered. Concrete reject
  classes reachable from a captured plan: (a) drop-set node already gone — a decoupler
  nested inside an *earlier* stage's drop set takes its subtree with it, so the later
  stage's commands all hit "unknown part node" (sim:6027); (b) a dropped node with a
  StuckDecoupler record (F2's edge case); (c) TOCTOU — the bound vessel was InBubble at
  press but railed/removed before the drain (the check is at issue time, the apply is a
  tick later).
- **Evidence:** The refusal matrix proves cues work for *pre-check* failures, but
  post-pop rejects surface nowhere — the §7.6 log is the only record. A player firing
  the inner stage of a stacked-decoupler craft sees "STAGED — N PART(S) JETTISONED"
  for a firing that removed nothing.
- **Impact:** Silent stage spend with a false success cue — same "control does
  nothing" class the cluster fixed, one layer down.
- **Suggestion:** Pre-filter the pop against the bound vessel's live
  `craft.graph.nodes` (and the F2 reliability check) so `fired` counts what will
  plausibly drop; an empty effective set can land an honest cue like "STAGE ALREADY
  SEPARATED" instead of claiming N removals.

### F4 — LOW: `Stage` is a dead key outside Flight mode — and the cue channel can't render there anyway

- **File:line:** `crates/csp-app/src/lib.rs:1809` (`KeyAction::Stage if mode ==
  ViewMode::Flight`; the `_ => {}` at :1862 swallows it in Map/VAB);
  `crates/csp-view/src/lib.rs:383` (`flight_hud` — the only consumer of `FlightSfx`
  cues and of `stages_left/total` — runs only in Flight mode).
- **Problem:** In Map mode Space does nothing and says nothing. The throttle verbs use
  `mode != ViewMode::Vab` (app:1769, :1804) and the whole point of binding the plan to
  its vessel rather than flight focus (lib.rs:1771–1776) is decoupling staging from
  what the player is looking at — staging from map view is the canonical KSP pattern.
  Compounding it: even if the gate widened, `flight_notice` cues are invisible outside
  Flight (P04-F1 — cues age out unseen, `sfx_edges` ticks TTLs in every mode), and the
  STG readout itself is Flight-only.
- **Evidence:** Dispatch arm gated at :1809; no other arm catches `Stage`; `map_overlay`
  has no notice channel.
- **Impact:** A player in map view pressing Space gets a dead key — the exact
  playability class this cluster was landed to fix.
- **Suggestion:** Widen the gate to `mode != ViewMode::Vab` (no live plan exists in the
  editor anyway) — but pair it with a Map-visible cue path, or the refusal notices
  remain invisible per P04-F1.

### F5 — LOW: Each launch silently discards the previous launch's unexpended plan

- **File:line:** `crates/csp-view/src/lib.rs:1759` (`insert_resource(plan)` overwrites
  the single `StagePlan` resource).
- **Problem:** The docs frame the plan as belonging to "the vessel its launch spawned"
  (flight.rs:426–429), but the single-resource shape makes it per-*latest*-launch: a
  still-flying earlier craft's unfired stages become permanently unreachable — no
  notice, no per-vessel plan, and the gone-from-arena branch (:1829–1840) only covers
  the bound vessel, not a superseded one.
- **Evidence:** `StagePlan` is `#[derive(Resource)]` — one instance; `bound_to`/
  `vessel_for` model a single vessel. Nothing retains plan A when plan B is captured.
- **Impact:** Real playability surprise in the multi-vessel world (map focus cycling,
  recovery, etc. all acknowledge multiple live vessels): launch A, stage once,
  re-launch — A's remaining stages are silently gone.
- **Suggestion:** At minimum a `flight_notice` when a plan with `stages.len() > 0` is
  replaced ("PREVIOUS LAUNCH'S STAGE PLAN DISCARDED"); properly, a small
  `VesselId → plan` map keyed by the same lazy-binding rule.

### F6 — INFO: Pre-launch Stage press claims "SINGLE-STAGE CRAFT"

- **File:line:** `crates/csp-view/src/lib.rs:1797–1801` + `flight.rs:436` (default
  `StagePlan` has `total: 0`).
- **Problem:** With no launch ever performed, Stage lands "SINGLE-STAGE CRAFT —
  NOTHING TO STAGE" — text describing a craft that doesn't exist. Cosmetic; the
  distinction it draws (empty-expended vs empty-single-stage via `total`) is otherwise
  honest.
- **Suggestion:** Track `total == 0 && vessel.is_none() && never-captured` (or a
  `captured: bool`) and say "NO LAUNCHED CRAFT TO STAGE".

## Summary

The core machinery — lazy binding against live same-craft copies, firing order, the
deferred-pop refusal matrix, InBubble gating, and FIFO ordering — is correct and
well-tested where the tests reach. The real exposure is at the edges the tests don't
model: same-fingerprint mints the `(min_id, fingerprint)` key can't order (F1), the
decoupler-node reliability check the child-drop mapping bypasses (F2), and cue honesty
past the enqueue boundary (F3, F4).
