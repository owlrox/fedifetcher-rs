//! Reading and reporting the process's own memory usage, so the app can
//! say what it used when it goes away. Resident memory is what the OS
//! holds for the process right now; the peak is the most it ever held
//! (the number that shows a leak); the heap is what the allocator holds
//! on the program's behalf, counted by the global allocator below.
//! Virtual size is left out: it counts reserved address space, not use.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::VERSION;

/// How much room the database takes on disk: the main file, and the
/// write-ahead log when one exists.
pub struct DiskUsage {
    pub database: u64,
    pub wal: Option<u64>,
}

/// The process's memory, in bytes, as far as the platform reports it.
pub struct MemorySnapshot {
    pub resident: Option<u64>,
    pub peak_resident: Option<u64>,
    pub heap: Option<u64>,
    pub heap_peak: Option<u64>,
}

/// A one-line summary of the process's memory — and, when given, the
/// database's disk usage — for the exit log.
pub fn exit_summary(context: &str, disk: Option<&DiskUsage>) -> String {
    let memory = snapshot();
    let mut parts: Vec<String> = Vec::new();
    if let Some(bytes) = memory.resident {
        parts.push(format!("resident {}", human(bytes)));
    }
    if let Some(bytes) = memory.peak_resident {
        parts.push(format!("peak resident {}", human(bytes)));
    }
    if let Some(bytes) = memory.heap {
        match memory.heap_peak {
            Some(peak) => parts.push(format!("heap {} (peak {})", human(bytes), human(peak))),
            None => parts.push(format!("heap {}", human(bytes))),
        }
    }
    if let Some(disk) = disk {
        match disk.wal {
            Some(wal) => parts.push(format!(
                "database {} (wal {})",
                human(disk.database),
                human(wal)
            )),
            None => parts.push(format!("database {}", human(disk.database))),
        }
    }
    if parts.is_empty() {
        format!("FediFetcher-rs v{VERSION} {context}: memory usage unknown on this platform")
    } else {
        format!(
            "FediFetcher-rs v{VERSION} {context}: memory {}",
            parts.join(", ")
        )
    }
}

/// Bytes in the unit that fits: kB under a megabyte, MB under a gigabyte,
/// GB with one decimal above.
pub fn human(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let bytes = bytes as f64;
    if bytes >= GIB {
        format!("{:.1} GB", bytes / GIB)
    } else if bytes >= MIB {
        format!("{} MB", (bytes / MIB) as u64)
    } else {
        format!("{} kB", (bytes / KIB) as u64)
    }
}

/// The process's memory right now, as far as the platform reports it.
pub fn snapshot() -> MemorySnapshot {
    MemorySnapshot {
        resident: resident_bytes(),
        peak_resident: peak_resident_bytes(),
        heap: Some(heap_allocated()),
        heap_peak: Some(heap_peak()),
    }
}

/// The process's resident memory in bytes, when the platform reports it.
pub fn resident_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        linux_field("VmRSS:").map(|kb| kb * 1024)
    }
    #[cfg(target_os = "macos")]
    {
        macos_taskinfo().map(|info| info.pti_resident_size)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// The most resident memory the process has ever held.
fn peak_resident_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        linux_field("VmHWM:").map(|kb| kb * 1024)
    }
    #[cfg(target_os = "macos")]
    {
        // rusage's ru_maxrss is in bytes on macOS (kilobytes on Linux)
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        let ok = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } == 0;
        ok.then_some(usage.ru_maxrss as u64)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// One `name:  N kB` field out of /proc/self/status.
#[cfg(target_os = "linux")]
fn linux_field(name: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with(name))?;
    let kb: u64 = line[name.len()..].trim().split(' ').next()?.parse().ok()?;
    Some(kb)
}

/// The task's memory counters, through libproc.
#[cfg(target_os = "macos")]
fn macos_taskinfo() -> Option<libc::proc_taskinfo> {
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
    let got = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDTASKINFO,
            0,
            &mut info as *mut libc::proc_taskinfo as *mut libc::c_void,
            size,
        )
    };
    (got == size).then_some(info)
}

/// The bytes the program's own allocations hold right now.
pub fn heap_allocated() -> u64 {
    ALLOCATED.load(Ordering::Relaxed)
}

/// The most bytes the program's allocations have ever held.
pub fn heap_peak() -> u64 {
    PEAK_ALLOCATED.load(Ordering::Relaxed)
}

