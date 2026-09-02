//! Process-global account state surfaced to shared UI (the titlebar user chip
//! and its dropdown).
//!
//! Like [`crate::edition`], this is a hand-off point: a **snapshot provider**
//! (for display) and an **action handler** (to open the sign-in dialog / sign
//! out). The shared titlebar reads the snapshot each render and routes clicks
//! back through the handler.
//!
//! [`install_default_account_provider`] wires both slots to this crate's own
//! [`crate::auth`] / [`crate::auth_dialog`], so a Community build signs in like
//! any other. The slots stay overridable: a build that wants a different account
//! layer can install its own instead.
//!
//! An account is not an entitlement. What a license grants is a separate
//! provider ([`crate::edition`]) and stays behind the Professional Edition's
//! verified license token.

use std::sync::{Arc, OnceLock, RwLock};

use gpui::{App, Window};

/// The signed-in user as the titlebar needs it. No tokens — display only.
#[derive(Debug, Clone, Default)]
pub struct AccountSnapshot {
    pub signed_in: bool,
    pub username: Option<String>,
    pub email: Option<String>,
    /// Remote profile-picture URL, if the provider supplied one.
    pub avatar_url: Option<String>,
}

/// What the titlebar chip / dropdown can ask the account layer to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountAction {
    /// Open the sign-in dialog (chip dispatches this when signed out).
    SignIn,
    /// Open the account menu / dropdown (chip dispatches this when signed in).
    OpenMenu,
    /// Sign the current user out.
    SignOut,
}

type AccountProvider = Arc<dyn Fn() -> AccountSnapshot + Send + Sync + 'static>;
type AccountActionHandler =
    Arc<dyn Fn(AccountAction, &mut Window, &mut App) + Send + Sync + 'static>;

fn provider_slot() -> &'static RwLock<Option<AccountProvider>> {
    static SLOT: OnceLock<RwLock<Option<AccountProvider>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

fn handler_slot() -> &'static RwLock<Option<AccountActionHandler>> {
    static SLOT: OnceLock<RwLock<Option<AccountActionHandler>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

/// Install the snapshot provider (Exclusive build, once at startup).
pub fn set_account_provider(provider: AccountProvider) {
    if let Ok(mut guard) = provider_slot().write() {
        *guard = Some(provider);
    }
}

/// Install the action handler (Exclusive build, once at startup).
pub fn set_account_action_handler(handler: AccountActionHandler) {
    if let Ok(mut guard) = handler_slot().write() {
        *guard = Some(handler);
    }
}

/// Current account snapshot, or `None` when no provider is installed (Community).
pub fn current_account() -> Option<AccountSnapshot> {
    let provider = {
        let guard = provider_slot().read().ok()?;
        guard.as_ref()?.clone()
    };
    Some(provider())
}

/// Route a titlebar action to the installed handler. No-op when unhandled, so a
/// Community build (or a race at startup) simply does nothing.
pub fn dispatch_account_action(action: AccountAction, window: &mut Window, cx: &mut App) {
    let handler = {
        let Ok(guard) = handler_slot().read() else {
            return;
        };
        guard.as_ref().cloned()
    };
    if let Some(handler) = handler {
        handler(action, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The provider slot is process-global and installed once, so these two
    /// tests cannot run concurrently: reading "is the slot empty?" and then
    /// asserting on it is a check-then-act that the installing test can land
    /// between. Holding this for the whole body makes the pair atomic; the
    /// empty-slot test still skips itself if the installer got there first,
    /// which is the only ordering the slot allows.
    static SLOT: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        SLOT.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn no_provider_reports_no_account() {
        let _guard = lock();
        if provider_slot().read().map(|g| g.is_some()).unwrap_or(false) {
            return;
        }
        assert!(current_account().is_none());
    }

    #[test]
    fn an_installed_provider_is_read_back() {
        let _guard = lock();
        set_account_provider(Arc::new(|| AccountSnapshot {
            signed_in: true,
            username: Some("Jane".to_string()),
            email: Some("jane@example.com".to_string()),
            avatar_url: None,
        }));
        let snapshot = current_account().expect("provider installed");
        assert!(snapshot.signed_in);
        assert_eq!(snapshot.username.as_deref(), Some("Jane"));
    }
}

/// Wire the titlebar chip to this crate's own account layer.
///
/// Loads any stored session (refreshing it in the background) and installs the
/// snapshot provider + action handler. A build with no account endpoint baked in
/// installs nothing, so the chip stays absent rather than offering a sign-in
/// that cannot work.
pub fn install_default_account_provider() {
    if !crate::auth::auth_configured() {
        return;
    }
    crate::auth::init_session();
    set_account_provider(Arc::new(default_account_snapshot));
    set_account_action_handler(Arc::new(default_account_action));
}

fn default_account_snapshot() -> AccountSnapshot {
    match crate::auth::current_profile() {
        Some(profile) => AccountSnapshot {
            signed_in: true,
            username: profile.username,
            email: profile.email,
            avatar_url: profile.avatar_url,
        },
        None => AccountSnapshot::default(),
    }
}

fn default_account_action(action: AccountAction, window: &mut Window, cx: &mut App) {
    let owner_bounds = Some(window.bounds());
    match action {
        AccountAction::SignIn => {
            if let Err(error) = crate::auth_dialog::open_login_window(owner_bounds, cx) {
                eprintln!("[Account] failed to open sign-in window: {error}");
            }
        }
        AccountAction::OpenMenu => {
            if let Err(error) = crate::auth_dialog::open_account_menu_window(owner_bounds, cx) {
                eprintln!("[Account] failed to open account menu: {error}");
            }
        }
        AccountAction::SignOut => {
            crate::auth::sign_out();
            // The chip reads the snapshot during render, so the titlebar has to
            // be told the identity it drew is gone.
            cx.refresh_windows();
        }
    }
}
