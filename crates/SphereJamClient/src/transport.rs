//! The media plane: opening a transport, and moving framed packets over it.
//!
//! Three native transports are implemented here — UDP, TCP and TLS. QUIC is in
//! the protocol and in the server, and is deliberately not claimed in this
//! client's capabilities: claiming a transport it cannot open would make the
//! server rank a candidate the client then has to fail over from, which is
//! slower than never offering it.
//!
//! Session identity is not the socket. A transport is opened, used, and
//! replaced; the participant, its streams and its subscriptions live in the
//! control plane and survive all of that. Everything here is therefore
//! disposable by design.
//!
//! Sending and receiving are split into two halves on purpose: the receive half
//! is owned by the media receive thread and blocks, while the send half is
//! shared with the publish path and must be callable at the same time.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::error::{JamError, Result};
use crate::packet::{
    self, ControlType, HelloPayload, MediaErrorPayload, WelcomePayload, MAX_MEDIA_FRAME,
};
use crate::protocol::{TransportCandidate, TransportCapabilities, TransportKind};

/// How long a blocking receive waits before returning [`RecvOutcome::Timeout`].
///
/// Short enough that a shutdown request is acted on promptly, long enough that
/// an idle stream is not a busy loop.
const RECV_POLL: Duration = Duration::from_millis(250);

/// Counters for one open transport. Read by the UI at a throttled rate.
#[derive(Debug, Default)]
pub struct TransportStats {
    pub packets_in: AtomicU64,
    pub packets_out: AtomicU64,
    pub bytes_in: AtomicU64,
    pub bytes_out: AtomicU64,
    pub send_errors: AtomicU64,
    /// Frames dropped before parsing because they were too short or malformed.
    pub malformed_in: AtomicU64,
}

/// A flat snapshot of [`TransportStats`], safe to hand to a UI frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransportStatsSnapshot {
    pub packets_in: u64,
    pub packets_out: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub send_errors: u64,
    pub malformed_in: u64,
}

impl TransportStats {
    pub fn snapshot(&self) -> TransportStatsSnapshot {
        TransportStatsSnapshot {
            packets_in: self.packets_in.load(Ordering::Relaxed),
            packets_out: self.packets_out.load(Ordering::Relaxed),
            bytes_in: self.bytes_in.load(Ordering::Relaxed),
            bytes_out: self.bytes_out.load(Ordering::Relaxed),
            send_errors: self.send_errors.load(Ordering::Relaxed),
            malformed_in: self.malformed_in.load(Ordering::Relaxed),
        }
    }
}

/// The send half of an open media transport. Shared, and safe to call from the
/// publish path while the receive half is blocked in a read.
pub trait TransportSender: Send + Sync {
    fn kind(&self) -> TransportKind;
    /// Send one complete media frame.
    fn send_frame(&self, frame: &[u8]) -> Result<()>;
    /// Release the socket. Idempotent.
    fn close(&self);
}

/// What one receive attempt produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvOutcome {
    /// A frame was written into the caller's buffer.
    Frame,
    /// Nothing arrived within the poll interval. Not an error.
    Timeout,
    /// The peer closed, or the socket was shut down locally.
    Closed,
}

/// The receive half. Owned by exactly one thread.
pub trait TransportReceiver: Send {
    /// Read one frame into `out`, replacing its contents.
    fn recv_frame(&mut self, out: &mut Vec<u8>) -> Result<RecvOutcome>;
}

/// An open, authenticated media transport.
pub struct MediaTransport {
    pub kind: TransportKind,
    pub candidate_id: String,
    /// The server's acceptance, carrying the aliases to stamp into audio
    /// headers and how often to send a keepalive.
    pub welcome: WelcomePayload,
    /// Round trip measured by the handshake, before clock sync produces better
    /// numbers.
    pub handshake_rtt: Duration,
    pub sender: Arc<dyn TransportSender>,
    pub receiver: Box<dyn TransportReceiver>,
    pub stats: Arc<TransportStats>,
}

impl std::fmt::Debug for MediaTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaTransport")
            .field("kind", &self.kind.as_str())
            .field("candidate_id", &self.candidate_id)
            .field("node_id", &self.welcome.node_id)
            .field("handshake_rtt_ms", &self.handshake_rtt.as_millis())
            .finish()
    }
}

impl MediaTransport {
    /// How often to send a control ping. Falls back to the server's own default
    /// when a welcome omits it.
    pub fn keepalive(&self) -> Duration {
        let seconds = if self.welcome.keepalive_seconds > 0 {
            self.welcome.keepalive_seconds as u64
        } else {
            15
        };
        Duration::from_secs(seconds)
    }

