# PRD-08 — Reliability, joints/docking, events, scripts host, save codec review

**Cluster files:** `crates/csp-sim/src/reliability.rs` (1834), `joints.rs` (1248), `events.rs` (719), `scripts.rs` (737), `save.rs` (754). Supporting reads: `crates/csp-sim/src/lib.rs` (dispatch, `check_serializable`, `apply_undock`, `apply_latch` call sites, `fire_due_events`, `WorldTickIo`, restore gates), `crates/csp-phys/src/{resolve.rs,profile.rs,weld.rs}`, `crates/csp-data/src/part.rs`, `crates/csp-view/src/lib.rs`, `crates/csp-ui/src/{program.rs,vab.rs}`, `crates/csp-app/src/lib.rs`, `crates/csp-server/src/{mcp.rs,backend.rs}`, `data/{balance.ron,parts/*.ron}`.

**PRD baseline (PLAN §3.4, §2.4, §2.6.5, §2.10, §7.2–7.4):** Λ/θ hazard with pre-drawn thresholds, dormant hazard at `λ_base·dormant_stress`, non-fatal dormant modes, qualification via flight+bench units, failure-as-published-`Silent`-event · transient joints ≤16/bubble, cap-degrade to clean separation + `Recoverable`, soft-capture → latch merge → `DockSeam` → undock split · event heap `(tick,seq)` with constant subsystem order · scripts as tick-entry programs, op budget, strikes≥5 disable, closed-form wakes only · CSPSAVE container (JSON header + DEFLATE/CBOR body, version chain) with the four §7.4 save refusals.

**Verdict:** the machinery is **honest and unusually thorough** — epoch-anchored Λ, closed-form dormant publish, verified stale-fire handling, the `{RELIABILITY}` renewal stream, the latch merge's momentum stamp, the wake↔event and record↔event pairing gates at restore, and the byte-exact save codec are all real and internally consistent. The gaps are (a) **content/UI reachability** — no `DockPort` part exists in `data/parts/` so the entire dock/latch/undock path is dead in shipped content; `RunBenchTest`, `LoadScript`, `EngageAutopilot`, `Undock`, `SetAlarm`, `WatchTransferWindow` have complete applies but no player path — and (b) a **second, divergent save format** in csp-server that bypasses the §7.2 codec entirely. Actual defects are few and mostly low-severity.

**Answers to the cluster's four prompt questions:**
- *Dormant-failure notification path:* exists and is honest — `Notification{PartFailed{mode,dormant},IgnitionMisfire}` → `World::notifications()` → `stage_program` → Program window "LOG" (8 newest, `csp-view/lib.rs:2987-3002`, `csp-ui/program.rs:489-498`) plus per-node `ReliabilityRow`s for the focused vessel (`csp-view/lib.rs:2711-2738`). The serialized-unhashed state class is implemented exactly as §7.2 requires. **But there is no HUD/in-flight cue at all** — a dormant failure on a racked vessel is invisible unless the player opens the Program window (see L3).
- *Joint cap degradation honesty:* honest but strict — the cap-refused pair enters the serialized `refused` set and stays refused for the whole approach (while inside capture geometry), even if a joint slot frees mid-approach (see M4). One `Recoverable` per approach, per the spec's logging rule — the log-storm avoidance is correctly handled.
- *Save-refusal coverage vs §7.4:* rules 1 (live joints) and 2 (live contacts, incl. vessel-vessel) are covered at `lib.rs:3210-3251`, plus an extra pending-command-queue check that is genuinely needed. Rule 3 (pending Stage-2) is **vacuous under the documented synchronous-Stage-2 deviation** — `ensure_aero` builds tables inline (`lib.rs:6215-6274`), so a "pending" weld can only ever exist as a queued `InstallAeroTables` command, which the queue check already catches. Rule 4 (session `SaveBlocking` from loader errors) is **not implemented** — no session-taint flag exists anywhere (see M2).
- *Honest machinery or defects too:* honest. The defects found are small: one crafted-save wedge (M1), one format fork (H1), one unimplemented §7.4 rule (M2), one stale doc claim (L5), and thin surfacing (L3).

