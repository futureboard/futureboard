//! Feature-gated native-window CEF runtime.

use std::marker::PhantomData;
use std::path::PathBuf;
use std::rc::Rc;
use std::thread::{self, ThreadId};

use cef::rc::Rc as _;
use cef::{ImplBrowser, ImplBrowserHost, ImplFrame};
use thiserror::Error;

pub use cef;

/// Opaque ARGB background for windowless browsers (`#111318`, the panel
/// surface the editor chrome uses) so an unpainted frame is never transparent.
const OPAQUE_BACKGROUND: u32 = 0xFF11_1318;

/// Windowless paint rate cap. CEF clamps this to 1..=60.
const WINDOWLESS_FRAME_RATE: i32 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessDispatch {
    BrowserProcess,
    SubprocessExit(i32),
}

/// Load platform runtime state and bind the CEF API version for this process.
///
/// macOS does not link the Chromium framework into the executable. The
/// framework must be loaded from the application bundle before even the first
/// generated CEF wrapper function is called; otherwise that wrapper dispatches
/// through an unresolved null function pointer. The process-wide loader is
/// intentionally retained for the lifetime of the process.
pub fn prepare_process() -> Result<(), CefRuntimeError> {
    #[cfg(target_os = "macos")]
    load_macos_framework()?;
    ensure_api_version();
    Ok(())
}

/// Bind the CEF API version after the platform runtime has been loaded.
///
/// cef-rs stamps a version into every wrapper object it creates. Until this has
/// run that version is `-1`, and the first C→C++ call aborts the process with
/// `CefApp_0_CToCpp called with invalid version -1`. It therefore has to happen
/// before **any** CEF object exists — including the [`cef::App`] that
/// [`execute_subprocess`] and [`CefRuntime::initialize`] are handed, which is
/// constructed by the caller long before either runs.
///
/// Idempotent. Call [`prepare_process`] at public process entry points so the
/// macOS framework has been loaded before this function invokes CEF.
pub fn ensure_api_version() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let hash = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
        eprintln!(
            "[cef-process] api_version={} api_version_last={} api_hash_available={}",
            cef::api_version(),
            cef::sys::CEF_API_VERSION_LAST,
            !hash.is_null()
        );
    });
}

/// Log process identity before any Futureboard subsystem is initialized.
///
/// CEF appends `--type` and, for the Network Service, `--utility-sub-type` to
/// the same executable. Keeping this at the first statement of `main` makes it
/// unambiguous whether a helper escaped into normal application startup.
pub fn log_process_entry() {
    let args: Vec<String> = std::env::args_os()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let process_type = command_line_switch(&args, "--type").unwrap_or("<browser>");
    let utility_sub_type = command_line_switch(&args, "--utility-sub-type").unwrap_or("<none>");
    eprintln!(
        "[cef-process] entry pid={} command_line={args:?} type={process_type:?} utility_sub_type={utility_sub_type:?}",
        std::process::id()
    );
}

fn command_line_switch<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().enumerate().find_map(|(index, arg)| {
        arg.strip_prefix(&format!("{name}=")).or_else(|| {
            (arg == name)
                .then(|| args.get(index + 1).map(String::as_str))
                .flatten()
        })
    })
}

/// Dispatch CEF subprocess command lines before starting the native UI.
pub fn execute_subprocess(application: Option<&mut cef::App>) -> ProcessDispatch {
    prepare_process().expect("failed to prepare the CEF process");
    let args = cef::args::Args::new();
    let exit_code =
        cef::execute_process(Some(args.as_main_args()), application, std::ptr::null_mut());
    eprintln!(
        "[cef-process] cef_execute_process_return={} pid={} thread={:?}",
        exit_code,
        std::process::id(),
        std::thread::current().id()
    );
    if exit_code < 0 {
        ProcessDispatch::BrowserProcess
    } else {
        ProcessDispatch::SubprocessExit(exit_code)
    }
}

