//! Audio Jam.
//!
//! The room, as a Studio surface: who is in it, what they are publishing, what
//! the link between them looks like, and one button per remote stream that
//! turns it into a track.
//!
//! Region contract — owner: this window. State owner:
//! [`crate::jam`] (a process-wide snapshot the network threads publish and this
//! window only reads). Coordinate space: window-local. Size source: fixed
//! dialog bounds. Scroll owner: the participant list. Clip owner: the same
//! list. Layer order: titlebar, header, list, footer. Focus: window-level, with
//! Escape closing.
//!
//! Nothing here touches audio. The panel reads a snapshot the jam controller
//! refreshes on a timer, at a bounded rate — a meter that repainted per packet
//! would make a busy jam a rerender storm.

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    div, px, size, App, AppContext, Bounds, Context, FocusHandle, InteractiveElement, IntoElement,
    KeyDownEvent, ParentElement, Render, StatefulInteractiveElement, Styled, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind,
};

use crate::components::controls::{fb_badge, fb_button, fb_section_header, FbButtonKind};
use crate::components::title_bar::external_window_titlebar;
use crate::jam::{self, JamStreamView, JamUiState};
use crate::theme::{self, radius, space, typography, Colors};
use crate::window_position::{apply_owner_display, centered_window_bounds};

pub const JAM_WINDOW_WIDTH: f32 = 460.0;
pub const JAM_WINDOW_HEIGHT: f32 = 620.0;

/// How often the panel re-reads the jam snapshot.
///
/// 30 Hz is the rate the rest of Studio meters at. Anything faster would repaint
/// for packets nobody can see arriving; anything slower makes a level meter look
/// broken.
const REFRESH: Duration = Duration::from_millis(33);

/// What the window asks the shell to do.
///
/// The window never edits the project itself: making a track and routing it are
/// the shell's job, and the routing goes through Audio Connections like any
/// other input rather than through a private jam-only path.
#[derive(Debug, Clone)]
pub struct CreateTrackFromStream {
    pub stream_id: String,
    pub track_name: String,
    /// The Audio Connections device id for the stream, `jam:<stream_id>`.
    pub device_id: String,
    pub channels: usize,
    /// Per-channel labels, so the bus's ports carry the publisher's own names.
    pub channel_labels: Vec<String>,
}

/// What the shell hands the window so a Create Track button can reach it.
pub type CreateTrackHandler = Arc<dyn Fn(CreateTrackFromStream, &mut App) + Send + Sync>;

pub struct JamWindow {
    focus_handle: FocusHandle,
    state: JamUiState,
    on_create_track: CreateTrackHandler,
    /// What the user typed for a new jam's name.
    jam_name: String,
    busy: Option<String>,
}

impl JamWindow {
    fn new(on_create_track: CreateTrackHandler, cx: &mut Context<Self>) -> Self {
        Self::spawn_refresh(cx);
        Self {
            focus_handle: cx.focus_handle(),
            state: jam::snapshot(),
            on_create_track,
            jam_name: default_jam_name(),
            busy: None,
        }
    }

    fn spawn_refresh(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(REFRESH).await;
            let alive = this
                .update(cx, |this, cx| {
                    // Read only. The controller's own poll thread advances the
                    // published state, so closing this window does not stop a
                    // jam that tracks are still listening to.
                    let next = jam::snapshot();
                    let changed = next.state_label != this.state.state_label
                        || next.streams.len() != this.state.streams.len()
                        || next.participants.len() != this.state.participants.len();
                    this.state = next;
                    if changed {
                        this.busy = None;
                    }
                    cx.notify();
                })
                .is_ok();
            if !alive {
                break;
            }
        })
        .detach();
    }

    fn create_jam(&mut self, cx: &mut Context<Self>) {
        let name = if self.jam_name.trim().is_empty() {
            default_jam_name()
        } else {
            self.jam_name.trim().to_string()
        };
        self.busy = Some("Creating…".to_string());
        // The REST call is blocking and short; running it here would stall a
        // frame, so it goes to the background pool and the panel picks the
        // result up from the snapshot.
        cx.background_executor()
            .spawn(async move {
                if let Err(error) =
                    jam::with_controller(|controller| controller.create_and_join(&name).map(|_| ()))
                {
                    eprintln!("[jam] create failed: {error}");
                }
            })
            .detach();
    }

    fn leave(&mut self, cx: &mut Context<Self>) {
        self.busy = Some("Leaving…".to_string());
        cx.background_executor()
            .spawn(async move {
                if let Err(error) = jam::with_controller(|controller| controller.leave()) {
                    eprintln!("[jam] leave failed: {error}");
                }
            })
            .detach();
    }

    fn publish_master(&mut self, cx: &mut Context<Self>) {
        self.busy = Some("Publishing…".to_string());
        cx.background_executor()
            .spawn(async move {
                if let Err(error) =
                    jam::with_controller(|controller| controller.publish_master("Studio Master"))
                {
                    eprintln!("[jam] publish failed: {error}");
                }
            })
            .detach();
    }

    fn create_invite(&mut self, cx: &mut Context<Self>) {
        self.busy = Some("Minting invite…".to_string());
        cx.background_executor()
            .spawn(async move {
                match jam::with_controller(|controller| controller.create_invite("performer")) {
                    // The link is a bearer secret: it is shown once so it can be
                    // copied and is never written to a log.
                    Ok(_) => {}
                    Err(error) => eprintln!("[jam] invite failed: {}", error.user_message()),
                }
            })
            .detach();
    }

    fn create_track(&mut self, stream: &JamStreamView, cx: &mut App) {
        (self.on_create_track)(
            CreateTrackFromStream {
                stream_id: stream.stream_id.clone(),
                track_name: track_name_for(stream),
                device_id: stream.device_id(),
                channels: stream.channels.max(1),
                channel_labels: stream.channel_labels.clone(),
            },
            cx,
        );
    }
}