**Correction to the exploration map:** *"failures fire and display in Program window"* is right but undersells it — dormant failures also reach the per-node reliability rows with wear fraction (Λ/θ), applied modes, misfire latch, and live thrust factor; the `dormant_stress` difficulty toggle is a *real* player path (`csp-app/lib.rs:560-572` writes `dormant_stress_scale=0` as construction config at new-game). Mission confidence (`exp(−Λ_expected)`) is also already displayed in the VAB (`csp-ui/vab.rs:2804-2815`), labeled honestly as "(unqualified family)" against the fixed `terrin_ascent` profile.

---

## Findings

### HIGH

**H1 — Two divergent save formats; the §7.2 codec is bypassed by the only functional save path.**

`save.rs` implements the §7.2-adjudicated container faithfully — `CSPSAVE\0` + plaintext JSON header (`save_version`, `game_version`, `hash_schema_version`, `kdf_version`, `data_hash_version`, `data_set_hash`, `label`, codec names, `body_len`) + DEFLATE-over-CBOR body, a real version chain (`SAVE_VERSION_MIN=2`, `CURRENT=3`), byte-exact re-encode proof (`save.rs:427-460`), `UnresolvableSchema` for the stamped-2 ambiguity, inflate bound, wasm32-safe framing, good tests. **It is used only by the bug-report bundle** (`csp-app/lib.rs:616-635, 2490-2502`).

The one live save/load path — MCP `session.save`/`session.load` — writes a **different format**: `SaveFile { schema: u32, tick, hash, snapshot: WorldSnapshot }` bincode (`csp-server/src/backend.rs:260-290`, encode at `:521-568`), also named `.cspsave`. It has no JSON header, no `data_set_hash`, no version chain, no byte-exact proof, and no DEFLATE. §7.2's ruling — one self-describing save format so field-tolerant decode and version migration are possible — is honored only by the artifact players can't use for saving.

*Impact:* two files with the same extension and different wire formats; the server path gets none of the codec's integrity or versioning guarantees; any future migration work lands on a format nobody loads. *Severity:* **High** as a design/PRD divergence (not a runtime crash).

*Suggestion:* route `csp-server`'s save store through `csp_sim::save::{encode_save,decode_save}` (or document the bincode store as a deliberately scoped session-artifact, per §7.6's "replay logs stay bincode" precedent — but then rename the extension).

### MEDIUM

**M1 — Restore accepts `θ = +∞`; the hash gate then fatals on it forever.**

`restore_vessel` accepts `rec.theta.is_finite() || rec.theta == f64::INFINITY` as coherent (`lib.rs:9814-9815`, "the `U=0` never-fails corner"). But `hash_record` writes `h.f64(r.theta)` (`reliability.rs:568`), and `StateHasher::f64` records a fault on NaN/±inf → `state_hash()` returns `Fatal` (`csp-core/hash.rs:296-311`). So a save containing `θ=+∞` — only producible by hand-editing a snapshot, since `fresh_record`/renewal clamp `U` to `MIN_POSITIVE` (`reliability.rs:417, 1300`) — **restores cleanly, then permanently wedges every state hash**. The restore gate and the hash gate disagree about the same value.

*Suggestion:* either reject `theta == +∞` at restore (the `MIN_POSITIVE` clamp makes it unreachable legitimately), or teach `hash_record`/`dormant_failure_tick` a canonical ∞ encoding. The first is a one-line gate change.

**M2 — §7.4 rule 4 ("session is `SaveBlocking` from loader errors") has no implementation.**

`check_serializable` (`lib.rs:3210-3251`) checks: pending commands (an extra, good rule), `joints.is_empty()` (rule 1), and per-bubble-body `ground_contact || bubble_vessel_contact` (rule 2 — with an honest comment that `SimpleBackend` cold-solves so this is "the plan's letter" protecting the Rapier future). There is **no session flag for "this world was built with loader errors"** — greps for `save_block|loader|tainted` over the sim return nothing. §2.13's `SaveBlocking` class is used only as an *immediate* error type at install/apply boundaries (`resolve.rs`, `weld.rs`, `aero.rs`, orbit duplicate ids) — never as a persisted "this session is tainted, do not save" marker.

