use crate::components::timeline::timeline_state::{
    ClipDragItem, ClipEdge, ClipResizeDrag, ClipState, StretchMode, TimelineState, TimelineTool,
};
use crate::components::timeline::waveform_canvas::waveform_canvas;
use crate::theme::Colors;
use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, relative, AppContext, DragMoveEvent, Empty, InteractiveElement, IntoElement,
    ParentElement, Render, StatefulInteractiveElement, Styled, Window,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AudioClipProcessUpdate {
    Gain(f32),
    FadeInMs(f32),
    FadeOutMs(f32),
}

pub type AudioClipProcessPreviewCb = std::sync::Arc<
    dyn Fn(&(String, AudioClipProcessUpdate), &mut gpui::Window, &mut gpui::App) + 'static,
>;
pub type AudioClipProcessCommitCb =
    std::sync::Arc<dyn Fn(&(String, ClipState), &mut gpui::Window, &mut gpui::App) + 'static>;

/// Cut/razor request: `(clip_id, window_x, bypass_snap)`. The timeline resolves
/// `window_x` to a snapped beat and splits the clip there. Optional so callers
/// that never enable the Cut tool can pass `None`.
pub type AudioClipCutCb =
    std::sync::Arc<dyn Fn(&(String, f32, bool), &mut gpui::Window, &mut gpui::App) + 'static>;

/// Authoritative horizontal geometry for an audio clip. Off-mode audio is
/// time-locked to its resolved source trim window, while stretched audio
/// follows its authored musical duration. Track culling and clip rendering
/// must share this result so cut/trim clips cannot be culled at one width and
/// painted at another until a later resize gesture reconciles them.
fn audio_clip_timeline_duration_seconds(clip: &ClipState, state: &TimelineState) -> f32 {
    let seconds_per_beat = state.seconds_per_beat();
    // The clip's own source window and stretch ratio are what it actually plays
    // for; `duration_beats` is derived from exactly this by
    // `TimelineState::reconcile_audio_clip_lengths`. Drawing from the same
    // formula the model is derived from is what keeps the drawn clip and the
    // grabbable clip one object across a tempo change.
    if let Some(seconds) = clip
        .stretch
        .played_seconds_for_project_bpm(state.bpm.max(1.0) as f64)
    {
        return (seconds as f32).max(0.001);
    }
    // Pending and legacy clips may not have decoded source bounds yet.
    (clip.duration_beats * seconds_per_beat).max(0.001)
}

pub(crate) fn audio_clip_timeline_geometry(clip: &ClipState, state: &TimelineState) -> (f32, f32) {
    let pixels_per_second = state.viewport.pixels_per_second;
    let time_locked = clip.stretch.mode == StretchMode::Off;
    let duration_seconds = audio_clip_timeline_duration_seconds(clip, state);
    let left = if time_locked {
        state.time_to_content_x(state.beats_to_seconds(clip.start_beat))
    } else {
        state.beats_to_x(clip.start_beat)
    };
    (left, (duration_seconds * pixels_per_second).max(10.0))
}

#[derive(Clone, Debug)]
struct AudioClipProcessDrag {
    id: String,
}

impl Render for AudioClipProcessDrag {
    fn render(&mut self, _w: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        Empty
    }
}

fn gain_to_db(gain: f32) -> f32 {
    if gain <= 0.000_001 {
        -60.0
    } else {
        (20.0 * gain.log10()).clamp(-60.0, 12.0)
    }
}

fn db_to_gain(db: f32) -> f32 {
    10.0_f32.powf(db.clamp(-60.0, 12.0) / 20.0)
}

fn gain_to_norm(gain: f32) -> f32 {
    let db = gain_to_db(gain);
    if db <= 0.0 {
        ((db + 60.0) / 60.0) * 0.5
    } else {
        0.5 + (db / 12.0) * 0.5
    }
}

