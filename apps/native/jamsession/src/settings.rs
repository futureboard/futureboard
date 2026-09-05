//! Audio Engine settings, and nothing else.
//!
//! Studio's Preferences is a whole dialog with a dozen sections, because Studio
//! is a whole DAW. Here the only questions worth asking are the four a jam
//! actually depends on — which interface you speak through, which one you hear
//! through, at what rate, and with how much buffer — so those four are the
//! window. Everything a jam does not use is not offered, rather than offered
//! and inert.
//!
//! The window owns no state of its own beyond what is being edited: the engine
//! is the truth, this reads it on open and hands back a complete
//! [`EngineConfig`] on Apply.

use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, size, App, AppContext, Context, InteractiveElement, IntoElement, ParentElement,
    Render, StatefulInteractiveElement, Styled, Window, WindowBounds, WindowHandle, WindowKind,
};
use sphere_ui_components::components::controls::{
    fb_button, fb_section_header, fb_segment, fb_segmented_track, FbButtonKind, FbSegment,
};
use sphere_ui_components::theme::{radius, space, typography, Colors};
use sphere_ui_components::window_position::centered_window_bounds;
use DirectAudio::native::{AudioBackend, AudioDeviceId};
use DirectAudio::EngineConfig;

use crate::monitor::JamMonitor;

const WINDOW_WIDTH: f32 = 420.0;
const WINDOW_HEIGHT: f32 = 520.0;

/// Backends offered, in the order a user should try them.
///
/// `Auto` first because it is right on every platform that has one obvious
/// answer. The rest are Windows' several ways of reaching the same hardware,
/// and they are not interchangeable: an interface with an ASIO driver is often
/// only usable at jam latencies through ASIO, and an exclusive-mode endpoint is
/// the difference between 10 ms and 30 ms on the ones without. A client that
/// hid this control would be telling those users their interface does not work.
///
/// `sanitize_for_current_build` drops what this build cannot drive, so a
/// Community build never offers ASIO it has no host for.
const BACKENDS: [AudioBackend; 5] = [
    AudioBackend::Auto,
    AudioBackend::WasapiShared,
    AudioBackend::WasapiExclusive,
    AudioBackend::WdmKs,
    AudioBackend::Asio,
];

/// Rates offered. The engine will fall back to what the device actually grants;
/// these are the ones worth asking for.
const SAMPLE_RATES: [u32; 3] = [44_100, 48_000, 96_000];

/// Buffer sizes offered, in frames.
///
/// A jam is latency before it is anything else, so the list starts at the
/// smallest a general-purpose interface will hold and stops well before the
/// sizes that only make sense for mixing a finished song.
const BUFFER_SIZES: [u32; 4] = [64, 128, 256, 512];

/// One device as the picker needs it.
#[derive(Clone, PartialEq)]
struct DeviceChoice {
    id: AudioDeviceId,
    label: String,
}

pub struct AudioSettingsWindow {
    monitor: Arc<JamMonitor>,
    inputs: Vec<DeviceChoice>,
    outputs: Vec<DeviceChoice>,
    /// What is being edited. Applied as a whole, so a half-made change never
    /// reaches the device.
    draft: EngineConfig,
    /// The last thing Apply had to say.
    status: Option<String>,
}

impl AudioSettingsWindow {
    fn new(monitor: Arc<JamMonitor>) -> Self {
        let draft = monitor.config().unwrap_or_default();
        let mut window = Self {
            inputs: Vec::new(),
            outputs: Vec::new(),
            draft,
            monitor,
            status: None,
        };
        window.reload_devices();
        window
    }

    /// Re-read the device lists for the backend currently drafted.
    ///
    /// A device id belongs to the backend that enumerated it: an ASIO driver
    /// and a WASAPI endpoint for the same interface are different devices with
    /// different ids. Listing one backend's devices while another is selected
    /// is how Apply ends up asking for a device the backend has never heard of.
    fn reload_devices(&mut self) {
        let Ok(engine) = self.monitor.engine() else {
            self.inputs.clear();
            self.outputs.clear();
            return;
        };
        let backend = self.draft.backend;
        let list = |devices: Vec<DirectAudio::native::EngineDeviceInfo>| {
            devices
                .into_iter()
                .map(|device| DeviceChoice {
                    id: device.device_id,
                    label: if device.is_default {
                        format!("{} (default)", device.name)
                    } else {
                        device.name
                    },
                })
                .collect::<Vec<_>>()
        };
        self.inputs = list(engine.list_input_devices_for_backend(backend));
        self.outputs = list(engine.list_output_devices_for_backend(backend));
    }

