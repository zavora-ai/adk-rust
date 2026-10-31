//! Function responses for tool calls that do not return their result.
//!
//! An agent answers every function call with exactly one function response. When the tool
//! fails, when its result cannot be released, or when the model's arguments cannot be decoded,
//! these builders produce that response without passing error detail to the model by default.
//! A [`ToolResultScreen`] decides what of a tool's result the model and the session may see.
//!
//! An answer the agent writes in place of the tool's result carries a [`ToolResultStatus`] in
//! the part's annotations ([`Part::tool_result_status`]), so a host, an eval, or an audit reads
//! what happened to the call without parsing the payload.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{ActionOutcome, AdkError, ErrorCategory, Part};

/// The annotation key of a function response's [`ToolResultStatus`].
pub const TOOL_RESULT_STATUS_KEY: &str = "adk.tool_result.status";

/// The annotation key of the [`AdkError::code`] behind a function response's status.
pub const TOOL_RESULT_ERROR_CODE_KEY: &str = "adk.tool_result.error_code";

/// The annotation key of the [`ErrorCategory`] behind a function response's status.
pub const TOOL_RESULT_ERROR_CATEGORY_KEY: &str = "adk.tool_result.error_category";

/// What happened to a tool call that the agent, not the tool, answered.
///
/// An agent sets it on the function response it writes in place of the tool's result —
/// a refusal, a hold, a failure, a withheld result, an unknown outcome, an action-ledger
/// replay — as the [`TOOL_RESULT_STATUS_KEY`] annotation; the payload the model reads is
/// unchanged. A response without a status is the tool's own result, an answer a callback or
/// plugin supplied, or one stored before the status existed. The status is never derived from
/// the payload, which a tool owns.
///
/// | Status | The call | Issued again |
/// |--------|----------|--------------|
/// | `Refused` | did not run: an authorization check refused it | meets the same check |
/// | `NotRun` | did not run, for a reason other than an authorization decision | may run once the reason clears |
/// | `Held` | waits for a confirmation decision; a later run answers it | should not be; decide it instead |
/// | `Failed` | ran and failed with a known outcome | as the tool allows |
/// | `OutcomeUnknown` | began and may have taken effect | only after checking its effect |
/// | `Withheld` | ran; its result is withheld | should not be |
/// | `Succeeded` | ran in an earlier attempt; this answer reports that, not the result | should not be |
///
/// # Example
///
/// ```rust
/// use adk_core::ToolResultStatus;
///
/// assert_eq!(serde_json::to_value(ToolResultStatus::NotRun).unwrap(), "not_run");
/// assert!(ToolResultStatus::Refused.is_error());
/// assert!(!ToolResultStatus::OutcomeUnknown.is_error());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolResultStatus {
    /// An authorization check refused the call, so it did not run: a `ToolPolicy` denial, a
    /// tool guardrail's denial, an approver's or handler's decline, a held call resumed by
    /// another agent or for another tool, an access guard inside the tool
    /// ([`TOOL_PERMISSION_DENIED_CODE`]), or a call a person asked for that the agent does not
    /// take.
    Refused,
    /// The call did not run, for a reason other than an authorization decision: the kill
    /// switch, the run budget, a transfer to another agent, arguments that did not decode, an
    /// unknown tool, an open circuit breaker, a concurrency limit, a before-tool callback or
    /// plugin that failed or answered with text, or an action ledger that could not be used.
    NotRun,
    /// The call waits for a confirmation decision; a later run answers it.
    Held,
    /// The call ran and failed with a known outcome.
    Failed,
    /// The call began and may or may not have taken effect: a non-idempotent call that timed
    /// out or panicked, a call in flight when the run was cancelled or its process stopped, or
    /// one whose action-ledger record was begun without an outcome.
    OutcomeUnknown,
    /// The call ran, but its result is withheld: an after-tool stage failed, or the result
    /// screen withheld it.
    Withheld,
    /// An earlier attempt of the call succeeded, as its action-ledger record shows; the answer
    /// reports that instead of the result, and the call did not run again.
    Succeeded,
}

