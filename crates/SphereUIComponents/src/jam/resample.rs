//! Sample-rate conversion on the jam branch only.
//!
//! **The project rate never follows the jam.** A 192 kHz session stays a
//! 192 kHz session while a guitarist streams 48 kHz PCM into it; the conversion
//! happens on the way in and on the way out, and nothing about the project
//! changes. Making it the other way round would let a network peer redefine a
//! recording session's resolution, which is not a trade anybody would accept.
//!
//! The converter is `rubato`, which the workspace already carries — a
//! band-limited sinc resampler rather than the linear interpolation that would
//! be quick to write here and audibly wrong on anything with high content.
//!
//! Drift is handled by moving the ratio, not by dropping samples. Two machines
//! that both call themselves 48 kHz are typically tens of parts per million
//! apart, which is a sample every few seconds — enough to accumulate into a
//! click if it is ever "corrected" by discarding one.

use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

/// Ratio bounds. Anything outside this is a bug or a hostile stream
/// description, not a rate two audio devices could really be running at.
const MIN_RATIO: f64 = 1.0 / 16.0;
const MAX_RATIO: f64 = 16.0;

/// How far the ratio may be pulled to absorb clock drift, as a fraction.
///
/// A hundred parts per million is far beyond any real crystal, so clamping here
/// bounds the damage from a bad drift estimate without limiting a genuine one.
const MAX_DRIFT: f64 = 100e-6;

/// The chunk size handed to the resampler. Fixed because `SincFixedIn` wants a
/// fixed input length; the caller's blocks are re-chunked to it.
const CHUNK: usize = 256;

/// A drift-aware, band-limited rate converter for any channel count.
///
/// Interleaved in, interleaved out, because that is what both the jam wire
/// format and the engine's rings use; `rubato` works in planar buffers, so the
/// deinterleave and re-interleave happen here, into buffers allocated once.
///
/// The channel count is fixed at construction. A multitrack stream is one
/// converter over all sixteen channels rather than eight stereo ones, so every
/// channel is resampled against the same ratio and the same drift correction —
/// eight converters nudged independently would slowly smear the phase
/// relationships that make a multitrack take worth sending as one stream.
pub struct JamResampler {
    inner: Option<SincFixedIn<f32>>,
    channels: usize,
    source_rate: u32,
    target_rate: u32,
    /// The nominal ratio, before drift.
    base_ratio: f64,
    /// Chunks of input waiting for a full `CHUNK`.
    pending: Vec<f32>,
    planar_in: Vec<Vec<f32>>,
    planar_out: Vec<Vec<f32>>,
    /// Ratio correction currently applied, as a fraction.
    drift: f64,
}

impl std::fmt::Debug for JamResampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JamResampler")
            .field("source_rate", &self.source_rate)
            .field("target_rate", &self.target_rate)
            .field("drift_ppm", &(self.drift * 1e6))
            .field("passthrough", &self.inner.is_none())
            .finish()
    }
}

impl JamResampler {
    /// Build a converter from `source_rate` to `target_rate`.
    ///
    /// Equal rates produce a pass-through that copies rather than filters:
    /// running a sinc kernel over audio that does not need it would add
    /// latency and ringing for nothing, and equal rates are the common case.
    pub fn new(source_rate: u32, target_rate: u32) -> Self {
        Self::with_channels(source_rate, target_rate, 2)
    }

    /// Build a converter for `channels` interleaved channels.
    pub fn with_channels(source_rate: u32, target_rate: u32, channels: usize) -> Self {
        let channels = channels.max(1);
        let base_ratio = if source_rate == 0 {
            1.0
        } else {
            target_rate as f64 / source_rate as f64
        };
        let inner = if source_rate == target_rate
            || source_rate == 0
            || target_rate == 0
            || !(MIN_RATIO..=MAX_RATIO).contains(&base_ratio)
        {
            None
        } else {
            let params = SincInterpolationParameters {
                // 128 taps at a 0.95 cutoff is transparent for musical
                // material; the cost is a fraction of a millisecond per block
                // on a thread that is not the audio callback.
                sinc_len: 128,
                f_cutoff: 0.95,
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: 128,
                window: WindowFunction::BlackmanHarris2,
            };
            SincFixedIn::<f32>::new(base_ratio, 2.0, params, CHUNK, channels).ok()
        };

        Self {
            inner,
            channels,
            source_rate,
            target_rate,
            base_ratio,
            pending: Vec::with_capacity(CHUNK * channels * 4),
            // `vec![Vec::with_capacity(n); n]` clones the first vector and
            // loses its capacity on the copy, which would put an allocation in
            // the first conversion of every stream.
            planar_in: (0..channels).map(|_| Vec::with_capacity(CHUNK)).collect(),
            planar_out: (0..channels)
                .map(|_| Vec::with_capacity(CHUNK * 2))
                .collect(),
            drift: 0.0,
        }
    }

