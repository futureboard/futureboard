//! The REST control plane.
//!
//! Every route here exists on the server; nothing is invented. The set is
//! deliberately the jam surface only — friends, chat and presence live on the
//! same service but belong to a different feature, and adding them would give
//! this crate reasons to change that have nothing to do with audio.
//!
//! Blocking, like the rest of the client: the jam worker owns a thread.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::JamConfig;
use crate::credentials::SharedCredentials;
use crate::error::{JamError, Result, WireError};
use crate::protocol::{
    JamPermissions, JamSummary, ParticipantSummary, RegionProbe, RegionSummary, Role, StreamSummary,
};

/// A single jam, with where its media plane lives and the page that links to it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct JamResponse {
    #[serde(default)]
    pub jam: JamSummary,
    #[serde(default)]
    pub region: RegionSummary,
    #[serde(default)]
    pub join_url: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CreateJamRequest {
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub region: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub region_probes: Vec<RegionProbe>,
    #[serde(skip_serializing_if = "is_zero")]
    pub max_participants: i32,
    #[serde(skip_serializing_if = "is_zero")]
    pub ttl_seconds: i32,
}

fn is_zero(value: &i32) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CreateInviteRequest {
    pub role: Role,
    #[serde(skip_serializing_if = "is_zero")]
    pub ttl_seconds: i32,
    /// Zero means unlimited.
    #[serde(skip_serializing_if = "is_zero")]
    pub max_uses: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_account: Option<bool>,
}

/// The storable, secret-free view of an invite.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct InviteSummary {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub jam_id: String,
    #[serde(default)]
    pub role: Role,
    #[serde(default)]
    pub permissions: JamPermissions,
    #[serde(default)]
    pub expires_at_unix: i64,
    #[serde(default)]
    pub max_uses: i32,
    #[serde(default)]
    pub current_uses: i32,
    #[serde(default)]
    pub remaining_uses: i32,
    #[serde(default)]
    pub revoked: bool,
    #[serde(default)]
    pub require_account: bool,
}

/// The response to minting an invite.
///
/// `secret` and `link` are returned exactly once and are never stored by the
/// server. They are bearer secrets: hand them to the user, never to a log.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct InviteCreated {
    #[serde(default)]
    pub invite: InviteSummary,
    #[serde(default)]
    pub secret: String,
    #[serde(default)]
    pub link: String,
}