fn norm_to_gain(norm: f32) -> f32 {
    let norm = norm.clamp(0.0, 1.0);
    let db = if norm <= 0.5 {
        norm * 120.0 - 60.0
    } else {
        (norm - 0.5) * 24.0
    };
    db_to_gain(db)
}

fn compact_gain_control(
    clip: &ClipState,
    on_preview: AudioClipProcessPreviewCb,
    on_commit: AudioClipProcessCommitCb,
) -> impl IntoElement {
    let value = gain_to_norm(clip.gain).clamp(0.0, 1.0);
    let id = format!("audio-clip-gain-{}", clip.id);
    let move_id = id.clone();
    let preview_id = clip.id.clone();
    let commit_id = clip.id.clone();
    let commit_out_id = clip.id.clone();
    let original = clip.clone();
    let original_out = clip.clone();
    let on_commit_out = on_commit.clone();
    let reset_id = clip.id.clone();
    let reset_preview = on_preview.clone();

    div()
        .id(gpui::ElementId::Name(id.into()))
        .w(px(62.0))
        .h(px(16.0))
        .flex_none()
        .relative()
        .cursor(gpui::CursorStyle::ResizeLeftRight)
        .child(
            div()
                .absolute()
                .left(px(4.0))
                .right(px(4.0))
                .top(px(7.0))
                .h(px(2.0))
                .rounded(px(crate::theme::radius::PILL))
                .bg(Colors::fader_rail())
                .border(px(1.0))
                .border_color(Colors::fader_groove()),
        )
        .child(
            div()
                .absolute()
                .left(relative(value))
                .ml(-px(3.0))
                .top(px(2.0))
                .w(px(6.0))
                .h(px(12.0))
                .rounded(px(crate::theme::radius::CONTROL))
                .bg(Colors::surface_input())
                .border(px(1.0))
                .border_color(Colors::fader_thumb_border())
                .child(
                    div()
                        .absolute()
                        .top(px(2.0))
                        .bottom(px(2.0))
                        .left(px(2.0))
                        .w(px(1.0))
                        .bg(Colors::accent_primary()),
                ),
        )
        .on_mouse_down(gpui::MouseButton::Left, move |event, window, cx| {
            cx.stop_propagation();
            if event.click_count >= 2 {
                // Clip gain is a bipolar dB control, independent of the track
                // volume fader. Its neutral/reset value is always 0 dB.
                reset_preview(
                    &(
                        reset_id.clone(),
                        AudioClipProcessUpdate::Gain(db_to_gain(0.0)),
                    ),
                    window,
                    cx,
                );
            }
        })
        .on_drag(
            AudioClipProcessDrag {
                id: move_id.clone(),
            },
            |drag, _offset, _window, cx| cx.new(|_| drag.clone()),
        )
        .on_drag_move::<AudioClipProcessDrag>(
            move |event: &DragMoveEvent<AudioClipProcessDrag>, window, cx| {
                if event.drag(cx).id != move_id {
                    return;
                }
                let x: f32 = event.event.position.x.into();
                let ox: f32 = event.bounds.origin.x.into();
                let width = f32::from(event.bounds.size.width).max(1.0);
                let gain = norm_to_gain((x - ox) / width);
                on_preview(
                    &(preview_id.clone(), AudioClipProcessUpdate::Gain(gain)),
                    window,
                    cx,
                );
            },
        )
        .on_mouse_up(gpui::MouseButton::Left, move |_, window, cx| {
            on_commit(&(commit_id.clone(), original.clone()), window, cx);
        })
        .on_mouse_up_out(gpui::MouseButton::Left, move |_, window, cx| {
            on_commit_out(&(commit_out_id.clone(), original_out.clone()), window, cx);
        })
}

#[derive(Clone, Copy)]
enum FadeEdge {
    In,
    Out,
}

