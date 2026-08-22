//! Durable tick watermarks for [`CronTrigger`](super::CronTrigger).
//!
//! A watermark records the most recent tick a schedule emitted. Without one a
//! [`CronTrigger`](super::CronTrigger) has no memory across process restarts: on resubscribe it
//! waits for the next future tick, and every tick that came due while the process was down is
//! lost with no record that it was skipped. Supplying a watermark is what lets
//! [`MissedTickPolicy`](crate::ambient::MissedTickPolicy) see that gap and act on it.

use std::path::{Path, PathBuf};

use adk_core::{AdkError, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// Records the most recent tick a schedule emitted, so a restarted process can tell which
/// scheduled runs it missed.
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
    /// Reads the most recently emitted tick, or `None` when the schedule has never fired.
    async fn read(&self) -> Result<Option<DateTime<Utc>>>;

    /// Records `tick` as the most recently emitted one.
    async fn write(&self, tick: DateTime<Utc>) -> Result<()>;
}

/// A [`TickWatermark`] backed by a single file holding one RFC 3339 timestamp.
///
/// Writes go to a sibling temporary file and are then renamed, so a crash or power loss
/// mid-write leaves the previous watermark intact rather than a truncated timestamp. Missing
/// parent directories are created on first write.
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
}

impl FileTickWatermark {
    /// Creates a watermark stored at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
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

            // Rename is atomic on the same filesystem, so a reader never observes a partial
            // timestamp.
            let temporary = path.with_extension("tick.tmp");
            std::fs::write(&temporary, encoded).map_err(|err| {
                AdkError::agent(format!(
                    "failed to write tick watermark to {}: {err}",
                    temporary.display()
                ))
            })?;
            std::fs::rename(&temporary, &path).map_err(|err| {
                AdkError::agent(format!(
                    "failed to publish tick watermark to {}: {err}",
                    path.display()
                ))
            })
        })
        .await
        .map_err(|err| AdkError::agent(format!("tick watermark write task failed: {err}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

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