impl ToolResultStatus {
    /// Whether a provider with a tool-error flag marks the answer as an error: `Refused`,
    /// `NotRun` and `Failed`. The others are not errors to report, or an error flag would
    /// invite the model to repeat a call that ran, may have run, or waits for a decision.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::ToolResultStatus;
    ///
    /// assert!(ToolResultStatus::Failed.is_error());
    /// assert!(!ToolResultStatus::Withheld.is_error());
    /// assert!(!ToolResultStatus::Held.is_error());
    /// ```
    pub fn is_error(self) -> bool {
        matches!(self, Self::Refused | Self::NotRun | Self::Failed)
    }

    /// The status of a call whose tool returned `error`: `Refused` for an access guard's
    /// denial ([`TOOL_PERMISSION_DENIED_CODE`]), which the guard returns before the tool runs,
    /// and `Failed` for any other error.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{AdkError, ErrorCategory, ErrorComponent, TOOL_PERMISSION_DENIED_CODE, ToolResultStatus};
    ///
    /// let denied = AdkError::new(
    ///     ErrorComponent::Tool,
    ///     ErrorCategory::Forbidden,
    ///     TOOL_PERMISSION_DENIED_CODE,
    ///     "missing scope: payments",
    /// );
    /// assert_eq!(ToolResultStatus::of_tool_error(&denied), ToolResultStatus::Refused);
    /// assert_eq!(ToolResultStatus::of_tool_error(&AdkError::tool("boom")), ToolResultStatus::Failed);
    /// ```
    pub fn of_tool_error(error: &AdkError) -> Self {
        if error.code == TOOL_PERMISSION_DENIED_CODE { Self::Refused } else { Self::Failed }
    }

    /// The status of an answer given from an action-ledger record whose outcome is `outcome`:
    /// `Succeeded` and `Failed` as recorded, `NotRun` for a call the kill switch refused after
    /// the record was begun ([`not_run_text`](crate::not_run_text)), and `OutcomeUnknown` for a
    /// record begun without an outcome.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{ActionOutcome, ToolResultStatus};
    ///
    /// let failed = ActionOutcome::Failed { error: "card declined".into() };
    /// assert_eq!(ToolResultStatus::of_record(Some(&failed)), ToolResultStatus::Failed);
    /// let frozen = ActionOutcome::Failed { error: "not run: execution is frozen".into() };
    /// assert_eq!(ToolResultStatus::of_record(Some(&frozen)), ToolResultStatus::NotRun);
    /// assert_eq!(ToolResultStatus::of_record(None), ToolResultStatus::OutcomeUnknown);
    /// ```
    pub fn of_record(outcome: Option<&ActionOutcome>) -> Self {
        match outcome {
            Some(ActionOutcome::Succeeded { .. }) => Self::Succeeded,
            Some(ActionOutcome::Failed { error }) if crate::is_not_run(error) => Self::NotRun,
            Some(ActionOutcome::Failed { .. }) => Self::Failed,
            _ => Self::OutcomeUnknown,
        }
    }

    /// The status of an answer whose result is withheld after it was written, given the
    /// answer's `previous` status and whether the tool `executed` in this dispatch.
    ///
    /// Withholding a result does not change whether the call ran: `NotRun`, `Refused` and
    /// `OutcomeUnknown` stand. Any other answer of a call that ran — now, or in the earlier
    /// attempt an action-ledger replay reports — is `Withheld`. An answer without a status of a
    /// call that did not execute, such as a before-tool callback's substitute, is `NotRun`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::ToolResultStatus;
    ///
    /// // A ledger replay of a call that succeeded never executes now, but it ran.
    /// let replay = Some(ToolResultStatus::Succeeded);
    /// assert_eq!(ToolResultStatus::when_withheld(replay, false), ToolResultStatus::Withheld);
    /// let refusal = Some(ToolResultStatus::Refused);
    /// assert_eq!(ToolResultStatus::when_withheld(refusal, false), ToolResultStatus::Refused);
    /// assert_eq!(ToolResultStatus::when_withheld(None, true), ToolResultStatus::Withheld);
    /// assert_eq!(ToolResultStatus::when_withheld(None, false), ToolResultStatus::NotRun);
    /// ```
    pub fn when_withheld(previous: Option<Self>, executed: bool) -> Self {
        match previous {
            Some(kept @ (Self::NotRun | Self::Refused | Self::OutcomeUnknown)) => kept,
            Some(Self::Failed | Self::Withheld | Self::Succeeded) => Self::Withheld,
            Some(Self::Held) | None if executed => Self::Withheld,
            Some(Self::Held) | None => Self::NotRun,
        }
    }
}

