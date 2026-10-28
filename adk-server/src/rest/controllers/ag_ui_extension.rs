//! The AG-UI extension point (spec 3.3 §6.1 `genui`, §10 S2): what a host adds to the
//! protocol-native AG-UI stream without this crate knowing what it adds.
//!
//! A host sets one [`AgUiExtension`] with [`RuntimeController::with_ag_ui_extension`] or
//! `ServerBuilder::with_ag_ui_extension`. On a protocol-native `ag_ui` run it:
//!
//! | Hook | When |
//! |---|---|
//! | [`events_for`](AgUiExtension::events_for) | After each ADK event's own AG-UI events: extra events, such as `ACTIVITY_SNAPSHOT` |
//! | [`keeps_activity`](AgUiExtension::keeps_activity) | For each `activity` message a client sends back: whether it reaches the stream, the session state and the messages snapshot |
//! | [`action_in`](AgUiExtension::action_in), [`requested_call`](AgUiExtension::requested_call) | When the run's `forwardedProps` carry an action: the call a person asked for, which the run's agent holds for their decision, so the run ends with a `tool_call` interrupt |
//! | [`features`](AgUiExtension::features) | Added to the `ag_ui` features of `GET /api/ui/capabilities` |
//!
//! [`RuntimeController::with_ag_ui_extension`]: super::RuntimeController::with_ag_ui_extension

use adk_core::{Event, RequestedCall, SubjectRef};
use async_trait::async_trait;
use serde_json::Value;

use super::runtime::AgUiInputMessage;

/// A run's forwarded action, as an [`AgUiExtension`] reads it (spec 3.3 §7.4 rule 5).
#[derive(Debug, Clone, Copy)]
pub struct AgUiAction<'a> {
    /// The app the run is for.
    pub app_name: &'a str,
    /// The agent the run executes, which holds the call.
    pub agent_name: &'a str,
    /// The session's user, as the request context authenticated them.
    pub user_id: &'a str,
    /// The session.
    pub session_id: &'a str,
    /// The person who asked.
    pub requested_by: &'a SubjectRef,
    /// The run input's `forwardedProps`.
    pub forwarded_props: &'a Value,
    /// The session's events before the run.
    pub events: &'a [Event],
}

/// What a host adds to protocol-native AG-UI runs (spec 3.3 §6.1 `genui`, §10 S2). Every
/// hook has a default that adds nothing, so a run without an extension, or with one that
/// overrides nothing, is unchanged.
///
/// # Example
///
/// ```rust
/// use adk_server::rest::controllers::ag_ui_extension::AgUiExtension;
/// use serde_json::{Value, json};
///
/// /// Says "hello" in an activity after every event.
/// struct Hello;
///
/// impl AgUiExtension for Hello {
///     fn features(&self) -> Vec<&'static str> {
///         vec!["hello_activity"]
///     }
///
///     fn events_for(&self, event: &adk_core::Event) -> Vec<Value> {
///         vec![json!({
///             "type": "ACTIVITY_SNAPSHOT",
///             "messageId": format!("hello-{}", event.id),
///             "activityType": "hello",
///             "content": {},
///         })]
///     }
/// }
///
/// assert_eq!(Hello.events_for(&adk_core::Event::new("inv-1"))[0]["activityType"], "hello");
/// ```
#[async_trait]
pub trait AgUiExtension: Send + Sync {
    /// The features this extension adds to `ag_ui` in `GET /api/ui/capabilities`.
    fn features(&self) -> Vec<&'static str> {
        Vec::new()
    }

    /// AG-UI events to send after the events `event` translates to.
    fn events_for(&self, _event: &Event) -> Vec<Value> {
        Vec::new()
    }

    /// Whether an `activity` message a client sent back is kept: a dropped one reaches neither
    /// the stream, nor `temp:ag_ui_messages`, nor the `MESSAGES_SNAPSHOT`.
    fn keeps_activity(&self, _message: &AgUiInputMessage) -> bool {
        true
    }

    /// Whether `forwarded_props` carry an action this extension answers with
    /// [`requested_call`](Self::requested_call).
    fn action_in(&self, _forwarded_props: &Value) -> bool {
        false
    }

    /// The call the action in `action` asks for, checked against the session: the run's agent
    /// holds it for the person's decision with no model call, and the run ends with a
    /// `tool_call` interrupt. `None` when the props carry no action after all.
    ///
    /// # Errors
    ///
    /// The refusal of an action that is stale, undeclared or malformed; the run does not start
    /// and the request answers with the error's status.
    async fn requested_call(
        &self,
        _action: AgUiAction<'_>,
    ) -> adk_core::Result<Option<RequestedCall>> {
        Ok(None)
    }
}