impl Render for JamWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(Colors::surface_base())
            .text_color(Colors::text_primary())
            .font(theme::ui_font())
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|_this, event: &KeyDownEvent, window, _cx| {
                if event.keystroke.key.as_str() == "escape" {
                    window.remove_window();
                }
            }))
            .child(external_window_titlebar(
                "Audio Jam",
                "jam-window-close",
                move |window, _cx| {
                    window.remove_window();
                },
            ))
            .child(self.header(&state))
            .child(self.body(&state, cx))
            .child(self.footer(&state, cx))
    }
}

impl JamWindow {
    /// Connection state, region, transport and round trip — the four numbers
    /// that answer "is this usable right now".
    fn header(&self, state: &JamUiState) -> impl IntoElement {
        let (tone, label) = status_tone(state);
        let rtt = if state.rtt_ms > 0.0 {
            format!("{:.1} ms", state.rtt_ms)
        } else {
            "—".to_string()
        };
        let clock = if state.clock_locked {
            format!(
                "{:+.1} ms · {:+.1} ppm",
                state.clock_offset_ms, state.clock_drift_ppm
            )
        } else {
            "not locked".to_string()
        };

        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(space::SNUG))
            .px(px(space::SECTION))
            .py(px(space::LOOSE))
            .border_b(px(1.0))
            .border_color(Colors::border_subtle())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(typography::UI_TITLE))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(if state.jam_name.is_empty() {
                                "No jam".to_string()
                            } else {
                                state.jam_name.clone()
                            }),
                    )
                    .child(fb_badge(label, tone)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(space::BLOCK))
                    .child(stat("Code", &or_dash(&state.public_id)))
                    .child(stat("Region", &or_dash(&state.region_label)))
                    .child(stat("Transport", &or_dash(&state.transport_label)))
                    .child(stat("RTT", &rtt)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(space::BLOCK))
                    .child(stat("Clock", &clock))
                    .child(stat(
                        "Packets",
                        &format!("{} in · {} out", state.packets_in, state.packets_out),
                    )),
            )
    }

    /// Participants, each with the streams they publish.
    fn body(&self, state: &JamUiState, cx: &mut Context<Self>) -> impl IntoElement {
        let mut list = div()
            .id("jam-participants")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .gap(px(space::LOOSE))
            .px(px(space::SECTION))
            .py(px(space::LOOSE))
            .overflow_y_scroll();

        if let Some(error) = state.last_error.as_ref() {
            list = list.child(notice(error, Colors::status_error()));
        }
        if !state.signed_in {
            list = list.child(notice(
                "Sign in to your Futureboard account to join a jam.",
                Colors::status_warning(),
            ));
        }
        if let Some(link) = state.invite_link.as_ref() {
            list = list.child(notice(link, Colors::accent_primary()));
        }

        if state.participants.is_empty() {
            list = list.child(
                div()
                    .py(px(space::BLOCK))
                    .text_size(px(typography::UI_SM))
                    .text_color(Colors::text_muted())
                    .child(if state.connected {
                        "Nobody else is here yet. Invite someone with the link below."
                    } else {
                        "Create a jam, or open an invite link, to start playing together."
                    }),
            );
            return list;
        }

        list = list.child(fb_section_header("Participants"));
        for (participant, streams) in state.by_participant() {
            let mut row = div()
                .flex()
                .flex_col()
                .gap(px(space::TIGHT))
                .p(px(space::LOOSE))
                .rounded(px(radius::SURFACE))
                .bg(Colors::surface_panel())
                .border_1()
                .border_color(Colors::border_subtle());

            let online = participant.connection_state == "connected";
            row = row.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(typography::UI_SM))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(participant.user.handle()),
                            )
                            .child(
                                div()
                                    .text_size(px(typography::DENSE_CAPTION))
                                    .text_color(Colors::text_muted())
                                    .child(if participant.device_name.is_empty() {
                                        participant.role.clone()
                                    } else {
                                        format!(
                                            "{} · {}",
                                            participant.device_name, participant.role
                                        )
                                    }),
                            ),
                    )
                    .child(fb_badge(
                        if online { "Online" } else { "Offline" },
                        if online {
                            Colors::status_success()
                        } else {
                            Colors::text_muted()
                        },
                    )),
            );

            if streams.is_empty() {
                row = row.child(
                    div()
                        .text_size(px(typography::DENSE_CAPTION))
                        .text_color(Colors::text_faint())
                        .child("Not publishing anything yet"),
                );
            }
            for stream in streams {
                row = row.child(self.stream_row(stream, cx));
            }
            list = list.child(row);
        }
        list
    }

    /// One stream: its name, its format, its level, and the button that turns it
    /// into a track.
    fn stream_row(&self, stream: &JamStreamView, cx: &mut Context<Self>) -> impl IntoElement {
        let layout = match stream.channels {
            0 | 1 => "Mono",
            2 => "Stereo",
            _ => "Multi",
        };
        let format = format!(
            "{} {} kHz · {}",
            stream.codec.to_uppercase(),
            stream.sample_rate as f32 / 1000.0,
            layout
        );
        let queued = stream.clone();
        let button_id = gpui::ElementId::Name(format!("jam-create-{}", stream.stream_id).into());

        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(space::BASE))
            .mt(px(space::TIGHT))
            .pl(px(space::BASE))
            .border_l(px(2.0))
            .border_color(if stream.receiving {
                Colors::accent_primary()
            } else {
                Colors::border_subtle()
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w(px(0.0))
                    .child(
                        div()
                            .text_size(px(typography::UI_SM))
                            .child(stream.stream_name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(typography::DENSE_CAPTION))
                            .text_color(Colors::text_muted())
                            .child(format),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(space::BASE))
                    .child(level_meter(stream.peak))
                    .child(fb_button(
                        button_id,
                        "Create Track",
                        FbButtonKind::Default,
                        stream.receiving,
                        cx.listener(move |this, _event, _window, cx| {
                            this.create_track(&queued, cx);
                            cx.notify();
                        }),
                    )),
            )
    }

    fn footer(&self, state: &JamUiState, cx: &mut Context<Self>) -> impl IntoElement {
        let connected = state.connected;
        let publishing = !state.publishing.is_empty();

        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .flex_none()
            .h(px(52.0))
            .px(px(space::SECTION))
            .border_t(px(1.0))
            .border_color(Colors::border_subtle())
            .child(
                div()
                    .text_size(px(typography::DENSE_CAPTION))
                    .text_color(Colors::text_muted())
                    .child(
                        self.busy
                            .clone()
                            .unwrap_or_else(|| state.state_label.clone()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(space::BASE))
                    .child(fb_button(
                        "jam-publish-master",
                        if publishing {
                            "Master published"
                        } else {
                            "Publish Master"
                        },
                        FbButtonKind::Default,
                        connected && !publishing,
                        cx.listener(|this, _event, _window, cx| {
                            this.publish_master(cx);
                            cx.notify();
                        }),
                    ))
                    .child(fb_button(
                        "jam-invite",
                        "Invite",
                        FbButtonKind::Default,
                        connected,
                        cx.listener(|this, _event, _window, cx| {
                            this.create_invite(cx);
                            cx.notify();
                        }),
                    ))
                    // One primary slot, two meanings: `fb_button` returns two
                    // different closure types, so the branch is resolved to
                    // `AnyElement` rather than left for the `if` to unify.
                    .child(if connected {
                        fb_button(
                            "jam-leave",
                            "Leave",
                            FbButtonKind::Danger,
                            true,
                            cx.listener(|this, _event, _window, cx| {
                                this.leave(cx);
                                cx.notify();
                            }),
                        )
                        .into_any_element()
                    } else {
                        fb_button(
                            "jam-create",
                            "Create Jam",
                            FbButtonKind::Primary,
                            state.signed_in,
                            cx.listener(|this, _event, _window, cx| {
                                this.create_jam(cx);
                                cx.notify();
                            }),
                        )
                        .into_any_element()
                    }),
            )
    }
}

