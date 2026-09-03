//! The signaling wire contract, mirrored field for field from the Jam server's
//! `pkg/protocol`.
//!
//! This is a translation, not a design. Every name, tag and default here exists
//! because the Go type has it; the React listener speaks the same messages, and
//! a divergence would produce two clients that cannot share a room. When the
//! server grows a field, add it here — do not reinterpret one.
//!
//! Media packets are binary and never pass through this module; see
//! [`crate::packet`].

use serde::{Deserialize, Serialize};

use crate::error::WireError;

/// Current signaling protocol version. The server rejects an envelope it cannot
/// interpret rather than guessing, so this is sent on every frame.
pub const VERSION: i32 = 1;

/// The subprotocol the server selects on the signaling upgrade.
pub const SUBPROTOCOL: &str = "futureboard.jam.v1";

// ── Envelope ────────────────────────────────────────────────────────────────

/// Wraps every signaling frame.
///
/// `request_id` is echoed verbatim on the reply, which is what lets several
/// requests be in flight without an ordering assumption. Server-initiated
/// events carry none.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub v: i32,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
}

impl Envelope {
    /// Build an outbound envelope around a typed payload.
    pub fn new<T: Serialize>(
        kind: &str,
        request_id: &str,
        payload: &T,
    ) -> Result<Self, serde_json::Error> {
        Ok(Self {
            v: VERSION,
            kind: kind.to_string(),
            request_id: request_id.to_string(),
            payload: Some(serde_json::to_value(payload)?),
        })
    }

    /// Decode this envelope's payload into a typed message.
    pub fn decode<T: for<'de> Deserialize<'de>>(&self) -> crate::error::Result<T> {
        let payload = self.payload.clone().ok_or_else(|| {
            crate::error::JamError::Protocol(format!("{}: missing payload", self.kind))
        })?;
        serde_json::from_value(payload).map_err(|error| {
            crate::error::JamError::Protocol(format!("{}: malformed payload: {error}", self.kind))
        })
    }
}

/// Message type names. Kept as constants rather than an enum because the set is
/// open at the server's end: an unrecognised type has to be ignorable, not a
/// parse failure that kills the socket.
pub mod message {
    pub const AUTH_READY: &str = "auth.ready";
    pub const PING: &str = "ping";
    pub const PONG: &str = "pong";
    pub const ERROR: &str = "error";

    pub const JAM_JOIN: &str = "jam.join";
    pub const JAM_JOINED: &str = "jam.joined";
    pub const JAM_LEAVE: &str = "jam.leave";
    pub const JAM_LEFT: &str = "jam.left";
    pub const JAM_PARTICIPANT_JOINED: &str = "jam.participant_joined";
    pub const JAM_PARTICIPANT_LEFT: &str = "jam.participant_left";
    pub const JAM_PARTICIPANT_STATE: &str = "jam.participant_state";
    pub const JAM_CLOSED: &str = "jam.closed";

    pub const JAM_STREAM_PUBLISH: &str = "jam.stream_publish";
    pub const JAM_STREAM_PUBLISHED: &str = "jam.stream_published";
    pub const JAM_STREAM_UNPUBLISH: &str = "jam.stream_unpublish";
    pub const JAM_STREAM_UNPUBLISHED: &str = "jam.stream_unpublished";
    pub const JAM_STREAM_ADDED: &str = "jam.stream_added";
    pub const JAM_STREAM_REMOVED: &str = "jam.stream_removed";

    pub const AUDIO_CAPABILITIES: &str = "audio.capabilities";
    pub const AUDIO_FORMAT_SELECTED: &str = "audio.format_selected";

    pub const TRANSPORT_CAPABILITIES: &str = "transport.capabilities";
    pub const TRANSPORT_CANDIDATES: &str = "transport.candidates";
    pub const TRANSPORT_SELECT: &str = "transport.select";
    pub const TRANSPORT_SELECTED: &str = "transport.selected";

    pub const CLOCK_SYNC_REQUEST: &str = "clock.sync_request";
    pub const CLOCK_SYNC_RESPONSE: &str = "clock.sync_response";
}

