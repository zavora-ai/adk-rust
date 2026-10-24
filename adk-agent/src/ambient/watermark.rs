//! Durable tick watermarks for [`CronTrigger`](super::CronTrigger).
//!
//! A watermark records the most recent tick a schedule emitted. Without one a
//! [`CronTrigger`](super::CronTrigger) has no memory across process restarts: on resubscribe it
//! waits for the next future tick, and every tick that came due while the process was down is
//! lost with no record that it was skipped. Supplying a watermark is what lets
//! [`MissedTickPolicy`](crate::ambient::MissedTickPolicy) see that gap and act on it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use adk_core::{AdkError, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// Records the point through which a schedule has been accounted for, so a restarted process can
/// tell which scheduled runs it missed.
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use adk_agent::ambient::{CronTrigger, FileTickWatermark, MissedTickPolicy};
///
/// let watermark = Arc::new(FileTickWatermark::new("/var/lib/my-agent/sweep.tick"));
/// let trigger = CronTrigger::new("0 */5 * * * *")?
///     .with_missed_tick_policy(MissedTickPolicy::CoalesceOne)
///     .with_watermark(watermark);
/// ```
#[async_trait]
pub trait TickWatermark: Send + Sync {
    /// Reads the most recent accounted-through cursor, or `None` when the schedule has never run.
    async fn read(&self) -> Result<Option<DateTime<Utc>>>;

    /// Records `cursor` as the point through which the schedule has been accounted for.
    ///
    /// The cursor is normally a scheduled tick. It can be a wall-clock instant when a bounded
    /// catch-up intentionally discards the remainder of an older gap.
    async fn write(&self, cursor: DateTime<Utc>) -> Result<()>;
}

/// A [`TickWatermark`] backed by a single file holding one RFC 3339 timestamp.
///
/// Writes go to a unique sibling temporary file, are synchronized, and atomically replace the
/// destination, so a reader never observes a partial timestamp. Missing parent directories are
/// created on first write. Writes through one watermark and its clones run one at a time, and
/// the last one wins. On Windows, replacing the file while another process replaces it fails
/// for a moment, so a write retries for up to [`FileTickWatermark::PUBLISH_RETRY`] before it
/// reports the conflict.
///
/// # Example
///
/// ```rust,ignore
/// use adk_agent::ambient::FileTickWatermark;
///
/// let watermark = FileTickWatermark::new("/var/lib/my-agent/sweep.tick");
/// ```
#[derive(Debug, Clone)]
pub struct FileTickWatermark {
    path: PathBuf,
    // On Windows a replace of the file fails while another replace of it is in flight, so the
    // writes through this watermark and its clones take turns.
    writing: Arc<tokio::sync::Mutex<()>>,
}

impl FileTickWatermark {
    /// How long a write keeps retrying on Windows while another process replaces the same file.
    pub const PUBLISH_RETRY: Duration = Duration::from_secs(1);

    /// Creates a watermark stored at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), writing: Arc::new(tokio::sync::Mutex::new(())) }
    }

    /// The file backing this watermark.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[async_trait]
impl TickWatermark for FileTickWatermark {
    async fn read(&self) -> Result<Option<DateTime<Utc>>> {
        let path = self.path.clone();

        // tokio's `fs` feature is not enabled for this crate, and a watermark is a few bytes
        // read at most once per subscribe, so the blocking pool is the cheaper dependency.
        let raw = tokio::task::spawn_blocking(move || match std::fs::read_to_string(&path) {
            Ok(contents) => Ok(Some(contents)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(AdkError::agent(format!(
                "failed to read tick watermark at {}: {err}",
                path.display()
            ))),
        })
        .await
        .map_err(|err| AdkError::agent(format!("tick watermark read task failed: {err}")))??;

        let Some(raw) = raw else {
            return Ok(None);
        };

        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }

        let parsed = DateTime::parse_from_rfc3339(trimmed).map_err(|err| {
            AdkError::agent(format!(
                "tick watermark at {} is not an RFC 3339 timestamp: {err}",
                self.path.display()
            ))
        })?;

        Ok(Some(parsed.with_timezone(&Utc)))
    }

