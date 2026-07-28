use std::sync::OnceLock;

use ::util::ResultExt;
use anyhow::Context;
use windows::{
    UI::{
        Color,
        ViewManagement::{UIColorType, UISettings},
    },
    Win32::{
        Foundation::*,
        Graphics::{Dwm::*, Gdi::*},
        System::LibraryLoader::LoadLibraryA,
        UI::{
            HiDpi::{GetDpiForSystem, GetDpiForWindow},
            WindowsAndMessaging::*,
        },
    },
    core::{BOOL, PCSTR},
};

use crate::*;
use gpui::*;

pub(crate) trait HiLoWord {
    fn hiword(&self) -> u16;
    fn loword(&self) -> u16;
    fn signed_hiword(&self) -> i16;
    fn signed_loword(&self) -> i16;
}

impl HiLoWord for WPARAM {
    fn hiword(&self) -> u16 {
        ((self.0 >> 16) & 0xFFFF) as u16
    }

    fn loword(&self) -> u16 {
        (self.0 & 0xFFFF) as u16
    }

    fn signed_hiword(&self) -> i16 {
        ((self.0 >> 16) & 0xFFFF) as i16
    }

    fn signed_loword(&self) -> i16 {
        (self.0 & 0xFFFF) as i16
    }
}

impl HiLoWord for LPARAM {
    fn hiword(&self) -> u16 {
        ((self.0 >> 16) & 0xFFFF) as u16
    }

    fn loword(&self) -> u16 {
        (self.0 & 0xFFFF) as u16
    }

    fn signed_hiword(&self) -> i16 {
        ((self.0 >> 16) & 0xFFFF) as i16
    }

    fn signed_loword(&self) -> i16 {
        (self.0 & 0xFFFF) as i16
    }
}

pub(crate) unsafe fn get_window_long(hwnd: HWND, nindex: WINDOW_LONG_PTR_INDEX) -> isize {
    #[cfg(target_pointer_width = "64")]
    unsafe {
        GetWindowLongPtrW(hwnd, nindex)
    }
    #[cfg(target_pointer_width = "32")]
    unsafe {
        GetWindowLongW(hwnd, nindex) as isize
    }
}

pub(crate) unsafe fn set_window_long(
    hwnd: HWND,
    nindex: WINDOW_LONG_PTR_INDEX,
    dwnewlong: isize,
) -> isize {
    #[cfg(target_pointer_width = "64")]
    unsafe {
        SetWindowLongPtrW(hwnd, nindex, dwnewlong)
    }
    #[cfg(target_pointer_width = "32")]
    unsafe {
        SetWindowLongW(hwnd, nindex, dwnewlong as i32) as isize
    }
}

pub(crate) fn windows_credentials_target_name(url: &str) -> String {
    format!("zed:url={}", url)
}

