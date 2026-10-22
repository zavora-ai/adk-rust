//! Queue classes (spec 3.2 §7.9).
//!
//! Priority is a queue (§6 D-5): the kernel runs one `DurableWorker` per [`QueueClass`],
//! each with its own fallback poll interval (§13 Q2). A worker sleeps until the earliest
//! `not_before` deadline in its queue, and `enqueue` and `wake` notify it, so the poll only
//! catches runs another process enqueued (§10 S3).
//!
//! Preemption, per-partition quotas, quiet hours and cost-aware admission are phase 2 and
//! are added here without changing [`QueueClass`].

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::KernelError;

/// A priority class: the run-store queue a process waits in and the worker that claims it
/// (spec 3.2 §7.9).
///
/// The three classes of the default configuration are [`Interactive`](Self::Interactive)
/// (a human turn), [`Background`](Self::Background) (goals, triggered and scheduled work)
/// and [`Maintenance`](Self::Maintenance) (the kernel's own work, `kernel.wake` runs).
/// [`Named`](Self::Named) is any other queue. A class serializes as its queue name.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_kernel::QueueClass;
///
/// let class: QueueClass = "background".parse().unwrap();
/// assert_eq!(class, QueueClass::Background);
/// assert_eq!(class.poll_interval(), Duration::from_secs(2));
/// assert_eq!(QueueClass::Interactive.poll_interval(), Duration::from_millis(500));
/// assert_eq!(QueueClass::from_name("payments"), QueueClass::Named("payments".into()));
/// assert_eq!(serde_json::to_value(QueueClass::Maintenance).unwrap(), "maintenance");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum QueueClass {
    /// Human turns; claimed first-come with a 500 ms fallback poll.
    Interactive,
    /// Goals, triggered and scheduled processes; 2 s fallback poll.
    Background,
    /// The kernel's own work; 5 s fallback poll.
    Maintenance,
    /// Any other queue; 2 s fallback poll.
    Named(String),
}

impl QueueClass {
    /// The run-store queue name of [`Interactive`](Self::Interactive).
    pub const INTERACTIVE: &'static str = "interactive";
    /// The run-store queue name of [`Background`](Self::Background).
    pub const BACKGROUND: &'static str = "background";
    /// The run-store queue name of [`Maintenance`](Self::Maintenance).
    pub const MAINTENANCE: &'static str = "maintenance";

    /// The class of the run-store queue `name`; the three default names map to their
    /// classes and anything else is [`Named`](Self::Named).
    pub fn from_name(name: &str) -> Self {
        match name {
            Self::INTERACTIVE => Self::Interactive,
            Self::BACKGROUND => Self::Background,
            Self::MAINTENANCE => Self::Maintenance,
            other => Self::Named(other.to_string()),
        }
    }

    /// The run-store queue name the class's worker claims from.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Interactive => Self::INTERACTIVE,
            Self::Background => Self::BACKGROUND,
            Self::Maintenance => Self::MAINTENANCE,
            Self::Named(name) => name,
        }
    }

    /// The worker's fallback poll interval (spec 3.2 §7.9, §13 Q2): 500 ms for
    /// `interactive`, 2 s for `background` and named queues, 5 s for `maintenance`.
    pub fn poll_interval(&self) -> Duration {
        match self {
            Self::Interactive => Duration::from_millis(500),
            Self::Background | Self::Named(_) => Duration::from_secs(2),
            Self::Maintenance => Duration::from_secs(5),
        }
    }

    /// The default queue classes: interactive, background and maintenance (spec 3.2 §7.2).
    pub fn defaults() -> Vec<Self> {
        vec![Self::Interactive, Self::Background, Self::Maintenance]
    }
}

impl fmt::Display for QueueClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for QueueClass {
    type Err = KernelError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        let name = name.trim();
        if name.is_empty() {
            return Err(KernelError::Config {
                reason: "a queue name must not be empty; use interactive, background, \
                         maintenance or a queue the kernel configures"
                    .to_string(),
            });
        }
        Ok(Self::from_name(name))
    }
}

impl Serialize for QueueClass {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for QueueClass {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        name.parse().map_err(serde::de::Error::custom)
    }
}
