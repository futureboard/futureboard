use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::{
    div, img, px, svg, App, Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, ParentElement, Render, Role, SharedString, StatefulInteractiveElement, Styled,
    Window, WindowControlArea,
};
use serde::Deserialize;
use serde_json::Value;

const FEED_API_URL: &str = "https://feed.futureboard.studio/api/posts";
const FEED_PUBLIC_BASE_URL: &str = "https://futureboard.studio/blog";
const FEED_FETCH_TIMEOUT_SECS: u64 = 8;

use crate::assets;
use crate::components::text_input::{
    bind_mouse_selection, is_repeatable_edit_key, text_field_with_callbacks_and_ime,
    TextInputCallbacks,
};
use crate::components::title_bar::{draggable_spacer, section_separator, window_control_button};
use crate::components::{TextInputAction, TextInputState};
use crate::embedded_assets::LOGO_TEXT_PATH;
use crate::i18n::I18n;
use crate::platform_chrome::PlatformChromePolicy;
use crate::project::{ProjectCreateOptions, ProjectTemplate, RecentProject, RecentProjectsStore};
use crate::settings::SettingsSchema;
use crate::theme::{self, Colors};

/// `FUTUREBOARD_WELCOME_DEBUG=1` enables QA logging for the start screen
/// (selected tab, resolved default project path, and recent project activity).
fn welcome_debug(args: std::fmt::Arguments<'_>) {
    if std::env::var("FUTUREBOARD_WELCOME_DEBUG").as_deref() == Ok("1") {
        eprintln!("[welcome] {args}");
    }
}

macro_rules! welcome_debug {
    ($($arg:tt)*) => { welcome_debug(format_args!($($arg)*)) };
}

#[derive(Debug, Clone, PartialEq)]
pub enum WelcomeAction {
    EmptyProject,
    MidiComposer,
    AudioSession,
    MixTemplate,
    CreateProject(ProjectCreateOptions),
    /// Legacy: request the studio-side Open Project dialog. Kept for back-compat;
    /// the Welcome screen now browses + validates itself and emits
    /// [`WelcomeAction::OpenProjectFile`] instead.
    OpenProject,
    /// Open a specific, already-validated project file (from the Welcome
    /// Open Project tab's browse flow).
    OpenProjectFile(PathBuf),
    OpenRecent(PathBuf),
    /// Open the workspace shell directly with a blank, unsaved project.
    OpenEmptyWorkspace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StartupNav {
    Welcome,
    NewProject,
    OpenProject,
    RecentProjects,
    Feed,
    AudioSetup,
}

/// Which New Project dropdown is open. Only one may be open at a time, so the
/// pane needs a single piece of state rather than one flag per field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectFieldMenu {
    SampleRate,
    Bpm,
    TimeSignature,
}

/// Sample rates offered when creating a project. Matches the rates the audio
/// settings expose, so a new project cannot be created at a rate the engine is
/// never configured for.
const PROJECT_SAMPLE_RATES: [u32; 5] = [44_100, 48_000, 88_200, 96_000, 192_000];

/// Tempo presets. Free entry stays available in Project Settings once the
/// project exists; the start screen only needs the common starting points.
const PROJECT_BPM_PRESETS: [f32; 10] = [
    70.0, 80.0, 90.0, 100.0, 110.0, 120.0, 128.0, 140.0, 160.0, 174.0,
];

