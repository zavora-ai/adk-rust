//! Encoding for identifiers joined into delimited storage keys.
//!
//! Backends that build one string key out of several identifiers (Redis keys,
//! Neo4j entry ids) join the segments with `:`. Identity validation allows `:`
//! inside an identifier, so the segments are percent-encoded first: app `a:b`
//! with user `c` and app `a` with user `b:c` then produce different keys.

use std::borrow::Cow;

/// Percent-encodes `%` and `:` so an encoded segment never contains the `:` delimiter.
///
/// Identifiers containing neither character encode to themselves, so keys
/// written before the encoding existed stay addressable.
pub(crate) fn encode_segment(segment: &str) -> Cow<'_, str> {
    if !segment.contains([':', '%']) {
        return Cow::Borrowed(segment);
    }
    let mut encoded = String::with_capacity(segment.len() + 8);
    for c in segment.chars() {
        match c {
            '%' => encoded.push_str("%25"),
            ':' => encoded.push_str("%3A"),
            other => encoded.push(other),
        }
    }
    Cow::Owned(encoded)
}

/// Escapes the Redis glob metacharacters `*`, `?`, `[`, `]` and `\` with a backslash.
///
/// The result matches only the literal text when embedded in a `SCAN MATCH` or
/// `KEYS` pattern.
#[cfg(feature = "redis-memory")]
pub(crate) fn escape_glob(literal: &str) -> Cow<'_, str> {
    if !literal.contains(['*', '?', '[', ']', '\\']) {
        return Cow::Borrowed(literal);
    }
    let mut escaped = String::with_capacity(literal.len() + 8);
    for c in literal.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    Cow::Owned(escaped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_segments_are_unchanged() {
        assert!(matches!(encode_segment("alice"), Cow::Borrowed("alice")));
    }

    #[test]
    fn delimiter_and_escape_characters_are_encoded() {
        assert_eq!(encode_segment("a:b"), "a%3Ab");
        assert_eq!(encode_segment("100%"), "100%25");
        // A literal `%3A` must not collide with an encoded `:`.
        assert_ne!(encode_segment("a%3Ab"), encode_segment("a:b"));
    }

    #[test]
    fn encoded_segments_never_contain_the_delimiter() {
        for raw in ["a:b", "::", "%3A:%25", "x"] {
            assert!(!encode_segment(raw).contains(':'), "{raw} leaked a delimiter");
        }
    }

    #[cfg(feature = "redis-memory")]
    #[test]
    fn glob_metacharacters_are_escaped() {
        assert!(matches!(escape_glob("alice"), Cow::Borrowed("alice")));
        assert_eq!(escape_glob("*"), "\\*");
        assert_eq!(escape_glob(r"a?b[c]d\e"), r"a\?b\[c\]d\\e");
    }
}
