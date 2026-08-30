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

use gpui::{
    div, px, svg, Context, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Window,
};

use crate::assets;
use crate::components::controls::fb_tooltip;
use crate::theme::{radius, size, space, typography, Colors};

/// Size of a field's glyph. Matched to the value's cap height rather than its
/// em box, so the icon sits as one more character in the row instead of a
/// picture beside it.
const FIELD_ICON: f32 = 12.0;

// Reserved width of each value column.
//
// The plate is sized by its content, but each *value* gets a fixed column wide
// enough for the longest string that field can produce. That is what stops the
// row twitching as digits come and go — and, unlike one width for the whole
// plate, it cannot leave one field starved while another has room to spare. The
// readout used to split the plate three ways with `flex_1`, so VOICE (longest
// label, shortest value) took a third of the width and "1024 MB" came out as
// "7…".
/// `100%`, with room for a runaway load's third digit. Shared by both loads.
const CPU_VALUE_W: f32 = 34.0;
/// `1024 MB` — the widest form before the unit switches to `1.9 GB`.
const MEMORY_VALUE_W: f32 = 58.0;
/// `256` — the SoundFont player's polyphony ceiling.
const VOICE_VALUE_W: f32 = 30.0;

/// Audio load above this reads as "this take is at risk", and the value takes
/// the warning hue. Chosen at the point a callback is using most of its
/// deadline and a single late block would drop out.
const CPU_WARN: f32 = 0.75;
/// Above this the engine is effectively at its budget.
const CPU_DANGER: f32 = 0.92;

// The UI thread has no deadline to miss, so its thresholds are about headroom
// rather than dropouts: past two thirds of a core there is nothing left to
// absorb a spike, and near a whole core the interface cannot keep a frame rate
// however fast the machine is.
const UI_WARN: f32 = 0.65;
const UI_DANGER: f32 = 0.90;

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
    /// CPU the UI thread is burning, as a share of one core. `None` before the
    /// first sampling window closes, and on platforms that cannot report it.
    ///
    /// The audio and interface threads fail in different ways and the readout
    /// used to describe only the first: an engine idling at 5% while the
    /// arrangement stutters is a real and common state, and it had no number.
    pub ui_load: Option<f32>,
    /// SoundFont voices sounding in the last block. See
    /// `EngineInner::active_voice_count` for what this does and does not count.
    pub voices: u32,
    /// Working set of Studio *and* its plugin hosts, or `None` on a platform
    /// that cannot supply it. See [`crate::perf::MemoryUsage`] for why the
    /// hosts are in the total: that is where a loaded sampler's gigabytes are.
    pub memory_bytes: Option<u64>,
    /// The plugin-host share of `memory_bytes`, for the tooltip's breakdown.
    pub plugin_host_bytes: u64,
    /// How many host processes contributed. Zero means no plug-in has been
    /// loaded yet, and the tooltip then says only Studio's own figure.
    pub plugin_hosts: usize,
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
        let ui = self
            .ui_load
            .map(|load| (load.clamp(0.0, 4.0) * 100.0).round() as u64 + 1)
            .unwrap_or(0);
        let mem = self
            .memory_bytes
            .map(|bytes| bytes / (1024 * 1024) + 1)
            .unwrap_or(0);
        // The host count is drawn only in the tooltip, but a host appearing or
        // going away can leave the megabyte total unchanged, and the tooltip
        // would then keep naming a process that has exited. `cpu` needs nine
        // bits and `voices` starts at twelve, so it rides the gap between them.
        let hosts = (self.plugin_hosts.min(7) as u64) << 9;
        // Each field gets its own bits: `cpu` and `ui` nine each, `hosts`
        // three, `voices` ten, and memory the top half. Voices is clamped to
        // its ten bits rather than trusted — an engine reporting a wild count
        // would otherwise carry into the field above and freeze that readout.
        let voices = (self.voices.min(1023) as u64) << 12;
        cpu | hosts | voices | (ui << 22) | (mem << 32)
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

