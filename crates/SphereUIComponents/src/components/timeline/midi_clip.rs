use crate::components::timeline::timeline_state::{
    midi_debug_enabled, ClipDragItem, ClipEdge, ClipResizeDrag, ClipState, ClipType,
    MidiControllerKind, MidiControllerPoint, MidiNoteState, TimelineState,
};
use crate::theme::Colors;
use gpui::{
    canvas, div, fill, point, px, size, AppContext, Bounds, InteractiveElement, IntoElement,
    ParentElement, Pixels, StatefulInteractiveElement, Styled,
};

pub fn midi_clip(
    clip: &ClipState,
    track_id: &str,
    track_color: gpui::Rgba,
    state: &TimelineState,
    row_height: f32,
    on_select_clip: std::sync::Arc<
        dyn Fn(&(String, bool, bool), &mut gpui::Window, &mut gpui::App) + 'static,
    >,
    on_context_menu: Option<
        std::sync::Arc<dyn Fn(&(String, f32, f32), &mut gpui::Window, &mut gpui::App) + 'static>,
    >,
    on_open_editor: Option<std::sync::Arc<dyn Fn(&mut gpui::Window, &mut gpui::App) + 'static>>,
    on_erase_clip: Option<
        std::sync::Arc<dyn Fn(&String, &mut gpui::Window, &mut gpui::App) + 'static>,
    >,
    erase_target: bool,
) -> impl IntoElement {
    let clip_id = clip.id.clone();
    let drag_clip_id = clip.id.clone();
    let drag_track_id = track_id.to_string();
    let drag_name = clip.name.clone();
    let drag_start_beat = clip.start_beat;
    let selected = state.selection.selected_clip_ids.contains(&clip.id);
    let pixels_per_second = state.viewport.pixels_per_second;
    let seconds_per_beat = state.seconds_per_beat();

    let left = state.beats_to_x(clip.start_beat);
    let width = (clip.duration_beats * seconds_per_beat * pixels_per_second).max(10.0);

    let pad = 7.0;
    let clip_h = row_height - pad * 2.0;
    let note_h = clip_h - 14.0; // height for notes preview

    // Draw notes and controller previews with canvases instead of one GPUI element
    // per note. Dense imported MIDI can contain thousands of notes per clip; the
    // canvas path keeps render cost proportional to visible pixels, not event count.
    let mut note_elements: Vec<gpui::AnyElement> = Vec::new();
    let clip_len = clip.duration_beats;
    if let ClipType::Midi {
        notes,
        controller_lanes,
        ..
    } = &clip.clip_type
    {
        let in_bounds: Vec<MidiNoteState> = notes
            .iter()
            .filter(|n| n.start < clip_len && n.start + n.duration > 0.0)
            .cloned()
            .collect();
        let ppb = pixels_per_second * seconds_per_beat;
        let preview_count = in_bounds.len();
        if !in_bounds.is_empty() {
            note_elements.push(
                midi_note_preview_canvas(in_bounds, clip_len, ppb, track_color)
                    .absolute()
                    .inset_0()
                    .into_any_element(),
            );
        }

        let controller_preview_lanes: Vec<(MidiControllerKind, Vec<MidiControllerPoint>)> =
            controller_lanes
                .iter()
                .filter(|lane| lane.visible && !lane.points.is_empty())
                .take(3)
                .map(|lane| (lane.kind, lane.points.clone()))
                .collect();
        if !controller_preview_lanes.is_empty() {
            let lane_count = controller_preview_lanes.len();
            note_elements.push(
                midi_controller_preview_canvas(controller_preview_lanes.clone(), clip_len, ppb)
                    .absolute()
                    .inset_0()
                    .into_any_element(),
            );
            if width >= 44.0 && note_h >= 18.0 {
                let band_h = controller_preview_band_h(note_h, lane_count);
                let row_h = (band_h / lane_count as f32).max(4.0);
                let band_top = (note_h - band_h - 1.0).max(1.0);
                for (idx, (kind, _)) in controller_preview_lanes.iter().enumerate() {
                    note_elements.push(
                        div()
                            .absolute()
                            .left(px(3.0))
                            .top(px(band_top + idx as f32 * row_h))
                            .text_size(px(7.0))
                            .text_color(Colors::text_faint())
                            .child(midi_controller_kind_label(*kind))
                            .into_any_element(),
                    );
                }
            }
        }

        if midi_debug_enabled() {
            eprintln!(
                "[midi] preview clip={} notes={}/{} len={:.2}",
                clip.id,
                preview_count,
                notes.len(),
                clip_len
            );
        }
    }

    let id_num = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        clip.id.hash(&mut hasher);
        hasher.finish() as usize
    };

    let on_select = on_select_clip.clone();
    let context_clip_id = clip.id.clone();
    let erase_cb = on_erase_clip.clone();
    let ctx_cb = on_context_menu.clone();
    let clip_for_erase = clip.id.clone();

    // Edge-resize drag payloads. The opposite edge stays fixed; the timeline
    // root resolves the new length from the live cursor (see `resize_clip`).
    let resize_left = ClipResizeDrag {
        clip_id: clip.id.clone(),
        edge: ClipEdge::Left,
        start_beat: clip.start_beat,
        duration_beats: clip.duration_beats,
        original: clip.clone(),
    };
    let resize_right = ClipResizeDrag {
        clip_id: clip.id.clone(),
        edge: ClipEdge::Right,
        start_beat: clip.start_beat,
        duration_beats: clip.duration_beats,
        original: clip.clone(),
    };
    const RESIZE_HANDLE_W: f32 = 6.0;

    div()
        .absolute()
        .left(px(left))
        .top(px(pad))
        .w(px(width))
        .h(px(clip_h))
        .rounded_md()
        .bg({
            let mut c = track_color;
            c.a = 0.12;
            c
        })
        .border(px(1.0))
        .border_color(if erase_target {
            Colors::status_error()
        } else if selected {
            Colors::text_primary()
        } else {
            let mut c = track_color;
            c.a = 0.4;
            c
        })
        .cursor(gpui::CursorStyle::OpenHand)
        .id(("midi-clip", id_num))
        .on_mouse_down(
            gpui::MouseButton::Left,
            move |event: &gpui::MouseDownEvent, window, cx| {
                // Stop the parent lane handler from re-selecting the track and
                // clearing this clip selection — the piano roll edits the
                // selected clip, so selection must survive the click.
                cx.stop_propagation();
                let additive = event.modifiers.control || event.modifiers.platform;
                on_select(
                    &(clip_id.clone(), additive, event.modifiers.alt),
                    window,
                    cx,
                );
                if event.click_count >= 2 {
                    if let Some(open) = on_open_editor.as_ref() {
                        open(window, cx);
                    }
                }
            },
        )
        .on_mouse_down(
            gpui::MouseButton::Right,
            move |event: &gpui::MouseDownEvent, window, cx| {
                cx.stop_propagation();
                if let Some(cb) = ctx_cb.as_ref() {
                    let x: f32 = event.position.x.into();
                    let y: f32 = event.position.y.into();
                    cb(&(context_clip_id.clone(), x, y), window, cx);
                } else if let Some(erase) = erase_cb.as_ref() {
                    erase(&clip_for_erase, window, cx);
                }
            },
        )
        .on_drag(
            ClipDragItem {
                clip_id: drag_clip_id,
                source_track_id: drag_track_id,
                start_beat: drag_start_beat,
            },
            move |_drag, _offset, _window, cx| {
                cx.new(
                    |_| crate::components::timeline::audio_clip::ClipDragPreview {
                        name: drag_name.clone(),
                        color: track_color,
                    },
                )
            },
        )
        .flex()
        .flex_col()
        .justify_between()
        // Notes preview area
        .child(div().flex_1().min_h_0().relative().children(note_elements))
        // Bottom Clip Label bar
        .child(
            div()
                .h(px(14.0))
                .bg(Colors::surface_panel_alt()) // dark bar
                .border_t(px(1.0))
                .border_color(Colors::divider())
                .px(px(6.0))
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(9.0))
                        .min_w(px(0.0))
                        .flex_1()
                        .truncate()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(if selected {
                            Colors::text_primary()
                        } else {
                            Colors::text_secondary()
                        })
                        .child(clip.name.clone()),
                ),
            // Clip length text intentionally not rendered on the clip body — the
            // name (flex_1) fills the bar, so no gap remains. Duration stays in the
            // model and the inspector; resize/trim handles are unaffected.
        )
        // Left/right edge resize handles (absolute, on top). Each starts a
        // typed `ClipResizeDrag`; `stop_propagation` keeps the body move-drag
        // and track re-select from also firing on an edge grab.
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .h_full()
                .w(px(RESIZE_HANDLE_W))
                .cursor(gpui::CursorStyle::ResizeLeft)
                .id(("midi-clip-resize-l", id_num))
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_drag(resize_left, |drag, _offset, _window, cx| {
                    cx.new(|_| drag.clone())
                }),
        )
        .child(
            div()
                .absolute()
                .top_0()
                .right_0()
                .h_full()
                .w(px(RESIZE_HANDLE_W))
                .cursor(gpui::CursorStyle::ResizeRight)
                .id(("midi-clip-resize-r", id_num))
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_drag(resize_right, |drag, _offset, _window, cx| {
                    cx.new(|_| drag.clone())
                }),
        )
}

