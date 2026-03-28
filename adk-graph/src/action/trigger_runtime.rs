//! Trigger runtime for managing background trigger listeners.
//!
//! The `TriggerRuntime` manages webhook routes, cron schedules, and event
//! subscriptions that start graph execution when triggered. This module is
//! gated behind the `action-trigger` feature flag.
//!
//! - **Webhook**: Validates auth (bearer token or API key) and returns the
//!   payload. Actual HTTP route registration happens in `adk-server`.
//! - **Schedule**: Uses `tokio-cron-scheduler` for cron-based execution.
//! - **Event**: Uses `tokio::sync::mpsc` channels for event-driven execution.

use std::sync::Arc;

use adk_action::{EventConfig, ScheduleConfig, TriggerNodeConfig, TriggerType, WebhookAuthConfig};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::agent::GraphAgent;
use crate::node::ExecutionConfig;
use crate::state::State;

// ── TriggerRuntime ────────────────────────────────────────────────────

/// Manages background trigger listeners that start graph execution.
///
/// The runtime holds a reference to the graph agent and spawns background
/// tasks for each configured trigger (webhook, schedule, event).
pub struct TriggerRuntime {
    /// The graph agent to invoke when a trigger fires.
    graph: Arc<GraphAgent>,
    /// Trigger configurations extracted from the workflow.
    triggers: Vec<TriggerNodeConfig>,
    /// Shutdown signal sender — set to `true` to stop all background tasks.
    shutdown_tx: watch::Sender<bool>,
    /// Shutdown signal receiver (cloned for each background task).
    shutdown_rx: watch::Receiver<bool>,
    /// Event sender for external systems to push events into the runtime.
    event_tx: Option<mpsc::Sender<IncomingEvent>>,
    /// Event receiver consumed by the event listener task.
    event_rx: Option<mpsc::Receiver<IncomingEvent>>,
}

/// An incoming event from an external system.
#[derive(Debug, Clone)]
pub struct IncomingEvent {
    /// The source system that produced the event.
    pub source: String,
    /// The type of event.
    pub event_type: String,
    /// The event payload.
    pub data: Value,
}

impl TriggerRuntime {
    /// Create a new `TriggerRuntime` for the given graph and trigger configs.
    pub fn new(graph: Arc<GraphAgent>, triggers: Vec<TriggerNodeConfig>) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        // Create event channel if any event triggers are configured
        let has_event_triggers =
            triggers.iter().any(|t| matches!(t.trigger_type, TriggerType::Event));

        let (event_tx, event_rx) = if has_event_triggers {
            let (tx, rx) = mpsc::channel::<IncomingEvent>(256);
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };

        Self { graph, triggers, shutdown_tx, shutdown_rx, event_tx, event_rx }
    }

    /// Returns a clone of the event sender for external systems to push events.
    ///
    /// Returns `None` if no event triggers are configured.
    pub fn event_sender(&self) -> Option<mpsc::Sender<IncomingEvent>> {
        self.event_tx.clone()
    }

    /// Start all background trigger tasks.
    ///
    /// Spawns a background task for each non-manual trigger and returns
    /// the `JoinHandle`s. Manual and webhook triggers are not spawned here —
    /// manual triggers are on-demand, and webhook routes are registered
    /// separately via `validate_webhook_auth()`.
    pub async fn start(&mut self) -> Vec<JoinHandle<()>> {
        let mut handles = Vec::new();

        // Collect schedule and event configs
        let mut schedule_configs: Vec<(ScheduleConfig, String)> = Vec::new();
        let mut event_configs: Vec<EventConfig> = Vec::new();

        for trigger in &self.triggers {
            match trigger.trigger_type {
                TriggerType::Schedule => {
                    if let Some(schedule) = &trigger.schedule {
                        let default_prompt = schedule.default_prompt.clone().unwrap_or_default();
                        schedule_configs.push((schedule.clone(), default_prompt));
                    }
                }
                TriggerType::Event => {
                    if let Some(event) = &trigger.event {
                        event_configs.push(event.clone());
                    }
                }
                // Manual and Webhook are not background tasks
                TriggerType::Manual | TriggerType::Webhook => {}
            }
        }

        // Spawn schedule triggers
        for (schedule_config, default_prompt) in schedule_configs {
            let graph = Arc::clone(&self.graph);
            let shutdown_rx = self.shutdown_rx.clone();
            let handle = tokio::spawn(async move {
                run_schedule_trigger(graph, schedule_config, default_prompt, shutdown_rx).await;
            });
            handles.push(handle);
        }

        // Spawn event listener (single task handles all event configs)
        if !event_configs.is_empty() {
            if let Some(event_rx) = self.event_rx.take() {
                let graph = Arc::clone(&self.graph);
                let shutdown_rx = self.shutdown_rx.clone();
                let handle = tokio::spawn(async move {
                    run_event_trigger(graph, event_configs, event_rx, shutdown_rx).await;
                });
                handles.push(handle);
            }
        }

        handles
    }

    /// Signal all background tasks to stop.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

// ── Webhook trigger ───────────────────────────────────────────────────

/// Result of webhook authentication validation.
#[derive(Debug)]
pub enum WebhookAuthResult {
    /// Authentication succeeded; contains the request payload.
    Ok(Value),
    /// Authentication failed.
    Unauthorized,
    /// No authentication configured; pass through.
    NoAuth(Value),
}

