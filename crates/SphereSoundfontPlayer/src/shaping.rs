//! Amp envelope and render-quality shaping for [`crate::SoundfontPlayer`].
//!
//! Both live here because they are the two things the player does *around*
//! rustysynth rather than through it:
//!
//! - [`SoundfontEnvelope`] is an A/D/S/R applied to the player's summed stereo
//!   output, gated by note activity. rustysynth exposes no per-voice envelope
//!   access and its SoundFont region generators are private, so a true per-note
//!   AHDSR is not reachable through the crate. What this is instead is honest
//!   and stated plainly: it shapes the instrument's output as a whole — attack
//!   and decay run when the first note starts from silence, release runs when
//!   the last note ends. Overlapping notes inside a phrase do not each get their
//!   own contour.
//!
//! - [`SoundfontRenderQuality`] oversamples the synthesizer. rustysynth's
//!   oscillator resamples SoundFont data with plain *linear* interpolation
//!   (`Oscillator::fill_block_*`), whose error rises steeply with transposition
//!   and folds back into the audible band. Running the synthesizer at 2x/4x the
//!   output rate pushes that error above the output Nyquist, where the
//!   decimation filter removes it.
//!
//! Everything a render block touches is preallocated at build time: filter
//! taps, history, and the oversampled scratch. [`Decimator::render_chunk_frames`]
//! bounds one pass, and longer buffers are processed as repeated passes rather
//! than by growing a buffer on the audio thread.

use serde::{Deserialize, Serialize};

/// Longest attack/decay/release the panel and the engine accept, in
/// milliseconds. Ten seconds covers slow pad swells without letting a project
/// file ask for an envelope that outlives any musical phrase.
pub const ENVELOPE_MAX_TIME_MS: f32 = 10_000.0;

/// Amplitude envelope applied to the player's summed output.
///
/// The default is the identity: attack/decay/release at 0 and sustain at 1.0.
/// [`Self::is_bypassed`] reports that state and the render path then skips the
/// envelope entirely, so an untouched player is sample-for-sample what it was
/// before this existed.
///
/// `release_ms == 0.0` means *off*, not "cut instantly": the envelope stays open
/// when the last note ends and the SoundFont's own release (and the reverb tail)
/// plays out. That keeps a user who only reaches for Attack from silently losing
/// every note tail, and it is what makes the default state continuous with the
/// bypassed one.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SoundfontEnvelope {
    pub attack_ms: f32,
    pub decay_ms: f32,
    /// Level held while notes are down, `0.0..=1.0`.
    pub sustain: f32,
    pub release_ms: f32,
}

impl Default for SoundfontEnvelope {
    fn default() -> Self {
        Self {
            attack_ms: 0.0,
            decay_ms: 0.0,
            sustain: 1.0,
            release_ms: 0.0,
        }
    }
}

impl SoundfontEnvelope {
    /// Clamps every field into its accepted range and replaces non-finite
    /// values with the default. A project file or an FFI caller must not be able
    /// to install a NaN rate that would silence the instrument forever.
    pub fn sanitized(self) -> Self {
        let time = |value: f32, fallback: f32| {
            if value.is_finite() {
                value.clamp(0.0, ENVELOPE_MAX_TIME_MS)
            } else {
                fallback
            }
        };
        Self {
            attack_ms: time(self.attack_ms, 0.0),
            decay_ms: time(self.decay_ms, 0.0),
            sustain: if self.sustain.is_finite() {
                self.sustain.clamp(0.0, 1.0)
            } else {
                1.0
            },
            release_ms: time(self.release_ms, 0.0),
        }
    }

    /// `true` when the envelope would not change the signal, so the render path
    /// can leave the SoundFont's own envelopes untouched.
    pub fn is_bypassed(&self) -> bool {
        let e = self.sanitized();
        e.attack_ms <= 0.0 && e.decay_ms <= 0.0 && e.sustain >= 1.0 && e.release_ms <= 0.0
    }
}

/// Internal synthesis rate relative to the output rate.
///
/// Higher settings cost proportionally more CPU: the synthesizer really does
/// render 2x or 4x as many samples per block, and the decimation filter runs on
/// all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SoundfontRenderQuality {
    /// Render at the output sample rate — rustysynth's own linear
    /// interpolation, unchanged.
    #[default]
    Standard,
    /// Render at 2x and decimate.
    High,
    /// Render at 4x and decimate.
    Ultra,
}

impl SoundfontRenderQuality {
    pub const ALL: [Self; 3] = [Self::Standard, Self::High, Self::Ultra];

    pub fn oversample(self) -> usize {
        match self {
            Self::Standard => 1,
            Self::High => 2,
            Self::Ultra => 4,
        }
    }

