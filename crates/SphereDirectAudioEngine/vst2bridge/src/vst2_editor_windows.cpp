// Windows editor hosting for VST2 plug-ins.
//
// Reuses the shared native editor shell (`daux_editor_*` from
// vst3bridge/editor_windows.hpp) — titlebar, DPI handling, loading overlay,
// pin-to-top, and the bare-Space transport claim all come from there. Only the
// attach itself is VST2-specific: `effEditGetRect` for the preferred size and
// `effEditOpen(content_hwnd)` to hand the plug-in its parent window.
//
// The plug-in creates and owns its own child window inside `content_hwnd`, so
// this file never registers a window class of its own.

#if !defined(_WIN32)
#error "vst2_editor_windows.cpp is Windows-only; other platforms use the mac/stub TU"
#endif

#include "vst2_processor_internal.hpp"

#include <objbase.h>

namespace {

bool vst2_embed_debug() {
  static const bool enabled =
      daux_vst2_debug() ||
      std::getenv("FUTUREBOARD_PLUGIN_EMBED_DEBUG") != nullptr;
  return enabled;
}

std::wstring widen(const char *value) {
  if (!value || !*value)
    return {};
  const int needed = MultiByteToWideChar(CP_UTF8, 0, value, -1, nullptr, 0);
  if (needed <= 0)
    return {};
  std::wstring out(static_cast<size_t>(needed - 1), L'\0');
  MultiByteToWideChar(CP_UTF8, 0, value, -1, out.data(), needed);
  return out;
}

/// COM (STA) on the editor thread. Some VST2 editors create COM controls in
/// effEditOpen and fail without it. Idempotent.
void ensure_com_initialized() {
  static thread_local bool done = false;
  if (done)
    return;
  const HRESULT hr = CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);
  if (hr == RPC_E_CHANGED_MODE) {
    // Another apartment model already owns this thread; that is fine.
  }
  done = true;
}

// ── Shell callbacks ─────────────────────────────────────────────────────────

SphereDauxVst2Processor *from_user_data(void *user_data) {
  return static_cast<SphereDauxVst2Processor *>(user_data);
}

bool cb_is_live(void *user_data) {
  auto *p = from_user_data(user_data);
  return p && p->processor_valid.load(std::memory_order_acquire);
}

bool cb_is_attached(void *user_data) {
  auto *p = from_user_data(user_data);
  return p && p->editor_attached;
}

bool cb_is_resize_in_progress(void *user_data) {
  auto *p = from_user_data(user_data);
  return p && p->embed_resize_in_progress;
}

void cb_set_resize_in_progress(void *user_data, bool value) {
  if (auto *p = from_user_data(user_data))
    p->embed_resize_in_progress = value;
}

bool cb_can_resize(void *user_data) {
  auto *p = from_user_data(user_data);
  return p && p->editor_resizable;
}

/// A VST2 editor has no `checkSizeConstraint` equivalent. A resizable editor
/// takes whatever the user drags to; a fixed-size one is pinned to the size
/// `effEditGetRect` reported.
bool cb_constrain_content_size(void *user_data, int *width, int *height) {
  auto *p = from_user_data(user_data);
  if (!p || !width || !height)
    return false;
  if (p->editor_resizable)
    return true;
  if (p->embed_content_w > 0 && p->embed_content_h > 0) {
    *width = p->embed_content_w;
    *height = p->embed_content_h;
    return true;
  }
  return false;
}

void cb_on_content_resized(void *user_data, void *content_hwnd, int width,
                           int height) {
  auto *p = from_user_data(user_data);
  if (!p || width <= 0 || height <= 0)
    return;
  p->embed_content_w = width;
  p->embed_content_h = height;
  auto content = reinterpret_cast<HWND>(content_hwnd);
  if (!content || !IsWindow(content))
    return;
  // Resize the plug-in's own child to fill the content area. VST2 has no
  // "host told me to resize" opcode, so this is all the notification a
  // resizable editor gets.
  if (HWND child = GetWindow(content, GW_CHILD)) {
    SetWindowPos(child, nullptr, 0, 0, width, height,
                 SWP_NOZORDER | SWP_NOACTIVATE);
  }
}