#[allow(clippy::too_many_arguments)]
fn fade_drag_zone(
    clip: &ClipState,
    edge: FadeEdge,
    clip_duration_seconds: f32,
    fade_width: f32,
    body_height: f32,
    on_preview: AudioClipProcessPreviewCb,
    on_commit: AudioClipProcessCommitCb,
) -> impl IntoElement {
    let edge_name = match edge {
        FadeEdge::In => "in",
        FadeEdge::Out => "out",
    };
    let id = format!("audio-clip-fade-{edge_name}-{}", clip.id);
    let move_id = id.clone();
    let preview_id = clip.id.clone();
    let commit_id = clip.id.clone();
    let commit_out_id = clip.id.clone();
    let original = clip.clone();
    let original_out = clip.clone();
    let on_commit_out = on_commit.clone();

    div()
        .id(gpui::ElementId::Name(id.into()))
        .absolute()
        .top_0()
        .when(matches!(edge, FadeEdge::In), |this| this.left_0())
        .when(matches!(edge, FadeEdge::Out), |this| this.right_0())
        .w(relative(0.5))
        .h(px(10.0))
        .cursor(match edge {
            FadeEdge::In => gpui::CursorStyle::ResizeLeft,
            FadeEdge::Out => gpui::CursorStyle::ResizeRight,
        })
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_drag(
            AudioClipProcessDrag {
                id: move_id.clone(),
            },
            |drag, _offset, _window, cx| cx.new(|_| drag.clone()),
        )
        .on_drag_move::<AudioClipProcessDrag>(
            move |event: &DragMoveEvent<AudioClipProcessDrag>, window, cx| {
                if event.drag(cx).id != move_id {
                    return;
                }
                let x: f32 = event.event.position.x.into();
                let ox: f32 = event.bounds.origin.x.into();
                let width = f32::from(event.bounds.size.width).max(1.0);
                let ratio = match edge {
                    FadeEdge::In => ((x - ox) / width).clamp(0.0, 1.0),
                    FadeEdge::Out => (1.0 - (x - ox) / width).clamp(0.0, 1.0),
                };
                let ms = ratio * clip_duration_seconds.max(0.001) * 500.0;
                let update = match edge {
                    FadeEdge::In => AudioClipProcessUpdate::FadeInMs(ms),
                    FadeEdge::Out => AudioClipProcessUpdate::FadeOutMs(ms),
                };
                on_preview(&(preview_id.clone(), update), window, cx);
            },
        )
        .on_mouse_up(gpui::MouseButton::Left, move |_, window, cx| {
            on_commit(&(commit_id.clone(), original.clone()), window, cx);
        })
        .on_mouse_up_out(gpui::MouseButton::Left, move |_, window, cx| {
            on_commit_out(&(commit_out_id.clone(), original_out.clone()), window, cx);
        })
        .child(
            div()
                .absolute()
                .top_0()
                .when(matches!(edge, FadeEdge::In), |this| {
                    this.left(px(fade_width.max(0.0) - 3.0))
                })
                .when(matches!(edge, FadeEdge::Out), |this| {
                    this.right(px(fade_width.max(0.0) - 3.0))
                })
                .w(px(6.0))
                .h(px(6.0))
                .rounded(px(crate::theme::radius::CONTROL))
                .bg(Colors::surface_input())
                .border(px(1.0))
                .border_color(Colors::accent_primary()),
        )
        .child(
            div()
                .absolute()
                .top(px(5.0))
                .when(matches!(edge, FadeEdge::In), |this| this.left_0())
                .when(matches!(edge, FadeEdge::Out), |this| this.right_0())
                .w(px(fade_width.max(0.0)))
                .h(px((body_height - 5.0).max(1.0)))
                .border_color(Colors::with_alpha(Colors::accent_primary(), 0.55))
                .when(matches!(edge, FadeEdge::In), |this| this.border_r(px(1.0)))
                .when(matches!(edge, FadeEdge::Out), |this| this.border_l(px(1.0))),
        )
}

pub struct ClipDragPreview {
    pub name: String,
    pub color: gpui::Rgba,
}

