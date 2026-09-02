//! Account sign-in against the Futureboard account service, session
//! persistence, and profile extraction.
//!
//! Community-wide: a Futureboard account is not an entitlement. What a *license*
//! grants still lives behind the Professional Edition's own verified license
//! provider, which asks this module for a session when a customer activates
//! from their account instead of entering a key.
//!
//! The account service is the website's own API on the site origin
//! (`futureboard.studio`), which is also what the licensing service asks when a
//! customer activates from their account instead of a key. One identity, one
//! session table, one place a sign-in can be revoked.
//!
//! The base must match the service's own `OAUTH_CALLBACK_BASE`. The flow sets
//! its state cookie on the host that starts it, and the callback lands on
//! whichever host the service is configured to redirect to — split them across
//! two hostnames and the cookie is simply absent on the callback.
//!
//! Trust model and secrets:
//! - **No client secret and no API key ship in the binary.** Sign-in happens in
//!   the system browser against the account service; the app only ever receives
//!   an opaque session credential for the account that signed in.
//! - The browser hands that session back through a one-shot **loopback**
//!   listener the app binds *before* the browser is pointed anywhere, on a port
//!   the OS assigns. The app generates a nonce, the service echoes it, and a
//!   callback that does not carry it is refused — so a stray page cannot plant
//!   someone else's session in the app.
//! - TLS is mandatory in release builds. A debug build may point at a local
//!   account service on loopback.
//!
//! The session credential is stored in the per-user app-data directory
//! (`session.json`), which the OS already restricts to the user's account. An OS
//! keychain would harden this further and is a documented follow-up.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// Futureboard account API base, e.g. `https://futureboard.studio`. Baked
/// at build time from `.env`/the build environment; absent in an unconfigured
/// build, which then renders the sign-in UI disabled rather than guessing.
const BAKED_AUTH_API_URL: Option<&str> = option_env!("FUTUREBOARD_AUTH_API_URL");

const SESSION_FILE: &str = "session.json";

/// Overall ceiling on a browser sign-in before the loopback gives up, so a user
/// who closes the browser never leaks the waiting thread.
const OAUTH_WAIT_SECS: u64 = 180;

/// Network timeout for a single auth HTTP call.
const HTTP_TIMEOUT_SECS: u64 = 20;

/// How long a stored session is trusted before it is re-confirmed with the
/// service. Sessions last far longer than this; the check is what notices a
/// sign-out performed elsewhere, so the app is not left claiming a dead one.
const SESSION_RECHECK_SECS: u64 = 12 * 60 * 60;

fn auth_debug_enabled() -> bool {
    std::env::var("FUTUREBOARD_AUTH_DEBUG")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn debug_log(message: &str) {
    if auth_debug_enabled() {
        eprintln!("[Auth] {message}");
    }
}

// ── Config ───────────────────────────────────────────────────────────────────

/// Resolve the account API base URL. Release builds use the baked value and
/// require TLS; debug builds may override it for local development.
pub fn api_base_url() -> Option<String> {
    #[cfg(debug_assertions)]
    {
        if let Some(url) = std::env::var("FUTUREBOARD_AUTH_API_URL")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty() && is_acceptable_endpoint(value))
        {
            return Some(url);
        }
    }
    BAKED_AUTH_API_URL
        .map(|url| url.trim().trim_end_matches('/').to_string())
        .filter(|url| !url.is_empty() && is_acceptable_endpoint(url))
}

/// TLS is mandatory. Debug builds additionally accept plaintext loopback so a
/// local account service can be developed against; `cfg!` folds to `false` in
/// release, so a shipped build has no plaintext path at all.
fn is_acceptable_endpoint(url: &str) -> bool {
    if url.starts_with("https://") {
        return true;
    }
    cfg!(debug_assertions)
        && (url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost"))
}

/// Whether this build can offer sign-in at all.
pub fn auth_configured() -> bool {
    api_base_url().is_some()
}

// ── Models ───────────────────────────────────────────────────────────────────

/// The signed-in user's public profile, pulled from the account service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfile {
    pub id: String,
    pub email: Option<String>,
    pub username: Option<String>,
    pub avatar_url: Option<String>,
    /// Whether the account service has proven this address belongs to the user.
    /// Account-based licensing requires it, so the UI can explain the gap
    /// instead of surfacing a bare refusal.
    #[serde(default)]
    pub email_verified: bool,
}

