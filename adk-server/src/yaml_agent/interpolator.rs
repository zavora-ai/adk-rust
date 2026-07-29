//! Environment variable interpolation for YAML agent definitions.
//!
//! Resolves `${VAR}` and `${VAR:-default}` placeholders in all string values
//! of a [`YamlAgentDefinition`]. Variables are resolved from the process
//! environment via [`std::env::var`].
//!
//! # Syntax
//!
//! - `${VARIABLE_NAME}` — replaced with the environment variable value; error if unset
//! - `${VARIABLE_NAME:-default_value}` — replaced with the env var value, or `default_value` if unset
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_server::yaml_agent::interpolator::EnvInterpolator;
//!
//! let result = EnvInterpolator::interpolate_str("Hello ${USER:-world}");
//! assert!(result.is_ok());
//! ```

use std::fmt;

use regex::Regex;
use std::sync::LazyLock;

use super::schema::{McpToolReference, ToolReference, YamlAgentDefinition};

/// Regex pattern for environment variable placeholders.
///
/// Matches `${VAR_NAME}` and `${VAR_NAME:-default_value}` where `VAR_NAME`
/// starts with a letter or underscore followed by alphanumeric characters or underscores.
static ENV_VAR_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)(?::-((?:[^}])*))?\}").unwrap());

/// An error indicating an unresolved environment variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterpolationError {
    /// The name of the unresolved environment variable.
    pub variable_name: String,
    /// The field path where the variable was referenced.
    pub field_path: String,
}

impl fmt::Display for InterpolationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unresolved environment variable '{}' in field '{}'",
            self.variable_name, self.field_path
        )
    }
}

impl std::error::Error for InterpolationError {}

/// Resolves `${VAR}` and `${VAR:-default}` placeholders in YAML agent definitions.
///
/// The interpolator processes all string fields recursively, collecting errors
/// for all unresolved variables rather than stopping at the first.
pub struct EnvInterpolator;

impl EnvInterpolator {
    /// Interpolate all string fields in a [`YamlAgentDefinition`].
    ///
    /// Processes name, description, instructions, model fields, tools,
    /// sub-agents, plugins, session, memory, and config values.
    ///
    /// # Errors
    ///
    /// Returns all unresolved variable errors at once (multi-error).
    pub fn interpolate(def: &mut YamlAgentDefinition) -> Result<(), Vec<InterpolationError>> {
        let mut errors = Vec::new();

        // Interpolate top-level string fields
        interpolate_field(&mut def.name, "name", &mut errors);
        interpolate_option_field(&mut def.description, "description", &mut errors);
        interpolate_option_field(&mut def.instructions, "instructions", &mut errors);

        // Interpolate model config
        interpolate_field(&mut def.model.provider, "model.provider", &mut errors);
        interpolate_field(&mut def.model.model_id, "model.model_id", &mut errors);

        // Interpolate tools
        for (i, tool) in def.tools.iter_mut().enumerate() {
            let prefix = format!("tools[{i}]");
            match tool {
                ToolReference::Named { name } => {
                    interpolate_field(name, &format!("{prefix}.name"), &mut errors);
                }
                ToolReference::Mcp { mcp } => {
                    interpolate_mcp_ref(mcp, &prefix, &mut errors);
                }
            }
        }

        // Interpolate sub-agent references
        for (i, sub) in def.sub_agents.iter_mut().enumerate() {
            interpolate_field(&mut sub.reference, &format!("sub_agents[{i}].ref"), &mut errors);
        }

        // Interpolate config map values
        for (key, value) in def.config.iter_mut() {
            interpolate_json_value(value, &format!("config.{key}"), &mut errors);
        }

        // Interpolate plugins
        for (i, plugin) in def.plugins.iter_mut().enumerate() {
            let prefix = format!("plugins[{i}]");
            interpolate_field(&mut plugin.name, &format!("{prefix}.name"), &mut errors);
            if let Some(config) = &mut plugin.config {
                interpolate_json_value(config, &format!("{prefix}.config"), &mut errors);
            }
        }

        // Interpolate session config
        if let Some(session) = &mut def.session {
            interpolate_field(&mut session.backend, "session.backend", &mut errors);
            for (key, value) in session.options.iter_mut() {
                interpolate_json_value(value, &format!("session.{key}"), &mut errors);
            }
        }

        // Interpolate memory config
        if let Some(memory) = &mut def.memory {
            interpolate_field(&mut memory.backend, "memory.backend", &mut errors);
            for (key, value) in memory.options.iter_mut() {
                interpolate_json_value(value, &format!("memory.{key}"), &mut errors);
            }
        }

        if errors.is_empty() { Ok(()) } else { Err(errors) }
    }

    /// Interpolate a single string value.
    ///
    /// Resolves all `${VAR}` and `${VAR:-default}` placeholders in the input.
    ///
    /// # Errors
    ///
    /// Returns errors for all unresolved variables (without defaults) found in the string.
    pub fn interpolate_str(input: &str) -> Result<String, Vec<InterpolationError>> {
        let mut errors = Vec::new();
        let result = resolve_placeholders(input, "", &mut errors);
        if errors.is_empty() { Ok(result) } else { Err(errors) }
    }
}

