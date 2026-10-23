//! The long-term memory of one harness: recall, writing, distillation and their
//! bookkeeping (spec 3.1 §7.15.2).

use std::collections::HashMap;
#[cfg(feature = "runner")]
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

#[cfg(feature = "runner")]
use adk_core::GovernanceControl;
use adk_core::audit_log::{AuditActor, AuditTrail};
use adk_core::principal::SubjectRef;
use adk_core::{BudgetTracker, InvocationContext, Llm, ReadonlyContext, Result, RunBudget};
use serde_json::json;

use super::index::FactIndex;
use super::store::FactStore;
use super::types::{FactScope, FactStatus, LongTermMemoryConfig, MemoryFact};
use crate::audit::HarnessAudit;
use crate::mode::{HarnessMode, ModeStore};

/// What a run carries that the per-request hook cannot see: its audit trail and budget.
#[derive(Clone)]
pub(crate) struct InvocationFacts {
    pub(crate) audit: Option<AuditTrail>,
    pub(crate) budget: Option<Arc<BudgetTracker>>,
}

/// One session's turn bookkeeping for distillation.
#[derive(Debug, Default)]
pub(crate) struct SessionTrack {
    /// User turns since the session was last distilled.
    pub(crate) pending_turns: u32,
    /// User turns in this process.
    pub(crate) turns: u64,
    /// Bumped on every turn; an idle timer distils only if it is unchanged.
    pub(crate) generation: u64,
    /// Whether the session's last turn was authenticated.
    pub(crate) authenticated: bool,
    /// Events up to this time were distilled.
    #[cfg_attr(not(feature = "runner"), allow(dead_code))] // read by distillation
    pub(crate) distilled_upto: Option<chrono::DateTime<chrono::Utc>>,
}

/// Where distillation reads sessions from, set when a [`Harness`](crate::Harness) wraps
/// the agent.
#[cfg(feature = "runner")]
pub(crate) struct Attached {
    pub(crate) sessions: Arc<dyn adk_session::SessionService>,
    pub(crate) app_name: String,
    pub(crate) governance: GovernanceControl,
    /// The runner's audit trail, which records made outside a run go to.
    pub(crate) audit: Option<AuditTrail>,
}

/// The recall block last computed for a session, and what it was computed for.
#[derive(Clone, Debug)]
pub(crate) struct CachedRecall {
    pub(crate) key: String,
    pub(crate) block: Option<String>,
    pub(crate) fact_ids: Vec<String>,
}

/// Session key: (app, user, session).
pub(crate) type SessionKey = (String, String, String);

/// The long-term memory of one harness (spec 3.1 §7.15.2).
pub(crate) struct FactsRuntime {
    pub(crate) config: LongTermMemoryConfig,
    pub(crate) store: Arc<dyn FactStore>,
    pub(crate) index: Arc<dyn FactIndex>,
    /// The default index's embedder, which consolidation embeds facts without a stored
    /// vector with (spec 3.2 §7.5).
    #[cfg_attr(not(feature = "runner"), allow(dead_code))] // read by consolidation
    pub(crate) embedder: Option<Arc<dyn adk_memory::EmbeddingProvider>>,
    /// The project of this harness's workspace.
    pub(crate) project: String,
    /// The harness's own agent; members neither recall nor write (spec 3.1 §9.2 I42).
    pub(crate) root_agent: String,
    /// Long-term memory is inert for runs without authenticated identity (I42).
    pub(crate) require_authenticated: bool,
    /// The harness's audit trail, for records outside a run.
    pub(crate) audit: Option<AuditTrail>,
    #[cfg_attr(not(feature = "runner"), allow(dead_code))] // read by distillation
    pub(crate) distill_model: Arc<dyn Llm>,
    #[cfg_attr(not(feature = "runner"), allow(dead_code))] // read by distillation
    pub(crate) budget: Option<RunBudget>,
    pub(crate) mode_store: Arc<dyn ModeStore>,
    pub(crate) default_mode: HarnessMode,
    pub(crate) recall_cache: Mutex<HashMap<String, CachedRecall>>,
    pub(crate) invocations: Mutex<HashMap<String, InvocationFacts>>,
    pub(crate) sessions: Mutex<HashMap<SessionKey, SessionTrack>>,
    #[cfg(feature = "runner")]
    pub(crate) attached: OnceLock<Attached>,
    /// The harness's durable run store, which says whether a session that ran in an
    /// earlier process was authenticated (I42; spec 3.2 §10 HK2).
    #[cfg(feature = "runner")]
    pub(crate) runs: Mutex<Option<Arc<dyn adk_core::DurableRunStore>>>,
    #[cfg_attr(not(feature = "runner"), allow(dead_code))] // read by distillation
    pub(crate) distilling: tokio::sync::Mutex<()>,
    #[cfg_attr(not(feature = "runner"), allow(dead_code))] // read by distillation
    pub(crate) closed: AtomicBool,
}

