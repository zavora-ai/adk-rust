//! Prewarming: readying a runner's agent, and optionally a session, before the first message.
//!
//! [`Runner::prewarm`](crate::Runner::prewarm) does what a first turn would otherwise do
//! before its first model token: it opens the model's pooled connection
//! ([`Llm::prewarm`](adk_core::Llm::prewarm)), starts the servers an agent starts lazily, such
//! as a harness's MCP servers ([`Agent::prewarm`](adk_core::Agent::prewarm)), and reads the
//! session, which opens the session store's connection and refuses an archived session early.
//! It sends no prompt and spends no tokens.
//!
//! | Bound | What it keeps |
//! |---|---|
//! | Kill switch | A frozen runner refuses to prewarm, as it refuses a turn |
//! | Budget | The whole prewarm stops at [`Prewarm::budget`] (10 seconds by default) with `runner.prewarm.timeout` |
//! | Expiry | A warm model connection lasts as long as the client keeps it idle, 90 seconds for `adk-anthropic`; [`PrewarmReport::warm_until`] says when to prewarm again |

use std::time::Duration;

use adk_core::identity::{SessionId, UserId};
use chrono::{DateTime, Utc};

/// How long a model client keeps an idle connection open: `adk-anthropic`'s and `adk-model`'s
/// pool idle timeout.
pub const WARM_FOR: Duration = Duration::from_secs(90);

/// What [`Runner::prewarm`](crate::Runner::prewarm) readies.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_core::{SessionId, UserId};
/// use adk_runner::Prewarm;
///
/// let request = Prewarm::new()
///     .session(UserId::new("ana").unwrap(), SessionId::new("trip").unwrap())
///     .budget(Duration::from_secs(5));
/// assert_eq!(request.budget_or_default(), Duration::from_secs(5));
/// ```
#[derive(Debug, Clone, Default)]
pub struct Prewarm {
    pub(crate) session: Option<(UserId, SessionId)>,
    pub(crate) budget: Option<Duration>,
}

impl Prewarm {
    /// The longest a prewarm runs when no [`budget`](Self::budget) is set.
    pub const DEFAULT_BUDGET: Duration = Duration::from_secs(10);

    /// Prewarms the agent tree only.
    pub fn new() -> Self {
        Self::default()
    }

    /// Also reads the session `session_id` of `user_id`, so its store's connection is open and
    /// an archived session is refused before anyone types.
    #[must_use]
    pub fn session(mut self, user_id: UserId, session_id: SessionId) -> Self {
        self.session = Some((user_id, session_id));
        self
    }

    /// Stops the prewarm after `budget`.
    #[must_use]
    pub fn budget(mut self, budget: Duration) -> Self {
        self.budget = Some(budget);
        self
    }

    /// The budget, or [`DEFAULT_BUDGET`](Self::DEFAULT_BUDGET).
    pub fn budget_or_default(&self) -> Duration {
        self.budget.unwrap_or(Self::DEFAULT_BUDGET)
    }
}

/// What a [`Runner::prewarm`](crate::Runner::prewarm) did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PrewarmReport {
    /// How long the prewarm took.
    pub elapsed: Duration,
    /// Whether a session was read.
    pub session_read: bool,
    /// When the model connection it opened is likely closed again: [`WARM_FOR`] from now.
    pub warm_until: DateTime<Utc>,
}