pub(crate) fn load_cursor(style: CursorStyle) -> Option<HCURSOR> {
    if let Some(cursor) = load_custom_cursor(style, current_cursor_dpi()) {
        return Some(cursor);
    }

    static ARROW: OnceLock<SafeCursor> = OnceLock::new();
    static IBEAM: OnceLock<SafeCursor> = OnceLock::new();
    static CROSS: OnceLock<SafeCursor> = OnceLock::new();
    static HAND: OnceLock<SafeCursor> = OnceLock::new();
    static SIZEWE: OnceLock<SafeCursor> = OnceLock::new();
    static SIZENS: OnceLock<SafeCursor> = OnceLock::new();
    static SIZENWSE: OnceLock<SafeCursor> = OnceLock::new();
    static SIZENESW: OnceLock<SafeCursor> = OnceLock::new();
    static NO: OnceLock<SafeCursor> = OnceLock::new();
    let (lock, name) = match style {
        CursorStyle::IBeam | CursorStyle::IBeamCursorForVerticalLayout => (&IBEAM, IDC_IBEAM),
        CursorStyle::Crosshair => (&CROSS, IDC_CROSS),
        CursorStyle::PointingHand | CursorStyle::DragLink => (&HAND, IDC_HAND),
        CursorStyle::ResizeLeft
        | CursorStyle::ResizeRight
        | CursorStyle::ResizeLeftRight
        | CursorStyle::ResizeColumn => (&SIZEWE, IDC_SIZEWE),
        CursorStyle::ResizeUp
        | CursorStyle::ResizeDown
        | CursorStyle::ResizeUpDown
        | CursorStyle::ResizeRow => (&SIZENS, IDC_SIZENS),
        CursorStyle::ResizeUpLeftDownRight => (&SIZENWSE, IDC_SIZENWSE),
        CursorStyle::ResizeUpRightDownLeft => (&SIZENESW, IDC_SIZENESW),
        CursorStyle::OperationNotAllowed => (&NO, IDC_NO),
        _ => (&ARROW, IDC_ARROW),
    };
    Some(
        *(*lock.get_or_init(|| {
            HCURSOR(
                unsafe { LoadImageW(None, name, IMAGE_CURSOR, 0, 0, LR_DEFAULTSIZE | LR_SHARED) }
                    .log_err()
                    .unwrap_or_default()
                    .0,
            )
            .into()
        })),
    )
}

#[allow(dead_code)]
struct CursorAssetSet {
    half: &'static [u8],
    one: &'static [u8],
    one_half: &'static [u8],
    two: &'static [u8],
    hotspot_1x: (u32, u32),
}

const FUTUREBOARD_CURSOR_RENDER_SCALE: f32 = 0.7;
const FUTUREBOARD_CURSOR_BASE_ASSET_SCALE: f32 = 0.5 * FUTUREBOARD_CURSOR_RENDER_SCALE;
const CURSOR_DPI_BUCKETS: [u32; 8] = [96, 120, 144, 168, 192, 240, 288, 384];
type CursorCache = [OnceLock<SafeCursor>; CURSOR_DPI_BUCKETS.len()];

const FB_ARROW: CursorAssetSet = CursorAssetSet {
    half: include_bytes!("../../../../../packages/shared/cursors/Arrow@0.5x.png"),
    one: include_bytes!("../../../../../packages/shared/cursors/Arrow@1x.png"),
    one_half: include_bytes!("../../../../../packages/shared/cursors/Arrow@1.5x.png"),
    two: include_bytes!("../../../../../packages/shared/cursors/Arrow@2x.png"),
    hotspot_1x: (2, 3),
};

const FB_SELECT: CursorAssetSet = CursorAssetSet {
    half: include_bytes!("../../../../../packages/shared/cursors/Select@0.5x.png"),
    one: include_bytes!("../../../../../packages/shared/cursors/Select@1x.png"),
    one_half: include_bytes!("../../../../../packages/shared/cursors/Select@1.5x.png"),
    two: include_bytes!("../../../../../packages/shared/cursors/Select@2x.png"),
    hotspot_1x: (2, 2),
};

const FB_MARQUEE: CursorAssetSet = CursorAssetSet {
    half: include_bytes!("../../../../../packages/shared/cursors/Marquee@0.5x.png"),
    one: include_bytes!("../../../../../packages/shared/cursors/Marquee@1x.png"),
    one_half: include_bytes!("../../../../../packages/shared/cursors/Marquee@1.5x.png"),
    two: include_bytes!("../../../../../packages/shared/cursors/Marquee@2x.png"),
    hotspot_1x: (8, 8),
};

const FB_MOVE: CursorAssetSet = CursorAssetSet {
    half: include_bytes!("../../../../../packages/shared/cursors/Move@0.5x.png"),
    one: include_bytes!("../../../../../packages/shared/cursors/Move@1x.png"),
    one_half: include_bytes!("../../../../../packages/shared/cursors/Move@1.5x.png"),
    two: include_bytes!("../../../../../packages/shared/cursors/Move@2x.png"),
    hotspot_1x: (52, 52),
};

