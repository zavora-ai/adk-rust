//! Gemini-specific schema normalization adapter.
//!
//! The [`GeminiSchemaAdapter`] applies all destructive transforms required by
//! Gemini's function-calling API. It composes shared utilities from
//! [`adk_core::schema_utils`] with Gemini-specific keyword removal to produce
//! schemas that Gemini accepts.
//!
//! # Transform Order
//!
//! Steps 3-10 exist because Gemini's **OpenAPI subset** cannot express those
//! keywords, so they are skipped for [`GeminiSchemaDialect::JsonSchema`], whose
//! field accepts them. Steps 1, 2, 11, 12 and 13 are about what the *endpoint*
//! rejects regardless of dialect, so they always run.
//!
//! | # | Transform | OpenAPI subset | JSON Schema |
//! | --- | --- | --- | --- |
//! | 1 | Resolve `$ref` (inline from definitions/`$defs`, break cycles at depth 10) | ✓ | ✓ |
//! | 2 | Strip `$schema` keyword | ✓ | ✓ |
//! | 3 | Collapse `anyOf`/`oneOf` combiners (select first non-null sub-schema) | ✓ | — |
//! | 4 | Merge `allOf` sub-schemas | ✓ | — |
//! | 5 | Collapse type arrays (`["string", "null"]` → `"string"`) | ✓ | — |
//! | 6 | Strip conditional keywords (`if`/`then`/`else`) | ✓ | — |
//! | 7 | Convert `const` to single-element `enum` | ✓ | — |
//! | 8 | Strip null values from `enum` arrays | ✓ | — |
//! | 9 | Add implicit `"type": "object"` when `properties` exists | ✓ | — |
//! | 10 | Remove unsupported keywords recursively (bounds the Schema proto defines are kept) | ✓ | — |
//! | 11 | Strip unsupported `format` values | ✓ | ✓ |
//! | 12 | Enforce nesting depth limit (5 levels) | ✓ | ✓ |
//! | 13 | Remove `definitions`/`$defs` blocks | ✓ | ✓ |
//!
//! # Example
//!
//! ```rust
//! use adk_gemini::schema_adapter::GeminiSchemaAdapter;
//! use adk_core::SchemaAdapter;
//! use serde_json::json;
//!
//! let adapter = GeminiSchemaAdapter::new();
//! let schema = json!({
//!     "$schema": "http://json-schema.org/draft-07/schema#",
//!     "type": "object",
//!     "properties": {
//!         "name": { "type": "string", "format": "hostname" }
//!     },
//!     "additionalProperties": true
//! });
//!
//! let normalized = adapter.normalize_schema(schema);
//! assert!(normalized.get("$schema").is_none());
//! assert!(normalized.get("additionalProperties").is_none());
//! assert!(normalized["properties"]["name"].get("format").is_none());
//! ```

use adk_core::SchemaAdapter;
use adk_core::schema_utils;
use serde_json::{Map, Value};
use std::borrow::Cow;

/// Allowed `format` values for the Gemini API.
const GEMINI_ALLOWED_FORMATS: &[&str] =
    &["date-time", "date", "time", "email", "uri", "uuid", "int32", "int64", "float", "double"];

/// Keywords that Gemini does not support and must be removed from all schema nodes
/// (standard API surface — removes `additionalProperties` entirely).
///
/// The Schema proto for function declarations rejects any field it does not
/// define with a 400 (`Unknown name "..."`) on `generateContent` and a WS 1007
/// close on the Live API. It defines the string, numeric and array bounds
/// `minLength`, `maxLength`, `pattern`, `minimum`, `maximum`, `minItems` and
/// `maxItems`, so those are kept; `exclusiveMinimum`, `exclusiveMaximum`,
/// `multipleOf` and the rest below are not, so they are stripped.
///
/// Reference: https://ai.google.dev/api/caching#Schema
const UNSUPPORTED_KEYWORDS: &[&str] = &[
    "$id",
    "additionalProperties",
    "contains",
    "contentEncoding",
    "contentMediaType",
    "default",
    "dependentRequired",
    "dependentSchemas",
    "deprecated",
    "examples",
    "exclusiveMaximum",
    "exclusiveMinimum",
    "maxProperties",
    "minProperties",
    "multipleOf",
    "not",
    "patternProperties",
    "prefixItems",
    "propertyNames",
    "readOnly",
    "title",
    "unevaluatedProperties",
    "uniqueItems",
    "writeOnly",
];

/// Keywords that Gemini does not support on the Vertex AI surface.
/// Unlike the standard surface, Vertex AI requires `additionalProperties: false`
/// on object schemas rather than removing it.
///
/// Same comprehensive list as [`UNSUPPORTED_KEYWORDS`] but without
/// `additionalProperties` (which is handled separately for Vertex AI).
const UNSUPPORTED_KEYWORDS_VERTEX: &[&str] = &[
    "$id",
    "contains",
    "contentEncoding",
    "contentMediaType",
    "default",
    "dependentRequired",
    "dependentSchemas",
    "deprecated",
    "examples",
    "exclusiveMaximum",
    "exclusiveMinimum",
    "maxProperties",
    "minProperties",
    "multipleOf",
    "not",
    "patternProperties",
    "prefixItems",
    "propertyNames",
    "readOnly",
    "title",
    "unevaluatedProperties",
    "uniqueItems",
    "writeOnly",
];

