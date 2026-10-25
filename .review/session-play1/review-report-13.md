# Review Report — P13: Audio (synthesized bank + low-latency subprocess backend)

Scope: `crates/csp-app/src/audio.rs` (cue_shot L100-130, CueTracker L132-165, PLAYERS/
player_args L167-222, MixState L224-287, writer_loop/MAX_PIPE_LEAD_S L289-323,
AudioBackend open/update/drop L357-545), `crates/csp-app/src/synth.rs` (OneShot bank,
render recipes, Rumble/Whoosh, xorshift Noise), `crates/csp-app/src/lib.rs` (post-
end_frame pump L897-910, ui_confirm L913-928). Cross-checked against
`csp-view/src/flight.rs` (SfxCue/FlightSfx/SFX_TTL_FRAMES, sfx_edges, focus_vessel,
PLASMA_* window) and `csp-view/src/lib.rs` (flight_notice, FlightFocusOverride,
presentation scheduling).

Ran `cargo test -p csp-app --lib -- audio synth` — all 9 tests pass, including
`open_update_drop_is_clean` on a box with all three players installed. Also
live-verified the three argv sets parse on this machine (`pw-play --raw --format=f32
--rate=44100 --channels=2 --latency=1024 -`, `paplay --raw --format=float32le
--latency-msec=50 --process-time-msec=25`, `aplay -t raw -f FLOAT_LE --period-size=512
--buffer-size=2048 -` all exit 0 — no unknown-option rejects), and confirmed
`--latency-msec`/`--process-time-msec` exist in the pacat(1) man page.

## Verified-clean areas (checked per focus list)

- **CueTracker multiset matching is correct for every reachable sequence.** New cues
  are always observed at `ttl = SFX_TTL_FRAMES − 1 = 89`: `sfx_edges` pushes at 90 then
  decrements all cues in the same pass (flight.rs:1230-1233, 1273-1278), and every
  `flight_notice` call site runs before `end_frame` in `run_frame` (handle_ui_outbox
  at lib.rs:740, input handlers, assemble) so notices are likewise decremented to 89
  before `update` reads the queue at lib.rs:902-910. Since a stored `prev` entry can
  never exceed 89, a fresh push (`c.ttl = 89`) can never satisfy the ancestor rule
  `p.1 > c.ttl` — no false suppression — and every survivor's ancestor exists at
  exactly `ttl+1` — no false fire. `used[]` multiset pairing handles same-text
  duplicates: two `CONTACT`s one frame apart fire twice (pinned by
  `cue_tracker_fires_only_new_pushes`). Walked the interleavings by hand: correct.
- **Decay can't stall under the tracker's feet.** `sfx_edges` runs unconditionally at
  the top of `flight_present` (flight.rs:1113) and `flight_present` is in the
  presentation schedule run every `end_frame` regardless of view mode — cues age even
  in VAB/Map, so `prev` never desynchronizes while `update` keeps pumping.
- **Inert-on-dead-child is panic-free.** `update`'s `try_wait` flips `alive` on
  `Ok(Some) | Err` (audio.rs:479-486), gates all subsequent work, never resets (no
  resurrection to re-baseline `prev` — correct). Writer exits on EPIPE; `Drop` sets
  stop, kills, joins, reaps — bounded teardown even mid-write.
- **Mirrored PLASMA constants currently agree**: audio.rs:96-98 = flight.rs:46-48
  (5.0e3 / 8.0e4). Drift remains possible (private const, no shared source) but the
  comment is honest about it — watch on future edits.
- **Bank/mixer invariants**: `OneShot::ALL`/`COUNT`/`slot()` consistent (9 slots);
  `render` hard-limits `[-1,1]`; `Noise` seeds are fixed (`|1` guards the xorshift
  zero-sink); determinism tests real. `MAX_ACTIVE_SHOTS` bounds breakup bursts; lock
  never held across the blocking write; gain gates skip voice generation at ~0 cost.
- **wasm gating is airtight**: `mod audio`/`mod synth` both `cfg(not(wasm32))`
  (lib.rs:30-38), `ui_confirm` has a no-op wasm twin (927-928).

## Findings

