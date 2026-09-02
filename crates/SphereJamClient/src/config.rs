//! Where the jam service lives, and how hard to try reaching it.
//!
//! Three sources, highest wins:
//!
//! 1. an explicit [`JamConfig`] built by the caller,
//! 2. the process environment, optionally seeded from the workspace `.env` in a
//!    development build,
//! 3. compiled-in defaults.
//!
//! A production binary therefore never depends on a `.env` file existing: with
//! no environment at all it points at the public service. The `.env` path is a
//! development convenience and is read only by a debug build.
//!
//! Nothing here is a secret. The client config carries public endpoints and
//! timings only; the credential comes from the signed-in Futureboard account
//! session at call time (see [`crate::credentials`]).

use std::path::{Path, PathBuf};
use std::time::Duration;

use url::Url;

use crate::error::{JamError, Result};

/// Public jam service, used when nothing overrides it.
pub const DEFAULT_API_URL: &str = "https://jam.futureboard.studio";
/// Signaling endpoint on the public service.
pub const DEFAULT_WS_URL: &str = "wss://jam.futureboard.studio/v1/realtime";
/// The browser-facing origin that serves jam links.
pub const DEFAULT_WEB_URL: &str = "https://jam.futureboard.studio";

const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 10_000;
const DEFAULT_RECONNECT_MAX_DELAY_MS: u64 = 10_000;

/// Which region the client asks for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RegionPreference {
    /// Let the server's selector choose, seeded by whatever probes the client
    /// has measured. The default: the server knows the topology and the load,
    /// and a client that pins one by accident strands its whole room there.
    #[default]
    Auto,
    /// Pin a specific region id.
    Pinned(String),
}

impl RegionPreference {
    /// Parse the configured value.
    ///
    /// Short aliases exist because `th-bkk` reads better in a config file than
    /// `th-bkk-1`, but the id sent to the server is always the full region id.
    /// An unknown value is an error rather than a silent fall back to `auto`:
    /// a typo that quietly moved a session to another continent would be very
    /// hard to notice from inside the room.
    pub fn parse(raw: &str) -> Result<Self> {
        let value = raw.trim().to_ascii_lowercase();
        if value.is_empty() || value == "auto" {
            return Ok(Self::Auto);
        }
        let canonical = match value.as_str() {
            "th-bkk" | "th-bkk-1" => "th-bkk-1",
            "th-ptn" | "th-ptn-1" => "th-ptn-1",
            "sg-sin" | "sg-sin-1" => "sg-sin-1",
            "id-jkt" | "id-jkt-1" => "id-jkt-1",
            other => {
                return Err(JamError::Config(format!(
                    "unknown jam region {other:?}; expected auto, th-bkk, th-ptn, sg-sin or id-jkt"
                )))
            }
        };
        Ok(Self::Pinned(canonical.to_string()))
    }

    /// The value to put in a join request. Empty means "let the server pick".
    pub fn wire_value(&self) -> &str {
        match self {
            Self::Auto => "",
            Self::Pinned(id) => id,
        }
    }
}

/// Which build this is, as far as endpoint policy is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JamEnv {
    Development,
    Production,
}

impl JamEnv {
    fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "development" | "dev" | "local" => Self::Development,
            _ => Self::Production,
        }
    }
}

/// Resolved, validated jam endpoints and timings.
#[derive(Debug, Clone)]
pub struct JamConfig {
    pub env: JamEnv,
    pub api_url: Url,
    pub websocket_url: Url,
    pub web_url: Option<Url>,
    pub preferred_region: RegionPreference,
    pub connect_timeout: Duration,
    pub reconnect: bool,
    pub reconnect_max_delay: Duration,
    /// Diagnostic verbosity for the jam subsystem only.
    pub log_level: String,
    /// A development-only account token, used when the local jam server runs
    /// with `JAM_AUTH_MODE=dev` and therefore cannot validate a real
    /// Futureboard session. Refused outside a debug build.
    pub dev_token: Option<String>,
}