impl std::fmt::Debug for FactsRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FactsRuntime")
            .field("config", &self.config)
            .field("project", &self.project)
            .finish_non_exhaustive()
    }
}

/// Upper bound of remembered invocations and sessions; the maps are cleared beyond it.
const TRACK_CAP: usize = 4096;

/// The subject of changes the harness makes on its own: distillation, eviction.
pub(crate) fn harness_subject() -> SubjectRef {
    SubjectRef::service("adk-harness")
}

impl FactsRuntime {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        config: LongTermMemoryConfig,
        store: Arc<dyn FactStore>,
        index: Arc<dyn FactIndex>,
        project: String,
        root_agent: String,
        require_authenticated: bool,
        audit: Option<AuditTrail>,
        distill_model: Arc<dyn Llm>,
        budget: Option<RunBudget>,
        mode_store: Arc<dyn ModeStore>,
        default_mode: HarnessMode,
    ) -> Self {
        Self {
            config,
            store,
            index,
            embedder: None,
            project,
            root_agent,
            require_authenticated,
            audit,
            distill_model,
            budget,
            mode_store,
            default_mode,
            recall_cache: Mutex::new(HashMap::new()),
            invocations: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            #[cfg(feature = "runner")]
            attached: OnceLock::new(),
            #[cfg(feature = "runner")]
            runs: Mutex::new(None),
            distilling: tokio::sync::Mutex::new(()),
            closed: AtomicBool::new(false),
        }
    }

    /// This runtime, embedding facts for consolidation with `embedder`.
    #[must_use]
    pub(crate) fn with_embedder(
        mut self,
        embedder: Option<Arc<dyn adk_memory::EmbeddingProvider>>,
    ) -> Self {
        self.embedder = embedder;
        self
    }

    /// The project scope of `user` of `app`.
    pub(crate) fn scope(&self, app: &str, user: &str) -> FactScope {
        FactScope::new(app, user, Some(self.project.clone()))
    }

    /// Whether long-term memory serves `ctx`: the harness's own agent, with authenticated
    /// identity when the harness requires it (spec 3.1 §9.2 I42).
    pub(crate) fn serves(&self, ctx: &dyn ReadonlyContext) -> bool {
        ctx.agent_name() == self.root_agent
            && (!self.require_authenticated || ctx.is_authenticated())
    }

    /// The session's mode; unreadable is `ReadOnly`.
    pub(crate) async fn mode(&self, app: &str, user: &str, session: &str) -> HarnessMode {
        match self.mode_store.get(app, user, session).await {
            Ok(Some(record)) => record.mode,
            Ok(None) => self.default_mode,
            Err(_) => HarnessMode::ReadOnly,
        }
    }

    /// The audit of the invocation `ctx` runs in: the run's own trail when the harness saw
    /// the run start, else the harness's.
    pub(crate) fn audit_for(&self, ctx: &dyn ReadonlyContext) -> Option<HarnessAudit> {
        let trail = self
            .invocation(ctx.invocation_id())
            .and_then(|facts| facts.audit)
            .or_else(|| self.audit.clone())?;
        Some(HarnessAudit::new(trail, AuditActor::for_context(ctx), ctx.session_id()))
    }

    pub(crate) fn invocation(&self, invocation_id: &str) -> Option<InvocationFacts> {
        self.invocations.lock().unwrap_or_else(|e| e.into_inner()).get(invocation_id).cloned()
    }

    /// Records a run's start: its audit trail and budget for the per-request hook, and, for
    /// a user turn, the turn for distillation (spec 3.1 §7.15.2).
    pub(crate) fn observe_invocation(self: &Arc<Self>, ctx: &dyn InvocationContext) {
        let config = ctx.run_config();
        {
            let mut invocations = self.invocations.lock().unwrap_or_else(|e| e.into_inner());
            if invocations.len() > TRACK_CAP {
                invocations.clear();
            }
            invocations.insert(
                ctx.invocation_id().to_string(),
                InvocationFacts {
                    audit: config.audit.clone(),
                    budget: config.budget_tracker.clone(),
                },
            );
        }
        if !ctx.user_content().carries_user_input() || ctx.agent_name() != self.root_agent {
            return;
        }
        let key: SessionKey =
            (ctx.app_name().to_string(), ctx.user_id().to_string(), ctx.session_id().to_string());
        let (generation, every_due) = {
            let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            if sessions.len() > TRACK_CAP {
                sessions.retain(|_, track| track.pending_turns > 0);
            }
            let track = sessions.entry(key.clone()).or_default();
            track.pending_turns = track.pending_turns.saturating_add(1);
            track.turns += 1;
            track.generation += 1;
            track.authenticated = ctx.is_authenticated();
            let every = u64::from(self.config.distill_every.max(1));
            (track.generation, track.turns.is_multiple_of(every))
        };
        #[cfg(feature = "runner")]
        self.schedule_distillation(key, generation, every_due);
        #[cfg(not(feature = "runner"))]
        let _ = (key, generation, every_due);
    }

    /// Records a status change of `fact` write-ahead, then makes it (spec 3.1 §9.3).
    pub(crate) async fn change_status(
        &self,
        audit: Option<&HarnessAudit>,
        fact: &MemoryFact,
        status: FactStatus,
        by: &SubjectRef,
    ) -> Result<()> {
        self.change_status_in(audit, fact, status, by, None).await
    }

    /// As [`change_status`](Self::change_status), the record naming the consolidation tick
    /// and the reason when `tick` is `(tick id, reason)` (spec 3.2 §7.5).
    pub(crate) async fn change_status_in(
        &self,
        audit: Option<&HarnessAudit>,
        fact: &MemoryFact,
        status: FactStatus,
        by: &SubjectRef,
        tick: Option<(&str, &str)>,
    ) -> Result<()> {
        let action = match &status {
            FactStatus::Known => "harness.memory.fact.promoted",
            FactStatus::Superseded { .. } => "harness.memory.fact.superseded",
            FactStatus::Forgotten => "harness.memory.fact.forgotten",
            FactStatus::Assumed => "harness.memory.fact.demoted",
        };
        if let Some(audit) = audit {
            let mut ext = json!({
                "factId": fact.id,
                "from": fact.status.label(),
                "to": status.label(),
                "by": by.id,
                "supersededBy": match &status {
                    FactStatus::Superseded { by } => Some(by.clone()),
                    _ => None,
                },
            });
            if let Some((tick_id, reason)) = tick {
                ext["tickId"] = json!(tick_id);
                ext["reason"] = json!(reason);
            }
            audit.record(action, ext).await?;
        }
        self.store.set_status(&fact.id, status.clone(), by).await?;
        if !status.is_live()
            && let Err(error) = self.index.remove(fact).await
        {
            tracing::warn!(error = %error, "could not drop a fact from the index; the store still excludes it");
        }
        self.forget_cached(&fact.id);
        Ok(())
    }

    /// Drops every cached recall block that shows `fact_id`, so a status change applies
    /// from the next model call (spec 3.1 §9.2 I44).
    pub(crate) fn forget_cached(&self, fact_id: &str) {
        self.recall_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, cached| !cached.fact_ids.iter().any(|id| id == fact_id));
    }

    #[cfg(feature = "runner")]
    /// The trail of records made outside a run: the runner's, else the harness's.
    pub(crate) fn outside_trail(&self) -> Option<AuditTrail> {
        #[cfg(feature = "runner")]
        if let Some(trail) = self.attached.get().and_then(|attached| attached.audit.clone()) {
            return Some(trail);
        }
        self.audit.clone()
    }

    #[cfg(feature = "runner")]
    /// The audit of an action outside a run, by `subject` in `session`.
    pub(crate) fn audit_as(&self, subject: SubjectRef, session: &str) -> Option<HarnessAudit> {
        Some(HarnessAudit::new(self.outside_trail()?, AuditActor::subject(subject), session))
    }

    #[cfg(feature = "runner")]
    pub(crate) fn mark_closed(&self) {
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(feature = "runner")]
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(std::sync::atomic::Ordering::SeqCst)
    }
}
