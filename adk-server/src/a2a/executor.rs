use crate::a2a::{
    Message, TaskState, TaskStatus, TaskStatusUpdateEvent, UpdateEvent, events::message_to_event,
    metadata::to_invocation_meta, processor::EventProcessor,
};
use adk_core::{Result, SessionId, UserId};
use adk_runner::{Runner, RunnerConfig};
use adk_session::{CreateRequest, GetRequest};
use futures::StreamExt;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[cfg(feature = "a2a-interceptors")]
use crate::a2a::interceptor::{A2aDelegationContext, InterceptorChain, InterceptorDecision};

pub struct ExecutorConfig {
    pub app_name: String,
    pub runner_config: Arc<RunnerConfig>,
    pub cancellation_token: Option<CancellationToken>,
    /// Optional interceptor chain for A2A request/response middleware.
    ///
    /// When set, the chain's `run_before` is called before processing a request,
    /// and `run_after` is called after the executor produces a response.
    #[cfg(feature = "a2a-interceptors")]
    pub interceptor_chain: Option<Arc<InterceptorChain>>,
}

pub struct Executor {
    config: ExecutorConfig,
}

impl Executor {
    pub fn new(config: ExecutorConfig) -> Self {
        Self { config }
    }

    pub async fn execute(
        &self,
        context_id: &str,
        task_id: &str,
        message: &Message,
    ) -> Result<Vec<UpdateEvent>> {
        // --- Interceptor: before delegation ---
        #[cfg(feature = "a2a-interceptors")]
        let interceptor_ctx = {
            if let Some(chain) = &self.config.interceptor_chain {
                let params = serde_json::to_value(message).unwrap_or(serde_json::Value::Null);

                let metadata_map = message
                    .metadata
                    .as_ref()
                    .map(|m| {
                        m.iter()
                            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                            .collect()
                    })
                    .unwrap_or_default();

                let mut ctx = A2aDelegationContext {
                    method: "message/send".to_string(),
                    params,
                    caller_id: message
                        .metadata
                        .as_ref()
                        .and_then(|m| m.get("caller_id"))
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    metadata: metadata_map,
                };

                let decision = chain
                    .run_before(&mut ctx)
                    .await
                    .map_err(|e| adk_core::AdkError::agent(e.to_string()))?;

                match decision {
                    InterceptorDecision::Continue => {}
                    InterceptorDecision::ShortCircuit(response) => {
                        // Return the short-circuited response as a completed task
                        let results = vec![UpdateEvent::TaskStatusUpdate(TaskStatusUpdateEvent {
                            task_id: task_id.to_string(),
                            context_id: Some(context_id.to_string()),
                            status: TaskStatus {
                                state: TaskState::Completed,
                                message: response.as_str().map(String::from),
                            },
                            final_update: true,
                        })];
                        return Ok(results);
                    }
                    InterceptorDecision::Reject { code, message: msg } => {
                        return Err(adk_core::AdkError::agent(format!(
                            "A2A request rejected (code {code}): {msg}"
                        )));
                    }
                }

                Some(ctx)
            } else {
                None
            }
        };

        let meta = to_invocation_meta(&self.config.app_name, context_id, None);
        let cancellation_token = self.config.cancellation_token.clone();

        // Prepare session
        self.prepare_session(&meta.user_id, &meta.session_id).await?;

        // Convert message to event
        let invocation_id = uuid::Uuid::new_v4().to_string();
        let event = message_to_event(message, invocation_id)?;

        // Create runner
        let mut runner_builder = Runner::builder()
            .app_name(self.config.runner_config.app_name.clone())
            .agent(self.config.runner_config.agent.clone())
            .session_service(self.config.runner_config.session_service.clone());
        if let Some(ref artifact_service) = self.config.runner_config.artifact_service {
            runner_builder = runner_builder.artifact_service(artifact_service.clone());
        }
        if let Some(ref memory_service) = self.config.runner_config.memory_service {
            runner_builder = runner_builder.memory_service(memory_service.clone());
        }
        if let Some(ref plugin_manager) = self.config.runner_config.plugin_manager {
            runner_builder = runner_builder.plugin_manager(plugin_manager.clone());
        }
        if let Some(ref run_config) = self.config.runner_config.run_config {
            runner_builder = runner_builder.run_config(run_config.clone());
        }
        if let Some(ref compaction_config) = self.config.runner_config.compaction_config {
            runner_builder = runner_builder.compaction_config(compaction_config.clone());
        }
        if let Some(ref context_cache_config) = self.config.runner_config.context_cache_config {
            runner_builder = runner_builder.context_cache_config(context_cache_config.clone());
        }
        if let Some(ref cache_capable) = self.config.runner_config.cache_capable {
            runner_builder = runner_builder.cache_capable(cache_capable.clone());
        }
        if let Some(ref request_context) = self.config.runner_config.request_context {
            runner_builder = runner_builder.request_context(request_context.clone());
        }
        if let Some(cancellation_token) = cancellation_token.clone() {
            runner_builder = runner_builder.cancellation_token(cancellation_token);
        }
        let runner = runner_builder.build()?;

        // Create processor
        let mut processor =
            EventProcessor::new(context_id.to_string(), task_id.to_string(), meta.clone());

        let mut results = vec![];

        // Send submitted event
        results.push(UpdateEvent::TaskStatusUpdate(TaskStatusUpdateEvent {
            task_id: task_id.to_string(),
            context_id: Some(context_id.to_string()),
            status: TaskStatus { state: TaskState::Submitted, message: None },
            final_update: false,
        }));

        // Send working event
        results.push(UpdateEvent::TaskStatusUpdate(TaskStatusUpdateEvent {
            task_id: task_id.to_string(),
            context_id: Some(context_id.to_string()),
            status: TaskStatus { state: TaskState::Working, message: None },
            final_update: false,
        }));

        // Run agent
        let content = event
            .llm_response
            .content
            .ok_or_else(|| adk_core::AdkError::agent("Event has no content"))?;

        let mut event_stream = runner
            .run(
                UserId::new(meta.user_id.clone())?,
                SessionId::new(meta.session_id.clone())?,
                content,
            )
            .await?;

        // Process events
        while let Some(result) = event_stream.next().await {
            if cancellation_token.as_ref().is_some_and(CancellationToken::is_cancelled) {
                results.push(UpdateEvent::TaskStatusUpdate(TaskStatusUpdateEvent {
                    task_id: task_id.to_string(),
                    context_id: Some(context_id.to_string()),
                    status: TaskStatus { state: TaskState::Canceled, message: None },
                    final_update: true,
                }));
                return Ok(results);
            }

            match result {
                Ok(adk_event) => {
                    if let Some(artifact_event) = processor.process(&adk_event)? {
                        results.push(UpdateEvent::TaskArtifactUpdate(artifact_event));
                    }
                }
                Err(e) => {
                    // Send failed event
                    results.push(UpdateEvent::TaskStatusUpdate(TaskStatusUpdateEvent {
                        task_id: task_id.to_string(),
                        context_id: Some(context_id.to_string()),
                        status: TaskStatus {
                            state: TaskState::Failed,
                            message: Some(e.to_string()),
                        },
                        final_update: true,
                    }));
                    return Ok(results);
                }
            }
        }

        if cancellation_token.as_ref().is_some_and(CancellationToken::is_cancelled) {
            results.push(UpdateEvent::TaskStatusUpdate(TaskStatusUpdateEvent {
                task_id: task_id.to_string(),
                context_id: Some(context_id.to_string()),
                status: TaskStatus { state: TaskState::Canceled, message: None },
                final_update: true,
            }));
            return Ok(results);
        }

        // Send terminal events
        for terminal_event in processor.make_terminal_events() {
            results.push(UpdateEvent::TaskStatusUpdate(terminal_event));
        }

        // --- Interceptor: after delegation ---
        #[cfg(feature = "a2a-interceptors")]
        if let Some(chain) = &self.config.interceptor_chain
            && let Some(ctx) = &interceptor_ctx
        {
            let mut response_value =
                serde_json::to_value(&results).unwrap_or(serde_json::Value::Null);
            chain
                .run_after(ctx, &mut response_value)
                .await
                .map_err(|e| adk_core::AdkError::agent(e.to_string()))?;
        }

        Ok(results)
    }

    pub async fn cancel(&self, context_id: &str, task_id: &str) -> Result<TaskStatusUpdateEvent> {
        Ok(TaskStatusUpdateEvent {
            task_id: task_id.to_string(),
            context_id: Some(context_id.to_string()),
            status: TaskStatus { state: TaskState::Canceled, message: None },
            final_update: true,
        })
    }

    async fn prepare_session(&self, user_id: &str, session_id: &str) -> Result<()> {
        let session_service = &self.config.runner_config.session_service;

        // Try to get existing session
        let get_result = session_service
            .get(GetRequest {
                app_name: self.config.app_name.clone(),
                user_id: user_id.to_string(),
                session_id: session_id.to_string(),
                num_recent_events: None,
                after: None,
            })
            .await;

        if get_result.is_ok() {
            return Ok(());
        }

        // Create new session
        session_service
            .create(CreateRequest {
                app_name: self.config.app_name.clone(),
                user_id: user_id.to_string(),
                session_id: Some(session_id.to_string()),
                state: std::collections::HashMap::new(),
            })
            .await?;

        Ok(())
    }
}