    /// Short display name. Also the persisted form — [`Self::from_key`] parses
    /// it back, so a project written by a newer build degrades to `Standard`
    /// instead of failing to load.
    pub fn key(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::High => "high",
            Self::Ultra => "ultra",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Standard => "Standard",
            Self::High => "High",
            Self::Ultra => "Ultra",
        }
    }

    pub fn from_key(key: &str) -> Self {
        match key {
            "high" => Self::High,
            "ultra" => Self::Ultra,
            _ => Self::Standard,
        }
    }
}

/// Stage of the gate envelope. `Idle` is both "never started" and "released to
/// silence"; `Held` is the open-but-not-shaping state a zero release leaves
/// behind (see [`SoundfontEnvelope`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateStage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Held,
    Release,
}

/// Note-gated A/D/S/R over the player's output.
#[derive(Debug)]
pub(crate) struct GateEnvelope {
    stage: GateStage,
    level: f32,
    /// Per-sample increments over the full `0.0..=1.0` span, so a stage's time
    /// constant is independent of the level it happens to start from.
    attack_rate: f32,
    decay_rate: f32,
    release_rate: f32,
    sustain: f32,
    /// `release_ms == 0`: closing the gate holds the level instead of ramping.
    release_is_off: bool,
    bypassed: bool,
    /// `false` while the envelope has never been opened, so a player that has
    /// not received a note yet renders its (silent) output untouched rather than
    /// being multiplied by a level of 0.
    started: bool,
}

impl GateEnvelope {
    pub(crate) fn new(envelope: SoundfontEnvelope, sample_rate: i32) -> Self {
        let mut gate = Self {
            stage: GateStage::Idle,
            level: 0.0,
            attack_rate: 1.0,
            decay_rate: 1.0,
            release_rate: 1.0,
            sustain: 1.0,
            release_is_off: true,
            bypassed: true,
            started: false,
        };
        gate.configure(envelope, sample_rate);
        gate
    }

    /// Recomputes the per-sample rates. Control-thread only — the render path
    /// reads them and never divides.
    pub(crate) fn configure(&mut self, envelope: SoundfontEnvelope, sample_rate: i32) {
        let envelope = envelope.sanitized();
        let sample_rate = sample_rate.max(1) as f32;
        let rate = |ms: f32| {
            if ms <= 0.0 {
                1.0
            } else {
                (1_000.0 / (ms * sample_rate)).clamp(f32::MIN_POSITIVE, 1.0)
            }
        };
        self.attack_rate = rate(envelope.attack_ms);
        self.decay_rate = rate(envelope.decay_ms);
        // A zero release is "off", not an instant cut, so it must not become a
        // rate at all — `close` checks the time and holds instead.
        self.release_rate = rate(envelope.release_ms);
        self.sustain = envelope.sustain;
        self.bypassed = envelope.is_bypassed();
        self.release_is_off = envelope.release_ms <= 0.0;
        if self.bypassed {
            self.stage = GateStage::Idle;
            self.started = false;
            self.level = 0.0;
        }
    }

    #[cfg(test)]
    pub(crate) fn is_bypassed(&self) -> bool {
        self.bypassed
    }

    /// Called when note activity goes from none to some.
    pub(crate) fn open(&mut self) {
        if self.bypassed {
            return;
        }
        self.started = true;
        // Retrigger from wherever the envelope currently sits: attacking from a
        // still-decaying release is what keeps a fast repeated phrase free of
        // the click a jump to zero would produce.
        self.stage = GateStage::Attack;
    }

    /// Called when the last held (or pedalled) note ends.
    pub(crate) fn close(&mut self) {
        if self.bypassed || !self.started {
            return;
        }
        if self.release_is_off {
            // Leave the output open at its current level and let the SoundFont's
            // own release and the reverb tail finish.
            self.stage = GateStage::Held;
            return;
        }
        self.stage = GateStage::Release;
    }

    /// Drops the envelope back to its unstarted state — used by `reset`, where
    /// the synthesizer's voices are discarded outright.
    pub(crate) fn reset(&mut self) {
        self.stage = GateStage::Idle;
        self.level = 0.0;
        self.started = false;
    }

