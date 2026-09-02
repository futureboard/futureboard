//! The session clock.
//!
//! A jam's timeline is a sample counter, not a wall clock: one tick is one
//! sample at 48 kHz by default, and every stream converts into that domain
//! regardless of the rate it runs at. Packet arrival time is never the audio
//! timeline — it carries the jitter of the network, and a recording built on it
//! would drift against every other take in the project.
//!
//! The sync exchange is the NTP/PTP shape, mirroring the server's
//! `internal/clock.Measure`:
//!
//! ```text
//! t1  client transmit    (echoed by the server)
//! t2  server receive
//! t3  server transmit
//! t4  client receive
//!
//! rtt    = (t4 - t1) - (t3 - t2)
//! offset = ((t2 - t1) + (t3 - t4)) / 2
//! ```
//!
//! Subtracting `(t3 - t2)` is what makes the result usable under load: a busy
//! server delays `t3`, and without the subtraction that delay is misread as
//! network latency and inflates every client's jitter buffer.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// One clock exchange, in nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClockMeasurement {
    /// Round trip with the server's own processing time removed.
    pub rtt_nanos: i64,
    /// How far the server's clock is ahead of this client's.
    pub offset_nanos: i64,
}

impl ClockMeasurement {
    pub fn rtt_millis(&self) -> f64 {
        self.rtt_nanos as f64 / 1e6
    }

    pub fn offset_millis(&self) -> f64 {
        self.offset_nanos as f64 / 1e6
    }
}

/// Compute one measurement from the four timestamps.
pub fn measure(t1: i64, t2: i64, t3: i64, t4: i64) -> ClockMeasurement {
    ClockMeasurement {
        rtt_nanos: (t4 - t1) - (t3 - t2),
        offset_nanos: ((t2 - t1) + (t3 - t4)) / 2,
    }
}

/// How many samples the client's estimate is allowed to move in one step when
/// it is already locked.
///
/// A single bad exchange — a packet that queued behind a big HTTP response, a
/// laptop waking from sleep — must not yank the timeline. Slewing means a real
/// change still arrives, over a few exchanges, rather than as a jump that would
/// tear a recording.
const MAX_SLEW_NANOS: i64 = 2_000_000;

/// How many samples the estimator keeps for the drift fit. Ten exchanges at the
/// five-second sync interval is just under a minute, which is long enough to
/// see a real sample-clock drift and short enough to follow a machine that has
/// just been re-synced by NTP.
const DRIFT_WINDOW: usize = 10;

/// Tracks the offset between this client's monotonic clock and the jam's
/// session clock, and how fast the two are drifting apart.
#[derive(Debug)]
pub struct SessionClock {
    /// Ticks per second. 48 000 unless the jam says otherwise.
    rate: u32,
    /// The reference point: this monotonic instant was this session tick.
    anchor: Option<Anchor>,
    /// Best estimate of the server-minus-client offset, in nanoseconds.
    offset_nanos: i64,
    /// Whether an estimate exists at all. Before the first exchange, callers
    /// must be able to tell "offset is zero" from "offset is unknown".
    locked: bool,
    /// Smoothed round trip.
    rtt_nanos: i64,
    /// Measured drift of the local clock against the session clock, in parts
    /// per million, positive when the local clock runs fast.
    drift_ppm: f64,
    /// Recent (local nanos, offset nanos) samples for the drift fit.
    history: Vec<(i64, i64)>,
    /// Total exchanges applied, for diagnostics.
    samples: u64,
}

#[derive(Debug, Clone, Copy)]
struct Anchor {
    local_nanos: i64,
    session_ticks: i64,
}

impl Default for SessionClock {
    fn default() -> Self {
        Self::new(crate::protocol::DEFAULT_CLOCK_RATE)
    }
}

