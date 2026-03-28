//! Variable interpolation utilities for `{{variable}}` template resolution.

use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::LazyLock;

/// Regex matching `{{variable}}` and `{{a.b.c}}` patterns.
static VARIABLE_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\{(\w+(?:\.\w+)*)\}\}").expect("invalid regex"));

/// Interpolates `{{variable}}` patterns in a template string using values from state.
///
/// Dot-notation paths like `{{a.b.c}}` are resolved by traversing nested objects.
/// Variables that cannot be resolved are replaced with an empty string.
///
/// # Examples
///
/// ```
/// use std::collections::HashMap;
/// use serde_json::json;
/// use adk_action::interpolate_variables;
///
/// let mut state = HashMap::new();
/// state.insert("name".to_string(), json!("world"));
///
/// assert_eq!(interpolate_variables("Hello {{name}}!", &state), "Hello world!");
/// ```
pub fn interpolate_variables(template: &str, state: &HashMap<String, Value>) -> String {
    VARIABLE_PATTERN
        .replace_all(template, |caps: &regex::Captures| {
            let path = &caps[1];
            match get_nested_value(state, path) {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Null) => String::new(),
                Some(v) => v.to_string(),
                None => String::new(),
            }
        })
        .into_owned()
}

/// Resolves a dot-notation path against a state map.
///
/// For example, `get_nested_value(state, "a.b.c")` will look up `state["a"]["b"]["c"]`.
///
/// # Examples
///
/// ```
/// use std::collections::HashMap;
/// use serde_json::json;
/// use adk_action::get_nested_value;
///
/// let mut state = HashMap::new();
/// state.insert("user".to_string(), json!({"name": "Alice"}));
///
/// assert_eq!(get_nested_value(&state, "user.name"), Some(&json!("Alice")));
/// assert_eq!(get_nested_value(&state, "missing"), None);
/// ```
pub fn get_nested_value<'a>(state: &'a HashMap<String, Value>, path: &str) -> Option<&'a Value> {
    let mut parts = path.split('.');
    let root_key = parts.next()?;
    let mut current = state.get(root_key)?;

    for part in parts {
        match current {
            Value::Object(map) => {
                current = map.get(part)?;
            }
            _ => return None,
        }
    }

    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_simple_interpolation() {
        let mut state = HashMap::new();
        state.insert("name".to_string(), json!("world"));
        assert_eq!(interpolate_variables("Hello {{name}}!", &state), "Hello world!");
    }

    #[test]
    fn test_nested_interpolation() {
        let mut state = HashMap::new();
        state.insert("user".to_string(), json!({"name": "Alice", "age": 30}));
        assert_eq!(interpolate_variables("Name: {{user.name}}", &state), "Name: Alice");
        assert_eq!(interpolate_variables("Age: {{user.age}}", &state), "Age: 30");
    }

    #[test]
    fn test_missing_variable_becomes_empty() {
        let state = HashMap::new();
        assert_eq!(interpolate_variables("Hello {{missing}}!", &state), "Hello !");
    }

    #[test]
    fn test_no_variables_unchanged() {
        let state = HashMap::new();
        assert_eq!(interpolate_variables("no variables here", &state), "no variables here");
    }

    #[test]
    fn test_multiple_variables() {
        let mut state = HashMap::new();
        state.insert("a".to_string(), json!("X"));
        state.insert("b".to_string(), json!("Y"));
        assert_eq!(interpolate_variables("{{a}} and {{b}}", &state), "X and Y");
    }

    #[test]
    fn test_numeric_value() {
        let mut state = HashMap::new();
        state.insert("count".to_string(), json!(42));
        assert_eq!(interpolate_variables("Count: {{count}}", &state), "Count: 42");
    }

    #[test]
    fn test_null_value_becomes_empty() {
        let mut state = HashMap::new();
        state.insert("val".to_string(), json!(null));
        assert_eq!(interpolate_variables("{{val}}", &state), "");
    }

    #[test]
    fn test_deeply_nested() {
        let mut state = HashMap::new();
        state.insert("a".to_string(), json!({"b": {"c": {"d": "deep"}}}));
        assert_eq!(get_nested_value(&state, "a.b.c.d"), Some(&json!("deep")));
    }

    #[test]
    fn test_nested_non_object_returns_none() {
        let mut state = HashMap::new();
        state.insert("a".to_string(), json!("string"));
        assert_eq!(get_nested_value(&state, "a.b"), None);
    }
}
