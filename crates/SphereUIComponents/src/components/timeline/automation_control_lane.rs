use crate::components::timeline::timeline_state::{
    LastTouchedPluginParam, TimelineState, AUTOMATION_CONTROL_LANE_HEIGHT, HEADER_WIDTH,
};
use crate::theme::Colors;
use gpui::{div, px, InteractiveElement, IntoElement, ParentElement, Styled};

/// Left inset for the automation control row header (slightly less than sub-lanes
/// so the hierarchy reads: parent → control → lanes).
const CONTROL_HEADER_INDENT: f32 = 24.0;

/// Actions fired from the automation control lane. UI-only row — never serialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationControlAction {
    /// Open the automation target picker (`x`, `y` are window-space anchors).
    OpenTargetPicker,
    /// Add automation for the last touched VST3 parameter on this track.
    AddLastTouched,
    /// Collapse the automation section (hide control + sub-lanes).
    HideAutomation,
    /// Request confirmation before removing all automation lanes.
    RequestClearAll,
}

/// Payload: `(track_id, action, window_x, window_y)`.
pub type AutomationControlCallback = std::sync::Arc<
    dyn Fn(&(String, AutomationControlAction, f32, f32), &mut gpui::Window, &mut gpui::App)
        + 'static,
>;

/// UI-only management row rendered directly below the parent track and above
/// automation sub-lanes. Not an audio track, not an envelope lane.
pub fn automation_control_lane(
    track_id: &str,
    track_color: gpui::Rgba,
    lane_height: f32,
    state: &TimelineState,
    on_action: Option<AutomationControlCallback>,
) -> impl IntoElement {
    let track_id = track_id.to_string();
    let last_touched = state
        .last_touched_plugin_param_for_track(&track_id)
        .cloned();
    let id_num = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        track_id.hash(&mut hasher);
        hasher.finish() as usize
    };

    let header = control_header(
        &track_id,
        track_color,
        last_touched.as_ref(),
        id_num,
        on_action,
    );

    // Right-side management strip stays on the neutral timeline canvas, not a
    // tinted block — only the left header carries any tint.
    let timeline_bg = div()
        .flex_1()
        .h_full()
        .bg(Colors::automation_canvas_bg())
        .border_b(px(1.0))
        .border_color(Colors::with_alpha(Colors::automation_separator(), 0.7));

    div()
        .flex()
        .flex_row()
        .w_full()
        .h(px(lane_height))
        // No row-level fill: the header paints its own (opaque) label surface and
        // the right strip is translucent so the timeline grid shows through.
        .border_b(px(1.0))
        .border_color(Colors::with_alpha(Colors::automation_separator(), 0.7))
        .child(header)
        .child(timeline_bg)
}

/// Visual weight for a control-row button. Only the primary action carries the
/// accent; destructive actions stay neutral until hovered.
#[derive(Clone, Copy)]
enum ControlButtonStyle {
    Primary,
    Neutral,
    Ghost,
    Danger,
}

