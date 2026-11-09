//! The function calls a task waits on when its run stops for the caller.

use std::collections::HashSet;

use adk_core::{Event, Part};

use super::convert::{tagged_function_call_part, tagged_function_response_part};

/// Collects, from a run's events, the calls the task waits on and the results they
/// returned so far, as A2A data parts.
///
/// A long-running call is listed with its interim response (for example a ticket
/// number), and a call held for confirmation is listed on its own. The caller answers
/// with a `function_response` data part for the call's `id`, sent with the task's
/// `taskId`. This is the shape adk-python and adk-go put in an `INPUT_REQUIRED` status.
#[derive(Default)]
pub(super) struct PendingCalls {
    parts: Vec<a2a_protocol_types::Part>,
    call_ids: HashSet<String>,
}

impl PendingCalls {
    /// Records the pending calls `event` starts and the responses it gives them.
    pub(super) fn observe(&mut self, event: &Event) {
        if let Some(request) = &event.actions.tool_confirmation {
            let id = request.function_call_id.as_deref();
            if id.is_none_or(|id| self.call_ids.insert(id.to_string())) {
                self.parts.push(tagged_function_call_part(
                    &request.tool_name,
                    &request.args,
                    id,
                    false,
                ));
            }
        }
        // `LlmAgent` lists a long-running call by its tool's name; other agents list the
        // call's id, as adk-python does.
        let long_running = |name: &String, id: &String| {
            event.long_running_tool_ids.iter().any(|listed| listed == id || listed == name)
        };
        for part in event.content().into_iter().flat_map(|content| &content.parts) {
            match part {
                Part::FunctionCall { name, args, id: Some(id), .. }
                    if long_running(name, id) && self.call_ids.insert(id.clone()) =>
                {
                    self.parts.push(tagged_function_call_part(name, args, Some(id), true));
                }
                Part::FunctionResponse { function_response, id: Some(id), .. }
                    if self.call_ids.contains(id) =>
                {
                    self.parts.push(tagged_function_response_part(
                        &function_response.name,
                        &function_response.response,
                        Some(id),
                    ));
                }
                _ => {}
            }
        }
    }

    /// The pending calls and their responses, in the order the run produced them.
    pub(super) fn into_parts(self) -> Vec<a2a_protocol_types::Part> {
        self.parts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{Content, FunctionResponseData};
    use serde_json::json;

    fn event_with(parts: Vec<Part>) -> Event {
        let mut event = Event::new("inv");
        event.set_content(Content { role: "model".to_string(), parts });
        event
    }

    #[test]
    fn a_long_running_call_is_listed_with_its_interim_response() {
        let mut pending = PendingCalls::default();
        let mut call = event_with(vec![
            Part::FunctionCall {
                name: "request_approval".to_string(),
                args: json!({"amount": 500}),
                id: Some("call-1".to_string()),
                thought_signature: None,
            },
            Part::FunctionCall {
                name: "lookup".to_string(),
                args: json!({}),
                id: Some("call-2".to_string()),
                thought_signature: None,
            },
        ]);
        call.long_running_tool_ids = vec!["call-1".to_string()];
        pending.observe(&call);
        pending.observe(&event_with(vec![
            Part::FunctionResponse {
                function_response: FunctionResponseData::new(
                    "request_approval",
                    json!({"status": "pending"}),
                ),
                id: Some("call-1".to_string()),
                annotations: None,
            },
            Part::FunctionResponse {
                function_response: FunctionResponseData::new("lookup", json!({"hit": true})),
                id: Some("call-2".to_string()),
                annotations: None,
            },
        ]));
        // A repeat of the same call does not list it twice.
        pending.observe(&call);

        let parts: Vec<_> =
            pending.into_parts().iter().map(|part| serde_json::to_value(part).unwrap()).collect();
        assert_eq!(
            parts,
            vec![
                json!({
                    "data": {"id": "call-1", "name": "request_approval", "args": {"amount": 500}},
                    "metadata": {"adk_type": "function_call", "adk_is_long_running": true}
                }),
                json!({
                    "data": {"id": "call-1", "name": "request_approval", "response": {"status": "pending"}},
                    "metadata": {"adk_type": "function_response"}
                }),
            ],
            "only the long-running call and its own response are pending"
        );
    }

    #[test]
    fn a_call_listed_by_its_tool_name_is_pending() {
        // `LlmAgent` lists long-running calls by tool name.
        let mut pending = PendingCalls::default();
        let mut call = event_with(vec![Part::FunctionCall {
            name: "request_approval".to_string(),
            args: json!({}),
            id: Some("call-7".to_string()),
            thought_signature: None,
        }]);
        call.long_running_tool_ids = vec!["request_approval".to_string()];
        pending.observe(&call);

        let parts = pending.into_parts();
        assert_eq!(parts.len(), 1);
        assert_eq!(serde_json::to_value(&parts[0]).unwrap()["data"]["id"], "call-7");
    }

    #[test]
    fn a_call_held_for_confirmation_is_listed() {
        let mut pending = PendingCalls::default();
        let mut held = Event::new("inv");
        held.actions.tool_confirmation = Some(adk_core::ToolConfirmationRequest {
            tool_name: "transfer".to_string(),
            function_call_id: Some("call-9".to_string()),
            args: json!({"to": "acct"}),
            acting: None,
            origin: None,
        });
        pending.observe(&held);

        let parts = pending.into_parts();
        assert_eq!(parts.len(), 1);
        assert_eq!(
            serde_json::to_value(&parts[0]).unwrap(),
            json!({
                "data": {"id": "call-9", "name": "transfer", "args": {"to": "acct"}},
                "metadata": {"adk_type": "function_call"}
            })
        );
    }
}
