//! The media-plane wire format, mirrored from the server's `internal/media`.
//!
//! Two frame kinds, told apart by the second byte: 0 is control, anything else
//! is a codec id and the frame is audio. Everything multi-byte is big-endian.
//!
//! Nothing in this module allocates on the decode path and nothing panics on a
//! malformed frame — both are hard requirements, because these functions run on
//! the media receive thread for every packet a jam produces, and a hostile or
//! merely broken peer must not be able to take the process down.

use crate::error::{JamError, Result};
use crate::protocol::{AudioCodec, SampleFormat};

/// Current media header version. A node rejects anything else rather than
/// guessing at a layout, and so do we.
pub const PACKET_VERSION: u8 = 1;

/// Fixed size of an encoded audio header.
pub const HEADER_SIZE: usize = 40;

/// Fixed prefix of a control frame.
pub const CONTROL_HEADER_SIZE: usize = 8;

/// Bound on a control payload. Handshakes are small; anything larger is a bug
/// or an attempt to make the receiver allocate.
pub const MAX_CONTROL_PAYLOAD: usize = 4 << 10;

/// Largest media frame this client will read off a stream transport. It matches
/// the server's own stream-framing bound.
pub const MAX_MEDIA_FRAME: usize = 64 << 10;

// Header field offsets.
const OFF_VERSION: usize = 0;
const OFF_CODEC: usize = 1;
const OFF_FLAGS: usize = 2;
const OFF_JAM_ID: usize = 4;
const OFF_STREAM_ID: usize = 12;
const OFF_GENERATION: usize = 16;
const OFF_SEQUENCE: usize = 20;
const OFF_CAPTURE: usize = 28;
const OFF_FRAMES: usize = 36;
const OFF_CHANNELS: usize = 38;
const OFF_RESERVED: usize = 39;

/// Marks the first packet of a take, so a recorder can align the start of a
/// region without waiting for the transport to settle.
pub const FLAG_MARKER: u16 = 1 << 0;
/// A discontinuous-transmission gap: the payload is empty and the receiver
/// should insert silence rather than conceal a loss.
pub const FLAG_SILENCE: u16 = 1 << 1;
/// The publisher's final packet.
pub const FLAG_END_OF_STREAM: u16 = 1 << 2;
/// A retransmission or a redundant copy sent over a second transport during a
/// migration. Receivers deduplicate by sequence.
pub const FLAG_REDUNDANT: u16 = 1 << 3;

const KIND_CONTROL: u8 = 0;
const CODEC_WIRE_PCM: u8 = 1;
const CODEC_WIRE_AAC_LC: u8 = 2;

/// The fixed header in front of every media payload.
///
/// `jam_alias` and `stream_alias` are the compact numeric aliases, not the
/// control plane's ULIDs. The mapping is handed out at publish time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPacketHeader {
    pub version: u8,
    pub codec: AudioCodec,
    pub flags: u16,
    pub jam_alias: u64,
    pub stream_alias: u32,
    /// The publisher's connection generation. A node drops packets carrying a
    /// generation older than the participant's current one.
    pub generation: u32,
    /// Per stream and per generation, incrementing by one per packet.
    pub sequence: u64,
    /// The session tick at which the first sample in this packet was captured.
    /// Forwarded untouched by the server: it is the anchor a remote take is
    /// aligned against, and it is never the packet's arrival time.
    pub capture_timestamp: u64,
    /// Samples per channel in this packet.
    pub frames: u16,
    pub channels: u8,
}

impl Default for AudioPacketHeader {
    fn default() -> Self {
        Self {
            version: PACKET_VERSION,
            codec: AudioCodec::Pcm,
            flags: 0,
            jam_alias: 0,
            stream_alias: 0,
            generation: 0,
            sequence: 0,
            capture_timestamp: 0,
            frames: 0,
            channels: 0,
        }
    }
}

impl AudioPacketHeader {
    pub fn has(&self, flag: u16) -> bool {
        self.flags & flag != 0
    }

