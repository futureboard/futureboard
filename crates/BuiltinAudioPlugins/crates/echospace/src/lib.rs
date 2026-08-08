//! EchoSpace — stereo / ping-pong delay with filtered feedback.
//!
//! Phase 2 (medium). Feedback tone controls use the MIT/Apache [`biquad`]
//! crate. Delay lines are preallocated ring buffers (no realtime heap growth).

use biquad::{Biquad, DirectForm1};
use builtin_dsp_core::{
    ParamDescriptor, PluginCategory, PluginDescriptor, StereoEffect, clamp, db_to_linear,
    make_eq_biquad, mix,
};
use serde::{Deserialize, Serialize};

pub mod ipc;
pub mod ui;

/// Editor-facing parameter id table, re-exported at the crate root so the host
/// resolves ids the same way for every built-in (`<plugin>::ui_param_index`).
pub use ipc::{UI_PARAM_IDS, ui_param_id, ui_param_index};

pub const PLUGIN_ID: &str = "futureboard.echospace";

/// Longest delay either line can be set to. The rings are sized for this at the
/// current sample rate, so a time edit only moves a read offset.
pub const MAX_DELAY_MS: f32 = 4_000.0;

/// Feedback ceiling. The delay line is a feedback loop: at exactly 1.0 it never
/// decays, and with the cross-feed sum below it can only be held short of unity
/// by the saturator — which the user is allowed to turn off.
const MAX_FEEDBACK: f32 = 0.9995;

/// Tempo window a synced delay time is derived from. The transport publishes a
/// real tempo every block, but a region read before the engine's first publish —
/// or a project with a nonsense tempo — must not turn into an infinite or
/// zero-length delay, so the conversion clamps rather than trusts.
pub const MIN_TEMPO_BPM: f32 = 20.0;
pub const MAX_TEMPO_BPM: f32 = 999.0;

/// Tempo assumed until the host publishes a transport block.
pub const DEFAULT_TEMPO_BPM: f32 = 120.0;

/// Note divisions a synced line can lock to, shortest first. Straight, dotted
/// and triplet forms are interleaved in duration order so stepping the control
/// sweeps the musical range monotonically instead of jumping between families.
///
/// Index *is* the wire value for `divisionL` / `divisionR`, so this table is
/// part of the persisted contract — append only, and only at the end.
pub const DIVISION_LABELS: [&str; DIVISION_COUNT] = [
    "1/32T", "1/32", "1/16T", "1/32.", "1/16", "1/8T", "1/16.", "1/8", "1/4T", "1/8.", "1/4",
    "1/2T", "1/4.", "1/2", "1/1T", "1/2.", "1/1", "1/1.",
];

/// Quarter notes each entry of [`DIVISION_LABELS`] spans. A dotted note is
/// 1.5x its straight form, a triplet 2/3 of it.
pub const DIVISION_BEATS: [f32; DIVISION_COUNT] = [
    0.083_333_336,
    0.125,
    0.166_666_67,
    0.1875,
    0.25,
    0.333_333_34,
    0.375,
    0.5,
    0.666_666_7,
    0.75,
    1.0,
    1.333_333_4,
    1.5,
    2.0,
    2.666_666_7,
    3.0,
    4.0,
    6.0,
];

pub const DIVISION_COUNT: usize = 18;

/// Highest valid `divisionL` / `divisionR` wire value.
pub const MAX_DIVISION_WIRE: f32 = 17.0;

/// Default division per side, chosen so switching Sync on from the factory
/// settings lands on the same dotted-eighth / quarter pattern the free times
/// describe at 120 BPM.
pub const DEFAULT_DIVISION_L: u8 = 9;
pub const DEFAULT_DIVISION_R: u8 = 10;

