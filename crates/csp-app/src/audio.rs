//! M10's audio backend — the real consumer of the M4c cue channel
//! (PLAN §3.7's presentation floor + the M10 "audio" exit row, the
//! sound output the M4c as-shipped note deferred).
//!
//! **The seam.** `csp_view::flight::FlightSfx` edge-detects the named
//! transitions — engine ignition/cut, stage jettison, breakup,
//! contact/impact — on the staged snapshot and pushes HUD cue lines.
//! This module reads that resource *read-only* after `end_frame`:
//! `CueTracker` diffs the live queue frame-over-frame, each newly
//! pushed cue maps to a synthesized one-shot (`synth`), and two
//! continuous voices track throttle (engine rumble) and convective
//! flux (plasma whoosh). The sim is never touched — audio is
//! presentation, like the VFX it accompanies.
//!
//! **The backend.** A real-time PCM mixer streams interleaved-stereo
//! f32 into a native player *subprocess* (`pw-play`, else `paplay`,
//! else `aplay`) over a piped stdin — no FFI, no unsafe, no link-time
//! audio libraries, so the build needs nothing beyond a C toolchain
//! and degrades to silence wherever no player (or server) exists.
//! That is also the deliberate "simpler approach" the audio row
//! allows: cpal/rodio/miniaudio all need platform dev packages or
//! libclang at *build* time; a subprocess needs none and talks to the
//! same sound servers (pipewire / pipewire-pulse / pipewire-alsa)
//! those crates would target.
//!
//! **Latency.** A piped player buffers at three layers — the writer's
//! chunk, the OS pipe (~185 ms of slack), and the player's own server
//! request (paplay defaults to ~2 s of `tlength`!). Cue-to-ear was
//! 0.2–0.7 s. The fix is three-fold: the players are spawned with
//! their low-latency flags (`pw-play --latency`, `paplay
//! --latency-msec`, `aplay --period-size`/`--buffer-size`), the mix
//! chunk is small (`FRAMES_PER_BUF` ≈ 12 ms), and the writer thread
//! caps how far ahead of the player's real-time drain it may fill
//! (`MAX_PIPE_LEAD_S`) — audio queued in a pipe the player hasn't
//! seen is stale cue latency, so the writer sleeps the excess off
//! instead of letting the queue grow unbounded.
//!
//! **Graceful absence.** No player binary, dead server, killed child —
//! none of it is an error. `AudioBackend::open` returns `None` when
//! nothing can be spawned and the app keeps an `Option`; a child that
//! exits mid-session flips the backend inert, never a panic.
//! Native-only: the whole module is `cfg(not(target_arch = "wasm32"))`,
//! so the wasm slice is untouched.

use std::collections::VecDeque;
use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use csp_view::flight::{FlightSfx, FlightSnapshot, SfxCue};
use csp_view::settings::AudioSettings;

use crate::synth::{self, OneShot, SAMPLE_RATE};

/// Output channel count — the stream is interleaved stereo; mono
/// sources are duplicated into both ears by the mixer.
const CHANNELS: usize = 2;
/// Frames mixed per pipe write — ~11.6 ms at 44.1 kHz. Small on
/// purpose: a one-shot pushed mid-buffer waits at most this long
/// before the mix hands it to the player.
const FRAMES_PER_BUF: usize = 512;
/// Stream-seconds one mix buffer covers — the writer's drain quantum.
const BUF_TIME_S: f64 = FRAMES_PER_BUF as f64 / SAMPLE_RATE as f64;
/// How far ahead of real time the writer may fill the player's stdin,
/// in seconds of queued audio — the bound on the pipe's contribution
/// to cue latency. The OS pipe alone holds ~185 ms of stream; without
/// this cap the writer's blocking write is a slack-filling pacer, not
/// a latency bound.
const MAX_PIPE_LEAD_S: f64 = 0.05;
/// The OS pipe's capacity in stream-seconds (64 KiB of interleaved
/// stereo f32 at 44.1 kHz ≈ 185 ms) — the occupancy the writer assumes
/// when a blocking write proves the pipe was full. It's an *anchor*,
/// not a measurement: on a platform with a smaller default pipe the
/// estimate over-states the backlog once per saturation event (the
/// recovery sleep runs a touch long — a bounded extra gap), never
/// under-states it into a permanent backlog.
const PIPE_CAP_S: f64 = 65_536.0 / (SAMPLE_RATE as f64 * CHANNELS as f64 * 4.0);
/// Grace period after spawning a player before committing to it — a
/// binary that exists but cannot connect (`pw-play` with no pipewire
/// daemon, `paplay` with no pulse socket) exits almost immediately;
/// detecting that here lets the fallback list reach the next player
/// instead of owning a dead pipe.
const PLAYER_SETTLE_MS: u64 = 60;
/// Concurrent one-shot cap — a transition burst (breakup chains) sheds
/// new shots past this rather than stacking unbounded voices.
const MAX_ACTIVE_SHOTS: usize = 16;

