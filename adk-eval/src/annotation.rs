//! Human annotation workflow via JSONL export/import.
//!
//! Provides JSONL-based file format for exporting evaluation cases for human review
//! and importing human verdicts back into the system.
//!
//! # Format
//!
//! Each line in the JSONL file is a JSON object representing an [`AnnotationRecord`].
//! On export, the `verdict` field is `null`; on import, annotators fill it in.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_eval::annotation::{AnnotationStore, AnnotationRecord};
//! use adk_eval::schema::EvalCase;
//! use adk_eval::report::EvaluationResult;
//! use std::collections::HashSet;
//!
//! // Export cases for human review
//! AnnotationStore::export(&cases, &results, "annotations.jsonl")?;
//!
//! // Import annotated verdicts
//! let valid_ids: HashSet<String> = cases.iter().map(|c| c.eval_id.clone()).collect();
//! let (records, warnings) = AnnotationStore::import("annotations.jsonl", &valid_ids)?;
//! ```

use std::collections::HashSet;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{EvalError, Result};
use crate::report::EvaluationResult;
use crate::schema::EvalCase;

/// A single annotation record for human review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationRecord {
    /// Identifier of the evaluation case
    pub case_id: String,
    /// Input text or conversation
    pub input: String,
    /// Expected response (if available)
    pub expected_response: Option<String>,
    /// Actual agent response (if available)
    pub actual_response: Option<String>,
    /// Human-provided verdict (empty on export, filled on import)
    pub verdict: Option<HumanVerdict>,
}

/// Human-provided verdict for an evaluation case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HumanVerdict {
    /// Score assigned by the human annotator
    pub score: f64,
    /// Reasoning for the score
    pub reasoning: String,
    /// Identifier of the annotator
    pub annotator_id: String,
}

/// Manages JSONL export and import for human annotation.
pub struct AnnotationStore;

impl AnnotationStore {
    /// Export evaluation cases to a JSONL file for human review.
    ///
    /// Each case is written as a single JSON line containing:
    /// - `case_id`: the eval case identifier
    /// - `input`: concatenated user input text from conversation turns
    /// - `expected_response`: expected final response text (if available)
    /// - `actual_response`: actual agent response from evaluation results (if available)
    /// - `verdict`: always `null` on export (to be filled by human annotators)
    ///
    /// # Errors
    ///
    /// Returns `EvalError::AnnotationError` if the file cannot be created or written to.
    pub fn export(
        cases: &[EvalCase],
        results: &[EvaluationResult],
        output_path: impl AsRef<Path>,
    ) -> Result<()> {
        let file = std::fs::File::create(output_path.as_ref()).map_err(|e| {
            EvalError::AnnotationError(format!(
                "failed to create annotation file '{}': {e}",
                output_path.as_ref().display()
            ))
        })?;
        let mut writer = BufWriter::new(file);

        for case in cases {
            // Build input from conversation turns
            let input = case
                .conversation
                .iter()
                .map(|turn| turn.user_content.get_text())
                .collect::<Vec<_>>()
                .join("\n");

            // Get expected response from last turn's final_response
            let expected_response = case
                .conversation
                .last()
                .and_then(|turn| turn.final_response.as_ref())
                .map(|content| content.get_text());

            // Find matching result for this case to get actual response
            let actual_response = results
                .iter()
                .find(|r| r.eval_id == case.eval_id)
                .and_then(|r| r.turn_results.last())
                .and_then(|tr| tr.actual_response.clone());

            let record = AnnotationRecord {
                case_id: case.eval_id.clone(),
                input,
                expected_response,
                actual_response,
                verdict: None,
            };

            let line = serde_json::to_string(&record).map_err(|e| {
                EvalError::AnnotationError(format!(
                    "failed to serialize annotation record for case '{}': {e}",
                    case.eval_id
                ))
            })?;

            writeln!(writer, "{line}").map_err(|e| {
                EvalError::AnnotationError(format!("failed to write annotation line: {e}"))
            })?;
        }

        writer.flush().map_err(|e| {
            EvalError::AnnotationError(format!("failed to flush annotation file: {e}"))
        })?;

        Ok(())
    }