void cb_on_dpi_changed(void *user_data, void *shell_hwnd, void *content_hwnd,
                       int width, int height) {
  (void)shell_hwnd;
  cb_on_content_resized(user_data, content_hwnd, width, height);
}

void cb_on_close_requested(void *user_data) {
  if (auto *p = from_user_data(user_data))
    p->embed_user_closed.store(true, std::memory_order_release);
}

DauxEditorWindowCallbacks make_callbacks(SphereDauxVst2Processor *p) {
  DauxEditorWindowCallbacks callbacks{};
  callbacks.user_data = p;
  callbacks.is_live = cb_is_live;
  callbacks.is_attached = cb_is_attached;
  callbacks.is_resize_in_progress = cb_is_resize_in_progress;
  callbacks.set_resize_in_progress = cb_set_resize_in_progress;
  callbacks.can_resize = cb_can_resize;
  callbacks.constrain_content_size = cb_constrain_content_size;
  callbacks.on_content_resized = cb_on_content_resized;
  callbacks.on_dpi_changed = cb_on_dpi_changed;
  callbacks.on_close_requested = cb_on_close_requested;
  return callbacks;
}

// ── Geometry ────────────────────────────────────────────────────────────────

/// Keep the editor shell aligned with the GPUI-provided host region.
/// Mirrors the VST3 bridge's `daux_embed_sync_geometry`; the two cannot share
/// an implementation because each owns its own processor struct.
bool sync_geometry(SphereDauxVst2Processor *p, int x, int y, int w, int h) {
  HWND top = p ? p->editor_embed_top_hwnd : nullptr;
  if (!p || !top || !IsWindow(top))
    return false;
  // Detached: a standalone OS window owns its own position and size.
  if (p->embed_host_kind == 2)
    return false;

  p->embed_host_x = x;
  p->embed_host_y = y;
  p->embed_host_w = w;
  p->embed_host_h = h;

  if (p->embed_host_kind == 1 && p->editor_parent_hwnd) {
    if (!IsWindow(p->editor_parent_hwnd))
      return false;
    const bool parent_visible = IsWindowVisible(p->editor_parent_hwnd) &&
                                !IsIconic(p->editor_parent_hwnd);
    ShowWindow(top, parent_visible ? SW_SHOWNA : SW_HIDE);
    RECT screen{};
    if (!daux_editor_content_screen_rect(p->editor_parent_hwnd, x, y, w, h,
                                         &screen.left, &screen.top,
                                         &screen.right, &screen.bottom)) {
      return false;
    }
    if (p->embed_geometry_valid && EqualRect(&screen, &p->embed_last_applied))
      return false;
    p->embed_last_applied = screen;
    p->embed_geometry_valid = true;
    SetWindowPos(top, p->editor_parent_hwnd, screen.left, screen.top,
                 screen.right - screen.left, screen.bottom - screen.top,
                 SWP_NOACTIVATE | SWP_SHOWWINDOW);
    daux_editor_apply_tool_styles(top, p->editor_parent_hwnd);
  } else {
    RECT want{x, y, x + w, y + h};
    if (p->embed_geometry_valid && EqualRect(&want, &p->embed_last_applied))
      return false;
    p->embed_last_applied = want;
    p->embed_geometry_valid = true;
    SetWindowPos(top, HWND_TOP, x, y, w, h, SWP_SHOWWINDOW | SWP_NOACTIVATE);
  }

  EnableWindow(top, TRUE);
  if (p->editor_attach_hwnd && IsWindow(p->editor_attach_hwnd)) {
    RECT rc{};
    GetClientRect(p->editor_attach_hwnd, &rc);
    if (HWND child = GetWindow(p->editor_attach_hwnd, GW_CHILD)) {
      SetWindowPos(child, nullptr, 0, 0, rc.right - rc.left,
                   rc.bottom - rc.top, SWP_NOZORDER | SWP_NOACTIVATE);
    }
  }
  daux_editor_raise_children(top);
  return true;
}