    #[inline]
    fn next(&mut self) -> f32 {
        match self.stage {
            GateStage::Idle => self.level = 0.0,
            GateStage::Attack => {
                self.level += self.attack_rate;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = GateStage::Decay;
                }
            }
            GateStage::Decay => {
                if self.sustain >= 1.0 {
                    self.level = 1.0;
                    self.stage = GateStage::Sustain;
                } else {
                    self.level -= self.decay_rate * (1.0 - self.sustain);
                    if self.level <= self.sustain {
                        self.level = self.sustain;
                        self.stage = GateStage::Sustain;
                    }
                }
            }
            GateStage::Sustain => self.level = self.sustain,
            GateStage::Held => {}
            GateStage::Release => {
                self.level -= self.release_rate;
                if self.level <= 0.0 {
                    self.level = 0.0;
                    self.stage = GateStage::Idle;
                }
            }
        }
        self.level
    }

    /// Applies the envelope in place. No-op while bypassed or before the first
    /// note, so both states pass the synthesizer's output through untouched.
    #[inline]
    pub(crate) fn apply(&mut self, left: &mut [f32], right: &mut [f32]) {
        if self.bypassed || !self.started {
            return;
        }
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let gain = self.next();
            *l *= gain;
            *r *= gain;
        }
    }
}

/// Windowed-sinc decimator for oversampled rendering.
///
/// Linear phase, so the delay is a constant `(taps - 1) / 2` oversampled
/// samples. The tap count is chosen as `32 * factor + 1` precisely so that
/// delay is `16` *output* samples at every quality setting — one number the
/// engine can hand to delay compensation without depending on the factor.
#[derive(Debug)]
pub(crate) struct Decimator {
    factor: usize,
    taps: Vec<f32>,
    /// The last `taps - 1` oversampled samples of the previous pass, so the
    /// filter is continuous across block boundaries.
    hist_l: Vec<f32>,
    hist_r: Vec<f32>,
    /// `hist` followed by this pass's freshly rendered oversampled samples.
    work_l: Vec<f32>,
    work_r: Vec<f32>,
    max_frames: usize,
}

/// Output-sample delay of the decimation filter at any oversampled quality.
pub const DECIMATOR_LATENCY_SAMPLES: u32 = 16;

/// Tap count per oversampling factor. See [`Decimator`] for why it is this.
fn tap_count(factor: usize) -> usize {
    32 * factor + 1
}

impl Decimator {
    /// Builds the filter for `factor` and preallocates for `max_frames` output
    /// samples per pass. Control-thread only.
    pub(crate) fn new(factor: usize, max_frames: usize) -> Self {
        let factor = factor.max(2);
        let max_frames = max_frames.max(1);
        let taps = design_lowpass(tap_count(factor), 0.454 / factor as f32);
        let tail = taps.len() - 1;
        Self {
            factor,
            taps,
            hist_l: vec![0.0; tail],
            hist_r: vec![0.0; tail],
            work_l: vec![0.0; tail + max_frames * factor],
            work_r: vec![0.0; tail + max_frames * factor],
            max_frames,
        }
    }

    /// Longest run of output samples one pass can produce. The caller loops
    /// instead of resizing when asked for more.
    #[cfg(test)]
    pub(crate) fn render_chunk_frames(&self) -> usize {
        self.max_frames
    }

    #[cfg(test)]
    pub(crate) fn factor(&self) -> usize {
        self.factor
    }

    /// Clears the filter history — used when the synthesizer's voices are reset,
    /// so a stale tail cannot leak into the next note.
    pub(crate) fn reset(&mut self) {
        self.hist_l.fill(0.0);
        self.hist_r.fill(0.0);
    }

    /// Renders `left.len()` output samples through `render_oversampled`, which
    /// is handed the two oversampled scratch slices to fill.
    pub(crate) fn process(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        mut render_oversampled: impl FnMut(&mut [f32], &mut [f32]),
    ) {
        let tail = self.taps.len() - 1;
        let frames = left.len();
        let mut start = 0usize;
        while start < frames {
            let n = (frames - start).min(self.max_frames);
            let oversampled = n * self.factor;

            self.work_l[..tail].copy_from_slice(&self.hist_l);
            self.work_r[..tail].copy_from_slice(&self.hist_r);
            render_oversampled(
                &mut self.work_l[tail..tail + oversampled],
                &mut self.work_r[tail..tail + oversampled],
            );

            for j in 0..n {
                // `base` is the newest oversampled sample of output frame `j`;
                // the filter walks backwards from it, and `base - k` bottoms out
                // at 0 exactly when `j == 0` and `k == tail`.
                let base = tail + j * self.factor;
                let mut acc_l = 0.0f32;
                let mut acc_r = 0.0f32;
                for (k, tap) in self.taps.iter().enumerate() {
                    acc_l += tap * self.work_l[base - k];
                    acc_r += tap * self.work_r[base - k];
                }
                left[start + j] = acc_l;
                right[start + j] = acc_r;
            }

            self.hist_l
                .copy_from_slice(&self.work_l[oversampled..oversampled + tail]);
            self.hist_r
                .copy_from_slice(&self.work_r[oversampled..oversampled + tail]);
            start += n;
        }
    }
}