#[allow(clippy::too_many_arguments)]
fn control_header(
    track_id: &str,
    _track_color: gpui::Rgba,
    last_touched: Option<&LastTouchedPluginParam>,
    id_num: usize,
    on_action: Option<AutomationControlCallback>,
) -> impl IntoElement {
    let label_row = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(5.0))
        .min_w(px(0.0))
        .child(
            div()
                .w(px(2.0))
                .h(px(8.0))
                .rounded(px(crate::theme::radius::PILL))
                .bg(Colors::automation_rail_active()),
        )
        .child(
            div()
                .text_size(px(9.0))
                .truncate()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(Colors::text_secondary())
                .child("Automation"),
        );

    let buttons = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .flex_none()
        .child(control_button(
            ("automation-ctrl-add", id_num).into(),
            "+ Parameter",
            ControlButtonStyle::Primary,
            track_id,
            AutomationControlAction::OpenTargetPicker,
            on_action.clone(),
        ))
        .children(last_touched.map(|touched| {
            let short = truncate_label(&touched.display_label(), 18);
            control_button(
                ("automation-ctrl-last", id_num).into(),
                &format!("Last: {short}"),
                ControlButtonStyle::Neutral,
                track_id,
                AutomationControlAction::AddLastTouched,
                on_action.clone(),
            )
        }))
        .child(control_button(
            ("automation-ctrl-hide", id_num).into(),
            "Hide",
            ControlButtonStyle::Ghost,
            track_id,
            AutomationControlAction::HideAutomation,
            on_action.clone(),
        ))
        // Destructive — kept quiet by default, only reads red on hover so it
        // doesn't compete with the primary "+ Parameter" action.
        .child(control_button(
            ("automation-ctrl-clear", id_num).into(),
            "Clear All",
            ControlButtonStyle::Danger,
            track_id,
            AutomationControlAction::RequestClearAll,
            on_action,
        ));

    div()
        .relative()
        .w(px(HEADER_WIDTH))
        .h(px(AUTOMATION_CONTROL_LANE_HEIGHT))
        .flex_none()
        // Opaque label surface so the left header reads as a header column, not
        // the translucent canvas used for the right strip.
        .bg(Colors::automation_lane_header_bg())
        .border_r(px(1.0))
        .border_color(Colors::border_subtle())
        .child(
            div()
                .absolute()
                .left_0()
                .top_0()
                .bottom_0()
                .w(px(CONTROL_HEADER_INDENT))
                .bg(Colors::with_alpha(Colors::surface_base(), 0.28)),
        )
        .child(
            div()
                .absolute()
                .left(px(14.0))
                .top(px(6.0))
                .bottom(px(6.0))
                .w(px(1.0))
                .bg(Colors::automation_rail()),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .w_full()
                .h_full()
                .pl(px(CONTROL_HEADER_INDENT))
                .pr(px(8.0))
                .child(label_row)
                .child(buttons),
        )
}

fn truncate_label(label: &str, max_chars: usize) -> String {
    if label.chars().count() <= max_chars {
        return label.to_string();
    }
    let mut out: String = label.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn control_button(
    id: gpui::ElementId,
    label: &str,
    style: ControlButtonStyle,
    track_id: &str,
    action: AutomationControlAction,
    cb: Option<AutomationControlCallback>,
) -> impl IntoElement {
    let track_id = track_id.to_string();
    let label = label.to_string();
    let mut btn = div()
        .flex()
        .items_center()
        .justify_center()
        .h(px(18.0))
        .px(px(7.0))
        .rounded(px(crate::theme::radius::CONTROL))
        .text_size(px(8.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .cursor(gpui::CursorStyle::PointingHand)
        .id(id);

    btn = match style {
        ControlButtonStyle::Primary => btn
            .bg(Colors::with_alpha(Colors::accent_primary(), 0.16))
            .text_color(Colors::accent_primary())
            .border(px(1.0))
            .border_color(Colors::with_alpha(Colors::accent_primary(), 0.40))
            .hover(|s| s.bg(Colors::with_alpha(Colors::accent_primary(), 0.26))),
        ControlButtonStyle::Neutral => btn
            .bg(Colors::button_bg())
            .text_color(Colors::button_text())
            .border(px(1.0))
            .border_color(Colors::button_border())
            .hover(|s| s.bg(Colors::button_bg_hover())),
        ControlButtonStyle::Ghost => btn.text_color(Colors::button_text_muted()).hover(|s| {
            s.bg(Colors::button_bg_hover())
                .text_color(Colors::text_secondary())
        }),
        ControlButtonStyle::Danger => btn.text_color(Colors::text_dim()).hover(|s| {
            s.bg(Colors::with_alpha(Colors::status_error(), 0.16))
                .text_color(Colors::status_error())
        }),
    };

    if let Some(cb) = cb {
        btn = btn.on_mouse_down(
            gpui::MouseButton::Left,
            move |event: &gpui::MouseDownEvent, window, cx| {
                cx.stop_propagation();
                let x: f32 = event.position.x.into();
                let y: f32 = event.position.y.into();
                cb(&(track_id.clone(), action, x, y), window, cx);
            },
        );
    }

    btn.child(label)
}