impl Default for JamConfig {
    fn default() -> Self {
        Self {
            env: JamEnv::Production,
            api_url: Url::parse(DEFAULT_API_URL).expect("the compiled default API url is valid"),
            websocket_url: Url::parse(DEFAULT_WS_URL)
                .expect("the compiled default signaling url is valid"),
            web_url: Url::parse(DEFAULT_WEB_URL).ok(),
            preferred_region: RegionPreference::Auto,
            connect_timeout: Duration::from_millis(DEFAULT_CONNECT_TIMEOUT_MS),
            reconnect: true,
            reconnect_max_delay: Duration::from_millis(DEFAULT_RECONNECT_MAX_DELAY_MS),
            log_level: "warn".to_string(),
            dev_token: None,
        }
    }
}

impl JamConfig {
    /// Load from the process environment over the compiled defaults.
    ///
    /// A debug build additionally seeds unset variables from the workspace
    /// `.env`, so `cargo run` reaches a local `jamd` without exporting
    /// anything. A release build never reads a file.
    pub fn from_env() -> Result<Self> {
        let mut source = EnvSource::process();
        #[cfg(debug_assertions)]
        {
            if let Some(path) = workspace_dotenv_path() {
                source.seed_from_dotenv(&path);
            }
        }
        Self::from_source(&source)
    }

    /// Build from an explicit key/value source. The `from_env` path is this
    /// with the process environment; tests use it with a fixed map.
    pub fn from_source(source: &EnvSource) -> Result<Self> {
        let mut config = Self::default();

        if let Some(value) = source.get("FUTUREBOARD_ENV") {
            config.env = JamEnv::parse(&value);
        }

        if let Some(value) = source.get("FUTUREBOARD_JAM_API_URL") {
            config.api_url = parse_endpoint(&value, "FUTUREBOARD_JAM_API_URL", &["http", "https"])?;
        }
        if let Some(value) = source.get("FUTUREBOARD_JAM_WS_URL") {
            config.websocket_url =
                parse_endpoint(&value, "FUTUREBOARD_JAM_WS_URL", &["ws", "wss"])?;
        } else if source.get("FUTUREBOARD_JAM_API_URL").is_some() {
            // Deriving the signaling url from the API base is what the server's
            // own quick start does, and it removes the commonest development
            // mistake: pointing the two halves at different processes.
            config.websocket_url = derive_signaling_url(&config.api_url)?;
        }
        if let Some(value) = source.get("FUTUREBOARD_JAM_WEB_URL") {
            config.web_url = Some(parse_endpoint(
                &value,
                "FUTUREBOARD_JAM_WEB_URL",
                &["http", "https"],
            )?);
        }
        if let Some(value) = source.get("FUTUREBOARD_JAM_REGION") {
            config.preferred_region = RegionPreference::parse(&value)?;
        }
        if let Some(value) = source.get("FUTUREBOARD_JAM_CONNECT_TIMEOUT_MS") {
            config.connect_timeout =
                Duration::from_millis(parse_millis(&value, "FUTUREBOARD_JAM_CONNECT_TIMEOUT_MS")?);
        }
        if let Some(value) = source.get("FUTUREBOARD_JAM_RECONNECT") {
            config.reconnect = parse_bool(&value);
        }
        if let Some(value) = source.get("FUTUREBOARD_JAM_RECONNECT_MAX_DELAY_MS") {
            config.reconnect_max_delay = Duration::from_millis(parse_millis(
                &value,
                "FUTUREBOARD_JAM_RECONNECT_MAX_DELAY_MS",
            )?);
        }
        if let Some(value) = source.get("FUTUREBOARD_JAM_LOG_LEVEL") {
            config.log_level = value.trim().to_ascii_lowercase();
        }

        // A development token is a complete authentication bypass on a server
        // running `JAM_AUTH_MODE=dev`. `cfg!` folds to false in release, so a
        // shipped build has no path that reads one at all.
        if cfg!(debug_assertions) && config.env == JamEnv::Development {
            config.dev_token = source
                .get("FUTUREBOARD_JAM_DEV_TOKEN")
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
        }

        config.validate()?;
        Ok(config)
    }