/// The schema dialect a [`GeminiSchemaAdapter`] produces, and the
/// function-declaration field its output must be posted under.
///
/// Gemini accepts **two mutually exclusive** schema fields on a function
/// declaration, and they are not two spellings of one thing:
///
/// | Dialect | Field | Expresses |
/// | --- | --- | --- |
/// | [`OpenApiSubset`](Self::OpenApiSubset) | `parameters` | An OpenAPI subset. No `allOf`, no `if`/`then`, no `additionalProperties`, no exclusive bounds. Keeps `minimum`/`maximum`, `minLength`/`maxLength`, `pattern` and `minItems`/`maxItems`. |
/// | [`VertexOpenApiSubset`](Self::VertexOpenApiSubset) | `parameters` | The same subset, except `additionalProperties: false` is set on objects rather than removed. |
/// | [`JsonSchema`](Self::JsonSchema) | `parametersJsonSchema` | Standard JSON Schema: `allOf`, `anyOf`, `if`/`then`, `additionalProperties`, `minLength`, numeric bounds. |
///
/// Selecting the dialect and selecting the field is therefore **one decision**,
/// which is why they live in one type. Posting a schema that kept its
/// constraints under the legacy `parameters` field does not degrade — the Live
/// socket closes with **WS 1007** (`Unknown name "additionalProperties"`), so
/// the call dies before any audio flows.
///
/// # Choosing
///
/// [`JsonSchema`](Self::JsonSchema) is verified on Google AI Studio `v1beta`
/// with `gemini-3.1-flash-live-preview`. Google's function-calling reference
/// documents only `parameters` and does not mention `parametersJsonSchema` at
/// all; support is stated in the structured-outputs announcement and confirmed
/// here by probing the live endpoint. Re-probe before assuming it on another
/// model, surface, or endpoint version — and note that acceptance is not
/// enforcement: `setupComplete` proves the schema *parsed*, not that the model
/// honours every keyword while generating.
///
/// The default remains [`OpenApiSubset`](Self::OpenApiSubset), so existing
/// callers see no behaviour change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GeminiSchemaDialect {
    /// OpenAPI subset on `parameters`. Removes `additionalProperties`.
    #[default]
    OpenApiSubset,
    /// OpenAPI subset on `parameters`, Vertex AI flavour: sets
    /// `additionalProperties: false` on object schemas instead of removing it.
    VertexOpenApiSubset,
    /// Standard JSON Schema on `parametersJsonSchema`.
    JsonSchema,
}

impl GeminiSchemaDialect {
    /// The function-declaration field a schema in this dialect belongs under.
    pub fn parameters_field(self) -> &'static str {
        match self {
            Self::OpenApiSubset | Self::VertexOpenApiSubset => "parameters",
            Self::JsonSchema => "parametersJsonSchema",
        }
    }

    /// Whether this dialect is limited to Gemini's OpenAPI subset, and so needs
    /// the combiners, conditionals and type arrays reduced away before sending.
    ///
    /// Asked as a *capability* rather than by comparing against a variant, so
    /// that adding a dialect means classifying it here once instead of hunting
    /// for scattered equality checks.
    pub fn requires_openapi_reduction(self) -> bool {
        match self {
            Self::OpenApiSubset | Self::VertexOpenApiSubset => true,
            Self::JsonSchema => false,
        }
    }
}

/// Schema adapter for the Gemini API surface.
///
/// Applies the transforms required by Gemini's function-calling API. How
/// destructive those transforms are depends on the target
/// [`GeminiSchemaDialect`]:
///
/// - **Standard** (`GeminiSchemaAdapter::new()`): OpenAPI subset; removes
///   `additionalProperties` entirely.
/// - **Vertex AI** (`GeminiSchemaAdapter::vertex_ai()`): OpenAPI subset; sets
///   `additionalProperties: false` on object schemas instead of removing it.
/// - **JSON Schema** (`GeminiSchemaAdapter::json_schema()`): keeps `allOf`,
///   `anyOf`, `if`/`then`, `additionalProperties` and the string/numeric
///   bounds, and its output must be posted under `parametersJsonSchema`.
///
/// Use [`parameters_field()`](adk_core::SchemaAdapter::parameters_field) rather
/// than hardcoding a field name; it is derived from the dialect so the two
/// cannot disagree.
///
/// # Example
///
/// ```rust
/// use adk_gemini::schema_adapter::GeminiSchemaAdapter;
/// use adk_core::SchemaAdapter;
/// use serde_json::json;
///
/// let adapter = GeminiSchemaAdapter::new();
/// let schema = json!({
///     "anyOf": [
///         {"type": "null"},
///         {"type": "string", "minLength": 1}
///     ]
/// });
///
/// let normalized = adapter.normalize_schema(schema);
/// assert_eq!(normalized["type"], "string");
/// assert!(normalized.get("anyOf").is_none());
/// ```
#[derive(Debug)]
pub struct GeminiSchemaAdapter {
    /// The dialect this adapter reduces to, which also decides the wire field.
    dialect: GeminiSchemaDialect,
}

impl GeminiSchemaAdapter {
    /// Creates a new `GeminiSchemaAdapter` for the standard Gemini API surface.
    ///
    /// This variant removes `additionalProperties` from all schema nodes.
    pub fn new() -> Self {
        Self::with_dialect(GeminiSchemaDialect::OpenApiSubset)
    }

    /// Creates a new `GeminiSchemaAdapter` for the Vertex AI surface.
    ///
    /// This variant sets `additionalProperties: false` on object schemas
    /// instead of removing the keyword entirely.
    pub fn vertex_ai() -> Self {
        Self::with_dialect(GeminiSchemaDialect::VertexOpenApiSubset)
    }

    /// Creates a new `GeminiSchemaAdapter` targeting standard JSON Schema.
    ///
    /// Keeps `allOf`, `anyOf`, `if`/`then`, `additionalProperties` and the
    /// string/numeric bounds instead of stripping them, so a model is shown the
    /// same contract the caller validates against. Its output belongs under
    /// `parametersJsonSchema`; see [`GeminiSchemaDialect::JsonSchema`] for the
    /// surfaces this is verified on.
    pub fn json_schema() -> Self {
        Self::with_dialect(GeminiSchemaDialect::JsonSchema)
    }

    /// Creates a new `GeminiSchemaAdapter` for an explicitly chosen dialect.
    pub fn with_dialect(dialect: GeminiSchemaDialect) -> Self {
        Self { dialect }
    }

