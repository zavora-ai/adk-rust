//! Schema normalization adapter for LLM provider function-calling APIs.
use crate::schema_utils;
use serde_json::Value;
use std::borrow::Cow;
use std::fmt;

/// Error returned when a tool schema cannot be compiled for a specific provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaCompileError {
    /// Human-readable error message explaining why compilation failed.
    pub message: String,
}

impl fmt::Display for SchemaCompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Schema compile error: {}", self.message)
    }
}

impl std::error::Error for SchemaCompileError {}

impl SchemaCompileError {
    /// Create a new schema compilation error.
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }
}

/// Normalizes JSON Schema for a specific LLM provider's function-calling API.
pub trait SchemaAdapter: Send + Sync + std::fmt::Debug {
    /// Normalize a raw JSON Schema for this provider (infallible).
    fn normalize_schema(&self, schema: Value) -> Value;

    /// Compiles a raw JSON Schema for this provider, returning an error if unsupported.
    fn compile_schema(&self, schema: &Value) -> Result<Value, SchemaCompileError> {
        Ok(self.normalize_schema(schema.clone()))
    }

    /// Normalize a tool name for this provider's limits.
    fn normalize_tool_name<'a>(&self, name: &'a str) -> Cow<'a, str> {
        if name.len() <= 64 {
            Cow::Borrowed(name)
        } else {
            let mut end = 64;
            while end > 0 && !name.is_char_boundary(end) {
                end -= 1;
            }
            Cow::Owned(name[..end].to_string())
        }
    }

    /// Fallback schema when a tool provides no parameters.
    fn empty_schema(&self) -> Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
}

/// Default schema adapter for providers with no specific requirements.
#[derive(Debug)]
pub struct GenericSchemaAdapter;

const GENERIC_ALLOWED_FORMATS: &[&str] =
    &["date-time", "date", "time", "email", "uri", "uuid", "int32", "int64", "float", "double"];

impl SchemaAdapter for GenericSchemaAdapter {
    fn normalize_schema(&self, mut schema: Value) -> Value {
        schema_utils::strip_schema_keyword(&mut schema);
        schema_utils::strip_conditional_keywords(&mut schema);
        schema_utils::convert_const_to_enum(&mut schema);
        schema_utils::add_implicit_object_type(&mut schema);
        schema_utils::strip_unsupported_formats(&mut schema, GENERIC_ALLOWED_FORMATS);
        schema
    }
}
