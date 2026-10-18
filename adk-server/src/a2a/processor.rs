use crate::a2a::{
    Artifact, TaskArtifactUpdateEvent, TaskState, TaskStatus, TaskStatusUpdateEvent,
    metadata::{InvocationMeta, to_event_meta},
    parts::adk_parts_to_a2a,
};
use adk_core::{Event, EventActions, Part, Result};

/// The function call an agent makes to ask its caller for credentials, as adk-python names it.
pub const REQUEST_CREDENTIAL_FUNCTION: &str = "adk_request_credential";

/// What an agent event leaves its A2A task waiting for. `Auth` outranks `Input`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Awaiting {
    /// A tool call held for confirmation, or a long-running call.
    Input,
    /// A credential request.
    Auth,
}

/// Whether `event` stops the task to wait on its caller, and for what.
pub(crate) fn awaiting(event: &Event) -> Option<Awaiting> {
    let requests_credential = event.content().into_iter().flat_map(|content| &content.parts).any(
        |part| matches!(part, Part::FunctionCall { name, .. } if name == REQUEST_CREDENTIAL_FUNCTION),
    );
    if requests_credential {
        Some(Awaiting::Auth)
    } else if event.actions.tool_confirmation.is_some() || !event.long_running_tool_ids.is_empty() {
        Some(Awaiting::Input)
    } else {
        None
    }
}

pub struct EventProcessor {
    context_id: String,
    task_id: String,
    meta: InvocationMeta,
    terminal_actions: EventActions,
    response_id: Option<String>,
    terminal_state: Option<TaskState>,
    has_artifacts: bool,
    text_deltas: adk_core::EventTextDeltas,
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
            text_deltas: adk_core::EventTextDeltas::default(),
        }
    }

    pub fn process(&mut self, event: &Event) -> Result<Option<TaskArtifactUpdateEvent>> {
        self.update_terminal_actions(event);
        if let Some(awaiting) = awaiting(event) {
            let state = match awaiting {
                Awaiting::Input => TaskState::InputRequired,
                Awaiting::Auth => TaskState::AuthRequired,
            };
            if self.terminal_state != Some(TaskState::AuthRequired) {
                self.terminal_state = Some(state);
            }
        }
        let event = self.text_deltas.push(event);

        let event_meta = to_event_meta(&self.meta, &event);
        let event_meta_map: serde_json::Map<String, serde_json::Value> =
            event_meta.into_iter().collect();

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

    /// The final status: `auth-required` or `input-required` when an event stopped the run
    /// to wait on the caller, `completed` otherwise.
    pub fn make_terminal_events(&self) -> Vec<TaskStatusUpdateEvent> {
        let mut events = vec![];

        let state = self.terminal_state.clone().unwrap_or(TaskState::Completed);

        events.push(TaskStatusUpdateEvent {
            task_id: self.task_id.clone(),
            context_id: Some(self.context_id.clone()),
            status: TaskStatus { state, message: None },
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a2a::to_invocation_meta;
    use adk_core::{Content, ToolConfirmationRequest};

    fn call(name: &str) -> Event {
        let mut event = Event::new("invocation");
        event.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: name.to_string(),
                args: serde_json::json!({}),
                id: Some(format!("call-{name}")),
                thought_signature: None,
            }],
        });
        event
    }

    fn final_state(events: &[Event]) -> TaskState {
        let meta = to_invocation_meta("app", "ctx", None);
        let mut processor = EventProcessor::new("ctx".to_string(), "task".to_string(), meta);
        for event in events {
            processor.process(event).unwrap();
        }
        processor.make_terminal_events().remove(0).status.state
    }

    #[test]
    fn a_confirmation_request_or_long_running_call_awaits_input() {
        let mut held = Event::new("invocation");
        held.actions.tool_confirmation =
            Some(ToolConfirmationRequest::new("delete_record", serde_json::json!({})));
        assert_eq!(final_state(&[held]), TaskState::InputRequired);

        let mut long_running = call("start_export");
        long_running.long_running_tool_ids = vec!["start_export".to_string()];
        assert_eq!(
            final_state(&[long_running, Event::new("invocation")]),
            TaskState::InputRequired
        );

        assert_eq!(final_state(&[call("lookup")]), TaskState::Completed);
    }

    #[test]
    fn a_credential_request_outranks_awaited_input() {
        let mut long_running = call("start_export");
        long_running.long_running_tool_ids = vec!["start_export".to_string()];
        let credential = call(REQUEST_CREDENTIAL_FUNCTION);
        assert_eq!(
            final_state(&[credential.clone(), long_running.clone()]),
            TaskState::AuthRequired
        );
        assert_eq!(final_state(&[long_running, credential]), TaskState::AuthRequired);
        assert!(Some(Awaiting::Auth) > Some(Awaiting::Input) && Some(Awaiting::Input) > None);
    }
}