### F1 — MEDIUM: `MAX_PIPE_LEAD_S` cannot recover once the pipe saturates — cue latency pins at ~185 ms permanently

- **File:** `crates/csp-app/src/audio.rs:296-321` (`writer_loop` lead accounting).
- **Problem:** The pacer bounds `lead = written_frames/SR − wall_elapsed` — a *wall-clock
  proxy* for pipe occupancy, not occupancy itself. The writer keeps writing whenever
  `lead ≤ 0.05`, and a blocking `write_all` tops the pipe off whenever the player is
  behind. Once the 64 KiB OS pipe fills (~185 ms of stereo-f32 at 44.1 kHz), the
  writer is paced by the player's drain — and a sound player can never drain faster
  than realtime, so `lead` becomes non-increasing and can never climb back above
  `MAX_PIPE_LEAD_S` for the sleep to drain the excess. Saturation is entered two ways:
  - **Any writer stall ≥ ~135 ms** (scheduling hiccup, swap, window drag, suspend,
    debugger). The player starves, `O → 0`; the catch-up burst writes `stall` worth of
    audio and hits the pipe cap before `lead` reaches 0.05 (cap − lead ≈ 135 ms).
  - **Any sustained drain-slower-than-wall clock mismatch** — audio-device crystals
    commonly run ±10-100 ppm off the host clock. With drain < realtime the queued
    slack grows at the mismatch rate: `~135 ms / δ` — ~22 min at 100 ppm, ~2 h at
    20 ppm — then saturation, permanently.
- **Evidence:** Trace: post-stall `lead ≈ −(stall − 0.05) < 0`; writes proceed until
  `write_all` blocks at cap; each completed write advances `W` and `t` in lockstep →
  `lead` frozen; if `lead` does momentarily exceed 0.05 the sleep only drops `O` by
  the sleep duration before the next write refills it — `O ≈ cap − ε` in both regimes.
- **Impact:** After one qualifying stall (or a slow audio clock over a long session),
  cue-to-ear latency silently regresses to ~185 ms + server buffer (~210-235 ms) —
  the pre-fix lower bound — for the remainder of the session. No log, no recovery.
- **Suggestion:** Detect the underrun (`lead < −MAX_PIPE_LEAD_S` is unrecoverable
  debt — the player already underran) and re-baseline the clock so the cap reasserts:
  e.g. `started = now − (written/SR − (MAX_PIPE_LEAD_S + buf_time))` sets `lead`
  slightly above the cap → the sleep path paces until the pipe drains back to ~50 ms.
  Alternatively write with `set_nonblocking` + drop-oldest to bound occupancy
  directly instead of inferring it.

### F2 — MEDIUM (cross-filed in P11-F2): `cue_shot` covers ~1/3 of the pushable vocabulary — warn-once fires on intended cues

- **File:** `crates/csp-app/src/audio.rs:105-130, 498-503`.
- **Problem:** The table maps the 10 `sfx_edges` strings + 5 refusal notices, but ≥15
  other `flight_notice` strings land unmapped: staging refusals (`NOT STAGED —`,
  `ALL STAGES FIRED`, `NOTHING TO STAGE`, `STAGING NOT READY`), recover refusals
  (`NO VESSEL TO RECOVER`, `VESSEL NOT RECOVERABLE`), all read-only refusals except
  attitude/throttle (`WARP DEMOTION`, `LAUNCH`, `MANEUVER`, `STAGING`,
  `RECOVER-CONTROL`), and two `format!` notices (`STAGED — {n} PART(S) JETTISONED`
  csp-view/lib.rs:1857, `SETTINGS — {n} KEY BINDING(S) MIGRATED` lib.rs:356) that can
  *never* literal-match. Every successful staging — a happy path — logs
  "cue vocabulary drifted"; each distinct `{n}` is a new warn.
- **Impact:** "Dead key ack" coverage is inconsistent with `cue_shot`'s own stated
  intent, and the drift detector desensitizes on non-drift.
- **Suggestion:** Per P11-F2 — prefix-map the refusal family to `UiConfirm`
  (`starts_with("SESSION IS READ-ONLY")` etc.), map `STAGED —`/`SETTINGS —` by prefix
  or split text/detail; extend `every_flight_cue_maps` to pin the notice strings.

