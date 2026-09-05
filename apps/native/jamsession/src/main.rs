//! Futureboard Jam — the Audio Jam client, on its own.
//!
//! Studio's Audio Jam is a window inside a DAW. Most of the time a jam does not
//! need one: a performer joining somebody else's room wants to hear the room and
//! send their instrument, and everything between those two things — a timeline,
//! a mixer, a plug-in host — is weight they are paying for and not using.
//!
//! So this binary is the same jam, with the DAW removed. The parts that decide
//! what a jam *is* are reused rather than reimplemented:
//!
//! ```txt
//! SphereUIComponents::jam       the controller, the session, publishing
//! SphereUIComponents::components  the shared controls, meters and chrome
//! SphereDirectAudioEngine       the device, the graph, the jam bus
//! ```
//!
//! What this crate adds is the shape those three take when there is no project
//! around them:
//!
//! * [`app`] — the window, laid out the way a jam client is used rather than
//!   the way a DAW panel is: your input at the top, the room in the middle,
//!   your output at the bottom.
//! * [`monitor`] — the room as a small mix. A remote performer is audible when
//!   some track's input is their stream, so listening to one is a track; the
//!   track list here is not a document, it is who this machine is listening to.
//! * [`settings`] — the four questions a jam depends on: which interface you
//!   speak through, which one you hear through, at what rate, with how much
//!   buffer. Separate, because they are a different job from playing.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod monitor;
mod settings;

use std::sync::Arc;

use gpui::Application;
use sphere_ui_components::embedded_assets::EmbeddedAssets;
use sphere_ui_components::theme;
use DirectAudio::EngineConfig;

use crate::monitor::JamMonitor;

fn main() {
    application().with_assets(EmbeddedAssets::new()).run(|cx| {
        sphere_ui_components::boot::log("Futureboard Jam boot start");
        let _ = theme::initialize_theme_system();
        let saved_theme = sphere_ui_components::settings::SettingsSchema::load_from_disk()
            .appearance
            .theme;
        let _ = theme::activate_theme_by_id(&saved_theme);
        sphere_ui_components::assets::register_fonts(cx);
        // The jam client needs an account before it can create or join
        // anything: the session asks for a bearer token on every call. Wiring
        // the same provider Studio uses means signing in here is signing in
        // there — one account, one session file, no second sign-in flow.
        sphere_ui_components::account::install_default_account_provider();

        // The room is unreachable without the engine: the jam bus lives in its
        // shared state, and every stream — in or out — is a ring the audio
        // callback reads or fills. A failure here is fatal in the honest sense
        // that there is nothing left for this application to do, so it is
        // reported and the process ends rather than opening a window that can
        // only ever say "no audio".
        let monitor = match JamMonitor::start(EngineConfig::default()) {
            Ok(monitor) => Arc::new(monitor),
            Err(error) => {
                eprintln!("[jam-app] the audio engine could not start: {error}");
                cx.quit();
                return;
            }
        };

        match monitor.engine() {
            Ok(engine) => {
                // Take the room. Studio routes each performer to a track
                // before hearing them, because a project decides what it
                // listens to. This client has no project and nothing to route
                // through: a listener who must ask for each performer
                // individually has joined a room that is silent for a reason
                // nothing on screen explains.
                if let Err(error) = sphere_ui_components::jam::install_with_ingress(
                    engine.jam_bus(),
                    Some(engine),
                    sphere_ui_components::jam::JamIngress::Everything,
                ) {
                    eprintln!("[jam-app] the jam controller could not be installed: {error}");
                    cx.quit();
                    return;
                }
            }
            Err(error) => {
                eprintln!("[jam-app] the audio engine is unreachable: {error}");
                cx.quit();
                return;
            }
        }

        // The main window ends the process. A settings window closing must not:
        // it is a second window over the same session, and quitting when it goes
        // away would end a jam somebody is still playing in.
        if let Err(error) = crate::app::open_jam_app_window(monitor, cx) {
            eprintln!("[jam-app] the window could not be opened: {error}");
            cx.quit();
        }
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
