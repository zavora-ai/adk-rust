use crate::{AdkError, InvocationContext, Result};

/// Checks if a character is valid as the first character of a placeholder identifier.
fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

/// Checks if a character is valid inside a placeholder identifier body.
fn is_ident_body(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == ':' || c == '.'
}

/// Finds the next placeholder `{...}` in `template` starting from byte offset `from`.
/// Returns `Some((start, end, content))` where start/end are byte offsets of the
/// outer braces and content is the inner string (without braces).
/// Returns `None` when no more placeholders exist.
fn find_next_placeholder(template: &str, from: usize) -> Option<(usize, usize, &str)> {
    let bytes = template.as_bytes();
    let len = bytes.len();
    let mut i = from;

    while i < len {
        if bytes[i] == b'{' {
            let content_start = i + 1;
            if content_start >= len {
                break;
            }
            // First char must be a valid identifier start
            if !is_ident_start(bytes[content_start] as char) {
                i += 1;
                continue;
            }
            // Scan the body
            let mut j = content_start + 1;
            while j < len && is_ident_body(bytes[j] as char) {
                j += 1;
            }
            // Optional trailing '?'
            if j < len && bytes[j] == b'?' {
                j += 1;
            }
            // Must close with '}'
            if j < len && bytes[j] == b'}' {
                let content = &template[content_start..j];
                return Some((i, j + 1, content));
            }
        }
        i += 1;
    }
    None
}

/// Checks if a string is a valid identifier (like Python's str.isidentifier())
/// Must start with letter or underscore, followed by letters, digits, or underscores
fn is_identifier(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }

    let mut chars = s.chars();
    let first = chars.next().unwrap();

    if !first.is_alphabetic() && first != '_' {
        return false;
    }

    chars.all(|c| c.is_alphanumeric() || c == '_')
}

/// Checks if a variable name is a valid state name
/// Supports prefixes: app:, user:, temp:
fn is_valid_state_name(var_name: &str) -> bool {
    let parts: Vec<&str> = var_name.split(':').collect();

    match parts.len() {
        1 => is_identifier(var_name),
        2 => {
            let prefix = format!("{}:", parts[0]);
            let valid_prefixes = ["app:", "user:", "temp:"];
            valid_prefixes.contains(&prefix.as_str()) && is_identifier(parts[1])
        }
        _ => false,
    }
}

/// Replaces a single placeholder match with its resolved value
/// Handles {var}, {var?}, and {artifact.name} syntax
async fn replace_match(ctx: &dyn InvocationContext, content: &str) -> Result<String> {
    let var_name = content.trim();

    // Check if optional (ends with ?)
    let (var_name, optional) =
        if let Some(name) = var_name.strip_suffix('?') { (name, true) } else { (var_name, false) };

    // Handle artifact.{name} pattern
    if let Some(file_name) = var_name.strip_prefix("artifact.") {
        if file_name.is_empty() {
            return Err(AdkError::Agent(
                "Invalid artifact name '': must include a file name after 'artifact.'".to_string(),
            ));
        }

        // Reject path traversal attempts in artifact names
        if file_name.contains("..") || file_name.contains('/') || file_name.contains('\\') {
            return Err(AdkError::Agent(format!(
                "Invalid artifact name '{}': must not contain path separators or '..'",
                file_name
            )));
        }

        let artifacts = ctx
            .artifacts()
            .ok_or_else(|| AdkError::Agent("Artifact service is not initialized".to_string()))?;

        match artifacts.load(file_name).await {
            Ok(part) => {
                if let Some(text) = part.text() {
                    return Ok(text.to_string());
                }
                Ok(String::new())
            }
            Err(e) => {
                if optional {
                    Ok(String::new())
                } else {
                    Err(AdkError::Agent(format!("Failed to load artifact {}: {}", file_name, e)))
                }
            }
        }
    } else if is_valid_state_name(var_name) {
        let state_value = ctx.session().state().get(var_name);

        match state_value {
            Some(value) => {
                if let Some(s) = value.as_str() {
                    Ok(s.to_string())
                } else {
                    Ok(format!("{}", value))
                }
            }
            None => {
                if optional {
                    Ok(String::new())
                } else {
                    Err(AdkError::Agent(format!("State variable '{}' not found", var_name)))
                }
            }
        }
    } else {
        // Not a valid variable name - return original match as literal
        Ok(format!("{{{}}}", content))
    }
}

