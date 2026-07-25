//! Circuit-level nonlinear stages — no memoryless waveshapers.
//!
//! Everything the distortion and amp paths used to do with a static curve
//! (`tanh`, `atan`, hard clip with a knee) is replaced here by a small circuit
//! that is *solved* per sample and carries its own state. That difference is
//! not cosmetic:
//!
//! * A waveshaper corners instantly and identically every time it is reached,
//!   which is the sound of digital clipping rather than a pedal. Both stages
//!   here are solved against a reactive element — the capacitor across the
//!   diodes, the cathode and Miller networks around the valve — so an edge can
//!   only round as fast as the circuit allows.
//! * A waveshaper's break-up is level-locked: the same input amplitude always
//!   produces the same harmonics, and asymmetry has to be faked by offsetting
//!   the input. The stages here move their own operating point (capacitor
//!   charge, cathode self-bias, grid conduction), so the *history* of what was
//!   played changes how the next sample breaks up, and an unequal diode pair or
//!   a self-biasing valve is asymmetric because its parts are.
//!
//! Neither stage decides *which band* gets distorted — that stays with the
//! callers' filters, where a real circuit puts it: a pedal's gain network lifts
//! only the band above its corner, and a valve stage's cathode bypass
//! degenerates its own low end. Driving the whole spectrum into one ceiling is
//! what made the previous build muddy, and no amount of curve-shaping fixes it.
//!
//! Both solvers are realtime-safe: fixed iteration counts, no allocation, no
//! branching on unbounded data, and every state guarded for finiteness.

/// Clamp to a sane audio range and kill non-finite values.
#[inline]
fn finite(x: f32) -> f32 {
    if x.is_finite() {
        x.clamp(-64.0, 64.0)
    } else {
        0.0
    }
}

// ---------------------------------------------------------------------------
// Diode clipper (pedals)
// ---------------------------------------------------------------------------

/// An antiparallel diode pair with its smoothing capacitor, shunting a node
/// fed through a series resistor — the clipping section of a real pedal.
///
/// ```text
///        Rs = 1
///   x ---/\/\/--+--- v (out)
///               |
///          +----+----+
///          |         |
///         === C     ### D+ || D-   (asymmetric pair)
///          |         |
///         gnd       gnd
/// ```
///
/// The node stores charge in two places: the fixed capacitor, and the diodes
/// themselves. A conducting junction holds a diffusion charge proportional to
/// its own current (`q = tau * i`, `tau` the transit time), so its capacitance
/// is not a constant — it is near zero while the diode is off and large while
/// it conducts. That is modelled by integrating *charge*, not by inserting a
/// capacitance into the node equation, which would be wrong for a nonlinear
/// element:
///
/// ```text
///   q(v) = C*v + tau_p*i_p(v) - tau_n*i_n(v)
///   F(v) = (v - x) + fs * (q(v) - q_prev) + i_p(v) - i_n(v) = 0
/// ```
///
/// What this buys is reverse recovery: a junction that has been conducting
/// cannot turn off until its stored charge is swept out, so it keeps conducting
/// briefly after the drive reverses. Slow parts therefore soften the corners
/// and hold the node up on the way out of clipping, which is a real part of why
/// builders hear diode types — a fast silicon switching diode stores almost
/// nothing and stays tight, while germanium and rectifier junctions run into
/// microseconds and audibly round the break-up. Two unequal parts recover at
/// different rates, so the two edges of the wave differ as well.
///
/// It is *not* a large source of even harmonics, and the measurements say so:
/// the effect lives in the transitions, which are a few samples out of a cycle,
/// so a strongly mismatched pair contributes h2 around −68 dB. Amplitude
/// asymmetry does not help either — a hard-clipped wave with unequal flat tops
/// is a symmetric square plus a constant, and the downstream blocker takes the
/// constant. The previous build's stronger h2 came from injecting an
/// envelope-driven offset ahead of a symmetric curve, which is not a circuit
/// and is not reproduced here.
///
/// `F` is smooth and strictly increasing in `v` (every derivative term is
/// positive), so a damped Newton iteration converges in a handful of steps.
///
/// Voltages are normalized so a nominal diode drop is `1.0`, which puts the
/// clipping threshold at plugin unity and makes the stage transparent below it.
#[derive(Debug, Clone, Copy)]
pub(super) struct DiodeParams {
    v_pos: f32,
    v_neg: f32,
    inv_knee_pos: f32,
    inv_knee_neg: f32,
    knee_pos: f32,
    knee_neg: f32,
    /// Fixed capacitor conductance `Gc = C * fs`, normalized against `Rs = 1`.
    gc: f32,
    /// Transit time × rate for each diode: how much diffusion charge the
    /// junction stores per unit of its own current.
    k_pos: f32,
    k_neg: f32,
}

