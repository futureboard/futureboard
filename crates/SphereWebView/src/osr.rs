//! Windowless (off-screen) CEF rendering for built-in plugin editors.
//!
//! Windows uses `OnAcceleratedPaint`: CEF exposes a callback-scoped D3D11 shared
//! texture which the host copies directly into GPUI-owned GPU memory. This
//! avoids CPU readback, per-frame image allocation, and atlas churn. Linux,
//! macOS, and Windows fallback use [`ImplRenderHandler::on_paint`]; the host
//! uploads that BGRA framebuffer as a texture and feeds input back in with
//! `send_*_event`.
//!
//! ## Threading
//!
//! `on_paint` runs on CEF's UI thread, which is the same thread that drives
//! `do_message_loop_work` — i.e. the GPUI UI thread. The `Mutex` here is
//! therefore effectively uncontended; it exists because CEF handler objects
//! must be `Send`/`Sync`-safe from Rust's point of view, not to synchronize a
//! real cross-thread producer.
//!
//! ## Coordinates
//!
//! Everything CEF is told (view rect, mouse positions, popup rects) is in
//! **logical** pixels (DIP). The framebuffer it hands back is in **physical**
//! pixels — `logical * device_scale_factor`, rounded by Chromium. The host
//! must therefore size the drawn image by the *reported* frame dimensions, not
//! by its own multiplication.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[cfg(target_os = "windows")]
use cef::AcceleratedPaintInfo;
use cef::rc::Rc as _;
use cef::{
    Browser, ImplBrowserHost, ImplRenderHandler, KeyEvent, KeyEventType, MouseButtonType,
    MouseEvent, PaintElementType, Rect, RenderHandler, ScreenInfo, WrapRenderHandler,
    wrap_render_handler,
};

// `cef_event_flags_t` values. The `modifiers` fields on `MouseEvent`/`KeyEvent`
// are plain `u32` bitmasks, so the constants are mirrored rather than converted
// back and forth through the newtype.
const EVENTFLAG_SHIFT_DOWN: u32 = 1 << 1;
const EVENTFLAG_CONTROL_DOWN: u32 = 1 << 2;
const EVENTFLAG_ALT_DOWN: u32 = 1 << 3;
const EVENTFLAG_LEFT_MOUSE_BUTTON: u32 = 1 << 4;
const EVENTFLAG_MIDDLE_MOUSE_BUTTON: u32 = 1 << 5;
const EVENTFLAG_RIGHT_MOUSE_BUTTON: u32 = 1 << 6;
const EVENTFLAG_COMMAND_DOWN: u32 = 1 << 7;

/// One BGRA surface in physical pixels.
#[derive(Default)]
struct Plane {
    width: i32,
    height: i32,
    bgra: Vec<u8>,
}

impl Plane {
    fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0 || self.bgra.is_empty()
    }

    /// Replace the plane's contents with `width * height * 4` bytes read from
    /// a CEF paint buffer.
    ///
    /// # Safety
    ///
    /// `buffer` must point to at least `width * height * 4` readable bytes, as
    /// guaranteed by CEF's `OnPaint` contract.
    unsafe fn update_from_cef(
        &mut self,
        buffer: *const u8,
        width: i32,
        height: i32,
        dirty_rects: &[Rect],
    ) {
        let len = (width as usize) * (height as usize) * 4;
        let needs_full_copy =
            self.width != width || self.height != height || self.bgra.len() != len;
        self.bgra.resize(len, 0);

        if needs_full_copy || dirty_rects.is_empty() {
            // SAFETY: caller guarantees `len` readable bytes at `buffer`, and
            // `resize` made the destination exactly that long.
            unsafe {
                std::ptr::copy_nonoverlapping(buffer, self.bgra.as_mut_ptr(), len);
            }
        } else {
            let stride = width as usize * 4;
            for rect in dirty_rects {
                let left = rect.x.clamp(0, width);
                let top = rect.y.clamp(0, height);
                let right = rect.x.saturating_add(rect.width).clamp(left, width);
                let bottom = rect.y.saturating_add(rect.height).clamp(top, height);
                let row_len = (right - left) as usize * 4;
                if row_len == 0 {
                    continue;
                }
                for row in top..bottom {
                    let offset = row as usize * stride + left as usize * 4;
                    // SAFETY: the clamped rectangle keeps this row within the
                    // full CEF source buffer and equally-sized destination.
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            buffer.add(offset),
                            self.bgra.as_mut_ptr().add(offset),
                            row_len,
                        );
                    }
                }
            }
        }
        self.width = width;
        self.height = height;
    }
}

