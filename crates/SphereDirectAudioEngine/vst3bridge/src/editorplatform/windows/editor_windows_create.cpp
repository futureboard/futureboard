#include "editor_windows_internal.hpp"

#include <cstdio>
#include <cstdlib>
#include <mutex>

namespace daux_editor_windows {

void register_classes() {
  static std::once_flag once;
  std::call_once(once, [] {
    WNDCLASSEXW top{};
    top.cbSize = sizeof(top);
    top.lpfnWndProc = top_proc;
    top.hInstance = GetModuleHandleW(nullptr);
    top.hCursor = LoadCursorW(nullptr, MAKEINTRESOURCEW(32512));
    top.hbrBackground = nullptr;
    top.lpszClassName = kTopClass;
    RegisterClassExW(&top);

    WNDCLASSEXW content{};
    content.cbSize = sizeof(content);
    content.lpfnWndProc = content_proc;
    content.hInstance = GetModuleHandleW(nullptr);
    content.hCursor = LoadCursorW(nullptr, MAKEINTRESOURCEW(32512));
    content.hbrBackground = nullptr;
    content.lpszClassName = kContentClass;
    RegisterClassExW(&content);
  });
}

const char *renderer_name() {
  const char *renderer = std::getenv("FUTUREBOARD_EDITOR_RENDERER");
  if (!renderer || !*renderer)
    return "gdi_dwrite";
  if (_stricmp(renderer, "gdi") == 0)
    return "gdi";
  if (_stricmp(renderer, "gdi_dwrite") == 0)
    return "gdi_dwrite";
  if (_stricmp(renderer, "dx11_dwrite") == 0)
    return "dx11_dwrite";
  return "gdi_dwrite";
}

HWND create_content(HWND top, int w, int h, int y) {
  if (!top || !IsWindow(top))
    return nullptr;
  return CreateWindowExW(WS_EX_NOPARENTNOTIFY, kContentClass, L"",
                         WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS |
                             WS_CLIPCHILDREN,
                         0, y, w > 0 ? w : 640, h > 0 ? h : 480, top, nullptr,
                         GetModuleHandleW(nullptr), nullptr);
}

HWND create_top(const DauxEditorWindowConfig &cfg, Context *ctx) {
  // `ChildHwndEmbed` makes the shell a `WS_CHILD`, so the HWND the host passed
  // is its *parent* and has to be used exactly as given: a docked editor hands
  // us a child window already positioned over its panel, and walking up to the
  // root would reparent the plug-in onto the whole application window at (0,0),
  // where the host's own renderer paints over it.
  //
  // The popup kinds still normalize: Win32 resolves a popup's owner to that
  // window's root anyway, so passing a child there is what needs correcting.
  HWND raw_owner = hwnd(cfg.owner_hwnd);
  const bool child_shell =
      cfg.host_kind != static_cast<int>(DauxEditorKind::OwnedToolWindow) &&
      cfg.host_kind != static_cast<int>(DauxEditorKind::DetachedNativeWindow);
  HWND parent = child_shell
                    ? ((raw_owner && IsWindow(raw_owner)) ? raw_owner : nullptr)
                    : normalize_owner_hwnd(raw_owner);
  DWORD style = WS_CLIPCHILDREN | WS_CLIPSIBLINGS;
  DWORD ex_style = WS_EX_NOPARENTNOTIFY;
  HWND owner = nullptr;
  if (cfg.host_kind == static_cast<int>(DauxEditorKind::DetachedNativeWindow)) {
    style |= WS_POPUP | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX;
    ex_style |= WS_EX_TOOLWINDOW;
    owner = (parent && IsWindow(parent)) ? parent : nullptr;
  } else if (cfg.host_kind ==
             static_cast<int>(DauxEditorKind::OwnedToolWindow)) {
    style |= WS_POPUP;
    ex_style |= WS_EX_TOOLWINDOW;
    owner = parent;
  } else {
    style |= WS_CHILD | WS_VISIBLE;
    owner = parent;
  }

  RECT r{0, 0, cfg.content_width > 0 ? cfg.content_width : 640,
         cfg.content_height > 0 ? cfg.content_height : 480};
  if (cfg.host_kind == static_cast<int>(DauxEditorKind::DetachedNativeWindow)) {
    r.bottom += MulDiv(kTitlebarLogicalH,
                       static_cast<int>(parent ? dpi(parent) : 96), 96);
  } else if (parent && IsWindow(parent)) {
    if (!AdjustWindowRectExForDpi(&r, style, FALSE, ex_style, dpi(parent))) {
      AdjustWindowRectEx(&r, style, FALSE, ex_style);
    }
  } else {
    AdjustWindowRectEx(&r, style, FALSE, ex_style);
  }

  int x = cfg.x;
  int y = cfg.y;
  if (cfg.host_kind == static_cast<int>(DauxEditorKind::DetachedNativeWindow)) {
    x = CW_USEDEFAULT;
    y = CW_USEDEFAULT;
  }
  if (cfg.host_kind == static_cast<int>(DauxEditorKind::OwnedToolWindow) &&
      parent && IsWindow(parent)) {
    RECT screen{};
    if (content_screen_rect(parent, cfg.x, cfg.y, cfg.content_width,
                            cfg.content_height, &screen)) {
      x = screen.left;
      y = screen.top;
    }
  } else if (cfg.host_kind ==
                 static_cast<int>(DauxEditorKind::DetachedNativeWindow) &&
             parent && IsWindow(parent)) {
    RECT pr{};
    if (GetWindowRect(parent, &pr)) {
      x = pr.left + 48;
      y = pr.top + 48;
    }
  }

  HWND top =
      CreateWindowExW(ex_style, kTopClass,
                      cfg.title && *cfg.title ? cfg.title : L"Plugin Editor",
                      style, x, y, r.right - r.left, r.bottom - r.top, owner,
                      nullptr, GetModuleHandleW(nullptr), ctx);
  if (top) {
    std::fprintf(stderr, "[NativeEditorShell] backend=cpp_shell\n");
    std::fprintf(stderr, "[NativeEditorShell] create hwnd=0x%p\n", ptr(top));
    std::fprintf(stderr, "[NativeEditorShell] style=0x%08lx exstyle=0x%08lx\n",
                 static_cast<unsigned long>(style),
                 static_cast<unsigned long>(ex_style));
    std::fprintf(stderr, "[NativeEditorShell] renderer=%s%s\n", renderer_name(),
                 _stricmp(renderer_name(), "dx11_dwrite") == 0 ? "|fallback=gdi"
                                                               : "");
    std::fprintf(
        stderr,
        "[NativeEditorShell] dwrite=forced gdi_fallback=true d2d=false\n");
    set_dark_titlebar(top);
    if (cfg.host_kind == static_cast<int>(DauxEditorKind::OwnedToolWindow)) {
      daux_editor_apply_tool_styles(ptr(top), cfg.owner_hwnd);
    }
    if (cfg.host_kind ==
        static_cast<int>(DauxEditorKind::DetachedNativeWindow)) {
      apply_owned_popup_styles(top, parent);
    }
  }
  return top;
}

} // namespace daux_editor_windows