impl Render for ClipDragPreview {
    fn render(&mut self, _w: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .h(px(24.0))
            .min_w(px(96.0))
            .max_w(px(220.0))
            .px(px(8.0))
            .rounded(px(crate::theme::radius::CONTROL))
            .border(px(1.0))
            .border_color({
                let mut c = self.color;
                c.a = 0.7;
                c
            })
            .bg(Colors::surface_raised())
            .shadow_lg()
            .child(
                div()
                    .min_w(px(0.0))
                    .flex_1()
                    .truncate()
                    .text_size(px(10.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(Colors::text_primary())
                    .child(self.name.clone()),
            )
    }
}

/// What the clip's processing strip says about its stretch state.
///
/// `locked` is the tempo-follow flag, not "has a ratio": a Tempo Sync clip's
/// bar count belongs to the project tempo, so its badge has to read differently
/// from a Manual clip that merely happens to sit at some ratio right now. The
/// caller paints the two on separate channels — colour *and* glyph — because
/// "is this clip pinned to the grid?" is the question a tempo change makes you
/// ask about every clip at once.
struct StretchBadge {
    label: String,
    locked: bool,
}

fn stretch_badge(clip: &ClipState, state: &TimelineState) -> Option<StretchBadge> {
    if clip.stretch.mode == StretchMode::Off {
        return None;
    }
    let locked = clip.stretch.follows_project_tempo();
    if let Some(source_bpm) = clip.stretch.bpm_source {
        return Some(StretchBadge {
            label: format!("{source_bpm:.0}->{:.0}", state.bpm),
            locked,
        });
    }
    let ratio = clip.stretch.effective_time_ratio(state.bpm as f64);
    let label = if (ratio - 1.0).abs() > 0.001 {
        format!("x{ratio:.2}")
    } else {
        "Stretch".to_string()
    };
    Some(StretchBadge { label, locked })
}

pub fn audio_clip(
    clip: &ClipState,
    track_id: &str,
    track_color: gpui::Rgba,
    state: &TimelineState,
    row_height: f32,
    on_select_clip: std::sync::Arc<
        dyn Fn(&(String, bool, bool), &mut gpui::Window, &mut gpui::App) + 'static,
    >,
    on_open_editor: Option<std::sync::Arc<dyn Fn(&mut gpui::Window, &mut gpui::App) + 'static>>,
    _on_context_menu: Option<
        std::sync::Arc<dyn Fn(&(String, f32, f32), &mut gpui::Window, &mut gpui::App) + 'static>,
    >,
    on_erase_clip: Option<
        std::sync::Arc<dyn Fn(&String, &mut gpui::Window, &mut gpui::App) + 'static>,
    >,
    on_cut_clip: Option<AudioClipCutCb>,
    erase_target: bool,
    auto_crossfade_in_beats: f32,
    auto_crossfade_out_beats: f32,
    on_process_preview: AudioClipProcessPreviewCb,
    on_process_commit: AudioClipProcessCommitCb,
) -> impl IntoElement {
    let _s = crate::perf::PerfScope::enter("AudioClip");
    let clip_id = clip.id.clone();
    let drag_clip_id = clip.id.clone();
    let drag_track_id = track_id.to_string();
    let drag_name = clip.name.clone();
    let drag_start_beat = clip.start_beat;
    let selected = state.selection.selected_clip_ids.contains(&clip.id);
    let pixels_per_second = state.viewport.pixels_per_second;
    let seconds_per_beat = state.seconds_per_beat();
    let stretch_badge = stretch_badge(clip, state);
    let (left, width) = audio_clip_timeline_geometry(clip, state);
    let clip_duration_seconds = audio_clip_timeline_duration_seconds(clip, state);
    let fade_in_seconds = (clip.stretch.fade_in_ms.max(0.0) / 1000.0)
        .max(auto_crossfade_in_beats.max(0.0) * seconds_per_beat)
        .min(clip_duration_seconds);
    let fade_out_seconds = (clip.stretch.fade_out_ms.max(0.0) / 1000.0)
        .max(auto_crossfade_out_beats.max(0.0) * seconds_per_beat)
        .min((clip_duration_seconds - fade_in_seconds).max(0.0));
    let fade_in_w = (fade_in_seconds * pixels_per_second).min(width);
    let fade_out_w = (fade_out_seconds * pixels_per_second).min((width - fade_in_w).max(0.0));
    let manual_fade_in_w =
        ((clip.stretch.fade_in_ms.max(0.0) / 1000.0) * pixels_per_second).min(width * 0.5);
    let manual_fade_out_w =
        ((clip.stretch.fade_out_ms.max(0.0) / 1000.0) * pixels_per_second).min(width * 0.5);
    let has_auto_crossfade = auto_crossfade_in_beats > 0.0 || auto_crossfade_out_beats > 0.0;
    let show_inline_gain = selected
        && width
            >= if has_auto_crossfade || stretch_badge.is_some() {
                220.0
            } else {
                150.0
            };
    let gain_db = gain_to_db(clip.gain);

    // Geometry offsets matching layout
    let pad = 7.0;
    let clip_h = row_height - pad * 2.0;

    let id_num = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        clip.id.hash(&mut hasher);
        hasher.finish() as usize
    };

    let on_select = on_select_clip.clone();
    let open_editor = on_open_editor.clone();
    let clip_for_erase = clip.id.clone();
    let erase_cb = on_erase_clip.clone();
    let active_tool = state.active_tool;
    let cut_cb = on_cut_clip.clone();
    let clip_for_cut = clip.id.clone();
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
    const HEADER_H: f32 = 20.0;
    let body_h = (clip_h - HEADER_H).max(1.0);
    let gain_preview = on_process_preview.clone();
    let gain_commit = on_process_commit.clone();
    let fade_in_preview = on_process_preview.clone();
    let fade_in_commit = on_process_commit.clone();
    let fade_out_preview = on_process_preview;
    let fade_out_commit = on_process_commit;

    div()
        .absolute()
        .left(px(left))
        .top(px(pad))
        .w(px(width))
        .h(px(clip_h))
        .rounded(px(crate::theme::radius::CONTROL))
        .overflow_hidden()
        .bg(Colors::timeline_audio_clip_fill(track_color, selected))
        .border(px(1.0))
        .border_color(if erase_target {
            Colors::status_error()
        } else {
            Colors::timeline_audio_clip_border(track_color, selected)
        })
        .cursor(if active_tool == TimelineTool::Cut {
            // Cut tool: a click splits (never drags), so drop the move cursor.
            if active_tool == TimelineTool::Pen {
                gpui::CursorStyle::Crosshair
            } else {
                gpui::CursorStyle::Arrow
            }
        } else {
            gpui::CursorStyle::OpenHand
        })
        .id(("audio-clip", id_num))
        .on_mouse_down(
            gpui::MouseButton::Left,
            move |event: &gpui::MouseDownEvent, window, cx| {
                cx.stop_propagation();
                // Cut/razor tool: a click splits the clip at the cursor instead
                // of selecting/opening it. The timeline resolves the window x to
                // a snapped beat (Shift bypasses snap, matching the lane tools).
                if active_tool == TimelineTool::Cut {
                    if let Some(cut) = cut_cb.as_ref() {
                        let x: f32 = event.position.x.into();
                        cut(
                            &(clip_for_cut.clone(), x, event.modifiers.shift),
                            window,
                            cx,
                        );
                    }
                    return;
                }
                let additive = event.modifiers.control || event.modifiers.platform;
                on_select(
                    &(clip_id.clone(), additive, event.modifiers.alt),
                    window,
                    cx,
                );
                if event.click_count >= 2 {
                    if let Some(open) = open_editor.as_ref() {
                        open(window, cx);
                    }
                }
            },
        )
        .on_mouse_down(
            gpui::MouseButton::Right,
            move |_event: &gpui::MouseDownEvent, window, cx| {
                cx.stop_propagation();
                if let Some(erase) = erase_cb.as_ref() {
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
                cx.new(|_| ClipDragPreview {
                    name: drag_name.clone(),
                    color: track_color,
                })
            },
        )
        .flex()
        .flex_col()
        .justify_between()
        // Waveform preview area
        .child(div().flex_1().min_h_0().child(waveform_canvas(
            clip,
            track_color,
            state,
            left,
            width,
        )))
        // Processing strip: clip identity, inline gain, crossfade, and stretch.
        .child(
            div()
                .h(px(HEADER_H))
                .flex_none()
                .bg(if selected {
                    Colors::surface_selected_soft()
                } else {
                    Colors::surface_panel_alt()
                })
                .border_t(px(1.0))
                .border_color(Colors::divider())
                .pl(px(6.0))
                .pr(px(4.0))
                .flex()
                .items_center()
                .gap(px(5.0))
                .child(
                    div()
                        .w(px(2.0))
                        .h(px(10.0))
                        .rounded(px(crate::theme::radius::CONTROL))
                        .bg(track_color),
                )
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
                )
                .children(
                    show_inline_gain.then(|| compact_gain_control(clip, gain_preview, gain_commit)),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(8.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(if gain_db.abs() > 0.05 {
                            Colors::accent_primary()
                        } else {
                            Colors::text_muted()
                        })
                        .child(format!("{gain_db:+.1} dB")),
                )
                .children(has_auto_crossfade.then(|| {
                    div()
                        .flex_none()
                        .px(px(4.0))
                        .rounded(px(crate::theme::radius::CONTROL))
                        .bg(Colors::accent_soft())
                        .text_size(px(7.5))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(Colors::accent_primary())
                        .child("XFADE")
                }))
                .children(stretch_badge.map(|badge| {
                    // A tempo-locked clip is marked on two channels: the
                    // automation hue (the same one the tempo lane uses, because
                    // that is what owns the clip's length) plus a leading glyph.
                    // A merely-stretched clip keeps the accent and no glyph.
                    let hue = if badge.locked {
                        Colors::state_automation()
                    } else {
                        Colors::accent_primary()
                    };
                    let mut chip = div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(2.0))
                        .flex_none()
                        .px(px(4.0))
                        .rounded(px(crate::theme::radius::CONTROL))
                        .bg(Colors::with_alpha(hue, 0.14))
                        .text_size(px(8.0))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(hue);
                    if badge.locked {
                        chip = chip.child(
                            gpui::svg()
                                .path(crate::assets::ICON_MAGNET_PATH)
                                .w(px(8.0))
                                .h(px(8.0))
                                .text_color(hue),
                        );
                    }
                    chip.child(badge.label)
                })),
            // Clip length text intentionally not rendered on the clip body — the
            // name (flex_1) fills the bar, so no gap remains. Duration stays in the
            // model and the inspector; resize/trim handles are unaffected.
        )
        .children((fade_in_w > 1.0).then(|| {
            div()
                .absolute()
                .left_0()
                .top_0()
                .bottom(px(HEADER_H))
                .w(px(fade_in_w))
                .bg(Colors::with_alpha(Colors::surface_panel_alt(), 0.34))
                .border_r(px(1.0))
                .border_color(if auto_crossfade_in_beats > 0.0 {
                    Colors::accent_primary()
                } else {
                    Colors::with_alpha(track_color, 0.55)
                })
        }))
        .children((fade_out_w > 1.0).then(|| {
            div()
                .absolute()
                .right_0()
                .top_0()
                .bottom(px(HEADER_H))
                .w(px(fade_out_w))
                .bg(Colors::with_alpha(Colors::surface_panel_alt(), 0.34))
                .border_l(px(1.0))
                .border_color(if auto_crossfade_out_beats > 0.0 {
                    Colors::accent_primary()
                } else {
                    Colors::with_alpha(track_color, 0.55)
                })
        }))
        .children((selected && auto_crossfade_in_beats <= 0.0).then(|| {
            fade_drag_zone(
                clip,
                FadeEdge::In,
                clip_duration_seconds,
                manual_fade_in_w,
                body_h,
                fade_in_preview,
                fade_in_commit,
            )
        }))
        .children((selected && auto_crossfade_out_beats <= 0.0).then(|| {
            fade_drag_zone(
                clip,
                FadeEdge::Out,
                clip_duration_seconds,
                manual_fade_out_w,
                body_h,
                fade_out_preview,
                fade_out_commit,
            )
        }))
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .h_full()
                .w(px(RESIZE_HANDLE_W))
                .cursor(gpui::CursorStyle::ResizeLeft)
                .id(("audio-clip-resize-l", id_num))
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
                .id(("audio-clip-resize-r", id_num))
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_drag(resize_right, |drag, _offset, _window, cx| {
                    cx.new(|_| drag.clone())
                }),
        )
}