/// Generic acknowledgement for messages that need a reply but carry no data.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Ack {
    #[serde(default)]
    pub ok: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

// ── Identity and membership ─────────────────────────────────────────────────

/// The public view of a Futureboard account.
///
/// `id` is immutable and is the only value anything relates on. `username` is a
/// mutable display alias — never a key.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UserSummary {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub avatar_url: String,
}

impl UserSummary {
    /// What a UI shows. Falls back through display name, `@username`, then the
    /// id, so a participant is never rendered as an empty string.
    pub fn label(&self) -> String {
        if !self.display_name.is_empty() {
            self.display_name.clone()
        } else if !self.username.is_empty() {
            format!("@{}", self.username)
        } else {
            self.id.clone()
        }
    }

    /// The `@handle` form, for the compact participant rows.
    pub fn handle(&self) -> String {
        if self.username.is_empty() {
            self.id.clone()
        } else {
            format!("@{}", self.username)
        }
    }
}

/// The coarse label attached to a participant. Authorisation is always decided
/// from [`JamPermissions`], never from this string.
pub type Role = String;

pub mod role {
    pub const LISTENER: &str = "listener";
    pub const PERFORMER: &str = "performer";
    pub const ENGINEER: &str = "engineer";
    pub const COHOST: &str = "cohost";
    pub const HOST: &str = "host";
}

/// The authoritative capability set of one participant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JamPermissions {
    #[serde(default)]
    pub receive_audio: bool,
    #[serde(default)]
    pub send_audio: bool,
    #[serde(default)]
    pub receive_midi: bool,
    #[serde(default)]
    pub send_midi: bool,
    #[serde(default)]
    pub control_transport: bool,
    #[serde(default)]
    pub control_mixer: bool,
    #[serde(default)]
    pub invite_users: bool,
    #[serde(default)]
    pub kick_users: bool,
}

/// Lifecycle state of a jam.
pub type JamStatus = String;

/// Control-plane view of a participant's link. It tracks the participant, not
/// the socket: a participant survives a socket loss and is removed only when it
/// leaves or its resume window expires.
pub type ConnectionState = String;

pub mod connection_state {
    pub const CONNECTING: &str = "connecting";
    pub const CONNECTED: &str = "connected";
    pub const RESUMING: &str = "resuming";
    pub const DISCONNECTED: &str = "disconnected";
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JamSummary {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub public_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub host_user_id: String,
    #[serde(default)]
    pub region_id: String,
    #[serde(default)]
    pub node_id: String,
    #[serde(default)]
    pub status: JamStatus,
    #[serde(default)]
    pub max_participants: i32,
    #[serde(default)]
    pub created_at_unix: i64,
    #[serde(default)]
    pub expires_at_unix: i64,
    #[serde(default)]
    pub participant_count: i32,
}

/// One device of one account attached to one jam.
///
/// One account may appear several times in the same jam, which is why nothing
/// here may be keyed by `user.id` alone.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParticipantSummary {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub jam_id: String,
    #[serde(default)]
    pub user: UserSummary,
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub device_name: String,
    #[serde(default)]
    pub role: Role,
    #[serde(default)]
    pub permissions: JamPermissions,
    #[serde(default)]
    pub connection_state: ConnectionState,
    #[serde(default)]
    pub transport: String,
    #[serde(default)]
    pub region_id: String,
    #[serde(default)]
    pub joined_at_unix: i64,
}

// ── Streams ─────────────────────────────────────────────────────────────────

/// A stream as seen by everyone in the room.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamSummary {
    #[serde(default)]
    pub id: String,
    /// The canonical, immutable routing path, built from identifiers only.
    #[serde(default)]
    pub path: String,
    /// The compact numeric id this stream carries in media packet headers. Sent
    /// explicitly rather than inferred from publish order, which breaks the
    /// first time anybody unpublishes.
    #[serde(default)]
    pub media_alias: u32,
    #[serde(default)]
    pub participant_id: String,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub device_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub direction: String,
    #[serde(default)]
    pub codec: AudioCodec,
    #[serde(default)]
    pub sample_rate: i32,
    #[serde(default)]
    pub sample_format: SampleFormat,
    #[serde(default)]
    pub channels: i32,
    #[serde(default)]
    pub channel_metadata: Vec<ChannelMetadata>,
    #[serde(default)]
    pub clock_domain: String,
    #[serde(default)]
    pub latency: LatencyMetadata,
    #[serde(default)]
    pub active: bool,
}

