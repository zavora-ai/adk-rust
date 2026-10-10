//! Test file schema definitions
//!
//! Defines the structure for test files (`.test.json`) and eval sets (`.evalset.json`).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

use crate::error::{EvalError, Result};
use crate::test_generator::EvalCaseMetadata;

/// A complete test file containing multiple evaluation cases
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestFile {
    /// Unique identifier for this eval set
    pub eval_set_id: String,
    /// Human-readable name
    pub name: String,
    /// Description of what these tests cover
    #[serde(default)]
    pub description: String,
    /// List of evaluation cases
    pub eval_cases: Vec<EvalCase>,
}

impl TestFile {
    /// Load a test file from disk
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let content = std::fs::read_to_string(path.as_ref())?;
        let test_file: TestFile = serde_json::from_str(&content)?;
        Ok(test_file)
    }

    /// Save test file to disk
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }
}

/// An eval set references multiple test files
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalSet {
    /// Unique identifier
    pub eval_set_id: String,
    /// Human-readable name
    pub name: String,
    /// Description
    #[serde(default)]
    pub description: String,
    /// List of test file paths or inline eval cases
    #[serde(default)]
    pub test_files: Vec<String>,
    /// Inline eval cases (alternative to test_files)
    #[serde(default)]
    pub eval_cases: Vec<EvalCase>,
}

impl EvalSet {
    /// Load an eval set from disk
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let content = std::fs::read_to_string(path.as_ref())?;
        let eval_set: EvalSet = serde_json::from_str(&content)?;
        Ok(eval_set)
    }

    /// Get all eval cases, loading from test files if needed
    pub fn get_all_cases(&self, base_path: impl AsRef<Path>) -> Result<Vec<EvalCase>> {
        let mut all_cases = self.eval_cases.clone();

        for test_file_path in &self.test_files {
            let full_path = base_path.as_ref().join(test_file_path);
            let test_file = TestFile::load(&full_path).map_err(|e| {
                EvalError::LoadError(format!("Failed to load {}: {}", test_file_path, e))
            })?;
            all_cases.extend(test_file.eval_cases);
        }

        Ok(all_cases)
    }
}

/// A single evaluation case (test case)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalCase {
    /// Unique identifier for this test case
    pub eval_id: String,
    /// Optional description
    #[serde(default)]
    pub description: String,
    /// The conversation turns to evaluate
    pub conversation: Vec<Turn>,
    /// Session configuration
    #[serde(default)]
    pub session_input: SessionInput,
    /// Optional tags for filtering
    #[serde(default)]
    pub tags: Vec<String>,
    /// Optional metadata (generation info, etc.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<EvalCaseMetadata>,
}

/// A single turn in a conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    /// Unique identifier for this turn
    pub invocation_id: String,
    /// User input content
    pub user_content: ContentData,
    /// Expected final response from the agent
    #[serde(default)]
    pub final_response: Option<ContentData>,
    /// Expected intermediate data (tool calls, etc.)
    #[serde(default)]
    pub intermediate_data: Option<IntermediateData>,
}

/// Content data structure (matches ADK Content)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentData {
    /// Content parts
    pub parts: Vec<Part>,
    /// Role (user, model, tool)
    #[serde(default = "default_role")]
    pub role: String,
}

fn default_role() -> String {
    "user".to_string()
}

impl ContentData {
    /// Create content from text
    pub fn text(text: &str) -> Self {
        Self { parts: vec![Part::Text { text: text.to_string() }], role: "user".to_string() }
    }

    /// Create model response content
    pub fn model_response(text: &str) -> Self {
        Self { parts: vec![Part::Text { text: text.to_string() }], role: "model".to_string() }
    }

