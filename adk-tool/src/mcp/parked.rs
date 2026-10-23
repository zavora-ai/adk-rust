//! Parked MCP calls: the durable form of a tool call that waits for input, and the outcome of
//! a call that may park (spec 3.2 §7.7, §10 T2).
//!
//! Under the 2026-07-28 revision an elicitation ends the server's request: the server answers
//! `input_required` with `inputRequests` and an opaque `requestState`, and the client retries
//! the call with the answers as an independent request. A task asks through `tasks/get` and
//! takes its answers through `tasks/update`. A [`ParkedMcpCall`] holds what that retry or
//! resumption needs, so it can be persisted beside an approval and re-issued with
//! [`McpToolset::reissue`](super::McpToolset::reissue) by another toolset, in another process.

use rmcp::model::{CallToolRequestParams, InputRequests};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// How a [`ParkedMcpCall`] resumes (spec 3.2 §7.7, §10 T2).
///
/// Serialized as `"mrtr"`, `{"task": {"taskId": "…"}}` or `"legacy"`.
///
/// # Example
///
/// ```rust
/// use adk_tool::mcp::ParkedCallKind;
///
/// let task = ParkedCallKind::Task { task_id: "t-1".to_string() };
/// assert_eq!(serde_json::to_value(&task).unwrap(), serde_json::json!({ "task": { "taskId": "t-1" } }));
/// assert_eq!(serde_json::to_value(ParkedCallKind::Mrtr).unwrap(), "mrtr");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ParkedCallKind {
    /// The server ended the call at `input_required`; a re-issue sends a new `tools/call` with
    /// `inputResponses` and the stored `requestState`.
    Mrtr,
    /// The server runs the call as a task that waits for input; a re-issue polls `tasks/get`
    /// and answers through `tasks/update`.
    #[serde(rename_all = "camelCase")]
    Task {
        /// The task's ID.
        task_id: String,
    },
    /// A server before 2026-07-28 asked with `elicitation/create` while the call ran on the
    /// live connection, which a restart loses; a re-issue runs the call again only when the
    /// tool is retry-safe.
    Legacy,
}

/// A tool call waiting for input, in a form that survives a restart (spec 3.2 §7.7, §10 T2).
///
/// A toolset produces one wherever a call parks:
///
/// | Where | Kind | How the caller receives it |
/// |---|---|---|
/// | [`McpToolset::call_tool_resumable`](super::McpToolset::call_tool_resumable) or a re-issue reaches `input_required` with input requests | [`Mrtr`](ParkedCallKind::Mrtr) | [`McpCallOutcome::Parked`]; the call ends |
/// | The same, for a task that reports `input_required` | [`Task`](ParkedCallKind::Task) | [`McpCallOutcome::Parked`]; the task keeps waiting on the server |
/// | Any call whose input the connection's handler answers in process | `Mrtr` or `Task` | [`ParkedMcpCall::current`] inside the handler |
/// | A server before 2026-07-28 sends `elicitation/create` during a call | [`Legacy`](ParkedCallKind::Legacy) | [`ParkedMcpCall::current`] inside an [`ElicitationHandler`](super::ElicitationHandler) served by [`AdkClientHandler`](super::AdkClientHandler) |
///
/// Persist it beside the approval that asks the person, and pass the answers keyed as
/// [`input_requests`](Self::input_requests) to
/// [`McpToolset::reissue`](super::McpToolset::reissue) on wake.
///
/// # Example
///
/// ```rust
/// use adk_tool::mcp::{ParkedCallKind, ParkedMcpCall};
///
/// let parked: ParkedMcpCall = serde_json::from_value(serde_json::json!({
///     "serverId": "email",
///     "toolName": "send_draft",
///     "arguments": { "draftId": "d-7" },
///     "callId": "call-1",
///     "requestState": "opaque",
///     "inputRequests": {},
///     "inputRequestsDigest": ParkedMcpCall::digest_input_requests(&Default::default()),
///     "inputRounds": 1,
///     "kind": "mrtr"
/// }))
/// .unwrap();
/// assert_eq!(parked.kind, ParkedCallKind::Mrtr);
/// assert_eq!(parked.request_state.as_deref(), Some("opaque"));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ParkedMcpCall {
    /// Name of the toolset that made the call; a managed server's ID.
    pub server_id: String,
    /// The MCP tool's name, as the server publishes it.
    pub tool_name: String,
    /// The call's arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Map<String, Value>>,
    /// The function call ID of the agent's call, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// The server's opaque `requestState`, echoed back unread on re-issue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_state: Option<String>,
    /// The questions waiting for an answer, keyed as the answers must be.
    #[serde(default)]
    pub input_requests: InputRequests,
    /// [`digest_input_requests`](Self::digest_input_requests) of `input_requests`, so an
    /// answer binds to the questions it was given for.
    pub input_requests_digest: String,
    /// Input rounds the call has taken, this one included; bounded by
    /// [`McpTaskConfig::max_input_rounds`](super::McpTaskConfig::max_input_rounds) across
    /// re-issues.
    pub input_rounds: usize,
    /// How the call resumes.
    pub kind: ParkedCallKind,
}

tokio::task_local! {
    static CURRENT_PARKED_CALL: ParkedMcpCall;
}

impl ParkedMcpCall {
    /// The parked form of the call whose input is being answered, if any (spec 3.2 §10 T2).
    ///
    /// Set while a toolset's connection handler answers MRTR input, a task's input, or a
    /// legacy `elicitation/create` of one of the toolset's calls, so a handler that parks the
    /// request as an approval can persist the call beside it and have it re-issued after a
    /// restart. With several calls awaiting their response on one connection, a legacy
    /// `elicitation/create` is attributed to the latest.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_tool::mcp::ParkedMcpCall;
    ///
    /// assert!(ParkedMcpCall::current().is_none());
    /// ```
    pub fn current() -> Option<Self> {
        CURRENT_PARKED_CALL.try_with(Clone::clone).ok()
    }