impl SessionClock {
    pub fn new(rate: u32) -> Self {
        Self {
            rate: if rate == 0 {
                crate::protocol::DEFAULT_CLOCK_RATE
            } else {
                rate
            },
            anchor: None,
            offset_nanos: 0,
            locked: false,
            rtt_nanos: 0,
            drift_ppm: 0.0,
            history: Vec::with_capacity(DRIFT_WINDOW),
            samples: 0,
        }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn set_rate(&mut self, rate: u32) {
        if rate != 0 && rate != self.rate {
            // The tick domain changed under us; every anchor and every drift
            // sample was expressed in the old one.
            self.rate = rate;
            self.anchor = None;
            self.history.clear();
        }
    }

    pub fn locked(&self) -> bool {
        self.locked
    }

    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// Nanoseconds of round trip, smoothed.
    pub fn rtt_nanos(&self) -> i64 {
        self.rtt_nanos
    }

    pub fn rtt_millis(&self) -> f64 {
        self.rtt_nanos as f64 / 1e6
    }

    pub fn offset_nanos(&self) -> i64 {
        self.offset_nanos
    }

    pub fn offset_millis(&self) -> f64 {
        self.offset_nanos as f64 / 1e6
    }

    /// Offset expressed in session ticks, which is what stream latency metadata
    /// carries.
    pub fn offset_ticks(&self) -> i64 {
        nanos_to_ticks(self.offset_nanos, self.rate)
    }

    pub fn drift_ppm(&self) -> f64 {
        self.drift_ppm
    }

    /// Apply one exchange.
    ///
    /// `local_nanos` is the client's own receive instant (`t4`), so the anchor
    /// is placed at a moment the caller actually observed rather than at
    /// "now, some time after the reply was handled".
    pub fn apply(
        &mut self,
        measurement: ClockMeasurement,
        server_session_ticks: i64,
        local_nanos: i64,
    ) {
        self.samples = self.samples.saturating_add(1);

        if !self.locked {
            self.offset_nanos = measurement.offset_nanos;
            self.rtt_nanos = measurement.rtt_nanos.max(0);
            self.locked = true;
        } else {
            // Exponential smoothing on the round trip; slew-limited correction
            // on the offset.
            self.rtt_nanos = (self.rtt_nanos * 3 + measurement.rtt_nanos.max(0)) / 4;
            let delta = measurement.offset_nanos - self.offset_nanos;
            let step = delta.clamp(-MAX_SLEW_NANOS, MAX_SLEW_NANOS);
            self.offset_nanos += step;
        }

        // The session tick at `t4` is what the server reported at `t3` plus the
        // half round trip it took to reach us. Using the reported tick directly
        // would place every anchor one network hop in the past.
        let one_way_nanos = (measurement.rtt_nanos.max(0)) / 2;
        let ticks_at_receive = server_session_ticks + nanos_to_ticks(one_way_nanos, self.rate);
        self.anchor = Some(Anchor {
            local_nanos,
            session_ticks: ticks_at_receive,
        });

        self.push_drift_sample(local_nanos, measurement.offset_nanos);
    }

    /// Estimate the session tick for a local monotonic instant.
    ///
    /// `None` until the first exchange lands: guessing would put a recorded
    /// take at an arbitrary place on the timeline, which is worse than not
    /// placing it at all.
    pub fn session_ticks_at(&self, local_nanos: i64) -> Option<i64> {
        let anchor = self.anchor?;
        let elapsed = local_nanos - anchor.local_nanos;
        // Correct the elapsed span by the measured drift before converting: at
        // 20 ppm a ten-minute take is twelve milliseconds out, which is audible
        // as a flam against a click.
        let corrected = elapsed as f64 * (1.0 - self.drift_ppm / 1e6);
        Some(anchor.session_ticks + nanos_to_ticks(corrected as i64, self.rate))
    }

    /// The inverse: which local instant a session tick corresponds to.
    pub fn local_nanos_at(&self, session_ticks: i64) -> Option<i64> {
        let anchor = self.anchor?;
        let delta_ticks = session_ticks - anchor.session_ticks;
        let nanos = ticks_to_nanos(delta_ticks, self.rate) as f64 / (1.0 - self.drift_ppm / 1e6);
        Some(anchor.local_nanos + nanos as i64)
    }

    /// Convert a session tick count into a sample position at a project rate.
    ///
    /// This is the whole point of the tick domain: a 48 kHz jam and a 96 kHz
    /// project agree on when a note happened without either changing its rate.
    pub fn ticks_to_project_samples(&self, ticks: i64, project_sample_rate: u32) -> i64 {
        if self.rate == 0 || project_sample_rate == 0 {
            return 0;
        }
        (ticks as i128 * project_sample_rate as i128 / self.rate as i128) as i64
    }

    /// The reverse conversion.
    pub fn project_samples_to_ticks(&self, samples: i64, project_sample_rate: u32) -> i64 {
        if project_sample_rate == 0 {
            return 0;
        }
        (samples as i128 * self.rate as i128 / project_sample_rate as i128) as i64
    }

    fn push_drift_sample(&mut self, local_nanos: i64, offset_nanos: i64) {
        self.history.push((local_nanos, offset_nanos));
        if self.history.len() > DRIFT_WINDOW {
            self.history.remove(0);
        }
        if self.history.len() < 3 {
            return;
        }
        // Least-squares slope of offset against local time. The slope is
        // seconds of offset per second of elapsed time, which is parts per one;
        // scaling by 1e6 gives ppm.
        let n = self.history.len() as f64;
        let (mut sum_x, mut sum_y, mut sum_xy, mut sum_xx) = (0.0, 0.0, 0.0, 0.0);
        let base = self.history[0].0;
        for &(x, y) in &self.history {
            let x = (x - base) as f64;
            let y = y as f64;
            sum_x += x;
            sum_y += y;
            sum_xy += x * y;
            sum_xx += x * x;
        }
        let denominator = n * sum_xx - sum_x * sum_x;
        if denominator.abs() < f64::EPSILON {
            return;
        }
        let slope = (n * sum_xy - sum_x * sum_y) / denominator;
        // A positive slope means the server is pulling ahead of us, i.e. the
        // local clock is slow, so the reported drift is the negation.
        self.drift_ppm = -slope * 1e6;
    }
}

/// The client's own timestamp for a sync exchange: Unix nanoseconds that
/// advance monotonically.
///
/// Both halves of that matter, and neither clock gives you both on its own.
///
/// The **epoch has to be Unix**, because the server reports `t2` and `t3` as
/// `UnixNano` and the offset is a subtraction across the two sides. Feeding the
/// formula a process-relative `t1` produces an "offset" of however long the
/// process has been running short of 1970 — a number that looks like decades
/// and would place every remote take at an absurd position.
///
/// The **rate has to be monotonic**, because a mid-session NTP correction would
/// otherwise appear as the jam's timeline jumping.
///
/// So the Unix epoch is sampled once, at first use, and every reading after
/// that is that instant plus elapsed monotonic time.
pub fn client_nanos() -> i64 {
    use std::sync::OnceLock;
    static ORIGIN: OnceLock<(i64, Instant)> = OnceLock::new();
    let (base_unix, origin) = ORIGIN.get_or_init(|| (unix_nanos(), Instant::now()));
    base_unix.saturating_add(origin.elapsed().as_nanos() as i64)
}

/// Wall-clock nanoseconds, used only where the server's own epoch is being
/// interpreted for display.
pub fn unix_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// Convert nanoseconds to session ticks at `rate`.
pub fn nanos_to_ticks(nanos: i64, rate: u32) -> i64 {
    (nanos as i128 * rate as i128 / 1_000_000_000i128) as i64
}

/// Convert session ticks at `rate` to nanoseconds.
pub fn ticks_to_nanos(ticks: i64, rate: u32) -> i64 {
    if rate == 0 {
        return 0;
    }
    (ticks as i128 * 1_000_000_000i128 / rate as i128) as i64
}

/// How often to run a clock exchange once joined. One sync is a sample; the
/// useful numbers come from watching them over time, and this matches the web
/// listener so both clients converge at the same rate.
pub const SYNC_INTERVAL: Duration = Duration::from_secs(5);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exchange_removes_the_servers_own_processing_time() {
        // 20 ms out and back, 10 ms of it spent inside the server.
        let m = measure(0, 15_000_000, 25_000_000, 30_000_000);
        assert_eq!(m.rtt_nanos, 20_000_000);
        // Server is 5 ms ahead: ((15 - 0) + (25 - 30)) / 2.
        assert_eq!(m.offset_nanos, 5_000_000);
    }

