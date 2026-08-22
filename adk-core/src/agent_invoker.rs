use std::sync::Arc;

use crate::{Agent, Content, EventStream, Result};
use async_trait::async_trait;

/// Starts an agent turn for a session, creating the session when it does not exist.
///
/// Callers that hand work to an agent from outside a conversation — a background trigger, a
/// queue consumer, a scheduler — need one operation: "run this content through the agent and
/// give me the events." They should not have to know which session service holds the session, or
/// that a session must be registered before a turn can start.
///
/// `adk-runner` implements this for `Runner`, so a caller can accept `Arc<dyn AgentInvoker>` and
/// stay independent of the runner's construction. Implementations are responsible for creating a
/// missing session rather than failing, because an external event has no opportunity to register
/// one first. Implementations that permit concurrent calls should also serialize turns targeting
/// the same session until the returned event stream completes or is dropped.
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use adk_core::{AgentInvoker, Content};
///
/// async fn on_event(invoker: Arc<dyn AgentInvoker>) -> adk_core::Result<()> {
///     let mut events = invoker
///         .invoke("system", "nightly-sweep", Content::new("user").with_text("run the sweep"))
///         .await?;
///     while let Some(event) = futures::StreamExt::next(&mut events).await {
///         let _ = event?;
///     }
///     Ok(())
/// }
/// ```
#[async_trait]
pub trait AgentInvoker: Send + Sync {
    /// Returns the agent this invoker executes when it can expose one.
    ///
    /// Wrappers that do not own an in-process agent may keep the default. Consumers use this to
    /// align diagnostics and lifecycle metadata with the executable root without coupling to a
    /// concrete runner type.
    fn agent(&self) -> Option<Arc<dyn Agent>> {
        None
    }

    /// Starts a turn for `(user_id, session_id)` with `content` and returns the event stream.
    ///
    /// # Errors
    ///
    /// Returns an error if either identifier fails validation, if the session cannot be created
    /// or retrieved, or if invocation setup fails. Failures during agent execution are yielded
    /// by the returned stream rather than returned here.
    async fn invoke(
        &self,
        user_id: &str,
        session_id: &str,
        content: Content,
    ) -> Result<EventStream>;
}
