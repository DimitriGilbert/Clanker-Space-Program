# Review report C04 — csp-sim lib.rs §C (L8620–11880): frame / hash / restore / breakup

Session `session-full-20261026`, target = current working tree (`7dd4b86`). Full range read;
cross-checked against `World`/`Vessel`/`WorldSnapshot` definitions (L292, L2183, L1390),
`World::restore`/`snapshot`/`check_serializable` (L2926/3556/3497), and every per-module hash
walk (research/tech/economy/contracts/colonies/manufacture/scripts/reliability/transfers/joints).
One finding verified empirically with a temporary in-crate test (file restored afterwards;
working tree clean).

---

### [HIGH] Finding 1: Poison save — an in-flight crew transfer whose source is lost makes the save permanently unloadable

**File**: `crates/csp-sim/src/lib.rs:9952-9967` (`restore_transfer_state`, seated-at-source gate)

**Problem**: the restore gate demands every in-flight crew member still be seated at the
transfer's source (`t.from` carries the member in its `crew` vec), but several *legitimate*
producers legally break that invariant while the transfer record and its `TransferComplete`
event survive:

- `remove_vessel` (L4997-5046, driven by `TerminateVessel` L5624, crash/impact L4600, LS
  depletion, SOI escape) — the seat record dies with the hull (`lose_aboard_crew`,
  economy.rs:588), the transfer + event deliberately survive (`event_names_vessel` L9851-53
  keeps `TransferComplete`).
- `apply_breakup` (L11252) — `reseat_roster` (L11549) moves the member's seat record to a
  fragment while `t.from` still names the retained hull id; thermal destruction of the seat
  node removes the seat from the retained `v.crew` (L11639).
- `apply_jettison` (~L7193-7197) — jettisoning the seat node kills the seat record; the
  in-flight transfer is not cancelled.

**Evidence** (empirical): temporary test built on the existing `physics_world`/`crew_craft`
helpers — board crew on A and B, `TransferCrew A→B` (in flight, 640 ticks), then
`TerminateVessel{A}` (a plain legal verb). `snapshot()` succeeds (§7.4 boundary inspects no
transfers), then `World::restore` fails:

```
Fatal("snapshot: in-flight crew is not seated at the transfer's source")
```

