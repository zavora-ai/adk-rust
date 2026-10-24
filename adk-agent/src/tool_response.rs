//! Responses constructed by the tool dispatcher rather than returned by a tool.

use adk_core::{AdkError, Content, ErrorCategory, FunctionResponseData, Part, ToolResultStatus};
use serde_json::{Value, json};

pub(crate) struct ToolResponse {
    pub(crate) value: Value,
    pub(crate) status: Option<ToolResultStatus>,
    error: Option<(&'static str, ErrorCategory)>,
}

impl ToolResponse {
    pub(crate) fn new(value: Value) -> Self {
        Self { value, status: None, error: None }
    }

    pub(crate) fn with_status(value: Value, status: ToolResultStatus) -> Self {
        Self { value, status: Some(status), error: None }
    }

    pub(crate) fn unsuccessful(value: Value, executed: bool, withheld: bool) -> Self {
        Self::with_status(value, ToolResultStatus::derive(executed, withheld))
    }

    pub(crate) fn not_run(message: impl Into<String>) -> Self {
        Self::unsuccessful(json!({ "error": message.into() }), false, false)
    }

    pub(crate) fn caused_by(mut self, error: &AdkError) -> Self {
        self.error = Some((error.code, error.category));
        self
    }

    pub(crate) fn with_value(self, value: Value) -> Self {
        Self { value, ..self }
    }

    pub(crate) fn into_part(self, name: &str, id: Option<&String>) -> Part {
        let mut part = Part::FunctionResponse {
            function_response: FunctionResponseData::from_tool_result(name, self.value),
            id: id.cloned(),
            annotations: None,
        };
        if let Some(status) = self.status {
            part.set_tool_result_status(status, self.error);
        }
        part
    }

    pub(crate) fn into_content(self, name: &str, id: Option<&String>) -> Content {
        Content { role: "function".to_string(), parts: vec![self.into_part(name, id)] }
    }

    /// A result transform cannot establish whether an uncertain side effect completed.
    pub(crate) fn preserve_unknown(previous: &Content, content: &mut Content) {
        let Some(previous) = previous
            .parts
            .iter()
            .find(|part| part.tool_result_status() == Some(ToolResultStatus::OutcomeUnknown))
        else {
            return;
        };
        let carried =
            previous.annotations().and_then(Value::as_object).cloned().unwrap_or_default();
        let error = previous.tool_result_error_code().zip(previous.tool_result_error_category());
        for part in &mut content.parts {
            let Part::FunctionResponse { annotations, .. } = part else { continue };
            let mut fields = carried.clone();
            if let Some(Value::Object(current)) = annotations.take() {
                fields.extend(current);
            }
            *annotations = Some(Value::Object(fields));
            part.set_tool_result_status(ToolResultStatus::OutcomeUnknown, error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{
        TOOL_RESULT_ERROR_CATEGORY_KEY, TOOL_RESULT_ERROR_CODE_KEY, TOOL_RESULT_STATUS_KEY,
    };

    #[test]
    fn replacement_keeps_unknown_annotations() {
        let mut previous = ToolResponse::with_status(
            json!({"status": "outcome_unknown"}),
            ToolResultStatus::OutcomeUnknown,
        )
        .caused_by(&AdkError::new(
            adk_core::ErrorComponent::Tool,
            ErrorCategory::Timeout,
            "tool.timeout",
            "expired",
        ))
        .into_content("charge", None);
        if let Part::FunctionResponse { annotations, .. } = &mut previous.parts[0] {
            *annotations = Some(json!({
                TOOL_RESULT_STATUS_KEY: ToolResultStatus::OutcomeUnknown,
                TOOL_RESULT_ERROR_CODE_KEY: "tool.timeout",
                TOOL_RESULT_ERROR_CATEGORY_KEY: ErrorCategory::Timeout,
                "trace": {"source": "execution"}
            }));
        }
        let mut replacement = ToolResponse::unsuccessful(json!({"error": "withheld"}), true, true)
            .into_content("charge", None);
        if let Part::FunctionResponse { annotations, .. } = &mut replacement.parts[0] {
            *annotations = Some(json!({
                TOOL_RESULT_STATUS_KEY: ToolResultStatus::Failed,
                "audit": "screened"
            }));
        }
        ToolResponse::preserve_unknown(&previous, &mut replacement);
        assert_eq!(
            json!(replacement),
            json!(Content {
                role: "function".to_string(),
                parts: vec![Part::FunctionResponse {
                    function_response: FunctionResponseData::new(
                        "charge",
                        json!({"error": "withheld"})
                    ),
                    id: None,
                    annotations: Some(json!({
                        TOOL_RESULT_STATUS_KEY: ToolResultStatus::OutcomeUnknown,
                        TOOL_RESULT_ERROR_CODE_KEY: "tool.timeout",
                        TOOL_RESULT_ERROR_CATEGORY_KEY: ErrorCategory::Timeout,
                        "trace": {"source": "execution"},
                        "audit": "screened"
                    })),
                }],
            })
        );
    }

    #[test]
    fn known_results_keep_replacements() {
        for status in [
            None,
            Some(ToolResultStatus::Failed),
            Some(ToolResultStatus::NotRun),
            Some(ToolResultStatus::Withheld),
            Some(ToolResultStatus::Pending),
        ] {
            let value = json!({"isError": true, "error": "domain row"});
            let previous = match status {
                Some(status) => ToolResponse::with_status(value.clone(), status),
                None => ToolResponse::new(value.clone()),
            }
            .into_content("charge", None);
            let mut replacement = ToolResponse::new(value).into_content("charge", None);
            let expected = replacement.clone();
            ToolResponse::preserve_unknown(&previous, &mut replacement);
            assert_eq!(json!(replacement), json!(expected));
        }
    }

    #[test]
    fn explicit_annotations_can_resolve_uncertainty() {
        for status in [
            ToolResultStatus::Failed,
            ToolResultStatus::NotRun,
            ToolResultStatus::Withheld,
            ToolResultStatus::Pending,
        ] {
            let mut part = ToolResponse::with_status(Value::Null, ToolResultStatus::OutcomeUnknown)
                .into_part("charge", None);
            part.set_tool_result_status(status, None);
            assert_eq!(part.tool_result_status(), Some(status));
        }
    }
}
