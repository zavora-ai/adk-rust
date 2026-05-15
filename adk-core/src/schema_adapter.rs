//! Schema normalization adapter for LLM provider function-calling APIs.
//!
//! Each LLM provider has different JSON Schema requirements for tool parameters.
//! The [`SchemaAdapter`] trait provides a consistent interface for transforming
//! raw MCP tool schemas into the provider's accepted format at request time.
//!
//! # Architecture
//!
//! `McpToolset` returns raw schemas verbatim. Each model adapter implements
//! `SchemaAdapter` to normalize schemas according to its backend's limitations.
//! This separation keeps MCP tool discovery independent of LLM-specific concerns.
//!
//! # Example
//!
//! ```rust
//! use adk_core::SchemaAdapter;
//! use serde_json::{json, Value};
//! use std::borrow::Cow;
//!
//! #[derive(Debug)]
//! struct MyAdapter;
//!
//! impl SchemaAdapter for MyAdapter {
//!     fn normalize_schema(&self, schema: Value) -> Value {
//!         // Apply provider-specific transforms
//!         schema
//!     }
//! }
//!
//! let adapter = MyAdapter;
//! let raw = json!({"type": "object", "properties": {"name": {"type": "string"}}});
//! let normalized = adapter.normalize_schema(raw);
//! ```

use serde_json::Value;
use std::borrow::Cow;

use crate::schema_utils;

/// Normalizes JSON Schema for a specific LLM provider's function-calling API.
///
/// Each provider has different schema requirements. The adapter transforms
/// raw MCP tool schemas into the provider's accepted format at request time.
///
/// # Default Implementations
///
/// - [`normalize_tool_name`](SchemaAdapter::normalize_tool_name): Truncates names
///   exceeding 64 bytes at a valid UTF-8 character boundary.
/// - [`empty_schema`](SchemaAdapter::empty_schema): Returns
///   `{"type": "object", "properties": {}}` as the fallback when no input schema
///   is provided.
///
/// # Thread Safety
///
/// All implementations must be `Send + Sync` to support concurrent request building
/// across async tasks.
pub trait SchemaAdapter: Send + Sync + std::fmt::Debug {
    /// Normalize a raw JSON Schema for this provider.
    ///
    /// Called once per tool per request (results may be cached by the model adapter layer).
    ///
    /// # Arguments
    ///
    /// * `schema` - The raw JSON Schema value from an MCP tool's `inputSchema`.
    ///
    /// # Returns
    ///
    /// A normalized JSON Schema value accepted by this provider's API.
    fn normalize_schema(&self, schema: Value) -> Value;

    /// Normalize a tool name for this provider's limits.
    ///
    /// Default implementation truncates names exceeding 64 bytes at the nearest
    /// valid UTF-8 character boundary, preserving the prefix.
    ///
    /// # Arguments
    ///
    /// * `name` - The original tool name.
    ///
    /// # Returns
    ///
    /// A [`Cow::Borrowed`] reference if the name fits within 64 bytes, or a
    /// [`Cow::Owned`] truncated string otherwise.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::SchemaAdapter;
    /// use serde_json::Value;
    /// use std::borrow::Cow;
    ///
    /// #[derive(Debug)]
    /// struct TestAdapter;
    /// impl SchemaAdapter for TestAdapter {
    ///     fn normalize_schema(&self, schema: Value) -> Value { schema }
    /// }
    ///
    /// let adapter = TestAdapter;
    ///
    /// // Short names are returned as-is
    /// assert_eq!(adapter.normalize_tool_name("get_weather"), Cow::Borrowed("get_weather"));
    ///
    /// // Long names are truncated to at most 64 bytes
    /// let long_name = "a".repeat(100);
    /// let result = adapter.normalize_tool_name(&long_name);
    /// assert!(result.len() <= 64);
    /// ```
    fn normalize_tool_name<'a>(&self, name: &'a str) -> Cow<'a, str> {
        if name.len() <= 64 {
            Cow::Borrowed(name)
        } else {
            // Find the largest valid UTF-8 boundary at or before 64 bytes.
            // Walk backward from byte 64 until we hit a byte that is not a
            // UTF-8 continuation byte (0b10xxxxxx).
            let mut end = 64;
            while end > 0 && !name.is_char_boundary(end) {
                end -= 1;
            }
            Cow::Owned(name[..end].to_string())
        }
    }

    /// Fallback schema when a tool provides no `parameters_schema`.
    ///
    /// Returns `{"type": "object", "properties": {}}` by default, which represents
    /// a tool that accepts no parameters.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::SchemaAdapter;
    /// use serde_json::{json, Value};
    ///
    /// #[derive(Debug)]
    /// struct TestAdapter;
    /// impl SchemaAdapter for TestAdapter {
    ///     fn normalize_schema(&self, schema: Value) -> Value { schema }
    /// }
    ///
    /// let adapter = TestAdapter;
    /// assert_eq!(adapter.empty_schema(), json!({"type": "object", "properties": {}}));
    /// ```
    fn empty_schema(&self) -> Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
}