    /// Reject a configuration that could not describe a reachable service.
    ///
    /// Plaintext is a development-only affordance and only on loopback: a
    /// production build that somehow received `http://` for the jam API would
    /// otherwise send an account bearer token in the clear.
    pub fn validate(&self) -> Result<()> {
        if self.api_url.host_str().is_none() {
            return Err(JamError::Config("the jam API url has no host".to_string()));
        }
        if self.websocket_url.host_str().is_none() {
            return Err(JamError::Config(
                "the jam signaling url has no host".to_string(),
            ));
        }
        if self.env == JamEnv::Production {
            if self.api_url.scheme() != "https" {
                return Err(JamError::Config(
                    "a production jam API url must use https".to_string(),
                ));
            }
            if self.websocket_url.scheme() != "wss" {
                return Err(JamError::Config(
                    "a production jam signaling url must use wss".to_string(),
                ));
            }
        } else {
            if self.api_url.scheme() == "http" && !is_loopback(&self.api_url) {
                return Err(JamError::Config(
                    "a plaintext jam API url is only accepted on loopback".to_string(),
                ));
            }
            if self.websocket_url.scheme() == "ws" && !is_loopback(&self.websocket_url) {
                return Err(JamError::Config(
                    "a plaintext jam signaling url is only accepted on loopback".to_string(),
                ));
            }
        }
        if self.connect_timeout.is_zero() {
            return Err(JamError::Config(
                "the jam connect timeout must be greater than zero".to_string(),
            ));
        }
        if self.reconnect && self.reconnect_max_delay.is_zero() {
            return Err(JamError::Config(
                "the jam reconnect maximum delay must be greater than zero".to_string(),
            ));
        }
        Ok(())
    }

    /// Join a path onto the API base, e.g. `v1/jams`.
    pub fn api_endpoint(&self, path: &str) -> Result<Url> {
        let base = self.api_url.as_str().trim_end_matches('/').to_string();
        Url::parse(&format!("{base}/{}", path.trim_start_matches('/')))
            .map_err(|error| JamError::Config(format!("invalid jam API path {path:?}: {error}")))
    }

    /// Whether jam diagnostics should be printed.
    pub fn verbose(&self) -> bool {
        matches!(self.log_level.as_str(), "debug" | "trace")
    }
}

/// A read-only view of configuration keys.
///
/// It exists so the loader has one code path whether the values came from the
/// process environment, from a `.env`, or from a test's map.
#[derive(Debug, Default, Clone)]
pub struct EnvSource {
    values: std::collections::HashMap<String, String>,
}

impl EnvSource {
    /// Snapshot the jam-relevant process environment.
    pub fn process() -> Self {
        let mut values = std::collections::HashMap::new();
        for key in KNOWN_KEYS {
            if let Ok(value) = std::env::var(key) {
                if !value.trim().is_empty() {
                    values.insert((*key).to_string(), value);
                }
            }
        }
        Self { values }
    }

    /// Build from explicit pairs, for tests and for a caller that keeps its own
    /// settings store.
    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self {
            values: pairs
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        }
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.values.get(key).cloned()
    }

    /// Fill in keys this source does not already have from a `.env` file.
    ///
    /// Existing values win, which is what makes an exported variable override
    /// the file rather than the other way round.
    pub fn seed_from_dotenv(&mut self, path: &Path) {
        let Ok(contents) = std::fs::read_to_string(path) else {
            return;
        };
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            if !KNOWN_KEYS.contains(&key) {
                continue;
            }
            let value = value.trim().trim_matches('"').trim_matches('\'');
            if value.is_empty() {
                continue;
            }
            self.values
                .entry(key.to_string())
                .or_insert_with(|| value.to_string());
        }
    }
}

/// Every key the loader reads. Listing them means an unrelated variable in the
/// workspace `.env` — a database url, a signing key — is never even read into
/// this process's jam configuration.
const KNOWN_KEYS: &[&str] = &[
    "FUTUREBOARD_ENV",
    "FUTUREBOARD_JAM_API_URL",
    "FUTUREBOARD_JAM_WS_URL",
    "FUTUREBOARD_JAM_WEB_URL",
    "FUTUREBOARD_JAM_REGION",
    "FUTUREBOARD_JAM_CONNECT_TIMEOUT_MS",
    "FUTUREBOARD_JAM_RECONNECT",
    "FUTUREBOARD_JAM_RECONNECT_MAX_DELAY_MS",
    "FUTUREBOARD_JAM_LOG_LEVEL",
    "FUTUREBOARD_JAM_DEV_TOKEN",
];

