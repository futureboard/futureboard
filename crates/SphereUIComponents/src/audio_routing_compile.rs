//! Compile the logical [`AudioConnectionRegistry`] into an immutable runtime
//! routing snapshot the audio callback can consume directly.
//!
//! ```txt
//! AudioConnectionRegistry + track assignments
//!     -> validate ids
//!     -> resolve device + stable port ids
//!     -> resolve physical channel indexes
//!     -> EngineAudioRoutingSnapshot   (immutable, Arc-swapped)
//!     -> audio callback
//! ```
//!
//! Everything expensive happens here, on the control thread. The snapshot holds
//! only pre-resolved integers, so the callback never parses an id, hashes a
//! string, walks a UI model, locks, or allocates. A connection that is missing,
//! disabled, or invalid compiles to *silence* — never to a fallback channel,
//! because monitoring or recording the wrong physical input is worse than
//! recording nothing.

use std::sync::Arc;

use crate::audio_connections::{
    AudioConnectionDirection, AudioConnectionId, AudioConnectionRegistry, AvailablePorts,
};

/// Maximum channels one logical bus can resolve to without spilling to the
/// heap. Covers mono through 7.1; larger custom layouts are truncated at
/// compile time rather than allocating on the audio thread.
pub const MAX_ROUTED_CHANNELS: usize = 8;

/// Fixed-capacity channel list. Inline storage keeps the whole snapshot free of
/// per-entry heap indirection, so the callback touches one contiguous block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SmallChannelList {
    channels: [u32; MAX_ROUTED_CHANNELS],
    len: u8,
}

impl SmallChannelList {
    pub fn from_slice(source: &[u32]) -> Self {
        let mut channels = [0u32; MAX_ROUTED_CHANNELS];
        let len = source.len().min(MAX_ROUTED_CHANNELS);
        channels[..len].copy_from_slice(&source[..len]);
        Self {
            channels,
            len: len as u8,
        }
    }

    #[inline]
    pub fn as_slice(&self) -> &[u32] {
        &self.channels[..self.len as usize]
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// A device resolved to a dense runtime index. The callback compares small
/// integers instead of device-id strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RuntimeAudioDeviceId(pub u32);

/// One track's fully resolved capture source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedInputSource {
    pub device_runtime_id: RuntimeAudioDeviceId,
    pub channels: SmallChannelList,
}

/// One track's input, in the engine's track order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTrackInput {
    /// Index into the engine's track table — never a string id.
    pub track_runtime_index: u32,
    /// `None` compiles to silence: unassigned, disabled, or unresolvable.
    pub source: Option<ResolvedInputSource>,
}

/// One output connection resolved to physical channels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedOutputConnection {
    pub connection_runtime_index: u32,
    pub device_runtime_id: RuntimeAudioDeviceId,
    pub channels: SmallChannelList,
}

/// Immutable routing snapshot. Published by `Arc` swap; the previous snapshot
/// stays alive until the callback holding it releases its clone.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EngineAudioRoutingSnapshot {
    /// Bumped on every publish so the callback can detect a swap without
    /// comparing contents.
    pub generation: u64,
    pub track_inputs: Vec<ResolvedTrackInput>,
    pub output_connections: Vec<ResolvedOutputConnection>,
    /// Index into `output_connections` for the master bus, if assigned.
    pub master_output: Option<u32>,
    /// Index into `output_connections` for the Control Room, if assigned.
    pub monitor_output: Option<u32>,
    /// True when master and monitor resolve to overlapping physical ports on
    /// the same device. The engine uses this to avoid writing the same mix
    /// twice into one hardware destination.
    pub master_monitor_share_ports: bool,
}

impl EngineAudioRoutingSnapshot {
    /// Resolved source for a track index. Callback-safe: a bounds-checked slice
    /// index, no lookup.
    #[inline]
    pub fn track_input(&self, track_runtime_index: usize) -> Option<&ResolvedInputSource> {
        self.track_inputs
            .get(track_runtime_index)
            .and_then(|entry| entry.source.as_ref())
    }