    /// Get all text parts concatenated
    pub fn get_text(&self) -> String {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    /// Convert to ADK Content
    pub fn to_adk_content(&self) -> adk_core::Content {
        let mut content = adk_core::Content::new(&self.role);
        for part in &self.parts {
            match part {
                Part::Text { text } => {
                    content = content.with_text(text);
                }
                Part::FunctionCall { .. } | Part::FunctionResponse { .. } => {
                    // Function calls/responses are handled separately in the evaluation
                    // The Content type doesn't have direct methods for these
                }
            }
        }
        content
    }
}

/// Content part variants
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Part {
    /// Text content
    Text { text: String },
    /// Function/tool call
    FunctionCall { name: String, args: Value },
    /// Function/tool response
    FunctionResponse { name: String, response: Value },
}

/// Intermediate data during a turn (tool calls, etc.)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IntermediateData {
    /// Expected tool calls in order
    #[serde(default)]
    pub tool_uses: Vec<ToolUse>,
    /// Intermediate responses before final
    #[serde(default)]
    pub intermediate_responses: Vec<ContentData>,
}

/// A tool use (function call)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUse {
    /// Tool/function name
    pub name: String,
    /// Arguments passed to the tool.
    ///
    /// In an expected tool use, `Value::Null` (the value when `args` is omitted from a
    /// test file) places no constraint on the actual arguments.
    #[serde(default)]
    pub args: Value,
    /// Expected response (optional, for mocking)
    #[serde(default)]
    pub expected_response: Option<Value>,
}

impl ToolUse {
    /// Create a new tool use
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            args: Value::Object(Default::default()),
            expected_response: None,
        }
    }

    /// Add arguments
    pub fn with_args(mut self, args: Value) -> Self {
        self.args = args;
        self
    }

    /// Checks whether `other`, an actual tool call, satisfies this expected tool use.
    ///
    /// Names must be equal. Expected arguments of `Value::Null` match any arguments.
    /// Otherwise arguments are compared recursively, and numbers compare by value, so
    /// `1` matches `1.0`.
    ///
    /// | `strict_args` | Objects | Arrays |
    /// |---------------|---------|--------|
    /// | `false` | every expected key is present with a matching value; extra keys are allowed at any depth | same length, matched element by element |
    /// | `true` | same key set, values matched recursively | same length, matched element by element |
    ///
    /// # Example
    ///
    /// ```
    /// use adk_eval::ToolUse;
    /// use serde_json::json;
    ///
    /// let expected = ToolUse::new("search").with_args(json!({"filter": {"limit": 10}}));
    /// let actual = ToolUse::new("search")
    ///     .with_args(json!({"query": "rust", "filter": {"limit": 10.0, "page": 2}}));
    ///
    /// assert!(expected.matches(&actual, false));
    /// assert!(!expected.matches(&actual, true));
    /// ```
    pub fn matches(&self, other: &ToolUse, strict_args: bool) -> bool {
        self.name == other.name
            && (self.args.is_null() || json_matches(&self.args, &other.args, strict_args))
    }
}

/// Compares an expected JSON value against an actual one; see [`ToolUse::matches`].
fn json_matches(expected: &Value, actual: &Value, strict: bool) -> bool {
    match (expected, actual) {
        (Value::Number(expected), Value::Number(actual)) => {
            // Integers compare exactly so values beyond 2^53 do not collapse in f64.
            if let (Some(expected), Some(actual)) = (expected.as_i64(), actual.as_i64()) {
                expected == actual
            } else if let (Some(expected), Some(actual)) = (expected.as_u64(), actual.as_u64()) {
                expected == actual
            } else {
                expected.as_f64() == actual.as_f64()
            }
        }
        (Value::Object(expected), Value::Object(actual)) => {
            (!strict || expected.len() == actual.len())
                && expected.iter().all(|(key, value)| {
                    actual.get(key).is_some_and(|actual| json_matches(value, actual, strict))
                })
        }
        (Value::Array(expected), Value::Array(actual)) => {
            expected.len() == actual.len()
                && expected.iter().zip(actual).all(|(e, a)| json_matches(e, a, strict))
        }
        _ => expected == actual,
    }
}