#[cfg(test)]
mod tests {
    use super::{audio_clip_timeline_geometry, db_to_gain, gain_to_db, gain_to_norm, norm_to_gain};
    use crate::components::timeline::timeline_state::{
        AudioClipStretchState, AudioImportState, ClipState, ClipType, TimelineState,
    };

    #[test]
    fn clip_gain_mapping_roundtrips_unity_and_limits() {
        assert!((gain_to_db(1.0) - 0.0).abs() < 1.0e-6);
        assert!((gain_to_norm(1.0) - 0.5).abs() < 1.0e-6);
        assert!((norm_to_gain(gain_to_norm(1.0)) - 1.0).abs() < 1.0e-5);
        assert!((db_to_gain(12.0) - 10.0_f32.powf(12.0 / 20.0)).abs() < 1.0e-5);
        assert_eq!(gain_to_db(0.0), -60.0);
    }

    #[test]
    fn timeline_geometry_uses_trimmed_source_window_not_full_asset_duration() {
        let state = TimelineState::default();
        let mut clip = ClipState {
            id: "clip-trimmed".to_string(),
            name: "Trimmed".to_string(),
            start_beat: 2.0,
            duration_beats: 4.0,
            source_duration_seconds: Some(30.0),
            offset_beats: 0.0,
            gain: 1.0,
            clip_type: ClipType::Audio {
                file_id: "asset".to_string(),
                source_path: Some("take.wav".to_string()),
            },
            muted: false,
            audio_import: AudioImportState::Ready,
            stretch: AudioClipStretchState::default(),
            ara: None,
        };
        clip.stretch.original_sample_rate = 48_000;
        clip.stretch.source_start_samples = 48_000;
        clip.stretch.source_end_samples = 144_000;

        let (left, width) = audio_clip_timeline_geometry(&clip, &state);
        assert!((left - 150.0).abs() < 0.001);
        assert!((width - 300.0).abs() < 0.001);

        clip.stretch.source_end_samples = 96_000;
        let (_, trimmed_width) = audio_clip_timeline_geometry(&clip, &state);
        assert!((trimmed_width - 150.0).abs() < 0.001);
    }

