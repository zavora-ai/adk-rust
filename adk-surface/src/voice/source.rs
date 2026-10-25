//! Decisions on calls a voice conversation holds go back to that conversation (spec 3.3 §5.6,
//! §7.3).
//!
//! A call the realtime session holds is in the session store like any other held call, so the
//! source's own resume — a turn of the harness on its text runner — would run it there too.
//! [`VoiceSource`] sends the decision of a call held by a live voice conversation to that
//! conversation instead, which resumes it on its realtime session, once.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::{ApprovalDecision, ApprovalRequest, Event};
use adk_harness::HarnessMode;
use adk_kernel::QuietHours;
use adk_kernel::proc::LedgerEntry;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::Notify;

use crate::error::Result;
use crate::origin::Caller;
use crate::source::{AgentState, AlwaysAllowOffer, RuleCommit, Source, SpendToday};

/// The voice conversations open in this process, by user and session: where a decision on
/// one of their held calls is sent (spec 3.3 §5.6).
///
/// Cheap to clone; every clone sees the same conversations.
#[derive(Clone, Default)]
pub struct LiveVoice {
    sessions: Arc<Mutex<Conversations>>,
}

/// The signal that wakes each live conversation, by user and session.
type Conversations = HashMap<(String, String), Arc<Notify>>;

impl std::fmt::Debug for LiveVoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let open = self.sessions.lock().unwrap_or_else(|e| e.into_inner()).len();
        f.debug_struct("LiveVoice").field("open", &open).finish()
    }
}

impl LiveVoice {
    /// No conversation open.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `user`'s session `session` is a live voice conversation.
    pub fn is_live(&self, user: &str, session: &str) -> bool {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&(user.to_string(), session.to_string()))
    }

    /// Registers a live conversation; it is woken through the returned signal until the
    /// guard drops.
    pub(crate) fn register(&self, user: &str, session: &str) -> (Registration, Arc<Notify>) {
        let wake = Arc::new(Notify::new());
        let key = (user.to_string(), session.to_string());
        self.sessions.lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone(), wake.clone());
        (Registration { live: self.clone(), key, wake: wake.clone() }, wake)
    }

    /// Wakes the live conversation of `user`'s session `session`; `false` when there is none.
    fn wake(&self, user: &str, session: &str) -> bool {
        let wake = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(user.to_string(), session.to_string()))
            .cloned();
        match wake {
            Some(wake) => {
                wake.notify_one();
                true
            }
            None => false,
        }
    }
}

/// A live conversation's place in [`LiveVoice`], removed when it drops.
pub(crate) struct Registration {
    live: LiveVoice,
    key: (String, String),
    wake: Arc<Notify>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        let mut sessions = self.live.sessions.lock().unwrap_or_else(|e| e.into_inner());
        // A later conversation on the same session registered its own signal: keep it.
        if sessions.get(&self.key).is_some_and(|wake| Arc::ptr_eq(wake, &self.wake)) {
            sessions.remove(&self.key);
        }
    }
}

/// The engine's source with voice (spec 3.3 §5.6): it delegates everything to the source it
/// wraps, except that a decision resolving a call held by a live voice conversation goes to
/// that conversation, which resumes the call on its realtime session. The wrapped source
/// never resumes it on the text runner, so the call runs once.
///
/// A decision made outside the engine — in Telegram through the kernel, or with
/// `adk-rust approvals` — never reaches [`Source::resolved`]; a conversation with held calls
/// also looks for those on every change the source signals.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::{InMemoryApprovalStore, StoreApprovalService};
/// use adk_session::InMemorySessionService;
/// use adk_surface::voice::VoiceSource;
/// use adk_surface::{LocalSource, SurfaceEngine};
///
/// let local = LocalSource::builder("adk-rust")
///     .sessions(Arc::new(InMemorySessionService::new()))
///     .approvals(Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))))
///     .build()
///     .unwrap();
/// let source = VoiceSource::new(Arc::new(local));
/// let live = source.live();
/// let engine = SurfaceEngine::builder(Arc::new(source)).build();
/// # let _ = (engine, live);
/// ```
pub struct VoiceSource {
    inner: Arc<dyn Source>,
    live: LiveVoice,
}

impl std::fmt::Debug for VoiceSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VoiceSource")
            .field("app", &self.inner.app_name())
            .field("live", &self.live)
            .finish()
    }
}

impl VoiceSource {
    /// Wraps `inner`.
    pub fn new(inner: Arc<dyn Source>) -> Self {
        Self { inner, live: LiveVoice::new() }
    }

    /// The live conversations this source routes decisions to; give it to the
    /// [`VoiceHub`](super::VoiceHub).
    pub fn live(&self) -> LiveVoice {
        self.live.clone()
    }

    /// The source it wraps.
    pub fn inner(&self) -> &Arc<dyn Source> {
        &self.inner
    }
}

#[async_trait]
impl Source for VoiceSource {
    fn app_name(&self) -> &str {
        self.inner.app_name()
    }

    async fn transcript(&self, user: &str, session: &str) -> Result<Option<Vec<Event>>> {
        self.inner.transcript(user, session).await
    }

    async fn changed(&self, max: Duration) {
        self.inner.changed(max).await;
    }

    async fn requests(
        &self,
        caller: &Caller,
        session: Option<&str>,
        open_only: bool,
    ) -> Result<Vec<ApprovalRequest>> {
        self.inner.requests(caller, session, open_only).await
    }

    async fn request(&self, id: &str) -> Result<Option<ApprovalRequest>> {
        self.inner.request(id).await
    }

    async fn decide(
        &self,
        id: &str,
        expected_version: u64,
        decision: ApprovalDecision,
    ) -> Result<ApprovalRequest> {
        self.inner.decide(id, expected_version, decision).await
    }

