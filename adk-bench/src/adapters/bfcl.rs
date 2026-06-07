//! BFCL (Berkeley Function Calling Leaderboard) adapter.
//!
//! Loads BFCL dataset entries and translates function definitions into
//! ADK-Rust Tool trait implementations for accuracy scoring.
//!
//! The BFCL adapter implements the scoring methodology used by the Berkeley
//! Function Calling Leaderboard: exact match on function name and argument
//! values, with accuracy reported as the fraction of correctly predicted
//! tool calls over total test cases.
//!
//! # Dataset Format
//!
//! BFCL entries are loaded from JSONL (JSON Lines) files where each line
//! is a complete JSON object representing one test case:
//!
//! ```json
//! {"id": "simple_1", "category": "simple", "question": "...", "function": [...], "expected_output": [...]}
//! ```
//!
//! # Scoring Methodology
//!
//! The adapter uses exact match scoring:
//! - Function name must match exactly
//! - All required arguments must be present with correct values
//! - No extra arguments are allowed (strict matching)
//! - A case scores 1.0 if all expected tool calls match, 0.0 otherwise
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_bench::adapters::bfcl::{BfclAdapter, BfclConfig};
//!
//! let config = BfclConfig {
//!     dataset_path: "/path/to/bfcl_dataset.jsonl".into(),
//!     categories: vec!["simple".to_string(), "multiple".to_string()],
//!     max_cases: Some(100),
//! };
//! let adapter = BfclAdapter::new(config);
//! let result = adapter.run("gemini-2.5-flash").await?;
//! println!("Accuracy: {:.1}%", result.accuracy * 100.0);
//! ```

use std::collections::HashMap;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{CaseResult, TaskQualityAdapter, TaskQualityResult};

// ─── Configuration ───────────────────────────────────────────────────────────

/// Configuration for the BFCL adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BfclConfig {
    /// Path to the BFCL dataset file (JSONL format).
    pub dataset_path: PathBuf,

    /// Categories to include (e.g., "simple", "multiple", "parallel",
    /// "parallel_multiple", "java", "javascript", "rest").
    /// If empty, all categories are included.
    pub categories: Vec<String>,

    /// Maximum number of test cases to execute (for cost control).
    /// If `None`, all matching cases are executed.
    pub max_cases: Option<usize>,
}

impl Default for BfclConfig {
    fn default() -> Self {
        Self {
            dataset_path: PathBuf::from("bfcl_dataset.jsonl"),
            categories: Vec::new(),
            max_cases: None,
        }
    }
}

// ─── BFCL Protocol Types ─────────────────────────────────────────────────────

/// A single entry from the BFCL dataset (one test case).
///
/// Each entry contains a question (user prompt), available function
/// definitions, and the expected tool call output.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BfclEntry {
    /// Unique identifier for this test case.
    pub id: String,

    /// Category of the test case (e.g., "simple", "multiple", "parallel").
    #[serde(default)]
    pub category: String,

    /// The user question/prompt that should trigger function calling.
    pub question: String,

    /// Available function definitions that the model can call.
    #[serde(rename = "function")]
    pub functions: Vec<BfclFunction>,

    /// The expected tool call output(s) for scoring.
    #[serde(rename = "expected_output")]
    pub expected_output: Vec<BfclExpectedOutput>,
}

/// A function definition in the BFCL dataset.
///
/// This is translated into an ADK-Rust `Tool` trait implementation
/// for the benchmark run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BfclFunction {
    /// The function name (used for matching against tool calls).
    pub name: String,

    /// Human-readable description of what the function does.
    pub description: String,

    /// JSON Schema defining the function's parameters.
    pub parameters: serde_json::Value,
}

/// Expected output for a BFCL test case.
///
/// Represents the expected tool call that the model should produce.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BfclExpectedOutput {
    /// The expected function name to be called.
    pub name: String,

    /// The expected arguments as key-value pairs.
    pub arguments: HashMap<String, serde_json::Value>,
}

/// Represents an actual tool call produced by the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BfclToolCallOutput {
    /// The function name that was called.
    pub name: String,

    /// The arguments passed to the function.
    pub arguments: HashMap<String, serde_json::Value>,
}

// ─── Leaderboard Result Format ───────────────────────────────────────────────

