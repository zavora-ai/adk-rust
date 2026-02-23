use crate::a2a::{
    Message, TaskState, TaskStatus, TaskStatusUpdateEvent, UpdateEvent, events::message_to_event,
    metadata::to_invocation_meta, processor::EventProcessor,
};
use adk_core::Result;
use adk_runner::{Runner, RunnerConfig};
use adk_session::{CreateRequest, GetRequest};
use futures::StreamExt;
use std::sync::Arc;

pub struct ExecutorConfig {
    pub app_name: String,
    pub runner_config: Arc<RunnerConfig>,
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
        let meta = to_invocation_meta(&self.config.app_name, context_id, None);

        // Prepare session
        self.prepare_session(&meta.user_id, &meta.session_id).await?;

        // Convert message to event
        let invocation_id = uuid::Uuid::new_v4().to_string();
        let event = message_to_event(message, invocation_id)?;

        // Create runner
        let runner = Runner::new(RunnerConfig {
            app_name: self.config.runner_config.app_name.clone(),
            agent: self.config.runner_config.agent.clone(),
            session_service: self.config.runner_config.session_service.clone(),
            artifact_service: self.config.runner_config.artifact_service.clone(),
            memory_service: self.config.runner_config.memory_service.clone(),
            plugin_manager: self.config.runner_config.plugin_manager.clone(),
            run_config: self.config.runner_config.run_config.clone(),
            compaction_config: self.config.runner_config.compaction_config.clone(),
            context_cache_config: self.config.runner_config.context_cache_config.clone(),
            cache_capable: self.config.runner_config.cache_capable.clone(),
        })?;

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
            .ok_or_else(|| adk_core::AdkError::Agent("Event has no content".to_string()))?;

        let mut event_stream =
            runner.run(meta.user_id.clone(), meta.session_id.clone(), content).await?;

        // Process events
        while let Some(result) = event_stream.next().await {
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

        // Send terminal events
        for terminal_event in processor.make_terminal_events() {
            results.push(UpdateEvent::TaskStatusUpdate(terminal_event));
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