impl DiodeParams {
    /// * `v_pos` / `v_neg` — forward voltage of each diode, normalized so a
    ///   nominal drop is 1.0. Unequal values are a real asymmetric pair.
    /// * `knee_pos` / `knee_neg` — emission coefficient × thermal voltage, in
    ///   the same normalized units. Larger is a softer, germanium-like knee.
    /// * `tau_pos` / `tau_neg` — junction transit time in seconds, which sets
    ///   the diffusion charge each diode stores while conducting. Fast silicon
    ///   switching parts are a few nanoseconds and do essentially nothing here;
    ///   germanium and rectifier junctions run into microseconds and are what
    ///   thicken and skew the break-up. The current scale is normalized rather
    ///   than in amps, so these are device-ballpark figures, not datasheet
    ///   extractions.
    /// * `smooth_hz` — corner of the fixed capacitor across the diodes. This is
    ///   the part that keeps hard clipping from sounding like a digital fold
    ///   even when nothing is conducting.
    /// * `node_rate` — the rate the solver actually runs at, i.e. the
    ///   *oversampled* rate.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        v_pos: f32,
        v_neg: f32,
        knee_pos: f32,
        knee_neg: f32,
        tau_pos: f32,
        tau_neg: f32,
        smooth_hz: f32,
        node_rate: f32,
    ) -> Self {
        let kp = knee_pos.clamp(0.005, 0.5);
        let kn = knee_neg.clamp(0.005, 0.5);
        let rate = node_rate.max(1.0);
        // Rs is normalized to 1, so C follows from the corner alone:
        // Gc = C * fs = fs / (2*pi*f_c).
        let gc = (rate / (std::f32::consts::TAU * smooth_hz.clamp(20.0, rate * 0.45))).min(64.0);
        Self {
            v_pos: v_pos.max(0.02),
            v_neg: v_neg.max(0.02),
            inv_knee_pos: 1.0 / kp,
            inv_knee_neg: 1.0 / kn,
            knee_pos: kp,
            knee_neg: kn,
            gc,
            k_pos: (tau_pos.max(0.0) * rate).min(16.0),
            k_neg: (tau_neg.max(0.0) * rate).min(16.0),
        }
    }

    /// Forward current of each diode, as positive quantities.
    #[inline]
    fn currents(&self, v: f32) -> (f32, f32) {
        (
            ((v - self.v_pos) * self.inv_knee_pos).min(24.0).exp(),
            ((-v - self.v_neg) * self.inv_knee_neg).min(24.0).exp(),
        )
    }

    /// Total charge stored on the node, already scaled by the sample rate so it
    /// reads as a current.
    #[inline]
    fn charge(&self, v: f32, ep: f32, en: f32) -> f32 {
        self.gc * v + self.k_pos * ep - self.k_neg * en
    }

    /// Log-domain first guess. Deep in conduction the diode fixes `v` to within
    /// a few millivolts of `v_f + knee*ln(i)`, so starting there keeps Newton
    /// out of the region where the exponential's curvature stalls it.
    #[inline]
    fn guess(&self, target: f32) -> f32 {
        if target > self.v_pos {
            (self.v_pos + self.knee_pos * (target * (1.0 + self.gc)).max(1.0e-9).ln())
                .clamp(0.0, target)
        } else if target < -self.v_neg {
            (-(self.v_neg + self.knee_neg * (-target * (1.0 + self.gc)).max(1.0e-9).ln()))
                .clamp(target, 0.0)
        } else {
            target
        }
    }
}