/// A persisted session. The credential is module-private so callers can never
/// read it out of a profile; serde still (de)serializes it within this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Opaque session id issued by the account service, sent as a bearer token.
    token: String,
    /// Unix seconds this session was last confirmed with the service.
    #[serde(default)]
    checked_at: u64,
    pub user: UserProfile,
}

/// OAuth identity providers the account service wires up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthProvider {
    Google,
    Discord,
    GitHub,
}

impl OAuthProvider {
    /// Path segment used by the account service (`/auth/{provider}`).
    fn as_path(self) -> &'static str {
        match self {
            OAuthProvider::Google => "google",
            OAuthProvider::Discord => "discord",
            OAuthProvider::GitHub => "github",
        }
    }

    /// Label for the sign-in button.
    pub fn label(self) -> &'static str {
        match self {
            OAuthProvider::Google => "Continue with Google",
            OAuthProvider::Discord => "Continue with Discord",
            OAuthProvider::GitHub => "Continue with GitHub",
        }
    }
}

// ── In-memory session cache ──────────────────────────────────────────────────

fn cache() -> &'static RwLock<Option<Session>> {
    static CACHE: OnceLock<RwLock<Option<Session>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(None))
}

/// Load the persisted session into memory and re-confirm it in the background
/// when it has not been checked recently. Call once at startup. Never blocks on
/// network — a slow account service must not delay the DAW opening.
pub fn init_session() {
    if let Some(session) = load_persisted_session() {
        debug_log(&format!(
            "loaded stored session for {}",
            session.user.username.as_deref().unwrap_or("(user)")
        ));
        if let Ok(mut guard) = cache().write() {
            *guard = Some(session);
        }
        spawn_refresh_if_due();
    }
}

/// The current signed-in profile, or `None` when signed out. Reads memory only.
pub fn current_profile() -> Option<UserProfile> {
    cache().read().ok()?.as_ref().map(|s| s.user.clone())
}

/// The session credential for the signed-in account, for callers that need to
/// prove who this is to another Futureboard service — today, the licensing
/// service's account activation.
///
/// Public only because the account layer and the licensing layer now live in
/// different crates: an account is Community-wide, what a license grants is
/// not. This is a **live credential** — the one caller sends it straight to the
/// activation endpoint over TLS. Never log it, never persist a second copy, and
/// never hand it to anything outside a Futureboard service.
pub fn session_token() -> Option<String> {
    cache().read().ok()?.as_ref().map(|s| s.token.clone())
}

// ── HTTP ─────────────────────────────────────────────────────────────────────

fn http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .https_only(!cfg!(debug_assertions))
        // The account service answers a native sign-in with a redirect to the
        // loopback listener; that hop belongs to the browser, never to this
        // client, so nothing here should chase a Location header.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("could not initialize sign-in: {error}"))
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The account service's `/me`. Only the fields the app shows are declared, so
/// the service can grow without breaking this build.
#[derive(Deserialize)]
struct MeResponse {
    user: AccountUser,
}

#[derive(Deserialize)]
struct AccountUser {
    #[serde(default)]
    id: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "avatarUrl")]
    avatar_url: Option<String>,
    #[serde(default, rename = "emailVerifiedAt")]
    email_verified_at: Option<String>,
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Extract the display profile from an account user. The display name falls back
/// to the email local-part, which is what an OAuth provider that shares no name
/// leaves behind.
fn profile_from_user(user: AccountUser) -> UserProfile {
    let email = non_empty(user.email.as_deref());
    let username = non_empty(user.name.as_deref()).or_else(|| {
        email
            .as_deref()
            .and_then(|email| email.split('@').next())
            .map(str::to_string)
    });
    UserProfile {
        id: user.id,
        email,
        username,
        avatar_url: non_empty(user.avatar_url.as_deref()),
        email_verified: non_empty(user.email_verified_at.as_deref()).is_some(),
    }
}

/// Ask the account service who a session belongs to. Doubles as the liveness
/// check: a session the service no longer knows answers 401 here.
fn fetch_profile(token: &str) -> Result<UserProfile, String> {
    let base = api_base_url().ok_or("sign-in is not configured for this build")?;
    let response = http_client()?
        .get(format!("{base}/me"))
        .bearer_auth(token)
        .header("Accept", "application/json")
        .send()
        .map_err(|error| format!("the account service could not be reached: {error}"))?;

    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err("that sign-in is no longer valid".to_string());
    }
    if !status.is_success() {
        return Err(format!("the account service returned {status}"));
    }
    let payload = response
        .json::<MeResponse>()
        .map_err(|error| format!("the account service returned an invalid response: {error}"))?;
    Ok(profile_from_user(payload.user))
}

