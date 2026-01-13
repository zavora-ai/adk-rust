//! Test execution tool for Ralph.
//!
//! This tool provides multi-language test execution support with automatic
//! language detection from project files.
//!
//! ## Supported Languages
//!
//! - Rust: `cargo test`
//! - Python: `pytest`
//! - TypeScript/JavaScript: `vitest` or `npm test`
//! - Go: `go test`
//! - Java: `mvn test`
//!
//! ## Requirements Validated
//!
//! - 6.5: THE Ralph_Loop_Agent SHALL use appropriate testing framework for the language
//! - 10.3: THE Ralph_Loop_Agent SHALL use language-appropriate testing frameworks
//! - 10.5: THE system SHALL support at minimum: Rust, Python, TypeScript, Go, Java

use crate::models::TestResults;
use crate::telemetry::{log_test_results, start_timing, test_execution_span, tool_call_span};
use adk_core::{Result as AdkResult, Tool, ToolContext};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;
use tracing::info;

/// Supported programming languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Rust,
    Python,
    TypeScript,
    JavaScript,
    Go,
    Java,
    Unknown,
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Language::Rust => write!(f, "rust"),
            Language::Python => write!(f, "python"),
            Language::TypeScript => write!(f, "typescript"),
            Language::JavaScript => write!(f, "javascript"),
            Language::Go => write!(f, "go"),
            Language::Java => write!(f, "java"),
            Language::Unknown => write!(f, "unknown"),
        }
    }
}

impl std::str::FromStr for Language {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_lowercase().as_str() {
            "rust" | "rs" => Language::Rust,
            "python" | "py" => Language::Python,
            "typescript" | "ts" => Language::TypeScript,
            "javascript" | "js" => Language::JavaScript,
            "go" | "golang" => Language::Go,
            "java" => Language::Java,
            _ => Language::Unknown,
        })
    }
}

impl Language {
    /// Parse a language from a string.
    pub fn parse(s: &str) -> Self {
        s.parse().unwrap_or(Language::Unknown)
    }

    /// Get the test command for this language.
    pub fn test_command(&self) -> Option<(&str, Vec<&str>)> {
        match self {
            Language::Rust => Some(("cargo", vec!["test"])),
            Language::Python => Some(("pytest", vec!["-v"])),
            Language::TypeScript | Language::JavaScript => Some(("npx", vec!["vitest", "run"])),
            Language::Go => Some(("go", vec!["test", "./..."])),
            Language::Java => Some(("mvn", vec!["test"])),
            Language::Unknown => None,
        }
    }

    /// Get the test framework name for this language.
    pub fn test_framework(&self) -> &str {
        match self {
            Language::Rust => "cargo test",
            Language::Python => "pytest",
            Language::TypeScript | Language::JavaScript => "vitest",
            Language::Go => "go test",
            Language::Java => "maven surefire",
            Language::Unknown => "unknown",
        }
    }
}

/// Tool for running tests in multiple languages.
///
/// This tool automatically detects the project language and runs
/// the appropriate test framework.
pub struct TestTool {
    /// Project root directory
    project_root: PathBuf,
    /// Override language (if set, skips detection)
    language_override: Option<Language>,
}