    /// Write the header into `dst`, which must be at least [`HEADER_SIZE`].
    pub fn encode(&self, dst: &mut [u8]) -> Result<()> {
        if dst.len() < HEADER_SIZE {
            return Err(JamError::Codec(format!(
                "media header needs {HEADER_SIZE} bytes, got {}",
                dst.len()
            )));
        }
        let version = if self.version == 0 {
            PACKET_VERSION
        } else {
            self.version
        };
        dst[OFF_VERSION] = version;
        dst[OFF_CODEC] = codec_wire(self.codec);
        dst[OFF_FLAGS..OFF_FLAGS + 2].copy_from_slice(&self.flags.to_be_bytes());
        dst[OFF_JAM_ID..OFF_JAM_ID + 8].copy_from_slice(&self.jam_alias.to_be_bytes());
        dst[OFF_STREAM_ID..OFF_STREAM_ID + 4].copy_from_slice(&self.stream_alias.to_be_bytes());
        dst[OFF_GENERATION..OFF_GENERATION + 4].copy_from_slice(&self.generation.to_be_bytes());
        dst[OFF_SEQUENCE..OFF_SEQUENCE + 8].copy_from_slice(&self.sequence.to_be_bytes());
        dst[OFF_CAPTURE..OFF_CAPTURE + 8].copy_from_slice(&self.capture_timestamp.to_be_bytes());
        dst[OFF_FRAMES..OFF_FRAMES + 2].copy_from_slice(&self.frames.to_be_bytes());
        dst[OFF_CHANNELS] = self.channels;
        dst[OFF_RESERVED] = 0;
        Ok(())
    }
}

/// Whether a frame is control (`Ok(true)`), audio (`Ok(false)`), or unreadable.
pub fn frame_is_control(frame: &[u8]) -> Result<bool> {
    if frame.len() < 2 {
        return Err(JamError::Codec(format!(
            "media frame is {} bytes; at least 2 are needed",
            frame.len()
        )));
    }
    if frame[OFF_VERSION] != PACKET_VERSION {
        return Err(JamError::Codec(format!(
            "unsupported media packet version {}",
            frame[OFF_VERSION]
        )));
    }
    Ok(frame[OFF_CODEC] == KIND_CONTROL)
}

/// Parse an audio header from the front of `frame` and return it with the
/// payload that follows. The payload borrows `frame`; nothing is copied.
pub fn decode_audio(frame: &[u8]) -> Result<(AudioPacketHeader, &[u8])> {
    if frame.len() < HEADER_SIZE {
        return Err(JamError::Codec(format!(
            "audio frame is {} bytes; the header alone is {HEADER_SIZE}",
            frame.len()
        )));
    }
    let version = frame[OFF_VERSION];
    if version != PACKET_VERSION {
        return Err(JamError::Codec(format!(
            "unsupported media packet version {version}"
        )));
    }
    if frame[OFF_RESERVED] != 0 {
        // A sender that sets the reserved byte is speaking a dialect this build
        // does not know. Accepting it would silently corrupt that meaning.
        return Err(JamError::Codec(
            "reserved media header byte is not zero".to_string(),
        ));
    }
    let codec = codec_from_wire(frame[OFF_CODEC])?;

    let header = AudioPacketHeader {
        version,
        codec,
        flags: be_u16(frame, OFF_FLAGS),
        jam_alias: be_u64(frame, OFF_JAM_ID),
        stream_alias: be_u32(frame, OFF_STREAM_ID),
        generation: be_u32(frame, OFF_GENERATION),
        sequence: be_u64(frame, OFF_SEQUENCE),
        capture_timestamp: be_u64(frame, OFF_CAPTURE),
        frames: be_u16(frame, OFF_FRAMES),
        channels: frame[OFF_CHANNELS],
    };
    Ok((header, &frame[HEADER_SIZE..]))
}

/// Control frame types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlType {
    /// The client's opening frame, carrying the candidate token from signaling.
    Hello,
    /// The server's acceptance, carrying the numeric aliases to stamp into
    /// audio headers.
    Welcome,
    /// Clean teardown from either side.
    Bye,
    /// Keeps a NAT binding open and measures the media path independently of
    /// the signaling socket.
    Ping,
    Pong,
    /// A refused handshake, sent before the socket closes.
    Error,
    /// A type this build does not know. Ignored rather than fatal.
    Unknown(u8),
}

impl ControlType {
    pub fn code(self) -> u8 {
        match self {
            Self::Hello => 1,
            Self::Welcome => 2,
            Self::Bye => 3,
            Self::Ping => 4,
            Self::Pong => 5,
            Self::Error => 6,
            Self::Unknown(raw) => raw,
        }
    }

    fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Hello,
            2 => Self::Welcome,
            3 => Self::Bye,
            4 => Self::Ping,
            5 => Self::Pong,
            6 => Self::Error,
            other => Self::Unknown(other),
        }
    }
}

/// Authenticates a media transport. The token names the jam, the participant,
/// the candidate and the connection generation, and it is signed — so a media
/// node needs no shared session store. A bearer secret; never log it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HelloPayload {
    pub token: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resumed: bool,
}

