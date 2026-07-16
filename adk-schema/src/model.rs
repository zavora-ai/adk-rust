use std::marker::PhantomData;

use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use thiserror::Error;

use crate::{
    IngestionPolicy, Input, Output, SchemaDocument, SchemaError, SchemaRole,
    ValidatedSchemaDocument,
};

/// Result type for typed model operations.
pub type ModelResult<T> = std::result::Result<T, ModelError>;

/// A Rust type bound to one canonical, compiled JSON Schema document.
///
/// Role-specific APIs are available through [`InputModel`] and [`OutputModel`].
/// The compiled validator is created once by `new` or `new_with_policy` and is
/// reused for every subsequent operation.
pub struct Model<T, R: SchemaRole> {
    schema: ValidatedSchemaDocument<R>,
    _type: PhantomData<fn() -> T>,
}

/// A typed input model that validates JSON before deserializing it.
pub type InputModel<T> = Model<T, Input>;

/// A typed output model that serializes a value before validating the JSON.
pub type OutputModel<T> = Model<T, Output>;

/// Error returned by typed model construction, parsing, and encoding.
///
/// Schema issue paths inside [`SchemaError`] are JSON Pointers. The `path`
/// fields on [`ModelError::Decode`] and [`ModelError::Encode`] use Serde's
/// field path notation (for example, `items[2].name`) and are intentionally a
/// separate path format.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum ModelError {
    /// Canonical schema generation, compilation, or instance validation failed.
    #[error("schema error: {0}")]
    Schema(#[from] SchemaError),
    /// Input bytes were not valid JSON.
    #[error("invalid JSON: {source}")]
    Json {
        /// Original JSON parser error.
        #[source]
        source: serde_json::Error,
    },
    /// Schema-valid JSON could not be deserialized into the Rust input type.
    #[error("failed to decode value at {path}: {source}")]
    Decode {
        /// Serde field path where deserialization failed.
        path: String,
        /// Original Serde JSON deserialization error.
        #[source]
        source: serde_json::Error,
    },
    /// The Rust output type could not be serialized into JSON.
    #[error("failed to encode value at {path}: {source}")]
    Encode {
        /// Serde field path where serialization failed.
        path: String,
        /// Original Serde JSON serialization error.
        #[source]
        source: serde_json::Error,
    },
}

impl<T, R: SchemaRole> Model<T, R> {
    /// Borrows the canonical schema document and its compiled validator.
    pub fn schema(&self) -> &ValidatedSchemaDocument<R> {
        &self.schema
    }

    /// Borrows the canonical, provider-neutral Draft 2020-12 JSON Schema.
    ///
    /// Provider adapters must project a clone of this value for their own
    /// constraints; they must not replace the canonical schema held here.
    pub fn json_schema(&self) -> &Value {
        self.schema.document()
    }

    /// Validates a JSON value against the model's canonical schema.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::Schema`] when the value does not satisfy the schema.
    pub fn validate(&self, value: &Value) -> ModelResult<()> {
        self.schema.validate(value).map_err(ModelError::Schema)
    }
}

impl<T> Model<T, Input>
where
    T: JsonSchema + DeserializeOwned,
{
    /// Builds an input model with the default ingestion policy.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::Schema`] if schema generation or compilation fails.
    pub fn new() -> ModelResult<Self> {
        Self::new_with_policy(&IngestionPolicy::default())
    }

    /// Builds an input model with a custom ingestion policy.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::Schema`] if schema generation, ingestion, or
    /// compilation fails under the supplied policy.
    pub fn new_with_policy(policy: &IngestionPolicy) -> ModelResult<Self> {
        let schema = SchemaDocument::<Input>::for_type_with_policy::<T>(policy)?.compile()?;
        Ok(Self { schema, _type: PhantomData })
    }

    /// Validates and deserializes a pre-parsed JSON value.
    ///
    /// Schema validation always runs before the typed deserializer.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::Schema`] for schema-invalid input or
    /// [`ModelError::Decode`] when a custom deserializer rejects schema-valid data.
    pub fn parse_value(&self, value: Value) -> ModelResult<T> {
        self.validate(&value)?;
        serde_path_to_error::deserialize(value).map_err(|error| {
            let path = error.path().to_string();
            let source = error.into_inner();
            ModelError::Decode { path, source }
        })
    }

    /// Parses, validates, and deserializes a JSON string.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::Json`] for malformed JSON, then the same validation
    /// and decode errors as [`Self::parse_value`].
    pub fn parse_str(&self, source: &str) -> ModelResult<T> {
        self.parse_slice(source.as_bytes())
    }

    /// Parses, validates, and deserializes JSON bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::Json`] for malformed JSON, then the same validation
    /// and decode errors as [`Self::parse_value`].
    pub fn parse_slice(&self, source: &[u8]) -> ModelResult<T> {
        let value = serde_json::from_slice(source).map_err(|source| ModelError::Json { source })?;
        self.parse_value(value)
    }
}

impl<T> Model<T, Output>
where
    T: JsonSchema + Serialize,
{
    /// Builds an output model with the default ingestion policy.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::Schema`] if schema generation or compilation fails.
    pub fn new() -> ModelResult<Self> {
        Self::new_with_policy(&IngestionPolicy::default())
    }

    /// Builds an output model with a custom ingestion policy.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::Schema`] if schema generation, ingestion, or
    /// compilation fails under the supplied policy.
    pub fn new_with_policy(policy: &IngestionPolicy) -> ModelResult<Self> {
        let schema = SchemaDocument::<Output>::for_type_with_policy::<T>(policy)?.compile()?;
        Ok(Self { schema, _type: PhantomData })
    }

    /// Serializes a Rust value and validates the resulting JSON against the
    /// canonical output schema.
    ///
    /// The JSON value is returned only after schema validation succeeds.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::Encode`] when serialization fails, or
    /// [`ModelError::Schema`] when the serialized value violates the output schema.
    pub fn encode_value(&self, value: &T) -> ModelResult<Value> {
        let mut track = serde_path_to_error::Track::new();
        let serializer =
            serde_path_to_error::Serializer::new(serde_json::value::Serializer, &mut track);
        let encoded = value
            .serialize(serializer)
            .map_err(|source| ModelError::Encode { path: track.path().to_string(), source })?;
        self.validate(&encoded)?;
        Ok(encoded)
    }
}
