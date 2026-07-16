#![cfg(feature = "typed")]

use std::cell::Cell;
use std::collections::BTreeMap;

use adk_schema::{InputModel, ModelError, OutputModel, SchemaError};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser};
use serde_json::json;

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
struct InputOnly {
    name: String,
    quantity: u32,
}

#[derive(JsonSchema, Serialize)]
struct OutputOnly {
    result: String,
}

#[test]
fn valid_input_returns_the_typed_value() {
    let model = InputModel::<InputOnly>::new().unwrap();

    let value = model.parse_str(r#"{"name":"bolts","quantity":4}"#).unwrap();

    assert_eq!(value, InputOnly { name: "bolts".into(), quantity: 4 });
}

#[test]
fn wrong_input_type_returns_a_schema_error() {
    let model = InputModel::<InputOnly>::new().unwrap();

    let error = model.parse_value(json!({"name": 12, "quantity": 4})).unwrap_err();

    assert!(matches!(error, ModelError::Schema(SchemaError::InvalidInstance { .. })));
}

#[test]
fn missing_required_input_returns_a_schema_error() {
    let model = InputModel::<InputOnly>::new().unwrap();

    let error = model.parse_value(json!({"name": "bolts"})).unwrap_err();

    assert!(matches!(error, ModelError::Schema(SchemaError::InvalidInstance { .. })));
}

thread_local! {
    static DECODE_CALLS: Cell<usize> = const { Cell::new(0) };
}

#[derive(Debug)]
struct RejectingString;

impl<'de> Deserialize<'de> for RejectingString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        DECODE_CALLS.set(DECODE_CALLS.get() + 1);
        let _ = String::deserialize(deserializer)?;
        Err(de::Error::custom("custom input rejection"))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CustomInput {
    #[schemars(with = "String")]
    #[allow(dead_code)]
    value: RejectingString,
}

#[test]
fn schema_failure_happens_before_the_typed_deserializer_runs() {
    DECODE_CALLS.set(0);
    let model = InputModel::<CustomInput>::new().unwrap();

    let error = model.parse_value(json!({"value": 42})).unwrap_err();

    assert!(
        matches!(error, ModelError::Schema(SchemaError::InvalidInstance { .. }))
            && DECODE_CALLS.get() == 0
    );
}

#[test]
fn custom_deserializer_error_includes_its_serde_path() {
    DECODE_CALLS.set(0);
    let model = InputModel::<CustomInput>::new().unwrap();

    let error = model.parse_value(json!({"value": "reject me"})).unwrap_err();

    assert!(matches!(error, ModelError::Decode { path, .. } if path == "value"));
}

#[test]
fn malformed_json_returns_the_original_json_error() {
    let model = InputModel::<InputOnly>::new().unwrap();

    let error = model.parse_slice(br#"{"name":"bolts",]"#).unwrap_err();

    assert!(matches!(error, ModelError::Json { source } if source.is_syntax()));
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase")]
struct AttributeInput {
    display_name: String,
    #[serde(default)]
    retry_count: u32,
    #[serde(flatten)]
    metadata: BTreeMap<String, String>,
}

#[test]
fn serde_rename_default_and_flatten_match_the_generated_input_schema() {
    let model = InputModel::<AttributeInput>::new().unwrap();

    let value = model.parse_value(json!({"displayName": "worker", "region": "west"})).unwrap();

    assert_eq!(
        value,
        AttributeInput {
            display_name: "worker".into(),
            retry_count: 0,
            metadata: BTreeMap::from([("region".into(), "west".into())]),
        }
    );
}

#[test]
fn input_type_does_not_need_serialize_and_output_type_does_not_need_deserialize() {
    let input = InputModel::<InputOnly>::new().unwrap();
    let output = OutputModel::<OutputOnly>::new().unwrap();

    let parsed = input.parse_value(json!({"name": "bolts", "quantity": 4})).unwrap();
    let encoded = output.encode_value(&OutputOnly { result: parsed.name }).unwrap();

    assert_eq!(encoded, json!({"result": "bolts"}));
}

struct FailingSerialize;

impl Serialize for FailingSerialize {
    fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Err(ser::Error::custom("custom output rejection"))
    }
}

#[derive(JsonSchema, Serialize)]
struct FailingOutput {
    #[schemars(with = "String")]
    payload: FailingSerialize,
}

#[test]
fn serialization_error_includes_its_serde_path() {
    let model = OutputModel::<FailingOutput>::new().unwrap();

    let error = model.encode_value(&FailingOutput { payload: FailingSerialize }).unwrap_err();

    assert!(matches!(error, ModelError::Encode { path, .. } if path == "payload"));
}

struct NumberEncodedAsString;

impl Serialize for NumberEncodedAsString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u64(42)
    }
}

#[derive(JsonSchema, Serialize)]
struct InvalidOutput {
    #[schemars(with = "String")]
    payload: NumberEncodedAsString,
}

#[test]
fn schema_invalid_serialized_value_is_not_returned() {
    let model = OutputModel::<InvalidOutput>::new().unwrap();

    let error = model.encode_value(&InvalidOutput { payload: NumberEncodedAsString }).unwrap_err();

    assert!(matches!(error, ModelError::Schema(SchemaError::InvalidInstance { .. })));
}

#[test]
fn repeated_operations_reuse_the_same_compiled_model() {
    let model = InputModel::<InputOnly>::new().unwrap();
    let schema = model.json_schema();

    model.parse_value(json!({"name": "bolts", "quantity": 4})).unwrap();
    model.parse_value(json!({"name": "nuts", "quantity": 2})).unwrap();

    assert!(std::ptr::eq(schema, model.json_schema()));
}
