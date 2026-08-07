//! Offscreen WGPU arrangement renderer (scaffold).
//!
//! Renders into a private `wgpu::Texture` — **not** a competing window surface.
//! Compositing into GPUI still requires Blade/GPUI texture interop.
//!
//! GPU preference is configurable so AMD iGPU / Intel iGPU / older laptop
//! GPUs aren't forced into the HighPerformance adapter slot (which on
//! hybrid systems can fail device creation outright). On adapter or device
//! failure we never panic — `render_arrangement` falls back to the GPUI
//! paint renderer.

use super::renderer::{TimelineRenderOutput, TimelineRenderer};
use super::snapshot::TimelineRenderSnapshot;

/// User-selectable GPU preference for the offscreen timeline renderer.
/// Drives both `PowerPreference` and the fallback-adapter retry path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineGpuPreference {
    /// No strong preference — let the driver pick. Best for iGPU/AMD/Intel.
    Auto,
    /// Prefer the low-power adapter. Hint to use the integrated GPU.
    LowPower,
    /// Prefer the discrete/high-performance adapter (legacy default).
    HighPerformance,
}

impl Default for TimelineGpuPreference {
    fn default() -> Self {
        TimelineGpuPreference::Auto
    }
}

impl TimelineGpuPreference {
    fn from_env() -> Self {
        match std::env::var("FUTUREBOARD_GPU_PREFERENCE")
            .map(|v| v.to_ascii_lowercase())
            .ok()
            .as_deref()
        {
            Some("lowpower") | Some("low-power") | Some("low") | Some("integrated") => {
                Self::LowPower
            }
            Some("highperformance")
            | Some("high-performance")
            | Some("high")
            | Some("discrete") => Self::HighPerformance,
            _ => Self::Auto,
        }
    }

    fn to_power(self) -> wgpu::PowerPreference {
        match self {
            // No `None` variant on `PowerPreference`; `LowPower` is the
            // conservative default that still lets the OS pick the iGPU
            // when present. Avoids the HighPerformance trap on hybrid
            // laptops where the discrete GPU isn't ready/available.
            TimelineGpuPreference::Auto => wgpu::PowerPreference::LowPower,
            TimelineGpuPreference::LowPower => wgpu::PowerPreference::LowPower,
            TimelineGpuPreference::HighPerformance => wgpu::PowerPreference::HighPerformance,
        }
    }
}

fn gpu_debug_enabled() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("FUTUREBOARD_GPU_RENDERER_DEBUG").is_some())
}

/// Saved GPU device id from Settings. Set once at app startup; consumed
/// by `WgpuTimelineRenderer::new` when the renderer is first constructed.
/// Empty string sentinel == "Auto" (no preference).
static PREFERRED_DEVICE_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Called at app startup with the saved GPU Device preference. `id == ""`
/// means Auto (default adapter selection). Subsequent calls are no-ops.
pub fn set_preferred_gpu_device_id(id: &str) {
    let _ = PREFERRED_DEVICE_ID.set(id.to_string());
}

/// Public summary of one detected GPU adapter. Used by the Settings UI
/// to populate the GPU Device combo. Stable `id` derived from
/// vendor/device/name so the saved preference is portable across
/// process restarts even if backend ordering changes.
#[derive(Debug, Clone)]
pub struct GpuDeviceInfo {
    pub id: String,
    pub name: String,
    pub backend: Option<String>,
    pub device_type: Option<String>,
    pub vendor_id: Option<u32>,
    pub device_id: Option<u32>,
}

/// Enumerate all GPU adapters visible to wgpu on the current machine.
/// Never panics — adapter enumeration is wrapped in `catch_unwind` so a
/// broken driver on one backend can't take down the settings dialog.
/// Returns an empty Vec when no GPU is detected; the Settings UI shows
/// "Auto" + "Unavailable" in that case.
pub fn list_available_gpu_devices() -> Vec<GpuDeviceInfo> {
    let result = std::panic::catch_unwind(|| {
        let instance = wgpu::Instance::default();
        // wgpu 29: enumerate_adapters is async (returns Future<Output = Vec<_>>).
        let adapters: Vec<wgpu::Adapter> =
            pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
        adapters
            .into_iter()
            .map(|adapter| {
                let info = adapter.get_info();
                let id = format!(
                    "{:?}:{:x}:{:x}:{}",
                    info.backend, info.vendor, info.device, info.name
                );
                GpuDeviceInfo {
                    id,
                    name: info.name.clone(),
                    backend: Some(format!("{:?}", info.backend)),
                    device_type: Some(format!("{:?}", info.device_type)),
                    vendor_id: Some(info.vendor),
                    device_id: Some(info.device),
                }
            })
            .collect::<Vec<_>>()
    });
    match result {
        Ok(devices) => {
            if gpu_debug_enabled() {
                eprintln!("[gpu-renderer] enumerated {} adapter(s)", devices.len());
                for d in &devices {
                    eprintln!(
                        "[gpu-renderer]   id={} name={} backend={:?} type={:?}",
                        d.id, d.name, d.backend, d.device_type
                    );
                }
            }
            devices
        }
        Err(_) => {
            if gpu_debug_enabled() {
                eprintln!("[gpu-renderer] adapter enumeration panicked; returning empty list");
            }
            Vec::new()
        }
    }
}

