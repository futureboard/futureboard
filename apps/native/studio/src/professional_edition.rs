//! Bridge to the ignored Exclusive Edition implementation.
//!
//! This tracked module deliberately uses `include!` instead of a `#[path]`
//! module. Rustfmt resolves `#[path]` modules even when their feature is
//! disabled, which made Community Edition checks require the private source
//! tree. The include macros are expanded only when this feature-gated module is
//! compiled by an Exclusive Edition build.
//!
//! Compiling with `--features exclusive` grants nothing on its own. Providers
//! below install only after a signed license token verifies for this machine,
//! so an Exclusive build on an unlicensed machine behaves like Community.

mod license {
    include!(concat!(
        env!("OUT_DIR"),
        "/futureboard-exclusive/license.rs"
    ));
}

mod license_activation_dialog {
    include!(concat!(
        env!("OUT_DIR"),
        "/futureboard-exclusive/license_activation_dialog.rs"
    ));
}

mod auth {
    include!(concat!(env!("OUT_DIR"), "/futureboard-exclusive/auth.rs"));
}

mod auth_dialog {
    include!(concat!(
        env!("OUT_DIR"),
        "/futureboard-exclusive/auth_dialog.rs"
    ));
}

mod eula {
    include!(concat!(env!("OUT_DIR"), "/futureboard-exclusive/eula.rs"));
}

mod eula_dialog {
    include!(concat!(
        env!("OUT_DIR"),
        "/futureboard-exclusive/eula_dialog.rs"
    ));
}

#[cfg(target_os = "windows")]
mod asio {
    include!(concat!(env!("OUT_DIR"), "/futureboard-exclusive/asio.rs"));
}

pub use license_activation_dialog::{configured_license_activator, open_license_activation_window};

use sphere_ui_components::account::{AccountAction, AccountSnapshot};

#[cfg(target_os = "windows")]
use std::sync::atomic::{AtomicBool, Ordering};

/// Cached result of the existing signed-token verification path. The engine's
/// provider callback is only an atomic load; it never verifies a token or does
/// filesystem/network work on an audio-related path.
#[cfg(target_os = "windows")]
static ASIO_ENTITLEMENT: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "windows")]
fn current_asio_entitlement() -> bool {
    ASIO_ENTITLEMENT.load(Ordering::Acquire)
}

/// Show the first-run EULA dialog when the current agreement version has not yet
/// been accepted. Called once the first app surface is up, so it appears as a
/// modal on top. Declining (or closing) the dialog quits the app.
pub fn show_eula_if_needed(cx: &mut gpui::App) {
    if eula::needs_acceptance() {
        if let Err(error) = eula_dialog::open_eula_window(cx) {
            eprintln!("[EULA] failed to open dialog: {error}");
        }
    }
}

/// Install the Exclusive Edition runtime providers that a verified license
/// grants. Safe to call more than once: provider registration is process-wide,
/// while the cached live entitlement is refreshed on every call so activation,
/// renewal, expiry, or entitlement changes take effect without a restart.
///
/// An unlicensed machine is not an error. Its cached capability is false, so
/// the audio backend list and all future ASIO host acquisitions stay disabled.
pub fn install_licensed_providers() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let asio_entitled = license::active_license()
            .is_some_and(|license| license.grants(license::ENTITLEMENT_ASIO));
        ASIO_ENTITLEMENT.store(asio_entitled, Ordering::Release);

        if !asio_entitled || DirectAudio::asio_support_enabled() {
            return Ok(());
        }

        DirectAudio::backend::register_asio_host_provider(
            DirectAudio::backend::AsioHostProvider::new(asio::host, current_asio_entitlement),
        )?;
    }

    Ok(())
}