    #[inline]
    pub fn output(&self, index: u32) -> Option<&ResolvedOutputConnection> {
        self.output_connections.get(index as usize)
    }
}

/// What the compiler is asked to resolve. Built on the control thread from the
/// project model.
#[derive(Debug, Clone, Default)]
pub struct RoutingCompileRequest {
    /// Track input assignments in engine track order. `None` = No Input.
    pub track_inputs: Vec<Option<AudioConnectionId>>,
    pub master_output: Option<AudioConnectionId>,
    pub monitor_output: Option<AudioConnectionId>,
}

/// A problem found while compiling. Surfaced to the UI as a warning; never a
/// load or playback failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingWarning {
    /// A track references a connection that no longer exists.
    UnknownTrackInput {
        track_runtime_index: u32,
    },
    /// A track's connection exists but cannot currently be resolved.
    TrackInputUnavailable {
        track_runtime_index: u32,
        connection: AudioConnectionId,
    },
    /// A track references an output connection as its input, or similar
    /// direction mismatch.
    DirectionMismatch {
        connection: AudioConnectionId,
    },
    UnknownOutput {
        connection: AudioConnectionId,
    },
    OutputUnavailable {
        connection: AudioConnectionId,
    },
    /// Master and monitor resolve to overlapping hardware ports.
    DuplicateHardwareWrite,
}

/// Result of one compile pass.
#[derive(Debug, Clone, Default)]
pub struct RoutingCompileResult {
    pub snapshot: EngineAudioRoutingSnapshot,
    pub warnings: Vec<RoutingWarning>,
}

/// Assigns dense runtime indexes to device ids, so the snapshot carries `u32`
/// rather than strings.
#[derive(Debug, Default)]
struct DeviceIndexer {
    devices: Vec<String>,
}

impl DeviceIndexer {
    fn index_of(&mut self, device_id: &str) -> RuntimeAudioDeviceId {
        if let Some(position) = self.devices.iter().position(|d| d == device_id) {
            return RuntimeAudioDeviceId(position as u32);
        }
        self.devices.push(device_id.to_string());
        RuntimeAudioDeviceId((self.devices.len() - 1) as u32)
    }
}

