//! What a jam stream looks like on the wire, and what that costs.
//!
//! Three knobs, and they are the three the protocol actually carries per
//! stream: bit depth, sample rate, and channel layout. There is no bitrate
//! slider because there is no compressed codec to slide — this build publishes
//! PCM, so the bitrate is not a setting but an arithmetic consequence of the
//! other three, and it is shown rather than asked for.
//!
//! That is worth being blunt about in the UI. A user who picks "32-bit float,
//! 96 kHz, sixteen channels" has chosen 49 Mbit/s, and the only honest thing to
//! do is put the number in front of them before they press Send.

use sphere_jam_client::bridge::{datagram_frame_sizes, DATAGRAM_PAYLOAD_BYTES};
use sphere_jam_client::protocol::SampleFormat;

/// Bit depths a jam stream can be published at.
///
/// All three are lossless — PCM is PCM — so this is a bandwidth control, not a
/// quality one in the way a compressed codec's bitrate is. 24-bit is the
/// default because it is transparent for anything a DAW sends and costs a
/// quarter less than float.
pub const SAMPLE_FORMATS: [SampleFormat; 3] = [
    SampleFormat::S16Le,
    SampleFormat::S24Le,
    SampleFormat::F32Le,
];

/// Rates the server's negotiator will select. The project's own rate is
/// unaffected by the choice: the publish tap converts.
pub const SAMPLE_RATES: [u32; 4] = [44_100, 48_000, 88_200, 96_000];

/// What this Studio publishes with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JamPublishQuality {
    pub sample_format: SampleFormat,
    pub sample_rate: u32,
    /// Whether the published master mix carries the metronome click.
    pub master_click: bool,
    /// Which of the two things this Studio sends.
    pub stream_mode: JamStreamMode,
}

impl Default for JamPublishQuality {
    fn default() -> Self {
        Self {
            // Float, because it is the one depth *every* receiver declares.
            //
            // 24-bit is the better trade on the wire — transparent for a mix
            // and a quarter less bandwidth — and it is offered. It is not the
            // default because the server does not transcode: a receiver that
            // did not list the publisher's depth is refused at negotiation and
            // then waits for a format that never comes, with no audio and
            // nothing on screen to say why. A default must never be able to do
            // that, so it is the one every client can take.
            sample_format: SampleFormat::F32Le,
            sample_rate: 48_000,
            // A jam runs to a count. A guest who cannot hear the click is
            // playing to a mix that appears to have no pulse, so the metronome
            // is in the stream unless someone says otherwise.
            master_click: true,
            stream_mode: JamStreamMode::MasterStereo,
        }
    }
}

/// What this Studio sends into the room.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JamStreamMode {
    /// The master mix, as two channels. What a listener wants, and what a
    /// performer plays along to.
    #[default]
    MasterStereo,
    /// The arrangement, one channel pair per track, as a single wide stream.
    /// What another Studio wants when it is going to record the take.
    Multitrack,
}

impl JamStreamMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::MasterStereo => "Master L/R",
            Self::Multitrack => "Multitrack",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Self::MasterStereo => "The mix, as everyone hears it",
            Self::Multitrack => "One channel pair per track",
        }
    }
}

/// What a depth is called in a menu.
pub fn sample_format_label(format: SampleFormat) -> &'static str {
    match format {
        SampleFormat::S16Le => "16-bit",
        SampleFormat::S24Le => "24-bit",
        SampleFormat::F32Le => "32-bit float",
        SampleFormat::None => "—",
    }
}

/// Depths every jam client declares, including the browser listener.
///
/// The server does not transcode, so a publisher that picks a depth outside
/// this set is choosing to be undecodable by anything that did not list it.
/// That is a legitimate Studio-to-Studio choice and a bad default, which is
/// what this constant is for: the panel says which depths are safe rather than
/// leaving the user to discover it as silence.
pub const UNIVERSAL_SAMPLE_FORMATS: [SampleFormat; 2] = [SampleFormat::S16Le, SampleFormat::F32Le];

/// Rates every jam client declares, including the browser listener.
///
/// The listener pins its `AudioContext` to 48 kHz and has no resampler, so the
/// higher rates are genuinely a Studio-to-Studio choice rather than a
/// conservative guess about browsers.
pub const UNIVERSAL_SAMPLE_RATES: [u32; 2] = [44_100, 48_000];