/// Delay time one division spans at `tempo_bpm`, already inside the line's
/// reachable range. Allocation-free and total: an out-of-table index saturates
/// and the tempo is clamped, so this can be called from the producer thread.
#[inline]
pub fn division_ms(division: u8, tempo_bpm: f32) -> f32 {
    let beats = DIVISION_BEATS[(division as usize).min(DIVISION_COUNT - 1)];
    let bpm = if tempo_bpm.is_finite() {
        clamp(tempo_bpm, MIN_TEMPO_BPM, MAX_TEMPO_BPM)
    } else {
        DEFAULT_TEMPO_BPM
    };
    clamp(beats * 60_000.0 / bpm, 1.0, MAX_DELAY_MS)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DelayMode {
    Stereo,
    PingPong,
    Mono,
}

impl DelayMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stereo => "stereo",
            Self::PingPong => "pingpong",
            Self::Mono => "mono",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "stereo" => Some(Self::Stereo),
            "pingpong" | "ping-pong" => Some(Self::PingPong),
            "mono" => Some(Self::Mono),
            _ => None,
        }
    }

    pub const fn to_wire(self) -> f32 {
        match self {
            Self::Stereo => 0.0,
            Self::PingPong => 1.0,
            Self::Mono => 2.0,
        }
    }

    pub fn from_wire(value: f32) -> Self {
        match value.round() as i32 {
            0 => Self::Stereo,
            2 => Self::Mono,
            _ => Self::PingPong,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub power: bool,
    pub mode: DelayMode,
    pub time_ms_l: f32,
    pub time_ms_r: f32,
    pub feedback: f32,
    pub cross_feedback: f32,
    pub low_cut_hz: f32,
    pub high_cut_hz: f32,
    pub saturation: f32,
    pub mix: f32,
    pub output_db: f32,
    pub freeze: bool,
    /// Derive both delay times from the host tempo and the divisions below,
    /// instead of from `time_ms_l` / `time_ms_r`.
    ///
    /// The free times are kept untouched while this is on, so switching back
    /// returns the line to exactly where it was rather than to a default.
    #[serde(default)]
    pub sync: bool,
    /// Note division per side while `sync` is on; an index into
    /// [`DIVISION_BEATS`].
    #[serde(default = "default_division_l")]
    pub division_l: u8,
    #[serde(default = "default_division_r")]
    pub division_r: u8,
    /// Both sides move together: an edit to either time or division is applied
    /// to the other as well.
    #[serde(default)]
    pub link: bool,
}

/// Serde fallbacks for the tempo-sync fields, which projects written before
/// EchoSpace had them do not carry. Without these the whole blob would be
/// rejected and the insert would silently open at factory settings.
fn default_division_l() -> u8 {
    DEFAULT_DIVISION_L
}

fn default_division_r() -> u8 {
    DEFAULT_DIVISION_R
}

pub fn default_params() -> Params {
    Params {
        power: true,
        mode: DelayMode::PingPong,
        time_ms_l: 375.0,
        time_ms_r: 563.0,
        feedback: 34.0,
        cross_feedback: 65.0,
        low_cut_hz: 180.0,
        high_cut_hz: 9_000.0,
        saturation: 8.0,
        mix: 20.0,
        output_db: 0.0,
        freeze: false,
        sync: false,
        division_l: DEFAULT_DIVISION_L,
        division_r: DEFAULT_DIVISION_R,
        link: false,
    }
}

impl Params {
    /// Delay time the left line actually runs at: the tempo-derived division
    /// while synced, the free time otherwise.
    #[inline]
    pub fn effective_time_ms_l(&self, tempo_bpm: f32) -> f32 {
        if self.sync {
            division_ms(self.division_l, tempo_bpm)
        } else {
            self.time_ms_l
        }
    }

    #[inline]
    pub fn effective_time_ms_r(&self, tempo_bpm: f32) -> f32 {
        if self.sync {
            division_ms(self.division_r, tempo_bpm)
        } else {
            self.time_ms_r
        }
    }
}

pub fn descriptor() -> PluginDescriptor {
    PluginDescriptor {
        id: PLUGIN_ID,
        name: "EchoSpace",
        vendor: "Futureboard",
        category: PluginCategory::Effect,
        version: env!("CARGO_PKG_VERSION"),
        params: &[
            ParamDescriptor {
                id: "power",
                name: "Power",
                default_value: 1.0,
                min: 0.0,
                max: 1.0,
                unit: "bool",
            },
            ParamDescriptor {
                id: "mode",
                name: "Mode",
                default_value: 1.0,
                min: 0.0,
                max: 2.0,
                unit: "enum",
            },
            ParamDescriptor {
                id: "timeMsL",
                name: "Time L",
                default_value: 375.0,
                min: 1.0,
                max: MAX_DELAY_MS,
                unit: "ms",
            },
            ParamDescriptor {
                id: "timeMsR",
                name: "Time R",
                default_value: 563.0,
                min: 1.0,
                max: MAX_DELAY_MS,
                unit: "ms",
            },
            ParamDescriptor {
                id: "feedback",
                name: "Feedback",
                default_value: 34.0,
                min: 0.0,
                max: 98.0,
                unit: "%",
            },
            ParamDescriptor {
                id: "crossFeedback",
                name: "Cross",
                default_value: 65.0,
                min: 0.0,
                max: 100.0,
                unit: "%",
            },
            ParamDescriptor {
                id: "lowCutHz",
                name: "Low Cut",
                default_value: 180.0,
                min: 20.0,
                max: 2_000.0,
                unit: "Hz",
            },
            ParamDescriptor {
                id: "highCutHz",
                name: "High Cut",
                default_value: 9_000.0,
                min: 1_000.0,
                max: 20_000.0,
                unit: "Hz",
            },
            ParamDescriptor {
                id: "saturation",
                name: "Saturation",
                default_value: 8.0,
                min: 0.0,
                max: 100.0,
                unit: "%",
            },
            ParamDescriptor {
                id: "mix",
                name: "Mix",
                default_value: 20.0,
                min: 0.0,
                max: 100.0,
                unit: "%",
            },
            ParamDescriptor {
                id: "outputDb",
                name: "Output",
                default_value: 0.0,
                min: -24.0,
                max: 12.0,
                unit: "dB",
            },
            ParamDescriptor {
                id: "freeze",
                name: "Freeze",
                default_value: 0.0,
                min: 0.0,
                max: 1.0,
                unit: "bool",
            },
            ParamDescriptor {
                id: "sync",
                name: "Tempo Sync",
                default_value: 0.0,
                min: 0.0,
                max: 1.0,
                unit: "bool",
            },
            ParamDescriptor {
                id: "divisionL",
                name: "Division L",
                default_value: DEFAULT_DIVISION_L as f32,
                min: 0.0,
                max: MAX_DIVISION_WIRE,
                unit: "note",
            },
            ParamDescriptor {
                id: "divisionR",
                name: "Division R",
                default_value: DEFAULT_DIVISION_R as f32,
                min: 0.0,
                max: MAX_DIVISION_WIRE,
                unit: "note",
            },
            ParamDescriptor {
                id: "link",
                name: "Link L/R",
                default_value: 0.0,
                min: 0.0,
                max: 1.0,
                unit: "bool",
            },
        ],
    }
}

#[derive(Debug, Clone)]
struct DelayLine {
    buffer: Vec<f32>,
    write: usize,
}

impl DelayLine {
    fn new(capacity: usize) -> Self {
        Self {
            buffer: vec![0.0; capacity.max(1)],
            write: 0,
        }
    }

    fn clear(&mut self) {
        self.buffer.fill(0.0);
        self.write = 0;
    }

    #[inline]
    fn read(&self, delay_samples: usize) -> f32 {
        let len = self.buffer.len();
        let idx = (self.write + len - (delay_samples.min(len - 1))) % len;
        self.buffer[idx]
    }

    #[inline]
    fn write_sample(&mut self, sample: f32) {
        self.buffer[self.write] = sample;
        self.write += 1;
        if self.write >= self.buffer.len() {
            self.write = 0;
        }
    }
}

#[derive(Debug, Clone)]
pub struct Dsp {
    sample_rate: f32,
    params: Params,
    /// Latest transport tempo, republished by the host each block. Only the
    /// synced delay times read it; the free times ignore it entirely.
    tempo_bpm: f32,
    delay_l: DelayLine,
    delay_r: DelayLine,
    delay_samples_l: usize,
    delay_samples_r: usize,
    hpf_l: Option<DirectForm1<f32>>,
    hpf_r: Option<DirectForm1<f32>>,
    lpf_l: Option<DirectForm1<f32>>,
    lpf_r: Option<DirectForm1<f32>>,
    output_gain: f32,
}

impl Dsp {
    /// Ring length for the longest reachable delay at `sample_rate`.
    ///
    /// Derived from the rate rather than a fixed sample count: a constant sized
    /// for 48 kHz silently caps `timeMs` at 2 s once the engine runs at 96 kHz,
    /// so the editor would offer 4 s that the DSP could not deliver.
    fn ring_capacity(sample_rate: f32) -> usize {
        ((sample_rate * MAX_DELAY_MS * 0.001).ceil() as usize).max(2) + 2
    }

    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let capacity = Self::ring_capacity(sr);
        let mut dsp = Self {
            sample_rate: sr,
            params: default_params(),
            tempo_bpm: DEFAULT_TEMPO_BPM,
            delay_l: DelayLine::new(capacity),
            delay_r: DelayLine::new(capacity),
            delay_samples_l: 1,
            delay_samples_r: 1,
            hpf_l: None,
            hpf_r: None,
            lpf_l: None,
            lpf_r: None,
            output_gain: 1.0,
        };
        dsp.apply_params();
        dsp
    }

    pub fn params(&self) -> &Params {
        &self.params
    }

    pub fn set_params(&mut self, params: Params) {
        self.params = params;
        ipc::sanitize_params(&mut self.params);
        self.apply_params();
    }

    /// Publish the block's transport tempo. Called from the host producer
    /// before `process_stereo`, so it must stay allocation-free: it is a
    /// compare plus, at most, the same two integer conversions a time edit
    /// already does, and only while a synced line is actually reading it.
    pub fn set_tempo_bpm(&mut self, tempo_bpm: f32) {
        if !tempo_bpm.is_finite() || (tempo_bpm - self.tempo_bpm).abs() < 1.0e-4 {
            return;
        }
        self.tempo_bpm = tempo_bpm;
        if self.params.sync {
            self.apply_scalars();
        }
    }

    pub fn tempo_bpm(&self) -> f32 {
        self.tempo_bpm
    }

    /// Apply a compact wire update already resolved by the UI/control thread.
    ///
    /// The audio path never parses JSON or looks up string parameter ids. Every
    /// arm below only recomputes derived scalars or biquad coefficients, so this
    /// stays allocation-free and safe to call from the producer thread between
    /// blocks.
    pub fn apply_wire_param(&mut self, wire_index: u32, value: f32) -> bool {
        if !ipc::apply_wire_param(&mut self.params, wire_index, value) {
            return false;
        }
        match wire_index {
            ipc::LOW_CUT_INDEX | ipc::HIGH_CUT_INDEX => self.rebuild_filters(),
            ipc::TIME_L_INDEX
            | ipc::TIME_R_INDEX
            | ipc::OUTPUT_INDEX
            | ipc::SYNC_INDEX
            | ipc::DIVISION_L_INDEX
            | ipc::DIVISION_R_INDEX
            | ipc::LINK_INDEX => self.apply_scalars(),
            // Feedback, cross, saturation, mix, mode and the two flags are read
            // straight off `params` in `process_stereo`; nothing to recompute.
            _ => {}
        }
        true
    }

    /// Resolve a string id off the realtime path (project restore, tests).
    pub fn apply_ui_param(&mut self, id: &str, value: f32) -> bool {
        match ipc::ui_param_index(id) {
            Some(index) => self.apply_wire_param(index, value),
            None => false,
        }
    }

    /// EchoSpace introduces no lookahead: the delay time is a musical
    /// parameter, not a processing latency the graph should compensate for.
    pub fn latency_samples(&self) -> usize {
        0
    }

    fn apply_params(&mut self) {
        self.apply_scalars();
        self.rebuild_filters();
    }

    fn apply_scalars(&mut self) {
        let max_delay = self.delay_l.buffer.len().saturating_sub(2).max(1);
        let time_l = self.params.effective_time_ms_l(self.tempo_bpm);
        let time_r = self.params.effective_time_ms_r(self.tempo_bpm);
        self.delay_samples_l = ((time_l * 0.001 * self.sample_rate) as usize).clamp(1, max_delay);
        self.delay_samples_r = ((time_r * 0.001 * self.sample_rate) as usize).clamp(1, max_delay);
        self.output_gain = db_to_linear(self.params.output_db);
    }

    fn rebuild_filters(&mut self) {
        let hpf = make_eq_biquad(
            "highpass",
            self.params.low_cut_hz,
            0.0,
            0.707,
            self.sample_rate,
        );
        self.hpf_l = hpf;
        self.hpf_r = hpf;
        let lpf = make_eq_biquad(
            "lowpass",
            self.params.high_cut_hz.min(self.sample_rate * 0.45),
            0.0,
            0.707,
            self.sample_rate,
        );
        self.lpf_l = lpf;
        self.lpf_r = lpf;
    }

    #[inline]
    fn saturate(sample: f32, amount: f32) -> f32 {
        if amount <= 0.0 {
            return sample;
        }
        let drive = 1.0 + amount * 6.0;
        (sample * drive).tanh() / drive.tanh().max(0.001)
    }

    #[inline]
    fn filter_feedback(
        sample: f32,
        hpf: &mut Option<DirectForm1<f32>>,
        lpf: &mut Option<DirectForm1<f32>>,
    ) -> f32 {
        let mut x = sample;
        if let Some(f) = hpf.as_mut() {
            x = f.run(x);
        }
        if let Some(f) = lpf.as_mut() {
            x = f.run(x);
        }
        x
    }
}

