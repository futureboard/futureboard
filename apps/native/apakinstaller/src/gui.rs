use std::{path::PathBuf, sync::Arc};

#[cfg(target_os = "windows")]
use crate::platform::show_startup_error;
use crate::platform::{ELEVATED_WARNING_GUI, is_process_elevated};
use apak::{InstallRoots, SignedInstallOptions, install_signed_package, read_signed_package_info};
use gpui::{
    App, AppContext, Bounds, Context, IntoElement, ParentElement, PathPromptOptions, Point, Render,
    Styled, Window, WindowBackgroundAppearance, WindowBounds, WindowKind, div, px, size,
};
use sphere_ui_components::components::title_bar::external_window_titlebar_compact;
use sphere_ui_components::components::{
    FbButtonKind, MessageBoxKind, MessageBoxOptions, ProgressBarValue, fb_button,
    open_message_box_window, progress_bar,
};
use sphere_ui_components::embedded_assets::EmbeddedAssets;
use sphere_ui_components::theme::{self, Colors};

const WINDOW_W: f32 = 560.0;
const WINDOW_H: f32 = 300.0;

pub fn run(initial_package: Option<PathBuf>) {
    application()
        .with_assets(EmbeddedAssets::new())
        .run(move |cx| setup(cx, initial_package));
}

fn setup(cx: &mut App, initial_package: Option<PathBuf>) {
    let _ = sphere_ui_components::theme::initialize_theme_system();
    sphere_ui_components::assets::register_fonts(cx);

    let _ = cx.open_window(apak_window_options(cx), move |_window, cx| {
        cx.new(|cx| ApakInstallerWindow::new(initial_package.clone(), cx))
    });

    if is_process_elevated() {
        let options = MessageBoxOptions::new(ELEVATED_WARNING_GUI)
            .title("APAK Installer")
            .kind(MessageBoxKind::Warning)
            .buttons(["Close", "Continue Anyway"])
            .default_id(1)
            .cancel_id(0);
        let _ = open_message_box_window(
            None,
            options,
            Arc::new(|result, _window, cx| {
                if result.response == 0 {
                    cx.quit();
                }
            }),
            cx,
        );
    }
}

fn apak_window_options(cx: &App) -> gpui::WindowOptions {
    let window_size = size(px(WINDOW_W), px(WINDOW_H));
    let origin = cx
        .primary_display()
        .map(|display| {
            let b = display.bounds();
            let ox = f32::from(b.origin.x);
            let oy = f32::from(b.origin.y);
            let dw = f32::from(b.size.width);
            let dh = f32::from(b.size.height);
            Point {
                x: px(ox + (dw - WINDOW_W).max(0.0) / 2.0),
                y: px(oy + (dh - WINDOW_H).max(0.0) / 2.0),
            }
        })
        .unwrap_or(Point {
            x: px(260.0),
            y: px(120.0),
        });

    let mut options =
        sphere_ui_components::platform_chrome::external_dialog_window_options_partial();
    options.window_bounds = Some(WindowBounds::Windowed(Bounds {
        origin,
        size: window_size,
    }));
    options.kind = WindowKind::Floating;
    options.is_resizable = false;
    options.is_minimizable = false;
    options.window_background = WindowBackgroundAppearance::Transparent;
    options
}

pub struct ApakInstallerWindow {
    package_path: Option<PathBuf>,
    summary: Option<apak::PackageSummary>,
    status: InstallStatus,
    detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallStatus {
    Idle,
    Ready,
    Installing,
    Complete,
    Error,
}

impl ApakInstallerWindow {
    fn new(initial_package: Option<PathBuf>, _cx: &mut Context<Self>) -> Self {
        let mut window = Self {
            package_path: None,
            summary: None,
            status: InstallStatus::Idle,
            detail: "Open a .apak package from Explorer, or run apakinstaller.exe <package.apak>."
                .to_string(),
        };
        if let Some(path) = initial_package {
            window.load_package(path);
        }
        window
    }