/// One channel inside a stream, so a UI can render `Guitar : L R` while the
/// wire keeps using immutable ids.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelMetadata {
    #[serde(default)]
    pub index: i32,
    #[serde(default)]
    pub label: String,
    /// A placement hint: `L`, `R`, `M`, `S` or `mono`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub role: String,
}

/// Everything Studio needs to place a remote take on the local timeline. The
/// server forwards these untouched and never compensates a waveform itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct LatencyMetadata {
    #[serde(default)]
    pub input_latency_samples: i64,
    #[serde(default)]
    pub capture_buffer_samples: i64,
    #[serde(default)]
    pub codec_delay_samples: i64,
    #[serde(default)]
    pub clock_offset_ticks: i64,
    #[serde(default)]
    pub clock_drift_ppm: f64,
}

impl LatencyMetadata {
    /// The fixed delays a receiver must remove to place a captured frame on the
    /// session timeline.
    pub fn total_offset_samples(&self) -> i64 {
        self.input_latency_samples + self.capture_buffer_samples + self.codec_delay_samples
    }
}

// ── Codecs ──────────────────────────────────────────────────────────────────

/// A payload format on the media plane. The server never decodes or re-encodes;
/// a codec is negotiation metadata and a routing key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioCodec {
    #[default]
    #[serde(rename = "pcm")]
    Pcm,
    #[serde(rename = "aac-lc")]
    AacLc,
}

impl AudioCodec {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pcm => "pcm",
            Self::AacLc => "aac-lc",
        }
    }
}

/// In-memory sample representation of a PCM stream. Meaningless for compressed
/// codecs, where the server requires it to be absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SampleFormat {
    #[serde(rename = "")]
    #[default]
    None,
    #[serde(rename = "s16le")]
    S16Le,
    #[serde(rename = "s24le")]
    S24Le,
    #[serde(rename = "f32le")]
    F32Le,
}

impl SampleFormat {
    /// Wire size of one sample of one channel. Zero for [`Self::None`].
    pub fn bytes_per_sample(self) -> usize {
        match self {
            Self::None => 0,
            Self::S16Le => 2,
            Self::S24Le => 3,
            Self::F32Le => 4,
        }
    }

    pub fn valid(self) -> bool {
        self.bytes_per_sample() > 0
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "",
            Self::S16Le => "s16le",
            Self::S24Le => "s24le",
            Self::F32Le => "f32le",
        }
    }
}

/// Stream direction from the owning participant's point of view.
pub mod direction {
    pub const SEND: &str = "send";
    pub const RECV: &str = "recv";
}

/// Rates the server's negotiator will select.
pub const SUPPORTED_SAMPLE_RATES: [i32; 6] = [44100, 48000, 88200, 96000, 176400, 192000];

/// The widest layout one stream may carry, matching the server's own
/// `protocol.MaxStreamChannels`.
///
/// Mono and stereo are the interoperable set every client handles; anything
/// wider is a multitrack layout that both ends opt into by listing the count in
/// their capabilities. Sixteen is where the datagram budget stops the idea
/// being useful anyway: at 16-bit that is 32 bytes a frame, or about
/// thirty-seven samples per packet.
pub const MAX_STREAM_CHANNELS: usize = 16;

/// One codec a client can handle, with the parameter sets it supports.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodecCapability {
    pub codec: AudioCodec,
    pub sample_rates: Vec<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bitrates: Vec<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub formats: Vec<SampleFormat>,
    pub channels: Vec<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frame_sizes: Vec<i32>,
}

/// What this client can decode and produce. Must be sent before publishing or
/// receiving: the server resolves a format per stream per receiver and cannot
/// pick one for a client that has not said what it can decode.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AudioCapabilities {
    pub codecs: Vec<CodecCapability>,
}

