//! The managed control plane must reflect the data plane.
//!
//! Two disconnects:
//!
//! 1. `delete_session` archived the handle and dropped it from the in-memory map, but never
//!    called `SessionService::delete` — even though `start_session` seeded a persistent
//!    session and the Runner appended every turn to it. The API reported deletion while the
//!    conversation stayed in the configured backend, outliving the process that "deleted" it.
//! 2. `ActiveSession` owned the `Arc<RwLock<SessionStatus>>` a caller reads, while
//!    `SessionLoop` owned a separate plain field. Normal queued → running → idle transitions
//!    updated only the loop's copy, so a session doing work kept reporting `Queued`. The
//!    struct comment claimed the two were shared.

use adk_core::{Content, FinishReason, Llm, LlmRequest, LlmResponse, LlmResponseStream};
use adk_managed::resolver::{ModelResolver, ResolverResult};
use adk_managed::types::{ContentBlock, ManagedAgentDef, ModelRef, SessionStatus, UserEvent};
use adk_managed::{DefaultManagedAgentRuntime, ManagedAgentRuntime};
use adk_session::InMemorySessionService;
use adk_session::service::{GetRequest, SessionService};
use async_trait::async_trait;
use std::sync::Arc;

/// A model that answers without a network call.
struct SilentModel;

#[async_trait]
impl Llm for SilentModel {
    fn name(&self) -> &str {
        "silent"
    }
    async fn generate_content(
        &self,
        _request: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let response = LlmResponse {
            content: Some(Content::new("model").with_text("ok")),
            partial: false,
            turn_complete: true,
            finish_reason: Some(FinishReason::Stop),
            ..Default::default()
        };
        Ok(Box::pin(async_stream::stream! { yield Ok(response); }))
    }
}

/// Resolves every model reference to the silent model.
struct SilentResolver;

#[async_trait]
impl ModelResolver for SilentResolver {
    async fn resolve(&self, _model: &ModelRef) -> ResolverResult<Arc<dyn Llm>> {
        Ok(Arc::new(SilentModel) as Arc<dyn Llm>)
    }
}

/// The identity `start_session` persists a managed session under.
const MANAGED_APP: &str = "managed";
const MANAGED_USER: &str = "managed_user";

/// Builds a runtime over a session service the test can inspect directly.
fn runtime_with_service() -> (DefaultManagedAgentRuntime, Arc<InMemorySessionService>) {
    let service = Arc::new(InMemorySessionService::new());
    let runtime = DefaultManagedAgentRuntime::new(
        Arc::new(SilentResolver) as Arc<dyn ModelResolver>,
        Arc::clone(&service) as Arc<dyn SessionService>,
    );
    (runtime, service)
}

/// A minimal agent definition; no model call is made by these tests.
fn agent_def(name: &str) -> ManagedAgentDef {
    ManagedAgentDef::new(name, ModelRef::Shorthand("silent".to_string()))
}

/// Whether the backend still holds the session.
async fn session_exists(service: &InMemorySessionService, session_id: &str) -> bool {
    service
        .get(GetRequest {
            app_name: MANAGED_APP.to_string(),
            user_id: MANAGED_USER.to_string(),
            session_id: session_id.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .map(|_| true)
        .unwrap_or(false)
}

#[tokio::test]
async fn deleting_a_managed_session_removes_its_persisted_conversation() {
    let (runtime, service) = runtime_with_service();
    let agent = runtime.create(agent_def("deleter")).await.expect("agent");
    let session = runtime.start_session(&agent, None).await.expect("session");

    assert!(
        session_exists(&service, session.0.as_str()).await,
        "start_session must seed a persistent session for the Runner to append to"
    );

    runtime.delete_session(&session).await.expect("delete");

    assert!(
        !session_exists(&service, session.0.as_str()).await,
        "the persisted conversation survived a reported deletion"
    );
}

#[tokio::test]
async fn deleting_an_unknown_session_is_still_reported_as_not_found() {
    let (runtime, _service) = runtime_with_service();
    let agent = runtime.create(agent_def("deleter")).await.expect("agent");
    let session = runtime.start_session(&agent, None).await.expect("session");

    runtime.delete_session(&session).await.expect("first delete");

    // The handle is gone, so a second delete must not silently succeed.
    assert!(
        runtime.delete_session(&session).await.is_err(),
        "deleting an already-deleted session must report not found"
    );
}

#[tokio::test]
async fn a_new_session_reports_queued_through_the_public_handle() {
    let (runtime, _service) = runtime_with_service();
    let agent = runtime.create(agent_def("reporter")).await.expect("agent");
    let session = runtime.start_session(&agent, None).await.expect("session");

    // The starting point. What matters is that this handle is the one the loop writes to,
    // asserted below.
    assert_eq!(runtime.status(&session).await.expect("status"), SessionStatus::Queued);
}

#[tokio::test]
async fn a_working_session_stops_reporting_queued() {
    let (runtime, _service) = runtime_with_service();
    let agent = runtime.create(agent_def("reporter")).await.expect("agent");
    let session = runtime.start_session(&agent, None).await.expect("session");

    runtime
        .send_event(
            &session,
            UserEvent::Message { content: vec![ContentBlock::Text { text: "hi".into() }] },
        )
        .await
        .expect("send");

    // The loop transitions queued → running → idle. Before the status handle was shared,
    // those writes landed on the loop's own field and this stayed `Queued` forever.
    let moved_on = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let status = runtime.status(&session).await.expect("status");
            if status != SessionStatus::Queued {
                return status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;

    let status = moved_on.expect("a session that has done work must stop reporting Queued");
    assert!(
        matches!(status, SessionStatus::Running | SessionStatus::Idle),
        "expected a normal working transition, saw {status:?}"
    );
}

#[tokio::test]
async fn archive_is_visible_through_the_public_handle() {
    let (runtime, _service) = runtime_with_service();
    let agent = runtime.create(agent_def("reporter")).await.expect("agent");
    let session = runtime.start_session(&agent, None).await.expect("session");

    runtime.archive(&session).await.expect("archive");

    assert_eq!(runtime.status(&session).await.expect("status"), SessionStatus::Archived);
}
