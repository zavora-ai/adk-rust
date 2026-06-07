//! Structured LLM judge producing typed verdicts.
//!
//! The [`StructuredJudge`] evaluates responses using an LLM and produces
//! machine-parseable [`StructuredVerdict`] results with scores, reasoning,
//! and categorical verdicts (pass/fail/partial).
//!
//! It attempts function-calling (via `response_schema`) first, then falls
//! back to prompting for JSON output with a lenient extractor.

use crate::error::{EvalError, Result};
use adk_core::{Content, GenerateContentConfig, Llm, LlmRequest};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Verdict from the structured judge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuredVerdict {
    /// Score from 0.0 to 1.0.
    pub score: f64,
    /// Human-readable reasoning for the verdict.
    pub reasoning: String,
    /// Categorical verdict.
    pub verdict: Verdict,
}

/// Categorical outcome of a structured judgment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The response fully satisfies the criterion.
    Pass,
    /// The response does not satisfy the criterion.
    Fail,
    /// The response partially satisfies the criterion.
    Partial,
}

/// Custom rubric for structured judging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeRubric {
    /// Name of the rubric.
    pub name: String,
    /// Description of what the rubric evaluates.
    pub description: String,
    /// Scoring scale with defined points.
    pub scale: Vec<ScalePoint>,
}

/// A single point on a rubric scoring scale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScalePoint {
    /// Numeric score for this level.
    pub score: f64,
    /// Short label (e.g., "Excellent", "Poor").
    pub label: String,
    /// Detailed description of what this level means.
    pub description: String,
}

/// Configuration for the structured judge.
#[derive(Debug, Clone)]
pub struct StructuredJudgeConfig {
    /// Whether to attempt function calling (response_schema) first.
    pub prefer_function_calling: bool,
    /// Temperature for the judge LLM.
    pub temperature: f64,
    /// Custom rubrics (optional).
    pub rubrics: Vec<JudgeRubric>,
}

impl Default for StructuredJudgeConfig {
    fn default() -> Self {
        Self { prefer_function_calling: true, temperature: 0.0, rubrics: Vec::new() }
    }
}

/// Structured LLM judge that produces typed verdicts.
///
/// Tries function calling first (via response schema), then falls back
/// to prompting for JSON output with a lenient parser.
pub struct StructuredJudge {
    model: Arc<dyn Llm>,
    config: StructuredJudgeConfig,
}

impl StructuredJudge {
    /// Create a new structured judge with default configuration.
    pub fn new(model: Arc<dyn Llm>) -> Self {
        Self { model, config: StructuredJudgeConfig::default() }
    }

    /// Create a structured judge with custom configuration.
    pub fn with_config(model: Arc<dyn Llm>, config: StructuredJudgeConfig) -> Self {
        Self { model, config }
    }

    /// Judge a response against expected output with a specific criterion.
    ///
    /// Tries function calling first, falls back to JSON extraction.
    /// On unparseable response, returns score 0.0 with parse error in reasoning.
    pub async fn judge(
        &self,
        expected: &str,
        actual: &str,
        criterion: &str,
    ) -> Result<StructuredVerdict> {
        let system_prompt = format!(
            r#"You are an evaluation judge. Evaluate the actual response against the expected response for the given criterion.

Criterion: {}

You MUST respond with a JSON object containing exactly these fields:
- "score": a number between 0.0 and 1.0
- "reasoning": a string explaining your evaluation
- "verdict": one of "pass", "fail", or "partial"

Example response:
{{"score": 0.85, "reasoning": "The response captures the key points but misses some details.", "verdict": "partial"}}"#,
            criterion
        );

        let user_prompt =
            format!("Expected response:\n\"{}\"\n\nActual response:\n\"{}\"", expected, actual);

        self.execute_judgment(&system_prompt, &user_prompt).await
    }