    /// Build a two-second audio clip at 48 kHz, un-stretched.
    fn two_second_clip(id: &str) -> ClipState {
        let mut clip = ClipState {
            id: id.to_string(),
            name: "Take".to_string(),
            start_beat: 0.0,
            duration_beats: 4.0,
            source_duration_seconds: Some(2.0),
            offset_beats: 0.0,
            gain: 1.0,
            clip_type: ClipType::Audio {
                file_id: "asset".to_string(),
                source_path: Some("take.wav".to_string()),
            },
            muted: false,
            audio_import: AudioImportState::Ready,
            stretch: AudioClipStretchState::default(),
            ara: None,
        };
        clip.stretch.original_sample_rate = 48_000;
        clip.stretch.project_sample_rate = 48_000;
        clip.stretch.original_duration_samples = 96_000;
        clip.stretch.source_start_samples = 0;
        clip.stretch.source_end_samples = 96_000;
        clip
    }

    fn state_with_clip(clip: ClipState, bpm: f32) -> TimelineState {
        let mut state = TimelineState::default();
        state.bpm = bpm;
        let track_id = state.create_audio_track();
        let track = state
            .tracks
            .iter_mut()
            .find(|t| t.id == track_id)
            .expect("track");
        track.clips.push(clip);
        state.reconcile_audio_clip_lengths();
        state
    }