    #[test]
    fn a_symmetric_path_with_no_offset_measures_none() {
        let m = measure(0, 5_000_000, 5_000_000, 10_000_000);
        assert_eq!(m.offset_nanos, 0);
        assert_eq!(m.rtt_nanos, 10_000_000);
    }

    #[test]
    fn the_clock_is_unknown_until_the_first_exchange() {
        let clock = SessionClock::default();
        assert!(!clock.locked());
        assert!(clock.session_ticks_at(client_nanos()).is_none());
    }

    #[test]
    fn client_timestamps_share_the_servers_epoch_and_still_advance_monotonically() {
        let first = client_nanos();
        let wall = unix_nanos();
        // Within a second of wall time: the same epoch, so an offset computed
        // against the server's UnixNano timestamps is a real number.
        assert!(
            (first - wall).abs() < 1_000_000_000,
            "client clock is {} ns from wall time",
            first - wall
        );
        assert!(client_nanos() >= first, "readings never go backwards");
    }

    #[test]
    fn the_first_exchange_anchors_the_session_tick_at_the_receive_instant() {
        let mut clock = SessionClock::new(48_000);
        // 20 ms round trip, so the server's reported tick is 10 ms old.
        let m = measure(0, 15_000_000, 25_000_000, 30_000_000);
        clock.apply(m, 20_000_000, 30_000_000);
        assert!(clock.locked());
        let ticks = clock
            .session_ticks_at(30_000_000)
            .expect("locked clocks answer");
        // 10 ms at 48 kHz is 480 samples.
        assert_eq!(ticks, 20_000_000 + 480);
    }

