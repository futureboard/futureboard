//! Load readout in the transport bar — audio CPU, process memory, voice count.
//!
//! These are the three numbers a player checks when a take starts crackling,
//! and the only place they existed before was a status-bar pill you had to
//! enable in Preferences and a debug overlay. They belong next to the transport
//! for the same reason the position readout does: they are read *during* a
//! take, not after one.
//!
//! Every value here comes from the running engine or the OS. Nothing is
//! smoothed into looking healthier than it is, and a value the platform cannot
//! supply is hidden rather than shown as zero.
//!
//! A GPUI entity, not a function in `app_chrome`: it updates at the audio meter
//! poll rate, and rendering it inline would repaint the whole shell on every
//! tick.

use gpui::{div, px, Context, IntoElement, ParentElement, Render, Styled, Window};

use crate::theme::{radius, space, typography, Colors};

/// Width of the readout. Sized for the widest each field gets — "100%",
/// "9999 MB", "256" — so the row never reflows while the numbers move.
pub const TRANSPORT_PERF_METER_WIDTH: f32 = 186.0;

/// Audio load above this reads as "this take is at risk", and the value takes
/// the warning hue. Chosen at the point a callback is using most of its
/// deadline and a single late block would drop out.
const CPU_WARN: f32 = 0.75;
/// Above this the engine is effectively at its budget.
const CPU_DANGER: f32 = 0.92;

/// One frame of engine load, pushed in by `StudioLayout`'s meter poll.
///
/// A snapshot rather than a live read of the engine: the poll already holds the
/// stats, and handing this entity the whole `EngineStats` would make its
/// repaint depend on fields it does not draw.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TransportPerfSnapshot {
    /// Audio callback time as a fraction of its deadline. `None` when no stream
    /// is open, or when the backend has not reported a deadline yet — the
    /// readout says so instead of printing 0%.
    pub cpu_load: Option<f32>,
    /// SoundFont voices sounding in the last block. See
    /// `EngineInner::active_voice_count` for what this does and does not count.
    pub voices: u32,
    /// Process working set, or `None` on a platform that cannot supply it.
    pub memory_bytes: Option<u64>,
}

impl TransportPerfSnapshot {
    /// Quantised identity, so a meter that has not visibly moved does not
    /// repaint. CPU to the percent, memory to the megabyte — finer than either
    /// is below what the readout can show.
    fn signature(&self) -> u64 {
        let cpu = self
            .cpu_load
            .map(|load| (load.clamp(0.0, 4.0) * 100.0).round() as u64 + 1)
            .unwrap_or(0);
        let mem = self
            .memory_bytes
            .map(|bytes| bytes / (1024 * 1024) + 1)
            .unwrap_or(0);
        cpu | ((self.voices as u64) << 12) | (mem << 32)
    }
}

pub struct TransportPerfMeter {
    snapshot: TransportPerfSnapshot,
    last_sig: u64,
}

impl Default for TransportPerfMeter {
    fn default() -> Self {
        Self::new()
    }
}

impl TransportPerfMeter {
    pub fn new() -> Self {
        Self {
            snapshot: TransportPerfSnapshot::default(),
            last_sig: u64::MAX,
        }
    }

    /// Push a poll tick. Repaints only when a drawn digit would change.
    pub fn apply(&mut self, snapshot: TransportPerfSnapshot, cx: &mut Context<Self>) -> bool {
        let sig = snapshot.signature();
        self.snapshot = snapshot;
        if sig == self.last_sig {
            return false;
        }
        self.last_sig = sig;
        cx.notify();
        true
    }
}

/// Colour for an audio load. Two channels with the value itself: the number
/// says how much, the hue says whether it matters.
fn cpu_color(load: f32) -> gpui::Rgba {
    if load >= CPU_DANGER {
        Colors::status_error()
    } else if load >= CPU_WARN {
        Colors::status_warning()
    } else {
        Colors::text_secondary()
    }
}

/// `1536` MB, `1.9` GB — one significant place past a gigabyte, because at that
/// size the megabyte digits are noise and the column would grow to fit them.
fn format_memory(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let mb = bytes as f64 / MB;
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else {
        format!("{mb:.0} MB")
    }
}

/// One `LABEL  value` pair. The label is quieter than the value on purpose —
/// the number is what is read at a glance, the label only says which number.
fn field(label: &'static str, value: String, value_color: gpui::Rgba) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_baseline()
        .gap(px(space::TIGHT))
        .flex_1()
        .min_w(px(0.0))
        .child(
            div()
                .flex_none()
                .text_size(px(typography::DENSE_CAPTION))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(Colors::text_faint())
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .truncate()
                .text_size(px(typography::UI_XS))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(value_color)
                .child(value),
        )
}

/// Hairline between two readouts. Inset top and bottom so it separates the
/// fields without drawing a full-height rule through the plate — the same
/// treatment the transport's own LCD divider uses.
fn divider() -> impl IntoElement {
    div()
        .flex_none()
        .w(px(1.0))
        .h(px(14.0))
        .bg(Colors::border_normal())
}