/// Time signatures offered when creating a project.
const PROJECT_TIME_SIGNATURES: [(u32, u32); 8] = [
    (4, 4),
    (3, 4),
    (2, 4),
    (6, 8),
    (5, 4),
    (7, 8),
    (9, 8),
    (12, 8),
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum WelcomeSelection {
    Start(usize),
    Recent(usize),
    Continue,
}

#[derive(Debug, Clone)]
enum FeedLoadState {
    Idle,
    Loading,
    Loaded,
    Failed(SharedString),
}

#[derive(Debug, Clone)]
struct FeedPost {
    title: SharedString,
    excerpt: SharedString,
    published_at: SharedString,
    slug: Option<SharedString>,
}

#[derive(Debug, Deserialize)]
struct FeedResponse {
    docs: Vec<PayloadPost>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PayloadPost {
    title: String,
    slug: Option<String>,
    published_at: Option<String>,
    content: Option<Value>,
    meta: Option<PayloadPostMeta>,
}

#[derive(Debug, Deserialize)]
struct PayloadPostMeta {
    description: Option<String>,
}

#[derive(Clone)]
pub struct WelcomeCallbacks {
    pub on_action: Arc<dyn Fn(WelcomeAction, &mut Window, &mut App) + 'static>,
    /// Optional edition-owned action rendered at the bottom of the Welcome rail.
    pub footer_action: Option<WelcomeFooterAction>,
}

#[derive(Clone)]
pub struct WelcomeFooterAction {
    pub label: &'static str,
    pub icon: &'static str,
    pub on_click: Arc<dyn Fn(&mut Window, &mut App) + 'static>,
}

pub struct WelcomeWindow {
    active_nav: StartupNav,
    recent_projects: Vec<RecentProject>,
    selected: Option<WelcomeSelection>,
    callbacks: WelcomeCallbacks,
    project_name_input: TextInputState,
    selected_template: ProjectTemplate,
    project_sample_rate: u32,
    project_bpm: f32,
    project_time_signature: (u32, u32),
    /// Open New Project dropdown, if any.
    open_project_menu: Option<ProjectFieldMenu>,
    // Default project location (resolved at construction from settings)
    default_project_dir: PathBuf,
    default_dir_configured: bool,
    // Quick audio status readout (from saved settings)
    audio_backend: SharedString,
    audio_device_out: SharedString,
    // Open Project tab: inline validation error from the last browse attempt.
    open_error: Option<SharedString>,
    feed_state: FeedLoadState,
    feed_posts: Vec<FeedPost>,
    /// UI language from settings at construction (`schema.general.language`).
    language: String,
}

impl WelcomeWindow {
    pub fn new(callbacks: WelcomeCallbacks, focus_handle: FocusHandle) -> Self {
        let mut recent = RecentProjectsStore::load();
        recent.refresh_missing();

        // Default project path + audio readout from saved settings (the global
        // SettingsModel entity does not exist yet at Welcome time).
        let schema = SettingsSchema::load_from_disk();
        let default_project_dir = schema.general.resolved_default_project_dir();
        let default_dir_configured = schema.general.has_configured_project_dir();
        welcome_debug!(
            "default project path resolved -> {} (configured={})",
            default_project_dir.display(),
            default_dir_configured
        );

        let language = schema.general.language.clone();
        let i18n = I18n::new(&language);
        let default_name = i18n.tr("project.default-name");
        let mut project_name_input = TextInputState::new("welcome-project-name", focus_handle)
            .with_placeholder(default_name.clone());
        project_name_input.set_value(default_name);

        Self {
            active_nav: StartupNav::Welcome,
            recent_projects: recent.entries().iter().take(7).cloned().collect(),
            selected: Some(WelcomeSelection::Start(0)),
            callbacks,
            project_name_input,
            selected_template: ProjectTemplate::Empty,
            project_sample_rate: schema.general.project_defaults.sample_rate,
            project_bpm: 120.0,
            project_time_signature: (4, 4),
            open_project_menu: None,
            default_project_dir,
            default_dir_configured,
            audio_backend: SharedString::from(schema.hardware.audio.driver_type),
            audio_device_out: SharedString::from(schema.hardware.audio.device_out),
            open_error: None,
            feed_state: FeedLoadState::Idle,
            feed_posts: Vec::new(),
            language,
        }
    }

    fn handle_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.is_held && !is_repeatable_edit_key(event) {
            return;
        }
        let modifiers = event.keystroke.modifiers;
        if event.keystroke.key.eq_ignore_ascii_case("tab")
            && !modifiers.control
            && !modifiers.alt
            && !modifiers.platform
            && !modifiers.function
        {
            if modifiers.shift {
                window.focus_prev(cx);
            } else {
                window.focus_next(cx);
            }
            window.prevent_default();
            cx.stop_propagation();
            return;
        }
        if self.project_name_input.is_focused(window) {
            let action = self.project_name_input.handle_key_ime(event, Some(cx));
            welcome_debug!("project name typed -> {}", self.project_name_input.value);
            match action {
                TextInputAction::Submit => self.create_project_from_welcome(window, cx),
                TextInputAction::Cancel => {
                    self.active_nav = StartupNav::Welcome;
                    cx.notify();
                }
                TextInputAction::Consumed | TextInputAction::Pass => cx.notify(),
            }
            return;
        }
        if let Some((index, action)) = welcome_shortcut_action(event) {
            self.show_start_action(index, &action, cx);
            return;
        }
        match event.keystroke.key.as_str() {
            "enter" | "numpad_enter" => {
                if let Some(action) = self.selected_action() {
                    // Open Project is handled inside Welcome (browse + validate),
                    // so Enter just reveals the Open Project tab rather than
                    // firing the legacy studio-side dialog.
                    if action == WelcomeAction::OpenProject {
                        self.active_nav = StartupNav::OpenProject;
                        cx.notify();
                    } else {
                        (self.callbacks.on_action)(action, window, cx);
                    }
                }
            }
            // Escape cancels the transient dropdown before it closes the window.
            "escape" => {
                if self.open_project_menu.take().is_some() {
                    cx.notify();
                } else {
                    window.remove_window();
                }
            }
            _ => {}
        }
    }

    fn show_start_action(&mut self, index: usize, action: &WelcomeAction, cx: &mut Context<Self>) {
        self.selected = Some(WelcomeSelection::Start(index));
        if action == &WelcomeAction::OpenProject {
            self.active_nav = StartupNav::OpenProject;
        } else if let Some(template) = template_for_action(action) {
            self.selected_template = template;
            self.project_bpm = template.default_bpm();
            self.project_time_signature = template.time_signature();
            self.active_nav = StartupNav::NewProject;
        }
        cx.notify();
    }

    fn create_project_from_welcome(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = crate::project::io::sanitize_project_name(&self.project_name_input.value);
        let options = ProjectCreateOptions {
            name,
            base_dir: self.default_project_dir.clone(),
            template: self.selected_template,
            sample_rate: self.project_sample_rate,
            bpm: self.project_bpm,
            time_signature_num: self.project_time_signature.0,
            time_signature_den: self.project_time_signature.1,
        };
        welcome_debug!(
            "create project requested name={} dir={} template={}",
            options.name,
            options.base_dir.display(),
            options.template.label()
        );
        (self.callbacks.on_action)(WelcomeAction::CreateProject(options), window, cx);
    }

    fn selected_action(&self) -> Option<WelcomeAction> {
        match self.selected {
            Some(WelcomeSelection::Start(index)) => {
                start_rows().get(index).map(|row| row.action.clone())
            }
            Some(WelcomeSelection::Recent(index)) => self
                .recent_projects
                .get(index)
                .filter(|recent| !recent.missing)
                .map(|recent| WelcomeAction::OpenRecent(recent.path.clone())),
            Some(WelcomeSelection::Continue) => Some(WelcomeAction::OpenEmptyWorkspace),
            None => None,
        }
    }

    /// Open a folder picker to choose the default project directory. Persists
    /// the choice to settings on success. If the picker is unavailable or the
    /// user cancels, nothing changes (no faked success).
    fn change_default_dir(&mut self, cx: &mut Context<Self>) {
        #[cfg(feature = "native-dialogs")]
        {
            let start_dir = self.default_project_dir.clone();
            let entity = cx.entity().clone();
            cx.spawn(async move |_this, cx| {
                let result = rfd::AsyncFileDialog::new()
                    .set_title("Choose Default Project Location")
                    .set_directory(&start_dir)
                    .pick_folder()
                    .await;
                let Some(handle) = result else {
                    welcome_debug!("default project path change cancelled");
                    return;
                };
                let path = handle.path().to_path_buf();
                // Best-effort: create the folder now so it is ready for new projects.
                let _ = std::fs::create_dir_all(&path);
                SettingsSchema::persist_default_project_directory(Some(path.clone()));
                welcome_debug!("default project path changed -> {}", path.display());
                let _ = entity.update(cx, |this, cx| {
                    this.default_project_dir = path;
                    this.default_dir_configured = true;
                    cx.notify();
                });
            })
            .detach();
        }

        #[cfg(not(feature = "native-dialogs"))]
        {
            self.open_error = Some(SharedString::from(
                "Native file dialogs are unavailable in this build.",
            ));
            cx.notify();
        }
    }

    /// Load the public PayloadCMS feed once per Welcome window. Network work stays
    /// on GPUI's background executor and uses the blocking HTTP client so it does
    /// not require a Tokio runtime on the UI/task executors.
    fn fetch_feed_if_needed(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.feed_state, FeedLoadState::Idle) {
            return;
        }
        self.feed_state = FeedLoadState::Loading;
        let entity = cx.entity().clone();
        cx.spawn(async move |_this, cx| {
            let result = cx
                .background_executor()
                .spawn(async { fetch_feed_posts() })
                .await;
            let _ = entity.update(cx, |this, cx| {
                match result {
                    Ok(posts) => {
                        this.feed_posts = posts;
                        this.feed_state = FeedLoadState::Loaded;
                    }
                    Err(error) => {
                        this.feed_posts.clear();
                        this.feed_state = FeedLoadState::Failed(SharedString::from(error));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn browse_and_open_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_error = None;
        cx.notify();
        #[cfg(feature = "native-dialogs")]
        {
            let start_dir = self.default_project_dir.clone();
            let on_action = self.callbacks.on_action.clone();
            // `spawn_in` keeps a window handle across the async picker await, so the
            // validated path can be handed to `on_action` (which needs a Window to
            // close Welcome) once the picker resolves.
            cx.spawn_in(window, async move |this, cx| {
                let result = rfd::AsyncFileDialog::new()
                    .set_title("Open Project")
                    .set_directory(&start_dir)
                    .add_filter(
                        "Futureboard Project",
                        crate::project::io::SUPPORTED_PROJECT_FILE_EXTS,
                    )
                    // Projects exported by another DAW open through the
                    // importers in `crate::project::import`.
                    .add_filter(
                        "Cubase XML Track Archive",
                        crate::project::IMPORT_PROJECT_FILE_EXTS,
                    )
                    .pick_file()
                    .await;
                let Some(handle) = result else {
                    welcome_debug!("open project cancelled");
                    return;
                };
                let path = handle.path().to_path_buf();
                match crate::project::validate_project_file(&path) {
                    Ok(version) => {
                        welcome_debug!("open project validated (v{version}) -> {}", path.display());
                        // Hand off to the app: opens the studio loading `path` and
                        // closes Welcome (see the on_action callback in app.rs).
                        let _ = cx.update(|window, app| {
                            on_action(WelcomeAction::OpenProjectFile(path), window, app);
                        });
                    }
                    Err(e) => {
                        let msg = format!("{} Details: {}", e.user_message(), e.technical_detail());
                        welcome_debug!("open project rejected -> {msg}");
                        let _ = this.update(cx, |this, cx| {
                            this.open_error = Some(SharedString::from(msg));
                            this.active_nav = StartupNav::OpenProject;
                            cx.notify();
                        });
                    }
                }
            })
            .detach();
        }

        #[cfg(not(feature = "native-dialogs"))]
        {
            let _ = window;
            self.open_error = Some(SharedString::from(
                "Native file dialogs are unavailable in this build.",
            ));
            self.active_nav = StartupNav::OpenProject;
            cx.notify();
        }
    }
}

fn welcome_shortcut_action(event: &KeyDownEvent) -> Option<(usize, WelcomeAction)> {
    let modifiers = event.keystroke.modifiers;
    if !(modifiers.control || modifiers.platform) || modifiers.alt {
        return None;
    }
    let key = event.keystroke.key.as_str();
    match (key, modifiers.shift) {
        ("n", false) => Some((0, WelcomeAction::EmptyProject)),
        ("m", true) => Some((1, WelcomeAction::MidiComposer)),
        ("a", true) => Some((2, WelcomeAction::AudioSession)),
        ("t", true) => Some((3, WelcomeAction::MixTemplate)),
        ("o", false) => Some((4, WelcomeAction::OpenProject)),
        _ => None,
    }
}

// Route platform IME (CJK/Thai composition + candidate-window positioning) to
// the project-name field. Coexists with handle_key_with_clipboard; GPUI
// suppresses key dispatch for keystrokes the IME consumes.
crate::impl_single_input_window_ime!(WelcomeWindow, project_name_input);

impl Render for WelcomeWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let i18n = I18n::new(&self.language);
        let target = cx.entity().clone();
        let project_name_callbacks =
            bind_mouse_selection(target.clone(), |this| &mut this.project_name_input);
        div()
            .id("futureboard-welcome-root")
            .role(Role::Application)
            .aria_label("Futureboard Studio Welcome")
            .key_context("WelcomeWindow")
            .capture_key_down(move |event, window, cx| {
                let _ = target.update(cx, |this, cx| this.handle_key(event, window, cx));
            })
            .flex()
            .flex_col()
            .size_full()
            .font(theme::ui_font())
            .bg(Colors::surface_window())
            .cursor(gpui::CursorStyle::Arrow)
            // Swallow the platform's default click-drag gesture on the Welcome
            // root so static labels/rows never render as "selected" while the
            // user drags across them (GPUI has no CSS `user-select`). Rows and
            // buttons register their own `on_mouse_down` deeper in the tree and
            // fire first (bubble order), so clicks are unaffected. `TextInputState`
            // calls `cx.stop_propagation()` on its own mouse-down, so this never
            // reaches (and never affects) the project-name field, which keeps its
            // own IBeam cursor and drag-to-select behavior.
            .on_mouse_down(MouseButton::Left, |_event, window, _cx| {
                window.prevent_default();
            })
            .child(startup_titlebar(window))
            .child(self.render_welcome(window, cx, project_name_callbacks, i18n))
    }
}

impl WelcomeWindow {
    fn render_welcome(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        project_name_callbacks: TextInputCallbacks,
        i18n: I18n,
    ) -> gpui::AnyElement {
        // Center pane content depends on the selected sidebar tab.
        let center = match self.active_nav {
            StartupNav::NewProject => new_project_pane(
                cx,
                &self.project_name_input,
                self.project_name_input.is_focused(window),
                project_name_callbacks,
                self.selected_template,
                ProjectFieldValues {
                    sample_rate: self.project_sample_rate,
                    bpm: self.project_bpm,
                    time_signature: self.project_time_signature,
                    open_menu: self.open_project_menu,
                },
                self.default_project_dir.clone(),
                &self.callbacks,
                i18n,
            ),
            StartupNav::OpenProject => open_project_pane(
                cx,
                &self.recent_projects,
                self.open_error.clone(),
                &self.callbacks,
                i18n,
            ),
            StartupNav::Feed => {
                self.fetch_feed_if_needed(cx);
                feed_pane(&self.feed_state, &self.feed_posts)
            }
            StartupNav::AudioSetup => {
                audio_setup_pane(self.audio_backend.clone(), self.audio_device_out.clone())
            }
            _ => center_actions(
                cx,
                &self.selected,
                self.selected_template,
                &self.callbacks,
                i18n,
            ),
        };

        let dismiss_target = cx.entity().clone();
        let menu_open = self.open_project_menu.is_some();

        div()
            // Anchor plane for the New Project dropdowns' click-outside
            // backdrop, which must cover the whole start-screen body.
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .bg(Colors::surface_panel())
            .child(welcome_header())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .child(left_rail(cx, &self.active_nav, &self.callbacks, i18n))
                    .child(center)
                    .child(right_panel(
                        cx,
                        &self.recent_projects,
                        &self.selected,
                        &self.callbacks,
                        self.default_project_dir.clone(),
                        self.default_dir_configured,
                        i18n,
                    )),
            )
            .when(menu_open, |root| {
                root.child(crate::components::form::select_dismiss_backdrop(Arc::new(
                    move |_: &(), _window, cx| {
                        let _ = dismiss_target.update(cx, |this, cx| {
                            this.open_project_menu = None;
                            cx.notify();
                        });
                    },
                )))
            })
            .into_any_element()
    }
}

#[derive(Clone)]
struct StartRow {
    title: &'static str,
    description: &'static str,
    shortcut: String,
    icon: &'static str,
    action: WelcomeAction,
}

fn start_rows() -> Vec<StartRow> {
    let modifier = if cfg!(target_os = "macos") {
        "Cmd"
    } else {
        "Ctrl"
    };
    vec![
        StartRow {
            title: "Blank Project",
            description: "Clean arrangement",
            shortcut: format!("{modifier} + N"),
            icon: assets::ICON_PLUS_PATH,
            action: WelcomeAction::EmptyProject,
        },
        StartRow {
            title: "MIDI Project",
            description: "Instruments and piano roll",
            shortcut: format!("{modifier} + Shift + M"),
            icon: assets::ICON_MUSIC_PATH,
            action: WelcomeAction::MidiComposer,
        },
        StartRow {
            title: "Audio Session",
            description: "Record and edit audio",
            shortcut: format!("{modifier} + Shift + A"),
            icon: assets::ICON_MIC_PATH,
            action: WelcomeAction::AudioSession,
        },
        StartRow {
            title: "Mix Template",
            description: "Mixer routing ready",
            shortcut: format!("{modifier} + Shift + T"),
            icon: assets::ICON_SLIDERS_HORIZONTAL_PATH,
            action: WelcomeAction::MixTemplate,
        },
        StartRow {
            title: "Open Project...",
            description: "Choose a project file",
            shortcut: format!("{modifier} + O"),
            icon: assets::ICON_FOLDER_OPEN_PATH,
            action: WelcomeAction::OpenProject,
        },
    ]
}

fn startup_titlebar(window: &Window) -> impl IntoElement {
    let policy = PlatformChromePolicy::current();

    let mut chrome = div()
        .flex()
        .flex_row()
        .items_center()
        .h(px(policy.titlebar_height_px))
        .w_full()
        .pl(policy.traffic_light_left_padding())
        .bg(Colors::surface_titlebar())
        .border_b(px(1.0))
        .border_color(Colors::border_subtle())
        .window_control_area(WindowControlArea::Drag)
        .on_mouse_down(gpui::MouseButton::Left, |_, window, _cx| {
            window.start_window_move();
        })
        .child(draggable_spacer());

    if policy.show_window_controls {
        let is_maximized = window.is_maximized();
        let (max_path, max_fallback) = if is_maximized {
            (assets::ICON_RESTORE_PATH, "RESTORE")
        } else {
            (assets::ICON_MAXIMIZE_PATH, "MAX")
        };
        chrome = chrome
            .child(section_separator())
            .child(window_control_button(
                WindowControlArea::Min,
                assets::ICON_MINIMIZE_PATH,
                "-",
            ))
            .child(window_control_button(
                WindowControlArea::Max,
                max_path,
                max_fallback,
            ))
            .child(window_control_button(
                WindowControlArea::Close,
                assets::ICON_X_PATH,
                "X",
            ));
    }
    chrome
}

fn welcome_header() -> impl IntoElement {
    let mut header = div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(16.0))
        .h(px(44.0))
        .px(px(16.0))
        .border_b(px(1.0))
        .border_color(Colors::border_subtle())
        .bg(Colors::surface_panel())
        .child(
            img(SharedString::from(LOGO_TEXT_PATH))
                .w(px(199.0))
                .h(px(18.0))
                .flex_none(),
        );

    if let Some(account) = crate::components::app_chrome::account_chip() {
        header = header.child(account);
    }

    header
}

fn left_rail(
    cx: &mut Context<WelcomeWindow>,
    active: &StartupNav,
    callbacks: &WelcomeCallbacks,
    i18n: I18n,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .w(px(164.0))
        .flex_none()
        .border_r(px(1.0))
        .border_color(Colors::border_subtle())
        .bg(Colors::surface_sidebar())
        .px(px(6.0))
        .py(px(8.0))
        .gap(px(1.0))
        .child(rail_item(
            cx,
            StartupNav::Welcome,
            "welcome-rail-start",
            i18n.tr("welcome.nav.start"),
            assets::ICON_STAR_PATH,
            active,
            None,
        ))
        .child(rail_item(
            cx,
            StartupNav::NewProject,
            "welcome-rail-new",
            i18n.tr("welcome.nav.new"),
            assets::ICON_PLUS_PATH,
            active,
            None,
        ))
        .child(rail_item(
            cx,
            StartupNav::OpenProject,
            "welcome-rail-open",
            i18n.tr("welcome.open-project"),
            assets::ICON_FOLDER_OPEN_PATH,
            active,
            None,
        ))
        .child(rail_item(
            cx,
            StartupNav::RecentProjects,
            "welcome-rail-recent",
            i18n.tr("welcome.nav.recent"),
            assets::ICON_CLOCK_PATH,
            active,
            None,
        ))
        .child(rail_item(
            cx,
            StartupNav::Feed,
            "welcome-rail-feed",
            i18n.tr("welcome.nav.feed"),
            assets::ICON_NEWSPAPER_PATH,
            active,
            None,
        ))
        .child(rail_item(
            cx,
            StartupNav::AudioSetup,
            "welcome-rail-audio",
            i18n.tr("welcome.nav.audio"),
            assets::ICON_VOLUME_2_PATH,
            active,
            None,
        ))
        .child(div().flex_1())
        .when_some(callbacks.footer_action.clone(), |rail, action| {
            rail.child(rail_item(
                cx,
                StartupNav::Welcome,
                "welcome-rail-footer",
                action.label.to_string(),
                action.icon,
                active,
                Some(action.on_click),
            ))
        })
}

fn rail_item(
    cx: &mut Context<WelcomeWindow>,
    nav: StartupNav,
    id: &'static str,
    label: impl Into<SharedString>,
    icon: &'static str,
    active: &StartupNav,
    action: Option<Arc<dyn Fn(&mut Window, &mut App) + 'static>>,
) -> impl IntoElement {
    let label = label.into();
    // Action rows (Open Project) never show the active highlight — only real
    // tabs do.
    let is_active = action.is_none() && active == &nav;
    let changes_nav = action.is_none();
    let target = cx.entity().clone();
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label.clone())
        .aria_selected(is_active)
        .focusable()
        .tab_stop(true)
        .focus_visible(|style| style.bg(Colors::surface_card_hover()))
        .flex()
        .items_center()
        .gap(px(8.0))
        .h(px(28.0))
        .px(px(7.0))
        .rounded_sm()
        .bg(if is_active {
            Colors::surface_selected()
        } else {
            Colors::surface_sidebar()
        })
        .border_l(px(if is_active { 2.0 } else { 0.0 }))
        .border_color(Colors::accent_primary())
        .cursor(gpui::CursorStyle::PointingHand)
        .hover(|style| style.bg(Colors::surface_card_hover()))
        .on_click(move |_event, window, cx| {
            if changes_nav {
                let _ = target.update(cx, |this, cx| {
                    this.active_nav = nav.clone();
                    welcome_debug!("selected tab -> {:?}", this.active_nav);
                    cx.notify();
                });
            }
            if let Some(callback) = &action {
                callback(window, cx);
            }
        })
        .child(
            svg()
                .path(icon)
                .w(px(12.0))
                .h(px(12.0))
                .text_color(if is_active {
                    Colors::text_primary()
                } else {
                    Colors::text_muted()
                }),
        )
        .child(
            div()
                .text_size(px(10.5))
                .font_weight(if is_active {
                    gpui::FontWeight::SEMIBOLD
                } else {
                    gpui::FontWeight::NORMAL
                })
                .text_color(if is_active {
                    Colors::text_primary()
                } else {
                    Colors::text_muted()
                })
                .child(label),
        )
}

fn center_actions(
    cx: &mut Context<WelcomeWindow>,
    selected: &Option<WelcomeSelection>,
    _selected_template: ProjectTemplate,
    callbacks: &WelcomeCallbacks,
    i18n: I18n,
) -> gpui::AnyElement {
    let mut rows = div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .max_w(px(620.0))
        .w_full();
    for (index, row) in start_rows().into_iter().enumerate() {
        let target = cx.entity().clone();
        let is_selected = selected == &Some(WelcomeSelection::Start(index));
        let action = row.action.clone();
        let on_action = callbacks.on_action.clone();
        rows = rows.child(start_row(index, row, is_selected, move |window, cx| {
            // Open Project lives in its own Welcome tab (browse + validate), so
            // its card reveals that tab rather than firing the legacy dialog.
            if action == WelcomeAction::OpenProject {
                let _ = target.update(cx, |this, cx| {
                    this.selected = Some(WelcomeSelection::Start(index));
                    this.active_nav = StartupNav::OpenProject;
                    cx.notify();
                });
                return;
            }
            if let Some(template) = template_for_action(&action) {
                let _ = target.update(cx, |this, cx| {
                    this.selected = Some(WelcomeSelection::Start(index));
                    this.selected_template = template;
                    this.project_bpm = template.default_bpm();
                    this.project_time_signature = template.time_signature();
                    this.active_nav = StartupNav::NewProject;
                    cx.notify();
                });
                return;
            }
            let _ = target.update(cx, |this, cx| {
                this.selected = Some(WelcomeSelection::Start(index));
                this.active_nav = StartupNav::NewProject;
                cx.notify();
            });
            on_action(action.clone(), window, cx);
        }));
    }

    let continue_selected = selected == &Some(WelcomeSelection::Continue);
    let target = cx.entity().clone();
    let on_continue = callbacks.on_action.clone();
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .p(px(20.0))
        .gap(px(10.0))
        .bg(Colors::surface_panel())
        .child(section_label(i18n.tr("welcome.nav.start")))
        .child(rows)
        .child(
            div()
                .mt(px(2.0))
                .max_w(px(620.0))
                .w_full()
                .child(continue_row(continue_selected, i18n, move |window, cx| {
                    let _ = target.update(cx, |this, cx| {
                        this.selected = Some(WelcomeSelection::Continue);
                        this.active_nav = StartupNav::Welcome;
                        cx.notify();
                    });
                    on_continue(WelcomeAction::OpenEmptyWorkspace, window, cx);
                })),
        )
        .into_any_element()
}

fn template_for_action(action: &WelcomeAction) -> Option<ProjectTemplate> {
    match action {
        WelcomeAction::EmptyProject => Some(ProjectTemplate::Empty),
        WelcomeAction::MidiComposer => Some(ProjectTemplate::BeatMaking),
        WelcomeAction::AudioSession => Some(ProjectTemplate::Recording),
        WelcomeAction::MixTemplate => Some(ProjectTemplate::Mixing),
        _ => None,
    }
}

/// The project-format fields the New Project pane edits, plus which of their
/// dropdowns is open. Grouped so the pane keeps one argument for "the format of
/// the project being created" instead of four positional ones.
#[derive(Clone, Copy)]
struct ProjectFieldValues {
    sample_rate: u32,
    bpm: f32,
    time_signature: (u32, u32),
    open_menu: Option<ProjectFieldMenu>,
}

/// One labelled dropdown row in the New Project pane. Every option commits
/// straight into `WelcomeWindow`'s real create-project state, so what the pane
/// shows is what [`WelcomeWindow::create_project_from_welcome`] sends.
fn project_field_row(
    cx: &mut Context<WelcomeWindow>,
    id: &'static str,
    label: impl Into<SharedString>,
    menu: ProjectFieldMenu,
    open_menu: Option<ProjectFieldMenu>,
    selected_id: String,
    options: Vec<crate::components::form::SelectOption>,
    apply: impl Fn(&mut WelcomeWindow, &str) + 'static,
) -> impl IntoElement {
    let toggle_target = cx.entity().clone();
    let change_target = cx.entity().clone();
    div()
        .flex()
        .flex_col()
        .gap(px(3.0))
        .flex_1()
        .min_w(px(0.0))
        .child(form_label(label))
        .child(crate::components::form::select(
            id,
            Some(selected_id.as_str()),
            "-",
            options,
            open_menu == Some(menu),
            false,
            Arc::new(move |_: &(), _window, cx| {
                let _ = toggle_target.update(cx, |this, cx| {
                    this.open_project_menu = if this.open_project_menu == Some(menu) {
                        None
                    } else {
                        Some(menu)
                    };
                    cx.notify();
                });
            }),
            Arc::new(move |value: &String, _window, cx| {
                let value = value.clone();
                let _ = change_target.update(cx, |this, cx| {
                    apply(this, &value);
                    this.open_project_menu = None;
                    cx.notify();
                });
            }),
        ))
}

#[allow(clippy::too_many_arguments)]
fn new_project_pane(
    cx: &mut Context<WelcomeWindow>,
    project_name_input: &TextInputState,
    name_focused: bool,
    name_callbacks: TextInputCallbacks,
    selected_template: ProjectTemplate,
    fields: ProjectFieldValues,
    default_dir: PathBuf,
    callbacks: &WelcomeCallbacks,
    i18n: I18n,
) -> gpui::AnyElement {
    let ProjectFieldValues {
        sample_rate,
        bpm,
        time_signature,
        open_menu,
    } = fields;
    let safe_name = crate::project::io::sanitize_project_name(&project_name_input.value);
    let preview = default_dir.join(&safe_name).to_string_lossy().to_string();
    let target = cx.entity().clone();
    let create_target = cx.entity().clone();
    let continue_target = cx.entity().clone();
    let on_continue = callbacks.on_action.clone();

    let mut template_row = div().flex().flex_row().flex_wrap().gap(px(6.0));
    for template in [
        ProjectTemplate::Empty,
        ProjectTemplate::BeatMaking,
        ProjectTemplate::Recording,
        ProjectTemplate::Mixing,
        ProjectTemplate::Scoring,
    ] {
        let is_active = selected_template == template;
        let target = target.clone();
        template_row = template_row.child(
            div()
                .id(SharedString::from(format!(
                    "welcome-template-{}",
                    template.label()
                )))
                .role(Role::Button)
                .aria_label(format!("{} project template", template.label()))
                .aria_selected(is_active)
                .focusable()
                .tab_stop(true)
                .focus_visible(|style| style.border_color(Colors::border_focus()))
                .flex()
                .items_center()
                .h(px(26.0))
                .px(px(9.0))
                .rounded_md()
                .border(px(1.0))
                .border_color(if is_active {
                    Colors::accent_primary()
                } else {
                    Colors::border_subtle()
                })
                .bg(if is_active {
                    Colors::surface_card_selected()
                } else {
                    Colors::surface_card()
                })
                .cursor(gpui::CursorStyle::PointingHand)
                .hover(|style| style.bg(Colors::surface_card_hover()))
                .on_click(move |_event, _window, cx| {
                    let _ = target.update(cx, |this, cx| {
                        this.selected_template = template;
                        this.project_bpm = template.default_bpm();
                        this.project_time_signature = template.time_signature();
                        cx.notify();
                    });
                })
                .child(
                    div()
                        .text_size(px(10.5))
                        .font_weight(if is_active {
                            gpui::FontWeight::SEMIBOLD
                        } else {
                            gpui::FontWeight::NORMAL
                        })
                        .text_color(if is_active {
                            Colors::text_primary()
                        } else {
                            Colors::text_muted()
                        })
                        .child(template.label()),
                ),
        );
    }

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .p(px(16.0))
        .gap(px(12.0))
        .bg(Colors::surface_panel())
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(3.0))
                .child(
                    div()
                        .text_size(px(13.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(Colors::text_primary())
                        .child(i18n.tr("wizard.title")),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(Colors::text_muted())
                        .child("Name it, choose a template, and start."),
                ),
        )
        .child(form_label(i18n.tr("wizard.field.name")))
        .child(text_field_with_callbacks_and_ime(
            project_name_input,
            name_focused,
            name_callbacks,
            target.clone(),
        ))
        .child(form_label(i18n.tr("wizard.field.location")))
        .child(
            div()
                .rounded_md()
                .border(px(1.0))
                .border_color(Colors::border_subtle())
                .bg(Colors::surface_card())
                .px(px(10.0))
                .py(px(8.0))
                .text_size(px(10.5))
                .text_color(Colors::text_secondary())
                .child(preview),
        )
        .child(form_label(i18n.tr("wizard.summary.template")))
        .child(template_row)
        .child(
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap(px(8.0))
                .child(project_field_row(
                    cx,
                    "welcome-project-sample-rate",
                    i18n.tr("settings.field.sample-rate"),
                    ProjectFieldMenu::SampleRate,
                    open_menu,
                    sample_rate.to_string(),
                    PROJECT_SAMPLE_RATES
                        .iter()
                        .map(|rate| {
                            crate::components::form::SelectOption::new(
                                rate.to_string(),
                                format!("{rate} Hz"),
                            )
                        })
                        .collect(),
                    |this, value| {
                        if let Ok(rate) = value.parse::<u32>() {
                            this.project_sample_rate = rate;
                        }
                    },
                ))
                .child(project_field_row(
                    cx,
                    "welcome-project-bpm",
                    i18n.tr("settings.bpm"),
                    ProjectFieldMenu::Bpm,
                    open_menu,
                    format!("{bpm:.0}"),
                    PROJECT_BPM_PRESETS
                        .iter()
                        .map(|preset| {
                            crate::components::form::SelectOption::new(
                                format!("{preset:.0}"),
                                format!("{preset:.0} BPM"),
                            )
                        })
                        .collect(),
                    |this, value| {
                        if let Ok(parsed) = value.parse::<f32>() {
                            this.project_bpm = parsed.clamp(20.0, 999.0);
                        }
                    },
                ))
                .child(project_field_row(
                    cx,
                    "welcome-project-time-signature",
                    "Time Signature",
                    ProjectFieldMenu::TimeSignature,
                    open_menu,
                    format!("{}/{}", time_signature.0, time_signature.1),
                    PROJECT_TIME_SIGNATURES
                        .iter()
                        .map(|(num, den)| {
                            let label = format!("{num}/{den}");
                            crate::components::form::SelectOption::new(label.clone(), label)
                        })
                        .collect(),
                    |this, value| {
                        if let Some((num, den)) = value.split_once('/') {
                            if let (Ok(num), Ok(den)) = (num.parse::<u32>(), den.parse::<u32>()) {
                                this.project_time_signature = (num.max(1), den.max(1));
                            }
                        }
                    },
                )),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .pt(px(4.0))
                .child(
                    div()
                        .id("welcome-create-project")
                        .flex()
                        .items_center()
                        .h(px(30.0))
                        .px(px(12.0))
                        .rounded_md()
                        .border(px(1.0))
                        .border_color(Colors::accent_primary())
                        .bg(Colors::surface_card_selected())
                        .cursor(gpui::CursorStyle::PointingHand)
                        .hover(|style| style.bg(Colors::surface_card_hover()))
                        .on_mouse_down(gpui::MouseButton::Left, move |_event, window, cx| {
                            let _ = create_target.update(cx, |this, cx| {
                                this.create_project_from_welcome(window, cx);
                            });
                        })
                        .child(
                            div()
                                .text_size(px(11.0))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(Colors::text_primary())
                                .child(i18n.tr("wizard.button.create")),
                        ),
                )
                .child(
                    div()
                        .id("welcome-new-continue")
                        .flex()
                        .items_center()
                        .h(px(30.0))
                        .px(px(10.0))
                        .rounded_md()
                        .border(px(1.0))
                        .border_color(Colors::border_default())
                        .bg(Colors::surface_card())
                        .cursor(gpui::CursorStyle::PointingHand)
                        .hover(|style| style.bg(Colors::surface_card_hover()))
                        .on_mouse_down(gpui::MouseButton::Left, move |_event, window, cx| {
                            let _ = continue_target.update(cx, |this, cx| {
                                this.selected = Some(WelcomeSelection::Continue);
                                cx.notify();
                            });
                            on_continue(WelcomeAction::OpenEmptyWorkspace, window, cx);
                        })
                        .child(
                            div()
                                .text_size(px(11.0))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(Colors::text_primary())
                                .child(i18n.tr("welcome.button.continue-without-project")),
                        ),
                ),
        )
        .into_any_element()
}

// ── Open Project tab (Part B) ─────────────────────────────────────────────────

fn open_project_pane(
    cx: &mut Context<WelcomeWindow>,
    recent: &[RecentProject],
    open_error: Option<SharedString>,
    callbacks: &WelcomeCallbacks,
    i18n: I18n,
) -> gpui::AnyElement {
    let browse_target = cx.entity().clone();

    let recent_list = if recent.is_empty() {
        div()
            .flex()
            .items_center()
            .justify_center()
            .min_h(px(80.0))
            .text_size(px(11.0))
            .text_color(Colors::text_muted())
            .child(i18n.tr("menu.file-open_recent-empty"))
            .into_any_element()
    } else {
        let mut list = div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .max_w(px(620.0))
            .w_full();
        for (index, item) in recent.iter().cloned().enumerate() {
            let target = cx.entity().clone();
            let on_action = callbacks.on_action.clone();
            list = list.child(recent_row(
                index,
                item,
                false,
                i18n,
                move |path, window, cx| {
                    welcome_debug!("recent project clicked (open tab) -> {}", path.display());
                    let _ = target.update(cx, |this, cx| {
                        this.active_nav = StartupNav::OpenProject;
                        cx.notify();
                    });
                    on_action(WelcomeAction::OpenRecent(path), window, cx);
                },
            ));
        }
        list.into_any_element()
    };

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .p(px(16.0))
        .gap(px(12.0))
        .bg(Colors::surface_panel())
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(3.0))
                .child(
                    div()
                        .text_size(px(13.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(Colors::text_primary())
                        .child(i18n.tr("welcome.open-project")),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(Colors::text_muted())
                        .child("Browse or pick a recent project."),
                ),
        )
        .child(
            div().flex().flex_row().items_center().gap(px(10.0)).child(
                div()
                    .id("welcome-open-browse")
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .h(px(30.0))
                    .px(px(12.0))
                    .rounded_md()
                    .border(px(1.0))
                    .border_color(Colors::accent_primary())
                    .bg(Colors::surface_card_selected())
                    .cursor(gpui::CursorStyle::PointingHand)
                    .hover(|style| style.bg(Colors::surface_card_hover()))
                    .on_mouse_down(gpui::MouseButton::Left, move |_event, window, cx| {
                        let _ = browse_target
                            .update(cx, |this, cx| this.browse_and_open_project(window, cx));
                    })
                    .child(
                        svg()
                            .path(assets::ICON_FOLDER_OPEN_PATH)
                            .w(px(13.0))
                            .h(px(13.0))
                            .text_color(Colors::text_primary()),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(Colors::text_primary())
                            .child(i18n.tr("wizard.button.browse")),
                    ),
            ),
        )
        .when_some(open_error, |el, msg| el.child(open_error_banner(msg)))
        .child(section_label(i18n.tr("welcome.nav.recent")))
        .child(
            div()
                .id("welcome-open-recent-scroll")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(recent_list),
        )
        .into_any_element()
}

fn open_error_banner(msg: SharedString) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .rounded_md()
        .border(px(1.0))
        .border_color(Colors::status_error())
        .bg(Colors::surface_card())
        .px(px(10.0))
        .py(px(8.0))
        .child(
            div()
                .text_size(px(10.5))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(Colors::status_error())
                .child(msg),
        )
}

