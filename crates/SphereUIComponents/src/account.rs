//! Process-global account state surfaced to shared UI (the titlebar user chip
//! and its dropdown).
//!
//! Like [`crate::edition`], this is a dependency-free hand-off point: the shared
//! crate never depends on the private Professional Edition crate. The Exclusive
//! build installs a **snapshot provider** (for display) and an **action
//! handler** (to open the sign-in dialog / sign out) here; the shared titlebar
//! reads the snapshot each render and routes clicks back through the handler.
//!
//! Both slots are empty in a Community build, so the titlebar shows no account
//! chip at all there.

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