#[derive(Default)]
struct SurfaceState {
    /// Logical size CEF is told to lay out at, and the scale it renders with.
    view_width: i32,
    view_height: i32,
    scale_factor: f32,
    /// Last `PET_VIEW` paint.
    view: Plane,
    /// Last `PET_POPUP` paint plus its logical placement, kept separate because
    /// CEF paints popups (`<select>` menus, autofill) as their own layer that
    /// the embedder is responsible for compositing.
    popup: Plane,
    popup_rect: Rect,
    popup_visible: bool,
    /// `view` with `popup` composited over it. This allocation exists only
    /// while a popup is visible; the normal path reads `view` directly.
    composited: Plane,
}

impl SurfaceState {
    fn composite(&mut self) {
        if self.view.is_empty() || !self.popup_visible || self.popup.is_empty() {
            self.composited = Plane::default();
            return;
        }
        self.composited.width = self.view.width;
        self.composited.height = self.view.height;
        self.composited.bgra.clone_from(&self.view.bgra);
        let scale = if self.scale_factor > 0.0 {
            self.scale_factor
        } else {
            1.0
        };
        let origin_x = (self.popup_rect.x as f32 * scale).round() as i32;
        let origin_y = (self.popup_rect.y as f32 * scale).round() as i32;
        let dst_w = self.composited.width;
        let dst_h = self.composited.height;
        for row in 0..self.popup.height {
            let dst_y = origin_y + row;
            if dst_y < 0 || dst_y >= dst_h {
                continue;
            }
            let copy_x = origin_x.max(0);
            let skip = (copy_x - origin_x).max(0);
            let copy_w = (self.popup.width - skip).min(dst_w - copy_x);
            if copy_w <= 0 {
                continue;
            }
            let src = ((row * self.popup.width + skip) * 4) as usize;
            let dst = ((dst_y * dst_w + copy_x) * 4) as usize;
            let len = (copy_w * 4) as usize;
            self.composited.bgra[dst..dst + len].copy_from_slice(&self.popup.bgra[src..src + len]);
        }
    }
}

struct SurfaceInner {
    state: Mutex<SurfaceState>,
    accelerated_sink: Option<Arc<dyn OsrAcceleratedFrameSink>>,
    accelerated_failed: AtomicBool,
    accelerated_error_reported: AtomicBool,
    /// Bumped on every composited frame. Read without locking so the host's
    /// pump can decide whether a repaint is even needed.
    generation: AtomicU64,
}

/// Which CEF OSR plane an accelerated texture contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsrPlane {
    View,
    Popup,
}

/// One callback-scoped accelerated CEF frame. The shared texture handle remains
/// owned by CEF and must be copied before [`OsrAcceleratedFrameSink::present`]
/// returns.
#[derive(Debug, Clone, Copy)]
pub struct OsrAcceleratedFrame {
    pub plane: OsrPlane,
    pub shared_texture_handle: usize,
    pub source_x: i32,
    pub source_y: i32,
    pub width: i32,
    pub height: i32,
}

/// Logical popup placement and the device scale used by its physical texture.
#[derive(Debug, Clone)]
pub struct OsrPopupState {
    pub visible: bool,
    pub rect: Rect,
    pub scale_factor: f32,
}

/// Synchronous destination for CEF accelerated OSR frames.
///
/// Implementations must perform the GPU copy inline. Retaining CEF's handle for
/// later use is invalid because Chromium returns that resource to its pool as
/// soon as the callback completes.
pub trait OsrAcceleratedFrameSink: Send + Sync {
    fn present(&self, frame: OsrAcceleratedFrame) -> Result<(), String>;
    fn set_popup_state(&self, state: OsrPopupState);

    /// Whether previously presented host-owned GPU images still exist. GPUI's
    /// device-loss recovery clears its atlas; returning false recreates this
    /// browser in software mode instead of leaving a static page blank.
    fn is_valid(&self) -> bool {
        true
    }
}

