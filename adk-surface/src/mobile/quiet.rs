//! When a wake-up waits for quiet time to end (spec 3.3 §5.4, §8.1 "Quiet time").

use adk_kernel::{Kernel, PartitionId, QuietHours};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// The end of the quiet time holding a moment.
#[async_trait]
pub(crate) trait QuietTime: Send + Sync {
    /// The end of the quiet time that holds `at` for the run `run_id` (its process's area),
    /// or for the default area without one; `None` when `at` is not quiet.
    async fn until(&self, run_id: Option<&str>, at: DateTime<Utc>) -> Option<DateTime<Utc>>;
}

/// Fixed quiet windows, the same for every run.
#[derive(Debug, Clone, Default)]
pub(crate) struct Windows(pub(crate) Vec<QuietHours>);

#[async_trait]
impl QuietTime for Windows {
    async fn until(&self, _run_id: Option<&str>, at: DateTime<Utc>) -> Option<DateTime<Utc>> {
        follow(&self.0, at)
    }
}

/// The kernel's quiet time, as the person changed it while it runs: the area of the run's
/// process when there is one, the default area otherwise (spec 3.3 §5.4, §10 K3).
#[derive(Clone)]
pub(crate) struct KernelQuiet(pub(crate) Kernel);

#[async_trait]
impl QuietTime for KernelQuiet {
    async fn until(&self, run_id: Option<&str>, at: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let partition = match run_id {
            Some(run) => match self.0.processes().get(run).await {
                Ok(Some(process)) => process.meta.partition,
                Ok(None) => PartitionId::default(),
                Err(error) => {
                    tracing::warn!(run.id = %run, error = %error, "a run's area could not be read; the default area's quiet time applies");
                    PartitionId::default()
                }
            },
            None => PartitionId::default(),
        };
        self.0.quiet_until(&partition, at).await
    }
}

/// The end of the windows holding `at`, followed through windows that touch, as the kernel's
/// channels follow them.
pub(crate) fn follow(windows: &[QuietHours], at: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let mut until = at;
    // Two windows release into each other at most once each.
    for _ in 0..=windows.len() {
        match windows
            .iter()
            .filter(|quiet| quiet.contains(until))
            .map(|quiet| quiet.release_at(until))
            .max()
        {
            Some(next) if next > until => until = next,
            _ => break,
        }
    }
    (until > at).then_some(until)
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveTime, TimeZone as _};

    use super::*;

    fn window(from: u32, to: u32) -> QuietHours {
        QuietHours::new(
            NaiveTime::from_hms_opt(from, 0, 0).unwrap(),
            NaiveTime::from_hms_opt(to, 0, 0).unwrap(),
        )
        .with_utc_offset_minutes(0)
    }

    #[test]
    fn touching_windows_are_followed_to_their_end() {
        let at = Utc.with_ymd_and_hms(2026, 10, 24, 22, 30, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 10, 25, 7, 0, 0).unwrap();
        assert_eq!(follow(&[window(22, 7)], at), Some(end));
        let later = Utc.with_ymd_and_hms(2026, 10, 25, 9, 0, 0).unwrap();
        assert_eq!(follow(&[window(22, 7), window(7, 9)], at), Some(later));
        let evening = Utc.with_ymd_and_hms(2026, 10, 24, 18, 0, 0).unwrap();
        assert_eq!(follow(&[window(22, 7)], evening), None);
    }
}
