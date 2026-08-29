use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, App, AppContext, DragMoveEvent, InteractiveElement, IntoElement, ParentElement, Role,
    StatefulInteractiveElement, Styled, Window,
};

use crate::components::combo_box::combo_box_trigger;
use crate::components::controls::fb_checkbox;
use crate::components::spin_drag::SpinDrag;
use crate::theme::Colors;

pub type InspectorNumericChangeCb = std::sync::Arc<dyn Fn(f64, &mut Window, &mut App) + 'static>;
pub type InspectorNumericGestureCb = std::sync::Arc<dyn Fn(&mut Window, &mut App) + 'static>;

#[derive(Clone, Copy)]
pub struct InspectorSelectOption<T: Copy + PartialEq + 'static> {
    pub label: &'static str,
    pub value: T,
}

pub fn inspector_section(
    title: impl Into<String>,
    subtitle: Option<impl Into<String>>,
    children: impl IntoElement,
) -> impl IntoElement {
    let title = title.into();
    let subtitle = subtitle.map(Into::into);
    div()
        .flex()
        .flex_col()
        .gap(px(5.0))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(1.0))
                .child(
                    div()
                        .h(px(18.0))
                        .flex()
                        .items_center()
                        .text_size(px(9.5))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(Colors::text_faint())
                        .child(title),
                )
                .children(subtitle.map(|text| {
                    div()
                        .min_w(px(0.0))
                        .text_size(px(10.0))
                        .text_color(Colors::text_faint())
                        .child(text)
                })),
        )
        .child(div().flex().flex_col().gap(px(3.0)).child(children))
}

pub fn inspector_row(
    label: impl Into<String>,
    disabled: bool,
    control: impl IntoElement,
) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .min_h(px(24.0))
        .opacity(if disabled { 0.48 } else { 1.0 })
        .child(
            div()
                .w(px(106.0))
                .flex_shrink_0()
                .truncate()
                .text_size(px(10.5))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(Colors::text_muted())
                .child(label.into()),
        )
        .child(div().flex_1().min_w_0().child(control))
}

