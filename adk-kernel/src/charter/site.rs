//! The site's process kinds (spec 3.5 §7.11, §7.19, §7.20).

use std::fmt;

use adk_core::NewRun;
use adk_core::schedule::MissedFirePolicy;
use serde::{Deserialize, Serialize};

use crate::partition::PartitionId;
use crate::process::{ProcessKind, ProcessMeta, ProcessSpec};
use crate::sched::QueueClass;
use crate::wake::{Trigger, Wake};

/// The label every site process carries, with its kind's name as the value (spec 3.5 §7.11).
pub const SITE_PROCESS_LABEL: &str = "site.process";

/// A kind of process a website's team runs (spec 3.5 §7.11, §7.19, §7.20): the kernel
/// recognises these, and the product does their work.
///
/// A site process is a [`ProcessKind::Triggered`] process whose source is the kind's name,
/// labelled [`SITE_PROCESS_LABEL`]; [`trigger`](Self::trigger) builds the standing trigger
/// that starts one, and [`of`](Self::of) recognises one in the process table.
///
/// | Kind | Job (§7.20) | Default wake (§7.19) |
/// |---|---|---|
/// | `site.probe` | Guardian: drift probes of the served release | every 5 minutes |
/// | `site.watch` | Guardian: advisories, certificates, domains, status | daily at 06:00 UTC |
/// | `site.content` | Growth: freshness and content gaps | a `content_gap` signal |
/// | `site.reviews` | Growth, Inbox: review replies | a `review_seen` signal |
/// | `site.outreach` | Revenue: nurture sequences and handoffs | daily at 09:00 UTC |
/// | `site.support` | Steward: support escalations | a `support_escalation` signal |
/// | `site.digest` | Steward, Finance: the weekly review | Mondays at 07:00 UTC |
///
/// # Example
///
/// ```rust
/// use adk_kernel::charter::SiteProcessKind;
/// use adk_kernel::{ProcessKind, ProcessMeta, QueueClass};
///
/// let kind: SiteProcessKind = "site.digest".parse().unwrap();
/// assert_eq!(kind, SiteProcessKind::Digest);
/// let meta = ProcessMeta::new("run-1", QueueClass::Background, ProcessKind::Triggered { source: "site.probe".into() });
/// assert_eq!(SiteProcessKind::of(&meta), Some(SiteProcessKind::Probe));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SiteProcessKind {
    /// `site.probe`.
    #[serde(rename = "site.probe")]
    Probe,
    /// `site.watch`.
    #[serde(rename = "site.watch")]
    Watch,
    /// `site.content`.
    #[serde(rename = "site.content")]
    Content,
    /// `site.reviews`.
    #[serde(rename = "site.reviews")]
    Reviews,
    /// `site.outreach`.
    #[serde(rename = "site.outreach")]
    Outreach,
    /// `site.support`.
    #[serde(rename = "site.support")]
    Support,
    /// `site.digest`.
    #[serde(rename = "site.digest")]
    Digest,
}

impl SiteProcessKind {
    /// Every kind.
    pub const ALL: [Self; 7] = [
        Self::Probe,
        Self::Watch,
        Self::Content,
        Self::Reviews,
        Self::Outreach,
        Self::Support,
        Self::Digest,
    ];