    fn load_package(&mut self, path: PathBuf) {
        let result = crate::bundled_verifying_key()
            .and_then(|verifying_key| read_signed_package_info(&path, &verifying_key));

        match result {
            Ok(summary) => {
                self.package_path = Some(path);
                self.summary = Some(summary);
                self.status = InstallStatus::Ready;
                self.detail = "Package signature verified. Ready to install.".to_string();
            }
            Err(error) => {
                self.package_path = Some(path);
                self.summary = None;
                self.status = InstallStatus::Error;
                self.detail = error.to_string();
            }
        }
    }

    fn choose_package(&mut self, cx: &mut Context<Self>) {
        self.status = InstallStatus::Idle;
        self.detail = "Choose a .apak package...".to_string();
        cx.notify();

        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });

        cx.spawn(async move |this, cx| match receiver.await {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.into_iter().next() {
                    let _ = this.update(cx, |this, cx| {
                        if path
                            .extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("apak"))
                        {
                            this.load_package(path);
                        } else {
                            this.package_path = Some(path);
                            this.summary = None;
                            this.status = InstallStatus::Error;
                            this.detail = "Selected file is not a .apak package.".to_string();
                        }
                        cx.notify();
                    });
                }
            }
            Ok(Ok(None)) => {
                let _ = this.update(cx, |this, cx| {
                    this.detail = "No package selected.".to_string();
                    cx.notify();
                });
            }
            Ok(Err(error)) => {
                let _ = this.update(cx, |this, cx| {
                    this.summary = None;
                    this.status = InstallStatus::Error;
                    this.detail = format!("Could not open file picker: {error}");
                    cx.notify();
                });
            }
            Err(_) => {
                let _ = this.update(cx, |this, cx| {
                    this.summary = None;
                    this.status = InstallStatus::Error;
                    this.detail =
                        "File picker was closed before a package was selected.".to_string();
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn install_selected(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.package_path.clone() else {
            return;
        };

        self.status = InstallStatus::Installing;
        self.detail = "Installing package...".to_string();
        cx.notify();

        let result = crate::bundled_verifying_key().and_then(|verifying_key| {
            InstallRoots::default_user().and_then(|roots| {
                install_signed_package(SignedInstallOptions {
                    package_path: path,
                    verifying_key,
                    roots,
                })
            })
        });

        match result {
            Ok(report) => {
                self.summary = Some(report.summary);
                self.status = InstallStatus::Complete;
                self.detail = format!("Installed {} files.", report.installed_files.len());
            }
            Err(error) => {
                self.status = InstallStatus::Error;
                self.detail = error.to_string();
            }
        }
        cx.notify();
    }

    fn status_label(&self) -> &'static str {
        match self.status {
            InstallStatus::Idle => "Idle",
            InstallStatus::Ready => "Ready",
            InstallStatus::Installing => "Installing",
            InstallStatus::Complete => "Complete",
            InstallStatus::Error => "Error",
        }
    }

    fn status_color(&self) -> gpui::Rgba {
        match self.status {
            InstallStatus::Complete => Colors::status_success(),
            InstallStatus::Error => Colors::status_error(),
            InstallStatus::Installing | InstallStatus::Ready => Colors::accent_primary(),
            InstallStatus::Idle => Colors::text_muted(),
        }
    }
}

impl Render for ApakInstallerWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let target = cx.entity();
        let can_install = matches!(self.status, InstallStatus::Ready | InstallStatus::Error)
            && self.summary.is_some();
        let progress = if self.status == InstallStatus::Installing {
            ProgressBarValue::Indeterminate
        } else if self.status == InstallStatus::Complete {
            ProgressBarValue::value(1.0)
        } else {
            ProgressBarValue::value(0.0)
        };

        div()
            .flex()
            .flex_col()
            .size_full()
            .font(theme::ui_font())
            .bg(Colors::surface_base())
            .overflow_hidden()
            .rounded_md()
            .border(px(1.0))
            .border_color(Colors::border_subtle())
            .shadow(vec![gpui::BoxShadow {
                color: Colors::surface_overlay().into(),
                offset: gpui::point(px(0.0), px(6.0)),
                blur_radius: px(20.0),
                spread_radius: px(0.0),
                inset: false,
            }])
            .child(external_window_titlebar_compact(
                "Apak Installer",
                "apak-installer-close",
                |window, cx| {
                    window.remove_window();
                    cx.quit();
                },
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .px(px(14.0))
                    .py(px(12.0))
                    .gap(px(10.0))
                    .child(package_panel(self))
                    .child(progress_bar(progress))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(10.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .min_w_0()
                                    .child(
                                        div()
                                            .w(px(8.0))
                                            .h(px(8.0))
                                            .rounded_full()
                                            .bg(self.status_color()),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.0))
                                            .text_color(Colors::text_muted())
                                            .child(self.status_label()),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(fb_button(
                                        "apak-choose",
                                        "Choose...",
                                        FbButtonKind::Default,
                                        self.status != InstallStatus::Installing,
                                        {
                                            let target = target.clone();
                                            move |_, _window, cx| {
                                                let _ = target.update(cx, |this, cx| {
                                                    this.choose_package(cx);
                                                });
                                            }
                                        },
                                    ))
                                    .child(fb_button(
                                        "apak-install",
                                        "Install",
                                        FbButtonKind::Primary,
                                        can_install,
                                        {
                                            let target = target;
                                            move |_, _window, cx| {
                                                let _ = target.update(cx, |this, cx| {
                                                    this.install_selected(cx);
                                                });
                                            }
                                        },
                                    )),
                            ),
                    ),
            )
    }
}