impl Part {
    /// Returns the [`ToolResultStatus`] of a function response the agent wrote, or `None` for
    /// a tool's own result, a callback's answer, another part, or an unknown status value.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{FunctionResponseData, Part, ToolResultStatus};
    /// use serde_json::json;
    ///
    /// let mut part = Part::FunctionResponse {
    ///     function_response: FunctionResponseData::new("pay", json!({"error": "denied"})),
    ///     id: Some("call-1".into()),
    ///     annotations: None,
    /// };
    /// assert_eq!(part.tool_result_status(), None);
    /// part.set_tool_result_status(ToolResultStatus::Refused, None);
    /// assert_eq!(part.tool_result_status(), Some(ToolResultStatus::Refused));
    /// ```
    pub fn tool_result_status(&self) -> Option<ToolResultStatus> {
        ToolResultStatus::deserialize(self.tool_result_annotation(TOOL_RESULT_STATUS_KEY)?).ok()
    }

    /// Returns the code of the error behind a function response's status, when one was.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{AdkError, ErrorCategory, ErrorComponent, FunctionResponseData, Part, ToolResultStatus};
    /// use serde_json::json;
    ///
    /// let timeout =
    ///     AdkError::new(ErrorComponent::Tool, ErrorCategory::Timeout, "tool.timeout", "timed out");
    /// let mut part = Part::FunctionResponse {
    ///     function_response: FunctionResponseData::new("lookup", json!({"error": "timed out"})),
    ///     id: None,
    ///     annotations: None,
    /// };
    /// part.set_tool_result_status(ToolResultStatus::Failed, Some(&timeout));
    /// assert_eq!(part.tool_result_error_code(), Some("tool.timeout"));
    /// ```
    pub fn tool_result_error_code(&self) -> Option<&str> {
        self.tool_result_annotation(TOOL_RESULT_ERROR_CODE_KEY)?.as_str()
    }

    /// Returns the category of the error behind a function response's status, when one was.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{AdkError, ErrorCategory, FunctionResponseData, Part, ToolResultStatus};
    /// use serde_json::json;
    ///
    /// let frozen = AdkError::agent("execution is frozen");
    /// let mut part = Part::FunctionResponse {
    ///     function_response: FunctionResponseData::new("pay", json!({"error": "not run"})),
    ///     id: None,
    ///     annotations: None,
    /// };
    /// part.set_tool_result_status(ToolResultStatus::NotRun, Some(&frozen));
    /// assert_eq!(part.tool_result_error_category(), Some(ErrorCategory::Internal));
    /// ```
    pub fn tool_result_error_category(&self) -> Option<ErrorCategory> {
        ErrorCategory::deserialize(self.tool_result_annotation(TOOL_RESULT_ERROR_CATEGORY_KEY)?)
            .ok()
    }

    /// Sets a function response's [`ToolResultStatus`], with the code and category of `cause`,
    /// the error behind it, when there is one. The payload and the part's other annotations are
    /// kept, a non-object annotation under `"value"`; an earlier cause is replaced. Other parts
    /// are left as they are.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{FunctionResponseData, Part, TOOL_RESULT_STATUS_KEY, ToolResultStatus};
    /// use serde_json::json;
    ///
    /// let mut part = Part::FunctionResponse {
    ///     function_response: FunctionResponseData::new("pay", json!({"error": "held"})),
    ///     id: None,
    ///     annotations: Some(json!({"priority": 1})),
    /// };
    /// part.set_tool_result_status(ToolResultStatus::Held, None);
    /// assert_eq!(part.annotations(), Some(&json!({"priority": 1, TOOL_RESULT_STATUS_KEY: "held"})));
    /// ```
    pub fn set_tool_result_status(&mut self, status: ToolResultStatus, cause: Option<&AdkError>) {
        let Part::FunctionResponse { annotations, .. } = self else { return };
        let mut fields = match annotations.take() {
            Some(Value::Object(fields)) => fields,
            Some(other) => {
                let mut fields = serde_json::Map::new();
                fields.insert("value".to_string(), other);
                fields
            }
            None => serde_json::Map::new(),
        };
        fields.insert(TOOL_RESULT_STATUS_KEY.to_string(), json!(status));
        match cause {
            Some(error) => {
                fields.insert(TOOL_RESULT_ERROR_CODE_KEY.to_string(), json!(error.code));
                fields.insert(TOOL_RESULT_ERROR_CATEGORY_KEY.to_string(), json!(error.category));
            }
            None => {
                fields.remove(TOOL_RESULT_ERROR_CODE_KEY);
                fields.remove(TOOL_RESULT_ERROR_CATEGORY_KEY);
            }
        }
        *annotations = Some(Value::Object(fields));
    }