/// Classify this machine from the adapters wgpu can see, for the UI's
/// render-cost profile (see [`crate::perf::GpuClass`]).
///
/// Only the presence of a discrete adapter matters. Enumeration is the same
/// call the Settings GPU list uses — a few milliseconds, once, at startup — and
/// a driver that cannot enumerate yields `Unknown`, which never slows the UI
/// down on a guess.
pub fn detect_gpu_class() -> crate::perf::GpuClass {
    use crate::perf::GpuClass;
    let result = std::panic::catch_unwind(|| {
        let instance = wgpu::Instance::default();
        let adapters: Vec<wgpu::Adapter> =
            pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
        adapters
            .into_iter()
            .map(|adapter| adapter.get_info().device_type)
            .collect::<Vec<_>>()
    });
    let Ok(types) = result else {
        return GpuClass::Unknown;
    };
    if types.is_empty() {
        return GpuClass::Unknown;
    }
    if types
        .iter()
        .any(|kind| matches!(kind, wgpu::DeviceType::DiscreteGpu))
    {
        GpuClass::Discrete
    } else {
        GpuClass::IntegratedOnly
    }
}

/// GPU texture produced by an offscreen arrangement pass.
pub struct WgpuOffscreenFrame {
    pub width: u32,
    pub height: u32,
    pub format: wgpu::TextureFormat,
    /// Offscreen color target — keep alive until composited or dropped.
    pub texture: wgpu::Texture,
}

pub struct WgpuTimelineRenderer {
    instance: wgpu::Instance,
    preference: TimelineGpuPreference,
    /// User-selected GPU device id (matches `GpuDeviceInfo::id`). `None`
    /// means Auto — let `request_adapter` pick.
    selected_device_id: Option<String>,
    device: Option<wgpu::Device>,
    queue: Option<wgpu::Queue>,
    max_texture_dimension_2d: u32,
    init_error: Option<String>,
}

impl WgpuTimelineRenderer {
    pub fn new() -> Self {
        let preference = TimelineGpuPreference::from_env();
        let selected_device_id = PREFERRED_DEVICE_ID.get().cloned().filter(|s| !s.is_empty());
        Self::with_preference_and_device(preference, selected_device_id)
    }

    pub fn with_preference(preference: TimelineGpuPreference) -> Self {
        Self::with_preference_and_device(preference, None)
    }

    pub fn with_preference_and_device(
        preference: TimelineGpuPreference,
        selected_device_id: Option<String>,
    ) -> Self {
        Self {
            instance: wgpu::Instance::default(),
            preference,
            selected_device_id,
            device: None,
            queue: None,
            max_texture_dimension_2d: wgpu::Limits::downlevel_defaults().max_texture_dimension_2d,
            init_error: None,
        }
    }

    pub fn is_available(&mut self) -> bool {
        self.init_error.is_none() && self.ensure_device().is_ok()
    }

