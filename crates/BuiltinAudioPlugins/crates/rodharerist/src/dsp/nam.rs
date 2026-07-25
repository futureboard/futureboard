//! Neural Amp Modeler (NAM) capture engine — a distinct processor from the
//! classic modeled [`super::amp::Amp`], selectable as an alternative engine for
//! the same Tone/Amp slot (see [`super::ToneEngineKind`]).
//!
//! Loading a `.nam` file parses JSON and builds a neural network — real
//! allocation, definitely not audio-thread work. [`prepare_nam_runtime`] does
//! that on the control thread and hands back a [`PreparedNamRuntime`] the
//! caller boxes and pushes into [`NamCapture::submit`]. The audio thread only
//! ever adopts it at a block boundary ([`NamCapture::begin_block`]); the
//! previous runtime is cross-faded out over a short window, then handed back
//! to the control thread ([`NamCapture::poll_garbage`]) to actually drop —
//! never inside [`NamCapture::process`].

use std::sync::Arc;

use builtin_dsp_core::make_eq_coefficients;
use nam_rs::{Model, NamModel};

use super::StereoBiquad;
use super::handoff::HandoffCell;
use super::rate::{MAX_RATIO, MIN_RATIO, RateAdapter};

/// Target integrated loudness (LUFS) captures are normalized to when loudness
/// normalization is enabled, matching the reference NAM plugin's convention.
const TARGET_LUFS: f32 = -18.0;

/// A `.nam` file's declared sample rate counts as matching the engine's within
/// this many Hz. Inside the tolerance the capture runs natively; outside it a
/// [`RateAdapter`] runs the model at its own rate (nam-rs does not resample,
/// and a mismatched model silently mis-runs its dilations/recurrence).
const SAMPLE_RATE_TOLERANCE_HZ: f64 = 0.5;

/// Roughly how long a runtime swap crossfades for, in milliseconds.
const SWAP_FADE_MS: f32 = 8.0;

/// A `.nam` failed to parse/build, or its sample rate is too far from the
/// engine's for the rate adapter to bridge.
#[derive(Debug)]
pub enum NamLoadError {
    Parse(nam_rs::Error),
    SampleRateMismatch { expected: f64, engine: f64 },
}

impl std::fmt::Display for NamLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NamLoadError::Parse(e) => write!(f, "NAM capture failed to load: {e}"),
            NamLoadError::SampleRateMismatch { expected, engine } => write!(
                f,
                "NAM capture expects {expected} Hz but the engine runs at {engine} Hz — \
                 more than {MAX_RATIO}x apart, too far to adapt (supported: \
                 {MIN_RATIO}x..{MAX_RATIO}x the engine rate)"
            ),
        }
    }
}

impl std::error::Error for NamLoadError {}

/// Info handed back to the host/UI after a successful load — enough to show
/// the capture's name, warn about startup latency, and offer "Bypass Cab" when
/// the capture already models a full rig (amp + cab + mic). Serde-serializable
/// so it can travel over the plugin-host IPC as-is.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NamCaptureInfo {
    pub name: String,
    pub full_rig: bool,
    pub receptive_field: usize,
    pub sample_rate: f64,
}

/// A fully-built, ready-to-run capture. Boxed and moved into [`NamCapture`]'s
/// hand-off cell; built entirely on the control thread by
/// [`prepare_nam_runtime`].
pub struct PreparedNamRuntime {
    name: String,
    model_l: Model,
    /// `None` for a mono capture: the single model's output is mirrored to
    /// both channels rather than running two redundant inferences.
    model_r: Option<Model>,
    sample_rate: f64,
    receptive_field: usize,
    /// Precomputed linear gain to bring the capture to [`TARGET_LUFS`], or
    /// `1.0` if the file carries no loudness metadata. Computed once here so
    /// the hot path is a single multiply, not a per-sample dB calculation.
    loudness_gain: f32,
    full_rig: bool,
    /// Present only when the capture's rate differs from the engine's — it
    /// runs the model at the capture's own rate from inside the engine's
    /// stream. `None` is the native, zero-overhead path.
    adapter: Option<RateAdapter>,
    /// The capture's total latency in **engine** samples: its receptive field
    /// converted out of the model's rate, plus whatever the adapter's own
    /// interpolation contributes. Equals `receptive_field` when running
    /// natively.
    latency_samples: usize,
}

