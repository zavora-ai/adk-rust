//! Platform-specific memory (RSS) sampling.
//!
//! Uses `/proc/self/statm` on Linux and `mach_task_basic_info` on macOS.
//! Returns `BenchError::MemoryUnavailable` on unsupported platforms.
//!
//! ## Platform Notes
//!
//! - **Linux** (`/proc/self/statm`): The authoritative platform for published
//!   cross-framework memory comparisons due to consistent RSS reporting.
//! - **macOS** (`mach_task_basic_info`): Informational — cross-platform
//!   comparison is approximate.
//!
//! ## Example
//!
//! ```rust,ignore
//! use adk_bench::memory::{current_rss_bytes, spawn_memory_sampler};
//! use std::time::Duration;
//!
//! // Single sample
//! let rss = current_rss_bytes()?;
//! println!("Current RSS: {} bytes", rss);
//!
//! // Background sampler
//! let (handle, samples) = spawn_memory_sampler(Duration::from_millis(100));
//! // ... run workload ...
//! handle.abort();
//! let collected = samples.lock().await;
//! println!("Peak RSS: {} bytes", collected.iter().max().unwrap_or(&0));
//! ```

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::task::JoinHandle;

/// Returns the current process RSS (resident set size) in bytes.
///
/// Uses platform-specific APIs:
/// - **Linux**: reads `/proc/self/statm`, parses the second field (RSS in pages),
///   and multiplies by the system page size.
/// - **macOS**: uses `mach_task_basic_info` via `libc::task_info` to obtain
///   `resident_size`.
///
/// # Errors
///
/// Returns `BenchError::MemoryUnavailable` if:
/// - The platform is not Linux or macOS
/// - The platform-specific API call fails
pub fn current_rss_bytes() -> crate::Result<u64> {
    #[cfg(target_os = "linux")]
    {
        linux_rss()
    }
    #[cfg(target_os = "macos")]
    {
        macos_rss()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(crate::BenchError::MemoryUnavailable(
            "memory sampling not supported on this platform".to_string(),
        ))
    }
}

#[cfg(target_os = "linux")]
fn linux_rss() -> crate::Result<u64> {
    use std::fs;

    let statm = fs::read_to_string("/proc/self/statm").map_err(|e| {
        crate::BenchError::MemoryUnavailable(format!("failed to read /proc/self/statm: {e}"))
    })?;

    let rss_pages: u64 =
        statm.split_whitespace().nth(1).and_then(|s| s.parse().ok()).ok_or_else(|| {
            crate::BenchError::MemoryUnavailable(
                "failed to parse RSS from /proc/self/statm".to_string(),
            )
        })?;

    // SAFETY: sysconf with _SC_PAGESIZE is always safe and returns the system page size.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;

    Ok(rss_pages * page_size)
}

#[cfg(target_os = "macos")]
fn macos_rss() -> crate::Result<u64> {
    use std::mem;

    // SAFETY: We are calling mach kernel APIs through libc bindings.
    // - mach_task_self() returns the current task port (always valid).
    // - task_info() fills the info struct with task statistics.
    // - We use the pre-computed MACH_TASK_BASIC_INFO_COUNT from libc.
    unsafe {
        let mut info: libc::mach_task_basic_info_data_t = mem::zeroed();
        let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;

        #[allow(deprecated)]
        // mach_task_self is deprecated in favor of mach2 crate, but we use libc
        let task_self = libc::mach_task_self();

        let ret = libc::task_info(
            task_self,
            libc::MACH_TASK_BASIC_INFO,
            (&raw mut info) as libc::task_info_t,
            &raw mut count,
        );

        if ret != libc::KERN_SUCCESS {
            return Err(crate::BenchError::MemoryUnavailable(format!(
                "task_info(MACH_TASK_BASIC_INFO) failed with kern_return_t={ret}"
            )));
        }

        Ok(info.resident_size)
    }
}

/// Spawns a background Tokio task that samples RSS at the given interval.
///
/// The sampler loops indefinitely: sample RSS → push to shared vec → sleep.
/// To stop sampling, abort the returned `JoinHandle`.
///
/// # Arguments
///
/// * `interval` - Duration between consecutive RSS samples.
///
/// # Returns
///
/// A tuple of:
/// - `JoinHandle<()>` — the spawned background task handle (abort to stop)
/// - `Arc<Mutex<Vec<u64>>>` — shared vector of RSS samples in bytes
///
/// # Example
///
/// ```rust,ignore
/// let (handle, samples) = spawn_memory_sampler(Duration::from_millis(100));
/// // ... run benchmark workload ...
/// handle.abort();
/// let data = samples.lock().await;
/// let peak = data.iter().copied().max().unwrap_or(0);
/// ```
pub fn spawn_memory_sampler(interval: Duration) -> (JoinHandle<()>, Arc<Mutex<Vec<u64>>>) {
    let samples = Arc::new(Mutex::new(Vec::new()));
    let samples_clone = samples.clone();

    let handle = tokio::spawn(async move {
        loop {
            if let Ok(rss) = current_rss_bytes() {
                samples_clone.lock().await.push(rss);
            }
            tokio::time::sleep(interval).await;
        }
    });

    (handle, samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn test_current_rss_bytes_returns_positive() {
        // On supported platforms (Linux/macOS), RSS should be > 0.
        let rss = current_rss_bytes().expect("RSS sampling should succeed on this platform");
        assert!(rss > 0, "RSS must be positive, got {rss}");
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn test_current_rss_bytes_reasonable_range() {
        // A running Rust test binary should use at least 1 MB and less than 64 GB.
        let rss = current_rss_bytes().expect("RSS sampling should succeed");
        assert!(rss >= 1_000_000, "RSS should be at least 1 MB, got {rss} bytes");
        assert!(rss < 64 * 1024 * 1024 * 1024, "RSS should be less than 64 GB, got {rss} bytes");
    }

    #[test]
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn test_current_rss_bytes_unsupported_platform() {
        let result = current_rss_bytes();
        assert!(result.is_err());
    }

    #[tokio::test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    async fn test_spawn_memory_sampler_collects_samples() {
        let interval = Duration::from_millis(10);
        let (handle, samples) = spawn_memory_sampler(interval);

        // Let the sampler run for a bit
        tokio::time::sleep(Duration::from_millis(60)).await;
        handle.abort();

        let data = samples.lock().await;
        assert!(!data.is_empty(), "sampler should have collected at least one sample");
        // All samples should be positive
        for &sample in data.iter() {
            assert!(sample > 0, "each RSS sample must be positive");
        }
    }

    #[tokio::test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    async fn test_spawn_memory_sampler_abort_stops_collection() {
        let interval = Duration::from_millis(10);
        let (handle, samples) = spawn_memory_sampler(interval);

        tokio::time::sleep(Duration::from_millis(30)).await;
        handle.abort();

        // Wait a bit more and verify no new samples appear
        let count_after_abort = samples.lock().await.len();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let count_later = samples.lock().await.len();

        assert_eq!(count_after_abort, count_later, "no new samples should appear after abort");
    }
}
