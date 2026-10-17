//! Checks a tool call's arguments against the tool's parameters schema before the tool runs.
//!
//! A model's arguments are only as good as its reading of the schema. Deserializing them with
//! serde drops keys the type does not declare and ignores the schema's constraints (`minimum`,
//! `maxLength`, `pattern`, ...), so a call the model believes was honoured can run with
//! different meaning. [`ToolArgumentValidator`] rejects such a call with an
//! [`TOOL_INVALID_ARGUMENTS_CODE`] error, whose message reaches the model so it can correct the
//! call; the tool does not run. The `#[tool]` macro applies it to every generated tool.

use std::collections::HashSet;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, TOOL_INVALID_ARGUMENTS_CODE};
use serde_json::{Map, Value};

/// The most schema violations listed in one invalid-arguments error.
const MAX_REPORTED_VIOLATIONS: usize = 8;

/// The most characters of one violation message; a message quotes the offending value.
const MAX_VIOLATION_CHARS: usize = 200;

/// The error code for a tool whose parameters schema does not compile.
pub const TOOL_INVALID_PARAMETERS_SCHEMA_CODE: &str = "tool.invalid_parameters_schema";

/// Validates tool-call arguments against a JSON Schema.
///
/// Compile it once per schema and call [`validate`](Self::validate) in
/// [`Tool::execute`](adk_core::Tool::execute) before acting on the arguments. Pair it with
/// [`close_object_schemas`] to reject keys the schema does not declare.
///
/// # Example
///
/// ```rust
/// use adk_tool::{TOOL_INVALID_ARGUMENTS_CODE, ToolArgumentValidator};
/// use serde_json::json;
///
/// let validator = ToolArgumentValidator::new(&json!({
///     "type": "object",
///     "properties": { "count": { "type": "integer", "minimum": 1, "maximum": 10 } },
///     "required": ["count"],
///     "additionalProperties": false
/// }))?;
///
/// assert!(validator.validate("book", &json!({ "count": 3 })).is_ok());
/// let err = validator.validate("book", &json!({ "count": 50, "dry_run": true })).unwrap_err();
/// assert_eq!(err.code, TOOL_INVALID_ARGUMENTS_CODE);
/// assert!(err.message.contains("/count"));
/// assert!(err.message.contains("dry_run"));
/// # Ok::<(), adk_tool::AdkError>(())
/// ```
pub struct ToolArgumentValidator {
    validator: jsonschema::Validator,
}

impl std::fmt::Debug for ToolArgumentValidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolArgumentValidator").finish_non_exhaustive()
    }
}

impl ToolArgumentValidator {
    /// Compiles `schema`. The draft follows its `$schema` keyword, defaulting to 2020-12.
    ///
    /// `format` is treated as an annotation, as the 2020-12 draft specifies, so a value's
    /// format is left to the tool's own deserialization.
    ///
    /// # Errors
    ///
    /// Returns an error with code [`TOOL_INVALID_PARAMETERS_SCHEMA_CODE`] when `schema` is not a
    /// valid JSON Schema.
    pub fn new(schema: &Value) -> Result<Self> {
        let validator = jsonschema::validator_for(schema).map_err(|error| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Internal,
                TOOL_INVALID_PARAMETERS_SCHEMA_CODE,
                format!(
                    "the tool's parameters schema does not compile: {error}. Fix the schema the \
                     tool declares."
                ),
            )
        })?;
        Ok(Self { validator })
    }

    /// Checks `args` against the schema.
    ///
    /// # Errors
    ///
    /// Returns an [`ErrorCategory::InvalidInput`] error with code
    /// [`TOOL_INVALID_ARGUMENTS_CODE`] that lists each violation by its JSON pointer, such as
    /// `/count: 50 is greater than the maximum of 10`. An agent shows this message to the model.
    pub fn validate(&self, tool_name: &str, args: &Value) -> Result<()> {
        let mut violations = Vec::new();
        let mut total = 0usize;
        for error in self.validator.iter_errors(args) {
            total += 1;
            if violations.len() == MAX_REPORTED_VIOLATIONS {
                continue;
            }
            let path = error.instance_path().as_str();
            let location = if path.is_empty() { "arguments" } else { path };
            violations.push(format!("{location}: {}", truncate(&error.to_string())));
        }
        if total == 0 {
            return Ok(());
        }
        if total > violations.len() {
            violations.push(format!("and {} more", total - violations.len()));
        }
        Err(AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::InvalidInput,
            TOOL_INVALID_ARGUMENTS_CODE,
            format!(
                "invalid arguments for '{tool_name}': {}. Correct the arguments to match the \
                 tool's parameters schema and call it again.",
                violations.join("; ")
            ),
        ))
    }
}