/// Browser-process configuration. The runtime uses a portable, integrated
/// message pump and must be driven from the creating UI thread.
#[derive(Debug, Clone, Default)]
pub struct CefRuntimeConfig {
    pub cache_path: Option<PathBuf>,
    pub root_cache_path: Option<PathBuf>,
    pub locale: Option<String>,
    pub user_agent: Option<String>,
    pub remote_debugging_port: Option<u16>,
    /// Allow [`RenderMode::Windowless`] browsers in this process. Chromium
    /// decides this once, at `cef_initialize`, so a runtime that may ever need
    /// off-screen rendering must opt in before any browser is created.
    pub windowless_rendering: bool,
}

/// Pixel bounds in the native parent's client coordinate space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowBounds {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl WindowBounds {
    pub fn new(x: i32, y: i32, width: i32, height: i32) -> Result<Self, CefRuntimeError> {
        if width <= 0 || height <= 0 {
            return Err(CefRuntimeError::InvalidBounds { width, height });
        }
        Ok(Self {
            x,
            y,
            width,
            height,
        })
    }

    fn as_cef_rect(self) -> cef::Rect {
        cef::Rect {
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
        }
    }
}

/// A native parent HWND (Windows), X11 Window (Linux), or NSView pointer
/// (macOS).
#[derive(Clone, Copy)]
pub struct NativeParent(cef::sys::cef_window_handle_t);

impl NativeParent {
    /// # Safety
    ///
    /// The handle must stay valid for all child [`WebView`] instances and must
    /// belong to the thread that owns [`CefRuntime`].
    pub unsafe fn from_raw(handle: cef::sys::cef_window_handle_t) -> Self {
        Self(handle)
    }

    pub fn as_raw(self) -> cef::sys::cef_window_handle_t {
        self.0
    }
}

/// How a browser is presented: a real native child window, or an off-screen
/// framebuffer the embedder draws itself.
#[derive(Clone)]
pub enum RenderMode {
    /// A native CEF child window inside `parent`. No render handler, shared
    /// texture, or off-screen rendering path is used.
    Windowed,
    /// Windowless rendering into `surface`. `parent` is still passed to CEF —
    /// it is only used to resolve monitor info and to parent dialogs — and may
    /// be null on platforms where no such handle exists.
    Windowless { surface: crate::osr::OsrSurface },
}