fn midi_note_preview_canvas(
    notes: Vec<MidiNoteState>,
    clip_len: f32,
    ppb: f32,
    track_color: gpui::Rgba,
) -> gpui::Canvas<()> {
    canvas(
        |_bounds, _window, _cx| {},
        move |bounds: Bounds<Pixels>, (), window, _cx| {
            let width: f32 = bounds.size.width.into();
            let height: f32 = bounds.size.height.into();
            if notes.is_empty() || width <= 1.0 || height <= 16.0 || ppb <= 0.0 {
                return;
            }

            let note_area_h = (height - 14.0).max(1.0);
            let visible_start_beat = 0.0_f32;
            let visible_end_beat = (width / ppb).min(clip_len).max(0.0);
            let visible_notes: Vec<_> = notes
                .iter()
                .filter(|note| {
                    note.start < visible_end_beat && note.start + note.duration > visible_start_beat
                })
                .collect();
            if visible_notes.is_empty() {
                return;
            }

            let lo = visible_notes.iter().map(|n| n.pitch).min().unwrap_or(48);
            let hi = visible_notes.iter().map(|n| n.pitch).max().unwrap_or(72);
            let top_pitch = hi.saturating_add(2).min(127);
            let bottom_pitch = lo.saturating_sub(2);
            let pitch_range = (top_pitch as i32 - bottom_pitch as i32).max(12) as f32;

            let mut note_color = track_color;
            note_color.a = 0.86;
            let min_note_w = if ppb < 3.0 { 1.0 } else { 2.0 };
            let note_h = if note_area_h < 30.0 { 1.4 } else { 2.0 };

            // Very dense/zoomed-out MIDI can map many notes to the same pixel.
            // Coalesce to one vertical span per x-column so paint calls stay bounded
            // by clip width rather than note count while preserving the musical mass.
            if ppb < 5.0 || visible_notes.len() > width as usize * 3 {
                let columns = width.ceil().clamp(1.0, 2400.0) as usize;
                let mut spans: Vec<Option<(f32, f32)>> = vec![None; columns];
                for note in visible_notes {
                    let visible_start = note.start.max(0.0);
                    let visible_end = (note.start + note.duration).min(clip_len);
                    if visible_end <= visible_start {
                        continue;
                    }
                    let x0 = (visible_start * ppb)
                        .floor()
                        .clamp(0.0, (columns - 1) as f32) as usize;
                    let x1 = (visible_end * ppb)
                        .ceil()
                        .clamp(x0 as f32, (columns - 1) as f32)
                        as usize;
                    let norm_pitch = (note.pitch as i32 - bottom_pitch as i32) as f32 / pitch_range;
                    let y = (1.0 - norm_pitch) * (note_area_h - 4.0) + 1.0;
                    for cell in &mut spans[x0..=x1] {
                        *cell = Some(match *cell {
                            Some((top, bottom)) => (top.min(y), bottom.max(y + note_h)),
                            None => (y, y + note_h),
                        });
                    }
                }
                for (col, span) in spans.into_iter().enumerate() {
                    if let Some((top, bottom)) = span {
                        window.paint_quad(fill(
                            Bounds::new(
                                bounds.origin + point(px(col as f32), px(top)),
                                size(px(1.0), px((bottom - top).max(1.0))),
                            ),
                            note_color,
                        ));
                    }
                }
                return;
            }

            for note in visible_notes {
                let visible_end = (note.start + note.duration).min(clip_len);
                let visible_start = note.start.max(0.0);
                if visible_end <= visible_start {
                    continue;
                }
                let x = visible_start * ppb;
                let w = ((visible_end - visible_start) * ppb).max(min_note_w);
                let norm_pitch = (note.pitch as i32 - bottom_pitch as i32) as f32 / pitch_range;
                let y = (1.0 - norm_pitch) * (note_area_h - 4.0) + 1.0;
                window.paint_quad(fill(
                    Bounds::new(bounds.origin + point(px(x), px(y)), size(px(w), px(note_h))),
                    note_color,
                ));
            }
        },
    )
}

