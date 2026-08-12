use gpui::{App, Empty, IntoElement, Render, Window};

// ── Section dimensions ─────────────────────────────────────────────────────
pub const STRIP_WIDTH: f32 = 88.0;
/// Minimum height for a channel strip. Below this the mixer should scroll/clip
/// as a whole rather than compressing the pan/fader controls into unusability.
pub const STRIP_MIN_HEIGHT: f32 = 320.0;

pub(crate) const SEC_HEADER_H: f32 = 40.0;
pub(crate) const SEC_SECTION_HEADER_H: f32 = 20.0;
pub(crate) const SEC_PAN_H: f32 = 60.0;
/// Two compact control rows: four channel-state buttons (M/S/R/I) above two
/// wider listen-tap buttons (PFL/AFL). This keeps the long listen labels clear
/// while preserving the narrow 88px mixer strip.
pub(crate) const SEC_BUTTONS_H: f32 = 40.0;
pub(crate) const SEC_FOOTER_H: f32 = 22.0;
pub(crate) const SEC_FADER_MIN_H: f32 = 66.0;
pub(crate) const LOWER_CONTROL_MIN_H: f32 = SEC_PAN_H + SEC_FADER_MIN_H + SEC_BUTTONS_H;

// ── Vertical mixer section resizing ─────────────────────────────────────────
// Inserts and sends each own a fixed-height clipped viewport with their own
// vertical scrolling. Heights are shared across all strips so rows stay aligned
// across the mixer. Splitter actions are routed to `StudioLayout`, which owns
// the shared values and mirrors them into the detached mixer window snapshot.
/// Visual + hitbox height of the splitter handle.
pub(crate) const SEC_SPLITTER_H: f32 = 6.0;
const SECTION_VIEWPORT_MIN_H: f32 = 42.0;
const SECTION_VIEWPORT_MAX_H: f32 = 180.0;
pub const MIXER_INSERT_SECTION_DEFAULT_PX: f32 = 72.0;
pub const MIXER_SEND_SECTION_DEFAULT_PX: f32 = 54.0;

/// Clamp one insert/send section height into the static supported range.
pub fn clamp_mixer_section_height_px(value: f32) -> f32 {
    value.clamp(SECTION_VIEWPORT_MIN_H, SECTION_VIEWPORT_MAX_H)
}

/// Clamp both section heights while preserving a usable lower pan/fader area
/// for the current strip allocation.
pub fn clamp_mixer_section_heights_for_strip(
    insert_px: f32,
    send_px: f32,
    strip_available_px: f32,
) -> (f32, f32) {
    let mut insert_px = clamp_mixer_section_height_px(insert_px);
    let mut send_px = clamp_mixer_section_height_px(send_px);
    let fixed_without_sections =
        2.0 + SEC_HEADER_H + (SEC_SPLITTER_H * 2.0) + LOWER_CONTROL_MIN_H + SEC_FOOTER_H;
    let max_total = (strip_available_px - fixed_without_sections).max(SECTION_VIEWPORT_MIN_H * 2.0);

    let total = insert_px + send_px;
    if total > max_total {
        let overflow = total - max_total;
        let shrinkable_insert = insert_px - SECTION_VIEWPORT_MIN_H;
        let shrinkable_send = send_px - SECTION_VIEWPORT_MIN_H;
        let shrinkable_total = shrinkable_insert + shrinkable_send;
        if shrinkable_total > 0.0 {
            insert_px -= overflow * (shrinkable_insert / shrinkable_total);
            send_px -= overflow * (shrinkable_send / shrinkable_total);
        }
        insert_px = clamp_mixer_section_height_px(insert_px);
        send_px = clamp_mixer_section_height_px(send_px);
    }

    (insert_px, send_px)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MixerSplitTarget {
    InsertSend,
    SendFader,
}

/// Splitter drag/reset intents emitted by the channel-strip splitter handle.
/// Pointer Y values are window-space (matches `MouseDownEvent::position.y`).
#[derive(Clone, Copy, Debug)]
pub enum MixerSplitAction {
    /// Pointer pressed on the splitter — record the drag anchor.
    ResizeStart(MixerSplitTarget, f32),
    /// Pointer moved while dragging — recompute the shared rack height.
    ResizeMove(f32),
    /// Pointer released — commit the drag.
    ResizeEnd,
    /// Double-click — reset the targeted section to its default height.
    Reset(MixerSplitTarget),
}

/// Shared split layout passed into the mixer. Insert/send heights are already
/// clamped by the owner; `on_action` routes splitter intents back to the owner
/// so all strips resize together.
#[derive(Clone)]
pub struct MixerSplit {
    pub insert_px: f32,
    pub send_px: f32,
    pub active_target: Option<MixerSplitTarget>,
    pub on_action: std::sync::Arc<dyn Fn(MixerSplitAction, &mut Window, &mut App) + 'static>,
}

impl MixerSplit {
    /// Inert split for fallback UI (no live owner to route drags to).
    pub fn inert() -> Self {
        Self {
            insert_px: MIXER_INSERT_SECTION_DEFAULT_PX,
            send_px: MIXER_SEND_SECTION_DEFAULT_PX,
            active_target: None,
            on_action: std::sync::Arc::new(|_, _, _| {}),
        }
    }
}

/// Zero-sized GPUI drag payload for the mixer splitter handle. Mirrors the
/// bottom-panel resize pattern: `on_drag` registers it, `on_drag_move` on the
/// mixer root recomputes height while the pointer is captured.
#[derive(Clone, Copy, Debug, Default)]
pub struct MixerSplitDrag;

impl Render for MixerSplitDrag {
    fn render(&mut self, _w: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        Empty
    }
}
