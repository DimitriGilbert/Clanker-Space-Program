# Fix Plan — session-deep1 Review Findings

## Overview

Synthesizes all CONFIRMED findings from the 22 verified reports
(`verified-report-01.md` … `verified-report-22.md`) into an orchestration-ready
plan. ~210 raw findings were verified; 3 verdicts were dismissed
(see "Dismissed findings" at the end); cross-report duplicates are merged
(~17 merges). **~191 deduplicated fixes are scheduled across 18 phases.**

Ordering: security + correctness HIGHs first, then MEDIUMs, then LOWs batched
into cleanup. Phases are grouped by subsystem so each implementer gets a
coherent slice of one crate (or one file family). The subsystem grouping keeps
a few HIGHs downstream of MEDIUM phases (Phase 9.1 data-validation HIGHs,
Phase 13.1 emit/focus HIGHs, Phase 14.1/14.3 warp+navball HIGHs) — those are
all independent of the sim-determinism work, so the orchestrator may pull any
of them forward without violating dependencies; the order below is the
recommended default.

**Load-bearing/determinism phases** (change hashed state, world evolution,
hashing, or serialization — require full gate + replay verification, and
possibly a §8.4 golden re-bless):
Phases 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12.1, 15.

**Presentation / tooling / UI phases** (no hashed-state semantics):
Phases 1, 12.2–12.4, 13, 14, 16, 17, 18.

## Prerequisites

- Rust toolchain pinned by `rust-toolchain.toml` (channel 1.97.1), wasm32
  target installed.
- Working tree clean at phase boundaries — `xtask bless-goldens` refuses on a
  dirty tree; each phase should end with a commit.
- Golden fixtures present at `data/test-fixtures/trivial` and
  `data/test-fixtures/flight-launch` (each with `replay.bin`,
  `expected-hash.txt`, `fingerprint.json`).

## Gate definitions

- **FMT**: `cargo fmt --all -- --check`
- **CLIPPY**: `cargo clippy --workspace --all-targets -- -D warnings`
- **CHECK**: `cargo check --workspace`
- **TEST**: `cargo test --workspace` (sub-phases may run `cargo test -p <crate>`;
  the phase-wide validator runs the workspace)
- **WASM**: `cargo check --workspace --target wasm32-unknown-unknown --exclude csp-server --exclude csp-bench --exclude xtask`
- **LINT**: `cargo xtask lint-deps && cargo xtask lint-attribution`
- **REPLAY**: `cargo run -p csp-bench --bin csp-replay -- verify data/test-fixtures/trivial && cargo run -p csp-bench --bin csp-replay -- verify data/test-fixtures/flight-launch`

**Golden-re-bless rule (applies to every load-bearing phase):** if REPLAY fails
with `HashMismatch` *because the fix intentionally changed world evolution*
(e.g. corrected CoM velocity, corrected event timing, corrected colony
solves), the implementer must confirm the mismatch is attributable to the fix,
then the orchestrator commits the phase work and runs
`cargo xtask bless-goldens --reason "session-deep1 fix: <phase name>"` before
re-running REPLAY. A `StampMismatch`/`MalformedGolden`/self-check failure is
never to be re-blessed silently — that is a real regression until proven
otherwise.

## Per-phase implementer/validator protocol

Every implementer dispatch MUST include: the complete Requirements section for
its phase, the Inputs/Outputs lists, the NO-SLOP policy (no `any`/placeholders/
TODO/FIXME/unused imports or variables), and instruction to run FMT + CLIPPY +
CHECK + TEST (-p for the touched crate) before reporting done. Every validator
dispatch MUST require reading the code line-by-line against the Requirements,
enforcing NO-SLOP, and running the phase's Validation Criteria gates. Fixers
fix ALL validator errors at once, then run the same gates.

---

## Phase 1: csp-server security & robustness
**Type**: Sequential, 3 sub-phases (1.1 → 1.2 → 1.3; they share files)
**Severity mix**: 2 HIGH, 5 MEDIUM, rest LOW

### Sub-phase 1.1 — Token generation & auth boundary (HIGH)

**Requirements** (verified-report-19):
- R19-F1 [HIGH]: `crates/csp-server/src/serve.rs:180-183` —
  `generate_token()` calls `std::fs::read("/dev/urandom")` which never returns
  EOF → `Server::start` deadlocks before `bind` (serve.rs:103). Fix: read
  exactly N bytes (`File::open` + `Read::take(32)`/`read_exact`) with the
  existing non-Unix fallback (serve.rs:186-197) kept as `Err` fallback.
- R19-F2 [HIGH]: `http.rs:219-222` exempts `/` and `/assets/` from the bearer
  check while `http.rs:261-279` injects the token into the served page —
  any unauthenticated client gets full control. Fix: serve the SPA shell
  without the secret (hand the token over the authenticated channel, or
  require the token file / env / first-boot flow); never embed the bearer
  token in a token-free response. Update `tests/e2e.rs:170-173`
  (`server_boots_and_serves`) which currently *asserts* the leak.
- R19-F7: token file write → `chmod 0600` window (serve.rs:204-209) and
  `temp_dir()/csp` fallback dir (serve.rs:65-70). Fix: create with restricted
  perms atomically (`OpenOptions::mode(0o600)` / `O_NOFOLLOW`-style
  handling), refuse a pre-existing foreign/symlinked path.
- R19-F12: non-constant-time bearer compare (http.rs:240-246) and `?token=`
  accepted on every guarded endpoint (http.rs:223-227). Fix: constant-time
  compare; restrict `?token=` to the WS bootstrap path only.
- R19-F20 (token-related items): doc says token file holds `<token>\n<port>\n`
  (serve.rs:199-200) but only the token is written (204); `Server::shutdown`
  (146-148) never removes stale token files; `Bearer ` is case-sensitive
  (http.rs:244). Fix doc/behavior consistently.
- NOT-A-BUG: the "server path echo via {e}" sub-item of F20 was verified
  dismissed — `io::Error` `Display` does not embed paths. No action.

**Inputs**: Read `crates/csp-server/src/{serve,http}.rs`,
`crates/csp-server/tests/e2e.rs`.
**Outputs**: Modify `crates/csp-server/src/serve.rs`,
`crates/csp-server/src/http.rs`, `crates/csp-server/tests/e2e.rs`.

**Validation**: FMT, CLIPPY, CHECK, `cargo test -p csp-server` (must add a
regression test proving `generate_token` returns promptly — e.g. call it
under a timeout — and that `GET /` no longer reveals the token while a
token-authenticated path still works).

### Sub-phase 1.2 — DoS surface & batch atomicity (MEDIUM)

**Requirements** (verified-report-19):
- R19-F3: Host/Origin allowlist hardcodes localhost (http.rs:146-183) and is
  applied to every request (205-213): `--bind-lan` fails closed for LAN users
  and open for `Host: localhost` spoofers. Fix: make the allowlist derive
  from `config.bind` (bind 0.0.0.0 ⇒ accept the bound addresses, not a
  hardcoded localhost set).
- R19-F4: `backend.rs:700` `rrx.recv()` blocks a tokio worker inside
  `GameHost::call` (also `inspect` at 677-690); reached synchronously from
  `mcp_post` (http.rs:443) and `ws_session` (http.rs:572-575). Fix:
  `spawn_blocking` (or an async channel) plus a timeout.
- R19-F5: `backend.rs:304-306` `submit_batch` leaves a pushed prefix queued
  on mid-loop push error; residue drains on the next unrelated submit.
  Fix: roll back the pushed prefix (or drain+report before returning Err) —
  both `LocalGame` (495-507) and `GameHost` (623) paths share `HostCore`.
- R19-F6: `mcp.rs:308-353` `vessel.control` submits each field through its own
  drain+step (burns multiple ticks, contradicts "lands coherently" doc at
  302-303). Fix: batch all fields of one call into a single `submit_batch`.
- R19-F15: `backend.rs:281-292` `drain_and_step` silently returns `Ok` after
  64 partial frames with the clock short of target. Fix: surface the
  shortfall in the response (e.g. `partial: true` + reached tick).

**Inputs**: Read `crates/csp-server/src/{backend,http,mcp,ingress}.rs`.
**Outputs**: Modify `crates/csp-server/src/{backend,http,mcp}.rs` (+ e2e).

**Validation**: FMT, CLIPPY, CHECK, `cargo test -p csp-server`. New tests:
a failed `submit_batch` leaves `pending` empty; a multi-field `vessel.control`
advances the clock exactly once and lands one boundary; blocking-call path
does not park the worker pool (bounded timeout test acceptable).

### Sub-phase 1.3 — Server LOW batch

**Requirements** (verified-report-19):
- R19-F8: `timewarp.set` `level as u32` truncates u64 (mcp.rs:413; schema
  `u()` at 622 has no max). Fix: reject `> u32::MAX` as Invalid params.
- R19-F9: `ClientRegistry` unbounded growth on attacker names
  (backend.rs:160-168; names from `?client=`/`X-CSP-Client`, http.rs:432-441,
  483). Fix: cap entries and name length.
- R19-F10: WS `max_frame_size` left at tungstenite default 16 MiB
  (http.rs:484). Fix: set it to the intended wire bound alongside
  `max_message_size`.
- R19-F11: `project`/`field` re-serialize the whole snapshot per field
  (telemetry.rs:78,103). Fix: serialize once per `project()`.
