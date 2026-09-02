//! Typed failures.
//!
//! Two audiences, one type. [`JamError::user_message`] is what a panel shows;
//! the `Display` form carries the diagnostic detail that belongs in a log.
//! Neither ever contains a credential: the API client and the signaling client
//! both build errors from status codes and server error payloads, never from
//! the request they sent.

use std::fmt;

/// The server's structured error payload. It is the single error shape across
/// the whole public surface — REST bodies and `error` envelopes alike.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WireError {
    pub code: ErrorCode,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default)]
    pub request_id: String,
}

impl std::error::Error for WireError {}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

/// Stable, machine-readable failure identifiers, mirroring the server's
/// `protocol.ErrorCode`.
///
/// `Unknown` keeps a future server code from turning into a parse failure: an
/// unrecognised code still reaches the UI as a message, which is strictly
/// better than "malformed response".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(from = "String", into = "String")]
pub enum ErrorCode {
    BadRequest,
    UnsupportedVersion,
    Unauthenticated,
    Forbidden,
    NotFound,
    Conflict,
    JamClosed,
    JamFull,
    InviteExpired,
    InviteRevoked,
    InviteExhausted,
    InviteInvalid,
    AccountRequired,
    NegotiationFailed,
    RateLimited,
    PayloadTooLarge,
    ResumeRejected,
    Internal,
    Unavailable,
    Unknown(String),
}

impl ErrorCode {
    pub fn as_str(&self) -> &str {
        match self {
            Self::BadRequest => "bad_request",
            Self::UnsupportedVersion => "unsupported_version",
            Self::Unauthenticated => "unauthenticated",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::JamClosed => "jam_closed",
            Self::JamFull => "jam_full",
            Self::InviteExpired => "invite_expired",
            Self::InviteRevoked => "invite_revoked",
            Self::InviteExhausted => "invite_exhausted",
            Self::InviteInvalid => "invite_invalid",
            Self::AccountRequired => "account_required",
            Self::NegotiationFailed => "negotiation_failed",
            Self::RateLimited => "rate_limited",
            Self::PayloadTooLarge => "payload_too_large",
            Self::ResumeRejected => "resume_rejected",
            Self::Internal => "internal",
            Self::Unavailable => "unavailable",
            Self::Unknown(raw) => raw,
        }
    }

    /// Whether retrying the same call could succeed. A resume rejection counts:
    /// the client drops its resume token and joins fresh.
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::RateLimited | Self::Unavailable | Self::Internal | Self::ResumeRejected
        )
    }

    /// Whether reconnecting would be pointless without user action.
    pub fn terminal(&self) -> bool {
        matches!(
            self,
            Self::Unauthenticated
                | Self::Forbidden
                | Self::NotFound
                | Self::JamClosed
                | Self::JamFull
                | Self::AccountRequired
                | Self::InviteExpired
                | Self::InviteRevoked
                | Self::InviteExhausted
                | Self::InviteInvalid
        )
    }
}

impl From<String> for ErrorCode {
    fn from(raw: String) -> Self {
        match raw.as_str() {
            "bad_request" => Self::BadRequest,
            "unsupported_version" => Self::UnsupportedVersion,
            "unauthenticated" => Self::Unauthenticated,
            "forbidden" => Self::Forbidden,
            "not_found" => Self::NotFound,
            "conflict" => Self::Conflict,
            "jam_closed" => Self::JamClosed,
            "jam_full" => Self::JamFull,
            "invite_expired" => Self::InviteExpired,
            "invite_revoked" => Self::InviteRevoked,
            "invite_exhausted" => Self::InviteExhausted,
            "invite_invalid" => Self::InviteInvalid,
            "account_required" => Self::AccountRequired,
            "negotiation_failed" => Self::NegotiationFailed,
            "rate_limited" => Self::RateLimited,
            "payload_too_large" => Self::PayloadTooLarge,
            "resume_rejected" => Self::ResumeRejected,
            "internal" => Self::Internal,
            "unavailable" => Self::Unavailable,
            _ => Self::Unknown(raw),
        }
    }
}

impl From<ErrorCode> for String {
    fn from(code: ErrorCode) -> Self {
        code.as_str().to_string()
    }
}

/// Everything that can go wrong in a jam client, grouped by the subsystem that
/// produced it so a caller can decide what to do without string matching.
#[derive(Debug, thiserror::Error)]
pub enum JamError {
    #[error("jam configuration: {0}")]
    Config(String),