impl StereoEffect for Dsp {
    fn reset(&mut self) {
        self.delay_l.clear();
        self.delay_r.clear();
        if let Some(f) = self.hpf_l.as_mut() {
            f.reset_state();
        }
        if let Some(f) = self.hpf_r.as_mut() {
            f.reset_state();
        }
        if let Some(f) = self.lpf_l.as_mut() {
            f.reset_state();
        }
        if let Some(f) = self.lpf_r.as_mut() {
            f.reset_state();
        }
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        let sr = sample_rate.max(1.0);
        if (sr - self.sample_rate).abs() < f32::EPSILON {
            return;
        }
        let capacity = Self::ring_capacity(sr);
        self.sample_rate = sr;
        self.delay_l = DelayLine::new(capacity);
        self.delay_r = DelayLine::new(capacity);
        self.apply_params();
    }

    fn process_stereo(&mut self, left: f32, right: f32) -> (f32, f32) {
        if !self.params.power {
            return (left, right);
        }

        let delayed_l = self.delay_l.read(self.delay_samples_l);
        let delayed_r = self.delay_r.read(self.delay_samples_r);

        let fb = if self.params.freeze {
            MAX_FEEDBACK
        } else {
            self.params.feedback / 100.0
        };
        let xfb = self.params.cross_feedback / 100.0;
        let sat = self.params.saturation / 100.0;
        // Normalising by the cross amount keeps the round-trip gain at `fb`
        // however much of it is routed across. Summing the two paths at full
        // strength reaches 1.96 at the maximum settings, and with saturation at
        // 0 — which the user is free to do — nothing else bounds the line.
        let fb_norm = fb / (1.0 + xfb);

        let (in_l, in_r) = match self.params.mode {
            DelayMode::Mono => {
                let m = (left + right) * 0.5;
                (m, m)
            }
            DelayMode::Stereo | DelayMode::PingPong => (left, right),
        };

        let mut fb_l = (delayed_l + delayed_r * xfb) * fb_norm;
        let mut fb_r = (delayed_r + delayed_l * xfb) * fb_norm;
        if self.params.mode == DelayMode::PingPong {
            // Swap cross paths for classic ping-pong bounce.
            std::mem::swap(&mut fb_l, &mut fb_r);
        }

        fb_l = Self::filter_feedback(fb_l, &mut self.hpf_l, &mut self.lpf_l);
        fb_r = Self::filter_feedback(fb_r, &mut self.hpf_r, &mut self.lpf_r);
        fb_l = Self::saturate(fb_l, sat);
        fb_r = Self::saturate(fb_r, sat);

        if !self.params.freeze {
            self.delay_l.write_sample(in_l + fb_l);
            self.delay_r.write_sample(in_r + fb_r);
        } else {
            self.delay_l.write_sample(fb_l);
            self.delay_r.write_sample(fb_r);
        }

        let wet_l = delayed_l * self.output_gain;
        let wet_r = delayed_r * self.output_gain;
        let amount = self.params.mix / 100.0;
        (mix(left, wet_l, amount), mix(right, wet_r, amount))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_ids_are_unique_and_match_defaults() {
        let d = descriptor();
        assert_eq!(d.id, PLUGIN_ID);
        assert_eq!(d.category, PluginCategory::Effect);

        let mut ids: Vec<_> = d.params.iter().map(|p| p.id).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(count, ids.len(), "duplicate parameter id in descriptor");

        let defaults = ipc::ui_values(&default_params());
        for param in d.params {
            let (_, actual) = defaults
                .iter()
                .find(|(id, _)| *id == param.id)
                .copied()
                .unwrap_or_else(|| panic!("`{}` is missing from ui_values", param.id));
            assert!(
                (param.default_value - actual).abs() < 1.0e-6,
                "`{}`: descriptor says {}, default_params() says {actual}",
                param.id,
                param.default_value,
            );
            assert!(
                param.default_value >= param.min && param.default_value <= param.max,
                "`{}`: default {} is outside {}..{}",
                param.id,
                param.default_value,
                param.min,
                param.max,
            );
        }
    }

    #[test]
    fn bypass_when_power_off() {
        let mut dsp = Dsp::new(48_000.0);
        let mut params = default_params();
        params.power = false;
        dsp.set_params(params);
        assert_eq!(dsp.process_stereo(0.25, -0.25), (0.25, -0.25));
    }

    #[test]
    fn delay_produces_echo() {
        let mut dsp = Dsp::new(48_000.0);
        let mut params = default_params();
        params.time_ms_l = 10.0;
        params.time_ms_r = 10.0;
        params.feedback = 0.0;
        params.mix = 100.0;
        params.mode = DelayMode::Stereo;
        dsp.set_params(params);

        // Impulse
        let _ = dsp.process_stereo(1.0, 1.0);
        let mut heard = 0.0f32;
        for _ in 0..480 {
            let (l, _) = dsp.process_stereo(0.0, 0.0);
            heard = heard.max(l.abs());
        }
        assert!(heard > 0.1);
    }

    /// The echo must land at the requested time, not at whatever the ring
    /// happens to be long enough for. Sizing the buffer with a fixed sample
    /// count silently capped this at 2 s once the engine ran at 96 kHz.
    #[test]
    fn the_longest_delay_is_reachable_at_every_rate() {
        for &sr in &[44_100.0f32, 48_000.0, 96_000.0, 192_000.0] {
            let mut dsp = Dsp::new(sr);
            let mut params = default_params();
            params.time_ms_l = MAX_DELAY_MS;
            params.time_ms_r = MAX_DELAY_MS;
            params.feedback = 0.0;
            params.mix = 100.0;
            params.mode = DelayMode::Stereo;
            dsp.set_params(params);

            let expected = (MAX_DELAY_MS * 0.001 * sr) as usize;
            let _ = dsp.process_stereo(1.0, 1.0);

            // Nothing before the tap...
            let mut early = 0.0f32;
            for _ in 0..(expected - 16) {
                let (l, _) = dsp.process_stereo(0.0, 0.0);
                early = early.max(l.abs());
            }
            assert!(early < 1.0e-4, "echo arrived early at {sr} Hz: {early}");

            // ...and the impulse right at it.
            let mut heard = 0.0f32;
            for _ in 0..32 {
                let (l, _) = dsp.process_stereo(0.0, 0.0);
                heard = heard.max(l.abs());
            }
            assert!(heard > 0.5, "no echo at {sr} Hz after {expected} samples");
        }
    }

    /// Feedback and cross-feed used to be summed at full strength, so the
    /// round-trip gain reached 1.96 at the maximum settings. The saturator hid
    /// it until saturation was turned off, and then the line grew without
    /// bound.
    #[test]
    fn maximum_cross_feedback_stays_bounded_without_saturation() {
        for mode in [DelayMode::Stereo, DelayMode::PingPong, DelayMode::Mono] {
            let mut dsp = Dsp::new(48_000.0);
            let mut params = default_params();
            params.mode = mode;
            params.time_ms_l = 120.0;
            params.time_ms_r = 180.0;
            params.feedback = 98.0;
            params.cross_feedback = 100.0;
            params.saturation = 0.0;
            params.mix = 100.0;
            dsp.set_params(params);

            let mut peak = 0.0f32;
            for n in 0..(48_000 * 20) {
                let x = if n < 4_800 {
                    (n as f32 * 0.02).sin() * 0.7
                } else {
                    0.0
                };
                let (l, r) = dsp.process_stereo(x, x);
                assert!(l.is_finite() && r.is_finite(), "{mode:?} diverged at {n}");
                peak = peak.max(l.abs()).max(r.abs());
            }
            assert!(peak < 8.0, "{mode:?} ran away: peak {peak}");
        }
    }

    /// Cross-feed changes where the repeats go, not how loud the loop is.
    #[test]
    fn cross_feedback_does_not_change_the_loop_gain() {
        let tail_peak = |cross: f32| {
            let mut dsp = Dsp::new(48_000.0);
            let mut params = default_params();
            params.mode = DelayMode::Stereo;
            params.time_ms_l = 100.0;
            params.time_ms_r = 100.0;
            params.feedback = 90.0;
            params.cross_feedback = cross;
            params.saturation = 0.0;
            params.mix = 100.0;
            dsp.set_params(params);

            let _ = dsp.process_stereo(1.0, 1.0);
            let mut peak = 0.0f32;
            for _ in 0..(48_000 * 3) {
                let (l, r) = dsp.process_stereo(0.0, 0.0);
                peak = peak.max(l.abs()).max(r.abs());
            }
            peak
        };
        let none = tail_peak(0.0);
        let full = tail_peak(100.0);
        assert!(none > 0.1 && full > 0.1, "no tail to compare");
        assert!(
            (none - full).abs() < none * 0.25,
            "loop gain moved with cross-feed: {none} vs {full}"
        );
    }

    #[test]
    fn freeze_holds_the_repeats_after_the_input_stops() {
        let mut dsp = Dsp::new(48_000.0);
        let mut params = default_params();
        params.mix = 100.0;
        params.time_ms_l = 250.0;
        params.time_ms_r = 250.0;
        params.feedback = 40.0;
        dsp.set_params(params);

        for n in 0..24_000 {
            let x = (n as f32 * 0.03).sin() * 0.5;
            let _ = dsp.process_stereo(x, x);
        }
        assert!(dsp.apply_ui_param("freeze", 1.0));

        let mut early = 0.0f32;
        for _ in 0..12_000 {
            let (l, r) = dsp.process_stereo(0.0, 0.0);
            early = early.max(l.abs()).max(r.abs());
        }
        let mut late = 0.0f32;
        for _ in 0..(48_000 * 4) {
            let (l, r) = dsp.process_stereo(0.0, 0.0);
            late = late.max(l.abs()).max(r.abs());
        }
        assert!(early > 1.0e-4, "nothing in the line to freeze");
        assert!(
            late > early * 0.25,
            "freeze decayed away: early {early}, late {late}"
        );
    }

    #[test]
    fn reset_clears_the_repeats() {
        let mut dsp = Dsp::new(48_000.0);
        let mut params = default_params();
        params.mix = 100.0;
        dsp.set_params(params);
        for _ in 0..4_800 {
            let _ = dsp.process_stereo(0.5, -0.5);
        }
        dsp.reset();
        let (l, r) = dsp.process_stereo(0.0, 0.0);
        assert!(l.abs() < 1.0e-6 && r.abs() < 1.0e-6);
    }

    #[test]
    fn sample_rate_change_keeps_taps_inside_the_new_rings() {
        let mut dsp = Dsp::new(192_000.0);
        let mut params = default_params();
        params.time_ms_l = MAX_DELAY_MS;
        params.time_ms_r = MAX_DELAY_MS;
        params.mix = 100.0;
        dsp.set_params(params);
        dsp.set_sample_rate(44_100.0);

        let capacity = dsp.delay_l.buffer.len();
        assert!(dsp.delay_samples_l < capacity);
        assert!(dsp.delay_samples_r < capacity);
        for _ in 0..4_410 {
            let (l, r) = dsp.process_stereo(0.3, -0.3);
            assert!(l.is_finite() && r.is_finite());
        }
    }

    /// A synced line's spacing is a note length: it has to come out of the
    /// transport tempo, not out of the free `time_ms_*` the control still
    /// holds.
    #[test]
    fn a_synced_line_takes_its_delay_from_the_tempo() {
        let mut dsp = Dsp::new(48_000.0);
        let mut params = default_params();
        params.time_ms_l = 12.0;
        params.time_ms_r = 12.0;
        params.sync = true;
        params.division_l = DIVISION_LABELS.iter().position(|l| *l == "1/4").unwrap() as u8;
        params.division_r = params.division_l;
        dsp.set_params(params);
        dsp.set_tempo_bpm(120.0);

        // A quarter note at 120 BPM is 500 ms.
        assert_eq!(dsp.delay_samples_l, 24_000);
        assert_eq!(dsp.delay_samples_r, 24_000);

        // Half the tempo, twice the spacing — without any parameter edit.
        dsp.set_tempo_bpm(60.0);
        assert_eq!(dsp.delay_samples_l, 48_000);

        // ...and switching sync off returns the line to the time the control
        // was holding all along.
        assert!(dsp.apply_ui_param("sync", 0.0));
        assert_eq!(dsp.delay_samples_l, 576);
    }

    #[test]
    fn a_free_line_ignores_the_tempo() {
        let mut dsp = Dsp::new(48_000.0);
        let mut params = default_params();
        params.time_ms_l = 250.0;
        params.sync = false;
        dsp.set_params(params);
        let before = dsp.delay_samples_l;
        dsp.set_tempo_bpm(174.0);
        assert_eq!(dsp.delay_samples_l, before);
        assert_eq!(dsp.tempo_bpm(), 174.0);
    }

    /// A nonsense tempo — a region read before the engine's first publish, or a
    /// project with a zero in it — must not turn into a zero-length or infinite
    /// delay.
    #[test]
    fn a_nonsense_tempo_cannot_break_the_delay_line() {
        let mut dsp = Dsp::new(48_000.0);
        let mut params = default_params();
        params.sync = true;
        dsp.set_params(params);
        for bpm in [0.0, -240.0, f32::NAN, f32::INFINITY, 1.0e12] {
            dsp.set_tempo_bpm(bpm);
            assert!(dsp.delay_samples_l >= 1);
            assert!(dsp.delay_samples_l < dsp.delay_l.buffer.len());
            let (l, r) = dsp.process_stereo(0.4, -0.4);
            assert!(l.is_finite() && r.is_finite(), "diverged at {bpm} BPM");
        }
    }

    #[test]
    fn every_division_stays_inside_the_line_at_any_tempo() {
        let mut dsp = Dsp::new(48_000.0);
        for bpm in [MIN_TEMPO_BPM, 60.0, 120.0, 174.0, MAX_TEMPO_BPM] {
            for division in 0..DIVISION_COUNT as u8 {
                let mut params = default_params();
                params.sync = true;
                params.division_l = division;
                params.division_r = division;
                dsp.set_params(params);
                dsp.set_tempo_bpm(bpm);
                assert!(
                    dsp.delay_samples_l >= 1 && dsp.delay_samples_l < dsp.delay_l.buffer.len(),
                    "division {division} at {bpm} BPM left the ring"
                );
                let ms = division_ms(division, bpm);
                assert!((1.0..=MAX_DELAY_MS).contains(&ms));
            }
        }
    }

    #[test]
    fn wire_update_changes_only_authoritative_params() {
        let mut dsp = Dsp::new(48_000.0);
        assert!(dsp.apply_wire_param(ipc::TIME_L_INDEX, 500.0));
        assert_eq!(dsp.params().time_ms_l, 500.0);
        assert_eq!(dsp.delay_samples_l, 24_000);
        assert!(!dsp.apply_wire_param(u32::MAX, 0.0));
        assert!(!dsp.apply_wire_param(ipc::TIME_L_INDEX, f32::NAN));
    }
}