- R19-F13: `/assets/{*path}` traversal on Windows — split only on `/`
  (http.rs:293-298). Fix: reject `\`, NUL, and any `..`/empty segment
  per `std::path::Component` walk.
- R19-F14: WS asymmetries — `subscribe.fields` silently truncated at
  MAX_FIELDS (http.rs:535), `command` arm skips the `proto` check (567-589),
  `command_ack` shares the telemetry `seq` counter (580,583). Fix each.
- R19-F16: blocking `std::fs` in async handlers (http.rs:259,300-308;
  dashboard.rs:183,204,231-233). Fix: `spawn_blocking`/`tokio::fs`.
- R19-F17: dashboard store TOCTOU (dashboard.rs:226) + non-atomic write
  (233). Fix: lock the check+write or serialize via the game thread;
  write temp-file + rename.
- R19-F18: no WS keepalive/idle timeout/connection cap (http.rs:493-595).
  Fix: ping/pong, send timeout, connection limit on `router()`.
- R19-F19: `spawn_full` `panic!`s (backend.rs:660-666); poisoned mutexes
  unwrapped with `into_inner()` (161,477,708). Fix: return an error; document
  or propagate poison.
- R19-F20 remainder: no MCP `initialize` state machine (mcp.rs:119-156);
  non-object `attitude`/`ang_vel` → `IDENTITY` instead of error
  (mcp.rs:327-334); non-Unix token fallback is a weak pid+nanos hash
  (serve.rs:186-197) — strengthen or remove.

**Inputs**: Read files listed per item.
**Outputs**: Modify `crates/csp-server/src/{http,backend,mcp,telemetry,
dashboard,serve}.rs` (+ tests/e2e.rs).

**Validation**: FMT, CLIPPY, CHECK, `cargo test -p csp-server`.

**Phase-level dependencies**: None (first phase).
**Phase-level notes**: Findings 1–3 are latent today (no shipped binary calls
`Server::start` or sets `bind`), but the code defects are real — fix anyway.

---

## Phase 2: TickShadow / Fatal-rollback coverage (csp-sim determinism core)
**Type**: Sequential · **Determinism-load-bearing: YES**
**Severity mix**: 2 HIGH, 4 LOW · 6 deduplicated fixes

**Requirements**:
- R8-F1 + R4-F7-related [HIGH]: `pending` command queue is not covered by
  `TickShadow` — `WorldTickIo::issue` (crates/csp-sim/src/lib.rs:7367-7372)
  pushes unrecorded commands that survive a later `Fatal` rollback (ghost
  commands; unhashed so invisible to the fingerprint). Fix: record
  issued-command pushbacks in `TickShadow` (lib.rs:1481-1573 struct,
  restore at 1618-1715) and truncate `pending` back to its pre-iteration
  shape on `restore`, preserving per-(class,source_id) FIFO order.
- R7-F1 + R8-F2 [HIGH] (merged — one fix): unshadowed `tracked_vessels` and
  `transfers` mutations inside `step_bubble_range`:
  `joints::latch_pass` receives `&mut self.tracked_vessels`/`&mut
  self.transfers` (lib.rs:3667,3669); `apply_latch` mutates both
  (joints.rs:1063-1066 `tracked.remove/insert`; joints.rs:1096
  `remap_transfer_vessels`); the dead-vessels drain mutates
  `tracked_vessels` at lib.rs:3697 with only `shadow_autopilots` at 3695.
  Fix: call the existing lazy `shadow_tracked`/`shadow_transfers` helpers
  before `latch_pass` and before the dead-vessels cleanup.
- R7-F5 + R8-F8 (merged): `script_log` survives `Fatal` rollback — no
  `TickShadow` field (lib.rs:1573 end); pushes at 6182-6219; drained by
  `mem::take` at 6869. Fix: shadow the log length like `command_log`'s
  `log_len` (1486/1621).
- R7-F8: `ensure_aero` (lib.rs:3202-3210) mutates `body.aero` before
  `shadow_vessels` (3211). Fix: reorder — capture first. (Vacuous today
  since `aero` is unhashed; hygiene fix only.)
- R8-F11a: `self.shadow_autopilots(shadow)` at lib.rs:3695 clones the whole
  autopilot arena unconditionally — comment claims "no-op when dead_vessels
  is empty". Fix: gate on `!dead_vessels.is_empty()` (or make the lazy
  capture actually lazy) and correct the comment.
- R4-F8: `AdvanceReport::rejected` keys alias `command_log` keys — `drain_at`
  builds `key` from pre-increment `source_seq`, restore re-issues the seq, so
  one `CommandKey` can appear in both `rejected` and `command_log`.
  Fix: give rejected entries a distinguishing stamp (e.g. keep the key but
  also restore-then-bump, or report `(key, e)` with an explicit
  `attempted_seq` field — choose the least invasive option consistent with
  the `CommandKey` doc).

**Inputs**: Read `crates/csp-sim/src/lib.rs` (TickShadow 1481-1715,
step_bubble_range 3200-3720, drain_at 5873-5961, step_frame 6745-6870),
`crates/csp-sim/src/joints.rs` (apply_latch 780-1100, latch_pass 1123-1155),
`crates/csp-sim/src/transfers.rs` (remap_transfer_vessels 1598-1611).

**Outputs**: Modify `crates/csp-sim/src/lib.rs`,
`crates/csp-sim/src/joints.rs` (capture call sites only).

**Validation Criteria**:
- FMT, CLIPPY, CHECK, TEST, WASM (csp-sim is in the wasm set).
- REPLAY on both fixtures (HashMismatch → golden-re-bless rule applies only
  if a world-evolution change is provably caused; this phase should be
  behavior-preserving — a mismatch here is a red flag).
- New regression tests (in `mod tests`, lib.rs): (a) force a `Fatal` after a
  script `issue` → assert `pending`/`state_hash` identical to pre-boundary;
  (b) latch-merge then `Fatal` → assert `tracked_vessels`/`transfers` roll
  back; (c) `Fatal` after a `script_log` push → log rolled back.

**Dependencies**: None (independent crate).

---

## Phase 3: Crew roster, research scope & control-lease lifecycle
**Type**: Sequential · **Determinism-load-bearing: YES** (hashed crew/science
state) — **Severity mix**: 2 HIGH, 2 MED, 1 LOW · 5 fixes

**Requirements**:
- R7-F2 [HIGH]: roster `CrewLocation`/`CrewStatus` desync across the physical
  crew lifecycle — six sites each leave roster `Active`@`Vessel(dead/old)`
  → next snapshot restore Fatals (lib.rs:7753-7788):
  1. `land_crew` V→V arm (transfers.rs:1116-1128) reseats without writing
     `m.location` — the routine `TransferCrew` trigger. MUST fix first.
  2. `apply_jettison_part` (lib.rs:5591) `v.crew.retain` — mark roster
     members in the jettisoned seat `Lost`+`Unassigned` per the
     `fire_vessel_ls_depleted` precedent (life_support.rs:377-392).
  3. `apply_breakup` (lib.rs:8775-8779, 8883) — fragment-bound crew need
     `location = Vessel(fragment)`; exploded-node crew `Lost`.
  4. `apply_undock` → `split_vessel_records` (lib.rs:5781 →
     transfers.rs:1616-1638) — re-seat roster records to the fragment.
  5. `apply_latch` → `merge_vessel_records` (joints.rs:946 →
     transfers.rs:1571-1592) — re-seat absorbed crew to `prim`.
  6. `remove_vessel` + all in-bubble removals (lib.rs:3801-3839, impact/
     decay/SOI-escape paths) — mark aboard roster members `Lost`+
     `Unassigned` (mirror economy.rs:898-904 recovery path).
- R10-F1 [HIGH]: `finalize_projects_on_nodes` is not vessel-scoped
  (research.rs:1147-1153) — jettisoning node N on vessel A finalizes every
  project on any vessel with `lab_node == N`. Fix: pass `VesselId` and filter
  `p.vessel == vessel`, mirroring `split_vessel_science` (research.rs:467).
- R4-F3: disabled-script lease lockout — strike-disable (lib.rs:~6180-6196)
  and `SetScriptEnabled` (4211-4223) clear `enabled` without touching
  `control_lease`; a non-Ui world can then never free the vessel (only
  `apply_unload_script` vacates, ~6331-6353, and it's bound-vessel gated).
  Fix: vacate `Lease::Script(id) → Lease::Player` whenever the script is
  disabled (same comment convention as the unload path).
- R4-F4 + R8-F11b (merged): `Lease::Script` u64→u32 asymmetry — producer
  clamps `id.0.min(u32::MAX)` (csp-script/src/lib.rs:1030, csp-ai/src/lib.rs:201)
  while `check_control_lease` wraps via `as u32` (lib.rs:~4280), and
  `apply_take_control` (~4296-4324) never verifies the `Script(id)`/`Agent`
  target exists → `TakeControl{lease:Script(2³²+k)}` steals script k's gate;
  `TakeControl{lease:Script(9999)}` wedges the vessel; all ids > u32::MAX also
  share one pending FIFO. Fix: use the same clamp in the gate (or store the
  full u64 in `Lease`), and validate the lease target exists
  (`Script`/`Agent` → membership check; `Ui` stays unrestricted).
- R7-F4 [LOW]: `latched_total` (lib.rs:3223 Vec) accumulates duplicates and
  non-transitions (extend at 3485, `else if` at 3471, moving-only skip at
  8474-8475); comment at 3708 falsely claims dedup. Fix: dedup via BTreeSet
  or push only on actual transitions; fix the comment.
- POLICY ITEM (R4-F5 + R7-F7, merged — DECIDE, default disposition below):
  `control_gated_vessel` (lib.rs:1144-1161) covers 12 steering variants but
  `RecoverVessel`, `InstallAeroTables`, `SetVesselTracked`,
  `SetLifeSupportEnabled`, and all transfer variants bypass the lease.
  Default fix: add the destructive/state-rewriting commands
  (`RecoverVessel`, `InstallAeroTables`, `SetVesselTracked`,
  `SetLifeSupportEnabled`) to the gate and tighten the doc at lib.rs:1131-1133
  to say resource/crew moves are deliberately ungated because they do not
  steer the craft. (Validator: flag if the codebase's own docs/tests assert
  the opposite.)

**Inputs**: Read `crates/csp-sim/src/{lib.rs (4033-4330, 5500-5900, 6170-6560,
8560-8900), transfers.rs (850-1250, 1570-1650), joints.rs (900-1100),
research.rs (450-580, 1140-1180), economy.rs (740-910), life_support.rs
(360-395)}`.

**Outputs**: Modify `crates/csp-sim/src/{lib.rs,transfers.rs,joints.rs,
research.rs}`.

**Validation Criteria**:
- FMT, CLIPPY, CHECK, TEST, WASM, REPLAY.
- New tests: roster-member `TransferCrew` V→V then snapshot+restore round-trip
  (previously Fatal); jettison/breakup/latch/removal each leave roster
  consistent; disabled script releases its lease; `TakeControl` to a
  nonexistent Script id → `Recoverable`; jettisoned node finalizes only the
  jettisoning vessel's projects.

**Dependencies**: Phase 2 (same files; run after to avoid diff collisions in
step_bubble_range/TickShadow regions — content is disjoint but keep
sequential per plan order anyway).

---

## Phase 4: Surface physics — origin-vs-CoM velocity stamp
**Type**: Sequential · **Determinism-load-bearing: YES** (changes stamped
velocities → goldens may need re-bless) — 2 fixes (1 merged)

**Requirements**:
- R4-F1 + R7-F3 (merged; reported independently in reports 04 and 07):
  `surface_pose_restore` computes `vel = env.body_spin.cross(pos - env.
  body_center)` at `crates/csp-sim/src/bubble.rs:245` — the co-rotation
  velocity of the body *origin*, then it is stored into `body.vel` which is
  the *CoM* slot (`simple.rs:26-35`, `lib.rs:3111-3118`, spawn doc
  lib.rs:553 "CoM velocity"). Fix: add the missing `ω × (rot·com_local)`
  term — pass `com_local` into `surface_pose_restore` (signature currently
  `(env, pose, tick)` — bubble.rs) or post-fix at the consumers
  (`unlatched_body` lib.rs:3113-3118, pad spawn in
  `crates/csp-view/src/lib.rs:1355-1366`); the jettison correction at
  lib.rs:5568-5576 and `structural.rs:345` `fragment_velocity` show the
  correct convention. Render-only `flight.rs:426` copy needs no change.
- R4-F2: `apply_to_rails_at` anchors the conic at the origin:
  `let (r, v2) = (body.pos - env.body_center, body.vel)` (lib.rs:4740) pairs
  origin position with CoM velocity — an O(|com_local|) wrong state; the
  `r_p > radius_m` check (~4746) evaluates that wrong conic; return leg
  stamps `body.pos = r + center` (4998-4999). Fix: build the conic from
  `body.com_world() - env.body_center` and convert back consistently
  (`pos = com_world - rot·com_local` on the way out).

**Inputs**: Read `crates/csp-sim/src/{bubble.rs (180-260), lib.rs
(3090-3140, 4700-5010, 5560-5580, 7300-7320)}`,
`crates/csp-phys/src/{simple.rs (26-35, 1330-1345), weld.rs (170-185),
structural.rs (340-350)}`, `crates/csp-view/src/lib.rs:1340-1370`.

**Outputs**: Modify `crates/csp-sim/src/{bubble.rs, lib.rs}`,
`crates/csp-view/src/lib.rs` (pad spawn site).

**Validation Criteria**:
- FMT, CLIPPY, CHECK, TEST, WASM, REPLAY (golden re-bless expected here —
  the pad-spawn velocity changes).
- New test: surface unlatch with a nonzero `com_local` — assert
  `body.vel ≈ ω × (com_world − center)` exactly per the
  `surface_vel(b.com_world(), env)` helper convention (lib.rs:11060).

**Dependencies**: None on phases 2-3 (different code paths), but keep
sequential order.

---

## Phase 5: Event firing & partition determinism
**Type**: Sequential · **Determinism-load-bearing: YES** (changes event
timing/stamps → replay verification mandatory) — 8 dedup fixes

**Requirements**:
- R8-F3 [HIGH]: mid-interval-published events fire at `clock == t_next` —
  `advance_interval_subsystems` runs at `clock == now` (lib.rs:6822-6828)
  while `fire_due_events` pops `e.tick <= t_next` (6416) after
  `try_advance` (6836); clock-relative writes then diverge across §2.6.1
  partitions. Fix the four clock-relative firing sites to anchor at
  `event.tick` (the `fire_part_failure`/`fire_salary_due` pattern):
  `fire_life_support_check` `clock+1` (life_support.rs:332),
  `charge_funds` `clock+1` Insolvent (economy.rs:645-651),
  `fire_colony_event` re-solve at `self.clock.ticks()` (colonies.rs:1862)
  and its `tick+1` schedules (2120-2138) — the colonies.rs:1855-1861 comment
  claiming partition-independence is wrong for mid-interval publishes and
  must be corrected — `finish_burns` degraded `SoiRescan` at
  `self.clock.ticks()` (3777-3781) vs `fire_due_events`' `t_next` anchor
  (6515-6521) — unify on `event.tick`/`t_next` semantics.
- R8-F4 + R9-F8 (merged): `Recoverable` escapes `step_frame` via `?` chains,
  violating "`Err` escapes only for `Fatal`" (lib.rs:1204): reweld
  `rebody` (lib.rs:3541 → transfers.rs:~620), `credit_qualification`
  (3533 → tech.rs:135-148), `apply_surface_impacts` (3705 → colonies.rs:1800
  → `recompute_colony` env-miss 2006-2010). Fix: map these to `Fatal` or
  pre-filter the conditions (the env-miss colony arm needs an explicit
  `colony_env` presence check alongside the existing dormant/abandoned
  filter at colonies.rs:1770-1778). Also R9-F8's second half: clock reset to
  range-start `now` (lib.rs:3692) makes impact damage retroactive over the
  whole `(now, t_next]` interval — apply damage at the impact tick, not the
  range start.
- R8-F5 + R4-F6 (merged): `ScriptWake` double-ticks a bubble-resident
  script — `fire_script_wake` (lib.rs:6360-6383) lacks a residence check;
  `control_tick` (6097-6104) then runs `on_tick` for the same tick again.
  Fix: skip wake-fired scripts already ticked via `control_tick` (residence
  check or consumed-tick mark).
- R8-F6: wake-issued commands stamp `t_next` not `t_next+1` — wake io clock
  is `t_next` inside `fire_due_events`; `drain_at(self.clock.ticks())`
  (5885-5886) stamps the same tick while control-path commands stamp +1
  (scripts.rs:34-35 doc). Fix: stamp wake-issued commands at `t_next + 1`
  or drain them at the next boundary with the boundary tick.
- R8-F7: `request_wake` (7397-7402) bypasses `schedule_event`'s
  `tick <= clock → Fatal` invariant (3918-3919) — deferred-tail
  `SoiTransition`/`ManeuverNodeStart` resolutions can push `tick < clock`
  heap entries. Fix: route through `schedule_event` or clamp/reject
  `tick <= clock` per the documented invariant.
- R4-F9: `unlatched_body` `SurfaceLatched` arm (lib.rs:3094-3139) never
  clears `v.maneuver` — both other transitions do (4759, 5004). Fix: clear
  it (latent zombie-node trap).
- R10-F2: `fire_part_failure` fatal-mode early return (reliability.rs:931-933)
  leaves `rec.pending_failure_tick` set → `publish_reliability` skips the
  record (851-853) and the next snapshot restore Fatals
  (lib.rs:8052-8058). Fix: clear `pending_failure_tick` before the
  fatal-mode return, matching the stale-fire branch (954).
- R7-F6 [LOW]: stale thrust factor in Δv accounting + zero-thrust burns
  wedge — `drive_node_burn` (lib.rs:3304) reads `net_thrust_axis`
  computed from last tick's `thrust_factor`-scaled `rated_n`
  (maneuver.rs:212-220) before `apply_thrust_factors` re-scales at
  3311-3313 (one-tick-stale, self-correcting); `f_max = 0` →
  `throttle = 0; return false` forever (7050-7053) → the node stays
  `Burning`, `SetManeuver` rejects "burn in progress" (5065-5071), and
  only manual `DeleteManeuver`/`ToRails` recovers. Fix: recompute the
  net thrust after the same-tick factor application (or document the
  one-tick lag), and add a zero-thrust escape — degrade to a
  `SoiRescan`-style republish / auto-complete or surface a typed
  notification so the burn slot unwedges without manual commands.

**Inputs**: Read `crates/csp-sim/src/{lib.rs (3300-3320, 3690-3790, 5070-5250,
6090-6560, 6760-6870, 7040-7070), events.rs (390-500, 600-615),
life_support.rs (280-355), economy.rs (640-660, 720-735),
colonies.rs (1840-1900, 2000-2140), reliability.rs (840-960, 1460-1490),
maneuver.rs (200-225), scripts.rs (30-45, 640-660), tech.rs (130-150),
transfers.rs (~615-625)}`.

**Outputs**: Modify `crates/csp-sim/src/{lib.rs, life_support.rs, economy.rs,
colonies.rs, reliability.rs}` (and `scripts.rs` docs if the wake contract is
clarified).

**Validation Criteria**:
- FMT, CLIPPY, CHECK, TEST, WASM, REPLAY (re-bless likely — event timing is
  hashed state).
- New tests: (a) same scenario run at two different tick partitions produces
  identical `state_hash` sequences (use the existing
  `warp_x1_vs_x1e5_equivalence_10_sim_days` pattern at lib.rs:10112);
  (b) enabled InBubble script with a live `AtTick` wake fires `on_tick`
  once at that tick; (c) wake-issued command stamps `t_next+1`;
  (d) crafted `Recoverable` arm no longer propagates as `Err` from
  `step_frame` (or converts to `Fatal` per chosen fix).

**Dependencies**: Phase 2 (TickShadow region), Phase 6 touches the same
colonies.rs firing code — run Phase 6 AFTER this phase.

---

## Phase 6: Colony & life-support logic
**Type**: Sequential · **Determinism-load-bearing: YES** — 7 fixes

**Requirements** (all verified-report-09):
- R9-F1 [MEDIUM]: power-pinned `LifeSupport` module = immortal crew —
  ungrounded LS registers no consumers/drains nothing (colonies.rs:556-568,
  713-741, 804, 865-885, 925-933), while the depletion scan still requires
  `stock <= EPS` (997-1017) → stocked O2 freezes crew alive forever.
  Fix per the report's options: ground LS on viability only, or treat
  unsatisfied LS requests as depletion evidence (picked fix must keep the
  `damaged_life_support_kills` contract).
- R9-F2 [MEDIUM]: committed transition events fire without re-checking the
  predicate — `LifeSupportDepleted`/`PowerLost`/`PowerRestored`/
  `VesselLifeSupportDepleted` (colonies.rs:1909-1983 firing arms,
  life_support.rs:363-394) check only meta-gates; a same-tick
  `TransferComplete` resupply cannot cancel them. Fix: re-check the
  scheduling predicate at firing (ls_depleted / power_starved / flag) per
  the belt-and-suspenders pattern already present.
- R9-F3 [LOW]: `next_terminator_tick` EPS skip — `dt > EPS` gate
  (colonies.rs:455-456) skips real crossings inside 1e-9 s. Fix: `dt > 0.0`
  or a tick-scaled epsilon.
- R9-F4 [LOW]: impact check vs `vessel_colony_linked` compute the colony
  surface point differently — `apply_surface_impacts` uses
  `altitude_m: 0.0` + physics `env.radius_m` (colonies.rs:1756-1791);
  `vessel_colony_linked` uses `site_defs[co.site].altitude_m` +
  `colony_env[body].radius_m` (transfers.rs:1198-1224). Fix: store the
  founding site (or altitude) on `Colony` (apply_found_colony currently
  drops it, colonies.rs:1448-1449) and use one convention at both sites.
- R9-F7 [LOW]: `allocation_pass` returns one-sweep-stale `grants` when the
  cap exhausts mid-change (colonies.rs:806-861; reachable via legal
  same-resource input/output recipes). Fix: recompute `grants` after the
  loop alongside `produced`, or break before the fraction update on the
  capped sweep.
- R9-F9 [LOW]: damaged `Store`/`Battery` keeps capacity — capacity is
  aggregated once at founding (colonies.rs:1472-1499) and never re-derived;
  `module_inert` covers only produce/consume (481-490). Fix: exclude
  damaged-store capacity from `credit_end`/level caps and
  `AbandonColony` salvage (mirror the `active`-filtered `eps_sigma_a`
  discipline at 948-960).
- R9-F6 [LOW]: `ManufactureCraft` reports reachable input errors as `Fatal`
  — `ticks_for` saturation → `Fatal` (manufacture.rs:219-222) where siblings
  are `Recoverable`; `economy_cfg.is_valid()` → `Fatal` at 118-119 vs
  `Recoverable` at the five transfer sites; `set_economy_config` accepts an
  invalid config unchecked (economy.rs:583-586). Fix: `Recoverable` for the
  input-reachable paths; reject invalid config at `set_economy_config`.

**Inputs**: Read `crates/csp-sim/src/{colonies.rs (430-460, 700-760, 790-1050,
1300-1330, 1440-1530, 1740-1810, 1900-2180), life_support.rs (320-395),
manufacture.rs (80-240), economy.rs (580-590), transfers.rs (680-700,
1060-1110, 1190-1230)}`.

**Outputs**: Modify `crates/csp-sim/src/{colonies.rs, life_support.rs,
manufacture.rs, economy.rs, transfers.rs}` (and possibly `csp-data`
`colony.rs` if `Colony` gains a site field — if so, schema_version handling
must be coordinated with Phase 9.2).

**Validation Criteria**:
- FMT, CLIPPY, CHECK, TEST, WASM, REPLAY.
- New tests: crewed `LifeSupport` with `power_ec_per_s > 0` + stocked O2 + no
  EC → crew dies (regression for F1); emergency-resupply scenario — commit
  `ColonyStoreEmpty` then land O2 same tick → crew survives (F2); damaged
  `Battery` stops accepting `credit_end` (F9).

**Dependencies**: Phase 5 (colonies.rs firing arms overlap).

---

## Phase 7: Contracts / research / economy program layer
**Type**: Sequential · **Determinism-load-bearing: YES** (offer generation,
science banking) — 6 fixes

**Requirements**:
- R2-F3 [HIGH] (validator half lives in Phase 9.1 — the sim half here):
  `ra = rp` clamp produces effectively-unsatisfiable satellite offers —
  `contracts.rs:243-249` draws `rp`/`ra` and clamps; predicate at 735-737
  then requires bit-exact `e = 0`. Fix: redraw/reject the degenerate draw
  (or relax the predicate with a documented altitude tolerance) so offers
  are satisfiable; keep `contract.rs:138-141`'s doc honest.
- R10-F3 [MEDIUM — design decision, default below]: `RecoverData`/
  `apply_recover_vessel` applies `1 − transmit_ratio` to units collected
  *after* the transmit (research.rs:846, 881-897; economy.rs:856-878).
  Default fix: document the per-key blanket semantics on
  `ScienceKeyState::transmitted` (doc explicitly, since the function doc at
  research.rs:822-823 already describes the blanket) — OR track the
  untransmitted remainder if the orchestrator's validator finds the plan
  intends post-transmit units at full value. Pick the doc route unless a
  §3.2 reference says otherwise.
- R10-F6 sim half (merged with R3-F1 validator half in Phase 9.1):
  `draw_params`/`generate_offer` Fatal on empty candidate lists
  (contracts.rs:223-225 `pick`, `.get(...).ok_or(Fatal)` at 239-319,
  publish `?` at 640-649). Fix: skip the template / `Recoverable` instead
  of `Fatal` — the `bodies_ok` validator fix in Phase 9.1 prevents reaching
  this from validated data, but `set_contracts` callers bypass it.
- R10-F4 [LOW]: `SurveyData` sums `s.collected`, not returned units
  (contracts.rs:739-757; comment says "returned" at 744-745). Fix: count
  actually returned (banked/transmitted) units or correct the semantics per
  contract doc.
- R10-F5 [LOW — policy, default: document]: already-satisfied contracts
  complete at `AcceptContract` (contracts.rs:698-705; `SatelliteOrbit`
  724-738, `PartTest` 759-778, `SurveyData`, `ColonyResupply` 793-806 —
  the colony-deliveries extension the report found). Fix: document the
  intended instant-completion semantics, or gate completion to
  post-acceptance facts.
- R10-F7 [LOW, five sub-items]: unchecked `+`/`*` overflow at
  economy.rs:691 (`(clock/period + 1) * period`); plain `+` inside `.max()`
  at research.rs:357; `fire_contract_deadline` `Recoverable` on tick
  mismatch (contracts.rs:918-919 — severity asymmetry vs siblings);
  `deadline_days` `NaN → 1 tick`/`+inf → u64::MAX` (contracts.rs:676-680);
  `bench` tick saturation → `Fatal` (reliability.rs:1144-1149). Fix each
  with the sibling convention (checked math / `Recoverable` for reachable
  inputs / is_finite guards).

**Inputs**: Read `crates/csp-sim/src/{contracts.rs (220-320, 540-650,
670-940), research.rs (340-360, 800-900), economy.rs (640-700, 850-880),
reliability.rs (1140-1150), tech.rs (130-150), transfer.rs}` and
`crates/csp-data/src/contract.rs:130-145`.

**Outputs**: Modify `crates/csp-sim/src/{contracts.rs, research.rs,
economy.rs, reliability.rs}`.

**Validation Criteria**: FMT, CLIPPY, CHECK, TEST, WASM, REPLAY. New tests:
authored satellite band with `lo(apo) < lo(peri)` still yields a
satisfiable offer; empty-`bodies` template via `set_contracts` →
`Recoverable`, not `Fatal`; deadline tick mismatch → `Fatal`-consistent
behavior per chosen convention.

**Dependencies**: None strict; keep sequential order.

---

## Phase 8: Save/restore contract (World::restore crafted-input gates)
**Type**: Sequential · **Determinism-load-bearing: YES** (restore is the
hash/serialization boundary) — 9 fixes + 2 fold-ins

**Requirements** (verified-report-06; the restore doc at lib.rs:2258-2262
makes refusing crafted snapshots contractual):
- R6-F1 [MEDIUM]: unvalidated hashed payloads:
  (a) `VesselState::OnRails(_) => {}` arm (lib.rs:8245) — no finiteness,
      `parent ∈ orbits`, or element-range checks; thread `orbits`
      (signature param at 2299, currently unused by `restore_vessel_state`
      at 8088) and resolve `orbit.parent` — *this also closes R11-F4's
      test-visible gap* (restore accepted an OnRails vessel against an
      empty model at lib.rs:16458).
  (b) `csp_phys::weld` `validate_structure`/`validate_effective` runs only
      for `InBubble` (8211) — `SurfaceLatched`/`OnRails` `craft` payloads
      and `ActiveBuild.craft` (manufacture.rs:59) skip it; pose.body never
      resolved. Fix: run the same checks for any serialized craft.
  (c) Colony floats (`latitude_rad`, `longitude_rad`, `t_eq_k`,
      `module_fractions`, `ColonyModuleDef` quantities — hashed at
      colonies.rs:1135-1198) get no finiteness checks in the colony loop
      (lib.rs:2672-2698); `colony.body`/`colony.site` unresolved.
- R1-F2 fold-in [MEDIUM]: snapshot quat gates — lib.rs:8213-8220 checks
  `rot.is_finite()` but not `rot.norm() > 0`; lib.rs:8237-8243 same for
  `pose.orientation` — a finite zero-norm quat passes and NaNs at unlatch
  (`bubble.rs:244`). Add `norm() > 0.0` to both, matching the
  `SetAttitude`/spawn convention (lib.rs:4067, 4531-4535).
- R6-F2 [MEDIUM]: `id.0 + 1` overflow in the next-id ahead-of-live checks —
  vessels (lib.rs:2328), scripts (2626), colonies (2699-2705). Fix: use the
  safe per-entry form already used at 7518/7589/7661/7850/8044/8065.
- R6-F3 [MEDIUM]: `colony.inspection_tick` has no ↔ pending
  `ColonyInspection` pairing check (event set built at 2459-2464/2485-2489;
  colony loop 2672-2698) — every sibling pairing is enforced. Add both
  directions; the `t <= clock` case self-heals via `publish_colonies` so a
  strict `Some(t) ⟹ event at t` gate suffices.
- R6-F4 [MEDIUM]: `economy.crew` roster keys unchecked vs `next_crew_id`
  (restore_economy_state 7748-7775). Fix: `id.0 >= snapshot.next_crew_id →
  Fatal`, matching seats (7569) and cargo (7545) gates.
- R6-F5 [LOW]: `SiteState.id != arena key` unchecked (site loop 7790-7796
  checks inventory only); hash reads `s.id` (economy.rs:446-458). Add the
  `x.id == key` gate like the seven sibling arenas.
- R6-F6 [LOW]: `autopilots` keys never checked ⊆ `vessels`
  (lib.rs:2647-2651); add the `tracked_vessels`-style subset gate.
- R6-F7 [LOW]: `script_op_budget == 0` admitted (2780) while
  `event_budget == 0 → Fatal` (2303-2305) — add the symmetric gate.
- R6-F8 [LOW]: `header.body_len as usize` truncates on wasm32
  (save.rs:542). Fix: compare in u64 / bound-check per the parse_header
  convention (568-575).
- R6-F9 [LOW bundle]: `crew_seen` covers seated crew only (7566-7573 —
  extend to `TransferCargo::Crew` ids and colony crew-set);
  `ActiveBench.site` unchecked vs `economy.sites` (8061-8076);
  part-instance uniqueness is per-(vessel, instance) but the reliability
  KDF keys `instance.0` alone (8049, reliability.rs:407+ — globalize the
  check or fix the doc at 8026); `hex_decode` accepts uppercase
  (save.rs:479) vs `BadHeader` doc (367-368).

**Inputs**: Read `crates/csp-sim/src/{lib.rs (2258-2800, 7500-7800,
8030-8260, 6898-7024, 9060-9100), save.rs (360-600), manufacture.rs (55-80),
colonies.rs (1040-1050, 1130-1200, 2080-2190), economy.rs (370-460,
740-780), reliability.rs (350-360, 400-410), transfers.rs (1100-1140),
joints.rs}` and `crates/csp-phys/src/weld.rs:338-451`.

**Outputs**: Modify `crates/csp-sim/src/{lib.rs, save.rs}` (and
`economy.rs`/`colonies.rs`/`transfers.rs` only if helpers are needed).

**Validation Criteria**: FMT, CLIPPY, CHECK, TEST, WASM, REPLAY. New tests
alongside the existing `restore_rejects_crafted_snapshots` (lib.rs:10341):
each listed crafted state → `Fatal`; `theta_survives_snapshot_restore_and_
revert`-style restore against a model missing `orbit.parent` → `Fatal`
(closes R11-F4's code half).

**Dependencies**: None strict.

---

## Phase 9: csp-data loaders & validators
**Type**: Parallel — 3 sub-phases, then phase-wide validator
**Determinism-load-bearing: PARTIAL** (datahash framing changes the dataset
fingerprint; validators feed world construction) · ~20 dedup fixes

### Sub-phase 9.1 — HIGH fixes

**Requirements**:
- R2-F1 [HIGH]: unknown RON keys silently ignored (empirically verified on
  pinned ron 0.12.2) — add `#[serde(deny_unknown_fields)]` to every
  deserializable data-file struct in `crates/csp-data/src/{system,site,
  balance,resource,recipe,biome,techtree,part,colony,contract,experiment,
  loc}.rs` and the `*File` wrappers. Verify compatibility with
  `#[serde(default)]` and the sites v1→v2 path (load.rs:386-389). Damage
  paths verified: `insolation_w_m2`, `soi_m`, `atmosphere`/`biome_map`,
  `deadline_days`, `resource_yields` misspellings all silently dead today.
