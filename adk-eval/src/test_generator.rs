//! LLM-driven test case generation.
//!
//! Generates evaluation test cases from natural language descriptions (via LLM)
//! or from production event logs (direct extraction). Produced cases follow
//! the standard [`TestFile`] JSON format and include generation metadata.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_eval::test_generator::{TestGenerator, GeneratorConfig};
//! use std::sync::Arc;
//!
//! let generator = TestGenerator::with_config(model, GeneratorConfig {
//!     cases_per_description: 3,
//!     include_tool_expectations: true,
//! });
//!
//! let cases = generator
//!     .generate_from_description("A weather assistant that can look up forecasts")
//!     .await?;
//! ```

use std::sync::Arc;

use adk_core::{Event, Llm, LlmRequest, Part};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::error::{EvalError, Result};
use crate::schema::{ContentData, EvalCase, Turn};

/// Configuration for test case generation.
#[derive(Debug, Clone)]
pub struct GeneratorConfig {
    /// Number of cases to generate per description.
    pub cases_per_description: usize,
    /// Whether to include tool use expectations in generated cases.
    pub include_tool_expectations: bool,
}

impl Default for GeneratorConfig {
    fn default() -> Self {
        Self { cases_per_description: 5, include_tool_expectations: true }
    }
}

/// Metadata for generated eval cases.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EvalCaseMetadata {
    /// Whether this case was auto-generated.
    #[serde(default)]
    pub generated: bool,
    /// Source description (e.g., "description: ..." or "events").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Generates evaluation test cases from descriptions or event logs.
pub struct TestGenerator {
    model: Arc<dyn Llm>,
    config: GeneratorConfig,
}

impl TestGenerator {
    /// Creates a new test generator with default configuration.
    pub fn new(model: Arc<dyn Llm>) -> Self {
        Self { model, config: GeneratorConfig::default() }
    }

    /// Creates a new test generator with custom configuration.
    pub fn with_config(model: Arc<dyn Llm>, config: GeneratorConfig) -> Self {
        Self { model, config }
    }

