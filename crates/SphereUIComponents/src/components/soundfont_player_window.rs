//! Floating "Soundfont Player" utility window.
//!
//! Hosts the built-in Soundfont Player instrument (see
//! `TrackState::builtin_soundfont_player`) in a simple floating utility window.
//! The player panel fills the window directly — no nested MDI/document chrome.
//!
//! Two separate players are in play here, and the split is deliberate:
//!
//! - this window owns a control-side [`SoundfontPlayer`] used to read the
//!   `.sf2`'s real bank name and preset list and to validate a preset choice;
//! - the audible instrument is the engine's own player on the owning track,
//!   rebuilt from the track state this window publishes.
//!
//! So the keyboard and the Test button do not play the window's instance. They
//! send MIDI preview through [`SoundfontPlayerPreview`] to the engine, which is
//! the same path the piano roll uses — what you hear here is what the track
//! will play back.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    div, px, size, App, AppContext, Bounds, Context, FocusHandle, InteractiveElement, IntoElement,
    ParentElement, Render, Styled, Window, WindowBackgroundAppearance, WindowBounds, WindowHandle,
    WindowKind,
};

use crate::components::soundfont_player_mdi::{
    soundfont_player_panel, SoundfontPlayerCallbacks, SoundfontPlayerPanelState,
    SOUNDFONT_PLAYER_MDI_TITLE,
};
use crate::components::timeline::timeline_state::SoundfontPlayerSettingsState;
use crate::components::title_bar::external_window_titlebar;
use crate::soundfont_player::{
    SoundfontEnvelope, SoundfontPlayer, SoundfontPlayerError, SoundfontPlayerSettings,
    SoundfontRenderQuality,
};
use crate::theme::Colors;

pub const SOUNDFONT_PLAYER_WINDOW_WIDTH: f32 = 640.0;
/// Tall enough to open with Source, Amp Envelope and Output all visible above
/// the keyboard. Shorter than this the instrument body scrolls; the header and
/// keyboard stay pinned.
pub const SOUNDFONT_PLAYER_WINDOW_HEIGHT: f32 = 680.0;
/// The envelope knob row is the widest fixed content: four 52 px knobs, their
/// gaps, the Reset button, and the section padding.
pub const SOUNDFONT_PLAYER_WINDOW_MIN_WIDTH: f32 = 480.0;
pub const SOUNDFONT_PLAYER_WINDOW_MIN_HEIGHT: f32 = 380.0;

const PREVIEW_MIDI_CHANNEL: u8 = 0;
const PREVIEW_VELOCITY: u8 = 100;
/// Notes the Test button auditions: a C major triad, low enough to be clear on
/// a bass or pad preset and high enough not to disappear on a lead.
const TEST_CHORD: [u8; 3] = [60, 64, 67];
/// How long the Test button holds its chord. Long enough to judge a slow
/// attack or a pad, short enough that the button is not a mode the user has to
/// exit — and Stop is there for the impatient.
const TEST_CHORD_HOLD: Duration = Duration::from_millis(2_200);

/// One MIDI preview gesture from this window, addressed to the track that owns
/// the built-in player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoundfontPlayerPreview {
    NoteOn {
        channel: u8,
        pitch: u8,
        velocity: u8,
    },
    NoteOff {
        channel: u8,
        pitch: u8,
    },
    AllNotesOff,
}

#[derive(Debug, Clone)]
pub struct SoundfontPlayerTrackUpdate {
    pub track_id: String,
    pub settings: SoundfontPlayerSettingsState,
}

/// A track's saved Soundfont Player settings, used to restore the window when
/// it is opened on a track that already has a `.sf2` (a reopened project, or a
/// window closed and reopened in the same session).
///
/// The same shape the timeline stores and the window publishes back, so the
/// panel cannot drift out of step with what a track actually holds.
pub type SoundfontPlayerTrackState = SoundfontPlayerSettingsState;

type PreviewCb = Arc<dyn Fn(&str, SoundfontPlayerPreview, &mut App) + Send + Sync>;

