# Verified Report — P10, P11, P12 (session-play1)

Verification method: every finding re-read against the cited code. Verdicts:
**CONFIRMED** = the code does what the report claims and the impact reasoning holds;
**CONFIRMED-TRIVIAL** = code matches but the impact is cosmetic/unreachable (nit-grade);
**DISMISSED** = the code contradicts the claim.

---

## P10 — VAB usability

### F1 — LOW — Click during an in-flight pick is silently dropped — **CONFIRMED**
`crates/csp-app/src/lib.rs:1264-1267`: `self.pending_pick.take()` runs before the
`pick_in_flight` check, and the `if` arm drops the coordinate with no encode and no
`resolve_background` → no `Pick` event of any kind. A second click inside the ~1–2
frame readback window is a dead drop, including a "click empty space to cancel"
gesture. The in-code comment ("the next click retriggers") acknowledges the
trade-off; the report's severity (LOW) is fair.

### F2 — LOW — Out-of-bounds pick produces no event — **CONFIRMED**
`crates/csp-app/src/lib.rs:1268`: `else if x < gpu.config.width && y < gpu.config.height`
— an out-of-bounds release skips the entire arm, so `resolve_background` is never
set and no `Pick(None)` is emitted. Inconsistent with the contract honored at
L1302-1307 for in-bounds unencodable picks.

**Focus-claim caveat — "empty viewport (encode_pick false → no Pick(None))": DISMISSED
as phrased.** `resolve_background = !encode_pick` (L1290) means an *in-bounds* click
that can't rasterize — empty packet, missing renderer — DOES emit `Pick(None)`
(L1302-1307). The dead-drop window is only (a) `pick_in_flight` (F1) and
(b) out-of-bounds release (F2).

### F3 — LOW — Stale pick id collapses into "background click" — **CONFIRMED**
`crates/csp-view/src/vab.rs:157-161` → `crates/csp-ui/src/vab.rs:642-654`:
`sel.filter(|id| self.session.contains(*id))` maps a dead id to `None`, then
`self.selected = None` + `pending_part.take()` → "placement cancelled" — identical
to a real background click. Case 2 also holds: `CraftSession::from_bytes` restores
serialized node ids (`crates/csp-phys/src/session.rs:1785-1794`), and `next_id` is
monotonic/never-recycled (test `remove_part_takes_subtree_and_ids_never_recycle`,
session.rs:2170), so a `New`/`LoadBytes` between encode and readback can decode a
stale id onto a *different* live part → wrong-parent attach. No `craft_epoch` is
stamped on the pick request/readback (`crates/csp-app/src/lib.rs:746-751` decodes
and pushes `Pick(node)` unconditionally); `craft_epoch` is only used for camera
refit (`crates/csp-view/src/vab.rs:233-237`). Report's suggestion is apt.

### F4 — LOW — `lmb_press` seeds from `last_cursor` — **CONFIRMED**
`crates/csp-app/src/lib.rs:1966`: press stores `self.last_cursor` (None until first
`CursorMoved`); release at L1973-1981 does nothing when `lmb_press.take()` is None →
a press+release with no intervening move produces no pick at all.

### F5 — LOW — `to_bytes` failure silently drops save — **CONFIRMED-TRIVIAL**
`crates/csp-view/src/lib.rs:452-456`: `.ok()` maps the error to `None` → empty
outbox, no notice. Only failure source is `bincode::serde::encode_to_vec`
(`crates/csp-phys/src/session.rs:1749-1750`), which a `Vec` writer cannot fail on
for this schema — unreachable in practice; honesty gap, not a live bug.

### F6 — LOW — No upward-attachment path — **CONFIRMED**
`crates/csp-ui/src/vab.rs:781-793`: `facing = -parent_t.apply_dir(pn.direction).z`
sorted strictly descending — the parent's lowest free node always wins while any
downward node exists; no vocabulary for "attach above". Design-level gap as stated.

### F7 — NIT — Save with empty `save_name` is a silent no-op — **CONFIRMED**
`crates/csp-ui/src/vab.rs:1326-1328`: button enabled, click consumed, no request, no cue.

### F8 — NIT — Whitespace-only names pass `validate_craft_name` — **CONFIRMED**
`crates/csp-app/src/lib.rs:2170-2184`: `" "` passes all checks (non-empty, no
separator/control); writes ` .cspcraft`. No `trim()` check exists.

