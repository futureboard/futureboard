//! Whole-machine load — CPU per core, physical memory, and every local drive.
//!
//! This is the "is the machine keeping up" half of the Performance Monitor.
//! [`crate::perf::resource_usage`] answers what *Futureboard* costs; this
//! answers what is left, which is the question a dropout actually turns on: a
//! take that glitches while Studio sits at 12% is a pinned core or a saturated
//! disk somewhere else on the machine, and a readout that only shows the DAW
//! cannot say so.
//!
//! # Threading
//!
//! Every reading here is a syscall, and two of them — opening a volume, asking
//! a drive for its free space — can block for seconds on a sleeping external
//! disk or a dropped network share. So nothing is sampled on the caller's
//! thread: one background worker takes a reading a second and publishes it, and
//! [`system_load`] hands back the latest published snapshot. The monitor window
//! repaints at its own rate against whatever is there, and a slow drive costs a
//! stale number rather than a frozen UI.
//!
//! Network and CD-ROM volumes are skipped outright. A DAW cares about the
//! disks its audio streams from, and a disconnected mapped drive is the exact
//! case that hangs for the longest.

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// How often the worker takes a reading. CPU and disk throughput are rates —
/// they only exist as a difference between two samples — and a second is short
/// enough to see a stream start while being long enough not to read as noise.
const POLL: Duration = Duration::from_secs(1);

/// One logical processor's share of the last window.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CoreLoad {
    /// Busy time as a percentage of this core's wall clock, 0..100.
    pub busy_percent: f32,
    /// Kernel time inside `busy_percent`. A DAW's disk and device work lands
    /// here, so a core that is 90% busy and 70% kernel is a driver problem, not
    /// a plug-in one — and the two are indistinguishable without the split.
    pub kernel_percent: f32,
}

/// Physical memory, machine-wide.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MemoryLoad {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

impl MemoryLoad {
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.available_bytes)
    }

    pub fn used_fraction(&self) -> f32 {
        if self.total_bytes == 0 {
            return 0.0;
        }
        (self.used_bytes() as f64 / self.total_bytes as f64).clamp(0.0, 1.0) as f32
    }
}

/// One local volume: how full it is, and how hard it is being worked.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DriveLoad {
    /// Root path as the OS names it — `C:\` on Windows.
    pub root: String,
    /// Volume label, or empty when the volume has none.
    pub label: String,
    /// `NTFS`, `exFAT`, … Empty when it could not be read.
    pub filesystem: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    /// Bytes per second through this volume over the last window. `None` until
    /// there are two readings, and on a volume that does not report counters —
    /// which is not the same as an idle disk and must not print as one.
    pub read_bytes_per_sec: Option<f64>,
    pub write_bytes_per_sec: Option<f64>,
    /// Fraction of the window the disk was not idle, 0..1, when reported.
    pub busy_fraction: Option<f32>,
}

impl DriveLoad {
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.free_bytes)
    }

    pub fn used_fraction(&self) -> f32 {
        if self.total_bytes == 0 {
            return 0.0;
        }
        (self.used_bytes() as f64 / self.total_bytes as f64).clamp(0.0, 1.0) as f32
    }
}

/// The machine's load as of the last published reading.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SystemLoad {
    /// One entry per logical processor, in the order the OS reports them.
    pub cores: Vec<CoreLoad>,
    pub memory: MemoryLoad,
    pub drives: Vec<DriveLoad>,
    /// False before the first reading lands, and on any platform with no
    /// reader. An unmeasured machine is not an idle one.
    pub known: bool,
}

impl SystemLoad {
    /// Mean busy percentage across every core, 0..100.
    pub fn cpu_percent(&self) -> f32 {
        if self.cores.is_empty() {
            return 0.0;
        }
        self.cores.iter().map(|c| c.busy_percent).sum::<f32>() / self.cores.len() as f32
    }

    /// The busiest single core. A DAW's realtime thread lives on one core, so
    /// this is the number that predicts a dropout — the average hides it.
    pub fn peak_core_percent(&self) -> f32 {
        self.cores
            .iter()
            .map(|c| c.busy_percent)
            .fold(0.0_f32, f32::max)
    }
}

/// The most recent published reading. Cheap: a clone of a small snapshot.
///
/// Starts the background sampler on first call, so a session that never opens
/// the Performance Monitor never runs it. The first call returns an unknown
/// load — there has been no time to take two readings yet.
pub fn system_load() -> SystemLoad {
    static LATEST: OnceLock<Mutex<SystemLoad>> = OnceLock::new();
    let latest = LATEST.get_or_init(|| {
        let cell: Mutex<SystemLoad> = Mutex::new(SystemLoad::default());
        cell
    });
    start_sampler(latest);
    latest.lock().map(|load| load.clone()).unwrap_or_default()
}