/// A label over a tabular value, the way every other Studio readout is built.
fn stat(label: &str, value: &str) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .text_size(px(typography::DENSE_CAPTION))
                .text_color(Colors::text_faint())
                .child(label.to_string()),
        )
        .child(
            div()
                .text_size(px(typography::UI_SM))
                .text_color(Colors::text_secondary())
                .child(value.to_string()),
        )
}

fn notice(message: &str, tone: gpui::Rgba) -> impl IntoElement {
    div()
        .p(px(space::BASE))
        .rounded(px(radius::CONTROL))
        .bg(Colors::with_alpha(tone, 0.12))
        .border_1()
        .border_color(Colors::with_alpha(tone, 0.4))
        .text_size(px(typography::DENSE_CAPTION))
        .text_color(Colors::text_primary())
        .child(message.to_string())
}

/// A stream's level, square-cornered because the topmost lit pixel must *be*
/// the value.
fn level_meter(peak: f32) -> impl IntoElement {
    let filled = (peak.clamp(0.0, 1.0) * 40.0).round();
    div()
        .flex()
        .flex_row()
        .items_center()
        .w(px(40.0))
        .h(px(4.0))
        .bg(Colors::surface_canvas())
        .child(div().w(px(filled)).h(px(4.0)).bg(if peak > 0.98 {
            Colors::status_error()
        } else {
            Colors::accent_primary()
        }))
}

