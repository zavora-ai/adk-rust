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