/// What a successful invite exchange returns. `access_token` authorises
/// `jam.join` for this account and this jam only, and expires in minutes.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct InviteExchanged {
    #[serde(default)]
    pub jam: JamSummary,
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub expires_at_unix: i64,
    #[serde(default)]
    pub role: Role,
    #[serde(default)]
    pub permissions: JamPermissions,
    #[serde(default)]
    pub region: RegionSummary,
    /// Where to open the signaling socket with this token. The client prefers
    /// its own configured signaling url; this is the server's own view, useful
    /// when a deployment moves.
    #[serde(default)]
    pub signaling_url: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RegionListing {
    #[serde(default)]
    pub regions: Vec<RegionSummary>,
    /// The region this node itself serves — the first thing worth probing.
    #[serde(default, rename = "home_region_id")]
    pub home_region_id: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ParticipantListing {
    #[serde(default)]
    participants: Vec<ParticipantSummary>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct StreamListing {
    #[serde(default)]
    streams: Vec<StreamSummary>,
}

#[derive(Debug, Clone, Serialize)]
struct ExchangeRequest<'a> {
    secret: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    public_id: &'a str,
}

/// The jam REST client.
pub struct JamApiClient {
    config: JamConfig,
    http: reqwest::blocking::Client,
    credentials: SharedCredentials,
}

impl JamApiClient {
    /// Build a client. The HTTP client is created once: rebuilding one per call
    /// would throw away the connection pool and pay a TLS handshake per
    /// request.
    pub fn new(config: JamConfig, credentials: SharedCredentials) -> Result<Self> {
        crate::crypto::ensure_crypto_provider();
        let http = reqwest::blocking::Client::builder()
            .timeout(config.connect_timeout.max(Duration::from_secs(1)))
            // Certificate validation is never relaxed. A development server on
            // loopback is reached over plaintext, which config already gates —
            // there is no path here that trusts a bad certificate.
            .build()
            .map_err(|error| {
                JamError::Http(format!("could not start the jam API client: {error}"))
            })?;
        Ok(Self {
            config,
            http,
            credentials,
        })
    }

    pub fn config(&self) -> &JamConfig {
        &self.config
    }

    /// `GET /v1/regions`
    pub fn regions(&self) -> Result<RegionListing> {
        self.get("/v1/regions")
    }

    /// `POST /v1/jams`. The host is the authenticated caller; there is no host
    /// field, and sending one would be ignored.
    pub fn create_jam(&self, request: &CreateJamRequest) -> Result<JamResponse> {
        self.post("/v1/jams", request)
    }

    /// `GET /v1/jams/{jam_id}`
    pub fn jam(&self, jam_id: &str) -> Result<JamResponse> {
        self.get(&format!("/v1/jams/{}", encode_segment(jam_id)))
    }

    /// `GET /v1/jams/by-code/{public_id}` — resolve a shareable code. A code is
    /// a lookup handle, not a credential.
    pub fn jam_by_code(&self, public_id: &str) -> Result<JamResponse> {
        self.get(&format!("/v1/jams/by-code/{}", encode_segment(public_id)))
    }

    /// `DELETE /v1/jams/{jam_id}`
    pub fn close_jam(&self, jam_id: &str) -> Result<()> {
        let url = self
            .config
            .api_endpoint(&format!("/v1/jams/{}", encode_segment(jam_id)))?;
        let response = self
            .authorized(self.http.delete(url))?
            .send()
            .map_err(http_error)?;
        self.check_status(response).map(|_| ())
    }

    /// `GET /v1/jams/{jam_id}/participants`
    pub fn participants(&self, jam_id: &str) -> Result<Vec<ParticipantSummary>> {
        let listing: ParticipantListing =
            self.get(&format!("/v1/jams/{}/participants", encode_segment(jam_id)))?;
        Ok(listing.participants)
    }

    /// `GET /v1/jams/{jam_id}/streams`
    pub fn streams(&self, jam_id: &str) -> Result<Vec<StreamSummary>> {
        let listing: StreamListing =
            self.get(&format!("/v1/jams/{}/streams", encode_segment(jam_id)))?;
        Ok(listing.streams)
    }

    /// `POST /v1/jams/{jam_id}/invites`
    pub fn create_invite(
        &self,
        jam_id: &str,
        request: &CreateInviteRequest,
    ) -> Result<InviteCreated> {
        self.post(
            &format!("/v1/jams/{}/invites", encode_segment(jam_id)),
            request,
        )
    }

    /// `POST /v1/invites/exchange`. The secret travels in the body, never in a
    /// url: a url ends up in proxy logs and browser history.
    pub fn exchange_invite(&self, secret: &str, public_id: &str) -> Result<InviteExchanged> {
        self.post(
            "/v1/invites/exchange",
            &ExchangeRequest { secret, public_id },
        )
    }

    // ── plumbing ────────────────────────────────────────────────────────────

    fn get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        let url = self.config.api_endpoint(path)?;
        let response = self
            .authorized(self.http.get(url))?
            .send()
            .map_err(http_error)?;
        self.decode(response, path)
    }

    fn post<B: Serialize, T: for<'de> Deserialize<'de>>(&self, path: &str, body: &B) -> Result<T> {
        let url = self.config.api_endpoint(path)?;
        let response = self
            .authorized(self.http.post(url))?
            .json(body)
            .send()
            .map_err(http_error)?;
        self.decode(response, path)
    }

    fn authorized(
        &self,
        builder: reqwest::blocking::RequestBuilder,
    ) -> Result<reqwest::blocking::RequestBuilder> {
        let token = self.credentials.access_token()?;
        Ok(builder.bearer_auth(token))
    }

    fn decode<T: for<'de> Deserialize<'de>>(
        &self,
        response: reqwest::blocking::Response,
        path: &str,
    ) -> Result<T> {
        let body = self.check_status(response)?;
        if body.trim().is_empty() {
            return Err(JamError::Api(WireError {
                code: crate::error::ErrorCode::Internal,
                message: format!("{path} returned an empty body"),
                retryable: true,
                request_id: String::new(),
            }));
        }
        serde_json::from_str(&body).map_err(|error| {
            JamError::Protocol(format!("{path}: could not read the response: {error}"))
        })
    }

    /// Turn a non-2xx response into the server's own error shape.
    ///
    /// A body that is not the documented shape still becomes a typed error
    /// rather than a parse failure: a proxy that returns HTML for a 502 must
    /// not look like a protocol bug.
    fn check_status(&self, response: reqwest::blocking::Response) -> Result<String> {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        if status.is_success() {
            return Ok(body);
        }
        if let Ok(wire) = serde_json::from_str::<WireError>(&body) {
            return Err(JamError::Api(wire));
        }
        Err(JamError::Api(WireError {
            code: status_code(status.as_u16()),
            message: format!("the jam service answered {}", status.as_u16()),
            retryable: status.is_server_error(),
            request_id: String::new(),
        }))
    }
}