    /// The dialect this adapter reduces to.
    pub fn dialect(&self) -> GeminiSchemaDialect {
        self.dialect
    }
}

impl Default for GeminiSchemaAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl SchemaAdapter for GeminiSchemaAdapter {
    fn normalize_schema(&self, mut schema: Value) -> Value {
        // Which steps run depends on the target dialect, along one line: a
        // transform that exists because the *OpenAPI subset cannot express* a
        // keyword is skipped for JSON Schema, which can; a transform that
        // exists because the *endpoint rejects* something runs for every
        // dialect. Steps 3-10 below are the first kind. Steps 1, 2, 11 and 12
        // are the second, so they are unconditional.

        // Step 1: Extract definitions and resolve $ref references.
        // Always resolve refs — even with empty definitions — so that
        // unresolvable $ref values are replaced with {"type": "object"}.
        //
        // Unconditional despite JSON Schema supporting `$ref`: Google's own
        // guidance warns that large or deeply nested schemas may be rejected,
        // and an inlined document is the shape both fields are known to accept.
        let definitions = extract_definitions(&schema);
        schema_utils::resolve_refs(&mut schema, &definitions, 0);

        // Step 2: Strip $schema keyword — rejected on both fields.
        schema_utils::strip_schema_keyword(&mut schema);

        if self.dialect.requires_openapi_reduction() {
            // Step 3: Collapse anyOf/oneOf combiners
            schema_utils::collapse_combiners(&mut schema);

            // Step 4: Merge allOf sub-schemas
            schema_utils::merge_all_of(&mut schema);

            // Step 5: Collapse type arrays
            schema_utils::collapse_type_arrays(&mut schema);

            // Step 6: Strip conditional keywords (if/then/else)
            schema_utils::strip_conditional_keywords(&mut schema);

            // Step 7: Convert const to single-element enum
            schema_utils::convert_const_to_enum(&mut schema);

            // Step 8: Strip null from enum arrays
            schema_utils::strip_null_from_enum(&mut schema);

            // Step 9: Add implicit object type.
            //
            // Skipped for JSON Schema not merely as unnecessary but as
            // *harmful*: it injects `type: "object"` into every schema-shaped
            // node it walks, including the `if` clause of a conditional, which
            // changes what that clause matches.
            schema_utils::add_implicit_object_type(&mut schema);
        }

        // Step 10: Remove unsupported keywords recursively.
        //
        // Matched exhaustively rather than defaulted: a dialect added later
        // must state what it strips, instead of silently inheriting the
        // subset's answer and quietly dropping constraints again.
        match self.dialect {
            // Nothing to remove. `additionalProperties`, `minLength`, `pattern`
            // and the numeric bounds are all expressible here, and stripping
            // them is exactly what leaves a model guessing at rules the caller
            // still enforces.
            GeminiSchemaDialect::JsonSchema => {}
            GeminiSchemaDialect::VertexOpenApiSubset => {
                remove_unsupported_keywords_vertex(&mut schema)
            }
            GeminiSchemaDialect::OpenApiSubset => remove_unsupported_keywords(&mut schema),
        }

        // Step 11: Strip unsupported format values
        schema_utils::strip_unsupported_formats(&mut schema, GEMINI_ALLOWED_FORMATS);

        // Step 12: Enforce nesting depth (max 5 levels)
        schema_utils::enforce_nesting_depth(&mut schema, 5, 0);

        // Step 13: Remove definitions/$defs blocks
        if let Some(obj) = schema.as_object_mut() {
            obj.remove("definitions");
            obj.remove("$defs");
        }

        schema
    }

    /// Truncates tool names exceeding 64 bytes at a valid UTF-8 character boundary.
    ///
    /// Preserves the prefix of the name, truncating from the end.
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

    /// Returns the fallback schema for tools with no `parameters_schema`.
    ///
    /// Gemini requires `{"type": "object", "properties": {}}` as the minimum
    /// valid function declaration parameters.
    fn empty_schema(&self) -> Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    /// The function-declaration field this adapter's output belongs under,
    /// derived from the dialect so the reduction and the field cannot disagree.
    fn parameters_field(&self) -> &'static str {
        self.dialect.parameters_field()
    }
}

/// Extracts and merges `definitions` and `$defs` from the top-level schema
/// into a single map for reference resolution.
fn extract_definitions(schema: &Value) -> Map<String, Value> {
    let mut defs = Map::new();

    if let Some(obj) = schema.as_object() {
        // Collect from "definitions" (Draft 4-7)
        if let Some(definitions) = obj.get("definitions").and_then(|v| v.as_object()) {
            for (key, value) in definitions {
                defs.insert(key.clone(), value.clone());
            }
        }

        // Collect from "$defs" (Draft 2019-09+)
        if let Some(dollar_defs) = obj.get("$defs").and_then(|v| v.as_object()) {
            for (key, value) in dollar_defs {
                defs.insert(key.clone(), value.clone());
            }
        }
    }

    defs
}