fn midi_controller_preview_canvas(
    lanes: Vec<(MidiControllerKind, Vec<MidiControllerPoint>)>,
    clip_len: f32,
    ppb: f32,
) -> gpui::Canvas<()> {
    canvas(
        |_bounds, _window, _cx| {},
        move |bounds: Bounds<Pixels>, (), window, _cx| {
            if lanes.is_empty() {
                return;
            }
            let width: f32 = bounds.size.width.into();
            let height: f32 = bounds.size.height.into();
            if width <= 1.0 || height <= 6.0 {
                return;
            }

            let lane_count = lanes.len();
            let band_h = controller_preview_band_h(height, lane_count);
            let row_h = (band_h / lane_count as f32).max(4.0);
            let band_top = (height - band_h - 1.0).max(1.0);
            let columns = width.ceil().clamp(1.0, 1200.0) as usize;
            let step_px = (width / columns as f32).max(1.0);

            for (lane_idx, (kind, points)) in lanes.iter().enumerate() {
                let row_top = band_top + lane_idx as f32 * row_h;
                let default_value = midi_controller_default_value(*kind);
                let baseline_y = row_top + (1.0 - default_value) * (row_h - 2.0).max(1.0) + 1.0;
                let mut line_color = match kind {
                    MidiControllerKind::PitchBend => Colors::accent_purple(),
                    MidiControllerKind::CC(_) => Colors::automation_curve(),
                    MidiControllerKind::ChannelPressure | MidiControllerKind::PolyPressure => {
                        Colors::accent_warning()
                    }
                };
                line_color.a = (0.82 - lane_idx as f32 * 0.14).clamp(0.42, 0.82);
                let mut baseline_color = Colors::text_primary();
                baseline_color.a = 0.12;

                window.paint_quad(fill(
                    Bounds::new(
                        bounds.origin + point(px(0.0), px(baseline_y)),
                        size(px(width), px(1.0)),
                    ),
                    baseline_color,
                ));

                let mut prev_y: Option<f32> = None;
                let mut point_index = 0usize;
                for col in 0..=columns {
                    let x = (col as f32 * step_px).min(width);
                    let beat = if ppb <= 0.0 {
                        0.0
                    } else {
                        (x / ppb).clamp(0.0, clip_len.max(0.0))
                    };
                    let value = evaluate_midi_controller_points_cursor(
                        points,
                        beat,
                        default_value,
                        &mut point_index,
                    );
                    let y = row_top + (1.0 - value) * (row_h - 2.0).max(1.0) + 1.0;
                    if let Some(prev) = prev_y {
                        let top = prev.min(y);
                        let h = (prev - y).abs().max(1.4);
                        window.paint_quad(fill(
                            Bounds::new(
                                bounds.origin + point(px(x), px(top)),
                                size(px(step_px), px(h)),
                            ),
                            line_color,
                        ));
                    }
                    prev_y = Some(y);
                }
            }
        },
    )
}