/// Session input configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionInput {
    /// Application name
    #[serde(default)]
    pub app_name: String,
    /// User identifier
    #[serde(default)]
    pub user_id: String,
    /// Initial state
    #[serde(default)]
    pub state: HashMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_test_file() {
        let json = r#"{
            "eval_set_id": "test_set",
            "name": "Test Set",
            "description": "A test set",
            "eval_cases": [
                {
                    "eval_id": "test_1",
                    "conversation": [
                        {
                            "invocation_id": "inv_1",
                            "user_content": {
                                "parts": [{"text": "Hello"}],
                                "role": "user"
                            },
                            "final_response": {
                                "parts": [{"text": "Hi there!"}],
                                "role": "model"
                            }
                        }
                    ]
                }
            ]
        }"#;

        let test_file: TestFile = serde_json::from_str(json).unwrap();
        assert_eq!(test_file.eval_set_id, "test_set");
        assert_eq!(test_file.eval_cases.len(), 1);
        assert_eq!(test_file.eval_cases[0].eval_id, "test_1");
    }

    #[test]
    fn test_tool_use_matching() {
        let expected = ToolUse::new("get_weather").with_args(json!({"location": "NYC"}));

        let actual_exact = ToolUse::new("get_weather").with_args(json!({"location": "NYC"}));
        assert!(expected.matches(&actual_exact, true));

        let actual_extra =
            ToolUse::new("get_weather").with_args(json!({"location": "NYC", "unit": "celsius"}));
        assert!(!expected.matches(&actual_extra, true)); // Strict fails
        assert!(expected.matches(&actual_extra, false)); // Partial passes

        let actual_wrong = ToolUse::new("get_weather").with_args(json!({"location": "LA"}));
        assert!(!expected.matches(&actual_wrong, true));
        assert!(!expected.matches(&actual_wrong, false));
    }

    #[test]
    fn partial_args_match_nested_objects_as_subsets() {
        let expected = ToolUse::new("book")
            .with_args(json!({"trip": {"from": "NBO", "legs": [{"seat": "12A"}]}}));
        let actual = ToolUse::new("book").with_args(json!({
            "trip": {"from": "NBO", "to": "LHR", "legs": [{"seat": "12A", "meal": "veg"}]},
            "currency": "KES"
        }));
        assert!(expected.matches(&actual, false));
        assert!(!expected.matches(&actual, true));

        let wrong_nested = ToolUse::new("book")
            .with_args(json!({"trip": {"from": "MBA", "legs": [{"seat": "12A"}]}}));
        assert!(!expected.matches(&wrong_nested, false));

        let missing_element =
            ToolUse::new("book").with_args(json!({"trip": {"from": "NBO", "legs": []}}));
        assert!(!expected.matches(&missing_element, false));
    }

    #[test]
    fn numbers_match_by_value() {
        let expected = ToolUse::new("set_volume").with_args(json!({"level": 1, "gain": 0.5}));
        let actual = ToolUse::new("set_volume").with_args(json!({"level": 1.0, "gain": 0.5}));
        assert!(expected.matches(&actual, false));
        assert!(expected.matches(&actual, true));

        let different = ToolUse::new("set_volume").with_args(json!({"level": 2, "gain": 0.5}));
        assert!(!expected.matches(&different, true));

        let large = ToolUse::new("seek").with_args(json!({"offset": 9_007_199_254_740_993_u64}));
        let off_by_one =
            ToolUse::new("seek").with_args(json!({"offset": 9_007_199_254_740_992_u64}));
        assert!(!large.matches(&off_by_one, true));
    }

    #[test]
    fn omitted_expected_args_match_any_args() {
        let expected: ToolUse = serde_json::from_value(json!({"name": "get_time"})).unwrap();
        let actual = ToolUse::new("get_time").with_args(json!({"timezone": "Africa/Nairobi"}));
        assert!(expected.matches(&actual, false));
        assert!(expected.matches(&actual, true));
        assert!(!expected.matches(&ToolUse::new("get_date"), false));
    }

    #[test]
    fn test_content_data() {
        let content = ContentData::text("Hello world");
        assert_eq!(content.get_text(), "Hello world");
        assert_eq!(content.role, "user");

        let model = ContentData::model_response("Hi there!");
        assert_eq!(model.role, "model");
    }
}