/// A fully resolved format: the output of negotiation between one publisher and
/// one receiver. Every field is concrete.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioFormat {
    pub codec: AudioCodec,
    pub sample_rate: i32,
    pub channels: i32,
    #[serde(default, skip_serializing_if = "is_no_format")]
    pub format: SampleFormat,
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub bitrate: i32,
    pub frame_samples: i32,
}

fn is_no_format(format: &SampleFormat) -> bool {
    matches!(format, SampleFormat::None)
}

fn is_zero_i32(value: &i32) -> bool {
    *value == 0
}

impl AudioFormat {
    /// Payload size of one frame for uncompressed formats; zero for compressed
    /// codecs, whose frame size is not fixed.
    pub fn payload_bytes(&self) -> usize {
        if self.codec != AudioCodec::Pcm {
            return 0;
        }
        self.frame_samples.max(0) as usize
            * self.channels.max(0) as usize
            * self.format.bytes_per_sample()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioFormatSelected {
    pub stream_id: String,
    pub receiver_participant_id: String,
    pub format: AudioFormat,
}

// ── Transport ───────────────────────────────────────────────────────────────

/// Every path media can take between a client and a media node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TransportKind {
    #[serde(rename = "udp")]
    Udp,
    #[serde(rename = "quic")]
    Quic,
    #[serde(rename = "tcp")]
    Tcp,
    #[serde(rename = "tls")]
    Tls,
    #[serde(rename = "turn-udp")]
    TurnUdp,
    #[serde(rename = "turn-tcp")]
    TurnTcp,
    #[serde(rename = "turn-tls")]
    TurnTls,
    #[serde(rename = "webrtc")]
    WebRtc,
    #[serde(rename = "webtransport")]
    WebTransport,
    #[serde(rename = "websocket")]
    WebSocket,
}

impl TransportKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Udp => "udp",
            Self::Quic => "quic",
            Self::Tcp => "tcp",
            Self::Tls => "tls",
            Self::TurnUdp => "turn-udp",
            Self::TurnTcp => "turn-tcp",
            Self::TurnTls => "turn-tls",
            Self::WebRtc => "webrtc",
            Self::WebTransport => "webtransport",
            Self::WebSocket => "websocket",
        }
    }

    /// Whether the transport delivers unordered datagrams. Ordered transports
    /// are survivable but head-of-line blocked, which is why they rank lower.
    pub fn datagram(self) -> bool {
        matches!(
            self,
            Self::Udp | Self::Quic | Self::TurnUdp | Self::WebRtc | Self::WebTransport
        )
    }

    pub fn relayed(self) -> bool {
        matches!(self, Self::TurnUdp | Self::TurnTcp | Self::TurnTls)
    }
}

/// What this client reports it can actually open, as determined by its own
/// probing. A statement about the network, not a preference list — ordering is
/// the server's job.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransportCapabilities {
    pub udp: bool,
    pub quic: bool,
    pub tcp: bool,
    pub tls: bool,
    pub turn_udp: bool,
    pub turn_tcp: bool,
    pub turn_tls: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub webrtc: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub webtransport: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub websocket: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub client_kind: String,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// One address the client may try, with the priority the server wants it tried
/// at. `token` authorises the media plane to accept packets on this candidate
/// and is a bearer secret — never log it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportCandidate {
    pub id: String,
    pub kind: TransportKind,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: i32,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub region_id: String,
    #[serde(default)]
    pub node_id: String,
    #[serde(default)]
    pub secure: bool,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub expires_at_unix: i64,
}

