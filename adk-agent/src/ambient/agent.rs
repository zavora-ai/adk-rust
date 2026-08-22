use std::sync::Arc;

use adk_core::{AdkError, Agent, Event, EventStream, Result};
use futures::StreamExt;
use tokio::sync::{Notify, RwLock, Semaphore, mpsc};
use tokio::task::JoinHandle;

use super::event_source::EventSource;

/// Callback invoked when the ambient agent's event source fires.
///
/// Receives the trigger event and the agent reference. The callback is responsible
/// for creating an appropriate `InvocationContext` (e.g. via a Runner) and invoking
/// the agent. Return the resulting event stream for the ambient agent to consume.
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use adk_agent::ambient::{AmbientAgent, TriggerHandler};
///
/// let handler: TriggerHandler = Arc::new(move |event, agent| {
///     let runner = runner.clone();
///     Box::pin(async move {
///         // Use the event payload as user content and run through a Runner
///         let content = Content::new("user").with_text(&event.payload.to_string());
///         runner.run("user".into(), "session".into(), content).await
///     })
/// });
/// ```
pub type TriggerHandler = Arc<
    dyn Fn(
            super::event_source::TriggerEvent,
            Arc<dyn Agent>,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<EventStream>> + Send>>
        + Send
        + Sync,
>;

/// Lifecycle status of an [`AmbientAgent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmbientAgentStatus {
    /// The agent is actively processing events.
    Running,
    /// The agent is paused — subscription is alive but events are buffered, not processed.
    Paused,
    /// The agent is stopped — no background task is running.
    Stopped,
}
/// Triggers handled at once unless [`AmbientAgent::with_max_concurrent_triggers`] says otherwise.
const DEFAULT_MAX_CONCURRENT_TRIGGERS: usize = 4;

/// A background agent triggered by an event source.
///
/// Wraps an [`Agent`] and an [`EventSource`], providing lifecycle control
/// (start, stop, pause, resume) over the background event processing loop.
///
/// # Lifecycle
///
/// ```text
/// Stopped → start() → Running → pause() → Paused → resume() → Running
///                        │                     │
///                        └── stop() → Stopped ←┘
/// ```
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use adk_agent::ambient::{AmbientAgent, CronTrigger};
///
/// let trigger = CronTrigger::new("0 * * * * *")?;
/// let mut ambient = AmbientAgent::new(agent, Arc::new(trigger));
/// ambient.start().await?;
/// // ... later
/// ambient.stop().await?;
/// ```
pub struct AmbientAgent {
    pub(super) agent: Arc<dyn Agent>,
    source: Arc<dyn EventSource>,
    trigger_handler: Option<TriggerHandler>,
    status: Arc<RwLock<AmbientAgentStatus>>,
    resume_notify: Arc<Notify>,
    handle: Option<JoinHandle<()>>,
    /// Bounds how many triggers are handled at once.
    ///
    /// Events used to be handled strictly one at a time — the loop drained a handler's whole
    /// event stream before polling the source again — so one slow trigger blocked every later
    /// one.
    max_concurrent_triggers: usize,
    /// Receives events the agent produces, when a caller asks for them.
    output_tx: Option<mpsc::Sender<Result<Event>>>,
}

impl AmbientAgent {
    /// Create a new ambient agent wrapping the given agent and event source.
    ///
    /// The agent starts in [`AmbientAgentStatus::Stopped`] state.
    pub fn new(agent: Arc<dyn Agent>, source: Arc<dyn EventSource>) -> Self {
        Self {
            agent,
            source,
            trigger_handler: None,
            status: Arc::new(RwLock::new(AmbientAgentStatus::Stopped)),
            resume_notify: Arc::new(Notify::new()),
            handle: None,
            max_concurrent_triggers: DEFAULT_MAX_CONCURRENT_TRIGGERS,
            output_tx: None,
        }
    }

    /// Bounds how many triggers are handled concurrently. Defaults to four.
    ///
    /// A bound of zero is treated as one.
    pub fn with_max_concurrent_triggers(mut self, max_concurrent_triggers: usize) -> Self {
        self.max_concurrent_triggers = max_concurrent_triggers.max(1);
        self
    }

    /// Delivers the events the agent produces to the returned receiver.
    ///
    /// Without this, produced events were logged at debug level and discarded, so a caller had
    /// no way to observe what an ambient run did. Errors are delivered too, so a failing trigger
    /// is visible rather than only logged.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let mut outputs = ambient.take_output(64);
    /// ambient.start().await?;
    /// while let Some(event) = outputs.recv().await {
    ///     // observe what each trigger produced
    /// }
    /// ```
    pub fn take_output(&mut self, capacity: usize) -> mpsc::Receiver<Result<Event>> {
        let (tx, rx) = mpsc::channel(capacity.max(1));
        self.output_tx = Some(tx);
        rx
    }

    /// Set a trigger handler that will be called when the event source fires.
    ///
    /// The handler receives the trigger event and agent, and should invoke the
    /// agent via a Runner or other mechanism. Without a handler, the ambient
    /// agent only logs trigger events.
    pub fn with_trigger_handler(mut self, handler: TriggerHandler) -> Self {
        self.trigger_handler = Some(handler);
        self
    }