    fn tool_result_annotation(&self, key: &str) -> Option<&Value> {
        match self {
            Part::FunctionResponse { annotations: Some(annotations), .. } => annotations.get(key),
            _ => None,
        }
    }
}

/// The error code for a call whose arguments do not fit the tool.
///
/// A tool returns an error with this code to tell the model what to change in its arguments,
/// so [`tool_error_response`] always shows its message. The `#[tool]` macro uses it when the
/// arguments do not deserialize.
pub const TOOL_INVALID_ARGUMENTS_CODE: &str = "tool.invalid_arguments";

/// The code adk-auth's scope and RBAC guards put on a denial; a rejection, so its message
/// (the missing scopes or roles) reaches the model (spec 3.1 §7.14).
pub const TOOL_PERMISSION_DENIED_CODE: &str = "tool.permission_denied";

/// The `status` value of the function response for a call that ran but whose result is withheld.
pub const RESULT_WITHHELD_STATUS: &str = "result_withheld";

/// The key under which a function call's arguments record that the model's arguments did not
/// decode to a JSON object.
pub const INVALID_TOOL_ARGUMENTS_KEY: &str = "adk_invalid_tool_arguments";

/// The most bytes of the model's undecodable arguments kept in [`invalid_tool_arguments`].
const RAW_ARGUMENTS_MAX_BYTES: usize = 1024;

/// The most characters of a rejection's message a function response carries.
pub const TOOL_ERROR_MESSAGE_LIMIT: usize = 500;

/// Builds the function response for a tool call that failed with `error`.
///
/// The response names the error's component, category, and code
/// ([`AdkError::redacted`]). A rejection ([`ErrorCategory::is_rejection`](crate::ErrorCategory::is_rejection)) — the tool
/// refused the call or could not use it as made — also carries its message, cut at
/// [`TOOL_ERROR_MESSAGE_LIMIT`] characters, since that message is the reason the model
/// needs to act on. The message of an upstream or internal failure stays out unless
/// `expose_message` is set or the code is [`TOOL_INVALID_ARGUMENTS_CODE`]: it can quote
/// credentials, internal hosts, or upstream bodies, and the function response is sent to
/// the model provider and stored in the session.
///
/// # Example
///
/// ```rust
/// use adk_core::{AdkError, ErrorCategory, ErrorComponent, tool_error_response};
///
/// let err = AdkError::new(
///     ErrorComponent::Tool,
///     ErrorCategory::Unavailable,
///     "tool.orders_db.unavailable",
///     "connect postgres://admin:secret@db.internal/prod failed",
/// );
/// let response = tool_error_response("lookup_order", &err, false);
/// assert_eq!(
///     response["error"],
///     "tool 'lookup_order' failed: tool.unavailable (tool.orders_db.unavailable)"
/// );
/// assert!(tool_error_response("lookup_order", &err, true)["error"]
///     .as_str()
///     .unwrap()
///     .contains("db.internal"));
///
/// let refused = AdkError::new(
///     ErrorComponent::Tool,
///     ErrorCategory::Forbidden,
///     "harness.shell.protected",
///     ".git/hooks is a protected path",
/// );
/// assert_eq!(
///     tool_error_response("bash", &refused, false)["error"],
///     "tool 'bash' failed: tool.forbidden (harness.shell.protected): .git/hooks is a protected path"
/// );
/// ```
pub fn tool_error_response(tool_name: &str, error: &AdkError, expose_message: bool) -> Value {
    let text = if expose_message || error.code == TOOL_INVALID_ARGUMENTS_CODE {
        error.to_string()
    } else if error.category.is_rejection() {
        format!(
            "tool '{tool_name}' failed: {}: {}",
            error.redacted(),
            bounded_message(&error.message)
        )
    } else {
        format!("tool '{tool_name}' failed: {}", error.redacted())
    };
    json!({ "error": text })
}