impl TransportCandidate {
    /// `host:port`, for logs and for socket connection.
    pub fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// Candidates, ordered best first.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportCandidates {
    #[serde(default)]
    pub candidates: Vec<TransportCandidate>,
    /// The candidate guaranteed to work on a network permitting only outbound
    /// TCP 443. It is always present in `candidates` too.
    pub fallback_kind: TransportKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportSelect {
    pub candidate_id: String,
    pub kind: TransportKind,
    /// The candidate's own token, echoed back — it is what proves the selection
    /// refers to a candidate this server offered.
    pub token: String,
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub rtt_ms: f64,
}

fn is_zero_f64(value: &f64) -> bool {
    *value == 0.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportSelected {
    #[serde(default)]
    pub participant_id: String,
    #[serde(default)]
    pub candidate_id: String,
    pub kind: TransportKind,
    #[serde(default)]
    pub datagram: bool,
    #[serde(default)]
    pub relayed: bool,
}

// ── Regions ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegionSummary {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub country: String,
    #[serde(default)]
    pub city: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub udp_port: i32,
    #[serde(default)]
    pub quic_port: i32,
    #[serde(default)]
    pub tls_port: i32,
    #[serde(default)]
    pub capacity: i32,
    #[serde(default)]
    pub current_load: i32,
    #[serde(default)]
    pub load_factor: f64,
    #[serde(default)]
    pub healthy: bool,
}

/// One client's measurement of one region.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegionProbe {
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub region_id: String,
    pub rtt_ms: f64,
    pub jitter_ms: f64,
    pub loss_pct: f64,
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub samples: i32,
}

// ── Clock ───────────────────────────────────────────────────────────────────

/// Tick rate of the session clock. One tick is one sample at 48 kHz; streams at
/// other rates convert. Packet arrival time is never the audio timeline.
pub const DEFAULT_CLOCK_RATE: u32 = 48_000;