/// Open the plug-in's editor into `content`, sizing the shell to the size the
/// plug-in reports. Returns false and leaves the shell untouched on failure.
bool attach_editor(SphereDauxVst2Processor *p, HWND content) {
  if (!p || !p->effect || !content || !IsWindow(content))
    return false;

  const auto result = p->dispatch(effEditOpen, 0, 0, content);
  HWND child = GetWindow(content, GW_CHILD);
  // Plug-ins are inconsistent here: some return 0 on success, and some create
  // their child window only on the first effEditIdle rather than inside
  // effEditOpen. Failing on "no child yet" would lose those editors entirely,
  // so only a plug-in that both declined AND created nothing counts as failed —
  // the host's ready probe polls `embed_has_visible_ui` for the rest.
  if (result == 0 && !child) {
    if (vst2_embed_debug()) {
      std::fprintf(stderr,
                   "[vst2-editor] effEditOpen declined and created no child\n");
    }
    p->dispatch(effEditClose);
    return false;
  }

  // Re-query the rect: many plug-ins only fill it once the editor is open.
  int width = p->embed_content_w;
  int height = p->embed_content_h;
  ERect *rect = nullptr;
  p->dispatch(effEditGetRect, 0, 0, &rect);
  if (rect) {
    const int w = rect->right - rect->left;
    const int h = rect->bottom - rect->top;
    if (w > 0 && h > 0) {
      width = w;
      height = h;
    }
  }
  if (width > 0 && height > 0) {
    p->embed_content_w = width;
    p->embed_content_h = height;
    daux_editor_resize_content(&p->editor_window, width, height);
    // Re-read: a plug-in that deferred creation may have made its child during
    // effEditGetRect, and the content HWND was just resized.
    if (!child) {
      child = GetWindow(content, GW_CHILD);
    }
    if (child) {
      RECT rc{};
      GetClientRect(content, &rc);
      SetWindowPos(child, nullptr, 0, 0, rc.right - rc.left, rc.bottom - rc.top,
                   SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW);
    }
  }

  p->editor_attached = true;
  p->editor_attach_hwnd = content;
  daux_editor_set_load_state(&p->editor_window, false, nullptr);
  if (child) {
    ShowWindow(child, SW_SHOW);
  }

  std::fprintf(stderr,
               "[vst2-editor] attached instance=%s size=%dx%d content=0x%p "
               "child=0x%p\n",
               p->embed_instance_label.empty() ? "<unknown>"
                                               : p->embed_instance_label.c_str(),
               width, height, static_cast<void *>(content),
               static_cast<void *>(child));
  return true;
}

