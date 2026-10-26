# PRD-19 Review — csp-server: HTTP/WS/MCP interface tier

**Files reviewed:** `crates/csp-server/src/{mcp.rs (1124), http.rs (1059), backend.rs (1170), dashboard.rs (472), serve.rs (289), ingress.rs (273), telemetry.rs (144), lib.rs (45)}`, `crates/csp-server/tests/e2e.rs` (1293), `crates/csp-server/PROTOCOL.md` (96). Cross-checked against `PLAN.md §2.12/§2.12.1–3`, the `SimCommand` enum (`csp-sim/src/lib.rs:515+`), `VesselTelemetry` (`csp-sim/src/scripts.rs:328+`), `AdvanceReport` (`csp-sim/src/lib.rs:1244+`), the `TakeControl` apply (`csp-sim/src/lib.rs:5079–5130`), `csp-bench/src/bin/csp-mcp.rs`, and every `Server::start`/`GameHost::spawn` callsite reachable by grep.

**Verdict:** This is the best-engineered crate in the review so far — the §2.12.3 security contract is implemented faithfully and *tested over real sockets* (Origin/Host allowlist derived from the bind, constant-time bearer compare, 0600-atomic token file under a 0700 dir, symlink-refusal, bounded everything, honest `partial`/`rejected` reporting, atomic batch rollback). The failures are **scope and reach**, not quality: **no shipped binary ever calls `Server::start`** (the "attach to a running game" client row of §2.12 exists only in tests), three of the four named session-class tool families (`editor.*`, `camera.*`, `map.focus`) are absent *and unimplementable* on the current `ControlBackend` seam, and the entire read surface bottoms out in a snapshot that contains only vessels + clock + warp — so an agent can mutate colonies/crew/contracts/funds/science through `sim.command` but can **never read any of it back**. The sim is writable by agents; it is not *playable* — the program layer is a write-only hole.

---

## Findings

### F1 — [HIGH] The HTTP/WS server has no host: `Server::start` is called only by its own tests

**Where:** `crates/csp-server/src/serve.rs:103` (`Server::start`), `src/backend.rs:797` (`GameHost::spawn`), `tests/e2e.rs:47` et al. (the only callers); `Cargo.toml` (no `[[bin]]`, no `src/bin/`); `crates/csp-app/Cargo.toml` (no `csp-server` dependency).

**Problem:** The crate ships zero binaries and nothing links it except `csp-bench` — which uses only `LocalGame` + `Mcp` for the **stdio** `csp-mcp` bin (`csp-bench/src/bin/csp-mcp.rs:27–104`). §2.12's table promises *two* MCP transports — stdio (own headless sim) and Streamable HTTP (**attach to a running game**) — plus telemetry web screens. The attach path exists as code (`GameHost` owns a `World` on a dedicated thread, exactly the shape `csp-app`'s frame loop would need, `backend.rs:716–986`) and is well-tested, but `csp-app` never embeds it, no binary parses `--bind-lan` or sets `spa_dir` (the latter is honestly documented as unwired, `http.rs:52–56`). The `<pid>.token` discovery file, the token-free SPA shell, the LAN allowlist — all production-hardened code with no production.

**Impact:** Every runtime guarantee in this crate is dead code outside `cargo test`. The "one control surface, four clients" table in §2.12 actually ships **one** client (stdio MCP) — the HTTP attach point and the telemetry screens it was built to serve are unreachable. This is the biggest gap in the cluster: not a bug, a missing integration.

**Suggestion:** Either wire `GameHost` + `Server::start` into `csp-app`'s frame loop (the design anticipates it — `GameHost` is `Send + Sync` precisely for axum) or ship a small `csp-serve` bin. Until then, `apps/telemetry`'s `CSP_SERVER_URL` proxy and the whole SPA have nothing to attach to.

---

### F2 — [HIGH] The promised tool list is short by the session-class families — `editor.*`, `camera.*`, `map.focus`, `craft.load` are absent, and `ControlBackend` cannot express them

**Where:** `src/mcp.rs:302–321` (`run_tool` — the complete dispatch: 16 tools), `mcp.rs:740–860` (`tools_v1`); `src/backend.rs:125–172` (`ControlBackend` — telemetry + submit + saves + summary, nothing else); `http.rs:41–51` (the `SessionCommand` deferral note); `PLAN.md:812` (session-class list) and `PLAN.md:826` (`craft.load`, `editor.build` named).