    /// Channels this converter was built for.
    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn source_rate(&self) -> u32 {
        self.source_rate
    }

    pub fn target_rate(&self) -> u32 {
        self.target_rate
    }

    /// Whether this converter is a pass-through.
    pub fn is_passthrough(&self) -> bool {
        self.inner.is_none()
    }

    /// Apply a measured clock drift, in parts per million.
    ///
    /// Positive means the source clock runs fast, so slightly fewer output
    /// samples are produced per input sample. A pass-through ignores it: two
    /// devices at the same nominal rate still drift, but correcting that needs
    /// a real converter, and quietly turning one on would change the sound of a
    /// session mid-flight.
    pub fn set_drift_ppm(&mut self, drift_ppm: f64) {
        if self.inner.is_none() {
            return;
        }
        let drift = (drift_ppm / 1e6).clamp(-MAX_DRIFT, MAX_DRIFT);
        if (drift - self.drift).abs() < 1e-9 {
            return;
        }
        self.drift = drift;
        if let Some(inner) = self.inner.as_mut() {
            // `1 - drift`: a fast source delivers more input per second, so the
            // ratio has to shrink for the output to stay on the target clock.
            let _ = inner.set_resample_ratio(self.base_ratio * (1.0 - drift), true);
        }
    }

    pub fn drift_ppm(&self) -> f64 {
        self.drift * 1e6
    }

    /// Convert one block of interleaved audio, appending into `out`.
    ///
    /// Input that does not fill a whole chunk is held for the next call, which
    /// is what makes a stream of small network packets come out as continuous
    /// audio rather than as a chunk boundary every packet.
    pub fn process(&mut self, interleaved: &[f32], out: &mut Vec<f32>) {
        out.clear();
        let Some(inner) = self.inner.as_mut() else {
            out.extend_from_slice(interleaved);
            return;
        };
        self.pending.extend_from_slice(interleaved);

        let frame_stride = self.channels;
        while self.pending.len() >= CHUNK * frame_stride {
            for plane in self.planar_in.iter_mut() {
                plane.clear();
            }
            for frame in 0..CHUNK {
                for (channel, plane) in self.planar_in.iter_mut().enumerate() {
                    plane.push(self.pending[frame * frame_stride + channel]);
                }
            }
            self.pending.drain(..CHUNK * frame_stride);

            // `process_into_buffer` needs the output vectors sized to the
            // maximum the resampler can produce; they are reused across calls,
            // so this only grows once.
            let needed = inner.output_frames_max();
            for plane in self.planar_out.iter_mut() {
                if plane.len() < needed {
                    plane.resize(needed, 0.0);
                }
            }
            let Ok((_, written)) =
                inner.process_into_buffer(&self.planar_in, &mut self.planar_out, None)
            else {
                // A failed conversion drops this chunk rather than emitting
                // garbage. It cannot happen with a fixed input length, but
                // silence is the right failure for audio.
                continue;
            };
            for frame in 0..written {
                for plane in self.planar_out.iter().take(frame_stride) {
                    out.push(plane[frame]);
                }
            }
        }
    }

    /// Frames held back waiting for a full chunk. Part of the jam branch's
    /// latency, and reported as such rather than hidden.
    pub fn pending_frames(&self) -> usize {
        self.pending.len() / self.channels.max(1)
    }