/// Compile the registry and assignments into a runtime snapshot.
///
/// `generation` must increase on every publish. Pure and allocation-heavy by
/// design — it runs on the control thread, never in the callback.
pub fn compile_routing(
    registry: &AudioConnectionRegistry,
    ports: &AvailablePorts,
    request: &RoutingCompileRequest,
    generation: u64,
) -> RoutingCompileResult {
    let mut warnings = Vec::new();
    let mut devices = DeviceIndexer::default();

    // ── Track inputs ────────────────────────────────────────────────────────
    let mut track_inputs = Vec::with_capacity(request.track_inputs.len());
    for (index, assignment) in request.track_inputs.iter().enumerate() {
        let track_runtime_index = index as u32;
        let source = match assignment {
            None => None,
            Some(id) => match registry.get(id) {
                None => {
                    warnings.push(RoutingWarning::UnknownTrackInput {
                        track_runtime_index,
                    });
                    None
                }
                Some(connection) if connection.direction != AudioConnectionDirection::Input => {
                    warnings.push(RoutingWarning::DirectionMismatch {
                        connection: id.clone(),
                    });
                    None
                }
                Some(connection) => match registry.resolved_ports(id, ports) {
                    Some(channels) => {
                        let device_id = connection.device_id.as_deref().unwrap_or_default();
                        Some(ResolvedInputSource {
                            device_runtime_id: devices.index_of(device_id),
                            channels: SmallChannelList::from_slice(&channels),
                        })
                    }
                    None => {
                        // Missing device or port: compile to silence, keep the
                        // assignment, and tell the UI why.
                        warnings.push(RoutingWarning::TrackInputUnavailable {
                            track_runtime_index,
                            connection: id.clone(),
                        });
                        None
                    }
                },
            },
        };
        track_inputs.push(ResolvedTrackInput {
            track_runtime_index,
            source,
        });
    }

    // ── Outputs ─────────────────────────────────────────────────────────────
    let mut output_connections: Vec<ResolvedOutputConnection> = Vec::new();
    let mut resolve_output = |id: &AudioConnectionId,
                              devices: &mut DeviceIndexer,
                              outputs: &mut Vec<ResolvedOutputConnection>,
                              warnings: &mut Vec<RoutingWarning>|
     -> Option<u32> {
        let connection = match registry.get(id) {
            None => {
                warnings.push(RoutingWarning::UnknownOutput {
                    connection: id.clone(),
                });
                return None;
            }
            Some(connection) => connection,
        };
        if connection.direction != AudioConnectionDirection::Output {
            warnings.push(RoutingWarning::DirectionMismatch {
                connection: id.clone(),
            });
            return None;
        }
        let channels = match registry.resolved_ports(id, ports) {
            Some(channels) => channels,
            None => {
                warnings.push(RoutingWarning::OutputUnavailable {
                    connection: id.clone(),
                });
                return None;
            }
        };
        let device_id = connection.device_id.as_deref().unwrap_or_default();
        let device_runtime_id = devices.index_of(device_id);
        let channels = SmallChannelList::from_slice(&channels);
        // Reuse an identical resolved output rather than emitting a duplicate
        // row, so the master/monitor overlap check below is a simple compare.
        if let Some(existing) = outputs.iter().position(|entry| {
            entry.device_runtime_id == device_runtime_id && entry.channels == channels
        }) {
            return Some(existing as u32);
        }
        let runtime_index = outputs.len() as u32;
        outputs.push(ResolvedOutputConnection {
            connection_runtime_index: runtime_index,
            device_runtime_id,
            channels,
        });
        Some(runtime_index)
    };

    let master_output = request
        .master_output
        .as_ref()
        .and_then(|id| resolve_output(id, &mut devices, &mut output_connections, &mut warnings));
    let monitor_output = request
        .monitor_output
        .as_ref()
        .and_then(|id| resolve_output(id, &mut devices, &mut output_connections, &mut warnings));

    // ── Duplicate hardware writes ───────────────────────────────────────────
    // Master and Monitor landing on the same physical ports would write the
    // same mix twice into one destination. Flag it so the engine can let the
    // Control Room own that pair instead of summing both.
    let master_monitor_share_ports = match (master_output, monitor_output) {
        (Some(master), Some(monitor)) => {
            let master = &output_connections[master as usize];
            let monitor = &output_connections[monitor as usize];
            master.device_runtime_id == monitor.device_runtime_id
                && master
                    .channels
                    .as_slice()
                    .iter()
                    .any(|channel| monitor.channels.as_slice().contains(channel))
        }
        _ => false,
    };
    if master_monitor_share_ports {
        warnings.push(RoutingWarning::DuplicateHardwareWrite);
    }

    RoutingCompileResult {
        snapshot: EngineAudioRoutingSnapshot {
            generation,
            track_inputs,
            output_connections,
            master_output,
            monitor_output,
            master_monitor_share_ports,
        },
        warnings,
    }
}

/// Atomically swappable holder for the published snapshot.
///
/// The audio callback clones the `Arc` (one atomic increment, no lock, no
/// allocation) and the previous snapshot stays alive until that clone is
/// dropped, so a publish can never free a snapshot mid-block.
#[derive(Debug, Default)]
pub struct RoutingSnapshotPublisher {
    current: std::sync::Mutex<Arc<EngineAudioRoutingSnapshot>>,
    generation: std::sync::atomic::AtomicU64,
}

