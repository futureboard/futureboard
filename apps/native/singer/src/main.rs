#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{sync::Arc, time::Duration};

use cpal::{
    Device, SampleFormat, Stream, StreamConfig, SupportedStreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use crossbeam_channel::{Receiver, Sender, TryRecvError};
use gpui::{
    App, AppContext, Application, Context, Entity, FocusHandle, InteractiveElement, IntoElement,
    MouseButton, ParentElement, Render, Styled, Window, WindowControlArea, WindowOptions, div, px,
    size,
};
use solfege_audio::SampleRate;
use solfege_core::{BowedStringConfig, RuntimeInstrument};
use solfege_engine::{EngineCommand, EngineConfig, SamplerEngine, SharedMetrics};
use solfege_event::{Articulation, Event};
use sphere_ui_components::{
    assets,
    components::{
        FbButtonKind, PianoRoll, fb_button, fb_section_header,
        title_bar::{TITLEBAR_HEIGHT, draggable_spacer, section_separator, window_control_button},
    },
    embedded_assets::EmbeddedAssets,
    platform_chrome::{self, PlatformChromePolicy},
    theme::{self, Colors},
};

use sphere_ui_components::components::timeline::Timeline;

const ENGINE_REVISION: &str = "5887bf3";

fn main() {
    application().with_assets(EmbeddedAssets::new()).run(|cx| {
        sphere_ui_components::boot::log("Singer boot start");
        let _ = theme::initialize_theme_system();
        let saved_theme = sphere_ui_components::settings::SettingsSchema::load_from_disk()
            .appearance
            .theme;
        let _ = theme::activate_theme_by_id(&saved_theme);
        sphere_ui_components::assets::register_fonts(cx);

        let timeline = cx.new(|_| Timeline::with_demo_content());
        let selected_clip = timeline
            .read(cx)
            .state
            .tracks
            .iter()
            .flat_map(|track| track.clips.iter())
            .find(|clip| clip.id == "clip-4")
            .map(|clip| clip.id.clone());
        if let Some(clip_id) = selected_clip {
            let _ = timeline.update(cx, |timeline, _| {
                timeline.state.selection.selected_track_id = Some("track-3".to_owned());
                timeline.state.selection.selected_clip_ids = vec![clip_id];
            });
        }

        let piano_roll = cx.new(|cx| PianoRoll::new(timeline.clone(), cx));
        let audio = SingerAudio::start().ok();
        let options = singer_window_options(cx);
        let _ = cx.open_window(options, move |_window, cx| {
            cx.new(|cx| SingerWindow::new(timeline, piano_roll, audio, cx))
        });
    });
}

fn application() -> Application {
    #[cfg(target_os = "windows")]
    let platform: std::rc::Rc<dyn gpui::Platform> = std::rc::Rc::new(
        gpui_windows::WindowsPlatform::new(false).expect("failed to initialize Windows platform"),
    );
    #[cfg(target_os = "macos")]
    let platform: std::rc::Rc<dyn gpui::Platform> =
        std::rc::Rc::new(gpui_macos::MacPlatform::new(false));
    #[cfg(target_os = "linux")]
    let platform: std::rc::Rc<dyn gpui::Platform> = gpui_linux::current_platform(false);

    Application::with_platform(platform)
}

fn singer_window_options(cx: &mut App) -> WindowOptions {
    let mut options = sphere_ui_components::platform_chrome::studio_window_options();
    let display = cx.primary_display();
    let bounds = display.map(|display| {
        let display_bounds = display.bounds();
        let width = f32::from(display_bounds.size.width).min(1440.0);
        let height = f32::from(display_bounds.size.height).min(900.0);
        gpui::Bounds {
            origin: gpui::point(
                display_bounds.origin.x + px((f32::from(display_bounds.size.width) - width) / 2.0),
                display_bounds.origin.y
                    + px((f32::from(display_bounds.size.height) - height) / 2.0),
            ),
            size: size(px(width.max(980.0)), px(height.max(620.0))),
        }
    });
    options.window_bounds = bounds.map(gpui::WindowBounds::Windowed);
    options.show = true;
    options.focus = true;
    options
}

struct SingerWindow {
    timeline: Entity<Timeline>,
    piano_roll: Entity<PianoRoll>,
    audio: Option<SingerAudio>,
    focus_handle: FocusHandle,
}

impl SingerWindow {
    fn new(
        timeline: Entity<Timeline>,
        piano_roll: Entity<PianoRoll>,
        audio: Option<SingerAudio>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            timeline,
            piano_roll,
            audio,
            focus_handle: cx.focus_handle(),
        }
    }

    fn send(&self, event: Event) {
        if let Some(audio) = &self.audio {
            let _ = audio.control.commands.try_send(EngineCommand::Event(event));
        }
    }

    fn audition(&self) {
        self.send(Event::Articulation {
            note_id: 1,
            articulation: Articulation::Legato,
        });
        self.send(Event::NoteOn {
            note: 60,
            velocity: 0.82,
            note_id: 1,
        });
        let control = self.audio.as_ref().map(|audio| audio.control.clone());
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(700));
            if let Some(control) = control {
                let _ = control
                    .commands
                    .try_send(EngineCommand::Event(Event::NoteOff {
                        note: 60,
                        velocity: 0.0,
                        note_id: 1,
                    }));
            }
        });
    }

    fn stop(&self) {
        self.send(Event::AllNotesOff);
    }
}