/// Shared off-screen framebuffer for one windowless browser.
///
/// Cloning shares the same surface: the host keeps one handle, the CEF render
/// handler holds another.
#[derive(Clone)]
pub struct OsrSurface(Arc<SurfaceInner>);

impl OsrSurface {
    /// Create a surface sized in logical pixels at `scale_factor`.
    pub fn new(width: i32, height: i32, scale_factor: f32) -> Self {
        Self::new_with_sink(width, height, scale_factor, None)
    }

    /// Create a Windows accelerated surface. CEF calls the sink with a D3D11
    /// shared texture, which the GPUI host copies into its own GPU texture before
    /// the callback returns.
    pub fn new_accelerated(
        width: i32,
        height: i32,
        scale_factor: f32,
        sink: Arc<dyn OsrAcceleratedFrameSink>,
    ) -> Self {
        Self::new_with_sink(width, height, scale_factor, Some(sink))
    }

    fn new_with_sink(
        width: i32,
        height: i32,
        scale_factor: f32,
        accelerated_sink: Option<Arc<dyn OsrAcceleratedFrameSink>>,
    ) -> Self {
        Self(Arc::new(SurfaceInner {
            state: Mutex::new(SurfaceState {
                view_width: width.max(1),
                view_height: height.max(1),
                scale_factor: if scale_factor > 0.0 {
                    scale_factor
                } else {
                    1.0
                },
                ..Default::default()
            }),
            accelerated_sink,
            accelerated_failed: AtomicBool::new(false),
            accelerated_error_reported: AtomicBool::new(false),
            generation: AtomicU64::new(0),
        }))
    }

    /// Whether this surface requests CEF's shared-texture accelerated paint path.
    pub fn is_accelerated(&self) -> bool {
        self.0.accelerated_sink.is_some()
    }

    /// Read and clear the signal that the host could not copy an accelerated
    /// frame. The browser must be recreated with shared textures disabled;
    /// CEF does not switch an existing browser from accelerated to `OnPaint`.
    pub fn take_accelerated_failure(&self) -> bool {
        let sink_invalid = self
            .0
            .accelerated_sink
            .as_ref()
            .is_some_and(|sink| !sink.is_valid());
        self.0.accelerated_failed.swap(false, Ordering::AcqRel) || sink_invalid
    }

    /// Update the logical size/scale CEF should lay out at. The caller must
    /// follow this with [`crate::runtime::WebView::notify_windowless_resized`]
    /// so the browser re-reads the view rect.
    pub fn set_view_size(&self, width: i32, height: i32, scale_factor: f32) {
        let popup_state = if let Ok(mut state) = self.0.state.lock() {
            state.view_width = width.max(1);
            state.view_height = height.max(1);
            state.scale_factor = if scale_factor > 0.0 {
                scale_factor
            } else {
                1.0
            };
            Some(OsrPopupState {
                visible: state.popup_visible,
                rect: state.popup_rect.clone(),
                scale_factor: state.scale_factor,
            })
        } else {
            None
        };
        if let (Some(sink), Some(popup_state)) = (&self.0.accelerated_sink, popup_state) {
            sink.set_popup_state(popup_state);
        }
    }

    /// Device scale factor CEF is currently rendering at.
    pub fn scale_factor(&self) -> f32 {
        self.0
            .state
            .lock()
            .map(|state| state.scale_factor)
            .unwrap_or(1.0)
    }

    /// Logical size CEF is currently laying out at.
    pub fn view_size(&self) -> (i32, i32) {
        self.0
            .state
            .lock()
            .map(|state| (state.view_width, state.view_height))
            .unwrap_or((0, 0))
    }

    /// Frame counter. Cheap enough to poll every pump tick.
    pub fn generation(&self) -> u64 {
        self.0.generation.load(Ordering::Acquire)
    }

    /// Run `read` against the latest BGRA frame
    /// (`bytes`, `width`, `height` in physical pixels). Returns `None` until
    /// the first paint arrives. The normal path borrows the view plane directly;
    /// a separate composited allocation is used only while a popup is visible.
    pub fn with_frame<R>(&self, read: impl FnOnce(&[u8], i32, i32) -> R) -> Option<R> {
        let state = self.0.state.lock().ok()?;
        let frame = if state.popup_visible && !state.composited.is_empty() {
            &state.composited
        } else {
            &state.view
        };
        if frame.is_empty() {
            return None;
        }
        Some(read(&frame.bgra, frame.width, frame.height))
    }