/// `message` cut at [`TOOL_ERROR_MESSAGE_LIMIT`] characters, marked when cut.
fn bounded_message(message: &str) -> String {
    let mut chars = message.chars();
    let head: String = chars.by_ref().take(TOOL_ERROR_MESSAGE_LIMIT).collect();
    if chars.next().is_some() { format!("{head}… (truncated)") } else { head }
}

/// Builds the function response for a call that ran but whose after-tool pipeline failed.
///
/// An after-tool callback or plugin is where results are redacted and screened, so the result
/// it did not finish with is withheld. The tool's side effect already happened, so the
/// response tells the model not to call the tool again, as a fresh call has a fresh
/// idempotency key and would repeat the effect. `error` is described as in
/// [`tool_error_response`].
///
/// # Example
///
/// ```rust
/// use adk_core::{AdkError, RESULT_WITHHELD_STATUS, result_withheld_response};
///
/// let response = result_withheld_response("charge_card", &AdkError::agent("audit sink down"), false);
/// assert_eq!(response["status"], RESULT_WITHHELD_STATUS);
/// let text = response["error"].as_str().unwrap();
/// assert!(text.contains("do not call it again"));
/// assert!(!text.contains("audit sink down"));
/// ```
pub fn result_withheld_response(tool_name: &str, error: &AdkError, expose_message: bool) -> Value {
    let failure = if expose_message { error.to_string() } else { error.redacted() };
    json!({
        "status": RESULT_WITHHELD_STATUS,
        "error": format!(
            "tool '{tool_name}' ran, but its after-tool pipeline failed with {failure}, so its \
             result is withheld; do not call it again for this request."
        ),
    })
}

/// Builds the function response for a call that ran but whose result a [`ToolResultScreen`]
/// withheld.
///
/// The response carries the screen's `reason` and, as [`result_withheld_response`] does, tells
/// the model not to call the tool again, since its side effect already happened.
///
/// # Example
///
/// ```rust
/// use adk_core::{RESULT_WITHHELD_STATUS, result_blocked_response};
///
/// let response = result_blocked_response("lookup", "guardrail 'pii' (High): names a patient");
/// assert_eq!(response["status"], RESULT_WITHHELD_STATUS);
/// assert!(response["error"].as_str().unwrap().contains("guardrail 'pii'"));
/// ```
pub fn result_blocked_response(tool_name: &str, reason: &str) -> Value {
    json!({
        "status": RESULT_WITHHELD_STATUS,
        "error": format!(
            "tool '{tool_name}' ran, but its result was withheld: {reason}; do not call it again for \
             this request."
        ),
    })
}

/// Screens a tool's result before the model and the session see it.
///
/// An agent runs the screen on every result it is about to send to the model, after its own
/// after-tool callbacks and plugins, so nothing reaches the provider or the stored tool event
/// that the screen did not pass. `adk-guardrail`'s `ToolGuardrailSet` implements it, so the
/// guardrails' `validate_result` checks apply wherever a screen is accepted.
///
/// A screen that cannot decide, because the service it relies on is down for example, returns
/// `Err`, and the result is withheld: the agent fails closed.
///
/// # Example
///
/// ```rust
/// use adk_core::{ToolResultScreen, async_trait};
/// use serde_json::{Value, json};
///
/// /// Drops a `secret` field from every result.
/// struct DropSecrets;
///
/// #[async_trait]
/// impl ToolResultScreen for DropSecrets {
///     async fn screen_result(&self, _tool: &str, _args: &Value, result: &Value) -> Result<Value, String> {
///         let mut result = result.clone();
///         if let Some(fields) = result.as_object_mut() {
///             fields.remove("secret");
///         }
///         Ok(result)
///     }
/// }
///
/// # futures::executor::block_on(async {
/// let screened = DropSecrets.screen_result("lookup", &json!({}), &json!({"id": 1, "secret": "s"})).await;
/// assert_eq!(screened, Ok(json!({"id": 1})));
/// # });
/// ```
#[async_trait]
pub trait ToolResultScreen: Send + Sync {
    /// Returns the result to deliver, possibly rewritten, or why it is withheld.
    ///
    /// `args` are the arguments the model sent for the call.
    async fn screen_result(
        &self,
        tool_name: &str,
        args: &Value,
        result: &Value,
    ) -> std::result::Result<Value, String>;
}