    /// Judge with a custom rubric.
    ///
    /// Evaluates the response against the rubric's scale points and produces
    /// a structured verdict.
    pub async fn judge_with_rubric(
        &self,
        response: &str,
        context: &str,
        rubric: &JudgeRubric,
    ) -> Result<StructuredVerdict> {
        let mut scale_description = String::new();
        for point in &rubric.scale {
            scale_description.push_str(&format!(
                "- {:.1} ({}): {}\n",
                point.score, point.label, point.description
            ));
        }

        let system_prompt = format!(
            r#"You are an evaluation judge. Evaluate the response using the following rubric.

Rubric: {}
Description: {}

Scoring Scale:
{}
You MUST respond with a JSON object containing exactly these fields:
- "score": a number between 0.0 and 1.0 matching one of the scale points
- "reasoning": a string explaining your evaluation
- "verdict": one of "pass", "fail", or "partial"

Example response:
{{"score": 0.75, "reasoning": "The response demonstrates good understanding but lacks depth.", "verdict": "partial"}}"#,
            rubric.name, rubric.description, scale_description
        );

        let user_prompt =
            format!("Context:\n\"{}\"\n\nResponse to evaluate:\n\"{}\"", context, response);

        self.execute_judgment(&system_prompt, &user_prompt).await
    }

    /// Execute a judgment using function-calling first, then JSON fallback.
    async fn execute_judgment(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<StructuredVerdict> {
        // Attempt 1: Try with response_schema (function-calling style)
        if self.config.prefer_function_calling {
            match self.call_with_schema(system_prompt, user_prompt).await {
                Ok(verdict) => return Ok(verdict),
                Err(_) => {
                    // Fall through to JSON fallback
                }
            }
        }

        // Attempt 2: JSON fallback — prompt for JSON and parse leniently
        self.call_with_json_fallback(system_prompt, user_prompt).await
    }

    /// Attempt judgment using response_schema for structured output.
    async fn call_with_schema(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<StructuredVerdict> {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "score": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
                "reasoning": { "type": "string" },
                "verdict": { "type": "string", "enum": ["pass", "fail", "partial"] }
            },
            "required": ["score", "reasoning", "verdict"]
        });

        let full_prompt = format!("{system_prompt}\n\n{user_prompt}");

        let config = GenerateContentConfig {
            temperature: Some(self.config.temperature as f32),
            response_schema: Some(schema),
            ..Default::default()
        };

        let request =
            LlmRequest::new(self.model.name(), vec![Content::new("user").with_text(&full_prompt)])
                .with_config(config);

        let response_text = self.collect_response(request).await?;
        self.parse_verdict_from_text(&response_text)
    }

    /// Attempt judgment by prompting for JSON and parsing leniently.
    async fn call_with_json_fallback(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<StructuredVerdict> {
        let full_prompt = format!("{system_prompt}\n\n{user_prompt}");

        let config = GenerateContentConfig {
            temperature: Some(self.config.temperature as f32),
            ..Default::default()
        };

        let request =
            LlmRequest::new(self.model.name(), vec![Content::new("user").with_text(&full_prompt)])
                .with_config(config);

        let response_text = self.collect_response(request).await?;
        self.parse_verdict_from_text(&response_text)
    }

    /// Collect all text from an LLM response stream.
    async fn collect_response(&self, request: LlmRequest) -> Result<String> {
        let mut stream = self
            .model
            .generate_content(request, false)
            .await
            .map_err(|e| EvalError::JudgeError(format!("LLM judge call failed: {e}")))?;

        let mut response_text = String::new();
        while let Some(result) = stream.next().await {
            let response =
                result.map_err(|e| EvalError::JudgeError(format!("LLM response error: {e}")))?;
            if let Some(content) = &response.content {
                for part in &content.parts {
                    if let Some(text) = part.text() {
                        response_text.push_str(text);
                    }
                }
            }
        }

        if response_text.is_empty() {
            return Err(EvalError::JudgeError("Empty response from judge".to_string()));
        }

        Ok(response_text)
    }

    /// Parse a StructuredVerdict from LLM response text.
    ///
    /// On failure, returns a fallback verdict with score 0.0 and the parse
    /// error in the reasoning field.
    fn parse_verdict_from_text(&self, text: &str) -> Result<StructuredVerdict> {
        match extract_json_from_text(text) {
            Some(json) => match serde_json::from_value::<StructuredVerdict>(json) {
                Ok(mut verdict) => {
                    // Clamp score to [0.0, 1.0]
                    verdict.score = verdict.score.clamp(0.0, 1.0);
                    Ok(verdict)
                }
                Err(e) => Ok(StructuredVerdict {
                    score: 0.0,
                    reasoning: format!("Parse error: failed to deserialize verdict: {e}"),
                    verdict: Verdict::Fail,
                }),
            },
            None => Ok(StructuredVerdict {
                score: 0.0,
                reasoning: format!(
                    "Parse error: could not extract JSON from response: {}",
                    truncate_for_error(text)
                ),
                verdict: Verdict::Fail,
            }),
        }
    }
}