- R3-F1 [HIGH] + R10-F6 validator half (merged): `bodies_ok` return
  discarded at all six call sites (validate.rs:2912,2940,2961,2969,2974,
  2984); empty `bodies` pushes no error then Fatals at first offer draw
  (contracts.rs:239-241). Fix: push a `ValidationError` on empty inside the
  closure (validate.rs:2885-2894) — the spec comment at 3009-3011 already
  claims the check exists.
- R2-F3 + R3-F12 validator half (merged): `SatellitePlacement`
  `periapsis_alt_m`/`apoapsis_alt_m` bands pass `range_ok` independently
  (validate.rs:2913-2922) with no cross-band check — clamped draws
  manufacture degenerate offers (~2.2% of satellite offers dead on arrival;
  a fully-inverted band is 100%). Fix: when `apoapsis_alt_m.hi <
  periapsis_alt_m.lo` (document exact overlap rule) push a validation
  error; consider warning on partial overlap. (Sim clamp lives in Phase 7.)
- R2-F2 + R3-F8 (merged) [HIGH]: no loc validators for experiments or
  sites — `ExperimentDef::display_name` (experiment.rs:53) and
  `SiteDef::display_name` (site.rs:51) checked nowhere; **live broken
  datum**: `data/experiments/exp.materials_study.ron:10` declares
  `exp.materials_study.name` which is absent from `data/loc/en.ron`.
  Fix: add `validate_experiment_loc_keys` and `validate_site_loc_keys`
  mirroring `validate_contract_loc_keys` (validate.rs:3139); wire them into
  the gate tests (tests/experiments.rs, tests/colonies.rs:58-102,
  tests/home_system.rs); add the missing `exp.*` keys to `data/loc/en.ron`
  (and address `apps/demo/data/` having no loc file at all — either ship
  one or exclude experiments needing keys).