fn truncate(message: &str) -> String {
    match message.char_indices().nth(MAX_VIOLATION_CHARS) {
        Some((end, _)) => format!("{}…", &message[..end]),
        None => message.to_string(),
    }
}

/// Sets `additionalProperties: false` on each object schema in `schema` that declares all of its
/// fields, so a validator rejects keys the schema does not name.
///
/// An object schema declares all of its fields when it lists `properties`, says nothing about
/// `additionalProperties`, `patternProperties` or `unevaluatedProperties`, and has no `$ref`,
/// `allOf`, `anyOf`, `oneOf` or `if` beside its `properties` that could add more. Schemas that
/// describe part of an object are left open: `allOf` branches, `if`/`then`/`else`, `not`,
/// `dependentSchemas`, the `anyOf`/`oneOf` branches of an object that has `properties` of its
/// own (a flattened enum), and definitions an `allOf` branch references.
///
/// # Example
///
/// ```rust
/// use adk_tool::close_object_schemas;
/// use serde_json::json;
///
/// let mut schema = json!({
///     "type": "object",
///     "properties": { "address": { "$ref": "#/$defs/Address" } },
///     "$defs": {
///         "Address": { "type": "object", "properties": { "city": { "type": "string" } } }
///     }
/// });
/// close_object_schemas(&mut schema);
/// assert_eq!(schema["additionalProperties"], false);
/// assert_eq!(schema["$defs"]["Address"]["additionalProperties"], false);
/// ```
pub fn close_object_schemas(schema: &mut Value) {
    let mut partial_definitions = HashSet::new();
    collect_all_of_references(schema, &mut partial_definitions);
    close(schema, true, &partial_definitions);
}

const COMBINATORS: [&str; 3] = ["allOf", "anyOf", "oneOf"];

fn close(schema: &mut Value, complete: bool, partial_definitions: &HashSet<String>) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };
    let has_properties = obj.get("properties").is_some_and(Value::is_object);
    let extendable = ["additionalProperties", "patternProperties", "unevaluatedProperties"]
        .iter()
        .chain(&["$ref", "if"])
        .chain(&COMBINATORS)
        .any(|keyword| obj.contains_key(*keyword));
    if complete && has_properties && !extendable {
        obj.insert("additionalProperties".to_string(), Value::Bool(false));
    }

    for keyword in ["properties", "patternProperties"] {
        if let Some(Value::Object(children)) = obj.get_mut(keyword) {
            children.values_mut().for_each(|child| close(child, true, partial_definitions));
        }
    }
    for keyword in ["$defs", "definitions"] {
        if let Some(Value::Object(definitions)) = obj.get_mut(keyword) {
            for (name, definition) in definitions.iter_mut() {
                close(definition, !partial_definitions.contains(name), partial_definitions);
            }
        }
    }
    for keyword in ["items", "additionalProperties", "unevaluatedProperties", "contains"] {
        match obj.get_mut(keyword) {
            Some(child @ Value::Object(_)) => close(child, true, partial_definitions),
            Some(Value::Array(children)) => {
                children.iter_mut().for_each(|child| close(child, true, partial_definitions));
            }
            _ => {}
        }
    }
    if let Some(Value::Array(children)) = obj.get_mut("prefixItems") {
        children.iter_mut().for_each(|child| close(child, true, partial_definitions));
    }
    for keyword in COMBINATORS {
        // A branch is a whole value only under `anyOf`/`oneOf` of an object with no fields of its own.
        let branch_complete = keyword != "allOf" && !has_properties;
        if let Some(Value::Array(branches)) = obj.get_mut(keyword) {
            for branch in branches {
                close(branch, branch_complete, partial_definitions);
            }
        }
    }
    for keyword in ["not", "if", "then", "else"] {
        if let Some(child) = obj.get_mut(keyword) {
            close(child, false, partial_definitions);
        }
    }
    if let Some(Value::Object(children)) = obj.get_mut("dependentSchemas") {
        children.values_mut().for_each(|child| close(child, false, partial_definitions));
    }
}