/// Runs `screen` on `result`, returning the result to deliver, or the
/// [`result_blocked_response`] that replaces a withheld one.
///
/// # Example
///
/// ```rust
/// use adk_core::{RESULT_WITHHELD_STATUS, ToolResultScreen, async_trait, screen_tool_result};
/// use serde_json::{Value, json};
///
/// struct Unavailable;
///
/// #[async_trait]
/// impl ToolResultScreen for Unavailable {
///     async fn screen_result(&self, _: &str, _: &Value, _: &Value) -> Result<Value, String> {
///         Err("the redaction service is unavailable".to_string())
///     }
/// }
///
/// # futures::executor::block_on(async {
/// let delivered = screen_tool_result(&Unavailable, "lookup", &json!({}), json!({"ssn": "x"})).await;
/// assert_eq!(delivered["status"], RESULT_WITHHELD_STATUS);
/// # });
/// ```
pub async fn screen_tool_result(
    screen: &dyn ToolResultScreen,
    tool_name: &str,
    args: &Value,
    result: Value,
) -> Value {
    match screen.screen_result(tool_name, args, &result).await {
        Ok(screened) => screened,
        Err(reason) => {
            // The reason can describe the result, so it goes to the model, not the log.
            tracing::warn!(tool.name = %tool_name, "tool result withheld by its screen");
            result_blocked_response(tool_name, &reason)
        }
    }
}

/// Builds the arguments of a function call whose arguments the model sent as something other
/// than a JSON object, such as truncated or malformed JSON.
///
/// A provider adapter emits the call with these arguments instead of failing the turn, so the
/// agent can answer the call with an error and the model can try again. The value is a JSON
/// object, so every provider accepts the call when it is replayed in history. `raw` is kept up
/// to 1 KiB.
///
/// # Example
///
/// ```rust
/// use adk_core::{invalid_tool_arguments, invalid_tool_arguments_reason};
///
/// let args = invalid_tool_arguments("arguments are not valid JSON", r#"{"city": "Nair"#);
/// assert_eq!(invalid_tool_arguments_reason(&args), Some("arguments are not valid JSON"));
/// ```
pub fn invalid_tool_arguments(reason: impl Into<String>, raw: &str) -> Value {
    let mut end = raw.len().min(RAW_ARGUMENTS_MAX_BYTES);
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    json!({ INVALID_TOOL_ARGUMENTS_KEY: { "reason": reason.into(), "raw": &raw[..end] } })
}

