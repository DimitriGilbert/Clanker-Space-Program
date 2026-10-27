# Verified report C27 (session-full-20261026)

Verifier: independent re-derivation of every finding against the current tree
(no edits made). Result: **8/8 findings CONFIRMED, 0 dismissed.** All cited
file:line anchors matched the tree (two off by ≤5 lines, noted inline). Both
PRD spot-check rows and both security spot-checks held up.

---

### Finding 1: `--serve` ships with zero flag surface — `--port`/`--bind-lan` missing, unknown flags silently ignored — CONFIRMED

**Original summary**: PLAN §2.12.3 (normative) requires `--port` and `--bind-lan`;
`serve_cli` parses nothing, so the flags are silently dropped while the LAN
machinery is fully coded and e2e-tested.

**Verification/Reason**:
- `crates/csp-app/src/main.rs:16` — exactly `if std::env::args().skip(1).any(|a| a == "--serve")`
  followed by `serve_cli()`. Every other argument is ignored; there is no unknown-flag rejection.
- `crates/csp-app/src/lib.rs:2996-2999` — `serve_cli` builds
  `csp_server::serve::ServerConfig { spa_dir: Some(spa_dir), ..Default::default() }`.
  `ServerConfig::default()` (`crates/csp-server/src/serve.rs:51-60`) hardcodes
  `bind: 127.0.0.1:47900`. No port/bind parsing exists anywhere in the binary.
- Repo-wide grep for `--port`/`--bind-lan`/`bind_lan` across `crates/`, `apps/`, `xtask/`
  (Rust sources): matches are **comments and doc text only** (http.rs:13/:235/:978/:986,
  serve.rs:4/:30, e2e.rs:1177, PROTOCOL.md:64) — zero parsing sites. The only production
  `ServerConfig` construction outside csp-server itself is csp-app lib.rs:2996.
- PLAN normative lines, quoted verbatim:
  - `PLAN.md:878`: "Default bind `127.0.0.1` on a fixed default port (`--port` to override);
    a lockfile in `$XDG_RUNTIME_DIR/csp/` handles multiple instances and discovery."
  - `PLAN.md:889-890`: "LAN binds require an explicit `--bind-lan` flag, print a warning, and
    require the token on every endpoint. No TLS in v1; that is stated as a limitation, not hidden."
- LAN path truly unreachable from shipped binaries: `bind` reaches `0.0.0.0` only if a caller
  constructs `ServerConfig` by hand. The one shipped caller uses the loopback default. The
  machinery is real and tested — `BindAllowlist::{Loopback,Bound,Wildcard}` +
  `for_bind` (http.rs:225-254), and `e2e.rs:1181-1219`
  (`lan_bind_allowlist_accepts_ip_authorities`) binds `0.0.0.0:0` directly in test config.
- Not PLAN-sanctioned: §6.4's cut list (PLAN.md:1543-1559) covers render tier, life support,
  third body, docking, web-tier polish — nothing about server flags.
  `docs/DATA-AND-SCRIPTING.md:646-649` and `:809`/`:822` document the absence as a
  divergence ("`--bind-lan` flag **does not exist** … `serve_cli` uses `ServerConfig::default()`"),
  which is honest recording, not a sanctioned cut.
- Severity HIGH is justified: `csp --serve --port 5000` appears honored but silently binds 47900.

### Finding 2: `session.save` writes non-atomically, breaking the atomic-rename discipline the Game menu and dashboard store follow — CONFIRMED

**Original summary**: `store_save` ends in bare `std::fs::write`; the windowed app and
`DashboardStore` both use temp+rename; a crash mid-overwrite destroys a good save.

**Verification/Reason**:
- `crates/csp-server/src/backend.rs:607` — `std::fs::write(self.save_path(name), bytes)`:
  confirmed bare in-place write (open-with-truncate then write).
- Contrast confirmed:
  - `crates/csp-app/src/lib.rs:1133-1138` — comment "The atomic-rename rule — a killed
    writer never leaves a half file for `inspect_save` to trip on." + `.{slot}.cspsave.tmp`
    write then `rename`.
  - `crates/csp-server/src/dashboard.rs:238-255` — "the temp+rename discipline means even a
    panic mid-write leaves at worst a stray `.*.tmp`, never a torn target file" + temp/rename
    under the write lock, with tmp cleanup on rename failure.