/// Create the shell and attach. Shared by the embedded and standalone paths.
unsigned long long open_shell(SphereDauxVst2Processor *p, HWND owner, int kind,
                              int x, int y, int width, int height) {
  ensure_com_initialized();

  if (!p || !p->effect) {
    vst2_set_last_error("VST2 editor: no plug-in instance");
    return 0;
  }
  if (!p->has_editor) {
    vst2_set_last_error("VST2 plug-in has no editor");
    return 0;
  }

  // Preferred size up front so the shell opens at the right size instead of
  // resizing visibly after attach.
  int editor_w = width > 0 ? width : 640;
  int editor_h = height > 0 ? height : 480;
  ERect *rect = nullptr;
  p->dispatch(effEditGetRect, 0, 0, &rect);
  if (rect) {
    const int w = rect->right - rect->left;
    const int h = rect->bottom - rect->top;
    if (w > 0 && h > 0) {
      editor_w = w;
      editor_h = h;
    }
  }
  p->embed_content_w = editor_w;
  p->embed_content_h = editor_h;

  const std::wstring title_w = p->editor_title.empty()
                                   ? std::wstring(L"Unknown Plugin")
                                   : widen(p->editor_title.c_str());

  DauxEditorWindowConfig config{};
  config.owner_hwnd = owner;
  config.title = title_w.c_str();
  config.plugin_name = title_w.c_str();
  config.host_kind = kind;
  config.x = x;
  config.y = y;
  config.content_width = editor_w;
  config.content_height = editor_h;
  config.pin_default = daux_editor_env_truthy("FUTUREBOARD_EDITOR_PIN_DEFAULT");
  config.callbacks = make_callbacks(p);

  // Reclaim a stale shell from a previous failed open before overwriting it.
  // DestroyWindow is only valid on the creating thread.
  if (daux_editor_is_window_valid(&p->editor_window)) {
    HWND stale = reinterpret_cast<HWND>(p->editor_window.shell_hwnd);
    if (stale &&
        GetWindowThreadProcessId(stale, nullptr) == GetCurrentThreadId()) {
      daux_editor_destroy_window(&p->editor_window);
    }
  }
  p->editor_window = DauxEditorWindow{};

  if (!daux_editor_create_window(&config, &p->editor_window)) {
    vst2_set_last_error("VST2 editor: native editor window creation failed");
    return 0;
  }
  HWND top = reinterpret_cast<HWND>(p->editor_window.shell_hwnd);
  HWND content = reinterpret_cast<HWND>(p->editor_window.content_hwnd);
  if (!top || !content) {
    daux_editor_destroy_window(&p->editor_window);
    p->editor_window = DauxEditorWindow{};
    vst2_set_last_error("VST2 editor: shell returned invalid HWNDs");
    return 0;
  }

  p->editor_embed_top_hwnd = top;
  p->embed_host_kind = kind;
  p->embed_user_closed.store(false, std::memory_order_release);
  p->embed_geometry_valid = false;

  if (!attach_editor(p, content)) {
    const std::wstring message = L"Editor failed to open";
    daux_editor_set_load_state(&p->editor_window, true, message.c_str());
    vst2_set_last_error("VST2 editor: effEditOpen did not create a view");
    return 0;
  }

  p->editor_handle = vst2_next_editor_handle();
  daux_editor_show_and_focus(&p->editor_window);
  return p->editor_handle;
}

} // namespace

// ── Processor member teardown ───────────────────────────────────────────────

void SphereDauxVst2Processor::close_embed_editor(const char *reason) {
  if (!editor_attached && !daux_editor_is_window_valid(&editor_window))
    return;

  if (editor_attached && effect) {
    dispatch(effEditClose);
    editor_attached = false;
  }
  editor_attach_hwnd = nullptr;

  if (daux_editor_is_window_valid(&editor_window)) {
    HWND shell = reinterpret_cast<HWND>(editor_window.shell_hwnd);
    // DestroyWindow is only legal on the creating thread; a cross-thread call
    // is a silent no-op in Win32, so skip it rather than issue an invalid one.
    if (!shell ||
        GetWindowThreadProcessId(shell, nullptr) == GetCurrentThreadId()) {
      daux_editor_destroy_window(&editor_window);
    }
  }
  editor_window = DauxEditorWindow{};
  editor_embed_top_hwnd = nullptr;
  editor_parent_hwnd = nullptr;
  embed_mode = false;
  embed_geometry_valid = false;
  embed_last_applied = RECT{};
  editor_handle = 0;

  if (vst2_embed_debug()) {
    std::fprintf(stderr, "[vst2-editor] detached reason=%s instance=%s\n",
                 reason ? reason : "unspecified",
                 embed_instance_label.empty() ? "<unknown>"
                                              : embed_instance_label.c_str());
  }
}

void SphereDauxVst2Processor::close_editor_window() {
  close_embed_editor("shutdown");
}

// ── C API ───────────────────────────────────────────────────────────────────

