# Verified report C04 — verification of review-report-C04 (session-full-20261026)

Verification agent pass over `review-report-C04.md` against the current working tree
(`7dd4b86`). Every finding was re-traced through the source; the HIGH was additionally
re-reproduced empirically with a temporary in-crate probe test (`zz_probe_c04_poison_save`,
inserted into `crates/csp-sim/src/lib.rs`, executed, then reverted via `git checkout` —
working tree verified clean afterwards).

Score: **Finding 1 CONFIRMED (empirically), Finding 2 PARTIALLY CONFIRMED (half dismissed),
Findings 3, 4, 5 CONFIRMED** (Finding 3 with one factual correction). PRD rows re-checked:
rule-3 UNREACHABLE stands, exclusion-list DEGRADED stands on one field only, arena hash
coverage spot-check passes.

---

### Finding 1: Poison save — in-flight crew transfer + source loss → permanently unloadable save — CONFIRMED (empirically reproduced)

**Original summary**: the restore gate at `crates/csp-sim/src/lib.rs:9952-9967` demands every
in-flight crew member still be seated at the transfer's source; legal producers (TerminateVessel,
crash, breakup reseat, seat jettison) break that invariant while the transfer record and its
`TransferComplete` event survive; `snapshot()` accepts but `restore` Fatals — the save is
unloadable forever.

**Verification**:

(a) **`snapshot()` does not validate this invariant.** `World::snapshot` (L3556-3601) runs only
`check_serializable` (L3497-3549), which checks exactly four things: save-blocked session,
pending command queues, non-empty joint island, and live-contact bubble bodies. Transfers are
never inspected.

(b) **`restore` Fatals, does not skip/refund.** `restore_transfer_state` L9952-9967: the
`seated_at_source` lookup requires `snapshot.vessels.get(&v)` to contain a seat record with the
member's id (or, for a colony source, `CrewLocation::Colony(c)`); a missing source vessel yields
`false` and returns `Err(SimError::Fatal("snapshot: in-flight crew is not seated at the
transfer's source"))` verbatim.

(c) **All four loss paths are legal for the source of an active transfer.**
- Transfers are only ever removed by the completion event itself (lib.rs:8259 and
  transfers.rs:1052 — both inside the `TransferComplete` apply). No cancellation path exists on
  vessel loss, ToBubble, breakup, or jettison.
- `TransferComplete` explicitly survives vessel removal: `event_names_vessel` (L9834-9857)
  returns false for it, with the in-code comment "it survives vessel removal by design (the
  completion's fallback rule resolves the dead endpoint)".
- **TerminateVessel** (L5624-5657): no transfer interaction; `remove_vessel` (L4997-5046)
  drops the arena entry and `lose_aboard_crew` (economy.rs:588) kills the seat record. Legal on
  any vessel in any state (only a burning maneuver node is refused).
- **Crash/impact** (L4600-4607): inline `vessels.remove(&vid)` + `lose_aboard_crew`; transfer
  untouched.
- **Breakup** (`apply_breakup`, L11252): seats on detached subtrees leave the retained hull
  (`v.crew.retain` at L11639) and ride the fragment (construction at L11493-11497, roster
  reseated to the fragment id at L11549 via `economy::reseat_roster`, economy.rs:566-579);
  seats on exploded nodes go `Lost` (L11551-11559). `t.from` still names the original hull id.
- **Seat jettison** (`apply_jettison_part`, L7169-7192): `v.crew.retain(|c| c.seat != node)` +
  `lose_aboard_crew`; transfer untouched.
- Crucially, transfers legally exist between two **InBubble** vessels (proximity clause 2,
  transfers.rs:677-682) and two SurfaceLatched vessels (clause 3) — so every in-bubble loss path
  (crash, breakup, jettison) is reachable mid-transfer, not just TerminateVessel.

