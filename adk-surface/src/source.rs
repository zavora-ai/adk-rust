//! Where the engine reads and acts (spec 3.3 §6): the seam between the engine and a
//! kernel, a harness or a plain runner.
//!
//! | Source | Reads | Acts |
//! |---|---|---|
//! | [`LocalSource`](crate::LocalSource) | the session store, the approval service, an in-process kernel's process table and `/proc` ledger, a harness's mode | decide, start a turn (kernel process, harness or runner), freeze and unfreeze, set the mode, standing rules |
//! | [`ControlSource`](crate::ControlSource) | a kernel's control plane through `ControlClient`: attach, the approvals inbox, processes, health, the ledger | decide, spawn, freeze, unfreeze |
//!
//! A source holds no surface state (spec 3.2 K-I17): every call reads its stores.

use std::time::Duration;

use adk_core::{ApprovalDecision, ApprovalRequest, Event, RunState};
use adk_harness::HarnessMode;
use adk_kernel::QuietHours;
use adk_kernel::proc::LedgerEntry;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::{Result, SurfaceError};
use crate::origin::Caller;

/// An agent's state as a source reports it, before projection into an
/// [`AgentRow`](crate::AgentRow) (spec 3.3 §8.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentState {
    /// The agent's name: a kernel target or the runner's root agent.
    pub name: String,
    /// The session its latest work belongs to.
    pub session_id: Option<String>,
    /// The latest run's state.
    pub state: RunState,
    /// The run waits on a decision.
    pub waiting_on_decision: bool,
    /// The run sleeps until this time.
    pub wake_at: Option<DateTime<Utc>>,
    /// The run was suspended, by a stop or by hand.
    pub suspended: bool,
}

/// Today's spend and the daily limit, in micro-USD (spec 3.3 §8.1 "Daily spending limit").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpendToday {
    /// Spent today (UTC day).
    pub spent: u64,
    /// The daily limit, when one is set.
    pub limit: Option<u64>,
}

/// How a source grants "Always allow" (spec 3.3 §8.2): the label it offers for a request,
/// and the standing rule it creates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlwaysAllowOffer {
    /// The button's words, such as "Always allow for this folder".
    pub label: String,
}

/// The seam between the engine and what it shows (spec 3.3 §6).
///
/// Implement it to put the engine over another runtime; the methods with defaults report
/// `surface.unsupported`, and the engine hides what a source cannot do.
#[async_trait]
pub trait Source: Send + Sync {
    /// The application whose sessions the source serves.
    fn app_name(&self) -> &str;

    /// The session's events in store order, or `None` when `user` has no such session.
    ///
    /// The feed numbers events by their position here (spec 3.3 §6), so a source returns
    /// every event, oldest first, exactly as its store keeps them.
    async fn transcript(&self, user: &str, session: &str) -> Result<Option<Vec<Event>>>;

    /// Waits until something the surfaces show may have changed, or `max` passes.
    async fn changed(&self, max: Duration) {
        tokio::time::sleep(max).await;
    }

    /// The decision requests `caller` may answer — of the caller's session `session` when
    /// it is set (`InboxQuery::with_session`, spec 3.3 §10 C1) — open ones only when
    /// `open_only`.
    async fn requests(
        &self,
        caller: &Caller,
        session: Option<&str>,
        open_only: bool,
    ) -> Result<Vec<ApprovalRequest>>;

    /// One decision request.
    async fn request(&self, id: &str) -> Result<Option<ApprovalRequest>>;

    /// Records `decision` at `expected_version`: `ApprovalService::decide` (spec 3.3 §6,
    /// S-I2). Never retried.
    async fn decide(
        &self,
        id: &str,
        expected_version: u64,
        decision: ApprovalDecision,
    ) -> Result<ApprovalRequest>;

    /// Called once a decision the engine recorded resolved its request, so a call held in a
    /// run that is not parked durably resumes. The default does nothing: an approval service
    /// with a run store wakes a parked run itself.
    async fn resolved(&self, request: &ApprovalRequest) -> Result<()> {
        let _ = request;
        Ok(())
    }

    /// What "Always allow" would create for `request`, when this source can create it.
    async fn always_allow_offer(&self, request: &ApprovalRequest) -> Option<AlwaysAllowOffer> {
        let _ = request;
        None
    }

    /// Creates the standing rule "Always allow" offered for `request`, as `caller`, before the
    /// decision is recorded. Returns a commit that grants it once the decision won.
    async fn prepare_always_allow(
        &self,
        caller: &Caller,
        request: &ApprovalRequest,
    ) -> Result<Box<dyn RuleCommit>> {
        let _ = (caller, request);
        Err(SurfaceError::unsupported("always allow"))
    }

    /// The agents of `user`.
    async fn agents(&self, user: &str) -> Result<Vec<AgentState>> {
        let _ = user;
        Ok(Vec::new())
    }

    /// Whether a turn of the session is running now.
    async fn busy(&self, user: &str, session: &str) -> Result<bool> {
        let _ = (user, session);
        Ok(false)
    }

    /// Starts a turn of the session on `text`.
    async fn start_turn(&self, caller: &Caller, session: &str, text: &str) -> Result<()> {
        let _ = (caller, session, text);
        Err(SurfaceError::unsupported("sending a message"))
    }

    /// Stops every agent (spec 3.3 §8.2 "Stop all").
    async fn stop_all(&self, caller: &Caller, reason: &str) -> Result<()> {
        let _ = (caller, reason);
        Err(SurfaceError::unsupported("stop all"))
    }

    /// Resumes after a stop (spec 3.3 §8.2 "Resume all").
    async fn resume_all(&self, caller: &Caller) -> Result<()> {
        let _ = caller;
        Err(SurfaceError::unsupported("resume all"))
    }

    /// Whether everything is stopped.
    async fn stopped(&self) -> Result<bool> {
        Ok(false)
    }

    /// The session's mode, when the source runs a harness.
    async fn mode(&self, user: &str, session: Option<&str>) -> Result<Option<HarnessMode>> {
        let _ = (user, session);
        Ok(None)
    }

    /// Sets the session's mode.
    async fn set_mode(&self, caller: &Caller, session: &str, mode: HarnessMode) -> Result<()> {
        let _ = (caller, session, mode);
        Err(SurfaceError::unsupported("changing the mode"))
    }

    /// Today's spend for `user`, and the daily limit.
    async fn spend(&self, user: &str) -> Result<Option<SpendToday>> {
        let _ = user;
        Ok(None)
    }

    /// Quiet time, when the source knows it.
    fn quiet_hours(&self) -> Option<QuietHours> {
        None
    }

    /// The `/proc` ledger rows since `since`, oldest first.
    async fn ledger(&self, since: DateTime<Utc>) -> Result<Vec<LedgerEntry>> {
        let _ = since;
        Ok(Vec::new())
    }
}

/// A standing rule prepared for "Always allow", granted only once the decision it belongs to
/// was recorded.
#[async_trait]
pub trait RuleCommit: Send + Sync {
    /// Grants the rule.
    async fn commit(self: Box<Self>) -> Result<()>;
}