/// Colour for the UI thread's load. Same two-channel idea as [`cpu_color`],
/// against the interface's own thresholds.
fn ui_color(load: f32) -> gpui::Rgba {
    if load >= UI_DANGER {
        Colors::status_error()
    } else if load >= UI_WARN {
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

/// A field's mark, drawn the way the transport LCD draws its captions — a
/// fixed `flex_none` box, tinted from `currentColor`. Fixed for the same reason
/// the value column is: nothing in this row may be resized by its neighbours.
fn mark(path: &'static str) -> impl IntoElement {
    svg()
        .path(path)
        .flex_none()
        .w(px(FIELD_ICON))
        .h(px(FIELD_ICON))
        .text_color(Colors::text_faint())
}

/// Where the memory figure comes from, named process by process.
///
/// The total is worth nothing on its own if the reader cannot tell whether the
/// gigabyte is Studio or the sampler they just loaded — and the plugin hosts
/// are usually the larger half, which is exactly the surprise the single-process
/// reading used to hide.
fn memory_tooltip(snapshot: TransportPerfSnapshot) -> String {
    let Some(total) = snapshot.memory_bytes else {
        return "Memory in use".to_string();
    };
    if snapshot.plugin_hosts == 0 {
        return format!("Memory in use — {} (Studio)", format_memory(total));
    }
    let studio = total.saturating_sub(snapshot.plugin_host_bytes);
    let hosts = if snapshot.plugin_hosts == 1 {
        "1 plug-in host".to_string()
    } else {
        format!("{} plug-in hosts", snapshot.plugin_hosts)
    };
    format!(
        "Memory in use — {} Studio + {} in {hosts}",
        format_memory(studio),
        format_memory(snapshot.plugin_host_bytes),
    )
}

/// One `glyph  value` pair. The glyph is quieter than the value on purpose —
/// the number is what is read at a glance, the mark only says which number.
///
/// A mark rather than a word: three of these sit in the transport's left
/// gutter, and `CPU`/`RAM`/`VOICE` spent more of that gutter naming the fields
/// than showing them — `VOICE` alone was wider than the number it labelled.
/// `name` goes to the tooltip, the same way the transport LCD names its own
/// icon-only fields, so the mark is never the only explanation.
///
/// Both halves are `flex_none`: the glyph takes its own box and the value takes
/// its reserved column. Nothing here competes for space, so nothing here can be
/// truncated by a neighbour.
///
/// `id` only has to be unique among the three fields — it exists because a
/// tooltip needs a stateful element to hang off.
fn field(
    id: &'static str,
    glyph: &'static str,
    name: impl Into<gpui::SharedString>,
    value: String,
    value_color: gpui::Rgba,
    value_width: f32,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .flex_row()
        // Centred, not baseline-aligned: an SVG has no baseline to share with
        // the digits, so aligning on one drops the glyph below the row.
        .items_center()
        .gap(px(space::TIGHT))
        .flex_none()
        .child(mark(glyph))
        .child(
            div()
                .flex_none()
                .w(px(value_width))
                .text_size(px(typography::UI_SM))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(value_color)
                .child(value),
        )
        .tooltip(fb_tooltip(name))
}

/// Hairline between two readouts. Inset top and bottom so it separates the
/// fields without drawing a full-height rule through the plate — the same
/// treatment the transport's own LCD divider uses.
fn divider() -> impl IntoElement {
    div()
        .flex_none()
        .w(px(1.0))
        .h(px(size::MICRO))
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
        let (ui_text, ui_hue) = match snapshot.ui_load {
            // No window has closed yet. Same reasoning as the engine: an
            // unmeasured load is not a load of zero.
            None => ("—".to_string(), Colors::text_faint()),
            Some(load) => (
                format!("{:.0}%", (load.clamp(0.0, 9.99) * 100.0).round()),
                ui_color(load),
            ),
        };

        // No fixed plate width. The fields reserve their own columns, so the
        // plate is exactly as wide as it needs to be and a field can never be
        // squeezed by the plate guessing too small.
        let mut plate = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(space::SNUG))
            .flex_none()
            .h(px(crate::shell_metrics::TRANSPORT_CONTROL_HEIGHT))
            .px(px(space::BASE))
            .rounded(px(radius::CONTROL))
            // Same recessed plane as the transport readout and the master strip
            // — this is a display, not a control.
            .bg(Colors::surface_canvas())
            .border(px(1.0))
            .border_color(Colors::border_normal())
            .child(field(
                "perf-cpu",
                assets::ICON_CPU_PATH,
                "Audio engine load — callback time against its deadline",
                cpu_text,
                cpu_hue,
                CPU_VALUE_W,
            ))
            // Second, beside the engine's, because the two answer different
            // questions and only together say which thread is in trouble.
            .child(divider())
            .child(field(
                "perf-ui",
                assets::ICON_MONITOR_PATH,
                "UI thread load — Studio's interface thread, as a share of one core",
                ui_text,
                ui_hue,
                CPU_VALUE_W,
            ));

        // Memory is dropped entirely where the platform cannot report it,
        // rather than shown empty, and the plate simply gets narrower.
        if let Some(bytes) = snapshot.memory_bytes {
            plate = plate.child(divider()).child(field(
                "perf-memory",
                assets::ICON_MEMORY_STICK_PATH,
                memory_tooltip(snapshot),
                format_memory(bytes),
                Colors::text_secondary(),
                MEMORY_VALUE_W,
            ));
        }

        plate.child(divider()).child(field(
            "perf-voices",
            assets::ICON_AUDIO_LINES_PATH,
            "Active voices",
            snapshot.voices.to_string(),
            if snapshot.voices > 0 {
                Colors::text_secondary()
            } else {
                Colors::text_faint()
            },
            VOICE_VALUE_W,
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
    let memory = crate::perf::memory_usage();
    TransportPerfSnapshot {
        cpu_load,
        // Sampled here rather than in `perf`'s frame accounting because this
        // poll is the one thing that runs on the UI thread every tick whether
        // or not tracing is on.
        ui_load: crate::perf::ui_thread_cpu_load(),
        voices,
        memory_bytes: memory.total_bytes(),
        plugin_host_bytes: memory.plugin_host_bytes,
        plugin_hosts: memory.plugin_hosts,
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

    /// Every reserved column has to hold the widest string its field can
    /// produce, or the value is truncated the moment the number grows — which
    /// is exactly how "1024 MB" first shipped as "7…".
    #[test]
    fn every_value_column_fits_its_widest_string() {
        // `estimate_label_width` is calibrated for `UI_XS`; the values render
        // at `UI_SM`, so scale before comparing.
        let width = |text: &str| {
            crate::theme::menu::estimate_label_width(text) * (typography::UI_SM / typography::UI_XS)
        };
        for (column, widest, label) in [
            (CPU_VALUE_W, "100%", "CPU"),
            (MEMORY_VALUE_W, "1024 MB", "RAM"),
            (VOICE_VALUE_W, "256", "VOICE"),
        ] {
            let needed = width(widest);
            assert!(
                column >= needed,
                "{label} reserves {column} px but \"{widest}\" needs {needed}"
            );
        }
    }

    /// The memory column is sized for the megabyte form, which is the wider of
    /// the two the formatter produces.
    #[test]
    fn the_gigabyte_form_is_never_the_wider_one() {
        let mb = format_memory(1023 * 1024 * 1024);
        let gb = format_memory(9 * 1024 * 1024 * 1024);
        assert!(
            mb.len() >= gb.len(),
            "sized for {mb:?} but {gb:?} is longer"
        );
    }

    /// A meter that has not visibly moved must not repaint the entity.
    #[test]
    fn sub_percent_drift_does_not_repaint() {
        let a = TransportPerfSnapshot {
            cpu_load: Some(0.4001),
            voices: 3,
            memory_bytes: Some(700 * 1024 * 1024),
            ..TransportPerfSnapshot::default()
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
            ..TransportPerfSnapshot::default()
        };
        let b = TransportPerfSnapshot { voices: 4, ..a };
        assert_ne!(a.signature(), b.signature());
    }
}

#[cfg(test)]
mod memory_readout_tests {
    use super::*;

    fn with_hosts(total_mb: u64, host_mb: u64, hosts: usize) -> TransportPerfSnapshot {
        TransportPerfSnapshot {
            cpu_load: Some(0.2),
            ui_load: Some(0.2),
            voices: 4,
            memory_bytes: Some(total_mb * 1024 * 1024),
            plugin_host_bytes: host_mb * 1024 * 1024,
            plugin_hosts: hosts,
        }
    }

    /// The whole reason the hosts are counted: a loaded sampler holds more than
    /// Studio does, and the reader has to be able to see which half is which.
    #[test]
    fn the_tooltip_names_both_halves() {
        let tip = memory_tooltip(with_hosts(5000, 3500, 1));
        assert!(tip.contains("1.5 GB Studio"), "{tip}");
        assert!(tip.contains("3.4 GB"), "{tip}");
        assert!(tip.contains("1 plug-in host"), "{tip}");
    }

    /// No plug-in loaded is a real state, not a missing reading, and must not
    /// claim a host share of zero.
    #[test]
    fn with_no_host_the_tooltip_says_studio_only() {
        let tip = memory_tooltip(with_hosts(1500, 0, 0));
        assert_eq!(tip, "Memory in use — 1.5 GB (Studio)");
    }

    #[test]
    fn several_hosts_are_counted_in_the_tooltip() {
        let tip = memory_tooltip(with_hosts(6000, 4000, 3));
        assert!(tip.contains("3 plug-in hosts"), "{tip}");
    }

    /// A host appearing or exiting can leave the megabyte total unchanged; the
    /// tooltip would then keep naming a process that is gone.
    #[test]
    fn a_host_count_change_repaints() {
        let one = with_hosts(5000, 3500, 1);
        let two = TransportPerfSnapshot {
            plugin_hosts: 2,
            ..one
        };
        assert_ne!(one.signature(), two.signature());
    }

    /// The host count must not bleed into the voice or memory fields.
    #[test]
    fn the_host_count_does_not_disturb_the_other_fields() {
        let a = with_hosts(5000, 3500, 0);
        let b = TransportPerfSnapshot { voices: 5, ..a };
        let c = TransportPerfSnapshot {
            memory_bytes: Some(5001 * 1024 * 1024),
            ..a
        };
        assert_ne!(a.signature(), b.signature());
        assert_ne!(a.signature(), c.signature());
        assert_ne!(b.signature(), c.signature());
    }

    /// Studio's own reading is the one the platform may not have; without it
    /// there is no honest total to print, hosts or not.
    #[test]
    fn no_studio_reading_means_no_total() {
        let usage = crate::perf::MemoryUsage {
            studio_bytes: None,
            plugin_host_bytes: 3500 * 1024 * 1024,
            plugin_hosts: 1,
        };
        assert_eq!(usage.total_bytes(), None);
    }

    /// The total is the sum, which is the number Task Manager's two rows add up
    /// to and the one that decides whether the session still fits in RAM.
    #[test]
    fn the_total_is_studio_plus_its_hosts() {
        let usage = crate::perf::MemoryUsage {
            studio_bytes: Some(1_600),
            plugin_host_bytes: 3_500,
            plugin_hosts: 1,
        };
        assert_eq!(usage.total_bytes(), Some(5_100));
    }
}

#[cfg(test)]
mod ui_load_tests {
    use super::*;

    fn snapshot() -> TransportPerfSnapshot {
        TransportPerfSnapshot {
            cpu_load: Some(0.05),
            ui_load: Some(0.30),
            voices: 4,
            memory_bytes: Some(1500 * 1024 * 1024),
            plugin_host_bytes: 0,
            plugin_hosts: 0,
        }
    }

    /// The case the second readout exists for: an idle engine and a UI thread
    /// pinned to a core. One number cannot say that; two can.
    #[test]
    fn the_two_loads_are_independent() {
        let a = snapshot();
        let b = TransportPerfSnapshot {
            ui_load: Some(0.95),
            ..a
        };
        assert_eq!(a.cpu_load, b.cpu_load);
        assert_ne!(a.signature(), b.signature());
    }

    /// Each field owns its own bits, or a change in one would silently freeze
    /// the readout of another.
    #[test]
    fn no_field_carries_into_its_neighbour() {
        let base = snapshot();
        let variants = [
            TransportPerfSnapshot {
                cpu_load: Some(0.06),
                ..base
            },
            TransportPerfSnapshot {
                ui_load: Some(0.31),
                ..base
            },
            TransportPerfSnapshot { voices: 5, ..base },
            TransportPerfSnapshot {
                plugin_hosts: 1,
                ..base
            },
            TransportPerfSnapshot {
                memory_bytes: Some(1501 * 1024 * 1024),
                ..base
            },
        ];
        for (i, a) in variants.iter().enumerate() {
            assert_ne!(base.signature(), a.signature(), "variant {i}");
            for (j, b) in variants.iter().enumerate().skip(i + 1) {
                assert_ne!(a.signature(), b.signature(), "variants {i} and {j}");
            }
        }
    }

    /// An engine reporting a wild voice count must not carry into the load
    /// above it and freeze that reading.
    #[test]
    fn an_absurd_voice_count_stays_in_its_own_bits() {
        let base = snapshot();
        let wild = TransportPerfSnapshot {
            voices: u32::MAX,
            ..base
        };
        let wild_and_busier = TransportPerfSnapshot {
            ui_load: Some(0.95),
            ..wild
        };
        assert_ne!(wild.signature(), wild_and_busier.signature());
    }

    /// The hue is the second channel on the load, so it has to change at the
    /// interface's own thresholds and not the engine's.
    #[test]
    fn ui_colour_changes_at_the_ui_thresholds() {
        assert_eq!(ui_color(0.10), Colors::text_secondary());
        assert_eq!(ui_color(UI_WARN), Colors::status_warning());
        assert_eq!(ui_color(UI_DANGER), Colors::status_error());
        // The engine's warning point is not the interface's.
        assert_eq!(ui_color(CPU_WARN), Colors::status_warning());
        assert_eq!(cpu_color(UI_WARN), Colors::text_secondary());
    }

    /// A load that has not been measured yet is not a load of zero.
    #[test]
    fn an_unmeasured_load_is_distinct_from_idle() {
        let unmeasured = TransportPerfSnapshot {
            ui_load: None,
            ..snapshot()
        };
        let idle = TransportPerfSnapshot {
            ui_load: Some(0.0),
            ..snapshot()
        };
        assert_ne!(unmeasured.signature(), idle.signature());
    }
}
