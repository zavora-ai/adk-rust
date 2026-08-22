//! [`AgentInvoker`] for [`Runner`].
//!
//! [`Runner::run`] resolves an *existing* session and yields `session.not_found` through the
//! stream when there is none. A caller driving an agent from an external event — a cron tick, a
//! webhook, a queue message — has no opportunity to register a session first, so every such
//! caller ended up writing the same create-then-run dance. This implementation owns it.

use adk_core::{AgentInvoker, Content, EventStream, Result};
use async_trait::async_trait;

use crate::Runner;

#[async_trait]
impl AgentInvoker for Runner {
    async fn invoke(
        &self,
        user_id: &str,
        session_id: &str,
        content: Content,
    ) -> Result<EventStream> {
        let existing = self
            .session_service()
            .get(adk_session::GetRequest {
                app_name: self.app_name().to_string(),
                user_id: user_id.to_string(),
                session_id: session_id.to_string(),
                num_recent_events: None,
                after: None,
            })
            .await;

        match existing {
            Ok(_) => {}
            // Only a genuine absence is created through. A backend or transport failure must
            // surface, not be papered over with a fresh session that silently discards history.
            Err(error) if error.is_not_found() => {
                self.session_service()
                    .create(adk_session::CreateRequest {
                        app_name: self.app_name().to_string(),
                        user_id: user_id.to_string(),
                        session_id: Some(session_id.to_string()),
                        state: std::collections::HashMap::new(),
                    })
                    .await?;
                tracing::debug!(
                    user_id,
                    session_id,
                    "created a session for an externally triggered invocation"
                );
            }
            Err(error) => return Err(error),
        }

        self.run_str(user_id, session_id, content).await
    }
}
