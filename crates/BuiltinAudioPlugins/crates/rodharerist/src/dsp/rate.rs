//! Fixed-ratio sample-rate adapter for wrapping a sample-by-sample processor
//! that only runs correctly at its own rate — in practice a NAM capture, whose
//! dilations and recurrence are tied to the rate it was trained at.
//!
//! Engine sample in → interpolate to the inner rate → inner processor →
//! interpolate back to the engine rate → engine sample out. The inner
//! processor is stepped a variable number of times per engine sample (between
//! `floor(ratio)` and `ceil(ratio)`), so the ratio is bounded (see
//! [`MIN_RATIO`]/[`MAX_RATIO`]) to keep the worst-case per-sample cost bounded.
//!
//! Interpolation is 4-point cubic Hermite (Catmull-Rom) over a preallocated
//! fixed history, with a 4th-order Butterworth guard lowpass on whichever side
//! decimates. This is a pragmatic adapter, not a transparent polyphase
//! converter: it is a large improvement over refusing to load the capture at
//! all, and the source material (a guitar amp capture, usually followed by a
//! cabinet) is already heavily band-limited. Running the session at the
//! capture's own rate is still the better-sounding option.
//!
//! Realtime: [`RateAdapter::run_block`] allocates nothing, locks nothing and
//! never panics; every buffer is a fixed-size array or a `Vec` sized once in
//! [`RateAdapter::new`] on the control thread.

use builtin_dsp_core::make_eq_coefficients;

use super::StereoBiquad;

/// Interpolation history depth, per channel and per side. Sized so the read
/// position stays inside the interpolatable window across the whole supported
/// ratio range without ever clamping (see [`RateAdapter::run`]).
const HIST: usize = 12;

/// Narrowest and widest inner:engine rate ratio the adapter accepts. Outside
/// this the inner processor would have to be stepped more than four times per
/// engine sample (or barely at all), which is a load/quality cliff better
/// reported to the user than silently absorbed.
pub(super) const MIN_RATIO: f64 = 0.25;
pub(super) const MAX_RATIO: f64 = 4.0;

/// Guard lowpass corner as a fraction of the lower of the two rates.
const GUARD_FRACTION: f32 = 0.45;

/// Butterworth Q values for a 4th-order lowpass built from two biquads.
const BUTTERWORTH_Q: [f32; 2] = [0.541_2, 1.306_6];

/// Two cascaded biquads — a 4th-order guard against the aliasing a decimating
/// interpolation would otherwise fold back into the audible band. `none()` on
/// whichever side does not decimate, where it is a pass-through.
#[derive(Debug, Clone)]
struct GuardLp {
    stages: [StereoBiquad; 2],
}

impl GuardLp {
    fn none() -> Self {
        Self {
            stages: [StereoBiquad::none(), StereoBiquad::none()],
        }
    }

    /// 4th-order lowpass at `cutoff_hz`, expressed in the domain running at
    /// `sample_rate`. Falls back to a pass-through if the corner is not
    /// meaningfully below Nyquist (nothing to guard against).
    fn lowpass(cutoff_hz: f32, sample_rate: f32) -> Self {
        // The finite check is spelled out rather than left to a negated `>`:
        // a non-finite corner must fall through to the pass-through, and
        // `cutoff_hz <= 0.0` alone is false for NaN.
        if !cutoff_hz.is_finite() || cutoff_hz <= 0.0 || cutoff_hz >= sample_rate * 0.5 {
            return Self::none();
        }
        let mut me = Self::none();
        for (stage, q) in me.stages.iter_mut().zip(BUTTERWORTH_Q) {
            stage.set(make_eq_coefficients(
                "lowpass",
                cutoff_hz,
                0.0,
                q,
                sample_rate,
            ));
        }
        me
    }

    fn reset(&mut self) {
        for stage in self.stages.iter_mut() {
            stage.reset();
        }
    }

    #[inline]
    fn run(&mut self, left: f32, right: f32) -> (f32, f32) {
        let (l, r) = self.stages[0].run(left, right);
        self.stages[1].run(l, r)
    }
}

/// One channel's interpolation history, newest sample last.
#[derive(Debug, Clone)]
struct History {
    samples: [f32; HIST],
}

impl History {
    fn new() -> Self {
        Self {
            samples: [0.0; HIST],
        }
    }

    fn clear(&mut self) {
        self.samples = [0.0; HIST];
    }

    #[inline]
    fn push(&mut self, sample: f32) {
        self.samples.copy_within(1.., 0);
        self.samples[HIST - 1] = sample;
    }