    /// Generate eval cases from a natural language description.
    ///
    /// Prompts the LLM to produce eval case definitions as JSON. On parse failure
    /// for any individual case, a warning is logged and that case is skipped
    /// without aborting the batch.
    pub async fn generate_from_description(&self, description: &str) -> Result<Vec<EvalCase>> {
        let prompt = self.build_generation_prompt(description);

        let request = LlmRequest::new(
            self.model.name().to_string(),
            vec![adk_core::Content::new("user").with_text(&prompt)],
        );

        let mut stream = self
            .model
            .generate_content(request, false)
            .await
            .map_err(|e| EvalError::GenerationError(format!("LLM request failed: {e}")))?;

        // Collect the full response text
        let mut response_text = String::new();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(response) => {
                    if let Some(content) = &response.content {
                        for part in &content.parts {
                            if let Part::Text { text } = part {
                                response_text.push_str(text);
                            }
                        }
                    }
                }
                Err(e) => {
                    return Err(EvalError::GenerationError(format!("LLM stream error: {e}")));
                }
            }
        }

        // Parse the response into eval cases
        self.parse_generated_cases(&response_text, description)
    }

    /// Generate eval cases from production event logs.
    ///
    /// Extracts conversation turns from the provided events, constructing
    /// [`EvalCase`] objects directly without invoking the LLM.
    pub fn generate_from_events(&self, events: &[Event]) -> Result<Vec<EvalCase>> {
        if events.is_empty() {
            return Ok(Vec::new());
        }

        // Group events by invocation_id to form conversation turns
        let mut invocations: Vec<(String, Vec<&Event>)> = Vec::new();
        for event in events {
            if let Some(last) = invocations.last_mut()
                && last.0 == event.invocation_id
            {
                last.1.push(event);
                continue;
            }
            invocations.push((event.invocation_id.clone(), vec![event]));
        }

        let mut turns = Vec::new();

        for (invocation_id, inv_events) in &invocations {
            let mut user_text = String::new();
            let mut model_text = String::new();
            let mut tool_uses = Vec::new();

            for event in inv_events {
                if let Some(content) = event.content() {
                    match content.role.as_str() {
                        "user" => {
                            for part in &content.parts {
                                if let Part::Text { text } = part {
                                    if !user_text.is_empty() {
                                        user_text.push(' ');
                                    }
                                    user_text.push_str(text);
                                }
                            }
                        }
                        "model" => {
                            for part in &content.parts {
                                match part {
                                    Part::Text { text } => {
                                        if !model_text.is_empty() {
                                            model_text.push(' ');
                                        }
                                        model_text.push_str(text);
                                    }
                                    Part::FunctionCall { name, args, .. }
                                        if self.config.include_tool_expectations =>
                                    {
                                        tool_uses.push(crate::schema::ToolUse {
                                            name: name.clone(),
                                            args: args.clone(),
                                            expected_response: None,
                                        });
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }

            // Only create a turn if we have user content
            if !user_text.is_empty() {
                let final_response = if model_text.is_empty() {
                    None
                } else {
                    Some(ContentData::model_response(&model_text))
                };

                let intermediate_data = if tool_uses.is_empty() {
                    None
                } else {
                    Some(crate::schema::IntermediateData {
                        tool_uses,
                        intermediate_responses: Vec::new(),
                    })
                };

                turns.push(Turn {
                    invocation_id: invocation_id.clone(),
                    user_content: ContentData::text(&user_text),
                    final_response,
                    intermediate_data,
                });
            }
        }

        if turns.is_empty() {
            return Ok(Vec::new());
        }

        let eval_case = EvalCase {
            eval_id: format!("generated_from_events_{}", uuid::Uuid::new_v4()),
            description: "Generated from event logs".to_string(),
            conversation: turns,
            session_input: Default::default(),
            tags: vec!["generated".to_string()],
            metadata: Some(EvalCaseMetadata {
                generated: true,
                source: Some("events".to_string()),
            }),
        };

        Ok(vec![eval_case])
    }

    /// Build the prompt for LLM-based case generation.
    fn build_generation_prompt(&self, description: &str) -> String {
        let tool_instruction = if self.config.include_tool_expectations {
            r#"Include "intermediate_data" with "tool_uses" where appropriate, each with "name" and "args" fields."#
        } else {
            r#"Do not include "intermediate_data" in the output."#
        };

        format!(
            r#"Generate exactly {count} evaluation test cases for the following agent description:

"{description}"

Each test case must be a JSON object with these fields:
- "eval_id": a unique string identifier (e.g., "test_1", "test_2")
- "description": a brief description of what the test case validates
- "conversation": an array of conversation turns, each with:
  - "invocation_id": a unique string (e.g., "inv_1")
  - "user_content": object with "parts": [{{"text": "..."}}] and "role": "user"
  - "final_response": object with "parts": [{{"text": "..."}}] and "role": "model"
  {tool_instruction}

Output ONLY a JSON array of test case objects. No markdown fences, no explanation text.
Example format:
[
  {{
    "eval_id": "test_1",
    "description": "Basic greeting test",
    "conversation": [
      {{
        "invocation_id": "inv_1",
        "user_content": {{"parts": [{{"text": "Hello"}}], "role": "user"}},
        "final_response": {{"parts": [{{"text": "Hi there! How can I help?"}}], "role": "model"}}
      }}
    ]
  }}
]"#,
            count = self.config.cases_per_description,
            description = description,
            tool_instruction = tool_instruction,
        )
    }

    /// Parse the LLM response text into eval cases, skipping unparseable entries.
    fn parse_generated_cases(
        &self,
        response_text: &str,
        description: &str,
    ) -> Result<Vec<EvalCase>> {
        let json_text = extract_json_array(response_text).unwrap_or(response_text);

        // Try parsing as an array of eval cases
        let raw_cases: Vec<serde_json::Value> = match serde_json::from_str(json_text) {
            Ok(cases) => cases,
            Err(e) => {
                // Try to extract JSON array from the text
                warn!("failed to parse LLM response as JSON array: {e}");
                return Err(EvalError::GenerationError(format!(
                    "LLM returned unparseable response: {e}"
                )));
            }
        };

        let source = format!("description: {description}");
        let mut cases = Vec::new();

        for (i, raw_case) in raw_cases.iter().enumerate() {
            match serde_json::from_value::<EvalCase>(raw_case.clone()) {
                Ok(mut eval_case) => {
                    // Add generation tags
                    if !eval_case.tags.contains(&"generated".to_string()) {
                        eval_case.tags.push("generated".to_string());
                    }
                    cases.push(eval_case);
                }
                Err(e) => {
                    // Log warning and skip this case without aborting the batch
                    warn!(
                        case_index = i,
                        error = %e,
                        "skipping unparseable generated case"
                    );
                }
            }
        }

        if cases.is_empty() && !raw_cases.is_empty() {
            return Err(EvalError::GenerationError(format!(
                "all {count} generated cases failed to parse (source: {source})",
                count = raw_cases.len(),
            )));
        }

        // Attach metadata as tags for traceability
        // The full EvalCaseMetadata integration happens in task 16.2
        for case in &mut cases {
            if !case.tags.contains(&source) {
                case.tags.push(source.clone());
            }
        }

        Ok(cases)
    }
}

/// Extract a JSON array from text that may contain markdown fences or prose.
///
/// Handles common LLM output patterns:
/// - Raw JSON array
/// - JSON wrapped in ```json ... ``` fences
/// - JSON embedded in prose text
fn extract_json_array(text: &str) -> Option<&str> {
    let trimmed = text.trim();

    // If it already starts with '[', use it directly
    if trimmed.starts_with('[') {
        return Some(trimmed);
    }

    // Try to find JSON within markdown code fences
    if let Some(start) = trimmed.find("```json") {
        let content_start = start + "```json".len();
        if let Some(end) = trimmed[content_start..].find("```") {
            let json_content = trimmed[content_start..content_start + end].trim();
            if json_content.starts_with('[') {
                return Some(json_content);
            }
        }
    }

    // Try generic code fences
    if let Some(start) = trimmed.find("```") {
        let content_start = start + 3;
        // Skip the optional language identifier on the same line
        let line_end = trimmed[content_start..]
            .find('\n')
            .map(|i| content_start + i + 1)
            .unwrap_or(content_start);
        if let Some(end) = trimmed[line_end..].find("```") {
            let json_content = trimmed[line_end..line_end + end].trim();
            if json_content.starts_with('[') {
                return Some(json_content);
            }
        }
    }

    // Try to find a JSON array anywhere in the text
    if let Some(start) = trimmed.find('[')
        && let Some(end) = trimmed.rfind(']')
        && end > start
    {
        return Some(&trimmed[start..=end]);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generator_config_defaults() {
        let config = GeneratorConfig::default();
        assert_eq!(config.cases_per_description, 5);
        assert!(config.include_tool_expectations);
    }

    #[test]
    fn test_extract_json_array_raw() {
        let input = r#"[{"eval_id": "test_1"}]"#;
        let result = extract_json_array(input);
        assert_eq!(result, Some(input));
    }

    #[test]
    fn test_extract_json_array_fenced() {
        let input = "Here are the cases:\n```json\n[{\"eval_id\": \"test_1\"}]\n```\nDone!";
        let result = extract_json_array(input);
        assert_eq!(result, Some(r#"[{"eval_id": "test_1"}]"#));
    }

    #[test]
    fn test_extract_json_array_embedded() {
        let input = "Sure, here are the cases: [{\"eval_id\": \"test_1\"}] and that's all.";
        let result = extract_json_array(input);
        assert_eq!(result, Some(r#"[{"eval_id": "test_1"}]"#));
    }

    #[test]
    fn test_extract_json_array_no_array() {
        let input = "No JSON here at all.";
        let result = extract_json_array(input);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_json_array_with_whitespace() {
        let input = "  \n  [{\"eval_id\": \"test_1\"}]  \n  ";
        let result = extract_json_array(input);
        assert_eq!(result, Some(r#"[{"eval_id": "test_1"}]"#));
    }

    #[test]
    fn test_generate_from_events_empty() {
        use adk_core::Llm;
        use async_trait::async_trait;

        struct MockLlm;

        #[async_trait]
        impl Llm for MockLlm {
            fn name(&self) -> &str {
                "mock"
            }
            async fn generate_content(
                &self,
                _req: LlmRequest,
                _stream: bool,
            ) -> adk_core::Result<adk_core::LlmResponseStream> {
                unimplemented!()
            }
        }

        let generator = TestGenerator::new(Arc::new(MockLlm));
        let result = generator.generate_from_events(&[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_generate_from_events_with_conversation() {
        use adk_core::{Content, Llm, LlmResponse};
        use async_trait::async_trait;

        struct MockLlm;

        #[async_trait]
        impl Llm for MockLlm {
            fn name(&self) -> &str {
                "mock"
            }
            async fn generate_content(
                &self,
                _req: LlmRequest,
                _stream: bool,
            ) -> adk_core::Result<adk_core::LlmResponseStream> {
                unimplemented!()
            }
        }

        let mut events = Vec::new();

        // User event
        let mut user_event = Event::new("inv_1");
        user_event.author = "user".to_string();
        user_event.llm_response = LlmResponse {
            content: Some(Content::new("user").with_text("What is the weather?")),
            ..Default::default()
        };
        events.push(user_event);

        // Model response event
        let mut model_event = Event::new("inv_1");
        model_event.author = "model".to_string();
        model_event.llm_response = LlmResponse {
            content: Some(Content::new("model").with_text("The weather is sunny.")),
            ..Default::default()
        };
        events.push(model_event);

        let generator = TestGenerator::new(Arc::new(MockLlm));
        let cases = generator.generate_from_events(&events).unwrap();

        assert_eq!(cases.len(), 1);
        let case = &cases[0];
        assert!(case.eval_id.starts_with("generated_from_events_"));
        assert_eq!(case.conversation.len(), 1);

        let turn = &case.conversation[0];
        assert_eq!(turn.invocation_id, "inv_1");
        assert_eq!(turn.user_content.get_text(), "What is the weather?");
        assert_eq!(turn.final_response.as_ref().unwrap().get_text(), "The weather is sunny.");
        assert!(case.tags.contains(&"generated".to_string()));
    }

    #[test]
    fn test_generate_from_events_with_tool_calls() {
        use adk_core::{Content, Llm, LlmResponse, Part};
        use async_trait::async_trait;

        struct MockLlm;

        #[async_trait]
        impl Llm for MockLlm {
            fn name(&self) -> &str {
                "mock"
            }
            async fn generate_content(
                &self,
                _req: LlmRequest,
                _stream: bool,
            ) -> adk_core::Result<adk_core::LlmResponseStream> {
                unimplemented!()
            }
        }

        let mut events = Vec::new();

        // User event
        let mut user_event = Event::new("inv_1");
        user_event.llm_response = LlmResponse {
            content: Some(Content::new("user").with_text("Get weather in NYC")),
            ..Default::default()
        };
        events.push(user_event);

        // Model event with tool call and text
        let mut model_event = Event::new("inv_1");
        model_event.llm_response = LlmResponse {
            content: Some(Content {
                role: "model".to_string(),
                parts: vec![
                    Part::FunctionCall {
                        name: "get_weather".to_string(),
                        args: serde_json::json!({"location": "NYC"}),
                        id: Some("call_1".to_string()),
                        thought_signature: None,
                    },
                    Part::Text { text: "It's 72°F in NYC.".to_string() },
                ],
            }),
            ..Default::default()
        };
        events.push(model_event);

        let generator = TestGenerator::new(Arc::new(MockLlm));
        let cases = generator.generate_from_events(&events).unwrap();

        assert_eq!(cases.len(), 1);
        let turn = &cases[0].conversation[0];
        let intermediate = turn.intermediate_data.as_ref().unwrap();
        assert_eq!(intermediate.tool_uses.len(), 1);
        assert_eq!(intermediate.tool_uses[0].name, "get_weather");
        assert_eq!(intermediate.tool_uses[0].args, serde_json::json!({"location": "NYC"}));
    }

    #[test]
    fn test_parse_generated_cases_valid() {
        use adk_core::Llm;
        use async_trait::async_trait;

        struct MockLlm;

        #[async_trait]
        impl Llm for MockLlm {
            fn name(&self) -> &str {
                "mock"
            }
            async fn generate_content(
                &self,
                _req: LlmRequest,
                _stream: bool,
            ) -> adk_core::Result<adk_core::LlmResponseStream> {
                unimplemented!()
            }
        }

        let generator = TestGenerator::new(Arc::new(MockLlm));
        let response = r#"[
            {
                "eval_id": "test_1",
                "description": "Greeting test",
                "conversation": [{
                    "invocation_id": "inv_1",
                    "user_content": {"parts": [{"text": "Hello"}], "role": "user"},
                    "final_response": {"parts": [{"text": "Hi!"}], "role": "model"}
                }]
            }
        ]"#;

        let cases = generator.parse_generated_cases(response, "test agent").unwrap();
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].eval_id, "test_1");
        assert!(cases[0].tags.contains(&"generated".to_string()));
        assert!(cases[0].tags.contains(&"description: test agent".to_string()));
    }

    #[test]
    fn test_parse_generated_cases_partial_failure() {
        use adk_core::Llm;
        use async_trait::async_trait;

        struct MockLlm;

        #[async_trait]
        impl Llm for MockLlm {
            fn name(&self) -> &str {
                "mock"
            }
            async fn generate_content(
                &self,
                _req: LlmRequest,
                _stream: bool,
            ) -> adk_core::Result<adk_core::LlmResponseStream> {
                unimplemented!()
            }
        }

        let generator = TestGenerator::new(Arc::new(MockLlm));
        // Mix of valid and invalid cases
        let response = r#"[
            {
                "eval_id": "test_1",
                "description": "Valid case",
                "conversation": [{
                    "invocation_id": "inv_1",
                    "user_content": {"parts": [{"text": "Hello"}], "role": "user"},
                    "final_response": {"parts": [{"text": "Hi!"}], "role": "model"}
                }]
            },
            {
                "invalid_field": "This is not a valid EvalCase"
            }
        ]"#;

        let cases = generator.parse_generated_cases(response, "test").unwrap();
        // Should parse the valid case and skip the invalid one
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].eval_id, "test_1");
    }

    #[test]
    fn test_parse_generated_cases_all_invalid() {
        use adk_core::Llm;
        use async_trait::async_trait;

        struct MockLlm;

        #[async_trait]
        impl Llm for MockLlm {
            fn name(&self) -> &str {
                "mock"
            }
            async fn generate_content(
                &self,
                _req: LlmRequest,
                _stream: bool,
            ) -> adk_core::Result<adk_core::LlmResponseStream> {
                unimplemented!()
            }
        }

        let generator = TestGenerator::new(Arc::new(MockLlm));
        let response = r#"[{"bad": true}, {"also_bad": "yes"}]"#;

        let result = generator.parse_generated_cases(response, "test");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("all 2 generated cases failed to parse"));
    }

    #[test]
    fn test_eval_case_metadata_serialization() {
        let meta = EvalCaseMetadata { generated: true, source: Some("events".to_string()) };

        let json = serde_json::to_string(&meta).unwrap();
        assert!(json.contains("\"generated\":true"));
        assert!(json.contains("\"source\":\"events\""));

        let deserialized: EvalCaseMetadata = serde_json::from_str(&json).unwrap();
        assert!(deserialized.generated);
        assert_eq!(deserialized.source.as_deref(), Some("events"));
    }

    #[test]
    fn test_eval_case_metadata_defaults() {
        let meta = EvalCaseMetadata::default();
        assert!(!meta.generated);
        assert!(meta.source.is_none());

        // source field should be skipped when None
        let json = serde_json::to_string(&meta).unwrap();
        assert!(!json.contains("source"));
    }

    #[test]
    fn test_generate_from_events_no_tool_expectations() {
        use adk_core::{Content, Llm, LlmResponse, Part};
        use async_trait::async_trait;

        struct MockLlm;

        #[async_trait]
        impl Llm for MockLlm {
            fn name(&self) -> &str {
                "mock"
            }
            async fn generate_content(
                &self,
                _req: LlmRequest,
                _stream: bool,
            ) -> adk_core::Result<adk_core::LlmResponseStream> {
                unimplemented!()
            }
        }

        let config = GeneratorConfig { cases_per_description: 5, include_tool_expectations: false };
        let generator = TestGenerator::with_config(Arc::new(MockLlm), config);

        let mut events = Vec::new();

        let mut user_event = Event::new("inv_1");
        user_event.llm_response = LlmResponse {
            content: Some(Content::new("user").with_text("Get weather")),
            ..Default::default()
        };
        events.push(user_event);

        let mut model_event = Event::new("inv_1");
        model_event.llm_response = LlmResponse {
            content: Some(Content {
                role: "model".to_string(),
                parts: vec![
                    Part::FunctionCall {
                        name: "get_weather".to_string(),
                        args: serde_json::json!({"location": "NYC"}),
                        id: None,
                        thought_signature: None,
                    },
                    Part::Text { text: "Sunny".to_string() },
                ],
            }),
            ..Default::default()
        };
        events.push(model_event);

        let cases = generator.generate_from_events(&events).unwrap();
        assert_eq!(cases.len(), 1);
        // Tool expectations should NOT be included
        assert!(cases[0].conversation[0].intermediate_data.is_none());
    }
}