- Overwrite-is-intended confirmed: backend.rs:563-573 — the cap exempts existing names
  ("the cap binds *new* entries, not updates (C17-F1)"), and unit test
  `store_save_is_capped_but_overwrites` (backend.rs test module) asserts overwrite at cap.
  So the torn-file scenario targets a previously good save.
- `list_saves` metadata-only confirmed: backend.rs:479-509 — header-prefix scan only;
  the code's own comment at :505-507 says "a truncated body still fails at `load_save`",
  i.e. a torn file stays listed until load.
- Two precision nuances (do not change the verdict):
  1. The "rule" is a project convention named in code ("The atomic-rename rule"), not an
     explicit PLAN §-norm — PLAN's only atomic-file-write rule is the token file
     (PLAN.md:636, "mode 0600, atomically"). The finding stands on the convention + real
     data-loss risk + three-writers-two-styles inconsistency.
  2. The report's framing "serve_cli deliberately shares the game's saves dir" echoes the
     serve_cli comment (lib.rs:2969-2970), but the windowed app's save dir is
     `<crafts parent>/saves` (lib.rs:490-496) while serve_cli uses
     `settings_io::config_dir().join("saves")` (lib.rs:2974) — the parallel saves-dir
 divergence finding. Either way, overwriting a prior `--serve` save mid-write destroys it.

### Finding 3: quaternion wire order `[w,x,y,z]` undocumented, comment wrong, telemetry emits `{x,y,z,w}` — CONFIRMED

**Original summary**: `get_quat` takes `attitude.rot` as `[w,x,y,z]`; the comment claims
"DQuat's serde order is (w, x, y, z)" which is false; telemetry output is an `{x,y,z,w}`
object; an agent round-tripping gets a silently wrong attitude.

**Verification/Reason**:
- `crates/csp-server/src/mcp.rs:720-733` — confirmed:
  - `:729` `DQuat::new(f(1)?, f(2)?, f(3)?, f(0)?)` — with `DQuat::new(x, y, z, w)`
    (quat.rs:47), the array is unpacked w-first. Input order IS `[w, x, y, z]`.
  - `:728` comment "DQuat's serde order is (w, x, y, z)." — **false**:
    `crates/csp-core/src/quat.rs:34-41` declares `#[derive(Serialize, Deserialize)] struct DQuat { x, y, z, w }`
    — derived serde order is `{x,y,z,w}`. (The code doesn't rely on serde here — manual
    unpack — so the bug is documentation, but it misleads exactly the reader it addresses.)
  - `:731` error message states the order only on the error path; the tool descriptor
    `:769` `"rot": v4()` and the `vessel.control` description (`:762-763`) state no order.
- Telemetry emits an object, confirmed: `VesselTelemetry.rot: DQuat`
  (`crates/csp-sim/src/scripts.rs:370`) is flattened into the wire snapshot
  (`SnapshotVessel` `#[serde(flatten)]`, telemetry.rs:107-116), serialized via
  `serde_json::to_value` (telemetry.rs:191/:222) → `{"x":..,"y":..,"z":..,"w":..}`.
- Silent-wrong-attitude confirmed: the apply-time gate at `crates/csp-sim/src/lib.rs:5279`
  is `rot.is_finite() && ang_vel.is_finite() && rot.norm() > 0.0` — a component
  permutation preserves the norm exactly, so the gate passes and the permuted quaternion
  applies. (quat.rs:95-102's own doc names this gate as "not optional" — it checks norm,
  not semantics.)
- Docs: `docs/DATA-AND-SCRIPTING.md:670` documents the tool as `{rot[4], ang_vel[3]}` with
  no order; the only order statement anywhere is for the *Rhai verb*
  `steer_quat(w, x, y, z)` ("scalar-first", line 462) — a different surface.
- Tests: the only attitude test in mcp.rs is `non_object_attitude_is_invalid_params`
  (shape rejection); e2e.rs has zero `attitude`/`rot` occurrences. No test pins the order.
  CONFIRMED in full.

### Finding 4: broken data dir panics the game thread → zombie server serving the stale empty snapshot — CONFIRMED

**Original summary**: the world-builder closure panics on `View::new()` error; the thread
dies before the serve loop's real snapshot publish; the pre-spawn `World::new()` snapshot
stays live; the server keeps 200-ing an empty world and mutations answer "game thread gone";
no non-zero exit.

