//! Runtime latency graph planning and PDC delays (Phase V/W).
//!
//! Built on the control thread alongside `RuntimeAudioGraph`. The audio callback
//! reads precomputed per-track delay sample counts and applies ring-buffer delay
//! lines before routing so parallel paths align at the master summing bus.

use std::collections::HashMap;

use crate::audio_graph::{is_master_track_type, is_routing_track_type, RuntimeAudioGraph};
use crate::runtime::RuntimeTrack;

/// Precomputed latency / PDC data for a runtime project snapshot.
#[derive(Debug, Clone, Default)]
pub struct RuntimeLatencyGraph {
    /// Sum of enabled native-plugin insert latencies on each track strip.
    pub track_plugin_latency: Vec<u32>,
    /// Latency at each track's output tap (includes upstream feed for routing tracks).
    pub track_output_latency: Vec<u32>,
    /// Delay applied to post-fader **main output** after sends, so dry/wet
    /// paths that diverge through return FX still align at the master bus.
    pub track_pdc_delay: Vec<u32>,
    /// Maximum path latency to the master summing bus (before master inserts).
    pub max_path_latency_samples: u32,
    pub master_plugin_latency: u32,
}

#[inline]
pub fn strip_plugin_latency_samples(track: &RuntimeTrack) -> u32 {
    // In-process VST3 insert latency, summed directly from the live processors.
    let from_inserts: u32 = track
        .inserts
        .iter()
        .filter(|insert| insert.enabled)
        .map(|insert| {
            insert
                .vst3
                .as_ref()
                .filter(|vst3| vst3.is_ready())
                .map(|vst3| vst3.get_latency_samples().max(0) as u32)
                .unwrap_or(0)
        })
        .sum();
    // `from_inserts` only sees in-process VST3 inserts — it does NOT account for
    // external-bridge inserts (their latency = reported + the one-block
    // handshake, tracked in `plugin_latency_samples` by
    // `RuntimeProject::track_insert_latency_samples`). Taking the max keeps both
    // sources: a track with only VST3 inserts uses the direct sum, and a track
    // that also has bridged inserts keeps the complete stored value instead of
    // silently dropping the bridge latency.
    //
    // The previous `if from_inserts > 0 { from_inserts } else { field }` dropped
    // the bridge component whenever a mixed track also had an in-process insert,
    // which both under-compensated the PDC path AND made
    // `refresh_runtime_latency_graph` see a permanent mismatch (perpetual graph
    // rebuild, never converging). `build` seeds `plugin_latency_samples = 0`
    // before its first `strip` call, so the max is a no-op there and only the
    // refresh path — which stores the complete observed latency — benefits.
    from_inserts.max(track.plugin_latency_samples)
}

fn is_master_output(id: &str) -> bool {
    id.is_empty() || id.eq_ignore_ascii_case("master")
}

fn resolve_output_target_index(
    track_index: usize,
    tracks: &[RuntimeTrack],
    id_to_index: &HashMap<String, usize>,
) -> Option<usize> {
    let output_id = tracks[track_index].output_track_id.as_deref()?;
    if is_master_output(output_id) {
        return None;
    }
    id_to_index.get(output_id).copied()
}

/// Tail latency from a routing track's output toward the master summing bus,
/// excluding the track's own `track_output_latency` (already counted separately).
fn routing_tail_to_master(
    mut track_index: usize,
    tracks: &[RuntimeTrack],
    plugin_latency: &[u32],
    id_to_index: &HashMap<String, usize>,
    master_index: Option<usize>,
) -> u32 {
    let mut tail = 0u32;
    let mut hops = 0usize;
    while hops < tracks.len() {
        hops += 1;
        let Some(next) = resolve_output_target_index(track_index, tracks, id_to_index) else {
            break;
        };
        if Some(next) == master_index {
            break;
        }
        if !is_routing_track_type(&tracks[next].track_type) {
            break;
        }
        tail = tail.saturating_add(plugin_latency[next]);
        track_index = next;
    }
    tail
}

