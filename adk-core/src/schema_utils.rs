//! # Schema Utilities
//!
//! Shared utility functions for normalizing JSON Schema documents across
//! multiple LLM provider adapters. Each function operates on `serde_json::Value`
//! via mutable references for in-place transformation and recurses into nested
//! schemas (properties, items, additionalProperties, allOf, anyOf, oneOf, etc.).
//!
//! These utilities are independently unit-testable and composable — each adapter
//! selects which transforms to apply and in what order.
//!
//! ## Example
//!
//! ```rust
//! use serde_json::json;
//! use adk_core::schema_utils;
//!
//! let mut schema = json!({
//!     "$schema": "http://json-schema.org/draft-07/schema#",
//!     "type": "object",
//!     "properties": {
//!         "name": { "type": "string" }
//!     }
//! });
//!
//! schema_utils::strip_schema_keyword(&mut schema);
//! assert!(schema.get("$schema").is_none());
//! ```

use std::borrow::Cow;

use serde_json::{Map, Value};

/// Removes the `$schema` keyword from the schema and all nested sub-schemas.
///
/// Many LLM providers reject schemas containing the `$schema` meta-keyword.
///
/// # Example
///
/// ```rust
/// use serde_json::json;
/// use adk_core::schema_utils::strip_schema_keyword;
///
/// let mut schema = json!({
///     "$schema": "http://json-schema.org/draft-07/schema#",
///     "type": "object",
///     "properties": {
///         "nested": {
///             "$schema": "http://json-schema.org/draft-07/schema#",
///             "type": "string"
///         }
///     }
/// });
///
/// strip_schema_keyword(&mut schema);
/// assert!(schema.get("$schema").is_none());
/// ```
pub fn strip_schema_keyword(schema: &mut Value) {
    if let Some(obj) = schema.as_object_mut() {
        obj.remove("$schema");
    }
    recurse_into_subschemas(schema, strip_schema_keyword);
}

/// Removes conditional keywords (`if`, `then`, `else`) from the schema and all
/// nested sub-schemas.
///
/// Providers like Gemini, OpenAI, and Anthropic do not support JSON Schema
/// conditional composition.
///
/// # Example
///
/// ```rust
/// use serde_json::json;
/// use adk_core::schema_utils::strip_conditional_keywords;
///
/// let mut schema = json!({
///     "type": "object",
///     "if": { "properties": { "kind": { "const": "a" } } },
///     "then": { "required": ["extra"] },
///     "else": { "required": [] }
/// });
///
/// strip_conditional_keywords(&mut schema);
/// assert!(schema.get("if").is_none());
/// assert!(schema.get("then").is_none());
/// assert!(schema.get("else").is_none());
/// ```
pub fn strip_conditional_keywords(schema: &mut Value) {
    if let Some(obj) = schema.as_object_mut() {
        obj.remove("if");
        obj.remove("then");
        obj.remove("else");
    }
    recurse_into_subschemas(schema, strip_conditional_keywords);
}

/// Adds `"type": "object"` when `properties` exists without a `type` field.
///
/// Some schema authors omit the explicit `type` when `properties` is present.
/// Most LLM providers require the `type` field to be explicit.
///
/// # Example
///
/// ```rust
/// use serde_json::json;
/// use adk_core::schema_utils::add_implicit_object_type;
///
/// let mut schema = json!({
///     "properties": {
///         "name": { "type": "string" }
///     }
/// });
///
/// add_implicit_object_type(&mut schema);
/// assert_eq!(schema["type"], "object");
/// ```
pub fn add_implicit_object_type(schema: &mut Value) {
    if let Some(obj) = schema.as_object_mut() {
        if obj.contains_key("properties") && !obj.contains_key("type") {
            obj.insert("type".to_string(), Value::String("object".to_string()));
        }
    }
    recurse_into_subschemas(schema, add_implicit_object_type);
}

/// Converts `const` values to single-element `enum` arrays.
///
/// Providers that do not support the `const` keyword can still enforce fixed
/// values via a single-element `enum`.
///
/// # Example
///
/// ```rust
/// use serde_json::json;
/// use adk_core::schema_utils::convert_const_to_enum;
///
/// let mut schema = json!({
///     "type": "string",
///     "const": "fixed_value"
/// });
///
/// convert_const_to_enum(&mut schema);
/// assert!(schema.get("const").is_none());
/// assert_eq!(schema["enum"], json!(["fixed_value"]));
/// ```
pub fn convert_const_to_enum(schema: &mut Value) {
    if let Some(obj) = schema.as_object_mut() {
        if let Some(const_val) = obj.remove("const") {
            obj.insert("enum".to_string(), Value::Array(vec![const_val]));
        }
    }
    recurse_into_subschemas(schema, convert_const_to_enum);
}

/// Removes `format` values not in the `allowed` list from the schema and all
/// nested sub-schemas.
///
/// Some providers reject schemas with unsupported format annotations. This
/// function strips any `format` value not present in the provided allowlist.
///
/// # Arguments
///
/// * `schema` - The schema to transform in place.
/// * `allowed` - Slice of allowed format strings (e.g., `&["date-time", "email"]`).
///
/// # Example
///
/// ```rust
/// use serde_json::json;
/// use adk_core::schema_utils::strip_unsupported_formats;
///
/// let mut schema = json!({
///     "type": "string",
///     "format": "hostname"
/// });
///
/// strip_unsupported_formats(&mut schema, &["date-time", "email", "uri"]);
/// assert!(schema.get("format").is_none());
/// ```
pub fn strip_unsupported_formats(schema: &mut Value, allowed: &[&str]) {
    if let Some(obj) = schema.as_object_mut() {
        let should_remove =
            obj.get("format").and_then(|f| f.as_str()).is_some_and(|f| !allowed.contains(&f));
        if should_remove {
            obj.remove("format");
        }
    }
    // Recurse manually since we need to pass `allowed` through
    recurse_into_subschemas_with_context(schema, allowed, strip_unsupported_formats);
}

/// Truncates a tool name to at most `max_bytes` bytes, preserving valid UTF-8.
///
/// If the name is already within the limit, returns a borrowed reference.
/// Otherwise, truncates at the nearest character boundary at or before
/// `max_bytes` and returns an owned string.
///
/// # Arguments
///
/// * `name` - The tool name to potentially truncate.
/// * `max_bytes` - Maximum byte length for the result.
///
/// # Example
///
/// ```rust
/// use adk_core::schema_utils::truncate_tool_name;
///
/// let short = truncate_tool_name("hello", 64);
/// assert_eq!(short, "hello");
///
/// let long = "a".repeat(100);
/// let truncated = truncate_tool_name(&long, 64);
/// assert_eq!(truncated.len(), 64);
/// ```
pub fn truncate_tool_name(name: &str, max_bytes: usize) -> Cow<'_, str> {
    if name.len() <= max_bytes {
        Cow::Borrowed(name)
    } else {
        // Find the nearest char boundary at or before max_bytes
        let mut end = max_bytes;
        while end > 0 && !name.is_char_boundary(end) {
            end -= 1;
        }
        Cow::Owned(name[..end].to_string())
    }
}