/// Per-frame approach rate for the continuous voices' gain — smooths
/// throttle/plasma steps into swells instead of clicks.
const LEVEL_SMOOTH: f32 = 0.25;
/// Below this level a continuous voice contributes nothing — the
/// mixer skips generating it, so silence costs ~no CPU.
const LEVEL_EPS: f32 = 0.01;
/// The engine voice's full-throttle gain (pre-master).
const ENGINE_GAIN: f32 = 0.9;
/// The plasma voice's full-saturation gain (pre-master).
const WHOOSH_GAIN: f32 = 0.6;

// The whoosh's intensity is `csp_view::flight::plasma_intensity`
// itself — the same flux-window × speed-gate curve the §3.7 sheath
// draws. (These used to be mirrored constants; C19-F1's speed gate
// needs the vessel's `v_rel` too, so sharing the function is the
// only honest way to keep the audible plasma on the visible one.)

/// The cue text → one-shot map. The `sfx_edges` transition vocabulary
/// maps literally — those strings are the wire format. Everything else
/// on the `FlightSfx` channel is an app/view `flight_notice`: a
/// player-facing ack or refusal line, several of them `format!`-built
/// (`STAGED — N PART(S) JETTISONED`, `SETTINGS — …`) so they can never
/// literal-match a table (P13-F2). Those map by prefix family, and any
/// notice no rule names still gets the ack blip — a cue the producers
/// pushed that resolves to silence was the "dead keypress" bug class,
/// and warn-once-per-text spammed on every intended notice. There is
/// deliberately no unmapped path: the map is best-effort enrichment,
/// not a contract the cue texts must satisfy.
fn cue_shot(text: &str) -> OneShot {
    match text {
        "ENGINE IGNITION" => return OneShot::EngineIgnition,
        "ENGINE CUT" => return OneShot::EngineCut,
        "STAGE — PART JETTISONED" => return OneShot::StageJettison,
        "CONTACT" => return OneShot::Contact,
        // §2.6.4 latch/unlatch edges — touchdown thump and the
        // launch rumble.
        "LANDED — SURFACE RAILS" => return OneShot::Contact,
        "LIFTOFF" => return OneShot::EngineIgnition,
        "BREAKUP" => return OneShot::Breakup,
        "DEBRIS IMPACT" => return OneShot::DebrisImpact,
        "VESSEL REMOVED" => return OneShot::VesselRemoved,
        "VESSEL LEFT BUBBLE" => return OneShot::VesselLeftBubble,
        _ => {}
    }
    // The success confirm "STAGED — N PART(S) JETTISONED" carries a
    // variable count, so it maps by prefix — same stage clunk the
    // `STAGE —` edge cue plays.
    if text.starts_with("STAGED —") {
        return OneShot::StageJettison;
    }
    // Every refusal/notice — "SESSION IS READ-ONLY — …", the
    // `*_LOCKED` verb refusals, "NO VESSEL …", staging/recover
    // refusals, `COMMAND REFUSED — …`, settings repairs — and any
    // future cue text gets the acknowledge blip.
    OneShot::UiConfirm
}

/// Edge-detector over `FlightSfx.cues`. The queue is a multiset of
/// `(text, ttl)` where every surviving cue decays one frame per
/// present pass: a *newly pushed* cue is any element that does not
/// descend from a last-frame cue of the same text. Multiset matching
/// keeps same-text duplicates honest — two identical pushes one frame
/// apart still fire twice.
#[derive(Default)]
struct CueTracker {
    /// Last frame's `(text, ttl)` list.
    prev: Vec<(String, u32)>,
}

impl CueTracker {
    /// The cue texts pushed since the previous call — in queue order.
    /// Post-condition: `prev` holds this frame's queue.
    fn drain_new(&mut self, cues: &VecDeque<SfxCue>) -> Vec<String> {
        let mut used = vec![false; self.prev.len()];
        let mut new = Vec::new();
        for c in cues {
            let ancestor = self
                .prev
                .iter()
                .enumerate()
                .find(|(i, p)| !used[*i] && p.0 == c.text && p.1 > c.ttl)
                .map(|(i, _)| i);
            match ancestor {
                Some(i) => used[i] = true,
                None => new.push(c.text.clone()),
            }
        }
        self.prev = cues.iter().map(|c| (c.text.clone(), c.ttl)).collect();
        new
    }
}