    /// The kind's name, `site.probe` to `site.digest`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Probe => "site.probe",
            Self::Watch => "site.watch",
            Self::Content => "site.content",
            Self::Reviews => "site.reviews",
            Self::Outreach => "site.outreach",
            Self::Support => "site.support",
            Self::Digest => "site.digest",
        }
    }

    /// The kind of `meta`'s process: its [`SITE_PROCESS_LABEL`], or the source of a
    /// [`ProcessKind::Triggered`] process; `None` for a process no site trigger started.
    pub fn of(meta: &ProcessMeta) -> Option<Self> {
        let named = |name: &str| Self::ALL.into_iter().find(|kind| kind.as_str() == name);
        meta.labels.get(SITE_PROCESS_LABEL).and_then(|name| named(name)).or_else(|| {
            match &meta.kind {
                ProcessKind::Triggered { source } => named(source),
                _ => None,
            }
        })
    }

    /// The queue its processes run on: a probe and a watch are the kernel's own upkeep
    /// (`maintenance`); the rest are the team's work (`background`).
    pub fn queue(self) -> QueueClass {
        match self {
            Self::Probe | Self::Watch => QueueClass::Maintenance,
            Self::Content | Self::Reviews | Self::Outreach | Self::Support | Self::Digest => {
                QueueClass::Background
            }
        }
    }

    /// The wake that starts it by default (spec 3.5 §7.19): a six-field UTC cron for the
    /// scheduled kinds, a [`Wake::Signal`] of the back office `partition` for the kinds a
    /// signal starts.
    pub fn default_wake(self, partition: &PartitionId) -> Wake {
        let cron =
            |expr: &str| Wake::Cron { expr: expr.to_string(), missed: MissedFirePolicy::Skip };
        let signal = |kind: &str| Wake::Signal {
            kind: kind.to_string(),
            partition: Some(partition.clone()),
        };
        match self {
            Self::Probe => cron("0 */5 * * * *"),
            Self::Watch => cron("0 0 6 * * *"),
            Self::Content => signal("content_gap"),
            Self::Reviews => signal("review_seen"),
            Self::Outreach => cron("0 0 9 * * *"),
            Self::Support => signal("support_escalation"),
            Self::Digest => cron("0 0 7 * * MON"),
        }
    }

    /// The process spec of a kind's process in `partition`: [`ProcessKind::Triggered`] with
    /// the kind's name as source, labelled [`SITE_PROCESS_LABEL`].
    pub fn spec(self, partition: impl Into<PartitionId>) -> ProcessSpec {
        ProcessSpec::new(ProcessKind::Triggered { source: self.as_str().to_string() })
            .with_partition(partition)
            .with_label(SITE_PROCESS_LABEL, self.as_str())
    }

    /// The standing trigger that starts a kind's process in `partition` when `wake` fires,
    /// running `run` on the kind's [`queue`](Self::queue); the trigger is named after the
    /// kind, so its processes are [`ProcessKind::Triggered`] with that source.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::NewRun;
    /// use adk_kernel::charter::SiteProcessKind;
    ///
    /// let run = NewRun::new("background", "adk-rust", "site", "guardian", serde_json::json!({"text": "probe"}));
    /// let partition = "back-office".into();
    /// let trigger = SiteProcessKind::Probe.trigger(SiteProcessKind::Probe.default_wake(&partition), run, partition);
    /// assert_eq!(trigger.name, "site.probe");
    /// assert_eq!(trigger.run.queue, "maintenance");
    /// ```
    pub fn trigger(
        self,
        wake: Wake,
        mut run: NewRun,
        partition: impl Into<PartitionId>,
    ) -> Trigger {
        run.queue = self.queue().as_str().to_string();
        Trigger::new(self.as_str(), wake, run).with_partition(partition)
    }
}

impl fmt::Display for SiteProcessKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for SiteProcessKind {
    type Err = crate::KernelError;

    fn from_str(text: &str) -> std::result::Result<Self, Self::Err> {
        let text = text.trim();
        Self::ALL.into_iter().find(|kind| kind.as_str() == text).ok_or_else(|| {
            crate::KernelError::Config {
                reason: format!(
                    "unknown site process kind `{text}`; use site.probe, site.watch, site.content, \
                     site.reviews, site.outreach, site.support or site.digest"
                ),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_parse_back_and_serialize_as_themselves() {
        for kind in SiteProcessKind::ALL {
            assert_eq!(kind.as_str().parse::<SiteProcessKind>().unwrap(), kind);
            assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
        }
        assert!("site.other".parse::<SiteProcessKind>().is_err());
    }

    #[test]
    fn a_spec_is_recognised_by_its_label_and_kind() {
        let spec = SiteProcessKind::Reviews.spec("back-office");
        assert_eq!(spec.labels.get(SITE_PROCESS_LABEL).map(String::as_str), Some("site.reviews"));
        let meta = ProcessMeta::new("run-1", QueueClass::Background, ProcessKind::Goal)
            .with_label(SITE_PROCESS_LABEL, "site.reviews");
        assert_eq!(SiteProcessKind::of(&meta), Some(SiteProcessKind::Reviews));
        let other = ProcessMeta::new("run-2", QueueClass::Background, ProcessKind::Goal);
        assert_eq!(SiteProcessKind::of(&other), None);
    }

    #[test]
    fn default_wakes_are_valid_crons_or_signals_of_the_partition() {
        let partition = PartitionId::new("back-office");
        for kind in SiteProcessKind::ALL {
            match kind.default_wake(&partition) {
                Wake::Cron { expr, .. } => {
                    adk_agent::ambient::CronTrigger::new(&expr).expect("a valid cron");
                }
                Wake::Signal { partition: Some(p), .. } => assert_eq!(p, partition),
                other => panic!("{kind} wakes on {other:?}"),
            }
        }
    }
}