/// Removes JSON `null` values from `enum` arrays in the schema and all nested
/// sub-schemas.
///
/// If removing null results in an empty `enum` array, the `enum` keyword is
/// removed entirely.
///
/// # Example
///
/// ```rust
/// use serde_json::json;
/// use adk_core::schema_utils::strip_null_from_enum;
///
/// let mut schema = json!({
///     "type": "string",
///     "enum": ["a", null, "b"]
/// });
///
/// strip_null_from_enum(&mut schema);
/// assert_eq!(schema["enum"], json!(["a", "b"]));
/// ```
pub fn strip_null_from_enum(schema: &mut Value) {
    if let Some(obj) = schema.as_object_mut() {
        if let Some(enum_val) = obj.get_mut("enum") {
            if let Some(arr) = enum_val.as_array_mut() {
                arr.retain(|v| !v.is_null());
                if arr.is_empty() {
                    obj.remove("enum");
                }
            }
        }
    }
    recurse_into_subschemas(schema, strip_null_from_enum);
}

/// Resolves `$ref` references by inlining the referenced sub-schema from a
/// definitions map.
///
/// Handles both `#/definitions/<name>` (Draft 4–7) and `#/$defs/<name>`
/// (Draft 2019-09+) reference formats. Unresolvable references are replaced
/// with `{"type": "object"}`. Circular reference chains are broken by
/// replacing the schema with `{"type": "object"}` when `depth` exceeds 10.
///
/// After resolving a `$ref`, the function recurses into the inlined schema
/// (incrementing depth) to resolve any nested references. When no `$ref` is
/// present, the function recurses into all sub-schemas (properties, items,
/// allOf, anyOf, oneOf, etc.).
///
/// # Arguments
///
/// * `schema` - The schema to transform in place.
/// * `definitions` - A map of definition names to their sub-schemas (combined
///   from both `definitions` and `$defs` at the top level).
/// * `depth` - Current recursion depth. Pass `0` for the initial call.
///
/// # Example
///
/// ```rust
/// use serde_json::{json, Map, Value};
/// use adk_core::schema_utils::resolve_refs;
///
/// let mut defs = Map::new();
/// defs.insert("Address".to_string(), json!({"type": "object", "properties": {"street": {"type": "string"}}}));
///
/// let mut schema = json!({
///     "type": "object",
///     "properties": {
///         "home": { "$ref": "#/definitions/Address" }
///     }
/// });
///
/// resolve_refs(&mut schema, &defs, 0);
/// assert_eq!(schema["properties"]["home"]["type"], "object");
/// assert!(schema["properties"]["home"].get("$ref").is_none());
/// ```
pub fn resolve_refs(schema: &mut Value, definitions: &Map<String, Value>, depth: usize) {
    // Break circular chains at max depth 10
    if depth > 10 {
        // Only replace if this schema itself has a $ref (circular chain detected)
        if schema.as_object().is_some_and(|obj| obj.contains_key("$ref")) {
            *schema = serde_json::json!({"type": "object"});
        }
        return;
    }

    let Some(obj) = schema.as_object() else {
        return;
    };

    if let Some(ref_val) = obj.get("$ref").and_then(|v| v.as_str()) {
        // Parse the ref path: #/definitions/<name> or #/$defs/<name>
        let name =
            ref_val.strip_prefix("#/definitions/").or_else(|| ref_val.strip_prefix("#/$defs/"));

        if let Some(def_name) = name {
            if let Some(def_schema) = definitions.get(def_name) {
                // Replace the entire schema node with the referenced sub-schema
                *schema = def_schema.clone();
            } else {
                // Unresolvable ref — replace with fallback
                *schema = serde_json::json!({"type": "object"});
            }
        } else {
            // Unsupported ref format — replace with fallback
            *schema = serde_json::json!({"type": "object"});
        }

        // Recursively resolve refs in the inlined schema
        resolve_refs(schema, definitions, depth + 1);
    } else {
        // No $ref — recurse into all sub-schemas
        resolve_refs_recurse(schema, definitions, depth);
    }
}

/// Recurses into all sub-schema locations to resolve nested `$ref` references.
fn resolve_refs_recurse(schema: &mut Value, definitions: &Map<String, Value>, depth: usize) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    // properties
    if let Some(props) = obj.get_mut("properties") {
        if let Some(props_obj) = props.as_object_mut() {
            for value in props_obj.values_mut() {
                resolve_refs(value, definitions, depth);
            }
        }
    }

    // items (single schema or array)
    if let Some(items) = obj.get_mut("items") {
        if items.is_object() {
            resolve_refs(items, definitions, depth);
        } else if let Some(arr) = items.as_array_mut() {
            for item in arr.iter_mut() {
                resolve_refs(item, definitions, depth);
            }
        }
    }

    // additionalProperties (when it's a schema object, not a boolean)
    if let Some(additional) = obj.get_mut("additionalProperties") {
        if additional.is_object() {
            resolve_refs(additional, definitions, depth);
        }
    }

    // allOf, anyOf, oneOf
    for keyword in &["allOf", "anyOf", "oneOf"] {
        if let Some(arr_val) = obj.get_mut(*keyword) {
            if let Some(arr) = arr_val.as_array_mut() {
                for sub in arr.iter_mut() {
                    resolve_refs(sub, definitions, depth);
                }
            }
        }
    }

    // not
    if let Some(not_schema) = obj.get_mut("not") {
        if not_schema.is_object() {
            resolve_refs(not_schema, definitions, depth);
        }
    }

    // patternProperties
    if let Some(pattern_props) = obj.get_mut("patternProperties") {
        if let Some(pp_obj) = pattern_props.as_object_mut() {
            for value in pp_obj.values_mut() {
                resolve_refs(value, definitions, depth);
            }
        }
    }

    // prefixItems
    if let Some(prefix_items) = obj.get_mut("prefixItems") {
        if let Some(arr) = prefix_items.as_array_mut() {
            for item in arr.iter_mut() {
                resolve_refs(item, definitions, depth);
            }
        }
    }

    // if, then, else
    for keyword in &["if", "then", "else"] {
        if let Some(sub) = obj.get_mut(*keyword) {
            if sub.is_object() {
                resolve_refs(sub, definitions, depth);
            }
        }
    }
}