const FB_FADE_IN: CursorAssetSet = CursorAssetSet {
    half: include_bytes!("../../../../../packages/shared/cursors/FadeIn@0.5x.png"),
    one: include_bytes!("../../../../../packages/shared/cursors/FadeIn@1x.png"),
    one_half: include_bytes!("../../../../../packages/shared/cursors/FadeIn@1.5x.png"),
    two: include_bytes!("../../../../../packages/shared/cursors/FadeIn@2x.png"),
    hotspot_1x: (6, 37),
};

const FB_FADE_OUT: CursorAssetSet = CursorAssetSet {
    half: include_bytes!("../../../../../packages/shared/cursors/FadeOut@0.5x.png"),
    one: include_bytes!("../../../../../packages/shared/cursors/FadeOut@1x.png"),
    one_half: include_bytes!("../../../../../packages/shared/cursors/FadeOut@1.5x.png"),
    two: include_bytes!("../../../../../packages/shared/cursors/FadeOut@2x.png"),
    hotspot_1x: (72, 37),
};

const FB_RESIZE_HORIZON: CursorAssetSet = CursorAssetSet {
    half: include_bytes!("../../../../../packages/shared/cursors/ResizeHorizon@0.5x.png"),
    one: include_bytes!("../../../../../packages/shared/cursors/ResizeHorizon@1x.png"),
    one_half: include_bytes!("../../../../../packages/shared/cursors/ResizeHorizon@1.5x.png"),
    two: include_bytes!("../../../../../packages/shared/cursors/ResizeHorizon@2x.png"),
    hotspot_1x: (45, 32),
};

const FB_RESIZE_LEFT: CursorAssetSet = CursorAssetSet {
    half: include_bytes!("../../../../../packages/shared/cursors/ResizeLeft@0.5x.png"),
    one: include_bytes!("../../../../../packages/shared/cursors/ResizeLeft@1x.png"),
    one_half: include_bytes!("../../../../../packages/shared/cursors/ResizeLeft@1.5x.png"),
    two: include_bytes!("../../../../../packages/shared/cursors/ResizeLeft@2x.png"),
    hotspot_1x: (4, 53),
};

const FB_RESIZE_RIGHT: CursorAssetSet = CursorAssetSet {
    half: include_bytes!("../../../../../packages/shared/cursors/ResizeRight@0.5x.png"),
    one: include_bytes!("../../../../../packages/shared/cursors/ResizeRight@1x.png"),
    one_half: include_bytes!("../../../../../packages/shared/cursors/ResizeRight@1.5x.png"),
    two: include_bytes!("../../../../../packages/shared/cursors/ResizeRight@2x.png"),
    hotspot_1x: (82, 53),
};