/// Whether every listener can decode this depth.
pub fn is_universally_decodable(format: SampleFormat) -> bool {
    UNIVERSAL_SAMPLE_FORMATS.contains(&format)
}

/// What a web listener will not be able to take at this setting, if anything.
///
/// One sentence rather than a badge per control, because the two constraints
/// have the same cause and the same consequence: the server does not transcode,
/// so a listener that did not declare the format is refused at negotiation and
/// then waits for a format that never arrives. The panel has to say that before
/// the fact — after it, there is nothing on any screen to say it at all.
pub fn web_listener_note(quality: &JamPublishQuality) -> Option<&'static str> {
    let depth_ok = is_universally_decodable(quality.sample_format);
    let rate_ok = UNIVERSAL_SAMPLE_RATES.contains(&quality.sample_rate);
    match (depth_ok, rate_ok) {
        (true, true) => None,
        (false, true) => Some("Web listeners take only 16-bit or float"),
        (true, false) => Some("Web listeners take only 44.1 or 48 kHz"),
        (false, false) => Some("Web listeners take 16-bit or float at 44.1 or 48 kHz"),
    }
}

/// A rate, as a menu reads it: `48 kHz`, `44.1 kHz`.
pub fn sample_rate_label(rate: u32) -> String {
    if rate % 1000 == 0 {
        format!("{} kHz", rate / 1000)
    } else {
        format!("{:.1} kHz", rate as f64 / 1000.0)
    }
}

/// What one stream at this format actually costs, so the UI can say so.
///
/// Every field is derived, never chosen. `frame_samples` is the frame length
/// the server will negotiate for the layout, which is what decides the packet
/// rate — and the packet rate, not the bitrate, is what makes a wide
/// uncompressed layout hard on a network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamCost {
    pub channels: usize,
    pub bits_per_second: u64,
    pub frame_samples: u32,
    pub packets_per_second: u32,
    pub payload_bytes: u32,
    /// Whether a frame of this layout fits one datagram at all. When it does
    /// not, the server refuses the stream, and the UI has to say so before the
    /// user presses Send rather than after.
    pub fits_datagram: bool,
}

impl StreamCost {
    /// The cost of publishing `channels` channels at this quality.
    pub fn of(quality: &JamPublishQuality, channels: usize) -> Self {
        let channels = channels.max(1);
        let bytes_per_sample = quality.sample_format.bytes_per_sample().max(1);
        let bits_per_second =
            quality.sample_rate as u64 * channels as u64 * bytes_per_sample as u64 * 8;

        // Stereo takes the session-wide default; a wide layout states its own,
        // and the two have to agree with what `JamPublishRequest` sends or the
        // number on screen describes a stream nobody publishes.
        let frame_samples = if channels <= 2 {
            STEREO_FRAME_SAMPLES
        } else {
            datagram_frame_sizes(channels, quality.sample_format)
                .first()
                .copied()
                .unwrap_or(0) as u32
        };
        let payload_bytes = frame_samples * (channels * bytes_per_sample) as u32;
        let packets_per_second = if frame_samples == 0 {
            0
        } else {
            quality.sample_rate.div_ceil(frame_samples)
        };

        Self {
            channels,
            bits_per_second,
            frame_samples,
            packets_per_second,
            payload_bytes,
            fits_datagram: frame_samples > 0 && payload_bytes as usize <= DATAGRAM_PAYLOAD_BYTES,
        }
    }

    /// The bitrate as a menu reads it: `2.3 Mbit/s`.
    pub fn bitrate_label(&self) -> String {
        let megabits = self.bits_per_second as f64 / 1_000_000.0;
        if megabits >= 10.0 {
            format!("{megabits:.0} Mbit/s")
        } else if megabits >= 1.0 {
            format!("{megabits:.1} Mbit/s")
        } else {
            format!("{:.0} kbit/s", self.bits_per_second as f64 / 1000.0)
        }
    }

    /// The whole cost as one caption line.
    pub fn summary(&self) -> String {
        if !self.fits_datagram {
            return "Too wide for one network packet".to_string();
        }
        format!(
            "{} · {} frames · {}/s packets",
            self.bitrate_label(),
            self.frame_samples,
            self.packets_per_second
        )
    }
}