fn form_label(label: impl Into<SharedString>) -> impl IntoElement {
    div()
        .text_size(px(10.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(Colors::text_muted())
        .child(label.into())
}

fn start_row(
    index: usize,
    row: StartRow,
    selected: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let accessible_label = format!(
        "{}. {}. Shortcut {}",
        row.title, row.description, row.shortcut
    );
    div()
        .id(("welcome-start-row", index))
        .role(Role::Button)
        .aria_label(accessible_label)
        .aria_selected(selected)
        .focusable()
        .tab_stop(true)
        .focus_visible(|style| style.border_color(Colors::border_focus()))
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(9.0))
        .min_h(px(50.0))
        .border(px(1.0))
        .border_color(if selected {
            Colors::border_normal()
        } else {
            Colors::border_subtle()
        })
        .rounded_sm()
        .bg(if selected {
            Colors::surface_selected()
        } else {
            Colors::surface_panel()
        })
        .px(px(9.0))
        .py(px(6.0))
        .cursor(gpui::CursorStyle::PointingHand)
        .hover(|style| style.bg(Colors::surface_card_hover()))
        .on_click(move |_event, window, cx| {
            on_click(window, cx);
        })
        .when(selected, |item| {
            item.child(
                div()
                    .absolute()
                    .left_0()
                    .top(px(5.0))
                    .bottom(px(5.0))
                    .w(px(2.0))
                    .bg(Colors::accent_primary()),
            )
        })
        .child(row_icon(row.icon))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .min_w_0()
                .flex_1()
                .child(
                    div()
                        .truncate()
                        .text_size(px(11.5))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(Colors::text_primary())
                        .child(row.title),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(px(10.0))
                        .text_color(Colors::text_muted())
                        .child(row.description),
                ),
        )
        .child(shortcut_badge(row.shortcut))
}