/// Default schema adapter for providers with no specific requirements (Ollama, etc.).
///
/// Applies a conservative set of shared utility transforms:
/// 1. Strip `$schema` keyword
/// 2. Strip conditional keywords (`if`/`then`/`else`)
/// 3. Convert `const` to single-element `enum`
/// 4. Add implicit `"type": "object"` when `properties` exists
/// 5. Strip unsupported `format` values
///
/// This adapter does not resolve `$ref`, collapse combiners, or enforce nesting
/// depth limits. It is suitable for providers that accept most JSON Schema features
/// but reject the meta-keywords and conditional constructs.
///
/// Used as the default return value of [`Llm::schema_adapter()`](crate::Llm::schema_adapter)
/// for providers that do not override it.
///
/// # Example
///
/// ```rust
/// use adk_core::{GenericSchemaAdapter, SchemaAdapter};
/// use serde_json::json;
///
/// let adapter = GenericSchemaAdapter;
/// let schema = json!({
///     "$schema": "http://json-schema.org/draft-07/schema#",
///     "properties": {
///         "name": { "type": "string", "const": "fixed" }
///     },
///     "if": { "properties": { "x": { "type": "number" } } },
///     "then": { "required": ["x"] }
/// });
///
/// let normalized = adapter.normalize_schema(schema);
/// assert!(normalized.get("$schema").is_none());
/// assert!(normalized.get("if").is_none());
/// assert!(normalized.get("then").is_none());
/// assert_eq!(normalized["type"], "object");
/// assert_eq!(normalized["properties"]["name"]["enum"], json!(["fixed"]));
/// ```
#[derive(Debug)]
pub struct GenericSchemaAdapter;

