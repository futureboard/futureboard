#![cfg_attr(not(target_os = "macos"), allow(dead_code, unused_imports))]

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::Duration;

    use dispatch2::{DispatchQueue, DispatchTime};
    use gpui::{Bounds, Context, Render, Window, WindowBounds as GpuiWindowBounds, WindowOptions};
    use gpui::{div, prelude::*, px, size};
    use sphere_webview::client::{BrowserLifecycle, plugin_browser_client_with_surface};
    use sphere_webview::osr::OsrSurface;
    use sphere_webview::runtime::{
        CefRuntime, CefRuntimeConfig, NativeParent, ProcessDispatch, WebView, WebViewConfig,
        WindowBounds, execute_subprocess, platform_browser_subprocess,
    };
    use sphere_webview::scheme::{
        MessagePumpSchedule, SchemeAsset, plugin_scheme_app, plugin_scheme_app_with_message_pump,
        register_plugin_scheme_factory,
    };

    const PROBE_DOCUMENT: SchemeAsset = SchemeAsset {
        bytes: b"<!doctype html><style>body{background:#111;color:#eee}</style><p>probe</p>",
        mime_type: "text/html; charset=utf-8",
    };

    thread_local! {
        static PROCESS_APP: RefCell<Option<sphere_webview::runtime::cef::App>> =
            const { RefCell::new(None) };
        // Browser/client must drop before Runtime, and Runtime before App.
        static RUNTIME: RefCell<Option<ProbeHost>> =
            const { RefCell::new(None) };
    }

    struct ProbeBrowser {
        view: WebView<'static>,
        _client: sphere_webview::runtime::cef::Client,
        lifecycle: BrowserLifecycle,
        surface: OsrSurface,
    }

    struct ProbeHost {
        browsers: Vec<ProbeBrowser>,
        runtime: CefRuntime,
        _app: sphere_webview::runtime::cef::App,
    }

    struct ProbeView;

    impl Render for ProbeView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full()
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum PumpMode {
        Integrated,
        IntegratedManualOnce,
        IntegratedManualBurst,
        ExternalUnscheduled,
        ExternalScheduled,
        ExternalSampleFallback,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum InitTiming {
        BeforeRun,
        DidFinishLaunching,
        FirstMainTurn,
    }

    struct ProbePump {
        generation: AtomicU64,
        running: AtomicBool,
        shutting_down: AtomicBool,
        sample_fallback: bool,
    }

    impl ProbePump {
        fn new(sample_fallback: bool) -> Arc<Self> {
            Arc::new(Self {
                generation: AtomicU64::new(0),
                running: AtomicBool::new(false),
                shutting_down: AtomicBool::new(false),
                sample_fallback,
            })
        }

        fn callback(self: &Arc<Self>) -> MessagePumpSchedule {
            let pump = self.clone();
            Arc::new(move |delay_ms| pump.schedule(delay_ms))
        }

        fn schedule(self: &Arc<Self>, delay_ms: i64) {
            let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
            eprintln!(
                "[cef-pump] event=schedule generation={generation} delay_ms={delay_ms} thread={:?}",
                std::thread::current().id()
            );
            let delay = Duration::from_millis(delay_ms.max(0) as u64);
            let deadline = DispatchTime::NOW.time(delay.as_nanos().min(i64::MAX as u128) as i64);
            let pump = self.clone();
            DispatchQueue::main()
                .after(deadline, move || pump.dispatch(generation))
                .expect("failed to schedule CEF probe pump work");
        }

        fn dispatch(self: Arc<Self>, generation: u64) {
            if self.shutting_down.load(Ordering::Acquire) {
                eprintln!("[cef-pump] event=skip generation={generation} reason=shutdown");
                return;
            }
            if generation != self.generation.load(Ordering::Acquire) {
                eprintln!("[cef-pump] event=skip generation={generation} reason=stale-generation");
                return;
            }
            if self.running.swap(true, Ordering::AcqRel) {
                eprintln!("[cef-pump] event=reentrant-call-rejected generation={generation}");
                return;
            }
            eprintln!(
                "[cef-pump] event=begin generation={generation} thread={:?}",
                std::thread::current().id()
            );
            RUNTIME.with(|slot| {
                if let Some(host) = slot.borrow().as_ref() {
                    if let Err(error) = host.runtime.do_message_loop_work() {
                        eprintln!("[cef-pump] event=error error={error}");
                    }
                } else {
                    eprintln!("[cef-pump] event=skip reason=runtime-not-ready");
                }
            });
            self.running.store(false, Ordering::Release);
            eprintln!("[cef-pump] event=end generation={generation}");
            if self.sample_fallback && !self.shutting_down.load(Ordering::Acquire) {
                // Diagnostic-only comparison with the pinned cef-rs external
                // pump sample, which schedules a maximum 33 ms follow-up after
                // every work turn. This is not a production scheduler.
                self.schedule(33);
            }
        }

        fn shutdown(&self) {
            self.shutting_down.store(true, Ordering::Release);
            self.generation.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn parse_modes() -> (PumpMode, InitTiming, u64, Option<bool>) {
        let mut pump = PumpMode::Integrated;
        let mut timing = InitTiming::DidFinishLaunching;
        let mut duration = 30;
        let mut browser_async = None;
        for argument in std::env::args().skip(1) {
            match argument.as_str() {
                "--pump=integrated" => pump = PumpMode::Integrated,
                "--pump=integrated-manual-once" => pump = PumpMode::IntegratedManualOnce,
                "--pump=integrated-manual-burst" => pump = PumpMode::IntegratedManualBurst,
                "--pump=external-unscheduled" => pump = PumpMode::ExternalUnscheduled,
                "--pump=external" => pump = PumpMode::ExternalScheduled,
                "--pump=external-sample-fallback" => pump = PumpMode::ExternalSampleFallback,
                "--timing=before-run" => timing = InitTiming::BeforeRun,
                "--timing=did-finish" => timing = InitTiming::DidFinishLaunching,
                "--timing=first-turn" => timing = InitTiming::FirstMainTurn,
                "--browser=none" => browser_async = None,
                "--browser=sync" => browser_async = Some(false),
                "--browser=async" => browser_async = Some(true),
                _ if argument.starts_with("--duration-seconds=") => {
                    duration = argument["--duration-seconds=".len()..]
                        .parse()
                        .expect("invalid duration");
                }
                _ => panic!("unknown probe argument: {argument}"),
            }
        }
        (pump, timing, duration, browser_async)
    }

    fn initialize(external_message_pump: bool) {
        eprintln!(
            "[cef-probe] event=initialize-begin thread={:?} external_message_pump={external_message_pump}",
            std::thread::current().id()
        );
        let mut app = PROCESS_APP
            .with(|slot| slot.borrow_mut().take())
            .expect("probe CefApp was not installed");
        let runtime = CefRuntime::initialize(
            CefRuntimeConfig {
                browser_subprocess: platform_browser_subprocess()
                    .expect("probe must run from a packaged .app"),
                windowless_rendering: true,
                external_message_pump,
                ..Default::default()
            },
            Some(&mut app),
        )
        .expect("CEF probe initialization failed");
        RUNTIME.with(|slot| {
            assert!(
                slot.borrow_mut()
                    .replace(ProbeHost {
                        browsers: Vec::new(),
                        runtime,
                        _app: app,
                    })
                    .is_none()
            );
        });
        register_plugin_scheme_factory(
            Arc::new(|origin, path| {
                (matches!(origin, "probe-a" | "probe-b") && path == "/index.html")
                    .then_some(PROBE_DOCUMENT)
            }),
            None,
        )
        .expect("register probe mikoplugin scheme");
        eprintln!(
            "[cef-probe] event=initialize-end thread={:?}",
            std::thread::current().id()
        );
    }

    fn create_browsers() {
        RUNTIME.with(|slot| {
            let mut slot = slot.borrow_mut();
            let host = slot.as_mut().expect("runtime not initialized");
            assert!(host.browsers.is_empty(), "probe browsers already exist");
            for url in [
                "mikoplugin://probe-a/index.html",
                "mikoplugin://probe-a/index.html",
                "mikoplugin://probe-b/index.html",
            ] {
                let surface = OsrSurface::new(2, 2, 1.0);
                let (mut client, lifecycle) =
                    plugin_browser_client_with_surface(url, Some(surface.clone()));
                let config = WebViewConfig::new(
                    url,
                    WindowBounds::new(0, 0, 2, 2).expect("valid probe bounds"),
                )
                .windowless(surface.clone());
                let view = unsafe {
                    host.runtime
                        .create_webview_detached(
                            NativeParent::from_raw(std::ptr::null_mut()),
                            config,
                            Some(&mut client),
                        )
                        .expect("create probe OSR browser")
                };
                eprintln!(
                    "[cef-probe] event=browser-created browser_id={} thread={:?}",
                    view.browser_identifier(),
                    std::thread::current().id()
                );
                host.browsers.push(ProbeBrowser {
                    view,
                    _client: client,
                    lifecycle,
                    surface,
                });
            }
        });
    }

    pub fn run() {
        let (pump_mode, timing, duration_seconds, browser_async) = parse_modes();
        eprintln!(
            "[cef-probe] event=entry pid={} pump={pump_mode:?} timing={timing:?} duration_seconds={duration_seconds} browser={browser_async:?}",
            std::process::id(),
        );

        sphere_webview::runtime::log_process_entry();
        let pump = ProbePump::new(pump_mode == PumpMode::ExternalSampleFallback);
        let mut app = match pump_mode {
            PumpMode::ExternalScheduled | PumpMode::ExternalSampleFallback => {
                plugin_scheme_app_with_message_pump(pump.callback()).expect("create probe CefApp")
            }
            PumpMode::Integrated
            | PumpMode::IntegratedManualOnce
            | PumpMode::IntegratedManualBurst
            | PumpMode::ExternalUnscheduled => plugin_scheme_app().expect("create probe CefApp"),
        };
        match execute_subprocess(Some(&mut app)).expect("CEF process dispatch") {
            ProcessDispatch::SubprocessExit(code) => std::process::exit(code),
            ProcessDispatch::BrowserProcess => {}
        }
        gpui_macos::configure_cef_application().expect("configure GPUIApplication for CEF");
        PROCESS_APP.with(|slot| {
            assert!(slot.borrow_mut().replace(app).is_none());
        });

        let external = matches!(
            pump_mode,
            PumpMode::ExternalUnscheduled
                | PumpMode::ExternalScheduled
                | PumpMode::ExternalSampleFallback
        );
        if timing == InitTiming::BeforeRun {
            initialize(external);
        }

        let platform: Rc<dyn gpui::Platform> = Rc::new(gpui_macos::MacPlatform::new(false));
        gpui::Application::with_platform(platform).run(move |cx| {
            eprintln!(
                "[cef-probe] event=did-finish-launching thread={:?}",
                std::thread::current().id()
            );
            match timing {
                InitTiming::BeforeRun => {}
                InitTiming::DidFinishLaunching => initialize(external),
                InitTiming::FirstMainTurn => {
                    DispatchQueue::main().exec_async(move || initialize(external))
                }
            }
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(GpuiWindowBounds::Windowed(Bounds::centered(
                        None,
                        size(px(160.0), px(100.0)),
                        cx,
                    ))),
                    ..Default::default()
                },
                |_, cx| cx.new(|_| ProbeView),
            )
            .expect("open minimal GPUI probe window");
            if let Some(async_creation) = browser_async {
                if async_creation {
                    DispatchQueue::main().exec_async(create_browsers);
                } else {
                    create_browsers();
                }
            }
            if pump_mode == PumpMode::IntegratedManualOnce {
                DispatchQueue::main().exec_async(|| {
                    eprintln!(
                        "[cef-probe] event=manual-pump-once thread={:?}",
                        std::thread::current().id()
                    );
                    RUNTIME.with(|slot| {
                        slot.borrow()
                            .as_ref()
                            .expect("runtime initialized")
                            .runtime
                            .do_message_loop_work()
                            .expect("manual probe pump");
                    });
                });
            }
            if pump_mode == PumpMode::IntegratedManualBurst {
                cx.spawn(async move |cx| {
                    for iteration in 0..150 {
                        cx.background_executor()
                            .timer(Duration::from_millis(16))
                            .await;
                        eprintln!("[cef-probe] event=manual-pump iteration={iteration}");
                        RUNTIME.with(|slot| {
                            slot.borrow()
                                .as_ref()
                                .expect("runtime initialized")
                                .runtime
                                .do_message_loop_work()
                                .expect("manual probe pump");
                        });
                    }
                })
                .detach();
            }

            let pump = pump.clone();
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_secs(duration_seconds))
                    .await;
                eprintln!("[cef-probe] event=duration-complete");
                RUNTIME.with(|slot| {
                    if let Some(host) = slot.borrow().as_ref() {
                        let ready = host
                            .browsers
                            .iter()
                            .filter(|browser| {
                                browser.lifecycle.after_created()
                                    && browser.lifecycle.javascript_executed()
                                    && browser.surface.generation() > 0
                            })
                            .count();
                        eprintln!(
                            "[cef-probe] event=browser-status total={} ready={ready}",
                            host.browsers.len()
                        );
                        assert_eq!(ready, 3, "all probe plugin instances must load and paint");
                        for browser in &host.browsers {
                            let _ = browser.view.close(false);
                        }
                    }
                });
                cx.background_executor().timer(Duration::from_secs(1)).await;
                RUNTIME.with(|slot| {
                    if let Some(host) = slot.borrow().as_ref() {
                        let closed = host
                            .browsers
                            .iter()
                            .filter(|browser| browser.lifecycle.before_close())
                            .count();
                        eprintln!(
                            "[cef-probe] event=browser-close-status total={} closed={closed}",
                            host.browsers.len()
                        );
                        assert_eq!(closed, 3, "all probe browsers must close cleanly");
                    }
                });
                pump.shutdown();
                RUNTIME.with(|slot| drop(slot.borrow_mut().take()));
                eprintln!("[cef-probe] event=shutdown-complete");
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
        eprintln!("[cef-probe] event=application-exit");
    }
}

#[cfg(target_os = "macos")]
fn main() {
    macos::run();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("cef_gpui_macos_probe is only supported on macOS");
}