### F9 — NIT — Armed banner text stale when craft empties — **CONFIRMED**
`VabAction::Remove` → `remove_node` does not clear `pending_part`
(`crates/csp-ui/src/vab.rs:501`); in `palette_banner` (L1120-1134) the
`pending_name` branch precedes the `empty` branch, so an empty craft still shows
"click the craft part to attach it under" while the next click will `place_root`.

### F10 — NIT — Armed row's hover verb doesn't admit re-click cancels — **CONFIRMED**
`crates/csp-ui/src/vab.rs:1054-1063`: `verb` ignores `armed`; the armed row still
reads "click to start placing…" though clicking it cancels (catalog_pick toggle at
L583-585).

### F11 — NIT — `NoCompatibleAttach` prints `PartNodeId` Debug — **CONFIRMED**
`crates/csp-ui/src/vab.rs:181-184`: `{parent:?}` leaks `PartNodeId(3)`; the
`node_label` helper exists at L319.

### F12 (unnumbered in P10; raised as focus item) — VAB-row notices not migrated to `flight_notice` outside VAB — **CONFIRMED**
`launch_refused` (`crates/csp-view/src/lib.rs:1881-1884`) pushes
`VabEvent::Notice` → `set_notice` → drawn only by `vab_editor` (Craft window notice
row `crates/csp-ui/src/vab.rs:1347` and the VAB hint bar L1737), which only runs in
VAB mode (`crates/csp-view/src/lib.rs:373`). But the LAUNCH button lives on
`top_bar` (`crates/csp-ui/src/vab.rs:892`), drawn in **every** mode
(`crates/csp-view/src/lib.rs:338-346`). So a resolve-failure refusal clicked from
Flight/Map is invisible — a dead click. The app's own comment acknowledges the row
"is invisible outside the editor" and migrates only the read-only refusal to
`flight_notice` (`crates/csp-app/src/lib.rs:1037-1041`); the `launch_craft`
resolve-failure refusal and `VabEvent::Notice` generally were not migrated.
(Craft save/load notices are fine — `vab_editor` is VAB-gated so they can only
originate inside the editor.)

---

## P11 — Flight HUD, navball & SFX cues

### F1 — MEDIUM — SFX edges + audio voices track `vessels.last()`, not the pin — **CONFIRMED (DUPLICATE of P04-F4 and P07-#1)**
`crates/csp-view/src/flight.rs:1218`: `sfx_edges` calls `focus_vessel(vessels)` =
`.last()` (L1290-1291), and is invoked at L1113 *before* the pin-aware focus
resolution at L1128-1131 — so the cue baseline never sees `FlightFocusOverride`.
`crates/csp-app/src/audio.rs:509`: continuous engine/plasma voices also read
`snap.vessels.last()` (the comment even cites the outdated "newest spawn" rule).
Wrong-vessel cues and wrong-vessel engine audio during `recover_control` are real.
**Dedup:** identical code, identical fix already filed as **P04-F4**
(review-report-04.md L139-152) and **P07 finding 1** (review-report-07.md L64-98).
Count once; the fix should be the shared pin-aware resolver (resolve pin at
L1128 first, pass resolved focus into `sfx_edges`, carry focus id for audio).

### F2 — MEDIUM — Cue vocabulary drift: ~15 `flight_notice` strings unmapped — **CONFIRMED**
`cue_shot` (`crates/csp-app/src/audio.rs:105-130`) maps the 10 `sfx_edges` strings +
exactly 5 refusal notices. Verified-unmapped push sites (all `flight_notice` →
`FlightSfx.cues` → `CueTracker::drain_new` → `cue_shot` → `None` → no one-shot +
warn-once at audio.rs:498-501):
- `crates/csp-view/src/lib.rs`: "VESSEL NOT RECOVERABLE — NOT ON RAILS HERE" (1511),
  "NO VESSEL TO RECOVER" (1523), "ALL STAGES FIRED — NOTHING LEFT" (1798),
  "SINGLE-STAGE CRAFT — NOTHING TO STAGE" (1800), "STAGING NOT READY — LAUNCHED
  CRAFT NOT SPAWNED" (1814), "NOT STAGED — PARKED ON THE SURFACE (THROTTLE TO
  LIFTOFF)" (1821), "NOT STAGED — VESSEL IS ON RAILS" (1826), "NOTHING TO STAGE —
  LAUNCHED CRAFT GONE" (1838), `format!("STAGED — {fired} PART(S) JETTISONED")` (1857).
