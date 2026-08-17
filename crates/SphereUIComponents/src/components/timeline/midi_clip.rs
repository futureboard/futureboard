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
    //
    // The preview geometry is resolved here, during element build, and the canvas
    // closures own only that resolved geometry. Handing them the note/controller
    // vectors instead meant a full clone of every note in the clip on every
    // repaint — and the arrangement repaints on each playback tick.
    let mut note_elements: Vec<gpui::AnyElement> = Vec::new();
    let clip_len = clip.duration_beats;
    if let ClipType::Midi {
        notes,
        controller_lanes,
        ..
    } = &clip.clip_type
    {
        let ppb = pixels_per_second * seconds_per_beat;
        // Only the on-screen slice of the clip needs quads. A clip that spans the
        // whole arrangement is mostly scrolled out of view, and the lane already
        // clips it — building spans for the hidden part was pure waste.
        let visible_px = visible_clip_px_range(left, width, state.viewport.viewport_width);
        let preview = visible_px.and_then(|(px_start, px_end)| {
            build_note_preview(notes, clip_len, ppb, px_start, px_end)
        });
        let preview_count = preview.as_ref().map(|p| p.note_count).unwrap_or(0);
        if let Some(preview) = preview {
            note_elements.push(
                midi_note_preview_canvas(preview, track_color)
                    .absolute()
                    .inset_0()
                    .into_any_element(),
            );
        }

        let controller_preview = build_controller_preview(controller_lanes, clip_len, ppb, width);
        if let Some(controller_preview) = controller_preview {
            let lane_kinds = controller_preview.lane_kinds.clone();
            let lane_count = lane_kinds.len();
            note_elements.push(
                midi_controller_preview_canvas(controller_preview)
                    .absolute()
                    .inset_0()
                    .into_any_element(),
            );
            if width >= 44.0 && note_h >= 18.0 {
                let band_h = controller_preview_band_h(note_h, lane_count);
                let row_h = (band_h / lane_count as f32).max(4.0);
                let band_top = (note_h - band_h - 1.0).max(1.0);
                for (idx, kind) in lane_kinds.iter().enumerate() {
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
    };
    let resize_right = ClipResizeDrag {
        clip_id: clip.id.clone(),
        edge: ClipEdge::Right,
        start_beat: clip.start_beat,
        duration_beats: clip.duration_beats,
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

/// Clip-local pixel window that is actually on screen, or `None` when the clip
/// is fully scrolled out. `left` is the clip's x in lane coordinates.
fn visible_clip_px_range(left: f32, width: f32, viewport_width: f32) -> Option<(f32, f32)> {
    let lane_w = viewport_width.max(1.0);
    let start = (-left).max(0.0);
    let end = (lane_w - left).min(width);
    (end > start).then_some((start, end))
}

/// One horizontal pixel column of coalesced note mass. Pitches are normalized
/// (0 = bottom of the clip's pitch span, 1 = top) so the paint pass can map them
/// against the real canvas height without re-reading the notes.
#[derive(Debug, Clone, Copy)]
struct NoteColumn {
    x: f32,
    lowest_norm: f32,
    highest_norm: f32,
}

/// A single note quad, used at zoom levels where notes are individually visible.
#[derive(Debug, Clone, Copy)]
struct NoteQuad {
    x: f32,
    width: f32,
    norm_pitch: f32,
}

/// Resolved note-preview geometry for one clip. Size is bounded by the clip's
/// visible pixel width, never by its note count.
struct NotePreview {
    columns: Vec<NoteColumn>,
    quads: Vec<NoteQuad>,
    note_count: usize,
}

/// Collapse a clip's notes into paintable geometry.
///
/// One allocation-free pass over the notes. Raw MIDI pitches are accumulated
/// into the output slots and normalized afterwards against the clip's full
/// pitch span, which keeps the whole thing single-pass while still mapping
/// pitch from the *entire* clip — so the preview does not shift vertically as
/// the clip scrolls in and out of view.
fn build_note_preview(
    notes: &[MidiNoteState],
    clip_len: f32,
    ppb: f32,
    px_start: f32,
    px_end: f32,
) -> Option<NotePreview> {
    if notes.is_empty() || ppb <= 0.0 || px_end <= px_start {
        return None;
    }

    let visible_start_beat = (px_start / ppb).max(0.0);
    let visible_end_beat = (px_end / ppb).min(clip_len).max(0.0);
    let visible_width = px_end - px_start;

    // Very dense / zoomed-out MIDI maps many notes to the same pixel. Coalesce to
    // one vertical span per x-column so paint calls stay bounded by clip width
    // rather than note count while preserving the musical mass. `notes.len()` is
    // the upper bound on how many land in the window; this is a density heuristic,
    // so the bound is as good as the exact count and costs no extra pass.
    let dense = ppb < 5.0 || notes.len() > (visible_width as usize).saturating_mul(3);
    let columns = visible_width.ceil().clamp(1.0, 2400.0) as usize;
    let mut spans: Vec<Option<(u8, u8)>> = if dense {
        vec![None; columns]
    } else {
        Vec::new()
    };
    let mut raw_quads: Vec<(f32, f32, u8)> = Vec::new();
    let min_note_w = if ppb < 3.0 { 1.0 } else { 2.0 };

    let mut lo = u8::MAX;
    let mut hi = 0u8;
    let mut in_bounds = 0usize;
    for note in notes {
        let start = note.start.max(0.0);
        let end = (note.start + note.duration).min(clip_len);
        if note.start >= clip_len || note.start + note.duration <= 0.0 || end <= start {
            continue;
        }
        // Pitch span covers the whole clip, not just the visible window.
        in_bounds += 1;
        lo = lo.min(note.pitch);
        hi = hi.max(note.pitch);

        if start >= visible_end_beat || end <= visible_start_beat {
            continue;
        }
        if dense {
            let x0 = ((start * ppb) - px_start)
                .floor()
                .clamp(0.0, (columns - 1) as f32) as usize;
            let x1 = ((end * ppb) - px_start)
                .ceil()
                .clamp(x0 as f32, (columns - 1) as f32) as usize;
            for cell in &mut spans[x0..=x1] {
                *cell = Some(match *cell {
                    Some((low, high)) => (low.min(note.pitch), high.max(note.pitch)),
                    None => (note.pitch, note.pitch),
                });
            }
        } else {
            raw_quads.push((
                start * ppb,
                ((end - start) * ppb).max(min_note_w),
                note.pitch,
            ));
        }
    }
    if in_bounds == 0 {
        return None;
    }

    let top_pitch = hi.saturating_add(2).min(127);
    let bottom_pitch = lo.saturating_sub(2);
    let pitch_range = (top_pitch as i32 - bottom_pitch as i32).max(12) as f32;
    let norm_of = |pitch: u8| (pitch as i32 - bottom_pitch as i32) as f32 / pitch_range;

    let columns: Vec<NoteColumn> = spans
        .into_iter()
        .enumerate()
        .filter_map(|(col, span)| {
            span.map(|(low, high)| NoteColumn {
                x: px_start + col as f32,
                lowest_norm: norm_of(low),
                highest_norm: norm_of(high),
            })
        })
        .collect();
    let quads: Vec<NoteQuad> = raw_quads
        .into_iter()
        .map(|(x, width, pitch)| NoteQuad {
            x,
            width,
            norm_pitch: norm_of(pitch),
        })
        .collect();
    if columns.is_empty() && quads.is_empty() {
        return None;
    }
    Some(NotePreview {
        columns,
        quads,
        note_count: in_bounds,
    })
}

fn midi_note_preview_canvas(preview: NotePreview, track_color: gpui::Rgba) -> gpui::Canvas<()> {
    canvas(
        |_bounds, _window, _cx| {},
        move |bounds: Bounds<Pixels>, (), window, _cx| {
            let width: f32 = bounds.size.width.into();
            let height: f32 = bounds.size.height.into();
            if width <= 1.0 || height <= 16.0 {
                return;
            }

            let note_area_h = (height - 14.0).max(1.0);
            let span = (note_area_h - 4.0).max(1.0);
            let note_h = if note_area_h < 30.0 { 1.4 } else { 2.0 };
            let y_of = |norm: f32| (1.0 - norm) * span + 1.0;

            let mut note_color = track_color;
            note_color.a = 0.86;

            for column in &preview.columns {
                // `highest_norm` is the top of the span, so it yields the smaller y.
                let top = y_of(column.highest_norm);
                let bottom = y_of(column.lowest_norm) + note_h;
                window.paint_quad(fill(
                    Bounds::new(
                        bounds.origin + point(px(column.x), px(top)),
                        size(px(1.0), px((bottom - top).max(1.0))),
                    ),
                    note_color,
                ));
            }

            for quad in &preview.quads {
                window.paint_quad(fill(
                    Bounds::new(
                        bounds.origin + point(px(quad.x), px(y_of(quad.norm_pitch))),
                        size(px(quad.width), px(note_h)),
                    ),
                    note_color,
                ));
            }
        },
    )
}

/// Resolved controller-lane geometry: one normalized value per sampled column,
/// so the paint pass never touches the (potentially very dense) point lists.
struct ControllerPreview {
    lane_kinds: Vec<MidiControllerKind>,
    /// Per lane, `columns + 1` normalized values sampled left to right.
    lane_values: Vec<Vec<f32>>,
    columns: usize,
    step_px: f32,
    width: f32,
}

fn build_controller_preview(
    lanes: &[crate::components::timeline::timeline_state::MidiControllerLane],
    clip_len: f32,
    ppb: f32,
    width: f32,
) -> Option<ControllerPreview> {
    if width <= 1.0 {
        return None;
    }
    let columns = width.ceil().clamp(1.0, 1200.0) as usize;
    let step_px = (width / columns as f32).max(1.0);

    let mut lane_kinds = Vec::new();
    let mut lane_values = Vec::new();
    for lane in lanes
        .iter()
        .filter(|lane| lane.visible && !lane.points.is_empty())
        .take(3)
    {
        let default_value = midi_controller_default_value(lane.kind);
        let mut values = Vec::with_capacity(columns + 1);
        let mut point_index = 0usize;
        for col in 0..=columns {
            let x = (col as f32 * step_px).min(width);
            let beat = if ppb <= 0.0 {
                0.0
            } else {
                (x / ppb).clamp(0.0, clip_len.max(0.0))
            };
            values.push(evaluate_midi_controller_points_cursor(
                &lane.points,
                beat,
                default_value,
                &mut point_index,
            ));
        }
        lane_kinds.push(lane.kind);
        lane_values.push(values);
    }

    (!lane_kinds.is_empty()).then_some(ControllerPreview {
        lane_kinds,
        lane_values,
        columns,
        step_px,
        width,
    })
}

fn midi_controller_preview_canvas(preview: ControllerPreview) -> gpui::Canvas<()> {
    canvas(
        |_bounds, _window, _cx| {},
        move |bounds: Bounds<Pixels>, (), window, _cx| {
            let width: f32 = bounds.size.width.into();
            let height: f32 = bounds.size.height.into();
            if width <= 1.0 || height <= 6.0 {
                return;
            }

            let lane_count = preview.lane_kinds.len();
            let band_h = controller_preview_band_h(height, lane_count);
            let row_h = (band_h / lane_count as f32).max(4.0);
            let band_top = (height - band_h - 1.0).max(1.0);
            let usable = (row_h - 2.0).max(1.0);

            for (lane_idx, kind) in preview.lane_kinds.iter().enumerate() {
                let row_top = band_top + lane_idx as f32 * row_h;
                let default_value = midi_controller_default_value(*kind);
                let baseline_y = row_top + (1.0 - default_value) * usable + 1.0;
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

                let values = &preview.lane_values[lane_idx];
                let mut prev_y: Option<f32> = None;
                for col in 0..=preview.columns {
                    let x = (col as f32 * preview.step_px).min(preview.width);
                    let y = row_top + (1.0 - values[col]) * usable + 1.0;
                    if let Some(prev) = prev_y {
                        let top = prev.min(y);
                        let h = (prev - y).abs().max(1.4);
                        window.paint_quad(fill(
                            Bounds::new(
                                bounds.origin + point(px(x), px(top)),
                                size(px(preview.step_px), px(h)),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn note(pitch: u8, start: f32, duration: f32) -> MidiNoteState {
        MidiNoteState::new(pitch, start, duration, 100)
    }

    #[test]
    fn offscreen_clips_build_no_preview_geometry() {
        // Scrolled off to the left, and off to the right of a 1000px lane.
        assert_eq!(visible_clip_px_range(-500.0, 200.0, 1000.0), None);
        assert_eq!(visible_clip_px_range(1200.0, 200.0, 1000.0), None);
    }

    #[test]
    fn partially_visible_clip_is_clipped_to_the_lane() {
        // Clip starts 100px left of the lane and runs 400px: only 0..300 shows.
        let (start, end) = visible_clip_px_range(-100.0, 400.0, 300.0).expect("partially visible");
        assert_eq!((start, end), (100.0, 400.0));
        // Clip starts inside the lane and overruns its right edge.
        let (start, end) = visible_clip_px_range(200.0, 400.0, 300.0).expect("partially visible");
        assert_eq!((start, end), (0.0, 100.0));
    }

    #[test]
    fn dense_preview_stays_bounded_by_visible_pixels_not_note_count() {
        // 20k notes packed into a clip drawn 200px wide — the pathological
        // imported-MIDI case that used to emit one quad per note per frame.
        let notes: Vec<MidiNoteState> = (0..20_000)
            .map(|i| note(48 + (i % 24) as u8, i as f32 * 0.01, 0.05))
            .collect();
        let preview =
            build_note_preview(&notes, 200.0, 1.0, 0.0, 200.0).expect("notes produce a preview");
        assert!(
            preview.quads.is_empty(),
            "dense zoom coalesces into columns"
        );
        assert!(
            preview.columns.len() <= 200,
            "columns bounded by visible width, got {}",
            preview.columns.len()
        );
        assert_eq!(preview.note_count, 20_000);
    }

    #[test]
    fn zoomed_in_preview_draws_one_quad_per_visible_note() {
        let notes = vec![note(60, 0.0, 1.0), note(64, 1.0, 1.0), note(67, 2.0, 1.0)];
        let preview =
            build_note_preview(&notes, 4.0, 40.0, 0.0, 160.0).expect("notes produce a preview");
        assert!(preview.columns.is_empty());
        assert_eq!(preview.quads.len(), 3);
        assert_eq!(preview.quads[0].width, 40.0);
    }

    #[test]
    fn scrolling_culls_notes_without_shifting_the_pitch_mapping() {
        let notes = vec![note(36, 0.0, 1.0), note(96, 100.0, 1.0)];
        // The pitch span must come from the whole clip, or the surviving note
        // would jump vertically as the low note scrolls out of view.
        let full = build_note_preview(&notes, 200.0, 10.0, 0.0, 2000.0).expect("preview");
        let scrolled = build_note_preview(&notes, 200.0, 10.0, 990.0, 1020.0).expect("preview");
        let high_in_full = full
            .quads
            .iter()
            .map(|q| q.norm_pitch)
            .fold(f32::MIN, f32::max);
        assert_eq!(scrolled.quads.len(), 1, "only the high note is on screen");
        assert!(
            (scrolled.quads[0].norm_pitch - high_in_full).abs() < 1.0e-6,
            "pitch mapping must not depend on the scroll window"
        );
    }

    #[test]
    fn empty_and_degenerate_inputs_produce_no_preview() {
        assert!(build_note_preview(&[], 4.0, 40.0, 0.0, 160.0).is_none());
        // Zero zoom, and a clip whose notes all sit outside its own bounds.
        assert!(build_note_preview(&[note(60, 0.0, 1.0)], 4.0, 0.0, 0.0, 160.0).is_none());
        assert!(build_note_preview(&[note(60, 8.0, 1.0)], 4.0, 40.0, 0.0, 160.0).is_none());
    }
}