/// Per-channel node state: the solved voltage and the current each junction was
/// carrying, which is what the diffusion charge is computed from.
#[derive(Debug, Clone, Copy, Default)]
struct DiodeState {
    v: f32,
    ep: f32,
    en: f32,
}

/// Stereo clipping node. State is the charge the node is holding — that is the
/// whole reason this is not a waveshaper.
#[derive(Debug, Clone)]
pub(super) struct DiodeClipper {
    params: DiodeParams,
    left: DiodeState,
    right: DiodeState,
}

impl DiodeClipper {
    pub(super) fn new(params: DiodeParams) -> Self {
        Self {
            params,
            left: DiodeState::default(),
            right: DiodeState::default(),
        }
    }

    pub(super) fn set_params(&mut self, params: DiodeParams) {
        self.params = params;
    }

    pub(super) fn reset(&mut self) {
        self.left = DiodeState::default();
        self.right = DiodeState::default();
    }

    #[inline]
    fn one(p: &DiodeParams, st: &mut DiodeState, x: f32) -> f32 {
        let q_prev = p.charge(st.v, st.ep, st.en);
        // While the junctions are holding charge the node cannot move far in a
        // sample, so the previous solution is the better starting point;
        // otherwise the log-domain estimate is near-exact.
        let stored = p.k_pos * st.ep + p.k_neg * st.en;
        let mut v = if stored > p.gc {
            st.v
        } else {
            p.guess((x + p.gc * st.v) / (1.0 + p.gc))
        };
        for _ in 0..6 {
            let (ep, en) = p.currents(v);
            let f = (v - x) + (p.charge(v, ep, en) - q_prev) + ep - en;
            let df = 1.0
                + p.gc
                + ep * p.inv_knee_pos * (1.0 + p.k_pos)
                + en * p.inv_knee_neg * (1.0 + p.k_neg);
            let step = (f / df).clamp(-0.5, 0.5);
            v -= step;
            if step.abs() < 1.0e-6 {
                break;
            }
        }
        let (ep, en) = p.currents(v);
        st.v = finite(v);
        st.ep = if ep.is_finite() { ep } else { 0.0 };
        st.en = if en.is_finite() { en } else { 0.0 };
        st.v
    }

    /// Solve both channels for one sample at the node's own rate. Call this
    /// *inside* an oversampler and construct [`DiodeParams`] with the
    /// oversampled rate.
    #[inline]
    pub(super) fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        let p = self.params;
        (
            Self::one(&p, &mut self.left, l),
            Self::one(&p, &mut self.right, r),
        )
    }
}

// ---------------------------------------------------------------------------
// Triode stage (amp)
// ---------------------------------------------------------------------------

/// Smooth `max(e, 0)` — the valve's cutoff region, rounded by `knee`.
#[inline]
fn smooth_max(e: f32, knee: f32) -> f32 {
    0.5 * (e + (e * e + knee).sqrt())
}

/// Smooth `min(i, ceiling)` — the plate running out of supply voltage.
#[inline]
fn smooth_min(i: f32, ceiling: f32, knee: f32) -> f32 {
    let d = i - ceiling;
    0.5 * (i + ceiling - (d * d + knee).sqrt())
}

/// Plate current: zero below cutoff, three-halves law above it.
///
/// The asymmetry that makes a valve stage sound like one falls straight out of
/// this: the cutoff side rounds off gradually while the conduction side grows
/// faster than linear until the plate saturates. No curve is being drawn — the
/// operating point is supplied by the caller's cathode and grid state, so the
/// same input voltage produces different current depending on what came before.
#[inline]
fn plate_current(e: f32, knee: f32) -> f32 {
    let s = smooth_max(e, knee);
    s * s.sqrt()
}

