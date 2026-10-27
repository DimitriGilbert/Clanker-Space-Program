# Review C02 — csp-sim lib.rs §A: command contract + World shell (L1–3634)

Scope: `crates/csp-sim/src/lib.rs` L1–3634 (module decls, Vessel/Lease/SourceClass, CommandKey,
SimCommand enum L534–1215, AdvanceReport, WorldSnapshot, WorldConfig, World + constructors +
set_* installers + install_config/missing_config_channels + restore + check_serializable +
snapshot). PRD sections: §2.2, §7.2, §7.4 (+§2.6.5 for the tracking opt-out), §6.4 for sanction
checks. Everything below verified against the current working tree.

---

### [SEVERITY: HIGH] Finding 1: `SetVesselTracked` opt-out lies — the PRD's "focused vessel SoiTransition clamps warp" is unreachable for players
**File**: crates/csp-sim/src/lib.rs:564–565 (opt-out), :5289–5317 (only mutator), crates/csp-sim/src/events.rs:451–456 (classing)
**Problem**: The opt-out reads "an MCP/agent tracking verb — the player's focus+tracking is view-side session state, not a sim command." That justification is false. PLAN §2.6.5 (PLAN.md:543) lists **"focused/tracked vessel `SoiTransition`"** as a `Clamping` member ("auto-demotes warp so the player cannot overshoot"). Event class lives on the sim-side heap entry (`events::class_of`: `SoiTransition` is `Clamping` **iff** `tracked.contains(vessel)`), and the only production mutator of `tracked_vessels` is the apply of `SimCommand::SetVesselTracked` itself (lib.rs:5295). Grep across csp-view/csp-ui/csp-app/csp-server/csp-bench/csp-script/csp-ai: **zero issuers**. View-side focus (`csp-view/src/map.rs:273 focus_vessel` session state) cannot influence the sim-side class — `csp-app/src/lib.rs:1260` demotes warp only on `e.class == EventClass::Clamping` from `fired_events`.
**Evidence**: csp-app/src/lib.rs:1257 comment concedes it: "a fired Clamping event demotes warp … (PlayerAlarm, **tracked-vessel** SoiTransition)" — and nothing ever tracks.
**Impact**: In the shipped game a player warping at ×10⁵ through their own moon encounter gets no demotion — exactly the overshoot §2.6.5's Clamping class exists to prevent. Mitigation exists only if the player manually sets an alarm at the SOI tick. The opt-out hides a DEGRADED feature behind a claim that the player case is covered elsewhere ("view-side") when it is covered nowhere.
**Suggestion**: Either (a) make the view auto-issue `SetVesselTracked{focused, true/false}` on focus change (the command exists, is logged, and restamps queued SOI events — lib.rs:5299–5316 already handles that), or (b) reclassify `SoiTransition` of the newest/focused vessel at schedule time. Then the annotation becomes true. If neither, the annotation must say "player warp-clamp on SOI is not implemented; only manual alarms clamp" instead of pointing at a view-side mechanism that cannot do the job.