*Impact:* narrow today — `View::new` propagates every install error (`csp-view/lib.rs:1127-1201`, all `map_err`), so a partially-installed world can't be reached through the real app. But a caller that swallows a `set_*`/`set_sites`/config error and continues gets a world that snapshots happily. The spec's mechanism — mark the session, refuse saves — simply doesn't exist. *Suggestion:* a `save_blocked: Option<String>` world field set by any install-time `SaveBlocking`, checked in `check_serializable`, serialized-but-not-hashed (it's session state, not evolution state — the `notifications` precedent).

**M3 — `data_set_hash` is recorded everywhere and checked nowhere.**

The §7.7 drift-warning key is written into the CSPSAVE header (`save.rs:358`), bug-report bundles (`csp-app/lib.rs:612-622`), and server `SaveInfo` — but no load path compares it to the live `csp_view::data_set_hash(&data_root())`. `decode_save` surfaces it on `Save` and nothing consumes it (`backend.rs::load_save` ignores it; the bug-report test only asserts the hex is in the fingerprint, `csp-app/lib.rs:2776`). §7.7's "a save written against a different data set warns" is unimplemented.

*Suggestion:* on `load_save`/any future player load path, compare `save.data_set_hash` against the current set and surface a `Recoverable`-class warning (load proceeds; the warning is the contract).

**M4 — Joint-cap refusal is whole-approach, not per-attempt — stricter than §2.4's letter.**

`lifecycle` at cap inserts the pair into `refused` and logs one `Recoverable` (`joints.rs:541-552`); refused pairs are pruned only when they *exit* capture geometry (`:518-520`) and the set is serialized (`joint_refused`, `lib.rs:3273`). So a pair refused while the bubble is saturated **cannot dock later in the same approach even if a slot frees** — it must physically leave the 0.5 m/15°/5 m/s predicate and re-enter. §2.4 says "joint creation beyond the cap degrades to an immediate clean separation (no joint)" — a per-attempt reading would let a freed slot capture a still-posed pair.