**Problem:** §2.12 names the session-class tools: `editor.*`, `camera.*`, `dashboard.*`, `map.focus`. Only `dashboard.*` exists. §2.12.1's example tool list names `craft.load` and `editor.build` — both absent. Worse than absent: **unimplementable on this seam.** `ControlBackend` exposes telemetry reads, `SimCommand` ingress, and save verbs — `editor.*`/`camera.*`/`map.focus` are *app-side* session state (the §2.2 category-C `SessionCommand` channel), which `http.rs:41–51` admits "does not exist in this crate" for dashboards alone. The deferral note is honest about dashboards but does not state the larger truth: attach-mode MCP can never reach the editor, camera, or map without a new backend verb family, and the stdio host (`csp-mcp` over `LocalGame`) can never have them at all — there is no app to edit.

**Impact:** The exploration-map question is answered: yes, `editor.*`, `camera.*`, `craft.load`, and `map.focus` are absent, and the absence is architectural. An agent can fly any vessel but cannot build or load a craft, move a camera, or focus a map — the attach surface covers the sim, not the session.

**Suggestion:** Extend `ControlBackend` with a session-verb method (a `SessionCommand`-shaped enum the app host can route and `LocalGame` can refuse honestly), then add the tools under `"class": "session"`. At minimum, `craft.load`/`session.load_craft` — loading a `CraftDef` by name from a craft library — is needed for `SpawnPhysicalVessel` to be usable without inlining a ≤64 KiB craft document into every `sim.command` call.

---

### F3 — [HIGH] The read surface is vessels + clock + warp: no program data (funds/crew/contracts/colonies/science/tech/scripts) is reachable over *any* interface

**Where:** `src/telemetry.rs:25–33` (`TelemetrySnapshot` = `{tick, warp_level, vessels}`), `telemetry.rs:53–72` (`from_world` reads only `vessels()`, `clock()`, `warp_level()`, `telemetry(id)`, `control_lease`); `src/mcp.rs:334–365` (`telemetry.get`, `vessel.list`), `mcp.rs:863–870` (`resources_v1` — the single `csp://telemetry/snapshot` resource), `mcp.rs:614–620` (`session.snapshot` → tick+hash only); `src/http.rs:559–564` (`/api/telemetry` — same struct). Contrast with what `World` actually offers: `economy()`/`funds_micros()` (`csp-sim/src/economy.rs:609–619`), `colonies()` (`colonies.rs:1498`), `contracts()` (`contracts.rs:610`), `science`/`tech` state (`lib.rs:1380–1392`), `scripts()`/`autopilots()` (`lib.rs:2525–2530`).

**Problem:** `sim.command` is a complete *write* surface — every one of the ~52 `SimCommand` variants (`HireCrew`, `AcceptContract`, `FoundColony`, `ResearchTech`, `ManufactureCraft`, `RunExperiment`, colony transfers, `LoadScript`, `EngageAutopilot`, …) is reachable. But the *read* side has exactly three doors — `TelemetrySnapshot` (WS fields, `/api/telemetry`, `telemetry.get`, the MCP resource), `session.snapshot` (tick + hash), and the dashboard store — and none of them contains a byte of program state. An agent can `HireCrew` and `FoundColony` but cannot read the roster it hired into, the funds it spent, the colony it founded, the contracts on offer, the science banked, or the tech researched. `GameHost::inspect` (`backend.rs:875`) is a ready-made generic read channel — used only by e2e tests, exposed by no transport.