    /// The algorithmic delay this converter adds, in output samples.
    ///
    /// Reported so it can be sent as `codec_delay_samples` on a published
    /// stream: a receiver that is told the number can remove it, and one that
    /// is not would place every take a few milliseconds late.
    pub fn delay_samples(&self) -> i64 {
        match self.inner.as_ref() {
            // Half the kernel, converted to the output rate. `rubato` centres
            // its sinc window, so the group delay is symmetric.
            Some(_) => ((128 / 2) as f64 * self.base_ratio).round() as i64,
            None => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interleave(frames: usize, sample: impl Fn(usize) -> f32) -> Vec<f32> {
        (0..frames).flat_map(|i| [sample(i), sample(i)]).collect()
    }

    #[test]
    fn equal_rates_are_a_pass_through_and_change_nothing() {
        let mut resampler = JamResampler::new(48_000, 48_000);
        assert!(resampler.is_passthrough());
        assert_eq!(resampler.delay_samples(), 0);

        let input = interleave(64, |i| (i as f32) / 64.0);
        let mut out = Vec::new();
        resampler.process(&input, &mut out);
        assert_eq!(out, input);
    }

    #[test]
    fn upsampling_produces_roughly_the_expected_number_of_frames() {
        let mut resampler = JamResampler::new(48_000, 96_000);
        assert!(!resampler.is_passthrough());

        // Feed a second's worth in chunks, as packets would arrive.
        let mut produced = 0usize;
        let mut out = Vec::new();
        for block in 0..(48_000 / 256) {
            let input = interleave(256, |i| ((block * 256 + i) as f32 * 0.01).sin() * 0.5);
            resampler.process(&input, &mut out);
            produced += out.len() / 2;
        }
        // Within one chunk of the ideal 96 000; the tail is still in the
        // resampler's own delay line.
        assert!(
            (96_000i64 - produced as i64).abs() < 1024,
            "produced {produced} frames"
        );
    }

    #[test]
    fn downsampling_produces_roughly_the_expected_number_of_frames() {
        let mut resampler = JamResampler::new(96_000, 48_000);
        let mut produced = 0usize;
        let mut out = Vec::new();
        for _ in 0..(96_000 / 256) {
            let input = interleave(256, |i| (i as f32 * 0.001).sin() * 0.25);
            resampler.process(&input, &mut out);
            produced += out.len() / 2;
        }
        assert!(
            (48_000i64 - produced as i64).abs() < 1024,
            "produced {produced} frames"
        );
    }

    #[test]
    fn a_partial_block_is_held_rather_than_padded_with_silence() {
        let mut resampler = JamResampler::new(48_000, 96_000);
        let mut out = Vec::new();
        // Less than one chunk: nothing can come out yet, and inventing
        // anything would put a gap in the middle of a continuous stream.
        resampler.process(&interleave(64, |_| 0.5), &mut out);
        assert!(out.is_empty());
        assert_eq!(resampler.pending_frames(), 64);

        resampler.process(&interleave(256, |_| 0.5), &mut out);
        assert!(!out.is_empty());
    }

    #[test]
    fn a_sine_survives_a_round_trip_through_two_conversions() {
        // 48k -> 96k -> 48k. The signal that comes back is the signal that went
        // in, which is the whole claim a resampler makes.
        let mut up = JamResampler::new(48_000, 96_000);
        let mut down = JamResampler::new(96_000, 48_000);

        let mut round_tripped = Vec::new();
        let mut intermediate = Vec::new();
        let mut final_block = Vec::new();
        for block in 0..64 {
            let input = interleave(256, |i| {
                // 1 kHz at 48 kHz, comfortably inside both passbands.
                let n = (block * 256 + i) as f32;
                (n * std::f32::consts::TAU * 1_000.0 / 48_000.0).sin() * 0.5
            });
            up.process(&input, &mut intermediate);
            down.process(&intermediate, &mut final_block);
            round_tripped.extend_from_slice(&final_block);
        }

        // Skip the filter delay at the start, then check the amplitude is
        // intact rather than attenuated or ringing.
        let tail: Vec<f32> = round_tripped
            .iter()
            .skip(4_000)
            .step_by(2)
            .copied()
            .collect();
        assert!(!tail.is_empty());
        let peak = tail.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
        assert!(
            (peak - 0.5).abs() < 0.05,
            "round-tripped peak was {peak}, expected about 0.5"
        );
    }

    #[test]
    fn drift_correction_is_clamped_to_something_a_clock_could_really_do() {
        let mut resampler = JamResampler::new(48_000, 96_000);
        resampler.set_drift_ppm(5_000.0);
        assert!(
            resampler.drift_ppm() <= 100.0 + 1e-6,
            "drift was {}",
            resampler.drift_ppm()
        );
        resampler.set_drift_ppm(-5_000.0);
        assert!(resampler.drift_ppm() >= -100.0 - 1e-6);
    }

    #[test]
    fn a_pass_through_reports_no_drift_rather_than_pretending_to_correct_it() {
        let mut resampler = JamResampler::new(48_000, 48_000);
        resampler.set_drift_ppm(20.0);
        assert_eq!(resampler.drift_ppm(), 0.0);
    }

    #[test]
    fn an_impossible_rate_pair_falls_back_to_a_pass_through() {
        // A hundredfold ratio is not two audio devices; it is a bad stream
        // description, and filtering it would be worse than copying it.
        assert!(JamResampler::new(48_000, 4_800_000).is_passthrough());
        assert!(JamResampler::new(0, 48_000).is_passthrough());
    }

    #[test]
    fn the_reported_delay_is_nonzero_only_when_a_filter_is_running() {
        assert_eq!(JamResampler::new(48_000, 48_000).delay_samples(), 0);
        assert!(JamResampler::new(48_000, 96_000).delay_samples() > 0);
    }
}