impl std::fmt::Debug for PreparedNamRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedNamRuntime")
            .field("name", &self.name)
            .field("sample_rate", &self.sample_rate)
            .field("receptive_field", &self.receptive_field)
            .field("full_rig", &self.full_rig)
            .field("resampled", &self.adapter.is_some())
            .finish_non_exhaustive()
    }
}

/// Parse and build a `.nam` capture off the audio thread. `stereo` selects
/// whether a second, independent model is built for the right channel (true
/// stereo width) or the left model's output is mirrored (mono bus / cheaper).
///
/// A capture whose declared rate differs from the engine's is *not* rejected:
/// it gets a [`RateAdapter`] that runs the model at its own rate from inside
/// the engine's stream, so a 44.1 kHz capture is usable in a 48 kHz session.
/// Only a ratio outside the adapter's supported range is refused — see
/// [`NamLoadError::SampleRateMismatch`]. Running the session at the capture's
/// own rate still sounds better; the adapter is a convenience, not a wash.
pub fn prepare_nam_runtime(
    json: &str,
    name: String,
    engine_sample_rate: f64,
    stereo: bool,
    full_rig: bool,
) -> Result<PreparedNamRuntime, NamLoadError> {
    let nam_model = NamModel::from_json_str(json).map_err(NamLoadError::Parse)?;
    let expected = nam_model.expected_sample_rate();
    let adapter = if (expected - engine_sample_rate).abs() > SAMPLE_RATE_TOLERANCE_HZ {
        Some(RateAdapter::new(expected, engine_sample_rate).ok_or(
            NamLoadError::SampleRateMismatch {
                expected,
                engine: engine_sample_rate,
            },
        )?)
    } else {
        None
    };

    let model_l = Model::from_nam(&nam_model).map_err(NamLoadError::Parse)?;
    let model_r = if stereo {
        Some(Model::from_nam(&nam_model).map_err(NamLoadError::Parse)?)
    } else {
        None
    };
    let receptive_field = model_l.receptive_field();
    let loudness_gain = nam_model
        .loudness()
        .map(|l| 10f32.powf((TARGET_LUFS - l) / 20.0).clamp(0.05, 20.0))
        .unwrap_or(1.0);
    // The receptive field is counted in the *model's* samples; the host's
    // delay compensation works in engine samples.
    let latency_samples = match adapter.as_ref() {
        Some(a) => a.inner_to_engine_samples(receptive_field) + a.latency_samples(),
        None => receptive_field,
    };

    Ok(PreparedNamRuntime {
        name,
        model_l,
        model_r,
        sample_rate: expected,
        receptive_field,
        loudness_gain,
        full_rig,
        adapter,
        latency_samples,
    })
}

impl PreparedNamRuntime {
    pub fn info(&self) -> NamCaptureInfo {
        NamCaptureInfo {
            name: self.name.clone(),
            full_rig: self.full_rig,
            receptive_field: self.receptive_field,
            sample_rate: self.sample_rate,
        }
    }

    #[inline]
    fn process(&mut self, left: f32, right: f32, loudness_on: bool) -> (f32, f32) {
        // Split the borrow so the inference closure and the adapter can be
        // held mutably at the same time.
        let Self {
            model_l,
            model_r,
            adapter,
            loudness_gain,
            ..
        } = self;
        let gain = if loudness_on { *loudness_gain } else { 1.0 };
        let mut infer = |l: f32, r: f32| {
            let out_l = model_l.process_sample(l) * gain;
            let out_r = match model_r.as_mut() {
                Some(model_r) => model_r.process_sample(r) * gain,
                None => out_l,
            };
            (out_l, out_r)
        };
        match adapter.as_mut() {
            // Rate-adapted: the model still sees its own rate, one inference
            // per *model* sample, not per engine sample.
            Some(adapter) => adapter.run(left, right, infer),
            None => infer(left, right),
        }
    }