/// The player binaries the backend tries, in order — each is spawned
/// to read interleaved-stereo f32 LE at `SAMPLE_RATE` from stdin.
/// `pw-play` first: on a pipewire desktop it is the most direct path —
/// no pulse-emulation layer, and `--latency` sets the node latency in
/// frames. `paplay` (pipewire-pulse/pulse) next, then `aplay` (ALSA,
/// usually via the pipewire-alsa plugin). A binary that spawns but
/// dies at connect is caught by `open`'s settle check, so ordering by
/// latency preference is safe.
const PLAYERS: &[&str] = &["pw-play", "paplay", "aplay"];

/// The argv for `player` — raw-stream flags naming our wire format
/// plus each player's low-latency request (without it the server-side
/// buffer is the dominant cue-to-ear term: paplay's default `tlength`
/// is ~2 s; aplay's ALSA ring is sized by whatever the plugin picks).
fn player_args(player: &str) -> Vec<String> {
    match player {
        // pw-play: `-` = stdin, `--raw` = raw stream, `--latency` in
        // frames (~23 ms at 44.1 kHz — pipewire's usual quantum).
        "pw-play" => vec![
            "--raw".into(),
            "--format=f32".into(),
            format!("--rate={SAMPLE_RATE}"),
            format!("--channels={CHANNELS}"),
            "--latency=1024".into(),
            "-".into(),
        ],
        "paplay" => vec![
            "--raw".into(),
            "--format=float32le".into(),
            format!("--rate={SAMPLE_RATE}"),
            format!("--channels={CHANNELS}"),
            // Bound the server request: 50 ms total stream latency,
            // fed in 25 ms requests.
            "--latency-msec=50".into(),
            "--process-time-msec=25".into(),
        ],
        // aplay: `-` = stdin, `-q` = quiet, FLOAT_LE = our f32 stream.
        // The ALSA ring is 4 × the mix chunk: ~12 ms interrupts behind
        // a ~46 ms buffer — small enough for cues, fat enough to ride
        // out a scheduling hiccup without an xrun.
        _ => vec![
            "-q".into(),
            "-t".into(),
            "raw".into(),
            "-f".into(),
            "FLOAT_LE".into(),
            "-r".into(),
            SAMPLE_RATE.to_string(),
            "-c".into(),
            CHANNELS.to_string(),
            format!("--period-size={FRAMES_PER_BUF}"),
            format!("--buffer-size={}", 4 * FRAMES_PER_BUF),
            "-".into(),
        ],
    }
}

/// A queued one-shot mid-playback — a cursor into a shared bank buffer.
struct ActiveShot {
    /// The bank's sample buffer (Arc — triggering never copies PCM).
    buf: Arc<Vec<f32>>,
    /// Next frame index to emit.
    pos: usize,
    /// Trigger-time gain (master volume baked in).
    gain: f32,
}

/// The shared half of the mixer — the app thread writes voice gains
/// and pushes shots; the writer thread pulls frames. The lock is held
/// only for the mix or the push — never across a blocking write.
struct MixState {
    /// Continuous voices + their fully-resolved gains (master folded).
    rumble: synth::Rumble,
    /// See `rumble`.
    whoosh: synth::Whoosh,
    /// `master * level * ENGINE_GAIN` — 0.0 skips generation entirely.
    engine_gain: f32,
    /// `master * level * WHOOSH_GAIN` — 0.0 skips generation entirely.
    whoosh_gain: f32,
    /// One-shots in flight, oldest first.
    shots: Vec<ActiveShot>,
}

impl MixState {
    fn new() -> Self {
        Self {
            rumble: synth::Rumble::new(),
            whoosh: synth::Whoosh::new(),
            engine_gain: 0.0,
            whoosh_gain: 0.0,
            shots: Vec::new(),
        }
    }