#[cfg(debug_assertions)]
fn workspace_dotenv_path() -> Option<PathBuf> {
    // crates/SphereJamClient -> workspace root.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    Some(manifest.join("..").join("..").join(".env"))
}

#[cfg(not(debug_assertions))]
#[allow(dead_code)]
fn workspace_dotenv_path() -> Option<PathBuf> {
    None
}

fn parse_endpoint(raw: &str, key: &str, schemes: &[&str]) -> Result<Url> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(JamError::Config(format!("{key} is empty")));
    }
    let url = Url::parse(trimmed)
        .map_err(|error| JamError::Config(format!("{key} is not a url: {error}")))?;
    if !schemes.contains(&url.scheme()) {
        return Err(JamError::Config(format!(
            "{key} uses scheme {:?}; expected one of {}",
            url.scheme(),
            schemes.join(", ")
        )));
    }
    if url.host_str().is_none() {
        return Err(JamError::Config(format!("{key} has no host")));
    }
    Ok(url)
}

/// Turn an API base into the signaling endpoint the server documents:
/// `http` becomes `ws`, `https` becomes `wss`, and the path is `/v1/realtime`.
fn derive_signaling_url(api: &Url) -> Result<Url> {
    let scheme = match api.scheme() {
        "https" => "wss",
        "http" => "ws",
        other => {
            return Err(JamError::Config(format!(
                "cannot derive a signaling url from scheme {other:?}"
            )))
        }
    };
    let host = api
        .host_str()
        .ok_or_else(|| JamError::Config("the jam API url has no host".to_string()))?;
    let authority = match api.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    };
    Url::parse(&format!("{scheme}://{authority}/v1/realtime"))
        .map_err(|error| JamError::Config(format!("derived signaling url is invalid: {error}")))
}

fn parse_millis(raw: &str, key: &str) -> Result<u64> {
    raw.trim()
        .parse::<u64>()
        .map_err(|_| JamError::Config(format!("{key} must be a whole number of milliseconds")))
}