/// BFCL leaderboard-compatible result format.
///
/// This structure matches the output format expected by the BFCL
/// leaderboard submission system.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BfclLeaderboardResult {
    /// Model identifier used for the run.
    pub model: String,

    /// Overall accuracy across all categories.
    pub overall_accuracy: f64,

    /// Per-category accuracy breakdown.
    pub category_accuracy: HashMap<String, CategoryAccuracy>,

    /// Total number of test cases evaluated.
    pub total_cases: usize,

    /// Number of test cases that passed (exact match).
    pub passed_cases: usize,
}

/// Accuracy metrics for a single BFCL category.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryAccuracy {
    /// Accuracy score for this category (0.0 to 1.0).
    pub accuracy: f64,

    /// Number of cases in this category.
    pub total: usize,

    /// Number of cases that passed in this category.
    pub passed: usize,
}

// ─── BFCL Adapter ────────────────────────────────────────────────────────────

/// BFCL adapter that scores function calling accuracy through ADK-Rust.
///
/// Loads BFCL dataset entries, translates function definitions into
/// ADK-Rust Tool trait implementations, and compares agent tool call
/// output against expected output using the BFCL scoring methodology.
///
/// The scoring methodology is:
/// - **Exact match on function name**: The called function must match exactly.
/// - **Exact match on arguments**: All required arguments must be present with
///   correct values. Extra arguments cause a failure.
/// - **Per-case scoring**: 1.0 if all expected calls match, 0.0 otherwise.
/// - **Overall accuracy**: Sum of per-case scores divided by total cases.
pub struct BfclAdapter {
    /// Adapter configuration.
    config: BfclConfig,
}

impl BfclAdapter {
    /// Creates a new BFCL adapter with the given configuration.
    ///
    /// # Arguments
    ///
    /// * `config` - Configuration specifying dataset path, categories, and limits.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_bench::adapters::bfcl::{BfclAdapter, BfclConfig};
    ///
    /// let config = BfclConfig::default();
    /// let adapter = BfclAdapter::new(config);
    /// ```
    pub fn new(config: BfclConfig) -> Self {
        Self { config }
    }

    /// Returns the adapter configuration.
    pub fn config(&self) -> &BfclConfig {
        &self.config
    }

    /// Loads BFCL entries from the configured dataset path.
    ///
    /// Reads the JSONL file line by line, parsing each line as a `BfclEntry`.
    /// Filters by configured categories and respects the `max_cases` limit.
    fn load_entries(&self) -> crate::Result<Vec<BfclEntry>> {
        let path = &self.config.dataset_path;

        if !path.exists() {
            return Err(crate::BenchError::WorkloadNotFound { path: path.display().to_string() });
        }

        let content = std::fs::read_to_string(path).map_err(crate::BenchError::Io)?;

        let mut entries: Vec<BfclEntry> = Vec::new();

        for (line_num, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let entry: BfclEntry =
                serde_json::from_str(line).map_err(|e| crate::BenchError::WorkloadValidation {
                    field: format!("line {}", line_num + 1),
                    reason: format!("failed to parse BFCL entry: {e}"),
                })?;

            // Filter by category if categories are specified
            if !self.config.categories.is_empty()
                && !self.config.categories.contains(&entry.category)
            {
                continue;
            }

            entries.push(entry);

            // Respect max_cases limit
            if let Some(max) = self.config.max_cases
                && entries.len() >= max
            {
                break;
            }
        }

        Ok(entries)
    }

    /// Translates BFCL function definitions into ADK-Rust tool schemas.
    ///
    /// Each `BfclFunction` is converted into a JSON representation
    /// compatible with the ADK-Rust tool system for inclusion in LLM
    /// requests.
    fn translate_functions_to_tool_schemas(functions: &[BfclFunction]) -> Vec<serde_json::Value> {
        functions
            .iter()
            .map(|f| {
                serde_json::json!({
                    "name": f.name,
                    "description": f.description,
                    "parameters": f.parameters,
                })
            })
            .collect()
    }

    /// Scores a single test case by comparing actual tool calls against expected output.
    ///
    /// Uses the BFCL exact-match scoring methodology:
    /// - Function name must match exactly
    /// - All arguments must be present with matching values
    /// - No extra arguments allowed
    ///
    /// Returns a score of 1.0 for a perfect match, 0.0 otherwise.
    fn score_case(
        expected: &[BfclExpectedOutput],
        actual: &[BfclToolCallOutput],
    ) -> (f64, Option<String>) {
        if expected.len() != actual.len() {
            return (
                0.0,
                Some(format!("expected {} tool call(s), got {}", expected.len(), actual.len())),
            );
        }

        for (i, (exp, act)) in expected.iter().zip(actual.iter()).enumerate() {
            // Check function name exact match
            if exp.name != act.name {
                return (
                    0.0,
                    Some(format!("call {i}: expected function '{}', got '{}'", exp.name, act.name)),
                );
            }

            // Check arguments exact match
            match Self::arguments_match(&exp.arguments, &act.arguments) {
                Ok(()) => {}
                Err(reason) => {
                    return (0.0, Some(format!("call {i}, function '{}': {reason}", exp.name)));
                }
            }
        }

        (1.0, None)
    }

