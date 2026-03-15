//! Rust compiler diagnostic parsing.
//!
//! This module provides types and a parser for `rustc --error-format=json` output.
//! Each line of stderr from `rustc` in JSON mode is a JSON object containing
//! diagnostic information. This module parses those objects into structured
//! [`RustDiagnostic`] values.
//!
//! # Example
//!
//! ```rust
//! use adk_code::diagnostics::parse_diagnostics;
//!
//! let stderr = r#"{"message":"expected `;`","code":{"code":"E0308","explanation":null},"level":"error","spans":[{"file_name":"main.rs","byte_start":10,"byte_end":11,"line_start":1,"line_end":1,"column_start":11,"column_end":12,"is_primary":true,"text":[{"text":"let x = 1","highlight_start":11,"highlight_end":12}],"label":null,"suggested_replacement":null,"suggestion_applicability":null,"expansion":null}],"children":[],"rendered":"error: expected `;`"}"#;
//! let diagnostics = parse_diagnostics(stderr);
//! assert_eq!(diagnostics.len(), 1);
//! assert_eq!(diagnostics[0].level, "error");
//! ```

use serde::{Deserialize, Serialize};

/// A parsed Rust compiler diagnostic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RustDiagnostic {
    /// Severity level: `"error"`, `"warning"`, `"note"`, `"help"`.
    pub level: String,
    /// The diagnostic message.
    pub message: String,
    /// Source spans where the diagnostic applies.
    #[serde(default)]
    pub spans: Vec<DiagnosticSpan>,
    /// Optional error code (e.g., `"E0308"`).
    pub code: Option<String>,
}

/// A source span within a diagnostic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticSpan {
    /// The file name where the diagnostic applies.
    pub file_name: String,
    /// Starting line number (1-indexed).
    pub line_start: u32,
    /// Ending line number (1-indexed).
    pub line_end: u32,
    /// Starting column number (1-indexed).
    pub column_start: u32,
    /// Ending column number (1-indexed).
    pub column_end: u32,
    /// Source text with highlight information.
    #[serde(default)]
    pub text: Vec<SpanText>,
}

/// A line of source text with highlight markers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpanText {
    /// The source text of the line.
    pub text: String,
    /// Column where the highlight starts (1-indexed).
    pub highlight_start: u32,
    /// Column where the highlight ends (1-indexed).
    pub highlight_end: u32,
}

/// Intermediate type for deserializing rustc JSON output.
///
/// The rustc JSON format nests the error code inside a `code` object:
/// `{"code": {"code": "E0308", "explanation": null}}`.
#[derive(Debug, Deserialize)]
struct RawDiagnostic {
    level: String,
    message: String,
    #[serde(default)]
    spans: Vec<RawSpan>,
    code: Option<RawCode>,
}

#[derive(Debug, Deserialize)]
struct RawCode {
    code: String,
}

#[derive(Debug, Deserialize)]
struct RawSpan {
    file_name: String,
    line_start: u32,
    line_end: u32,
    column_start: u32,
    column_end: u32,
    #[serde(default)]
    text: Vec<RawSpanText>,
}

#[derive(Debug, Deserialize)]
struct RawSpanText {
    text: String,
    highlight_start: u32,
    highlight_end: u32,
}