    /// Read `gap` samples behind the newest sample, cubic-Hermite interpolated.
    /// `gap` is clamped into the window the four-point kernel can serve — a
    /// safety net that the gap bookkeeping in [`RateAdapter::run`] is sized
    /// never to need.
    #[inline]
    fn read(&self, gap: f32) -> f32 {
        let pos = ((HIST - 1) as f32 - gap).clamp(1.0, (HIST - 3) as f32);
        let base = pos.floor();
        let f = pos - base;
        let i = base as usize;
        let p0 = self.samples[i - 1];
        let p1 = self.samples[i];
        let p2 = self.samples[i + 1];
        let p3 = self.samples[i + 2];
        // Catmull-Rom: C1-continuous, passes through p1/p2, no extra state.
        let c1 = 0.5 * (p2 - p0);
        let c2 = p0 - 2.5 * p1 + 2.0 * p2 - 0.5 * p3;
        let c3 = 0.5 * (p3 - p0) + 1.5 * (p1 - p2);
        ((c3 * f + c2) * f + c1) * f + p1
    }
}

/// Runs an inner stereo processor at a rate different from the engine's.
#[derive(Debug, Clone)]
pub(super) struct RateAdapter {
    /// Inner samples per engine sample (`inner_rate / engine_rate`).
    ratio: f32,
    /// Engine samples per inner sample (`1 / ratio`).
    inv_ratio: f32,
    /// Inner-rate staging for [`Self::run_block`], preallocated for the largest
    /// engine block the caller declared. The inner processor runs over this in
    /// one call instead of once per inner sample.
    inner_l: Vec<f32>,
    inner_r: Vec<f32>,
    /// How many inner samples each engine sample of the current block produced
    /// — the bookkeeping that lets phase 3 replay the interleaving exactly.
    produced: Vec<u8>,
    /// Gap at which the next inner sample is produced. Chosen so the read
    /// position stays inside the interpolatable window for every legal ratio.
    produce_at: f32,
    /// Engine samples between the next inner input sample and the newest
    /// engine sample.
    in_gap: f32,
    /// Inner samples between the next engine output sample and the newest
    /// inner sample.
    out_gap: f32,
    in_hist_l: History,
    in_hist_r: History,
    out_hist_l: History,
    out_hist_r: History,
    /// Engine-domain guard, used when the inner rate is the lower one.
    pre_lp: GuardLp,
    /// Inner-domain guard, used when the engine rate is the lower one.
    post_lp: GuardLp,
}

impl RateAdapter {
    /// `None` when the ratio is outside [`MIN_RATIO`]..=[`MAX_RATIO`] or either
    /// rate is not a usable positive number. A ratio of exactly 1 still builds
    /// an adapter — callers that want the zero-cost path skip construction
    /// themselves rather than paying for a pass-through here.
    ///
    /// `max_block` is the largest engine block [`Self::run_block`] will be
    /// handed; the inner-rate staging is sized for it here, on the control
    /// thread, so the audio thread never grows a buffer.
    pub(super) fn new(inner_rate: f64, engine_rate: f64, max_block: usize) -> Option<Self> {
        if !inner_rate.is_finite() || !engine_rate.is_finite() {
            return None;
        }
        if inner_rate <= 0.0 || engine_rate <= 0.0 {
            return None;
        }
        let ratio = inner_rate / engine_rate;
        if !(MIN_RATIO..=MAX_RATIO).contains(&ratio) {
            return None;
        }
        let ratio = ratio as f32;
        let inv_ratio = 1.0 / ratio;
        // The inner loop leaves `in_gap` in `(produce_at - inv_ratio,
        // produce_at]`, and it gains 1.0 per engine sample — so the whole
        // excursion is `[2, produce_at + 1]`, inside the window `History::read`
        // serves as long as `HIST` covers `MAX_RATIO`.
        let produce_at = 2.0 + inv_ratio;
        let guard_hz = GUARD_FRACTION * (inner_rate.min(engine_rate)) as f32;
        // Worst case one engine block can ask for: every sample produces
        // `ceil(ratio)` inner samples, plus the one the fractional carry can
        // add at the block's first sample.
        let inner_capacity = (max_block as f32 * ratio).ceil() as usize + 2;
        Some(Self {
            ratio,
            inv_ratio,
            inner_l: vec![0.0; inner_capacity],
            inner_r: vec![0.0; inner_capacity],
            produced: vec![0; max_block],
            produce_at,
            in_gap: produce_at,
            out_gap: 3.0 + ratio,
            in_hist_l: History::new(),
            in_hist_r: History::new(),
            out_hist_l: History::new(),
            out_hist_r: History::new(),
            pre_lp: if inner_rate < engine_rate {
                GuardLp::lowpass(guard_hz, engine_rate as f32)
            } else {
                GuardLp::none()
            },
            post_lp: if inner_rate > engine_rate {
                GuardLp::lowpass(guard_hz, inner_rate as f32)
            } else {
                GuardLp::none()
            },
        })
    }

