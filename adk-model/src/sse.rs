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