/// Validate webhook authentication and return the payload if valid.
///
/// This is a standalone function that `adk-server` can call from its Axum
/// route handler. It does not depend on Axum types — the caller extracts
/// the relevant headers and body before calling this function.
///
/// # Arguments
///
/// * `auth_config` - Optional webhook auth configuration
/// * `authorization_header` - The value of the `Authorization` header (if present)
/// * `custom_header_value` - The value of the custom API key header (if present)
/// * `payload` - The request body as a JSON value
pub fn validate_webhook_auth(
    auth_config: Option<&WebhookAuthConfig>,
    authorization_header: Option<&str>,
    custom_header_value: Option<&str>,
    payload: Value,
) -> WebhookAuthResult {
    let Some(auth) = auth_config else {
        return WebhookAuthResult::NoAuth(payload);
    };

    match auth.auth_type.as_str() {
        "bearer" => {
            let expected_token = auth.token.as_deref().unwrap_or("");
            match authorization_header {
                Some(header_value) => {
                    let token = header_value
                        .strip_prefix("Bearer ")
                        .or_else(|| header_value.strip_prefix("bearer "))
                        .unwrap_or("");
                    if token == expected_token {
                        WebhookAuthResult::Ok(payload)
                    } else {
                        WebhookAuthResult::Unauthorized
                    }
                }
                None => WebhookAuthResult::Unauthorized,
            }
        }
        "api_key" => {
            let expected_key = auth.api_key.as_deref().unwrap_or("");
            match custom_header_value {
                Some(value) if value == expected_key => WebhookAuthResult::Ok(payload),
                _ => WebhookAuthResult::Unauthorized,
            }
        }
        // Unknown auth type — reject
        _ => WebhookAuthResult::Unauthorized,
    }
}

/// Invoke the graph with a webhook payload injected into state.
///
/// This is a helper for `adk-server` to call after successful auth validation.
pub async fn invoke_graph_with_webhook(
    graph: &GraphAgent,
    payload: Value,
    thread_id: &str,
) -> crate::error::Result<State> {
    let mut input = State::new();
    input.insert("webhook_payload".to_string(), payload);
    graph.invoke(input, ExecutionConfig::new(thread_id)).await
}

// ── Schedule trigger ──────────────────────────────────────────────────

/// Run a schedule trigger using `tokio-cron-scheduler`.
///
/// Parses the cron expression, creates a job, and invokes the graph on each
/// tick until the shutdown signal is received.
async fn run_schedule_trigger(
    graph: Arc<GraphAgent>,
    config: ScheduleConfig,
    default_prompt: String,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    use tokio_cron_scheduler::{Job, JobScheduler};

    let mut scheduler = match JobScheduler::new().await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, cron = %config.cron, "failed to create job scheduler");
            return;
        }
    };

    let cron_expr = config.cron.clone();
    let graph_clone = Arc::clone(&graph);
    let prompt = default_prompt.clone();

    let job = match Job::new_async(cron_expr.as_str(), move |_uuid, _lock| {
        let graph = Arc::clone(&graph_clone);
        let prompt = prompt.clone();
        Box::pin(async move {
            let mut input = State::new();
            if !prompt.is_empty() {
                input.insert("input".to_string(), json!(prompt));
            }
            let thread_id = uuid::Uuid::new_v4().to_string();
            match graph.invoke(input, ExecutionConfig::new(&thread_id)).await {
                Ok(_) => {
                    tracing::debug!(thread_id = %thread_id, "schedule trigger invocation completed");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "schedule trigger invocation failed");
                }
            }
        })
    }) {
        Ok(j) => j,
        Err(e) => {
            tracing::error!(error = %e, cron = %cron_expr, "failed to create cron job");
            return;
        }
    };

    if let Err(e) = scheduler.add(job).await {
        tracing::error!(error = %e, "failed to add job to scheduler");
        return;
    }

    if let Err(e) = scheduler.start().await {
        tracing::error!(error = %e, "failed to start scheduler");
        return;
    }

    tracing::info!(cron = %cron_expr, "schedule trigger started");

    // Wait for shutdown signal
    let _ = shutdown_rx.wait_for(|&val| val).await;

    tracing::info!(cron = %cron_expr, "schedule trigger shutting down");
    if let Err(e) = scheduler.shutdown().await {
        tracing::warn!(error = %e, "error shutting down scheduler");
    }
}

// ── Event trigger ─────────────────────────────────────────────────────

/// Run the event trigger listener.
///
/// Receives events from the `mpsc` channel, filters them against the
/// configured event sources and types, and invokes the graph for matches.
async fn run_event_trigger(
    graph: Arc<GraphAgent>,
    event_configs: Vec<EventConfig>,
    mut event_rx: mpsc::Receiver<IncomingEvent>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    tracing::info!(config_count = event_configs.len(), "event trigger listener started");

    loop {
        tokio::select! {
            // Check for shutdown
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    tracing::info!("event trigger listener shutting down");
                    break;
                }
            }
            // Receive events
            event = event_rx.recv() => {
                let Some(event) = event else {
                    tracing::info!("event channel closed, stopping listener");
                    break;
                };

                // Check if any config matches this event
                let matches = event_configs.iter().any(|cfg| {
                    cfg.source == event.source && cfg.event_type == event.event_type
                });

                if matches {
                    let graph = Arc::clone(&graph);
                    let event_data = event.data.clone();

                    // Spawn invocation so we don't block the listener
                    tokio::spawn(async move {
                        let mut input = State::new();
                        input.insert("event_data".to_string(), event_data);
                        let thread_id = uuid::Uuid::new_v4().to_string();
                        match graph.invoke(input, ExecutionConfig::new(&thread_id)).await {
                            Ok(_) => {
                                tracing::debug!(
                                    thread_id = %thread_id,
                                    "event trigger invocation completed"
                                );
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "event trigger invocation failed");
                            }
                        }
                    });
                }
            }
        }
    }
}