    /// Checks if actual arguments match expected arguments exactly.
    ///
    /// Returns `Ok(())` if all arguments match, or an `Err` with a
    /// description of the mismatch.
    fn arguments_match(
        expected: &HashMap<String, serde_json::Value>,
        actual: &HashMap<String, serde_json::Value>,
    ) -> Result<(), String> {
        // Check for missing arguments
        for key in expected.keys() {
            if !actual.contains_key(key) {
                return Err(format!("missing argument '{key}'"));
            }
        }

        // Check for extra arguments (strict matching)
        for key in actual.keys() {
            if !expected.contains_key(key) {
                return Err(format!("unexpected extra argument '{key}'"));
            }
        }

        // Check value equality
        for (key, expected_val) in expected {
            let actual_val = &actual[key];
            if !json_values_equal(expected_val, actual_val) {
                return Err(format!("argument '{key}': expected {expected_val}, got {actual_val}"));
            }
        }

        Ok(())
    }

    /// Generates a leaderboard-compatible result from case results.
    fn generate_leaderboard_result(
        &self,
        model: &str,
        entries: &[BfclEntry],
        cases: &[CaseResult],
    ) -> BfclLeaderboardResult {
        let total_cases = cases.len();
        let passed_cases = cases.iter().filter(|c| c.passed).count();
        let overall_accuracy =
            if total_cases > 0 { passed_cases as f64 / total_cases as f64 } else { 0.0 };

        // Compute per-category accuracy
        let mut category_accuracy: HashMap<String, CategoryAccuracy> = HashMap::new();

        for (entry, case) in entries.iter().zip(cases.iter()) {
            let cat = category_accuracy.entry(entry.category.clone()).or_insert(CategoryAccuracy {
                accuracy: 0.0,
                total: 0,
                passed: 0,
            });
            cat.total += 1;
            if case.passed {
                cat.passed += 1;
            }
        }

        // Compute accuracy for each category
        for cat in category_accuracy.values_mut() {
            cat.accuracy = if cat.total > 0 { cat.passed as f64 / cat.total as f64 } else { 0.0 };
        }

        BfclLeaderboardResult {
            model: model.to_string(),
            overall_accuracy,
            category_accuracy,
            total_cases,
            passed_cases,
        }
    }
}

impl Default for BfclAdapter {
    fn default() -> Self {
        Self::new(BfclConfig::default())
    }
}

#[async_trait]
impl TaskQualityAdapter for BfclAdapter {
    fn name(&self) -> &str {
        "bfcl"
    }

    async fn run(&self, model: &str) -> crate::Result<TaskQualityResult> {
        // Load BFCL dataset entries
        let entries = self.load_entries()?;

        if entries.is_empty() {
            return Ok(TaskQualityResult {
                adapter_name: self.name().to_string(),
                model: model.to_string(),
                total_cases: 0,
                passed_cases: 0,
                accuracy: 0.0,
                cases: Vec::new(),
            });
        }

        let mut cases: Vec<CaseResult> = Vec::with_capacity(entries.len());

        for entry in &entries {
            // Translate function definitions to ADK-Rust tool schemas
            let _tool_schemas = Self::translate_functions_to_tool_schemas(&entry.functions);

            // TODO: Route function calling through adk-runner with real LLM calls.
            //
            // The execution flow would be:
            // 1. Create an LlmAgent with the tool schemas and entry.question as input
            // 2. Execute through adk-runner with the specified model
            // 3. Collect tool calls from the agent's response events
            // 4. Convert tool calls to BfclToolCallOutput format
            //
            // For now, we produce empty actual output (all cases will fail)
            // until the LLM execution path is wired up.
            let actual_output: Vec<BfclToolCallOutput> = Vec::new();

            // Score using BFCL exact-match methodology
            let (score, details) = Self::score_case(&entry.expected_output, &actual_output);

            cases.push(CaseResult {
                case_id: entry.id.clone(),
                passed: score >= 1.0,
                score,
                details,
            });
        }

        let total_cases = cases.len();
        let passed_cases = cases.iter().filter(|c| c.passed).count();
        let accuracy = if total_cases > 0 { passed_cases as f64 / total_cases as f64 } else { 0.0 };

        // Generate leaderboard result for logging/debugging
        let _leaderboard = self.generate_leaderboard_result(model, &entries, &cases);

        Ok(TaskQualityResult {
            adapter_name: self.name().to_string(),
            model: model.to_string(),
            total_cases,
            passed_cases,
            accuracy,
            cases,
        })
    }
}

