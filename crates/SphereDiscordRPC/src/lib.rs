//! Non-blocking Discord Rich Presence integration for Futureboard Studio.
//!
//! Discord IPC is owned by a background worker. UI callers only enqueue small
//! presence updates and never wait for Discord to be installed or running.

use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use discord_rich_presence::{DiscordIpc, DiscordIpcClient, activity};
use thiserror::Error;

const RETRY_INTERVAL: Duration = Duration::from_secs(15);
const DEFAULT_LARGE_TEXT: &str = "Futureboard Studio";

/// Futureboard's own Discord application ID.
///
/// Baked in rather than sourced from a file or environment variable so a plain
/// checkout builds with Rich Presence working. Previously it came only from
/// `.discordrpcsecret` or `FUTUREBOARD_DISCORD_CLIENT_ID`, and a build without
/// either silently shipped with Discord integration switched off.
///
/// This is not a credential. A Discord application ID is a public identifier —
/// it is sent in the RPC handshake and is visible to anyone running the app.
/// The client *secret* and bot tokens are the sensitive values, and neither is
/// used here: Rich Presence over local IPC needs no authentication.
pub const DEFAULT_APPLICATION_ID: &str = "1517963140170649640";

/// Runtime configuration for the Discord IPC worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordRpcConfig {
    pub application_id: String,
    pub app_version: String,
    pub large_image: Option<String>,
    pub large_text: String,
    pub show_project_name: bool,
}

impl DiscordRpcConfig {
    /// Builds configuration from the Futureboard Discord environment flags,
    /// falling back to [`DEFAULT_APPLICATION_ID`].
    ///
    /// Resolution order, most specific first: a runtime
    /// `FUTUREBOARD_DISCORD_CLIENT_ID`, the same name baked in at build time,
    /// then the built-in default. The overrides exist so a fork or a CI build
    /// can point at its own Discord application; nothing has to be configured
    /// for the shipped one to work.
    pub fn from_env(app_version: impl Into<String>) -> Option<Self> {
        Self::from_application_id(resolve_application_id(), app_version)
    }

    /// Builds configuration around an application ID supplied by the native
    /// executable (for example, one embedded by its build script).
    pub fn from_application_id(
        application_id: impl Into<String>,
        app_version: impl Into<String>,
    ) -> Option<Self> {
        let application_id = non_empty(application_id.into().as_str())?;
        let large_image = non_empty_env("FUTUREBOARD_DISCORD_LARGE_IMAGE")
            .or_else(|| option_env!("FUTUREBOARD_DISCORD_LARGE_IMAGE").and_then(non_empty));
        let large_text = non_empty_env("FUTUREBOARD_DISCORD_LARGE_TEXT")
            .or_else(|| option_env!("FUTUREBOARD_DISCORD_LARGE_TEXT").and_then(non_empty))
            .unwrap_or_else(|| DEFAULT_LARGE_TEXT.to_string());

        Some(Self {
            application_id,
            app_version: app_version.into(),
            large_image,
            large_text,
            show_project_name: env_flag("FUTUREBOARD_DISCORD_SHOW_PROJECT_NAME"),
        })
    }

    fn validate(&self) -> Result<(), DiscordRpcStartError> {
        if self.application_id.trim().is_empty() {
            return Err(DiscordRpcStartError::MissingApplicationId);
        }
        Ok(())
    }
}

/// High-level states exposed by the native shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presence {
    Welcome,
    Loading,
    Editing { project_name: String },
}

impl Presence {
    pub fn editing(project_name: impl Into<String>) -> Self {
        Self::Editing {
            project_name: project_name.into(),
        }
    }
}

/// A cheap, cloneable command handle used by the UI thread.
#[derive(Clone)]
pub struct DiscordRpcHandle {
    commands: Sender<Command>,
}

impl DiscordRpcHandle {
    /// Queues an application presence without touching Discord IPC on the
    /// calling thread. A closed worker is treated as a no-op.
    pub fn set_presence(&self, presence: Presence) {
        let _ = self.commands.send(Command::Set(presence));
    }

    /// Requests activity removal. This does not stop the retry worker.
    pub fn clear(&self) {
        let _ = self.commands.send(Command::Clear);
    }

    /// Enables or disables Rich Presence without blocking the caller. When
    /// re-enabled, the worker publishes the most recently supplied presence.
    pub fn set_enabled(&self, enabled: bool) {
        let _ = self.commands.send(Command::SetEnabled(enabled));
    }

