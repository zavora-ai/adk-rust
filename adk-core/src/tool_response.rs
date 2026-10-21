//! Function responses for tool calls that do not return their result.
//!
//! An agent answers every function call with exactly one function response. When the tool
//! fails, when its result cannot be released, or when the model's arguments cannot be decoded,
//! these builders produce that response without passing error detail to the model by default.
//! A [`ToolResultScreen`] decides what of a tool's result the model and the session may see.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::AdkError;

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
/// ([`AdkError::redacted`]). A rejection ([`ErrorCategory::is_rejection`]) — the tool
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
}
