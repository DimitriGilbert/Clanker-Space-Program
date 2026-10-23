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
//! f32 into a native player *subprocess* (`paplay`, else `aplay`) over
//! a piped stdin — no FFI, no unsafe, no link-time audio libraries,
//! so the build needs nothing beyond a C toolchain and degrades to
//! silence wherever no player (or server) exists. That is also the
//! deliberate "simpler approach" the audio row allows: cpal/rodio/
//! miniaudio all need platform dev packages or libclang at *build*
//! time; a subprocess needs none and talks to the same sound servers
//! (pipewire-pulse / pipewire-alsa) those crates would target.
//!
//! **Graceful absence.** No player binary, dead server, killed child —
//! none of it is an error. `AudioBackend::open` returns `None` when
//! nothing can be spawned and the app keeps an `Option`; a child that
//! exits mid-session flips the backend inert, never a panic.
//! Native-only: the whole module is `cfg(not(target_arch = "wasm32"))`,
//! so the wasm slice is untouched.

use std::collections::{BTreeSet, VecDeque};
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
/// Frames mixed per pipe write — ~46 ms at 44.1 kHz, the cue-to-ear
/// latency budget (a SFX cue lands well inside a frame's notice).
const FRAMES_PER_BUF: usize = 2048;
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

/// The whoosh's display normalization — the same convective-flux
/// window `csp_view::flight` normalizes the plasma sheath by (its
/// `PLASMA_ONSET_W_M2`/`PLASMA_FULL_W_M2` — private there; the window
/// is a presentation curve, not sim state, so mirroring the constants
/// here is honest).
const PLASMA_ONSET_W_M2: f64 = 5.0e3;
/// See `PLASMA_ONSET_W_M2`.
const PLASMA_FULL_W_M2: f64 = 8.0e4;

/// The cue text → one-shot map — the vocabulary `flight_present`
/// pushes, mirrored literally (the strings are its wire format; the
/// drift detector in `AudioBackend::update` logs any cue that lands
/// unmapped).
fn cue_shot(text: &str) -> Option<OneShot> {
    match text {
        "ENGINE IGNITION" => Some(OneShot::EngineIgnition),
        "ENGINE CUT" => Some(OneShot::EngineCut),
        "STAGE — PART JETTISONED" => Some(OneShot::StageJettison),
        "CONTACT" => Some(OneShot::Contact),
        "BREAKUP" => Some(OneShot::Breakup),
        "DEBRIS IMPACT" => Some(OneShot::DebrisImpact),
        "VESSEL REMOVED" => Some(OneShot::VesselRemoved),
        "VESSEL LEFT BUBBLE" => Some(OneShot::VesselLeftBubble),
        _ => None,
    }
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
/// `paplay` (pipewire-pulse/pulse) first — it is the desktop session's
/// native API; `aplay` (ALSA, usually via the pipewire-alsa plugin)
/// as the fallback.
const PLAYERS: &[&str] = &["paplay", "aplay"];

/// The argv for `player` — raw-stream flags naming our wire format.
fn player_args(player: &str) -> Vec<String> {
    match player {
        "paplay" => vec![
            "--raw".into(),
            "--format=float32le".into(),
            format!("--rate={SAMPLE_RATE}"),
            format!("--channels={CHANNELS}"),
        ],
        // aplay: `-` = stdin, `-q` = quiet, FLOAT_LE = our f32 stream.
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
/// the natural pacer (the player drains at real-time rate), and it
/// happens *outside* the mix lock.
fn writer_loop(stdin: &mut ChildStdin, state: &Mutex<MixState>, stop: &AtomicBool) {
    let mut buf = vec![0.0f32; FRAMES_PER_BUF * CHANNELS];
    let mut bytes = vec![0u8; buf.len() * 4];
    while !stop.load(Ordering::Relaxed) {
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
        if stdin.write_all(&bytes).is_err() {
            return; // player exited or pipe closed — silently done
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
    /// Cue texts already warned about — unmapped cues log once, not
    /// per frame (a `flight.rs` string drift is a report, not spam).
    unmapped: BTreeSet<String>,
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
                unmapped: BTreeSet::new(),
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
        self.set_master(settings);
        for text in self.cues.drain_new(&sfx.cues) {
            match cue_shot(&text) {
                Some(shot) => self.play(shot),
                // Log once per text — an unmapped cue means flight.rs's
                // vocabulary drifted from `cue_shot`'s table.
                None => {
                    if self.unmapped.insert(text.clone()) {
                        log::warn!("unmapped SFX cue {text:?} — cue vocabulary drifted");
                    }
                }
            }
        }
        if !self.alive {
            return;
        }

        // Continuous voices: amplitude follows the focused vessel —
        // same `vessels.first()` focus convention `flight_present` uses.
        let focus = snap.vessels.first();
        let engine_target = focus.map_or(0.0, |v| v.throttle.clamp(0.0, 1.0) as f32);
        let plasma_target = focus.map_or(0.0, |v| {
            ((v.plasma_w_m2 - PLASMA_ONSET_W_M2) / (PLASMA_FULL_W_M2 - PLASMA_ONSET_W_M2))
                .clamp(0.0, 1.0) as f32
        });
        self.engine_level += (engine_target - self.engine_level) * LEVEL_SMOOTH;
        self.plasma_level += (plasma_target - self.plasma_level) * LEVEL_SMOOTH;
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
/// of volume; an out-of-range volume clamps into `[0, 1]`.
fn master_gain(settings: &AudioSettings) -> f32 {
    if settings.muted {
        0.0
    } else {
        settings.master_volume.clamp(0.0, 1.0)
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

    /// The vocabulary lock: every cue text `flight_present` can push
    /// maps to a one-shot — the table is the wire format, so this test
    /// is the drift tripwire (update() also logs unknowns once).
    #[test]
    fn every_flight_cue_maps() {
        for text in [
            "ENGINE IGNITION",
            "ENGINE CUT",
            "STAGE — PART JETTISONED",
            "CONTACT",
            "BREAKUP",
            "DEBRIS IMPACT",
            "VESSEL REMOVED",
            "VESSEL LEFT BUBBLE",
        ] {
            assert!(cue_shot(text).is_some(), "{text:?} unmapped");
        }
        assert!(cue_shot("NO SUCH CUE").is_none());
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
        let mut sfx = FlightSfx::default();
        sfx.cues.push_back(cue("BREAKUP", 89));
        let snap = FlightSnapshot {
            vessels: vec![csp_view::flight::FlightBody {
                id: 1,
                pos: csp_core::DVec3::ZERO,
                rot: csp_core::DQuat::IDENTITY,
                vel: csp_core::DVec3::ZERO,
                throttle: 0.7,
                debris: false,
                contact: false,
                plasma_w_m2: 4.0e4,
                nodes: 3,
                boxes: Vec::new(),
                plumes: Vec::new(),
                com: csp_core::DVec3::ZERO,
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