fn start_sampler(latest: &'static Mutex<SystemLoad>) {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        // Named so it is identifiable in a profiler or a hang dump — this is
        // the thread that will be parked inside a drive query when an external
        // disk spins up.
        let _ = std::thread::Builder::new()
            .name("fb-system-load".into())
            .spawn(move || {
                let mut sampler = platform::Sampler::default();
                loop {
                    let load = sampler.sample();
                    if let Ok(mut slot) = latest.lock() {
                        *slot = load;
                    }
                    std::thread::sleep(POLL);
                }
            });
    });
}

// ── Windows ───────────────────────────────────────────────────────────────────

#[cfg(windows)]
mod platform {
    use super::{CoreLoad, DriveLoad, MemoryLoad, SystemLoad};
    use std::time::Instant;

    /// Raw per-core times from one reading, in 100 ns units.
    #[derive(Clone, Copy, Default)]
    struct CoreTimes {
        idle: i64,
        kernel: i64,
        user: i64,
    }

    /// Raw volume counters from one reading.
    #[derive(Clone, Copy, Default)]
    struct DriveCounters {
        read_bytes: i64,
        write_bytes: i64,
        idle_time: i64,
    }

    #[derive(Default)]
    pub(super) struct Sampler {
        previous_cores: Vec<CoreTimes>,
        previous_drives: Vec<(String, DriveCounters)>,
        previous_at: Option<Instant>,
    }

    impl Sampler {
        pub(super) fn sample(&mut self) -> SystemLoad {
            let now = Instant::now();
            let elapsed = self
                .previous_at
                .map(|at| now.duration_since(at).as_secs_f64())
                .filter(|seconds| *seconds > 0.0);
            self.previous_at = Some(now);

            let cores = self.sample_cores();
            let drives = self.sample_drives(elapsed);

            SystemLoad {
                cores,
                memory: read_memory(),
                drives,
                known: true,
            }
        }

        fn sample_cores(&mut self) -> Vec<CoreLoad> {
            let current = read_core_times();
            if current.is_empty() {
                return Vec::new();
            }
            let mut cores = Vec::with_capacity(current.len());
            for (index, now) in current.iter().enumerate() {
                let Some(before) = self.previous_cores.get(index) else {
                    // No prior reading: report zero rather than inventing one
                    // from absolute counters, which would read as the machine's
                    // whole uptime compressed into one second.
                    cores.push(CoreLoad::default());
                    continue;
                };
                // `KernelTime` includes idle, which is what makes the naive
                // "kernel + user" both wrong and always near 100%.
                let idle = (now.idle - before.idle).max(0) as f64;
                let kernel = (now.kernel - before.kernel).max(0) as f64;
                let user = (now.user - before.user).max(0) as f64;
                let total = kernel + user;
                if total <= 0.0 {
                    cores.push(CoreLoad::default());
                    continue;
                }
                let busy = (total - idle).max(0.0);
                let kernel_busy = (kernel - idle).max(0.0);
                cores.push(CoreLoad {
                    busy_percent: ((busy / total) * 100.0).clamp(0.0, 100.0) as f32,
                    kernel_percent: ((kernel_busy / total) * 100.0).clamp(0.0, 100.0) as f32,
                });
            }
            self.previous_cores = current;
            cores
        }

        fn sample_drives(&mut self, elapsed: Option<f64>) -> Vec<DriveLoad> {
            let mut drives = Vec::new();
            let mut counters = Vec::new();
            for root in local_drive_roots() {
                let (label, filesystem) = read_volume_identity(&root);
                let (total_bytes, free_bytes) = read_volume_space(&root);
                let mut drive = DriveLoad {
                    root: root.clone(),
                    label,
                    filesystem,
                    total_bytes,
                    free_bytes,
                    read_bytes_per_sec: None,
                    write_bytes_per_sec: None,
                    busy_fraction: None,
                };
                if let Some(now) = read_drive_counters(&root) {
                    let before = self
                        .previous_drives
                        .iter()
                        .find(|(previous_root, _)| *previous_root == root)
                        .map(|(_, counters)| *counters);
                    if let (Some(before), Some(elapsed)) = (before, elapsed) {
                        // A counter that went backwards means the volume was
                        // remounted, not negative traffic.
                        let read = (now.read_bytes - before.read_bytes).max(0) as f64;
                        let written = (now.write_bytes - before.write_bytes).max(0) as f64;
                        let idle = (now.idle_time - before.idle_time).max(0) as f64;
                        drive.read_bytes_per_sec = Some(read / elapsed);
                        drive.write_bytes_per_sec = Some(written / elapsed);
                        // Idle time is in 100 ns units against wall clock.
                        let idle_fraction = idle / (elapsed * 1.0e7);
                        drive.busy_fraction = Some((1.0 - idle_fraction).clamp(0.0, 1.0) as f32);
                    }
                    counters.push((root, now));
                }
                drives.push(drive);
            }
            self.previous_drives = counters;
            drives
        }
    }