/// The jam-wide clock domain every participant shares unless it declares
/// otherwise.
pub const DOMAIN_SESSION: &str = "session";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClockInfo {
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub rate: u32,
    /// Wall-clock instant of tick 0.
    #[serde(default)]
    pub epoch_unix_nanos: i64,
    /// Session clock reading when this message was built.
    #[serde(default)]
    pub ticks: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ClockSyncRequest {
    /// Client transmit timestamp on its own monotonic clock, in nanoseconds.
    /// The server never interprets it; it only echoes it back.
    pub t1: i64,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub seq: u32,
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ClockSyncResponse {
    pub t1: i64,
    pub t2: i64,
    pub t3: i64,
    #[serde(default)]
    pub seq: u32,
    /// Session clock reading at `t3`, so a client can lock its sample counter
    /// without a second round trip.
    #[serde(default)]
    pub session_ticks: i64,
    #[serde(default)]
    pub clock_rate: u32,
}

// ── Join / leave ────────────────────────────────────────────────────────────

/// Identity is never taken from this message: the account is whatever the
/// authenticated connection says it is. `jam_access_token` only proves the
/// caller was invited to this jam and at what role.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JamJoinRequest {
    pub jam_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub jam_access_token: String,
    /// Client-generated and stable across restarts, so a reconnecting laptop is
    /// recognised as the same device instead of accumulating ghosts.
    pub device_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub device_name: String,
    /// Re-attaches to an existing participant after a transport change or a
    /// drop, instead of creating a new one. A bearer secret.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub resume_token: String,
    /// The highest event sequence already applied; the server replays from
    /// there when resuming.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub last_seq: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub preferred_region: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub region_probes: Vec<RegionProbe>,
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

/// The full room snapshot: everything needed before anything can be rendered.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JamJoined {
    #[serde(default)]
    pub jam: JamSummary,
    #[serde(default)]
    pub participant: ParticipantSummary,
    #[serde(default)]
    pub participants: Vec<ParticipantSummary>,
    #[serde(default)]
    pub streams: Vec<StreamSummary>,
    #[serde(default)]
    pub region: RegionSummary,
    #[serde(default)]
    pub clock: ClockInfo,
    /// Presented on a later `jam.join` to re-attach to this participant. A
    /// bearer secret; never log it.
    #[serde(default)]
    pub resume_token: String,
    /// Increments every time this participant re-attaches. Media nodes drop
    /// packets stamped with a stale generation.
    #[serde(default)]
    pub connection_generation: u32,
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub resumed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JamLeaveRequest {
    pub jam_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JamLeft {
    #[serde(default)]
    pub jam_id: String,
    #[serde(default)]
    pub participant_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JamParticipantEvent {
    #[serde(default)]
    pub jam_id: String,
    #[serde(default)]
    pub participant: ParticipantSummary,
    #[serde(default)]
    pub seq: u64,
    /// Set on departures: `left`, `kicked`, `timeout`, `jam_closed`.
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JamParticipantState {
    #[serde(default)]
    pub jam_id: String,
    #[serde(default)]
    pub participant_id: String,
    #[serde(default)]
    pub connection_state: ConnectionState,
    #[serde(default)]
    pub transport: String,
    #[serde(default)]
    pub seq: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JamClosed {
    #[serde(default)]
    pub jam_id: String,
    #[serde(default)]
    pub reason: String,
}

// ── Stream publishing ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamPublishRequest {
    pub jam_id: String,
    pub name: String,
    pub direction: String,
    pub codec: AudioCodec,
    pub sample_rate: i32,
    #[serde(default, skip_serializing_if = "is_no_format")]
    pub sample_format: SampleFormat,
    pub channels: i32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channel_metadata: Vec<ChannelMetadata>,
    /// Frame sizes offered for this stream alone, overriding the ones in
    /// `audio.capabilities`.
    ///
    /// Frame length and channel count are not independent for uncompressed
    /// audio: a datagram carries about 1200 bytes, which is roomy at two
    /// channels and very tight at sixteen. One session publishes both, and a
    /// session-wide list cannot say "256 for the master, 32 for the multitrack
    /// take" — so the wide stream states its own. Empty leaves the capability
    /// list in charge, which is what the server assumes of a client that sends
    /// nothing here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frame_sizes: Vec<i32>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub clock_domain: String,
    pub latency: LatencyMetadata,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamPublished {
    #[serde(default)]
    pub jam_id: String,
    #[serde(default)]
    pub stream: StreamSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamUnpublishRequest {
    pub jam_id: String,
    pub stream_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamUnpublished {
    #[serde(default)]
    pub jam_id: String,
    #[serde(default)]
    pub stream_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamEvent {
    #[serde(default)]
    pub jam_id: String,
    #[serde(default)]
    pub stream: StreamSummary,
    #[serde(default)]
    pub stream_id: String,
    #[serde(default)]
    pub seq: u64,
}

// ── Connection lifecycle ────────────────────────────────────────────────────

/// The first message the server sends. It reports which account this connection
/// acts as, so a client with several accounts cannot mistake one for another.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthReady {
    #[serde(default)]
    pub user: UserSummary,
    #[serde(default)]
    pub connection_id: String,
    #[serde(default)]
    pub server_node_id: String,
    #[serde(default)]
    pub server_region: String,
    #[serde(default)]
    pub protocol_version: i32,
    #[serde(default)]
    pub heartbeat_seconds: i32,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Ping {
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub nonce: i64,
}

fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Pong {
    #[serde(default)]
    pub nonce: i64,
    #[serde(default, rename = "server_unix_nanos")]
    pub server_unix_nanos: i64,
}

/// Decode an `error` envelope payload.
pub fn decode_error(envelope: &Envelope) -> WireError {
    envelope.decode::<WireError>().unwrap_or(WireError {
        code: crate::error::ErrorCode::Unknown("malformed_error".to_string()),
        message: "the server reported an error this build could not read".to_string(),
        retryable: false,
        request_id: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_participant_event_from_the_server_decodes() {
        let raw = r#"{
            "v": 1,
            "type": "jam.participant_joined",
            "payload": {
                "jam_id": "jam_1",
                "participant": {
                    "id": "pcp_1",
                    "jam_id": "jam_1",
                    "user": {"id":"usr_1","username":"hachi224","display_name":"Hachi"},
                    "device_id": "studio-mac",
                    "role": "performer",
                    "permissions": {"receive_audio": true, "send_audio": true},
                    "connection_state": "connected",
                    "joined_at_unix": 1772539200
                },
                "seq": 7
            }
        }"#;
        let envelope: Envelope = serde_json::from_str(raw).expect("envelope parses");
        assert_eq!(envelope.kind, message::JAM_PARTICIPANT_JOINED);
        let event: JamParticipantEvent = envelope.decode().expect("payload parses");
        assert_eq!(event.seq, 7);
        assert_eq!(event.participant.user.username, "hachi224");
        assert_eq!(event.participant.user.handle(), "@hachi224");
        assert!(event.participant.permissions.send_audio);
        // Absent permissions default to denied rather than to granted.
        assert!(!event.participant.permissions.kick_users);
    }

    #[test]
    fn stream_metadata_carries_the_media_alias_and_channel_roles() {
        let raw = r#"{
            "id":"str_1","path":"jam/j/u/d/str_1","media_alias":42,
            "participant_id":"pcp_1","user_id":"usr_1","device_id":"dev_1",
            "name":"Guitar","direction":"send","codec":"pcm","sample_rate":48000,
            "sample_format":"f32le","channels":2,
            "channel_metadata":[{"index":0,"label":"Guitar L","role":"L"},
                                {"index":1,"label":"Guitar R","role":"R"}],
            "clock_domain":"session",
            "latency":{"input_latency_samples":128,"capture_buffer_samples":256,
                       "codec_delay_samples":0,"clock_offset_ticks":-47,
                       "clock_drift_ppm":20.8},
            "active":true
        }"#;
        let stream: StreamSummary = serde_json::from_str(raw).expect("stream parses");
        assert_eq!(stream.media_alias, 42);
        assert_eq!(stream.codec, AudioCodec::Pcm);
        assert_eq!(stream.sample_format, SampleFormat::F32Le);
        assert_eq!(stream.channel_metadata[1].role, "R");
        assert_eq!(stream.latency.total_offset_samples(), 384);
    }

    #[test]
    fn an_unknown_message_type_is_data_not_a_parse_failure() {
        let raw = r#"{"v":1,"type":"jam.future_thing","payload":{"x":1}}"#;
        let envelope: Envelope = serde_json::from_str(raw).expect("unknown types still parse");
        assert_eq!(envelope.kind, "jam.future_thing");
    }

    #[test]
    fn an_envelope_with_the_wrong_version_is_still_readable_so_it_can_be_reported() {
        let raw = r#"{"v":99,"type":"auth.ready","payload":{}}"#;
        let envelope: Envelope = serde_json::from_str(raw).expect("parses");
        assert_ne!(envelope.v, VERSION);
    }

    #[test]
    fn capabilities_omit_empty_optional_lists() {
        let caps = AudioCapabilities {
            codecs: vec![CodecCapability {
                codec: AudioCodec::Pcm,
                sample_rates: vec![48000],
                bitrates: Vec::new(),
                formats: vec![SampleFormat::F32Le],
                channels: vec![1, 2],
                frame_sizes: vec![128, 256],
            }],
        };
        let json = serde_json::to_string(&caps).expect("encodes");
        assert!(!json.contains("bitrates"));
        assert!(json.contains("\"f32le\""));
        assert!(json.contains("\"pcm\""));
    }

    #[test]
    fn transport_kinds_round_trip_with_their_hyphenated_names() {
        let json = serde_json::to_string(&TransportKind::TurnTls).expect("encodes");
        assert_eq!(json, "\"turn-tls\"");
        let parsed: TransportKind = serde_json::from_str("\"udp\"").expect("decodes");
        assert_eq!(parsed, TransportKind::Udp);
        assert!(parsed.datagram());
        assert!(!TransportKind::Tls.datagram());
    }

    #[test]
    fn a_pcm_format_reports_its_frame_payload_size() {
        let format = AudioFormat {
            codec: AudioCodec::Pcm,
            sample_rate: 48000,
            channels: 2,
            format: SampleFormat::F32Le,
            bitrate: 0,
            frame_samples: 128,
        };
        assert_eq!(format.payload_bytes(), 128 * 2 * 4);

        let aac = AudioFormat {
            codec: AudioCodec::AacLc,
            bitrate: 256000,
            format: SampleFormat::None,
            ..format
        };
        assert_eq!(aac.payload_bytes(), 0);
    }
}