impl Render for TransportPerfMeter {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let _scope = crate::perf::PerfScope::enter("TransportPerfMeter");
        crate::perf::count("transport_perf_meter_paint_count", 1);

        let snapshot = self.snapshot;
        let (cpu_text, cpu_hue) = match snapshot.cpu_load {
            // An engine that is not running has no load to report, and "0%"
            // would read as "running and idle".
            None => ("—".to_string(), Colors::text_faint()),
            Some(load) => (
                format!("{:.0}%", (load.clamp(0.0, 9.99) * 100.0).round()),
                cpu_color(load),
            ),
        };

        let mut plate = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(space::SNUG))
            .flex_none()
            .w(px(TRANSPORT_PERF_METER_WIDTH))
            .h(px(crate::shell_metrics::TRANSPORT_CONTROL_HEIGHT))
            .px(px(space::SNUG))
            .rounded(px(radius::CONTROL))
            // Same recessed plane as the transport readout and the master strip
            // — this is a display, not a control.
            .bg(Colors::surface_canvas())
            .border(px(1.0))
            .border_color(Colors::border_normal())
            .child(field("CPU", cpu_text, cpu_hue));

        // Memory is dropped entirely where the platform cannot report it,
        // rather than shown empty: the two remaining fields then get the width.
        if let Some(bytes) = snapshot.memory_bytes {
            plate = plate.child(divider()).child(field(
                "RAM",
                format_memory(bytes),
                Colors::text_secondary(),
            ));
        }

        plate.child(divider()).child(field(
            "VOICE",
            snapshot.voices.to_string(),
            if snapshot.voices > 0 {
                Colors::text_secondary()
            } else {
                Colors::text_faint()
            },
        ))
    }
}

/// Build a snapshot from the engine's last stats block.
///
/// `callback_deadline_us` is zero until a stream has opened and reported its
/// block budget; dividing by it anyway would print a load of infinity or, worse,
/// a plausible-looking number.
pub fn perf_snapshot_from_engine(
    running: bool,
    callback_last_us: u32,
    callback_deadline_us: u32,
    voices: u32,
) -> TransportPerfSnapshot {
    let cpu_load = (running && callback_deadline_us > 0)
        .then(|| callback_last_us as f32 / callback_deadline_us as f32);
    TransportPerfSnapshot {
        cpu_load,
        voices,
        memory_bytes: crate::perf::process_memory_bytes(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stopped engine has no load to report; printing 0% would read as
    /// "running and idle", which is a different fact.
    #[test]
    fn a_stopped_engine_reports_no_load() {
        let snapshot = perf_snapshot_from_engine(false, 500, 1000, 0);
        assert_eq!(snapshot.cpu_load, None);
    }

    /// A stream that has not reported its block budget yet cannot be divided
    /// by.
    #[test]
    fn a_missing_deadline_reports_no_load() {
        let snapshot = perf_snapshot_from_engine(true, 500, 0, 0);
        assert_eq!(snapshot.cpu_load, None);
    }

    #[test]
    fn load_is_the_callback_against_its_deadline() {
        let snapshot = perf_snapshot_from_engine(true, 750, 1000, 12);
        assert_eq!(snapshot.voices, 12);
        let load = snapshot.cpu_load.expect("running with a deadline");
        assert!((load - 0.75).abs() < 1.0e-6, "got {load}");
    }

    /// The hue is the second channel on the load: it has to change at the
    /// thresholds, not merely track the number.
    #[test]
    fn load_colour_changes_at_the_thresholds() {
        assert_eq!(cpu_color(0.10), Colors::text_secondary());
        assert_eq!(cpu_color(CPU_WARN), Colors::status_warning());
        assert_eq!(cpu_color(CPU_DANGER), Colors::status_error());
    }

    /// Past a gigabyte the megabyte digits are noise and would widen the
    /// column; below it they are the whole reading.
    #[test]
    fn memory_switches_unit_at_a_gigabyte() {
        assert_eq!(format_memory(512 * 1024 * 1024), "512 MB");
        assert_eq!(format_memory(2 * 1024 * 1024 * 1024), "2.0 GB");
    }

    /// A meter that has not visibly moved must not repaint the entity.
    #[test]
    fn sub_percent_drift_does_not_repaint() {
        let a = TransportPerfSnapshot {
            cpu_load: Some(0.4001),
            voices: 3,
            memory_bytes: Some(700 * 1024 * 1024),
        };
        let b = TransportPerfSnapshot {
            cpu_load: Some(0.4004),
            ..a
        };
        assert_eq!(a.signature(), b.signature());
    }

    /// One more voice is a drawn digit, so it must.
    #[test]
    fn a_new_voice_repaints() {
        let a = TransportPerfSnapshot {
            cpu_load: Some(0.4),
            voices: 3,
            memory_bytes: Some(700 * 1024 * 1024),
        };
        let b = TransportPerfSnapshot { voices: 4, ..a };
        assert_ne!(a.signature(), b.signature());
    }
}