impl RoutingSnapshotPublisher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Next generation number. Monotonic across the process.
    pub fn next_generation(&self) -> u64 {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1
    }

    /// Publish from the control thread.
    pub fn publish(&self, snapshot: EngineAudioRoutingSnapshot) {
        let next = Arc::new(snapshot);
        if let Ok(mut guard) = self.current.lock() {
            *guard = next;
        }
    }

    /// Read the current snapshot from the control thread.
    pub fn current(&self) -> Arc<EngineAudioRoutingSnapshot> {
        self.current
            .lock()
            .map(|guard| Arc::clone(&guard))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_connections::{AudioConnection, ChannelLayout};

    fn device() -> AvailablePorts {
        AvailablePorts::for_device("dev-1", "System Audio Device", 4, 4)
    }

    fn registry() -> AudioConnectionRegistry {
        AudioConnectionRegistry::default_template(&device(), "dev-1")
    }

    fn input_id(registry: &AudioConnectionRegistry, name: &str) -> AudioConnectionId {
        registry
            .by_direction(AudioConnectionDirection::Input)
            .into_iter()
            .find(|c| c.name == name)
            .expect("input connection")
            .id
            .clone()
    }

    fn output_id(registry: &AudioConnectionRegistry, name: &str) -> AudioConnectionId {
        registry
            .by_direction(AudioConnectionDirection::Output)
            .into_iter()
            .find(|c| c.name == name)
            .expect("output connection")
            .id
            .clone()
    }

    #[test]
    fn a_mono_input_compiles_to_one_physical_channel() {
        let ports = device();
        let registry = registry();
        let request = RoutingCompileRequest {
            track_inputs: vec![Some(input_id(&registry, "Mono Input 1"))],
            ..Default::default()
        };
        let result = compile_routing(&registry, &ports, &request, 1);

        let source = result.snapshot.track_input(0).expect("resolved source");
        assert_eq!(source.channels.as_slice(), &[0]);
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn a_stereo_input_compiles_left_then_right() {
        let ports = device();
        let registry = registry();
        let request = RoutingCompileRequest {
            track_inputs: vec![Some(input_id(&registry, "Stereo Input 1-2"))],
            ..Default::default()
        };
        let result = compile_routing(&registry, &ports, &request, 1);
        assert_eq!(
            result.snapshot.track_input(0).unwrap().channels.as_slice(),
            &[0, 1],
            "channel order carries Left then Right"
        );
    }

    #[test]
    fn no_input_compiles_to_silence_without_a_warning() {
        let ports = device();
        let registry = registry();
        let request = RoutingCompileRequest {
            track_inputs: vec![None],
            ..Default::default()
        };
        let result = compile_routing(&registry, &ports, &request, 1);
        assert!(result.snapshot.track_input(0).is_none());
        assert!(result.warnings.is_empty());
    }

    /// A missing device must compile to silence — never to an unrelated
    /// physical channel — and must say so.
    #[test]
    fn a_missing_device_compiles_to_silence_and_warns() {
        let mut registry = registry();
        let id = input_id(&registry, "Mono Input 1");
        registry.revalidate(&AvailablePorts::default());

        let request = RoutingCompileRequest {
            track_inputs: vec![Some(id.clone())],
            ..Default::default()
        };
        let result = compile_routing(&registry, &AvailablePorts::default(), &request, 1);

        assert!(result.snapshot.track_input(0).is_none());
        assert_eq!(
            result.warnings,
            vec![RoutingWarning::TrackInputUnavailable {
                track_runtime_index: 0,
                connection: id
            }]
        );
    }

    #[test]
    fn an_unknown_connection_id_compiles_to_silence_and_warns() {
        let ports = device();
        let registry = registry();
        let request = RoutingCompileRequest {
            track_inputs: vec![Some(AudioConnectionId::from_stored("ac-ghost"))],
            ..Default::default()
        };
        let result = compile_routing(&registry, &ports, &request, 1);
        assert!(result.snapshot.track_input(0).is_none());
        assert_eq!(
            result.warnings,
            vec![RoutingWarning::UnknownTrackInput {
                track_runtime_index: 0
            }]
        );
    }

    #[test]
    fn using_an_output_connection_as_a_track_input_is_rejected() {
        let ports = device();
        let registry = registry();
        let id = output_id(&registry, "Main Output 1-2");
        let request = RoutingCompileRequest {
            track_inputs: vec![Some(id.clone())],
            ..Default::default()
        };
        let result = compile_routing(&registry, &ports, &request, 1);
        assert!(result.snapshot.track_input(0).is_none());
        assert_eq!(
            result.warnings,
            vec![RoutingWarning::DirectionMismatch { connection: id }]
        );
    }

    #[test]
    fn master_and_monitor_resolve_to_output_indexes() {
        let ports = device();
        let mut registry = registry();
        let headphones = registry.add(
            AudioConnection::new(
                "Headphones",
                AudioConnectionDirection::Output,
                ChannelLayout::Stereo,
            )
            .bind_consecutive("dev-1", 2, |i| format!("Output {}", i + 1)),
        );
        registry.revalidate(&ports);

        let request = RoutingCompileRequest {
            master_output: Some(output_id(&registry, "Main Output 1-2")),
            monitor_output: Some(headphones),
            ..Default::default()
        };
        let result = compile_routing(&registry, &ports, &request, 7);

        assert_eq!(result.snapshot.generation, 7);
        let master = result
            .snapshot
            .output(result.snapshot.master_output.unwrap())
            .unwrap();
        let monitor = result
            .snapshot
            .output(result.snapshot.monitor_output.unwrap())
            .unwrap();
        assert_eq!(master.channels.as_slice(), &[0, 1]);
        assert_eq!(monitor.channels.as_slice(), &[2, 3]);
        assert!(!result.snapshot.master_monitor_share_ports);
        assert!(result.warnings.is_empty());
    }

    /// Master and Monitor on the same pair is the duplicate-write case: the
    /// engine must be told so it does not sum the same mix twice.
    #[test]
    fn master_and_monitor_on_the_same_ports_flag_a_duplicate_write() {
        let ports = device();
        let registry = registry();
        let main = output_id(&registry, "Main Output 1-2");
        let request = RoutingCompileRequest {
            master_output: Some(main.clone()),
            monitor_output: Some(main),
            ..Default::default()
        };
        let result = compile_routing(&registry, &ports, &request, 1);

        assert!(result.snapshot.master_monitor_share_ports);
        assert!(result
            .warnings
            .contains(&RoutingWarning::DuplicateHardwareWrite));
        assert_eq!(
            result.snapshot.output_connections.len(),
            1,
            "an identical resolved output is reused, not duplicated"
        );
    }

    /// Two *different* output buses overlapping on a physical port are caught
    /// one layer earlier, by registry validation, so the compiler never sees a
    /// resolvable pair. It compiles the loser to silence and warns rather than
    /// emitting a route that would double-write the port.
    ///
    /// This is why `master_monitor_share_ports` only has to catch the case
    /// where both buses resolve to the *same* Active connection.
    #[test]
    fn overlapping_output_buses_are_rejected_before_they_can_double_write() {
        let ports = device();
        let mut registry = registry();
        let overlapping = registry.add(
            AudioConnection::new(
                "Offset Pair",
                AudioConnectionDirection::Output,
                ChannelLayout::Stereo,
            )
            .bind_consecutive("dev-1", 1, |i| format!("Output {}", i + 1)),
        );
        registry.revalidate(&ports);
        assert_eq!(
            registry.get(&overlapping).unwrap().status,
            crate::audio_connections::AudioConnectionStatus::Conflict,
            "sharing output channel 1 with Main Output 1-2 is a registry conflict"
        );

        let request = RoutingCompileRequest {
            master_output: Some(output_id(&registry, "Main Output 1-2")),
            monitor_output: Some(overlapping.clone()),
            ..Default::default()
        };
        let result = compile_routing(&registry, &ports, &request, 1);

        assert!(
            result.snapshot.monitor_output.is_none(),
            "a conflicted output must compile to silence, not to a shared port"
        );
        assert!(result
            .warnings
            .contains(&RoutingWarning::OutputUnavailable {
                connection: overlapping
            }));
    }

    #[test]
    fn devices_are_assigned_dense_runtime_indexes() {
        let ports = AvailablePorts::for_device("dev-1", "First", 2, 2)
            .merge(AvailablePorts::for_device("dev-2", "Second", 2, 2));
        let mut registry = AudioConnectionRegistry::new();
        let a = registry.add(
            AudioConnection::new("A", AudioConnectionDirection::Input, ChannelLayout::Mono)
                .bind_consecutive("dev-1", 0, |i| format!("Input {}", i + 1)),
        );
        let b = registry.add(
            AudioConnection::new("B", AudioConnectionDirection::Input, ChannelLayout::Mono)
                .bind_consecutive("dev-2", 0, |i| format!("Input {}", i + 1)),
        );
        registry.revalidate(&ports);

        let request = RoutingCompileRequest {
            track_inputs: vec![Some(a), Some(b)],
            ..Default::default()
        };
        let result = compile_routing(&registry, &ports, &request, 1);
        assert_eq!(
            result.snapshot.track_input(0).unwrap().device_runtime_id,
            RuntimeAudioDeviceId(0)
        );
        assert_eq!(
            result.snapshot.track_input(1).unwrap().device_runtime_id,
            RuntimeAudioDeviceId(1)
        );
    }

    /// The snapshot the callback reads must be plain integers — no strings, no
    /// maps, no ids to parse.
    #[test]
    fn the_snapshot_the_callback_reads_is_copyable_integer_data() {
        let ports = device();
        let registry = registry();
        let request = RoutingCompileRequest {
            track_inputs: vec![Some(input_id(&registry, "Stereo Input 1-2"))],
            master_output: Some(output_id(&registry, "Main Output 1-2")),
            ..Default::default()
        };
        let result = compile_routing(&registry, &ports, &request, 1);

        // `ResolvedInputSource` and `SmallChannelList` are `Copy`: reading one
        // in the callback cannot allocate or drop.
        fn assert_copy<T: Copy>(_: &T) {}
        let source = result.snapshot.track_input(0).unwrap();
        assert_copy(source);
        assert_copy(&source.channels);
        assert_copy(&source.device_runtime_id);
        assert_eq!(source.channels.len(), 2);
    }

    #[test]
    fn a_custom_layout_is_truncated_at_the_fixed_channel_capacity() {
        let wide: Vec<u32> = (0..32).collect();
        let list = SmallChannelList::from_slice(&wide);
        assert_eq!(list.len(), MAX_ROUTED_CHANNELS);
        assert_eq!(list.as_slice()[0], 0);
        assert_eq!(list.as_slice()[MAX_ROUTED_CHANNELS - 1], 7);
    }

    #[test]
    fn publishing_keeps_the_previous_snapshot_alive_for_its_readers() {
        let publisher = RoutingSnapshotPublisher::new();
        let first_generation = publisher.next_generation();
        publisher.publish(EngineAudioRoutingSnapshot {
            generation: first_generation,
            ..Default::default()
        });

        // A "callback" holding the old snapshot.
        let held = publisher.current();
        assert_eq!(held.generation, first_generation);

        let second_generation = publisher.next_generation();
        publisher.publish(EngineAudioRoutingSnapshot {
            generation: second_generation,
            ..Default::default()
        });

        assert_eq!(
            held.generation, first_generation,
            "the in-flight snapshot must survive the swap"
        );
        assert_eq!(publisher.current().generation, second_generation);
        assert!(second_generation > first_generation);
    }
}