impl std::fmt::Debug for RenderMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Windowed => write!(f, "Windowed"),
            Self::Windowless { .. } => write!(f, "Windowless"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WebViewConfig {
    pub url: String,
    pub bounds: WindowBounds,
    pub render_mode: RenderMode,
}

impl WebViewConfig {
    pub fn new(url: impl Into<String>, bounds: WindowBounds) -> Self {
        Self {
            url: url.into(),
            bounds,
            render_mode: RenderMode::Windowed,
        }
    }

    /// Render off-screen into `surface` instead of a native child window.
    pub fn windowless(mut self, surface: crate::osr::OsrSurface) -> Self {
        self.render_mode = RenderMode::Windowless { surface };
        self
    }
}

/// Process-wide CEF state. This is intentionally `!Send` and `!Sync`.
pub struct CefRuntime {
    owner_thread: ThreadId,
    shutdown: bool,
    _not_send: PhantomData<Rc<()>>,
}

impl CefRuntime {
    pub fn initialize(
        config: CefRuntimeConfig,
        application: Option<&mut cef::App>,
    ) -> Result<Self, CefRuntimeError> {
        prepare_process()?;
        let args = cef::args::Args::new();
        let settings = cef::Settings {
            no_sandbox: 1,
            multi_threaded_message_loop: 0,
            external_message_pump: 0,
            windowless_rendering_enabled: i32::from(config.windowless_rendering),
            cache_path: cef_path(config.cache_path.as_ref()),
            root_cache_path: cef_path(config.root_cache_path.as_ref()),
            // Intentionally empty during CEF integration diagnosis: every CEF
            // helper re-enters this executable and is dispatched at the first
            // statement of `main`.
            browser_subprocess_path: cef::CefString::default(),
            locale: cef_string(config.locale.as_deref()),
            user_agent: cef_string(config.user_agent.as_deref()),
            remote_debugging_port: config.remote_debugging_port.unwrap_or(0) as i32,
            ..Default::default()
        };
        if cef::initialize(
            Some(args.as_main_args()),
            Some(&settings),
            application,
            std::ptr::null_mut(),
        ) != 1
        {
            return Err(CefRuntimeError::InitializeFailed);
        }

        Ok(Self {
            owner_thread: thread::current().id(),
            shutdown: false,
            _not_send: PhantomData,
        })
    }

    /// Advance CEF from the native application's UI loop.
    pub fn do_message_loop_work(&self) -> Result<(), CefRuntimeError> {
        self.ensure_thread()?;
        cef::do_message_loop_work();
        Ok(())
    }

    /// Create a browser: either a real native CEF child window, or — when the
    /// config selects [`RenderMode::Windowless`] — an off-screen browser that
    /// paints into the supplied [`crate::osr::OsrSurface`].
    ///
    /// `client` should normally be `Some` — see [`crate::client`]. `None` is
    /// still accepted (e.g. a test double) but gives the browser zero
    /// navigation policy and no crash signal. A windowless browser additionally
    /// needs the client to expose an OSR render handler, otherwise CEF has
    /// nowhere to paint.
    pub fn create_webview<'runtime>(
        &'runtime self,
        parent: NativeParent,
        config: WebViewConfig,
        client: Option<&mut cef::Client>,
    ) -> Result<WebView<'runtime>, CefRuntimeError> {
        self.ensure_thread()?;
        if config.url.trim().is_empty() {
            return Err(CefRuntimeError::EmptyUrl);
        }
        let windowless = matches!(config.render_mode, RenderMode::Windowless { .. });
        let window_info = if windowless {
            cef::WindowInfo::default().set_as_windowless(parent.as_raw())
        } else {
            cef::WindowInfo::default().set_as_child(parent.as_raw(), &config.bounds.as_cef_rect())
        };
        debug_assert_eq!(
            window_info.windowless_rendering_enabled,
            i32::from(windowless)
        );
        eprintln!(
            "[cef-lifecycle] event=CreateBrowserSync begin url={:?} parent={:?} bounds={:?} render_mode={:?} thread={:?}",
            config.url,
            parent.as_raw(),
            config.bounds,
            config.render_mode,
            std::thread::current().id()
        );
        let browser_settings = cef::BrowserSettings {
            // A transparent windowless surface would composite the timeline
            // through the editor; an opaque background also lets the host
            // upload frames without premultiplied-alpha handling.
            background_color: if windowless { OPAQUE_BACKGROUND } else { 0 },
            windowless_frame_rate: if windowless { WINDOWLESS_FRAME_RATE } else { 0 },
            ..Default::default()
        };
        let browser = cef::browser_host_create_browser_sync(
            Some(&window_info),
            client,
            Some(&cef::CefString::from(config.url.as_str())),
            Some(&browser_settings),
            None,
            None,
        );
        let Some(browser) = browser else {
            eprintln!(
                "[cef-lifecycle] event=CreateBrowserSync result=false url={:?} thread={:?}",
                config.url,
                std::thread::current().id()
            );
            return Err(CefRuntimeError::CreateBrowserFailed);
        };
        eprintln!(
            "[cef-lifecycle] event=CreateBrowserSync result=true browser_id={} url={:?} thread={:?}",
            browser.identifier(),
            config.url,
            std::thread::current().id()
        );

        Ok(WebView {
            browser,
            render_mode: config.render_mode,
            owner_thread: self.owner_thread,
            _runtime: PhantomData,
            _not_send: PhantomData,
        })
    }

    /// Create a web view whose lifetime is not tied to this borrow.
    ///
    /// [`Self::create_webview`] returns a `WebView<'runtime>`, which cannot be
    /// stored in the same struct as the runtime it borrows. A host that owns
    /// both (one CEF runtime plus a map of open editor views) needs this.
    ///
    /// # Safety
    ///
    /// The returned view must be dropped, or [`WebView::close`]d, **before**
    /// the [`CefRuntime`] it came from. Storing both in one struct satisfies
    /// this by declaring the view field before the runtime field, since Rust
    /// drops fields in declaration order.
    pub unsafe fn create_webview_detached(
        &self,
        parent: NativeParent,
        config: WebViewConfig,
        client: Option<&mut cef::Client>,
    ) -> Result<WebView<'static>, CefRuntimeError> {
        let view = self.create_webview(parent, config, client)?;
        // Only the PhantomData borrow marker changes; the browser handle and
        // its thread affinity are carried over unchanged.
        Ok(WebView {
            browser: view.browser.clone(),
            render_mode: view.render_mode.clone(),
            owner_thread: view.owner_thread,
            _runtime: PhantomData,
            _not_send: PhantomData,
        })
    }

    pub fn shutdown(mut self) -> Result<(), CefRuntimeError> {
        self.ensure_thread()?;
        if !self.shutdown {
            cef::shutdown();
            self.shutdown = true;
        }
        Ok(())
    }

    fn ensure_thread(&self) -> Result<(), CefRuntimeError> {
        if thread::current().id() != self.owner_thread {
            return Err(CefRuntimeError::WrongThread);
        }
        Ok(())
    }
}