    fn reset(&mut self) {
        self.model_l.reset();
        if let Some(model_r) = self.model_r.as_mut() {
            model_r.reset();
        }
        if let Some(adapter) = self.adapter.as_mut() {
            adapter.reset();
        }
    }
}

/// The two lock-free hand-off cells between the control side and the audio
/// side, shareable via [`NamLoader`] so a control thread in another ownership
/// domain (e.g. the plugin-host IPC thread, which must never touch the `Dsp`
/// the audio producer owns) can still submit captures and drain garbage.
pub struct NamChannel {
    /// Control thread → audio thread: a freshly built runtime awaiting adoption.
    pending: HandoffCell<PreparedNamRuntime>,
    /// Audio thread → control thread: a retired runtime awaiting disposal.
    retired: HandoffCell<PreparedNamRuntime>,
}

/// Cloneable control-side handle to a [`NamCapture`]'s hand-off cells.
///
/// Thread contract: exactly **one** control thread may use the loader at a
/// time (the cells are single-producer/single-consumer per direction) — the
/// same discipline `Dsp::load_nam_capture_json` always required, now
/// structurally separated from `&mut Dsp` so it works across the
/// `UnsafeCell` ownership boundary in the plugin-host process.
#[derive(Clone)]
pub struct NamLoader {
    channel: Arc<NamChannel>,
}

impl NamLoader {
    /// Push a freshly-built runtime for the audio thread to adopt at the next
    /// block boundary. A not-yet-adopted runtime already waiting is dropped
    /// here (safe: the audio thread never touched it).
    pub fn submit(&self, runtime: Box<PreparedNamRuntime>) {
        if let Some(bumped) = self.channel.pending.put(runtime) {
            drop(bumped);
        }
    }

    /// Drop any runtime the audio thread has retired. Call periodically and
    /// before each [`Self::submit`] as an opportunistic sweep.
    pub fn collect_garbage(&self) {
        if let Some(dead) = self.channel.retired.take() {
            drop(dead);
        }
    }

    /// Parse, build and submit a `.nam` capture in one call (control thread —
    /// parsing allocates and can take a while for large models).
    pub fn load_json(
        &self,
        json: &str,
        name: impl Into<String>,
        engine_sample_rate: f64,
        stereo: bool,
        full_rig: bool,
    ) -> Result<NamCaptureInfo, NamLoadError> {
        let prepared =
            prepare_nam_runtime(json, name.into(), engine_sample_rate, stereo, full_rig)?;
        let info = prepared.info();
        self.collect_garbage();
        self.submit(Box::new(prepared));
        Ok(info)
    }
}

/// The audio-thread-resident NAM engine: a preallocated DC blocker, live trim/
/// mix/loudness knobs, and the lock-free hand-off machinery that lets the
/// control thread swap in a freshly-built [`PreparedNamRuntime`] without ever
/// blocking or allocating on the audio thread.
pub(super) struct NamCapture {
    active: Option<Box<PreparedNamRuntime>>,
    /// The just-replaced runtime, still running so [`Self::process`] can
    /// crossfade away from it instead of cutting over with a click.
    fading_out: Option<Box<PreparedNamRuntime>>,
    /// A retiree that didn't fit in `retired` (control thread hasn't drained
    /// it yet). Held here — never dropped on the audio thread — until a later
    /// block finds `retired` empty again.
    retire_overflow: Option<Box<PreparedNamRuntime>>,

    /// The shared hand-off cells; [`Self::loader`] clones this out.
    channel: Arc<NamChannel>,

    sample_rate: f32,
    dc_hpf: StereoBiquad,

    input_trim: f32,
    output_trim: f32,
    loudness_norm_on: bool,
    mix: f32,

    /// 0 = fully `fading_out`, 1 = fully `active`. Sits at 1.0 when no fade
    /// is in progress.
    fade: f32,
    fade_step: f32,
}