fn status_tone(state: &JamUiState) -> (gpui::Rgba, &'static str) {
    if state.connected {
        (Colors::status_success(), "Connected")
    } else if state.state_label.starts_with("Reconnect") {
        (Colors::status_warning(), "Reconnecting")
    } else if state.state_label == "Failed" {
        (Colors::status_error(), "Failed")
    } else {
        (Colors::text_muted(), "Disconnected")
    }
}

fn or_dash(value: &str) -> String {
    if value.is_empty() {
        "—".to_string()
    } else {
        value.to_string()
    }
}

/// The name a track created from a stream gets: the performer and the stream,
/// which is what the person reading the arrangement later needs.
pub fn track_name_for(stream: &JamStreamView) -> String {
    let who = if stream.display_name.is_empty() {
        stream.handle.trim_start_matches('@').to_string()
    } else {
        stream.display_name.clone()
    };
    if who.is_empty() {
        stream.stream_name.clone()
    } else {
        format!("{who} - {}", stream.stream_name)
    }
}

fn default_jam_name() -> String {
    "Studio Jam".to_string()
}

pub fn open_jam_window(
    owner_bounds: Option<Bounds<gpui::Pixels>>,
    on_create_track: CreateTrackHandler,
    cx: &mut App,
) -> Result<WindowHandle<JamWindow>, String> {
    let window_bounds = centered_window_bounds(
        owner_bounds,
        size(px(JAM_WINDOW_WIDTH), px(JAM_WINDOW_HEIGHT)),
        cx,
    );
    let mut options = crate::platform_chrome::external_dialog_window_options_partial();
    options.window_bounds = Some(WindowBounds::Windowed(window_bounds));
    options.kind = WindowKind::Normal;
    options.is_resizable = true;
    options.window_background = WindowBackgroundAppearance::Transparent;
    apply_owner_display(&mut options, owner_bounds, cx);

    cx.open_window(options, move |_window, cx| {
        cx.new(|cx| JamWindow::new(on_create_track, cx))
    })
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(handle: &str, display: &str, name: &str) -> JamStreamView {
        JamStreamView {
            stream_id: "str_1".to_string(),
            user_id: "usr_1".to_string(),
            device_id: "studio-mac".to_string(),
            handle: handle.to_string(),
            display_name: display.to_string(),
            stream_name: name.to_string(),
            channels: 2,
            channel_labels: vec!["L".to_string(), "R".to_string()],
            sample_rate: 48_000,
            codec: "pcm".to_string(),
            receiving: true,
            peak: 0.0,
            rtt_ms: 0.0,
        }
    }

    #[test]
    fn a_created_track_is_named_for_the_performer_and_the_stream() {
        assert_eq!(
            track_name_for(&stream("@hachi224", "Hachi", "Guitar")),
            "Hachi - Guitar"
        );
    }

    #[test]
    fn a_performer_with_no_display_name_falls_back_to_the_handle() {
        assert_eq!(
            track_name_for(&stream("@hachi224", "", "Guitar")),
            "hachi224 - Guitar"
        );
    }

    #[test]
    fn an_anonymous_stream_is_named_after_itself_rather_than_a_dangling_dash() {
        assert_eq!(track_name_for(&stream("", "", "Guitar")), "Guitar");
    }

    #[test]
    fn a_disconnected_panel_reports_disconnected_rather_than_blank() {
        let state = JamUiState::default();
        let (_, label) = status_tone(&state);
        assert_eq!(label, "Disconnected");
        assert_eq!(or_dash(""), "—");
    }

    #[test]
    fn a_reconnecting_session_is_shown_as_a_warning_not_a_failure() {
        let state = JamUiState {
            state_label: "Reconnecting".to_string(),
            ..Default::default()
        };
        let (_, label) = status_tone(&state);
        assert_eq!(label, "Reconnecting");
    }
}