    /// `SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION`. Declared here because the
    /// binding crate does not carry it; the layout is part of the NT ABI and
    /// has not changed since it was documented.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct ProcessorPerformance {
        idle_time: i64,
        kernel_time: i64,
        user_time: i64,
        dpc_time: i64,
        interrupt_time: i64,
        interrupt_count: u32,
        _padding: u32,
    }

    /// `SystemProcessorPerformanceInformation`.
    const SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION: i32 = 8;

    fn read_core_times() -> Vec<CoreTimes> {
        use windows::Wdk::System::SystemInformation::{
            NtQuerySystemInformation, SYSTEM_INFORMATION_CLASS,
        };

        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .max(1);
        let mut buffer = vec![ProcessorPerformance::default(); cores];
        let bytes = (std::mem::size_of::<ProcessorPerformance>() * cores) as u32;
        let mut written = 0u32;
        let status = unsafe {
            NtQuerySystemInformation(
                SYSTEM_INFORMATION_CLASS(SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION),
                buffer.as_mut_ptr().cast(),
                bytes,
                &mut written,
            )
        };
        if status.is_err() {
            return Vec::new();
        }
        let reported = (written as usize) / std::mem::size_of::<ProcessorPerformance>();
        buffer
            .into_iter()
            .take(reported.min(cores))
            .map(|entry| CoreTimes {
                idle: entry.idle_time,
                kernel: entry.kernel_time,
                user: entry.user_time,
            })
            .collect()
    }

    fn read_memory() -> MemoryLoad {
        use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

        let mut status = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        if unsafe { GlobalMemoryStatusEx(&mut status) }.is_err() {
            return MemoryLoad::default();
        }
        MemoryLoad {
            total_bytes: status.ullTotalPhys,
            available_bytes: status.ullAvailPhys,
        }
    }

    /// Roots of the volumes worth watching: fixed, removable and RAM disks.
    ///
    /// Network and optical drives are deliberately absent — a disconnected
    /// mapped drive is the one query that reliably blocks for seconds, and a
    /// DAW does not stream takes off a CD.
    fn local_drive_roots() -> Vec<String> {
        use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};

        // `GetDriveTypeW` return values. Not re-exported by the binding crate
        // at this version; the numbers are part of the documented Win32 ABI.
        const DRIVE_REMOVABLE: u32 = 2;
        const DRIVE_FIXED: u32 = 3;
        const DRIVE_RAMDISK: u32 = 6;

        let mask = unsafe { GetLogicalDrives() };
        let mut roots = Vec::new();
        for bit in 0..26u32 {
            if mask & (1 << bit) == 0 {
                continue;
            }
            let letter = char::from(b'A' + bit as u8);
            let root = format!("{letter}:\\");
            let wide = wide(&root);
            let kind = unsafe { GetDriveTypeW(windows::core::PCWSTR(wide.as_ptr())) };
            if kind == DRIVE_FIXED || kind == DRIVE_REMOVABLE || kind == DRIVE_RAMDISK {
                roots.push(root);
            }
        }
        roots
    }

    fn read_volume_identity(root: &str) -> (String, String) {
        use windows::Win32::Storage::FileSystem::GetVolumeInformationW;

        let wide_root = wide(root);
        let mut label = [0u16; 256];
        let mut filesystem = [0u16; 64];
        let ok = unsafe {
            GetVolumeInformationW(
                windows::core::PCWSTR(wide_root.as_ptr()),
                Some(&mut label),
                None,
                None,
                None,
                Some(&mut filesystem),
            )
        };
        if ok.is_err() {
            return (String::new(), String::new());
        }
        (from_wide(&label), from_wide(&filesystem))
    }

    fn read_volume_space(root: &str) -> (u64, u64) {
        use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

        let wide_root = wide(root);
        let mut free_to_caller = 0u64;
        let mut total = 0u64;
        let mut total_free = 0u64;
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                windows::core::PCWSTR(wide_root.as_ptr()),
                Some(&mut free_to_caller),
                Some(&mut total),
                Some(&mut total_free),
            )
        };
        if ok.is_err() {
            return (0, 0);
        }
        (total, total_free)
    }

    /// `DISK_PERFORMANCE`, as returned by `IOCTL_DISK_PERFORMANCE`. Declared
    /// locally for the same reason as [`ProcessorPerformance`].
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct DiskPerformance {
        bytes_read: i64,
        bytes_written: i64,
        read_time: i64,
        write_time: i64,
        idle_time: i64,
        read_count: u32,
        write_count: u32,
        queue_depth: u32,
        split_count: u32,
        query_time: i64,
        storage_device_number: u32,
        storage_manager_name: [u16; 8],
    }

    const IOCTL_DISK_PERFORMANCE: u32 = 0x0007_0020;

    /// Per-volume byte counters, or `None` when the volume does not report them.
    ///
    /// The volume is opened for *no* access at all (`dwDesiredAccess = 0`),
    /// which is enough to send a query IOCTL and is why this needs no elevation.
    fn read_drive_counters(root: &str) -> Option<DriveCounters> {
        use windows::Win32::Foundation::{CloseHandle, GENERIC_ACCESS_RIGHTS};
        use windows::Win32::Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        };
        use windows::Win32::System::IO::DeviceIoControl;

        // `\\.\C:` — the volume device, not the filesystem root.
        let letter = root.chars().next()?;
        let device = wide(&format!("\\\\.\\{letter}:"));
        let handle = unsafe {
            CreateFileW(
                windows::core::PCWSTR(device.as_ptr()),
                GENERIC_ACCESS_RIGHTS(0).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }
        .ok()?;

        let mut performance = DiskPerformance::default();
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                handle,
                IOCTL_DISK_PERFORMANCE,
                None,
                0,
                Some((&mut performance as *mut DiskPerformance).cast()),
                std::mem::size_of::<DiskPerformance>() as u32,
                Some(&mut returned),
                None,
            )
        };
        unsafe {
            let _ = CloseHandle(handle);
        }
        ok.ok()?;
        Some(DriveCounters {
            read_bytes: performance.bytes_read,
            write_bytes: performance.bytes_written,
            idle_time: performance.idle_time,
        })
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn from_wide(buffer: &[u16]) -> String {
        let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
        String::from_utf16_lossy(&buffer[..end]).trim().to_string()
    }
}

