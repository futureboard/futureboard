//! Horizontal normalized slider used by the timeline TrackHeader volume row.
//!
//! Drag-driven. The slider's hitbox is intentionally taller than its visible
//! rail so vertical wiggle during a drag does not lose tracking. Mouse-down
//! does **not** snap value-to-cursor; the user has to start moving for the
//! slider to update. This keeps accidental clicks on the rail from changing
//! volume.

use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, AccessibleAction, App, AppContext, DragMoveEvent, Empty, InteractiveElement,
    IntoElement, Orientation, ParentElement, Render, Role, StatefulInteractiveElement, Styled,
    Window,
};
use std::sync::Arc;

use crate::theme::Colors;

type SliderValueCb = Arc<dyn Fn(&f32, &mut Window, &mut App) + 'static>;
type SliderCommitCb = Arc<dyn Fn(&mut Window, &mut App) + 'static>;

fn accessible_label_from_id(id: &str) -> String {
    id.replace(['-', '_'], " ")
}

/// Drag payload sent from a slider mouse-down. Carries the slider's `id` so a
/// shared on_drag_move listener (or multiple sliders on screen) can dispatch
/// to the correct callback.
#[derive(Clone, Debug)]
pub struct SliderDrag {
    pub id: String,
}

impl Render for SliderDrag {
    fn render(&mut self, _w: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// Render a horizontal slider.
///
/// * `id`           – stable id (also stamped into the drag marker so move
///                    events can be filtered by which slider started the drag).
/// * `value_norm`   – current value in `0.0..=1.0`.
/// * `accent`       – fill color (usually the track color).
/// * `on_change`    – called with the new normalized value each time the user
///                    drags. Wire this into a `TimelineState::set_track_volume`
///                    callback.
pub fn slider(
    id: impl Into<gpui::SharedString>,
    value_norm: f32,
    accent: gpui::Rgba,
    on_change: impl Fn(&f32, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    slider_with_reset(
        id,
        value_norm,
        accent,
        on_change,
        None::<fn(&mut Window, &mut App)>,
    )
}

pub fn slider_with_reset(
    id: impl Into<gpui::SharedString>,
    value_norm: f32,
    accent: gpui::Rgba,
    on_change: impl Fn(&f32, &mut Window, &mut App) + 'static,
    on_double_click_reset: Option<impl Fn(&mut Window, &mut App) + 'static>,
) -> impl IntoElement {
    slider_with_drag_callbacks(
        id,
        value_norm,
        accent,
        None::<fn(&f32, &mut Window, &mut App)>,
        Some(on_change),
        None::<fn(&mut Window, &mut App)>,
        on_double_click_reset,
    )
}

/// Compact fill-only slider for dense rows (mixer send gain). No thumb — value
/// is read from the filled rail length. Smaller hit/rail than [`slider`].
pub fn compact_slider_with_reset(
    id: impl Into<gpui::SharedString>,
    value_norm: f32,
    accent: gpui::Rgba,
    on_change: impl Fn(&f32, &mut Window, &mut App) + 'static,
    on_double_click_reset: Option<impl Fn(&mut Window, &mut App) + 'static>,
) -> impl IntoElement {
    let id_str: gpui::SharedString = id.into();
    let id_string = id_str.to_string();
    let accessible_label = accessible_label_from_id(&id_string);
    let value = value_norm.clamp(0.0, 1.0);
    let on_change: SliderValueCb = Arc::new(on_change);
    let on_change_drag = on_change.clone();
    let on_change_increment = on_change.clone();
    let on_change_decrement = on_change.clone();
    let fill = {
        let mut c = accent;
        c.a = 0.9;
        c
    };

    div()
        .id(gpui::ElementId::Name(id_str.clone()))
        .role(Role::Slider)
        .aria_label(accessible_label)
        .aria_numeric_value(value as f64)
        .aria_min_numeric_value(0.0)
        .aria_max_numeric_value(1.0)
        .aria_orientation(Orientation::Horizontal)
        .focusable()
        .tab_stop(true)
        .focus_visible(|style| style.border(px(1.0)).border_color(Colors::border_focus()))
        .on_a11y_action(AccessibleAction::Increment, move |_, window, cx| {
            let next = (value + 0.01).min(1.0);
            on_change_increment(&next, window, cx);
        })
        .on_a11y_action(AccessibleAction::Decrement, move |_, window, cx| {
            let next = (value - 0.01).max(0.0);
            on_change_decrement(&next, window, cx);
        })
        .h(px(10.0))
        .flex_1()
        .min_w_0()
        .relative()
        .cursor(gpui::CursorStyle::ResizeLeftRight)
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(px(3.5))
                .h(px(3.0))
                .bg(Colors::divider())
                .rounded(px(crate::theme::radius::PILL)),
        )
        .child(
            div()
                .absolute()
                .left_0()
                .top(px(3.5))
                .h(px(3.0))
                .w(gpui::relative(value))
                .bg(fill)
                .rounded(px(crate::theme::radius::PILL)),
        )
        .on_drag(
            SliderDrag {
                id: id_string.clone(),
            },
            move |drag, _offset, _window, cx| {
                cx.new(|_| SliderDrag {
                    id: drag.id.clone(),
                })
            },
        )
        .on_drag_move::<SliderDrag>(move |event: &DragMoveEvent<SliderDrag>, window, cx| {
            if event.drag(cx).id != id_string {
                return;
            }
            let bounds = event.bounds;
            let x: f32 = event.event.position.x.into();
            let ox: f32 = bounds.origin.x.into();
            let ow: f32 = f32::from(bounds.size.width).max(1.0);
            let new_value = ((x - ox) / ow).clamp(0.0, 1.0);
            on_change_drag(&new_value, window, cx);
        })
        .when_some(on_double_click_reset, |this, reset| {
            this.on_click(move |event, window, cx| {
                if event.click_count() >= 2 {
                    reset(window, cx);
                }
            })
        })
}

pub fn slider_with_drag_callbacks(
    id: impl Into<gpui::SharedString>,
    value_norm: f32,
    accent: gpui::Rgba,
    on_drag_start: Option<impl Fn(&f32, &mut Window, &mut App) + 'static>,
    on_drag_preview: Option<impl Fn(&f32, &mut Window, &mut App) + 'static>,
    on_drag_commit: Option<impl Fn(&mut Window, &mut App) + 'static>,
    on_double_click_reset: Option<impl Fn(&mut Window, &mut App) + 'static>,
) -> impl IntoElement {
    let id_str: gpui::SharedString = id.into();
    let id_string = id_str.to_string();
    let accessible_label = accessible_label_from_id(&id_string);
    let value = value_norm.clamp(0.0, 1.0);
    let on_drag_start: Option<SliderValueCb> =
        on_drag_start.map(|callback| Arc::new(callback) as SliderValueCb);
    let on_drag_preview: Option<SliderValueCb> =
        on_drag_preview.map(|callback| Arc::new(callback) as SliderValueCb);
    let on_drag_commit: Option<SliderCommitCb> =
        on_drag_commit.map(|callback| Arc::new(callback) as SliderCommitCb);
    let a11y_start_increment = on_drag_start.clone();
    let a11y_preview_increment = on_drag_preview.clone();
    let a11y_commit_increment = on_drag_commit.clone();
    let a11y_start_decrement = on_drag_start.clone();
    let a11y_preview_decrement = on_drag_preview.clone();
    let a11y_commit_decrement = on_drag_commit.clone();

    let fill = {
        let mut c = accent;
        c.a = 0.95;
        c
    };
    let mut accent_glow = accent;
    accent_glow.a = 0.35;

    div()
        .id(gpui::ElementId::Name(id_str.clone()))
        .role(Role::Slider)
        .aria_label(accessible_label)
        .aria_numeric_value(value as f64)
        .aria_min_numeric_value(0.0)
        .aria_max_numeric_value(1.0)
        .aria_orientation(Orientation::Horizontal)
        .focusable()
        .tab_stop(true)
        .focus_visible(|style| style.border(px(1.0)).border_color(Colors::border_focus()))
        .on_a11y_action(AccessibleAction::Increment, move |_, window, cx| {
            let next = (value + 0.01).min(1.0);
            if let Some(start) = a11y_start_increment.as_ref() {
                start(&value, window, cx);
            }
            if let Some(preview) = a11y_preview_increment.as_ref() {
                preview(&next, window, cx);
            }
            if let Some(commit) = a11y_commit_increment.as_ref() {
                commit(window, cx);
            }
        })
        .on_a11y_action(AccessibleAction::Decrement, move |_, window, cx| {
            let next = (value - 0.01).max(0.0);
            if let Some(start) = a11y_start_decrement.as_ref() {
                start(&value, window, cx);
            }
            if let Some(preview) = a11y_preview_decrement.as_ref() {
                preview(&next, window, cx);
            }
            if let Some(commit) = a11y_commit_decrement.as_ref() {
                commit(window, cx);
            }
        })
        // Generous vertical hit area so the user can drift up/down during drag.
        .h(px(20.0))
        .flex_1()
        .relative()
        .cursor(gpui::CursorStyle::ResizeLeftRight)
        // Recessed rail
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(px(7.0))
                .h(px(6.0))
                .bg(Colors::divider())
                .border(px(1.0))
                .border_color(Colors::with_alpha(Colors::surface_canvas(), 0.25))
                .rounded(px(crate::theme::radius::PILL)),
        )
        // Fill bar
        .child(
            div()
                .absolute()
                .left_0()
                .top(px(7.0))
                .h(px(6.0))
                .w(gpui::relative(value))
                .bg(fill)
                .rounded(px(crate::theme::radius::PILL)),
        )
        // Soft glow halo behind the thumb so it reads as the active element.
        .child(
            div()
                .absolute()
                .top(px(2.0))
                .left(gpui::relative(value))
                .ml(px(-6.0))
                .w(px(12.0))
                .h(px(16.0))
                .rounded(px(crate::theme::radius::PILL))
                .bg(accent_glow),
        )
        // Handle thumb — bordered, brighter, and a hair taller than the rail.
        .child(
            div()
                .absolute()
                .top(px(3.0))
                .left(gpui::relative(value))
                .ml(px(-4.0))
                .w(px(8.0))
                .h(px(14.0))
                .rounded(px(crate::theme::radius::CONTROL))
                .bg(Colors::text_primary())
                .border(px(1.0))
                .border_color(Colors::border_strong())
                // Center accent notch inside the handle so the user can read
                // which slider they're touching at a glance.
                .child(
                    div()
                        .absolute()
                        .top(px(3.0))
                        .left(px(2.0))
                        .right(px(2.0))
                        .h(px(1.5))
                        .bg(accent),
                ),
        )
        .on_drag(
            SliderDrag {
                id: id_string.clone(),
            },
            move |drag, _offset, window, cx| {
                if let Some(start) = on_drag_start.as_ref() {
                    start(&value, window, cx);
                }
                cx.new(|_| SliderDrag {
                    id: drag.id.clone(),
                })
            },
        )
        .on_drag_move::<SliderDrag>(move |event: &DragMoveEvent<SliderDrag>, window, cx| {
            if event.drag(cx).id != id_string {
                return;
            }
            let bounds = event.bounds;
            let x: f32 = event.event.position.x.into();
            let ox: f32 = bounds.origin.x.into();
            let ow: f32 = f32::from(bounds.size.width).max(1.0);
            let new_value = ((x - ox) / ow).clamp(0.0, 1.0);
            if let Some(preview) = on_drag_preview.as_ref() {
                preview(&new_value, window, cx);
            }
        })
        .when_some(on_drag_commit, |this, commit| {
            this.on_mouse_up(gpui::MouseButton::Left, {
                let commit = commit.clone();
                move |_event, window, cx| commit(window, cx)
            })
            .on_mouse_up_out(gpui::MouseButton::Left, move |_event, window, cx| {
                commit(window, cx)
            })
        })
        .when_some(on_double_click_reset, |this, reset| {
            this.on_click(move |event, window, cx| {
                if event.click_count() >= 2 {
                    reset(window, cx);
                }
            })
        })
}