/// The server's acceptance.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct WelcomePayload {
    #[serde(default)]
    pub jam_alias: u64,
    #[serde(default)]
    pub participant_id: String,
    #[serde(default)]
    pub generation: u32,
    #[serde(default)]
    pub node_id: String,
    /// How often to send a control ping to hold the NAT binding open.
    #[serde(default)]
    pub keepalive_seconds: i32,
    /// The largest audio payload this node will forward.
    #[serde(default)]
    pub max_payload_bytes: i32,
}

/// Explains a refused handshake.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct MediaErrorPayload {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub message: String,
}

/// Build a control frame around a JSON payload.
pub fn encode_control<T: serde::Serialize>(
    kind: ControlType,
    payload: Option<&T>,
) -> Result<Vec<u8>> {
    let body = match payload {
        Some(value) => serde_json::to_vec(value)
            .map_err(|error| JamError::Codec(format!("encode control payload: {error}")))?,
        None => Vec::new(),
    };
    if body.len() > MAX_CONTROL_PAYLOAD {
        return Err(JamError::Codec(format!(
            "control payload is {} bytes; the maximum is {MAX_CONTROL_PAYLOAD}",
            body.len()
        )));
    }
    let mut frame = vec![0u8; CONTROL_HEADER_SIZE + body.len()];
    frame[OFF_VERSION] = PACKET_VERSION;
    frame[OFF_CODEC] = KIND_CONTROL;
    frame[2] = kind.code();
    frame[3] = 0;
    frame[4..8].copy_from_slice(&(body.len() as u32).to_be_bytes());
    frame[CONTROL_HEADER_SIZE..].copy_from_slice(&body);
    Ok(frame)
}

/// Parse a control frame, returning its type and its raw payload, which borrows
/// `frame`.
pub fn decode_control(frame: &[u8]) -> Result<(ControlType, &[u8])> {
    if frame.len() < CONTROL_HEADER_SIZE {
        return Err(JamError::Codec(format!(
            "control frame is {} bytes; the header alone is {CONTROL_HEADER_SIZE}",
            frame.len()
        )));
    }
    if frame[OFF_VERSION] != PACKET_VERSION {
        return Err(JamError::Codec(format!(
            "unsupported media packet version {}",
            frame[OFF_VERSION]
        )));
    }
    if frame[OFF_CODEC] != KIND_CONTROL {
        return Err(JamError::Codec("frame is not a control frame".to_string()));
    }
    if frame[3] != 0 {
        return Err(JamError::Codec(
            "reserved control header byte is not zero".to_string(),
        ));
    }
    let length = be_u32(frame, 4) as usize;
    if length > MAX_CONTROL_PAYLOAD {
        return Err(JamError::Codec(format!(
            "control payload declares {length} bytes; the maximum is {MAX_CONTROL_PAYLOAD}"
        )));
    }
    let end = CONTROL_HEADER_SIZE + length;
    if frame.len() < end {
        return Err(JamError::Codec(format!(
            "control payload is {} bytes but declares {length}",
            frame.len() - CONTROL_HEADER_SIZE
        )));
    }
    Ok((
        ControlType::from_code(frame[2]),
        &frame[CONTROL_HEADER_SIZE..end],
    ))
}

/// Decode a control payload into a typed value.
pub fn decode_control_payload<T: for<'de> serde::Deserialize<'de>>(raw: &[u8]) -> Result<T> {
    if raw.is_empty() {
        return Err(JamError::Codec(
            "control frame carries no payload".to_string(),
        ));
    }
    serde_json::from_slice(raw)
        .map_err(|error| JamError::Codec(format!("malformed control payload: {error}")))
}

// ── PCM conversion ──────────────────────────────────────────────────────────

/// Convert an interleaved PCM payload into interleaved `f32`, appending into
/// `out`.
///
/// `out` is a caller-owned buffer that the receive thread reuses across
/// packets, so the steady state performs no allocation. A payload that is not a
/// whole number of frames is truncated rather than rejected: the frames that
/// did arrive are still music, and the header's `frames` field is what the
/// caller trusts for timing.
pub fn pcm_to_f32(payload: &[u8], format: SampleFormat, channels: usize, out: &mut Vec<f32>) {
    let bytes_per_sample = format.bytes_per_sample();
    if bytes_per_sample == 0 || channels == 0 {
        return;
    }
    let frame_bytes = bytes_per_sample * channels;
    let frames = payload.len() / frame_bytes;
    out.clear();
    out.reserve(frames * channels);

    for frame in 0..frames {
        for channel in 0..channels {
            let at = frame * frame_bytes + channel * bytes_per_sample;
            let sample = match format {
                SampleFormat::S16Le => {
                    let raw = i16::from_le_bytes([payload[at], payload[at + 1]]);
                    raw as f32 / 32768.0
                }
                SampleFormat::S24Le => {
                    // Sign-extend a 24-bit little-endian sample by hand: there
                    // is no i24, and shifting into the top of an i32 keeps the
                    // sign bit where the arithmetic shift can find it.
                    let raw = ((payload[at] as i32) << 8)
                        | ((payload[at + 1] as i32) << 16)
                        | ((payload[at + 2] as i32) << 24);
                    (raw >> 8) as f32 / 8_388_608.0
                }
                SampleFormat::F32Le => f32::from_le_bytes([
                    payload[at],
                    payload[at + 1],
                    payload[at + 2],
                    payload[at + 3],
                ]),
                SampleFormat::None => 0.0,
            };
            out.push(sample);
        }
    }
}