fn continue_row(
    selected: bool,
    i18n: I18n,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label = i18n.tr("welcome.button.continue-without-project");
    div()
        .id("welcome-continue-row")
        .role(Role::Button)
        .aria_label(label.clone())
        .aria_selected(selected)
        .focusable()
        .tab_stop(true)
        .focus_visible(|style| style.border_color(Colors::border_focus()))
        .relative()
        .flex()
        .items_center()
        .gap(px(9.0))
        .min_h(px(46.0))
        .rounded_sm()
        .border(px(1.0))
        .border_color(if selected {
            Colors::border_normal()
        } else {
            Colors::border_subtle()
        })
        .bg(if selected {
            Colors::surface_selected()
        } else {
            Colors::surface_panel()
        })
        .px(px(9.0))
        .cursor(gpui::CursorStyle::PointingHand)
        .hover(|style| style.bg(Colors::surface_card_hover()))
        .on_click(move |_event, window, cx| {
            on_click(window, cx);
        })
        .when(selected, |item| {
            item.child(
                div()
                    .absolute()
                    .left_0()
                    .top(px(5.0))
                    .bottom(px(5.0))
                    .w(px(2.0))
                    .bg(Colors::accent_primary()),
            )
        })
        .child(row_icon(assets::ICON_CORNER_DOWN_LEFT_PATH))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .min_w_0()
                .child(
                    div()
                        .text_size(px(11.5))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(Colors::text_primary())
                        .child(label),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(px(10.0))
                        .text_color(Colors::text_muted())
                        .child("Use source files until you save"),
                ),
        )
}