### [SEVERITY: HIGH] Finding 2: `active_vessel` does not exist as sim state — §2.2 and §7.2 both name it, unsanctioned by §6.4
**File**: crates/csp-sim/src/lib.rs:2183–2432 (World struct — no field), :1389–1552 (WorldSnapshot — no field)
**Problem**: PLAN §2.2 (PLAN.md:195–196): "`active_vessel: Option<VesselId>` is **sim state** (it gates control authority), changed only by command." PLAN §7.2 (PLAN.md:1627) lists `active_vessel` in the serialized set. The word appears nowhere in csp-sim; the concept lives as `focus_vessel` session state in csp-view (map.rs:273, flight.rs:1965). This deviation was explicitly adjudicated INTO the plan by the prior review round (REVIEW2-normal.md H17 fix: "move 'active vessel / control authority' into sim state, since it gates control"; M49: "`active_vessel` is sim state, changed only by command… dashboards display it").
**Evidence**: `grep -rn active_vessel crates/csp-sim` → nothing. Focus after load falls back to "newest spawn takes the camera" (flight.rs:1960–1966) — a heuristic the doc itself admits hands focus to "a new breakup fragment". The save payload (csp-view `save_payload`, csp-ui/game.rs) carries no focus. §2.3's bubble references (PLAN.md:267/269: "radius of `active_vessel`", "recentre when the active vessel's lattice cell changes") are likewise unserved — the bubble is body-centered.
**Impact**: PRD-named serialized state is missing without §6.4 sanction; the player-visible half is that save/load does not restore which vessel you were flying (camera/HUD jump to the newest vessel on every load), and no doc comment anywhere records the deviation or its rationale. The control-authority half of the PRD rationale IS served (per-vessel `Lease` + explicit `VesselId` addressing make a global active vessel unnecessary for gating — a reasonable supersession, but an undocumented one).
**Suggestion**: Minimum honest fix: persist focus in the §7.8 sidecar (game settings) or add `active_vessel` to WorldSnapshot changed only by a logged command (the PRD's letter). At minimum, add a doc comment at `World`/`WorldSnapshot` stating the deviation ("focus is view-side session state; `control_lease` is the authority gate") so PLAN vs code divergence is discoverable.

### [SEVERITY: MEDIUM] Finding 3: `SpawnVessel` opt-out names a variant that does not exist (`BuildVessel`)
**File**: crates/csp-sim/src/lib.rs:552–553
**Problem**: The opt-out says "the launch path uses `BuildVessel` + `SpawnPhysicalVessel`". `BuildVessel` appears nowhere in the codebase except this comment (verified by grep across crates/). The player launch path is `ManufactureCraft` → `ConstructionComplete` (csp-view/src/lib.rs:3018–3025), with `SpawnPhysicalVessel` as the site-less sandbox fallback (csp-view/src/lib.rs:3037).
**Evidence**: `grep -rn BuildVessel crates/` → only lib.rs:552. The conclusion ("elements-only spawn is MCP/test-only") is still true — csp-view hits for `SpawnVessel` are in test ranges (lib.rs:5630, map.rs:3257), and MCP's `sim.command` (csp-server/src/mcp.rs:532) can issue it.
**Impact**: The opt-out ledger is the honesty mechanism; an annotation whose stated reason references a non-existent API proves `lint-reach` checks presence, not truth, and misleads the next reader about the launch architecture.
**Suggestion**: Rewrite: "the launch path is `ManufactureCraft` (sandbox fallback `SpawnPhysicalVessel`); elements-only spawn is MCP/test-only." Consider having `lint-reach` flag opt-outs containing unknown backticked identifiers.

### [SEVERITY: MEDIUM] Finding 4: §7.4 rule 3 (pending Stage-2 weld) is neither implemented nor documented as vacuous; `check_serializable` doc is stale
**File**: crates/csp-sim/src/lib.rs:3491–3550
**Problem**: PLAN §7.4 (PLAN.md:1663–1664) lists four refusal rules. `check_serializable` implements rules 1 (live joints, :3518), 2 (bubble contacts incl. correct vessel-vessel "any contact" reading via `bubble_vessel_contact(..., false)`, :3528–3548), 4 (loader-error taint, :3501), plus the queue-empty refusal — but rule 3 ("a Stage-2 weld is pending for a vessel in or about to enter atmosphere") appears nowhere and nothing says why. In the current architecture the rule is vacuous (aero tables are computed synchronously via `ensure_aero`, lib.rs:6846; `InstallAeroTables` attaches at apply), so this is not a live bug — but the function doc still says it applies "to the one M1 case that can violate it: undrained commands in flight" while the body checks four conditions, and the vacuousness is fragile: the prior review's §2.4.1 fix contemplates *incremental/deferred voxelization on wasm*, which would reintroduce exactly the pending-weld state rule 3 guards.
**Impact**: A future async Stage-2 path lands with no refusal hook and no doc trail saying the rule was ever considered; today's reader cannot tell rule 3 from forgotten.
**Suggestion**: Add a comment at `check_serializable`: "§7.4 rule 3 is vacuous in this architecture — Stage-2 tables compute synchronously (`ensure_aero`); if voxelization ever defers, this boundary must refuse." Fix the "one M1 case" lead-in to enumerate the four checks.

### [SEVERITY: MEDIUM] Finding 5 (DRY/CRAP): TickShadow's ~30 `Option` fields hand-rolled across capture/merge/restore plus four parallel field lists
**File**: crates/csp-sim/src/lib.rs:1703–2174
**Problem**: Every shadowed arena adds: a field on `TickShadow` (:1703–1822), an init in `capture` (:1827–1863), a near-identical `if self.X.is_none() { self.X = committed.X; }` arm in `merge_committed` (:1882–2009, `#[allow(clippy::cognitive_complexity, clippy::too_many_lines)]`), an `if let Some(v)` arm in `restore` (:2015–2173, same allow), plus parallel entries in `WorldSnapshot`, `snapshot()`, and `restore()`'s constructor (:3408–3488) — 6–8 touch points per new arena, of which all but `vessels`/`event_heap`/`pending` follow one identical "earliest-wins Option" pattern. The two `#[allow]`s are the codebase conceding the complexity budget.
**Impact**: This is the highest-drift surface in the crate: an arena added in one place and missed in another silently breaks rollback (exactly the class of bug the shadow exists to prevent), and the snapshot/constructor lists are the same hazard for save parity.
**Suggestion**: Replace the scalar block with a declarative macro that walks a field list once:
`shadow_fields! { joints: Vec<LiveJoint>, joint_refused: BTreeSet<..>, science: ScienceState, … }`
generating the capture-init, merge (earliest-wins), and restore (Option-apply) arms; keep `vessels`, `event_heap`, `pending`, and the counters as explicit special cases. Same shape (a `Shadowable` trait + fixed array of field-handles) also works and keeps the special cases as trait impls.

### [SEVERITY: LOW] Finding 6: Stale "M1 / no vessel removal" rationales on the arena docs
**File**: crates/csp-sim/src/lib.rs:1403–1406 (WorldSnapshot.vessels), :2216–2219 (World.vessels)
**Problem**: WorldSnapshot says "at M1 there is no vessel removal, so no free list or slot generations exist to serialize"; World says "the M1 slot/generation arena machinery lands with removal; with none, the map IS the arena". Vessel removal has existed since M4c/M7-C (`remove_vessel`, `RecoverVessel`, `TerminateVessel`, debris eviction).
**Impact**: The conclusion still holds — a `BTreeMap` arena with a monotonic counter genuinely has no free lists/generations to serialize (§7.1 rule 5) — but the stated reason is wrong and invites someone to "fix" a problem that no longer exists the way the doc describes.
**Suggestion**: Reword: "removal drops map entries; the BTreeMap arena keeps no free list or slot generations by construction, so §7.2's free-list/generation clause is satisfied vacuously."

### [SEVERITY: LOW] Finding 7: `let _ = reason;` dead-binding dance in `check_serializable`
**File**: crates/csp-sim/src/lib.rs:3501–3508
**Problem**: `if let Some(reason) = &self.save_blocked { let _ = reason; return Err(...) }` binds a variable only to immediately discard it; the comment explains the detail rides `save_blocked_reason()`.
**Impact**: Reads like a silenced lint; the honest form is clearer.
**Suggestion**: `if self.save_blocked.is_some() { return Err(SimError::SaveBlocking("session is save-blocked from a loader/install error")); }` (keep the comment).

---

## PRD coverage table

### §2.2 command ordering
| Item | Status | Evidence |
|---|---|---|
| CommandKey total order, wall-clock-free | WIRED | lib.rs:504–525; derived `Ord` over (tick, class, id, seq); per-source FIFO `pending` BTreeMap; no floats in key; `SourceClass` discriminants match declaration order |
| Stamped at dequeue, never production | WIRED | lib.rs:494–503; drain stamps (C03 drains in map-key order = CommandKey order) |
| Command log (§7.6 artifact) | WIRED | lib.rs:2198–2200, appended only by drain |
| `source_seq` counters serialized | WIRED | WorldSnapshot.source_seqs:1397 |
| Absolute-never-relative commands | WIRED | all 53 variant payloads checked — SetThrottle/SetAttitude/SetWarp/AdvanceTo absolute; rate integration app-side pre-command (csp-app held-axis) |
| Control lease (rejections on channel, Player preempts) | WIRED | Lease:458–478; `check_control_lease`:5527 (u32 clamp verified); `control_gated_vessel`:1243 with documented ungated set; rejected via `AdvanceReport::rejected`, no log entry |
| External ≤32 cmds/tick + bounded ingress | WIRED | csp-server/src/ingress.rs:36 `MAX_COMMANDS_PER_CLIENT=32` (server side per plan) |
| `active_vessel` sim state | **MISSING** | Finding 2 |
| HashMap/HashSet/std::time ban | WIRED | none in csp-sim src (BTreeMap/VecDeque/BinaryHeap only) |
| Next-ID counters monotonic + serialized | WIRED | 8 counters, all in WorldSnapshot |

### §7.2 WorldSnapshot coverage (full World↔WorldSnapshot diff)
| Item | Status | Evidence |
|---|---|---|
| tick, warp, leases, all arenas, next-ids, event heap+seq, RNG indices (offer_index), source_seqs, colonies/stores, research/lab, tech, contracts+facts, bubble poses, surface-latch, DockSeams, craft, scripts+persist, notifications | WIRED | every state field of World (L2183–2432) maps 1:1 into WorldSnapshot (L1389–1552); nothing settable-but-unrestored found; construction-config channels correctly excluded and re-installed via `install_config` (7 channels, both directions) + `missing_config_channels` postcondition |
| `active_vessel` | **MISSING** | Finding 2 |
| free lists / slot generations | N/A (stale doc) | Finding 6 |

### §7.4 save refusal
| Rule | Status | Evidence |
|---|---|---|
| 1 live joint island | WIRED | check_serializable:3518 |
| 2 bubble contacts, not latched (any contact incl. vessel-vessel) | WIRED | :3528–3548 |
| 3 pending Stage-2 weld | **VACUOUS, UNDOCUMENTED** | Finding 4 |
| 4 SaveBlocking loader errors | WIRED | save_blocked:2424–2431, :3498–3509 |
| (extra) pending-queue refusal | WIRED | :3510 (good addition; makes the §7.6 log reproducible from the snapshot alone) |

### The 9 `ui-unreachable:` honesty audit
| Line | Variant | Claim | Verdict |
|---|---|---|---|
| :552 | SpawnVessel | "launch path uses `BuildVessel` + `SpawnPhysicalVessel`" | **STALE** — `BuildVessel` does not exist; launch = `ManufactureCraft` (sandbox fallback `SpawnPhysicalVessel`); conclusion (MCP/test-only) still true (Finding 3) |
| :564 | SetVesselTracked | "player's focus+tracking is view-side session state, not a sim command" | **LYING** — view-side focus cannot promote the sim-side event class; PRD §2.6.5 focused-vessel SOI clamping is unreachable in the shipped game (Finding 1) |
| :631 | Unlatch | "surface-ops UI future scope; MCP/benchmarks issue it" | HONEST — no UI verb; player path exists via throttle/attitude unlatch-first (`unlatched_body`, :5270/:5282) |
| :644 | ToRails | "auto-rails transition and bubble-exit path issue it internally; no player verb" | HONEST (wording nit) — internal paths call `apply_to_rails_at` directly (:4884/:4956) rather than issuing the command; auto-rails does cover the player |
| :674 | InstallAeroTables | "agent/perf channel; `ensure_aero` covers the player" | HONEST — `ensure_aero` (:6846) invoked at :4093/:6582/:6823 covers players; no actual perf-bin issuer exists today (aspirational "perf channel") |
| :692 | SetAeroConfig | "aero-config UI (bay doors) future scope; MCP only" | HONEST — no bay-door UI or parts |
| :778 | Undock | "docking port UI future scope; MCP/benchmarks only" | HONEST — `apply_undock` (:7273) is a full implementation (graph split, momentum stamps, seam/aero partition), **not** a no-op; seams are produced by the auto latch pass; only the UI is absent |
| :939 | ContractReward | "economy-harness stub; real funds path drives contract completion" | HONEST — completion credits via `FundsReason::ContractCompletion` directly (contracts.rs:1150/:1200), not via the command |
| :997 | DamageColonyModule | "stand-in for tests; M8-B's detection hook is the real path" | HONEST — `apply_surface_impacts` exists AND is wired in the tick path (lib.rs:4857, colonies.rs:1914) |

Tally: **7 honest / 1 stale / 1 lying**.

## Top 3 issues
1. Finding 1 (HIGH) — `SetVesselTracked` opt-out lies; PRD §2.6.5 focused-vessel SOI warp-clamp unreachable for players.
2. Finding 2 (HIGH) — `active_vessel` missing as sim state/serialized field (§2.2, §7.2; unsanctioned; focus lost across save/load).
3. Finding 5 (MEDIUM) — TickShadow 30-field × 3-site hand-rolled shadow machinery; macro-generate before the next arena lands.
