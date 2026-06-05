//! State schema validation for the functional API.
//!
//! Provides validation of workflow state against a declared schema at
//! workflow start and before applying task output reducers. Wraps the
//! existing [`StateSchema`] from `adk-graph` and adds type-level
//! validation capabilities specific to the functional API.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_graph::functional::StateSchemaValidator;
//! use adk_graph::state::StateSchema;
//! use serde_json::json;
//! use std::collections::HashMap;
//!
//! let schema = StateSchema::builder()
//!     .channel("counter")
//!     .list_channel("messages")
//!     .build();
//!
//! let validator = StateSchemaValidator::new(schema)
//!     .expect_type("counter", ExpectedType::Number)
//!     .expect_type("messages", ExpectedType::Array);
//!
//! let mut state = HashMap::new();
//! state.insert("counter".to_string(), json!(42));
//! state.insert("messages".to_string(), json!([]));
//!
//! // Passes validation
//! validator.validate_state(&state).unwrap();
//! ```

use serde_json::Value;
use std::collections::HashMap;

use crate::state::{State, StateSchema};

use super::error::FunctionalError;

/// Expected JSON value type for a state field.
///
/// Used by [`StateSchemaValidator`] to check that state values conform
/// to the declared type expectations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedType {
    /// JSON null value.
    Null,
    /// JSON boolean value.
    Boolean,
    /// JSON numeric value (integer or float).
    Number,
    /// JSON string value.
    String,
    /// JSON array value.
    Array,
    /// JSON object value.
    Object,
}

impl ExpectedType {
    /// Check if a JSON value matches this expected type.
    pub fn matches(&self, value: &Value) -> bool {
        match self {
            ExpectedType::Null => value.is_null(),
            ExpectedType::Boolean => value.is_boolean(),
            ExpectedType::Number => value.is_number(),
            ExpectedType::String => value.is_string(),
            ExpectedType::Array => value.is_array(),
            ExpectedType::Object => value.is_object(),
        }
    }

    /// Return a human-readable name for this type.
    pub fn type_name(&self) -> &'static str {
        match self {
            ExpectedType::Null => "null",
            ExpectedType::Boolean => "boolean",
            ExpectedType::Number => "number",
            ExpectedType::String => "string",
            ExpectedType::Array => "array",
            ExpectedType::Object => "object",
        }
    }
}

impl std::fmt::Display for ExpectedType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.type_name())
    }
}

/// Get a human-readable type name for a JSON value.
fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Validates workflow state against a declared schema.
///
/// Wraps the existing [`StateSchema`] and adds field-level type validation
/// capabilities for the functional API. Used at workflow start to validate
/// initial state, and before applying reducers to validate task output.
///
/// # Example
///
/// ```rust,ignore
/// use adk_graph::functional::StateSchemaValidator;
/// use adk_graph::state::StateSchema;
///
/// let schema = StateSchema::builder()
///     .channel("status")
///     .counter_channel("count")
///     .build();
///
/// let validator = StateSchemaValidator::new(schema)
///     .expect_type("status", ExpectedType::String)
///     .expect_type("count", ExpectedType::Number);
/// ```
#[derive(Clone)]
pub struct StateSchemaValidator {
    /// The underlying state schema with channels and reducers.
    schema: StateSchema,
    /// Expected types for each field.
    type_expectations: HashMap<String, ExpectedType>,
    /// Fields that are required (must be present in state).
    required_fields: Vec<String>,
}

impl std::fmt::Debug for StateSchemaValidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateSchemaValidator")
            .field("type_expectations", &self.type_expectations)
            .field("required_fields", &self.required_fields)
            .finish_non_exhaustive()
    }
}

impl StateSchemaValidator {
    /// Create a new validator wrapping an existing [`StateSchema`].
    pub fn new(schema: StateSchema) -> Self {
        Self { schema, type_expectations: HashMap::new(), required_fields: Vec::new() }
    }

    /// Declare the expected type for a state field.
    ///
    /// Fields with type expectations will be validated when present.
    /// Use [`Self::require_field`] to also require the field's presence.
    pub fn expect_type(mut self, field: &str, expected: ExpectedType) -> Self {
        self.type_expectations.insert(field.to_string(), expected);
        self
    }

    /// Mark a field as required (must be present in state).
    ///
    /// Required fields that are missing from state will cause validation
    /// to fail with a descriptive error.
    pub fn require_field(mut self, field: &str) -> Self {
        if !self.required_fields.contains(&field.to_string()) {
            self.required_fields.push(field.to_string());
        }
        self
    }

    /// Get the underlying [`StateSchema`].
    pub fn schema(&self) -> &StateSchema {
        &self.schema
    }

    /// Validate that all required fields exist with correct types in state.
    ///
    /// Called at workflow start to validate the initial state.
    ///
    /// # Errors
    ///
    /// Returns [`FunctionalError::SchemaValidation`] if:
    /// - A required field is missing from state
    /// - A field value does not match its declared expected type
    pub fn validate_state(&self, state: &State) -> Result<(), FunctionalError> {
        // Check required fields are present.
        for field in &self.required_fields {
            if !state.contains_key(field) {
                return Err(FunctionalError::SchemaValidation {
                    field: field.clone(),
                    expected: "present".to_string(),
                    actual: "missing".to_string(),
                });
            }
        }

        // Check type expectations for present fields.
        for (field, expected_type) in &self.type_expectations {
            if let Some(value) = state.get(field) {
                if !expected_type.matches(value) {
                    return Err(FunctionalError::SchemaValidation {
                        field: field.clone(),
                        expected: expected_type.type_name().to_string(),
                        actual: value_type_name(value).to_string(),
                    });
                }
            }
        }

        Ok(())
    }

