//! Track loopback: one track's output as another track's input.
//!
//! ```txt
//! Instrument track (VSTi) ──▶ post-fader block ──▶ Audio track input
//!                                                  monitor · meter · record
//! ```
//!
//! This is what lets an instrument be recorded as audio without a bounce: point
//! an audio track's Input at the instrument track, arm it, and the take is the
//! instrument's own output — through its inserts and its fader, the same signal
//! an export would capture.
//!
//! # Why it is a device id
//!
//! A loopback source is selected through Audio Connections like any other
//! input, under a device id prefix — exactly the way a remote Audio Jam stream
//! is (see [`crate::jam_bus::JAM_DEVICE_PREFIX`]). That keeps one routing model
//! rather than a second, private one: the same stable ids, the same
//! non-destructive behaviour when the source disappears, the same Input menu.
//!
//! # Timing
//!
//! The destination reads what the source rendered in the **previous** callback,
//! so a loopback costs exactly one buffer of latency and nothing else.
//!
//! That is deliberate, and it is what makes the feature safe. A track's input
//! is mixed before any track has rendered, so reading the source's *current*
//! block would read silence; ordering the render around it would need a
//! topological sort of the track graph and a rule for what to do about a cycle.
//! With a one-block delay there is no ordering constraint and no cycle to
//! resolve — routing A into B and B into A is simply a delay line, not a hang.
//! At 256 frames and 48 kHz that is 5.3 ms.

/// Device-id prefix that marks an Audio Connections port as another track's
/// output rather than a hardware input.
///
/// Deliberately not `jam:`: `jam_stream_id` strips that prefix to recover a
/// stream id, and a loopback source is a track id.
pub const LOOPBACK_DEVICE_PREFIX: &str = "trk:";

/// Build the Audio Connections device id for one source track.
pub fn loopback_device_id(track_id: &str) -> String {
    format!("{LOOPBACK_DEVICE_PREFIX}{track_id}")
}

/// The source track id inside a loopback device id, or `None` for anything
/// else.
pub fn loopback_track_id(device_id: &str) -> Option<&str> {
    device_id
        .strip_prefix(LOOPBACK_DEVICE_PREFIX)
        .filter(|id| !id.is_empty())
}

/// Whether a device id names a track loopback source.
pub fn is_loopback_device(device_id: &str) -> bool {
    loopback_track_id(device_id).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_loopback_device_id_round_trips_to_its_track() {
        let id = loopback_device_id("trk_01J8");
        assert_eq!(id, "trk:trk_01J8");
        assert!(is_loopback_device(&id));
        assert_eq!(loopback_track_id(&id), Some("trk_01J8"));
    }

    /// The two virtual device families must never be mistaken for one another:
    /// a jam id resolves to a remote stream and a loopback id to a local track,
    /// and feeding one to the other's lookup would silently route the wrong
    /// audio.
    #[test]
    fn a_jam_stream_is_not_a_loopback_source_and_the_reverse() {
        let jam = crate::jam_bus::jam_device_id("str_1");
        assert!(!is_loopback_device(&jam));
        assert!(loopback_track_id(&jam).is_none());

        let loopback = loopback_device_id("trk_1");
        assert!(!crate::jam_bus::is_jam_device(&loopback));
        assert!(crate::jam_bus::jam_stream_id(&loopback).is_none());
    }

    #[test]
    fn hardware_and_malformed_ids_are_refused() {
        assert!(!is_loopback_device("focusrite"));
        assert!(!is_loopback_device("trk:"));
        assert!(loopback_track_id("trk").is_none());
    }
}