/// Everything about a triode stage that only changes when the model or a knob
/// is reconfigured. The two things that move per sample — the stage's gain and
/// its coupling pole — are passed to [`TriodeState::process`] instead, so the
/// operating point never has to be re-solved on the audio thread.
#[derive(Debug, Clone, Copy)]
pub(super) struct TriodeParams {
    /// Quiescent operating point above cutoff.
    pub(super) grid_offset: f32,
    /// Cutoff rounding.
    pub(super) knee: f32,
    /// Plate current ceiling — where the stage runs out of supply.
    pub(super) headroom: f32,
    /// Rounding of that ceiling.
    pub(super) sat_knee: f32,
    /// Cathode resistor: how much of the plate current feeds back as bias.
    pub(super) cathode_r: f32,
    /// Cathode bypass corner as a one-pole coefficient. Below it the cathode
    /// follows the signal and degenerates the gain — this is why a real preamp
    /// amplifies bass *less* than mids and does not turn to mud when driven.
    pub(super) cathode_alpha: f32,
    /// Miller capacitance at the plate: HF rolloff *inside* the stage, so the
    /// next stage never gets the raw clipping edge to re-clip into fizz.
    pub(super) miller_alpha: f32,
    /// How far above the quiescent point the stage can be driven before grid
    /// current starts flowing. Derived into `grid_clamp` at configure time.
    pub(super) grid_headroom: f32,
    /// Instantaneous grid-current loading above that point.
    pub(super) grid_soft: f32,
    /// How fast grid current charges the coupling capacitor.
    pub(super) grid_attack: f32,
    /// How fast the grid-leak resistor discharges it again. Together with
    /// `grid_attack` this is blocking distortion: hit it hard and the stage
    /// chokes, then recovers over milliseconds. A waveshaper cannot do this.
    pub(super) grid_release: f32,
    /// Operating point at which grid conduction begins (derived).
    pub(super) grid_clamp: f32,
    /// Plate load, normalized at configure time so small-signal gain == `gain`.
    pub(super) plate_r: f32,
    /// Quiescent current, subtracted so the stage is DC-free at rest.
    pub(super) quiescent: f32,
}

impl TriodeParams {
    /// Solve the self-bias operating point: the cathode voltage and the plate
    /// current determine each other, so damped fixed-point iteration on the
    /// control path (never in the audio callback) settles it.
    pub(super) fn solve_operating_point(&mut self) {
        let mut i = 1.0f32;
        for _ in 0..64 {
            let e = self.grid_offset - self.cathode_r * i;
            let target = smooth_min(plate_current(e, self.knee), self.headroom, self.sat_knee);
            i += 0.35 * (target - i);
            if !i.is_finite() {
                i = 0.0;
                break;
            }
        }
        self.quiescent = i;
        let e_q = (self.grid_offset - self.cathode_r * i).max(1.0e-3);
        self.grid_clamp = e_q + self.grid_headroom.max(0.05);
        // d(i)/d(e) = 1.5 * sqrt(e) at the operating point; normalizing by it
        // makes `gain` an honest voltage gain instead of an arbitrary scalar.
        self.plate_r = 1.0 / (1.5 * e_q.sqrt()).max(1.0e-3);
    }

    pub(super) fn cathode_rest(&self) -> f32 {
        self.cathode_r * self.quiescent
    }
}

/// Per-channel state of one triode stage.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct TriodeState {
    coupling_x: f32,
    coupling_y: f32,
    grid_charge: f32,
    cathode: f32,
    miller: f32,
}

impl TriodeState {
    /// Seed the cathode at its resting bias so the stage is at its operating
    /// point on the first sample instead of fading in over the RC.
    pub(super) fn prime(&mut self, params: &TriodeParams) {
        *self = Self::default();
        self.cathode = params.cathode_rest();
    }