    fn on_paint(
        &self,
        element: PaintElementType,
        dirty_rects: &[Rect],
        buffer: *const u8,
        width: i32,
        height: i32,
    ) {
        if buffer.is_null() || width <= 0 || height <= 0 {
            return;
        }
        let Ok(mut state) = self.0.state.lock() else {
            return;
        };
        // SAFETY: CEF documents `buffer` as `width * height * 4` bytes valid
        // for the duration of the callback.
        unsafe {
            if element == PaintElementType::POPUP {
                state
                    .popup
                    .update_from_cef(buffer, width, height, dirty_rects);
            } else {
                state
                    .view
                    .update_from_cef(buffer, width, height, dirty_rects);
            }
        }
        state.composite();
        drop(state);
        self.0.generation.fetch_add(1, Ordering::Release);
    }

    fn set_popup_visible(&self, visible: bool) {
        let popup_state = if let Ok(mut state) = self.0.state.lock() {
            state.popup_visible = visible;
            if !visible {
                state.popup = Plane::default();
            }
            state.composite();
            Some(OsrPopupState {
                visible,
                rect: state.popup_rect.clone(),
                scale_factor: state.scale_factor,
            })
        } else {
            None
        };
        if let (Some(sink), Some(popup_state)) = (&self.0.accelerated_sink, popup_state) {
            sink.set_popup_state(popup_state);
        }
        self.0.generation.fetch_add(1, Ordering::Release);
    }

    fn set_popup_rect(&self, rect: Rect) {
        let popup_state = if let Ok(mut state) = self.0.state.lock() {
            state.popup_rect = rect;
            Some(OsrPopupState {
                visible: state.popup_visible,
                rect: state.popup_rect.clone(),
                scale_factor: state.scale_factor,
            })
        } else {
            None
        };
        if let (Some(sink), Some(popup_state)) = (&self.0.accelerated_sink, popup_state) {
            sink.set_popup_state(popup_state);
        }
        self.0.generation.fetch_add(1, Ordering::Release);
    }

    #[cfg(target_os = "windows")]
    fn on_accelerated_paint(&self, element: PaintElementType, info: Option<&AcceleratedPaintInfo>) {
        let (Some(sink), Some(info)) = (&self.0.accelerated_sink, info) else {
            return;
        };
        let visible = &info.extra.visible_rect;
        let coded = &info.extra.coded_size;
        let (source_x, source_y, width, height) = if visible.width > 0 && visible.height > 0 {
            (visible.x, visible.y, visible.width, visible.height)
        } else {
            (0, 0, coded.width, coded.height)
        };
        let frame = OsrAcceleratedFrame {
            plane: if element == PaintElementType::POPUP {
                OsrPlane::Popup
            } else {
                OsrPlane::View
            },
            shared_texture_handle: info.shared_texture_handle as usize,
            source_x,
            source_y,
            width,
            height,
        };
        if frame.shared_texture_handle == 0 || frame.width <= 0 || frame.height <= 0 {
            return;
        }
        match sink.present(frame) {
            Ok(()) => {
                self.0.generation.fetch_add(1, Ordering::Release);
            }
            Err(error) => {
                self.0.accelerated_failed.store(true, Ordering::Release);
                if !self
                    .0
                    .accelerated_error_reported
                    .swap(true, Ordering::AcqRel)
                {
                    eprintln!("[cef-osr] accelerated texture copy failed: {error}");
                }
            }
        }
    }
}

