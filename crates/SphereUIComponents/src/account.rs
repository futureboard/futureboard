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

use std::sync::atomic::{AtomicBool, Ordering};
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
    if let Some(url) = crate::auth::current_profile().and_then(|profile| profile.avatar_url) {
        prefetch_account_avatar(&url);
    }
    set_account_provider(Arc::new(default_account_snapshot));
    set_account_action_handler(Arc::new(default_account_action));
}

fn default_account_snapshot() -> AccountSnapshot {
    match crate::auth::current_profile() {
        Some(profile) => {
            if let Some(url) = profile.avatar_url.as_deref() {
                // Covers signing in mid-session: the identity appears without a
                // restart, so the picture has to be able to follow it.
                prefetch_account_avatar(url);
            }
            AccountSnapshot {
                signed_in: true,
                username: profile.username,
                email: profile.email,
                avatar_url: profile.avatar_url,
            }
        }
        None => AccountSnapshot::default(),
    }
}

fn default_account_action(action: AccountAction, window: &mut Window, cx: &mut App) {
    let owner_bounds = Some(window.bounds());
    match action {
        AccountAction::SignIn => {
            set_account_menu_open(false);
            if let Err(error) = crate::auth_dialog::open_login_window(owner_bounds, cx) {
                eprintln!("[Account] failed to open sign-in window: {error}");
            }
        }
        AccountAction::OpenMenu => {
            let open = !account_menu_open();
            set_account_menu_open(open);
            // The chip and the menu are drawn by the shared chrome, which has no
            // entity of its own to notify.
            cx.refresh_windows();
        }
        AccountAction::SignOut => {
            crate::auth::sign_out();
            clear_account_avatar();
            set_account_menu_open(false);
            // The chip reads the snapshot during render, so the titlebar has to
            // be told the identity it drew is gone.
            cx.refresh_windows();
        }
    }
}

// ── Profile picture ──────────────────────────────────────────────────────────

/// The signed-in account's picture, as the identity provider serves it.
///
/// GPUI can render a remote URL directly, but only through the `HttpClient` on
/// the `App`, and this application never installs one — the default is
/// `NullHttpClient`, so an `img(url)` would have failed silently forever. The
/// bytes are fetched here instead, on the same blocking client the rest of the
/// account layer already uses, and handed to GPUI as an in-memory image it
/// decodes and caches like any other.
struct AvatarCache {
    /// URL the cached bytes belong to. A different URL (a different account, or
    /// a provider that rotated the picture) invalidates them.
    url: Option<String>,
    image: Option<Arc<gpui::Image>>,
}

fn avatar_slot() -> &'static RwLock<AvatarCache> {
    static SLOT: OnceLock<RwLock<AvatarCache>> = OnceLock::new();
    SLOT.get_or_init(|| {
        RwLock::new(AvatarCache {
            url: None,
            image: None,
        })
    })
}

/// Guards the single in-flight download so a repeated render cannot queue a
/// second one.
fn avatar_fetch_in_flight() -> &'static AtomicBool {
    static FLAG: OnceLock<AtomicBool> = OnceLock::new();
    FLAG.get_or_init(|| AtomicBool::new(false))
}

/// The cached picture, if one has been downloaded for the current account.
/// Never blocks and never starts work — the titlebar reads this every frame.
pub fn account_avatar_image() -> Option<Arc<gpui::Image>> {
    let cache = avatar_slot().read().ok()?;
    cache.image.clone()
}

/// Start downloading `url` unless its bytes are already cached or a download is
/// already running. Returns immediately; the picture appears on the frame after
/// it lands.
pub fn prefetch_account_avatar(url: &str) {
    if url.trim().is_empty() {
        return;
    }
    if let Ok(cache) = avatar_slot().read() {
        if cache.url.as_deref() == Some(url) {
            return;
        }
    }
    if avatar_fetch_in_flight().swap(true, Ordering::AcqRel) {
        return;
    }
    let url = url.to_string();
    // A detached thread rather than a GPUI task: this runs before (and
    // independently of) any window, and the blocking client is the one the
    // account layer already carries.
    let spawned = std::thread::Builder::new()
        .name("account-avatar".into())
        .spawn(move || {
            let fetched = download_avatar(&url);
            if let Ok(mut cache) = avatar_slot().write() {
                match fetched {
                    Some(image) => {
                        cache.url = Some(url);
                        cache.image = Some(Arc::new(image));
                    }
                    // Remember the failure against the URL too, so a provider
                    // that 404s is not re-requested on every sign-in check.
                    None => {
                        cache.url = Some(url);
                        cache.image = None;
                    }
                }
            }
            avatar_fetch_in_flight().store(false, Ordering::Release);
        });
    if spawned.is_err() {
        avatar_fetch_in_flight().store(false, Ordering::Release);
    }
}

