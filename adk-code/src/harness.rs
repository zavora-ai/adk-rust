//! Harness template and source validation for Rust code execution.
//!
//! This module contains the harness template that wraps user code, source
//! validation functions, and output extraction utilities. These are shared
//! between `RustSandboxExecutor` (legacy) and `RustExecutor` (new).

use crate::ExecutionError;

/// Patterns that are rejected in user code because they conflict with the
/// harness or exceed the phase 1 source model.
///
/// Each entry is `(pattern, human-readable reason)`. The pattern is matched
/// against the source after stripping comments and string literals would be
/// a more robust approach, but for phase 1 a simple token-level scan is
/// sufficient — `fn main` inside a string literal is an unlikely false positive
/// and the compile step would catch real conflicts anyway.
pub const REJECTED_PATTERNS: &[(&str, &str)] = &[
    ("fn main", "user code must not define `fn main()` — the harness provides it"),
    ("#![", "crate-level attributes (`#![...]`) are not supported in the harness body"),
];

/// The harness template that wraps user code.
///
/// The user provides `fn run(input: serde_json::Value) -> serde_json::Value`.
/// The harness reads JSON from stdin, calls `run()`, and prints JSON to stdout.
///
/// ## Available to User Code
///
/// - `serde_json::Value` (imported at top level)
/// - All public items from `serde_json` (e.g., `serde_json::json!`, `serde_json::Map`)
/// - The full Rust standard library
///
/// ## Not Available
///
/// - External crates other than `serde_json`
/// - `fn main()` (provided by the harness)
/// - Crate-level attributes (`#![...]`)
/// - Multi-file modules
pub const HARNESS_TEMPLATE: &str = r#"use serde_json::Value;

{user_code}

fn main() {
    let input: Value = serde_json::from_reader(std::io::stdin()).unwrap_or(Value::Null);
    let output = run(input);
    println!("{}", serde_json::to_string(&output).unwrap());
}
"#;

/// Validate that user source code fits the phase 1 bounded source model.
///
/// The phase 1 model requires self-contained snippets that provide
/// `fn run(input: Value) -> Value`. The harness supplies `fn main()`,
/// `use serde_json::Value;`, and links `serde_json`. User code must not
/// redefine `main` or use crate-level attributes.
///
/// Returns `Ok(())` if the code passes validation, or
/// `Err(ExecutionError::InvalidRequest(...))` with a descriptive message.
///
/// # Example
///
/// ```rust
/// use adk_code::validate_rust_source;
///
/// // Valid: provides the run() contract
/// assert!(validate_rust_source(r#"
///     fn run(input: serde_json::Value) -> serde_json::Value {
///         input
///     }
/// "#).is_ok());
///
/// // Invalid: defines fn main()
/// assert!(validate_rust_source(r#"
///     fn main() { println!("hello"); }
/// "#).is_err());
/// ```
pub fn validate_rust_source(code: &str) -> Result<(), ExecutionError> {
    // Strip single-line comments and block comments to reduce false positives.
    let stripped = strip_comments(code);

    for &(pattern, reason) in REJECTED_PATTERNS {
        if stripped.contains(pattern) {
            return Err(ExecutionError::InvalidRequest(reason.to_string()));
        }
    }

    Ok(())
}

/// Strip single-line (`//`) and block (`/* */`) comments from Rust source.
///
/// This is a best-effort heuristic for phase 1 validation. It does not handle
/// string literals containing comment-like sequences, but that is acceptable
/// for the patterns we check (e.g., `fn main` inside a string literal is
/// unlikely and would be caught at compile time anyway).
pub fn strip_comments(code: &str) -> String {
    let mut result = String::with_capacity(code.len());
    let mut chars = code.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '/' {
            match chars.peek() {
                Some('/') => {
                    // Single-line comment — skip to end of line.
                    chars.next();
                    for ch in chars.by_ref() {
                        if ch == '\n' {
                            result.push('\n');
                            break;
                        }
                    }
                }
                Some('*') => {
                    // Block comment — skip to closing `*/`.
                    chars.next();
                    let mut depth = 1u32;
                    while depth > 0 {
                        match chars.next() {
                            Some('/') if chars.peek() == Some(&'*') => {
                                chars.next();
                                depth += 1;
                            }
                            Some('*') if chars.peek() == Some(&'/') => {
                                chars.next();
                                depth -= 1;
                            }
                            Some(_) => {}
                            None => break,
                        }
                    }
                    result.push(' ');
                }
                _ => result.push(c),
            }
        } else {
            result.push(c);
        }
    }

    result
}