// ── Feed tab ────────────────────────────────────────────────────────────────

fn feed_pane(state: &FeedLoadState, posts: &[FeedPost]) -> gpui::AnyElement {
    let content = match state {
        FeedLoadState::Idle | FeedLoadState::Loading => feed_status_card(
            "Loading Futureboard Feed...",
            "Fetching the latest public posts from feed.futureboard.studio.",
        ),
        FeedLoadState::Failed(error) => feed_status_card("Feed unavailable", error.to_string()),
        FeedLoadState::Loaded if posts.is_empty() => {
            feed_status_card("No posts yet", "Published updates will appear here.")
        }
        FeedLoadState::Loaded => {
            let mut list = div()
                .flex()
                .flex_col()
                .gap(px(7.0))
                .max_w(px(640.0))
                .w_full();
            for post in posts.iter().take(8).cloned() {
                list = list.child(feed_post_row(post));
            }
            list.into_any_element()
        }
    };

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .p(px(16.0))
        .gap(px(12.0))
        .bg(Colors::surface_panel())
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(3.0))
                .child(
                    div()
                        .text_size(px(13.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(Colors::text_primary())
                        .child("Futureboard Feed"),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(Colors::text_muted())
                        .child("Latest public Studio updates."),
                ),
        )
        .child(
            div()
                .id("welcome-feed-scroll")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(content),
        )
        .into_any_element()
}