*Assessment:* defensible ("clean separation *for that approach*" is the module doc's own reading at `joints.rs:512-516`), deterministic, and the once-per-approach `Recoverable` matches the spec's logging rule. Flagged because it is *stricter* than the spec's letter and silently persistent across save/load — a 16-joint saturated station would refuse a waiting vessel with no obvious way for the player to learn why re-approach is required. *Suggestion:* if per-attempt is preferred, separate `cap_refused` (un-serialized, re-evaluated per tick) from `refused` (expiry-persistent). If whole-approach is intended, surface the refusal reason in the notification log — the `Recoverable` currently only reaches `degraded`/`log::warn`.

### LOW

**L1 — `VesselLifeSupportDepleted` pending on an absorbed vessel evaporates silently.**

`apply_latch`'s invariant comment and `debug_assert` (`joints.rs:1128-1138`) only cover `event_names_vessel` kinds — which deliberately excludes `VesselLifeSupportDepleted` (`lib.rs:8966-8971`: "never scrubbed... a stale firing is a no-op"). A pending crew-death event naming the secondary survives the merge, fires naming a dead `VesselId`, and no-ops — the re-seated crew on the merged hull (`reseat_roster`, `joints.rs:1171-1176`) escape a scheduled depletion kill. `TransferComplete` is correctly remapped (`remap_transfer_vessels`, `:1166`); scripts bound to `sec` correctly degrade to `Missing` telemetry; the absorbed autopilot is removed (`:1157-1162`). Only the LS-depletion edge is uncovered.

*Impact:* rare (requires a pending kill event during a docking tick) — low severity; worth a `transfers`-style redirect note or explicit event rewrite to the surviving `prim` if crew LS is ever reachable.

**L2 — `fire_stage` announces success optimistically before the jettison drains.**

`csp-view/lib.rs:2346-2351` issues `JettisonPart` commands and immediately pushes "STAGED — N PART(S) JETTISONED". A `StuckDecoupler`-flagged part rejects at apply (`lib.rs:6464-6474`), surfacing "COMMAND REFUSED — JETTISON: DECOUPLER IS STUCK" a frame later — the two notices contradict for one frame. Cosmetic; arguably correct-enough feedback since the refusal *is* shown.

**L3 — No HUD-level surfacing of `PartFailure`/`IgnitionMisfire` — the notification path is window-only.**

Per §3.4 the `PartFailure` event *should* be `Silent` (it is — `events.rs:474`), and the notification log *is* the spec's surface ("recorded… surfaced in a notification log"). Implemented exactly. But the log only renders inside the Program window's LOG section, capped at 8 lines (`csp-view/lib.rs:2987`), while the in-flight HUD has no failure cue at all — an engine failing mid-burn shows only as lost thrust (plus `thrust_factor` in the rows). `NoticeRow` also drops `n.node` — "vessel 3 — PART FAILED Leak" doesn't say which part (`csp-view/lib.rs:2989-2995`). *Suggestion:* pipe new `notifications` entries into `flight_notice` for vessels the player is flying (not a `Clamping` event — just a toast), and include the node id/part name in the notice text.

**L4 — {IGNITION} stream counter 0 is never drawn.**

`bubble_part_tick` increments `usage.ignitions` *before* drawing (`reliability.rs:1408-1427`), so the first ignition draws `U_1`, not `U_0`. Harmless — the draw stream is still per-index deterministic — but the `{IGNITION, instance, 0}` slot is dead by construction; if a future change ever wants "first ignition uses index 0" this is a silent schema-relevant detail. Note only.

**L5 — Stale module doc: `StuckDecoupler` claimed to have "no decouple command".**

`reliability.rs:69-72` says `StuckGimbal`, `SensorFault`, `StuckDecoupler`, `PanelJam` "record flags — the sim has no gimbal channel, sensor-fusion model, decouple command, or deployable state at v1". `StuckDecoupler` is wrong: it gates `apply_jettison_part` (`lib.rs:6464-6474`) which is the staging/decouple path (`fire_stage` → `JettisonPart`, `csp-view/lib.rs:2347`). The other three genuinely are flag-only. One-line doc fix.

**L6 — `DockingConfig::DEFAULT`'s `-1.0` sentinel is a latent trap.**

The `const DEFAULT` carries `capture_min_alignment_dot: -1.0` ("replaced below", `joints.rs:195`) and only `default_config()` resolves it to `cos(15°)` — any direct use of `DockingConfig::DEFAULT` produces an invalid config that `is_valid` rejects. Every current construction site uses `joints::default_config()` (`lib.rs:2425, 11199`), so it's a documented trap rather than a bug — but a `#[deprecated]`/`pub(crate)` demotion or a lazily-resolved accessor would remove the footgun.

---

## Reachability ledger (what's complete vs what the player can touch)

| Mechanism | Sim state | Player path | Evidence |
|---|---|---|---|
| Hazard Λ/θ, dormant accrual, renewal chain, non-fatal dormant filter | Complete, hashed+serialized | Live in flight (all 11 shipped parts carry `reliability:`) | `reliability.rs`, `data/parts/*.ron:45-62` |
| Failure modes → effects | `DegradedThrust`/`IgnitionFailure`/`Leak`/`StuckDecoupler`/`Explosion` real; `StuckGimbal`/`SensorFault`/`PanelJam` honest flag-only | Effects + Program rows + LOG | `reliability.rs`, `lib.rs:6464`, module doc `:69-72` |
| Ignition Bernoulli/misfire | Complete | Works; notice in LOG | `reliability.rs:542-559, 1401-1447` |
| Qualification (flight units) | Complete incl. burn credit on throttle-down/latch/mid-burn-kill | Accrues silently | `lib.rs:4154-4170`, `reliability.rs:764-866` |
| `RunBenchTest` + `BenchTestComplete` | Complete (funds+site-inventory charge, event pairing at restore) | **None** — apply at `lib.rs:4998`, no UI issues it | `reliability.rs:1129-1248` |
| Mission confidence `exp(−Λ)` | Complete (`profile.rs`) | VAB readout, fixed profile, "unqualified" caveat | `csp-ui/vab.rs:2804-2815` |
| Revert-to-launch | State-restore capable | **None** — no revert verb anywhere; documented deferral | `reliability.rs:32-38` |
| Joint soft-capture + latch merge + undock | Complete (PGS, documented non-Rapier) | **Dead** — no `DockPort` part in `data/parts/`; `ModuleDef::DockPort` schema exists | `joints.rs`, `csp-data/part.rs:568-574`, `data/parts/` listing |
| `joint_refused` serialization | Correct (hashed+serialized, canonical-pair gate at restore) | — | `joints.rs`, `lib.rs:9623-9640` |
| Event heap + classes + subsystem order | Complete | Warp demotion via `fired_events` (`csp-app/lib.rs:926-934`) | `events.rs` |
| Scripts: `LoadScript`/`UnloadScript`/`SetScriptEnabled`/wakes/strikes/poll | Complete; RhaiHost installed | **None** — MCP `sim.command`/bench only | `lib.rs:4969-4993, 7120-7392`, `csp-view/lib.rs:1125` |
| Autopilot: `EngageAutopilot`/`DisengageAutopilot` | Complete; `AscentAutopilot` installed, driver-presence gated | **None** | `lib.rs:4999-5036`, `csp-view/lib.rs:1126` |
| `SetAlarm`/`PlayerAlarm`, `WatchTransferWindow`/`TransferWindow` | Complete applies | **None** | `lib.rs:5204-5240` |
| `Undock` | Complete (seam split, momentum stamps, record partition) | **Dead** — needs a `DockSeam`, which needs docking | `lib.rs:6575-6825` |
| Save codec (CSPSAVE v2/v3) | Complete, well-tested | Bug-report bundles only | `save.rs`, `csp-app/lib.rs:616` |
| Player save/load | Server `SaveFile` (bincode) + codec exist | **None** — no UI verb, no key | `csp-server/backend.rs:521-568` |

## Notes on what is *correct* (verified, for the record)

- `dormant_failure_tick` is epoch-anchored against the *stamped* Λ and constant dormant λ — the publish stays bit-stable under every warp partition (`reliability.rs:525-537`).
- Stale early `PartFailure` fire: verified against `hazard_at(fire_tick) < theta`, restamps dormant accrual, clears the flag, re-publishes — and a crafted fatal-mode event clears `pending_failure_tick` rather than wedging the restore gate (`reliability.rs:968-1011`).
- Crossing consumes Λ (`hazard_integral = theta`) on both dormant and in-bubble paths — a late fire can't cascade backlog renewals (`reliability.rs:1066, 1286`).
- `event_names_vessel` scrubs `PartFailure` on ToBubble/unlatch/remove (`lib.rs:8932-8939, 3611`); `pending_failure_tick`↔event pairing enforced both directions at restore (`lib.rs:9708-9714`); absorbed-vessel records clear flags on merge/split (`joints.rs:1012`, `reliability.rs:1572-1590`).
- `apply_latch` momentum: `v = p/m`, `ω = I_m⁻¹·L` about merged CoM in the primary frame — conservation-exact (`joints.rs:955-979`); mixed reliability-spec coverage refuses the merge rather than silently binding specs wrong (`:922-938`); seam records remap all id fields; `ConfigMask` ≥64-bit edge handled (`:1057-1075`).
- `check_serializable` contact rule covers vessel-vessel contact too (`bubble_vessel_contact`) — mid-dock saves blocked before a joint even forms.
- `WorldTickIo::issue` derives the `(class, source_id)` stamp from the binding — a guest can't stamp `Ui` to bypass the lease gate (`lib.rs:8764-8779`); wake events pair strictly with `wake_tick` at restore (`lib.rs:3084-3100`); `fire_due_events` fires each event anchored at its own tick (partition-independence, `lib.rs:7409-7421`).
- Save codec: wasm32 overflow-safe `header_len` handling, `u64` `body_len` compare, inflate bound, byte-exact canonical-CBOR proof, honest `UnresolvableSchema` vs `UnsupportedVersion` distinction (`save.rs:385-460`).