**Impact**: any save taken in that window can never be loaded again — silent player data
loss in the Phase4 save/load backbone. The gate also contradicts the module's own doc at
L9896-9898 ("Vessel ids may name *removed* vessels — a mid-flight removal is legal; the
completion's fallback resolves it"): resource transfers with a dead source restore fine,
only crew cargo trips. The existing `save_load_mid_transfer` test (L20715) covers resource
transfers only; the crafted-save tests (L13513+) never exercise the legit sequence.

**Suggestion**: the double-booking hazard the gate guards against (report-04 D7) is already
prevented at fire time — `land_crew` (transfers.rs:1162-1189) only moves a member it *finds*
seated at the source, so a lost seat degrades to the documented no-op. Drop the
seated-at-source requirement (keep `crew.0 < next_crew_id` and the in-flight-twice check),
or narrow it to "member is `Active` in the roster AND seated on some *other* live vessel".
Add the legit-sequence regression test (transfer → terminate source → snapshot → restore →
step to completion).

---

### [MEDIUM] Finding 2: state hash excludes serialized fields beyond §7.1's sanctioned exclusion list

**File**: `crates/csp-sim/src/lib.rs:10915-11061` (`hash_craft`), `:8767-8893` (`state_hash_masked`)

**Problem**: PLAN §7.1: "Exclusion list = exactly §7.3, plus session state (§2.2 category C)
and the `SimEvent` ring buffer." Two serialized/validated fields are hashed by nothing:

- `CraftDef::part_ids` — restore-validated (L10598: empty or parallel to `parts`), merged in
  the latch merge (joints.rs:943-951), read by telemetry (L9562) and the manufacture tech
  gate at apply time. `hash_craft` walks graph/parts/part_costs/reliability but never
  `part_ids`.
- `PhysicsConfig::bubble_radius_m` — a balance tunable that changes state evolution (the
  auto-rails floor, L4824 `atm_top.max(physics.bubble_radius_m)`); the codebase's own rule
  ("construction config that changes state evolution — hashed like `caps`", applied to
  `docking`/`transfer_cfg`/`economy_cfg`/…) is not applied to it.

**Impact**: two worlds differing only in a stored craft's `part_ids` hash identically
forever (part_ids never feeds tick evolution, so no determinism proof weakens today);
`bubble_radius_m` drift is only caught indirectly once behavior diverges. It is a §7.1
letter violation in the contract the goldens rely on, not a live determinism hole.

**Suggestion**: add `h.vec(&craft.part_ids, |h, id| h.str(id.as_str()))` to `hash_craft`
(after the reliability table), and hash `bubble_radius_m` next to the `docking` tunables in
`state_hash_masked` (requires reaching physics config — it is immutable post-construction,
so hashing it via `self.physics.as_ref()` is sound). Bump `HASH_SCHEMA_VERSION` with the
golden rebless in the same commit, per §7.1.

---

### [MEDIUM] Finding 3: restore never enforces `notification_cap` on the serialized notification log

**File**: `crates/csp-sim/src/lib.rs:3480` (`notifications: snapshot.notifications`)

**Problem**: `reliability_cfg.is_valid()` is checked (L10619) but nothing bounds
`snapshot.notifications.len()` against `reliability_cfg.notification_cap` (default 256,
reliability.rs:260/288). The cap only evicts oldest-first on *push* (the writer's rule), so
a crafted save plants a deque of arbitrary size that persists indefinitely.

**Evidence**: the CSPSAVE codec bounds the *inflated body* at `MAX_INFLATED_BODY = 256 MiB`
(save.rs:111) — a crafted 200 MB notification log passes the codec and lands in the world
verbatim; every later `hash`/`snapshot`/shadow-capture (TickShadow L1819-1821 clones it)
carries the bloat.

**Impact**: bounded (256 MiB ceiling) but cheap-to-prevent memory amplification and
save-file bloat re-emission on the next save; inconsistent with the per-arena hardening
style of every other restore gate.

**Suggestion**: at restore, `if snapshot.notifications.len() > cap { return
Err(Fatal("snapshot: notification log over cap")) }` — or truncate to the last `cap`
entries (matching the writer's eviction) — alongside the `reliability_cfg` validity check.

---

### [MEDIUM] Finding 4 (DRY/CRAP): three copy-paste idioms across the restore/breakup family

**File**: `crates/csp-sim/src/lib.rs` (sites listed below)

**Problem / Evidence**:
1. The record↔event pairing match is re-written 8+ times, always the same shape
   `(Some(&t), Some(&e)) if t == e => {} _ => Fatal`: L9970-9977 (transfers),
   L10031-10038 (builds), L10152-10159 (research pair), L10279-10287 (contracts),
   L10511-10519 (colony inspections), L10636-10642 (part failures), L10658-10662
   (benches), plus the inline node/wake pairings at L3282-3291 and L3332-3342.
2. The heap-scrub idiom `mem::take(heap).into_iter().filter(…).collect()` appears 3× in the
   range: L9731-9736, L9755-9760, L11713-11724.
3. `apply_breakup` (L11252-11728, 475 lines / 17 params) repeats the
   `filter(|(k,_)| frag_nodes.contains(k)).map(...).collect()` partition seven times for
   temps/seams/tanks/reliability/crew/science/deployed (L11448-11521).

**Impact**: every new paired arena (the map counts 10 and growing) re-implements the same
gate; a missed arm is exactly the class of silent-coverage bug this review hunts.

**Suggestion**: (1) one generic
`fn check_pairing<K: Ord + Copy, V>(records: &BTreeMap<K, V>, events: &BTreeMap<K, u64>,
tick_of: impl Fn(&V) -> u64, what: &str) -> Result<(), SimError>` (plus a both-directions
variant) collapsing all sites to one-liners; (2) `fn scrub_events_where(heap: &mut
BinaryHeap<ScheduledEvent>, pred: impl Fn(&ScheduledEvent) -> bool)`; (3) a small
`fn partition_by_nodes<K: Ord + Copy, V: Clone>(src: &BTreeMap<K, V>, nodes:
&BTreeSet<PartNodeId>) -> BTreeMap<K, V>` for the breakup/undock/merge partition family.

---

### [LOW] Finding 5: §7.4 save-refusal rule 3 (pending Stage-2 weld) has no implementation — currently unreachable

**File**: `crates/csp-sim/src/lib.rs:3497-3549` (`check_serializable`)

**Problem**: §7.4 names four refusal conditions; the code implements 1 (live joints),
2 (live contacts, incl. the any-contact reading with `bubble_vessel_contact`), and 4
(save-blocked session). Rule 3 ("a Stage-2 weld is pending for a vessel in or about to
enter atmosphere") has no code. In the current architecture Stage-2 aero tables are built
synchronously on the sim thread (`ensure_aero`), so no "pending weld" state exists — the
refusal is *unreachable*, not silently dropped.

**Impact**: none today; it becomes a live gap the moment the §2.4 worker-thread Stage-2
path lands (the `InstallAeroTables` command already exists for it).

**Suggestion**: none required now; leave a `// §7.4 rule 3:` pointer at
`check_serializable` naming the future condition so the worker-thread work doesn't forget
the boundary check.

---

## Verified-good (checked, no finding)

- **Hash coverage of the new arenas is complete**: colonies (stores/state/modules/fractions/
  t_eq/crew/inspection/solved_tick — colonies.rs:1259-1317), builds (manufacture.rs:65-78),
  scripts+autopilots (scripts.rs:581-634), science (research.rs:418-427), economy
  (economy.rs:435-459), contracts (contracts.rs:534+), reliability records/config/benches
  (reliability.rs:605-678), transfers, and on-vessel `deployed`/`joined_colony`/
  `control_lease` (L11788-11829). All config structs (VesselCaps/Docking/Transfer/Science/
  Economy/Contract) hash field-complete. `StateHasher::arena` implements §7.1 rule 5
  (len, ascending id, id prefix, duplicate fault — hash.rs:424-444). Documented exclusions
  (command_log §7.6, ledger, `period_s` ±inf-marker, SimEvent ring) are each justified
  in-place.
- **Restore is atomic**: every gate runs against the snapshot before `Self { … }` is
  constructed (L3408); no half-restored world on error. Arena counters checked
  per-entry (transfers/builds/benches/contracts/instances) or via max-key (vessels/
  colonies/scripts), overflow-safe per the L2962 comment.
- **Config-channel reinstall seam is defended**: `restore` empties all nine data/engine
  channels by contract; `install_config` (L2715) covers all seven data channels,
  `missing_config_channels` (L2742) is an honest postcondition, and the player path
  (`rebuild_from_snapshot`, csp-view/src/lib.rs:1574-1602) installs everything and surfaces
  the warnings.
- **Breakup consequence completeness** (§2.4): momentum split with the jettison-convention
  velocity stamps (L11425, L11601); ancestor-first claim ordering + deepest-first member
  assignment prevents double-claimed nodes (L11341-11407); fragments inherit the parent
  aero table and are invalidated by the `graph_hash` staleness check (L11091-11110);
  contents/crew/science/reliability/deployed/seams all partition by node membership;
  departing research projects finalize with escrow landing on the carrying fragment
  (L11670-11725); `dead` vessels get `tracked_vessels`+`autopilots` scrubbed by the caller
  (L4844-4853) — no orphaned mirrors. Fragment cap counts `pending_builds` exactly like
  `vessel_slots_in_use` (L11415 vs L2632).
- **Parachute decel refresh** (L8911-8944) is a pure per-tick recompute invoked in the
  bubble step for every craft-carrying vessel (L4251), so every re-weld/materialization
  path (restore included) is covered with zero per-path bookkeeping; `WeldedBody::decel`
  being `#[serde(skip)]` is coherent with that.
- **§3.4 revert semantics**: restore reinstates reliability records verbatim (pre-drawn θ
  untouched — L10738-10757 only validates), so revert cannot reroll outcomes.
- `step_frame` (L8620-8740): budget arithmetic cannot underflow (t_next clamped by
  `now + tick_budget`), flush iterations are bounded by the event budget, and
  `TickShadow::capture` is cheap/lazy (L1824-1863) — no per-tick clone cost.

## PRD coverage table (this range)

| PRD rule | Status | Evidence |
|---|---|---|
| §7.1 hand-written visitor, rule 5 arenas, canonical walk | **WIRED** | `state_hash_masked` L8767-8893; `arena` helper hash.rs:424 |
| §7.1 exclusion list "exactly §7.3 + session + ring" | **DEGRADED** | `craft.part_ids` + `bubble_radius_m` unhashed (Finding 2); notifications/ledger/period_s exclusions documented in-place |
| §7.1 bubble mask (§2.6.1 equivalence) | **WIRED** | `state_hash_masked` + `csp_core::bubble_mask` L8801-8806 |
| §7.2 serialized set completeness | **WIRED** | `WorldSnapshot` L1390-1552 covers every §7.2 bullet incl. seams, latch poses, notification log, offer_index |
| §7.3 re-derived-on-load caches | **WIRED** | re-weld L10832, aero/thermal caches cleared L3461-3467, data channels re-handed |
| §7.4 refusal rules 1/2/4 | **WIRED** | `check_serializable` L3497-3549; rule 2 uses the any-contact reading |
| §7.4 refusal rule 3 (pending Stage-2 weld) | **UNREACHABLE** (no pending-weld state exists; see Finding 5) |
| §7.5 save→load→N hash equality | **WIRED** | e.g. `save_load_mid_transfer` L20744; atomic restore |
| §7.7 "never silently half-loaded" | **WIRED** | all-or-nothing restore with named Fatal reasons |
| §2.4 breakup consequences | **WIRED** | Finding-1's reseat side-effect aside, see verified-good |
| §3.4 revert keeps pre-drawn thresholds | **WIRED** | θ restored verbatim |

## Top 3 issues

1. **[HIGH] Poison save** — in-flight crew transfer + source loss (termination/crash/
   breakup reseat/jettison) → `snapshot()` accepts, `restore` Fatals; save unloadable
   forever (L9952; empirically reproduced).
2. **[MEDIUM] §7.1 exclusion-list violation** — `craft.part_ids` and
   `PhysicsConfig.bubble_radius_m` serialized/evolution-relevant but never hashed
   (L10915, L8767).
3. **[MEDIUM] `notification_cap` not enforced at restore** — crafted save may plant an
   arbitrarily large (codec-bounded 256 MiB) notification deque (L3480).
