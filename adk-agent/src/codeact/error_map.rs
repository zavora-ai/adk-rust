//! Error-message helpers for the strings raised into scripts and fed back to
//! the model.
//!
//! The framework is language-agnostic, so these are just message strings. When
//! a tool fails or is denied, the driver raises an error *into* the script via
//! [`ResumeWith::Raise`](crate::codeact::ResumeWith); the runtime represents it
//! however its language does (a Python exception, a JS throw, etc.).

use adk_core::AdkError;
use serde_json::Value;

/// Message raised into the script when a tool fails.
pub fn tool_error_message(err: &AdkError) -> String {
    format!("{}: {}", err.category, err.message)
}

/// Message raised when the script calls a tool that is not registered.
pub fn unknown_tool_message(tool_name: &str) -> String {
    format!("tool '{tool_name}' is not available")
}

/// Message raised when a human denies a confirmation-gated tool call.
pub fn denied_message(tool_name: &str) -> String {
    format!("call to '{tool_name}' was denied by a human reviewer")
}

/// Render a tool result for inclusion in an observation fed back to the model.
pub(crate) fn render_value(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{ErrorCategory, ErrorComponent};

    #[test]
    fn tool_error_includes_category_and_message() {
        let err = AdkError::new(ErrorComponent::Tool, ErrorCategory::NotFound, "t", "no file");
        let msg = tool_error_message(&err);
        assert!(msg.contains("not_found"));
        assert!(msg.contains("no file"));
    }

    #[test]
    fn helpers_name_the_tool() {
        assert!(unknown_tool_message("foo").contains("foo"));
        assert!(denied_message("rm").contains("rm"));
    }
}
