//! A session service that writes nothing lasting for a subject without its consent.

use std::sync::Arc;

use adk_core::identity::AdkIdentity;
use adk_core::principal::SubjectRef;
use adk_core::subject::{ConsentGate, ConsentPurpose, consent_required_error};
use adk_core::{KEY_PREFIX_USER, Result};
use async_trait::async_trait;
use serde_json::Value;

use crate::Event;
use crate::inmemory::InMemorySessionService;
use crate::service::{
    AppendEventRequest, CreateRequest, DeleteRequest, GetRequest, ListRequest, SessionService,
};
use crate::session::Session;

/// A [`SessionService`] that keeps a gated subject's conversation out of durable storage
/// until the subject consents.
///
/// For a user the [`ConsentGate`] gates — a visitor id by default:
///
/// | Purpose | With live consent | Without |
/// |---------|-------------------|---------|
/// | [`SessionContinuity`](ConsentPurpose::SessionContinuity) | New sessions go to the durable service | New sessions live in process memory and end with the process; appending to a durable session fails with `consent.required` |
/// | [`PersistentProfile`](ConsentPurpose::PersistentProfile) | `user:` keys are written | `user:` keys are dropped from created state and event deltas |
///
/// A subject being erased can create nothing and append nothing (`subject.erasing`). Reads
/// and deletes reach both stores, so a conversation started before consent continues where
/// it began, and erasure finds it. Users the gate does not gate pass straight through.
///
/// [`append_event_for_identity`](SessionService::append_event_for_identity) applies the
/// gate; the identity-less [`append_event`](SessionService::append_event) names no user, so
/// it appends to whichever store holds the session without dropping `user:` keys. The
/// runner uses the identity form.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::subject::{ConsentGate, InMemoryConsentStore, VisitorId};
/// use adk_session::{ConsentGatedSessionService, CreateRequest, InMemorySessionService, SessionService};
/// use serde_json::json;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_core::Result<()> {
/// let durable = Arc::new(InMemorySessionService::new());
/// let gate = ConsentGate::new(Arc::new(InMemoryConsentStore::new()));
/// let sessions = ConsentGatedSessionService::new(durable.clone(), gate);
///
/// let visitor = VisitorId::generate();
/// let session = sessions
///     .create(CreateRequest {
///         app_name: "shop".into(),
///         user_id: visitor.to_string(),
///         session_id: None,
///         state: [("user:name".to_string(), json!("Wanjiru"))].into(),
///     })
///     .await?;
/// assert!(session.state().get("user:name").is_none(), "no profile consent");
/// let listed = durable
///     .list(adk_session::ListRequest { app_name: "shop".into(), user_id: visitor.to_string(), limit: None, offset: None })
///     .await?;
/// assert!(listed.is_empty(), "no continuity consent: nothing durable");
/// # Ok(())
/// # }
/// ```
pub struct ConsentGatedSessionService {
    durable: Arc<dyn SessionService>,
    ephemeral: Arc<dyn SessionService>,
    gate: ConsentGate,
}

impl std::fmt::Debug for ConsentGatedSessionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsentGatedSessionService")
            .field("gate", &self.gate)
            .finish_non_exhaustive()
    }
}

impl ConsentGatedSessionService {
    /// Gates `durable` with `gate`, keeping unconsented sessions in process memory.
    pub fn new(durable: Arc<dyn SessionService>, gate: ConsentGate) -> Self {
        Self { durable, ephemeral: Arc::new(InMemorySessionService::new()), gate }
    }

    /// Keeps unconsented sessions in `ephemeral` instead of process memory, such as a
    /// store whose records expire.
    #[must_use]
    pub fn with_ephemeral(mut self, ephemeral: Arc<dyn SessionService>) -> Self {
        self.ephemeral = ephemeral;
        self
    }

    /// The gated subject `user_id` names, after checking it may be written.
    async fn writable(&self, user_id: &str) -> Result<Option<SubjectRef>> {
        let Some(subject) = self.gate.subject_of(user_id) else { return Ok(None) };
        self.gate.ensure_writable(&subject).await?;
        Ok(Some(subject))
    }