**Inputs**: Read `crates/csp-data/src/{validate.rs (780-820, 1200-1370,
2500-2560, 2870-2996, 3020-3180), site.rs, experiment.rs, contract.rs,
load.rs}` and `data/loc/en.ron`, `data/experiments/`,
`crates/csp-data/tests/*`.

**Outputs**: Modify `crates/csp-data/src/validate.rs` + the serde attrs on
the def structs; `data/loc/en.ron`; possibly `apps/demo/data/`.

**Validation**: FMT, CLIPPY, CHECK, `cargo test -p csp-data`, TEST
(workspace for downstream fallout — `deny_unknown_fields` may surface
hidden typos), LINT (attribution if data files change). Add gate tests
asserting a misspelled key is a parse error and that empty `bodies`/
crossed bands produce `ValidationError`s.

### Sub-phase 9.2 — MEDIUM fixes

**Requirements**:
- R2-F4 [MEDIUM]: no `validate_balance` — `load_balance` (load.rs:349-353)
  checks shape + version only; ~45 fields ungated; `reliability_bench_
  resource` never resolved against `resources.ron` (balance.rs:242;
  runtime `unwrap_or(0.0)` at reliability.rs:1104-1115). Fix: implement
  `validate_balance` covering finiteness/ranges of the numerics and
  resolving `reliability_bench_resource` against the resource registry
  (needs a `resources` parameter — coordinate the call-site signature).
- R2-F5 + R3-F7 (merged) [MEDIUM]: schema-version discipline —
  `validate_experiments` (validate.rs:2504) and `validate_contracts`
  (3020) skip `schema_version` (and validate_contracts' docstring at 3004
  falsely claims the check); `SiteDef.schema_version` (site.rs:45-47)
  checked nowhere; `LocFile.schema_version` (loc.rs:16) has no loader at
  all — every consumer hand-parses en.ron; `COLONY_SCHEMA_VERSION`/
  `BALANCE_SCHEMA_VERSION` exact-match errors misattribute missing-field
  RON failures (load.rs:351,418). Fix: apply version checks everywhere,
  add a `load_loc`/`LocFile` validation path, per-entry SiteDef check, and
  report version-vs-shape errors in the right order (parse leniently
  enough to diagnose version first, or pre-scan `schema_version`).
- R2-F6 + R3-F10 (merged) [MEDIUM]: unvalidated authored path components —
  `body_dir.join(&r.legend)`/`join(&r.png)` (load.rs:328-332) and
  `dir.join(id.as_str())` (load.rs:320) run before validation; `..`
  escapes `FsDataSource`; `heightmap` (system.rs:117) and
  `surface_resources` unvalidated. Fix: validate path components
  (reject `..`/absolute/`\0`, require relative single-body names) before
  use — this is load-time hardening so the gate should run at load or a
  dedicated pre-check.
- R2-F7 + R3-F2 + R2-F10-cap.body (merged) [MEDIUM]: unresolved
  cross-references — `ModuleDef::Converter.recipe` (part.rs:589;
  check_module arm at validate.rs:2055-2061 checks only `power_w`),
  `ModuleDef::Experiment.kind` (part.rs:567; arm at 2041-2053 skips
  `kind`), `ExperimentCap.body` (cap_for exact match, experiment.rs:63-69)
  never resolved — runtime consequences: dead converters
  (colonies.rs:545-549), `Recoverable` on RunExperiment
  (research.rs:705-707), dead caps. Fix: thread the recipe/experiment/
  body registries into `validate_parts`/`validate_experiments` signatures
  and resolve all three.
- R3-F3 [MEDIUM]: `data_set_hash` framing is non-injective —
  `path || 0x00 || content` (datahash.rs:72-74; identical construction at
  csp-bench/src/replay.rs:460-462) provably collides. Fix: length-prefix
  (or NUL-terminate) the content, bump `DATA_HASH_VERSION`
  (csp-data/src/lib.rs:47), and keep both implementations byte-identical.
  **Re-bless required**: `data_set_hash` feeds `BuiltPart::data_set_hash`
  (csp-sim/src/tech.rs:114,316) — fingerprints/expected hashes change.

**Inputs**: Read `crates/csp-data/src/{load.rs (300-400, 440-720),
validate.rs (780-820, 1280-1620, 1860-2400, 2500-2560, 3020-3133),
datahash.rs (60-145), lib.rs (40-115), balance.rs, loc.rs, part.rs
(560-620, 830-860), colony.rs, site.rs}` and
`crates/csp-bench/src/replay.rs (455-520)`;
consumers `csp-sim/src/{colonies.rs (540-560), research.rs (700-710),
reliability.rs (1100-1120)}`.

**Outputs**: Modify `crates/csp-data/src/{validate.rs, load.rs, datahash.rs,
lib.rs}`, `crates/csp-bench/src/replay.rs` (mirrored canonicalize/framing),
data def struct files as needed; callers in `csp-sim`/`csp-view`/`csp-bench`
if signatures change.

**Validation**: FMT, CLIPPY, CHECK, `cargo test -p csp-data` + TEST, LINT.
REPLAY after datahash change (re-bless fingerprints per the rule — the
mismatch is expected and intentional here). Add tests: injectable-framing
collision probe; `validate_balance` catches a bad `reliability_bench_
resource`; version checks on each registry.

### Sub-phase 9.3 — LOW batch

**Requirements**:
- R2-F8: `SituationMask` spare bits unvalidated (part.rs:400-453; only
  `is_empty` checked at validate.rs:2047,2534). Fix: `mask & !ALL == 0`.
- R2-F9: Delta-track `base > 0` not covered by the family check
  (validate.rs:2815-2825 accepts `Some(0.0)`; manufacture-time gate at
  csp-phys/resolve.rs:832-839). Fix: require positive base for Delta
  tracks.
- R2-F10 (remaining): `check_id` gaps — `SiteId`, `RecipeId`,
  `ExperimentId`, `BiomeId` legend entries, `ModuleDef::Experiment.kind`
  (validate.rs sites listed in report). Fix: apply `check_id`.
- R2-F11 + R3-F9 (merged): recipe errors stamped `InvalidColony`
  (validate.rs:1379-1384); `BadResourceTable` dead variant (declared
  407-410). Fix: proper variant/kind.
- R2-F12: `by_id` first-wins vs validator-map last-wins divergence
  (load.rs:441-452, 507-513, 571-579 vs validate.rs:1519-1523). Fix: one
  convention (make the validator reject duplicate ids outright — dup
  detection already exists at 1394-1401; use the first-def map).
- R2-F13 + R3-F6 (merged): `collect_ron_files` swallows entry errors
  (load.rs:688 vs datahash propagating 94-97); `load_parts_from` missing
  `exists(dir)` early-out (711-721 vs the four sibling loaders).
- R2-F14 + R3-F11b (merged): colony `Drill.min_concentration` bound
  diverges from the part-side `(0.0..=1.0)` (validate.rs:1579-1587 vs
  2109; doc claims they match at colony.rs:102-104). Fix: same `[0,1]`
  bound (or document + validate the divergent intent).
- R2-F15: biome-map validation is header-only (check_biomes
  1157-1203; `decode_indexed_png` exists at biome.rs:155-236). Fix:
  decode pixels in the validator and reject unlegended in-palette
  indices.
- R3-F4: `datahash::collect` follows symlinks + lossy non-UTF-8 names
  (datahash.rs:98-104). Fix: `file_type().is_symlink()` handling + reject
  non-UTF-8 names.
- R3-F5: loader ordering ≠ `data_set_hash` ordering — doc claims at
  load.rs:81-82,196-199,432-434,494-497,558-560,623-626,679-681 are
  false for `X/` vs `X.ron` prefix collisions. Fix: either unify the
  orders or correct every doc comment.
- R3-F11a: `check_power_collision` hardcodes `"electric_charge"`
  (validate.rs:1487). Fix: introduce a shared const or resolve the id
  from the resource table.