    /// Validate task output before applying reducers.
    ///
    /// Checks that output fields match their declared types. This is
    /// called after a task produces output but before the output is
    /// merged into the workflow state via reducers.
    ///
    /// # Arguments
    ///
    /// * `output` - A map of field names to values produced by the task.
    ///
    /// # Errors
    ///
    /// Returns [`FunctionalError::SchemaValidation`] if any output field
    /// has a value that does not match its declared expected type.
    pub fn validate_task_output(&self, output: &State) -> Result<(), FunctionalError> {
        for (field, value) in output {
            if let Some(expected_type) = self.type_expectations.get(field) {
                if !expected_type.matches(value) {
                    return Err(FunctionalError::SchemaValidation {
                        field: field.clone(),
                        expected: expected_type.type_name().to_string(),
                        actual: value_type_name(value).to_string(),
                    });
                }
            }
        }

        Ok(())
    }

    /// Apply an update to state using the underlying schema's reducer.
    ///
    /// Delegates to [`StateSchema::apply_update`].
    pub fn apply_update(&self, state: &mut State, key: &str, value: Value) {
        self.schema.apply_update(state, key, value);
    }
}

impl Default for StateSchemaValidator {
    fn default() -> Self {
        Self::new(StateSchema::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_validate_state_passes_with_correct_types() {
        let schema = StateSchema::builder().channel("name").counter_channel("count").build();

        let validator = StateSchemaValidator::new(schema)
            .expect_type("name", ExpectedType::String)
            .expect_type("count", ExpectedType::Number)
            .require_field("name");

        let mut state = State::new();
        state.insert("name".to_string(), json!("workflow_1"));
        state.insert("count".to_string(), json!(0));

        assert!(validator.validate_state(&state).is_ok());
    }

    #[test]
    fn test_validate_state_fails_on_missing_required_field() {
        let validator =
            StateSchemaValidator::new(StateSchema::default()).require_field("required_field");

        let state = State::new();

        let err = validator.validate_state(&state).unwrap_err();
        match err {
            FunctionalError::SchemaValidation { field, expected, actual } => {
                assert_eq!(field, "required_field");
                assert_eq!(expected, "present");
                assert_eq!(actual, "missing");
            }
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn test_validate_state_fails_on_type_mismatch() {
        let validator = StateSchemaValidator::new(StateSchema::default())
            .expect_type("count", ExpectedType::Number);

        let mut state = State::new();
        state.insert("count".to_string(), json!("not_a_number"));

        let err = validator.validate_state(&state).unwrap_err();
        match err {
            FunctionalError::SchemaValidation { field, expected, actual } => {
                assert_eq!(field, "count");
                assert_eq!(expected, "number");
                assert_eq!(actual, "string");
            }
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn test_validate_task_output_passes_with_correct_types() {
        let validator = StateSchemaValidator::new(StateSchema::default())
            .expect_type("result", ExpectedType::Object)
            .expect_type("score", ExpectedType::Number);

        let mut output = State::new();
        output.insert("result".to_string(), json!({"key": "value"}));
        output.insert("score".to_string(), json!(95));

        assert!(validator.validate_task_output(&output).is_ok());
    }

    #[test]
    fn test_validate_task_output_fails_on_type_mismatch() {
        let validator = StateSchemaValidator::new(StateSchema::default())
            .expect_type("items", ExpectedType::Array);

        let mut output = State::new();
        output.insert("items".to_string(), json!("not_an_array"));

        let err = validator.validate_task_output(&output).unwrap_err();
        match err {
            FunctionalError::SchemaValidation { field, expected, actual } => {
                assert_eq!(field, "items");
                assert_eq!(expected, "array");
                assert_eq!(actual, "string");
            }
            _ => panic!("unexpected error variant"),
        }
    }

    #[test]
    fn test_validate_state_skips_absent_optional_fields() {
        let validator = StateSchemaValidator::new(StateSchema::default())
            .expect_type("optional_field", ExpectedType::String);

        // Field not present — should pass since it's not required.
        let state = State::new();
        assert!(validator.validate_state(&state).is_ok());
    }

    #[test]
    fn test_validate_task_output_ignores_unknown_fields() {
        let validator = StateSchemaValidator::new(StateSchema::default())
            .expect_type("known", ExpectedType::Number);

        let mut output = State::new();
        output.insert("known".to_string(), json!(42));
        output.insert("unknown".to_string(), json!("anything"));

        // "unknown" has no type expectation, so validation passes.
        assert!(validator.validate_task_output(&output).is_ok());
    }

    #[test]
    fn test_expected_type_matches() {
        assert!(ExpectedType::Null.matches(&json!(null)));
        assert!(ExpectedType::Boolean.matches(&json!(true)));
        assert!(ExpectedType::Number.matches(&json!(42)));
        assert!(ExpectedType::Number.matches(&json!(3.14)));
        assert!(ExpectedType::String.matches(&json!("hello")));
        assert!(ExpectedType::Array.matches(&json!([1, 2, 3])));
        assert!(ExpectedType::Object.matches(&json!({"key": "value"})));

        assert!(!ExpectedType::Number.matches(&json!("42")));
        assert!(!ExpectedType::String.matches(&json!(42)));
        assert!(!ExpectedType::Array.matches(&json!({})));
    }
}
