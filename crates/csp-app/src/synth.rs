//! M10's synthesized sound bank — PLAN §3.7's procedural-first rule
//! made concrete: every SFX is *generated*, so the audio surface has
//! zero asset files, zero `ATTRIBUTION.md` rows, and zero third-party
//! licenses to audit.
//!
//! Everything here is a **pure function of its parameters** — no
//! device types, no device, no wall clock. Noise comes from a fixed-seed
//! xorshift64* (never the banned `rand` facade — deny.toml), so a given
//! sound renders identically per build and tests can assert on the actual
//! samples (note: `f32::sin`/`f32::exp` are platform libm, so bit-identity
//! is per-machine, not cross-platform — fine for presentation-only audio).
//! `audio.rs` mixes the products into the player subprocess; this module
//! never knows a backend exists.

/// The bank's render rate — mono f32 samples; the audio backend's mixer
/// interleaves pairs into stereo for the player subprocess.
pub(crate) const SAMPLE_RATE: u32 = 44_100;

/// Sample count for `dur_s` seconds of audio.
fn frames(dur_s: f32) -> usize {
    (dur_s * SAMPLE_RATE as f32) as usize
}

/// Deterministic noise — xorshift64* over a fixed seed. Same seed in,
/// same byte sequence out, on every platform: the "asset" is the seed,
/// and the seed is code.
struct Noise {
    state: u64,
}

impl Noise {
    fn new(seed: u64) -> Self {
        // Zero is the xorshift sink state — never let a seed land there.
        Self { state: seed | 1 }
    }

    /// The next sample in `[-1, 1)` — top 24 bits of the output word,
    /// well clear of the low-bit linearity xorshift is known for.
    fn next_f32(&mut self) -> f32 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        let v = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
        ((v >> 40) as f32) * (1.0 / 8_388_608.0) - 1.0
    }
}

/// `sin(2π·f·t)` in samples — every oscillator in the bank is this.
fn sine(hz: f32, i: usize) -> f32 {
    (2.0 * std::f32::consts::PI * hz * i as f32 / SAMPLE_RATE as f32).sin()
}

/// A free-running oscillator for the endless voices — a phase
/// accumulator that wraps at the *period* boundary, so unlike a raw
/// sample counter it never jumps.
struct Osc {
    /// Cycles per second.
    hz: f32,
    /// Phase in `[0, 1)` — fraction of the current cycle.
    phase: f32,
}

impl Osc {
    fn new(hz: f32) -> Self {
        Self { hz, phase: 0.0 }
    }

    fn next(&mut self) -> f32 {
        let s = (2.0 * std::f32::consts::PI * self.phase).sin();
        self.phase += self.hz / SAMPLE_RATE as f32;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        s
    }
}

/// Exponential decay envelope: `exp(-t/τ)` — 1 at attack, gone at ~5τ.
fn decay(i: usize, tau_s: f32) -> f32 {
    f32::exp(-(i as f32) / (tau_s * SAMPLE_RATE as f32))
}

/// A resonant thump: a decaying sine plus a short noise burst for the
/// strike transient. The contact/impact family.
fn thump(out: &mut [f32], hz: f32, tau_s: f32, noise_gain: f32, noise_tau_s: f32, seed: u64) {
    let mut noise = Noise::new(seed);
    for (i, s) in out.iter_mut().enumerate() {
        *s += sine(hz, i) * decay(i, tau_s) + noise.next_f32() * noise_gain * decay(i, noise_tau_s);
    }
}

/// The discrete sounds the cue map names — one variant per §3.7/M4c
/// transition the `FlightSfx` channel can push, plus the UI ack blip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OneShot {
    /// Throttle rising edge — a rumble swell.
    EngineIgnition,
    /// Throttle falling edge — a short puff.
    EngineCut,
    /// Part-graph node count dropped — the stage clunk.
    StageJettison,
    /// Ground-contact rising edge — soft touchdown thump.
    Contact,
    /// Debris count rose — the breakup crunch.
    Breakup,
    /// Debris left the world — impact thump.
    DebrisImpact,
    /// A non-debris vessel left the world — heavier thump.
    VesselRemoved,
    /// The focus vessel left the bubble — a falling two-tone fade.
    VesselLeftBubble,
    /// Session-side acknowledge (settings apply, mode switch).
    UiConfirm,
}

impl OneShot {
    /// Bank slot for this sound — the backend renders each variant
    /// once into `bank[shot.slot()]`.
    pub(crate) fn slot(self) -> usize {
        self as usize
    }

    /// The variant count — the bank array's size.
    pub(crate) const COUNT: usize = Self::UiConfirm as usize + 1;

    /// Every variant in slot order — the bank's render table.
    pub(crate) const ALL: [OneShot; Self::COUNT] = [
        Self::EngineIgnition,
        Self::EngineCut,
        Self::StageJettison,
        Self::Contact,
        Self::Breakup,
        Self::DebrisImpact,
        Self::VesselRemoved,
        Self::VesselLeftBubble,
        Self::UiConfirm,
    ];
}