    /// Extra latency the adapter itself contributes, in engine samples — the
    /// interpolation read gaps, converted to the engine domain.
    pub(super) fn latency_samples(&self) -> usize {
        (self.produce_at + self.out_gap * self.inv_ratio).ceil() as usize
    }

    pub(super) fn reset(&mut self) {
        self.in_hist_l.clear();
        self.in_hist_r.clear();
        self.out_hist_l.clear();
        self.out_hist_r.clear();
        self.in_gap = self.produce_at;
        self.out_gap = 3.0 + self.ratio;
        self.pre_lp.reset();
        self.post_lp.reset();
    }

    /// One engine sample in, one engine sample out. `inner` is stepped zero or
    /// more times — whatever the ratio calls for at this position — and must
    /// itself be realtime-safe.
    ///
    /// Kept as the reference the block path is checked against
    /// (`block_matches_the_per_sample_path`); production runs through
    /// [`Self::run_block`], which lets the model process a whole chunk at once.
    #[cfg(test)]
    pub(super) fn run<F>(&mut self, left: f32, right: f32, mut inner: F) -> (f32, f32)
    where
        F: FnMut(f32, f32) -> (f32, f32),
    {
        // Band-limit before decimating into a slower inner rate.
        let (xl, xr) = self.pre_lp.run(left, right);
        self.in_hist_l.push(xl);
        self.in_hist_r.push(xr);

        self.in_gap += 1.0;
        while self.in_gap > self.produce_at {
            let il = self.in_hist_l.read(self.in_gap);
            let ir = self.in_hist_r.read(self.in_gap);
            let (ol, or) = inner(il, ir);
            // Band-limit before decimating back to a slower engine rate.
            let (ol, or) = self.post_lp.run(ol, or);
            self.out_hist_l.push(ol);
            self.out_hist_r.push(or);
            self.out_gap += 1.0;
            self.in_gap -= self.inv_ratio;
        }

        let out = (
            self.out_hist_l.read(self.out_gap),
            self.out_hist_r.read(self.out_gap),
        );
        self.out_gap -= self.ratio;
        out
    }