fn package_panel(state: &ApakInstallerWindow) -> impl IntoElement {
    let path_label = state
        .package_path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "No package selected".to_string());

    let mut panel = div()
        .flex()
        .flex_col()
        .gap(px(7.0))
        .min_h(px(150.0))
        .p(px(10.0))
        .rounded_md()
        .border(px(1.0))
        .border_color(Colors::border_subtle())
        .bg(Colors::surface_panel_alt())
        .child(
            div()
                .text_size(px(10.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(Colors::text_faint())
                .child("PACKAGE"),
        )
        .child(
            div()
                .text_size(px(12.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(Colors::text_primary())
                .truncate()
                .child(path_label),
        );

    if let Some(summary) = &state.summary {
        panel = panel
            .child(summary_row("Name", summary.name.clone()))
            .child(summary_row("Version", summary.version.clone()))
            .child(summary_row("Target", summary.target.to_string()))
            .child(summary_row("Publisher", summary.publisher.clone()));
    }

    panel.child(
        div()
            .mt_auto()
            .text_size(px(11.0))
            .line_height(px(15.0))
            .text_color(if state.status == InstallStatus::Error {
                Colors::status_error()
            } else {
                Colors::text_muted()
            })
            .child(state.detail.clone()),
    )
}

fn summary_row(label: &'static str, value: String) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(10.0))
        .min_w_0()
        .child(
            div()
                .w(px(70.0))
                .text_size(px(10.5))
                .text_color(Colors::text_faint())
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(11.0))
                .text_color(Colors::text_secondary())
                .child(value),
        )
}

fn application() -> gpui::Application {
    #[cfg(target_os = "windows")]
    let platform: std::rc::Rc<dyn gpui::Platform> = std::rc::Rc::new(
        gpui_windows::WindowsPlatform::new(false).unwrap_or_else(|error| {
            show_startup_error(
                "APAK Installer",
                &format!("Failed to initialize Windows platform: {error}"),
            );
            std::process::exit(1);
        }),
    );

    #[cfg(target_os = "macos")]
    let platform: std::rc::Rc<dyn gpui::Platform> =
        std::rc::Rc::new(gpui_macos::MacPlatform::new(false));

    #[cfg(target_os = "linux")]
    let platform: std::rc::Rc<dyn gpui::Platform> = gpui_linux::current_platform(false);

    gpui::Application::with_platform(platform)
}
