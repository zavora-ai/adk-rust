use crate::error::ReferenceRejection;
use serde_json::Value;

/// Error type for JSON Pointer resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PointerError {
    /// The pointer syntax itself is invalid.
    Syntax,
    /// The pointer is syntactically valid but fails to evaluate to a value.
    Unresolved,
}

pub(crate) fn parse_local_ref(ref_str: &str) -> Result<String, ReferenceRejection> {
    if !ref_str.starts_with('#') {
        return Err(ReferenceRejection::NonLocalReference);
    }
    if ref_str == "#" {
        return Ok(String::new());
    }
    if !ref_str.starts_with("#/") {
        return Err(ReferenceRejection::UnsupportedAnchor);
    }
    percent_decode_pointer(&ref_str[1..])
}

fn percent_decode_pointer(s: &str) -> Result<String, ReferenceRejection> {
    let mut bytes = Vec::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let h1 = chars.next().ok_or(ReferenceRejection::MalformedPointer)?;
            let h2 = chars.next().ok_or(ReferenceRejection::MalformedPointer)?;
            let hex = format!("{}{}", h1, h2);
            let byte =
                u8::from_str_radix(&hex, 16).map_err(|_| ReferenceRejection::MalformedPointer)?;
            bytes.push(byte);
        } else {
            let mut buf = [0; 4];
            bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
    }
    let decoded = String::from_utf8(bytes).map_err(|_| ReferenceRejection::MalformedPointer)?;
    let mut iter = decoded.chars().peekable();
    while let Some(c) = iter.next() {
        if c == '~' {
            match iter.peek() {
                Some('0') | Some('1') => {
                    iter.next();
                }
                _ => return Err(ReferenceRejection::MalformedPointer),
            }
        }
    }
    Ok(decoded)
}

/// Checks if a string segment is a valid RFC 6901 array index (no leading zeros allowed unless exactly "0").
fn is_valid_array_index(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if s == "0" {
        return true;
    }
    let mut chars = s.chars();
    if let Some(first) = chars.next()
        && (!first.is_ascii_digit() || first == '0')
    {
        return false;
    }
    chars.all(|c| c.is_ascii_digit())
}

pub(crate) fn resolve_local_pointer<'a>(
    root: &'a Value,
    pointer_str: &str,
) -> Result<&'a Value, PointerError> {
    if pointer_str.is_empty() {
        return Ok(root);
    }
    if !pointer_str.starts_with('/') {
        return Err(PointerError::Syntax);
    }
    let mut current = root;
    for step in pointer_str[1..].split('/') {
        let unescaped = step.replace("~1", "/").replace("~0", "~");
        match current {
            Value::Object(map) => {
                current = map.get(&unescaped).ok_or(PointerError::Unresolved)?;
            }
            Value::Array(arr) => {
                if !is_valid_array_index(&unescaped) {
                    return Err(PointerError::Unresolved);
                }
                let idx = unescaped.parse::<usize>().map_err(|_| PointerError::Unresolved)?;
                current = arr.get(idx).ok_or(PointerError::Unresolved)?;
            }
            _ => return Err(PointerError::Unresolved),
        }
    }
    Ok(current)
}