    /// Start listening for events and invoking the agent.
    ///
    /// # Errors
    ///
    /// Returns an error if the agent is already running or paused, or if no trigger handler is
    /// configured.
    ///
    /// Starting without a handler used to succeed and then only log each trigger, so
    /// `AmbientAgent::new(..).start()` looked like it was running the agent while the agent was
    /// never invoked. Refusing makes that visible at the call site.
    pub async fn start(&mut self) -> Result<()> {
        let current = *self.status.read().await;
        if current != AmbientAgentStatus::Stopped {
            return Err(AdkError::agent("agent already running"));
        }

        if self.trigger_handler.is_none() {
            return Err(AdkError::agent(
                "AmbientAgent has no trigger handler, so starting it would log trigger events \
                 without ever invoking the agent. Call `with_trigger_handler` with a closure \
                 that drives the agent through a Runner.",
            ));
        }

        // Subscribe to the event source
        let stream = self.source.subscribe().await?;

        let status = Arc::clone(&self.status);
        let resume_notify = Arc::clone(&self.resume_notify);
        let agent = Arc::clone(&self.agent);
        let trigger_handler = self.trigger_handler.clone();

        *self.status.write().await = AmbientAgentStatus::Running;

        let permits = Arc::new(Semaphore::new(self.max_concurrent_triggers));
        let output_tx = self.output_tx.clone();
        let handler = trigger_handler.expect("checked above");

        let handle = tokio::spawn(async move {
            let mut stream = stream;
            let mut running = futures::stream::FuturesUnordered::new();

            loop {
                // Check if paused — wait until resumed.
                loop {
                    let current_status = *status.read().await;
                    match current_status {
                        AmbientAgentStatus::Running => break,
                        AmbientAgentStatus::Paused => resume_notify.notified().await,
                        AmbientAgentStatus::Stopped => return,
                    }
                }

                // Draining finished triggers alongside reading the source is what removes the
                // head-of-line blocking: one slow trigger no longer holds up every later one.
                let event = if running.is_empty() {
                    stream.next().await
                } else {
                    tokio::select! {
                        biased;
                        Some(()) = running.next() => continue,
                        event = stream.next() => event,
                    }
                };

                let Some(event) = event else {
                    // Source exhausted: let dispatched triggers finish so their output is not
                    // dropped mid-flight.
                    while running.next().await.is_some() {}
                    return;
                };

                let handler = Arc::clone(&handler);
                let agent = Arc::clone(&agent);
                let permits = Arc::clone(&permits);
                let output_tx = output_tx.clone();

                running.push(async move {
                    let _permit = permits.acquire_owned().await;

                    tracing::info!(
                        agent = agent.name(),
                        source = %event.source,
                        "ambient agent triggered"
                    );

                    match handler(event, Arc::clone(&agent)).await {
                        Ok(mut event_stream) => {
                            while let Some(result) = event_stream.next().await {
                                let failed = result.is_err();
                                if let Err(ref e) = result {
                                    tracing::warn!(error = %e, "ambient agent invocation error");
                                }

                                // Delivered when a caller asked for output, instead of being
                                // logged and dropped.
                                if let Some(ref tx) = output_tx
                                    && tx.send(result).await.is_err()
                                {
                                    tracing::debug!("ambient output receiver dropped");
                                    return;
                                }

                                if failed {
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "ambient agent trigger handler failed");
                            if let Some(ref tx) = output_tx {
                                let _ = tx.send(Err(e)).await;
                            }
                        }
                    }
                });
            }
        });

        self.handle = Some(handle);
        Ok(())
    }

    /// Stop the agent and cancel in-progress work.
    ///
    /// # Errors
    ///
    /// Returns an error if the agent is already stopped.
    pub async fn stop(&mut self) -> Result<()> {
        let current = *self.status.read().await;
        if current == AmbientAgentStatus::Stopped {
            return Err(AdkError::agent("agent already stopped"));
        }

        *self.status.write().await = AmbientAgentStatus::Stopped;

        // Wake the task if paused so it can observe the Stopped state
        self.resume_notify.notify_one();

        if let Some(handle) = self.handle.take() {
            handle.abort();
        }

        Ok(())
    }

    /// Pause event processing. The subscription remains alive but events are buffered.
    ///
    /// # Errors
    ///
    /// Returns an error if the agent is not currently running.
    pub async fn pause(&mut self) -> Result<()> {
        let current = *self.status.read().await;
        if current != AmbientAgentStatus::Running {
            return Err(AdkError::agent("can only pause a running agent"));
        }

        *self.status.write().await = AmbientAgentStatus::Paused;
        Ok(())
    }

    /// Resume event processing after a pause.
    ///
    /// # Errors
    ///
    /// Returns an error if the agent is not currently paused.
    pub async fn resume(&mut self) -> Result<()> {
        let current = *self.status.read().await;
        if current != AmbientAgentStatus::Paused {
            return Err(AdkError::agent("can only resume a paused agent"));
        }

        *self.status.write().await = AmbientAgentStatus::Running;
        self.resume_notify.notify_one();
        Ok(())
    }

    /// Read the current lifecycle status.
    pub async fn status(&self) -> AmbientAgentStatus {
        *self.status.read().await
    }
}

impl Drop for AmbientAgent {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

impl std::fmt::Debug for AmbientAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AmbientAgent")
            .field("agent", &self.agent.name())
            .field("source", &self.source.name())
            .finish()
    }
}