/// Lenient JSON extractor that finds JSON objects in arbitrary text.
///
/// Handles common LLM output patterns:
/// - Raw JSON object
/// - JSON wrapped in markdown code fences (```json ... ```)
/// - JSON embedded in prose text
pub fn extract_json_from_text(text: &str) -> Option<serde_json::Value> {
    let trimmed = text.trim();

    // Pattern 1: Raw JSON object — starts with `{`
    if trimmed.starts_with('{')
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed)
        && value.is_object()
    {
        return Some(value);
    }

    // Pattern 2: Markdown code fences (```json ... ``` or ``` ... ```)
    if let Some(json_str) = extract_from_code_fence(trimmed)
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(json_str)
        && value.is_object()
    {
        return Some(value);
    }

    // Pattern 3: Embedded JSON in prose — find the first `{` and try to parse
    if let Some(start) = trimmed.find('{') {
        // Try progressively from the outermost `{` to find a valid JSON object
        let substring = &trimmed[start..];
        if let Some(value) = try_parse_json_object(substring) {
            return Some(value);
        }
    }

    None
}

/// Extract content from markdown code fences.
fn extract_from_code_fence(text: &str) -> Option<&str> {
    // Look for ```json\n...\n``` or ```\n...\n```
    let fence_start = text.find("```")?;
    let after_fence = &text[fence_start + 3..];

    // Skip optional language tag (e.g., "json")
    let content_start = after_fence.find('\n')? + 1;
    let content = &after_fence[content_start..];

    // Find closing fence
    let fence_end = content.find("```")?;
    let inner = content[..fence_end].trim();

    if inner.is_empty() { None } else { Some(inner) }
}

/// Try to parse a valid JSON object starting from the beginning of the string.
///
/// Uses brace counting to find the matching closing brace.
fn try_parse_json_object(text: &str) -> Option<serde_json::Value> {
    if !text.starts_with('{') {
        return None;
    }

    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape_next = false;

    for (i, ch) in text.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }

        if ch == '\\' && in_string {
            escape_next = true;
            continue;
        }

        if ch == '"' {
            in_string = !in_string;
            continue;
        }

        if in_string {
            continue;
        }

        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let candidate = &text[..=i];
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(candidate)
                        && value.is_object()
                    {
                        return Some(value);
                    }
                    // If parse failed at this brace, keep going — there might
                    // be a deeper valid match, but that's unlikely. Return None.
                    return None;
                }
            }
            _ => {}
        }
    }

    None
}