- Also (from verified-report-03's informational note): `ValidationError`
  Display ends with `_ => Ok(())` (~781) — make it exhaustive or
  `unreachable!`/debug_assert so future variants can't format empty.

**Outputs**: Modify `crates/csp-data/src/{validate.rs, load.rs, datahash.rs,
part.rs, colony.rs, biome.rs}` + def structs as needed.

**Validation**: FMT, CLIPPY, CHECK, `cargo test -p csp-data` + TEST, LINT.

**Phase-level validation**: phase-wide validator reads all three sub-phase
diffs together — check `deny_unknown_fields` doesn't conflict with new
validators, signature changes are consistent across callers
(`csp-view/src/lib.rs:691-697`, `csp-bench`, `csp-server`), and the
datahash canonicalization stays identical in both crates.

**Dependencies**: None strict. REPLAY must run after 9.2 lands (datahash
version bump).

---

## Phase 10: csp-core encapsulation & math guards
**Type**: Sequential · **Determinism-adjacent: YES** (core math crate) — 3 fixes

**Requirements**:
- R1-F1 [MEDIUM]: `SimTime(pub u64)` (crates/csp-core/src/time.rs:17) —
  the public field bypasses the `try_advance` checked-overflow contract
  ("the only mutator" doc at 22) → silent release wrap. Fix: make the
  field private, add `from_ticks`/`ticks` accessors (serde stays
  transparent — works on private fields), and migrate all ~100
  `SimTime(x)` construction sites across `csp-sim` (orbit.rs:1629, lib.rs
  ~47 sites, transfer.rs ~26), `csp-view/src/map.rs` (3), and tests.
- R1-F2 [MEDIUM]: `DQuat::normalize` (quat.rs:64-67) and
  `from_axis_angle` (43-49) silently NaN on zero input; `normalize`'s doc
  doesn't even warn. Fix: `debug_assert!`/`debug_assert` on zero/inf norm
  (matching the hash.rs:277 trap convention), correct the docs, add an
  optional `try_normalize` if any caller needs the checked form. (The
  snapshot-boundary `norm > 0` checks land in Phase 8.)
- R1-F3 [LOW]: bit-exactness relies on unguaranteed no-FMA codegen. Fix:
  document the reliance in `vec3.rs`/`quat.rs` module docs (the pinned
  toolchain + §8.4 fingerprint mitigations) and add `to_bits` golden
  tests for `dot`/`cross`/Hamilton `mul`/`rotate`/`normalize` on
  contraction-sensitive inputs.

**Inputs**: Read `crates/csp-core/src/{time.rs, quat.rs, vec3.rs, hash.rs,
lib.rs}` + a `grep` for `SimTime(` construction sites.

**Outputs**: Modify `crates/csp-core/src/{time.rs, quat.rs, vec3.rs}` +
mechanical call-site updates in `csp-sim`, `csp-view`, and tests.

**Validation Criteria**: FMT, CLIPPY, CHECK, TEST, WASM, REPLAY (no
behavior change expected — pure encapsulation).

**Dependencies**: Best run early — it touches many call sites; keep
position here but expect mechanical conflicts if later phases land first
(orchestrator: run before Phases 11-12 to minimize churn; alternative is
to re-run at the end).

---

## Phase 11: Orbit / transfer / maneuver query contracts
**Type**: Sequential · **Determinism-adjacent** (query API correctness;
no hashed-state change) — 6 fixes

**Requirements** (verified-report-05):
- R5-F1 [MEDIUM]: `predict_encounter_excluding` hard-codes `suppress: None`
  (transfer.rs:1162-1170) — the documented chaining recipe (doc at
  transfer.rs:1111-1114) returns wrong-side `Err` artifacts; live-reproduced
  `Recoverable("soi enter: target is not a child of the vessel's parent")`.
  Fix: expose `suppress` on the public API (pass the just-fired
  `(body, direction)` like the sim does at lib.rs:6571-6576/3988-3994) and
  make `predict_encounter_excluding`'s `parentless_exit` guard cover the
  Enter artifact too (or document the contract).
- R5-F2 [MEDIUM]: `departure_node` can return `tick ≤ clock` —
  `burn_s` (transfer.rs:1359-1363) has no `now` bound; `apply_set_maneuver`
  silently clamps at lib.rs:5088 and re-resolves components at `event.tick`
  (5211-5219) → wrong-longitude burn. Fix: take a `now`/`clock` parameter
  or document + assert `node.tick > clock` at the API boundary; warn in
  the `DepartureNode` doc (transfer.rs:1242-1245).
- R5-F3 [LOW]: degenerate-frame gap — `burn_frame` gate tests exact
  `hn <= 0.0` (maneuver.rs:146-157) while the doc says `r × v ≈ 0`; a
  near-radial state produces a rounding-dominated normal. Fix: relative
  (epsilon-scaled) gate or tighten the doc; `cartesian_to_elements`
  (orbit.rs:670-675) doc is already honest — mirror if the gate is added.
- R5-F4 [LOW]: `brent_root` machine term degrades the unconditional
  "≤ 1 tick" doc bound (orbit.rs:1402 tol term; docs at 64-66, 161-162,
  1362-1364). Fix: bound the doc or tighten the tolerance formula.
- R5-F5 [LOW]: `lambert_multi` swallows `lambert_velocities` failures
  (transfer.rs:561-567) that `lambert` propagates (526) — silent empty
  `Vec` indistinguishable from "no roots". Fix: propagate or record the
  failure mode.
- R5-F6 [LOW]: NaN `soi_m` silently yields no events / burns the stride
  budget (orbit.rs:772-774, 1190-1211, 1263-1278). Fix: guard `r_soi`
  finiteness in `soi_candidates`/`next_soi_event_within` per the
  `nan_angle_orbit_state_fails_next_soi_event` precedent (orbit.rs:2265).

**Inputs**: Read `crates/csp-sim/src/{transfer.rs (470-630, 1100-1400,
1800-1950), orbit.rs (600-1000, 1000-1450, 2240-2410), maneuver.rs (full),
lib.rs (3940-4010, 5029-5240, 6520-6680)}`.

**Outputs**: Modify `crates/csp-sim/src/{transfer.rs, orbit.rs, maneuver.rs}`
+ doc updates; `lib.rs` call sites only if the signature changes.

**Validation Criteria**: FMT, CLIPPY, CHECK, TEST, WASM, REPLAY. New tests:
chained `predict_encounter` on the report's fixture (vessel parented to `m`
at r=2.44e6) → clean `Ok` or typed `Err` without the artifact;
`departure_node` past-tick → typed refusal.

**Dependencies**: None strict.

---

## Phase 12: csp-phys correctness
**Type**: Parallel — 4 sub-phases + phase-wide validator
**Determinism: 12.1 is load-bearing; 12.2-12.4 are robustness/heuristic** — 26 fixes

### Sub-phase 12.1 — Stability bounds (load-bearing math)

- R13-F1 [MEDIUM]: SoftCapture `pull_per_s` unbounded — explicit map
  `c_{n+1} ≈ (1 − pull·dt)·c_n` via unclamped bilateral PGS targets
  (simple.rs:868-881, 1706-1740); stability bound `pull_per_s < 128`,
  non-oscillatory `< 64`; `DockingConfig::is_valid` accepts any finite
  ≥0 (csp-sim/joints.rs:218-219) and `set_docking_config` installs
  unchecked (lib.rs:2159-2161); `from_balance` copies verbatim
  (joints.rs:194-206). Fix: enforce the bound in `DockingConfig::is_valid`
  (with the constant derived from `PHYSICS_DT`), reject in
  `set_docking_config`, and add the balance-side bound via Phase 9.2's
  `validate_balance` (cross-phase note).
- R13-F2 [MEDIUM]: thermal explicit Euler unguarded — `thermal::step`
  writes `t + dt·rates/C` with no clamp/substep/finite check
  (thermal.rs:350-357); divergence when `G·dt·(1/C_a + 1/C_b) > 2`;
  NaN evades lethality (`new_t > max_temp_k` false for NaN) and spreads
  one edge-hop/tick → hard `Fatal` at the next `state_hash`. Fix: (a)
  per-tick `new_t.is_finite()` writeback guard → `Recoverable`/clamp;
  (b) weld-side coupled bound `G·dt·(1/C_a+1/C_b) < 2` (weld.rs:425-437
  has only per-field gates). Impact note: non-finite temps do NOT hash
  silently — they surface as typed `Fatal`; the fix converts that to a
  source-level error.

**Inputs**: `crates/csp-phys/src/{simple.rs (220-230, 840-940, 1690-1745),
thermal.rs (300-360), weld.rs (420-450)}`, `crates/csp-sim/src/joints.rs
(120-240, 530, 760-770)`, `crates/csp-sim/src/lib.rs (2155-2165)`.
**Outputs**: `crates/csp-phys/src/{thermal.rs, weld.rs}` (+ constraint
code if clamped), `crates/csp-sim/src/{joints.rs, lib.rs}`.
**Validation**: FMT, CLIPPY, CHECK, `cargo test -p csp-phys` + `-p csp-sim`,
WASM, REPLAY. New tests: `pull_per_s ≥ 128` config rejected; divergent
thermal net → typed error, not NaN spread.

### Sub-phase 12.2 — Solver / contact / aero heuristics

- R12-F1-residual [LOW]: coincident-centre attach axis is arbitrary +X
  (structural.rs:216-226; dismissed as a "zero-axis" bug but the
  arbitrary-axis behavior diverges from `build_thermal_net`'s edge-drop at
  weld.rs:689-691). Fix: use the `c_s − p_j` fallback or drop the edge —
  one convention.
- R12-F2 [MEDIUM-doc]: `joint_loads`/`failed_joints` emit DFS preorder,
  not ascending `PartNodeId` — docs at structural.rs:113-114 and 249-251
  are false (order `[1,2,5,3]`). Fix: correct the docs (the DFS order is
  actually what the split driver needs; do not re-sort — consumers already
  re-sort at lib.rs:8626-8629).
- R12-F3 [MEDIUM]: `facet_planes` O(n⁴) on the unreduced authored vertex
  set (hull.rs:43-62; per-tick at simple.rs:1022, 1442-1463); no vertex
  cap anywhere. Fix: hull-reduce the vertex set (or cap n via
  `ColliderShape::is_valid`/csp-data validator) — choose the cheapest fix
  that bounds per-tick work.
- R12-F4 [LOW]: raw-index panics in `pub` functions —
  `graph.nodes[&l.node]`/`graph.nodes[&id]`/`transforms[&id]`
  (structural.rs:261,321,323) panic on contract violation while siblings
  return `WeldError::InternalInvariant` (174-186). Fix: typed errors.
- R12-F5 [LOW]: `merge_graphs` cannot detect a too-small `effective_base`
  (merge.rs:239-244). Fix: document the caller precondition + a debug
  assertion if derivable.
- R12-F6 [LOW]: secondary parent cycles pass merge validation
  (merge.rs:253-262 checks existence only; doc at 88-96 overstates). Fix:
  reachability walk or doc correction.
- R12-F7 [LOW]: weld≡merge bit-identity only under exact-unit arithmetic —
  `compose` renormalizes every step (graph.rs:57); doc at merge.rs:50-53
  omits the qualifier. Fix: document.
- R12-F8 [LOW]: `facet_planes` dedup merges distinct facets at ~km scale —
  tolerance mixes dimensionless normal-difference with scaled offset
  (hull.rs:98-107,143). Fix: scale-normalize or bound the tolerance.
- R12-F9 [LOW]: non-unit `ForceSite::local_dir` accepted (weld.rs:409-415
  vs the `NonUnitRotation` precedent at 377-379). Fix: unit-norm gate.
- R12-F10 [LOW]: thin-slab boundary suppression — `df >= thick·0.5 − tol`
  with `tol ≥ 1e-3` (hull.rs:195,127) rejects all boundary candidates on
  hulls thinner than `2·tol`. Fix: min-bound the margin or scale it.
- R13-F3 [LOW]: `ref_area_m2` sums total (incl. occluded) area
  (aero.rs:1296-1300) — knots divide by it; diagnostic-only today. Fix:
  use exposed `Σ area·(1−shielded)` or document.
- R13-F4 [LOW]: zero-area `cop` fallback is vertex centroid mislabelled
  "CoM" (aero.rs:1216,1234-1238,652-654). Fix: use `WeldPart.com` or
  correct the doc.
- R13-F5 [LOW]: pair contacts reuse ground `env.friction_mu`
  (simple.rs:1658 vs 1605; doc at 294 says ground-only). Fix: separate
  coefficient or document the approximation.
- R13-F6 [LOW]: self-joints not rejected (simple.rs:913) — `body_a ==
  body_b` halves dl and injects torque. Fix: filter.
- R13-F7 [LOW]: latched + `throttle > 0` flicker — latch leaves
  `b.throttle` (simple.rs:1960-1967) → unlatch next tick (1995-2003) →
  ~128-tick re-latch cycle. Fix: clear throttle or require throttle to
  hold unlatched; fix the doc (module doc 1996 claims "throttle is zero").
- R13-F8 [LOW]: `voxelize_craft` dims-product overflow — `usize` multiply
  can wrap → under-allocated `part_of` → OOB panic or wrong tables
  (aero.rs:990-993,1003,440-441,539-542). Fix: `checked_mul`/`u128`
  product before the budget compare.

**Inputs**: `crates/csp-phys/src/{structural.rs (110-330), hull.rs
(40-145, 180-200), weld.rs (360-450, 680-740), merge.rs (80-100, 170-260,
560-580), graph.rs (50-130, 240-270), simple.rs (290-300, 900-930, 1020,
1440-1470, 1600-1660, 1955-2005), aero.rs (440-445, 530-545, 640-660,
750-760, 985-1010, 1210-1240, 1290-1340)}`, `csp-data/src/validate.rs:1890`.
**Outputs**: `crates/csp-phys/src/{structural.rs, hull.rs, weld.rs,
merge.rs, graph.rs, simple.rs, aero.rs}` (+ possibly
`csp-data/src/validate.rs` if the vertex cap lands there — coordinate
with Phase 9.3 to avoid double-edits; prefer csp-phys-side cap).
**Validation**: FMT, CLIPPY, CHECK, `cargo test -p csp-phys`, WASM, REPLAY.

### Sub-phase 12.3 — Resolve / session / readout

- R14-F1 [MEDIUM]: `expected_hazard` drops invalid specs silently
  (profile.rs:206-208) and reads NaN/±∞ qual units as the best knot
  (209-210 → qual_factor resolve.rs:1091-1105 falls through to
  `knots.last()`). Fix: surface invalid specs (count/result) and gate
  `qual.units()` finiteness; `is_valid` asymmetry at resolve.rs:1252
  (`multiplier >= 1.0` lacks `is_finite`) — fix for hand-built specs.