/// Persist and cache a freshly obtained session.
fn apply_session(token: String, user: UserProfile) -> Result<UserProfile, String> {
    let session = Session {
        token,
        checked_at: now_unix(),
        user: user.clone(),
    };
    store_persisted_session(&session)?;
    if let Ok(mut guard) = cache().write() {
        *guard = Some(session);
    }
    Ok(user)
}

// ── Browser sign-in via the loopback callback ────────────────────────────────

/// Sign in with `provider` and, on success, pull the profile.
///
/// Blocking: opens the system browser, runs a one-shot loopback HTTP listener to
/// catch the redirect, then confirms the handed-back session. Meant to run on a
/// background thread — the UI keeps it off the GPUI thread.
///
/// There is no password path. The account service authenticates people through
/// their identity provider only, so a password field here would be a control
/// that cannot work.
pub fn oauth_sign_in(provider: OAuthProvider) -> Result<UserProfile, String> {
    let base = api_base_url().ok_or("sign-in is not configured for this build")?;

    // Bind the listener up front so the port is ours before the browser is
    // pointed at it, and let the OS pick it: a fixed port is one already-running
    // instance away from failing, and nothing about this port needs to be known
    // in advance — the account service is told it in the request.
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .map_err(|error| format!("could not open a local sign-in port: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("could not read the local sign-in port: {error}"))?
        .port();

    // The nonce is generated here, not by the service: it is what proves the
    // callback answers *this* attempt, so a page that guesses the port cannot
    // hand the app a session belonging to someone else.
    let nonce = random_token(24);
    let authorize = format!(
        "{base}/auth/{provider}?native={port}&nonce={nonce}",
        provider = provider.as_path(),
        nonce = url_encode(&nonce),
    );

    open_in_browser(&authorize)?;
    let token = wait_for_session(listener, &nonce, Duration::from_secs(OAUTH_WAIT_SECS))?;

    let profile = fetch_profile(&token)?;
    apply_session(token, profile)
}

/// One-shot loopback capture of the `?fbsession=` (or `?error=`) redirect.
/// Verifies the `fbstate` nonce, ignores unrelated requests (e.g. favicon), and
/// bounds the wait so a thread never leaks.
fn wait_for_session(
    listener: TcpListener,
    expected_state: &str,
    deadline: Duration,
) -> Result<String, String> {
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("local sign-in listener failed: {error}"))?;
    let start = Instant::now();
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                if let Some(result) = handle_callback(stream, expected_state) {
                    return result;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if start.elapsed() >= deadline {
                    return Err("timed out waiting for the browser sign-in".to_string());
                }
                std::thread::sleep(Duration::from_millis(120));
            }
            Err(error) => return Err(format!("local sign-in listener failed: {error}")),
        }
    }
}

/// Parse one loopback request. `None` means "not the callback, keep waiting";
/// `Some` is the terminal result (session credential or error).
fn handle_callback(mut stream: TcpStream, expected_state: &str) -> Option<Result<String, String>> {
    let mut reader = BufReader::new(&stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return None;
    }

    // "GET /path?query HTTP/1.1"
    let target = request_line.split_whitespace().nth(1).unwrap_or("");
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
    let params = parse_query(query);

    let has_auth = params.iter().any(|(k, _)| k == "fbsession" || k == "error");
    if !has_auth {
        // Favicon or a stray hit — acknowledge and keep listening.
        let _ = write_browser_response(&mut stream, "Waiting for sign-in…");
        return None;
    }

    let state_ok = params
        .iter()
        .find(|(k, _)| k == "fbstate")
        .map(|(_, v)| constant_time_eq(v.as_bytes(), expected_state.as_bytes()))
        .unwrap_or(false);
    if !state_ok {
        let _ = write_browser_response(&mut stream, "Sign-in could not be verified.");
        return Some(Err("the sign-in response failed verification".to_string()));
    }

    if let Some((_, error)) = params.iter().find(|(k, _)| k == "error") {
        let _ = write_browser_response(&mut stream, "Sign-in was cancelled.");
        return Some(Err(format!("sign-in was refused: {error}")));
    }

    match params.into_iter().find(|(k, _)| k == "fbsession") {
        Some((_, session)) if !session.is_empty() => {
            let _ = write_browser_response(
                &mut stream,
                "Signed in to Futureboard Studio. You can close this tab.",
            );
            Some(Ok(session))
        }
        _ => {
            let _ = write_browser_response(&mut stream, "Sign-in did not complete.");
            Some(Err(
                "the sign-in response did not include a session".to_string()
            ))
        }
    }
}

