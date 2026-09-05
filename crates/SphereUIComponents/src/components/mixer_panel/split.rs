use gpui::{App, Empty, IntoElement, Render, Window};

// ── Section dimensions ─────────────────────────────────────────────────────
//
// The per-section heights themselves live in [`super::console`], which owns the
// look; what lives here is what the splitter arithmetic needs — the strip's
// overall bounds and the fixed height it must leave alone.
pub const STRIP_WIDTH: f32 = 88.0;
/// Minimum height for a channel strip. Below this the mixer should scroll/clip
/// as a whole rather than compressing the pan/fader controls into unusability.
pub const STRIP_MIN_HEIGHT: f32 = 320.0;

/// Everything below the racks: I/O, pan, the fader bay at its floor, and the
/// toggle row. Fixed on every strip, which is what keeps the four kinds of
/// strip on one set of baselines.
pub(crate) const LOWER_CONTROL_MIN_H: f32 = super::console::IO_ROW_H
    + super::console::PAN_H
    + super::console::FADER_MIN_H
    + super::console::BUTTONS_H;

// ── Vertical mixer section resizing ─────────────────────────────────────────
// Inserts and sends each own a fixed-height clipped viewport with their own
// vertical scrolling. Heights are shared across all strips so rows stay aligned
// across the mixer. Splitter actions are routed to `StudioLayout`, which owns
// the shared values and mirrors them into the detached mixer window snapshot.
/// Visual + hitbox height of the splitter handle.
pub(crate) const SEC_SPLITTER_H: f32 = 6.0;
const SECTION_VIEWPORT_MIN_H: f32 = 42.0;
const SECTION_VIEWPORT_MAX_H: f32 = 180.0;
/// Default height of the inserts viewport.
///
/// A fresh session has no inserts, so this reserves a visible gap between
/// INSERTS and SENDS — but it cannot simply be lowered. The Monitor strip's
/// Source selector deliberately occupies this same slot to keep the two pinned
/// strips on matching baselines (`monitor_strip`), and shortening it collapses
/// that selector to an ellipsis. Tightening the empty bay needs the Monitor's
/// routing block decoupled from the inserts height first.
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
    // Everything the two racks may never eat into: the type row, both splitter
    // handles, the lower console, and the name plate.
    let fixed_without_sections = super::console::TOP_ROW_H
        + (SEC_SPLITTER_H * 2.0)
        + LOWER_CONTROL_MIN_H
        + super::console::PLATE_H;
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
