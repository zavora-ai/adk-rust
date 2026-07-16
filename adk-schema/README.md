# adk-schema

Canonical, provider-neutral Draft 2020-12 JSON Schema documents and typed
validation models for ADK-Rust.

`adk-schema` keeps the durable schema separate from provider projection. It
canonicalizes and bounds schema ingestion, rejects external references, compiles
one reusable validator, and distinguishes input schemas from output schemas at
compile time.

## Installation

Typed models are enabled by default:

```toml
[dependencies]
adk-schema = "2.0.0"
schemars = "1.2"
serde = { version = "1", features = ["derive"] }
```

Dynamic-only consumers can omit Schemars, Serde, and runtime validation:

```toml
[dependencies]
adk-schema = { version = "2.0.0", default-features = false }
serde_json = "1"
```

## Typed models

```rust
use adk_schema::{InputModel, OutputModel};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
struct Request {
    message: String,
}

#[derive(JsonSchema, Serialize)]
struct Response {
    accepted: bool,
}

# fn example() -> Result<(), adk_schema::ModelError> {
let input = InputModel::<Request>::new()?;
let request = input.parse_str(r#"{"message":"hello"}"#)?;
assert_eq!(request, Request { message: "hello".into() });

let output = OutputModel::<Response>::new()?;
let encoded = output.encode_value(&Response { accepted: true })?;
assert_eq!(encoded, serde_json::json!({"accepted": true}));
# Ok(())
# }
```

Input processing uses this fixed order:

```text
JSON bytes -> Value -> canonical input schema validation -> Serde deserialization -> T
```

Output processing uses the reverse typed boundary:

```text
&T -> Serde serialization -> canonical output schema validation -> Value
```

Schema validation failures return `ModelError::Schema`. Malformed JSON returns
`ModelError::Json`. A custom Serde implementation can still reject schema-valid
data; those failures retain the original `serde_json::Error` and a Serde field
path in `ModelError::Decode` or `ModelError::Encode`. Schema issue paths are JSON
Pointers, while Serde paths use field/index notation such as `items[2].name`.

`json_schema()` returns the canonical provider-neutral schema. Give provider
adapters a clone to project; do not replace or mutate the canonical document.

## Dynamic schemas

Dynamic schemas remain available without a Rust type:

```rust
use adk_schema::{IngestionPolicy, InputSchema};

# fn example() -> Result<(), adk_schema::SchemaError> {
let schema = serde_json::json!({
    "type": "object",
    "properties": {"message": {"type": "string"}},
    "required": ["message"]
});
let document = InputSchema::from_value(schema, &IngestionPolicy::default())?;
assert_eq!(document.as_value()["type"], "object");
# Ok(())
# }
```

## Features

| Feature | Default | Purpose |
|---------|---------|---------|
| `typed` | yes | `InputModel<T>` and `OutputModel<T>` with generated schemas and validation |
| `schemars` | via `typed` | Generate directional schemas from Rust types |
| `runtime-validation` | via `typed` | Compile and validate schema instances |

## Stability

`adk-schema` is Beta. Typed tools, structured agent output, and provider
projection must stabilize before promotion to Stable.

## License

Apache-2.0
