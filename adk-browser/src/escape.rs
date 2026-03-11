//! String escaping utilities for safe JavaScript interpolation.

/// Escape a string for safe interpolation into JavaScript code.
///
/// This prevents CSS selector injection attacks when user-supplied
/// selectors are interpolated into JavaScript strings. It escapes
/// characters that could break out of a JS string literal or inject
/// malicious code.
///
/// # Example
///
/// ```
/// use adk_browser::escape_js_string;
///
/// let safe = escape_js_string("div[data-id='test']");
/// assert!(safe.contains("\\'"));  // single quotes are escaped
/// ```
pub fn escape_js_string(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len() + 16);
    for ch in input.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '\'' => escaped.push_str("\\'"),
            '"' => escaped.push_str("\\\""),
            '`' => escaped.push_str("\\`"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\0' => escaped.push_str("\\0"),
            '<' => escaped.push_str("\\x3c"),
            '>' => escaped.push_str("\\x3e"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_single_quotes() {
        assert_eq!(escape_js_string("a'b"), "a\\'b");
    }

    #[test]
    fn test_escape_double_quotes() {
        assert_eq!(escape_js_string("a\"b"), "a\\\"b");
    }

    #[test]
    fn test_escape_backslash() {
        assert_eq!(escape_js_string("a\\b"), "a\\\\b");
    }

    #[test]
    fn test_escape_backtick() {
        assert_eq!(escape_js_string("a`b"), "a\\`b");
    }

    #[test]
    fn test_escape_newlines() {
        assert_eq!(escape_js_string("a\nb"), "a\\nb");
        assert_eq!(escape_js_string("a\rb"), "a\\rb");
    }

    #[test]
    fn test_escape_null_byte() {
        assert_eq!(escape_js_string("a\0b"), "a\\0b");
    }

    #[test]
    fn test_escape_script_tags() {
        assert_eq!(escape_js_string("</script>"), "\\x3c/script\\x3e");
    }

    #[test]
    fn test_escape_normal_selector() {
        assert_eq!(escape_js_string("#my-button"), "#my-button");
        assert_eq!(escape_js_string(".nav-link"), ".nav-link");
        assert_eq!(escape_js_string("button[type=submit]"), "button[type=submit]");
    }

    #[test]
    fn test_escape_injection_attempt() {
        let malicious = "'); document.cookie='stolen'; ('";
        let escaped = escape_js_string(malicious);
        // The single quotes are escaped, so the string can't break out of a JS literal
        assert!(escaped.starts_with("\\'"));
        assert!(escaped.contains("\\'"));
    }

    #[test]
    fn test_escape_empty_string() {
        assert_eq!(escape_js_string(""), "");
    }
}
