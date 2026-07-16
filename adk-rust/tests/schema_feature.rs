#![cfg(feature = "schema")]

use adk_rust::prelude::InputModel;
use adk_rust::serde::Deserialize;
use adk_rust::{GenericSchemaAdapter, SchemaAdapter};
use schemars::JsonSchema;

#[derive(Deserialize, JsonSchema)]
struct Request {
    #[allow(dead_code)]
    message: String,
}

#[test]
fn provider_projection_does_not_mutate_the_canonical_schema() {
    let model = InputModel::<Request>::new().unwrap();
    let canonical = model.json_schema().clone();

    let _provider_schema = GenericSchemaAdapter.normalize_schema(canonical.clone());

    assert_eq!(model.json_schema(), &canonical);
}