/// Allowed format values for the generic adapter (same as Gemini).
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_generic_adapter_strips_schema_keyword() {
        let adapter = GenericSchemaAdapter;
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": { "name": { "type": "string" } }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("$schema").is_none());
        assert_eq!(result["type"], "object");
    }

    #[test]
    fn test_generic_adapter_strips_conditional_keywords() {
        let adapter = GenericSchemaAdapter;
        let schema = json!({
            "type": "object",
            "if": { "properties": { "kind": { "const": "a" } } },
            "then": { "required": ["extra"] },
            "else": { "required": [] }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("if").is_none());
        assert!(result.get("then").is_none());
        assert!(result.get("else").is_none());
    }

    #[test]
    fn test_generic_adapter_converts_const_to_enum() {
        let adapter = GenericSchemaAdapter;
        let schema = json!({
            "type": "string",
            "const": "fixed_value"
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("const").is_none());
        assert_eq!(result["enum"], json!(["fixed_value"]));
    }

    #[test]
    fn test_generic_adapter_adds_implicit_object_type() {
        let adapter = GenericSchemaAdapter;
        let schema = json!({
            "properties": {
                "name": { "type": "string" }
            }
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["type"], "object");
    }

    #[test]
    fn test_generic_adapter_strips_unsupported_formats() {
        let adapter = GenericSchemaAdapter;
        let schema = json!({
            "type": "object",
            "properties": {
                "created": { "type": "string", "format": "date-time" },
                "hostname": { "type": "string", "format": "hostname" },
                "email": { "type": "string", "format": "email" }
            }
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["properties"]["created"]["format"], "date-time");
        assert!(result["properties"]["hostname"].get("format").is_none());
        assert_eq!(result["properties"]["email"]["format"], "email");
    }

    #[test]
    fn test_generic_adapter_preserves_allowed_formats() {
        let adapter = GenericSchemaAdapter;
        for format in GENERIC_ALLOWED_FORMATS {
            let schema = json!({ "type": "string", "format": format });
            let result = adapter.normalize_schema(schema);
            assert_eq!(result["format"], *format, "format '{format}' should be preserved");
        }
    }

    #[test]
    fn test_generic_adapter_all_transforms_combined() {
        let adapter = GenericSchemaAdapter;
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "properties": {
                "status": { "type": "string", "const": "active" },
                "host": { "type": "string", "format": "hostname" },
                "created": { "type": "string", "format": "date-time" }
            },
            "if": { "properties": { "status": { "const": "active" } } },
            "then": { "required": ["host"] }
        });
        let result = adapter.normalize_schema(schema);

        // $schema removed
        assert!(result.get("$schema").is_none());
        // conditional keywords removed
        assert!(result.get("if").is_none());
        assert!(result.get("then").is_none());
        // implicit type added
        assert_eq!(result["type"], "object");
        // const converted to enum
        assert!(result["properties"]["status"].get("const").is_none());
        assert_eq!(result["properties"]["status"]["enum"], json!(["active"]));
        // unsupported format stripped
        assert!(result["properties"]["host"].get("format").is_none());
        // allowed format preserved
        assert_eq!(result["properties"]["created"]["format"], "date-time");
    }

    #[test]
    fn test_generic_adapter_nested_transforms() {
        let adapter = GenericSchemaAdapter;
        let schema = json!({
            "type": "object",
            "properties": {
                "nested": {
                    "$schema": "draft-07",
                    "properties": {
                        "deep": {
                            "type": "string",
                            "const": "value",
                            "format": "ipv4"
                        }
                    },
                    "if": { "const": true },
                    "then": { "type": "string" }
                }
            }
        });
        let result = adapter.normalize_schema(schema);
        let nested = &result["properties"]["nested"];
        assert!(nested.get("$schema").is_none());
        assert!(nested.get("if").is_none());
        assert!(nested.get("then").is_none());
        assert_eq!(nested["type"], "object");
        assert_eq!(nested["properties"]["deep"]["enum"], json!(["value"]));
        assert!(nested["properties"]["deep"].get("format").is_none());
    }

    #[test]
    fn test_generic_adapter_idempotent() {
        let adapter = GenericSchemaAdapter;
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "properties": {
                "name": { "type": "string", "const": "test", "format": "hostname" }
            },
            "if": { "const": true },
            "then": { "required": ["name"] }
        });
        let first = adapter.normalize_schema(schema);
        let second = adapter.normalize_schema(first.clone());
        assert_eq!(first, second);
    }

    #[test]
    fn test_generic_adapter_empty_schema_passthrough() {
        let adapter = GenericSchemaAdapter;
        let schema = json!({});
        let result = adapter.normalize_schema(schema);
        assert_eq!(result, json!({}));
    }

    #[test]
    fn test_generic_adapter_preserves_refs_and_combiners() {
        let adapter = GenericSchemaAdapter;
        let schema = json!({
            "type": "object",
            "$ref": "#/definitions/Foo",
            "anyOf": [{ "type": "string" }, { "type": "number" }],
            "oneOf": [{ "type": "boolean" }],
            "allOf": [{ "required": ["a"] }],
            "additionalProperties": false
        });
        let result = adapter.normalize_schema(schema);
        // GenericSchemaAdapter does NOT resolve refs or collapse combiners
        assert!(result.get("$ref").is_some());
        assert!(result.get("anyOf").is_some());
        assert!(result.get("oneOf").is_some());
        assert!(result.get("allOf").is_some());
        assert!(result.get("additionalProperties").is_some());
    }
}