extern "C" {

unsigned long long sphere_daux_vst2_embed_editor(SphereDauxVst2Processor *p,
                                                 unsigned long long parent,
                                                 int x, int y, int width,
                                                 int height) {
  HWND parent_hwnd = reinterpret_cast<HWND>(static_cast<std::uintptr_t>(parent));
  if (!p || !parent_hwnd || !IsWindow(parent_hwnd) || width <= 0 ||
      height <= 0) {
    vst2_set_last_error("VST2 embed editor: invalid parent HWND or region");
    return 0;
  }

  // Reuse: an already-attached editor only needs its geometry re-synced.
  if (p->embed_mode && p->editor_attached && p->editor_embed_top_hwnd &&
      IsWindow(p->editor_embed_top_hwnd)) {
    p->editor_parent_hwnd = parent_hwnd;
    if (p->embed_host_kind == 2) {
      daux_editor_apply_owner(&p->editor_window, parent_hwnd);
      daux_editor_show_and_focus(&p->editor_window);
    }
    p->embed_geometry_valid = false;
    sync_geometry(p, x, y, width, height);
    return p->editor_handle;
  }

  const int kind = daux_editor_resolve_host_kind();
  p->editor_parent_hwnd = parent_hwnd;
  p->embed_mode = true;
  const auto handle = open_shell(p, parent_hwnd, kind, x, y, width, height);
  if (handle == 0) {
    p->embed_mode = false;
    return 0;
  }
  sync_geometry(p, x, y, width, height);
  return handle;
}

void sphere_daux_vst2_embed_set_bounds(SphereDauxVst2Processor *p, int x, int y,
                                       int width, int height) {
  if (!p || width <= 0 || height <= 0)
    return;
  sync_geometry(p, x, y, width, height);
}

void sphere_daux_vst2_embed_refresh(SphereDauxVst2Processor *p) {
  if (!p || !p->embed_mode || !p->editor_embed_top_hwnd)
    return;
  // Cheap per-frame poll: re-apply the last host region so a parent-window move
  // is tracked. sync_geometry no-ops when the resulting screen rect is
  // unchanged.
  p->embed_geometry_valid = false;
  sync_geometry(p, p->embed_host_x, p->embed_host_y, p->embed_host_w,
                p->embed_host_h);
  // VST2 editors need periodic idle to repaint and run their own animation.
  if (p->editor_attached)
    p->dispatch(effEditIdle);
}

unsigned long long
sphere_daux_vst2_embed_attach_hwnd(SphereDauxVst2Processor *p) {
  if (!p || !p->editor_attach_hwnd)
    return 0;
  return static_cast<unsigned long long>(
      reinterpret_cast<std::uintptr_t>(p->editor_attach_hwnd));
}

void sphere_daux_vst2_embed_detach(SphereDauxVst2Processor *p) {
  if (!p)
    return;
  p->close_embed_editor("embed_detach");
}

int sphere_daux_vst2_embed_is_valid(SphereDauxVst2Processor *p) {
  if (!p || !p->embed_mode || !p->editor_attached)
    return 0;
  return (p->editor_embed_top_hwnd && IsWindow(p->editor_embed_top_hwnd)) ? 1
                                                                          : 0;
}

int sphere_daux_vst2_embed_has_visible_ui(SphereDauxVst2Processor *p) {
  if (!p || !p->editor_attach_hwnd || !IsWindow(p->editor_attach_hwnd))
    return 0;
  HWND child = GetWindow(p->editor_attach_hwnd, GW_CHILD);
  return (child && IsWindowVisible(child)) ? 1 : 0;
}

unsigned long long sphere_daux_vst2_open_editor(SphereDauxVst2Processor *p,
                                                const char *window_id,
                                                const char *title, int width,
                                                int height) {
  if (!p)
    return 0;
  p->editor_window_id = window_id ? window_id : "";
  if (title && *title)
    p->editor_title = title;
  if (p->editor_attached && p->editor_embed_top_hwnd &&
      IsWindow(p->editor_embed_top_hwnd)) {
    daux_editor_show_and_focus(&p->editor_window);
    return p->editor_handle;
  }
  p->embed_mode = false;
  // Standalone editor: a detached top-level window the user owns.
  return open_shell(p, nullptr, 2, 0, 0, width, height);
}

void sphere_daux_vst2_close_editor(SphereDauxVst2Processor *p) {
  if (!p)
    return;
  p->close_embed_editor("close_editor");
}

int sphere_daux_vst2_focus_editor(SphereDauxVst2Processor *p) {
  if (!p || !daux_editor_is_window_valid(&p->editor_window))
    return 0;
  return daux_editor_show_and_focus(&p->editor_window) ? 1 : 0;
}

} // extern "C"