    /// An un-stretched clip is two seconds of audio at any tempo. Doubling the
    /// project tempo must double its bar count, not squeeze the audio.
    #[test]
    fn an_unstretched_clip_keeps_its_seconds_when_the_tempo_changes() {
        let mut state = state_with_clip(two_second_clip("clip-off"), 120.0);
        let before = state.tracks[0].clips[0].duration_beats;
        assert!((before - 4.0).abs() < 0.01, "2 s at 120 BPM is 4 beats");

        state.bpm = 240.0;
        assert!(state.reconcile_audio_clip_lengths());
        let after = state.tracks[0].clips[0].duration_beats;
        assert!(
            (after - 8.0).abs() < 0.01,
            "2 s at 240 BPM is 8 beats, got {after}"
        );
    }

    /// A tempo-synced clip is *defined* in bars. Doubling the tempo must leave
    /// its bar count alone — that is what "locked to the timeline" means.
    #[test]
    fn a_tempo_synced_clip_keeps_its_bars_when_the_tempo_changes() {
        let mut clip = two_second_clip("clip-sync");
        clip.stretch.mode = crate::components::timeline::timeline_state::StretchMode::TempoSync;
        clip.stretch.bpm_source = Some(120.0);
        clip.stretch.apply_tempo_sync(120.0);
        let mut state = state_with_clip(clip, 120.0);
        let before = state.tracks[0].clips[0].duration_beats;

        state.bpm = 240.0;
        state
            .tracks
            .iter_mut()
            .flat_map(|t| t.clips.iter_mut())
            .for_each(|c| c.stretch.apply_tempo_sync(240.0));
        state.reconcile_audio_clip_lengths();
        let after = state.tracks[0].clips[0].duration_beats;
        assert!(
            (after - before).abs() < 0.05,
            "a tempo-synced clip keeps {before} beats, got {after}"
        );
    }