    async fn write(&self, tick: DateTime<Utc>) -> Result<()> {
        let path = self.path.clone();
        let encoded = tick.to_rfc3339();
        let _turn = self.writing.lock().await;

        tokio::task::spawn_blocking(move || {
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent).map_err(|err| {
                    AdkError::agent(format!(
                        "failed to create tick watermark directory {}: {err}",
                        parent.display()
                    ))
                })?;
            }

            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|err| {
                AdkError::agent(format!(
                    "failed to create a temporary tick watermark beside {}: {err}",
                    path.display()
                ))
            })?;
            temporary.write_all(encoded.as_bytes()).map_err(|err| {
                AdkError::agent(format!(
                    "failed to write tick watermark for {}: {err}",
                    path.display()
                ))
            })?;
            temporary.as_file().sync_all().map_err(|err| {
                AdkError::agent(format!(
                    "failed to synchronize tick watermark for {}: {err}",
                    path.display()
                ))
            })?;
            publish(temporary, &path)?;

            #[cfg(unix)]
            std::fs::File::open(parent).and_then(|directory| directory.sync_all()).map_err(
                |err| {
                    AdkError::agent(format!(
                        "failed to synchronize tick watermark directory {}: {err}",
                        parent.display()
                    ))
                },
            )?;

            Ok(())
        })
        .await
        .map_err(|err| AdkError::agent(format!("tick watermark write task failed: {err}")))?
    }
}

