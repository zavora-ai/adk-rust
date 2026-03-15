//! Shared text extraction utilities for memory backends.

use adk_core::Part;
use std::collections::HashSet;

/// Extract all text parts from a [`Content`](adk_core::Content) into a single string.
///
/// Parts are joined with a single space. Non-text parts (images, function calls,
/// etc.) are silently skipped.
pub fn extract_text(content: &adk_core::Content) -> String {
    content
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Tokenize text into a set of lowercase words for keyword matching.
pub fn extract_words(text: &str) -> HashSet<String> {
    text.split_whitespace().filter(|s| !s.is_empty()).map(|s| s.to_lowercase()).collect()
}

/// Extract and tokenize all text from a [`Content`](adk_core::Content) into word set.
pub fn extract_words_from_content(content: &adk_core::Content) -> HashSet<String> {
    let mut words = HashSet::new();
    for part in &content.parts {
        if let Part::Text { text } = part {
            words.extend(extract_words(text));
        }
    }
    words
}
