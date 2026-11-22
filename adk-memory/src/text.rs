//! Shared text extraction utilities for memory backends.
//!
//! A memory's searchable text is its text parts alone ([`extract_text`]). The artifacts it
//! refers to — an image a caption describes, a document a note cites — are kept beside the
//! text as [`ArtifactReference`]s ([`artifact_references`]): a URI and a media type, never
//! the bytes.

use adk_core::Part;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// An artifact a memory refers to: its URI and media type, never its bytes.
///
/// A memory carries one as a `Part::FileData` beside its text
/// ([`MemoryEntry::with_artifact`](crate::MemoryEntry::with_artifact)). Every backend that
/// stores the entry's content keeps the part; a backend that stores text alone, such as
/// the knowledge graph's episodic log, keeps the references in a column of their own and
/// returns them as `Part::FileData` again.
///
/// # Example
///
/// ```rust
/// use adk_core::Content;
/// use adk_memory::text::{ArtifactReference, artifact_references, extract_text};
///
/// let reference = ArtifactReference::new("artifact://inspector/u1/s1/frame.png#0", "image/png");
/// let mut content = Content::new("model").with_text("torn seal on carton 117");
/// content.parts.push(reference.to_part());
///
/// assert_eq!(extract_text(&content), "torn seal on carton 117");
/// assert_eq!(artifact_references(&content), vec![reference]);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactReference {
    /// Where the artifact is: an `artifact://` locator, a URL, or a cloud storage path.
    pub uri: String,
    /// Its media type, such as `image/png`.
    pub mime_type: String,
}

impl ArtifactReference {
    /// A reference to the artifact at `uri` of media type `mime_type`.
    pub fn new(uri: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self { uri: uri.into(), mime_type: mime_type.into() }
    }

    /// The `Part::FileData` that carries this reference in a [`Content`](adk_core::Content).
    pub fn to_part(&self) -> Part {
        Part::FileData {
            mime_type: self.mime_type.clone(),
            file_uri: self.uri.clone(),
            annotations: None,
        }
    }
}

/// The artifacts a [`Content`](adk_core::Content) refers to, in part order: every
/// `Part::FileData`, and every `Part::InlineData` or `Part::EmbeddedResource` that names its
/// source URI. Inline bytes are never part of a reference; inline data without a `uri`
/// refers to nothing.
pub fn artifact_references(content: &adk_core::Content) -> Vec<ArtifactReference> {
    content
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::FileData { mime_type, file_uri, .. } => {
                Some(ArtifactReference::new(file_uri, mime_type))
            }
            Part::InlineData { mime_type, uri: Some(uri), .. } => {
                Some(ArtifactReference::new(uri, mime_type))
            }
            Part::EmbeddedResource { resource } => Some(ArtifactReference::new(
                resource.uri(),
                resource.mime_type().unwrap_or("application/octet-stream"),
            )),
            Part::InlineData { uri: None, .. }
            | Part::Thinking { .. }
            | Part::Text { .. }
            | Part::FunctionCall { .. }
            | Part::FunctionResponse { .. }
            | Part::ServerToolCall { .. }
            | Part::ServerToolResponse { .. } => None,
        })
        .collect()
}

/// Extract all text parts from a [`Content`](adk_core::Content) into a single string.
///
/// Parts are joined with a single space. Non-text parts (images, function calls,
/// etc.) are skipped; [`artifact_references`] returns the artifacts among them.
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

/// Returns `true` if the character is in a CJK Unified Ideographs block.
fn is_cjk_char(c: char) -> bool {
    matches!(c,
        '\u{4e00}'..='\u{9fff}'   // CJK Unified Ideographs
        | '\u{3400}'..='\u{4dbf}' // CJK Unified Ideographs Extension A
        | '\u{f900}'..='\u{faff}' // CJK Compatibility Ideographs
        | '\u{2e80}'..='\u{2eff}' // CJK Radicals Supplement
        | '\u{3000}'..='\u{303f}' // CJK Symbols and Punctuation
        | '\u{3040}'..='\u{309f}' // Hiragana
        | '\u{30a0}'..='\u{30ff}' // Katakana
        | '\u{ac00}'..='\u{d7af}' // Hangul Syllables
    )
}

/// Tokenize text into a set of lowercase words for keyword matching.
///
/// A word is a maximal run of Unicode letters and digits ([`char::is_alphanumeric`]), so
/// punctuation never sticks to a word: `"Nairobi."`, `"(Nairobi)"` and `"Nairobi"` all
/// yield `nairobi`, in every script.
///
/// CJK text (Chinese, Japanese, Korean) has no spaces between words, so each word that
/// contains CJK characters also yields its character unigrams and bigrams, which lets a
/// query match part of a longer run of CJK text.
pub fn extract_words(text: &str) -> HashSet<String> {
    let mut words = HashSet::new();
    let lower = text.to_lowercase();

    for word in lower.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty()) {
        if word.chars().any(is_cjk_char) {
            // "编程" matches within "用户喜欢用rust编程".
            let chars: Vec<char> = word.chars().collect();
            for c in &chars {
                if is_cjk_char(*c) {
                    words.insert(c.to_string());
                }
            }
            for window in chars.windows(2) {
                if window.iter().any(|c| is_cjk_char(*c)) {
                    words.insert(window.iter().collect());
                }
            }
        }
        words.insert(word.to_string());
    }

    words
}