fn feed_status_card(title: impl Into<String>, detail: impl Into<String>) -> gpui::AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .max_w(px(640.0))
        .w_full()
        .rounded_md()
        .border(px(1.0))
        .border_color(Colors::border_subtle())
        .bg(Colors::surface_card())
        .px(px(12.0))
        .py(px(10.0))
        .child(
            div()
                .text_size(px(11.5))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(Colors::text_primary())
                .child(title.into()),
        )
        .child(
            div()
                .text_size(px(10.5))
                .text_color(Colors::text_muted())
                .child(detail.into()),
        )
        .into_any_element()
}

fn feed_post_row(post: FeedPost) -> impl IntoElement {
    let published_at = post.published_at.clone();
    let public_url = post
        .slug
        .map(|slug| format!("{FEED_PUBLIC_BASE_URL}/p/{slug}"));
    div()
        .flex()
        .flex_col()
        .gap(px(5.0))
        .min_h(px(70.0))
        .rounded_md()
        .border(px(1.0))
        .border_color(Colors::border_subtle())
        .bg(Colors::surface_card())
        .px(px(12.0))
        .py(px(10.0))
        .hover(|style| {
            style
                .bg(Colors::surface_card_hover())
                .border_color(Colors::border_default())
        })
        .when_some(public_url.clone(), |row, url| {
            row.cursor(gpui::CursorStyle::PointingHand).on_mouse_down(
                gpui::MouseButton::Left,
                move |_event, _window, cx| {
                    cx.stop_propagation();
                    cx.open_url(&url);
                },
            )
        })
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(10.0))
                .child(
                    div()
                        .truncate()
                        .min_w_0()
                        .flex_1()
                        .text_size(px(12.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(Colors::text_primary())
                        .child(post.title),
                )
                .when(!published_at.is_empty(), |row| {
                    row.child(
                        div()
                            .flex_none()
                            .rounded_sm()
                            .border(px(1.0))
                            .border_color(Colors::border_subtle())
                            .bg(Colors::surface_badge())
                            .px(px(6.0))
                            .py(px(2.0))
                            .text_size(px(9.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(Colors::text_muted())
                            .child(published_at),
                    )
                }),
        )
        .child(
            div()
                .text_size(px(10.5))
                .text_color(Colors::text_secondary())
                .child(post.excerpt),
        )
        .when_some(public_url, |row, url| {
            row.child(
                div()
                    .text_size(px(9.5))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(Colors::accent_primary())
                    .child(url),
            )
        })
}

// ── Audio Setup tab ───────────────────────────────────────────────────────────

fn audio_setup_pane(backend: SharedString, device_out: SharedString) -> gpui::AnyElement {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .p(px(16.0))
        .gap(px(12.0))
        .bg(Colors::surface_panel())
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(3.0))
                .child(
                    div()
                        .text_size(px(13.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(Colors::text_primary())
                        .child("Audio"),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(Colors::text_muted())
                        .child("Current output settings."),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(0.0))
                .max_w(px(560.0))
                .w_full()
                .rounded_md()
                .border(px(1.0))
                .border_color(Colors::border_subtle())
                .bg(Colors::surface_card())
                .px(px(12.0))
                .py(px(4.0))
                .child(info_row("Audio Backend", backend))
                .child(info_row("Output Device", device_out)),
        )
        .into_any_element()
}

fn info_row(label: &'static str, value: SharedString) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.0))
        .h(px(30.0))
        .child(
            div()
                .text_size(px(10.5))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(Colors::text_muted())
                .child(label),
        )
        .child(
            div()
                .truncate()
                .text_size(px(10.5))
                .text_color(Colors::text_primary())
                .child(value),
        )
}

// ── Right panel: recent + project location ─────────────────────────────────

fn right_panel(
    cx: &mut Context<WelcomeWindow>,
    recent: &[RecentProject],
    selected: &Option<WelcomeSelection>,
    callbacks: &WelcomeCallbacks,
    default_dir: PathBuf,
    configured: bool,
    i18n: I18n,
) -> impl IntoElement {
    let recent_content = if recent.is_empty() {
        div()
            .flex()
            .items_center()
            .justify_center()
            .min_h(px(120.0))
            .text_size(px(11.0))
            .text_color(Colors::text_muted())
            .child(i18n.tr("menu.file-open_recent-empty"))
            .into_any_element()
    } else {
        let mut list = div().flex().flex_col().gap(px(3.0));
        for (index, item) in recent.iter().cloned().enumerate() {
            let target = cx.entity().clone();
            let on_action = callbacks.on_action.clone();
            let is_selected = selected == &Some(WelcomeSelection::Recent(index));
            list = list.child(recent_row(
                index,
                item,
                is_selected,
                i18n,
                move |path, window, cx| {
                    welcome_debug!("recent project clicked -> {}", path.display());
                    let _ = target.update(cx, |this, cx| {
                        this.selected = Some(WelcomeSelection::Recent(index));
                        this.active_nav = StartupNav::RecentProjects;
                        cx.notify();
                    });
                    on_action(WelcomeAction::OpenRecent(path), window, cx);
                },
            ));
        }
        list.into_any_element()
    };

    div()
        .flex()
        .flex_col()
        .w(px(320.0))
        .flex_none()
        .min_h_0()
        .border_l(px(1.0))
        .border_color(Colors::border_subtle())
        .bg(Colors::surface_sidebar())
        .px(px(12.0))
        .py(px(10.0))
        .gap(px(8.0))
        .child(section_label(i18n.tr("welcome.nav.recent")))
        .child(
            div()
                .id("welcome-recent-scroll")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(recent_content),
        )
        .child(default_location_section(cx, default_dir, configured, i18n))
}

fn default_location_section(
    cx: &mut Context<WelcomeWindow>,
    default_dir: PathBuf,
    configured: bool,
    i18n: I18n,
) -> impl IntoElement {
    let exists = default_dir.exists();
    let path_label = default_dir.to_string_lossy().to_string();
    let target = cx.entity().clone();

    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .pt(px(10.0))
        .border_t(px(1.0))
        .border_color(Colors::border_subtle())
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(section_label(i18n.tr("wizard.field.location")))
                .child(
                    div()
                        .id("welcome-change-default-dir")
                        .flex()
                        .items_center()
                        .h(px(22.0))
                        .px(px(8.0))
                        .rounded_md()
                        .border(px(1.0))
                        .border_color(Colors::border_default())
                        .bg(Colors::surface_card())
                        .cursor(gpui::CursorStyle::PointingHand)
                        .hover(|style| {
                            style
                                .bg(Colors::surface_card_hover())
                                .border_color(Colors::accent_hover())
                        })
                        .on_mouse_down(gpui::MouseButton::Left, move |_event, _window, cx| {
                            let _ = target.update(cx, |this, cx| this.change_default_dir(cx));
                        })
                        .child(
                            div()
                                .text_size(px(10.0))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(Colors::text_primary())
                                .child(i18n.tr("wizard.button.browse")),
                        ),
                ),
        )
        .child(
            div()
                .rounded_md()
                .border(px(1.0))
                .border_color(Colors::border_subtle())
                .bg(Colors::surface_input())
                .px(px(10.0))
                .py(px(8.0))
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(Colors::text_secondary())
                        .child(path_label),
                ),
        )
        .when(!configured || !exists, |section| {
            section.child(
                div()
                    .text_size(px(9.5))
                    .text_color(Colors::text_muted())
                    .child("Folder will be created when needed."),
            )
        })
}