- R14-F2 [MEDIUM]: resolver under-gates its output contract —
  `resolve_module` emits `isp_vac_s`/`isp_sl_s`/`rated_burn_s`/
  `throttle_range`/`gimbal_deg`/`propellants` unchecked
  (resolve.rs:1044-1079; only `thrust_vac_n` gated at 1412-1414); engine
  propellant ResourceIds never resolved (validator does gate them at
  csp-data validate.rs:1722-1744 — mirror or rely + document);
  `build_resources` amounts ungated (1024-1028); `resolve_envelopes`
  emits all structural/thermal/aero/attach fields unvalidated
  (1540-1576,1750); `ColliderShape::Points([])` resolves a zero-vertex
  collider (resolve.rs:996-998,1410-1411 vs `is_valid` vacuous at
  part.rs:180-185); `by_id` `BTreeMap::collect` keeps the last duplicate
  `part_id` (1807-1811); `EffectivePart` doc "every number is finite"
  (resolve.rs:446-447) is falsified. Fix: gate each emitted numeric
  family (finiteness/ranges), reject empty colliders, reject duplicate
  part ids, resolve propellant ids or document the layer that does.
- R14-F3 [MEDIUM]: `compute_attach` never checks `position_m` —
  `snap_transform` (session.rs:684-688) yields a NaN-offset transform on
  non-finite `cn/pn.position_m`; `add_part`/`reattach` store it (981-989,
  1099-1105) while `set_transform` gates `is_valid` (1205); `to_bytes`
  writes it verbatim (1580) and `from_file` rejects it (1627-1629) — a
  session can save a file it cannot reload. Fix: gate `position_m`
  finiteness in `compute_attach` (mirroring the `direction` gate at
  1032-1045) and check the computed transform before storing.
- R14-F4 [LOW]: `upgrade_levels_at_build` ungated at the session boundary —
  `check_built` (session.rs:697-732) has only scale + variant arms; bad
  TrackLevels/StatPaths serialize and later fail at `fold_multipliers`
  (resolve.rs:886-898). Fix: validate each upgrade level against
  `StatPath::base_value` (resolve.rs:773-775,817) in `check_built`.
- R14-F5 [LOW]: `apply_wear`/`wear_multiplier` emit non-finite hazard —
  `1 + operating_s/base_mtbf_s` overflows for tiny mtbf
  (resolve.rs:1121-1144). Fix: finite-output gate.
- R14-F6 [LOW]: propellant draw order is ascending `PartNodeId` —
  `attached` `BTreeSet` drains by id allocation order (readout.rs:320-335)
  affecting later stages. Fix: document or implement a stable authored
  rule.
- R14-F7 [LOW]: empty session validates `is_launchable` but `to_part_graph`
  returns `EmptySession` (session.rs:479-481,749-751,1364;
  readout.rs:148-150). Fix: mark empty as a validation error.
- R14-F8 [INFO]: `insert_stage` `u32::MAX` saturation silently merges
  stages (session.rs:1322; `set_stage` accepts any u32 at 1271-1278).
  Fix: cap `set_stage`/`insert_stage` inputs.

**Inputs**: `crates/csp-phys/src/{resolve.rs (320-460, 770-910, 990-1150,
1230-1280, 1350-1470, 1540-1620, 1800-1820), session.rs (470-490, 620-740,
940-1120, 1190-1290, 1310-1400, 1570-1670), readout.rs (240-360, 570-660),
profile.rs (130-145, 200-260), part.rs (180-190)}`.
**Outputs**: `crates/csp-phys/src/{resolve.rs, session.rs, readout.rs,
profile.rs}`.
**Validation**: FMT, CLIPPY, CHECK, `cargo test -p csp-phys`, WASM.
New tests: NaN attach position rejected at compute_attach; save→load
round-trip can't produce `BadTransform`; invalid upgrade levels rejected
at check_built.

**Phase-level validation**: phase-wide validator reads all sub-phase diffs
— check the weld-side thermal bound (12.1) and the resolver envelope gates
(12.3) don't double-reject valid input, and doc corrections are
consistent.

**Dependencies**: None strict; 12.1's `DockingConfig` bound coordinates
with Phase 9.2 `validate_balance` (assign the bound's canonical home:
`csp-sim` `DockingConfig::is_valid`; 9.2 calls it).

---

## Phase 13: Render & view-layer correctness
**Type**: Parallel — 3 sub-phases + phase-wide validator
**Determinism: NO (presentation)** — 14 fixes

### Sub-phase 13.1 — csp-view flight emit & focus (HIGH)

- R16-F1 [HIGH]: `emit` drops single vertices → misaligned `TriangleList`
  — flight.rs:813-817 pushes only on `Some`, but plume fan
  (flight.rs:902-904), plasma sheath (945-950), dust fan (1015-1017)
  emit per-vertex into the shared unindexed stream → one behind-eye
  vertex (`project_into` `None` at map.rs:977) shifts every later
  triangle. Fix: project all corners first, emit whole-triangle or
  skip — the all-or-nothing pattern already used at flight.rs:855-868
  and vab.rs:372-385.
- R15-F1 [HIGH]: flight focus pinned to `vessels.first()` — lowest
  `VesselId` forever (flight.rs:642,658; `FlightUiData.focus` written
  nowhere else; no focus-switch `FlightEvent` exists); empirically
  reproduced — second launch leaves camera/HUD/throttle on the parked
  starter craft. Fix: focus the most recently spawned/trackable vessel
  (or add a focus-switch event); keep csp-app's `SetThrottle` routing
  (csp-app/lib.rs:1169-1183) working.
- R15-F2 [MEDIUM]: `vessel_parts` fallback box double-offset —
  fallback builds `Box { lo: center−half, hi: center+half }` AND
  `transform.offset = center` (flight.rs:488-498) → renders shifted by
  `center`. Fix: `Box { lo: −half, hi: +half }` or identity transform.
- R15-F10 [LOW]: SurfaceLatched vessels re-weld every frame
  (flight.rs:430 + per-part walks at 451 inside `capture_sync_input`,
  lib.rs:955-956). Fix: cache keyed by vessel id + graph identity.

**Inputs**: `crates/csp-view/src/{flight.rs (370-500, 630-670, 760-1020),
map.rs (960-990), vab.rs (370-390), lib.rs (950-960, 1240-1260)}`,
`crates/csp-render/src/flight.rs:95-190`, `crates/csp-app/src/lib.rs:1160-1190`.
**Outputs**: `crates/csp-view/src/{flight.rs, lib.rs}` (+ csp-app only if
focus routing needs it).
**Validation**: FMT, CLIPPY, CHECK, `cargo test -p csp-view`, WASM.
Regression test: staged second spawn changes `flight_focus()` (extend
the pattern the verifier used — two `launch_craft`s then assert focus ==
the newest vessel); behind-eye vertex produces no vertex-stream
misalignment (count test or synthetic packet).

### Sub-phase 13.2 — csp-view map / camera / terrain + csp-render passes

- R15-F3 [MEDIUM]: `MapCameraF64` written by both `map_present`
  (map.rs:510) and `flight_present` (flight.rs:643/648) inside one
  unordered presentation tuple (lib.rs:846-854) — last writer wins in an
  unspecified order. Fix: split the resource or order the writes
  (chain/set the camera once); also gate `extract_flight`'s wasted
  `build_packet` on mode (flight.rs:775-787).
- R15-F6 [LOW]: parabolic mean-anomaly 2× the sim convention —
  `t + t³/3` (map.rs:851-852) vs sim `0.5*(d + d³/3)` (orbit.rs:724).
  Fix: `0.5 * (t + t³/3)` wait — per the verified math, divide by 2.
- R15-F7 [LOW]: `patch_might_hold_sea` assumes `ocean_datum_m ==
  radius_m` (terrain.rs:218-233,287-291). Fix: pass `datum` and compare
  `lo·amp < (datum − radius_m) + margin`.
- R16-F3 [MEDIUM]: transmittance LUT endpoint-baked vs interior-sampled —
  bake `col/(LUT_W−1)`, `row/(LUT_H−1)` (terrain.rs:539-541) vs shader
  `·d − 0.5` texel-center convention (terrain.wgsl:79-80) → up to ½-texel
  systematic bias. Fix: make the shader use `·f32(d.x − 1)`/`(d.y − 1)`,
  or interior-center the bake (choose one; update `transmittance_lut_is_sane`
  to cover the mapping).
- R16-F4 [LOW]: `max_level > 16` only `debug_assert!` — release `u16`
  wrap (terrain.rs:299-305,344-345). Fix: real assert/clamp.
- R16-F5 [LOW]: biome re-upload keys on `Arc::as_ptr` → latent ABA
  (terrain.rs:1032-1049). Fix: content key/generation counter like
  `uploaded_height_key`.
- R16-F6 [LOW]: `fs_sky` shell test uses f32 discriminant at planetary
  magnitude (terrain.wgsl:346-367). Fix: precompute the discriminant
  constant on the CPU in f64 (uniform) per the suggestion.
- R16-F7 [LOW]: `Validation`/`Timeout`/`Occluded` surface errors →
  silent permanent `Skip` (csp-render/src/lib.rs:160-167; caller
  csp-app/lib.rs:951-957). Fix: `warn!`-once + attempt reconfigure on
  `Validation`-class errors.

**Inputs**: `crates/csp-view/src/{map.rs (500-520, 590-600, 810-870,
930-1065), flight.rs (630-660, 765-790), terrain.rs (50-70, 100-130,
160-175, 190-235, 270-350), lib.rs (840-860)}`,
`crates/csp-render/src/{terrain.rs (290-350, 520-545, 980-1050),
lib.rs (140-170)}`, `crates/csp-render/src/terrain.wgsl`,
`crates/csp-app/src/lib.rs:945-960`.
**Outputs**: `crates/csp-view/src/{map.rs, flight.rs, terrain.rs}`,
`crates/csp-render/src/{terrain.rs, lib.rs, terrain.wgsl}`.

**Validation**: FMT, CLIPPY, CHECK, `cargo test -p csp-view`,
`cargo test -p csp-render`, WASM. Extend `render_gate.rs` if a
camera-ordering regression test is feasible.

### Sub-phase 13.3 — csp-app viewport & window plumbing

- R16-F2 + R17-F4 (merged) [MEDIUM]: `set_flight_viewport` called only in
  the `Resized` handler behind `(Some(g), Some(f))` (csp-app/lib.rs:1248-1252)
  — swallowed during deferred GPU init (wasm path 504-539) or when no
  `Resized` arrives → aspect stuck at `(1.0, 1.0)` (flight.rs:84) for the
  session; map/VAB re-feed per frame (688-693) but flight doesn't.
  Fix: re-feed flight viewport per frame alongside map/vab or call it in
  `finish_gpu_init` (545-599).
- R17-F5 [LOW]: `settings.graphics.window_size` written for every
  `Resized` including 0×0 (lib.rs:1244) → persists a 1×1 window next
  launch. Fix: skip zero-dimension writes.

**Inputs**: `crates/csp-app/src/lib.rs (460-540, 640-700, 950-960,
1230-1260)`, `crates/csp-view/src/flight.rs:80-90`.
**Outputs**: `crates/csp-app/src/lib.rs`.

**Phase-level validation**: phase-wide validator checks the camera
resource split (13.2) doesn't conflict with the viewport re-feed (13.3)
and the emit-atomicity change matches renderer topology.

**Dependencies**: None strict. All presentation-only.

---

## Phase 14: App & UI behavior
**Type**: Parallel — 5 sub-phases + phase-wide validator
**Determinism: NO** — ~20 fixes

### Sub-phase 14.1 — Warp controller (HIGH)

- R17-F1 [HIGH]: `demote(0)` no-ops at `effective == 0` (early return at
  warp.rs:219) leaving `requested`/`promote_streak` intact; `decide()`
  (warp.rs:142-180) runs pre-step (lib.rs:649-660) and rebuilds the
  promotion streak under a persistent `rails_warp_refusal` — the §2.6.2
  gate is defeated every ~30 frames (PROMOTE_FRAMES), injecting
  `SetWarp(100_000)` + ~1667 sim-s per cycle. Fix: make the refusal path
  reset `promote_streak`/`promote_candidate` (or make `demote` handle
  rung 0 correctly) and/or make `decide()` refusal-aware.
- R17-F2 [HIGH]: `scaled_nanos` never rebased (warp.rs:196-204) — after
  sustained high warp, stepping down leaves an outstanding absolute
  target that keeps stepping `MAX_TICKS_PER_FRAME` (~×64,800
  wall-effective) for ~30 s while HUD shows ×1. Fix: rebase/clamp the
  target on demote/step-down (rebase is legal per
  `advance_to_never_retreats_the_clock`, csp-sim lib.rs:9550-9566).

**Inputs**: `crates/csp-app/src/warp.rs (full)`,
`crates/csp-app/src/lib.rs:640-750`,
`crates/csp-sim/src/lib.rs:2968-3013,4044,6760-6801,9539-9566`.
**Outputs**: `crates/csp-app/src/warp.rs` (+ lib.rs only if the refusal
signal plumbing needs it).
**Validation**: FMT, CLIPPY, CHECK, `cargo test -p csp-app`, WASM.
Test: simulate refusal → assert no `SetWarp` promotion is emitted while
refused and the target is rebased on step-down.

### Sub-phase 14.2 — csp-app craft save & input gates

- R17-F3 [MEDIUM]: craft name is raw `TextEdit` text —
  `dir.join("{name}.cspcraft")` (lib.rs:1471-1477) permits `..`/absolute
  escape; separator names produce phantom `craft_store` entries
  (insert at 831-832 before the failing write; `set_craft_files` +
  Notice unconditional at 834-836; `load_craft_store` non-recursive at
  1443-1459). Fix: sanitize the name (reject separators/`..`/empty/
  control chars) and reorder insert-after-write.
- R17-F6 [LOW]: `outbox.launch`/`outbox.maneuver` enqueued with no
  `read_only`/demo/fatal gate (lib.rs:867-874) — inert-but-misleading
  mode switch (csp-view/lib.rs:1357-1368) while `fatal.is_some()` skips
  stepping. Fix: gate both paths with the same `demo`/`read_only`
  discipline as `craft_save` (816-830) + honest Notice.