impl Render for SingerWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let play_target = cx.entity();
        let stop_target = cx.entity();
        let audio_status = self
            .audio
            .as_ref()
            .map(|audio| audio.status())
            .unwrap_or_else(|| "Audio unavailable — physical editor remains available".to_owned());
        let selected_clip = self
            .timeline
            .read(cx)
            .state
            .selection
            .selected_clip_ids
            .first()
            .cloned()
            .unwrap_or_else(|| "none".to_owned());
        let piano_roll = self.piano_roll.clone();
        let titlebar = singer_titlebar(window, self.audio.is_some(), play_target, stop_target);

        div()
            .size_full()
            .flex()
            .flex_col()
            .font(theme::ui_font())
            .bg(Colors::surface_base())
            .track_focus(&self.focus_handle)
            .child(titlebar)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .p(px(12.0))
                            .gap(px(8.0))
                            .child(fb_section_header("MIDI EDITOR · REUSED FROM SPHEREUICOMPONENTS"))
                            .child(div().flex_1().min_h_0().child(piano_roll)),
                    )
                    .child(
                        div()
                            .w(px(270.0))
                            .flex_shrink_0()
                            .flex()
                            .flex_col()
                            .gap(px(12.0))
                            .p(px(14.0))
                            .bg(Colors::surface_panel())
                            .border_l(px(1.0))
                            .border_color(Colors::border_subtle())
                            .child(fb_section_header("INSTRUMENT"))
                            .child(readout("Instrument", "Solo Violin"))
                            .child(readout("Engine", "Solfege SamplerEngine"))
                            .child(readout("Git source", ENGINE_REVISION))
                            .child(readout("Selected clip", selected_clip))
                            .child(fb_section_header("AUDIO OUTPUT"))
                            .child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(Colors::text_muted())
                                    .child(audio_status),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_size(px(10.0))
                                    .text_color(Colors::text_faint())
                                    .child("Piano Roll, velocity, controller and articulation editing are provided by the shared Futureboard UI kit."),
                            ),
                    ),
            )
    }
}

fn singer_titlebar(
    window: &Window,
    audio_available: bool,
    play_target: Entity<SingerWindow>,
    stop_target: Entity<SingerWindow>,
) -> impl IntoElement {
    let policy = PlatformChromePolicy::current();
    let (max_path, max_fallback) = if window.is_maximized() {
        (assets::ICON_RESTORE_PATH, "RESTORE")
    } else {
        (assets::ICON_MAXIMIZE_PATH, "MAX")
    };

    let mut bar = div()
        .flex()
        .flex_row()
        .items_center()
        .h(px(TITLEBAR_HEIGHT))
        .w_full()
        .pl(policy.traffic_light_left_padding())
        .bg(Colors::surface_titlebar())
        .border_b(px(1.0))
        .border_color(Colors::border_subtle())
        .window_control_area(WindowControlArea::Drag)
        .on_mouse_down(MouseButton::Left, |_, window, _cx| {
            window.start_window_move();
        })
        .child(
            div()
                .flex()
                .items_center()
                .h_full()
                .px(px(12.0))
                .gap(px(8.0))
                .text_size(px(11.0))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(Colors::text_primary())
                .child(platform_chrome::branded_window_title("SINGER")),
        )
        .child(
            div()
                .text_size(px(10.0))
                .text_color(Colors::text_muted())
                .child("Solfege violin playground"),
        )
        .child(draggable_spacer())
        .child(fb_button(
            "singer-audition",
            "Audition",
            FbButtonKind::Primary,
            audio_available,
            move |_, _, cx| {
                let _ = play_target.update(cx, |window, _| window.audition());
            },
        ))
        .child(fb_button(
            "singer-stop",
            "Stop",
            FbButtonKind::Default,
            audio_available,
            move |_, _, cx| {
                let _ = stop_target.update(cx, |window, _| window.stop());
            },
        ));

    if policy.show_window_controls {
        bar = bar
            .child(section_separator())
            .child(window_control_button(
                WindowControlArea::Min,
                assets::ICON_MINIMIZE_PATH,
                "MINIMIZE",
            ))
            .child(window_control_button(
                WindowControlArea::Max,
                max_path,
                max_fallback,
            ))
            .child(window_control_button(
                WindowControlArea::Close,
                assets::ICON_X_PATH,
                "CLOSE",
            ));
    }

    bar
}

