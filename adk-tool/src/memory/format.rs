//! Result formatting utilities for memory tools.
//!
//! Provides [`format_memory_results`] which converts memory search results
//! into structured JSON suitable for LLM consumption.

use adk_memory::MemoryEntry;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};

/// A single formatted memory result entry.
#[derive(Debug, Serialize)]
pub struct MemoryResult {
    /// The text content of the memory entry.
    pub content: String,
    /// The author of the memory entry.
    pub author: String,
    /// ISO 8601 formatted timestamp.
    pub timestamp: String,
}

/// Format memory search results into a JSON value suitable for LLM consumption.
///
/// Each entry includes content text, author, and an ISO 8601 timestamp.
/// Returns `{"memories": [...], "count": N}`.
///
/// # Example
///
/// ```rust,ignore
/// use adk_tool::memory::format::format_memory_results;
///
/// let results = format_memory_results(&[]);
/// assert_eq!(results["count"], 0);
/// assert_eq!(results["memories"].as_array().unwrap().len(), 0);
/// ```
pub fn format_memory_results(entries: &[MemoryEntry]) -> Value {
    let results: Vec<MemoryResult> = entries
        .iter()
        .map(|entry| {
            let content = extract_text_from_content(&entry.content);
            MemoryResult {
                content,
                author: entry.author.clone(),
                timestamp: entry.timestamp.to_rfc3339(),
            }
        })
        .collect();

    let count = results.len();
    json!({
        "memories": results,
        "count": count
    })
}

/// Format memory results as a text block suitable for injection into system instructions.
pub fn format_memory_results_as_text(entries: &[MemoryEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }

    let mut text = String::from("\n\n--- Relevant Memories ---\n");
    for entry in entries {
        let content = extract_text_from_content(&entry.content);
        let timestamp = entry.timestamp.to_rfc3339();
        text.push_str(&format!("[{timestamp}] {}: {content}\n", entry.author));
    }
    text.push_str("--- End Memories ---\n");
    text
}

/// Extract text content from a `Content` value by concatenating all text parts.
fn extract_text_from_content(content: &adk_core::Content) -> String {
    content.parts.iter().filter_map(|part| part.text()).collect::<Vec<_>>().join(" ")
}

/// Validate that a timestamp string is valid ISO 8601 (RFC 3339).
pub fn is_valid_iso8601(timestamp: &str) -> bool {
    DateTime::parse_from_rfc3339(timestamp).is_ok() || timestamp.parse::<DateTime<Utc>>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::Content;

    fn make_entry(content: &str, author: &str) -> MemoryEntry {
        MemoryEntry::new(Content::new("user").with_text(content), author)
    }

    #[test]
    fn test_format_empty_results() {
        let result = format_memory_results(&[]);
        assert_eq!(result["count"], 0);
        assert_eq!(result["memories"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_format_single_result() {
        let entries = vec![make_entry("Hello world", "user")];
        let result = format_memory_results(&entries);

        assert_eq!(result["count"], 1);
        let memories = result["memories"].as_array().unwrap();
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0]["content"], "Hello world");
        assert_eq!(memories[0]["author"], "user");
        assert!(memories[0]["timestamp"].as_str().is_some());
    }

    #[test]
    fn test_format_multiple_results() {
        let entries =
            vec![make_entry("First memory", "user"), make_entry("Second memory", "assistant")];
        let result = format_memory_results(&entries);

        assert_eq!(result["count"], 2);
        let memories = result["memories"].as_array().unwrap();
        assert_eq!(memories[0]["content"], "First memory");
        assert_eq!(memories[1]["content"], "Second memory");
    }

    #[test]
    fn test_timestamp_is_iso8601() {
        let entries = vec![make_entry("test", "user")];
        let result = format_memory_results(&entries);
        let timestamp = result["memories"][0]["timestamp"].as_str().unwrap();
        assert!(is_valid_iso8601(timestamp));
    }

    #[test]
    fn test_format_as_text_empty() {
        let text = format_memory_results_as_text(&[]);
        assert!(text.is_empty());
    }

    #[test]
    fn test_format_as_text_with_entries() {
        let entries = vec![make_entry("Hello", "user")];
        let text = format_memory_results_as_text(&entries);
        assert!(text.contains("Relevant Memories"));
        assert!(text.contains("Hello"));
        assert!(text.contains("user"));
    }
}