    /// The drawn width and the model's bar count describe one object: after a
    /// tempo change the clip must be grabbable exactly where it is painted.
    #[test]
    fn the_drawn_width_and_the_model_agree_after_a_tempo_change() {
        let mut state = state_with_clip(two_second_clip("clip-agree"), 120.0);
        state.bpm = 172.0;
        state.reconcile_audio_clip_lengths();

        let clip = &state.tracks[0].clips[0];
        let (_, drawn_w) = audio_clip_timeline_geometry(clip, &state);
        let model_w =
            clip.duration_beats * state.seconds_per_beat() * state.viewport.pixels_per_second;
        assert!(
            (drawn_w - model_w).abs() < 0.5,
            "drawn {drawn_w} px vs model {model_w} px"
        );
    }

    /// A clip whose source has not been decoded yet has nothing authoritative
    /// to derive from and must be left where it is.
    #[test]
    fn a_pending_clip_is_left_alone() {
        let mut clip = two_second_clip("clip-pending");
        clip.stretch.source_end_samples = 0;
        clip.stretch.original_duration_samples = 0;
        clip.audio_import = AudioImportState::Pending;
        let mut state = state_with_clip(clip, 120.0);
        state.bpm = 240.0;
        assert!(!state.reconcile_audio_clip_lengths());
        assert!((state.tracks[0].clips[0].duration_beats - 4.0).abs() < 1.0e-6);
    }
}