/// Recursively applies a transform function to all nested sub-schemas.
///
/// This traverses into:
/// - `properties` (each property value)
/// - `items` (single schema or array of schemas)
/// - `additionalProperties` (when it's a schema object)
/// - `allOf`, `anyOf`, `oneOf` (each sub-schema in the array)
/// - `not` (single sub-schema)
/// - `patternProperties` (each property value)
/// - `prefixItems` (each sub-schema in the array)
/// - `if`, `then`, `else` (single sub-schemas)
fn recurse_into_subschemas(schema: &mut Value, transform: fn(&mut Value)) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    // properties
    if let Some(props) = obj.get_mut("properties") {
        if let Some(props_obj) = props.as_object_mut() {
            for value in props_obj.values_mut() {
                transform(value);
            }
        }
    }

    // items (single schema or array)
    if let Some(items) = obj.get_mut("items") {
        if items.is_object() {
            transform(items);
        } else if let Some(arr) = items.as_array_mut() {
            for item in arr.iter_mut() {
                transform(item);
            }
        }
    }

    // additionalProperties (when it's a schema object, not a boolean)
    if let Some(additional) = obj.get_mut("additionalProperties") {
        if additional.is_object() {
            transform(additional);
        }
    }

    // allOf, anyOf, oneOf
    for keyword in &["allOf", "anyOf", "oneOf"] {
        if let Some(arr_val) = obj.get_mut(*keyword) {
            if let Some(arr) = arr_val.as_array_mut() {
                for sub in arr.iter_mut() {
                    transform(sub);
                }
            }
        }
    }

    // not
    if let Some(not_schema) = obj.get_mut("not") {
        if not_schema.is_object() {
            transform(not_schema);
        }
    }

    // patternProperties
    if let Some(pattern_props) = obj.get_mut("patternProperties") {
        if let Some(pp_obj) = pattern_props.as_object_mut() {
            for value in pp_obj.values_mut() {
                transform(value);
            }
        }
    }

    // prefixItems
    if let Some(prefix_items) = obj.get_mut("prefixItems") {
        if let Some(arr) = prefix_items.as_array_mut() {
            for item in arr.iter_mut() {
                transform(item);
            }
        }
    }

    // if, then, else (for transforms that don't strip them)
    for keyword in &["if", "then", "else"] {
        if let Some(sub) = obj.get_mut(*keyword) {
            if sub.is_object() {
                transform(sub);
            }
        }
    }
}

/// Recursively applies a transform function with additional context to all nested sub-schemas.
///
/// Same traversal as [`recurse_into_subschemas`] but passes a context parameter through.
fn recurse_into_subschemas_with_context<C: ?Sized>(
    schema: &mut Value,
    ctx: &C,
    transform: fn(&mut Value, &C),
) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    // properties
    if let Some(props) = obj.get_mut("properties") {
        if let Some(props_obj) = props.as_object_mut() {
            for value in props_obj.values_mut() {
                transform(value, ctx);
            }
        }
    }

    // items (single schema or array)
    if let Some(items) = obj.get_mut("items") {
        if items.is_object() {
            transform(items, ctx);
        } else if let Some(arr) = items.as_array_mut() {
            for item in arr.iter_mut() {
                transform(item, ctx);
            }
        }
    }

    // additionalProperties (when it's a schema object, not a boolean)
    if let Some(additional) = obj.get_mut("additionalProperties") {
        if additional.is_object() {
            transform(additional, ctx);
        }
    }

    // allOf, anyOf, oneOf
    for keyword in &["allOf", "anyOf", "oneOf"] {
        if let Some(arr_val) = obj.get_mut(*keyword) {
            if let Some(arr) = arr_val.as_array_mut() {
                for sub in arr.iter_mut() {
                    transform(sub, ctx);
                }
            }
        }
    }

    // not
    if let Some(not_schema) = obj.get_mut("not") {
        if not_schema.is_object() {
            transform(not_schema, ctx);
        }
    }

    // patternProperties
    if let Some(pattern_props) = obj.get_mut("patternProperties") {
        if let Some(pp_obj) = pattern_props.as_object_mut() {
            for value in pp_obj.values_mut() {
                transform(value, ctx);
            }
        }
    }

    // prefixItems
    if let Some(prefix_items) = obj.get_mut("prefixItems") {
        if let Some(arr) = prefix_items.as_array_mut() {
            for item in arr.iter_mut() {
                transform(item, ctx);
            }
        }
    }

    // if, then, else
    for keyword in &["if", "then", "else"] {
        if let Some(sub) = obj.get_mut(*keyword) {
            if sub.is_object() {
                transform(sub, ctx);
            }
        }
    }
}

/// Collapses `anyOf`/`oneOf` arrays to the first non-null sub-schema.
///
/// For each `anyOf` or `oneOf` array, finds the first sub-schema that is NOT
/// `{"type": "null"}`, merges its fields into the parent schema, and removes
/// the combiner key. If all sub-schemas are null, uses the first one.
///
/// Recurses into nested schemas after collapsing.
///
/// # Example
///
/// ```rust
/// use serde_json::json;
/// use adk_core::schema_utils::collapse_combiners;
///
/// let mut schema = json!({
///     "anyOf": [
///         {"type": "null"},
///         {"type": "string", "minLength": 1}
///     ]
/// });
///
/// collapse_combiners(&mut schema);
/// assert_eq!(schema["type"], "string");
/// assert_eq!(schema["minLength"], 1);
/// assert!(schema.get("anyOf").is_none());
/// ```
pub fn collapse_combiners(schema: &mut Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    for keyword in &["anyOf", "oneOf"] {
        if let Some(arr_val) = obj.remove(*keyword) {
            if let Some(arr) = arr_val.as_array() {
                // Find the first non-null sub-schema
                let chosen = arr.iter().find(|sub| !is_null_schema(sub)).or_else(|| arr.first());

                if let Some(chosen_schema) = chosen {
                    if let Some(chosen_obj) = chosen_schema.as_object() {
                        // Merge chosen sub-schema fields into parent
                        for (key, value) in chosen_obj {
                            obj.insert(key.clone(), value.clone());
                        }
                    }
                }
            }
            // Only process one combiner keyword per level
            break;
        }
    }

    recurse_into_subschemas(schema, collapse_combiners);
}