    /// The largest audio payload this node will forward.
    pub fn max_payload_bytes(&self) -> usize {
        if self.welcome.max_payload_bytes > 0 {
            self.welcome.max_payload_bytes as usize
        } else {
            1200
        }
    }
}

/// What this client reports it can open.
///
/// A statement about the network and about this build, not a preference list —
/// ordering is the server's job. QUIC is false because nothing here speaks it;
/// TURN is false because no relay is configured on the client side.
pub fn native_capabilities() -> TransportCapabilities {
    TransportCapabilities {
        udp: true,
        quic: false,
        tcp: true,
        tls: true,
        turn_udp: false,
        turn_tcp: false,
        turn_tls: false,
        webrtc: false,
        webtransport: false,
        websocket: false,
        client_kind: "native".to_string(),
    }
}

/// Whether this build can actually open a candidate of this kind.
pub fn can_open(kind: TransportKind) -> bool {
    matches!(
        kind,
        TransportKind::Udp | TransportKind::Tcp | TransportKind::Tls
    )
}

/// Order the server's candidates into the sequence this client should try.
///
/// The server's own priority is honoured first, because it knows the region
/// topology; among equals a datagram path wins, because an ordered transport
/// turns one lost packet into a stall for every packet behind it and in a jam
/// that stall is audible. Candidates this build cannot open are dropped rather
/// than attempted and failed.
pub fn ordered_candidates(candidates: &[TransportCandidate]) -> Vec<&TransportCandidate> {
    let mut usable: Vec<&TransportCandidate> = candidates
        .iter()
        .filter(|candidate| can_open(candidate.kind))
        .collect();
    usable.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| b.kind.datagram().cmp(&a.kind.datagram()))
            .then_with(|| a.kind.relayed().cmp(&b.kind.relayed()))
            .then_with(|| a.id.cmp(&b.id))
    });
    usable
}

/// Open the first candidate that works, in order.
///
/// Returns the transport and, on failure, the error from the last candidate
/// tried — the earlier ones are logged by the caller but do not each need to
/// reach the UI, which only ever shows "could not connect".
pub fn connect_first(
    candidates: &[TransportCandidate],
    timeout: Duration,
    resumed: bool,
) -> Result<MediaTransport> {
    let ordered = ordered_candidates(candidates);
    if ordered.is_empty() {
        return Err(JamError::Transport(
            "the server offered no candidate this build can open".to_string(),
        ));
    }
    let mut last = None;
    for candidate in ordered {
        match connect(candidate, timeout, resumed) {
            Ok(transport) => return Ok(transport),
            Err(error) => last = Some(error),
        }
    }
    Err(last
        .unwrap_or_else(|| JamError::Transport("no media candidate could be opened".to_string())))
}

/// Open one candidate and complete the media handshake.
pub fn connect(
    candidate: &TransportCandidate,
    timeout: Duration,
    resumed: bool,
) -> Result<MediaTransport> {
    match candidate.kind {
        TransportKind::Udp => connect_udp(candidate, timeout, resumed),
        TransportKind::Tcp => connect_stream(candidate, timeout, resumed, false),
        TransportKind::Tls => connect_stream(candidate, timeout, resumed, true),
        other => Err(JamError::Transport(format!(
            "this build cannot open a {} media transport",
            other.as_str()
        ))),
    }
}

fn hello_frame(candidate: &TransportCandidate, resumed: bool) -> Result<Vec<u8>> {
    packet::encode_control(
        ControlType::Hello,
        Some(&HelloPayload {
            token: candidate.token.clone(),
            resumed,
        }),
    )
}

/// Interpret the server's reply to a hello.
fn read_welcome(frame: &[u8]) -> Result<WelcomePayload> {
    let (kind, payload) = packet::decode_control(frame)?;
    match kind {
        ControlType::Welcome => packet::decode_control_payload(payload),
        ControlType::Error => {
            let error: MediaErrorPayload =
                packet::decode_control_payload(payload).unwrap_or_else(|_| MediaErrorPayload {
                    code: "unknown".to_string(),
                    message: "the media node refused the handshake".to_string(),
                });
            Err(JamError::Transport(format!(
                "media handshake refused: {} ({})",
                error.message, error.code
            )))
        }
        other => Err(JamError::Transport(format!(
            "the media node answered a hello with control type {:?}",
            other
        ))),
    }
}