fn write_browser_response(stream: &mut TcpStream, message: &str) -> std::io::Result<()> {
    // Plain text, no reflected input beyond our own fixed strings.
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>Futureboard Studio</title>\
         <body style=\"font-family:system-ui;background:#16181d;color:#e6e6e6;\
         display:flex;align-items:center;justify-content:center;height:100vh;margin:0\">\
         <p>{message}</p></body>"
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

// ── Session liveness ─────────────────────────────────────────────────────────

fn recheck_is_due(session: &Session) -> bool {
    now_unix().saturating_sub(session.checked_at) >= SESSION_RECHECK_SECS
}

/// Re-confirm the stored session with the account service and refresh the cached
/// profile (a renamed account or a new avatar lands here).
fn recheck_session() -> Result<UserProfile, String> {
    let token = session_token().ok_or("there is no session to check")?;
    let profile = fetch_profile(&token)?;
    apply_session(token, profile)
}

/// Re-confirm the stored session in the background when it is stale. Returns
/// immediately; a signed-out or recently-checked session does nothing.
///
/// Only an answer from the service — "this session is not valid" — signs the app
/// out. An unreachable service leaves the session alone, because a musician
/// offline is not a musician signed out.
pub fn spawn_refresh_if_due() {
    let due = cache()
        .read()
        .ok()
        .and_then(|guard| guard.as_ref().map(recheck_is_due))
        .unwrap_or(false);
    if !due || !auth_configured() {
        return;
    }

    std::thread::Builder::new()
        .name("futureboard-auth-check".into())
        .spawn(|| match recheck_session() {
            Ok(_) => debug_log("session confirmed"),
            Err(error) => {
                if error.contains("no longer valid") {
                    debug_log(&format!("{error}; signing out"));
                    clear_local_session();
                } else {
                    debug_log(&format!("session check deferred: {error}"));
                }
            }
        })
        .map(|_| ())
        .unwrap_or_else(|error| debug_log(&format!("could not start session check: {error}")));
}

// ── Sign out ─────────────────────────────────────────────────────────────────

/// Sign out: clear the local session immediately, then best-effort end it on the
/// service so the same credential cannot be used again. Local sign-out never
/// fails.
pub fn sign_out() {
    let token = session_token();
    clear_local_session();

    let (Some(base), Some(token)) = (api_base_url(), token) else {
        return;
    };
    std::thread::Builder::new()
        .name("futureboard-auth-logout".into())
        .spawn(move || {
            let result = http_client().and_then(|client| {
                client
                    .post(format!("{base}/auth/logout"))
                    .bearer_auth(&token)
                    .send()
                    .map_err(|error| format!("logout call failed: {error}"))
            });
            if let Err(error) = result {
                debug_log(&error);
            }
        })
        .ok();
}

fn clear_local_session() {
    if let Ok(mut guard) = cache().write() {
        *guard = None;
    }
    let path = session_path();
    if path.exists() {
        if let Err(error) = std::fs::remove_file(&path) {
            debug_log(&format!("could not remove stored session: {error}"));
        }
    }
}

// ── Persistence ──────────────────────────────────────────────────────────────

fn app_data_dir() -> PathBuf {
    crate::paths::FutureboardPaths::resolve().app_data
}

fn session_path() -> PathBuf {
    app_data_dir().join(SESSION_FILE)
}

fn store_persisted_session(session: &Session) -> Result<(), String> {
    let path = session_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create the session directory: {error}"))?;
    }
    let json = serde_json::to_vec_pretty(session)
        .map_err(|error| format!("could not encode the session: {error}"))?;
    std::fs::write(&path, json).map_err(|error| format!("could not save the session: {error}"))
}

fn load_persisted_session() -> Option<Session> {
    let bytes = std::fs::read(session_path()).ok()?;
    match serde_json::from_slice::<Session>(&bytes) {
        Ok(session) => Some(session),
        Err(error) => {
            // A session written by an older build (the Supabase shape) lands
            // here. Dropping it costs one sign-in and is the only honest read of
            // a credential this build cannot use.
            debug_log(&format!("stored session is unreadable: {error}"));
            None
        }
    }
}

