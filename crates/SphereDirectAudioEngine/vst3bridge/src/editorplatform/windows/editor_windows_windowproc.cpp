#include "editor_windows_internal.hpp"

#include <algorithm>
#include <cstdio>

// Process-wide transport claim (same counter the host IPC loop drains).
extern "C" void sphere_daux_vst3_claim_transport_toggle(void);

namespace daux_editor_windows {

LRESULT hit_test(HWND h, Context *c, LPARAM lp) {
  POINT pt{static_cast<int>(static_cast<short>(LOWORD(lp))),
           static_cast<int>(static_cast<short>(HIWORD(lp)))};
  ScreenToClient(h, &pt);
  RECT rc{};
  GetClientRect(h, &rc);
  const int cw = rc.right;
  const int ch = rc.bottom;
  if (can_resize(c) && !IsZoomed(h)) {
    const int grab = std::max<int>(4, MulDiv(6, static_cast<int>(dpi(h)), 96));
    const bool l = pt.x < grab, r = pt.x >= cw - grab;
    const bool t = pt.y < grab, b = pt.y >= ch - grab;
    if (t && l)
      return HTTOPLEFT;
    if (t && r)
      return HTTOPRIGHT;
    if (b && l)
      return HTBOTTOMLEFT;
    if (b && r)
      return HTBOTTOMRIGHT;
    if (l)
      return HTLEFT;
    if (r)
      return HTRIGHT;
    if (t)
      return HTTOP;
    if (b)
      return HTBOTTOM;
  }
  const int th = titlebar_h(h);
  if (pt.y >= 0 && pt.y < th) {
    if (button_at(h, pt.x, pt.y) != kBtnNone)
      return HTCLIENT;
    return HTCAPTION;
  }
  return HTCLIENT;
}

void resize_content(HWND h, Context *c) {
  HWND content = hwnd(c->window.content_hwnd);
  if (!content || !IsWindow(content) || resizing(c))
    return;
  RECT rc{};
  GetClientRect(h, &rc);
  const int th = c->window.host_kind ==
                         static_cast<int>(DauxEditorKind::DetachedNativeWindow)
                     ? titlebar_h(h)
                     : 0;
  const int w = std::max<LONG>(0, rc.right - rc.left);
  const int ht = std::max<LONG>(0, (rc.bottom - rc.top) - th);
  if (w <= 0 || ht <= 0)
    return;
  set_resizing(c, true);
  SetWindowPos(content, nullptr, 0, th, w, ht,
               SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW);
  set_resizing(c, false);
  c->window.content_width = w;
  c->window.content_height = ht;
  std::fprintf(stderr, "[plugin-view] resize top=(%d,%d) content=(%d,%d)\n", w,
               ht, w, ht);
  if (attached(c) && c->cb.on_content_resized)
    c->cb.on_content_resized(c->cb.user_data, ptr(content), w, ht);
}

bool handle_sizing(HWND h, Context *c, WPARAM wp, LPARAM lp, bool borderless) {
  if (!attached(c) || !lp || !can_resize(c))
    return false;
  RECT *drag = reinterpret_cast<RECT *>(lp);
  int nc_w = 0;
  int nc_h = 0;
  if (borderless) {
    nc_h = titlebar_h(h);
  } else {
    RECT frame{0, 0, 0, 0};
    const DWORD style = static_cast<DWORD>(GetWindowLongPtrW(h, GWL_STYLE));
    const DWORD ex_style =
        static_cast<DWORD>(GetWindowLongPtrW(h, GWL_EXSTYLE));
    if (!AdjustWindowRectExForDpi(&frame, style, FALSE, ex_style, dpi(h))) {
      AdjustWindowRectEx(&frame, style, FALSE, ex_style);
    }
    nc_w = static_cast<int>(frame.right - frame.left);
    nc_h = static_cast<int>(frame.bottom - frame.top);
  }
  int w = static_cast<int>(drag->right - drag->left) - nc_w;
  int ht = static_cast<int>(drag->bottom - drag->top) - nc_h;
  if (w <= 0 || ht <= 0 || !constrain(c, &w, &ht))
    return false;
  const int ow = w + nc_w;
  const int oh = ht + nc_h;
  switch (wp) {
  case WMSZ_LEFT:
  case WMSZ_TOPLEFT:
  case WMSZ_BOTTOMLEFT:
    drag->left = drag->right - ow;
    break;
  default:
    drag->right = drag->left + ow;
    break;
  }
  switch (wp) {
  case WMSZ_TOP:
  case WMSZ_TOPLEFT:
  case WMSZ_TOPRIGHT:
    drag->top = drag->bottom - oh;
    break;
  default:
    drag->bottom = drag->top + oh;
    break;
  }
  return true;
}

LRESULT CALLBACK content_proc(HWND h, UINT msg, WPARAM wp, LPARAM lp) {
  log_message("plugin-content-hwnd", h, msg);
  switch (msg) {
  case WM_TIMER:
    if (wp == kWakeTimerTop || wp == kWakeTimerContent) {
      KillTimer(h, wp);
      return 0;
    }
    break;
  case WM_KEYDOWN:
  case WM_SYSKEYDOWN:
    // Bare Space → DAW transport. Claims before DefWindowProc / plugin child
    // can consume it when focus is on our content surface.
    if (wp == VK_SPACE && (lp & (1u << 30)) == 0) {
      if ((GetKeyState(VK_CONTROL) >= 0) && (GetKeyState(VK_MENU) >= 0) &&
          (GetKeyState(VK_LWIN) >= 0) && (GetKeyState(VK_RWIN) >= 0)) {
        sphere_daux_vst3_claim_transport_toggle();
        return 0;
      }
    }
    break;
  case WM_ERASEBKGND:
    return 1; // fully repainted in WM_PAINT (no flicker)
  case WM_PAINT: {
    auto *c = reinterpret_cast<Context *>(GetWindowLongPtrW(h, GWLP_USERDATA));
    paint_content_overlay(h, c);
    return 0;
  }
  case WM_MOUSEACTIVATE:
    return MA_ACTIVATE;
  case WM_LBUTTONDOWN: {
    const POINT pt{static_cast<short>(LOWORD(lp)),
                   static_cast<short>(HIWORD(lp))};
    HWND target = ChildWindowFromPointEx(
        h, pt, CWP_SKIPINVISIBLE | CWP_SKIPDISABLED | CWP_SKIPTRANSPARENT);
    SetFocus(target ? target : h);
    break;
  }
  default:
    break;
  }
  return DefWindowProcW(h, msg, wp, lp);
}

LRESULT CALLBACK top_proc(HWND h, UINT msg, WPARAM wp, LPARAM lp) {
  auto *c = reinterpret_cast<Context *>(GetWindowLongPtrW(h, GWLP_USERDATA));
  const LONG_PTR style = GetWindowLongPtrW(h, GWL_STYLE);
  const bool borderless = (style & WS_THICKFRAME) && !(style & WS_CAPTION);
  const bool ok = live(c);
  const bool detached =
      ok && c &&
      c->window.host_kind ==
          static_cast<int>(DauxEditorKind::DetachedNativeWindow);
  log_message("plugin-top-hwnd", h, msg);
  switch (msg) {
  case WM_NCCREATE: {
    auto *create = reinterpret_cast<CREATESTRUCTW *>(lp);
    c = reinterpret_cast<Context *>(create ? create->lpCreateParams : nullptr);
    SetWindowLongPtrW(h, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(c));
    if (c)
      c->window.shell_hwnd = ptr(h);
    return TRUE;
  }
  case WM_KEYDOWN:
  case WM_SYSKEYDOWN:
    if (wp == VK_SPACE && (lp & (1u << 30)) == 0) {
      if ((GetKeyState(VK_CONTROL) >= 0) && (GetKeyState(VK_MENU) >= 0) &&
          (GetKeyState(VK_LWIN) >= 0) && (GetKeyState(VK_RWIN) >= 0)) {
        sphere_daux_vst3_claim_transport_toggle();
        return 0;
      }
    }
    break;
  case WM_NCCALCSIZE:
    if (wp && borderless)
      return 0;
    break;
  case WM_NCACTIVATE:
    if (borderless) {
      invalidate_titlebar(h);
      return 1;
    }
    break;
  case WM_NCHITTEST:
    if (detached)
      return hit_test(h, c, lp);
    break;
  case WM_LBUTTONDOWN:
    if (detached) {
      const int x = static_cast<int>(static_cast<short>(LOWORD(lp)));
      const int y = static_cast<int>(static_cast<short>(HIWORD(lp)));
      const int b = button_at(h, x, y);
      if (b != kBtnNone) {
        c->press = b;
        SetCapture(h);
      }
      return 0;
    }
    break;
  case WM_LBUTTONUP:
    if (detached) {
      const int x = static_cast<int>(static_cast<short>(LOWORD(lp)));
      const int y = static_cast<int>(static_cast<short>(HIWORD(lp)));
      const int press = c->press;
      c->press = kBtnNone;
      if (press != kBtnNone) {
        ReleaseCapture();
        if (button_at(h, x, y) == press) {
          if (press == kBtnPin) {
            DauxEditorWindow copy = c->window;
            daux_editor_set_pin_to_top(&copy, !c->window.pinned);
            c->window = copy;
          } else if (press == kBtnMin) {
            ShowWindow(h, SW_MINIMIZE);
          } else if (press == kBtnMax) {
            ShowWindow(h, IsZoomed(h) ? SW_RESTORE : SW_MAXIMIZE);
          } else {
            SendMessageW(h, WM_CLOSE, 0, 0);
          }
        }
      }
      return 0;
    }
    break;
  case WM_MOUSEMOVE:
    if (detached) {
      const int x = static_cast<int>(static_cast<short>(LOWORD(lp)));
      const int y = static_cast<int>(static_cast<short>(HIWORD(lp)));
      const int b = button_at(h, x, y);
      if (b != c->hover) {
        c->hover = b;
        invalidate_titlebar(h);
      }
      if (!c->tracking) {
        TRACKMOUSEEVENT tme{sizeof(TRACKMOUSEEVENT), TME_LEAVE, h, 0};
        TrackMouseEvent(&tme);
        c->tracking = true;
      }
      return 0;
    }
    break;
  case WM_MOUSELEAVE:
    if (detached) {
      c->tracking = false;
      if (c->hover != kBtnNone) {
        c->hover = kBtnNone;
        invalidate_titlebar(h);
      }
      return 0;
    }
    break;
  case WM_ERASEBKGND:
    return 1;
  case WM_TIMER:
    if (wp == kWakeTimerTop || wp == kWakeTimerContent) {
      KillTimer(h, wp);
      return 0;
    }
    break;
  case WM_PAINT:
    if (detached) {
      paint_titlebar(h, c);
      return 0;
    }
    {
      PAINTSTRUCT ps{};
      BeginPaint(h, &ps);
      EndPaint(h, &ps);
      return 0;
    }
  case WM_SIZE:
    if (wp == SIZE_MINIMIZED)
      return 0;
    if (ok && c)
      resize_content(h, c);
    if (detached)
      invalidate_titlebar(h);
    return 0;
  case WM_GETMINMAXINFO:
    if (detached && attached(c) && !resizing(c) && !can_resize(c) && lp) {
      RECT wr{};
      if (GetWindowRect(h, &wr)) {
        auto *mmi = reinterpret_cast<MINMAXINFO *>(lp);
        const POINT size{wr.right - wr.left, wr.bottom - wr.top};
        mmi->ptMinTrackSize = size;
        mmi->ptMaxTrackSize = size;
        mmi->ptMaxSize = size;
        return 0;
      }
    }
    break;
  case WM_SIZING:
    if (detached && handle_sizing(h, c, wp, lp, borderless))
      return TRUE;
    break;
  case WM_DPICHANGED:
    if (ok && c && lp) {
      const unsigned old_dpi = dpi(h);
      const unsigned new_dpi = LOWORD(wp) ? LOWORD(wp) : old_dpi;
      const RECT *suggested = reinterpret_cast<RECT *>(lp);
      // The suggested rect is the shell scaled by new/old DPI. A fixed-size
      // editor pins WM_GETMINMAXINFO to the *current* outer size, which
      // silently rejected this move — the plug-in then rescaled its content
      // for the new DPI inside a window that never grew, and was cropped.
      // Mark the resize as shell-driven so the lock lets the DPI size through.
      set_resizing(c, true);
      SetWindowPos(h, nullptr, suggested->left, suggested->top,
                   suggested->right - suggested->left,
                   suggested->bottom - suggested->top,
                   SWP_NOZORDER | SWP_NOACTIVATE);
      set_resizing(c, false);
      std::fprintf(stderr, "[NativeEditorShell] wm_dpichanged old=%u new=%u\n",
                   old_dpi, new_dpi);
      std::fprintf(stderr, "[PluginEditor] WM_DPICHANGED dpi=%u\n", dpi(h));
      resize_content(h, c);
      invalidate_titlebar(h);
      HWND content = hwnd(c->window.content_hwnd);
      // The callback re-queries IPlugView::getSize at the new content scale
      // and recomputes the windowed size from it; the rect handed over here is
      // only the fallback when the view has no size to report.
      if (content && IsWindow(content) && attached(c) && c->cb.on_dpi_changed) {
        RECT rc{};
        GetClientRect(content, &rc);
        const int w = rc.right - rc.left;
        const int ht = rc.bottom - rc.top;
        if (w > 0 && ht > 0)
          c->cb.on_dpi_changed(c->cb.user_data, ptr(h), ptr(content), w, ht);
      }
      return 0;
    }
    break;
  case WM_CLOSE:
    if (ok && c && c->cb.on_close_requested)
      c->cb.on_close_requested(c->cb.user_data);
    ShowWindow(h, SW_HIDE);
    return 0;
  case WM_MOUSEACTIVATE:
    return MA_ACTIVATE;
  default:
    break;
  }
  return DefWindowProcW(h, msg, wp, lp);
}

} // namespace daux_editor_windows