/// Render one bank slot — a finished, listen-ready mono buffer.
/// Deterministic in, deterministic out per build. A hard limit on the
/// way out guarantees the mixer never sees a sample outside `[-1, 1]`.
pub(crate) fn render(shot: OneShot) -> Vec<f32> {
    let mut out = render_raw(shot);
    for s in &mut out {
        *s = s.clamp(-1.0, 1.0);
    }
    out
}

/// The per-variant recipes — see [`render`].
fn render_raw(shot: OneShot) -> Vec<f32> {
    match shot {
        OneShot::EngineIgnition => {
            // 0.6 s rumble swell: leaky-integrated noise ramping in,
            // 50 Hz sub swelling behind it, both decaying out.
            let mut out = vec![0.0; frames(0.6)];
            let mut noise = Noise::new(0x1A11_7101);
            let mut brown = 0.0f32;
            for (i, s) in out.iter_mut().enumerate() {
                brown = brown * 0.99 + noise.next_f32() * 0.08;
                let swell = 1.0 - f32::exp(-(i as f32) / (0.08 * SAMPLE_RATE as f32));
                *s = (brown * 0.9 + sine(50.0, i) * 0.35) * swell * decay(i, 0.35);
            }
            out
        }
        OneShot::EngineCut => {
            // 0.35 s cut puff: fast noise burst + a falling blip.
            let mut out = vec![0.0; frames(0.35)];
            let mut noise = Noise::new(0xC017_5EED);
            for (i, s) in out.iter_mut().enumerate() {
                // Slight pitch sag — the engine "dying", 140 → 80 Hz.
                let f = 140.0 - 60.0 * i as f32 / out_len(0.35);
                *s = noise.next_f32() * 0.5 * decay(i, 0.05) + sine(f, i) * 0.45 * decay(i, 0.09);
            }
            out
        }
        OneShot::StageJettison => {
            // 0.4 s metallic clunk: two decaying partials + a strike
            // burst, then a quieter echo clunk 90 ms later (the latches
            // letting go in sequence).
            let mut out = vec![0.0; frames(0.4)];
            thump(&mut out, 170.0, 0.06, 0.5, 0.03, 0x0057_A6E1);
            let mut noise = Noise::new(0x0057_A6E2);
            let off = frames(0.09);
            for (i, s) in out.iter_mut().enumerate().skip(off) {
                *s += (sine(150.0, i - off) * decay(i - off, 0.05)
                    + sine(640.0, i - off) * 0.35 * decay(i - off, 0.025)
                    + noise.next_f32() * 0.3 * decay(i - off, 0.02))
                    * 0.5;
            }
            out
        }
        OneShot::Contact => {
            // 0.3 s touchdown: low body thump + a dust-puff hiss.
            let mut out = vec![0.0; frames(0.3)];
            thump(&mut out, 62.0, 0.11, 0.35, 0.03, 0x0C04_47A6);
            out
        }
        OneShot::Breakup => {
            // 0.9 s crunch: amplitude-rattled noise over a slow low
            // decay — the "structure tearing" sound.
            let mut out = vec![0.0; frames(0.9)];
            let mut noise = Noise::new(0xB8EA_AA01);
            for (i, s) in out.iter_mut().enumerate() {
                let rattle = 0.55 + 0.45 * sine(23.0, i);
                *s = noise.next_f32() * rattle * decay(i, 0.3) * 0.8
                    + sine(48.0, i) * 0.5 * decay(i, 0.4);
            }
            out
        }
        OneShot::DebrisImpact => {
            // 0.35 s mid thump — debris hitting at range.
            let mut out = vec![0.0; frames(0.35)];
            thump(&mut out, 90.0, 0.09, 0.4, 0.04, 0x00DE_8815);
            out
        }
        OneShot::VesselRemoved => {
            // 0.4 s heavier thump — the focus craft itself impacted.
            let mut out = vec![0.0; frames(0.4)];
            thump(&mut out, 75.0, 0.13, 0.45, 0.04, 0x005E_11ED);
            out
        }
        OneShot::VesselLeftBubble => {
            // 0.25 s falling two-tone — the "gone" fade.
            let mut out = vec![0.0; frames(0.25)];
            let split = frames(0.12);
            for (i, s) in out.iter_mut().enumerate() {
                let hz = if i < split { 520.0 } else { 390.0 };
                *s = sine(hz, i) * 0.4 * decay(i, 0.09);
            }
            out
        }
        OneShot::UiConfirm => {
            // 0.09 s clean ping + overtone — panel acknowledge.
            let mut out = vec![0.0; frames(0.09)];
            for (i, s) in out.iter_mut().enumerate() {
                *s = sine(880.0, i) * 0.35 * decay(i, 0.03)
                    + sine(1320.0, i) * 0.12 * decay(i, 0.02);
            }
            out
        }
    }
}