fn parse_bool(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn is_loopback(url: &Url) -> bool {
    matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut out = vec![("FUTUREBOARD_ENV".to_string(), "development".to_string())];
        out.extend(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string())),
        );
        out
    }

    #[test]
    fn defaults_need_no_environment_and_point_at_the_public_service() {
        let config = JamConfig::from_source(&EnvSource::default()).expect("defaults are valid");
        assert_eq!(config.env, JamEnv::Production);
        assert_eq!(config.api_url.as_str(), "https://jam.futureboard.studio/");
        assert_eq!(config.websocket_url.scheme(), "wss");
        assert!(config.reconnect);
        assert!(config.dev_token.is_none());
    }

    #[test]
    fn the_environment_overrides_the_defaults() {
        let source = EnvSource::from_pairs(dev(&[
            ("FUTUREBOARD_JAM_API_URL", "http://127.0.0.1:8090"),
            ("FUTUREBOARD_JAM_WS_URL", "ws://127.0.0.1:8090/v1/realtime"),
            ("FUTUREBOARD_JAM_REGION", "th-bkk"),
            ("FUTUREBOARD_JAM_CONNECT_TIMEOUT_MS", "2500"),
            ("FUTUREBOARD_JAM_RECONNECT", "false"),
        ]));
        let config = JamConfig::from_source(&source).expect("development config is valid");
        assert_eq!(config.api_url.as_str(), "http://127.0.0.1:8090/");
        assert_eq!(config.preferred_region.wire_value(), "th-bkk-1");
        assert_eq!(config.connect_timeout, Duration::from_millis(2500));
        assert!(!config.reconnect);
    }

    #[test]
    fn the_signaling_url_is_derived_from_the_api_base_when_it_is_not_given() {
        let source =
            EnvSource::from_pairs(dev(&[("FUTUREBOARD_JAM_API_URL", "http://localhost:8090")]));
        let config = JamConfig::from_source(&source).expect("valid");
        assert_eq!(
            config.websocket_url.as_str(),
            "ws://localhost:8090/v1/realtime"
        );
    }

    #[test]
    fn a_malformed_url_is_an_explicit_error() {
        let source = EnvSource::from_pairs(dev(&[("FUTUREBOARD_JAM_API_URL", "not a url")]));
        let error = JamConfig::from_source(&source).expect_err("rejected");
        assert!(matches!(error, JamError::Config(_)));
    }

    #[test]
    fn an_unsupported_scheme_is_an_explicit_error() {
        let source =
            EnvSource::from_pairs(dev(&[("FUTUREBOARD_JAM_API_URL", "ftp://example.com")]));
        assert!(JamConfig::from_source(&source).is_err());
    }

    #[test]
    fn plaintext_is_refused_off_loopback_even_in_development() {
        let source = EnvSource::from_pairs(dev(&[(
            "FUTUREBOARD_JAM_API_URL",
            "http://jam.example.com",
        )]));
        assert!(JamConfig::from_source(&source).is_err());
    }

    #[test]
    fn production_refuses_a_plaintext_endpoint() {
        let source = EnvSource::from_pairs([
            ("FUTUREBOARD_ENV", "production"),
            ("FUTUREBOARD_JAM_API_URL", "http://127.0.0.1:8090"),
        ]);
        assert!(JamConfig::from_source(&source).is_err());
    }

    #[test]
    fn an_unknown_region_is_refused_rather_than_silently_automatic() {
        assert!(RegionPreference::parse("mars").is_err());
        assert_eq!(RegionPreference::parse("").unwrap(), RegionPreference::Auto);
        assert_eq!(
            RegionPreference::parse("AUTO").unwrap(),
            RegionPreference::Auto
        );
        assert_eq!(
            RegionPreference::parse("id-jkt").unwrap().wire_value(),
            "id-jkt-1"
        );
    }

    #[test]
    fn a_production_config_never_carries_a_development_token() {
        let source = EnvSource::from_pairs([
            ("FUTUREBOARD_ENV", "production"),
            ("FUTUREBOARD_JAM_DEV_TOKEN", "dev:hachi224"),
        ]);
        let config = JamConfig::from_source(&source).expect("valid");
        assert!(config.dev_token.is_none());
    }

    #[test]
    fn api_paths_join_without_doubling_the_separator() {
        let config = JamConfig::default();
        assert_eq!(
            config.api_endpoint("/v1/jams").unwrap().as_str(),
            "https://jam.futureboard.studio/v1/jams"
        );
        assert_eq!(
            config.api_endpoint("v1/regions").unwrap().as_str(),
            "https://jam.futureboard.studio/v1/regions"
        );
    }

    #[test]
    fn a_dotenv_seeds_only_unset_known_keys() {
        let dir = std::env::temp_dir().join("futureboard-jam-config-test");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(".env");
        std::fs::write(
            &path,
            "# comment\nFUTUREBOARD_JAM_API_URL=http://127.0.0.1:9999\n\
             FUTUREBOARD_JAM_REGION=sg-sin\nSUPABASE_SECRET_KEY=must-not-be-read\n",
        )
        .expect("write");

        let mut source =
            EnvSource::from_pairs(dev(&[("FUTUREBOARD_JAM_API_URL", "http://127.0.0.1:8090")]));
        source.seed_from_dotenv(&path);

        // The already-set key wins; the unset one is filled in; an unrelated
        // key is not even loaded.
        assert_eq!(
            source.get("FUTUREBOARD_JAM_API_URL").as_deref(),
            Some("http://127.0.0.1:8090")
        );
        assert_eq!(
            source.get("FUTUREBOARD_JAM_REGION").as_deref(),
            Some("sg-sin")
        );
        assert!(source.get("SUPABASE_SECRET_KEY").is_none());

        let _ = std::fs::remove_file(&path);
    }
}
