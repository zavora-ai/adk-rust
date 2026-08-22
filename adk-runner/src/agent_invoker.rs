//! [`AgentInvoker`] for [`Runner`].
//!
//! [`Runner::run`] resolves an *existing* session and yields `session.not_found` through the
//! stream when there is none. A caller driving an agent from an external event — a cron tick, a
//! webhook, a queue message — has no opportunity to register a session first, so every such
//! caller ended up writing the same create-then-run dance. This implementation owns it.

use std::sync::{Arc, Weak};

use adk_core::{Agent, AgentInvoker, Content, EventStream, Result};
use async_trait::async_trait;
use futures::StreamExt;
use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::Runner;

type SessionLockRegistry =
    Arc<std::sync::Mutex<std::collections::HashMap<String, Weak<Mutex<()>>>>>;

/// Keeps one session lock held for the lifetime of the returned event stream and removes an idle
/// weak registry entry when the stream completes or is dropped.
struct SessionInvocationLease {
    key: String,
    registry: SessionLockRegistry,
    _guard: OwnedMutexGuard<()>,
}

impl Drop for SessionInvocationLease {
    fn drop(&mut self) {
        let mut registry = self.registry.lock().unwrap_or_else(|error| error.into_inner());
        if registry.get(&self.key).is_some_and(|lock| lock.strong_count() == 1) {
            registry.remove(&self.key);
        }
    }
}

#[async_trait]
impl AgentInvoker for Runner {
    fn agent(&self) -> Option<Arc<dyn Agent>> {
        Some(self.root_agent())
    }

    async fn invoke(
        &self,
        user_id: &str,
        session_id: &str,
        content: Content,
    ) -> Result<EventStream> {
        let lock_key = format!("{}\0{user_id}\0{session_id}", self.app_name());
        let session_lock = {
            let mut registry =
                self.external_session_locks.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(lock) = registry.get(&lock_key).and_then(Weak::upgrade) {
                lock
            } else {
                let lock = Arc::new(Mutex::new(()));
                registry.insert(lock_key.clone(), Arc::downgrade(&lock));
                lock
            }
        };
        let guard = session_lock.lock_owned().await;
        let lease = SessionInvocationLease {
            key: lock_key,
            registry: Arc::clone(&self.external_session_locks),
            _guard: guard,
        };

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
                let created = self
                    .session_service()
                    .create(adk_session::CreateRequest {
                        app_name: self.app_name().to_string(),
                        user_id: user_id.to_string(),
                        session_id: Some(session_id.to_string()),
                        state: std::collections::HashMap::new(),
                    })
                    .await;
                if let Err(create_error) = created {
                    // Another runner or process may have won the create race. Only suppress the
                    // error when a fresh lookup proves the requested session now exists.
                    if self
                        .session_service()
                        .get(adk_session::GetRequest {
                            app_name: self.app_name().to_string(),
                            user_id: user_id.to_string(),
                            session_id: session_id.to_string(),
                            num_recent_events: None,
                            after: None,
                        })
                        .await
                        .is_err()
                    {
                        return Err(create_error);
                    }
                }
                tracing::debug!(
                    user_id,
                    session_id,
                    "session is ready for an externally triggered invocation"
                );
            }
            Err(error) => return Err(error),
        }

        let mut events = self.run_str(user_id, session_id, content).await?;

        Ok(Box::pin(async_stream::stream! {
            let _lease = lease;
            while let Some(event) = events.next().await {
                yield event;
            }
        }))
    }
}