impl Drop for CefRuntime {
    fn drop(&mut self) {
        if !self.shutdown && thread::current().id() == self.owner_thread {
            cef::shutdown();
            self.shutdown = true;
        }
    }
}

pub struct WebView<'runtime> {
    browser: cef::Browser,
    render_mode: RenderMode,
    owner_thread: ThreadId,
    _runtime: PhantomData<&'runtime CefRuntime>,
    _not_send: PhantomData<Rc<()>>,
}

impl Drop for WebView<'_> {
    fn drop(&mut self) {
        eprintln!(
            "[cef-ref] object_type=cef_browser_t browser_id={} event=webview_release has_one_ref={} has_at_least_one_ref={} thread={:?}",
            self.browser.identifier(),
            self.browser.has_one_ref(),
            self.browser.has_at_least_one_ref(),
            std::thread::current().id()
        );
    }
}

impl WebView<'_> {
    pub fn browser_identifier(&self) -> i32 {
        self.browser.identifier()
    }

    pub fn load_url(&self, url: &str) -> Result<(), CefRuntimeError> {
        self.ensure_thread()?;
        if url.trim().is_empty() {
            return Err(CefRuntimeError::EmptyUrl);
        }
        let frame = self
            .browser
            .main_frame()
            .ok_or(CefRuntimeError::MissingMainFrame)?;
        frame.load_url(Some(&cef::CefString::from(url)));
        Ok(())
    }

    /// Run `code` in the document's main frame. Fire-and-forget — CEF gives no
    /// synchronous return value for this call. Used to push bridge protocol
    /// messages (`futureboard.selectInstance`, ...) into the already-loaded
    /// React app without navigating or reloading the page.
    pub fn execute_javascript(&self, code: &str) -> Result<(), CefRuntimeError> {
        self.ensure_thread()?;
        let frame = self
            .browser
            .main_frame()
            .ok_or(CefRuntimeError::MissingMainFrame)?;
        frame.execute_java_script(Some(&cef::CefString::from(code)), None, 0);
        Ok(())
    }

    /// Resize the view. For a windowed browser this moves the native child
    /// window; for a windowless one it republishes the logical view size to the
    /// off-screen surface and asks CEF to re-read it, which produces a fresh
    /// `OnPaint` at the new size.
    pub fn set_bounds(&self, bounds: WindowBounds) -> Result<(), CefRuntimeError> {
        self.ensure_thread()?;
        let host = self
            .browser
            .host()
            .ok_or(CefRuntimeError::MissingBrowserHost)?;
        match &self.render_mode {
            RenderMode::Windowed => {
                platform_set_bounds(host.window_handle(), bounds)?;
                host.notify_move_or_resize_started();
            }
            RenderMode::Windowless { surface } => {
                let (width, height) = surface.view_size();
                if (width, height) != (bounds.width, bounds.height) {
                    surface.set_view_size(bounds.width, bounds.height, surface.scale_factor());
                }
                host.was_resized();
            }
        }
        Ok(())
    }

    /// The off-screen surface this view paints into, if it is windowless.
    pub fn osr_surface(&self) -> Option<&crate::osr::OsrSurface> {
        match &self.render_mode {
            RenderMode::Windowed => None,
            RenderMode::Windowless { surface } => Some(surface),
        }
    }

    /// Tell CEF the windowless view rect changed (after updating the surface).
    /// No-op for a windowed browser, which resizes through its own HWND.
    pub fn notify_windowless_resized(&self) -> Result<(), CefRuntimeError> {
        self.ensure_thread()?;
        if matches!(self.render_mode, RenderMode::Windowed) {
            return Ok(());
        }
        self.browser
            .host()
            .ok_or(CefRuntimeError::MissingBrowserHost)?
            .was_resized();
        Ok(())
    }

    /// Replay one input event into a windowless browser. Rejected for a
    /// windowed browser, which receives real platform input directly.
    pub fn send_input(&self, input: crate::osr::OsrInput) -> Result<(), CefRuntimeError> {
        self.ensure_thread()?;
        if matches!(self.render_mode, RenderMode::Windowed) {
            return Err(CefRuntimeError::NotWindowless);
        }
        let host = self
            .browser
            .host()
            .ok_or(CefRuntimeError::MissingBrowserHost)?;
        crate::osr::dispatch_input(&host, input);
        Ok(())
    }

    pub fn native_window_handle(&self) -> Result<cef::sys::cef_window_handle_t, CefRuntimeError> {
        self.ensure_thread()?;
        self.browser
            .host()
            .map(|host| host.window_handle())
            .ok_or(CefRuntimeError::MissingBrowserHost)
    }

    pub fn go_back(&self) -> Result<(), CefRuntimeError> {
        self.ensure_thread()?;
        if self.browser.can_go_back() != 0 {
            self.browser.go_back();
        }
        Ok(())
    }

    pub fn go_forward(&self) -> Result<(), CefRuntimeError> {
        self.ensure_thread()?;
        if self.browser.can_go_forward() != 0 {
            self.browser.go_forward();
        }
        Ok(())
    }

    pub fn reload(&self) -> Result<(), CefRuntimeError> {
        self.ensure_thread()?;
        self.browser.reload();
        Ok(())
    }

    pub fn close(&self, force: bool) -> Result<(), CefRuntimeError> {
        self.ensure_thread()?;
        let host = self
            .browser
            .host()
            .ok_or(CefRuntimeError::MissingBrowserHost)?;
        host.close_browser(i32::from(force));
        Ok(())
    }

    fn ensure_thread(&self) -> Result<(), CefRuntimeError> {
        if thread::current().id() != self.owner_thread {
            return Err(CefRuntimeError::WrongThread);
        }
        Ok(())
    }
}