wrap_render_handler! {
    pub struct OsrRenderHandler {
        surface: OsrSurface,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            let Some(rect) = rect else { return };
            let (width, height) = self.surface.view_size();
            rect.x = 0;
            rect.y = 0;
            rect.width = width.max(1);
            rect.height = height.max(1);
        }

        fn screen_info(
            &self,
            _browser: Option<&mut Browser>,
            screen_info: Option<&mut ScreenInfo>,
        ) -> ::std::os::raw::c_int {
            let Some(screen_info) = screen_info else { return 0 };
            let (width, height) = self.surface.view_size();
            {
                use std::sync::atomic::{AtomicBool, Ordering};
                static LOGGED: AtomicBool = AtomicBool::new(false);
                if !LOGGED.swap(true, Ordering::Relaxed) {
                    eprintln!(
                        "[cef-osr] GetScreenInfo called: reporting scale={} view={}x{}",
                        self.surface.scale_factor(),
                        width,
                        height
                    );
                }
            }
            screen_info.device_scale_factor = self.surface.scale_factor();
            screen_info.depth = 32;
            screen_info.depth_per_component = 8;
            screen_info.is_monochrome = 0;
            screen_info.rect = Rect { x: 0, y: 0, width: width.max(1), height: height.max(1) };
            screen_info.available_rect = screen_info.rect.clone();
            1
        }

        fn on_popup_show(&self, _browser: Option<&mut Browser>, show: ::std::os::raw::c_int) {
            self.surface.set_popup_visible(show != 0);
        }

        fn on_popup_size(&self, _browser: Option<&mut Browser>, rect: Option<&Rect>) {
            if let Some(rect) = rect {
                self.surface.set_popup_rect(rect.clone());
            }
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: ::std::os::raw::c_int,
            height: ::std::os::raw::c_int,
        ) {
            self.surface
                .on_paint(type_, _dirty_rects.unwrap_or(&[]), buffer, width, height);
        }

        #[cfg(target_os = "windows")]
        fn on_accelerated_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            info: Option<&AcceleratedPaintInfo>,
        ) {
            self.surface.on_accelerated_paint(type_, info);
        }
    }
}

/// Build the render handler CEF paints into for `surface`.
pub fn osr_render_handler(surface: OsrSurface) -> RenderHandler {
    OsrRenderHandler::new(surface)
}

/// Keyboard/mouse modifier state, translated to CEF event flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OsrModifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub command: bool,
    pub left_button: bool,
    pub middle_button: bool,
    pub right_button: bool,
}