fn recent_row(
    index: usize,
    recent: RecentProject,
    selected: bool,
    i18n: I18n,
    on_click: impl Fn(PathBuf, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let path = recent.path.clone();
    let path_label = recent.path.to_string_lossy().to_string();
    let missing = recent.missing;
    let missing_label = i18n.tr("project.file.missing");
    let last_opened = format_last_opened(recent.last_opened_at);
    div()
        .id(("welcome-recent-row", index))
        .relative()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .min_h(px(48.0))
        .rounded_sm()
        .border(px(1.0))
        .border_color(if selected {
            Colors::border_normal()
        } else {
            Colors::border_subtle()
        })
        .bg(if selected {
            Colors::surface_selected()
        } else {
            Colors::surface_sidebar()
        })
        .opacity(if missing { 0.48 } else { 1.0 })
        .px(px(9.0))
        .py(px(6.0))
        .cursor(if missing {
            gpui::CursorStyle::Arrow
        } else {
            gpui::CursorStyle::PointingHand
        })
        .hover(|style| {
            if missing {
                style
            } else {
                style.bg(Colors::surface_card_hover())
            }
        })
        .when(!missing, |row| {
            row.on_mouse_down(gpui::MouseButton::Left, move |_event, window, cx| {
                on_click(path.clone(), window, cx);
            })
        })
        .when(selected, |item| {
            item.child(
                div()
                    .absolute()
                    .left_0()
                    .top(px(5.0))
                    .bottom(px(5.0))
                    .w(px(2.0))
                    .bg(Colors::accent_primary()),
            )
        })
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(8.0))
                .child(
                    div()
                        .truncate()
                        .min_w_0()
                        .flex_1()
                        .text_size(px(11.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(Colors::text_primary())
                        .child(recent.name),
                )
                .when(missing, |row| {
                    row.child(
                        div()
                            .flex_none()
                            .rounded_sm()
                            .bg(Colors::surface_badge())
                            .px(px(6.0))
                            .py(px(1.0))
                            .text_size(px(8.5))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(Colors::semantic_warning())
                            .child(missing_label.clone()),
                    )
                })
                .when(!missing && !last_opened.is_empty(), |row| {
                    row.child(
                        div()
                            .flex_none()
                            .text_size(px(9.0))
                            .text_color(Colors::text_muted())
                            .child(last_opened.clone()),
                    )
                }),
        )
        .child(
            div()
                .truncate()
                .text_size(px(9.5))
                .text_color(Colors::text_muted())
                .child(path_label),
        )
}

/// Render a coarse "time ago" label from a unix-seconds timestamp. Empty when
/// the timestamp is zero/unknown. Intentionally low-resolution — exact times
/// add no value on the start screen.
fn fetch_feed_posts() -> Result<Vec<FeedPost>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(FEED_FETCH_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("Failed to create feed client: {e}"))?;

    let response = client
        .get(FEED_API_URL)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .map_err(|e| format!("Could not reach the public feed API: {e}"))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!("Feed API returned HTTP {status}."));
    }

    let payload = response
        .json::<FeedResponse>()
        .map_err(|e| format!("Could not read feed payload: {e}"))?;

    Ok(payload
        .docs
        .into_iter()
        .map(feed_post_from_payload)
        .collect())
}