    /// Fill `out` (interleaved stereo) with the current mix —
    /// soft-clamped so a worst-case stack never clips hard.
    fn mix(&mut self, out: &mut [f32]) {
        for frame in out.chunks_mut(CHANNELS) {
            let mut s = 0.0f32;
            if self.engine_gain > 0.0 {
                s += self.rumble.next().unwrap_or(0.0) * self.engine_gain;
            }
            if self.whoosh_gain > 0.0 {
                s += self.whoosh.next().unwrap_or(0.0) * self.whoosh_gain;
            }
            let sum = &mut s;
            self.shots.retain_mut(|shot| match shot.buf.get(shot.pos) {
                Some(&v) => {
                    shot.pos += 1;
                    *sum += v * shot.gain;
                    true
                }
                None => false,
            });
            let m = s.clamp(-1.0, 1.0);
            for c in frame.iter_mut() {
                *c = m;
            }
        }
    }
}

/// The writer thread's loop: mix one buffer, encode f32 LE, write —
/// repeat until the stop flag or a broken pipe. The blocking write is
/// the slow-path pacer; the lead cap below is the latency bound (see
/// `MAX_PIPE_LEAD_S`), and it runs *outside* the mix lock.
///
/// P13-F1: the old `written/SR − wall_elapsed` lead was a *proxy* for
/// pipe occupancy — it could never see saturation. A ≥135 ms stall
/// (suspend, debugger, window drag) or a player whose device clock
/// runs just off realtime filled the 64 KiB pipe, after which every
/// `write_all` paced at drain rate with `lead` frozen under the cap:
/// cue latency silently pegged at ~185 ms for the rest of the
/// session. The rewrite estimates *occupancy* instead, and occupancy
/// is observable at exactly two points: realtime drain decays the
/// estimate between iterations (a writer stall collapses it to 0 —
/// the player underran and the pipe is empty again), and a *blocked*
/// write means the pipe is at capacity, re-anchoring the estimate at
/// `PIPE_CAP_S` so the sleep branch drains the backlog instead of
/// pacing it. Between those anchors the estimate stays conservative:
/// a player pre-buffering faster than realtime only lowers true
/// occupancy, and a slower-than-realtime drain eventually blocks a
/// write and re-anchors.
fn writer_loop(stdin: &mut ChildStdin, state: &Mutex<MixState>, stop: &AtomicBool) {
    let mut buf = vec![0.0f32; FRAMES_PER_BUF * CHANNELS];
    let mut bytes = vec![0u8; buf.len() * 4];
    // Estimated seconds of queued audio sitting in the pipe — the
    // quantity `MAX_PIPE_LEAD_S` bounds. See the function doc: a stall
    // decays it to zero (the player underran), a blocked write
    // re-anchors it at the pipe cap.
    let mut queued_s = 0.0_f64;
    let mut last = std::time::Instant::now();
    while !stop.load(Ordering::Relaxed) {
        // The player reads the pipe at realtime; decay the estimate
        // by wall elapsed since the last iteration. A stall lands
        // here too: while we slept, the player drained — and then
        // starved — so the pipe is empty and the estimate floors at
        // zero rather than pretending the backlog still exists.
        let now = std::time::Instant::now();
        queued_s = (queued_s - now.duration_since(last).as_secs_f64()).max(0.0);
        last = now;
        if queued_s > MAX_PIPE_LEAD_S {
            std::thread::sleep(std::time::Duration::from_secs_f64(
                queued_s - MAX_PIPE_LEAD_S,
            ));
            continue;
        }
        match state.lock() {
            Ok(mut st) => st.mix(&mut buf),
            Err(_) => {
                log::warn!("audio mixer state poisoned — audio writer stopping");
                return;
            }
        }
        for (b, s) in bytes.chunks_mut(4).zip(buf.iter()) {
            b.copy_from_slice(&s.to_le_bytes());
        }
        let write_start = std::time::Instant::now();
        if stdin.write_all(&bytes).is_err() {
            return; // player exited or pipe closed — silently done
        }
        queued_s += BUF_TIME_S;
        // A write that had to wait means the pipe was full — the one
        // place real occupancy is observable. Re-anchor at the pipe
        // cap so the next iterations sleep while the backlog drains
        // to `MAX_PIPE_LEAD_S` instead of pacing a saturated pipe
        // forever. Half a buffer is the discriminator: a blocked
        // 4 KiB write waits roughly the drain time for the deficit
        // it needed, while an uncontended write is sub-millisecond.
        if write_start.elapsed() >= std::time::Duration::from_secs_f64(BUF_TIME_S * 0.5) {
            queued_s = PIPE_CAP_S;
            last = std::time::Instant::now();
        }
    }
}