// ─── Utility Functions ───────────────────────────────────────────────────────

/// Compares two JSON values for equality using BFCL semantics.
///
/// BFCL uses exact value matching with the following rules:
/// - Numbers are compared by value (integer/float normalization)
/// - Strings are compared exactly (case-sensitive)
/// - Arrays are compared element-by-element in order
/// - Objects are compared key-by-key (order independent)
/// - Null matches null
fn json_values_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    use serde_json::Value;

    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Number(a), Value::Number(b)) => {
            // Compare as f64 for numeric equivalence
            match (a.as_f64(), b.as_f64()) {
                (Some(fa), Some(fb)) => (fa - fb).abs() < f64::EPSILON,
                _ => false,
            }
        }
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| json_values_equal(x, y))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, val)| b.get(key).is_some_and(|bval| json_values_equal(val, bval)))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_values_equal_numbers() {
        let a = serde_json::json!(42);
        let b = serde_json::json!(42.0);
        assert!(json_values_equal(&a, &b));
    }

    #[test]
    fn test_json_values_equal_strings() {
        let a = serde_json::json!("hello");
        let b = serde_json::json!("hello");
        assert!(json_values_equal(&a, &b));

        let c = serde_json::json!("Hello");
        assert!(!json_values_equal(&a, &c));
    }

    #[test]
    fn test_json_values_equal_objects() {
        let a = serde_json::json!({"x": 1, "y": 2});
        let b = serde_json::json!({"y": 2, "x": 1});
        assert!(json_values_equal(&a, &b));
    }

    #[test]
    fn test_json_values_equal_different_types() {
        let a = serde_json::json!(42);
        let b = serde_json::json!("42");
        assert!(!json_values_equal(&a, &b));
    }

    #[test]
    fn test_score_case_exact_match() {
        let expected = vec![BfclExpectedOutput {
            name: "get_weather".to_string(),
            arguments: HashMap::from([
                ("city".to_string(), serde_json::json!("Seattle")),
                ("unit".to_string(), serde_json::json!("fahrenheit")),
            ]),
        }];

        let actual = vec![BfclToolCallOutput {
            name: "get_weather".to_string(),
            arguments: HashMap::from([
                ("city".to_string(), serde_json::json!("Seattle")),
                ("unit".to_string(), serde_json::json!("fahrenheit")),
            ]),
        }];

        let (score, details) = BfclAdapter::score_case(&expected, &actual);
        assert_eq!(score, 1.0);
        assert!(details.is_none());
    }

    #[test]
    fn test_score_case_wrong_function_name() {
        let expected =
            vec![BfclExpectedOutput { name: "get_weather".to_string(), arguments: HashMap::new() }];

        let actual = vec![BfclToolCallOutput {
            name: "get_temperature".to_string(),
            arguments: HashMap::new(),
        }];

        let (score, details) = BfclAdapter::score_case(&expected, &actual);
        assert_eq!(score, 0.0);
        assert!(details.unwrap().contains("expected function 'get_weather'"));
    }

    #[test]
    fn test_score_case_missing_argument() {
        let expected = vec![BfclExpectedOutput {
            name: "search".to_string(),
            arguments: HashMap::from([
                ("query".to_string(), serde_json::json!("rust")),
                ("limit".to_string(), serde_json::json!(10)),
            ]),
        }];

        let actual = vec![BfclToolCallOutput {
            name: "search".to_string(),
            arguments: HashMap::from([("query".to_string(), serde_json::json!("rust"))]),
        }];

        let (score, details) = BfclAdapter::score_case(&expected, &actual);
        assert_eq!(score, 0.0);
        assert!(details.unwrap().contains("missing argument"));
    }

    #[test]
    fn test_score_case_extra_argument() {
        let expected = vec![BfclExpectedOutput {
            name: "search".to_string(),
            arguments: HashMap::from([("query".to_string(), serde_json::json!("rust"))]),
        }];

        let actual = vec![BfclToolCallOutput {
            name: "search".to_string(),
            arguments: HashMap::from([
                ("query".to_string(), serde_json::json!("rust")),
                ("extra".to_string(), serde_json::json!("unexpected")),
            ]),
        }];

        let (score, details) = BfclAdapter::score_case(&expected, &actual);
        assert_eq!(score, 0.0);
        assert!(details.unwrap().contains("unexpected extra argument"));
    }

    #[test]
    fn test_score_case_wrong_count() {
        let expected = vec![
            BfclExpectedOutput { name: "a".to_string(), arguments: HashMap::new() },
            BfclExpectedOutput { name: "b".to_string(), arguments: HashMap::new() },
        ];

        let actual = vec![BfclToolCallOutput { name: "a".to_string(), arguments: HashMap::new() }];

        let (score, details) = BfclAdapter::score_case(&expected, &actual);
        assert_eq!(score, 0.0);
        assert!(details.unwrap().contains("expected 2 tool call(s), got 1"));
    }

    #[test]
    fn test_translate_functions_to_tool_schemas() {
        let functions = vec![BfclFunction {
            name: "get_weather".to_string(),
            description: "Get the weather for a city".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "city": {"type": "string"}
                },
                "required": ["city"]
            }),
        }];

        let schemas = BfclAdapter::translate_functions_to_tool_schemas(&functions);
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0]["name"], "get_weather");
        assert_eq!(schemas[0]["description"], "Get the weather for a city");
        assert!(schemas[0]["parameters"]["properties"]["city"].is_object());
    }

    #[test]
    fn test_bfcl_config_default() {
        let config = BfclConfig::default();
        assert_eq!(config.dataset_path, PathBuf::from("bfcl_dataset.jsonl"));
        assert!(config.categories.is_empty());
        assert!(config.max_cases.is_none());
    }

    #[test]
    fn test_bfcl_adapter_default() {
        let adapter = BfclAdapter::default();
        assert_eq!(adapter.name(), "bfcl");
    }

    #[test]
    fn test_bfcl_entry_deserialization() {
        let json = r#"{
            "id": "test_1",
            "category": "simple",
            "question": "What is the weather in Seattle?",
            "function": [
                {
                    "name": "get_weather",
                    "description": "Get weather info",
                    "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}
                }
            ],
            "expected_output": [
                {
                    "name": "get_weather",
                    "arguments": {"city": "Seattle"}
                }
            ]
        }"#;

        let entry: BfclEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.id, "test_1");
        assert_eq!(entry.category, "simple");
        assert_eq!(entry.functions.len(), 1);
        assert_eq!(entry.expected_output.len(), 1);
        assert_eq!(entry.expected_output[0].name, "get_weather");
    }

    #[test]
    fn test_leaderboard_result_generation() {
        let config = BfclConfig::default();
        let adapter = BfclAdapter::new(config);

        let entries = vec![
            BfclEntry {
                id: "test_1".to_string(),
                category: "simple".to_string(),
                question: "q1".to_string(),
                functions: vec![],
                expected_output: vec![],
            },
            BfclEntry {
                id: "test_2".to_string(),
                category: "simple".to_string(),
                question: "q2".to_string(),
                functions: vec![],
                expected_output: vec![],
            },
            BfclEntry {
                id: "test_3".to_string(),
                category: "multiple".to_string(),
                question: "q3".to_string(),
                functions: vec![],
                expected_output: vec![],
            },
        ];

        let cases = vec![
            CaseResult { case_id: "test_1".to_string(), passed: true, score: 1.0, details: None },
            CaseResult {
                case_id: "test_2".to_string(),
                passed: false,
                score: 0.0,
                details: Some("wrong function".to_string()),
            },
            CaseResult { case_id: "test_3".to_string(), passed: true, score: 1.0, details: None },
        ];

        let result = adapter.generate_leaderboard_result("gemini-2.5-flash", &entries, &cases);
        assert_eq!(result.model, "gemini-2.5-flash");
        assert_eq!(result.total_cases, 3);
        assert_eq!(result.passed_cases, 2);
        assert!((result.overall_accuracy - 2.0 / 3.0).abs() < f64::EPSILON);

        let simple = &result.category_accuracy["simple"];
        assert_eq!(simple.total, 2);
        assert_eq!(simple.passed, 1);
        assert!((simple.accuracy - 0.5).abs() < f64::EPSILON);

        let multiple = &result.category_accuracy["multiple"];
        assert_eq!(multiple.total, 1);
        assert_eq!(multiple.passed, 1);
        assert!((multiple.accuracy - 1.0).abs() < f64::EPSILON);
    }
}