fn cef_path(path: Option<&PathBuf>) -> cef::CefString {
    path.map(|path| cef::CefString::from(path.to_string_lossy().as_ref()))
        .unwrap_or_default()
}

fn cef_string(value: Option<&str>) -> cef::CefString {
    value.map(cef::CefString::from).unwrap_or_default()
}

#[cfg(target_os = "macos")]
fn load_macos_framework() -> Result<(), CefRuntimeError> {
    static LIBRARY: std::sync::OnceLock<Result<cef::library_loader::LibraryLoader, ()>> =
        std::sync::OnceLock::new();

    match LIBRARY.get_or_init(|| {
        let executable = std::env::current_exe().map_err(|_| ())?;
        let loader = std::panic::catch_unwind(|| {
            cef::library_loader::LibraryLoader::new(&executable, false)
        })
        .map_err(|_| ())?;
        loader.load().then_some(loader).ok_or(())
    }) {
        Ok(_) => Ok(()),
        Err(()) => Err(CefRuntimeError::MacFrameworkNotFound),
    }
}

#[cfg(windows)]
fn platform_set_bounds(
    handle: cef::sys::cef_window_handle_t,
    bounds: WindowBounds,
) -> Result<(), CefRuntimeError> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos};
    let ok = unsafe {
        SetWindowPos(
            handle.0.cast(),
            std::ptr::null_mut(),
            bounds.x,
            bounds.y,
            bounds.width,
            bounds.height,
            SWP_NOACTIVATE | SWP_NOZORDER,
        )
    };
    if ok == 0 {
        return Err(CefRuntimeError::PlatformResizeFailed(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn platform_set_bounds(
    handle: cef::sys::cef_window_handle_t,
    bounds: WindowBounds,
) -> Result<(), CefRuntimeError> {
    let xlib = x11_dl::xlib::Xlib::open()
        .map_err(|error| CefRuntimeError::PlatformResizeFailed(error.to_string()))?;
    let display = unsafe { (xlib.XOpenDisplay)(std::ptr::null()) };
    if display.is_null() {
        return Err(CefRuntimeError::PlatformResizeFailed(
            "XOpenDisplay returned null".to_owned(),
        ));
    }
    unsafe {
        (xlib.XMoveResizeWindow)(
            display,
            handle,
            bounds.x,
            bounds.y,
            bounds.width as u32,
            bounds.height as u32,
        );
        (xlib.XFlush)(display);
        (xlib.XCloseDisplay)(display);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn platform_set_bounds(
    handle: cef::sys::cef_window_handle_t,
    bounds: WindowBounds,
) -> Result<(), CefRuntimeError> {
    use objc2::{msg_send, runtime::AnyObject};
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    if handle.is_null() {
        return Err(CefRuntimeError::PlatformResizeFailed(
            "CEF returned a null NSView".to_owned(),
        ));
    }
    let view = unsafe { &*handle.cast::<AnyObject>() };
    let frame = NSRect {
        origin: NSPoint {
            x: bounds.x as f64,
            y: bounds.y as f64,
        },
        size: NSSize {
            width: bounds.width as f64,
            height: bounds.height as f64,
        },
    };
    unsafe {
        let _: () = msg_send![view, setFrame: frame];
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum CefRuntimeError {
    #[error("CEF initialization failed")]
    InitializeFailed,
    #[error("CEF browser creation failed")]
    CreateBrowserFailed,
    #[error("CEF operations must run on the runtime's creating thread")]
    WrongThread,
    #[error("web view URL cannot be empty")]
    EmptyUrl,
    #[error("invalid web view bounds {width}x{height}")]
    InvalidBounds { width: i32, height: i32 },
    #[error("CEF browser has no main frame")]
    MissingMainFrame,
    #[error("CEF browser has no host")]
    MissingBrowserHost,
    #[error("native web view resize failed: {0}")]
    PlatformResizeFailed(String),
    #[error("this operation is only valid for a windowless (off-screen) web view")]
    NotWindowless,
    #[error("failed to resolve the current executable: {0}")]
    CurrentExecutable(#[from] std::io::Error),
    #[cfg(target_os = "macos")]
    #[error("Chromium Embedded Framework.framework was not found in the application bundle")]
    MacFrameworkNotFound,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_positive_bounds() {
        assert!(WindowBounds::new(0, 0, 0, 100).is_err());
        assert!(WindowBounds::new(0, 0, 100, -1).is_err());
    }

    #[test]
    fn accepts_native_child_bounds() {
        assert_eq!(
            WindowBounds::new(4, 8, 1280, 720).unwrap(),
            WindowBounds {
                x: 4,
                y: 8,
                width: 1280,
                height: 720,
            }
        );
    }
}