/// Returns why the model's arguments could not be decoded, when `args` were built by
/// [`invalid_tool_arguments`].
///
/// An agent answers such a call with an error and does not execute the tool.
///
/// # Example
///
/// ```rust
/// use adk_core::invalid_tool_arguments_reason;
/// use serde_json::json;
///
/// assert_eq!(invalid_tool_arguments_reason(&json!({ "city": "Nairobi" })), None);
/// ```
pub fn invalid_tool_arguments_reason(args: &Value) -> Option<&str> {
    let fields = args.as_object().filter(|fields| fields.len() == 1)?;
    fields.get(INVALID_TOOL_ARGUMENTS_KEY)?.get("reason")?.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ErrorCategory, ErrorComponent};

    #[test]
    fn permission_denials_keep_their_message() {
        let err = AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::Forbidden,
            TOOL_PERMISSION_DENIED_CODE,
            "missing required scopes: [verified] (tool requires: [verified])",
        );
        assert_eq!(
            tool_error_response("transfer", &err, false)["error"],
            "tool 'transfer' failed: tool.forbidden (tool.permission_denied): missing required \
             scopes: [verified] (tool requires: [verified])"
        );
    }

    #[test]
    fn a_rejection_keeps_its_message_bounded_and_a_failure_does_not() {
        let refused = AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::Forbidden,
            "harness.shell.protected",
            ".git/hooks is a protected path; ask for approval",
        );
        assert_eq!(
            tool_error_response("bash", &refused, false)["error"],
            "tool 'bash' failed: tool.forbidden (harness.shell.protected): .git/hooks is a \
             protected path; ask for approval"
        );

        let long = AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::InvalidInput,
            "tool.path.outside_workspace",
            "é".repeat(TOOL_ERROR_MESSAGE_LIMIT + 7),
        );
        let text =
            tool_error_response("read_file", &long, false)["error"].as_str().unwrap().to_string();
        let expected_head = "é".repeat(TOOL_ERROR_MESSAGE_LIMIT);
        assert_eq!(
            text,
            format!(
                "tool 'read_file' failed: tool.invalid_input (tool.path.outside_workspace): \
                 {expected_head}… (truncated)"
            )
        );

        for category in [
            ErrorCategory::Unauthorized,
            ErrorCategory::RateLimited,
            ErrorCategory::Timeout,
            ErrorCategory::Unavailable,
            ErrorCategory::Cancelled,
            ErrorCategory::Internal,
        ] {
            let failed = AdkError::new(ErrorComponent::Tool, category, "tool.x", "secret body");
            assert_eq!(
                tool_error_response("x", &failed, false)["error"],
                format!("tool 'x' failed: tool.{category} (tool.x)"),
                "{category} withholds its message"
            );
        }
    }

    #[test]
    fn invalid_arguments_errors_keep_their_message() {
        let err = AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::InvalidInput,
            TOOL_INVALID_ARGUMENTS_CODE,
            "missing field `city`",
        );
        assert_eq!(
            tool_error_response("weather", &err, false),
            json!({ "error": "tool.invalid_input: missing field `city`" })
        );
    }

    #[test]
    fn withheld_response_exposes_the_message_only_when_asked() {
        let err = AdkError::agent("audit sink https://u:token@audit.internal unavailable");
        let hidden = result_withheld_response("charge", &err, false);
        assert_eq!(
            hidden,
            json!({
                "status": RESULT_WITHHELD_STATUS,
                "error": "tool 'charge' ran, but its after-tool pipeline failed with \
                          agent.internal (agent.legacy), so its result is withheld; do not call \
                          it again for this request.",
            })
        );
        let shown = result_withheld_response("charge", &err, true);
        assert!(shown["error"].as_str().unwrap().contains("audit.internal"));
    }

    #[test]
    fn raw_arguments_are_capped_on_a_char_boundary() {
        let raw = "é".repeat(RAW_ARGUMENTS_MAX_BYTES);
        let args = invalid_tool_arguments("too long", &raw);
        let kept = args[INVALID_TOOL_ARGUMENTS_KEY]["raw"].as_str().unwrap();
        assert!(kept.len() <= RAW_ARGUMENTS_MAX_BYTES);
        assert!(raw.starts_with(kept));
    }

    #[test]
    fn only_the_marker_object_is_invalid_arguments() {
        let args = invalid_tool_arguments("bad", "{");
        assert_eq!(invalid_tool_arguments_reason(&args), Some("bad"));
        let mut extended = args.clone();
        extended["city"] = json!("Nairobi");
        assert_eq!(invalid_tool_arguments_reason(&extended), None);
        assert_eq!(invalid_tool_arguments_reason(&json!("{")), None);
    }

    fn response(annotations: Option<Value>) -> Part {
        Part::FunctionResponse {
            function_response: crate::FunctionResponseData::new(
                "lookup",
                json!({"error": "x", TOOL_RESULT_STATUS_KEY: "failed"}),
            ),
            id: Some("call-1".into()),
            annotations,
        }
    }

    #[test]
    fn every_status_round_trips_through_the_annotation() {
        for (encoded, status) in [
            ("refused", ToolResultStatus::Refused),
            ("not_run", ToolResultStatus::NotRun),
            ("held", ToolResultStatus::Held),
            ("failed", ToolResultStatus::Failed),
            ("outcome_unknown", ToolResultStatus::OutcomeUnknown),
            ("withheld", ToolResultStatus::Withheld),
            ("succeeded", ToolResultStatus::Succeeded),
        ] {
            let mut part = response(None);
            part.set_tool_result_status(status, None);
            assert_eq!(part, response(Some(json!({ TOOL_RESULT_STATUS_KEY: encoded }))));
            let decoded: Part =
                serde_json::from_value(serde_json::to_value(&part).unwrap()).unwrap();
            assert_eq!(decoded.tool_result_status(), Some(status));
        }
    }

    #[test]
    fn the_status_is_read_from_the_annotation_never_the_payload() {
        assert_eq!(response(None).tool_result_status(), None);
        assert_eq!(
            response(Some(json!({ TOOL_RESULT_STATUS_KEY: "cancelled" }))).tool_result_status(),
            None
        );
        let file = Part::FileData {
            mime_type: "text/plain".into(),
            file_uri: "https://example.test/a".into(),
            annotations: Some(json!({ TOOL_RESULT_STATUS_KEY: "failed" })),
        };
        assert_eq!(file.tool_result_status(), None);
    }

    #[test]
    fn setting_a_status_keeps_other_annotations_and_replaces_the_cause() {
        let timeout =
            AdkError::new(ErrorComponent::Tool, ErrorCategory::Timeout, "tool.timeout", "slow");
        let mut part = response(Some(json!({ "priority": 1 })));
        part.set_tool_result_status(ToolResultStatus::OutcomeUnknown, Some(&timeout));
        assert_eq!(
            part,
            response(Some(json!({
                "priority": 1,
                TOOL_RESULT_STATUS_KEY: "outcome_unknown",
                TOOL_RESULT_ERROR_CODE_KEY: "tool.timeout",
                TOOL_RESULT_ERROR_CATEGORY_KEY: "timeout",
            })))
        );
        assert_eq!(part.tool_result_error_code(), Some("tool.timeout"));
        assert_eq!(part.tool_result_error_category(), Some(ErrorCategory::Timeout));

        part.set_tool_result_status(ToolResultStatus::Withheld, None);
        assert_eq!(
            part,
            response(Some(json!({ "priority": 1, TOOL_RESULT_STATUS_KEY: "withheld" })))
        );

        let mut odd = response(Some(json!("note")));
        odd.set_tool_result_status(ToolResultStatus::Held, None);
        assert_eq!(odd, response(Some(json!({ "value": "note", TOOL_RESULT_STATUS_KEY: "held" }))));
    }

    #[test]
    fn a_ledger_record_maps_to_one_status() {
        let cases = [
            (
                Some(ActionOutcome::Succeeded { result_digest: "d".into() }),
                ToolResultStatus::Succeeded,
            ),
            (Some(ActionOutcome::Failed { error: "declined".into() }), ToolResultStatus::Failed),
            (
                Some(ActionOutcome::Failed {
                    error: crate::not_run_text(&AdkError::agent("frozen")),
                }),
                ToolResultStatus::NotRun,
            ),
            (None, ToolResultStatus::OutcomeUnknown),
        ];
        for (outcome, status) in cases {
            assert_eq!(ToolResultStatus::of_record(outcome.as_ref()), status, "{outcome:?}");
        }
    }

    #[test]
    fn withholding_keeps_what_is_known_about_whether_the_call_ran() {
        use ToolResultStatus::*;
        let cases = [
            (Some(NotRun), true, NotRun),
            (Some(Refused), true, Refused),
            (Some(OutcomeUnknown), false, OutcomeUnknown),
            (Some(Failed), false, Withheld),
            (Some(Succeeded), false, Withheld),
            (Some(Withheld), false, Withheld),
            (Some(Held), false, NotRun),
            (None, true, Withheld),
            (None, false, NotRun),
        ];
        for (previous, executed, expected) in cases {
            assert_eq!(
                ToolResultStatus::when_withheld(previous, executed),
                expected,
                "{previous:?}, executed {executed}"
            );
        }
        let errors: Vec<_> = [Refused, NotRun, Held, Failed, OutcomeUnknown, Withheld, Succeeded]
            .into_iter()
            .filter(|status| status.is_error())
            .collect();
        assert_eq!(errors, [Refused, NotRun, Failed]);
    }
}