/// Blackman-windowed sinc low-pass, normalized to unity DC gain.
/// `cutoff` is the corner frequency as a fraction of the *oversampled* rate.
fn design_lowpass(taps: usize, cutoff: f32) -> Vec<f32> {
    let taps = taps | 1; // odd, so the delay is an exact integer
    let m = (taps - 1) as f32;
    let mut h = Vec::with_capacity(taps);
    for i in 0..taps {
        let n = i as f32;
        let x = n - m / 2.0;
        let sinc = if x.abs() < 1.0e-6 {
            2.0 * cutoff
        } else {
            (2.0 * std::f32::consts::PI * cutoff * x).sin() / (std::f32::consts::PI * x)
        };
        let t = 2.0 * std::f32::consts::PI * n / m;
        let window = 0.42 - 0.5 * t.cos() + 0.08 * (2.0 * t).cos();
        h.push(sinc * window);
    }
    let sum: f32 = h.iter().sum();
    if sum.abs() > 1.0e-9 {
        for tap in &mut h {
            *tap /= sum;
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_envelope_is_bypassed() {
        assert!(SoundfontEnvelope::default().is_bypassed());
    }

    #[test]
    fn any_shaping_leaves_the_bypass_state() {
        for envelope in [
            SoundfontEnvelope {
                attack_ms: 5.0,
                ..Default::default()
            },
            SoundfontEnvelope {
                decay_ms: 5.0,
                ..Default::default()
            },
            SoundfontEnvelope {
                sustain: 0.5,
                ..Default::default()
            },
            SoundfontEnvelope {
                release_ms: 5.0,
                ..Default::default()
            },
        ] {
            assert!(!envelope.is_bypassed(), "{envelope:?}");
        }
    }

    #[test]
    fn sanitize_clamps_and_rejects_non_finite_values() {
        let envelope = SoundfontEnvelope {
            attack_ms: f32::NAN,
            decay_ms: -10.0,
            sustain: 4.0,
            release_ms: 1.0e9,
        }
        .sanitized();
        assert_eq!(envelope.attack_ms, 0.0);
        assert_eq!(envelope.decay_ms, 0.0);
        assert_eq!(envelope.sustain, 1.0);
        assert_eq!(envelope.release_ms, ENVELOPE_MAX_TIME_MS);
    }

    #[test]
    fn quality_keys_round_trip_and_unknown_degrades_to_standard() {
        for quality in SoundfontRenderQuality::ALL {
            assert_eq!(SoundfontRenderQuality::from_key(quality.key()), quality);
        }
        assert_eq!(
            SoundfontRenderQuality::from_key("insane"),
            SoundfontRenderQuality::Standard
        );
    }

    fn drive(gate: &mut GateEnvelope, frames: usize) -> Vec<f32> {
        let mut left = vec![1.0; frames];
        let mut right = vec![1.0; frames];
        gate.apply(&mut left, &mut right);
        assert_eq!(left, right, "both channels get the same gain");
        left
    }

    #[test]
    fn attack_ramps_up_over_its_configured_time() {
        // 10 ms at 1 kHz is 10 samples.
        let mut gate = GateEnvelope::new(
            SoundfontEnvelope {
                attack_ms: 10.0,
                ..Default::default()
            },
            1_000,
        );
        gate.open();
        let out = drive(&mut gate, 20);
        assert!(out[0] < 0.2, "starts near silence: {}", out[0]);
        assert!(out[4] > 0.3 && out[4] < 0.7, "mid ramp: {}", out[4]);
        assert!((out[19] - 1.0).abs() < 1.0e-5, "reaches unity: {}", out[19]);
    }

    #[test]
    fn decay_falls_to_the_sustain_level_and_holds_there() {
        let mut gate = GateEnvelope::new(
            SoundfontEnvelope {
                decay_ms: 10.0,
                sustain: 0.25,
                ..Default::default()
            },
            1_000,
        );
        gate.open();
        let out = drive(&mut gate, 40);
        assert!((out[39] - 0.25).abs() < 1.0e-5, "sustain: {}", out[39]);
    }

    #[test]
    fn release_ramps_to_silence_after_the_last_note() {
        let mut gate = GateEnvelope::new(
            SoundfontEnvelope {
                release_ms: 10.0,
                ..Default::default()
            },
            1_000,
        );
        gate.open();
        drive(&mut gate, 4);
        gate.close();
        let out = drive(&mut gate, 20);
        assert!(out[0] > 0.5, "release starts from the held level");
        assert_eq!(out[19], 0.0, "release reaches silence");
    }

    #[test]
    fn a_zero_release_holds_the_output_open_for_the_soundfont_tail() {
        let mut gate = GateEnvelope::new(
            SoundfontEnvelope {
                attack_ms: 1.0,
                ..Default::default()
            },
            1_000,
        );
        gate.open();
        drive(&mut gate, 8);
        gate.close();
        let out = drive(&mut gate, 8);
        assert!(
            out.iter().all(|gain| (*gain - 1.0).abs() < 1.0e-5),
            "a zero release must not cut the tail: {out:?}"
        );
    }

    #[test]
    fn a_bypassed_envelope_never_touches_the_signal() {
        let mut gate = GateEnvelope::new(SoundfontEnvelope::default(), 48_000);
        assert!(gate.is_bypassed());
        gate.open();
        let out = drive(&mut gate, 16);
        assert!(out.iter().all(|gain| *gain == 1.0));
    }

    #[test]
    fn an_unstarted_envelope_never_touches_the_signal() {
        // Configured but no note yet — the output must not be multiplied by the
        // envelope's initial level of 0.
        let mut gate = GateEnvelope::new(
            SoundfontEnvelope {
                attack_ms: 50.0,
                ..Default::default()
            },
            1_000,
        );
        let out = drive(&mut gate, 16);
        assert!(out.iter().all(|gain| *gain == 1.0));
    }

    #[test]
    fn retrigger_resumes_from_the_current_level_instead_of_clicking() {
        let mut gate = GateEnvelope::new(
            SoundfontEnvelope {
                attack_ms: 10.0,
                release_ms: 100.0,
                ..Default::default()
            },
            1_000,
        );
        gate.open();
        drive(&mut gate, 20); // up to unity
        gate.close();
        drive(&mut gate, 10); // partway down the release
        gate.open();
        let out = drive(&mut gate, 1);
        assert!(
            out[0] > 0.5,
            "retrigger must continue from the released level, got {}",
            out[0]
        );
    }

    #[test]
    fn decimator_passes_dc_at_unity() {
        for factor in [2usize, 4] {
            let mut decimator = Decimator::new(factor, 64);
            let mut left = vec![0.0; 64];
            let mut right = vec![0.0; 64];
            // Two passes: the first is dominated by the zeroed history, the
            // second is the filter's steady state.
            for _ in 0..2 {
                decimator.process(&mut left, &mut right, |l, r| {
                    l.fill(1.0);
                    r.fill(1.0);
                });
            }
            assert!(
                (left[32] - 1.0).abs() < 1.0e-4,
                "factor {factor} DC gain {}",
                left[32]
            );
        }
    }

    #[test]
    fn decimator_delay_is_sixteen_output_samples_at_every_factor() {
        for factor in [2usize, 4] {
            let taps = tap_count(factor);
            assert_eq!((taps - 1) / 2 / factor, DECIMATOR_LATENCY_SAMPLES as usize);
            assert_eq!((taps - 1) / 2 % factor, 0, "delay must be a whole frame");
        }
    }

    #[test]
    fn decimator_rejects_content_above_the_output_nyquist() {
        // Nyquist of the oversampled rate — the worst case the decimation
        // filter exists to remove.
        let factor = 2usize;
        let mut decimator = Decimator::new(factor, 128);
        let mut left = vec![0.0; 128];
        let mut right = vec![0.0; 128];
        let mut phase = 0usize;
        for _ in 0..3 {
            decimator.process(&mut left, &mut right, |l, r| {
                for i in 0..l.len() {
                    let sample = if (phase + i) % 2 == 0 { 1.0 } else { -1.0 };
                    l[i] = sample;
                    r[i] = sample;
                }
                phase += l.len();
            });
        }
        let peak = left.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
        assert!(peak < 0.01, "nyquist tone should be rejected, peak {peak}");
    }

    #[test]
    fn decimator_chunks_a_buffer_longer_than_its_preallocation() {
        // The audio path must never grow a buffer, so an oversized request is
        // processed as repeated passes and must still be continuous.
        let mut decimator = Decimator::new(2, 16);
        let mut left = vec![0.0; 100];
        let mut right = vec![0.0; 100];
        decimator.process(&mut left, &mut right, |l, r| {
            l.fill(1.0);
            r.fill(1.0);
        });
        assert!((left[99] - 1.0).abs() < 1.0e-4, "tail sample {}", left[99]);
        assert_eq!(decimator.factor(), 2);
        assert_eq!(decimator.render_chunk_frames(), 16);
    }
}