/// Parse rustc JSON diagnostics from `--error-format=json` output.
///
/// Each line of stderr is attempted as a JSON diagnostic object. Lines that
/// fail to parse (e.g., non-JSON rendered output) are silently skipped.
///
/// # Example
///
/// ```rust
/// use adk_code::diagnostics::parse_diagnostics;
///
/// let stderr = "";
/// let diagnostics = parse_diagnostics(stderr);
/// assert!(diagnostics.is_empty());
/// ```
pub fn parse_diagnostics(stderr: &str) -> Vec<RustDiagnostic> {
    stderr
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let raw: RawDiagnostic = serde_json::from_str(line).ok()?;
            Some(RustDiagnostic {
                level: raw.level,
                message: raw.message,
                spans: raw
                    .spans
                    .into_iter()
                    .map(|s| DiagnosticSpan {
                        file_name: s.file_name,
                        line_start: s.line_start,
                        line_end: s.line_end,
                        column_start: s.column_start,
                        column_end: s.column_end,
                        text: s
                            .text
                            .into_iter()
                            .map(|t| SpanText {
                                text: t.text,
                                highlight_start: t.highlight_start,
                                highlight_end: t.highlight_end,
                            })
                            .collect(),
                    })
                    .collect(),
                code: raw.code.map(|c| c.code),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_stderr() {
        let diagnostics = parse_diagnostics("");
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn parse_non_json_lines_skipped() {
        let stderr = "some random text\nnot json at all\n";
        let diagnostics = parse_diagnostics(stderr);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn parse_single_error_diagnostic() {
        let stderr = r#"{"message":"expected `;`","code":{"code":"E0308","explanation":null},"level":"error","spans":[{"file_name":"main.rs","byte_start":10,"byte_end":11,"line_start":1,"line_end":1,"column_start":11,"column_end":12,"is_primary":true,"text":[{"text":"let x = 1","highlight_start":11,"highlight_end":12}],"label":null,"suggested_replacement":null,"suggestion_applicability":null,"expansion":null}],"children":[],"rendered":"error: expected `;`"}"#;
        let diagnostics = parse_diagnostics(stderr);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].level, "error");
        assert_eq!(diagnostics[0].message, "expected `;`");
        assert_eq!(diagnostics[0].code.as_deref(), Some("E0308"));
        assert_eq!(diagnostics[0].spans.len(), 1);
        assert_eq!(diagnostics[0].spans[0].file_name, "main.rs");
        assert_eq!(diagnostics[0].spans[0].line_start, 1);
        assert_eq!(diagnostics[0].spans[0].column_start, 11);
        assert_eq!(diagnostics[0].spans[0].text.len(), 1);
        assert_eq!(diagnostics[0].spans[0].text[0].text, "let x = 1");
    }

    #[test]
    fn parse_warning_without_code() {
        let stderr = r#"{"message":"unused variable: `x`","code":null,"level":"warning","spans":[],"children":[],"rendered":"warning: unused variable"}"#;
        let diagnostics = parse_diagnostics(stderr);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].level, "warning");
        assert!(diagnostics[0].code.is_none());
        assert!(diagnostics[0].spans.is_empty());
    }

    #[test]
    fn parse_mixed_json_and_text() {
        let stderr = format!(
            "{}\nerror[E0308]: mismatched types\n{}",
            r#"{"message":"type mismatch","code":{"code":"E0308","explanation":null},"level":"error","spans":[],"children":[],"rendered":"error"}"#,
            r#"{"message":"help: consider","code":null,"level":"help","spans":[],"children":[],"rendered":"help"}"#,
        );
        let diagnostics = parse_diagnostics(&stderr);
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].level, "error");
        assert_eq!(diagnostics[1].level, "help");
    }

    #[test]
    fn parse_diagnostic_with_multiple_spans() {
        let stderr = r#"{"message":"mismatched types","code":{"code":"E0308","explanation":null},"level":"error","spans":[{"file_name":"main.rs","byte_start":10,"byte_end":11,"line_start":1,"line_end":1,"column_start":11,"column_end":12,"is_primary":true,"text":[{"text":"let x: i32 = \"hello\"","highlight_start":15,"highlight_end":22}],"label":null,"suggested_replacement":null,"suggestion_applicability":null,"expansion":null},{"file_name":"main.rs","byte_start":20,"byte_end":25,"line_start":2,"line_end":2,"column_start":5,"column_end":10,"is_primary":false,"text":[{"text":"    x + 1","highlight_start":5,"highlight_end":10}],"label":null,"suggested_replacement":null,"suggestion_applicability":null,"expansion":null}],"children":[],"rendered":"error"}"#;
        let diagnostics = parse_diagnostics(stderr);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].spans.len(), 2);
        assert_eq!(diagnostics[0].spans[0].line_start, 1);
        assert_eq!(diagnostics[0].spans[1].line_start, 2);
    }
}