/// Recursively removes unsupported keywords from the schema and all nested sub-schemas.
///
/// Removes: `additionalProperties`, `exclusiveMinimum`, `exclusiveMaximum`,
/// `items` (when type is not "array"), `not`, `propertyNames`, `patternProperties`,
/// `unevaluatedProperties`.
fn remove_unsupported_keywords(schema: &mut Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    // Remove standard unsupported keywords
    for keyword in UNSUPPORTED_KEYWORDS {
        obj.remove(*keyword);
    }

    // Handle `items`:
    // 1. If type is NOT "array", remove items entirely (meaningless on non-array types).
    // 2. If type IS "array" and items is a JSON array (tuple validation syntax),
    //    convert to single schema using the first element. Gemini requires items
    //    on array types but only supports a single schema, not tuple validation.
    // 3. If type IS "array" and items is already an object, keep it (valid).
    // 4. If type IS "array" and items is missing, add a default items schema.
    let is_array_type = obj.get("type").and_then(|t| t.as_str()).is_some_and(|t| t == "array");
    if !is_array_type {
        obj.remove("items");
    } else if obj.get("items").is_some_and(|v| v.is_array()) {
        // Convert tuple items [schema1, schema2, ...] → first schema as single items
        let first_schema = obj
            .get("items")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .cloned()
            .unwrap_or_else(|| serde_json::json!({"type": "string"}));
        obj.insert("items".to_string(), first_schema);
    } else if !obj.contains_key("items") {
        // Gemini requires items on array types — add default if missing
        obj.insert("items".to_string(), serde_json::json!({"type": "string"}));
    }

    // Recurse into properties
    if let Some(props) = obj.get_mut("properties")
        && let Some(props_obj) = props.as_object_mut()
    {
        for value in props_obj.values_mut() {
            remove_unsupported_keywords(value);
        }
    }

    // Recurse into items (now guaranteed to be a single schema object if present)
    if let Some(items) = obj.get_mut("items")
        && items.is_object()
    {
        remove_unsupported_keywords(items);
    }

    // Recurse into allOf, anyOf, oneOf (may still exist if not collapsed)
    for keyword in &["allOf", "anyOf", "oneOf"] {
        if let Some(arr_val) = obj.get_mut(*keyword)
            && let Some(arr) = arr_val.as_array_mut()
        {
            for sub in arr.iter_mut() {
                remove_unsupported_keywords(sub);
            }
        }
    }
}