fn path_to_master_sum(
    track_index: usize,
    tracks: &[RuntimeTrack],
    output_latency: &[u32],
    plugin_latency: &[u32],
    id_to_index: &HashMap<String, usize>,
    master_index: Option<usize>,
) -> u32 {
    if Some(track_index) == master_index {
        return 0;
    }
    output_latency
        .get(track_index)
        .copied()
        .unwrap_or(0)
        .saturating_add(routing_tail_to_master(
            track_index,
            tracks,
            plugin_latency,
            id_to_index,
            master_index,
        ))
}

fn effective_path_to_master(
    track_index: usize,
    tracks: &[RuntimeTrack],
    output_latency: &[u32],
    plugin_latency: &[u32],
    id_to_index: &HashMap<String, usize>,
    master_index: Option<usize>,
) -> u32 {
    let mut path = main_path_to_master(
        track_index,
        tracks,
        output_latency,
        plugin_latency,
        id_to_index,
        master_index,
    );

    for send in &tracks[track_index].sends {
        if !send.enabled {
            continue;
        }
        if let Some(&ret_idx) = id_to_index.get(&send.return_track_id) {
            let via_return = path_to_master_sum(
                ret_idx,
                tracks,
                output_latency,
                plugin_latency,
                id_to_index,
                master_index,
            );
            path = path.max(via_return);
        }
    }
    path
}

/// Latency of the track's main-output (dry) path only — excludes send/return
/// branches. Used for PDC delay amounts so dry can catch up to wet without
/// delaying the send feed itself.
fn main_path_to_master(
    track_index: usize,
    tracks: &[RuntimeTrack],
    output_latency: &[u32],
    plugin_latency: &[u32],
    id_to_index: &HashMap<String, usize>,
    master_index: Option<usize>,
) -> u32 {
    resolve_output_target_index(track_index, tracks, id_to_index)
        .filter(|&target| is_routing_track_type(&tracks[target].track_type))
        .map(|target| {
            path_to_master_sum(
                target,
                tracks,
                output_latency,
                plugin_latency,
                id_to_index,
                master_index,
            )
        })
        .unwrap_or_else(|| {
            path_to_master_sum(
                track_index,
                tracks,
                output_latency,
                plugin_latency,
                id_to_index,
                master_index,
            )
        })
}