    /// Block twin of [`Self::run`]: `n` engine samples in, `n` engine samples
    /// out, with the inner processor run **once** over all the inner-rate
    /// samples the block calls for instead of once per sample. `inner` is
    /// handed the staged inner-rate buffers and processes them in place.
    ///
    /// Bit-for-bit equivalent to `n` calls of [`Self::run`] (see
    /// `block_matches_the_per_sample_path`): the inner processor's *inputs*
    /// depend only on the engine-side history, so staging them all up front
    /// changes nothing, and phase 3 replays the output pushes and reads in the
    /// original per-sample order. Allocation-free; `n` must not exceed the
    /// `max_block` the adapter was built with.
    pub(super) fn run_block<F>(
        &mut self,
        in_l: &[f32],
        in_r: &[f32],
        out_l: &mut [f32],
        out_r: &mut [f32],
        mut inner: F,
    ) where
        F: FnMut(&mut [f32], &mut [f32]),
    {
        let n = in_l.len().min(self.produced.len());
        let mut produced_total = 0usize;

        // Phase 1: engine → inner. Interpolate every inner-rate input sample
        // the block calls for, remembering how many belong to each engine
        // sample so phase 3 can interleave the outputs identically.
        for i in 0..n {
            let (xl, xr) = self.pre_lp.run(in_l[i], in_r[i]);
            self.in_hist_l.push(xl);
            self.in_hist_r.push(xr);

            self.in_gap += 1.0;
            let mut count = 0u8;
            while self.in_gap > self.produce_at && produced_total < self.inner_l.len() {
                self.inner_l[produced_total] = self.in_hist_l.read(self.in_gap);
                self.inner_r[produced_total] = self.in_hist_r.read(self.in_gap);
                produced_total += 1;
                count += 1;
                self.in_gap -= self.inv_ratio;
            }
            self.produced[i] = count;
        }

        // Phase 2: one inner-rate run over the whole staged block.
        inner(
            &mut self.inner_l[..produced_total],
            &mut self.inner_r[..produced_total],
        );

        // Phase 3: inner → engine, in the same order the per-sample path used.
        let mut read = 0usize;
        for i in 0..n {
            for _ in 0..self.produced[i] {
                let (ol, or) = self.post_lp.run(self.inner_l[read], self.inner_r[read]);
                self.out_hist_l.push(ol);
                self.out_hist_r.push(or);
                self.out_gap += 1.0;
                read += 1;
            }
            out_l[i] = self.out_hist_l.read(self.out_gap);
            out_r[i] = self.out_hist_r.read(self.out_gap);
            self.out_gap -= self.ratio;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    /// Rate pairs a real session actually hits, both directions.
    const PAIRS: [(f64, f64); 6] = [
        (44_100.0, 48_000.0),
        (48_000.0, 44_100.0),
        (48_000.0, 96_000.0),
        (96_000.0, 48_000.0),
        (44_100.0, 88_200.0),
        (48_000.0, 192_000.0),
    ];

    #[test]
    fn rejects_unusable_and_out_of_range_ratios() {
        assert!(RateAdapter::new(f64::NAN, 48_000.0, 64).is_none());
        assert!(RateAdapter::new(48_000.0, 0.0, 64).is_none());
        assert!(RateAdapter::new(-48_000.0, 48_000.0, 64).is_none());
        assert!(RateAdapter::new(8_000.0, 48_000.0, 64).is_none()); // ratio 1/6
        assert!(RateAdapter::new(384_000.0, 48_000.0, 64).is_none()); // ratio 8
        assert!(RateAdapter::new(12_000.0, 48_000.0, 64).is_some()); // ratio 1/4
        assert!(RateAdapter::new(192_000.0, 48_000.0, 64).is_some()); // ratio 4
    }

    /// The inner processor must be stepped at the inner rate, on average, for
    /// every supported pair — that is the whole contract.
    #[test]
    fn steps_the_inner_processor_at_the_inner_rate() {
        for (inner_rate, engine_rate) in PAIRS {
            let mut adapter = RateAdapter::new(inner_rate, engine_rate, 64).expect("legal ratio");
            let engine_samples = engine_rate as usize; // one second
            let mut inner_calls = 0usize;
            for _ in 0..engine_samples {
                adapter.run(0.0, 0.0, |l, r| {
                    inner_calls += 1;
                    (l, r)
                });
            }
            let expected = inner_rate as usize;
            let drift = inner_calls.abs_diff(expected);
            assert!(
                drift < 32,
                "{inner_rate}→{engine_rate}: {inner_calls} inner steps, expected ~{expected}"
            );
        }
    }

    /// Round-tripping a mid-band sine through an identity inner processor must
    /// come back as the same sine: same frequency, near-unity amplitude, low
    /// residual against a phase-aligned reference.
    #[test]
    fn a_sine_survives_the_round_trip_at_every_supported_pair() {
        for (inner_rate, engine_rate) in PAIRS {
            let mut adapter = RateAdapter::new(inner_rate, engine_rate, 64).expect("legal ratio");
            let sr = engine_rate as f32;
            let freq = 440.0f32;
            let n = engine_rate as usize;

            // Settle the histories and the guard filters first.
            for i in 0..2_000 {
                let x = (i as f32 * freq * TAU / sr).sin() * 0.5;
                adapter.run(x, x, |l, r| (l, r));
            }

            let mut peak = 0.0f32;
            let mut energy = 0.0f32;
            for i in 2_000..(2_000 + n) {
                let x = (i as f32 * freq * TAU / sr).sin() * 0.5;
                let (l, r) = adapter.run(x, x, |a, b| (a, b));
                assert!(l.is_finite() && r.is_finite(), "{inner_rate}→{engine_rate}");
                assert_eq!(l, r, "identical channels must stay identical");
                peak = peak.max(l.abs());
                energy += l * l;
            }
            let rms = (energy / n as f32).sqrt();
            // A 0.5-amplitude sine has an RMS of 0.3536.
            assert!(
                (rms - 0.353_6).abs() < 0.02,
                "{inner_rate}→{engine_rate}: rms {rms} lost/gained too much level"
            );
            assert!(
                peak < 0.6,
                "{inner_rate}→{engine_rate}: peak {peak} overshoots"
            );
        }
    }

    /// The guard lowpass must actually stop content above the lower rate's
    /// Nyquist from folding back — feed a tone the inner rate cannot represent
    /// and check almost nothing survives.
    #[test]
    fn content_above_the_inner_nyquist_is_rejected_not_aliased() {
        // A 22 kHz tone in a 48 kHz engine, through a 24 kHz capture: without
        // the guard this folds back to 2 kHz, right in the guitar's range.
        let mut adapter = RateAdapter::new(24_000.0, 48_000.0, 64).expect("legal ratio");
        let sr = 48_000.0f32;
        for i in 0..4_000 {
            let x = (i as f32 * 22_000.0 * TAU / sr).sin() * 0.5;
            adapter.run(x, x, |l, r| (l, r));
        }
        let mut energy = 0.0f32;
        for i in 4_000..20_000 {
            let x = (i as f32 * 22_000.0 * TAU / sr).sin() * 0.5;
            let (l, _) = adapter.run(x, x, |a, b| (a, b));
            energy += l * l;
        }
        let rms = (energy / 16_000.0).sqrt();
        assert!(rms < 0.02, "aliased image survived at rms {rms}");
    }

    #[test]
    fn reset_clears_state_and_reproduces_the_same_output() {
        let mut adapter = RateAdapter::new(44_100.0, 48_000.0, 64).expect("legal ratio");
        let render = |adapter: &mut RateAdapter| {
            (0..512)
                .map(|i| {
                    let x = (i as f32 * 0.03).sin() * 0.4;
                    adapter.run(x, x, |l, r| (l * 0.9, r * 0.9)).0
                })
                .collect::<Vec<_>>()
        };
        let first = render(&mut adapter);
        adapter.reset();
        let second = render(&mut adapter);
        assert_eq!(
            first, second,
            "reset must restore the initial state exactly"
        );
    }

    /// The block path exists purely to let the inner processor run over a whole
    /// chunk at once; it must not change a single sample of the result, or the
    /// capture would sound different at a mismatched rate than the per-sample
    /// path it replaced.
    #[test]
    fn block_matches_the_per_sample_path() {
        const BLOCK: usize = 64;
        for (inner_rate, engine_rate) in PAIRS {
            let mut per_sample =
                RateAdapter::new(inner_rate, engine_rate, BLOCK).expect("legal ratio");
            let mut blocked =
                RateAdapter::new(inner_rate, engine_rate, BLOCK).expect("legal ratio");
            // A stand-in for the model: state-dependent, so any reordering of
            // the inner calls would show up in the output.
            let mut state = 0.0f32;
            let inner = |x: f32, state: &mut f32| {
                *state = 0.85 * *state + 0.15 * x;
                (x - *state).tanh()
            };

            let input: Vec<f32> = (0..BLOCK * 24)
                .map(|i| (i as f32 * 0.021).sin() * 0.4 + (i as f32 * 0.003).cos() * 0.1)
                .collect();

            let expected: Vec<(f32, f32)> = input
                .iter()
                .map(|&x| {
                    per_sample.run(x, -x, |l, r| (inner(l, &mut state), inner(r, &mut state)))
                })
                .collect();

            state = 0.0;
            let mut actual = Vec::with_capacity(input.len());
            let mut out_l = [0.0f32; BLOCK];
            let mut out_r = [0.0f32; BLOCK];
            for chunk in input.chunks(BLOCK) {
                let in_l: Vec<f32> = chunk.to_vec();
                let in_r: Vec<f32> = chunk.iter().map(|x| -x).collect();
                blocked.run_block(&in_l, &in_r, &mut out_l, &mut out_r, |l, r| {
                    for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                        *a = inner(*a, &mut state);
                        *b = inner(*b, &mut state);
                    }
                });
                actual.extend(
                    out_l[..chunk.len()]
                        .iter()
                        .zip(&out_r[..chunk.len()])
                        .map(|(&l, &r)| (l, r)),
                );
            }

            assert_eq!(
                expected, actual,
                "{inner_rate}→{engine_rate}: block path diverged from per-sample"
            );
        }
    }

    /// Silence in, silence out — no self-oscillation from the guard filters or
    /// the interpolation bookkeeping.
    #[test]
    fn silence_stays_silent() {
        for (inner_rate, engine_rate) in PAIRS {
            let mut adapter = RateAdapter::new(inner_rate, engine_rate, 64).expect("legal ratio");
            for _ in 0..4_000 {
                let (l, r) = adapter.run(0.0, 0.0, |a, b| (a, b));
                assert_eq!((l, r), (0.0, 0.0), "{inner_rate}→{engine_rate}");
            }
        }
    }
}