pub struct SoundfontPlayerWindow {
    track_id: String,
    on_close: Arc<dyn Fn(&mut Window, &mut App) + Send + Sync>,
    on_update_track: Arc<dyn Fn(SoundfontPlayerTrackUpdate, &mut App) + Send + Sync>,
    on_preview: PreviewCb,
    focus_handle: FocusHandle,
    player: Option<SoundfontPlayer>,
    loaded_path: Option<PathBuf>,
    panel: SoundfontPlayerPanelState,
    /// Bumped whenever an audition starts or is cancelled, so a hold timer that
    /// belongs to an earlier press cannot release the notes of a later one.
    test_generation: u64,
}

impl SoundfontPlayerWindow {
    pub fn new(
        track_id: String,
        initial: SoundfontPlayerTrackState,
        on_close: Arc<dyn Fn(&mut Window, &mut App) + Send + Sync>,
        on_update_track: Arc<dyn Fn(SoundfontPlayerTrackUpdate, &mut App) + Send + Sync>,
        on_preview: PreviewCb,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut window = Self {
            track_id,
            on_close,
            on_update_track,
            on_preview,
            focus_handle: cx.focus_handle(),
            player: None,
            loaded_path: None,
            panel: SoundfontPlayerPanelState::default(),
            test_generation: 0,
        };
        window.restore_track_state(initial, cx);
        window
    }