/// Build latency metadata and per-track PDC delays from runtime tracks and the
/// audio graph plan. When `pdc_enabled` is false, delays are zeroed but path
/// latencies are still computed for reporting.
pub fn plan_runtime_latency_graph(
    tracks: &[RuntimeTrack],
    audio_graph: &RuntimeAudioGraph,
    pdc_enabled: bool,
) -> RuntimeLatencyGraph {
    let n = tracks.len();
    if n == 0 {
        return RuntimeLatencyGraph::default();
    }

    let mut id_to_index: HashMap<String, usize> = HashMap::new();
    for (idx, track) in tracks.iter().enumerate() {
        id_to_index.insert(track.id.clone(), idx);
    }

    let plugin_latency: Vec<u32> = tracks.iter().map(strip_plugin_latency_samples).collect();
    let master_index = audio_graph.master_index;
    let master_plugin_latency = master_index
        .and_then(|idx| plugin_latency.get(idx).copied())
        .unwrap_or(0);

    let mut output_latency = plugin_latency.clone();

    for &idx in &audio_graph.pass2_routing_indices {
        let mut feed_max = 0u32;
        for (src_idx, track) in tracks.iter().enumerate() {
            if is_master_track_type(&track.track_type) {
                continue;
            }
            for send in &track.sends {
                if !send.enabled {
                    continue;
                }
                if id_to_index.get(&send.return_track_id) == Some(&idx) {
                    feed_max = feed_max.max(output_latency[src_idx]);
                }
            }
            // Main-output → bus/return feeds also contribute upstream latency.
            if resolve_output_target_index(src_idx, tracks, &id_to_index) == Some(idx) {
                feed_max = feed_max.max(output_latency[src_idx]);
            }
        }
        output_latency[idx] = plugin_latency[idx].saturating_add(feed_max);
    }

    let mut max_path_latency_samples = 0u32;
    for idx in 0..n {
        if Some(idx) == master_index {
            continue;
        }
        if is_master_track_type(&tracks[idx].track_type) {
            continue;
        }
        let path = effective_path_to_master(
            idx,
            tracks,
            &output_latency,
            &plugin_latency,
            &id_to_index,
            master_index,
        );
        max_path_latency_samples = max_path_latency_samples.max(path);
    }

    let mut track_pdc_delay = vec![0u32; n];
    if pdc_enabled && max_path_latency_samples > 0 {
        for idx in 0..n {
            if Some(idx) == master_index || is_master_track_type(&tracks[idx].track_type) {
                continue;
            }
            // PDC delay is relative to the *main/dry* path so return-FX latency
            // can pull dry forward without also delaying the send feed.
            let path = main_path_to_master(
                idx,
                tracks,
                &output_latency,
                &plugin_latency,
                &id_to_index,
                master_index,
            );
            track_pdc_delay[idx] = max_path_latency_samples.saturating_sub(path);
        }
    }

    RuntimeLatencyGraph {
        track_plugin_latency: plugin_latency,
        track_output_latency: output_latency,
        track_pdc_delay,
        max_path_latency_samples,
        master_plugin_latency,
    }
}