    #[test]
    fn one_wild_exchange_cannot_yank_a_locked_clock() {
        let mut clock = SessionClock::new(48_000);
        clock.apply(measure(0, 5_000_000, 5_000_000, 10_000_000), 0, 10_000_000);
        let before = clock.offset_nanos();

        // A reply that queued behind something big: half a second of apparent
        // offset in one step.
        clock.apply(
            measure(0, 500_000_000, 500_000_000, 10_000_000),
            0,
            20_000_000,
        );
        let moved = (clock.offset_nanos() - before).abs();
        assert!(
            moved <= MAX_SLEW_NANOS,
            "offset moved {moved} ns in one exchange"
        );
    }

    #[test]
    fn a_steady_offset_ramp_is_reported_as_drift() {
        let mut clock = SessionClock::new(48_000);
        // The server pulls ahead by 100 µs every second: the local clock is
        // 100 ppm slow, so the reported drift is -100 ppm.
        for i in 0..DRIFT_WINDOW as i64 {
            let local = i * 1_000_000_000;
            let offset = i * 100_000;
            clock.apply(
                ClockMeasurement {
                    rtt_nanos: 0,
                    offset_nanos: offset,
                },
                0,
                local,
            );
        }
        assert!(
            (clock.drift_ppm() + 100.0).abs() < 1.0,
            "drift was {} ppm",
            clock.drift_ppm()
        );
    }

    #[test]
    fn ticks_convert_between_the_jam_and_the_project_rate() {
        let clock = SessionClock::new(48_000);
        // One second of session clock is 96 000 samples in a 96 kHz project.
        assert_eq!(clock.ticks_to_project_samples(48_000, 96_000), 96_000);
        assert_eq!(clock.ticks_to_project_samples(48_000, 44_100), 44_100);
        assert_eq!(clock.project_samples_to_ticks(96_000, 96_000), 48_000);
        // And the conversion does not overflow at a realistic session length:
        // twelve hours of 192 kHz samples.
        let samples = 192_000i64 * 3600 * 12;
        assert_eq!(
            clock.project_samples_to_ticks(samples, 192_000),
            48_000i64 * 3600 * 12
        );
    }

    #[test]
    fn tick_and_nanosecond_conversion_round_trips() {
        assert_eq!(nanos_to_ticks(1_000_000_000, 48_000), 48_000);
        assert_eq!(ticks_to_nanos(48_000, 48_000), 1_000_000_000);
        assert_eq!(ticks_to_nanos(0, 0), 0);
    }

    #[test]
    fn a_rate_change_drops_the_anchor_rather_than_reinterpreting_it() {
        let mut clock = SessionClock::new(48_000);
        clock.apply(measure(0, 0, 0, 0), 1_000, 0);
        assert!(clock.session_ticks_at(0).is_some());
        clock.set_rate(96_000);
        assert!(clock.session_ticks_at(0).is_none());
        assert_eq!(clock.rate(), 96_000);
    }
}