/// Records the definition names that `allOf` branches reference, which describe part of an object.
fn collect_all_of_references(schema: &Value, names: &mut HashSet<String>) {
    match schema {
        Value::Object(obj) => {
            if let Some(Value::Array(branches)) = obj.get("allOf") {
                for branch in branches {
                    if let Some(name) = branch.get("$ref").and_then(Value::as_str).and_then(|r| {
                        r.strip_prefix("#/$defs/").or_else(|| r.strip_prefix("#/definitions/"))
                    }) {
                        names.insert(name.to_string());
                    }
                }
            }
            obj.values().for_each(|child| collect_all_of_references(child, names));
        }
        Value::Array(children) => {
            children.iter().for_each(|child| collect_all_of_references(child, names));
        }
        _ => {}
    }
}

/// Reduces a schema generated for a `#[tool]` argument type to the form sent to the model.
///
/// Removes the root `$schema` and `title`, and reduces nullable types that some providers
/// reject: `{"type": ["string", "null"]}` becomes `{"type": "string"}`, and an `anyOf` of one
/// schema and `null` becomes that schema. Validation uses the schema before this reduction, so a
/// model may still send `null` for an optional field.
///
/// # Example
///
/// ```rust
/// use adk_tool::model_facing_schema;
/// use serde_json::json;
///
/// let schema = model_facing_schema(json!({
///     "$schema": "https://json-schema.org/draft/2020-12/schema",
///     "title": "Args",
///     "type": "object",
///     "properties": { "note": { "type": ["string", "null"] } }
/// }));
/// assert_eq!(schema, json!({
///     "type": "object",
///     "properties": { "note": { "type": "string" } }
/// }));
/// ```
pub fn model_facing_schema(mut schema: Value) -> Value {
    if let Some(obj) = schema.as_object_mut() {
        obj.remove("$schema");
        obj.remove("title");
    }
    simplify_nullable(&mut schema);
    schema
}

fn simplify_nullable(schema: &mut Value) {
    match schema {
        Value::Object(map) => {
            if let Some(Value::Array(types)) = map.get("type") {
                let non_null: Vec<_> =
                    types.iter().filter(|t| t.as_str() != Some("null")).cloned().collect();
                if let [only] = non_null.as_slice() {
                    map.insert("type".to_string(), only.clone());
                }
            }
            if let Some(Value::Array(any_of)) = map.remove("anyOf") {
                merge_first_non_null(map, &any_of);
            }
            map.values_mut().for_each(simplify_nullable);
        }
        Value::Array(items) => items.iter_mut().for_each(simplify_nullable),
        _ => {}
    }
}