fn controller_preview_band_h(height: f32, lane_count: usize) -> f32 {
    let min_needed = (lane_count as f32 * 6.0).max(8.0);
    (height * 0.44).clamp(min_needed, 30.0).min(height.max(1.0))
}

fn midi_controller_default_value(kind: MidiControllerKind) -> f32 {
    match kind {
        MidiControllerKind::PitchBend => 0.5,
        MidiControllerKind::CC(_)
        | MidiControllerKind::ChannelPressure
        | MidiControllerKind::PolyPressure => 0.0,
    }
}

fn evaluate_midi_controller_points_cursor(
    points: &[MidiControllerPoint],
    beat: f32,
    default_value: f32,
    point_index: &mut usize,
) -> f32 {
    if points.is_empty() {
        return default_value.clamp(0.0, 1.0);
    }
    let beat = beat.max(0.0);
    if beat <= points[0].beat {
        *point_index = 0;
        return points[0].value.clamp(0.0, 1.0);
    }
    let last = points.len() - 1;
    if beat >= points[last].beat {
        *point_index = last.saturating_sub(1);
        return points[last].value.clamp(0.0, 1.0);
    }

    while *point_index + 1 < points.len() && beat > points[*point_index + 1].beat {
        *point_index += 1;
    }
    while *point_index > 0 && beat < points[*point_index].beat {
        *point_index -= 1;
    }
    let next = (*point_index + 1).min(last);
    let a = &points[*point_index];
    let b = &points[next];
    let span = (b.beat - a.beat).max(1.0e-6);
    let t = ((beat - a.beat) / span).clamp(0.0, 1.0);
    (a.value + (b.value - a.value) * t).clamp(0.0, 1.0)
}

fn midi_controller_kind_label(kind: MidiControllerKind) -> String {
    match kind {
        MidiControllerKind::CC(number) => format!("CC{}", number),
        MidiControllerKind::PitchBend => "PB".to_string(),
        MidiControllerKind::ChannelPressure => "AT".to_string(),
        MidiControllerKind::PolyPressure => "PAT".to_string(),
    }
}
