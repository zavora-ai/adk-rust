//! The subject data store suite over a session backend: the harness writes sessions with
//! events and `user:` state, and counts what the backend's read paths return.
// Each test binary that declares `mod common` uses a different subset of these checks.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use adk_core::async_trait;
use adk_core::conformance::SubjectDataStoreHarness;
use adk_core::subject::SubjectDataStore;
use adk_session::{
    AppendEventRequest, CreateRequest, DeleteRequest, Event, GetRequest, ListRequest,
    SessionService, SessionSubjectData,
};
use serde_json::json;

/// Opens a session backend; each call returns a handle over the same data.
#[async_trait]
pub trait OpenSessions: Send + Sync {
    async fn service(&self) -> Arc<dyn SessionService>;
}

/// The [`SessionSubjectData`] adapter over the backend `O` opens.
pub struct SessionData<O>(pub O);

#[async_trait]
impl<O: OpenSessions> SubjectDataStoreHarness for SessionData<O> {
    async fn open(&self) -> Arc<dyn SubjectDataStore> {
        Arc::new(SessionSubjectData::new(self.0.service().await))
    }

    async fn seed(&self, app_name: &str, user_id: &str) {
        let service = self.0.service().await;
        let session = service
            .create(CreateRequest {
                app_name: app_name.to_owned(),
                user_id: user_id.to_owned(),
                session_id: None,
                state: HashMap::from([
                    ("user:language".to_owned(), json!("sw")),
                    ("cart".to_owned(), json!(["kikoi"])),
                ]),
            })
            .await
            .expect("create session");
        let identity = session.try_identity().expect("valid identity");
        let mut event = Event::new("inv-subject");
        event.author = "user".to_owned();
        event.llm_response.content = Some(adk_core::Content::new("user").with_text("hujambo"));
        service
            .append_event_for_identity(AppendEventRequest { identity, event })
            .await
            .expect("append event");
    }

    async fn visible(&self, app_name: &str, user_id: &str) -> usize {
        let service = self.0.service().await;
        let sessions = service
            .list(ListRequest {
                app_name: app_name.to_owned(),
                user_id: user_id.to_owned(),
                limit: None,
                offset: None,
            })
            .await
            .expect("list sessions");
        let mut count = sessions.len();
        for session in &sessions {
            let full = service
                .get(GetRequest {
                    app_name: app_name.to_owned(),
                    user_id: user_id.to_owned(),
                    session_id: session.id().to_owned(),
                    num_recent_events: None,
                    after: None,
                })
                .await
                .expect("get session");
            count += full.events().len();
        }
        // A fresh session reads the `user:` tier, so leftover profile data shows up here.
        let probe = service
            .create(CreateRequest {
                app_name: app_name.to_owned(),
                user_id: user_id.to_owned(),
                session_id: None,
                state: HashMap::new(),
            })
            .await
            .expect("create probe session");
        count += probe.state().all().keys().filter(|key| key.starts_with("user:")).count();
        service
            .delete(DeleteRequest {
                app_name: app_name.to_owned(),
                user_id: user_id.to_owned(),
                session_id: probe.id().to_owned(),
            })
            .await
            .expect("delete probe session");
        count
    }
}