    /// Drops `user:` keys from `state` unless `subject` consents to a persistent profile.
    async fn profile_filter(
        &self,
        subject: &SubjectRef,
        state: &mut std::collections::HashMap<String, Value>,
    ) -> Result<()> {
        if self.gate.permits(subject, &ConsentPurpose::PersistentProfile).await? {
            return Ok(());
        }
        let before = state.len();
        state.retain(|key, _| !key.starts_with(KEY_PREFIX_USER));
        if state.len() != before {
            tracing::warn!(
                subject.kind = %subject.kind,
                state.dropped = before - state.len(),
                "user: state not written without consent for a persistent profile"
            );
        }
        Ok(())
    }

    /// The store that holds the session `identity`: the durable one, else the ephemeral one.
    async fn holder(&self, identity: &AdkIdentity) -> Result<&Arc<dyn SessionService>> {
        match self.durable.get_for_identity(identity).await {
            Ok(_) => Ok(&self.durable),
            Err(error) if error.is_not_found() => Ok(&self.ephemeral),
            Err(error) => Err(error),
        }
    }
}

#[async_trait]
impl SessionService for ConsentGatedSessionService {
    async fn create(&self, mut req: CreateRequest) -> Result<Box<dyn Session>> {
        let Some(subject) = self.writable(&req.user_id).await? else {
            return self.durable.create(req).await;
        };
        self.profile_filter(&subject, &mut req.state).await?;
        if self.gate.permits(&subject, &ConsentPurpose::SessionContinuity).await? {
            self.durable.create(req).await
        } else {
            self.ephemeral.create(req).await
        }
    }

    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        if self.gate.subject_of(&req.user_id).is_none() {
            return self.durable.get(req).await;
        }
        match self.durable.get(req.clone()).await {
            Err(error) if error.is_not_found() => self.ephemeral.get(req).await,
            found => found,
        }
    }

    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>> {
        if self.gate.subject_of(&req.user_id).is_none() {
            return self.durable.list(req).await;
        }
        let all = ListRequest { limit: None, offset: None, ..req.clone() };
        let mut sessions = self.durable.list(all.clone()).await?;
        sessions.extend(self.ephemeral.list(all).await?);
        sessions.sort_by(|a, b| {
            b.last_update_time().cmp(&a.last_update_time()).then_with(|| b.id().cmp(a.id()))
        });
        let offset = req.offset.unwrap_or(0);
        Ok(sessions.into_iter().skip(offset).take(req.limit.unwrap_or(usize::MAX)).collect())
    }

    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        if self.gate.subject_of(&req.user_id).is_some() {
            self.ephemeral.delete(req.clone()).await?;
        }
        self.durable.delete(req).await
    }

    async fn append_event(&self, session_id: &str, event: Event) -> Result<()> {
        match self.durable.append_event(session_id, event.clone()).await {
            Err(error) if error.is_not_found() => {
                self.ephemeral.append_event(session_id, event).await
            }
            appended => appended,
        }
    }

    async fn append_event_for_identity(&self, mut req: AppendEventRequest) -> Result<()> {
        let Some(subject) = self.writable(req.identity.user_id.as_ref()).await? else {
            return self.durable.append_event_for_identity(req).await;
        };
        self.profile_filter(&subject, &mut req.event.actions.state_delta).await?;
        let holder = self.holder(&req.identity).await?;
        if Arc::ptr_eq(holder, &self.durable)
            && !self.gate.permits(&subject, &ConsentPurpose::SessionContinuity).await?
        {
            return Err(consent_required_error(&ConsentPurpose::SessionContinuity));
        }
        holder.append_event_for_identity(req).await
    }

    async fn delete_all_sessions(&self, app_name: &str, user_id: &str) -> Result<()> {
        if self.gate.subject_of(user_id).is_some() {
            self.ephemeral.delete_all_sessions(app_name, user_id).await?;
        }
        self.durable.delete_all_sessions(app_name, user_id).await
    }

    async fn rewind(
        &self,
        identity: &AdkIdentity,
        target_event_id: &str,
    ) -> Result<Box<dyn Session>> {
        self.holder(identity).await?.rewind(identity, target_event_id).await
    }

    async fn rewind_steps(&self, identity: &AdkIdentity, steps: usize) -> Result<Box<dyn Session>> {
        self.holder(identity).await?.rewind_steps(identity, steps).await
    }

    async fn health_check(&self) -> Result<()> {
        self.durable.health_check().await
    }
}