/// The frame length a stereo stream negotiates: the smallest the session-wide
/// capability list offers, which is what the server's negotiator picks.
///
/// It has to match `JamSessionOptions::studio_capabilities`. A constant rather
/// than a read of that list because this is a display value and must not depend
/// on a session existing.
const STEREO_FRAME_SAMPLES: u32 = 128;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stereo_master_reports_the_bitrate_it_actually_sends() {
        let quality = JamPublishQuality::default();
        let cost = StreamCost::of(&quality, 2);
        // 48 kHz, 32-bit float, two channels: 48000 * 2 * 4 * 8.
        assert_eq!(cost.bits_per_second, 3_072_000);
        assert_eq!(cost.bitrate_label(), "3.1 Mbit/s");
        assert_eq!(cost.frame_samples, STEREO_FRAME_SAMPLES);
        assert!(cost.fits_datagram);
    }

    /// The regression this default exists to prevent: a depth no receiver
    /// listed is refused at negotiation, and a refused receiver is told
    /// nothing — it simply waits for a format that never arrives.
    #[test]
    fn the_default_depth_is_one_every_receiver_declares() {
        assert_eq!(
            JamPublishQuality::default().sample_format,
            SampleFormat::F32Le
        );
        assert!(
            UNIVERSAL_SAMPLE_FORMATS.contains(&JamPublishQuality::default().sample_format),
            "the default must be decodable by every listener, not only by Studio"
        );
    }

    #[test]
    fn a_wide_layout_is_reported_as_unsendable_rather_than_merely_expensive() {
        let quality = JamPublishQuality {
            sample_format: SampleFormat::F32Le,
            ..Default::default()
        };
        // Sixteen channels of float is 64 bytes a frame; nothing fits.
        let cost = StreamCost::of(&quality, 16);
        assert!(!cost.fits_datagram);
        assert_eq!(cost.summary(), "Too wide for one network packet");

        // The same layout at 16-bit is 32 bytes a frame, and does fit.
        let smaller = JamPublishQuality {
            sample_format: SampleFormat::S16Le,
            ..Default::default()
        };
        let cost = StreamCost::of(&smaller, 16);
        assert!(cost.fits_datagram);
        assert_eq!(cost.frame_samples, 32);
        assert!(cost.payload_bytes as usize <= DATAGRAM_PAYLOAD_BYTES);
        // 48 kHz in 32-sample packets is a hard 1500 a second, which is exactly
        // the number the panel has to show before anybody presses Send.
        assert_eq!(cost.packets_per_second, 1_500);
    }

    #[test]
    fn a_setting_no_web_listener_can_take_says_so_before_it_is_sent() {
        // The default reaches everybody and says nothing.
        assert_eq!(web_listener_note(&JamPublishQuality::default()), None);

        let deep = JamPublishQuality {
            sample_format: SampleFormat::S24Le,
            ..Default::default()
        };
        assert_eq!(
            web_listener_note(&deep),
            Some("Web listeners take only 16-bit or float")
        );

        let fast = JamPublishQuality {
            sample_rate: 96_000,
            ..Default::default()
        };
        assert_eq!(
            web_listener_note(&fast),
            Some("Web listeners take only 44.1 or 48 kHz")
        );

        let both = JamPublishQuality {
            sample_format: SampleFormat::S24Le,
            sample_rate: 88_200,
            ..Default::default()
        };
        assert_eq!(
            web_listener_note(&both),
            Some("Web listeners take 16-bit or float at 44.1 or 48 kHz")
        );
    }

    #[test]
    fn depth_and_rate_read_the_way_a_menu_needs_them_to() {
        assert_eq!(sample_format_label(SampleFormat::S24Le), "24-bit");
        assert_eq!(sample_rate_label(48_000), "48 kHz");
        assert_eq!(sample_rate_label(44_100), "44.1 kHz");
    }

    #[test]
    fn the_default_carries_the_click_because_a_jam_runs_to_a_count() {
        assert!(JamPublishQuality::default().master_click);
        assert_eq!(
            JamPublishQuality::default().stream_mode,
            JamStreamMode::MasterStereo
        );
    }
}
