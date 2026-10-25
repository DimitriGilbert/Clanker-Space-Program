# Verified Report 17 — csp-server (HTTP/WS/MCP telemetry server)

Source report: review-report-17.md. Verified against working tree at HEAD (1e8c6da).
Concurrent-edit check: `git diff HEAD -- crates/csp-server` is empty and recent commits
(render/app/map/sim fixes) do not touch csp-server — no FIXED-BY-CONCURRENT-EDIT cases.

### Finding 1: `session.save` uncapped + `list_saves` full-decodes every save — CONFIRMED
**Original**: store_save has no count/size cap (contrast MAX_DASHBOARDS=64); list_saves bincode-decodes every save's full WorldSnapshot on the game thread (O(total-save-bytes) per call); disk fill by any token holder.
**Verification**:
- backend.rs:458-486 `store_save`: only `safe_name(name)?` then `std::fs::write` — no count or byte cap. `safe_name` (backend.rs:282-295) bounds only charset/length (1-64 chars of `[A-Za-z0-9._-]`) per name; the count of distinct names is unbounded.
- Contrast confirmed: dashboard.rs:49 `MAX_DASHBOARDS: usize = 64`; dashboard.rs:243-247 refuses new names at the cap under the write lock (R19-F17 TOCTOU note). The save store has no equivalent.
- backend.rs:409-414 `list_saves`: `std::fs::read` of each `*.cspsave` then full `bincode::serde::decode_from_slice::<SaveFile>` — decodes the entire snapshot to extract only `name`/`tick`/`hash` (lines 421-429). O(total-save-bytes) CPU per call.
- Game-thread claim confirmed: `SaveFile` embeds the full `WorldSnapshot` (backend.rs:473-478); `Request::ListSaves` is handled serially on the dedicated `csp-game` thread (backend.rs:736-776) alongside `Request::Submit`, so a slow `list_saves` stalls every submit/step/telemetry publish.
- 30s-timeout claim confirmed: http.rs:99 `BACKEND_CALL_TIMEOUT = 30s`, applied at http.rs:632; when the caller times out and drops its receiver, the game thread still finishes the decode (loop comment at backend.rs:742-745 explicitly keeps serving after a failed `reply.send` — the work is not cancellable).
- Reachability: `session.save`/`session.list_saves` dispatch at mcp.rs:313 and mcp.rs:588-603, token-gated by the guard (http.rs:351-367) — authenticated-only, exactly as scoped. MEDIUM severity is calibrated correctly.

### Finding 2: PROTOCOL.md session-class enumeration omits `dashboard.*` — CONFIRMED
**Original**: PROTOCOL.md:48-53 enumerates only the four `session.*` tools as session-class, but tools_v1() declares `dashboard.list/get/save` as `"class": "session"` too — pin-file drift.
**Verification**:
- PROTOCOL.md:49-51: "Session-class tools declare `\"class\": \"session\"` ... and are **not replay-relevant**: `session.list_saves`, `session.save`, `session.load`, `session.snapshot`." — reads as exhaustive.
- mcp.rs:742: `ses` constructor stamps `"class": "session"`; mcp.rs:843-858: `dashboard.list`, `dashboard.get`, `dashboard.save` built via `ses(...)`.
- mcp.rs:1011-1031 `tools_list_marks_session_class` asserts exactly 7 session-class tools, with the comment "M9d — the dashboard.* family joined the session class" — the family was added later and PROTOCOL.md's enumeration was never extended.
- PROTOCOL.md:11 carries the normative-pin language ("the revision is a deliberate commit: update this file and the constants"), so the doc is the designated source of truth and it under-states the surface. Doc-only, LOW — correct call.

### Finding 3: WS subscribe silently drops non-string fields and coerces non-u64 rate_hz to 16 — CONFIRMED
**Original**: filter_map drops non-string field entries without an error frame; non-u64 rate_hz becomes default 16 via unwrap_or — contradicting the explicit-error doctrine the same function states for the fields cap (R19-F14).
**Verification**:
- http.rs:792-800: `a.iter().filter_map(|f| f.as_str().map(str::to_string)).collect()` — `{"fields": ["tick", 5, "warp_level"]}` yields two fields; the `5` vanishes. If the surviving list is non-empty and under cap, the server replies `{"kind":"subscribed","rate_hz":...}` (http.rs:845-849) with no error and no field echo.
- http.rs:826: `v.get("rate_hz").and_then(Value::as_u64).unwrap_or(16)` — `"64"` (string), `0.5` (float), or missing-shaped values silently become 16, which is in `VALID_RATES` (http.rs:86: `[64, 32, 16, 8, 4, 2, 1]`), so the subsequent VALID_RATES check (827) passes and the mismatched cadence is acknowledged as honored.
- Doctrine contrast confirmed verbatim: http.rs:810-814 comment — "Over-cap is an explicit error, never a silent truncation ... would vanish without even an `unknown_fields` mention (R19-F14)" — sits directly between the two silent paths.
- Severity LOW (correctness/honesty, not exploitable) is right.

### Spot-check (clean claim): fail-closed Origin/Host allowlist + constant-time token compare — HOLDS
- http.rs:243-254 `BindAllowlist::for_bind`: unspecified→Wildcard, loopback→Loopback, else `Bound(ip)` — derived from the actual bind, never a hardcoded localhost set.
- http.rs:258-265 `name_allowed`: `Bound(ip)` requires exact `parse::<IpAddr>` equality; `Wildcard` accepts IP literals and localhost names only — unverifiable DNS names denied. Non-parseable hosts (`Origin: null`, percent-encoded garbage) fail parse and `localhost_name` → 403. Fail-closed confirmed.
- http.rs:303-309 `authority_host`: userinfo via `rsplit('@')`, IPv6 `[...]` bracket termination before the port split.
- http.rs:393-399 `constant_time_eq`: length XOR + every byte pair folded into one accumulator over `max(len)` — no early divergence exit; used for both bearer (http.rs:387) and the `/ws`-scoped `?token=` (http.rs:356-363, path check `path == "/ws"` prevents query-token on REST/MCP).
- guard order (http.rs:315-369): allowlist 403 precedes token 401; token-free surface is exactly `/` + `/assets/`. The report's strong-verdict prose is accurate.

## Summary
3 confirmed, 0 dismissed, 0 fixed by concurrent edit.