/// The live backend — owns the player process, the writer thread, and
/// the app-side state (bank, levels, cue tracking).
pub(crate) struct AudioBackend {
    /// Which player binary carries the stream (for the drop log).
    player: &'static str,
    /// The player process — killed/reaped on drop.
    child: Child,
    /// The mixer thread's join handle (kept for `Drop`).
    writer: Option<JoinHandle<()>>,
    /// Stop flag for `writer_loop`.
    stop: Arc<AtomicBool>,
    /// The shared mixer state.
    state: Arc<Mutex<MixState>>,
    /// One banked buffer per `OneShot` slot, rendered once at open.
    bank: Vec<Arc<Vec<f32>>>,
    /// The settings-derived master gain (mute folds to 0) — applied at
    /// trigger time for one-shots, per-frame for the voices.
    master: f32,
    /// Smoothed continuous-voice levels in `[0, 1]`.
    engine_level: f32,
    /// See `engine_level`.
    plasma_level: f32,
    /// The `FlightSfx` edge detector.
    cues: CueTracker,
    /// `false` once the child has exited — the backend goes inert
    /// instead of writing into a dead pipe every frame.
    alive: bool,
}

impl AudioBackend {
    /// Spawn the first usable player and start the mixer thread.
    /// `None` whenever nothing can be spawned — missing binaries,
    /// restricted spawning — the caller keeps `Option` and the game
    /// runs silent (audio is presentation, never a startup failure).
    pub(crate) fn open(settings: &AudioSettings) -> Option<Self> {
        for player in PLAYERS {
            let mut child = match Command::new(player)
                .args(player_args(player))
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(c) => c,
                Err(_) => continue, // not installed — try the next player
            };
            let Some(mut stdin) = child.stdin.take() else {
                let _ = child.kill();
                let _ = child.wait();
                continue;
            };
            // The settle check: a player that exists but cannot reach
            // its server exits almost immediately — catching it here
            // lets the loop try the next candidate instead of
            // committing to a pipe whose reader is already dead
            // (`update`'s liveness check would only notice after the
            // first frame, with no fallback left to try).
            std::thread::sleep(std::time::Duration::from_millis(PLAYER_SETTLE_MS));
            match child.try_wait() {
                Ok(None) => {}
                Ok(Some(_)) => continue,
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    continue;
                }
            }
            let state = Arc::new(Mutex::new(MixState::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let writer = {
                let state = Arc::clone(&state);
                let stop = Arc::clone(&stop);
                std::thread::Builder::new()
                    .name("csp-audio".into())
                    .spawn(move || writer_loop(&mut stdin, &state, &stop))
            };
            let writer = match writer {
                Ok(w) => w,
                Err(e) => {
                    log::warn!("audio writer thread failed to spawn ({e}) — running silent");
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            };
            let mut backend = Self {
                player,
                child,
                writer: Some(writer),
                stop,
                state,
                bank: OneShot::ALL
                    .iter()
                    .map(|s| Arc::new(synth::render(*s)))
                    .collect(),
                master: 0.0,
                engine_level: 0.0,
                plasma_level: 0.0,
                cues: CueTracker::default(),
                alive: true,
            };
            backend.set_master(settings);
            log::info!("audio backend open via '{player}' — synthesized cues armed");
            return Some(backend);
        }
        log::warn!("no audio player found (tried {PLAYERS:?}) — running silent");
        None
    }

    /// Fold `muted`/`master_volume` into the effective master gain.
    fn set_master(&mut self, settings: &AudioSettings) {
        self.master = master_gain(settings);
    }

    /// Queue one banked one-shot at the current master gain — an
    /// `ActiveShot` cursor onto the shared buffer (no PCM copy).
    fn play(&self, shot: OneShot) {
        if let Ok(mut st) = self.state.lock()
            && st.shots.len() < MAX_ACTIVE_SHOTS
        {
            st.shots.push(ActiveShot {
                buf: Arc::clone(&self.bank[shot.slot()]),
                pos: 0,
                gain: self.master,
            });
        }
        // A poisoned lock means the writer already gave up — the
        // backend is inert; dropping the shot is the graceful path.
    }

    /// Session-side acknowledge — settings applied, mode switched.
    /// Lives here (not the cue stream) because it is an *app* event,
    /// not a sim transition the `FlightSfx` channel detects.
    pub(crate) fn ui_confirm(&self) {
        if self.alive {
            self.play(OneShot::UiConfirm);
        }
    }

    /// The per-frame pump, called after `View::end_frame` (the cue
    /// queue is fresh, the snapshot is staged): check liveness, drain
    /// new cues into one-shots, then re-derive the continuous voices'
    /// gains. All inputs are read-only presentation resources.
    pub(crate) fn update(
        &mut self,
        sfx: &FlightSfx,
        snap: &FlightSnapshot,
        settings: &AudioSettings,
    ) {
        if self.alive {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => {
                    log::warn!("audio player '{}' exited — running silent", self.player);
                    self.alive = false;
                }
                Ok(None) => {}
            }
        }
        // A dead player makes the whole pass a no-op — cue drains,
        // gain writes, and `play` pushes all happen only while alive.
        if !self.alive {
            return;
        }
        self.set_master(settings);
        for text in self.cues.drain_new(&sfx.cues) {
            // `cue_shot` is total — every pushed cue resolves to a
            // sound, unrecognized texts to the ack blip (P13-F2).
            self.play(cue_shot(&text));
        }

        // Continuous voices: amplitude follows the focused vessel —
        // the SAME pin-aware resolution `flight_present` applies to
        // the chase camera, HUD, and cue edges (a `recover_control`
        // pin must not leave the voices on the newest spawn while the
        // player flies the recovered vessel — P07-F1).
        let focus = sfx
            .focus_vessel
            .and_then(|id| snap.vessels.iter().find(|v| v.id == id));
        // P13-F4: a NaN throttle/flux frame must not poison the
        // smoother — `NaN.clamp` passes NaN through, and a NaN level
        // would pin the gain at 0 forever (NaN > LEVEL_EPS is false).
        // Non-finite input reads as silence for that frame.
        let engine_target = focus.map_or(0.0, |v| {
            if v.throttle.is_finite() {
                v.throttle.clamp(0.0, 1.0) as f32
            } else {
                0.0
            }
        });
        let plasma_target = focus.map_or(0.0, |v| {
            if v.plasma_w_m2.is_finite() {
                // The gate's speed is the same surface-relative
                // `v_rel` the sheath solves (`flow_sample`'s
                // convention) — `vel` alone under the view's own
                // no-environment fallback.
                let v_rel = snap
                    .env
                    .as_ref()
                    .map_or(v.vel, |e| v.vel - e.body_spin.cross(v.com - e.body_center));
                csp_view::flight::plasma_intensity(v.plasma_w_m2, v_rel.norm()) as f32
            } else {
                0.0
            }
        });
        self.engine_level += (engine_target - self.engine_level) * LEVEL_SMOOTH;
        self.plasma_level += (plasma_target - self.plasma_level) * LEVEL_SMOOTH;
        // …and the same guard on the state itself: if a non-finite
        // level ever lands here anyway, reset to silence so the NEXT
        // finite frame revives the voice instead of staying dead.
        if !self.engine_level.is_finite() {
            self.engine_level = 0.0;
        }
        if !self.plasma_level.is_finite() {
            self.plasma_level = 0.0;
        }
        if let Ok(mut st) = self.state.lock() {
            st.engine_gain = if self.engine_level > LEVEL_EPS {
                self.master * self.engine_level * ENGINE_GAIN
            } else {
                0.0
            };
            st.whoosh_gain = if self.plasma_level > LEVEL_EPS {
                self.master * self.plasma_level * WHOOSH_GAIN
            } else {
                0.0
            };
        }
    }
}