impl NamCapture {
    pub(super) fn new(sample_rate: f32) -> Self {
        let mut me = Self {
            active: None,
            fading_out: None,
            retire_overflow: None,
            channel: Arc::new(NamChannel {
                pending: HandoffCell::new(),
                retired: HandoffCell::new(),
            }),
            sample_rate: sample_rate.max(1.0),
            dc_hpf: StereoBiquad::none(),
            input_trim: 1.0,
            output_trim: 1.0,
            loudness_norm_on: true,
            mix: 1.0,
            fade: 1.0,
            fade_step: 1.0,
        };
        me.recompute_sample_rate_derived();
        me
    }

    pub(super) fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate.max(1.0);
        self.recompute_sample_rate_derived();
    }

    fn recompute_sample_rate_derived(&mut self) {
        self.dc_hpf.set(make_eq_coefficients(
            "highpass",
            20.0,
            0.0,
            0.707,
            self.sample_rate,
        ));
        let fade_len = (self.sample_rate * (SWAP_FADE_MS / 1_000.0)).max(1.0);
        self.fade_step = 1.0 / fade_len;
    }

    pub(super) fn reset(&mut self) {
        self.dc_hpf.reset();
        if let Some(rt) = self.active.as_mut() {
            rt.reset();
        }
        if let Some(rt) = self.fading_out.as_mut() {
            rt.reset();
        }
    }

    /// Live knob update (control thread only): trims in dB, mix in 0..100 %.
    pub(super) fn configure(
        &mut self,
        input_trim_db: f32,
        output_trim_db: f32,
        mix_pct: f32,
        loudness_norm_on: bool,
    ) {
        self.input_trim = db_to_linear(input_trim_db);
        self.output_trim = db_to_linear(output_trim_db);
        self.mix = (mix_pct / 100.0).clamp(0.0, 1.0);
        self.loudness_norm_on = loudness_norm_on;
    }

    /// Clone out a control-side loader handle for this capture's cells.
    pub(super) fn loader(&self) -> NamLoader {
        NamLoader {
            channel: Arc::clone(&self.channel),
        }
    }

    /// Control thread: push a freshly-built runtime for the audio thread to
    /// adopt at the next block boundary. Any not-yet-adopted runtime already
    /// waiting is dropped here (safe: the audio thread never touched it).
    pub(super) fn submit(&self, runtime: Box<PreparedNamRuntime>) {
        if let Some(bumped) = self.channel.pending.put(runtime) {
            drop(bumped);
        }
    }

    /// Control thread: drop any runtime the audio thread has retired. Call
    /// periodically (e.g. an idle/UI timer); also safe to call before
    /// [`Self::submit`] as an opportunistic sweep.
    pub(super) fn poll_garbage(&mut self) {
        if let Some(dead) = self.channel.retired.take() {
            drop(dead);
        }
    }

    /// Info about the currently active capture, if one is loaded.
    pub(super) fn active_info(&self) -> Option<NamCaptureInfo> {
        self.active.as_ref().map(|rt| rt.info())
    }

    /// Latency contributed by the active capture, in **engine** samples (0 if
    /// none loaded, or an LSTM capture, which has no warmup). Already accounts
    /// for a rate-adapted capture, whose receptive field is counted in its own
    /// rate's samples.
    pub(super) fn latency_samples(&self) -> usize {
        self.active
            .as_ref()
            .map(|rt| rt.latency_samples)
            .unwrap_or(0)
    }

    /// Audio thread: adopt a pending runtime and retire a finished fade.
    /// Called once per audio block, never per sample — this is the only place
    /// the swap happens.
    pub(super) fn begin_block(&mut self) {
        // Drain a previous overflow now that a block boundary has passed.
        if let Some(carry) = self.retire_overflow.take() {
            if let Some(bounced) = self.channel.retired.put(carry) {
                self.retire_overflow = Some(bounced);
            }
        }

        // Only start a new swap once any in-progress fade has fully resolved,
        // so at most one runtime is ever "in flight" toward retirement.
        if self.fading_out.is_none() {
            if let Some(new_rt) = self.channel.pending.take() {
                if let Some(old) = self.active.replace(new_rt) {
                    self.fading_out = Some(old);
                    self.fade = 0.0;
                }
            }
        }

        if self.fade >= 1.0 {
            if let Some(done) = self.fading_out.take() {
                if let Some(bounced) = self.channel.retired.put(done) {
                    self.retire_overflow = Some(bounced);
                }
            }
        }
    }

    /// Audio thread hot path: input trim → model(s) → DC block → loudness →
    /// output trim → wet/dry mix. Crossfades against `fading_out` if a swap is
    /// in progress. No allocation, no locks, no swap logic (see
    /// [`Self::begin_block`]).
    #[inline]
    pub(super) fn process(&mut self, left: f32, right: f32) -> (f32, f32) {
        let dry = (left, right);
        let xin = (left * self.input_trim, right * self.input_trim);

        let new_out = match self.active.as_mut() {
            Some(rt) => rt.process(xin.0, xin.1, self.loudness_norm_on),
            None => xin,
        };

        let wet = if let Some(old_rt) = self.fading_out.as_mut() {
            let old_out = old_rt.process(xin.0, xin.1, self.loudness_norm_on);
            self.fade = (self.fade + self.fade_step).min(1.0);
            (
                old_out.0 * (1.0 - self.fade) + new_out.0 * self.fade,
                old_out.1 * (1.0 - self.fade) + new_out.1 * self.fade,
            )
        } else {
            new_out
        };

        let (mut ol, mut or) = self.dc_hpf.run(wet.0, wet.1);
        ol *= self.output_trim;
        or *= self.output_trim;

        let m = self.mix;
        (dry.0 * (1.0 - m) + ol * m, dry.1 * (1.0 - m) + or * m)
    }
}