    /// `gain` is the stage's small-signal voltage gain and `coupling_pole` its
    /// interstage AC-coupling pole; both are smoothed control values, so they
    /// arrive per sample rather than living in [`TriodeParams`].
    #[inline]
    pub(super) fn process(
        &mut self,
        x: f32,
        p: &TriodeParams,
        gain: f32,
        coupling_pole: f32,
    ) -> f32 {
        // Interstage coupling capacitor.
        let coupled = x - self.coupling_x + coupling_pole * self.coupling_y;
        self.coupling_x = x;
        self.coupling_y = finite(coupled);

        // Operating point = fixed offset, the amplified signal, the cathode's
        // own memory, and whatever charge previous grid current left behind.
        let raw = p.grid_offset + self.coupling_y * gain - self.cathode - self.grid_charge;
        // Drive the grid positive and it draws current: the source is loaded
        // (instantaneous squash) *and* the coupling capacitor charges, dragging
        // the whole stage toward cutoff for milliseconds afterwards. That is
        // blocking distortion, and it is why a cranked amp coughs on a hard
        // chord and clears as it decays.
        let over = raw - p.grid_clamp;
        let e = if over > 0.0 {
            self.grid_charge += p.grid_attack * over;
            p.grid_clamp + over * p.grid_soft
        } else {
            raw
        };
        self.grid_charge = finite(self.grid_charge - self.grid_charge * p.grid_release);

        let i = smooth_min(plate_current(e, p.knee), p.headroom, p.sat_knee);

        // Cathode RC. `cathode_alpha` is the bypass capacitor: fast for the
        // band it shorts (full gain), slow below it (degenerated gain).
        self.cathode = finite(self.cathode + p.cathode_alpha * (p.cathode_r * i - self.cathode));

        // Inverting plate output, then the Miller pole.
        let out = (p.quiescent - i) * p.plate_r;
        self.miller = finite(self.miller + p.miller_alpha * (out - self.miller));
        self.miller
    }
}

// ---------------------------------------------------------------------------
// Push-pull output stage
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub(super) struct PushPullParams {
    /// Idle current. Low bias is class B — the halves hand over with a real
    /// crossover notch, which is the grind of a cranked output stage.
    pub(super) bias: f32,
    pub(super) knee: f32,
    pub(super) headroom: f32,
    pub(super) sat_knee: f32,
    /// Mismatch between the two halves.
    pub(super) asymmetry: f32,
    pub(super) scale: f32,
}

impl PushPullParams {
    pub(super) fn normalize(&mut self) {
        let slope = 1.5 * self.bias.max(1.0e-3).sqrt() * (2.0 - self.asymmetry);
        self.scale = 1.0 / slope.max(1.0e-3);
    }