    fn request_adapter(&self, fallback: bool) -> Result<wgpu::Adapter, String> {
        pollster::block_on(self.instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: self.preference.to_power(),
            compatible_surface: None,
            force_fallback_adapter: fallback,
        }))
        .map_err(|_| {
            if fallback {
                "no WGPU adapter (including fallback)".to_string()
            } else {
                "no WGPU adapter".to_string()
            }
        })
    }

    fn ensure_device(&mut self) -> Result<(), String> {
        if self.device.is_some() {
            return Ok(());
        }
        if let Some(err) = &self.init_error {
            return Err(err.clone());
        }
        // 1. If the user picked a specific GPU Device, scan enumerated
        //    adapters and use the matching one. Falls through to Auto if
        //    that adapter is no longer present (e.g. eGPU unplugged).
        // 2. Otherwise — or on miss — try the preferred adapter via
        //    `request_adapter`.
        // 3. Final retry uses `force_fallback_adapter = true` so the
        //    software (CPU) adapter is taken before we declare defeat.
        let adapter = if let Some(saved_id) = self.selected_device_id.as_deref() {
            let adapters: Vec<wgpu::Adapter> =
                pollster::block_on(self.instance.enumerate_adapters(wgpu::Backends::all()));
            let mut matched: Option<wgpu::Adapter> = None;
            for adapter in adapters {
                let info = adapter.get_info();
                let id = format!(
                    "{:?}:{:x}:{:x}:{}",
                    info.backend, info.vendor, info.device, info.name
                );
                if id == saved_id {
                    if gpu_debug_enabled() {
                        eprintln!(
                            "[gpu-renderer] using saved adapter: id={id} name={:?}",
                            info.name
                        );
                    }
                    matched = Some(adapter);
                    break;
                }
            }
            match matched {
                Some(a) => a,
                None => {
                    if gpu_debug_enabled() {
                        eprintln!(
                            "[gpu-renderer] saved GPU device id {saved_id:?} not found among enumerated adapters; falling back to Auto"
                        );
                    }
                    self.request_adapter(false).or_else(|primary| {
                        if gpu_debug_enabled() {
                            eprintln!(
                                "[gpu-renderer] auto adapter failed ({primary}); retrying with fallback"
                            );
                        }
                        self.request_adapter(true)
                    })?
                }
            }
        } else {
            match self.request_adapter(false) {
                Ok(a) => a,
                Err(primary) => {
                    if gpu_debug_enabled() {
                        eprintln!(
                            "[gpu-renderer] primary adapter request failed ({primary}); retrying with fallback"
                        );
                    }
                    self.request_adapter(true).map_err(|e| {
                        let msg = format!("{primary}; fallback also failed: {e}");
                        self.init_error = Some(msg.clone());
                        msg
                    })?
                }
            }
        };

        if gpu_debug_enabled() {
            let info = adapter.get_info();
            eprintln!(
                "[gpu-renderer] adapter selected: name={:?} backend={:?} device_type={:?} vendor=0x{:x} device=0x{:x} preference={:?}",
                info.name, info.backend, info.device_type, info.vendor, info.device, self.preference
            );
        }

        // Start with downlevel defaults for broad compatibility, but request
        // the adapter's native 2D texture size. A maximized 4K timeline can be
        // wider than the downlevel 2048px cap even at 100% scale.
        let adapter_limits = adapter.limits();
        let mut limits = wgpu::Limits::downlevel_defaults();
        limits.max_texture_dimension_2d = adapter_limits.max_texture_dimension_2d;
        let max_texture_dimension_2d = limits.max_texture_dimension_2d;
        let device_result = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("futureboard-timeline"),
            required_features: wgpu::Features::empty(),
            required_limits: limits,
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        }));
        let (device, queue) = match device_result {
            Ok(pair) => pair,
            Err(e) => {
                let msg = format!("device creation failed: {e}");
                if gpu_debug_enabled() {
                    eprintln!("[gpu-renderer] {msg}; falling back to GPUI paint");
                }
                self.init_error = Some(msg.clone());
                return Err(msg);
            }
        };

        self.max_texture_dimension_2d = max_texture_dimension_2d;
        self.device = Some(device);
        self.queue = Some(queue);
        Ok(())
    }

    fn render_offscreen(
        &mut self,
        snapshot: &TimelineRenderSnapshot,
    ) -> Result<WgpuOffscreenFrame, String> {
        self.ensure_device()?;
        let device = self.device.as_ref().expect("device");
        let queue = self.queue.as_ref().expect("queue");

        let width = snapshot.viewport.width.max(1.0) as u32;
        let height = snapshot.viewport.height.max(1.0) as u32;
        let max_texture_dimension_2d = self.max_texture_dimension_2d;
        if width > max_texture_dimension_2d || height > max_texture_dimension_2d {
            return Err(format!(
                "viewport {}x{} exceeds WGPU texture limit {}; falling back to GPUI paint",
                width, height, max_texture_dimension_2d
            ));
        }
        let format = wgpu::TextureFormat::Rgba8Unorm;

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("timeline-offscreen"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        // Timeline arrangement background — matches `Colors::surface_base()` feel.
        let bg = wgpu::Color {
            r: 0.043,
            g: 0.059,
            b: 0.078,
            a: 1.0,
        };

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("timeline-arrangement"),
        });

        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("timeline-clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(bg),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // Scaffold: grid lines, lane fills, clip rects, and waveform chunks will be
            // drawn here via instanced pipelines reading `TimelineRenderSnapshot` only.
        }

        queue.submit(Some(encoder.finish()));

        if gpu_debug_enabled() {
            eprintln!(
                "[gpu-renderer] WgpuTimelineRenderer offscreen {}x{} grid={} clips={} waveform_handles={}",
                width,
                height,
                snapshot.grid_lines.len(),
                snapshot.clips.len(),
                snapshot
                    .clips
                    .iter()
                    .filter(|c| c.waveform.is_some())
                    .count(),
            );
        }

        Ok(WgpuOffscreenFrame {
            width,
            height,
            format,
            texture,
        })
    }
}

impl TimelineRenderer for WgpuTimelineRenderer {
    fn backend_name(&self) -> &'static str {
        "wgpu-offscreen"
    }

    fn render_arrangement(&mut self, snapshot: &TimelineRenderSnapshot) -> TimelineRenderOutput {
        let _s = crate::perf::PerfScope::enter("WgpuTimelineRenderer");
        match self.render_offscreen(snapshot) {
            Ok(frame) => TimelineRenderOutput::WgpuOffscreen(frame),
            Err(error) => {
                eprintln!("[gpu-renderer] offscreen render failed: {error}");
                super::gpui_paint::GpuiPaintTimelineRenderer::new().render_arrangement(snapshot)
            }
        }
    }
}
