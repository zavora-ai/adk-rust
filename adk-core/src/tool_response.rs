//! Function responses for tool calls that do not return their result.
//!
//! An agent answers every function call with exactly one function response. When the tool
//! fails, when its result cannot be released, or when the model's arguments cannot be decoded,
//! these builders produce that response without passing error detail to the model by default.

use serde_json::{Value, json};

use crate::AdkError;

/// The error code for a call whose arguments do not fit the tool.
///
/// A tool returns an error with this code to tell the model what to change in its arguments,
/// so [`tool_error_response`] always shows its message. The `#[tool]` macro uses it when the
/// arguments do not deserialize.
pub const TOOL_INVALID_ARGUMENTS_CODE: &str = "tool.invalid_arguments";

/// The `status` value of the function response for a call that ran but whose result is withheld.
pub const RESULT_WITHHELD_STATUS: &str = "result_withheld";

/// The key under which a function call's arguments record that the model's arguments did not
/// decode to a JSON object.
pub const INVALID_TOOL_ARGUMENTS_KEY: &str = "adk_invalid_tool_arguments";

/// The most bytes of the model's undecodable arguments kept in [`invalid_tool_arguments`].
const RAW_ARGUMENTS_MAX_BYTES: usize = 1024;

/// Builds the function response for a tool call that failed with `error`.
///
/// The response names the error's component, category, and code
/// ([`AdkError::redacted`]); its message stays out unless `expose_message` is set or the code is
/// [`TOOL_INVALID_ARGUMENTS_CODE`]. A message can quote credentials, internal hosts, or upstream
/// bodies, and the function response is sent to the model provider and stored in the session.
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
/// ```
pub fn tool_error_response(tool_name: &str, error: &AdkError, expose_message: bool) -> Value {
    let text = if expose_message || error.code == TOOL_INVALID_ARGUMENTS_CODE {
        error.to_string()
    } else {
        format!("tool '{tool_name}' failed: {}", error.redacted())
    };
    json!({ "error": text })
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