    /// Import annotations from a JSONL file.
    ///
    /// Reads the file line-by-line, parsing each line as an [`AnnotationRecord`].
    /// Validates that each record's `case_id` exists in the provided set of valid IDs.
    ///
    /// # Returns
    ///
    /// A tuple of:
    /// - Valid annotation records (those with case_ids in the valid set)
    /// - Warning messages for unmatched case_ids
    ///
    /// # Errors
    ///
    /// Returns `EvalError::AnnotationError` if the file cannot be read or a line
    /// contains malformed JSON.
    pub fn import(
        path: impl AsRef<Path>,
        valid_case_ids: &HashSet<String>,
    ) -> Result<(Vec<AnnotationRecord>, Vec<String>)> {
        let file = std::fs::File::open(path.as_ref()).map_err(|e| {
            EvalError::AnnotationError(format!(
                "failed to open annotation file '{}': {e}",
                path.as_ref().display()
            ))
        })?;
        let reader = BufReader::new(file);

        let mut records = Vec::new();
        let mut warnings = Vec::new();

        for (line_num, line_result) in reader.lines().enumerate() {
            let line = line_result.map_err(|e| {
                EvalError::AnnotationError(format!("failed to read line {}: {e}", line_num + 1))
            })?;

            // Skip empty lines
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let record: AnnotationRecord = serde_json::from_str(trimmed).map_err(|e| {
                EvalError::AnnotationError(format!(
                    "failed to parse annotation at line {}: {e}",
                    line_num + 1
                ))
            })?;

            if valid_case_ids.contains(&record.case_id) {
                records.push(record);
            } else {
                warnings.push(format!(
                    "unmatched case_id '{}' at line {}",
                    record.case_id,
                    line_num + 1
                ));
            }
        }

        Ok((records, warnings))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::EvaluationResult;
    use crate::schema::{ContentData, EvalCase, Turn};
    use std::collections::HashMap;
    use std::time::Duration;
    use tempfile::NamedTempFile;

    fn make_case(id: &str, input: &str, expected: Option<&str>) -> EvalCase {
        let mut conversation = vec![Turn {
            invocation_id: format!("inv_{id}"),
            user_content: ContentData::text(input),
            final_response: expected.map(ContentData::model_response),
            intermediate_data: None,
        }];

        // If there's no expected response on the turn, still keep it consistent
        if expected.is_none() {
            conversation[0].final_response = None;
        }

        EvalCase {
            eval_id: id.to_string(),
            description: String::new(),
            conversation,
            session_input: Default::default(),
            tags: vec![],
            metadata: None,
        }
    }

    fn make_result(id: &str) -> EvaluationResult {
        EvaluationResult::passed(id, HashMap::new(), Duration::from_millis(50))
    }

    #[test]
    fn test_export_creates_jsonl_file() {
        let cases = vec![
            make_case("case_1", "Hello", Some("Hi there")),
            make_case("case_2", "How are you?", None),
        ];
        let results = vec![make_result("case_1"), make_result("case_2")];

        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        AnnotationStore::export(&cases, &results, &path).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);

        // Parse first line
        let record: AnnotationRecord = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(record.case_id, "case_1");
        assert_eq!(record.input, "Hello");
        assert_eq!(record.expected_response, Some("Hi there".to_string()));
        assert!(record.verdict.is_none());

        // Parse second line
        let record: AnnotationRecord = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(record.case_id, "case_2");
        assert_eq!(record.input, "How are you?");
        assert_eq!(record.expected_response, None);
        assert!(record.verdict.is_none());
    }

    #[test]
    fn test_import_valid_records() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        let records = [
            AnnotationRecord {
                case_id: "case_1".to_string(),
                input: "Hello".to_string(),
                expected_response: Some("Hi".to_string()),
                actual_response: Some("Hey".to_string()),
                verdict: Some(HumanVerdict {
                    score: 0.9,
                    reasoning: "Good response".to_string(),
                    annotator_id: "reviewer_1".to_string(),
                }),
            },
            AnnotationRecord {
                case_id: "case_2".to_string(),
                input: "Bye".to_string(),
                expected_response: None,
                actual_response: None,
                verdict: None,
            },
        ];