/// Convenience for the renderers: `frames(dur)` as f32 for rate math.
fn out_len(dur_s: f32) -> f32 {
    dur_s * SAMPLE_RATE as f32
}

/// The continuous engine voice — endless low rumble. Amplitude is the
/// backend's job (its gain tracks throttle); this voice is a
/// constant-level texture so any gain rides cleanly on top.
pub(crate) struct Rumble {
    noise: Noise,
    /// The 46 Hz sub tone.
    sub: Osc,
    /// Leaky integrator — a bounded "brown" rumble, no DC runaway.
    brown: f32,
    /// One-pole smoother on `brown`.
    lp: f32,
}

impl Rumble {
    pub(crate) fn new() -> Self {
        Self {
            noise: Noise::new(0x8088_5EED),
            sub: Osc::new(46.0),
            brown: 0.0,
            lp: 0.0,
        }
    }
}

impl Iterator for Rumble {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let w = self.noise.next_f32();
        self.brown = self.brown * 0.995 + w * 0.05;
        self.lp += (self.brown - self.lp) * 0.4;
        Some((self.lp * 2.2 + self.sub.next() * 0.4).clamp(-1.0, 1.0) * 0.55)
    }
}

/// The continuous reentry voice — band-limited wind noise with a slow
/// gust LFO. Volume tracks the plasma flux (backend); texture only.
pub(crate) struct Whoosh {
    noise: Noise,
    /// The ~0.35 Hz gust LFO.
    gust: Osc,
    /// Slow one-pole — the baseline the hiss floats above.
    lp: f32,
    /// Second pole — softens the top of the band.
    lp2: f32,
}

impl Whoosh {
    pub(crate) fn new() -> Self {
        Self {
            noise: Noise::new(0xEE02_AAAA),
            gust: Osc::new(0.35),
            lp: 0.0,
            lp2: 0.0,
        }
    }
}

impl Iterator for Whoosh {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let w = self.noise.next_f32();
        self.lp += (w - self.lp) * 0.05;
        let band = w - self.lp; // highpass-flavored noise
        self.lp2 += (band - self.lp2) * 0.5;
        // Slow gusts — the wind sound isn't a flat hiss.
        let gust = 0.7 + 0.3 * self.gust.next();
        Some((self.lp2 * 1.6 * gust).clamp(-1.0, 1.0) * 0.5)
    }
}

#[cfg(test)]
// Tests signal failure by panicking (§2.13 covers shipping paths).
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn peak(buf: &[f32]) -> f32 {
        buf.iter().fold(0.0f32, |a, s| a.max(s.abs()))
    }

    fn rms(buf: &[f32]) -> f32 {
        (buf.iter().map(|s| s * s).sum::<f32>() / buf.len().max(1) as f32).sqrt()
    }

    /// Every bank slot renders: nonzero energy, output bounded for the
    /// mixer, and a sane duration (nothing zero-length, nothing over 2 s).
    #[test]
    fn every_one_shot_renders_bounded() {
        for shot in OneShot::ALL {
            let buf = render(shot);
            assert!(!buf.is_empty() && buf.len() <= frames(2.0), "{shot:?} len");
            assert!(
                peak(&buf) <= 1.0,
                "{shot:?} clipped past the mixer's headroom"
            );
            assert!(rms(&buf) > 0.01, "{shot:?} is silent");
        }
    }

    /// The deterministic contract: identical params → identical bytes.
    /// This is what makes synthesized audio a zero-attribution asset —
    /// the bytes ARE the code.
    #[test]
    fn renders_are_deterministic() {
        assert_eq!(render(OneShot::Breakup), render(OneShot::Breakup));
        assert_eq!(
            render(OneShot::StageJettison),
            render(OneShot::StageJettison)
        );
        // And different sounds differ (guards the all-zero regression).
        assert_ne!(render(OneShot::Breakup), render(OneShot::Contact));
    }

    /// The continuous voices are endless and bounded — the backend
    /// gates them by volume, so they must never run dry or clip.
    #[test]
    fn continuous_voices_are_bounded_and_endless() {
        let mut rumble = Rumble::new();
        let mut whoosh = Whoosh::new();
        for _ in 0..SAMPLE_RATE * 3 {
            let r = rumble.next().expect("rumble ran dry");
            let w = whoosh.next().expect("whoosh ran dry");
            assert!(r.abs() <= 1.0 && w.abs() <= 1.0);
        }
        // A few seconds in, both have found their level — not silence.
        assert!(rms(&rumble.take(4096).collect::<Vec<_>>()) > 0.005);
        assert!(rms(&whoosh.take(4096).collect::<Vec<_>>()) > 0.005);
    }
}