    /// Reloads the panel from a track's saved settings. Without this the window
    /// would open on "No .sf2 loaded" for a track whose SoundFont the engine has
    /// had loaded since the project opened, and the first gesture would publish
    /// that empty state back over the saved one.
    fn restore_track_state(&mut self, state: SoundfontPlayerTrackState, cx: &mut Context<Self>) {
        let state = state.sanitized();
        self.panel.master_volume = state.volume;
        self.panel.reverb_chorus = state.reverb_chorus;
        self.panel.polyphony = state.polyphony;
        self.panel.envelope = state.envelope;
        self.panel.quality = state.quality;

        let Some(path) = state
            .path
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
        else {
            self.player = None;
            self.loaded_path = None;
            self.panel.file_name = None;
            self.panel.bank_name = None;
            self.panel.presets.clear();
            self.panel.selected_preset = None;
            return;
        };
        if self.loaded_path.as_deref() == Some(path.as_path()) {
            return;
        }

        // Parsing a General MIDI bank takes long enough to stall the first
        // frame, so load it the same way Browse does — off the UI thread, with
        // the panel showing its loading state until it lands.
        self.panel.loading = true;
        self.panel.status = None;
        let settings = self.player_settings(44_100);
        let entity = cx.entity().clone();
        let preset = state.preset;
        cx.spawn(async move |_this, cx| {
            let result = cx
                .background_spawn({
                    let path = path.clone();
                    async move { SoundfontPlayer::from_path(&path, settings) }
                })
                .await;
            let _ = entity.update(cx, |this, cx| {
                this.apply_loaded_player(path, result);
                // Prefer the track's saved preset over the font's first one.
                if let Some((bank, patch)) = preset {
                    this.select_preset(bank, patch);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Player settings for the panel's current controls. The window's own
    /// instance only reads bank/preset metadata and validates a preset choice,
    /// but it is built from the same settings the engine will use so a rejected
    /// combination surfaces here rather than silently on the audio thread.
    fn player_settings(&self, sample_rate: i32) -> SoundfontPlayerSettings {
        SoundfontPlayerSettings {
            sample_rate,
            block_size: 0,
            maximum_polyphony: self.panel.polyphony,
            enable_reverb_and_chorus: self.panel.reverb_chorus,
            envelope: self.panel.envelope,
            quality: self.panel.quality,
            max_render_frames: 0,
        }
    }

    fn preview(&self, event: SoundfontPlayerPreview, app: &mut App) {
        (self.on_preview)(&self.track_id, event, app);
    }

    /// The channel this window previews on.
    ///
    /// Always the plain preview channel: the engine's player routes a note to
    /// the channel its selected preset actually lives on (a drum kit only exists
    /// on channel 10), so this window must not second-guess it. Duplicating the
    /// rule here is what let the panel keyboard audition a kit that the track's
    /// own notes could not reach.
    fn preview_channel(&self) -> u8 {
        PREVIEW_MIDI_CHANNEL
    }

    /// Presses one panel key. Held until [`Self::note_off`] so a sustained
    /// preset actually sustains, matching the piano roll's key behavior.
    fn note_on(&mut self, pitch: u8, app: &mut App) {
        if !self.panel.is_playable() || self.panel.active_notes.contains(&pitch) {
            return;
        }
        let channel = self.preview_channel();
        self.panel.active_notes.push(pitch);
        self.panel.status = None;
        self.preview(
            SoundfontPlayerPreview::NoteOn {
                channel,
                pitch,
                velocity: PREVIEW_VELOCITY,
            },
            app,
        );
    }

    fn note_off(&mut self, pitch: u8, app: &mut App) {
        if !self.panel.active_notes.contains(&pitch) {
            return;
        }
        let channel = self.preview_channel();
        self.panel.active_notes.retain(|held| *held != pitch);
        if self.panel.active_notes.is_empty() {
            self.panel.testing = false;
        }
        self.preview(SoundfontPlayerPreview::NoteOff { channel, pitch }, app);
    }

    /// Auditions the loaded preset through the engine and releases the chord
    /// after [`TEST_CHORD_HOLD`].
    fn start_test(&mut self, cx: &mut Context<Self>) {
        if !self.panel.is_playable() {
            self.panel.status = Some("Load a SoundFont before testing.".into());
            return;
        }
        self.release_all(cx);
        self.test_generation = self.test_generation.wrapping_add(1);
        let generation = self.test_generation;
        let channel = self.preview_channel();
        self.panel.testing = true;
        self.panel.status = None;
        for pitch in TEST_CHORD {
            self.panel.active_notes.push(pitch);
            self.preview(
                SoundfontPlayerPreview::NoteOn {
                    channel,
                    pitch,
                    velocity: PREVIEW_VELOCITY,
                },
                cx,
            );
        }

        let entity = cx.entity().clone();
        cx.spawn(async move |_this, cx| {
            cx.background_executor().timer(TEST_CHORD_HOLD).await;
            let _ = entity.update(cx, |this, cx| {
                if this.test_generation != generation {
                    return;
                }
                this.release_all(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Releases every note this panel is holding. Also invalidates any pending
    /// audition timer.
    fn release_all(&mut self, app: &mut App) {
        self.test_generation = self.test_generation.wrapping_add(1);
        self.panel.testing = false;
        if self.panel.active_notes.is_empty() {
            return;
        }
        self.panel.active_notes.clear();
        self.preview(SoundfontPlayerPreview::AllNotesOff, app);
    }

    fn shift_octave(&mut self, delta: i32, app: &mut App) {
        self.release_all(app);
        self.panel.shift_keyboard_octave(delta);
    }

    /// Retargets an already-open window at `track_id` and reloads the panel from
    /// that track's saved settings. Focusing the OS window is the caller's job.
    pub fn focus_soundfont_player(
        &mut self,
        track_id: String,
        state: SoundfontPlayerTrackState,
        cx: &mut Context<Self>,
    ) {
        if self.track_id != track_id {
            self.release_all(cx);
            self.track_id = track_id;
            self.loaded_path = None;
        }
        self.restore_track_state(state, cx);
    }

    fn notify_track_update(&self, app: &mut App) {
        (self.on_update_track)(
            SoundfontPlayerTrackUpdate {
                track_id: self.track_id.clone(),
                settings: SoundfontPlayerSettingsState {
                    path: self
                        .loaded_path
                        .as_ref()
                        .map(|path| path.to_string_lossy().into_owned()),
                    preset: self.panel.selected_preset,
                    volume: self.panel.master_volume,
                    reverb_chorus: self.panel.reverb_chorus,
                    polyphony: self.panel.polyphony,
                    envelope: self.panel.envelope,
                    quality: self.panel.quality,
                },
            },
            app,
        );
    }

    fn browse_soundfont(&mut self, cx: &mut Context<Self>) {
        self.panel.loading = true;
        self.panel.status = Some("Loading SoundFont…".into());
        cx.notify();
        #[cfg(feature = "native-dialogs")]
        {
            let settings = self.player_settings(44_100);
            let entity = cx.entity().clone();
            cx.spawn(async move |_this, cx| {
                let result = rfd::AsyncFileDialog::new()
                    .set_title("Load SoundFont")
                    .add_filter("SoundFont", &["sf2"])
                    .pick_file()
                    .await;
                let Some(handle) = result else {
                    let _ = entity.update(cx, |this, cx| {
                        this.panel.loading = false;
                        this.panel.status = None;
                        cx.notify();
                    });
                    return;
                };
                let path = handle.path().to_path_buf();
                let load_result = SoundfontPlayer::from_path(&path, settings);
                let _ = entity.update(cx, |this, cx| {
                    this.apply_loaded_player(path, load_result);
                    this.notify_track_update(cx);
                    cx.notify();
                });
            })
            .detach();
        }
        #[cfg(not(feature = "native-dialogs"))]
        {
            self.panel.loading = false;
            self.panel.status = Some("Native file dialogs are unavailable in this build.".into());
            cx.notify();
        }
    }

    fn apply_loaded_player(
        &mut self,
        path: PathBuf,
        result: Result<SoundfontPlayer, SoundfontPlayerError>,
    ) {
        match result {
            Ok(mut player) => {
                self.panel.loading = false;
                self.panel.bank_name = Some(player.bank_name().to_string());
                self.panel.presets = player.list_presets();
                self.panel.file_name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned());
                self.panel.selected_preset = None;
                self.panel.status = None;
                if let Some(first) = self.panel.presets.first() {
                    match player.select_preset_all_channels(first.bank, first.patch) {
                        Ok(()) => self.panel.selected_preset = Some((first.bank, first.patch)),
                        Err(error) => {
                            self.panel.status =
                                Some(format!("Default preset select failed: {error}"));
                        }
                    }
                }
                // The panel (and through it the track state the engine reads) is
                // the authority for volume — a freshly built synthesizer starts
                // at RustySynth's own default, which would otherwise show up as
                // a volume the user never chose and disagree with playback.
                player.set_master_volume(self.panel.master_volume);
                self.panel.reverb_chorus = player.enable_reverb_and_chorus();
                self.panel.polyphony = player.maximum_polyphony();
                self.panel.envelope = player.envelope();
                self.panel.quality = player.quality();
                self.player = Some(player);
                self.loaded_path = Some(path);
            }
            Err(error) => {
                self.panel.loading = false;
                self.panel.status = Some(format!("Load failed: {error}"));
            }
        }
    }

    fn toggle_preset_list(&mut self) {
        self.panel.preset_list_open = !self.panel.preset_list_open;
    }

    fn select_preset(&mut self, bank: i32, patch: i32) {
        let Some(player) = self.player.as_mut() else {
            return;
        };
        match player.select_preset_all_channels(bank, patch) {
            Ok(()) => {
                self.panel.selected_preset = Some((bank, patch));
                self.panel.preset_list_open = false;
                self.panel.status = None;
            }
            Err(error) => {
                self.panel.status = Some(format!("Preset select failed: {error}"));
            }
        }
    }

    fn set_volume(&mut self, value: f32) {
        let value = value.clamp(0.0, 1.0);
        self.panel.master_volume = value;
        if let Some(player) = self.player.as_mut() {
            player.set_master_volume(value);
        }
    }

    /// Reverb/chorus, polyphony and render quality are all fixed at synthesizer
    /// creation in RustySynth — no live setter exists — so applying any of them
    /// reloads the same file (a control/offline operation, same as the initial
    /// load) and reapplies volume + the selected preset. The amp envelope is
    /// ours rather than RustySynth's and needs no reload; see
    /// [`Self::set_envelope`].
    fn reload_with_settings(&mut self) {
        let Some(path) = self.loaded_path.clone() else {
            return;
        };
        let sample_rate = self
            .player
            .as_ref()
            .map(SoundfontPlayer::sample_rate)
            .unwrap_or(44_100);
        let settings = self.player_settings(sample_rate);
        match SoundfontPlayer::from_path(&path, settings) {
            Ok(mut player) => {
                player.set_master_volume(self.panel.master_volume);
                if let Some((bank, patch)) = self.panel.selected_preset {
                    if let Err(error) = player.select_preset_all_channels(bank, patch) {
                        self.panel.status = Some(format!("Preset reselect failed: {error}"));
                    }
                }
                self.player = Some(player);
            }
            Err(error) => {
                self.panel.status = Some(format!("Reload failed: {error}"));
            }
        }
    }

    fn toggle_reverb_chorus(&mut self) {
        self.panel.reverb_chorus = !self.panel.reverb_chorus;
        self.reload_with_settings();
    }

    fn set_polyphony(&mut self, value: usize) {
        self.panel.polyphony = value.clamp(1, 256);
        self.reload_with_settings();
    }

    /// Amp envelope edits apply live — no synthesizer rebuild — so dragging a
    /// knob does not restart the voices under the user's fingers.
    fn set_envelope(&mut self, envelope: SoundfontEnvelope) {
        self.panel.envelope = envelope.sanitized();
        if let Some(player) = self.player.as_mut() {
            player.set_envelope(self.panel.envelope);
        }
    }

    fn set_quality(&mut self, quality: SoundfontRenderQuality) {
        if self.panel.quality == quality {
            return;
        }
        self.panel.quality = quality;
        self.reload_with_settings();
    }
}

impl Render for SoundfontPlayerWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focus_handle.is_focused(window) {
            self.focus_handle.focus(window, cx);
        }
        let on_close = self.on_close.clone();
        let entity = cx.entity().clone();
        let self_entity = entity.clone();

        let panel_callbacks = SoundfontPlayerCallbacks {
            on_browse: Arc::new({
                let entity = entity.clone();
                move |_window, app: &mut App| {
                    let _ = entity.update(app, |this, cx| {
                        this.browse_soundfont(cx);
                    });
                }
            }),
            on_toggle_preset_list: Arc::new({
                let entity = entity.clone();
                move |_window, app: &mut App| {
                    let _ = entity.update(app, |this, cx| {
                        this.toggle_preset_list();
                        cx.notify();
                    });
                }
            }),
            on_select_preset: Arc::new({
                let entity = entity.clone();
                move |(bank, patch): &(i32, i32), _window, app: &mut App| {
                    let (bank, patch) = (*bank, *patch);
                    let _ = entity.update(app, |this, cx| {
                        this.select_preset(bank, patch);
                        this.notify_track_update(cx);
                        cx.notify();
                    });
                }
            }),
            on_set_volume: Arc::new({
                let entity = entity.clone();
                move |value: &f32, _window, app: &mut App| {
                    let value = *value;
                    let _ = entity.update(app, |this, cx| {
                        this.set_volume(value);
                        this.notify_track_update(cx);
                        cx.notify();
                    });
                }
            }),
            on_toggle_reverb_chorus: Arc::new({
                let entity = entity.clone();
                move |_window, app: &mut App| {
                    let _ = entity.update(app, |this, cx| {
                        this.toggle_reverb_chorus();
                        this.notify_track_update(cx);
                        cx.notify();
                    });
                }
            }),
            on_set_polyphony: Arc::new({
                let entity = entity.clone();
                move |value: &usize, _window, app: &mut App| {
                    let value = *value;
                    let _ = entity.update(app, |this, cx| {
                        this.set_polyphony(value);
                        this.notify_track_update(cx);
                        cx.notify();
                    });
                }
            }),
            on_set_envelope: Arc::new({
                let entity = entity.clone();
                move |envelope: &SoundfontEnvelope, _window, app: &mut App| {
                    let envelope = *envelope;
                    let _ = entity.update(app, |this, cx| {
                        this.set_envelope(envelope);
                        this.notify_track_update(cx);
                        cx.notify();
                    });
                }
            }),
            on_set_quality: Arc::new({
                let entity = entity.clone();
                move |quality: &SoundfontRenderQuality, _window, app: &mut App| {
                    let quality = *quality;
                    let _ = entity.update(app, |this, cx| {
                        this.set_quality(quality);
                        this.notify_track_update(cx);
                        cx.notify();
                    });
                }
            }),
            on_note_on: Arc::new({
                let entity = entity.clone();
                move |pitch: &u8, _window, app: &mut App| {
                    let pitch = *pitch;
                    let _ = entity.update(app, |this, cx| {
                        this.note_on(pitch, cx);
                        cx.notify();
                    });
                }
            }),
            on_note_off: Arc::new({
                let entity = entity.clone();
                move |pitch: &u8, _window, app: &mut App| {
                    let pitch = *pitch;
                    let _ = entity.update(app, |this, cx| {
                        this.note_off(pitch, cx);
                        cx.notify();
                    });
                }
            }),
            on_test: Arc::new({
                let entity = entity.clone();
                move |_window, app: &mut App| {
                    let _ = entity.update(app, |this, cx| {
                        this.start_test(cx);
                        cx.notify();
                    });
                }
            }),
            on_all_notes_off: Arc::new({
                let entity = entity.clone();
                move |_window, app: &mut App| {
                    let _ = entity.update(app, |this, cx| {
                        this.release_all(cx);
                        cx.notify();
                    });
                }
            }),
            on_shift_octave: Arc::new(move |delta: &i32, _window, app: &mut App| {
                let delta = *delta;
                let _ = entity.update(app, |this, cx| {
                    this.shift_octave(delta, cx);
                    cx.notify();
                });
            }),
        };
        let panel = self.panel.clone();

        div()
            .flex()
            .flex_col()
            .size_full()
            .font(crate::theme::ui_font())
            .bg(Colors::surface_window())
            .overflow_hidden()
            .child(div().w(px(0.0)).h(px(0.0)).track_focus(&self.focus_handle))
            .child(external_window_titlebar(
                SOUNDFONT_PLAYER_MDI_TITLE,
                "soundfont-player-window-close",
                {
                    let entity = self_entity.clone();
                    move |window, cx| {
                        // Closing must not leave an auditioned note held on the
                        // engine — nothing would ever send its note-off.
                        let _ = entity.update(cx, |this, cx| this.release_all(cx));
                        on_close(window, cx);
                        window.remove_window();
                    }
                },
            ))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .relative()
                    .child(soundfont_player_panel(&panel, panel_callbacks)),
            )
    }
}

pub fn open_soundfont_player_window(
    owner_bounds: Option<Bounds<gpui::Pixels>>,
    track_id: String,
    initial: SoundfontPlayerTrackState,
    on_close: Arc<dyn Fn(&mut Window, &mut App) + Send + Sync>,
    on_update_track: Arc<dyn Fn(SoundfontPlayerTrackUpdate, &mut App) + Send + Sync>,
    on_preview: PreviewCb,
    cx: &mut App,
) -> Result<WindowHandle<SoundfontPlayerWindow>, String> {
    let window_bounds = crate::window_position::centered_window_bounds(
        owner_bounds,
        size(
            px(SOUNDFONT_PLAYER_WINDOW_WIDTH),
            px(SOUNDFONT_PLAYER_WINDOW_HEIGHT),
        ),
        cx,
    );
    let mut options = crate::platform_chrome::external_dialog_window_options_partial();
    options.window_bounds = Some(WindowBounds::Windowed(window_bounds));
    options.kind = WindowKind::Floating;
    options.is_resizable = true;
    options.is_minimizable = true;
    options.window_background = WindowBackgroundAppearance::Opaque;
    options.window_min_size = Some(size(
        px(SOUNDFONT_PLAYER_WINDOW_MIN_WIDTH),
        px(SOUNDFONT_PLAYER_WINDOW_MIN_HEIGHT),
    ));
    crate::window_position::apply_owner_display(&mut options, owner_bounds, cx);

    cx.open_window(options, move |_window, cx| {
        cx.new(|cx| {
            SoundfontPlayerWindow::new(track_id, initial, on_close, on_update_track, on_preview, cx)
        })
    })
    .map_err(|error| error.to_string())
}