impl OsrModifiers {
    fn flags(self) -> u32 {
        let mut flags = 0;
        if self.shift {
            flags |= EVENTFLAG_SHIFT_DOWN;
        }
        if self.control {
            flags |= EVENTFLAG_CONTROL_DOWN;
        }
        if self.alt {
            flags |= EVENTFLAG_ALT_DOWN;
        }
        if self.command {
            flags |= EVENTFLAG_COMMAND_DOWN;
        }
        if self.left_button {
            flags |= EVENTFLAG_LEFT_MOUSE_BUTTON;
        }
        if self.middle_button {
            flags |= EVENTFLAG_MIDDLE_MOUSE_BUTTON;
        }
        if self.right_button {
            flags |= EVENTFLAG_RIGHT_MOUSE_BUTTON;
        }
        flags
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsrMouseButton {
    Left,
    Middle,
    Right,
}

impl OsrMouseButton {
    fn cef(self) -> MouseButtonType {
        match self {
            Self::Left => MouseButtonType::LEFT,
            Self::Middle => MouseButtonType::MIDDLE,
            Self::Right => MouseButtonType::RIGHT,
        }
    }
}

/// A key press to replay into the browser.
///
/// `windows_key_code` is Chromium's `VKEY_*` (identical to Win32 `VK_*`) —
/// the platform-independent code CEF expects on every OS. `character` is the
/// UTF-16 code unit for a `Char` event and is ignored otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsrKeyKind {
    Down,
    Up,
    Char,
}

#[derive(Debug, Clone, Copy)]
pub struct OsrKey {
    pub kind: OsrKeyKind,
    pub windows_key_code: i32,
    pub character: u16,
    pub modifiers: OsrModifiers,
}

/// One input event destined for a windowless browser.
#[derive(Debug, Clone, Copy)]
pub enum OsrInput {
    /// Logical-pixel cursor position inside the view.
    MouseMove {
        x: i32,
        y: i32,
        modifiers: OsrModifiers,
        leaving: bool,
    },
    MouseButton {
        x: i32,
        y: i32,
        button: OsrMouseButton,
        pressed: bool,
        click_count: i32,
        modifiers: OsrModifiers,
    },
    /// Logical-pixel scroll deltas, in the same direction convention as
    /// Chromium (positive `delta_y` scrolls content down).
    MouseWheel {
        x: i32,
        y: i32,
        delta_x: i32,
        delta_y: i32,
        modifiers: OsrModifiers,
    },
    Key(OsrKey),
    Focus(bool),
}

/// Replay `input` into `host`. Called only from the CEF/GPUI UI thread — the
/// caller ([`crate::runtime::WebView::send_input`]) enforces that.
pub(crate) fn dispatch_input(host: &cef::BrowserHost, input: OsrInput) {
    match input {
        OsrInput::MouseMove {
            x,
            y,
            modifiers,
            leaving,
        } => {
            let event = MouseEvent {
                x,
                y,
                modifiers: modifiers.flags(),
            };
            host.send_mouse_move_event(Some(&event), i32::from(leaving));
        }
        OsrInput::MouseButton {
            x,
            y,
            button,
            pressed,
            click_count,
            modifiers,
        } => {
            let event = MouseEvent {
                x,
                y,
                modifiers: modifiers.flags(),
            };
            host.send_mouse_click_event(
                Some(&event),
                button.cef(),
                i32::from(!pressed),
                click_count.max(1),
            );
        }
        OsrInput::MouseWheel {
            x,
            y,
            delta_x,
            delta_y,
            modifiers,
        } => {
            let event = MouseEvent {
                x,
                y,
                modifiers: modifiers.flags(),
            };
            host.send_mouse_wheel_event(Some(&event), delta_x, delta_y);
        }
        OsrInput::Key(key) => {
            let event = KeyEvent {
                type_: match key.kind {
                    OsrKeyKind::Down => KeyEventType::RAWKEYDOWN,
                    OsrKeyKind::Up => KeyEventType::KEYUP,
                    OsrKeyKind::Char => KeyEventType::CHAR,
                },
                modifiers: key.modifiers.flags(),
                windows_key_code: key.windows_key_code,
                native_key_code: 0,
                character: key.character,
                unmodified_character: key.character,
                ..Default::default()
            };
            host.send_key_event(Some(&event));
        }
        OsrInput::Focus(focused) => host.set_focus(i32::from(focused)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_flags_match_cef_event_flags() {
        let modifiers = OsrModifiers {
            shift: true,
            control: true,
            left_button: true,
            ..Default::default()
        };
        assert_eq!(
            modifiers.flags(),
            EVENTFLAG_SHIFT_DOWN | EVENTFLAG_CONTROL_DOWN | EVENTFLAG_LEFT_MOUSE_BUTTON
        );
        assert_eq!(OsrModifiers::default().flags(), 0);
    }

    #[test]
    fn a_fresh_surface_has_no_frame_yet() {
        let surface = OsrSurface::new(320, 200, 1.0);
        assert_eq!(surface.view_size(), (320, 200));
        assert_eq!(surface.generation(), 0);
        assert!(surface.with_frame(|_, _, _| ()).is_none());
    }

    #[cfg(target_os = "windows")]
    #[derive(Default)]
    struct RecordingAcceleratedSink {
        frames: Mutex<Vec<OsrAcceleratedFrame>>,
        fail: AtomicBool,
    }

    #[cfg(target_os = "windows")]
    impl OsrAcceleratedFrameSink for RecordingAcceleratedSink {
        fn present(&self, frame: OsrAcceleratedFrame) -> Result<(), String> {
            if self.fail.load(Ordering::Acquire) {
                return Err("test failure".to_string());
            }
            self.frames.lock().expect("frames").push(frame);
            Ok(())
        }

        fn set_popup_state(&self, _state: OsrPopupState) {}
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn accelerated_paint_uses_the_visible_rect_and_signals_failure() {
        let sink = Arc::new(RecordingAcceleratedSink::default());
        let surface = OsrSurface::new_accelerated(320, 200, 1.0, sink.clone());
        let mut info = AcceleratedPaintInfo::default();
        info.shared_texture_handle = 1usize as *mut core::ffi::c_void;
        info.extra.coded_size.width = 512;
        info.extra.coded_size.height = 256;
        info.extra.visible_rect = Rect {
            x: 4,
            y: 8,
            width: 320,
            height: 200,
        };

        surface.on_accelerated_paint(PaintElementType::VIEW, Some(&info));
        assert_eq!(surface.generation(), 1);
        let frames = sink.frames.lock().expect("frames");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].plane, OsrPlane::View);
        assert_eq!(
            (
                frames[0].source_x,
                frames[0].source_y,
                frames[0].width,
                frames[0].height
            ),
            (4, 8, 320, 200)
        );
        drop(frames);

        sink.fail.store(true, Ordering::Release);
        surface.on_accelerated_paint(PaintElementType::VIEW, Some(&info));
        assert_eq!(surface.generation(), 1);
        assert!(surface.take_accelerated_failure());
        assert!(!surface.take_accelerated_failure());
    }

    #[test]
    fn a_view_paint_becomes_the_composited_frame() {
        let surface = OsrSurface::new(2, 2, 1.0);
        let pixels = [7u8; 2 * 2 * 4];
        surface.on_paint(PaintElementType::VIEW, &[], pixels.as_ptr(), 2, 2);
        assert_eq!(surface.generation(), 1);
        let (len, w, h) = surface
            .with_frame(|bytes, w, h| (bytes.len(), w, h))
            .expect("a frame was painted");
        assert_eq!((len, w, h), (16, 2, 2));
        let state = surface.0.state.lock().expect("surface state");
        assert!(
            state.composited.is_empty(),
            "the normal path must not retain a duplicate full-frame buffer"
        );
    }

    #[test]
    fn a_popup_paint_is_composited_over_the_view_at_its_rect() {
        let surface = OsrSurface::new(4, 4, 1.0);
        let view = [0u8; 4 * 4 * 4];
        surface.on_paint(PaintElementType::VIEW, &[], view.as_ptr(), 4, 4);
        surface.set_popup_rect(Rect {
            x: 1,
            y: 1,
            width: 2,
            height: 2,
        });
        surface.set_popup_visible(true);
        let popup = [9u8; 2 * 2 * 4];
        surface.on_paint(PaintElementType::POPUP, &[], popup.as_ptr(), 2, 2);

        let bytes = surface
            .with_frame(|bytes, _, _| bytes.to_vec())
            .expect("a frame was painted");
        // Row 0 is untouched view content; row 1 has the popup at column 1..3.
        assert_eq!(&bytes[0..16], &[0u8; 16]);
        assert_eq!(&bytes[(4 + 1) * 4..(4 + 3) * 4], &[9u8; 8]);
    }

    #[test]
    fn hiding_a_popup_restores_the_view_underneath() {
        let surface = OsrSurface::new(2, 2, 1.0);
        let view = [0u8; 2 * 2 * 4];
        surface.on_paint(PaintElementType::VIEW, &[], view.as_ptr(), 2, 2);
        surface.set_popup_visible(true);
        let popup = [9u8; 2 * 2 * 4];
        surface.on_paint(PaintElementType::POPUP, &[], popup.as_ptr(), 2, 2);
        surface.set_popup_visible(false);

        let bytes = surface
            .with_frame(|bytes, _, _| bytes.to_vec())
            .expect("a frame was painted");
        assert_eq!(bytes, vec![0u8; 16]);
        let state = surface.0.state.lock().expect("surface state");
        assert!(state.popup.is_empty());
        assert!(state.composited.is_empty());
    }

    #[test]
    fn dirty_rect_paints_only_update_changed_pixels() {
        let surface = OsrSurface::new(3, 2, 1.0);
        let initial = [1u8; 3 * 2 * 4];
        surface.on_paint(PaintElementType::VIEW, &[], initial.as_ptr(), 3, 2);

        let mut next = [9u8; 3 * 2 * 4];
        next[0..4].fill(5);
        surface.on_paint(
            PaintElementType::VIEW,
            &[Rect {
                x: 1,
                y: 0,
                width: 1,
                height: 1,
            }],
            next.as_ptr(),
            3,
            2,
        );

        let bytes = surface
            .with_frame(|bytes, _, _| bytes.to_vec())
            .expect("a frame was painted");
        assert_eq!(&bytes[0..4], &[1u8; 4]);
        assert_eq!(&bytes[4..8], &[9u8; 4]);
        assert_eq!(&bytes[8..], &[1u8; 16]);
    }

    #[test]
    fn resizing_the_view_is_reported_to_the_render_handler() {
        let surface = OsrSurface::new(320, 200, 1.0);
        surface.set_view_size(640, 480, 2.0);
        assert_eq!(surface.view_size(), (640, 480));
    }

    /// A degenerate size must never reach CEF: a zero-width view rect makes
    /// Chromium drop the browser's compositor frame entirely.
    #[test]
    fn degenerate_sizes_clamp_to_one_pixel() {
        let surface = OsrSurface::new(0, -4, 0.0);
        assert_eq!(surface.view_size(), (1, 1));
    }
}