// ── Small helpers ────────────────────────────────────────────────────────────

/// A URL-safe random token from OS entropy, ~`bytes` of randomness.
fn random_token(bytes: usize) -> String {
    use rand::RngCore;
    let mut buffer = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buffer);
    URL_SAFE_NO_PAD.encode(buffer)
}

fn open_in_browser(url: &str) -> Result<(), String> {
    let mut command = browser_command();
    command
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("could not open your browser for sign-in: {error}"))
}

#[cfg(target_os = "windows")]
fn browser_command() -> std::process::Command {
    // rundll32 hands the URL straight to the default handler, avoiding cmd's
    // `start` quoting quirks with `&` in OAuth query strings.
    let mut command = std::process::Command::new("rundll32.exe");
    command.arg("url.dll,FileProtocolHandler");
    command
}

#[cfg(target_os = "macos")]
fn browser_command() -> std::process::Command {
    std::process::Command::new("open")
}

#[cfg(target_os = "linux")]
fn browser_command() -> std::process::Command {
    std::process::Command::new("xdg-open")
}

/// Percent-encode a value for use inside a URL query. Conservative: only
/// unreserved characters pass through unescaped.
fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Parse `a=b&c=d` into pairs, percent-decoding each side.
fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|part| !part.is_empty())
        .map(|part| match part.split_once('=') {
            Some((key, value)) => (percent_decode(key), percent_decode(value)),
            None => (percent_decode(part), String::new()),
        })
        .collect()
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hi = (bytes[index + 1] as char).to_digit(16);
                let lo = (bytes[index + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push(((hi << 4) | lo) as u8);
                    index += 3;
                    continue;
                }
                out.push(b'%');
                index += 1;
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            other => {
                out.push(other);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Length-independent compare for the callback nonce.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_prefers_the_account_name_then_the_email_localpart() {
        let profile = profile_from_user(AccountUser {
            id: "abc".to_string(),
            email: Some("jane@example.com".to_string()),
            name: Some("  Jane Doe ".to_string()),
            avatar_url: Some("https://cdn/x.png".to_string()),
            email_verified_at: Some("2026-01-01T00:00:00Z".to_string()),
        });
        assert_eq!(profile.username.as_deref(), Some("Jane Doe"));
        assert_eq!(profile.avatar_url.as_deref(), Some("https://cdn/x.png"));
        assert_eq!(profile.email.as_deref(), Some("jane@example.com"));
        assert!(profile.email_verified);
    }

    #[test]
    fn profile_falls_back_to_the_email_localpart() {
        let profile = profile_from_user(AccountUser {
            id: "abc".to_string(),
            email: Some("bob@example.com".to_string()),
            name: None,
            avatar_url: None,
            email_verified_at: None,
        });
        assert_eq!(profile.username.as_deref(), Some("bob"));
        assert!(profile.avatar_url.is_none());
        // Account licensing needs a proven address, so an unproven one must not
        // read as verified.
        assert!(!profile.email_verified);
    }

    #[test]
    fn query_parsing_and_percent_decoding() {
        let params = parse_query("fbsession=ab%2Fcd&fbstate=xyz&error=");
        assert_eq!(params[0], ("fbsession".to_string(), "ab/cd".to_string()));
        assert_eq!(params[1], ("fbstate".to_string(), "xyz".to_string()));
        assert_eq!(params[2], ("error".to_string(), String::new()));
    }

    #[test]
    fn url_encode_escapes_reserved_characters() {
        assert_eq!(
            url_encode("http://127.0.0.1:8788/?fbstate=a b"),
            "http%3A%2F%2F127.0.0.1%3A8788%2F%3Ffbstate%3Da%20b"
        );
    }

    /// The nonce is the only thing standing between the loopback listener and a
    /// session someone else handed it.
    #[test]
    fn the_callback_nonce_must_match() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert_ne!(random_token(24), random_token(24));
    }

    /// A plaintext account service would put a live session credential on the
    /// wire in clear. Release builds have no such path at all.
    #[test]
    fn plaintext_remote_endpoints_are_never_acceptable() {
        assert!(!is_acceptable_endpoint("http://auth.example.com"));
        assert!(is_acceptable_endpoint("https://futureboard.studio"));
    }
}
