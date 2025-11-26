use crate::{InvocationContext, Result, AdkError};
use regex::Regex;
use std::sync::OnceLock;

/// Regex pattern to match template placeholders like {variable} or {artifact.file_name}
/// Matches {+[^{}]*}+ to handle nested braces
static PLACEHOLDER_REGEX: OnceLock<Regex> = OnceLock::new();

fn get_placeholder_regex() -> &'static Regex {
    PLACEHOLDER_REGEX.get_or_init(|| {
        Regex::new(r"\{+[^{}]*\}+").expect("Invalid regex pattern")
    })
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
async fn replace_match(ctx: &dyn InvocationContext, match_str: &str) -> Result<String> {
    // Trim curly braces: "{var_name}" -> "var_name"
    let var_name = match_str.trim_matches(|c| c == '{' || c == '}').trim();
    
    // Check if optional (ends with ?)
    let (var_name, optional) = if let Some(name) = var_name.strip_suffix('?') {
        (name, true)
    } else {
        (var_name, false)
    };
    
    // Handle artifact.{name} pattern
    if let Some(file_name) = var_name.strip_prefix("artifact.") {
        let artifacts = ctx.artifacts().ok_or_else(|| {
            AdkError::Agent("Artifact service is not initialized".to_string())
        })?;
        
        match artifacts.load(file_name).await {
            Ok(part) => {
                // Extract text from the part
                if let Some(text) = part.text() {
                    return Ok(text.to_string());
                }
                Ok(String::new())
            }
            Err(e) => {
                if optional {
                    // Optional artifact missing - return empty string
                    Ok(String::new())
                } else {
                    Err(AdkError::Agent(format!("Failed to load artifact {}: {}", file_name, e)))
                }
            }
        }
    } else if is_valid_state_name(var_name) {
        // Handle session state variable
        let state_value = ctx.session().state().get(var_name);
        
        match state_value {
            Some(value) => {
                // Convert value to string
                Ok(format!("{}", value))
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
        Ok(match_str.to_string())
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
    let regex = get_placeholder_regex();
    let mut result = String::with_capacity(template.len());
    let mut last_end = 0;
    
    for captures in regex.find_iter(template) {
        let match_range = captures.range();
        
        // Append text between last match and this one
        result.push_str(&template[last_end..match_range.start]);
        
        // Get the replacement for the current match
        let match_str = captures.as_str();
        let replacement = replace_match(ctx, match_str).await?;
        result.push_str(&replacement);
        
        last_end = match_range.end;
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
}