**Verification/Reason** — full trace:
- `crates/csp-app/src/lib.rs:2980-2984` — builder: `match csp_view::View::new() { Ok(v) => v.sim, Err(e) => panic!("data/ startup load failed: {e}") }`.
- `crates/csp-server/src/backend.rs:856-858` — `spawn_full` publishes the pre-spawn
  `TelemetrySnapshot::from_world(&World::new())` into the ArcSwap **before** spawning the
  thread; `:863-865` — inside the thread, `make()` runs first (`HostCore::new(make(), ..)`),
  then the real snapshot is stored. A panic in `make()` kills the thread between the two
  publishes; the ArcSwap keeps the tick-0 empty world forever.
- Thread death does not propagate: `std::thread::spawn` panics surface only at `join()`;
  `spawn_full` returns Ok, `serve_cli` proceeds to `run_blocking`.
- `crates/csp-server/src/serve.rs:186-214` — `run_blocking` prints
  "csp-server: listening on http://127.0.0.1:47900" then `tokio::select!` on **ctrl_c or the
  serve task** only. The axum serve task outlives the game thread, so the process keeps
  serving; a startup failure never exits non-zero.
- Reads vs mutations after death: `ControlBackend::telemetry` returns
  `self.published.load_full()` (backend.rs:988-990) — the stale snapshot, so `/` and
  `/api/telemetry` keep 200-ing the empty tick-0 world; every `call` fails `tx.send` and
  returns exactly `BackendError::Session("game thread gone")` (backend.rs:943-944; the
  timeout arm at :946-953 never even applies — send fails instantly).
- Detail: csp-app's bug-report panic hook is installed in `run()` (lib.rs:3080), not
  `serve_cli`, so the panic prints via the default hook and the process keeps running.
  CONFIRMED as described.

### Finding 5: MCP read surface exposes counts, not ids — `ManufactureCraft` undiscoverable over the protocol — CONFIRMED

**Original summary**: `ProgramSnapshot.sites` is a `usize`; the worked example's launch step
needs `ManufactureCraft {site: SiteId, craft: CraftDef, build_resources}`, but no MCP tool
or resource exposes site ids or craft/part definitions.

**Verification/Reason**:
- `crates/csp-server/src/telemetry.rs:72` — `pub sites: usize` (likewise `contract_offers: usize`
  at :58, `contracts_active: usize` at :60). Only `colonies` and `scripts` carry identity rows.
- `crates/csp-sim/src/lib.rs:1025-1035` — `ManufactureCraft { site: SiteId, craft: CraftDef,
  build_resources: Vec<(ResourceId, f64)> }`, exactly as quoted.
- Full `tools_v1` walk (mcp.rs:740-859): telemetry.get, vessel.list, vessel.control,
  vessel.take_control/release_control, mission.nodes, timewarp.set/advance_to, sim.command +
  7 session/dashboard tools — none expose site ids or craft/part definitions;
  `resources_v1` is a single snapshot resource. The only path is the `sim.command` raw
  passthrough, which requires already knowing the ids. CONFIRMED.

### Finding 6: DATA-AND-SCRIPTING's "every step is exercised in e2e.rs" overstates — CONFIRMED

**Original summary**: e2e covers connect/launch(SpawnVessel)/lease/advance/hash parity, but
the science verbs and `session.save`/`session.load` over the wire are absent; the save
round-trip is unit-tested in backend.rs only; `ManufactureCraft` is driven nowhere.

**Verification/Reason**:
- `docs/DATA-AND-SCRIPTING.md:748` — the sentence exists verbatim: "Every step is exercised
  in `crates/csp-server/tests/e2e.rs`" (following §5.8's steps: launch incl.
  `ManufactureCraft`, science `RunExperiment`/`TransmitData`/`RecoverData`, save).
- Grep of `crates/csp-server/tests/e2e.rs` for `RunExperiment|TransmitData|RecoverData|
  ManufactureCraft|session.save|session.load`: **zero matches** (the only "session" hit is
  the tools/class assertion at :482). The e2e coverage list the doc itself enumerates
  (token lifecycle, allowlists, round-trip/ordering, ingress, lease, hash parity, WS,
  dashboards, SPA, blocking-pool) matches what the suite contains — none of it is the
  science or wire-save steps.
- backend.rs test module: `store_save_is_capped_but_overwrites`, `list_saves_reads_headers_only`,
  truncated-save `load_save` failure — the save round-trip is unit-tested there only.
  CONFIRMED.

