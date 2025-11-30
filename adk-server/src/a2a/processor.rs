use crate::a2a::{
    metadata::{to_event_meta, InvocationMeta},
    parts::adk_parts_to_a2a,
    Artifact, TaskArtifactUpdateEvent, TaskState, TaskStatus, TaskStatusUpdateEvent,
};
use adk_core::{Event, EventActions, Result};

pub struct EventProcessor {
    context_id: String,
    task_id: String,
    meta: InvocationMeta,
    terminal_actions: EventActions,
    response_id: Option<String>,
    terminal_state: Option<TaskState>,
    has_artifacts: bool,
}

impl EventProcessor {
    pub fn new(context_id: String, task_id: String, meta: InvocationMeta) -> Self {
        Self {
            context_id,
            task_id,
            meta,
            terminal_actions: EventActions::default(),
            response_id: None,
            terminal_state: None,
            has_artifacts: false,
        }
    }

    pub fn process(&mut self, event: &Event) -> Result<Option<TaskArtifactUpdateEvent>> {
        self.update_terminal_actions(event);

        let event_meta = to_event_meta(&self.meta, event);
        let event_meta_map: serde_json::Map<String, serde_json::Value> = event_meta.into_iter().collect();

        // Get content
        let content = match &event.llm_response.content {
            Some(c) => c,
            None => return Ok(None),
        };

        if content.parts.is_empty() {
            return Ok(None);
        }

        // Convert parts
        let parts = adk_parts_to_a2a(&content.parts, &[])?;
        
        if parts.is_empty() {
            return Ok(None);
        }

        self.has_artifacts = true;

        let artifact_event = if let Some(response_id) = &self.response_id {
            TaskArtifactUpdateEvent {
                task_id: self.task_id.clone(),
                context_id: Some(self.context_id.clone()),
                artifact: Artifact {
                    artifact_id: response_id.clone(),
                    name: None,
                    description: None,
                    parts,
                    metadata: Some(event_meta_map),
                    extensions: None,
                },
                append: true,
                last_chunk: false,
            }
        } else {
            let artifact_id = uuid::Uuid::new_v4().to_string();
            self.response_id = Some(artifact_id.clone());
            
            TaskArtifactUpdateEvent {
                task_id: self.task_id.clone(),
                context_id: Some(self.context_id.clone()),
                artifact: Artifact {
                    artifact_id,
                    name: None,
                    description: None,
                    parts,
                    metadata: Some(event_meta_map),
                    extensions: None,
                },
                append: true,
                last_chunk: false,
            }
        };

        Ok(Some(artifact_event))
    }

    pub fn make_terminal_events(&self) -> Vec<TaskStatusUpdateEvent> {
        let mut events = vec![];

        // Terminal status
        let state = self.terminal_state.clone().unwrap_or(TaskState::Completed);

        events.push(TaskStatusUpdateEvent {
            task_id: self.task_id.clone(),
            context_id: Some(self.context_id.clone()),
            status: TaskStatus {
                state,
                message: None,
            },
            final_update: true,
        });

        events
    }

    fn update_terminal_actions(&mut self, event: &Event) {
        self.terminal_actions.escalate = self.terminal_actions.escalate || event.actions.escalate;
        if let Some(agent) = &event.actions.transfer_to_agent {
            self.terminal_actions.transfer_to_agent = Some(agent.clone());
        }
    }
}