/// The share of `query_words` that `entry_words` contains, from 0.0 (none) to 1.0 (all).
pub(crate) fn match_score(query_words: &HashSet<String>, entry_words: &HashSet<String>) -> f32 {
    if query_words.is_empty() {
        return 0.0;
    }
    let matched = query_words.iter().filter(|word| entry_words.contains(*word)).count();
    matched as f32 / query_words.len() as f32
}

/// Orders keyword matches best first: the higher score, then the newer entry.
pub(crate) fn rank_matches(
    mut matches: Vec<(f32, crate::service::MemoryEntry)>,
    limit: usize,
) -> Vec<crate::service::MemoryEntry> {
    matches.sort_by(|(left_score, left), (right_score, right)| {
        right_score.total_cmp(left_score).then_with(|| right.timestamp.cmp(&left.timestamp))
    });
    matches.into_iter().take(limit).map(|(_, entry)| entry).collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_references_keep_uris_and_never_bytes() {
        let content = adk_core::Content {
            role: "model".into(),
            parts: vec![
                Part::Text { text: "torn seal".into() },
                Part::FileData {
                    mime_type: "image/png".into(),
                    file_uri: "artifact://a/u/s/f.png#0".into(),
                    annotations: None,
                },
                Part::InlineData {
                    mime_type: "image/jpeg".into(),
                    data: vec![0xff, 0xd8],
                    uri: Some("artifact://a/u/s/g.jpg#1".into()),
                    annotations: None,
                },
                Part::InlineData {
                    mime_type: "image/jpeg".into(),
                    data: vec![0xff, 0xd8],
                    uri: None,
                    annotations: None,
                },
            ],
        };
        assert_eq!(extract_text(&content), "torn seal");
        assert_eq!(
            artifact_references(&content),
            vec![
                ArtifactReference::new("artifact://a/u/s/f.png#0", "image/png"),
                ArtifactReference::new("artifact://a/u/s/g.jpg#1", "image/jpeg"),
            ]
        );
        let reference = ArtifactReference::new("artifact://a/u/s/f.png#0", "image/png");
        assert_eq!(
            serde_json::to_value(&reference).unwrap(),
            serde_json::json!({ "uri": "artifact://a/u/s/f.png#0", "mimeType": "image/png" })
        );
    }

    #[test]
    fn test_extract_words_english() {
        let words = extract_words("Hello World foo bar");
        assert!(words.contains("hello"));
        assert!(words.contains("world"));
        assert!(words.contains("foo"));
        assert!(words.contains("bar"));
    }

    #[test]
    fn test_extract_words_cjk_bigram_matching() {
        // "用户喜欢用Rust编程" should produce bigrams that include "编程"
        let stored = extract_words("用户喜欢用Rust编程");
        let query = extract_words("编程");

        // The query "编程" should match because it's a bigram in the stored text
        let matches: HashSet<_> = stored.intersection(&query).collect();
        assert!(
            !matches.is_empty(),
            "CJK search should find matches. Stored: {stored:?}, Query: {query:?}"
        );
    }

    #[test]
    fn test_extract_words_cjk_single_char() {
        let stored = extract_words("今天天气很好");
        let query = extract_words("天气");

        let matches: HashSet<_> = stored.intersection(&query).collect();
        assert!(
            !matches.is_empty(),
            "CJK bigram '天气' should match. Stored: {stored:?}, Query: {query:?}"
        );
    }

    #[test]
    fn test_extract_words_mixed_cjk_english() {
        let words = extract_words("Hello 你好 World");
        assert!(words.contains("hello"));
        assert!(words.contains("world"));
        assert!(words.contains("你"));
        assert!(words.contains("好"));
        assert!(words.contains("你好"));
    }

    #[test]
    fn test_extract_words_ignore_punctuation() {
        let words = extract_words("Hometown: Nairobi. (Kenya!) state-of-the-art, \"quoted\"");
        for word in ["hometown", "nairobi", "kenya", "state", "art", "quoted"] {
            assert!(words.contains(word), "{word} missing from {words:?}");
        }
        assert!(!words.iter().any(|word| word.contains(|c: char| !c.is_alphanumeric())));
        assert!(extract_words("Ünïcödé: café, naïve!").contains("café"));
        assert!(extract_words("Привет, мир!").contains("мир"));
        assert!(extract_words("!!! ... ---").is_empty());
    }

    #[test]
    fn test_extract_words_cjk_punctuation_is_not_a_gram() {
        let words = extract_words("你好，世界。");
        assert!(words.contains("你好") && words.contains("世界"));
        assert!(!words.iter().any(|word| word.contains(['，', '。'])));
    }

    #[test]
    fn test_match_score_counts_matched_query_words() {
        let query = extract_words("rust ownership borrowing");
        assert_eq!(match_score(&query, &extract_words("Rust ownership and borrowing.")), 1.0);
        assert!((match_score(&query, &extract_words("Rust note.")) - 1.0 / 3.0).abs() < 1e-6);
        assert_eq!(match_score(&query, &extract_words("unrelated")), 0.0);
        assert_eq!(match_score(&HashSet::new(), &extract_words("rust")), 0.0);
    }

    #[test]
    fn test_extract_words_japanese() {
        let stored = extract_words("東京タワー");
        let query = extract_words("東京");

        let matches: HashSet<_> = stored.intersection(&query).collect();
        assert!(!matches.is_empty(), "Japanese bigram should match");
    }
}