    /// Requests worker shutdown. Normally [`DiscordRpc::shutdown`] owns this.
    pub fn request_shutdown(&self) {
        let _ = self.commands.send(Command::Shutdown);
    }
}

/// Owns the Discord IPC worker for the lifetime of the application.
pub struct DiscordRpc {
    handle: DiscordRpcHandle,
    worker: Option<JoinHandle<()>>,
}

impl DiscordRpc {
    pub fn start(
        config: DiscordRpcConfig,
        initial_presence: Presence,
        enabled: bool,
    ) -> Result<Self, DiscordRpcStartError> {
        config.validate()?;
        let (commands, receiver) = mpsc::channel();
        let handle = DiscordRpcHandle { commands };
        let worker = thread::Builder::new()
            .name("futureboard-discord-rpc".to_string())
            .spawn(move || run_worker(config, initial_presence, enabled, receiver))
            .map_err(DiscordRpcStartError::SpawnWorker)?;

        Ok(Self {
            handle,
            worker: Some(worker),
        })
    }

    pub fn handle(&self) -> DiscordRpcHandle {
        self.handle.clone()
    }

    /// Clears Rich Presence and joins the IPC worker.
    pub fn shutdown(mut self) {
        self.stop_worker();
    }

    fn stop_worker(&mut self) {
        self.handle.request_shutdown();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for DiscordRpc {
    fn drop(&mut self) {
        self.stop_worker();
    }
}

#[derive(Debug, Error)]
pub enum DiscordRpcStartError {
    #[error("Discord application ID is empty")]
    MissingApplicationId,
    #[error("failed to spawn Discord RPC worker: {0}")]
    SpawnWorker(std::io::Error),
}

enum Command {
    Set(Presence),
    Clear,
    SetEnabled(bool),
    Shutdown,
}

fn run_worker(
    config: DiscordRpcConfig,
    initial_presence: Presence,
    mut enabled: bool,
    commands: mpsc::Receiver<Command>,
) {
    let started_at_ms = unix_time_ms();
    let mut current_presence = Some(initial_presence);
    let mut client: Option<DiscordIpcClient> = None;

    loop {
        if enabled && client.is_none() {
            client = connect_and_publish(&config, current_presence.as_ref(), started_at_ms);
        }

        match commands.recv_timeout(RETRY_INTERVAL) {
            Ok(Command::Set(presence)) => {
                current_presence = Some(presence);
                if enabled {
                    publish_or_disconnect(
                        &config,
                        &mut client,
                        current_presence.as_ref(),
                        started_at_ms,
                    );
                }
            }
            Ok(Command::Clear) => {
                current_presence = None;
                if let Some(active) = client.as_mut() {
                    if let Err(error) = active.clear_activity() {
                        debug_log(&format!("clear failed: {error}"));
                        client = None;
                    }
                }
            }
            Ok(Command::SetEnabled(next)) => {
                if next != enabled {
                    enabled = next;
                    if enabled {
                        client =
                            connect_and_publish(&config, current_presence.as_ref(), started_at_ms);
                    } else if let Some(mut active) = client.take() {
                        let _ = active.clear_activity();
                        let _ = active.close();
                    }
                }
            }
            Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                if let Some(mut active) = client {
                    let _ = active.clear_activity();
                    let _ = active.close();
                }
                return;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

fn connect_and_publish(
    config: &DiscordRpcConfig,
    presence: Option<&Presence>,
    started_at_ms: i64,
) -> Option<DiscordIpcClient> {
    let mut client = DiscordIpcClient::new(&config.application_id);
    if let Err(error) = client.connect() {
        debug_log(&format!("Discord unavailable: {error}"));
        return None;
    }
    debug_log("connected");

    if let Some(presence) = presence {
        if let Err(error) = client.set_activity(build_activity(config, presence, started_at_ms)) {
            debug_log(&format!("initial publish failed: {error}"));
            return None;
        }
    }
    Some(client)
}

fn publish_or_disconnect(
    config: &DiscordRpcConfig,
    client: &mut Option<DiscordIpcClient>,
    presence: Option<&Presence>,
    started_at_ms: i64,
) {
    let Some(active) = client.as_mut() else {
        return;
    };
    let Some(presence) = presence else {
        return;
    };
    if let Err(error) = active.set_activity(build_activity(config, presence, started_at_ms)) {
        debug_log(&format!("publish failed: {error}"));
        *client = None;
    }
}

fn build_activity<'a>(
    config: &'a DiscordRpcConfig,
    presence: &'a Presence,
    started_at_ms: i64,
) -> activity::Activity<'a> {
    let (details, state) = match presence {
        Presence::Welcome => ("Getting ready to create", "Welcome"),
        Presence::Loading => ("Opening a project", "Loading"),
        Presence::Editing { project_name } if config.show_project_name => {
            ("Creating music", project_name.as_str())
        }
        Presence::Editing { .. } => ("Creating music", "Editing a project"),
    };

    let mut result = activity::Activity::new()
        .details(details)
        .state(state)
        .timestamps(activity::Timestamps::new().start(started_at_ms));

    if let Some(large_image) = config.large_image.as_deref() {
        result = result.assets(
            activity::Assets::new()
                .large_image(large_image)
                .large_text(format!("{} {}", config.large_text, config.app_version)),
        );
    }
    result
}

fn unix_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

/// The Discord application ID this build should use.
///
/// Always yields something, so Rich Presence no longer depends on the build
/// host having a `.discordrpcsecret` or an exported environment variable.
pub fn resolve_application_id() -> String {
    non_empty_env("FUTUREBOARD_DISCORD_CLIENT_ID")
        .or_else(|| option_env!("FUTUREBOARD_DISCORD_CLIENT_ID").and_then(non_empty))
        .unwrap_or_else(|| DEFAULT_APPLICATION_ID.to_string())
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().and_then(|value| non_empty(&value))
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes") | Ok("YES")
    )
}

fn debug_log(message: &str) {
    if env_flag("FUTUREBOARD_DISCORD_RPC_DEBUG") {
        eprintln!("[DiscordRPC] {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rich Presence has to work from a plain checkout. This used to return
    /// `None` unless the build host had a `.discordrpcsecret` or an exported
    /// `FUTUREBOARD_DISCORD_CLIENT_ID`, which silently shipped the app with
    /// Discord integration switched off.
    #[test]
    fn config_resolves_without_any_local_configuration() {
        // Not `set_var`: the resolver reads the process environment, and tests
        // share it. This asserts the fallback, which is what has no override.
        let config =
            DiscordRpcConfig::from_env("2026.7.2").expect("an application id must always resolve");
        assert!(!config.application_id.trim().is_empty());
        config
            .validate()
            .expect("the resolved config must be startable");
    }

    /// The baked-in id has to be a plausible Discord snowflake — a typo here
    /// would fail only at runtime, as a handshake Discord silently ignores.
    #[test]
    fn default_application_id_is_a_snowflake() {
        assert!(
            DEFAULT_APPLICATION_ID.len() >= 17 && DEFAULT_APPLICATION_ID.len() <= 20,
            "unexpected length: {DEFAULT_APPLICATION_ID}"
        );
        assert!(
            DEFAULT_APPLICATION_ID
                .chars()
                .all(|character| character.is_ascii_digit()),
            "must be all digits: {DEFAULT_APPLICATION_ID}"
        );
    }

    #[test]
    fn an_empty_application_id_is_still_rejected() {
        let config = DiscordRpcConfig {
            application_id: "  ".to_string(),
            ..config(false)
        };
        assert!(matches!(
            config.validate(),
            Err(DiscordRpcStartError::MissingApplicationId)
        ));
    }

    fn config(show_project_name: bool) -> DiscordRpcConfig {
        DiscordRpcConfig {
            application_id: "123".to_string(),
            app_version: "2026.7.2".to_string(),
            large_image: None,
            large_text: DEFAULT_LARGE_TEXT.to_string(),
            show_project_name,
        }
    }

    #[test]
    fn empty_application_id_is_rejected() {
        let mut config = config(false);
        config.application_id.clear();
        assert!(matches!(
            config.validate(),
            Err(DiscordRpcStartError::MissingApplicationId)
        ));
    }

    #[test]
    fn explicit_application_id_is_trimmed() {
        let config = DiscordRpcConfig::from_application_id(" 123 ", "2026.7.2")
            .expect("application id should be accepted");
        assert_eq!(config.application_id, "123");
    }

    #[test]
    fn project_name_is_private_by_default() {
        let config = config(false);
        let presence = Presence::editing("Secret Album");
        let activity = build_activity(&config, &presence, 123);
        let json = serde_json::to_value(activity).expect("activity should serialize");
        assert_eq!(json["state"], "Editing a project");
        assert!(!json.to_string().contains("Secret Album"));
    }

    #[test]
    fn project_name_can_be_enabled() {
        let config = config(true);
        let presence = Presence::editing("My Song");
        let activity = build_activity(&config, &presence, 123);
        let json = serde_json::to_value(activity).expect("activity should serialize");
        assert_eq!(json["state"], "My Song");
    }
}