/// Resolve all placeholders in a string, collecting errors for unresolved variables.
fn resolve_placeholders(
    input: &str,
    field_path: &str,
    errors: &mut Vec<InterpolationError>,
) -> String {
    let mut result = String::with_capacity(input.len());
    let mut last_end = 0;

    for caps in ENV_VAR_PATTERN.captures_iter(input) {
        let full_match = caps.get(0).unwrap();
        result.push_str(&input[last_end..full_match.start()]);

        let var_name = caps.get(1).unwrap().as_str();
        let default_value = caps.get(2).map(|m| m.as_str());

        match std::env::var(var_name) {
            Ok(value) => {
                result.push_str(&value);
            }
            Err(_) => {
                if let Some(default) = default_value {
                    result.push_str(default);
                } else {
                    errors.push(InterpolationError {
                        variable_name: var_name.to_string(),
                        field_path: field_path.to_string(),
                    });
                    // Keep the original placeholder in the output for debugging
                    result.push_str(full_match.as_str());
                }
            }
        }

        last_end = full_match.end();
    }

    result.push_str(&input[last_end..]);
    result
}

/// Interpolate a required string field.
fn interpolate_field(field: &mut String, path: &str, errors: &mut Vec<InterpolationError>) {
    if field.contains("${") {
        *field = resolve_placeholders(field, path, errors);
    }
}

/// Interpolate an optional string field.
fn interpolate_option_field(
    field: &mut Option<String>,
    path: &str,
    errors: &mut Vec<InterpolationError>,
) {
    if let Some(value) = field
        && value.contains("${")
    {
        *value = resolve_placeholders(value, path, errors);
    }
}

/// Interpolate an MCP tool reference.
fn interpolate_mcp_ref(
    mcp: &mut McpToolReference,
    prefix: &str,
    errors: &mut Vec<InterpolationError>,
) {
    interpolate_field(&mut mcp.endpoint, &format!("{prefix}.mcp.endpoint"), errors);
    for (j, arg) in mcp.args.iter_mut().enumerate() {
        interpolate_field(arg, &format!("{prefix}.mcp.args[{j}]"), errors);
    }
}