/// Clear the cached picture. Called on sign-out so the next account cannot
/// inherit the previous one's face.
pub fn clear_account_avatar() {
    if let Ok(mut cache) = avatar_slot().write() {
        cache.url = None;
        cache.image = None;
    }
}

/// Cap on a profile picture. Providers serve small square images; anything far
/// larger is not a picture we should be decoding into the titlebar.
const AVATAR_MAX_BYTES: usize = 2 * 1024 * 1024;

fn download_avatar(url: &str) -> Option<gpui::Image> {
    // Only ever an https URL from the account service's own profile payload.
    if !url.starts_with("https://") {
        return None;
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .ok()?;
    let response = client.get(url).send().ok()?;
    if !response.status().is_success() {
        return None;
    }
    let format = avatar_format(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        url,
    )?;
    let bytes = response.bytes().ok()?;
    if bytes.is_empty() || bytes.len() > AVATAR_MAX_BYTES {
        return None;
    }
    Some(gpui::Image::from_bytes(format, bytes.to_vec()))
}

/// Resolve the encoding from the response's content type, falling back to the
/// URL's extension. An unrecognised type is refused rather than guessed: GPUI
/// decodes by the format we declare, and a wrong guess is a decode failure with
/// no diagnosis attached.
fn avatar_format(content_type: Option<&str>, url: &str) -> Option<gpui::ImageFormat> {
    let by_mime = content_type
        .map(|value| value.split(';').next().unwrap_or(value).trim().to_owned())
        .and_then(|mime| match mime.as_str() {
            "image/png" => Some(gpui::ImageFormat::Png),
            "image/jpeg" | "image/jpg" => Some(gpui::ImageFormat::Jpeg),
            "image/webp" => Some(gpui::ImageFormat::Webp),
            "image/gif" => Some(gpui::ImageFormat::Gif),
            _ => None,
        });
    if by_mime.is_some() {
        return by_mime;
    }
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    if path.ends_with(".png") {
        Some(gpui::ImageFormat::Png)
    } else if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        Some(gpui::ImageFormat::Jpeg)
    } else if path.ends_with(".webp") {
        Some(gpui::ImageFormat::Webp)
    } else if path.ends_with(".gif") {
        Some(gpui::ImageFormat::Gif)
    } else {
        None
    }
}

#[cfg(test)]
mod avatar_tests {
    use super::avatar_format;

    /// Google and Discord serve `image/jpeg` and `image/png` with no extension
    /// on the path, so the content type has to lead.
    #[test]
    fn content_type_decides_the_format() {
        assert_eq!(
            avatar_format(Some("image/png"), "https://cdn.example/a"),
            Some(gpui::ImageFormat::Png)
        );
        assert_eq!(
            avatar_format(Some("image/jpeg; charset=binary"), "https://cdn.example/a"),
            Some(gpui::ImageFormat::Jpeg)
        );
    }

    /// GitHub serves avatars with a query string; the extension check must not
    /// be fooled by it.
    #[test]
    fn extension_is_the_fallback_and_ignores_the_query() {
        assert_eq!(
            avatar_format(None, "https://cdn.example/a.PNG?size=64"),
            Some(gpui::ImageFormat::Png)
        );
    }

    /// An unknown type is refused, not guessed: declaring the wrong format to
    /// GPUI produces a decode failure with nothing to diagnose it by.
    #[test]
    fn unknown_encodings_are_refused() {
        assert_eq!(
            avatar_format(Some("text/html"), "https://cdn.example/a"),
            None
        );
        assert_eq!(avatar_format(None, "https://cdn.example/a"), None);
    }
}

// ── Account menu ─────────────────────────────────────────────────────────────

/// Whether the titlebar's account menu is showing.
///
/// The menu used to be a real `WindowKind::PopUp` window positioned against the
/// owner's screen rectangle, which is why it floated outside the application —
/// it *was* outside it, and it kept its own copy of the profile (including a
/// second avatar that never learned about the downloaded picture). It is a
/// dropdown, so it belongs inside the window that owns the chip.
///
/// Process-global for the same reason the provider slots are: there is one
/// signed-in account, one chip showing it, and one menu for it. The chrome that
/// draws both is a plain function with nowhere to keep state.
fn account_menu_flag() -> &'static AtomicBool {
    static FLAG: OnceLock<AtomicBool> = OnceLock::new();
    FLAG.get_or_init(|| AtomicBool::new(false))
}

/// Whether the account menu should be drawn this frame.
pub fn account_menu_open() -> bool {
    account_menu_flag().load(Ordering::Relaxed)
}

/// Open or dismiss the account menu. The caller repaints.
pub fn set_account_menu_open(open: bool) {
    account_menu_flag().store(open, Ordering::Relaxed);
}