fn http_error(error: reqwest::Error) -> JamError {
    // The url is included; the Authorization header never is, because reqwest's
    // own Display for an error carries only the url.
    JamError::Http(error.to_string())
}

fn status_code(status: u16) -> crate::error::ErrorCode {
    use crate::error::ErrorCode;
    match status {
        400 => ErrorCode::BadRequest,
        401 => ErrorCode::Unauthenticated,
        403 => ErrorCode::Forbidden,
        404 => ErrorCode::NotFound,
        409 => ErrorCode::Conflict,
        413 => ErrorCode::PayloadTooLarge,
        429 => ErrorCode::RateLimited,
        503 => ErrorCode::Unavailable,
        _ => ErrorCode::Internal,
    }
}

/// Percent-encode a path segment.
///
/// Jam and invite ids are ULID shaped and would never need it, but a public
/// code comes from a link the user pasted, and a `/` or `?` in one must not be
/// able to reach a different route.
fn encode_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_segments_cannot_escape_their_route() {
        assert_eq!(encode_segment("jam_01K4S8"), "jam_01K4S8");
        assert_eq!(encode_segment("../admin"), "..%2Fadmin");
        assert_eq!(encode_segment("a b?c=d"), "a%20b%3Fc%3Dd");
    }

    #[test]
    fn a_create_request_omits_the_fields_the_server_defaults() {
        let json = serde_json::to_string(&CreateJamRequest {
            name: "Saturday Session".to_string(),
            ..Default::default()
        })
        .expect("encodes");
        assert_eq!(json, r#"{"name":"Saturday Session"}"#);
    }

    #[test]
    fn an_exchange_response_parses_the_documented_body() {
        let raw = r#"{
            "jam": {"id":"jam_1","public_id":"J8KM4V","name":"Saturday Session"},
            "access_token": "fbj1.token",
            "expires_at_unix": 1772539200,
            "role": "performer",
            "permissions": {"receive_audio":true,"send_audio":true},
            "region": {"id":"th-bkk-1","endpoint":"127.0.0.1","udp_port":40000},
            "signaling_url": "ws://localhost:8090/v1/realtime"
        }"#;
        let exchanged: InviteExchanged = serde_json::from_str(raw).expect("parses");
        assert_eq!(exchanged.jam.public_id, "J8KM4V");
        assert_eq!(exchanged.role, "performer");
        assert!(exchanged.permissions.send_audio);
        assert_eq!(exchanged.region.udp_port, 40000);
    }

    #[test]
    fn a_status_without_a_json_body_still_becomes_a_typed_error() {
        assert_eq!(status_code(401), crate::error::ErrorCode::Unauthenticated);
        assert_eq!(status_code(429), crate::error::ErrorCode::RateLimited);
        assert_eq!(status_code(502), crate::error::ErrorCode::Internal);
    }
}