/// Convert interleaved `f32` into an interleaved PCM payload, appending into
/// `out`.
///
/// Values outside [-1, 1] are clamped for the integer formats: wrapping a hot
/// signal would turn a clipped peak into full-scale noise, which is far worse
/// than the flat top clamping produces.
pub fn f32_to_pcm(samples: &[f32], format: SampleFormat, out: &mut Vec<u8>) {
    out.clear();
    out.reserve(samples.len() * format.bytes_per_sample());
    for &sample in samples {
        match format {
            SampleFormat::S16Le => {
                let scaled = (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16;
                out.extend_from_slice(&scaled.to_le_bytes());
            }
            SampleFormat::S24Le => {
                let scaled = (sample.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32;
                let bytes = scaled.to_le_bytes();
                out.extend_from_slice(&bytes[0..3]);
            }
            SampleFormat::F32Le => out.extend_from_slice(&sample.to_le_bytes()),
            SampleFormat::None => {}
        }
    }
}

/// Derive the media-plane numeric alias for a jam id.
///
/// FNV-1a over the id, matching the server: a hash rather than a counter,
/// because the alias must be identical on every node without any of them
/// coordinating.
pub fn jam_alias(jam_id: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for byte in jam_id.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

fn codec_wire(codec: AudioCodec) -> u8 {
    match codec {
        AudioCodec::Pcm => CODEC_WIRE_PCM,
        AudioCodec::AacLc => CODEC_WIRE_AAC_LC,
    }
}

fn codec_from_wire(value: u8) -> Result<AudioCodec> {
    match value {
        CODEC_WIRE_PCM => Ok(AudioCodec::Pcm),
        CODEC_WIRE_AAC_LC => Ok(AudioCodec::AacLc),
        other => Err(JamError::Codec(format!("unknown media codec id {other}"))),
    }
}

#[inline]
fn be_u16(buf: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([buf[at], buf[at + 1]])
}

#[inline]
fn be_u32(buf: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}

#[inline]
fn be_u64(buf: &[u8], at: usize) -> u64 {
    u64::from_be_bytes([
        buf[at],
        buf[at + 1],
        buf[at + 2],
        buf[at + 3],
        buf[at + 4],
        buf[at + 5],
        buf[at + 6],
        buf[at + 7],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_header() -> AudioPacketHeader {
        AudioPacketHeader {
            version: PACKET_VERSION,
            codec: AudioCodec::Pcm,
            flags: FLAG_MARKER,
            jam_alias: 0x0102_0304_0506_0708,
            stream_alias: 42,
            generation: 3,
            sequence: 9_000_000_000,
            capture_timestamp: 20_000_000,
            frames: 128,
            channels: 2,
        }
    }

    #[test]
    fn an_audio_header_survives_a_round_trip_at_the_documented_offsets() {
        let header = sample_header();
        let mut frame = vec![0u8; HEADER_SIZE + 4];
        header.encode(&mut frame).expect("encodes");
        frame[HEADER_SIZE..].copy_from_slice(&[1, 2, 3, 4]);

        // The layout is a wire contract with a Go server and a TypeScript
        // client, so the offsets are asserted, not just the round trip.
        assert_eq!(frame[0], PACKET_VERSION);
        assert_eq!(frame[1], CODEC_WIRE_PCM);
        assert_eq!(&frame[4..12], &header.jam_alias.to_be_bytes());
        assert_eq!(&frame[12..16], &42u32.to_be_bytes());
        assert_eq!(&frame[28..36], &20_000_000u64.to_be_bytes());
        assert_eq!(frame[38], 2);
        assert_eq!(frame[39], 0);

        let (decoded, payload) = decode_audio(&frame).expect("decodes");
        assert_eq!(decoded, header);
        assert_eq!(payload, &[1, 2, 3, 4]);
        assert!(decoded.has(FLAG_MARKER));
        assert!(!decoded.has(FLAG_SILENCE));
    }

    #[test]
    fn malformed_audio_frames_are_errors_not_panics() {
        assert!(decode_audio(&[]).is_err());
        assert!(decode_audio(&[0u8; HEADER_SIZE - 1]).is_err());

        let mut short_version = [0u8; HEADER_SIZE];
        short_version[0] = 9;
        assert!(decode_audio(&short_version).is_err());

        let mut bad_codec = [0u8; HEADER_SIZE];
        bad_codec[0] = PACKET_VERSION;
        bad_codec[1] = 77;
        assert!(decode_audio(&bad_codec).is_err());

        let mut reserved_set = [0u8; HEADER_SIZE];
        reserved_set[0] = PACKET_VERSION;
        reserved_set[1] = CODEC_WIRE_PCM;
        reserved_set[39] = 1;
        assert!(decode_audio(&reserved_set).is_err());
    }

    #[test]
    fn control_frames_round_trip_and_reject_a_lying_length() {
        let frame = encode_control(
            ControlType::Hello,
            Some(&HelloPayload {
                token: "fbj1.token".to_string(),
                resumed: true,
            }),
        )
        .expect("encodes");
        assert!(frame_is_control(&frame).expect("classifies"));

        let (kind, payload) = decode_control(&frame).expect("decodes");
        assert_eq!(kind, ControlType::Hello);
        let hello: HelloPayload = decode_control_payload(payload).expect("payload decodes");
        assert_eq!(hello.token, "fbj1.token");
        assert!(hello.resumed);

        let mut truncated = frame.clone();
        truncated[4..8].copy_from_slice(&9999u32.to_be_bytes());
        assert!(decode_control(&truncated).is_err());
    }

    #[test]
    fn an_unknown_control_type_is_reported_rather_than_rejected() {
        let mut frame = encode_control::<()>(ControlType::Ping, None).expect("encodes");
        frame[2] = 99;
        let (kind, payload) = decode_control(&frame).expect("decodes");
        assert_eq!(kind, ControlType::Unknown(99));
        assert!(payload.is_empty());
    }

    #[test]
    fn an_audio_frame_is_not_mistaken_for_a_control_frame() {
        let mut frame = vec![0u8; HEADER_SIZE];
        sample_header().encode(&mut frame).expect("encodes");
        assert!(!frame_is_control(&frame).expect("classifies"));
    }

    #[test]
    fn pcm_conversion_round_trips_within_the_format_resolution() {
        for (format, tolerance) in [
            (SampleFormat::F32Le, 1e-7_f32),
            (SampleFormat::S24Le, 1e-6),
            (SampleFormat::S16Le, 1e-4),
        ] {
            let source = vec![0.0_f32, 0.5, -0.5, 0.25, -0.999, 0.75];
            let mut encoded = Vec::new();
            f32_to_pcm(&source, format, &mut encoded);
            assert_eq!(encoded.len(), source.len() * format.bytes_per_sample());

            let mut decoded = Vec::new();
            pcm_to_f32(&encoded, format, 2, &mut decoded);
            assert_eq!(decoded.len(), source.len());
            for (got, want) in decoded.iter().zip(source.iter()) {
                assert!(
                    (got - want).abs() <= tolerance,
                    "{format:?}: {got} != {want}"
                );
            }
        }
    }

    #[test]
    fn a_partial_pcm_frame_is_truncated_rather_than_read_out_of_bounds() {
        // Five bytes of stereo s16le is one whole frame plus one stray byte.
        let payload = [0u8, 0, 0, 0, 7];
        let mut out = Vec::new();
        pcm_to_f32(&payload, SampleFormat::S16Le, 2, &mut out);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn integer_conversion_clamps_instead_of_wrapping() {
        let mut encoded = Vec::new();
        f32_to_pcm(&[4.0, -4.0], SampleFormat::S16Le, &mut encoded);
        let mut decoded = Vec::new();
        pcm_to_f32(&encoded, SampleFormat::S16Le, 1, &mut decoded);
        assert!(decoded[0] > 0.99, "positive overload clamped to full scale");
        assert!(
            decoded[1] < -0.99,
            "negative overload clamped to full scale"
        );
    }

    #[test]
    fn the_jam_alias_matches_fnv_1a() {
        // FNV-1a of the empty string is the offset basis.
        assert_eq!(jam_alias(""), 0xcbf2_9ce4_8422_2325);
        // And a known vector, so a refactor cannot quietly change the hash the
        // media plane routes on.
        assert_eq!(jam_alias("a"), 0xaf63_dc4c_8601_ec8c);
    }
}