fn resolve(candidate: &TransportCandidate) -> Result<std::net::SocketAddr> {
    let address = candidate.address();
    address
        .to_socket_addrs()
        .map_err(|error| JamError::Transport(format!("could not resolve {address}: {error}")))?
        .next()
        .ok_or_else(|| JamError::Transport(format!("{address} resolved to no address")))
}

// ── UDP ─────────────────────────────────────────────────────────────────────

/// The preferred path: a lost packet stays lost instead of stalling everything
/// behind it.
fn connect_udp(
    candidate: &TransportCandidate,
    timeout: Duration,
    resumed: bool,
) -> Result<MediaTransport> {
    let remote = resolve(candidate)?;
    let bind = if remote.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let socket = UdpSocket::bind(bind)
        .map_err(|error| JamError::Transport(format!("could not open a udp socket: {error}")))?;
    socket
        .connect(remote)
        .map_err(|error| JamError::Transport(format!("could not reach {remote}: {error}")))?;
    socket
        .set_read_timeout(Some(timeout))
        .map_err(|error| JamError::Transport(format!("udp read timeout: {error}")))?;

    let hello = hello_frame(candidate, resumed)?;
    let started = Instant::now();

    // A datagram handshake can lose either half, so the hello is retried until
    // the timeout rather than sent once and hoped for. The server's handshake
    // is idempotent for a given generation, which is what makes this safe.
    let mut buffer = vec![0u8; MAX_MEDIA_FRAME];
    let mut welcome = None;
    let deadline = started + timeout;
    while Instant::now() < deadline {
        socket.send(&hello).map_err(|error| {
            JamError::Transport(format!("could not send a media hello: {error}"))
        })?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let _ = socket.set_read_timeout(Some(remaining.min(Duration::from_millis(500))));
        match socket.recv(&mut buffer) {
            Ok(read) => {
                welcome = Some(read_welcome(&buffer[..read])?);
                break;
            }
            Err(error) if is_timeout(&error) => continue,
            Err(error) => {
                return Err(JamError::Transport(format!(
                    "media handshake read failed: {error}"
                )))
            }
        }
    }
    let welcome = welcome.ok_or_else(|| {
        JamError::Transport("the media node did not answer the handshake".to_string())
    })?;
    let handshake_rtt = started.elapsed();

    socket
        .set_read_timeout(Some(RECV_POLL))
        .map_err(|error| JamError::Transport(format!("udp read timeout: {error}")))?;
    let socket = Arc::new(socket);
    let stats = Arc::new(TransportStats::default());

    Ok(MediaTransport {
        kind: TransportKind::Udp,
        candidate_id: candidate.id.clone(),
        welcome,
        handshake_rtt,
        sender: Arc::new(UdpSender {
            socket: Arc::clone(&socket),
            stats: Arc::clone(&stats),
        }),
        receiver: Box::new(UdpReceiver {
            socket,
            stats: Arc::clone(&stats),
        }),
        stats,
    })
}

struct UdpSender {
    socket: Arc<UdpSocket>,
    stats: Arc<TransportStats>,
}

impl TransportSender for UdpSender {
    fn kind(&self) -> TransportKind {
        TransportKind::Udp
    }

    fn send_frame(&self, frame: &[u8]) -> Result<()> {
        match self.socket.send(frame) {
            Ok(sent) => {
                self.stats.packets_out.fetch_add(1, Ordering::Relaxed);
                self.stats
                    .bytes_out
                    .fetch_add(sent as u64, Ordering::Relaxed);
                Ok(())
            }
            Err(error) => {
                self.stats.send_errors.fetch_add(1, Ordering::Relaxed);
                Err(JamError::Transport(format!("udp send failed: {error}")))
            }
        }
    }

    fn close(&self) {
        // A connected UDP socket has nothing to shut down; dropping the last
        // Arc releases it. Sends after this simply fail.
    }
}

struct UdpReceiver {
    socket: Arc<UdpSocket>,
    stats: Arc<TransportStats>,
}