        // Write JSONL manually
        let content: String = records
            .iter()
            .map(|r| serde_json::to_string(r).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, content).unwrap();

        let valid_ids: HashSet<String> =
            ["case_1", "case_2"].iter().map(|s| s.to_string()).collect();
        let (imported, warnings) = AnnotationStore::import(&path, &valid_ids).unwrap();

        assert_eq!(imported.len(), 2);
        assert!(warnings.is_empty());
        assert_eq!(imported[0].case_id, "case_1");
        assert!(imported[0].verdict.is_some());
        assert_eq!(imported[0].verdict.as_ref().unwrap().score, 0.9);
    }

    #[test]
    fn test_import_unmatched_case_ids_produce_warnings() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        let record = AnnotationRecord {
            case_id: "unknown_case".to_string(),
            input: "test".to_string(),
            expected_response: None,
            actual_response: None,
            verdict: None,
        };

        let content = serde_json::to_string(&record).unwrap();
        std::fs::write(&path, content).unwrap();

        let valid_ids: HashSet<String> = ["case_1"].iter().map(|s| s.to_string()).collect();
        let (imported, warnings) = AnnotationStore::import(&path, &valid_ids).unwrap();

        assert!(imported.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("unknown_case"));
    }

    #[test]
    fn test_import_malformed_json_returns_error() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        std::fs::write(&path, "not valid json\n").unwrap();

        let valid_ids: HashSet<String> = HashSet::new();
        let result = AnnotationStore::import(&path, &valid_ids);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("annotation"));
    }

    #[test]
    fn test_import_skips_empty_lines() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        let record = AnnotationRecord {
            case_id: "case_1".to_string(),
            input: "hello".to_string(),
            expected_response: None,
            actual_response: None,
            verdict: None,
        };

        let line = serde_json::to_string(&record).unwrap();
        let content = format!("\n{line}\n\n");
        std::fs::write(&path, content).unwrap();

        let valid_ids: HashSet<String> = ["case_1"].iter().map(|s| s.to_string()).collect();
        let (imported, warnings) = AnnotationStore::import(&path, &valid_ids).unwrap();

        assert_eq!(imported.len(), 1);
        assert!(warnings.is_empty());
    }

    #[test]
    fn test_export_import_round_trip() {
        let cases = vec![
            make_case("rt_1", "What is Rust?", Some("A systems programming language")),
            make_case("rt_2", "Tell me a joke", Some("Why did the crab cross the road?")),
        ];
        let results = vec![make_result("rt_1"), make_result("rt_2")];

        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        // Export
        AnnotationStore::export(&cases, &results, &path).unwrap();

        // Import
        let valid_ids: HashSet<String> = cases.iter().map(|c| c.eval_id.clone()).collect();
        let (imported, warnings) = AnnotationStore::import(&path, &valid_ids).unwrap();

        assert!(warnings.is_empty());
        assert_eq!(imported.len(), 2);

        // Verify round-trip fidelity
        assert_eq!(imported[0].case_id, "rt_1");
        assert_eq!(imported[0].input, "What is Rust?");
        assert_eq!(
            imported[0].expected_response,
            Some("A systems programming language".to_string())
        );

        assert_eq!(imported[1].case_id, "rt_2");
        assert_eq!(imported[1].input, "Tell me a joke");
        assert_eq!(
            imported[1].expected_response,
            Some("Why did the crab cross the road?".to_string())
        );
    }

    #[test]
    fn test_export_nonexistent_directory_returns_error() {
        let cases = vec![make_case("c1", "hi", None)];
        let results = vec![];

        let result =
            AnnotationStore::export(&cases, &results, "/nonexistent/dir/annotations.jsonl");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("annotation"));
    }

    #[test]
    fn test_import_nonexistent_file_returns_error() {
        let valid_ids: HashSet<String> = HashSet::new();
        let result = AnnotationStore::import("/nonexistent/file.jsonl", &valid_ids);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("annotation"));
    }
}