/// Extract structured JSON output from stdout.
///
/// The harness prints the JSON output as the last line of stdout. This function
/// tries to parse the last non-empty line as JSON. If successful, it returns
/// the parsed value and the remaining stdout (everything before the last line).
/// If parsing fails, it returns `None` and the full stdout.
pub fn extract_structured_output(stdout: &str) -> (Option<serde_json::Value>, String) {
    let trimmed = stdout.trim_end();
    if trimmed.is_empty() {
        return (None, String::new());
    }

    // Find the last line.
    if let Some(last_newline_pos) = trimmed.rfind('\n') {
        let last_line = &trimmed[last_newline_pos + 1..];
        let before = &trimmed[..last_newline_pos];

        if let Ok(value) = serde_json::from_str::<serde_json::Value>(last_line) {
            return (Some(value), before.to_string());
        }
    } else {
        // Only one line — try to parse it as JSON.
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
            return (Some(value), String::new());
        }
    }

    (None, stdout.to_string())
}

/// Truncate output to the given byte limit. Returns the (possibly truncated)
/// string and whether truncation occurred.
pub fn truncate_output(output: String, max_bytes: usize) -> (String, bool) {
    if output.len() <= max_bytes {
        (output, false)
    } else {
        // Truncate at a char boundary.
        let truncated = output
            .char_indices()
            .take_while(|(i, _)| *i < max_bytes)
            .map(|(_, c)| c)
            .collect::<String>();
        (truncated, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Truncation tests ───────────────────────────────────────────

    #[test]
    fn truncate_output_no_truncation() {
        let (result, truncated) = truncate_output("hello".to_string(), 100);
        assert_eq!(result, "hello");
        assert!(!truncated);
    }

    #[test]
    fn truncate_output_at_limit() {
        let (result, truncated) = truncate_output("hello".to_string(), 5);
        assert_eq!(result, "hello");
        assert!(!truncated);
    }

    #[test]
    fn truncate_output_over_limit() {
        let (result, truncated) = truncate_output("hello world".to_string(), 5);
        assert_eq!(result, "hello");
        assert!(truncated);
    }

    #[test]
    fn truncate_output_respects_char_boundaries() {
        // Multi-byte character: "é" is 2 bytes in UTF-8.
        // "café" = 5 bytes: c(0), a(1), f(2), é(3..4).
        // With limit 4, "é" starts at byte 3 which is < 4, so it's included.
        let (result, truncated) = truncate_output("café".to_string(), 4);
        assert_eq!(result, "café");
        assert!(truncated);

        // With limit 3, "é" starts at byte 3 which is NOT < 3, so it's excluded.
        let (result, truncated) = truncate_output("café".to_string(), 3);
        assert_eq!(result, "caf");
        assert!(truncated);
    }

    // ── Structured output extraction tests ─────────────────────────

    #[test]
    fn extract_structured_output_single_json_line() {
        let (output, display) = extract_structured_output(r#"{"answer":42}"#);
        assert_eq!(output, Some(serde_json::json!({"answer": 42})));
        assert_eq!(display, "");
    }

    #[test]
    fn extract_structured_output_with_preceding_text() {
        let stdout = "some debug output\n{\"answer\":42}";
        let (output, display) = extract_structured_output(stdout);
        assert_eq!(output, Some(serde_json::json!({"answer": 42})));
        assert_eq!(display, "some debug output");
    }

    #[test]
    fn extract_structured_output_no_json() {
        let stdout = "just plain text\nmore text";
        let (output, display) = extract_structured_output(stdout);
        assert!(output.is_none());
        assert_eq!(display, stdout);
    }

    #[test]
    fn extract_structured_output_empty() {
        let (output, display) = extract_structured_output("");
        assert!(output.is_none());
        assert_eq!(display, "");
    }

    // ── Source model validation tests ──────────────────────────────

    #[test]
    fn validate_accepts_valid_run_function() {
        let code = r#"
fn run(input: serde_json::Value) -> serde_json::Value {
    let v = input["x"].as_i64().unwrap_or(0);
    serde_json::json!({ "result": v * 2 })
}
"#;
        assert!(validate_rust_source(code).is_ok());
    }

    #[test]
    fn validate_accepts_helper_functions() {
        let code = r#"
fn helper(x: i64) -> i64 { x + 1 }

fn run(input: serde_json::Value) -> serde_json::Value {
    let v = input["x"].as_i64().unwrap_or(0);
    serde_json::json!({ "result": helper(v) })
}
"#;
        assert!(validate_rust_source(code).is_ok());
    }

    #[test]
    fn validate_rejects_fn_main() {
        let code = r#"
fn main() {
    println!("hello");
}
"#;
        let err = validate_rust_source(code).unwrap_err();
        assert!(matches!(err, ExecutionError::InvalidRequest(_)));
        assert!(err.to_string().contains("fn main()"));
    }

    #[test]
    fn validate_rejects_crate_level_attributes() {
        let code = r#"
#![allow(unused)]
fn run(input: serde_json::Value) -> serde_json::Value { input }
"#;
        let err = validate_rust_source(code).unwrap_err();
        assert!(matches!(err, ExecutionError::InvalidRequest(_)));
        assert!(err.to_string().contains("crate-level attributes"));
    }

    #[test]
    fn validate_ignores_fn_main_in_comments() {
        let code = r#"
// fn main() is provided by the harness
fn run(input: serde_json::Value) -> serde_json::Value { input }
"#;
        assert!(validate_rust_source(code).is_ok());
    }

    #[test]
    fn validate_ignores_fn_main_in_block_comments() {
        let code = r#"
/* fn main() { } */
fn run(input: serde_json::Value) -> serde_json::Value { input }
"#;
        assert!(validate_rust_source(code).is_ok());
    }

    #[test]
    fn validate_ignores_crate_attr_in_comments() {
        let code = r#"
// #![allow(unused)]
fn run(input: serde_json::Value) -> serde_json::Value { input }
"#;
        assert!(validate_rust_source(code).is_ok());
    }

    #[test]
    fn validate_accepts_item_level_attributes() {
        let code = r#"
#[derive(Debug)]
struct Foo { x: i64 }

fn run(input: serde_json::Value) -> serde_json::Value { input }
"#;
        assert!(validate_rust_source(code).is_ok());
    }

    #[test]
    fn validate_accepts_empty_code() {
        assert!(validate_rust_source("").is_ok());
    }

    // ── Comment stripping tests ────────────────────────────────────

    #[test]
    fn strip_comments_removes_single_line() {
        let code = "let x = 1; // this is a comment\nlet y = 2;";
        let stripped = strip_comments(code);
        assert!(!stripped.contains("this is a comment"));
        assert!(stripped.contains("let x = 1;"));
        assert!(stripped.contains("let y = 2;"));
    }

    #[test]
    fn strip_comments_removes_block_comment() {
        let code = "let x = /* hidden */ 1;";
        let stripped = strip_comments(code);
        assert!(!stripped.contains("hidden"));
        assert!(stripped.contains("let x ="));
        assert!(stripped.contains("1;"));
    }

    #[test]
    fn strip_comments_handles_nested_block_comments() {
        let code = "before /* outer /* inner */ still outer */ after";
        let stripped = strip_comments(code);
        assert!(!stripped.contains("outer"));
        assert!(!stripped.contains("inner"));
        assert!(stripped.contains("before"));
        assert!(stripped.contains("after"));
    }
}
