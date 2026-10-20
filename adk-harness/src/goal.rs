//! Goal state (spec 3.1 §7.11). The goal engine lands with the plan graph; these types
//! fix the shape `Harness::start_goal` and `Harness::goal` exchange.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A loop-until goal (spec 3.1 §7.11).
///
/// # Example
///
/// ```rust
/// use adk_harness::GoalSpec;
/// use std::time::Duration;
///
/// let goal = GoalSpec {
///     goal: "make the tests pass".into(),
///     until: "cargo test".into(),
///     max_iterations: 10,
///     check_timeout: Duration::from_secs(600),
/// };
/// assert_eq!(goal.max_iterations, 10);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalSpec {
    /// What to achieve.
    pub goal: String,
    /// The operator's check command; exit 0 means done.
    pub until: String,
    /// Most iterations.
    pub max_iterations: u32,
    /// Timeout of one check.
    pub check_timeout: Duration,
}

/// Where a goal stands (spec 3.1 §7.11).
///
/// # Example
///
/// ```rust
/// assert_eq!(serde_json::to_value(adk_harness::GoalStatus::Exhausted).unwrap(), "exhausted");
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GoalStatus {
    /// Iterating.
    Running,
    /// The check passed.
    Done,
    /// The iteration cap was reached.
    Exhausted,
    /// Cancelled.
    Cancelled,
}

/// A goal's state, as mirrored to `harness:goal` (spec 3.1 §7.11).
///
/// # Example
///
/// ```rust,ignore
/// if let Some(state) = harness.goal(&user, &session).await? {
///     println!("{} of {}", state.iteration, state.max_iterations);
/// }
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalState {
    /// What to achieve.
    pub goal: String,
    /// The check command.
    pub until: String,
    /// The current iteration.
    pub iteration: u32,
    /// Most iterations.
    pub max_iterations: u32,
    /// Where it stands.
    pub status: GoalStatus,
    /// The last check's exit code.
    pub last_check: Option<i32>,
    /// When it last changed.
    pub updated_at: DateTime<Utc>,
}

/// The goal loop's engine (spec 3.1 §7.11, feature `graph`).
///
/// # Example
///
/// ```rust
/// let engine = adk_harness::GoalEngine::Loop;
/// # let _ = engine;
/// ```
#[cfg(feature = "graph")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoalEngine {
    /// A `LoopAgent` over the harness agent and an until-check agent.
    Loop,
    /// A checkpointed `StateGraph`: iterate → check → (done | iterate).
    Graph,
}