// ── Everything else ───────────────────────────────────────────────────────────

#[cfg(not(windows))]
mod platform {
    use super::SystemLoad;

    /// No reader on this platform. [`SystemLoad::known`] stays false, and the
    /// monitor says "unavailable" rather than drawing zeroes.
    #[derive(Default)]
    pub(super) struct Sampler;

    impl Sampler {
        pub(super) fn sample(&mut self) -> SystemLoad {
            SystemLoad::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unmeasured_machine_is_not_an_idle_one() {
        let load = SystemLoad::default();
        assert!(!load.known);
        assert_eq!(load.cpu_percent(), 0.0);
        assert_eq!(load.peak_core_percent(), 0.0);
    }

    /// The average hides the one thing that predicts a dropout: a DAW's
    /// realtime thread lives on a single core, so a machine at 25% average with
    /// one core pinned is a machine about to glitch.
    #[test]
    fn the_peak_core_is_reported_separately_from_the_average() {
        let load = SystemLoad {
            cores: vec![
                CoreLoad {
                    busy_percent: 100.0,
                    kernel_percent: 10.0,
                },
                CoreLoad::default(),
                CoreLoad::default(),
                CoreLoad::default(),
            ],
            known: true,
            ..Default::default()
        };
        assert_eq!(load.cpu_percent(), 25.0);
        assert_eq!(load.peak_core_percent(), 100.0);
    }

    #[test]
    fn memory_and_drive_fractions_are_used_not_free() {
        let memory = MemoryLoad {
            total_bytes: 32 * 1024 * 1024 * 1024,
            available_bytes: 8 * 1024 * 1024 * 1024,
        };
        assert_eq!(memory.used_bytes(), 24 * 1024 * 1024 * 1024);
        assert!((memory.used_fraction() - 0.75).abs() < 1.0e-6);

        let drive = DriveLoad {
            total_bytes: 1000,
            free_bytes: 250,
            ..Default::default()
        };
        assert_eq!(drive.used_bytes(), 750);
        assert!((drive.used_fraction() - 0.75).abs() < 1.0e-6);
    }

    /// A volume with no capacity reading must not divide by zero or claim to be
    /// completely full.
    #[test]
    fn an_unreadable_volume_reports_nothing_rather_than_full() {
        let drive = DriveLoad::default();
        assert_eq!(drive.used_fraction(), 0.0);
        assert_eq!(MemoryLoad::default().used_fraction(), 0.0);
    }
}