    #[error("jam sign-in: {0}")]
    Auth(String),

    #[error("jam api: {0}")]
    Api(#[from] WireError),

    #[error("jam api transport: {0}")]
    Http(String),

    #[error("jam signaling: {0}")]
    WebSocket(String),

    #[error("jam protocol: {0}")]
    Protocol(String),

    #[error("jam media transport: {0}")]
    Transport(String),

    #[error("jam audio: {0}")]
    Audio(String),

    #[error("jam codec: {0}")]
    Codec(String),

    #[error("jam clock: {0}")]
    Clock(String),

    #[error("jam session: {0}")]
    Session(String),
}

impl JamError {
    /// A sentence for a panel. Server messages are developer-facing, so the
    /// mapping is by code where one exists and by category otherwise.
    pub fn user_message(&self) -> String {
        match self {
            Self::Api(wire) => match wire.code {
                ErrorCode::Unauthenticated | ErrorCode::AccountRequired => {
                    "Sign in to your Futureboard account to join this jam.".to_string()
                }
                ErrorCode::Forbidden => "You do not have access to this jam.".to_string(),
                ErrorCode::NotFound => "That jam no longer exists.".to_string(),
                ErrorCode::JamClosed => "This jam has ended.".to_string(),
                ErrorCode::JamFull => "This jam is full.".to_string(),
                ErrorCode::InviteExpired => "That invite has expired.".to_string(),
                ErrorCode::InviteRevoked => "That invite was revoked.".to_string(),
                ErrorCode::InviteExhausted => "That invite has been used up.".to_string(),
                ErrorCode::InviteInvalid => "That invite link is not valid.".to_string(),
                ErrorCode::RateLimited => "Too many requests. Try again shortly.".to_string(),
                ErrorCode::Unavailable | ErrorCode::Internal => {
                    "The jam service is unavailable. Try again shortly.".to_string()
                }
                _ => "The jam service refused that request.".to_string(),
            },
            Self::Config(_) => "Audio Jam is not configured in this build.".to_string(),
            Self::Auth(_) => "Sign in to your Futureboard account to join a jam.".to_string(),
            Self::Http(_) | Self::WebSocket(_) => "Could not reach the jam service.".to_string(),
            Self::Transport(_) => "Could not open a media connection to the jam.".to_string(),
            Self::Codec(_) => "No audio format is shared with this jam.".to_string(),
            Self::Protocol(_) => {
                "The jam service sent something this build cannot read.".to_string()
            }
            Self::Audio(_) => "Jam audio could not be started.".to_string(),
            Self::Clock(_) => "Jam clock synchronisation failed.".to_string(),
            Self::Session(_) => "The jam session ended.".to_string(),
        }
    }

    /// Whether a reconnect loop should keep trying.
    pub fn recoverable(&self) -> bool {
        match self {
            Self::Api(wire) => !wire.code.terminal(),
            Self::Config(_) | Self::Auth(_) | Self::Codec(_) => false,
            _ => true,
        }
    }
}

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, JamError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_error_codes_survive_a_round_trip() {
        let wire: WireError =
            serde_json::from_str(r#"{"code":"teapot","message":"nope","retryable":false}"#)
                .expect("unknown codes parse");
        assert_eq!(wire.code, ErrorCode::Unknown("teapot".to_string()));
        assert_eq!(wire.code.as_str(), "teapot");
    }

    #[test]
    fn terminal_codes_stop_the_reconnect_loop() {
        let closed = JamError::Api(WireError {
            code: ErrorCode::JamClosed,
            message: "the jam ended".into(),
            retryable: false,
            request_id: String::new(),
        });
        assert!(!closed.recoverable());

        let busy = JamError::Api(WireError {
            code: ErrorCode::Unavailable,
            message: "try later".into(),
            retryable: true,
            request_id: String::new(),
        });
        assert!(busy.recoverable());
    }

    #[test]
    fn user_messages_never_repeat_server_detail() {
        let err = JamError::Api(WireError {
            code: ErrorCode::Unauthenticated,
            message: "bearer token fbj1.secret was refused".into(),
            retryable: false,
            request_id: String::new(),
        });
        assert!(!err.user_message().contains("fbj1"));
    }
}