/// Recursively interpolate string values within a JSON value.
fn interpolate_json_value(
    value: &mut serde_json::Value,
    path: &str,
    errors: &mut Vec<InterpolationError>,
) {
    match value {
        serde_json::Value::String(s) => {
            if s.contains("${") {
                *s = resolve_placeholders(s, path, errors);
            }
        }
        serde_json::Value::Object(map) => {
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                if let Some(v) = map.get_mut(&key) {
                    interpolate_json_value(v, &format!("{path}.{key}"), errors);
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for (i, item) in arr.iter_mut().enumerate() {
                interpolate_json_value(item, &format!("{path}[{i}]"), errors);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::yaml_agent::schema::{MemoryConfig, ModelConfig, SessionConfig};
    use std::collections::HashMap;

    #[test]
    fn test_interpolate_str_simple_var() {
        // SAFETY: test-only, single-threaded test execution
        unsafe { std::env::set_var("TEST_INTERP_VAR", "hello") };
        let result = EnvInterpolator::interpolate_str("${TEST_INTERP_VAR}").unwrap();
        assert_eq!(result, "hello");
        unsafe { std::env::remove_var("TEST_INTERP_VAR") };
    }

    #[test]
    fn test_interpolate_str_with_default() {
        unsafe { std::env::remove_var("TEST_INTERP_UNSET_VAR") };
        let result =
            EnvInterpolator::interpolate_str("${TEST_INTERP_UNSET_VAR:-fallback}").unwrap();
        assert_eq!(result, "fallback");
    }

    #[test]
    fn test_interpolate_str_var_set_ignores_default() {
        unsafe { std::env::set_var("TEST_INTERP_SET_VAR", "actual") };
        let result = EnvInterpolator::interpolate_str("${TEST_INTERP_SET_VAR:-fallback}").unwrap();
        assert_eq!(result, "actual");
        unsafe { std::env::remove_var("TEST_INTERP_SET_VAR") };
    }

    #[test]
    fn test_interpolate_str_unset_no_default_errors() {
        unsafe { std::env::remove_var("TEST_INTERP_MISSING_VAR") };
        let err = EnvInterpolator::interpolate_str("${TEST_INTERP_MISSING_VAR}").unwrap_err();
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].variable_name, "TEST_INTERP_MISSING_VAR");
    }

    #[test]
    fn test_interpolate_str_multiple_vars() {
        unsafe { std::env::set_var("TEST_INTERP_A", "foo") };
        unsafe { std::env::set_var("TEST_INTERP_B", "bar") };
        let result = EnvInterpolator::interpolate_str("${TEST_INTERP_A}-${TEST_INTERP_B}").unwrap();
        assert_eq!(result, "foo-bar");
        unsafe { std::env::remove_var("TEST_INTERP_A") };
        unsafe { std::env::remove_var("TEST_INTERP_B") };
    }

    #[test]
    fn test_interpolate_str_no_placeholders() {
        let result = EnvInterpolator::interpolate_str("no placeholders here").unwrap();
        assert_eq!(result, "no placeholders here");
    }

    #[test]
    fn test_interpolate_str_empty_default() {
        unsafe { std::env::remove_var("TEST_INTERP_EMPTY_DEFAULT") };
        let result =
            EnvInterpolator::interpolate_str("prefix-${TEST_INTERP_EMPTY_DEFAULT:-}-suffix")
                .unwrap();
        assert_eq!(result, "prefix--suffix");
    }

    #[test]
    fn test_interpolate_str_multiple_errors() {
        unsafe { std::env::remove_var("TEST_INTERP_MISS_1") };
        unsafe { std::env::remove_var("TEST_INTERP_MISS_2") };
        let err =
            EnvInterpolator::interpolate_str("${TEST_INTERP_MISS_1} and ${TEST_INTERP_MISS_2}")
                .unwrap_err();
        assert_eq!(err.len(), 2);
        assert_eq!(err[0].variable_name, "TEST_INTERP_MISS_1");
        assert_eq!(err[1].variable_name, "TEST_INTERP_MISS_2");
    }

    #[test]
    fn test_interpolate_definition() {
        unsafe { std::env::set_var("TEST_INTERP_PROVIDER", "gemini") };
        unsafe { std::env::set_var("TEST_INTERP_MODEL", "gemini-2.5-flash") };

        let mut def = YamlAgentDefinition {
            name: "test_agent".to_string(),
            description: Some("Uses ${TEST_INTERP_PROVIDER}".to_string()),
            model: ModelConfig {
                provider: "${TEST_INTERP_PROVIDER}".to_string(),
                model_id: "${TEST_INTERP_MODEL}".to_string(),
                temperature: None,
                max_tokens: None,
            },
            instructions: None,
            tools: vec![],
            sub_agents: vec![],
            config: HashMap::new(),
            metadata: HashMap::new(),
            plugins: vec![],
            session: None,
            memory: None,
        };

        EnvInterpolator::interpolate(&mut def).unwrap();
        assert_eq!(def.model.provider, "gemini");
        assert_eq!(def.model.model_id, "gemini-2.5-flash");
        assert_eq!(def.description.as_deref(), Some("Uses gemini"));

        unsafe { std::env::remove_var("TEST_INTERP_PROVIDER") };
        unsafe { std::env::remove_var("TEST_INTERP_MODEL") };
    }

    #[test]
    fn test_interpolate_definition_collects_all_errors() {
        unsafe { std::env::remove_var("TEST_INTERP_ERR_A") };
        unsafe { std::env::remove_var("TEST_INTERP_ERR_B") };

        let mut def = YamlAgentDefinition {
            name: "${TEST_INTERP_ERR_A}".to_string(),
            description: None,
            model: ModelConfig {
                provider: "${TEST_INTERP_ERR_B}".to_string(),
                model_id: "fixed".to_string(),
                temperature: None,
                max_tokens: None,
            },
            instructions: None,
            tools: vec![],
            sub_agents: vec![],
            config: HashMap::new(),
            metadata: HashMap::new(),
            plugins: vec![],
            session: None,
            memory: None,
        };

        let errors = EnvInterpolator::interpolate(&mut def).unwrap_err();
        assert_eq!(errors.len(), 2);
        assert!(errors.iter().any(|e| e.variable_name == "TEST_INTERP_ERR_A"));
        assert!(errors.iter().any(|e| e.variable_name == "TEST_INTERP_ERR_B"));
    }

    #[test]
    fn test_interpolate_session_and_memory() {
        unsafe { std::env::set_var("TEST_INTERP_DB_URL", "postgres://localhost/db") };

        let mut def = YamlAgentDefinition {
            name: "test".to_string(),
            description: None,
            model: ModelConfig {
                provider: "gemini".to_string(),
                model_id: "gemini-2.5-flash".to_string(),
                temperature: None,
                max_tokens: None,
            },
            instructions: None,
            tools: vec![],
            sub_agents: vec![],
            config: HashMap::new(),
            metadata: HashMap::new(),
            plugins: vec![],
            session: Some(SessionConfig {
                backend: "postgres".to_string(),
                options: {
                    let mut m = HashMap::new();
                    m.insert(
                        "connection_string".to_string(),
                        serde_json::Value::String("${TEST_INTERP_DB_URL}".to_string()),
                    );
                    m
                },
            }),
            memory: Some(MemoryConfig { backend: "postgres".to_string(), options: HashMap::new() }),
        };

        EnvInterpolator::interpolate(&mut def).unwrap();
        let session = def.session.unwrap();
        let conn = session.options.get("connection_string").unwrap();
        assert_eq!(conn.as_str().unwrap(), "postgres://localhost/db");

        unsafe { std::env::remove_var("TEST_INTERP_DB_URL") };
    }
}