**Impact:** "Fully playable by agents" is half true: playable means observe → decide → act, and the observe leg stops at vessels. An LLM agent driving a career game is blind on ~30 of the variants it can invoke — exactly the systems (colonies/crew/contracts) the PRD asked this review to check. The telemetry SPA inherits the same hole: `apps/telemetry` cannot surface program data because the snapshot does not carry it (PRD-20's "accidental better UI" is bounded by this struct).

**Suggestion:** Extend `TelemetrySnapshot` (or add sibling published snapshots — `ArcSwap` reads are free) with the program layer: funds/ledger tail, crew roster, contract offers + active, colonies summary, science banked/ledger keys, tech researched, scripts/autopilots status. All are already `Serialize`-able read states in `World`. At minimum, add a `program.get` tool or `csp://program/{economy,colonies,contracts,science,tech}` resources so the MCP surface is complete enough to *build* the missing UI on — which is the actual PRD question.

---

### F4 — [MED] `drain_and_step` discards `fired_events` and `degraded` — `timewarp.advance_to` cannot report what happened during the advance

**Where:** `src/backend.rs:393–421` (the drain loop collects `applied`/`rejected`/`reached`/`target`/`partial` only); `SubmitReport` (`backend.rs:68–86`) has no field for them; `csp-sim`'s `AdvanceReport.fired_events`/`degraded` (`lib.rs:1254–1281`) are dropped at `backend.rs:404–415`. Surfaced to agents at `mcp.rs:521–530` (`timewarp.advance_to` returns `{tick, rejected, partial, reached, target}`).

**Problem:** `step_frame` deliberately reports every event it fired — `PlayerAlarm`, `SoiTransition`, `ManeuverNodeStart`, `SoiRescan` — "so a UI/log layer can surface notifications without re-querying sim state" (`lib.rs:1256–1259`), plus `degraded` for §2.13 Recoverable event errors. The server is exactly that layer, and it throws both lists away. Consequences:

- An agent issuing `AdvanceTo` cannot learn that an alarm fired, a vessel crossed an SOI, or its maneuver burn started mid-advance — the events vanish between the sim and the wire.
- `degraded` Recoverable failures are dropped to **nowhere** — csp-server has no `tracing` dependency at all (`Cargo.toml`), so §2.14's "tracing at the csp-server boundary" is absent and these errors are not merely unrouted, they are unrecorded.
- `commands_vs_local_world_hash` (`e2e.rs:656+`) proves determinism survives; nothing proves *observability* survives.

**Impact:** Medium — agents can re-query telemetry after an advance (maneuver node state, residence, position all change observably), so this is degraded-observability, not blindness. But alarms are *the* notification channel the plan built for long timewarps, and dropping them makes `SetAlarm` pointless over MCP.

**Suggestion:** Add `events` (bounded — cap at the first N, report a truncation count) and `degraded` to `SubmitReport`; `fired_events` is already `Serialize`-shaped data. Also add `tracing` to the crate and log `degraded`/`rejected` at `warn`, closing the §2.14 boundary instrumentation the crate currently lacks entirely.

---

### F5 — [MED] Client identity is self-asserted: any bearer holder can claim any `client` name, inheriting its lease and refreshing its ingress budget

**Where:** `src/http.rs:615–625` (`?client=`/`X-CSP-Client` → `registry.id(name)` — no proof, just a string), `http.rs:703` (same for `/ws`), `src/backend.rs:197–237` (`ClientRegistry`), `src/ingress.rs:140–167` (per-client budget keyed on the claimed id). The lease side: `Lease::Agent(source_id)` (`csp-sim/src/lib.rs:441–448`), gated by `source_id` at apply (`lib.rs:5079–5130`).

**Problem:** The sim-side apply is sound — `TakeControl` checks `me == Agent(stamped source_id)`, so a command cannot forge another client's identity *inside* the command. But the stamping layer trusts the declared name: any token holder posting `?client=agent-a` *is* client `agent-a`'s index — inherits its `Agent(id)` lease (so `vessel.control` under a stolen name passes the drain check on a vessel that client legitimately took), shares its command log identity, and — because the ≤32-commands-per-drain budget is keyed on the name-derived id — can mint fresh names to reset its budget (bounded in absolute terms by `MAX_CLIENTS = 256` names × 32 + the 256-deep `DEFAULT_QUEUE_DEPTH`, so evasion buys unfairness, not unboundedness).

**Impact:** Within the single-bearer-token trust model this is defensible — PROTOCOL.md:23–28 even says it ("a `source_id` is a diagnostic tag, not a capability") — but the *lease* is a capability in §2.2.1's design, and it is keyed on a claim anyone can make. The documentation undersells the consequence: client naming controls lease identity and budget, not just log stamps.

**Suggestion:** Either scope leases to an authenticated identity (bind client index to the token + a server-issued session id), or document plainly that per-client budgets and leases are cooperative conventions among equally-trusted token holders — the plan's "the §2.2.1 rate limit and control lease" language reads stronger than this implementation provides.

---

### F6 — [LOW] The Origin/Host guard fails open on unparseable header values

**Where:** `src/http.rs:320–325` (`let Ok(s) = origin.to_str()` inside a let-chain — a `to_str` failure makes the whole condition false and the request proceeds), `http.rs:331–339` (same shape for `Host`, falling back to `req.uri().host()`).

**Problem:** An `Origin` header whose bytes are not a valid `str` (legal in `HeaderValue` — opaque bytes are allowed) skips the allowlist check entirely. Same for a `Host` that fails `to_str` when the URI carries no authority. The fail-safe direction for a DNS-rebinding gate is deny-on-unreadable; this is deny-on-mismatch, allow-on-garbage. Impact is limited — a hostname is ASCII, so an unparseable `Origin` can't carry a working attacker origin, and a browser never emits one — but a hostile non-browser client could send an unparseable `Origin` to stand *outside* the checked set, which is the wrong default for a guard whose entire job is paranoia.

**Suggestion:** `match origin.to_str() { Ok(s) if allowed => pass, _ => 403 }` — treat unparseable as foreign.

---

### F7 — [LOW] `GameHost` publishes a fabricated `World::new()` snapshot before the real world exists; `telemetry()` can never return `None`

**Where:** `src/backend.rs:813–815` (`ArcSwap::from_pointee(TelemetrySnapshot::from_world(&World::new()))` at spawn), `:822` (the real publish inside the thread). Consumers assume `None` means "not yet published" (`mcp.rs:268–270`, `http.rs:559–563`).

**Problem:** Between `GameHost::spawn_full` returning and the game thread's first `pub_in.store`, readers see a snapshot of an empty placeholder world — `tick 0`, zero vessels — indistinguishable from a genuinely empty world. `TelemetrySnapshot`'s `None` contract ("no snapshot published yet") is unreachable on `GameHost`. Small window (the thread spawns fast), but the fix is also small.

**Suggestion:** Store `Option<Arc<TelemetrySnapshot>>` in the `ArcSwap` (or `ArcSwapOption`) and publish the real one first — then `telemetry()`'s `None` is honest on both hosts.

---

### F8 — [LOW] A timed-out `/mcp` call still mutates the world — the 503 lies about what happened

**Where:** `src/http.rs:631–652` (`BACKEND_CALL_TIMEOUT` = 30 s around `spawn_blocking` → 503 "game thread did not answer in time"), `src/backend.rs:895–912` (`GameHost::call` waits up to `REPLY_TIMEOUT` = 120 s — the request keeps running on the game thread after the HTTP caller is told it failed).

**Problem:** The HTTP timeout detaches the *caller*, not the call: a `session.load` or a 64-`step_frame` drain that outlives 30 s still completes server-side while the client received a failure. The dropped `reply.send` is handled correctly on the host side (`backend.rs:823–837`), so the server stays healthy — but the client's model of the world ("the command did not run") can be wrong, which is a worse failure than a slow `Ok`. For `AdvanceTo` this is mostly benign (idempotent-ish); for `session.load` it is a silent world replacement.

**Suggestion:** Cheap fix — make the 503 body honest ("request may still have applied; re-query `session.snapshot`"). Better: propagate cancellation (drop the reply channel → the host skips the drain) where feasible.

---

### F9 — [INFO] ~44 `SimCommand` variants are reachable only through schemaless `sim.command` — no discoverability at all

**Where:** `src/mcp.rs:815–822` — the descriptor is `{"command": {"type": "object"}, "commands": {"type": "array"}}`, i.e. no schema; decode failures surface as raw serde strings (`mcp.rs:541–549`). Named tools cover only 8 variants (`SetWarp`, `AdvanceTo`, `SetThrottle`, `SetAttitude`, `TakeControl`, `SetManeuver`, `DeleteManeuver` + telemetry reads).

**Problem:** The escape hatch is *complete* (every variant decodes) but *opaque*: an agent must know variant names and field shapes out-of-band — the wire offers no enumeration (no `csp://commands/schema` resource, no `sim.commands` listing tool, no tool description pointing at the variant set). Combined with F3 this is the practical ceiling on "fully playable": write-blind *and* schema-blind for everything past the flight basics. The e2e proves an agent that already knows `{"SpawnVessel": {...}}` works (`e2e.rs:598–605`); nothing helps an agent that doesn't.

**Suggestion:** A static resource (`csp://schema/commands`) embedding `SimCommand`'s variant shapes — even just variant names + field lists generated from a debug impl or a hand-maintained table — closes discoverability without changing semantics (additive-only is the v1 rule anyway).

---

### F10 — [INFO] Documented deferrals, recorded here so they're in the gap ledger

- **MessagePack for array fields** (`PLAN.md §2.12.2`) — deferred with a tripwire (`http.rs:30–40`, the `ws_subscription_stays_within_byte_budget` e2e at `e2e.rs:987–1080` enforces 16 KiB/frame @ 16 Hz). Honest deferral — but note the deeper gap: `VesselTelemetry` (`csp-sim/src/scripts.rs:328+`) contains **no per-part fields at all**, so the "per-part detail is capped and opt-in" row has no data to encode in any format. MessagePack isn't the missing piece; the snapshot content is.
- **`SessionCommand` channel** (`PLAN.md §2.12.2`: dashboards "mutated via SessionCommand") — deferred at `http.rs:41–51` with the correct argument that the sidecar invariant (never in save/log/hash) holds regardless. Sound reasoning; F2 covers the part of this hole it doesn't mention.
- **`spa_dir` unwired** (`http.rs:52–56`) — documented; subsumed by F1.
- **Lockfile/multi-instance** — `PLAN.md §2.12.3` says "a lockfile in `$XDG_RUNTIME_DIR/csp/` handles multiple instances and discovery". What exists is the `<pid>.token` discovery file (`serve.rs:224–248`); a second `Server::start` on the default port simply fails `bind` (`serve.rs:134–136`) rather than coordinating. Discovery: yes; multi-instance handling: no. Minor.

---

### F11 — [LOW] Minor defects worth noting, not worth a section each

- `mcp.rs:662` — `self.dashboards()?.save(&dash.name.clone(), &dash)` clones the name to take a reference to it; harmless but odd (should be `save(&dash.name, &dash)` — may trip a clippy pedantic lint).
- `telemetry.rs:105–126` — `project` serializes the entire snapshot once per *subscribed client per tick*: 16 WS clients × 64 Hz = 1024 full `to_value`s/s. Bounded and small today (vessels-only snapshot), but if F3 lands (program state in the snapshot) the per-client projection cost grows — consider one serialization shared across subscribers per tick.
- `mcp.rs:180–183` — `initialize` marks the client initialized unconditionally; the method can never fail, so a malformed `initialize` (missing `protocolVersion`) still opens the surface for that client. Spec-lenient; harmless, noted.
- `http.rs:86` vs `dashboard.rs:62` — `VALID_RATES` is duplicated across the two files (same constant, two definitions); a drift hazard if a rate ever changes.

---

## What the review confirms is *right*

- **§2.12.3 security contract: strong.** Origin/Host allowlist *derived from the bind* (`http.rs:224–309` — loopback/bound/wildcard shapes all tested, `e2e.rs:399–442,1180–1219`); constant-time bearer compare (`http.rs:393–400`); `?token=` confined to `/ws` (`e2e.rs:272–289`); token file created 0600 atomically under a 0700 dir with symlink refusal (`serve.rs:224–289`, `e2e.rs:313–397`); token never in the served shell (`e2e.rs:200–267`); 64 KiB caps on body, frame, and encoded command (`ingress.rs:30`, `http.rs:704–705`); `/assets/*` traversal gate incl. Windows `\`/`:`/NUL shapes (`http.rs:437–444`, `e2e.rs:1088–1156`).
- **§2.2.1 lease + budget honesty:** per-client budget, queue depth, and rollback-on-partial-batch are real and tested end-to-end (`ingress.rs`, `e2e.rs:529–654`); commands stamp `(External, client_index)` and replay byte-identically (`e2e.rs:656–718`).
- **Transport correctness:** pinned `2026-07-28` with `2025-11-25` accepted and the revision asserted by test (`mcp.rs:43–48,902–908`); GET/DELETE `/mcp` → 405; no `Mcp-Session-Id`; the sessionless lifecycle gate is per-client-index and tested (`mcp.rs:916–949`); per-request SSE upgrade shape matches the revision.
- **Honest backpressure:** `partial`/`reached`/`target` on drains, `rejected` carries drain-time failures to the originating channel, WS drop-to-latest with bounded sends (`WS_SEND_TIMEOUT`), connection cap 16, idle-session reaping.
- **Test quality:** 27 e2e tests over real sockets including adversarial cases (symlinked token files, foreign Hosts, batch-residue leaks, worker-pool starvation).

## Cluster headline

The interface tier is **secure, correct, and unreachable**: it enforces §2.12's contract better than any other cluster implements its own section, but it ships as a library with no entry point (F1), its read surface stops at the flight layer while its write surface covers the whole sim (F3), and the session-class tools §2.12 names cannot exist on the seam as built (F2). `sim.command` makes the sim *scriptable* by agents; nothing yet makes it *legible* to them.