fn load_custom_cursor(style: CursorStyle, dpi: u32) -> Option<HCURSOR> {
    static ARROW: CursorCache = [const { OnceLock::new() }; CURSOR_DPI_BUCKETS.len()];
    static SELECT: CursorCache = [const { OnceLock::new() }; CURSOR_DPI_BUCKETS.len()];
    static MARQUEE: CursorCache = [const { OnceLock::new() }; CURSOR_DPI_BUCKETS.len()];
    static MOVE: CursorCache = [const { OnceLock::new() }; CURSOR_DPI_BUCKETS.len()];
    static FADE_IN: CursorCache = [const { OnceLock::new() }; CURSOR_DPI_BUCKETS.len()];
    static FADE_OUT: CursorCache = [const { OnceLock::new() }; CURSOR_DPI_BUCKETS.len()];
    static RESIZE_HORIZON: CursorCache = [const { OnceLock::new() }; CURSOR_DPI_BUCKETS.len()];
    static RESIZE_LEFT: CursorCache = [const { OnceLock::new() }; CURSOR_DPI_BUCKETS.len()];
    static RESIZE_RIGHT: CursorCache = [const { OnceLock::new() }; CURSOR_DPI_BUCKETS.len()];

    let (lock, assets) = match style {
        // Keep the ordinary desktop pointer native. Futureboard's illustrated
        // arrow is opt-in for editing surfaces through `FutureboardArrow`.
        CursorStyle::FutureboardArrow => (&ARROW, &FB_ARROW),
        CursorStyle::FutureboardSelect => (&SELECT, &FB_SELECT),
        CursorStyle::FutureboardMarquee => (&MARQUEE, &FB_MARQUEE),
        CursorStyle::FutureboardMove => (&MOVE, &FB_MOVE),
        CursorStyle::FutureboardFadeIn => (&FADE_IN, &FB_FADE_IN),
        CursorStyle::FutureboardFadeOut => (&FADE_OUT, &FB_FADE_OUT),
        CursorStyle::FutureboardResizeHorizon => (&RESIZE_HORIZON, &FB_RESIZE_HORIZON),
        CursorStyle::FutureboardResizeLeft => (&RESIZE_LEFT, &FB_RESIZE_LEFT),
        CursorStyle::FutureboardResizeRight => (&RESIZE_RIGHT, &FB_RESIZE_RIGHT),
        _ => return None,
    };

    let bucket = cursor_dpi_bucket(dpi);
    Some(**lock[bucket].get_or_init(|| {
        create_custom_cursor(assets, CURSOR_DPI_BUCKETS[bucket])
            .unwrap_or_default()
            .into()
    }))
}

fn current_cursor_dpi() -> u32 {
    let mut point = POINT::default();
    let window_dpi = unsafe {
        GetCursorPos(&mut point)
            .ok()
            .map(|_| WindowFromPoint(point))
            .filter(|window| !window.is_invalid())
            .map(|window| GetDpiForWindow(window))
            .filter(|dpi| *dpi > 0)
    };
    window_dpi.unwrap_or_else(|| unsafe { GetDpiForSystem() }.max(96))
}