fn merge_first_non_null(map: &mut Map<String, Value>, any_of: &[Value]) {
    let variant = any_of
        .iter()
        .filter_map(Value::as_object)
        .find(|variant| variant.get("type").and_then(Value::as_str) != Some("null"));
    if let Some(variant) = variant {
        for (key, value) in variant {
            map.insert(key.clone(), value.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn closed(mut schema: Value) -> Value {
        close_object_schemas(&mut schema);
        schema
    }

    #[test]
    fn closes_complete_objects_and_their_definitions() {
        let schema = closed(json!({
            "type": "object",
            "properties": {
                "address": { "$ref": "#/$defs/Address", "description": "where" },
                "tags": { "type": "array", "items": { "type": "object", "properties": { "k": {} } } }
            },
            "$defs": { "Address": { "type": "object", "properties": { "city": {} } } }
        }));
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["$defs"]["Address"]["additionalProperties"], false);
        assert_eq!(schema["properties"]["tags"]["items"]["additionalProperties"], false);
        // The `$ref` node itself takes its fields from the definition.
        assert!(schema["properties"]["address"].get("additionalProperties").is_none());
    }

    #[test]
    fn leaves_declared_and_open_ended_objects_alone() {
        let schema = closed(json!({
            "type": "object",
            "properties": {
                "open": { "type": "object", "properties": { "a": {} }, "additionalProperties": true },
                "map": { "type": "object", "additionalProperties": { "type": "string" } },
                "free": { "type": "object" }
            }
        }));
        assert_eq!(schema["properties"]["open"]["additionalProperties"], true);
        assert_eq!(
            schema["properties"]["map"]["additionalProperties"],
            json!({ "type": "string" })
        );
        assert!(schema["properties"]["free"].get("additionalProperties").is_none());
    }

    #[test]
    fn leaves_partial_schemas_open() {
        // A flattened enum: the branches only name the variant's own fields.
        let flattened = closed(json!({
            "type": "object",
            "properties": { "name": {} },
            "oneOf": [
                { "type": "object", "properties": { "a": {} } },
                { "type": "object", "properties": { "b": {} } }
            ]
        }));
        assert!(flattened.get("additionalProperties").is_none());
        assert!(flattened["oneOf"][0].get("additionalProperties").is_none());

        let composed = closed(json!({
            "allOf": [
                { "$ref": "#/$defs/Base" },
                { "type": "object", "properties": { "extra": {} } }
            ],
            "if": { "properties": { "kind": { "const": "x" } } },
            "$defs": { "Base": { "type": "object", "properties": { "id": {} } } }
        }));
        assert!(composed["allOf"][1].get("additionalProperties").is_none());
        assert!(composed["if"].get("additionalProperties").is_none());
        assert!(composed["$defs"]["Base"].get("additionalProperties").is_none());
    }

    #[test]
    fn closes_whole_value_branches_of_a_tagged_enum() {
        let schema = closed(json!({
            "oneOf": [
                { "type": "object", "properties": { "kind": { "const": "a" }, "a": {} } },
                { "type": "null" }
            ]
        }));
        assert_eq!(schema["oneOf"][0]["additionalProperties"], false);
    }

    #[test]
    fn validation_lists_every_violation_by_pointer() {
        let validator = ToolArgumentValidator::new(&closed(json!({
            "type": "object",
            "properties": {
                "count": { "type": "integer", "minimum": 1, "maximum": 10 },
                "room": { "type": "string", "pattern": "^[A-Z][0-9]$" },
                "address": { "$ref": "#/$defs/Address" },
                "note": { "type": ["string", "null"] }
            },
            "required": ["count", "room"],
            "$defs": { "Address": { "type": "object", "properties": { "city": { "type": "string" } } } }
        })))
        .unwrap();

        assert!(
            validator.validate("book", &json!({ "count": 2, "room": "A1", "note": null })).is_ok()
        );
        let err = validator
            .validate(
                "book",
                &json!({ "count": 50, "room": "lobby", "dry_run": true, "address": { "zip": "1" } }),
            )
            .unwrap_err();
        assert_eq!(err.code, TOOL_INVALID_ARGUMENTS_CODE);
        assert_eq!(err.category, ErrorCategory::InvalidInput);
        for expected in
            ["'book'", "/count: 50", "/room:", "arguments: ", "dry_run", "/address:", "zip"]
        {
            assert!(err.message.contains(expected), "{expected} missing from {}", err.message);
        }
    }

    #[test]
    fn long_violation_lists_are_capped() {
        let properties: Map<String, Value> =
            (0..12).map(|i| (format!("f{i}"), json!({ "type": "integer" }))).collect();
        let validator =
            ToolArgumentValidator::new(&json!({ "type": "object", "properties": properties }))
                .unwrap();
        let args: Map<String, Value> =
            (0..12).map(|i| (format!("f{i}"), json!("x".repeat(500)))).collect();
        let err = validator.validate("t", &Value::Object(args)).unwrap_err();
        assert!(err.message.contains("and 4 more"), "{}", err.message);
        assert!(err.message.len() < 8 * (MAX_VIOLATION_CHARS + 16) + 200, "{}", err.message);
    }

    #[test]
    fn an_invalid_schema_is_reported() {
        let err = ToolArgumentValidator::new(&json!({ "type": 12 })).unwrap_err();
        assert_eq!(err.code, TOOL_INVALID_PARAMETERS_SCHEMA_CODE);
    }

    #[test]
    fn model_facing_schema_reduces_nullable_types() {
        let schema = model_facing_schema(json!({
            "$schema": "x",
            "title": "T",
            "properties": {
                "a": { "anyOf": [{ "$ref": "#/$defs/A" }, { "type": "null" }], "description": "d" },
                "b": { "type": ["integer", "null"] }
            }
        }));
        assert_eq!(
            schema,
            json!({
                "properties": {
                    "a": { "$ref": "#/$defs/A", "description": "d" },
                    "b": { "type": "integer" }
                }
            })
        );
    }
}
