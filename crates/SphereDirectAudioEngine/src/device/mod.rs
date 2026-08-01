use crate::types::JsAudioDeviceInfo;
use cpal::traits::{DeviceTrait, HostTrait};

/// `FUTUREBOARD_AUDIO_DEVICE_DEBUG=1` traces enumerated devices + channel counts.
fn device_debug_enabled() -> bool {
    std::env::var_os("FUTUREBOARD_AUDIO_DEVICE_DEBUG").is_some()
}

fn log_devices(direction: &str, devices: &[JsAudioDeviceInfo]) {
    if !device_debug_enabled() {
        return;
    }
    for d in devices {
        eprintln!(
            "[audio-device] {direction} name={:?} channels={} default_sr={} default={} backend={}",
            d.name, d.channels, d.default_sample_rate, d.is_default, d.backend
        );
    }
}

/// Enumerate all available output devices on the default host.
/// Never panics — returns empty vec on any cpal error.
pub fn list_output_devices() -> Vec<JsAudioDeviceInfo> {
    list_output_devices_for_host(&cpal::default_host())
}

/// Enumerate output devices from a specific CPAL host (for example ASIO).
pub(crate) fn list_output_devices_for_host(host: &cpal::Host) -> Vec<JsAudioDeviceInfo> {
    let backend = host.id().name().to_string();
    let default_name = host.default_output_device().and_then(|d| d.name().ok());

    match host.output_devices() {
        Err(e) => {
            eprintln!("[SphereAudio] list_output_devices error: {e}");
            vec![]
        }
        Ok(devices) => {
            let list: Vec<JsAudioDeviceInfo> = devices
                .filter_map(|dev| {
                    let name = dev.name().ok()?;
                    let cfg = dev.default_output_config().ok().or_else(|| {
                        dev.supported_output_configs()
                            .ok()?
                            .next()
                            .map(|range| range.with_max_sample_rate())
                    })?;
                    Some(JsAudioDeviceInfo {
                        id: name.clone(),
                        name: name.clone(),
                        kind: "output".into(),
                        channels: cfg.channels() as u32,
                        default_sample_rate: cfg.sample_rate().0,
                        is_default: Some(&name) == default_name.as_ref(),
                        backend: backend.clone(),
                    })
                })
                .collect();
            log_devices("output", &list);
            list
        }
    }
}

/// Enumerate all available input devices on the default host.
pub fn list_input_devices() -> Vec<JsAudioDeviceInfo> {
    list_input_devices_for_host(&cpal::default_host())
}

/// Enumerate input devices from a specific CPAL host (for example ASIO).
pub(crate) fn list_input_devices_for_host(host: &cpal::Host) -> Vec<JsAudioDeviceInfo> {
    let backend = host.id().name().to_string();
    let default_name = host.default_input_device().and_then(|d| d.name().ok());

    match host.input_devices() {
        Err(e) => {
            eprintln!("[SphereAudio] list_input_devices error: {e}");
            vec![]
        }
        Ok(devices) => {
            let list: Vec<JsAudioDeviceInfo> = devices
                .filter_map(|dev| {
                    let name = dev.name().ok()?;
                    let cfg = dev.default_input_config().ok().or_else(|| {
                        dev.supported_input_configs()
                            .ok()?
                            .next()
                            .map(|range| range.with_max_sample_rate())
                    })?;
                    Some(JsAudioDeviceInfo {
                        id: name.clone(),
                        name: name.clone(),
                        kind: "input".into(),
                        channels: cfg.channels() as u32,
                        default_sample_rate: cfg.sample_rate().0,
                        is_default: Some(&name) == default_name.as_ref(),
                        backend: backend.clone(),
                    })
                })
                .collect();
            log_devices("input", &list);
            list
        }
    }
}

/// Resolve a named output device (or the system default if `id` is None).
/// Returns `(device, actual_name)` or an error string.
pub fn resolve_output_device(id: Option<&str>) -> Result<(cpal::Device, String), String> {
    resolve_output_device_for_host(&cpal::default_host(), id)
}

/// Resolve an output device against a specific CPAL host. ASIO never routes
/// through here — its duplex session resolves the driver itself.
pub(crate) fn resolve_output_device_for_host(
    host: &cpal::Host,
    id: Option<&str>,
) -> Result<(cpal::Device, String), String> {
    match id {
        None => {
            let dev = host
                .default_output_device()
                .ok_or_else(|| "No default output device found".to_string())?;
            let name = dev.name().unwrap_or_else(|_| "Unknown".into());
            Ok((dev, name))
        }
        Some(wanted) => {
            let mut devices = host.output_devices().map_err(|e| e.to_string())?;
            devices
                .find(|d| d.name().map(|n| n == wanted).unwrap_or(false))
                .map(|d| {
                    let n = d.name().unwrap_or_else(|_| wanted.into());
                    (d, n)
                })
                .ok_or_else(|| format!("Output device '{wanted}' not found"))
        }
    }
}