fn feed_post_from_payload(post: PayloadPost) -> FeedPost {
    let excerpt = post
        .meta
        .as_ref()
        .and_then(|meta| meta.description.as_deref())
        .filter(|description| !description.trim().is_empty())
        .map(trim_feed_excerpt)
        .or_else(|| post.content.as_ref().map(lexical_excerpt))
        .filter(|excerpt| !excerpt.trim().is_empty())
        .unwrap_or_else(|| "Read the latest Futureboard Studio update.".to_string());

    FeedPost {
        title: SharedString::from(post.title),
        excerpt: SharedString::from(excerpt),
        published_at: SharedString::from(format_feed_date(post.published_at.as_deref())),
        slug: post.slug.map(SharedString::from),
    }
}

fn format_feed_date(value: Option<&str>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    let date = value.split('T').next().unwrap_or(value);
    let mut parts = date.split('-');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(year), Some(month), Some(day)) => format!("{day}/{month}/{year}"),
        _ => String::new(),
    }
}

fn lexical_excerpt(content: &Value) -> String {
    let mut out = String::new();
    collect_lexical_text(content, &mut out);
    trim_feed_excerpt(&out)
}

fn collect_lexical_text(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            if let Some(text) = map.get("text").and_then(Value::as_str) {
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
                out.push_str(text);
            }
            if let Some(children) = map.get("children") {
                collect_lexical_text(children, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_lexical_text(item, out);
            }
        }
        _ => {}
    }
}

fn trim_feed_excerpt(input: &str) -> String {
    let normalized = input.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX_CHARS: usize = 170;
    if normalized.chars().count() <= MAX_CHARS {
        return normalized;
    }
    let mut trimmed: String = normalized.chars().take(MAX_CHARS).collect();
    trimmed.push('…');
    trimmed
}

fn format_last_opened(last_opened_at: u64) -> String {
    if last_opened_at == 0 {
        return String::new();
    }
    let now = crate::project::now_secs();
    if now <= last_opened_at {
        return "Just now".to_string();
    }
    let secs = now - last_opened_at;
    let mins = secs / 60;
    let hours = mins / 60;
    let days = hours / 24;
    if mins < 1 {
        "Just now".to_string()
    } else if hours < 1 {
        format!("{mins}m ago")
    } else if days < 1 {
        format!("{hours}h ago")
    } else if days < 30 {
        format!("{days}d ago")
    } else {
        String::new()
    }
}

fn section_label(label: impl Into<String>) -> impl IntoElement {
    div()
        .h(px(22.0))
        .flex()
        .items_center()
        .text_size(px(9.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(Colors::text_muted())
        .child(label.into().to_uppercase())
}

fn row_icon(path: &'static str) -> impl IntoElement {
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .w(px(28.0))
        .h(px(28.0))
        .rounded_sm()
        .border(px(1.0))
        .border_color(Colors::border_subtle())
        .bg(Colors::surface_badge())
        .child(
            svg()
                .path(path)
                .w(px(13.0))
                .h(px(13.0))
                .text_color(Colors::text_secondary()),
        )
}

fn shortcut_badge(shortcut: String) -> impl IntoElement {
    div()
        .flex_none()
        .px(px(4.0))
        .text_size(px(9.0))
        .text_color(Colors::text_faint())
        .child(shortcut)
}