/// Replaces `path` with `temporary`. On Windows the replace fails with `PermissionDenied`, or
/// a sharing violation, while another process replaces or holds the file, so it is retried
/// with a doubling pause for up to [`FileTickWatermark::PUBLISH_RETRY`].
fn publish(mut temporary: tempfile::NamedTempFile, path: &Path) -> Result<()> {
    let started = std::time::Instant::now();
    let mut pause = Duration::from_millis(10);
    loop {
        let error = match temporary.persist(path) {
            Ok(_) => return Ok(()),
            Err(error) => error,
        };
        // ERROR_SHARING_VIOLATION: another process has the file open without sharing it.
        let held = cfg!(windows)
            && (error.error.kind() == std::io::ErrorKind::PermissionDenied
                || error.error.raw_os_error() == Some(32));
        if held && started.elapsed() + pause <= FileTickWatermark::PUBLISH_RETRY {
            std::thread::sleep(pause);
            pause = (pause * 2).min(Duration::from_millis(200));
            temporary = error.file;
            continue;
        }
        let remedy = if held {
            format!(
                "; another process kept it for {:?}: give each schedule its own watermark file",
                FileTickWatermark::PUBLISH_RETRY
            )
        } else {
            String::new()
        };
        return Err(AdkError::agent(format!(
            "failed to publish tick watermark to {}: {}{remedy}",
            path.display(),
            error.error
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::sync::Arc;

    #[tokio::test]
    async fn read_returns_none_when_the_file_is_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let watermark = FileTickWatermark::new(dir.path().join("missing.tick"));

        assert_eq!(watermark.read().await.expect("read"), None);
    }

    #[tokio::test]
    async fn write_then_read_round_trips_the_tick() {
        let dir = tempfile::tempdir().expect("tempdir");
        let watermark = FileTickWatermark::new(dir.path().join("sweep.tick"));
        let tick = Utc.with_ymd_and_hms(2026, 8, 22, 13, 45, 0).unwrap();

        watermark.write(tick).await.expect("write");

        assert_eq!(watermark.read().await.expect("read"), Some(tick));
    }

    #[tokio::test]
    async fn write_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let watermark = FileTickWatermark::new(dir.path().join("nested/deeper/sweep.tick"));
        let tick = Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap();

        watermark.write(tick).await.expect("write");

        assert_eq!(watermark.read().await.expect("read"), Some(tick));
    }

    #[tokio::test]
    async fn write_overwrites_a_previous_tick() {
        let dir = tempfile::tempdir().expect("tempdir");
        let watermark = FileTickWatermark::new(dir.path().join("sweep.tick"));
        let first = Utc.with_ymd_and_hms(2026, 8, 22, 10, 0, 0).unwrap();
        let second = Utc.with_ymd_and_hms(2026, 8, 22, 11, 0, 0).unwrap();

        watermark.write(first).await.expect("first write");
        watermark.write(second).await.expect("second write");

        assert_eq!(watermark.read().await.expect("read"), Some(second));
    }

    #[tokio::test]
    async fn concurrent_writes_use_independent_temporary_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let watermark = Arc::new(FileTickWatermark::new(dir.path().join("sweep.tick")));
        let first = Utc.with_ymd_and_hms(2026, 8, 22, 10, 0, 0).unwrap();
        let second = Utc.with_ymd_and_hms(2026, 8, 22, 11, 0, 0).unwrap();

        let (first_result, second_result) =
            tokio::join!(watermark.write(first), watermark.write(second));
        first_result.expect("first write");
        second_result.expect("second write");

        let stored = watermark.read().await.expect("read").expect("stored");
        assert!(stored == first || stored == second);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn many_concurrent_writes_through_clones_all_land() {
        let dir = tempfile::tempdir().expect("tempdir");
        let watermark = FileTickWatermark::new(dir.path().join("sweep.tick"));
        let ticks: Vec<_> =
            (0..16).map(|hour| Utc.with_ymd_and_hms(2026, 8, 22, hour, 0, 0).unwrap()).collect();

        let writes = ticks.iter().map(|tick| {
            let watermark = watermark.clone();
            let tick = *tick;
            tokio::spawn(async move { watermark.write(tick).await })
        });
        for write in futures::future::join_all(writes).await {
            write.expect("write task").expect("write");
        }

        let stored = watermark.read().await.expect("read").expect("stored");
        assert!(ticks.contains(&stored), "{stored}");
    }

    /// Another process holding the file without sharing it: the write waits it out.
    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_write_waits_out_another_process_holding_the_file() {
        use std::os::windows::fs::OpenOptionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sweep.tick");
        std::fs::write(&path, "2026-08-22T10:00:00+00:00").expect("seed");
        let tick = Utc.with_ymd_and_hms(2026, 8, 22, 11, 0, 0).unwrap();

        let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(&path).expect("hold");
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(held);
        });
        FileTickWatermark::new(&path).write(tick).await.expect("write after the release");
        release.join().unwrap();
        assert_eq!(FileTickWatermark::new(&path).read().await.expect("read"), Some(tick));

        // Held past the retry, the write fails and says why.
        let _held = std::fs::OpenOptions::new().read(true).share_mode(0).open(&path).expect("hold");
        let started = std::time::Instant::now();
        let error = FileTickWatermark::new(&path).write(tick).await.unwrap_err();
        assert!(started.elapsed() < FileTickWatermark::PUBLISH_RETRY * 3, "the retry is bounded");
        assert!(error.to_string().contains("its own watermark file"), "{error}");
    }

    #[tokio::test]
    async fn read_treats_an_empty_file_as_no_watermark() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("empty.tick");
        std::fs::write(&path, "   \n").expect("seed");
        let watermark = FileTickWatermark::new(path);

        assert_eq!(watermark.read().await.expect("read"), None);
    }

    #[tokio::test]
    async fn read_rejects_a_corrupt_watermark() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("corrupt.tick");
        std::fs::write(&path, "not-a-timestamp").expect("seed");
        let watermark = FileTickWatermark::new(path);

        assert!(watermark.read().await.is_err());
    }
}
