//! Driving an agent from a trigger without hand-rolling the invocation.
//!
//! [`TriggerHandler`](crate::ambient::TriggerHandler) hands a closure a [`TriggerEvent`] and an agent, and
//! leaves it to build content, pick a session, and start a run. Every ambient caller therefore
//! wrote the same three things, and getting the session wrong fails inside the event stream
//! rather than at the call site. [`AmbientAgent::with_invoker`](super::AmbientAgent::with_invoker)
//! supplies that wiring.

use std::sync::Arc;

use adk_core::{AgentInvoker, Content};

use super::agent::{AmbientAgent, TriggerHandler};
use super::event_source::TriggerEvent;

/// Builds the prompt text a trigger event is turned into.
type PromptFn = Arc<dyn Fn(&TriggerEvent) -> String + Send + Sync>;

/// How trigger events map onto sessions.
#[derive(Debug, Clone, Default)]
pub enum TriggerSessionPolicy {
    /// A fresh session per trigger, so each run starts with no history.
    ///
    /// The default. A schedule that fires every minute into one shared session would grow that
    /// session's history — and the token cost of every subsequent run — without bound.
    #[default]
    PerTrigger,
    /// One session reused by every trigger, carrying history forward.
    ///
    /// Suits a trigger that should accumulate context, and only where the run frequency is low
    /// enough that unbounded history growth is acceptable. `Runner` serializes externally invoked
    /// turns for the same shared session so their history cannot overlap.
    Shared(String),
}

/// Configures how [`AmbientAgent::with_invoker`](super::AmbientAgent::with_invoker) turns trigger
/// events into agent runs.
///
/// # Example
///
/// ```rust,ignore
/// use adk_agent::ambient::{RunnerTriggerConfig, TriggerSessionPolicy};
///
/// let config = RunnerTriggerConfig::new("system")
///     .with_session_policy(TriggerSessionPolicy::Shared("nightly-sweep".into()))
///     .with_prompt(|event| format!("Disk sweep triggered by {}", event.source));
/// ```
#[derive(Clone)]
pub struct RunnerTriggerConfig {
    user_id: String,
    session_policy: TriggerSessionPolicy,
    prompt: PromptFn,
}

impl RunnerTriggerConfig {
    /// Creates a configuration attributing runs to `user_id`.
    ///
    /// A trigger has no interactive user, so this is the identity the runs are recorded under —
    /// `"system"` or a service account name rather than a person.
    pub fn new(user_id: impl Into<String>) -> Self {
        Self {
            user_id: user_id.into(),
            session_policy: TriggerSessionPolicy::default(),
            prompt: Arc::new(default_prompt),
        }
    }

    /// Sets how trigger events map onto sessions. Defaults to
    /// [`TriggerSessionPolicy::PerTrigger`].
    pub fn with_session_policy(mut self, policy: TriggerSessionPolicy) -> Self {
        self.session_policy = policy;
        self
    }

    /// Sets how a trigger event becomes prompt text.
    ///
    /// The default states the source and serializes the payload, which is rarely the phrasing an
    /// agent should act on. Supply the instruction the run is meant to carry out.
    pub fn with_prompt(
        mut self,
        prompt: impl Fn(&TriggerEvent) -> String + Send + Sync + 'static,
    ) -> Self {
        self.prompt = Arc::new(prompt);
        self
    }

    /// The identity runs are attributed to.
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// Resolves the session id for `event` under the configured policy.
    fn session_id(&self, event: &TriggerEvent) -> String {
        match &self.session_policy {
            TriggerSessionPolicy::PerTrigger => {
                format!("{}-{}", event.source, uuid::Uuid::new_v4())
            }
            TriggerSessionPolicy::Shared(session_id) => session_id.clone(),
        }
    }
}

impl std::fmt::Debug for RunnerTriggerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunnerTriggerConfig")
            .field("user_id", &self.user_id)
            .field("session_policy", &self.session_policy)
            .finish_non_exhaustive()
    }
}