/// The system allocator, wrapped so every allocation is counted. The
/// counters cost two atomic operations per allocation; nothing else.
struct CountingAllocator;

static ALLOCATED: AtomicU64 = AtomicU64::new(0);
static PEAK_ALLOCATED: AtomicU64 = AtomicU64::new(0);

unsafe impl std::alloc::GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        let ptr = unsafe { std::alloc::System.alloc(layout) };
        if !ptr.is_null() {
            let now =
                ALLOCATED.fetch_add(layout.size() as u64, Ordering::Relaxed) + layout.size() as u64;
            bump_peak(now);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(ptr, layout) };
        ALLOCATED.fetch_sub(layout.size() as u64, Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        let ptr = unsafe { std::alloc::System.realloc(ptr, layout, new_size) };
        if !ptr.is_null() {
            let delta = new_size as i64 - layout.size() as i64;
            let now = ALLOCATED.fetch_add(delta as u64, Ordering::Relaxed);
            bump_peak((now as i64 + delta).max(0) as u64);
        }
        ptr
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

/// Raise the peak marker to `now` when it has grown past it.
fn bump_peak(now: u64) {
    let mut peak = PEAK_ALLOCATED.load(Ordering::Relaxed);
    while now > peak {
        match PEAK_ALLOCATED.compare_exchange_weak(peak, now, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => break,
            Err(seen) => peak = seen,
        }
    }
}

/// Log the process's memory usage — and the database's disk usage when
/// given — as an INFO line, so it reaches both stderr and the database
/// the logs page reads.
pub fn log_exit_usage(context: &str, disk: Option<&DiskUsage>) {
    tracing::info!("{}", exit_summary(context, disk));
}

#[cfg(test)]
mod tests {
    use super::{DiskUsage, exit_summary, human, resident_bytes, snapshot};

    #[test]
    fn the_process_knows_its_own_resident_memory() {
        let bytes = resident_bytes().expect("this platform reports the process's resident memory");
        assert!(
            bytes > 1_000_000,
            "a running Rust process holds more than a megabyte: {bytes}"
        );
    }

    #[test]
    fn the_snapshot_carries_the_memory_kinds_worth_reporting() {
        let memory = snapshot();
        let resident = memory.resident.expect("resident memory is reported");
        assert!(resident > 1_000_000, "resident is plausible: {resident}");
        let peak = memory.peak_resident.expect("peak resident is reported");
        assert!(peak >= resident, "the peak is at least the current: {peak}");
        let heap = memory.heap.expect("the heap is counted");
        assert!(heap > 0, "the process has allocated something: {heap}");
        assert!(memory.heap_peak.unwrap_or(0) >= heap);
    }

    #[test]
    fn the_exit_summary_names_every_memory_kind_it_reports() {
        let summary = exit_summary("shut down after SIGTERM", None);
        assert!(summary.starts_with(&format!(
            "FediFetcher-rs v{} shut down after SIGTERM",
            crate::VERSION
        )));
        assert!(summary.contains("resident"), "{summary}");
        assert!(summary.contains("peak resident"), "{summary}");
        assert!(summary.contains("heap"), "{summary}");
        assert!(
            !summary.contains("virtual"),
            "virtual is reservation, not use: {summary}"
        );
    }

    #[test]
    fn the_exit_summary_reports_the_database_when_asked() {
        let disk = DiskUsage {
            database: 12_582_912,
            wal: Some(3_145_728),
        };
        let summary = exit_summary("shut down after SIGTERM", Some(&disk));
        assert!(
            summary.contains("database 12 MB") && summary.contains("wal 3 MB"),
            "{summary}"
        );

        let disk = DiskUsage {
            database: 12_582_912,
            wal: None,
        };
        let summary = exit_summary("shut down after SIGTERM", Some(&disk));
        assert!(
            summary.contains("database 12 MB") && !summary.contains("wal"),
            "{summary}"
        );

        let summary = exit_summary("shut down after SIGTERM", None);
        assert!(!summary.contains("database"), "{summary}");
    }

    #[test]
    fn sizes_render_in_the_unit_that_fits() {
        assert_eq!(human(0), "0 kB");
        assert_eq!(human(500_000), "488 kB");
        assert_eq!(human(31_457_280), "30 MB");
        assert_eq!(human(401_353_000_000), "373.8 GB");
    }
}
