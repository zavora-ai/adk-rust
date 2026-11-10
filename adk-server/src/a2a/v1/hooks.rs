//! Hooks into an A2A v1 agent run, as adk-go's A2A executor offers them
//! (`ExecutorConfig`: `BeforeExecuteCallback`, `AfterEventCallback`, `AfterExecuteCallback`,
//! `A2APartConverter`, `GenAIPartConverter`, `A2AExecutionCleanupCallback`).
//!
//! Set them on an [`A2aV1ServerBuilder`](super::server::A2aV1ServerBuilder) or a
//! [`RequestHandler`](super::request_handler::RequestHandler):
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use adk_server::a2a::v1::hooks::ExecutorHooks;
//!
//! let hooks = ExecutorHooks::default()
//!     .after_event(Arc::new(|run, _event, update| {
//!         let meta = update.metadata.get_or_insert_with(Default::default);
//!         meta.insert("tenant".into(), run.user_id.clone().into());
//!         Ok(())
//!     }));
//! let server = A2aV1Server::builder(runner_config, base_url).hooks(hooks).build();
//! ```

use std::sync::Arc;

use a2a_protocol_types::Message;
use a2a_protocol_types::Part as WirePart;
use a2a_protocol_types::events::TaskStatusUpdateEvent;
use futures::future::BoxFuture;

use super::error::A2aError;
pub use super::processor::{ArtifactUpdate, FinalStatus};
use crate::auth_bridge::RequestContext;

/// The run a hook is called for.
#[derive(Debug, Clone)]
pub struct ExecutionContext {
    /// The task the run is for.
    pub task_id: String,
    /// The task's context, which is also the ADK session ID.
    pub context_id: String,
    /// The ADK app the agent runs in.
    pub app_name: String,
    /// The ADK user whose session the run uses.
    pub user_id: String,
    /// The ADK session the run uses.
    pub session_id: String,
    /// The authenticated caller, if any.
    pub caller: Option<RequestContext>,
    /// The message the run answers.
    pub message: Message,
    /// The extensions the caller asked for (`A2A-Extensions`).
    pub extensions: Vec<String>,
}

/// Called before the agent runs. An error fails the task with
/// `before execute: <error>`, and the agent does not run.
pub type BeforeExecute =
    Arc<dyn Fn(Arc<ExecutionContext>) -> BoxFuture<'static, Result<(), A2aError>> + Send + Sync>;

/// Called for each artifact update an agent event produces, before it is stored and sent;
/// it may change the update. An error ends the run with `processor failed: <error>`.
pub type AfterEvent = Arc<
    dyn Fn(&ExecutionContext, &adk_core::Event, &mut ArtifactUpdate) -> Result<(), A2aError>
        + Send
        + Sync,
>;

/// Called with the status a run ends on, before it is stored and sent; it may change it.
/// An error fails the task with `after execute: <error>`.
pub type AfterExecute =
    Arc<dyn Fn(&ExecutionContext, &mut FinalStatus) -> Result<(), A2aError> + Send + Sync>;

/// Turns one of an agent event's parts into the A2A part a caller sees, replacing the
/// default conversion (and its `adk_type` tagging); `None` leaves the part out.
pub type ToWirePart = Arc<
    dyn Fn(&adk_core::Event, &adk_core::Part) -> Result<Option<WirePart>, A2aError> + Send + Sync,
>;

/// Turns one of the caller's A2A parts into the ADK part the agent sees, replacing the
/// default conversion; `None` leaves the part out.
pub type FromWirePart =
    Arc<dyn Fn(&WirePart) -> Result<Option<adk_core::Part>, A2aError> + Send + Sync>;

/// Called once a run has ended or was cancelled, with the status it ended on.
pub type OnCleanup = Arc<dyn Fn(&ExecutionContext, &TaskStatusUpdateEvent) + Send + Sync>;

/// The hooks of an A2A v1 run; see the [module documentation](self). All are optional.
#[derive(Clone, Default)]
pub struct ExecutorHooks {
    pub(super) before_execute: Option<BeforeExecute>,
    pub(super) after_event: Option<AfterEvent>,
    pub(super) after_execute: Option<AfterExecute>,
    pub(super) to_wire_part: Option<ToWirePart>,
    pub(super) from_wire_part: Option<FromWirePart>,
    pub(super) on_cleanup: Option<OnCleanup>,
}

impl ExecutorHooks {
    /// Calls `hook` before the agent runs; see [`BeforeExecute`].
    #[must_use]
    pub fn before_execute(mut self, hook: BeforeExecute) -> Self {
        self.before_execute = Some(hook);
        self
    }

    /// Calls `hook` for each artifact update; see [`AfterEvent`].
    #[must_use]
    pub fn after_event(mut self, hook: AfterEvent) -> Self {
        self.after_event = Some(hook);
        self
    }

    /// Calls `hook` with the final status; see [`AfterExecute`].
    #[must_use]
    pub fn after_execute(mut self, hook: AfterExecute) -> Self {
        self.after_execute = Some(hook);
        self
    }

    /// Converts agent parts for callers with `converter`; see [`ToWirePart`].
    #[must_use]
    pub fn to_wire_part(mut self, converter: ToWirePart) -> Self {
        self.to_wire_part = Some(converter);
        self
    }

    /// Converts caller parts for the agent with `converter`; see [`FromWirePart`].
    #[must_use]
    pub fn from_wire_part(mut self, converter: FromWirePart) -> Self {
        self.from_wire_part = Some(converter);
        self
    }

    /// Calls `hook` when a run has ended; see [`OnCleanup`].
    #[must_use]
    pub fn on_cleanup(mut self, hook: OnCleanup) -> Self {
        self.on_cleanup = Some(hook);
        self
    }
}

impl std::fmt::Debug for ExecutorHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutorHooks")
            .field("before_execute", &self.before_execute.is_some())
            .field("after_event", &self.after_event.is_some())
            .field("after_execute", &self.after_execute.is_some())
            .field("to_wire_part", &self.to_wire_part.is_some())
            .field("from_wire_part", &self.from_wire_part.is_some())
            .field("on_cleanup", &self.on_cleanup.is_some())
            .finish()
    }
}