### F3 — LOW (cross-filed in P11-F1): continuous voices track `vessels.last()`, not the pin-aware focus

- **File:** `crates/csp-app/src/audio.rs:509` vs `flight.rs:1128-1131`
  (`focus_override.target … or_else(focus_vessel)`).
- **Problem:** `recover_control` pins `FlightFocusOverride` (csp-view/lib.rs:1543) and
  the pin persists while the vessel is staged — but audio's engine/plasma targets read
  raw `last()`. When the pin target isn't the newest staged vessel (a newer debris
  fragment or spawn exists), rumble/whoosh follow the wrong ship — including silence
  while the *controlled* vessel burns.
- **Suggestion:** Per P11-F1 — use the resolved `FlightUiData.focus` id (already
  pin-aware) to select the vessel in the snapshot.

### F4 — LOW: a single NaN frame kills a continuous voice for the session

- **File:** `crates/csp-app/src/audio.rs:510-516`.
- **Problem:** `engine_level += (target − engine_level) * LEVEL_SMOOTH` — a NaN
  `throttle`/`plasma_w_m2` (reachable via a NaN `flow_sample` or corrupted staged
  body) poisons the smoother: `level` stays NaN forever, `NaN > LEVEL_EPS` is false →
  gain pinned at 0 → voice permanently dead, silently. `f64::clamp` passes NaN
  through so the clamp doesn't save it.
- **Suggestion:** Guard `target.is_finite()` before the smoother (fold to 0.0), or
  reset `level` when it goes non-finite.

### F5 — LOW: settle check commits to players that die after 60 ms; no mid-session fallback

- **File:** `crates/csp-app/src/audio.rs:385-394, 479-486`.
- **Problem:** A player that fails slower than `PLAYER_SETTLE_MS` (paplay against a
  hung/absent pulse socket can take longer to give up; a device-busy error surfacing
  on first I/O) is committed — `update` then flips inert and the remaining `PLAYERS`
  candidates are never tried, though one might have worked. Also note the settle
  sleep runs unconditionally on the main thread: +60 ms when audio works, up to
  ~180 ms probing dead candidates — acceptable one-time cost, but it's the startup
  path.
- **Suggestion:** Cheap improvement: on child exit within the first few seconds of
  `open`, fall through to the next `PLAYERS` candidate (re-spawn) instead of going
  permanently inert.

### F6 — LOW: pad launch stacks two identical `EngineIgnition` shots (+6 dB)

- **File:** `crates/csp-app/src/audio.rs:113-114` + `flight.rs:1236-1253`.
- **Problem:** `SetThrottle` on a latched craft unlatches *and* raises throttle the
  same frame → `sfx_edges` pushes `LIFTOFF` (latch edge) and `ENGINE IGNITION`
  (throttle edge) together; both map to `OneShot::EngineIgnition` → two coherent
  copies of the same 0.6 s buffer sum to +6 dB on every pad launch. Likely reads as
  emphasis, but it's a coincidence, not a design.
- **Suggestion:** Map `LIFTOFF` to a distinct shot (or to `None` — the rumble swell
  already covers it), or dedupe same-shot pushes within a frame.

## Nits (not findings)

- `MixState::mix` doc says "soft-clamped" — `s.clamp(-1.0, 1.0)` is a hard clip
  (audio.rs:262, 281). A 16-shot worst case distorts; wording only.
- `LEVEL_SMOOTH` is per-frame → voice attack/decay rate is fps-dependent
  (audio.rs:515-516). Presentation-only, masked by the short constant.
- `play` queues shots at `gain = self.master` — muted pushes still occupy
  `MAX_ACTIVE_SHOTS` slots for up to ~0.9 s (edge case).
- `CueTracker` correctness rests on the `update`-after-`end_frame` pairing rather
  than on the data: two `update` calls against an unchanged queue re-fire every cue
  (visible in `open_update_drop_is_clean`, which pumps 3× and would re-trigger
  "BREAKUP" thrice). In production the pairing is adjacent and unconditional, so
  unreachable — but a comment noting the strict-decay invariant would harden it.