/// Injects session state and artifact values into an instruction template
///
/// Supports the following placeholder syntax:
/// - `{var_name}` - Required session state variable (errors if missing)
/// - `{var_name?}` - Optional variable (empty string if missing)
/// - `{artifact.file_name}` - Artifact content insertion
/// - `{app:var}`, `{user:var}`, `{temp:var}` - Prefixed state variables
///
/// # Examples
///
/// ```ignore
/// let template = "Hello {user_name}, your score is {score}";
/// let result = inject_session_state(ctx, template).await?;
/// // Result: "Hello Alice, your score is 100"
/// ```
///
/// # Errors
///
/// Returns an error if:
/// - A required variable is not found in session state
/// - A required artifact cannot be loaded
/// - The artifact service is not initialized
pub async fn inject_session_state(ctx: &dyn InvocationContext, template: &str) -> Result<String> {
    // Pre-allocate 20% extra capacity to reduce reallocations when placeholders expand
    let mut result = String::with_capacity((template.len() as f32 * 1.2) as usize);
    let mut last_end = 0;

    while let Some((start, end, content)) = find_next_placeholder(template, last_end) {
        // Append text between last match and this one
        result.push_str(&template[last_end..start]);

        // Get the replacement for the current match
        let replacement = replace_match(ctx, content).await?;
        result.push_str(&replacement);

        last_end = end;
    }

    // Append any remaining text
    result.push_str(&template[last_end..]);

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_identifier() {
        assert!(is_identifier("valid_name"));
        assert!(is_identifier("_private"));
        assert!(is_identifier("name123"));
        assert!(!is_identifier("123invalid"));
        assert!(!is_identifier(""));
        assert!(!is_identifier("with-dash"));
    }

    #[test]
    fn test_is_valid_state_name() {
        assert!(is_valid_state_name("valid_var"));
        assert!(is_valid_state_name("app:config"));
        assert!(is_valid_state_name("user:preference"));
        assert!(is_valid_state_name("temp:data"));
        assert!(!is_valid_state_name("invalid:prefix"));
        assert!(!is_valid_state_name("app:invalid-name"));
        assert!(!is_valid_state_name("too:many:parts"));
    }

    #[test]
    fn test_find_placeholder_basic() {
        let t = "Hello {name}, welcome!";
        let (s, e, c) = find_next_placeholder(t, 0).unwrap();
        assert_eq!(c, "name");
        assert_eq!(&t[s..e], "{name}");
    }

    #[test]
    fn test_find_placeholder_optional() {
        let t = "Hello {name?}!";
        let (_, _, c) = find_next_placeholder(t, 0).unwrap();
        assert_eq!(c, "name?");
    }

    #[test]
    fn test_find_placeholder_prefixed() {
        let t = "Value: {app:config}";
        let (_, _, c) = find_next_placeholder(t, 0).unwrap();
        assert_eq!(c, "app:config");
    }

    #[test]
    fn test_find_placeholder_artifact() {
        let t = "Content: {artifact.readme}";
        let (_, _, c) = find_next_placeholder(t, 0).unwrap();
        assert_eq!(c, "artifact.readme");
    }

    #[test]
    fn test_find_placeholder_skips_invalid() {
        // {123} should not match (starts with digit)
        assert!(find_next_placeholder("{123}", 0).is_none());
        // Empty braces
        assert!(find_next_placeholder("{}", 0).is_none());
        // JSON-like content
        assert!(find_next_placeholder("{\"key\": \"value\"}", 0).is_none());
    }

    #[test]
    fn test_find_placeholder_multiple() {
        let t = "{a} and {b}";
        let (_, e1, c1) = find_next_placeholder(t, 0).unwrap();
        assert_eq!(c1, "a");
        let (_, _, c2) = find_next_placeholder(t, e1).unwrap();
        assert_eq!(c2, "b");
    }
}