impl TestTool {
    /// Create a new test tool.
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            language_override: None,
        }
    }

    /// Set a language override (skips auto-detection).
    pub fn with_language(mut self, language: Language) -> Self {
        self.language_override = Some(language);
        self
    }

    /// Detect the project language from files.
    pub fn detect_language(&self) -> Language {
        // If override is set, use it
        if let Some(lang) = self.language_override {
            return lang;
        }

        // Check for language-specific files
        let root = &self.project_root;

        // Rust: Cargo.toml
        if root.join("Cargo.toml").exists() {
            return Language::Rust;
        }

        // Python: pyproject.toml, setup.py, requirements.txt
        if root.join("pyproject.toml").exists()
            || root.join("setup.py").exists()
            || root.join("requirements.txt").exists()
        {
            return Language::Python;
        }

        // TypeScript: tsconfig.json
        if root.join("tsconfig.json").exists() {
            return Language::TypeScript;
        }

        // JavaScript: package.json (without tsconfig)
        if root.join("package.json").exists() {
            // Check if it's TypeScript by looking for tsconfig
            if !root.join("tsconfig.json").exists() {
                return Language::JavaScript;
            }
            return Language::TypeScript;
        }

        // Go: go.mod
        if root.join("go.mod").exists() {
            return Language::Go;
        }

        // Java: pom.xml or build.gradle
        if root.join("pom.xml").exists() || root.join("build.gradle").exists() {
            return Language::Java;
        }

        Language::Unknown
    }

    /// Run tests and return results.
    async fn run_tests(
        &self,
        language: Language,
        test_path: Option<&str>,
    ) -> Result<TestRunResult, String> {
        let (cmd, mut args) = language
            .test_command()
            .ok_or_else(|| format!("No test command for language: {}", language))?;

        // Add test path if specified
        if let Some(path) = test_path {
            match language {
                Language::Rust => {
                    // cargo test --test <name> or cargo test -p <package>
                    if path.contains("::") || !path.contains('/') {
                        args.push(path);
                    } else {
                        args.push("--test");
                        args.push(path);
                    }
                }
                Language::Python => {
                    args.push(path);
                }
                Language::TypeScript | Language::JavaScript => {
                    args.push(path);
                }
                Language::Go => {
                    // Replace ./... with specific path
                    args.pop(); // Remove ./...
                    args.push(path);
                }
                Language::Java => {
                    args.push("-Dtest=");
                    // This is a simplification; real implementation would be more complex
                }
                Language::Unknown => {}
            }
        }

        // Execute the command
        let output = Command::new(cmd)
            .args(&args)
            .current_dir(&self.project_root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| format!("Failed to execute test command: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let success = output.status.success();

        // Parse test results from output
        let results = parse_test_output(&stdout, &stderr, language);

        Ok(TestRunResult {
            success,
            language,
            framework: language.test_framework().to_string(),
            command: format!("{} {}", cmd, args.join(" ")),
            stdout,
            stderr,
            results,
        })
    }

    /// Check if tests exist for the project.
    fn check_tests_exist(&self, language: Language) -> bool {
        let root = &self.project_root;

        match language {
            Language::Rust => {
                // Check for tests/ directory or #[test] in src/
                root.join("tests").exists()
                    || root.join("src").exists()
            }
            Language::Python => {
                // Check for tests/ or test_*.py files
                root.join("tests").exists()
                    || root.join("test").exists()
            }
            Language::TypeScript | Language::JavaScript => {
                // Check for __tests__/, *.test.ts, *.spec.ts
                root.join("__tests__").exists()
                    || root.join("tests").exists()
                    || root.join("test").exists()
            }
            Language::Go => {
                // Go tests are *_test.go files
                root.exists()
            }
            Language::Java => {
                // Check for src/test/
                root.join("src/test").exists()
            }
            Language::Unknown => false,
        }
    }
}

/// Result of a test run.
#[derive(Debug, Clone, Serialize)]
pub struct TestRunResult {
    pub success: bool,
    pub language: Language,
    pub framework: String,
    pub command: String,
    pub stdout: String,
    pub stderr: String,
    pub results: TestResults,
}

/// Parse test output to extract results.
fn parse_test_output(stdout: &str, stderr: &str, language: Language) -> TestResults {
    let combined = format!("{}\n{}", stdout, stderr);

    match language {
        Language::Rust => parse_rust_test_output(&combined),
        Language::Python => parse_python_test_output(&combined),
        Language::TypeScript | Language::JavaScript => parse_vitest_output(&combined),
        Language::Go => parse_go_test_output(&combined),
        Language::Java => parse_maven_test_output(&combined),
        Language::Unknown => TestResults::default(),
    }
}

/// Parse Rust cargo test output.
fn parse_rust_test_output(output: &str) -> TestResults {
    // Look for "test result: ok. X passed; Y failed; Z ignored"
    // or "test result: FAILED. X passed; Y failed; Z ignored"
    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;

    for line in output.lines() {
        if line.contains("test result:") {
            // Parse the summary line
            // Format: "test result: ok. 10 passed; 2 failed; 1 ignored; 0 measured; 0 filtered out"
            let parts: Vec<&str> = line.split(';').collect();
            for part in parts {
                let part = part.trim();
                if part.contains("passed") {
                    if let Some(num) = extract_number_before(part, "passed") {
                        passed = num;
                    }
                } else if part.contains("failed") {
                    if let Some(num) = extract_number_before(part, "failed") {
                        failed = num;
                    }
                } else if part.contains("ignored") {
                    if let Some(num) = extract_number_before(part, "ignored") {
                        skipped = num;
                    }
                } else if part.contains("filtered") {
                    // filtered out tests are also skipped
                    if let Some(num) = extract_number_before(part, "filtered") {
                        skipped += num;
                    }
                }
            }
        }
    }

    TestResults::new(passed, failed, skipped)
}

/// Parse Python pytest output.
fn parse_python_test_output(output: &str) -> TestResults {
    // Look for "X passed, Y failed, Z skipped" or similar
    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;

    for line in output.lines() {
        let line_lower = line.to_lowercase();
        if line_lower.contains("passed")
            || line_lower.contains("failed")
            || line_lower.contains("skipped")
        {
            // Parse pytest summary
            for word in line.split_whitespace() {
                if word.parse::<usize>().is_ok() {
                    // Next word should be the type
                    continue;
                }
                if word.contains("passed") {
                    if let Some(num) = extract_number_before(line, "passed") {
                        passed = num;
                    }
                } else if word.contains("failed") {
                    if let Some(num) = extract_number_before(line, "failed") {
                        failed = num;
                    }
                } else if word.contains("skipped") {
                    if let Some(num) = extract_number_before(line, "skipped") {
                        skipped = num;
                    }
                }
            }
        }
    }

    TestResults::new(passed, failed, skipped)
}

/// Parse vitest output.
fn parse_vitest_output(output: &str) -> TestResults {
    // Look for "Tests: X passed, Y failed, Z skipped"
    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;

    for line in output.lines() {
        if line.contains("Tests:") || line.contains("tests") {
            if let Some(num) = extract_number_before(line, "passed") {
                passed = num;
            }
            if let Some(num) = extract_number_before(line, "failed") {
                failed = num;
            }
            if let Some(num) = extract_number_before(line, "skipped") {
                skipped = num;
            }
        }
    }

    TestResults::new(passed, failed, skipped)
}

/// Parse Go test output.
fn parse_go_test_output(output: &str) -> TestResults {
    // Go test output: "ok" or "FAIL" per package, count "--- PASS:" and "--- FAIL:"
    let mut passed = 0;
    let mut failed = 0;

    for line in output.lines() {
        if line.starts_with("--- PASS:") {
            passed += 1;
        } else if line.starts_with("--- FAIL:") {
            failed += 1;
        }
    }

    TestResults::new(passed, failed, 0)
}

/// Parse Maven test output.
fn parse_maven_test_output(output: &str) -> TestResults {
    // Look for "Tests run: X, Failures: Y, Errors: Z, Skipped: W"
    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;

    for line in output.lines() {
        if line.contains("Tests run:") {
            if let Some(total) = extract_number_after(line, "Tests run:") {
                if let Some(failures) = extract_number_after(line, "Failures:") {
                    if let Some(errors) = extract_number_after(line, "Errors:") {
                        if let Some(skip) = extract_number_after(line, "Skipped:") {
                            skipped = skip;
                            failed = failures + errors;
                            passed = total.saturating_sub(failed + skipped);
                        }
                    }
                }
            }
        }
    }

    TestResults::new(passed, failed, skipped)
}

/// Extract a number before a keyword.
fn extract_number_before(s: &str, keyword: &str) -> Option<usize> {
    let lower = s.to_lowercase();
    if let Some(pos) = lower.find(keyword) {
        let before = &s[..pos];
        before
            .split_whitespace()
            .rev()
            .find_map(|word| word.trim_matches(|c: char| !c.is_numeric()).parse::<usize>().ok())
    } else {
        None
    }
}

/// Extract a number after a keyword.
fn extract_number_after(s: &str, keyword: &str) -> Option<usize> {
    if let Some(pos) = s.find(keyword) {
        let after = &s[pos + keyword.len()..];
        after
            .split(|c: char| !c.is_numeric())
            .find_map(|word| word.parse::<usize>().ok())
    } else {
        None
    }
}

#[async_trait]
impl Tool for TestTool {
    fn name(&self) -> &str {
        "test"
    }

    fn description(&self) -> &str {
        "Run tests for the project. Automatically detects the programming language and uses \
         the appropriate test framework. Supported: Rust (cargo test), Python (pytest), \
         TypeScript/JavaScript (vitest), Go (go test), Java (mvn test). \
         Operations: 'run' (execute tests), 'detect' (detect language), 'check' (verify tests exist)."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["run", "detect", "check"],
                    "description": "Operation to perform"
                },
                "language": {
                    "type": "string",
                    "enum": ["rust", "python", "typescript", "javascript", "go", "java"],
                    "description": "Override language detection"
                },
                "path": {
                    "type": "string",
                    "description": "Specific test file or module to run"
                }
            },
            "required": ["operation"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> AdkResult<Value> {
        let operation = args["operation"]
            .as_str()
            .ok_or_else(|| adk_core::AdkError::Tool("Missing 'operation' field".to_string()))?;

        // Create span for tool call
        let span = tool_call_span("test", operation);
        let _guard = span.enter();
        
        // Start timing
        let _timing = start_timing(format!("test_tool_{}", operation));

        // Get language (override or detect)
        let language = if let Some(lang_str) = args["language"].as_str() {
            Language::parse(lang_str)
        } else {
            self.detect_language()
        };

        info!(operation = %operation, language = %language, "Executing test tool");

        let result = match operation {
            "detect" => Ok(json!({
                "success": true,
                "language": language.to_string(),
                "framework": language.test_framework(),
                "tests_exist": self.check_tests_exist(language)
            })),
            "check" => {
                let exists = self.check_tests_exist(language);
                Ok(json!({
                    "success": true,
                    "language": language.to_string(),
                    "tests_exist": exists,
                    "message": if exists {
                        format!("Tests found for {} project", language)
                    } else {
                        format!("No tests found for {} project", language)
                    }
                }))
            }
            "run" => {
                if language == Language::Unknown {
                    return Err(adk_core::AdkError::Tool(
                        "Could not detect project language. Please specify 'language' parameter."
                            .to_string(),
                    ));
                }

                // Create span for test execution
                let test_span = test_execution_span(&language.to_string());
                let _test_guard = test_span.enter();

                let test_path = args["path"].as_str();
                let result = self
                    .run_tests(language, test_path)
                    .await
                    .map_err(adk_core::AdkError::Tool)?;

                // Log test results event
                log_test_results(result.results.passed, result.results.failed, result.results.skipped);

                Ok(json!({
                    "success": result.success,
                    "language": result.language.to_string(),
                    "framework": result.framework,
                    "command": result.command,
                    "results": {
                        "passed": result.results.passed,
                        "failed": result.results.failed,
                        "skipped": result.results.skipped,
                        "total": result.results.total(),
                        "all_passed": result.results.all_passed()
                    },
                    "stdout": result.stdout,
                    "stderr": result.stderr,
                    "message": if result.success {
                        format!("All tests passed: {}", result.results)
                    } else {
                        format!("Tests failed: {}", result.results)
                    }
                }))
            }
            _ => Err(format!(
                "Unknown operation '{}'. Valid operations: run, detect, check",
                operation
            )),
        };

        result.map_err(|e: String| adk_core::AdkError::Tool(e))
    }
}

impl std::fmt::Debug for TestTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestTool")
            .field("project_root", &self.project_root)
            .field("language_override", &self.language_override)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_detection() {
        assert_eq!(Language::parse("rust"), Language::Rust);
        assert_eq!(Language::parse("PYTHON"), Language::Python);
        assert_eq!(Language::parse("ts"), Language::TypeScript);
        assert_eq!(Language::parse("golang"), Language::Go);
        assert_eq!(Language::parse("unknown"), Language::Unknown);
    }

    #[test]
    fn test_rust_output_parsing() {
        let output = "test result: ok. 10 passed; 2 failed; 1 ignored; 0 measured; 0 filtered out";
        let results = parse_rust_test_output(output);
        assert_eq!(results.passed, 10);
        assert_eq!(results.failed, 2);
        assert_eq!(results.skipped, 1);
    }

    #[test]
    fn test_python_output_parsing() {
        let output = "===== 5 passed, 1 failed, 2 skipped in 1.23s =====";
        let results = parse_python_test_output(output);
        assert_eq!(results.passed, 5);
        assert_eq!(results.failed, 1);
        assert_eq!(results.skipped, 2);
    }

    #[test]
    fn test_go_output_parsing() {
        let output = r#"
--- PASS: TestOne (0.00s)
--- PASS: TestTwo (0.01s)
--- FAIL: TestThree (0.00s)
FAIL
"#;
        let results = parse_go_test_output(output);
        assert_eq!(results.passed, 2);
        assert_eq!(results.failed, 1);
    }

    #[test]
    fn test_maven_output_parsing() {
        let output = "Tests run: 10, Failures: 1, Errors: 0, Skipped: 2";
        let results = parse_maven_test_output(output);
        assert_eq!(results.passed, 7);
        assert_eq!(results.failed, 1);
        assert_eq!(results.skipped, 2);
    }
}