    /// Runs `future` with this call as [`current`](Self::current).
    pub(crate) async fn within<F: Future>(self, future: F) -> F::Output {
        CURRENT_PARKED_CALL.scope(self, future).await
    }

    /// The digest of `input_requests` stored in
    /// [`input_requests_digest`](Self::input_requests_digest): [`adk_core::json_digest`] of
    /// their JSON (spec 3.2 §7.7).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_tool::mcp::ParkedMcpCall;
    ///
    /// let digest = ParkedMcpCall::digest_input_requests(&Default::default());
    /// assert!(digest.starts_with("fnv1a128:"));
    /// ```
    pub fn digest_input_requests(input_requests: &InputRequests) -> String {
        adk_core::json_digest(&serde_json::to_value(input_requests).unwrap_or(Value::Null))
    }

    /// The call `params` names, made through toolset `server_id`, before any input.
    pub(crate) fn of_call(
        server_id: &str,
        params: &CallToolRequestParams,
        call_id: Option<&str>,
    ) -> Self {
        Self {
            server_id: server_id.to_string(),
            tool_name: params.name.to_string(),
            arguments: params.arguments.clone(),
            call_id: call_id.map(str::to_string),
            request_state: None,
            input_requests: InputRequests::new(),
            input_requests_digest: Self::digest_input_requests(&InputRequests::new()),
            input_rounds: 0,
            kind: ParkedCallKind::Legacy,
        }
    }

    /// This call parked as `kind`, waiting on `input_requests` in its `input_rounds`th round.
    pub(crate) fn waiting(
        mut self,
        kind: ParkedCallKind,
        request_state: Option<String>,
        input_requests: InputRequests,
        input_rounds: usize,
    ) -> Self {
        self.input_requests_digest = Self::digest_input_requests(&input_requests);
        self.input_requests = input_requests;
        self.request_state = request_state;
        self.input_rounds = input_rounds;
        self.kind = kind;
        self
    }

    /// Whether `input_requests` still has the digest stored with it.
    pub(crate) fn is_intact(&self) -> bool {
        Self::digest_input_requests(&self.input_requests) == self.input_requests_digest
    }
}

/// What a call that may park returned (spec 3.2 §10 T2).
///
/// # Example
///
/// ```rust
/// use adk_tool::mcp::McpCallOutcome;
///
/// fn answer(outcome: McpCallOutcome) -> Option<serde_json::Value> {
///     match outcome {
///         McpCallOutcome::Completed(value) => Some(value),
///         // Persist the parked call and ask the person; re-issue it with the answers.
///         McpCallOutcome::Parked(_parked) => None,
///     }
/// }
/// ```
#[derive(Debug, Clone, PartialEq)]
pub enum McpCallOutcome {
    /// The call finished; the value has the shape the toolset's tools return.
    Completed(Value),
    /// The call waits for input; nothing waits for it in this process.
    Parked(ParkedMcpCall),
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::{ElicitRequest, ElicitRequestParams, InputRequest};
    use serde_json::json;

    fn requests() -> InputRequests {
        let schema = serde_json::from_value(
            json!({ "type": "object", "properties": { "ok": { "type": "boolean" } } }),
        )
        .unwrap();
        let question = ElicitRequestParams::FormElicitationParams {
            meta: None,
            message: "Send it?".into(),
            requested_schema: schema,
        };
        [("confirm".to_string(), InputRequest::Elicitation(ElicitRequest::new(question)))].into()
    }

    fn parked(kind: ParkedCallKind) -> ParkedMcpCall {
        let params = CallToolRequestParams::new("send_draft")
            .with_arguments(json!({ "draftId": "d-7" }).as_object().unwrap().clone());
        ParkedMcpCall::of_call("email", &params, Some("call-1")).waiting(
            kind,
            Some("opaque".to_string()),
            requests(),
            2,
        )
    }

    #[test]
    fn every_kind_round_trips_through_json() {
        for (kind, json_kind) in [
            (ParkedCallKind::Mrtr, json!("mrtr")),
            (
                ParkedCallKind::Task { task_id: "t-1".to_string() },
                json!({ "task": { "taskId": "t-1" } }),
            ),
            (ParkedCallKind::Legacy, json!("legacy")),
        ] {
            let call = parked(kind);
            let value = serde_json::to_value(&call).unwrap();
            assert_eq!(value["kind"], json_kind);
            assert_eq!(value["serverId"], "email");
            assert_eq!(value["callId"], "call-1");
            assert_eq!(value["requestState"], "opaque");
            assert_eq!(value["inputRounds"], 2);
            let back: ParkedMcpCall = serde_json::from_value(value).unwrap();
            assert_eq!(back, call);
            assert!(back.is_intact());
        }
    }

    #[test]
    fn a_changed_question_no_longer_matches_its_digest() {
        let mut call = parked(ParkedCallKind::Mrtr);
        assert_eq!(call.input_requests_digest, ParkedMcpCall::digest_input_requests(&requests()));
        call.input_requests.clear();
        assert!(!call.is_intact());
    }

    #[tokio::test]
    async fn current_is_set_only_within() {
        let call = parked(ParkedCallKind::Mrtr);
        assert_eq!(ParkedMcpCall::current(), None);
        let seen = call.clone().within(async { ParkedMcpCall::current() }).await;
        assert_eq!(seen, Some(call));
        assert_eq!(ParkedMcpCall::current(), None);
    }
}