pub fn inspector_value(text: impl Into<String>) -> impl IntoElement {
    div()
        .min_w(px(0.0))
        .h(px(24.0))
        .flex()
        .items_center()
        .justify_end()
        .truncate()
        .text_size(px(11.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(Colors::text_secondary())
        .child(text.into())
}

pub fn inspector_select<T: Copy + PartialEq + 'static>(
    id: impl Into<gpui::ElementId>,
    selected: T,
    options: &'static [InspectorSelectOption<T>],
    disabled: bool,
    on_change: impl Fn(T, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let selected_index = options
        .iter()
        .position(|option| option.value == selected)
        .unwrap_or(0);
    let label = options
        .get(selected_index)
        .map(|option| option.label)
        .unwrap_or("-");
    let next_value = options
        .get((selected_index + 1).min(options.len().saturating_sub(1)))
        .map(|option| option.value)
        .unwrap_or(selected);
    div()
        .h(px(24.0))
        .when(disabled, |this| this.opacity(0.48))
        .child(combo_box_trigger(id, label, false, move |_, window, cx| {
            if !disabled && !options.is_empty() {
                let next = if selected_index + 1 >= options.len() {
                    options[0].value
                } else {
                    next_value
                };
                on_change(next, window, cx);
            }
        }))
}

pub fn inspector_checkbox(
    id: impl Into<gpui::ElementId>,
    checked: bool,
    disabled: bool,
    label: impl Into<String>,
    on_change: impl Fn(bool, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    fb_checkbox(id, label, checked, !disabled, move |_, window, cx| {
        if !disabled {
            on_change(!checked, window, cx);
        }
    })
}

pub fn inspector_numeric_stepper(
    id: &'static str,
    value: f64,
    display: impl Into<String>,
    min: f64,
    max: f64,
    step: f64,
    disabled: bool,
    on_change: impl Fn(f64, &mut Window, &mut App) + Clone + 'static,
) -> impl IntoElement {
    inspector_numeric_stepper_with_drag_callbacks(
        id,
        value,
        display,
        min,
        max,
        step,
        disabled,
        None,
        std::sync::Arc::new(on_change),
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn inspector_numeric_stepper_with_drag_callbacks(
    id: &'static str,
    value: f64,
    display: impl Into<String>,
    min: f64,
    max: f64,
    step: f64,
    disabled: bool,
    on_drag_start: Option<InspectorNumericGestureCb>,
    on_drag_preview: InspectorNumericChangeCb,
    on_drag_commit: Option<InspectorNumericGestureCb>,
) -> impl IntoElement {
    const SCRUB_PIXELS_PER_STEP: f32 = 5.0;
    let drag_id = id.to_string();
    let drag_id_move = drag_id.clone();
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_end()
        .opacity(if disabled { 0.48 } else { 1.0 })
        .child({
            let display = display.into();
            let field = div()
                .id((id, 0usize))
                .w(px(108.0))
                .h(px(24.0))
                .flex()
                .items_center()
                .justify_end()
                .rounded(px(crate::theme::radius::CONTROL))
                .border(px(1.0))
                .border_color(Colors::border_subtle())
                .bg(Colors::surface_input())
                .px(px(7.0))
                .text_size(px(11.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(Colors::text_primary())
                .child(display);
            if disabled {
                field.into_any_element()
            } else {
                field
                    .cursor(gpui::CursorStyle::ResizeUpDown)
                    .hover(|style| {
                        style
                            .bg(Colors::surface_control_hover())
                            .border_color(Colors::border_strong())
                    })
                    .on_drag(
                        SpinDrag::new(drag_id, value),
                        move |drag, _offset, window, cx| {
                            drag.begin();
                            if let Some(start) = on_drag_start.as_ref() {
                                start(window, cx);
                            }
                            cx.new(|_| drag.clone())
                        },
                    )
                    .on_drag_move::<SpinDrag>(move |event: &DragMoveEvent<SpinDrag>, window, cx| {
                        let drag = event.drag(cx);
                        if !drag.matches(&drag_id_move) {
                            return;
                        }
                        let current_y: f32 = event.event.position.y.into();
                        let next = drag.value_at(
                            current_y,
                            step / f64::from(SCRUB_PIXELS_PER_STEP),
                            min,
                            max,
                            Some(step),
                        );
                        on_drag_preview(next, window, cx);
                    })
                    .when_some(on_drag_commit, |field, commit| {
                        let commit_out = commit.clone();
                        field
                            .on_mouse_up(gpui::MouseButton::Left, move |_, window, cx| {
                                commit(window, cx)
                            })
                            .on_mouse_up_out(gpui::MouseButton::Left, move |_, window, cx| {
                                commit_out(window, cx)
                            })
                    })
                    .into_any_element()
            }
        })
}

pub fn inspector_mini_button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<String>,
    enabled: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label: String = label.into();
    let mut button = div()
        .id(id)
        .role(Role::Button)
        .aria_label(label.clone())
        .aria_disabled(!enabled)
        .when(enabled, |button| {
            button
                .focusable()
                .tab_stop(true)
                .focus_visible(|style| style.border_color(Colors::border_focus()))
        })
        .h(px(24.0))
        .min_w(px(26.0))
        .px(px(7.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(crate::theme::radius::CONTROL))
        .border(px(1.0))
        .border_color(Colors::border_subtle())
        .bg(Colors::surface_input())
        .opacity(if enabled { 1.0 } else { 0.45 })
        .text_size(px(10.5))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(Colors::text_secondary())
        .child(label);
    if enabled {
        button = button
            .cursor(gpui::CursorStyle::PointingHand)
            .hover(|s| {
                s.bg(Colors::surface_control_hover())
                    .border_color(Colors::border_strong())
            })
            .on_click(on_click);
    }
    button
}

pub fn inspector_hint_text(text: impl Into<String>) -> impl IntoElement {
    div()
        .min_w(px(0.0))
        .pt(px(1.0))
        .text_size(px(10.0))
        .text_color(Colors::text_faint())
        .child(text.into())
}