/// Recursively removes unsupported keywords for the Vertex AI surface.
///
/// Unlike the standard surface, Vertex AI requires `additionalProperties: false`
/// on object schemas. This function:
/// - Sets `additionalProperties` to `false` on object schemas (instead of removing it)
/// - Removes all other unsupported keywords the same as the standard surface
fn remove_unsupported_keywords_vertex(schema: &mut Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    // Remove Vertex-specific unsupported keywords (does NOT include additionalProperties)
    for keyword in UNSUPPORTED_KEYWORDS_VERTEX {
        obj.remove(*keyword);
    }

    // For object schemas, set additionalProperties to false
    let is_object_type = obj.get("type").and_then(|t| t.as_str()).is_some_and(|t| t == "object");
    if is_object_type {
        obj.insert("additionalProperties".to_string(), Value::Bool(false));
    } else {
        // For non-object schemas, remove additionalProperties if present
        obj.remove("additionalProperties");
    }

    // Handle `items`:
    // 1. If type is NOT "array", remove items entirely (meaningless on non-array types).
    // 2. If type IS "array" and items is a JSON array (tuple validation syntax),
    //    convert to single schema using the first element. Gemini requires items
    //    on array types but only supports a single schema, not tuple validation.
    // 3. If type IS "array" and items is already an object, keep it (valid).
    // 4. If type IS "array" and items is missing, add a default items schema.
    let is_array_type = obj.get("type").and_then(|t| t.as_str()).is_some_and(|t| t == "array");
    if !is_array_type {
        obj.remove("items");
    } else if obj.get("items").is_some_and(|v| v.is_array()) {
        let first_schema = obj
            .get("items")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .cloned()
            .unwrap_or_else(|| serde_json::json!({"type": "string"}));
        obj.insert("items".to_string(), first_schema);
    } else if !obj.contains_key("items") {
        obj.insert("items".to_string(), serde_json::json!({"type": "string"}));
    }

    // Recurse into properties
    if let Some(props) = obj.get_mut("properties")
        && let Some(props_obj) = props.as_object_mut()
    {
        for value in props_obj.values_mut() {
            remove_unsupported_keywords_vertex(value);
        }
    }

    // Recurse into items (now guaranteed to be a single schema object if present)
    if let Some(items) = obj.get_mut("items")
        && items.is_object()
    {
        remove_unsupported_keywords_vertex(items);
    }

    // Recurse into allOf, anyOf, oneOf (may still exist if not collapsed)
    for keyword in &["allOf", "anyOf", "oneOf"] {
        if let Some(arr_val) = obj.get_mut(*keyword)
            && let Some(arr) = arr_val.as_array_mut()
        {
            for sub in arr.iter_mut() {
                remove_unsupported_keywords_vertex(sub);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_strips_schema_keyword() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": { "name": { "type": "string" } }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("$schema").is_none());
    }

    #[test]
    fn test_removes_additional_properties() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "additionalProperties": true
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("additionalProperties").is_none());
    }

    #[test]
    fn test_removes_exclusive_min_max() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "number",
            "exclusiveMinimum": 0,
            "exclusiveMaximum": 100
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("exclusiveMinimum").is_none());
        assert!(result.get("exclusiveMaximum").is_none());
    }

    #[test]
    fn test_removes_items_when_not_array() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "items": { "type": "string" }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("items").is_none());
    }

    #[test]
    fn test_preserves_items_when_array() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "array",
            "items": { "type": "string" }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("items").is_some());
        assert_eq!(result["items"]["type"], "string");
    }

    #[test]
    fn test_converts_items_tuple_validation_to_single_schema() {
        // Gemini's proto doesn't support tuple validation (items as JSON array).
        // Convert to single schema using first element so arrays still have items.
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "array",
            "items": [
                { "type": "number" },
                { "type": "number" }
            ]
        });
        let result = adapter.normalize_schema(schema);
        // items should be converted to the first element schema, not removed
        assert_eq!(result["items"], json!({"type": "number"}));
        assert_eq!(result["type"], "array");
    }

    #[test]
    fn test_vertex_ai_converts_items_tuple_validation() {
        let adapter = GeminiSchemaAdapter::vertex_ai();
        let schema = json!({
            "type": "array",
            "items": [
                { "type": "integer" },
                { "type": "boolean" }
            ]
        });
        let result = adapter.normalize_schema(schema);
        // Converts to first element schema
        assert_eq!(result["items"], json!({"type": "integer"}));
    }

    #[test]
    fn test_removes_not_keyword() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "string",
            "not": { "enum": ["bad"] }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("not").is_none());
    }

    #[test]
    fn test_removes_property_names() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "propertyNames": { "pattern": "^[a-z]+$" }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("propertyNames").is_none());
    }

    #[test]
    fn test_removes_pattern_properties() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "patternProperties": { "^S_": { "type": "string" } }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("patternProperties").is_none());
    }

    #[test]
    fn test_removes_unevaluated_properties() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "unevaluatedProperties": false
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("unevaluatedProperties").is_none());
    }

    #[test]
    fn test_collapses_any_of() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "anyOf": [
                { "type": "null" },
                { "type": "string", "description": "A non-empty string" }
            ]
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("anyOf").is_none());
        assert_eq!(result["type"], "string");
        assert_eq!(result["description"], "A non-empty string");
    }

    #[test]
    fn test_collapses_one_of() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "oneOf": [
                { "type": "null" },
                { "type": "integer", "minimum": 0 }
            ]
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("oneOf").is_none());
        assert_eq!(result["type"], "integer");
    }

    #[test]
    fn test_merges_all_of() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "allOf": [
                { "type": "object", "properties": { "a": { "type": "string" } } },
                { "properties": { "b": { "type": "number" } }, "required": ["b"] }
            ]
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("allOf").is_none());
        assert_eq!(result["properties"]["a"]["type"], "string");
        assert_eq!(result["properties"]["b"]["type"], "number");
        assert_eq!(result["required"], json!(["b"]));
    }

    #[test]
    fn test_collapses_type_arrays() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": ["string", "null"],
            "minLength": 1
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["type"], "string");
    }

    #[test]
    fn test_strips_conditional_keywords() {
        let adapter = GeminiSchemaAdapter::new();
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
    fn test_converts_const_to_enum() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "string",
            "const": "fixed"
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("const").is_none());
        assert_eq!(result["enum"], json!(["fixed"]));
    }

    #[test]
    fn test_strips_null_from_enum() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "string",
            "enum": ["a", null, "b"]
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["enum"], json!(["a", "b"]));
    }

    #[test]
    fn test_removes_empty_enum_after_null_strip() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "string",
            "enum": [null]
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("enum").is_none());
    }

    #[test]
    fn test_adds_implicit_object_type() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "properties": { "name": { "type": "string" } }
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["type"], "object");
    }

    #[test]
    fn test_strips_unsupported_formats() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "properties": {
                "created": { "type": "string", "format": "date-time" },
                "hostname": { "type": "string", "format": "hostname" },
                "id": { "type": "string", "format": "uuid" }
            }
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["properties"]["created"]["format"], "date-time");
        assert!(result["properties"]["hostname"].get("format").is_none());
        assert_eq!(result["properties"]["id"]["format"], "uuid");
    }

    #[test]
    fn test_preserves_all_allowed_formats() {
        let adapter = GeminiSchemaAdapter::new();
        for format in GEMINI_ALLOWED_FORMATS {
            let schema = json!({ "type": "string", "format": format });
            let result = adapter.normalize_schema(schema);
            assert_eq!(result["format"], *format, "format '{format}' should be preserved");
        }
    }

    #[test]
    fn test_enforces_nesting_depth() {
        let adapter = GeminiSchemaAdapter::new();
        // Create a schema nested 7 levels deep
        let schema = json!({
            "type": "object",
            "properties": {
                "l1": {
                    "type": "object",
                    "properties": {
                        "l2": {
                            "type": "object",
                            "properties": {
                                "l3": {
                                    "type": "object",
                                    "properties": {
                                        "l4": {
                                            "type": "object",
                                            "properties": {
                                                "l5": {
                                                    "type": "object",
                                                    "properties": {
                                                        "l6": { "type": "string" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
        let result = adapter.normalize_schema(schema);
        // At depth 5, the schema should be truncated to {"type": "object"}
        let l5 = &result["properties"]["l1"]["properties"]["l2"]["properties"]["l3"]["properties"]
            ["l4"]["properties"]["l5"];
        assert_eq!(l5, &json!({"type": "object"}));
    }

    #[test]
    fn test_resolves_refs() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "properties": {
                "address": { "$ref": "#/definitions/Address" }
            },
            "definitions": {
                "Address": {
                    "type": "object",
                    "properties": {
                        "street": { "type": "string" }
                    }
                }
            }
        });
        let result = adapter.normalize_schema(schema);
        // $ref should be resolved
        assert!(result["properties"]["address"].get("$ref").is_none());
        assert_eq!(result["properties"]["address"]["type"], "object");
        assert_eq!(result["properties"]["address"]["properties"]["street"]["type"], "string");
        // definitions should be removed
        assert!(result.get("definitions").is_none());
    }

    #[test]
    fn test_resolves_dollar_defs() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "properties": {
                "item": { "$ref": "#/$defs/Item" }
            },
            "$defs": {
                "Item": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" }
                    }
                }
            }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result["properties"]["item"].get("$ref").is_none());
        assert_eq!(result["properties"]["item"]["type"], "object");
        assert!(result.get("$defs").is_none());
    }

    #[test]
    fn test_unresolvable_ref_becomes_object() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "properties": {
                "unknown": { "$ref": "#/definitions/DoesNotExist" }
            }
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["properties"]["unknown"], json!({"type": "object"}));
    }

    #[test]
    fn test_circular_ref_breaks() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "properties": {
                "self_ref": { "$ref": "#/definitions/Node" }
            },
            "definitions": {
                "Node": {
                    "type": "object",
                    "properties": {
                        "child": { "$ref": "#/definitions/Node" }
                    }
                }
            }
        });
        let result = adapter.normalize_schema(schema);
        // Should not panic and should terminate
        assert_eq!(result["properties"]["self_ref"]["type"], "object");
        assert!(result.get("definitions").is_none());
    }

    #[test]
    fn test_removes_definitions_and_defs() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "definitions": { "Foo": { "type": "string" } },
            "$defs": { "Bar": { "type": "number" } }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("definitions").is_none());
        assert!(result.get("$defs").is_none());
    }

    #[test]
    fn test_nested_unsupported_keywords_removed() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "properties": {
                "inner": {
                    "type": "object",
                    "additionalProperties": false,
                    "exclusiveMinimum": 5,
                    "properties": {
                        "deep": {
                            "type": "number",
                            "exclusiveMaximum": 100
                        }
                    }
                }
            }
        });
        let result = adapter.normalize_schema(schema);
        let inner = &result["properties"]["inner"];
        assert!(inner.get("additionalProperties").is_none());
        assert!(inner.get("exclusiveMinimum").is_none());
        assert!(inner["properties"]["deep"].get("exclusiveMaximum").is_none());
    }

    #[test]
    fn test_full_transform_pipeline() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "definitions": {
                "Status": { "type": "string", "enum": ["active", null, "inactive"] }
            },
            "properties": {
                "name": { "type": ["string", "null"], "format": "hostname" },
                "status": { "$ref": "#/definitions/Status" },
                "config": {
                    "type": "object",
                    "additionalProperties": true,
                    "properties": {
                        "value": { "const": "fixed" }
                    }
                }
            },
            "if": { "properties": { "name": { "type": "string" } } },
            "then": { "required": ["status"] },
            "additionalProperties": false
        });
        let result = adapter.normalize_schema(schema);

        // $schema removed
        assert!(result.get("$schema").is_none());
        // definitions removed
        assert!(result.get("definitions").is_none());
        // conditional keywords removed
        assert!(result.get("if").is_none());
        assert!(result.get("then").is_none());
        // additionalProperties removed
        assert!(result.get("additionalProperties").is_none());
        // type array collapsed
        assert_eq!(result["properties"]["name"]["type"], "string");
        // unsupported format stripped
        assert!(result["properties"]["name"].get("format").is_none());
        // $ref resolved and null stripped from enum
        assert_eq!(result["properties"]["status"]["enum"], json!(["active", "inactive"]));
        // const converted to enum
        assert_eq!(result["properties"]["config"]["properties"]["value"]["enum"], json!(["fixed"]));
        // nested additionalProperties removed
        assert!(result["properties"]["config"].get("additionalProperties").is_none());
        // implicit type added
        assert_eq!(result["type"], "object");
    }

    #[test]
    fn test_idempotent() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": {
                "name": { "type": ["string", "null"], "format": "hostname" },
                "items": { "type": "array", "items": { "type": "string" } }
            },
            "additionalProperties": true,
            "if": { "const": true },
            "then": { "required": ["name"] }
        });
        let first = adapter.normalize_schema(schema);
        let second = adapter.normalize_schema(first.clone());
        assert_eq!(first, second);
    }

    #[test]
    fn test_empty_schema() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({});
        let result = adapter.normalize_schema(schema);
        assert_eq!(result, json!({}));
    }

    #[test]
    fn test_array_items_nested_cleanup() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "array",
            "items": {
                "type": "object",
                "additionalProperties": true,
                "properties": {
                    "id": { "type": "integer", "exclusiveMinimum": 0 }
                }
            }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result["items"].get("additionalProperties").is_none());
        assert!(result["items"]["properties"]["id"].get("exclusiveMinimum").is_none());
    }

    // --- Task 4.2: Vertex AI surface variant tests ---

    #[test]
    fn test_vertex_ai_sets_additional_properties_false() {
        let adapter = GeminiSchemaAdapter::vertex_ai();
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "additionalProperties": true
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["additionalProperties"], json!(false));
    }

    #[test]
    fn test_vertex_ai_sets_additional_properties_false_on_nested_objects() {
        let adapter = GeminiSchemaAdapter::vertex_ai();
        let schema = json!({
            "type": "object",
            "properties": {
                "inner": {
                    "type": "object",
                    "properties": {
                        "value": { "type": "string" }
                    }
                }
            }
        });
        let result = adapter.normalize_schema(schema);
        assert_eq!(result["additionalProperties"], json!(false));
        assert_eq!(result["properties"]["inner"]["additionalProperties"], json!(false));
    }

    #[test]
    fn test_vertex_ai_does_not_set_additional_properties_on_non_object() {
        let adapter = GeminiSchemaAdapter::vertex_ai();
        let schema = json!({
            "type": "string",
            "additionalProperties": true
        });
        let result = adapter.normalize_schema(schema);
        // Non-object schemas should have additionalProperties removed
        assert!(result.get("additionalProperties").is_none());
    }

    #[test]
    fn test_standard_mode_removes_additional_properties() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "additionalProperties": true
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("additionalProperties").is_none());
    }

    #[test]
    fn test_vertex_ai_still_removes_other_unsupported_keywords() {
        let adapter = GeminiSchemaAdapter::vertex_ai();
        let schema = json!({
            "type": "object",
            "properties": { "x": { "type": "number" } },
            "exclusiveMinimum": 0,
            "exclusiveMaximum": 100,
            "not": { "type": "null" },
            "propertyNames": { "pattern": "^[a-z]" },
            "patternProperties": { "^S_": { "type": "string" } },
            "unevaluatedProperties": false
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("exclusiveMinimum").is_none());
        assert!(result.get("exclusiveMaximum").is_none());
        assert!(result.get("not").is_none());
        assert!(result.get("propertyNames").is_none());
        assert!(result.get("patternProperties").is_none());
        assert!(result.get("unevaluatedProperties").is_none());
        // But additionalProperties: false is set
        assert_eq!(result["additionalProperties"], json!(false));
    }

    // --- Task 4.3: normalize_tool_name tests ---

    #[test]
    fn test_normalize_tool_name_short_name_unchanged() {
        let adapter = GeminiSchemaAdapter::new();
        let name = "get_weather";
        let result = adapter.normalize_tool_name(name);
        assert_eq!(result, "get_weather");
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn test_normalize_tool_name_exactly_64_bytes() {
        let adapter = GeminiSchemaAdapter::new();
        let name = "a".repeat(64);
        let result = adapter.normalize_tool_name(&name);
        assert_eq!(result.len(), 64);
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn test_normalize_tool_name_truncates_at_64_bytes() {
        let adapter = GeminiSchemaAdapter::new();
        let name = "a".repeat(100);
        let result = adapter.normalize_tool_name(&name);
        assert_eq!(result.len(), 64);
        assert_eq!(result.as_ref(), "a".repeat(64));
    }

    #[test]
    fn test_normalize_tool_name_multibyte_boundary() {
        let adapter = GeminiSchemaAdapter::new();
        // Each '日' is 3 bytes in UTF-8. 21 chars = 63 bytes.
        // Adding one more '日' would be 66 bytes, so truncation should stop at 63.
        let name = "日".repeat(22); // 66 bytes
        let result = adapter.normalize_tool_name(&name);
        assert!(result.len() <= 64);
        // Should be 63 bytes (21 chars × 3 bytes)
        assert_eq!(result.len(), 63);
        assert_eq!(result.as_ref(), "日".repeat(21));
        // Verify it's valid UTF-8
        assert!(std::str::from_utf8(result.as_bytes()).is_ok());
    }

    #[test]
    fn test_normalize_tool_name_emoji_boundary() {
        let adapter = GeminiSchemaAdapter::new();
        // '🎯' is 4 bytes. 16 emojis = 64 bytes exactly.
        let name = "🎯".repeat(16);
        assert_eq!(name.len(), 64);
        let result = adapter.normalize_tool_name(&name);
        assert_eq!(result.len(), 64);

        // 17 emojis = 68 bytes, should truncate to 16 emojis = 64 bytes
        let name = "🎯".repeat(17);
        let result = adapter.normalize_tool_name(&name);
        assert_eq!(result.len(), 64);
        assert_eq!(result.as_ref(), "🎯".repeat(16));
    }

    // --- Task 4.4: empty_schema tests ---

    #[test]
    fn test_empty_schema_returns_object_with_properties() {
        let adapter = GeminiSchemaAdapter::new();
        let result = adapter.empty_schema();
        assert_eq!(result, json!({"type": "object", "properties": {}}));
    }

    #[test]
    fn test_empty_schema_vertex_ai_same_as_standard() {
        let adapter = GeminiSchemaAdapter::vertex_ai();
        let result = adapter.empty_schema();
        assert_eq!(result, json!({"type": "object", "properties": {}}));
    }

    // --- Comprehensive unsupported keyword stripping tests ---
    // These validate that ALL keywords not in Gemini's Schema proto are removed.

    #[test]
    fn test_removes_unsupported_and_keeps_proto_bounds() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "title": "MySchema",
            "$id": "https://example.com/schema",
            "default": {},
            "deprecated": true,
            "readOnly": true,
            "writeOnly": false,
            "examples": [{"name": "test"}],
            "minProperties": 1,
            "maxProperties": 10,
            "properties": {
                "name": {
                    "type": "string",
                    "title": "Name",
                    "default": "",
                    "minLength": 1,
                    "maxLength": 100,
                    "pattern": "^[a-z]+$"
                },
                "age": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 150,
                    "multipleOf": 1
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                    "maxItems": 10,
                    "uniqueItems": true,
                    "contains": { "type": "string" }
                }
            }
        });
        let result = adapter.normalize_schema(schema);

        // Top-level annotation/validation keywords removed
        assert!(result.get("title").is_none());
        assert!(result.get("$id").is_none());
        assert!(result.get("default").is_none());
        assert!(result.get("deprecated").is_none());
        assert!(result.get("readOnly").is_none());
        assert!(result.get("writeOnly").is_none());
        assert!(result.get("examples").is_none());
        assert!(result.get("minProperties").is_none());
        assert!(result.get("maxProperties").is_none());

        // String property: annotations removed, the proto's string bounds kept
        let name = &result["properties"]["name"];
        assert!(name.get("title").is_none());
        assert!(name.get("default").is_none());
        assert_eq!(name["minLength"], 1);
        assert_eq!(name["maxLength"], 100);
        assert_eq!(name["pattern"], "^[a-z]+$");
        assert_eq!(name["type"], "string");

        // Integer property: inclusive bounds kept, `multipleOf` removed
        let age = &result["properties"]["age"];
        assert_eq!(age["minimum"], 0);
        assert_eq!(age["maximum"], 150);
        assert!(age.get("multipleOf").is_none());
        assert_eq!(age["type"], "integer");

        // Array property: item-count bounds kept, other array keywords removed
        let tags = &result["properties"]["tags"];
        assert_eq!(tags["minItems"], 1);
        assert_eq!(tags["maxItems"], 10);
        assert!(tags.get("uniqueItems").is_none());
        assert!(tags.get("contains").is_none());
        assert_eq!(tags["type"], "array");
        assert_eq!(tags["items"]["type"], "string");
    }

    /// Every bound the Schema proto defines survives both OpenAPI-subset
    /// dialects; the exclusive bounds, which it does not define, do not.
    #[test]
    fn openapi_subset_dialects_keep_the_bounds_the_proto_defines() {
        let schema = json!({
            "type": "object",
            "properties": {
                "room": {"type": "string", "minLength": 2, "maxLength": 4, "pattern": "^[A-Z][0-9]$"},
                "count": {"type": "integer", "minimum": 1, "maximum": 10, "exclusiveMaximum": 11},
                "tags": {"type": "array", "items": {"type": "string"}, "minItems": 0, "maxItems": 3}
            }
        });
        for adapter in [GeminiSchemaAdapter::new(), GeminiSchemaAdapter::vertex_ai()] {
            let properties = &adapter.normalize_schema(schema.clone())["properties"];
            assert_eq!(properties["room"]["minLength"], 2);
            assert_eq!(properties["room"]["maxLength"], 4);
            assert_eq!(properties["room"]["pattern"], "^[A-Z][0-9]$");
            assert_eq!(properties["count"]["minimum"], 1);
            assert_eq!(properties["count"]["maximum"], 10);
            assert!(properties["count"].get("exclusiveMaximum").is_none());
            assert_eq!(properties["tags"]["minItems"], 0);
            assert_eq!(properties["tags"]["maxItems"], 3);
        }
    }

    /// A field's own description sits beside its `$ref`; inlining the
    /// definition keeps it, and the inlined fields keep their bounds.
    #[test]
    fn ref_sibling_description_survives_inlining() {
        let schema = json!({
            "type": "object",
            "properties": {
                "confirm_to": {"$ref": "#/$defs/Address", "description": "Where to post it."},
                "home": {"$ref": "#/$defs/Address"}
            },
            "$defs": {
                "Address": {
                    "type": "object",
                    "description": "A postal address.",
                    "properties": {"city": {"type": "string", "maxLength": 40}},
                    "additionalProperties": false
                }
            }
        });
        let result = GeminiSchemaAdapter::new().normalize_schema(schema);
        let confirm_to = &result["properties"]["confirm_to"];
        assert_eq!(confirm_to["description"], "Where to post it.");
        assert_eq!(confirm_to["properties"]["city"]["maxLength"], 40);
        assert!(confirm_to.get("$ref").is_none());
        assert!(confirm_to.get("additionalProperties").is_none());
        assert_eq!(result["properties"]["home"]["description"], "A postal address.");
        assert!(result.get("$defs").is_none());
    }

    #[test]
    fn test_removes_prefix_items() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "array",
            "prefixItems": [
                { "type": "string" },
                { "type": "integer" }
            ]
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("prefixItems").is_none());
    }

    #[test]
    fn test_removes_dependent_keywords() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "credit_card": { "type": "string" }
            },
            "dependentRequired": {
                "credit_card": ["billing_address"]
            },
            "dependentSchemas": {
                "credit_card": {
                    "properties": {
                        "billing_address": { "type": "string" }
                    }
                }
            }
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("dependentRequired").is_none());
        assert!(result.get("dependentSchemas").is_none());
    }

    #[test]
    fn test_removes_content_keywords() {
        let adapter = GeminiSchemaAdapter::new();
        let schema = json!({
            "type": "string",
            "contentMediaType": "application/json",
            "contentEncoding": "base64"
        });
        let result = adapter.normalize_schema(schema);
        assert!(result.get("contentMediaType").is_none());
        assert!(result.get("contentEncoding").is_none());
    }

    // --- Dialect selection -------------------------------------------------

    /// The mapping that makes the reduction and the wire field one decision.
    #[test]
    fn parameters_field_follows_the_dialect() {
        assert_eq!(GeminiSchemaAdapter::new().parameters_field(), "parameters");
        assert_eq!(GeminiSchemaAdapter::vertex_ai().parameters_field(), "parameters");
        assert_eq!(GeminiSchemaAdapter::json_schema().parameters_field(), "parametersJsonSchema");
        // The default must stay the legacy field: it is what every existing
        // caller gets without asking.
        assert_eq!(GeminiSchemaDialect::default(), GeminiSchemaDialect::OpenApiSubset);
    }

    /// A realistic acquisition schema: a conditional rule, a closed object, and
    /// a bounded string. The OpenAPI subset cannot carry the first two, so it
    /// strips them; the JSON Schema dialect keeps all three.
    fn schema_with_constraints_the_subset_cannot_carry() -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "request_kind": {"type": "string", "enum": ["order", "information"]},
                "callback_number": {"type": "string", "minLength": 7},
                "party_size": {"type": "integer", "minimum": 1, "maximum": 40}
            },
            "required": ["request_kind"],
            "allOf": [{
                "if": {"properties": {"request_kind": {"const": "order"}}, "required": ["request_kind"]},
                "then": {"required": ["callback_number"]}
            }]
        })
    }

    #[test]
    fn json_schema_dialect_keeps_what_the_subset_strips() {
        let result = GeminiSchemaAdapter::json_schema()
            .normalize_schema(schema_with_constraints_the_subset_cannot_carry());

        assert_eq!(result["additionalProperties"], json!(false));
        assert!(result.get("allOf").is_some(), "conditional rule dropped: {result}");
        assert_eq!(result["properties"]["callback_number"]["minLength"], 7);
        assert_eq!(result["properties"]["party_size"]["minimum"], 1);
        assert_eq!(result["properties"]["party_size"]["maximum"], 40);
    }

    /// `add_implicit_object_type` is skipped for JSON Schema because it is
    /// actively wrong there, not merely redundant: it stamps `type: "object"`
    /// onto every schema-shaped node it walks, including an `if` clause, which
    /// changes what that clause matches.
    #[test]
    fn json_schema_dialect_does_not_stamp_a_type_onto_conditionals() {
        let result = GeminiSchemaAdapter::json_schema()
            .normalize_schema(schema_with_constraints_the_subset_cannot_carry());

        let if_clause = &result["allOf"][0]["if"];
        assert!(
            if_clause.get("type").is_none(),
            "an implicit object type was injected into the `if` clause: {if_clause}"
        );
    }

    /// The two OpenAPI-subset constructors still post under the legacy field
    /// and still reduce away what the subset cannot express, while keeping the
    /// bounds it can.
    #[test]
    fn openapi_subset_dialects_reduce_what_the_subset_cannot_carry() {
        for adapter in [GeminiSchemaAdapter::new(), GeminiSchemaAdapter::vertex_ai()] {
            assert_eq!(adapter.parameters_field(), "parameters");

            let result =
                adapter.normalize_schema(schema_with_constraints_the_subset_cannot_carry());

            assert!(result.get("allOf").is_none(), "{result}");
            assert_eq!(result["properties"]["callback_number"]["minLength"], 7);
            assert_eq!(result["properties"]["party_size"]["minimum"], 1);
            assert_eq!(result["properties"]["party_size"]["maximum"], 40);
        }
    }
}