#[inline]
fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TINY_WAVENET_48K: &str = r#"{
        "version": "0.5.4", "architecture": "WaveNet",
        "config": { "layers": [{
            "input_size": 1, "condition_size": 1, "channels": 1, "head_size": 1,
            "kernel_size": 1, "dilations": [1], "activation": "ReLU",
            "gated": false, "head_bias": false
        }], "head": null, "head_scale": 1.0 },
        "weights": [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
        "sample_rate": 48000.0
    }"#;

    /// A rate mismatch inside the adapter's range builds a runtime that runs
    /// the model at its own rate; the reported latency is in engine samples,
    /// so it must scale with the ratio rather than echo the raw field.
    #[test]
    fn adapts_a_mismatched_rate_instead_of_rejecting_it() {
        let native = prepare_nam_runtime(TINY_WAVENET_48K, "t".into(), 48_000.0, false, false)
            .expect("native rate loads");
        assert!(native.adapter.is_none(), "matching rate must not resample");
        assert_eq!(native.latency_samples, native.receptive_field);

        let adapted = prepare_nam_runtime(TINY_WAVENET_48K, "t".into(), 44_100.0, false, false)
            .expect("a 48 kHz capture must adapt into a 44.1 kHz engine");
        assert!(adapted.adapter.is_some(), "mismatch must build an adapter");
        assert_eq!(
            adapted.sample_rate, 48_000.0,
            "info keeps the capture's rate"
        );
        // 48 kHz model samples are fewer 44.1 kHz engine samples, plus the
        // adapter's own interpolation delay.
        assert!(adapted.latency_samples >= adapted.receptive_field * 44_100 / 48_000);

        let mut cap = NamCapture::new(44_100.0);
        cap.configure(0.0, 0.0, 100.0, false);
        cap.submit(Box::new(adapted));
        cap.begin_block();
        for n in 0..4_000 {
            let x = (n as f32 * 0.02).sin() * 0.3;
            let (l, r) = cap.process(x, -x);
            assert!(l.is_finite() && r.is_finite(), "resampled capture blew up");
        }
        assert!(cap.latency_samples() > 0 || cap.active_info().is_some());
    }

    #[test]
    fn refuses_a_rate_ratio_beyond_the_adapter_range() {
        // 48 kHz capture in a 6 kHz engine — an 8x ratio.
        let err = prepare_nam_runtime(TINY_WAVENET_48K, "t".into(), 6_000.0, false, false)
            .expect_err("an 8x rate ratio must be refused");
        assert!(matches!(err, NamLoadError::SampleRateMismatch { .. }));
    }

    #[test]
    fn loads_and_processes_at_matching_rate() {
        let prepared = prepare_nam_runtime(TINY_WAVENET_48K, "t".into(), 48_000.0, false, false)
            .expect("matching rate must load");
        assert_eq!(prepared.model_r.is_some(), false);

        let mut cap = NamCapture::new(48_000.0);
        cap.configure(0.0, 0.0, 100.0, false);
        cap.submit(Box::new(prepared));
        cap.begin_block();
        for _ in 0..64 {
            let (l, r) = cap.process(0.1, -0.1);
            assert!(l.is_finite() && r.is_finite());
        }
        assert!(cap.active_info().is_some());
    }

    #[test]
    fn stereo_capture_builds_two_independent_models() {
        let prepared = prepare_nam_runtime(TINY_WAVENET_48K, "t".into(), 48_000.0, true, true)
            .expect("matching rate must load");
        assert!(prepared.model_r.is_some());
        assert!(prepared.full_rig);
    }

    #[test]
    fn swap_crossfades_without_dropping_on_audio_thread() {
        let mut cap = NamCapture::new(48_000.0);
        cap.configure(0.0, 0.0, 100.0, false);

        let first =
            prepare_nam_runtime(TINY_WAVENET_48K, "a".into(), 48_000.0, false, false).unwrap();
        cap.submit(Box::new(first));
        cap.begin_block();
        for _ in 0..8 {
            cap.process(0.2, 0.2);
        }

        let second =
            prepare_nam_runtime(TINY_WAVENET_48K, "b".into(), 48_000.0, false, false).unwrap();
        cap.submit(Box::new(second));
        cap.begin_block(); // adopts `second`, starts fading `first` out
        assert!(cap.fading_out.is_some());

        // Run well past the fade window; every sample must stay finite, and
        // the fade must fully resolve without the audio thread ever calling
        // `retired.take()` (only `begin_block` — called here — does).
        for _ in 0..2_000 {
            let (l, r) = cap.process(0.2, -0.2);
            assert!(l.is_finite() && r.is_finite());
            cap.begin_block();
        }
        assert!(cap.fading_out.is_none(), "fade must resolve and retire");

        // Control thread drains what the audio thread retired.
        cap.poll_garbage();
    }

    /// The loader handle must reach the same cells as the capture itself:
    /// submit through the loader, adopt via `begin_block`, retire, and drain
    /// garbage through the loader — the cross-process (host IPC thread) flow.
    #[test]
    fn loader_handle_round_trips_submit_and_garbage() {
        let mut cap = NamCapture::new(48_000.0);
        cap.configure(0.0, 0.0, 100.0, false);
        let loader = cap.loader();

        let info = loader
            .load_json(TINY_WAVENET_48K, "via-loader", 48_000.0, false, false)
            .expect("load through loader");
        assert_eq!(info.name, "via-loader");

        cap.begin_block();
        assert_eq!(cap.active_info().map(|i| i.name), Some("via-loader".into()));

        // Swap in a second capture and let the fade retire the first.
        loader
            .load_json(TINY_WAVENET_48K, "second", 48_000.0, false, false)
            .expect("second load");
        cap.begin_block();
        for _ in 0..2_000 {
            let (l, r) = cap.process(0.2, -0.2);
            assert!(l.is_finite() && r.is_finite());
            cap.begin_block();
        }
        assert!(cap.fading_out.is_none());
        loader.collect_garbage(); // drains the retired first capture
        assert_eq!(cap.active_info().map(|i| i.name), Some("second".into()));
    }

    #[test]
    fn no_capture_loaded_is_pass_through_at_unity() {
        let mut cap = NamCapture::new(48_000.0);
        cap.configure(0.0, 0.0, 100.0, false);
        let (l, r) = cap.process(0.3, -0.3);
        // DC blocker still runs, so allow a small tolerance rather than exact equality.
        assert!((l - 0.3).abs() < 1.0e-3);
        assert!((r + 0.3).abs() < 1.0e-3);
    }
}