- R17-F7 [LOW]: rebind capture misses egui-consumed keys —
  `key_press` only fires under `!consumed` (lib.rs:1260-1263); a
  keypress aimed at a focused TextEdit is swallowed; staged vs live
  keymap divergence after capture (bind_key writes staged,
  dispatch reads live until Apply, lib.rs:1069-1077,1135). Fix: route
  rebind capture before egui consumption and block repeats during
  capture.
- R17-F8 [LOW bundle]: `ui_confirm` blip asymmetry (lib.rs:806-814 vs
  1148-1206); wasm `save_settings` `Ok(())` no-op shows "settings saved"
  (settings_io.rs:124-126, lib.rs:860); audio `update()` plays cues
  before the `!alive` check (audio.rs:389-413); bare `1` throttle source
  id vs `WARP/MANEUVER/SETTINGS_SOURCE_ID` consts (lib.rs:1172,72-78;
  csp-view/lib.rs:1359 also emits on `1` — introduce a named const).

**Outputs**: `crates/csp-app/src/{lib.rs, settings_io.rs, audio.rs}` +
possibly `crates/csp-ui/src/vab.rs` (save_name sanitize at :978-980) and
`crates/csp-view/src/lib.rs` (shared source-id const).
**Validation**: FMT, CLIPPY, CHECK, `cargo test -p csp-app`, WASM.

### Sub-phase 14.3 — Navball (HIGH)

- R18-F1 [HIGH]: navball ground-cap fan paints sky as ground at positive
  pitch — independently reproduced ~9%→47% disc overpaint from +15° to
  +85°; "always star-shaped" comment false (navball.rs:133-163);
  `indices.windows(2)` drops the closing triangle (79 verts → 79
  triangles, closing edge unfanned); chord artifacts; dead-astern
  compass letters draw at ball center (navball.rs:196-198 vs the
  prograde clamp at 219-223). Fix: decompose into nadir half-disc +
  half-ellipse cap painted `ground` iff `uz < 0` (per the verified
  suggestion); fan the closing edge; skip dead-astern letters.

**Outputs**: `crates/csp-ui/src/navball.rs`.
**Validation**: FMT, CLIPPY, CHECK, `cargo test -p csp-ui`, WASM.
Regression: a geometry/paint-count test or golden region check for a few
pitch angles if feasible (the verifier's Python model can guide
tolerances).

### Sub-phase 14.4 — csp-ui settings / tutorial / VAB

- R18-F2 [MEDIUM]: settings X-close leaves `listening` armed —
  `egui::Window::open` at settings.rs:463-557 never clears `listening`;
  `rebind_target()` returns it unconditioned on `open`
  (csp-view/lib.rs:1056-1058); the next keypress is swallowed into the
  staged keymap (csp-app/lib.rs:1117-1131). Fix:
  `if !open { state.listening = None; }` (+ same for `reset_defaults`
  restage at csp-view/lib.rs:473-476).
- R15-F4 [MEDIUM]: `tutorial_drive` counts debris as `has_vessel`
  (tutorial.rs:81) — a wrecked climb suppresses the `vessel_lost`
  rewind (csp-ui/tutorial.rs:114-117,199-201). Fix: exclude
  `debris_since.is_some()` vessels.
- R18-F4 [LOW]: stale/hardcoded key names — `help_panel` fed the staged
  keymap (csp-view/lib.rs:326); hardcoded hints at csp-ui/lib.rs:228,
  328, 345, 388, vab.rs:1355, tutorial.rs:62. Fix: feed the live keymap /
  derive hints from bindings.
- R18-F5 [LOW]: hand-edited `settings.ron` bypasses validation —
  `#[serde(transparent)] KeyBinding` + `ron::from_str` accept raw strings
  (settings.rs:271-273, settings_io.rs:83-92); unparseable tokens bind
  dead actions; duplicates resolve inconsistently. Fix: run
  `KeyBinding::parse`/dedup at load.
- R18-F6 [LOW]: staged `Settings` not re-synced on reopen —
  `stage_settings` runs only at startup (csp-app/lib.rs:284; doc at
  csp-view/lib.rs:1007-1009 falsely claims opens-on-applied-state).
  Fix: restage on open.
- R18-F7..F11 [NITs]: dead `NavballInput::heading_deg` (navball.rs:34-36);
  module-doc contradiction on the dirty contract (vab.rs:28-30 vs 437+);
  hardcoded `64.0` ticks/s (csp-ui/lib.rs:551,573 — comment or wire
  through); dead `best_periapsis_m` (tutorial.rs:156-158); tutorial
  overlay overlapping the VAB inspector (tutorial.rs:246 vs vab.rs:848).
  Fix each trivially (remove/dead-code/doc/anchor).

**Outputs**: `crates/csp-ui/src/{settings.rs, lib.rs, navball.rs,
tutorial.rs, vab.rs}`, `crates/csp-view/src/lib.rs`,
`crates/csp-app/src/{lib.rs, settings_io.rs}`.
**Validation**: FMT, CLIPPY, CHECK, `cargo test -p csp-ui` + `-p csp-view`,
WASM.

### Sub-phase 14.5 — Launch/analysis staleness & spawn fallbacks

- R15-F5 + R18-F3 (merged) [MEDIUM]: `launch_craft` consumes the
  dirty-gated `analysis` cache — fresh `to_part_graph` + stale `effective`
  → `EffectiveIndexOutOfRange` (silent reject → log-only) or positionally
  misassigned part data; `analysis: None` → silent no-op return
  (csp-view/lib.rs:1268-1369); `demo.rs:189-193` documents the exact
  workaround LAUNCH lacks; `set_mode(Flight)` still runs on a doomed
  launch. Fix: `v.recompute()` in `launch_craft` before consuming
  analysis; push a `VabEvent::Notice` on failure; gate the mode switch on
  success.
- R15-F8 [LOW]: pad fallback picks an arbitrary non-launch site —
  `.reduce(|a,b| if b.launch { b } else { a })` (lib.rs:1314-1320) returns
  the last site in id order, not the documented equator fallback at 1321.
  Fix: prefer launch sites then the documented fallback.
- R15-F9 [LOW]: VAB renders an empty scene when `analysis` is `None`
  (vab.rs:230-254; `recompute` sets `analysis_error` at csp-ui/vab.rs:
  370-376 which the viewport ignores). Fix: surface `analysis_error` in
  the VAB view instead of an empty viewport.

**Outputs**: `crates/csp-view/src/{lib.rs, vab.rs}`,
`crates/csp-ui/src/vab.rs` (error surface if needed).
**Validation**: FMT, CLIPPY, CHECK, `cargo test -p csp-view` +
`-p csp-ui`, WASM.

**Phase-level validation**: phase-wide validator checks the
`launch_craft` recompute doesn't double-run the analysis on the same
frame and the source-id const (14.2) is consistent across crates.

**Dependencies**: None strict; all presentation/UI.

---

## Phase 15: Replay / scenario / bench harness
**Type**: Sequential · **Determinism-adjacent: harness correctness**
(goldens may need §8.4 re-bless after fixes) — 13 fixes

**Requirements** (verified-report-20; findings 1,2,3,5,6 empirically
reproduced):
- R20-F1 [HIGH]: `replay()` re-enqueues sim-produced Script/Autopilot
  entries → guaranteed `StampMismatch` — the restored world's control
  ticks re-issue the same commands into `pending`; `world.enqueue` at
  replay.rs:672 appends a second copy → `produced.len() != j`. Reproduced:
  `record()` of a scripted run returns `SelfCheckFailed`. Fix: filter
  recorded log entries whose `source_class` is `Script`/`Autopilot` from
  the replay enqueue path (they are re-derived deterministically), or
  suppress re-issue during replay — pick the option consistent with the
  §7.6 contract; document the ambiguity for externally-enqueued
  Script-class commands.
- R20-F2 [MEDIUM]: `LogAgent` off-by-one — `act` issues when
  `snap.tick + 1 == step.tick` (scenario.rs:533-552) while the drain
  stamps the pre-advance clock → `LoggedStep{tick:64}` stamps 63;
  `step.tick == snap.tick` → spurious "behind the clock". Fix: issue at
  `snap.tick == step.tick` boundary (per the dequeue-stamp semantics) and
  fix the doc at scenario.rs:90-91 + `log-replay.ron`'s comment. **This
  changes recorded stamp semantics — affected goldens need re-bless.**
- R20-F3 [MEDIUM]: `drain_pending` ignores `report.partial` —
  `step_frame(t + 1)` then only `rejected` inspected (scenario.rs:480-481);
  a setup `AdvanceTo(200_000)` partials at 69 120 and the rest is dropped
  — reproduced. Fix: continue stepping until `!partial` (bounded) or
  report the shortfall as an error.
- R20-F4 [MEDIUM]: `Lockstep::step` drops `partial` and `rejected` —
  single `step_frame` (agent.rs:301); `TranscriptEntry` has no
  rejection field (agent.rs:99-110); `scenario.rs:627` discards the
  report. Fix: propagate rejections into the transcript/EvalReport and
  re-issue or fail on truncated `AdvanceTo`s.
- R20-F5 [MEDIUM]: `ScenarioFile.save_seed` never applied —
  declared at scenario.rs:334-338 but `build_world` (426-462) never calls
  `set_save_seed` (replay.rs:539 does); reproduced: seed-42 scenario
  hashes identically to seed-0. Fix: call it in `build_world`; fix the
  stale "not consumed" doc.
- R20-F6 [LOW]: `record()` writes goldens `verify_golden` rejects —
  no empty-log gate (replay.rs:533-574) vs the `MalformedGolden` check
  (763-767). Fix: refuse empty-log recordings (or gate at write).
- R20-F7 [LOW]: `fixture_hash` name-match too broad + symlink loop —
  `path.file_name()` exclusions drop nested same-named files
  (replay.rs:488-493); `path.is_dir()` follows symlinks with no visited
  set (484). Fix: exclude only top-level artifact paths + symlink guard.
- R20-F8 [LOW]: `tick_timeout` drops on the (n+1)-th `Wait` —
  `waits > t` after increment (agent.rs:265-266) vs docs at 88,170-171
  reading as the n-th. Fix: doc or off-by-one.