- `crates/csp-app/src/lib.rs`: `format!("SETTINGS — {migrated} KEY BINDING(S)
  MIGRATED TO POSITIONAL KEYS")` (356), "SESSION IS READ-ONLY — WARP DEMOTION NOT
  APPLIED" (450), "…— LAUNCH REFUSED" (1041), "…— MANEUVER NOT APPLIED" (1055),
  "…— STAGING NOT APPLIED" (1619), "…— RECOVER-CONTROL NOT APPLIED" (1819).
15 distinct texts (2 dynamic/unmatchable by literal). Silent + drift-warn on
intended cues confirmed.

### F3 — LOW — "STAGE — PART JETTISONED" fires on any node-count decrease — **CONFIRMED**
`crates/csp-view/src/flight.rs:1241-1243` diffs `nodes` =
`craft.graph.nodes.len()` (L691); any shrink — impact destruction, ablation —
fires the staging text and the `StageJettison` one-shot. Confirmed mislabel.

### F4 — LOW — T-Pe suppressed on open conics — **CONFIRMED**
`crates/csp-view/src/flight.rs:1193-1205`: `t_apo`/`t_pe` computed only inside the
closed-conic gate; `pe_alt_m` publishes outside it (L1190-1192).
`csp_sim::scripts::resolve_apsis_ticks` (scripts.rs:657-666) already returns
`Ok(None)` for open apoapsis and `Some` for inbound-open periapsis — the HUD gate,
not the resolver, suppresses the most useful apsis time on a flyby.

### F5 — LOW — "ON PAD" off-pad + `hint()` vs `hint_labeled()` glyph divergence — **CONFIRMED**
(a) `crates/csp-ui/src/lib.rs:551-560`: `"ON PAD — [{}] to launch"` keys on
`input.latched`, which is `focus.latched` = any `SurfaceLatched` vessel — a field
landing reads "ON PAD". (b) `keys.hint(action)` =
`hint_labeled(action, &BTreeMap::new())` (`crates/csp-ui/src/settings.rs:646-648`)
→ QWERTY `physical_fallback` (settings.rs:468) for un-observed physical tokens,
while the hint bar uses `hint_labeled` with `key_labels` (e.g. vab.rs:1728). Both
halves confirmed.

### F6 — LOW — "DEBRIS IMPACT" on any debris-count decrease + "VESSEL REMOVED" suppression — **CONFIRMED**
`crates/csp-view/src/flight.rs:1258-1265`: `c.debris < p.debris → "DEBRIS IMPACT"`;
`VesselState::OnRails(_) => {}` (L710) means a debris vessel going to rails exits
the staged set → count drops → false "DEBRIS IMPACT". The
`c.debris >= p.debris` conjunct on the `VESSEL REMOVED` edge (L1263) does eat a
same-frame clean-vessel removal. Both halves confirmed.

---

## P12 — Tutorial

### F1 — MEDIUM — Tutorial ignores `FlightFocusOverride` — **CONFIRMED**
`crates/csp-view/src/tutorial.rs:70`: `snap.vessels.iter().rev().find(|v| !v.debris)`
— newest non-debris; the system's params (L58-63) don't even take
`Res<FlightFocusOverride>`. The HUD/throttle/camera focus rule at
`crates/csp-view/src/flight.rs:1128-1131` prefers `focus_override.target`. A
`recover_control` pin on an older vessel leaves the tutorial watching a different
ship → steps 3-6 (`throttle`, `alt_m`, `periapsis_m` predicates,
`crates/csp-ui/src/tutorial.rs:141-152`) stall despite correct play. Same root
cause as P11-F1/P04-F4 but a distinct consumer — should share the pin-aware
resolver fix.