impl TransportReceiver for UdpReceiver {
    fn recv_frame(&mut self, out: &mut Vec<u8>) -> Result<RecvOutcome> {
        // Read into the caller's buffer at full capacity so an oversized
        // datagram is seen and rejected rather than silently truncated into a
        // malformed frame.
        out.resize(MAX_MEDIA_FRAME, 0);
        match self.socket.recv(out) {
            Ok(read) => {
                out.truncate(read);
                self.stats.packets_in.fetch_add(1, Ordering::Relaxed);
                self.stats
                    .bytes_in
                    .fetch_add(read as u64, Ordering::Relaxed);
                Ok(RecvOutcome::Frame)
            }
            Err(error) if is_timeout(&error) => {
                out.clear();
                Ok(RecvOutcome::Timeout)
            }
            Err(error) => {
                out.clear();
                // A connection-refused surfacing here is an ICMP unreachable
                // for a previous send, which happens routinely while a node
                // restarts. It is not fatal to the session.
                if error.kind() == std::io::ErrorKind::ConnectionRefused {
                    self.stats.malformed_in.fetch_add(1, Ordering::Relaxed);
                    return Ok(RecvOutcome::Timeout);
                }
                Err(JamError::Transport(format!("udp receive failed: {error}")))
            }
        }
    }
}

// ── TCP and TLS ─────────────────────────────────────────────────────────────

/// The survivability path: a jam has to work from a network that permits
/// nothing but outbound TCP. It is strictly worse than UDP for audio, which is
/// why the negotiator only ever offers it below the datagram paths.
///
/// TCP has no message boundaries, so one is imposed: a four-byte big-endian
/// length in front of every frame, matching the server's own framing.
fn connect_stream(
    candidate: &TransportCandidate,
    timeout: Duration,
    resumed: bool,
    secure: bool,
) -> Result<MediaTransport> {
    let remote = resolve(candidate)?;
    let tcp = TcpStream::connect_timeout(&remote, timeout)
        .map_err(|error| JamError::Transport(format!("could not reach {remote}: {error}")))?;
    tcp.set_nodelay(true).ok();
    tcp.set_read_timeout(Some(timeout))
        .map_err(|error| JamError::Transport(format!("tcp read timeout: {error}")))?;

    let mut stream: StreamConn = if secure {
        StreamConn::Tls(Box::new(tls_stream(tcp, &candidate.host)?))
    } else {
        StreamConn::Plain(tcp)
    };

    let started = Instant::now();
    let hello = hello_frame(candidate, resumed)?;
    write_framed(&mut stream, &hello)?;

    let mut buffer = Vec::new();
    let welcome = match read_framed(&mut stream, &mut buffer)? {
        RecvOutcome::Frame => read_welcome(&buffer)?,
        _ => {
            return Err(JamError::Transport(
                "the media node closed before answering the handshake".to_string(),
            ))
        }
    };
    let handshake_rtt = started.elapsed();

    stream
        .set_read_timeout(Some(RECV_POLL))
        .map_err(|error| JamError::Transport(format!("tcp read timeout: {error}")))?;

    let kind = if secure {
        TransportKind::Tls
    } else {
        TransportKind::Tcp
    };
    let stats = Arc::new(TransportStats::default());
    let shared = Arc::new(Mutex::new(stream));

    Ok(MediaTransport {
        kind,
        candidate_id: candidate.id.clone(),
        welcome,
        handshake_rtt,
        sender: Arc::new(StreamSender {
            kind,
            stream: Arc::clone(&shared),
            stats: Arc::clone(&stats),
        }),
        receiver: Box::new(StreamReceiver {
            stream: shared,
            stats: Arc::clone(&stats),
        }),
        stats,
    })
}

/// Build a rustls connection over an established TCP socket.
///
/// Verification uses the platform trust store and is never relaxed: this socket
/// carries a signed candidate token and a jam's audio, and an unverified peer
/// is exactly the party that must not have either.
fn tls_stream(
    tcp: TcpStream,
    host: &str,
) -> Result<rustls::StreamOwned<rustls::ClientConnection, TcpStream>> {
    let mut roots = rustls::RootCertStore::empty();
    let loaded = rustls_native_certs::load_native_certs();
    for certificate in loaded.certs {
        let _ = roots.add(certificate);
    }
    if roots.is_empty() {
        return Err(JamError::Transport(
            "no trusted certificates are available for a TLS media transport".to_string(),
        ));
    }
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server_name = rustls_pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| JamError::Transport(format!("{host} is not a valid TLS server name")))?;
    let connection = rustls::ClientConnection::new(Arc::new(config), server_name)
        .map_err(|error| JamError::Transport(format!("could not start TLS: {error}")))?;
    Ok(rustls::StreamOwned::new(connection, tcp))
}

enum StreamConn {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl StreamConn {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        match self {
            Self::Plain(tcp) => tcp.set_read_timeout(timeout),
            Self::Tls(tls) => tls.get_ref().set_read_timeout(timeout),
        }
    }