/// In-place stereo delay line for PDC. `delay_l` / `delay_r` must be preallocated
/// with length >= `delay_samples + frames`.
#[inline]
pub fn apply_pdc_delay_block(
    block_l: &mut [f32],
    block_r: &mut [f32],
    delay_l: &mut [f32],
    delay_r: &mut [f32],
    write_pos: &mut usize,
    delay_samples: u32,
    frames: usize,
) {
    let delay = delay_samples as usize;
    if delay == 0 || frames == 0 {
        return;
    }
    let cap = delay_l.len();
    if cap <= delay {
        return;
    }

    for frame in 0..frames {
        let wp = *write_pos % cap;
        let rp = (wp + cap - delay) % cap;
        let out_l = delay_l[rp];
        let out_r = delay_r[rp];
        delay_l[wp] = block_l[frame];
        delay_r[wp] = block_r[frame];
        block_l[frame] = out_l;
        block_r[frame] = out_r;
        *write_pos = (wp + 1) % cap;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_graph::plan_runtime_audio_graph;
    use crate::runtime::{RuntimePreviewMode, RuntimeSend, RuntimeTrack};

    fn track(id: &str, ty: &str, plugin_latency: u32, sends: Vec<RuntimeSend>) -> RuntimeTrack {
        RuntimeTrack {
            id: id.to_string(),
            track_type: ty.to_string(),
            volume: 1.0,
            pan: 0.0,
            muted: false,
            solo: false,
            record_armed: false,
            monitor_enabled: false,
            input_source: crate::runtime::RuntimeTrackInputSource::None,
            preview_mode: RuntimePreviewMode::Stereo,
            output_track_id: None,
            output_track_index: None,
            inserts: Vec::new(),
            sends,
            automation_lanes: Vec::new(),
            plugin_param_automation: Vec::new(),
            meter: std::sync::Arc::new(crate::runtime::RuntimeTrackMeter::default()),
            meter_peak_l: 0.0,
            meter_peak_r: 0.0,
            meter_sum_sq_l: 0.0,
            meter_sum_sq_r: 0.0,
            callback_insert_log_done: false,
            callback_clip_route_log_done: false,
            block_l: vec![0.0; 64],
            block_r: vec![0.0; 64],
            recv_l: vec![0.0; 64],
            recv_r: vec![0.0; 64],
            soundfont_l: vec![0.0; 64],
            soundfont_r: vec![0.0; 64],
            midi_block_events: Vec::new(),
            midi_instrument_insert_ix: None,
            soundfont_player: None,
            pdc_delay_l: Vec::new(),
            pdc_delay_r: Vec::new(),
            pdc_write_pos: 0,
            plugin_latency_samples: plugin_latency,
            smoothed_gain_l: 1.0,
            smoothed_gain_r: 1.0,
        }
    }

    fn send(id: &str, target: &str) -> RuntimeSend {
        RuntimeSend {
            id: id.to_string(),
            return_track_id: target.to_string(),
            return_track_index: None,
            level: 1.0,
            enabled: true,
            pre_fader: false,
        }
    }

    #[test]
    fn pdc_delays_shorter_track_to_match_longer_path() {
        let tracks = vec![
            track("fast", "audio", 0, vec![]),
            track("slow", "audio", 512, vec![]),
            track("master", "master", 0, vec![]),
        ];
        let audio_graph = plan_runtime_audio_graph(&tracks).unwrap();
        let latency = plan_runtime_latency_graph(&tracks, &audio_graph, true);
        assert_eq!(latency.max_path_latency_samples, 512);
        assert_eq!(latency.track_pdc_delay[0], 512);
        assert_eq!(latency.track_pdc_delay[1], 0);
    }

    #[test]
    fn return_feed_increases_path_latency() {
        let tracks = vec![
            track("src", "audio", 128, vec![send("s", "ret")]),
            track("ret", "return", 256, vec![]),
            track("master", "master", 0, vec![]),
        ];
        let audio_graph = plan_runtime_audio_graph(&tracks).unwrap();
        let latency = plan_runtime_latency_graph(&tracks, &audio_graph, true);
        assert_eq!(latency.track_output_latency[1], 128 + 256);
        assert_eq!(latency.max_path_latency_samples, 128 + 256);
        // Dry main path is 128; wet via return is 384. PDC delays dry by 256
        // (applied after the send tap) so dry and wet meet at the master.
        assert_eq!(latency.track_pdc_delay[0], 256);
        assert_eq!(latency.track_pdc_delay[1], 0);
    }

    #[test]
    fn strip_latency_uses_stored_value_as_a_floor() {
        // A track carrying bridge latency in `plugin_latency_samples` (set by the
        // refresh path) must never have it dropped by `strip`. With no in-process
        // VST3 inserts the direct sum is 0, so the stored value is the result —
        // and the max means a mixed VST3+bridge track keeps the complete value
        // rather than collapsing to the VST3-only sum (the perpetual-rebuild /
        // PDC-undercount bug). Insert-level mixing needs live VST3/bridge
        // processors, so this locks the field-floor contract the fix relies on.
        let t = track("bridged", "audio", 900, vec![]);
        assert_eq!(strip_plugin_latency_samples(&t), 900);

        let zero = track("empty", "audio", 0, vec![]);
        assert_eq!(strip_plugin_latency_samples(&zero), 0);
    }

    #[test]
    fn apply_pdc_delay_block_shifts_samples() {
        let mut block_l = vec![1.0, 2.0, 3.0, 4.0];
        let mut block_r = vec![-1.0, -2.0, -3.0, -4.0];
        let mut delay_l = vec![0.0; 8];
        let mut delay_r = vec![0.0; 8];
        let mut pos = 0usize;
        apply_pdc_delay_block(
            &mut block_l,
            &mut block_r,
            &mut delay_l,
            &mut delay_r,
            &mut pos,
            2,
            4,
        );
        assert_eq!(block_l[0], 0.0);
        assert_eq!(block_l[1], 0.0);
        assert_eq!(block_l[2], 1.0);
        assert_eq!(block_l[3], 2.0);
    }
}