    /// Choose a backend, and drop any device pinned on the old one.
    fn set_backend(&mut self, backend: AudioBackend) {
        if self.draft.backend == backend {
            return;
        }
        self.draft.backend = backend;
        // The ids do not survive the switch, and keeping them would ask the new
        // backend to open a device belonging to the old one.
        self.draft.input_device = None;
        self.draft.output_device = None;
        self.reload_devices();
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        self.status = Some(match self.monitor.reopen(self.draft.clone()) {
            // The rate and buffer the device actually granted, which is not
            // always what was asked for. Reporting the request rather than the
            // result is how a settings dialog ends up lying.
            Ok(()) => match self.monitor.config() {
                Ok(config) => format!(
                    "{} \u{2022} {} Hz \u{2022} {} frames",
                    config.backend.display_name(),
                    config.sample_rate,
                    config.buffer_size
                ),
                Err(error) => error,
            },
            Err(error) => format!("Could not open that device: {error}"),
        });
        cx.notify();
    }

    fn device_row(
        &self,
        label: &'static str,
        id_prefix: &'static str,
        choices: &[DeviceChoice],
        selected: Option<&AudioDeviceId>,
        on_pick: impl Fn(&mut Self, Option<AudioDeviceId>) + Copy + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut list = div()
            .id(id_prefix)
            .flex()
            .flex_col()
            // Width first, and explicitly. A row that clips its own text —
            // which is what `truncate` does — needs a width to clip against;
            // inside a scroll container that width does not fall out of the
            // layout on its own, and the rows rendered as slivers of glyphs.
            .w_full()
            .max_h(px(104.0))
            .overflow_y_scroll()
            .rounded(px(radius::CONTROL))
            .border(px(1.0))
            .border_color(Colors::border_subtle())
            .bg(Colors::surface_input());

        // "System default" is a real choice, not the absence of one: it is what
        // follows the machine when somebody unplugs an interface mid-session.
        let rows: Vec<(Option<AudioDeviceId>, String)> =
            std::iter::once((None, "System default".to_string()))
                .chain(
                    choices
                        .iter()
                        .map(|choice| (Some(choice.id.clone()), choice.label.clone())),
                )
                .collect();

        for (index, (id, label)) in rows.into_iter().enumerate() {
            let active = id.as_ref() == selected;
            let picked = id.clone();
            list = list.child(
                div()
                    .id((id_prefix, index))
                    .flex()
                    .flex_row()
                    .items_center()
                    .w_full()
                    .px(px(space::BASE))
                    .py(px(space::SNUG))
                    .cursor(gpui::CursorStyle::PointingHand)
                    .bg(if active {
                        Colors::accent_muted()
                    } else {
                        gpui::transparent_black().into()
                    })
                    .hover(|style| style.bg(Colors::surface_control_hover()))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        on_pick(this, picked.clone());
                        this.status = None;
                        cx.notify();
                    }))
                    // The label truncates, the row does not. Clipping on the
                    // row itself clips the glyphs; clipping on a flex child
                    // that owns a resolved width clips the *text*, which is
                    // what was wanted.
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .truncate()
                            .text_size(px(typography::UI_XS))
                            .text_color(if active {
                                Colors::text_primary()
                            } else {
                                Colors::text_secondary()
                            })
                            .child(label),
                    ),
            );
        }

        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(space::TIGHT))
            .child(fb_section_header(label))
            .child(list)
    }

    fn choice_row<T: Copy + PartialEq + 'static>(
        &self,
        label: &'static str,
        id_prefix: &'static str,
        options: &[T],
        selected: T,
        render: impl Fn(T) -> String,
        on_pick: impl Fn(&mut Self, T) + Copy + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut track = fb_segmented_track().w_full();
        let count = options.len();
        for (index, option) in options.iter().copied().enumerate() {
            let position = if count == 1 {
                FbSegment::Only
            } else if index == 0 {
                FbSegment::First
            } else if index + 1 == count {
                FbSegment::Last
            } else {
                FbSegment::Middle
            };
            track = track.child(div().flex_1().min_w(px(0.0)).child(fb_segment(
                gpui::ElementId::Name(format!("{id_prefix}-{index}").into()),
                render(option),
                option == selected,
                position,
                cx.listener(move |this, _event, _window, cx| {
                    on_pick(this, option);
                    this.status = None;
                    cx.notify();
                }),
            )));
        }
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(space::TIGHT))
            .child(fb_section_header(label))
            .child(track)
    }
}