/// Merges `allOf` arrays by combining all sub-schemas into a single schema.
///
/// Combines `properties`, `required`, and other fields from all sub-schemas.
/// If `type` conflicts across sub-schemas, prefers `"object"`. Removes the
/// `allOf` key after merging.
///
/// Recurses into nested schemas after merging.
///
/// # Example
///
/// ```rust
/// use serde_json::json;
/// use adk_core::schema_utils::merge_all_of;
///
/// let mut schema = json!({
///     "allOf": [
///         {"type": "object", "properties": {"a": {"type": "string"}}},
///         {"properties": {"b": {"type": "number"}}, "required": ["b"]}
///     ]
/// });
///
/// merge_all_of(&mut schema);
/// assert!(schema.get("allOf").is_none());
/// assert_eq!(schema["properties"]["a"]["type"], "string");
/// assert_eq!(schema["properties"]["b"]["type"], "number");
/// assert_eq!(schema["required"], json!(["b"]));
/// ```
pub fn merge_all_of(schema: &mut Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    if let Some(arr_val) = obj.remove("allOf") {
        if let Some(arr) = arr_val.as_array() {
            let mut merged_properties = Map::new();
            let mut merged_required: Vec<Value> = Vec::new();
            let mut merged_type: Option<Value> = None;
            let mut other_fields = Map::new();

            for sub in arr {
                let Some(sub_obj) = sub.as_object() else {
                    continue;
                };

                for (key, value) in sub_obj {
                    match key.as_str() {
                        "properties" => {
                            if let Some(props) = value.as_object() {
                                for (pk, pv) in props {
                                    merged_properties.insert(pk.clone(), pv.clone());
                                }
                            }
                        }
                        "required" => {
                            if let Some(req_arr) = value.as_array() {
                                for item in req_arr {
                                    if !merged_required.contains(item) {
                                        merged_required.push(item.clone());
                                    }
                                }
                            }
                        }
                        "type" => {
                            if let Some(existing) = &merged_type {
                                // Conflict: prefer "object"
                                if existing != value {
                                    merged_type = Some(Value::String("object".to_string()));
                                }
                            } else {
                                merged_type = Some(value.clone());
                            }
                        }
                        _ => {
                            other_fields.insert(key.clone(), value.clone());
                        }
                    }
                }
            }

            // Merge other fields first (lower priority)
            for (key, value) in other_fields {
                obj.entry(key).or_insert(value);
            }

            // Merge type
            if let Some(type_val) = merged_type {
                obj.insert("type".to_string(), type_val);
            }

            // Merge properties
            if !merged_properties.is_empty() {
                let existing_props =
                    obj.entry("properties").or_insert_with(|| Value::Object(Map::new()));
                if let Some(existing_obj) = existing_props.as_object_mut() {
                    for (key, value) in merged_properties {
                        existing_obj.insert(key, value);
                    }
                }
            }

            // Merge required
            if !merged_required.is_empty() {
                let existing_required =
                    obj.entry("required").or_insert_with(|| Value::Array(Vec::new()));
                if let Some(existing_arr) = existing_required.as_array_mut() {
                    for item in merged_required {
                        if !existing_arr.contains(&item) {
                            existing_arr.push(item);
                        }
                    }
                }
            }
        }
    }

    recurse_into_subschemas(schema, merge_all_of);
}

/// Collapses type arrays to the first non-null type string.
///
/// When `type` is an array like `["string", "null"]`, collapses to the first
/// non-null type string (e.g., `"string"`). If all types are `"null"`, uses
/// `"null"`.
///
/// Recurses into nested schemas.
///
/// # Example
///
/// ```rust
/// use serde_json::json;
/// use adk_core::schema_utils::collapse_type_arrays;
///
/// let mut schema = json!({
///     "type": ["string", "null"]
/// });
///
/// collapse_type_arrays(&mut schema);
/// assert_eq!(schema["type"], "string");
/// ```
pub fn collapse_type_arrays(schema: &mut Value) {
    if let Some(obj) = schema.as_object_mut() {
        if let Some(type_val) = obj.get("type").cloned() {
            if let Some(arr) = type_val.as_array() {
                let chosen =
                    arr.iter().find(|t| t.as_str() != Some("null")).or_else(|| arr.first());

                if let Some(chosen_type) = chosen {
                    obj.insert("type".to_string(), chosen_type.clone());
                }
            }
        }
    }
    recurse_into_subschemas(schema, collapse_type_arrays);
}

/// Enforces a maximum nesting depth for object schemas.
///
/// Tracks nesting depth through object schemas. When `current >= max_depth`,
/// replaces the schema with `{"type": "object"}` and emits a `tracing::warn!()`
/// log. Recurses into properties/items/etc, incrementing depth for object schemas.
///
/// # Arguments
///
/// * `schema` - The schema to enforce depth on.
/// * `max_depth` - Maximum allowed nesting depth.
/// * `current` - Current depth (start at 0).
///
/// # Example
///
/// ```rust
/// use serde_json::json;
/// use adk_core::schema_utils::enforce_nesting_depth;
///
/// let mut schema = json!({
///     "type": "object",
///     "properties": {
///         "level1": {
///             "type": "object",
///             "properties": {
///                 "level2": { "type": "string" }
///             }
///         }
///     }
/// });
///
/// enforce_nesting_depth(&mut schema, 1, 0);
/// // level1 is at depth 1, so it gets replaced
/// assert_eq!(schema["properties"]["level1"], json!({"type": "object"}));
/// ```
pub fn enforce_nesting_depth(schema: &mut Value, max_depth: usize, current: usize) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    let is_object_schema = obj.get("type").and_then(|t| t.as_str()).is_some_and(|t| t == "object")
        || obj.contains_key("properties");

    if is_object_schema && current >= max_depth {
        tracing::warn!(
            depth = current,
            max_depth,
            "schema nesting depth exceeded, truncating to {{\"type\": \"object\"}}"
        );
        *schema = serde_json::json!({"type": "object"});
        return;
    }

    let next_depth = if is_object_schema { current + 1 } else { current };

    // Recurse into properties
    if let Some(props) = obj.get_mut("properties") {
        if let Some(props_obj) = props.as_object_mut() {
            for value in props_obj.values_mut() {
                enforce_nesting_depth(value, max_depth, next_depth);
            }
        }
    }

    // Recurse into items
    if let Some(items) = obj.get_mut("items") {
        if items.is_object() {
            enforce_nesting_depth(items, max_depth, next_depth);
        } else if let Some(arr) = items.as_array_mut() {
            for item in arr.iter_mut() {
                enforce_nesting_depth(item, max_depth, next_depth);
            }
        }
    }

    // Recurse into additionalProperties
    if let Some(additional) = obj.get_mut("additionalProperties") {
        if additional.is_object() {
            enforce_nesting_depth(additional, max_depth, next_depth);
        }
    }

    // Recurse into allOf, anyOf, oneOf
    for keyword in &["allOf", "anyOf", "oneOf"] {
        if let Some(arr_val) = obj.get_mut(*keyword) {
            if let Some(arr) = arr_val.as_array_mut() {
                for sub in arr.iter_mut() {
                    enforce_nesting_depth(sub, max_depth, next_depth);
                }
            }
        }
    }

    // Recurse into not
    if let Some(not_schema) = obj.get_mut("not") {
        if not_schema.is_object() {
            enforce_nesting_depth(not_schema, max_depth, next_depth);
        }
    }

    // Recurse into patternProperties
    if let Some(pattern_props) = obj.get_mut("patternProperties") {
        if let Some(pp_obj) = pattern_props.as_object_mut() {
            for value in pp_obj.values_mut() {
                enforce_nesting_depth(value, max_depth, next_depth);
            }
        }
    }
}