    async fn resolved(&self, request: &ApprovalRequest) -> Result<()> {
        if self.live.wake(&request.scope.user_id, &request.scope.session_id) {
            return Ok(());
        }
        self.inner.resolved(request).await
    }

    async fn resolved_by(&self, caller: &Caller, request: &ApprovalRequest) -> Result<()> {
        if self.live.wake(&request.scope.user_id, &request.scope.session_id) {
            return Ok(());
        }
        self.inner.resolved_by(caller, request).await
    }

    async fn always_allow_offer(&self, request: &ApprovalRequest) -> Option<AlwaysAllowOffer> {
        self.inner.always_allow_offer(request).await
    }

    async fn prepare_always_allow(
        &self,
        caller: &Caller,
        request: &ApprovalRequest,
    ) -> Result<Box<dyn RuleCommit>> {
        self.inner.prepare_always_allow(caller, request).await
    }

    async fn agents(&self, user: &str) -> Result<Vec<AgentState>> {
        self.inner.agents(user).await
    }

    fn primary_agent(&self) -> Option<String> {
        self.inner.primary_agent()
    }

    async fn busy(&self, user: &str, session: &str) -> Result<bool> {
        self.inner.busy(user, session).await
    }

    async fn waiting_on_decision(&self, user: &str, session: &str) -> Result<bool> {
        self.inner.waiting_on_decision(user, session).await
    }

    async fn start_turn(&self, caller: &Caller, session: &str, text: &str) -> Result<()> {
        self.inner.start_turn(caller, session, text).await
    }

    async fn stop_all(&self, caller: &Caller, reason: &str) -> Result<()> {
        self.inner.stop_all(caller, reason).await
    }

    async fn resume_all(&self, caller: &Caller) -> Result<()> {
        self.inner.resume_all(caller).await
    }

    async fn stopped(&self) -> Result<bool> {
        self.inner.stopped().await
    }

    async fn mode(&self, user: &str, session: Option<&str>) -> Result<Option<HarnessMode>> {
        self.inner.mode(user, session).await
    }

    async fn set_mode(&self, caller: &Caller, session: &str, mode: HarnessMode) -> Result<()> {
        self.inner.set_mode(caller, session, mode).await
    }

    async fn spend(&self, user: &str) -> Result<Option<SpendToday>> {
        self.inner.spend(user).await
    }

    fn quiet_hours(&self) -> Option<QuietHours> {
        self.inner.quiet_hours()
    }

    fn keeps_ledger(&self) -> bool {
        self.inner.keeps_ledger()
    }

    async fn ledger(&self, since: DateTime<Utc>) -> Result<Vec<LedgerEntry>> {
        self.inner.ledger(since).await
    }
}

#[cfg(test)]
mod tests {
    use adk_core::{ApprovalPolicy, ApprovalScope, ApproverSelector, ToolConfirmationRequest};

    use super::*;
    use crate::error::SurfaceError;
    use crate::origin::{Origin, SurfaceKind};

    /// A source that records which sessions it was asked to resume.
    #[derive(Default)]
    struct Recording {
        resumed: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Source for Recording {
        fn app_name(&self) -> &str {
            "home"
        }

        async fn transcript(&self, _user: &str, _session: &str) -> Result<Option<Vec<Event>>> {
            Ok(None)
        }

        async fn requests(
            &self,
            _caller: &Caller,
            _session: Option<&str>,
            _open_only: bool,
        ) -> Result<Vec<ApprovalRequest>> {
            Ok(Vec::new())
        }

        async fn request(&self, _id: &str) -> Result<Option<ApprovalRequest>> {
            Ok(None)
        }

        async fn decide(
            &self,
            _id: &str,
            _expected_version: u64,
            _decision: ApprovalDecision,
        ) -> Result<ApprovalRequest> {
            Err(SurfaceError::unsupported("decide"))
        }

        async fn resolved(&self, request: &ApprovalRequest) -> Result<()> {
            self.resumed.lock().unwrap().push(request.scope.session_id.clone());
            Ok(())
        }
    }

    fn decided(session: &str) -> ApprovalRequest {
        let call = ToolConfirmationRequest {
            tool_name: "bash".into(),
            function_call_id: Some("call-1".into()),
            args: serde_json::json!({ "command": "ls" }),
            origin: None,
            acting: None,
        };
        ApprovalRequest::new(
            format!("apr-{session}"),
            ApprovalScope::new("home", "ana", session),
            &call,
            ApprovalPolicy::new([ApproverSelector::group("owner")]),
        )
    }

    /// A decision on a live voice conversation's call wakes that conversation, never the
    /// wrapped source; any other session is the wrapped source's to resume.
    #[tokio::test]
    async fn a_live_conversation_is_woken_and_every_other_session_delegated() {
        let recording = Arc::new(Recording::default());
        let source = VoiceSource::new(recording.clone());
        let (registration, wake) = source.live().register("ana", "voice-1");
        let ana = Caller::human("ana", SurfaceKind::Web, Origin::Pointer);

        source.resolved_by(&ana, &decided("voice-1")).await.unwrap();
        source.resolved(&decided("voice-1")).await.unwrap();
        assert!(recording.resumed.lock().unwrap().is_empty());
        tokio::time::timeout(Duration::from_secs(1), wake.notified()).await.unwrap();

        source.resolved_by(&ana, &decided("typed")).await.unwrap();
        assert_eq!(*recording.resumed.lock().unwrap(), ["typed"]);

        // Once the conversation ends, its session is the wrapped source's again.
        drop(registration);
        assert!(!source.live().is_live("ana", "voice-1"));
        source.resolved(&decided("voice-1")).await.unwrap();
        assert_eq!(*recording.resumed.lock().unwrap(), ["typed", "voice-1"]);
    }
}