impl Drop for AudioBackend {
    /// Bounded teardown: kill the player (unblocks any in-flight write
    /// with EPIPE), join the writer (its `stdin` drop gives the player
    /// EOF), reap the process. A player that already exited makes each
    /// step a no-op error — ignored by contract.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.child.kill();
        if let Some(w) = self.writer.take() {
            let _ = w.join();
        }
        let _ = self.child.wait();
    }
}

/// The settings fold: `muted` collapses the mix to silence regardless
/// of volume; an out-of-range volume clamps into `[0, 1]`. A
/// non-finite value — a `NaN`/`inf` literal in a hand-edited
/// `settings.ron` survives `f32::clamp` (P14-F3) — folds to the
/// shipped default rather than feeding NaN PCM into the player.
fn master_gain(settings: &AudioSettings) -> f32 {
    if settings.muted {
        0.0
    } else if settings.master_volume.is_finite() {
        settings.master_volume.clamp(0.0, 1.0)
    } else {
        AudioSettings::default().master_volume
    }
}

#[cfg(test)]
// Tests signal failure by panicking (§2.13 covers shipping paths).
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn cue(text: &str, ttl: u32) -> SfxCue {
        SfxCue {
            text: text.to_owned(),
            ttl,
        }
    }

    /// The vocabulary lock: every `sfx_edges` transition text maps to
    /// its own one-shot, the notice families map by prefix to the ack
    /// blip (incl. `format!` texts that can never literal-match), and
    /// an unrecognized cue still gets the default beep — `cue_shot`
    /// is total, so no pushed cue is ever audio-silent (P13-F2).
    #[test]
    fn every_flight_cue_maps() {
        for (text, want) in [
            ("ENGINE IGNITION", OneShot::EngineIgnition),
            ("ENGINE CUT", OneShot::EngineCut),
            ("STAGE — PART JETTISONED", OneShot::StageJettison),
            ("CONTACT", OneShot::Contact),
            ("LANDED — SURFACE RAILS", OneShot::Contact),
            ("LIFTOFF", OneShot::EngineIgnition),
            ("BREAKUP", OneShot::Breakup),
            ("DEBRIS IMPACT", OneShot::DebrisImpact),
            ("VESSEL REMOVED", OneShot::VesselRemoved),
            ("VESSEL LEFT BUBBLE", OneShot::VesselLeftBubble),
        ] {
            assert_eq!(cue_shot(text), want, "{text:?} mapped wrong");
        }
        // The `format!` confirm and every refusal/notice family —
        // exact-matchable or not — resolve to a shot.
        for (text, want) in [
            ("STAGED — 3 PART(S) JETTISONED", OneShot::StageJettison),
            ("SESSION IS READ-ONLY — LAUNCH REFUSED", OneShot::UiConfirm),
            (
                "SESSION IS READ-ONLY — ATTITUDE NOT APPLIED",
                OneShot::UiConfirm,
            ),
            (
                "SESSION IS READ-ONLY — THROTTLE NOT APPLIED",
                OneShot::UiConfirm,
            ),
            (
                "PARKED — ATTITUDE LOCKED UNTIL LAUNCH (THROTTLE TO LIFTOFF)",
                OneShot::UiConfirm,
            ),
            ("ATTITUDE LOCKED — VESSEL IS ON RAILS", OneShot::UiConfirm),
            ("NO VESSEL — ATTITUDE LOCKED", OneShot::UiConfirm),
            ("VESSEL ON RAILS — THROTTLE LOCKED", OneShot::UiConfirm),
            ("NO VESSEL — THROTTLE LOCKED", OneShot::UiConfirm),
            ("NOT STAGED — VESSEL IS ON RAILS", OneShot::UiConfirm),
            ("NO VESSEL TO RECOVER", OneShot::UiConfirm),
            (
                "COMMAND REFUSED — CONTROL: VESSEL ON RAILS",
                OneShot::UiConfirm,
            ),
            ("SETTINGS — 2 KEY BINDING(S) MIGRATED", OneShot::UiConfirm),
            // Unknown future text still gets the default beep.
            ("SOMETHING FUTURE-PRODUCED", OneShot::UiConfirm),
        ] {
            assert_eq!(cue_shot(text), want, "{text:?} mapped wrong");
        }
    }

    /// `CueTracker` fires each push exactly once: a fresh cue is new,
    /// its decayed form next frame is not, a same-text push one frame
    /// later fires again, and same-frame duplicates all fire.
    #[test]
    fn cue_tracker_fires_only_new_pushes() {
        let mut tr = CueTracker::default();
        // Frame 1: one push at the post-decrement TTL.
        let q = VecDeque::from([cue("CONTACT", 89)]);
        assert_eq!(tr.drain_new(&q), vec!["CONTACT".to_string()]);
        // Frame 2: decayed — nothing new.
        let q = VecDeque::from([cue("CONTACT", 88)]);
        assert!(tr.drain_new(&q).is_empty());
        // Frame 3: the old cue decays AND a second CONTACT lands.
        let q = VecDeque::from([cue("CONTACT", 87), cue("CONTACT", 89)]);
        assert_eq!(tr.drain_new(&q), vec!["CONTACT".to_string()]);
        // Frame 4: two different pushes in one pass both fire.
        let q = VecDeque::from([
            cue("CONTACT", 86),
            cue("CONTACT", 88),
            cue("BREAKUP", 89),
            cue("DEBRIS IMPACT", 89),
        ]);
        assert_eq!(
            tr.drain_new(&q),
            vec!["BREAKUP".to_string(), "DEBRIS IMPACT".to_string()]
        );
        // Frame 5: everything expired — the drain is quiet.
        assert!(tr.drain_new(&VecDeque::new()).is_empty());
        // Frame 6: fresh cue after an empty frame still fires.
        let q = VecDeque::from([cue("ENGINE IGNITION", 89)]);
        assert_eq!(tr.drain_new(&q), vec!["ENGINE IGNITION".to_string()]);
    }

    /// The mixer: shots play to completion and expire, gains gate the
    /// continuous voices, output stays inside headroom.
    #[test]
    fn mix_state_combines_and_expires_shots() {
        let mut st = MixState::new();
        st.engine_gain = 0.5;
        st.shots.push(ActiveShot {
            buf: Arc::new(vec![0.8; 64]),
            pos: 0,
            gain: 0.5,
        });
        let mut buf = vec![0.0f32; 16 * CHANNELS];
        st.mix(&mut buf);
        assert!(buf.iter().any(|s| *s > 0.0));
        // 64-sample shot is consumed across mixes; it expires.
        let mut big = vec![0.0f32; 128 * CHANNELS];
        st.mix(&mut big);
        assert!(st.shots.is_empty());
        // Gain zero → the voice contributes nothing (rumble skipped).
        st.engine_gain = 0.0;
        let mut quiet = vec![1.0f32; 8 * CHANNELS];
        st.mix(&mut quiet);
        assert!(quiet.iter().all(|s| *s == 0.0));
    }

    /// The settings fold: volume clamps into `[0, 1]`, mute zeroes.
    #[test]
    fn master_volume_fold() {
        let mut s = AudioSettings {
            master_volume: 0.4,
            muted: false,
        };
        assert_eq!(master_gain(&s), 0.4);
        s.master_volume = 1.7;
        assert_eq!(master_gain(&s), 1.0);
        s.muted = true;
        assert_eq!(master_gain(&s), 0.0);
        s.muted = false;
        s.master_volume = -0.5;
        assert_eq!(master_gain(&s), 0.0);
    }

    /// The argv contract: every player carries its low-latency flag —
    /// the regression guard for the latency work (a dropped flag
    /// silently returns cue latency to the server-buffered default).
    #[test]
    fn player_args_request_low_latency() {
        for player in PLAYERS {
            let args = player_args(player).join(" ");
            let bounded = match *player {
                "pw-play" => args.contains("--latency=1024"),
                "paplay" => args.contains("--latency-msec=50"),
                _ => args.contains("--period-size=512") && args.contains("--buffer-size=2048"),
            };
            assert!(bounded, "{player} argv lacks its latency flag: {args}");
        }
    }

    /// Open + pump + drop, exercising the real subprocess path whenever
    /// a player binary exists (headless/minimal boxes get `None` — the
    /// graceful-absence contract this test guards: `open` never panics).
    /// Drop kills + reaps the child and joins the writer — a clean
    /// exit through `Drop` IS the init/drop gate.
    #[test]
    fn open_update_drop_is_clean() {
        let settings = AudioSettings::default();
        let Some(mut backend) = AudioBackend::open(&settings) else {
            return; // graceful-absence path
        };
        // `prev` is flight.rs-private — stage cues via the pub queue.
        // The voices' focus mirror: `focus_vessel` names the vessel the
        // pin-aware resolver picked (vessel id 1 below) — without it
        // there is no focus and the voices correctly read silence.
        let mut sfx = FlightSfx::default();
        sfx.cues.push_back(cue("BREAKUP", 89));
        sfx.focus_vessel = Some(1);
        let snap = FlightSnapshot {
            vessels: vec![csp_view::flight::FlightBody {
                id: 1,
                pos: csp_core::DVec3::ZERO,
                rot: csp_core::DQuat::IDENTITY,
                // Reentry speed: C19-F1's speed gate reads `|v_rel|`
                // (`env: None` → v_rel = vel), so a stationary vessel
                // carrying flux alone must — and does — read silent.
                vel: csp_core::DVec3::new(0.0, 0.0, -2200.0),
                throttle: 0.7,
                debris: false,
                contact: false,
                plasma_w_m2: 4.0e4,
                nodes: 3,
                parts: Vec::new(),
                plumes: Vec::new(),
                com: csp_core::DVec3::ZERO,
                latched: false,
                tanks: Vec::new(),
            }],
            env: None,
        };
        for _ in 0..3 {
            backend.update(&sfx, &snap, &settings);
        }
        assert!(backend.engine_level > 0.0);
        assert!(backend.plasma_level > 0.0);
        backend.ui_confirm();
        // drop(backend): kill → join → reap — see Drop's contract note.
    }
}