/// Returns `true` if the schema represents a null type.
///
/// Matches `{"type": "null"}` exactly (with no other fields) or schemas
/// where the only meaningful content is `type: null`.
fn is_null_schema(schema: &Value) -> bool {
    schema
        .as_object()
        .and_then(|obj| obj.get("type"))
        .and_then(|t| t.as_str())
        .is_some_and(|t| t == "null")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- strip_schema_keyword tests ---

    #[test]
    fn test_strip_schema_keyword_top_level() {
        let mut schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object"
        });
        strip_schema_keyword(&mut schema);
        assert!(schema.get("$schema").is_none());
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn test_strip_schema_keyword_nested() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "child": {
                    "$schema": "http://json-schema.org/draft-07/schema#",
                    "type": "string"
                }
            }
        });
        strip_schema_keyword(&mut schema);
        assert!(schema["properties"]["child"].get("$schema").is_none());
    }

    #[test]
    fn test_strip_schema_keyword_no_op_when_absent() {
        let mut schema = json!({"type": "string"});
        let expected = schema.clone();
        strip_schema_keyword(&mut schema);
        assert_eq!(schema, expected);
    }

    // --- strip_conditional_keywords tests ---

    #[test]
    fn test_strip_conditional_keywords() {
        let mut schema = json!({
            "type": "object",
            "if": { "properties": { "kind": { "const": "a" } } },
            "then": { "required": ["extra"] },
            "else": { "required": [] },
            "properties": { "kind": { "type": "string" } }
        });
        strip_conditional_keywords(&mut schema);
        assert!(schema.get("if").is_none());
        assert!(schema.get("then").is_none());
        assert!(schema.get("else").is_none());
        assert!(schema.get("properties").is_some());
    }

    #[test]
    fn test_strip_conditional_keywords_nested() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "child": {
                    "type": "object",
                    "if": { "const": true },
                    "then": { "type": "string" }
                }
            }
        });
        strip_conditional_keywords(&mut schema);
        assert!(schema["properties"]["child"].get("if").is_none());
        assert!(schema["properties"]["child"].get("then").is_none());
    }

    // --- add_implicit_object_type tests ---

    #[test]
    fn test_add_implicit_object_type() {
        let mut schema = json!({
            "properties": {
                "name": { "type": "string" }
            }
        });
        add_implicit_object_type(&mut schema);
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn test_add_implicit_object_type_no_op_when_type_present() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            }
        });
        let expected = schema.clone();
        add_implicit_object_type(&mut schema);
        assert_eq!(schema, expected);
    }

    #[test]
    fn test_add_implicit_object_type_nested() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "nested": {
                    "properties": {
                        "field": { "type": "number" }
                    }
                }
            }
        });
        add_implicit_object_type(&mut schema);
        assert_eq!(schema["properties"]["nested"]["type"], "object");
    }

    // --- convert_const_to_enum tests ---

    #[test]
    fn test_convert_const_to_enum() {
        let mut schema = json!({
            "type": "string",
            "const": "fixed"
        });
        convert_const_to_enum(&mut schema);
        assert!(schema.get("const").is_none());
        assert_eq!(schema["enum"], json!(["fixed"]));
    }

    #[test]
    fn test_convert_const_to_enum_null() {
        let mut schema = json!({
            "const": null
        });
        convert_const_to_enum(&mut schema);
        assert!(schema.get("const").is_none());
        assert_eq!(schema["enum"], json!([null]));
    }

    #[test]
    fn test_convert_const_to_enum_nested() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "const": "active"
                }
            }
        });
        convert_const_to_enum(&mut schema);
        assert_eq!(schema["properties"]["status"]["enum"], json!(["active"]));
    }

    // --- strip_unsupported_formats tests ---

    #[test]
    fn test_strip_unsupported_formats_removes_unsupported() {
        let mut schema = json!({
            "type": "string",
            "format": "hostname"
        });
        strip_unsupported_formats(&mut schema, &["date-time", "email"]);
        assert!(schema.get("format").is_none());
    }

    #[test]
    fn test_strip_unsupported_formats_keeps_allowed() {
        let mut schema = json!({
            "type": "string",
            "format": "email"
        });
        strip_unsupported_formats(&mut schema, &["date-time", "email"]);
        assert_eq!(schema["format"], "email");
    }

    #[test]
    fn test_strip_unsupported_formats_nested() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "created": { "type": "string", "format": "date-time" },
                "hostname": { "type": "string", "format": "hostname" }
            }
        });
        strip_unsupported_formats(&mut schema, &["date-time"]);
        assert_eq!(schema["properties"]["created"]["format"], "date-time");
        assert!(schema["properties"]["hostname"].get("format").is_none());
    }

    // --- truncate_tool_name tests ---

    #[test]
    fn test_truncate_tool_name_short() {
        let result = truncate_tool_name("short_name", 64);
        assert_eq!(result, "short_name");
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn test_truncate_tool_name_exact_boundary() {
        let name = "a".repeat(64);
        let result = truncate_tool_name(&name, 64);
        assert_eq!(result.len(), 64);
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn test_truncate_tool_name_over_limit() {
        let name = "a".repeat(100);
        let result = truncate_tool_name(&name, 64);
        assert_eq!(result.len(), 64);
        assert!(matches!(result, Cow::Owned(_)));
    }

    #[test]
    fn test_truncate_tool_name_multibyte_boundary() {
        // "é" is 2 bytes in UTF-8. Create a string where byte 64 falls mid-character.
        let name = "a".repeat(63) + "é"; // 63 + 2 = 65 bytes
        let result = truncate_tool_name(&name, 64);
        // Should truncate to 63 bytes (before the multi-byte char)
        assert_eq!(result.len(), 63);
        assert!(result.is_char_boundary(result.len()));
    }

    #[test]
    fn test_truncate_tool_name_emoji() {
        // "🎯" is 4 bytes. Create a string where byte 64 falls mid-emoji.
        let name = "a".repeat(62) + "🎯"; // 62 + 4 = 66 bytes
        let result = truncate_tool_name(&name, 64);
        // Should truncate to 62 bytes (before the emoji)
        assert_eq!(result.len(), 62);
    }

    #[test]
    fn test_truncate_tool_name_empty() {
        let result = truncate_tool_name("", 64);
        assert_eq!(result, "");
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    // --- strip_null_from_enum tests ---

    #[test]
    fn test_strip_null_from_enum() {
        let mut schema = json!({
            "type": "string",
            "enum": ["a", null, "b"]
        });
        strip_null_from_enum(&mut schema);
        assert_eq!(schema["enum"], json!(["a", "b"]));
    }

    #[test]
    fn test_strip_null_from_enum_all_null() {
        let mut schema = json!({
            "type": "string",
            "enum": [null]
        });
        strip_null_from_enum(&mut schema);
        assert!(schema.get("enum").is_none());
    }

    #[test]
    fn test_strip_null_from_enum_no_null() {
        let mut schema = json!({
            "type": "string",
            "enum": ["a", "b"]
        });
        let expected = schema.clone();
        strip_null_from_enum(&mut schema);
        assert_eq!(schema, expected);
    }

    #[test]
    fn test_strip_null_from_enum_nested() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "enum": ["active", null, "inactive"]
                }
            }
        });
        strip_null_from_enum(&mut schema);
        assert_eq!(schema["properties"]["status"]["enum"], json!(["active", "inactive"]));
    }

    // --- Recursion into combiners tests ---

    #[test]
    fn test_recursion_into_any_of() {
        let mut schema = json!({
            "anyOf": [
                { "$schema": "draft-07", "type": "string" },
                { "$schema": "draft-07", "type": "number" }
            ]
        });
        strip_schema_keyword(&mut schema);
        assert!(schema["anyOf"][0].get("$schema").is_none());
        assert!(schema["anyOf"][1].get("$schema").is_none());
    }

    #[test]
    fn test_recursion_into_all_of() {
        let mut schema = json!({
            "allOf": [
                { "properties": { "a": { "type": "string" } } },
                { "properties": { "b": { "type": "number" } } }
            ]
        });
        add_implicit_object_type(&mut schema);
        assert_eq!(schema["allOf"][0]["type"], "object");
        assert_eq!(schema["allOf"][1]["type"], "object");
    }

    #[test]
    fn test_recursion_into_items() {
        let mut schema = json!({
            "type": "array",
            "items": {
                "$schema": "draft-07",
                "type": "string",
                "format": "hostname"
            }
        });
        strip_schema_keyword(&mut schema);
        strip_unsupported_formats(&mut schema, &["date-time"]);
        assert!(schema["items"].get("$schema").is_none());
        assert!(schema["items"].get("format").is_none());
    }

    #[test]
    fn test_recursion_into_additional_properties() {
        let mut schema = json!({
            "type": "object",
            "additionalProperties": {
                "$schema": "draft-07",
                "type": "string"
            }
        });
        strip_schema_keyword(&mut schema);
        assert!(schema["additionalProperties"].get("$schema").is_none());
    }

    #[test]
    fn test_recursion_into_not() {
        let mut schema = json!({
            "not": {
                "$schema": "draft-07",
                "type": "null"
            }
        });
        strip_schema_keyword(&mut schema);
        assert!(schema["not"].get("$schema").is_none());
    }

    #[test]
    fn test_deeply_nested_recursion() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "level1": {
                    "type": "object",
                    "properties": {
                        "level2": {
                            "type": "object",
                            "properties": {
                                "level3": {
                                    "$schema": "draft-07",
                                    "type": "string",
                                    "const": "deep"
                                }
                            }
                        }
                    }
                }
            }
        });
        strip_schema_keyword(&mut schema);
        convert_const_to_enum(&mut schema);
        let deep = &schema["properties"]["level1"]["properties"]["level2"]["properties"]["level3"];
        assert!(deep.get("$schema").is_none());
        assert_eq!(deep["enum"], json!(["deep"]));
    }

    // --- resolve_refs tests ---

    #[test]
    fn test_resolve_refs_simple_definitions() {
        let mut defs = Map::new();
        defs.insert(
            "Address".to_string(),
            json!({"type": "object", "properties": {"street": {"type": "string"}}}),
        );

        let mut schema = json!({
            "type": "object",
            "properties": {
                "home": { "$ref": "#/definitions/Address" }
            }
        });

        resolve_refs(&mut schema, &defs, 0);
        assert_eq!(schema["properties"]["home"]["type"], "object");
        assert!(schema["properties"]["home"].get("$ref").is_none());
        assert_eq!(schema["properties"]["home"]["properties"]["street"]["type"], "string");
    }

    #[test]
    fn test_resolve_refs_simple_defs_format() {
        let mut defs = Map::new();
        defs.insert("Name".to_string(), json!({"type": "string", "minLength": 1}));

        let mut schema = json!({
            "type": "object",
            "properties": {
                "name": { "$ref": "#/$defs/Name" }
            }
        });

        resolve_refs(&mut schema, &defs, 0);
        assert_eq!(schema["properties"]["name"]["type"], "string");
        assert_eq!(schema["properties"]["name"]["minLength"], 1);
        assert!(schema["properties"]["name"].get("$ref").is_none());
    }

    #[test]
    fn test_resolve_refs_nested_refs() {
        let mut defs = Map::new();
        defs.insert("Inner".to_string(), json!({"type": "string"}));
        defs.insert(
            "Outer".to_string(),
            json!({
                "type": "object",
                "properties": {
                    "value": { "$ref": "#/definitions/Inner" }
                }
            }),
        );

        let mut schema = json!({
            "type": "object",
            "properties": {
                "wrapper": { "$ref": "#/definitions/Outer" }
            }
        });

        resolve_refs(&mut schema, &defs, 0);
        // Outer was inlined
        assert_eq!(schema["properties"]["wrapper"]["type"], "object");
        // Inner was also resolved within Outer
        assert_eq!(schema["properties"]["wrapper"]["properties"]["value"]["type"], "string");
        assert!(schema["properties"]["wrapper"]["properties"]["value"].get("$ref").is_none());
    }

    #[test]
    fn test_resolve_refs_unresolvable_ref() {
        let defs = Map::new(); // empty definitions

        let mut schema = json!({
            "type": "object",
            "properties": {
                "missing": { "$ref": "#/definitions/DoesNotExist" }
            }
        });

        resolve_refs(&mut schema, &defs, 0);
        // Unresolvable ref replaced with {"type": "object"}
        assert_eq!(schema["properties"]["missing"], json!({"type": "object"}));
    }

    #[test]
    fn test_resolve_refs_unsupported_ref_format() {
        let defs = Map::new();

        let mut schema = json!({
            "type": "object",
            "properties": {
                "external": { "$ref": "https://example.com/schema.json" }
            }
        });

        resolve_refs(&mut schema, &defs, 0);
        // Unsupported ref format replaced with {"type": "object"}
        assert_eq!(schema["properties"]["external"], json!({"type": "object"}));
    }

    #[test]
    fn test_resolve_refs_circular_self_reference() {
        let mut defs = Map::new();
        defs.insert(
            "Node".to_string(),
            json!({
                "type": "object",
                "properties": {
                    "child": { "$ref": "#/definitions/Node" }
                }
            }),
        );

        let mut schema = json!({ "$ref": "#/definitions/Node" });

        resolve_refs(&mut schema, &defs, 0);
        // The schema should resolve but eventually hit depth limit
        assert_eq!(schema["type"], "object");
        // At some nesting level, the circular ref should be broken
        // Walk down the chain to verify termination
        let mut current = &schema;
        let mut found_termination = false;
        for _ in 0..15 {
            if let Some(child) = current.get("properties").and_then(|p| p.get("child")) {
                if child == &json!({"type": "object"}) {
                    found_termination = true;
                    break;
                }
                current = child;
            } else {
                found_termination = true;
                break;
            }
        }
        assert!(found_termination, "circular ref chain should terminate within depth limit");
    }

    #[test]
    fn test_resolve_refs_mutual_circular_reference() {
        let mut defs = Map::new();
        defs.insert(
            "A".to_string(),
            json!({
                "type": "object",
                "properties": {
                    "b": { "$ref": "#/definitions/B" }
                }
            }),
        );
        defs.insert(
            "B".to_string(),
            json!({
                "type": "object",
                "properties": {
                    "a": { "$ref": "#/definitions/A" }
                }
            }),
        );

        let mut schema = json!({ "$ref": "#/definitions/A" });

        resolve_refs(&mut schema, &defs, 0);
        // Should terminate without stack overflow
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn test_resolve_refs_depth_limit_exact() {
        // Starting at depth 11 with a $ref should replace with fallback
        let mut defs = Map::new();
        defs.insert("Foo".to_string(), json!({"type": "number"}));

        let mut schema = json!({ "$ref": "#/definitions/Foo" });

        resolve_refs(&mut schema, &defs, 11);
        // At depth > 10 with a $ref, it should be replaced with fallback
        assert_eq!(schema, json!({"type": "object"}));
    }

    #[test]
    fn test_resolve_refs_depth_limit_no_ref_passthrough() {
        // Starting at depth 11 without a $ref should leave schema unchanged
        let defs = Map::new();
        let mut schema = json!({"type": "string", "minLength": 5});
        let expected = schema.clone();

        resolve_refs(&mut schema, &defs, 11);
        assert_eq!(schema, expected);
    }

    #[test]
    fn test_resolve_refs_depth_10_still_resolves() {
        let mut defs = Map::new();
        defs.insert("Foo".to_string(), json!({"type": "number"}));

        let mut schema = json!({ "$ref": "#/definitions/Foo" });

        // At depth 10, should still resolve (limit is > 10)
        resolve_refs(&mut schema, &defs, 10);
        assert_eq!(schema, json!({"type": "number"}));
    }

    #[test]
    fn test_resolve_refs_in_array_items() {
        let mut defs = Map::new();
        defs.insert("Item".to_string(), json!({"type": "string"}));

        let mut schema = json!({
            "type": "array",
            "items": { "$ref": "#/definitions/Item" }
        });

        resolve_refs(&mut schema, &defs, 0);
        assert_eq!(schema["items"]["type"], "string");
        assert!(schema["items"].get("$ref").is_none());
    }

    #[test]
    fn test_resolve_refs_in_any_of() {
        let mut defs = Map::new();
        defs.insert("Str".to_string(), json!({"type": "string"}));
        defs.insert("Num".to_string(), json!({"type": "number"}));

        let mut schema = json!({
            "anyOf": [
                { "$ref": "#/definitions/Str" },
                { "$ref": "#/$defs/Num" }
            ]
        });

        resolve_refs(&mut schema, &defs, 0);
        assert_eq!(schema["anyOf"][0], json!({"type": "string"}));
        assert_eq!(schema["anyOf"][1], json!({"type": "number"}));
    }

    #[test]
    fn test_resolve_refs_no_ref_passthrough() {
        let defs = Map::new();
        let mut schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "age": { "type": "integer" }
            }
        });
        let expected = schema.clone();

        resolve_refs(&mut schema, &defs, 0);
        assert_eq!(schema, expected);
    }

    #[test]
    fn test_resolve_refs_both_definitions_and_defs() {
        // The function uses a single definitions map; both formats are looked up
        let mut defs = Map::new();
        defs.insert("FromDefs".to_string(), json!({"type": "boolean"}));
        defs.insert("FromDefinitions".to_string(), json!({"type": "integer"}));

        let mut schema = json!({
            "type": "object",
            "properties": {
                "a": { "$ref": "#/$defs/FromDefs" },
                "b": { "$ref": "#/definitions/FromDefinitions" }
            }
        });

        resolve_refs(&mut schema, &defs, 0);
        assert_eq!(schema["properties"]["a"], json!({"type": "boolean"}));
        assert_eq!(schema["properties"]["b"], json!({"type": "integer"}));
    }

    // --- collapse_combiners tests ---

    #[test]
    fn test_collapse_combiners_any_of_picks_first_non_null() {
        let mut schema = json!({
            "anyOf": [
                {"type": "null"},
                {"type": "string", "minLength": 1}
            ]
        });
        collapse_combiners(&mut schema);
        assert_eq!(schema["type"], "string");
        assert_eq!(schema["minLength"], 1);
        assert!(schema.get("anyOf").is_none());
    }

    #[test]
    fn test_collapse_combiners_one_of_picks_first_non_null() {
        let mut schema = json!({
            "oneOf": [
                {"type": "null"},
                {"type": "integer", "minimum": 0}
            ]
        });
        collapse_combiners(&mut schema);
        assert_eq!(schema["type"], "integer");
        assert_eq!(schema["minimum"], 0);
        assert!(schema.get("oneOf").is_none());
    }

    #[test]
    fn test_collapse_combiners_all_null_uses_first() {
        let mut schema = json!({
            "anyOf": [
                {"type": "null"},
                {"type": "null"}
            ]
        });
        collapse_combiners(&mut schema);
        assert_eq!(schema["type"], "null");
        assert!(schema.get("anyOf").is_none());
    }

    #[test]
    fn test_collapse_combiners_no_null() {
        let mut schema = json!({
            "anyOf": [
                {"type": "string"},
                {"type": "number"}
            ]
        });
        collapse_combiners(&mut schema);
        assert_eq!(schema["type"], "string");
        assert!(schema.get("anyOf").is_none());
    }

    #[test]
    fn test_collapse_combiners_nested() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "field": {
                    "oneOf": [
                        {"type": "null"},
                        {"type": "boolean"}
                    ]
                }
            }
        });
        collapse_combiners(&mut schema);
        assert_eq!(schema["properties"]["field"]["type"], "boolean");
        assert!(schema["properties"]["field"].get("oneOf").is_none());
    }

    #[test]
    fn test_collapse_combiners_preserves_existing_fields() {
        let mut schema = json!({
            "description": "A nullable string",
            "anyOf": [
                {"type": "null"},
                {"type": "string", "maxLength": 100}
            ]
        });
        collapse_combiners(&mut schema);
        assert_eq!(schema["description"], "A nullable string");
        assert_eq!(schema["type"], "string");
        assert_eq!(schema["maxLength"], 100);
    }

    // --- merge_all_of tests ---

    #[test]
    fn test_merge_all_of_combines_properties() {
        let mut schema = json!({
            "allOf": [
                {"type": "object", "properties": {"a": {"type": "string"}}},
                {"properties": {"b": {"type": "number"}}}
            ]
        });
        merge_all_of(&mut schema);
        assert!(schema.get("allOf").is_none());
        assert_eq!(schema["properties"]["a"]["type"], "string");
        assert_eq!(schema["properties"]["b"]["type"], "number");
    }

    #[test]
    fn test_merge_all_of_combines_required() {
        let mut schema = json!({
            "allOf": [
                {"required": ["a", "b"]},
                {"required": ["b", "c"]}
            ]
        });
        merge_all_of(&mut schema);
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("a")));
        assert!(required.contains(&json!("b")));
        assert!(required.contains(&json!("c")));
        // No duplicates
        assert_eq!(required.len(), 3);
    }

    #[test]
    fn test_merge_all_of_conflicting_type_prefers_object() {
        let mut schema = json!({
            "allOf": [
                {"type": "string"},
                {"type": "number"}
            ]
        });
        merge_all_of(&mut schema);
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn test_merge_all_of_same_type_no_conflict() {
        let mut schema = json!({
            "allOf": [
                {"type": "object", "properties": {"a": {"type": "string"}}},
                {"type": "object", "properties": {"b": {"type": "number"}}}
            ]
        });
        merge_all_of(&mut schema);
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn test_merge_all_of_nested() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "nested": {
                    "allOf": [
                        {"properties": {"x": {"type": "integer"}}},
                        {"properties": {"y": {"type": "integer"}}}
                    ]
                }
            }
        });
        merge_all_of(&mut schema);
        assert!(schema["properties"]["nested"].get("allOf").is_none());
        assert_eq!(schema["properties"]["nested"]["properties"]["x"]["type"], "integer");
        assert_eq!(schema["properties"]["nested"]["properties"]["y"]["type"], "integer");
    }

    #[test]
    fn test_merge_all_of_other_fields() {
        let mut schema = json!({
            "allOf": [
                {"type": "object", "description": "First"},
                {"title": "Second"}
            ]
        });
        merge_all_of(&mut schema);
        assert_eq!(schema["description"], "First");
        assert_eq!(schema["title"], "Second");
    }

    // --- collapse_type_arrays tests ---

    #[test]
    fn test_collapse_type_arrays_string_null() {
        let mut schema = json!({"type": ["string", "null"]});
        collapse_type_arrays(&mut schema);
        assert_eq!(schema["type"], "string");
    }

    #[test]
    fn test_collapse_type_arrays_null_first() {
        let mut schema = json!({"type": ["null", "integer"]});
        collapse_type_arrays(&mut schema);
        assert_eq!(schema["type"], "integer");
    }

    #[test]
    fn test_collapse_type_arrays_all_null() {
        let mut schema = json!({"type": ["null"]});
        collapse_type_arrays(&mut schema);
        assert_eq!(schema["type"], "null");
    }

    #[test]
    fn test_collapse_type_arrays_single_non_null() {
        let mut schema = json!({"type": ["boolean"]});
        collapse_type_arrays(&mut schema);
        assert_eq!(schema["type"], "boolean");
    }

    #[test]
    fn test_collapse_type_arrays_already_string() {
        let mut schema = json!({"type": "string"});
        let expected = schema.clone();
        collapse_type_arrays(&mut schema);
        assert_eq!(schema, expected);
    }

    #[test]
    fn test_collapse_type_arrays_nested() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "field": {"type": ["number", "null"]}
            }
        });
        collapse_type_arrays(&mut schema);
        assert_eq!(schema["properties"]["field"]["type"], "number");
    }

    #[test]
    fn test_collapse_type_arrays_multiple_non_null() {
        let mut schema = json!({"type": ["string", "number", "null"]});
        collapse_type_arrays(&mut schema);
        // Picks the first non-null
        assert_eq!(schema["type"], "string");
    }

    // --- enforce_nesting_depth tests ---

    #[test]
    fn test_enforce_nesting_depth_within_limit() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"}
            }
        });
        let expected = schema.clone();
        enforce_nesting_depth(&mut schema, 5, 0);
        assert_eq!(schema, expected);
    }

    #[test]
    fn test_enforce_nesting_depth_at_limit() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "deep": {
                    "type": "object",
                    "properties": {
                        "deeper": {"type": "string"}
                    }
                }
            }
        });
        enforce_nesting_depth(&mut schema, 1, 0);
        // The root is at depth 0 (object), so next_depth = 1
        // "deep" is an object at depth 1 which equals max_depth, so it gets truncated
        assert_eq!(schema["properties"]["deep"], json!({"type": "object"}));
    }

    #[test]
    fn test_enforce_nesting_depth_exceeds_limit() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "level1": {
                    "type": "object",
                    "properties": {
                        "level2": {
                            "type": "object",
                            "properties": {
                                "level3": {"type": "string"}
                            }
                        }
                    }
                }
            }
        });
        enforce_nesting_depth(&mut schema, 2, 0);
        // Root at depth 0, level1 at depth 1, level2 at depth 2 (== max_depth) → truncated
        assert_eq!(
            schema["properties"]["level1"]["properties"]["level2"],
            json!({"type": "object"})
        );
    }

    #[test]
    fn test_enforce_nesting_depth_non_object_not_counted() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "arr": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": {"type": "string"}
                        }
                    }
                }
            }
        });
        enforce_nesting_depth(&mut schema, 2, 0);
        // Root at depth 0 (object, next=1), arr is array (not object, next stays 1),
        // items is object at depth 1 (next=2), name is string — no truncation at depth 2
        assert_eq!(schema["properties"]["arr"]["items"]["properties"]["name"]["type"], "string");
    }

    #[test]
    fn test_enforce_nesting_depth_gemini_5_levels() {
        // Simulate Gemini's 5-level limit
        let mut schema = json!({
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
                                                        "deep": {"type": "string"}
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
        enforce_nesting_depth(&mut schema, 5, 0);
        // l5 is at depth 5 (root=0, l1=1, l2=2, l3=3, l4=4, l5=5) → truncated
        assert_eq!(
            schema["properties"]["l1"]["properties"]["l2"]["properties"]["l3"]["properties"]["l4"]
                ["properties"]["l5"],
            json!({"type": "object"})
        );
        // l4 should still have its properties
        assert!(
            schema["properties"]["l1"]["properties"]["l2"]["properties"]["l3"]["properties"]["l4"]
                .get("properties")
                .is_some()
        );
    }

    #[test]
    fn test_enforce_nesting_depth_zero_truncates_root_object() {
        let mut schema = json!({
            "type": "object",
            "properties": {"a": {"type": "string"}}
        });
        enforce_nesting_depth(&mut schema, 0, 0);
        assert_eq!(schema, json!({"type": "object"}));
    }

    // --- is_null_schema tests ---

    #[test]
    fn test_is_null_schema_true() {
        assert!(is_null_schema(&json!({"type": "null"})));
    }

    #[test]
    fn test_is_null_schema_false_for_string() {
        assert!(!is_null_schema(&json!({"type": "string"})));
    }

    #[test]
    fn test_is_null_schema_false_for_non_object() {
        assert!(!is_null_schema(&json!("null")));
        assert!(!is_null_schema(&Value::Null));
    }
}