### Finding 7: stale in-tree comments claim the SPA is not wired into any binary — CONFIRMED

**Original summary**: http.rs:52-56 and e2e.rs:78-79 say `spa_dir` has no binary wiring /
"`/` serves the placeholder in production today", but `serve_cli` sets
`spa_dir: Some(packaged_spa_dir())` and the SPA dist exists.

**Verification/Reason**:
- `crates/csp-server/src/http.rs:52-56` — "`spa_dir` has no binary wiring yet. … is `None`
  in every shipped path (no binary sets it), so `/` serves the placeholder in production
  today." Stale: `crates/csp-app/src/lib.rs:2995-2997` sets `spa_dir: Some(spa_dir)` with
  `packaged_spa_dir()` (lib.rs:3007-3029), and
  `apps/telemetry/dist/client/index.html` exists on disk.
- `crates/csp-server/tests/e2e.rs:78-79` — "the M9d SPA-serving path no binary wires up
  yet" — same staleness. This matters beyond tidiness: the comment sits in the
  security-relevant doc block describing what `/` and `/assets/*` serve in production.
  CONFIRMED.

### Finding 8: every WS telemetry frame re-serializes the entire snapshot, per client, per tick — CONFIRMED

**Original summary**: `project` pays one whole-snapshot `serde_json::to_value` per call;
the WS loop calls it per client per tick — 16 clients × 16 Hz = 256 full serializations/s
even for identical subscriptions; fine at current sizes, scales with vessel count.

**Verification/Reason**:
- `crates/csp-server/src/telemetry.rs:216-237` — `project` calls `serde_json::to_value(self)`
  at :222 per invocation (its doc comment at :213-215 correctly claims only "once per call,
  not once per field" — per-client is unbatched). The diagnostic `field()` (:190-192) is the
  same shape per call.
- `crates/csp-server/src/http.rs:709/:745` — "One WS session: subscribe → project fields per
  tick of the rate" — `snap.project(fs)` runs inside each session's per-tick loop, so N
  clients × M ticks = N×M whole-snapshot serializations. The 16-connection cap and the
  16 Hz default rate (docs §5.7: `rate_hz ∈ {64..1}`, default 16) make the 256/s figure
  arithmetically right at the caps. Confirmed as a real, correctly-scoped perf nit (the
  report itself flags current sizes as fine).

---

## PRD coverage table spot-checks (2 of 21 rows)

- **MessagePack "DEGRADED (JSON-only; documented deferral + budget tripwire; not on §6.4
  cut list)"** — ACCURATE. http.rs:30-40 is an explicit deferral note ("§2.12.2 deferrals
  (recorded here so the gap is explicit, not silent)") naming the tripwire e2e
  (`ws_subscription_stays_within_byte_budget`) and the `proto` bump mechanism; PLAN §6.4's
  cut list contains no such item.
- **`craft.load`/`editor.build` "MISSING"** — ACCURATE. Neither string appears anywhere in
  `mcp.rs` (tools_v1 fully walked, Finding 5); DATA-AND-SCRIPTING.md:806 marks them
  "**[ABSENT]** (use `sim.command`)" and §8.1 divergence #1 records the same.
- (Bonus, from deep-verify a: the `--port`/`--bind-lan` MISSING rows are accurate per Finding 1.)

## Security verdict spot-checks (2)

- **Constant-time token compare** — CONFIRMED present: `constant_time_eq`
  (http.rs:393) used in both bearer paths (http.rs:362, :387).
- **Traversal-proof asset gate** — CONFIRMED present: `clean_asset_path` (http.rs:437-444)
  rejects `\`, NUL, `:`, any empty `/`-segment, and requires every `Component` to be
  `Normal(_)` — killing `..`, `.`, roots, and Windows prefixes; the Origin/Host allowlist
  machinery (http.rs:225-278) matches the report's description and the e2e LAN test
  (e2e.rs:1181-1219) exercises the wildcard shape. The clean security verdict stands on
  everything sampled.

## Verdict

8 confirmed / 0 dismissed. The HIGH (Finding 1) is real and exactly as described:
PLAN §2.12.3's normative `--port`/`--bind-lan` are unwired, unknown `--serve` flags are
silently ignored, and no shipped path can reach the (working, e2e-tested) LAN machinery.
Original report: review-report-C27.md; this verification: verified-report-C27.md.
