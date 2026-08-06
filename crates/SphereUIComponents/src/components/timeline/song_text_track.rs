use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use crate::components::timeline::global_lane_header::{
    global_lane_header, GlobalLaneHeaderActions,
};
use crate::components::timeline::timeline_state::{SongTextEventType, TimelineState};
use crate::theme::Colors;
use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, AppContext, Empty, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Window,
};

pub const SONG_TEXT_LANE_HEIGHT: f32 = 58.0;
const ROW_HEIGHT: f32 = 18.0;

#[derive(Clone, Debug)]
pub struct SongTextDragSession {
    pub anchor_event_id: String,
    pub original_positions: std::sync::Arc<Vec<(String, f64)>>,
    pub pointer_offset_x: f32,
}

impl Render for SongTextDragSession {
    fn render(&mut self, _window: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        Empty
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SongTextDragPreview {
    pub positions: Vec<(String, f64)>,
}

pub fn song_text_drag_positions(
    anchor_event_id: &str,
    original_positions: &[(String, f64)],
    proposed_anchor: f64,
) -> Vec<(String, f64)> {
    let Some(anchor_start) = original_positions
        .iter()
        .find(|(id, _)| id == anchor_event_id)
        .map(|(_, beat)| *beat)
    else {
        return original_positions.to_vec();
    };
    let minimum = original_positions
        .iter()
        .map(|(_, beat)| *beat)
        .fold(anchor_start, f64::min);
    let delta = (proposed_anchor - anchor_start).max(-minimum);
    original_positions
        .iter()
        .map(|(id, beat)| (id.clone(), (beat + delta).max(0.0)))
        .collect()
}

#[derive(Clone, Debug)]
pub struct SongTextMarkerDown {
    pub event_id: String,
    pub beat: f64,
    pub additive: bool,
    pub click_count: u32,
}

pub type SongTextMarkerDownCallback =
    std::sync::Arc<dyn Fn(&SongTextMarkerDown, &mut gpui::Window, &mut gpui::App) + 'static>;

pub type SongTextLaneSeekCallback =
    std::sync::Arc<dyn Fn(&f32, &mut gpui::Window, &mut gpui::App) + 'static>;

pub type SongTextMarkerContextCallback =
    std::sync::Arc<dyn Fn(&(String, f64, f32, f32), &mut gpui::Window, &mut gpui::App) + 'static>;

pub fn song_text_track_lane(
    state: &TimelineState,
    drag_preview: Option<&SongTextDragPreview>,
    on_marker_down: SongTextMarkerDownCallback,
    on_marker_context: Option<SongTextMarkerContextCallback>,
    on_empty_seek: SongTextLaneSeekCallback,
) -> impl IntoElement {
    let lane_width = state.viewport.viewport_width.max(1.0);
    let overscan_beats = 160.0 / state.viewport.pixels_per_beat.max(1.0) as f64;
    let start_beat = (state.x_to_beat(0.0) - overscan_beats).max(0.0);
    let end_beat = state.x_to_beat(lane_width) + overscan_beats;
    let visible = state.song_text_events_in_range(start_beat, end_beat);
    let preview_positions: HashMap<&str, f64> = drag_preview
        .into_iter()
        .flat_map(|preview| preview.positions.iter())
        .map(|(id, beat)| (id.as_str(), *beat))
        .collect();
    let selected_ids: std::collections::HashSet<_> = state
        .selection
        .selected_song_text_event_ids
        .iter()
        .map(String::as_str)
        .collect();
    let selected_positions = std::sync::Arc::new(
        state
            .song_text_events
            .iter()
            .filter(|event| selected_ids.contains(event.id.as_str()))
            .map(|event| (event.id.clone(), event.beat))
            .collect::<Vec<_>>(),
    );
    let active_section = state
        .active_song_text_event(SongTextEventType::Section)
        .map(|event| event.id.as_str());
    let active_chord = state
        .active_song_text_event(SongTextEventType::Chord)
        .map(|event| event.id.as_str());
    let active_lyric = state
        .active_song_text_event(SongTextEventType::Lyric)
        .map(|event| event.id.as_str());

    let mut markers = Vec::with_capacity(visible.len());
    let mut last_right = [f32::NEG_INFINITY; 3];
    let mut collision_level = [0usize; 3];

    for event in visible {
        let beat = preview_positions
            .get(event.id.as_str())
            .copied()
            .unwrap_or(event.beat);
        let x = state.beats_to_x(beat as f32);
        let event_type = event.event_type();
        let row = event_type.sort_key() as usize;
        let estimated_width = (event.text().chars().count() as f32 * 5.7 + 14.0).clamp(28.0, 132.0);
        if x < last_right[row] + 3.0 {
            collision_level[row] = (collision_level[row] + 1) % 2;
        } else {
            collision_level[row] = 0;
        }
        let compact = collision_level[row] == 1;
        let marker_height = if compact { 8.0 } else { 15.0 };
        let top = row as f32 * ROW_HEIGHT
            + if compact {
                ROW_HEIGHT - marker_height - 1.0
            } else {
                1.0
            };
        let width = if compact {
            estimated_width.min(56.0)
        } else {
            estimated_width
        };
        last_right[row] = x + width;

        let is_selected = selected_ids.contains(event.id.as_str());
        let is_active = match event_type {
            SongTextEventType::Section => active_section == Some(event.id.as_str()),
            SongTextEventType::Chord => active_chord == Some(event.id.as_str()),
            SongTextEventType::Lyric => active_lyric == Some(event.id.as_str()),
        };
        let accent = match event_type {
            SongTextEventType::Section => Colors::accent_success(),
            SongTextEventType::Chord => Colors::accent_primary(),
            SongTextEventType::Lyric => Colors::text_secondary(),
        };
        let background = if is_selected {
            Colors::with_alpha(Colors::accent_primary(), 0.28)
        } else if is_active {
            Colors::with_alpha(accent, 0.18)
        } else {
            Colors::with_alpha(Colors::surface_raised(), 0.94)
        };
        let border = if is_selected {
            Colors::accent_primary()
        } else if is_active {
            Colors::with_alpha(accent, 0.75)
        } else {
            Colors::with_alpha(accent, 0.35)
        };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        event.id.hash(&mut hasher);
        let marker_id = hasher.finish();
        let event_id = event.id.clone();
        let context_event_id = event.id.clone();
        let down_callback = on_marker_down.clone();
        let context_callback = on_marker_context.clone();
        let original_positions = if is_selected {
            selected_positions.clone()
        } else {
            std::sync::Arc::new(vec![(event.id.clone(), event.beat)])
        };
        let drag = SongTextDragSession {
            anchor_event_id: event.id.clone(),
            original_positions,
            pointer_offset_x: 0.0,
        };

        markers.push(
            div()
                .id(("song-text-marker", marker_id))
                .absolute()
                .left(px(x))
                .top(px(top))
                .w(px(width))
                .h(px(marker_height))
                .flex()
                .items_center()
                .px(px(if compact { 3.0 } else { 5.0 }))
                .rounded_sm()
                .border(px(1.0))
                .border_color(border)
                .bg(background)
                .overflow_hidden()
                .cursor(gpui::CursorStyle::PointingHand)
                .hover(move |style| {
                    style
                        .bg(Colors::with_alpha(accent, 0.22))
                        .border_color(accent)
                })
                .text_size(px(if compact { 7.0 } else { 9.0 }))
                .font_weight(if event_type == SongTextEventType::Chord {
                    gpui::FontWeight::BOLD
                } else {
                    gpui::FontWeight::SEMIBOLD
                })
                .text_color(if is_active || is_selected {
                    Colors::text_primary()
                } else {
                    accent
                })
                .whitespace_nowrap()
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    move |mouse: &gpui::MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        down_callback(
                            &SongTextMarkerDown {
                                event_id: event_id.clone(),
                                beat,
                                additive: mouse.modifiers.control
                                    || mouse.modifiers.platform
                                    || mouse.modifiers.shift,
                                click_count: mouse.click_count as u32,
                            },
                            window,
                            cx,
                        );
                    },
                )
                .when_some(context_callback, |marker, callback| {
                    marker.on_mouse_down(
                        gpui::MouseButton::Right,
                        move |mouse: &gpui::MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            callback(
                                &(
                                    context_event_id.clone(),
                                    beat,
                                    mouse.position.x.into(),
                                    mouse.position.y.into(),
                                ),
                                window,
                                cx,
                            );
                        },
                    )
                })
                .on_drag(drag, |drag, offset, _window, cx| {
                    cx.new(|_| SongTextDragSession {
                        pointer_offset_x: offset.x.into(),
                        ..drag.clone()
                    })
                })
                .child(event.text().to_string()),
        );
    }

    // Measured lane origin (the browser panel is collapsible), captured once so
    // the click handler shares the transform used to place the markers above.
    let lane_origin_x = state.lane_origin_x();
    let empty_seek = div()
        .absolute()
        .inset_0()
        .id("song-text-lane-empty-hit")
        .on_mouse_down(
            gpui::MouseButton::Left,
            move |mouse: &gpui::MouseDownEvent, window, cx| {
                let window_x: f32 = mouse.position.x.into();
                let lane_x = window_x - lane_origin_x;
                on_empty_seek(&lane_x, window, cx);
            },
        );

    let active_summary = match (
        state.active_song_text_event(SongTextEventType::Chord),
        state.active_song_text_event(SongTextEventType::Lyric),
    ) {
        (Some(chord), Some(lyric)) => format!("{} · {}", chord.text(), lyric.text()),
        (Some(chord), None) => chord.text().to_string(),
        (None, Some(lyric)) => lyric.text().to_string(),
        (None, None) => "No events".to_string(),
    };
    let header = global_lane_header(
        "song-text",
        "Song Text",
        active_summary,
        false,
        "Song Text lane is always visible",
        GlobalLaneHeaderActions {
            on_add: None,
            on_menu: None,
            on_hide: None,
            on_toggle_collapsed: None,
        },
    );

    div()
        .flex()
        .flex_row()
        .h(px(SONG_TEXT_LANE_HEIGHT))
        .w_full()
        .bg(Colors::surface_panel_alt())
        .border_b(px(1.0))
        .border_color(Colors::border_subtle())
        .child(header)
        .child(
            div()
                .flex_1()
                .h_full()
                .relative()
                .overflow_hidden()
                .child(empty_seek)
                .children(markers)
                .children(crate::perf::debug_clip_outline()),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drag_uses_start_snapshot_and_preserves_spacing() {
        let original = vec![("a".to_string(), 4.0), ("b".to_string(), 6.5)];
        assert_eq!(
            song_text_drag_positions("a", &original, 8.0),
            vec![("a".to_string(), 8.0), ("b".to_string(), 10.5)]
        );
        assert_eq!(
            song_text_drag_positions("a", &original, 9.0),
            vec![("a".to_string(), 9.0), ("b".to_string(), 11.5)],
            "a second frame must still derive from the original snapshot"
        );
    }

    #[test]
    fn drag_clamps_group_at_project_start() {
        let original = vec![("a".to_string(), 2.0), ("b".to_string(), 0.5)];
        assert_eq!(
            song_text_drag_positions("a", &original, -10.0),
            vec![("a".to_string(), 1.5), ("b".to_string(), 0.0)]
        );
    }
}