### F2 — MEDIUM — `has_vessel` conflates OnRails with destroyed — **CONFIRMED**
`has_vessel = focus.is_some()` (tutorial.rs:87) over the staged set, which skips
`VesselState::OnRails` (`crates/csp-view/src/flight.rs:710`). A live vessel going
to rails — burn completion via `apply_to_rails_at`
(`crates/csp-sim/src/lib.rs:4182`) or scripted `ToRails` — vanishes identically to
a crash → `observe` rewinds to `STEP_LAUNCH` and shows "vessel lost"
(`crates/csp-ui/src/tutorial.rs:230-233`, banner L335-340). Report's own
mitigation verified: `SetManeuver` requires the vessel already OnRails
(csp-sim/lib.rs:5587-5589), so a stock always-in-bubble tutorial craft cannot
reach this — needs multi-vessel/scenario/scripted flow. Correctly scoped MEDIUM.

### F3 — LOW — Osculating apsides computed for latched vessels — **CONFIRMED**
`crates/csp-view/src/tutorial.rs:73-78`: no `v.latched` check. `stage_latched`
stamps `vel` = CoM co-rotation (`crates/csp-view/src/flight.rs:758-761`), so a
parked craft on a spinning body yields junk `Some(…)` apsides folded into
`best_periapsis_m`. `flight_present` explicitly declines (`!focus.latched`,
flight.rs:1181). Cannot falsely credit orbit (periapsis ≤ radius < space_alt) —
bogus readout only, as reported.

### F4 — LOW — `osculating_apsides` NaN guard incomplete — **CONFIRMED**
`crates/csp-view/src/tutorial.rs:36`: `r <= 0.0` doesn't reject NaN; NaN flows
through `p`/`energy` (`NaN >= 0.0` is false → passes the escape gate), then
`(1.0 − p/a).max(0.0)` maps NaN→0 → `e = 0` → `Some((NaN, NaN))` instead of the
documented `None`. Harmless today (all consumers are `>`/`f64::max`, both
NaN-safe). Correct.

### F5 — LOW — One-frame-stale VAB analysis read — **CONFIRMED**
`crates/csp-view/src/tutorial.rs:79-86` reads `vab.0.analysis()`; the dirty-gated
`vab.0.recompute()` runs in `vab_present` (`crates/csp-view/src/vab.rs:229`) in the
*same unordered* presentation tuple (`crates/csp-view/src/lib.rs:1041-1049`) — so
the "independent of sibling ordering" comment (tutorial.rs:54-57) overclaims for
the VAB cache. Benign direction only (step-1 tick one frame late). Also worth
noting: `vab_present` early-returns without `recompute` when `!input.open`
(vab.rs:224-227), so outside VAB the cache can be arbitrarily stale — still only
affects the already-completed step-1 predicate.

### Observation (starter seed auto-completes steps 0-2) — **CONFIRMED**
`crates/csp-app/src/lib.rs:322-324`: `App::new_inner` calls
`view.seed_starter_craft()`, so first `observe` sees `has_vessel` and the catch-up
loop completes steps 0-2 — checklist opens on "Open the throttle", and Restart can
never re-show step 0 while a non-debris vessel is staged. Design tension as noted.

---

## Summary

| Report | Finding | Verdict |
|--------|---------|---------|
| P10 | F1 in-flight pick drop | CONFIRMED |
| P10 | F2 out-of-bounds pick drop | CONFIRMED |
| P10 | (focus) "empty viewport → no Pick(None)" | DISMISSED (resolve_background covers it; real drops are F1/F2) |
| P10 | F3 stale pick id → background/wrong attach | CONFIRMED |
| P10 | F4 lmb_press needs prior CursorMoved | CONFIRMED |
| P10 | F5 to_bytes .ok() drop | CONFIRMED-TRIVIAL (unreachable) |
| P10 | F6 no upward attach | CONFIRMED (design) |
| P10 | F7–F11 nits | CONFIRMED |
| P10 | F12 VAB notices invisible outside VAB (launch_refused) | CONFIRMED |
| P11 | F1 sfx/audio focus ignores pin | CONFIRMED — DUPLICATE of P04-F4 / P07-#1 |
| P11 | F2 ~15 unmapped flight_notice strings | CONFIRMED |
| P11 | F3–F6 | CONFIRMED |
| P12 | F1 tutorial ignores pin | CONFIRMED (3rd consumer of same root cause) |
| P12 | F2 has_vessel conflates OnRails/destroyed | CONFIRMED |
| P12 | F3–F5, observation | CONFIRMED |

No false positives among the numbered findings; the only dismissal is the
loosely-phrased focus claim about `encode_pick`. MEDIUM items all stand:
P10-F12, P11-F1 (dup), P11-F2, P12-F1, P12-F2.