fn readout(label: &str, value: impl Into<String>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(3.0))
        .child(
            div()
                .text_size(px(9.5))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(Colors::text_faint())
                .child(label.to_owned()),
        )
        .child(
            div()
                .text_size(px(11.0))
                .text_color(Colors::text_secondary())
                .child(value.into()),
        )
}

struct SingerAudioControl {
    commands: Sender<EngineCommand>,
    metrics: Arc<SharedMetrics>,
    device_name: String,
    sample_rate: u32,
}

struct SingerAudio {
    control: Arc<SingerAudioControl>,
    _stream: Stream,
}

impl SingerAudio {
    fn start() -> Result<Self, String> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or_else(|| "no default output device".to_owned())?;
        let device_name = device
            .name()
            .unwrap_or_else(|_| "Default output".to_owned());
        let supported = device
            .default_output_config()
            .map_err(|error| format!("default output config: {error}"))?;
        let sample_rate = supported.sample_rate().0;
        let channels = supported.channels() as usize;
        let metrics = Arc::new(SharedMetrics::default());
        let instrument =
            RuntimeInstrument::bowed_string("Singer Solo Violin", BowedStringConfig::default());
        let engine = SamplerEngine::prepare(
            EngineConfig::realtime(
                SampleRate::new(sample_rate as f32)
                    .map_err(|error| format!("sample rate: {error}"))?,
            ),
            Some(instrument),
            metrics.clone(),
        );
        let (commands, receiver) = crossbeam_channel::bounded(512);
        let stream = build_stream(
            &device,
            &supported,
            channels,
            engine,
            receiver,
            metrics.clone(),
        )?;
        stream
            .play()
            .map_err(|error| format!("start stream: {error}"))?;
        Ok(Self {
            control: Arc::new(SingerAudioControl {
                commands,
                metrics,
                device_name,
                sample_rate,
            }),
            _stream: stream,
        })
    }

    fn status(&self) -> String {
        let metrics = self.control.metrics.snapshot();
        format!(
            "{} · {} Hz · {} voices",
            self.control.device_name, self.control.sample_rate, metrics.active_voices
        )
    }
}

fn build_stream(
    device: &Device,
    supported: &SupportedStreamConfig,
    channels: usize,
    engine: SamplerEngine,
    commands: Receiver<EngineCommand>,
    metrics: Arc<SharedMetrics>,
) -> Result<Stream, String> {
    let config: StreamConfig = supported.config();
    match supported.sample_format() {
        SampleFormat::F32 => {
            build_typed_stream::<f32>(device, &config, channels, engine, commands, metrics)
        }
        SampleFormat::I16 => {
            build_typed_stream::<i16>(device, &config, channels, engine, commands, metrics)
        }
        SampleFormat::U16 => {
            build_typed_stream::<u16>(device, &config, channels, engine, commands, metrics)
        }
        format => Err(format!("unsupported output sample format: {format:?}")),
    }
}

trait OutputSample: cpal::SizedSample {
    fn from_engine(value: f32) -> Self;
}

impl OutputSample for f32 {
    fn from_engine(value: f32) -> Self {
        value
    }
}

impl OutputSample for i16 {
    fn from_engine(value: f32) -> Self {
        (value.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16
    }
}

impl OutputSample for u16 {
    fn from_engine(value: f32) -> Self {
        ((value.clamp(-1.0, 1.0) * 0.5 + 0.5) * u16::MAX as f32).round() as u16
    }
}

fn build_typed_stream<T>(
    device: &Device,
    config: &StreamConfig,
    channels: usize,
    mut engine: SamplerEngine,
    commands: Receiver<EngineCommand>,
    metrics: Arc<SharedMetrics>,
) -> Result<Stream, String>
where
    T: OutputSample + Send + 'static,
{
    const MAX_CALLBACK_SAMPLES: usize = 8192 * 8;
    let mut scratch = vec![0.0_f32; MAX_CALLBACK_SAMPLES];
    let callback_metrics = metrics.clone();
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                for _ in 0..256 {
                    match commands.try_recv() {
                        Ok(command) => engine.handle_command(command),
                        Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
                    }
                }
                if data.len() > scratch.len() {
                    data.fill(T::from_engine(0.0));
                    callback_metrics.record_underrun();
                    return;
                }
                let sample_count = data.len();
                engine.process_interleaved(&mut scratch[..sample_count], channels, &[]);
                for (destination, source) in
                    data.iter_mut().zip(scratch[..sample_count].iter().copied())
                {
                    *destination = T::from_engine(source.clamp(-1.0, 1.0));
                }
            },
            move |_| metrics.record_underrun(),
            None,
        )
        .map_err(|error| format!("build output stream: {error}"))
}