impl Render for AudioSettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let draft = self.draft.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(Colors::surface_base())
            .text_color(Colors::text_primary())
            .font(sphere_ui_components::theme::ui_font())
            .child(
                sphere_ui_components::components::title_bar::external_window_titlebar(
                    "Audio Engine",
                    "jam-settings-close",
                    move |window, _cx| {
                        window.remove_window();
                    },
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.0))
                    .id("jam-settings-body")
                    .w_full()
                    .gap(px(space::SECTION))
                    .px(px(space::SECTION))
                    .py(px(space::LOOSE))
                    .overflow_y_scroll()
                    .child(
                        self.choice_row(
                            "Audio backend",
                            "jam-settings-backend",
                            &BACKENDS
                                .into_iter()
                                .filter(|backend| backend.sanitize_for_current_build() == *backend)
                                .collect::<Vec<_>>(),
                            draft.backend,
                            |backend| backend.display_name().to_string(),
                            |this, backend| this.set_backend(backend),
                            cx,
                        ),
                    )
                    .child(self.device_row(
                        "Input device",
                        "jam-settings-input",
                        &self.inputs.clone(),
                        draft.input_device.as_ref(),
                        |this, id| this.draft.input_device = id,
                        cx,
                    ))
                    .child(self.device_row(
                        "Output device",
                        "jam-settings-output",
                        &self.outputs.clone(),
                        draft.output_device.as_ref(),
                        |this, id| this.draft.output_device = id,
                        cx,
                    ))
                    .child(self.choice_row(
                        "Sample rate",
                        "jam-settings-rate",
                        &SAMPLE_RATES,
                        draft.sample_rate,
                        |rate| format!("{:.1} kHz", rate as f32 / 1000.0),
                        |this, rate| this.draft.sample_rate = rate,
                        cx,
                    ))
                    .child(self.choice_row(
                        "Buffer size",
                        "jam-settings-buffer",
                        &BUFFER_SIZES,
                        draft.buffer_size,
                        |frames| format!("{frames}"),
                        |this, frames| this.draft.buffer_size = frames,
                        cx,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(px(space::BASE))
                    .flex_none()
                    .px(px(space::SECTION))
                    .py(px(space::LOOSE))
                    .border_t(px(1.0))
                    .border_color(Colors::border_subtle())
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .truncate()
                            .text_size(px(typography::UI_XS))
                            .text_color(Colors::text_muted())
                            .when_some(self.status.clone(), |element, status| {
                                element.child(status)
                            }),
                    )
                    .child(fb_button(
                        "jam-settings-apply",
                        "Apply",
                        FbButtonKind::Primary,
                        true,
                        cx.listener(|this, _event, _window, cx| {
                            this.apply(cx);
                        }),
                    )),
            )
    }
}

/// Open the settings window, or bring the one already open to the front.
pub fn open_audio_settings(
    monitor: Arc<JamMonitor>,
    cx: &mut App,
) -> Result<WindowHandle<AudioSettingsWindow>, String> {
    let mut options =
        sphere_ui_components::platform_chrome::external_dialog_window_options_partial();
    options.window_bounds = Some(WindowBounds::Windowed(centered_window_bounds(
        cx.primary_display().map(|display| display.bounds()),
        size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
        cx,
    )));
    options.kind = WindowKind::Normal;
    options.is_resizable = true;

    cx.open_window(options, move |_window, cx| {
        cx.new(|_cx| AudioSettingsWindow::new(monitor))
    })
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{BUFFER_SIZES, SAMPLE_RATES};

    /// Auto is first because it is the answer for anyone who has no reason to
    /// care, and every alternative below it is a different way of reaching the
    /// same hardware rather than a different quality setting.
    #[test]
    fn auto_leads_the_backend_list() {
        assert_eq!(super::BACKENDS[0], DirectAudio::native::AudioBackend::Auto);
        assert!(super::BACKENDS.contains(&DirectAudio::native::AudioBackend::Asio));
    }

    /// A jam is latency before it is anything else. Offering only the sizes a
    /// mixing session wants would make the app unusable for the thing it is
    /// for, and offering sizes no interface will hold would make Apply a
    /// coin toss.
    #[test]
    fn the_buffer_choices_start_small_and_stay_sane() {
        assert_eq!(BUFFER_SIZES[0], 64);
        assert!(BUFFER_SIZES.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(*BUFFER_SIZES.last().unwrap() <= 512);
    }

    #[test]
    fn the_rate_choices_cover_what_a_room_negotiates() {
        assert!(SAMPLE_RATES.contains(&48_000), "the jam session's own rate");
        assert!(SAMPLE_RATES.contains(&44_100));
        assert!(SAMPLE_RATES.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
