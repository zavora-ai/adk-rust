//! Test file schema definitions
//!
//! Defines the structure for test files (`.test.json`) and eval sets (`.evalset.json`).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

use crate::error::{EvalError, Result};

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
                Part::FunctionCall { .. }
                | Part::FunctionResponse { .. }
                | Part::CodeExecutionResult { .. } => {
                    // Function calls/responses and code execution are handled separately in the evaluation
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
    /// Code execution result
    CodeExecutionResult { outcome: String, output: String },
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
    /// Arguments passed to the tool
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

    /// Check if this tool use matches another (name and args)
    pub fn matches(&self, other: &ToolUse, strict_args: bool) -> bool {
        if self.name != other.name {
            return false;
        }

        if strict_args {
            self.args == other.args
        } else {
            // Partial match: check that expected args are present in actual
            match (&self.args, &other.args) {
                (Value::Object(expected), Value::Object(actual)) => {
                    expected.iter().all(|(k, v)| actual.get(k) == Some(v))
                }
                _ => self.args == other.args,
            }
        }
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
    fn test_content_data() {
        let content = ContentData::text("Hello world");
        assert_eq!(content.get_text(), "Hello world");
        assert_eq!(content.role, "user");

        let model = ContentData::model_response("Hi there!");
        assert_eq!(model.role, "model");
    }
}