/// States the source and payload. Callers are expected to replace this with the instruction the
/// run should carry out.
fn default_prompt(event: &TriggerEvent) -> String {
    format!("Triggered by {}. Payload: {}", event.source, event.payload)
}

impl AmbientAgent {
    /// Drives the agent through `invoker` on every trigger.
    ///
    /// Pass a `Runner`, which implements [`AgentInvoker`]. This replaces the hand-written
    /// [`with_trigger_handler`](AmbientAgent::with_trigger_handler) closure and creates the
    /// session for each run, which [`Runner::run`](adk_core::AgentInvoker) does not do on its
    /// own.
    ///
    /// When the invoker exposes its executable agent, as `Runner` does, the ambient wrapper adopts
    /// that agent for logging and diagnostics. Opaque invokers retain the agent supplied to
    /// [`AmbientAgent::new`].
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use std::sync::Arc;
    /// use adk_agent::ambient::{AmbientAgent, CronTrigger, RunnerTriggerConfig};
    ///
    /// let runner = Arc::new(Runner::builder()
    ///     .app_name("sweeper")
    ///     .agent(Arc::clone(&agent))
    ///     .session_service(sessions)
    ///     .build()?);
    ///
    /// let mut ambient = AmbientAgent::new(agent, Arc::new(CronTrigger::new("0 * * * * *")?))
    ///     .with_invoker(runner, RunnerTriggerConfig::new("system"));
    /// ambient.start().await?;
    /// ```
    pub fn with_invoker(
        mut self,
        invoker: Arc<dyn AgentInvoker>,
        config: RunnerTriggerConfig,
    ) -> Self {
        if let Some(executable_agent) = invoker.agent() {
            self.agent = executable_agent;
        }
        let handler: TriggerHandler = Arc::new(move |event, _agent| {
            let invoker = Arc::clone(&invoker);
            let config = config.clone();
            Box::pin(async move {
                let session_id = config.session_id(&event);
                let content = Content::new("user").with_text((config.prompt)(&event));
                invoker.invoke(&config.user_id, &session_id, content).await
            })
        });

        self.with_trigger_handler(handler)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> TriggerEvent {
        TriggerEvent {
            source: "cron:0 * * * * *".to_string(),
            payload: serde_json::json!({ "tick": "2026-08-22T13:45:00Z" }),
            principal: None,
        }
    }

    #[test]
    fn per_trigger_is_the_default_policy() {
        let config = RunnerTriggerConfig::new("system");

        assert!(matches!(config.session_policy, TriggerSessionPolicy::PerTrigger));
    }

    #[test]
    fn per_trigger_gives_every_event_its_own_session() {
        let config = RunnerTriggerConfig::new("system");

        let first = config.session_id(&event());
        let second = config.session_id(&event());

        assert_ne!(first, second, "a shared session would grow history without bound across ticks");
        assert!(first.starts_with("cron:0 * * * * *-"), "got {first}");
    }

    #[test]
    fn shared_reuses_one_session_across_events() {
        let config = RunnerTriggerConfig::new("system")
            .with_session_policy(TriggerSessionPolicy::Shared("sweep".to_string()));

        assert_eq!(config.session_id(&event()), "sweep");
        assert_eq!(config.session_id(&event()), "sweep");
    }

    #[test]
    fn the_default_prompt_names_the_source_and_payload() {
        let rendered = default_prompt(&event());

        assert!(rendered.contains("cron:0 * * * * *"), "got {rendered}");
        assert!(rendered.contains("2026-08-22T13:45:00Z"), "got {rendered}");
    }

    #[test]
    fn a_custom_prompt_replaces_the_default() {
        let config =
            RunnerTriggerConfig::new("system").with_prompt(|event| format!("go: {}", event.source));

        assert_eq!((config.prompt)(&event()), "go: cron:0 * * * * *");
    }

    #[test]
    fn user_id_is_reported() {
        assert_eq!(RunnerTriggerConfig::new("service-account").user_id(), "service-account");
    }
}