- R20-F9 [LOW]: `Replay.save_seed` not cross-checked — `replay()` never
  reads `artifact.save_seed` (the restored world takes the snapshot's).
  Fix: cross-check or remove the field.
- R20-F10 [LOW]: `StampMismatch{index}` reports group start (677-678) vs
  doc "first divergent entry" (226). Fix: report the actual index.
- R20-F11 [LOW]: `run_suite` drops `read_dir` entry errors
  (eval.rs:213-216 `.flatten()`). Fix: surface enumeration errors.
- R20-F12 [LOW]: economy probe discards `AdvanceReport` (economy.rs:114)
  and `staffed_base_drains_in_band` never asserts `crew_count == staff`;
  `TICKS_PER_SECOND = 64` duplicates `csp_core::TICK_HZ` (economy.rs:63).
  Fix: assert + use the const.
- R20-F13 [LOW]: `mcp_mission_agent_flies_ascent` accepts non-JSON `raw` —
  assertion passes on any malformed response (eval_suite.rs:299-305,
  375-379). Fix: require the typed field.

**Inputs**: Read `crates/csp-bench/src/{replay.rs (140-230, 455-520,
530-720, 760-780), scenario.rs (85-110, 320-340, 420-490, 525-560,
600-640), agent.rs (80-110, 240-310, 370-400), eval.rs (200-230),
eval_suite.rs (290-380), economy.rs (60-120, 350-405)}` and
`crates/csp-sim/src/lib.rs (4044, 5885-5891, 6867)`.

**Outputs**: Modify `crates/csp-bench/src/{replay.rs, scenario.rs,
agent.rs, eval.rs, eval_suite.rs, economy.rs}` +
`data/eval-scenarios/log-replay.ron` comments if semantics change.

**Validation Criteria**: FMT, CLIPPY, CHECK, `cargo test -p csp-bench`,
TEST, REPLAY — plus the golden-re-bless rule for the stamp-semantics
change (F2). New tests: scripted-recipe record→verify round-trips clean
(the F1 reproduction must now pass); `drain_pending` surfaces a partial
`AdvanceTo`; `save_seed` changes the final hash.

**Dependencies**: Run after the csp-sim phases (2-8) so the harness
verifies the final sim semantics.

---

## Phase 16: xtask & CLI tooling
**Type**: Sequential · **Determinism: NO** — 13 fixes

**Requirements** (verified-report-21):
- R21-F1 [MEDIUM]: `shorten` panics on non-ASCII — byte-index slice
  `&s[..110]` (crates/csp-bench/src/bin/csp-perf.rs:82-89); real detail
  strings contain `≤`,`—`,`§`,`×`,`⁵` — latent today (no char straddles
  byte 110) but shifts with any edit. Fix: `char_indices`/floor char
  boundary.
- R21-F2 [MEDIUM]: `ALL_PKGS` hand-maintained (lint_deps.rs:192-206)
  vs glob `members = ["crates/*", "xtask"]` — a 14th crate passes the
  lint silently; same staleness trap in `DETERMINISM_CRATES` (59) and the
  hardcoded clippy.toml test list (786,811). Fix: derive membership from
  `cargo metadata`/the workspace and cross-check.
- R21-F3 [LOW]: `locked_version` first-match pin is order-dependent
  (build.rs:73-125) — first source-less block wins; a divergent
  `rand_chacha` pin earlier in file order would fingerprint the wrong
  version. Fix: prefer the csp-core/determinism-domain member's pin or
  collect-then-resolve.
- R21-F4 [LOW]: `named` survives non-package tables (build.rs:88-94) —
  one-line reset fix.
- R21-F5 [LOW]: `semver_key` mis-orders release vs pre-release
  (build.rs:130-135). Fix: proper semver ordering or document the bound.
- R21-F6 [LOW]: no `--locked` on `cargo tree`/`cargo build`
  (lint_deps.rs:470-484,517-531; main.rs:241-253) — the nominally
  read-only lint can rewrite `Cargo.lock`. Fix: add `--locked`.
- R21-F7 [LOW]: `(*)` dedup marker pollutes `rhai_feature_set`
  (lint_deps.rs:543-565) — the `*` token lands in both feature sets;
  asymmetric dedup would false-fire. Fix: filter the `*` token.
- R21-F8 [LOW]: `lint_attribution` resolves `data/`/`assets/` relative
  to CWD (lint_attr.rs:217-224) — panics outside the root unlike the
  `env!("CARGO_MANIFEST_DIR")`-anchored siblings. Fix: anchor to manifest
  dir.
- R21-F9 [LOW]: `csp-eval` arg gaps — `--transcript` silently dropped in
  suite mode (csp-eval.rs:60-65,73-93,102-106); positional + `--suite`
  silently discards the positional; empty suite dir → `all_pass` vacuous
  `Ok(vec![])` (eval.rs:212-228). Fix: reject the conflicts and treat an
  empty suite as a harness error.
- R21-F10 [LOW]: partial golden dir treated as brand-new —
  `dir.join(REPLAY_FILE).exists()` keys both the orphan check (main.rs:109)
  and existing-vs-new (139-152) → a dir with `expected-hash.txt` but no
  `replay.bin` skips the hash-change refusal. Fix: key on the full
  artifact set.
- R21-F11 [LOW]: `csp-mcp` reconfigure swallows `load_balance` failure
  (csp-mcp.rs:74-84) and its comment is wrong (`script_op_budget` IS
  serialized — lib.rs:2883/2780; what isn't is `script_engine`/
  `autopilot_driver`). Fix: warn on `Err` + correct the comment.
- R21-F12 [LOW]: `gen_biome_maps` unbounded palette index +
  sloppy flags — `rasterize` (gen_biome_maps.rs:147-158) doesn't check
  index < palette len; `args().any(|a| a == "--check")` ignores every
  other arg (280). Fix: validate indices; strict arg parsing.
- R21-F13 [LOW]: `build_web` leaves a partial `dist/` on bindgen failure
  (main.rs:261-303). Fix: stage to a temp dir then rename, or clean on
  failure.

**Inputs**: Read `crates/csp-bench/src/{build.rs (60-140), perf.rs
(110-160, 340-380, 950-960), src/bin/csp-perf.rs, src/bin/csp-eval.rs,
src/bin/csp-mcp.rs}`, `xtask/src/{main.rs (60-310), lint_deps.rs
(50-210, 460-590, 780-815), lint_attr.rs (210-230),
bin/gen_biome_maps.rs}`.

**Outputs**: Modify `crates/csp-bench/{build.rs, src/bin/*, src/eval.rs}`
and `xtask/src/{main.rs, lint_deps.rs, lint_attr.rs,
bin/gen_biome_maps.rs}`.

**Validation Criteria**: FMT, CLIPPY, CHECK, `cargo test -p csp-bench`,
LINT (must still pass with derived membership), and run the touched CLIs
once (`cargo xtask lint-deps`, `cargo xtask lint-attribution`,
`cargo xtask bless-goldens --dry-run` equivalent if present).

**Dependencies**: None strict.

---

## Phase 17: apps/telemetry dashboard (TypeScript)
**Type**: Sequential · **Determinism: NO** — 8 fixes

**Requirements** (verified-report-22):
- R22-F1 [MEDIUM]: orbit-map projects `pos.z` as screen-y
  (apps/telemetry/src/lib/widgets/orbit-map.tsx:43-46) — `pos.z ≡ 0`
  for `i = 0` (inclination measured from +z, orbit.rs:742; shipped
  system is ~equatorial) → the marker slides along a horizontal
  diameter. Fix: project `pos.x`/`pos.y` into the ellipse's screen
  convention (verified internally consistent at :39-46); note the
  deeper limitation — for inclined orbits the marker shows a
  reference-plane projection while the ellipse is drawn in-plane;
  document or fold inclination in.
- R22-F2 [MEDIUM]: no reconnect/resubscribe — `subscribeTelemetry`
  once in `[]` deps (index.tsx:16-36); `onerror`/`onclose` only invoke
  `onState` (ws.ts:81-82); stale frame keeps painting. Fix: clear/mark
  the stale frame on close/error (and implement bounded reconnect for
  transient drops — in-place recovery is impossible after a server
  restart since the token rotates; document that).
- R22-F3 [LOW]: `.dash-grid` has no row sizing — `grid-template-columns`
  only (styles.css:66-70) while `rect` rows are documented in "tile
  height" units (server dashboard.rs:89-90). Fix: `grid-auto-rows` or
  `grid-template-rows` tile units.
- R22-F4 [LOW]: no inbound `proto` check — `ws.ts:68-79` dispatches on
  `kind` only; the server stamps + enforces `proto` (http.rs:507-585).
  Fix: carry + check `proto` per the bump contract (http.rs:82-84).
- R22-F5 [LOW]: `unknown_fields` only surfaced by gauge —
  `table.tsx:22` renders `—`, `graph.tsx:19-22` drops non-numbers,
  `orbit-map.tsx:25-26` mislabels unknown `elements` as "not on rails".
  Fix: surface unknown-field state in all widgets.
- R22-F6 [LOW]: graph hardcodes 16 Hz — `const rate = 16` (graph.tsx:14);
  the server's `subscribed` ack carries `rate_hz` (http.rs:562-565) and
  the client discards it (ws.ts:75-76). Fix: plumb `rate_hz` through.
- R22-F7 [LOW]: `vite dev` has no path to csp-server — no proxy in
  vite.config.ts; `cspToken()` returns `''` under dev. Fix: add a
  `/ws` + `/api` proxy block (with token forwarding documented) or a
  clear dev-mode notice.
- R22-F8 [LOW]: `loadDashboard` swallows failures (dashboard.ts:56-68 —
  any non-ok → bundled default silently); `saveDashboard` is dead code
  (71-81, zero callers) discarding server 400s. Fix: surface the failure
  distinction (unconfigured vs invalid vs network) and either wire
  `saveDashboard` or remove it.

**Inputs**: Read `apps/telemetry/src/lib/{widgets/*.tsx, ws.ts,
dashboard.ts, index.tsx, styles.css}`, `apps/telemetry/vite.config.ts`,
`crates/csp-server/src/{http.rs (70-90, 340-370, 480-590),
telemetry.rs (90-115), dashboard.rs (55-95, 145-160)}`.

**Outputs**: Modify `apps/telemetry/src/**` + `vite.config.ts`.

**Validation Criteria**: `npm`/project lint+build+test commands for the
app (run whatever `package.json` provides — `tsc --noEmit` at minimum,
plus `vite build`), and a code-review pass on the orbit-map projection
against the ellipse convention.

**Dependencies**: None strict.

---

## Phase 18: Test-suite & residual cleanup (LOW batch)
**Type**: Parallel — 3 sub-phases + phase-wide validator · ~6 fixes

### Sub-phase 18.1 — csp-sim test-module fixes

- R11-F1: `install_aero_tables_accepts_and_binds` only tests the reject
  path (lib.rs:10802-10846) — the `#[cfg(debug_assertions)]`
  worker-vs-sim byte-compare `Fatal` (5356-5371) is dead under test; add
  the success-path test covering install + `aero_mask` hot-swap + the
  byte-compare gate; also cover `SetAeroConfig` (zero coverage; hamming
  fallback at 5458-5467).
- R11-F2: `test_env`/`home_system` diverge from authored data —
  `test_env` has 2 `temperature_knots` vs authored 4 (lib.rs:10519-10552
  vs terrin/body.ron) and at least one test (`explosion_and_overheat_…`
  at 16595-16648) is load-bearing on the mock's 10 km ambient; nothing
  asserts `home_system` vs `PatchedConics::from_body_defs` (orbit.rs:2124
  pins the RON path only). Fix: field-wise binding tests with documented
  carve-outs (`period_s` unused in `home_system`, knot-count delta) or
  document + assert the intended deltas explicitly.
- R11-F3: `second_dock_records_a_second_seam_on_one_body` performs no
  second dock (lib.rs:14729-14749 — comment concedes it). Fix: rename
  honestly or exercise a real second capture.
- R11-F4 (test half): `theta_survives_snapshot_restore_and_revert`
  restores against an empty orbit model then hot-swaps (lib.rs:16458-16459)
  — after Phase 8 adds the `orbit.parent` gate, this test must restore
  against a populated model. Fix the test accordingly.
- R11-F5: stale comment + dead code — contradictory lines at
  lib.rs:16950-16952 (tick 66 vs 65) and dead `orbits` binding at
  16915-16916. Fix.

**Outputs**: `crates/csp-sim/src/lib.rs` (test module only),
possibly `crates/csp-sim/src/orbit.rs` tests.

### Sub-phase 18.2 — Residual LOW policy & doc items

- R4-F7 [LOW]: `TickIo::issue` trusts the engine's `SourceClass` stamp
  (lib.rs:7367-7372; `check_control_lease` returns `Ok` for `Ui`
  unconditionally at ~4272-4273). Fix: validate the stamp against the
  engine type or document the trust boundary.
- R8-F9 [LOW]: `steer`/`steer_dir` zero-dir parity — Rhai `steer(0,0,0)`
  → `SetAttitude{IDENTITY}` (csp-script/lib.rs:714-733) vs
  `IoControlApi::steer_dir` → current telemetry rotation
  (csp-script/lib.rs:150-157). Fix: align semantics.
- R8-F10 [LOW]: `BudgetExceeded` classified by error-string matching —
  `msg.contains("Terminated") || "too many operations"`
  (csp-script/lib.rs:973-986); a guest `throw` with the substring
  misclassifies. Fix: structured classification if rhai exposes one;
  else prefix-match or document.
- R8-F11b residual: source-id collapse — all ids > u32::MAX share one
  FIFO (csp-script:1030, csp-ai:201) — covered in code by Phase 3's
  clamp/gate unification; the remaining piece is documenting the
  `source_seq` channel semantics.
- R12-F7-adjacent docs & the R3 "Display `_ => Ok(())`" wildcard note
  (informational — fold into Phase 9.3 if not already handled there).
- Any `//`-comment corrections verified during earlier phases
  (e.g. the `latched_total` comment at lib.rs:3708 if Phase 3 didn't
  cover it; validate.rs:3004 docstring — covered by Phase 9.2).

**Outputs**: `crates/csp-script/src/lib.rs`, `crates/csp-ai/src/lib.rs`,
`crates/csp-sim/src/lib.rs` (docs only).

### Sub-phase 18.3 — Final sweep

- Re-run the full "found clean / verified clean" claims only where a fix
  touched the adjacent code (no re-review of clean code).
- Grep for any new `TODO`/`FIXME`/unused introduced during earlier
  phases (NO-SLOP audit).
- Docs: update PLAN.md/PLAN §2.6/§7.6 references only where a fix changed
  a documented contract (e.g. lease gate, `step_frame` Err contract,
  `AdvanceTo` stamp semantics).

**Phase-level validation**: phase-wide validator + all final gates
(see Success Criteria).

**Dependencies**: ALL earlier phases complete (this is the last phase).

---

## Final gates (run after all phases pass)

1. `cargo fmt --all -- --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo check --workspace`
4. `cargo test --workspace`
5. `cargo check --workspace --target wasm32-unknown-unknown --exclude csp-server --exclude csp-bench --exclude xtask`
6. `cargo xtask lint-deps && cargo xtask lint-attribution`
7. `cargo run -p csp-bench --bin csp-replay -- verify data/test-fixtures/trivial`
8. `cargo run -p csp-bench --bin csp-replay -- verify data/test-fixtures/flight-launch`
9. apps/telemetry build (`npm run build` / `tsc --noEmit` in `apps/telemetry`)

If REPLAY reds on intentional behavior changes (Phases 4, 5, 6, 9.2, 15
are the candidates), use `cargo xtask bless-goldens --reason` on a clean
tree per the §8.4 rule, then re-verify — never bless a `StampMismatch`
without root-causing it first (Phase 15's F1 fix specifically eliminates
that class of failure going forward).

## Dismissed findings (verified not-a-bug, no action)

- R9-F5 — off-grid `LifeSupportCheck` re-arm is unreachable: every event
  fires with `event.tick == clock` (lib.rs:6394-6399,6416,6795-6801);
  `.max(clock+1)` is dead code. No action.
- R12-F1 central claim — coincident AABB centres do NOT yield a zero
  attach axis (`f64::signum(+0.0) == 1.0` → axis +X); the residual LOW
  (arbitrary-axis inconsistency vs thermal edge-drop) IS fixed in
  Phase 12.2.
- R19-F20 sub-item — "server filesystem path echo via {e}":
  `io::Error`'s `Display` does not embed the path. No action.

## Statistics

- 18 phases (4 contain parallel sub-phases: 9×3, 12×4, 13×3, 14×5, 18×3
  — treat each listed sub-phase as its own implementer→validator loop
  with a mandatory phase-wide validator afterwards).
- ~191 deduplicated fixes scheduled (from ~210 verified finding entries;
  ~17 cross-report merges; 3 dismissed).
- Determinism-load-bearing phases: 2, 3, 4, 5, 6, 7, 8, 9.2, 10, 11, 12.1,
  15. Everything else is presentation, robustness, tooling, or docs.