fn cursor_dpi_bucket(dpi: u32) -> usize {
    CURSOR_DPI_BUCKETS
        .iter()
        .enumerate()
        .min_by_key(|(_, bucket)| bucket.abs_diff(dpi.max(96)))
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn target_cursor_asset_scale(dpi: u32) -> f32 {
    FUTUREBOARD_CURSOR_BASE_ASSET_SCALE * dpi.max(96) as f32 / 96.0
}

fn select_cursor_png(assets: &CursorAssetSet, target_scale: f32) -> (&'static [u8], f32) {
    [
        (assets.half, 0.5_f32),
        (assets.one, 1.0),
        (assets.one_half, 1.5),
        (assets.two, 2.0),
    ]
    .into_iter()
    .min_by(|(_, left), (_, right)| {
        (left - target_scale)
            .abs()
            .total_cmp(&(right - target_scale).abs())
    })
    .unwrap_or((assets.half, 0.5))
}

fn create_custom_cursor(assets: &CursorAssetSet, dpi: u32) -> Option<HCURSOR> {
    let target_scale = target_cursor_asset_scale(dpi);
    let (bytes, source_scale) = select_cursor_png(assets, target_scale);
    let mut image = image::load_from_memory(bytes).log_err()?.into_rgba8();
    let (width, height) = image.dimensions();
    let resize_scale = target_scale / source_scale;
    if (resize_scale - 1.0).abs() > f32::EPSILON {
        let scaled_width = ((width as f32) * resize_scale).round().max(1.0) as u32;
        let scaled_height = ((height as f32) * resize_scale).round().max(1.0) as u32;
        image = image::imageops::resize(
            &image,
            scaled_width,
            scaled_height,
            image::imageops::FilterType::Lanczos3,
        );
    }
    let (width, height) = image.dimensions();
    let hotspot_x = ((assets.hotspot_1x.0 as f32) * target_scale).round() as u32;
    let hotspot_y = ((assets.hotspot_1x.1 as f32) * target_scale).round() as u32;

    unsafe {
        let mut bitmap_info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            bmiColors: [RGBQUAD::default()],
        };
        let mut bits = std::ptr::null_mut();
        let color_bitmap =
            CreateDIBSection(None, &mut bitmap_info, DIB_RGB_COLORS, &mut bits, None, 0)
                .log_err()?;
        if color_bitmap.is_invalid() || bits.is_null() {
            return None;
        }

        let dest = std::slice::from_raw_parts_mut(bits.cast::<u8>(), (width * height * 4) as usize);
        for (src, dst) in image.as_raw().chunks_exact(4).zip(dest.chunks_exact_mut(4)) {
            let r = src[0] as u16;
            let g = src[1] as u16;
            let b = src[2] as u16;
            let a = src[3] as u16;
            dst[0] = ((b * a + 127) / 255) as u8;
            dst[1] = ((g * a + 127) / 255) as u8;
            dst[2] = ((r * a + 127) / 255) as u8;
            dst[3] = a as u8;
        }

        let mask_bitmap = CreateBitmap(width as i32, height as i32, 1, 1, None);
        if mask_bitmap.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(color_bitmap.0));
            return None;
        }

        let icon_info = ICONINFO {
            fIcon: BOOL(0),
            xHotspot: hotspot_x.min(width.saturating_sub(1)),
            yHotspot: hotspot_y.min(height.saturating_sub(1)),
            hbmMask: mask_bitmap,
            hbmColor: color_bitmap,
        };
        let icon = CreateIconIndirect(&icon_info).log_err();
        let _ = DeleteObject(HGDIOBJ(color_bitmap.0));
        let _ = DeleteObject(HGDIOBJ(mask_bitmap.0));
        icon.map(|icon| HCURSOR(icon.0))
            .filter(|cursor| !cursor.is_invalid())
    }
}

/// This function is used to configure the dark mode for the window built-in title bar.
pub(crate) fn configure_dwm_dark_mode(hwnd: HWND, appearance: WindowAppearance) {
    let dark_mode_enabled: BOOL = match appearance {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => true.into(),
        WindowAppearance::Light | WindowAppearance::VibrantLight => false.into(),
    };
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark_mode_enabled as *const _ as _,
            std::mem::size_of::<BOOL>() as u32,
        )
        .log_err();
    }
}

#[inline]
pub(crate) fn logical_point(x: f32, y: f32, scale_factor: f32) -> Point<Pixels> {
    Point {
        x: px(x / scale_factor),
        y: px(y / scale_factor),
    }
}

// https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/apply-windows-themes
#[inline]
pub(crate) fn system_appearance() -> Result<WindowAppearance> {
    let ui_settings = UISettings::new()?;
    let foreground_color = ui_settings.GetColorValue(UIColorType::Foreground)?;
    // If the foreground is light, then is_color_light will evaluate to true,
    // meaning Dark mode is enabled.
    if is_color_light(&foreground_color) {
        Ok(WindowAppearance::Dark)
    } else {
        Ok(WindowAppearance::Light)
    }
}

#[inline(always)]
fn is_color_light(color: &Color) -> bool {
    ((5 * color.G as u32) + (2 * color.R as u32) + color.B as u32) > (8 * 128)
}

pub(crate) fn with_dll_library<R, F>(dll_name: PCSTR, f: F) -> Result<R>
where
    F: FnOnce(HMODULE) -> Result<R>,
{
    let library = unsafe {
        LoadLibraryA(dll_name).with_context(|| format!("Loading dll: {}", dll_name.display()))?
    };
    let result = f(library);
    unsafe {
        FreeLibrary(library)
            .with_context(|| format!("Freeing dll: {}", dll_name.display()))
            .log_err();
    }
    result
}