/// Truncate text for inclusion in error messages.
fn truncate_for_error(text: &str) -> String {
    if text.len() <= 200 { text.to_string() } else { format!("{}...", &text[..200]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_raw_json() {
        let input = r#"{"score": 0.8, "reasoning": "Good answer", "verdict": "pass"}"#;
        let result = extract_json_from_text(input).unwrap();
        assert_eq!(result["score"], 0.8);
        assert_eq!(result["reasoning"], "Good answer");
        assert_eq!(result["verdict"], "pass");
    }

    #[test]
    fn test_extract_json_with_whitespace() {
        let input = r#"
        {"score": 0.5, "reasoning": "Average", "verdict": "partial"}
        "#;
        let result = extract_json_from_text(input).unwrap();
        assert_eq!(result["score"], 0.5);
        assert_eq!(result["verdict"], "partial");
    }

    #[test]
    fn test_extract_json_from_markdown_fence() {
        let input = r#"Here is my evaluation:

```json
{"score": 0.9, "reasoning": "Excellent match", "verdict": "pass"}
```

That's my assessment."#;
        let result = extract_json_from_text(input).unwrap();
        assert_eq!(result["score"], 0.9);
        assert_eq!(result["verdict"], "pass");
    }

    #[test]
    fn test_extract_json_from_fence_without_language() {
        let input = r#"```
{"score": 0.3, "reasoning": "Poor", "verdict": "fail"}
```"#;
        let result = extract_json_from_text(input).unwrap();
        assert_eq!(result["score"], 0.3);
        assert_eq!(result["verdict"], "fail");
    }

    #[test]
    fn test_extract_json_embedded_in_prose() {
        let input = r#"After careful consideration, I believe the score should be:
{"score": 0.7, "reasoning": "Mostly correct but missing key detail", "verdict": "partial"}
That is my final answer."#;
        let result = extract_json_from_text(input).unwrap();
        assert_eq!(result["score"], 0.7);
        assert_eq!(result["verdict"], "partial");
    }

    #[test]
    fn test_extract_json_returns_none_for_garbage() {
        let input = "This is just a bunch of random text with no JSON at all.";
        assert!(extract_json_from_text(input).is_none());
    }

    #[test]
    fn test_extract_json_returns_none_for_invalid_json() {
        let input = r#"{"score": bad_value, "reasoning": "test"}"#;
        assert!(extract_json_from_text(input).is_none());
    }

    #[test]
    fn test_extract_json_handles_nested_braces() {
        let input =
            r#"{"score": 0.6, "reasoning": "The {nested} braces are fine", "verdict": "partial"}"#;
        let result = extract_json_from_text(input).unwrap();
        assert_eq!(result["score"], 0.6);
        assert!(result["reasoning"].as_str().unwrap().contains("{nested}"));
    }

    #[test]
    fn test_extract_json_handles_escaped_quotes() {
        let input =
            r#"{"score": 0.5, "reasoning": "He said \"hello\" to me", "verdict": "partial"}"#;
        let result = extract_json_from_text(input).unwrap();
        assert_eq!(result["score"], 0.5);
    }

    #[test]
    fn test_parse_verdict_fallback_on_missing_fields() {
        let judge = StructuredJudge::new(Arc::new(adk_model::MockLlm::new("test")));
        // JSON missing "verdict" field
        let result = judge.parse_verdict_from_text(r#"{"score": 0.5, "reasoning": "ok"}"#);
        let verdict = result.unwrap();
        assert_eq!(verdict.score, 0.0);
        assert!(verdict.reasoning.contains("Parse error"));
    }

    #[test]
    fn test_parse_verdict_fallback_on_no_json() {
        let judge = StructuredJudge::new(Arc::new(adk_model::MockLlm::new("test")));
        let result = judge.parse_verdict_from_text("I think the answer is good.");
        let verdict = result.unwrap();
        assert_eq!(verdict.score, 0.0);
        assert!(verdict.reasoning.contains("Parse error"));
    }

    #[test]
    fn test_parse_verdict_clamps_score() {
        let judge = StructuredJudge::new(Arc::new(adk_model::MockLlm::new("test")));
        let text = r#"{"score": 1.5, "reasoning": "Great", "verdict": "pass"}"#;
        let verdict = judge.parse_verdict_from_text(text).unwrap();
        assert_eq!(verdict.score, 1.0);

        let text = r#"{"score": -0.3, "reasoning": "Bad", "verdict": "fail"}"#;
        let verdict = judge.parse_verdict_from_text(text).unwrap();
        assert_eq!(verdict.score, 0.0);
    }

    #[test]
    fn test_structured_judge_config_defaults() {
        let config = StructuredJudgeConfig::default();
        assert!(config.prefer_function_calling);
        assert_eq!(config.temperature, 0.0);
        assert!(config.rubrics.is_empty());
    }
}
