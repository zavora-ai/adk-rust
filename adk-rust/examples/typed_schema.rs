use adk_rust::prelude::{InputModel, ModelError, OutputModel};
use adk_rust::serde::{Deserialize, Serialize};
use schemars::JsonSchema;

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
struct Request {
    message: String,
}

#[derive(JsonSchema, Serialize)]
struct Response {
    accepted: bool,
}

fn main() -> Result<(), ModelError> {
    let input = InputModel::<Request>::new()?;
    let request = input.parse_str(r#"{"message":"hello"}"#)?;
    assert_eq!(request, Request { message: "hello".into() });

    let output = OutputModel::<Response>::new()?;
    let encoded = output.encode_value(&Response { accepted: true })?;
    assert_eq!(encoded, adk_rust::serde_json::json!({"accepted": true}));
    Ok(())
}