    /// `x` is the already-driven grid voltage; `supply` is the sagged rail
    /// (1.0 = full). The supply scales the *ceiling*, so a collapsing rail
    /// compresses the peaks and leaves the small signal alone — which is what
    /// a sagging amp actually does. Small-signal gain through the pair is
    /// normalized to unity, so Master remains the one drive control.
    #[inline]
    pub(super) fn process(&self, x: f32, supply: f32) -> f32 {
        let drive = x;
        let ceiling = (self.headroom * supply).max(1.0e-3);
        let a = smooth_min(
            plate_current(self.bias + drive, self.knee),
            ceiling,
            self.sat_knee,
        );
        let b = smooth_min(
            plate_current(self.bias - drive * (1.0 - self.asymmetry), self.knee),
            ceiling * (1.0 - self.asymmetry * 0.5),
            self.sat_knee,
        );
        finite((a - b) * self.scale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    fn render_clipper(params: DiodeParams, freq: f32, amp: f32, rate: f32) -> Vec<f32> {
        let mut clipper = DiodeClipper::new(params);
        let n = (rate * 0.25) as usize;
        let mut out = Vec::with_capacity(n);
        for k in 0..n {
            let phase = std::f32::consts::TAU * freq * k as f32 / rate;
            let (l, _) = clipper.process(amp * phase.sin(), 0.0);
            out.push(l);
        }
        // Drop the first 50 ms so the capacitor has settled.
        out.split_off((rate * 0.05) as usize)
    }

    #[test]
    fn diode_node_is_transparent_below_the_knee_and_clamps_above_it() {
        let rate = 96_000.0;
        let params = DiodeParams::new(1.0, 1.0, 0.075, 0.075, 1.0e-8, 1.0e-8, 12_000.0, rate);
        let clean = render_clipper(params, 400.0, 0.25, rate);
        let slammed = render_clipper(params, 400.0, 40.0, rate);
        let clean_peak = clean.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let slammed_peak = slammed.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(
            (clean_peak - 0.25).abs() < 0.02,
            "well under the knee the node should pass unchanged, got {clean_peak}"
        );
        // 44 dB of extra input buys well under 6 dB of extra output: the diode
        // exponential sets the ceiling, not a chosen clamp value.
        assert!(
            (1.0..1.6).contains(&slammed_peak),
            "hard drive should land near the diode drop, got {slammed_peak}"
        );
    }

    #[test]
    fn diode_clipper_is_finite_and_bounded_under_abuse() {
        let rate = 192_000.0;
        let params = DiodeParams::new(0.9, 1.1, 0.05, 0.12, 2.0e-6, 5.0e-7, 9_000.0, rate);
        let mut clipper = DiodeClipper::new(params);
        let mut peak = 0.0f32;
        for k in 0..20_000 {
            let x = if k % 97 < 48 { 400.0 } else { -400.0 };
            let (l, r) = clipper.process(x, -x);
            assert!(l.is_finite() && r.is_finite());
            peak = peak.max(l.abs()).max(r.abs());
        }
        assert!(peak < 2.5, "clipped square should stay bounded, got {peak}");
    }

    #[test]
    fn asymmetric_diodes_skew_the_two_halves() {
        let rate = 96_000.0;
        let params = DiodeParams::new(0.70, 1.30, 0.05, 0.05, 1.0e-8, 1.0e-8, 12_000.0, rate);
        let out = render_clipper(params, 400.0, 12.0, rate);
        let high = out.iter().fold(0.0f32, |m, v| m.max(*v));
        let low = out.iter().fold(0.0f32, |m, v| m.min(*v)).abs();
        assert!(
            low > high * 1.4,
            "unequal diodes should clip the halves at different levels: {high} vs {low}"
        );
    }

    /// Harmonic magnitude at `harmonic * freq`, measured about the signal's own
    /// mean so a DC offset cannot be mistaken for even-harmonic content — the
    /// same thing the downstream DC blocker does.
    fn harmonic(out: &[f32], freq: f32, rate: f32, harmonic: usize) -> f32 {
        let mean = out.iter().sum::<f32>() / out.len().max(1) as f32;
        let w = std::f32::consts::TAU * freq * harmonic as f32 / rate;
        let (mut re, mut im) = (0.0f32, 0.0f32);
        for (k, v) in out.iter().enumerate() {
            let phase = w * k as f32;
            re += (v - mean) * phase.cos();
            im += (v - mean) * phase.sin();
        }
        (re * re + im * im).sqrt() / out.len().max(1) as f32
    }

    /// Pins the *measured size* of the junction-charge contribution to even
    /// harmonics, so the claim in the module docs stays honest. Unequal transit
    /// times do generate h2, and they generate more of it than a matched fast
    /// pair — but the effect lives in the edges, so it stays tiny. Anything
    /// that made this assertion pass by an order of magnitude would not be a
    /// diode any more.
    #[test]
    fn junction_charge_skews_the_edges_but_is_a_minor_source_of_even_harmonics() {
        let rate = 192_000.0;
        let freq = 400.0;
        // Identical forward voltages, so there is no amplitude asymmetry at all
        // and the only difference between the pairs is stored junction charge.
        let matched = DiodeParams::new(1.0, 1.0, 0.05, 0.05, 8.0e-9, 8.0e-9, 9_000.0, rate);
        let mismatched = DiodeParams::new(1.0, 1.0, 0.05, 0.05, 2.5e-6, 6.0e-7, 9_000.0, rate);

        let ratio = |p: DiodeParams| {
            let out = render_clipper(p, freq, 20.0, rate);
            harmonic(&out, freq, rate, 2) / harmonic(&out, freq, rate, 1).max(1.0e-12)
        };
        let matched_h2 = ratio(matched);
        let mismatched_h2 = ratio(mismatched);

        assert!(
            mismatched_h2 > matched_h2 * 4.0,
            "unequal junction charge should skew the halves: {mismatched_h2} vs {matched_h2}"
        );
        // ...and this is the honest ceiling of the mechanism: about -60 dB.
        assert!(
            mismatched_h2 < 0.005,
            "junction charge is an edge effect, not a thickener: h2 {mismatched_h2}"
        );
    }

    #[test]
    fn a_slow_junction_holds_the_node_in_conduction_after_the_drive_stops() {
        let rate = 192_000.0;
        let fast = DiodeParams::new(1.0, 1.0, 0.05, 0.05, 5.0e-9, 5.0e-9, 9_000.0, rate);
        let slow = DiodeParams::new(1.0, 1.0, 0.05, 0.05, 3.0e-6, 3.0e-6, 9_000.0, rate);
        // Drive hard, then drop to zero: the stored diffusion charge has to be
        // removed before the junction can turn off, so a slow part sits higher
        // on the sample after the drive is gone.
        let recover = |p: DiodeParams| {
            let mut c = DiodeClipper::new(p);
            for _ in 0..256 {
                c.process(30.0, 0.0);
            }
            c.process(0.0, 0.0).0
        };
        let fast_after = recover(fast);
        let slow_after = recover(slow);
        assert!(
            slow_after > fast_after + 0.02,
            "the slow junction should still be conducting: {slow_after} vs {fast_after}"
        );
    }

    #[test]
    fn the_smoothing_capacitor_gives_the_node_memory() {
        let rate = 192_000.0;
        let sharp = DiodeParams::new(1.0, 1.0, 0.05, 0.05, 1.0e-8, 1.0e-8, 30_000.0, rate);
        let smooth = DiodeParams::new(1.0, 1.0, 0.05, 0.05, 1.0e-8, 1.0e-8, 4_000.0, rate);
        // Step to the knee: with a bigger capacitor the node cannot corner as
        // fast, so it takes more samples to reach its settled value.
        let settle = |p: DiodeParams| {
            let mut c = DiodeClipper::new(p);
            for _ in 0..64 {
                c.process(0.0, 0.0);
            }
            let mut samples = 0;
            for _ in 0..512 {
                let before = c.process(1.2, 0.0).0;
                let after = c.process(1.2, 0.0).0;
                samples += 1;
                if (after - before).abs() < 1.0e-4 {
                    break;
                }
            }
            samples
        };
        let fast = settle(sharp);
        let slow = settle(smooth);
        assert!(
            slow > fast,
            "the capacitor should round the edge: settled in {slow} vs {fast} samples"
        );
    }

    #[test]
    fn triode_gain_matches_its_normalized_small_signal_target() {
        let rate = 192_000.0;
        let mut p = TriodeParams {
            grid_offset: 1.4,
            knee: 0.05,
            headroom: 6.0,
            sat_knee: 0.05,
            cathode_r: 0.20,
            cathode_alpha: 0.01,
            miller_alpha: 0.5,
            grid_headroom: 1.2,
            grid_soft: 0.25,
            grid_attack: 0.02,
            grid_release: 0.0002,
            grid_clamp: 0.0,
            plate_r: 1.0,
            quiescent: 0.0,
        };
        p.solve_operating_point();
        let mut state = TriodeState::default();
        state.prime(&p);
        let n = (rate * 0.2) as usize;
        let mut input = Vec::with_capacity(n);
        let mut output = Vec::with_capacity(n);
        for k in 0..n {
            let x = 0.01 * (std::f32::consts::TAU * 1_000.0 * k as f32 / rate).sin();
            let y = state.process(x, &p, 3.0, 0.999);
            input.push(x);
            output.push(y);
        }
        let skip = (rate * 0.05) as usize;
        let measured = rms(&output[skip..]) / rms(&input[skip..]).max(1.0e-9);
        // Cathode degeneration takes a little off the raw transconductance;
        // the point is that it lands near the requested gain, not 10x off.
        assert!(
            (1.5..3.2).contains(&measured),
            "small-signal gain should track the parameter, got {measured}"
        );
    }

    #[test]
    fn triode_blocking_recovers_after_a_hard_transient() {
        let rate = 192_000.0;
        let mut p = TriodeParams {
            grid_offset: 1.4,
            knee: 0.05,
            headroom: 5.0,
            sat_knee: 0.05,
            cathode_r: 0.22,
            cathode_alpha: 0.004,
            miller_alpha: 0.4,
            grid_headroom: 0.8,
            grid_soft: 0.20,
            grid_attack: 0.05,
            grid_release: 0.00004,
            grid_clamp: 0.0,
            plate_r: 1.0,
            quiescent: 0.0,
        };
        p.solve_operating_point();
        let mut state = TriodeState::default();
        state.prime(&p);
        let tone =
            |k: usize, amp: f32| amp * (std::f32::consts::TAU * 220.0 * k as f32 / rate).sin();
        // The probe rides a DC transient from the coupling capacitor for a few
        // milliseconds after any level change, so every window is measured
        // about its own mean.
        let ac_rms = |w: &[f32]| {
            let mean = w.iter().sum::<f32>() / w.len().max(1) as f32;
            rms(&w.iter().map(|v| v - mean).collect::<Vec<_>>())
        };
        let probe = |state: &mut TriodeState, seconds: f32, amp: f32| {
            let n = (rate * seconds) as usize;
            (0..n)
                .map(|k| state.process(tone(k, amp), &p, 6.0, 0.999))
                .collect::<Vec<_>>()
        };

        let settled = probe(&mut state, 0.30, 0.05);
        let reference = ac_rms(&settled[(rate * 0.2) as usize..]);
        // Slam it, then go quiet again.
        probe(&mut state, 0.05, 6.0);
        let recovery = probe(&mut state, 0.40, 0.05);
        let just_after = ac_rms(&recovery[(rate * 0.01) as usize..(rate * 0.03) as usize]);
        let long_after = ac_rms(&recovery[(rate * 0.30) as usize..]);

        assert!(
            just_after < reference * 0.9,
            "the stage should choke right after a slam: {just_after} vs {reference}"
        );
        assert!(
            long_after > just_after * 1.05,
            "and recover afterwards: {long_after} vs {just_after}"
        );
    }

    #[test]
    fn push_pull_supply_sag_compresses_peaks_not_small_signal() {
        let mut p = PushPullParams {
            bias: 0.55,
            knee: 0.04,
            headroom: 2.4,
            sat_knee: 0.05,
            asymmetry: 0.06,
            scale: 1.0,
        };
        p.normalize();
        let small_full = p.process(0.01, 1.0);
        let small_sag = p.process(0.01, 0.6);
        let loud_full = p.process(3.0, 1.0);
        let loud_sag = p.process(3.0, 0.6);
        assert!(
            (small_full - small_sag).abs() < small_full.abs() * 0.05 + 1.0e-4,
            "sag must not act as a volume control on quiet signal"
        );
        assert!(
            loud_sag < loud_full * 0.9,
            "sag must compress the peaks: {loud_sag} vs {loud_full}"
        );
    }
}