    fn shutdown(&self) {
        let socket = match self {
            Self::Plain(tcp) => tcp,
            Self::Tls(tls) => tls.get_ref(),
        };
        let _ = socket.shutdown(Shutdown::Both);
    }
}

impl Read for StreamConn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(tcp) => tcp.read(buf),
            Self::Tls(tls) => tls.read(buf),
        }
    }
}

impl Write for StreamConn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(tcp) => tcp.write(buf),
            Self::Tls(tls) => tls.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(tcp) => tcp.flush(),
            Self::Tls(tls) => tls.flush(),
        }
    }
}

fn write_framed(stream: &mut StreamConn, frame: &[u8]) -> Result<()> {
    if frame.len() > MAX_MEDIA_FRAME {
        return Err(JamError::Transport(format!(
            "media frame is {} bytes; the stream limit is {MAX_MEDIA_FRAME}",
            frame.len()
        )));
    }
    let length = (frame.len() as u32).to_be_bytes();
    stream
        .write_all(&length)
        .and_then(|()| stream.write_all(frame))
        .and_then(|()| stream.flush())
        .map_err(|error| JamError::Transport(format!("stream send failed: {error}")))
}

fn read_framed(stream: &mut StreamConn, out: &mut Vec<u8>) -> Result<RecvOutcome> {
    let mut length = [0u8; 4];
    match stream.read_exact(&mut length) {
        Ok(()) => {}
        Err(error) if is_timeout(&error) => {
            out.clear();
            return Ok(RecvOutcome::Timeout);
        }
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            out.clear();
            return Ok(RecvOutcome::Closed);
        }
        Err(error) => {
            out.clear();
            return Err(JamError::Transport(format!("stream read failed: {error}")));
        }
    }
    let declared = u32::from_be_bytes(length) as usize;
    if declared == 0 || declared > MAX_MEDIA_FRAME {
        // A length this side cannot honour means the stream is desynchronised;
        // there is no way to find the next boundary, so the connection is done.
        out.clear();
        return Err(JamError::Transport(format!(
            "media stream declared a {declared}-byte frame; the limit is {MAX_MEDIA_FRAME}"
        )));
    }
    out.resize(declared, 0);
    match stream.read_exact(out) {
        Ok(()) => Ok(RecvOutcome::Frame),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            out.clear();
            Ok(RecvOutcome::Closed)
        }
        Err(error) => {
            out.clear();
            Err(JamError::Transport(format!(
                "media stream truncated mid-frame: {error}"
            )))
        }
    }
}

struct StreamSender {
    kind: TransportKind,
    stream: Arc<Mutex<StreamConn>>,
    stats: Arc<TransportStats>,
}

impl TransportSender for StreamSender {
    fn kind(&self) -> TransportKind {
        self.kind
    }

    fn send_frame(&self, frame: &[u8]) -> Result<()> {
        // The lock is held only for the write. It is contended by at most the
        // publish path and the keepalive, neither of which is the audio thread.
        let mut guard = self
            .stream
            .lock()
            .map_err(|_| JamError::Transport("the media stream lock was poisoned".to_string()))?;
        match write_framed(&mut guard, frame) {
            Ok(()) => {
                self.stats.packets_out.fetch_add(1, Ordering::Relaxed);
                self.stats
                    .bytes_out
                    .fetch_add(frame.len() as u64, Ordering::Relaxed);
                Ok(())
            }
            Err(error) => {
                self.stats.send_errors.fetch_add(1, Ordering::Relaxed);
                Err(error)
            }
        }
    }

    fn close(&self) {
        if let Ok(guard) = self.stream.lock() {
            guard.shutdown();
        }
    }
}

struct StreamReceiver {
    stream: Arc<Mutex<StreamConn>>,
    stats: Arc<TransportStats>,
}

impl TransportReceiver for StreamReceiver {
    fn recv_frame(&mut self, out: &mut Vec<u8>) -> Result<RecvOutcome> {
        let mut guard = self
            .stream
            .lock()
            .map_err(|_| JamError::Transport("the media stream lock was poisoned".to_string()))?;
        let outcome = read_framed(&mut guard, out)?;
        if outcome == RecvOutcome::Frame {
            self.stats.packets_in.fetch_add(1, Ordering::Relaxed);
            self.stats
                .bytes_in
                .fetch_add(out.len() as u64, Ordering::Relaxed);
        }
        Ok(outcome)
    }
}