/// Install Exclusive Edition runtime providers before the application starts.
///
/// Providers install from the stored token with no network involved, so this
/// stays off the critical path. Renewal is kicked onto a background thread: a
/// slow or unreachable licensing service must never delay the DAW opening.
///
/// The `spawn_renewal_if_due` call is also what re-pulls a lapsed-but-bound
/// license on a fresh launch: an expired token still triggers a background
/// re-check with the service, and installs providers again on success.
pub fn install() -> Result<(), String> {
    sphere_ui_components::edition::set_edition_provider(std::sync::Arc::new(edition_info));

    // Account/auth is available only on a Supabase-configured build. When it is,
    // load any stored session (refreshing it in the background) and register the
    // titlebar account provider + action handler.
    if auth::auth_configured() {
        auth::init_session();
        sphere_ui_components::account::set_account_provider(std::sync::Arc::new(account_snapshot));
        sphere_ui_components::account::set_account_action_handler(std::sync::Arc::new(
            handle_account_action,
        ));
    }

    install_licensed_providers()?;
    license::spawn_renewal_if_due();
    Ok(())
}

/// Snapshot of the signed-in user for the titlebar chip. Reads the in-memory
/// session each call, so sign-in / sign-out reflect without extra wiring.
fn account_snapshot() -> AccountSnapshot {
    match auth::current_profile() {
        Some(profile) => AccountSnapshot {
            signed_in: true,
            username: profile.username,
            email: profile.email,
            avatar_url: profile.avatar_url,
        },
        None => AccountSnapshot::default(),
    }
}

/// Route a titlebar account action to the sign-in dialog / account menu / sign
/// out. Opening windows and refreshing chrome both need the live `App`.
fn handle_account_action(action: AccountAction, window: &mut gpui::Window, cx: &mut gpui::App) {
    let owner = Some(window.bounds());
    match action {
        AccountAction::SignIn => {
            if let Err(error) = auth_dialog::open_login_window(owner, cx) {
                eprintln!("[Auth] failed to open sign-in dialog: {error}");
            }
        }
        AccountAction::OpenMenu => {
            if let Err(error) = auth_dialog::open_account_menu_window(owner, cx) {
                eprintln!("[Auth] failed to open account menu: {error}");
            }
        }
        AccountAction::SignOut => {
            auth::sign_out();
            cx.refresh_windows();
        }
    }
}

/// Build the edition/license snapshot the shared About panel renders. Re-reads
/// and re-verifies the stored token on each call, so it always reflects current
/// state (post-activation, post-renewal) with no explicit refresh wiring.
fn edition_info() -> sphere_ui_components::edition::EditionInfo {
    use sphere_ui_components::edition::{EditionInfo, LicenseDisplay, LicenseDisplayState};

    let license = license::stored_license_status().map(|status| LicenseDisplay {
        state: match status.state {
            license::LicenseState::Active => LicenseDisplayState::Active,
            license::LicenseState::Expired => LicenseDisplayState::Expired,
        },
        licensee: status.licensee,
        entitlements: status.entitlements,
        expires_at: status.expires_at,
    });

    EditionInfo {
        edition: "Exclusive",
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        license,
    }
}

#[cfg(test)]
mod tests {
    use super::license;

    /// ASIO registration must track the verified license exactly — never the
    /// `exclusive` feature, which only decides whether the code is compiled in.
    ///
    /// Asserting both directions against the same machine keeps this honest on
    /// a developer box either way: unlicensed, ASIO must stay off; licensed, it
    /// must come on.
    #[test]
    fn asio_registration_tracks_the_verified_license() {
        let licensed = license::active_license()
            .is_some_and(|license| license.grants(license::ENTITLEMENT_ASIO));
        eprintln!("[license-e2e] machine holds an ASIO entitlement: {licensed}");

        super::install_licensed_providers().expect("installing must not fail");

        assert_eq!(
            DirectAudio::asio_support_enabled(),
            licensed,
            "an ASIO host must be registered if and only if a verified license grants it"
        );
        assert_eq!(
            DirectAudio::backend::BackendKind::allowed_for_current_platform()
                .contains(&DirectAudio::backend::BackendKind::Asio),
            licensed,
            "the backend list must offer ASIO if and only if a verified license grants it"
        );
    }
}
