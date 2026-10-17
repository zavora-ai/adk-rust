use adk_core::AdkError;

// Network chunks may split a UTF-8 code point. Decode only complete SSE lines.
pub(crate) fn take_line(buffer: &mut Vec<u8>) -> Result<Option<String>, AdkError> {
    let Some(end) = buffer.iter().position(|byte| *byte == b'\n') else {
        return Ok(None);
    };
    let line = std::str::from_utf8(&buffer[..end])
        .map_err(|_| AdkError::model("invalid UTF-8 in model event stream"))?
        .trim()
        .to_owned();
    buffer.drain(..=end);
    Ok(Some(line))
}

/// Returns the value of a `data` field, with or without the optional space after the
/// colon; `None` for comments and every other field.
#[cfg(any(feature = "openai", feature = "groq"))]
pub(crate) fn data(line: &str) -> Option<&str> {
    let value = line.strip_prefix("data:")?;
    Some(value.strip_prefix(' ').unwrap_or(value))
}

#[cfg(all(test, any(feature = "openai", feature = "groq")))]
mod tests {
    use super::data;

    #[test]
    fn data_accepts_the_field_with_or_without_a_space() {
        assert_eq!(data("data: {\"a\":1}"), Some("{\"a\":1}"));
        assert_eq!(data("data:{\"a\":1}"), Some("{\"a\":1}"));
        assert_eq!(data("data:[DONE]"), Some("[DONE]"));
        assert_eq!(data("data:  padded"), Some(" padded"));
        assert_eq!(data("data:"), Some(""));
    }

    #[test]
    fn data_ignores_comments_and_other_fields() {
        for line in [": keep-alive", "event: error", "id: 7", "retry: 1000", "database: x"] {
            assert_eq!(data(line), None, "{line}");
        }
    }
}