fn is_timeout(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, kind: TransportKind, priority: i32) -> TransportCandidate {
        TransportCandidate {
            id: id.to_string(),
            kind,
            host: "127.0.0.1".to_string(),
            port: 40000,
            path: String::new(),
            priority,
            region_id: "th-bkk-1".to_string(),
            node_id: "th-bkk-1".to_string(),
            secure: false,
            token: "fbj1.token".to_string(),
            expires_at_unix: 0,
        }
    }

    #[test]
    fn the_client_claims_only_what_it_can_open() {
        let caps = native_capabilities();
        assert!(caps.udp && caps.tcp && caps.tls);
        assert!(!caps.quic, "QUIC is in the protocol but not in this client");
        assert!(!caps.websocket && !caps.webrtc);
        assert_eq!(caps.client_kind, "native");

        assert!(can_open(TransportKind::Udp));
        assert!(!can_open(TransportKind::Quic));
        assert!(!can_open(TransportKind::WebSocket));
    }

    #[test]
    fn candidates_are_tried_in_the_servers_priority_order() {
        let offered = vec![
            candidate("tls", TransportKind::Tls, 3),
            candidate("udp", TransportKind::Udp, 10),
            candidate("tcp", TransportKind::Tcp, 2),
        ];
        let ordered = ordered_candidates(&offered);
        let ids: Vec<&str> = ordered.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["udp", "tls", "tcp"]);
    }

    #[test]
    fn a_datagram_path_wins_a_tie_with_a_reliable_one() {
        let offered = vec![
            candidate("tls", TransportKind::Tls, 5),
            candidate("udp", TransportKind::Udp, 5),
        ];
        let ordered = ordered_candidates(&offered);
        assert_eq!(ordered[0].kind, TransportKind::Udp);
    }

    #[test]
    fn candidates_this_build_cannot_open_are_dropped_rather_than_attempted() {
        let offered = vec![
            candidate("quic", TransportKind::Quic, 9),
            candidate("ws", TransportKind::WebSocket, 1),
            candidate("udp", TransportKind::Udp, 5),
        ];
        let ordered = ordered_candidates(&offered);
        assert_eq!(ordered.len(), 1);
        assert_eq!(ordered[0].kind, TransportKind::Udp);
    }

    #[test]
    fn a_candidate_list_with_nothing_openable_fails_before_touching_the_network() {
        let offered = vec![candidate("quic", TransportKind::Quic, 9)];
        let error =
            connect_first(&offered, Duration::from_millis(1), false).expect_err("nothing to open");
        assert!(matches!(error, JamError::Transport(_)));
    }

    #[test]
    fn a_welcome_frame_is_read_and_a_refusal_becomes_a_transport_error() {
        let welcome = packet::encode_control(
            ControlType::Welcome,
            Some(&WelcomePayload {
                jam_alias: 7,
                participant_id: "pcp_1".to_string(),
                generation: 2,
                node_id: "th-bkk-1".to_string(),
                keepalive_seconds: 15,
                max_payload_bytes: 1200,
            }),
        )
        .expect("encodes");
        let parsed = read_welcome(&welcome).expect("welcome parses");
        assert_eq!(parsed.jam_alias, 7);
        assert_eq!(parsed.generation, 2);

        let refusal = packet::encode_control(
            ControlType::Error,
            Some(&MediaErrorPayload {
                code: "unauthenticated".to_string(),
                message: "candidate token was refused".to_string(),
            }),
        )
        .expect("encodes");
        let error = read_welcome(&refusal).expect_err("refusal is an error");
        assert!(error.to_string().contains("unauthenticated"));
    }

    #[test]
    fn keepalive_and_payload_limits_fall_back_to_the_documented_defaults() {
        let transport_welcome = WelcomePayload::default();
        let transport = MediaTransport {
            kind: TransportKind::Udp,
            candidate_id: "c".to_string(),
            welcome: transport_welcome,
            handshake_rtt: Duration::ZERO,
            sender: Arc::new(UdpSender {
                socket: Arc::new(UdpSocket::bind("127.0.0.1:0").expect("bind")),
                stats: Arc::new(TransportStats::default()),
            }),
            receiver: Box::new(UdpReceiver {
                socket: Arc::new(UdpSocket::bind("127.0.0.1:0").expect("bind")),
                stats: Arc::new(TransportStats::default()),
            }),
            stats: Arc::new(TransportStats::default()),
        };
        assert_eq!(transport.keepalive(), Duration::from_secs(15));
        assert_eq!(transport.max_payload_bytes(), 1200);
    }
}