**Empirical reproduction (mine, independent of the reviewer's)**: probe test using the crate's
own `physics_world`/`crew_craft`/`enqueue_physical` helpers — spawn two 2-seat crafts 50 m
apart, `BoardCrew{A}`, `TransferCrew{A→B, c0}` (accepted, in flight, complete_tick ahead),
`TerminateVessel{A}` (accepted, no rejections), then:

```
snapshot()  -> Ok(...)                      (boundary does not refuse)
restore()   -> Fatal("snapshot: in-flight crew is not seated at the transfer's source")
```

Exactly the reported failure, via an entirely legal command sequence.

**The suggested fix's premise also checks out**: the double-booking hazard the gate guards
(report-04 D7) is already prevented at fire time — `land_crew` (transfers.rs:1162-1189) only
moves a member whose seat record it *finds* at `t.from` (`src.crew.iter().position(...)`),
degrading to the documented no-op when the seat is gone. The module's own doc at L9896-9898
("Vessel ids may name *removed* vessels — a mid-flight removal is legal; the completion's
fallback resolves it") is indeed contradicted by the gate for crew cargo while resource cargo
restores fine. Severity HIGH is justified: silent, permanent player data loss from a legal verb
sequence in the save/load backbone.

---

### Finding 2: state hash excludes serialized fields beyond §7.1's exclusion list — PARTIALLY CONFIRMED (`part_ids` CONFIRMED; `bubble_radius_m` DISMISSED)

**Original summary**: PLAN §7.1's exclusion list is "exactly §7.3, plus session state and the
SimEvent ring buffer", yet `CraftDef::part_ids` and `PhysicsConfig::bubble_radius_m` are
serialized but never hashed.

**Verification**:

- **`CraftDef::part_ids` — CONFIRMED.** It is serialized (rides `CraftDef` in vessels and
  builds), restore-validated (L10596-10600: "empty or parallel to parts" — exactly the report's
  L10598), maintained by the latch merge (joints.rs:943-951), and read at L9562 — but
  `hash_craft` (L10915-11061) walks only graph, parts (all resolved sub-structs), `part_costs`,
  and `reliability`. `craft_graph_hash` (L11077-11089) just delegates to `hash_craft`, so
  `part_ids` is covered by no hash anywhere. It is not a §7.3 re-derived cache (restored
  verbatim, not re-derived), not session state, not the ring — a genuine §7.1 letter violation.
  The report's own impact calibration ("no determinism proof weakens today" — part_ids feeds
  gates/telemetry, not per-tick evolution) is accurate.
- **`PhysicsConfig::bubble_radius_m` — DISMISSED as filed.** The finding's premise calls it a
  "serialized/validated field", but `WorldSnapshot` (L1390-1552) contains **no physics field at
  all** — `World::restore(orbits, physics, snapshot)` (L2926-2929) takes `PhysicsConfig` as a
  caller-supplied parameter, and the restore body rebuilds `Physics` from it (L3461-3467). The
  `physics` field the report may have been led by (L1563-1565) belongs to `WorldConfig`, the
  §7.6 *replay artifact*, whose doc says "data-derived construction config … the artifact
  carries it so a replay rebuilds the same construction" — and which is not hashed for any of
  its members. `bubble_radius_m` comes from `balance.ron` through the data set, the same channel
  as orbits/environment/atmosphere — none of which are hashed, per the documented §7.3
  "re-handed, not serialized" convention (in-code comment L3452-3454). It does change state
  evolution (the auto-rails floor at L4824 `atm_top.max(physics.bubble_radius_m)`; also L6276),
  and there is a real design-consistency argument that balance tunables the world owns
  (`caps`/`docking`/`transfer_cfg`) are serialized+hashed while this one is neither — but that
  is a design suggestion, not a §7.1 exclusion-list violation, because §7.1's exclusion list
  governs *serialized* state and this field is not serialized in the snapshot. The report's
  Impact line ("two worlds differing only in a stored craft's `part_ids` hash identically") is
  correct for the confirmed half; the PRD "DEGRADED" row stands, but on `part_ids` alone.

---

### Finding 3: restore never enforces `notification_cap` on the serialized notification log — CONFIRMED (with one correction)

**Original summary**: nothing bounds `snapshot.notifications.len()` against
`reliability_cfg.notification_cap` (default 256); a crafted save plants an oversized deque that
"persists indefinitely", bounded only by the codec's 256 MiB inflated-body ceiling.

**Verification**:

- Restore assigns the deque verbatim: `notifications: snapshot.notifications` (L3480), inside
  the `Self { … }` construction — no length gate anywhere in `restore` or
  `restore_reliability_state` (which validates `reliability_cfg.is_valid()` at L10619 and
  instance/event pairing, but never the log length).
- The cap is a writer's rule only: `push` + `while len > cap { pop_front }` at
  reliability.rs:760-762, and the same trim after the bubble-pass push at lib.rs:4492-4501.
  Default 256 at reliability.rs:288 (`notification_cap: usize`, L260). The cap value itself is
  hashed (reliability.rs:654), so a crafted save can pair a valid cap with an oversized log.
- The codec bound is as claimed: `MAX_INFLATED_BODY: usize = 256 << 20` (save.rs:111), enforced
  at inflate (save.rs:411). A ~200 MB notification log passes the codec and lands verbatim.
- The snapshot's own field doc (L1542) says "records, bounded by `notification_cap`.
  Serialized, never hashed" — restore does not enforce the documented bound.

**Correction**: "persists indefinitely" is overstated. The very next notification push after
restore runs the `while len > cap { pop_front }` trim (reliability.rs:761), which collapses the
deque to the cap in one shot; the TickShadow clone of the deque is likewise lazy (captured only
before the first push, L1819-1821, captured in `shadow_notifications` on demand). The real
impact window is "until the next reliability notification fires" — during which the world holds
the bloat and every save re-emits it. Fact pattern confirmed; severity is arguably LOW-to-MEDIUM
rather than solidly MEDIUM, but the cheap-fix recommendation is sound.

---

### Finding 4 (DRY/CRAP): three copy-paste idiom families across restore/breakup — CONFIRMED

**Original summary**: (1) the record↔event pairing gate re-written 8+ times; (2) the
`mem::take(heap).filter().collect()` scrub 3×; (3) `apply_breakup` repeats the
filter-by-node partition 7× and is 475 lines / 17 params.

**Verification** (all sites opened):

1. Pairing-gate family — confirmed at L9970-9977 (transfers, match form),
   L10031-10038 (builds, match form), L10152-10159 (research, two-event variant),
   L10636-10642 (part failures, if-form), L10658-10662 (benches, if-form),
   L3282-3291 (maneuver node, option↔event match), L3332-3342 (script wake, option↔event
   match). One nuance: the sites are *not* literally "always the same shape" — they fall into
   three variants (map-record↔event, option↔event, two-event), so the proposed single generic
   helper needs its 2-3 variants exactly as the report's own suggestion concedes. The family
   and its growth risk (each new paired arena re-implements a gate) are real.
2. Heap-scrub idiom — confirmed verbatim twice in `set_wake`/`cancel_wake`
   (L9731-9736, L9755-9760) and a third time in `apply_breakup`'s project finalize
   (L11713-11724).
3. `apply_breakup` — signature counted: **17 parameters** (L11252-11274) and **477 lines**
   (L11252-11728; report said 475 — immaterial). The seven filter-by-`frag_nodes` partitions
   confirmed at L11448-11521 (temps, seams, tanks, reliability, crew, science data/spent,
   deployed). The function carries explicit `#[allow(clippy::too_many_arguments)]` +
   `#[allow(clippy::too_many_lines)]`, i.e. the codebase itself concedes the size.

Maintainability finding, factually accurate at every cited site. MEDIUM severity is a judgment
call (pure DRY, no behavior bug), but it is honestly labeled and the suggested helpers are
concrete.

---

### Finding 5: §7.4 save-refusal rule 3 (pending Stage-2 weld) unimplemented — CONFIRMED

**Original summary**: PLAN §7.4 names four refusal conditions; the code implements 1, 2, and 4
plus the queue-drained precondition; rule 3 has no code because no pending-weld state exists
(Stage-2 aero tables build synchronously), making it *unreachable* rather than silently
dropped.

**Verification**: PLAN.md §7.4 ("When saving is refused") lists exactly the four conditions,
rule 3 = "a Stage-2 weld is pending for a vessel in or about to enter atmosphere". 
`check_serializable` (L3497-3549) implements: rule 4 (`save_blocked`, L3501-3509), the
drained-queue precondition (L3510-3514), rule 1 (joints, L3518-3522), rule 2 (contacts incl.
`bubble_vessel_contact`, L3528-4547) — no rule-3 check. No pending-weld state exists:
`ensure_aero` (L6846-6906) builds `build_aero_tables` synchronously inline on the sim thread
(L6883); the `InstallAeroTables` command (L676, apply at L6908) exists as the future
worker-thread install seam, as the report says. "UNREACHABLE (no pending-weld state exists)" is
the correct status; the suggested `// §7.4 rule 3:` pointer is a reasonable zero-cost action.

---

## PRD row verification

| Row | Verdict | Evidence |
|---|---|---|
| §7.4 rule 3 UNREACHABLE | **Confirmed** | PLAN §7.4 lists 4 conditions; `check_serializable` L3497-3549 implements 1/2/4; `ensure_aero` L6846-6906 synchronous; `InstallAeroTables` L676/L6908 is the future seam |
| Exclusion-list DEGRADED | **Confirmed, but half as wide as filed** | `part_ids` serialized+validated+unhashed (hash_craft L10915-11061) — real violation; `bubble_radius_m` NOT serialized (no physics field in WorldSnapshot L1390-1552; caller-supplied via restore L2926-2929) — not a §7.1 letter violation, only a design-consistency argument |
| New-arena hash coverage field-complete | **Confirmed on 3 spot-checks** | colonies: `hash_colonies` (colonies.rs:1259-1317) covers all 14 state fields of `Colony` (site/body/pose/state+reason/daylight/modules/stores/crew/t_eq/fractions/solved/inspection; `rotation_period_s` belongs to `ColonyBodyEnv`, documented never-hashed construction config); builds: `hash_builds` (manufacture.rs:65-78) covers all 4 `ActiveBuild` fields incl. full `hash_craft`; scripts: `hash_script`/`hash_autopilot` (scripts.rs:581-634) cover all 10 `ScriptState` fields and the autopilot enum+persist |

## Bottom line

The report is high quality. Its HIGH finding is real, empirically reproducible, and correctly
diagnosed (including the fix premise). One MEDIUM (Finding 2) is half-wrong in its premise
(`bubble_radius_m` is not serialized, so it is not a §7.1 exclusion-list violation), one MEDIUM
(Finding 3) overstates persistence but is otherwise accurate, and the DRY and LOW findings are
accurate at every cited site.
