//! # adk-schema
//!
//! Canonical JSON Schema documents and validation for ADK.
//!
//! The default `typed` feature adds [`InputModel`] and [`OutputModel`], which
//! bind a Rust type to one compiled, canonical Draft 2020-12 schema. Disable
//! default features to use dynamic [`InputSchema`] and [`OutputSchema`] values
//! without Schemars, Serde, or runtime validation dependencies.
//!
//! ## Typed models
//!
//! ```
//! # #[cfg(feature = "typed")]
//! # fn main() -> Result<(), adk_schema::ModelError> {
//! use adk_schema::{InputModel, OutputModel};
//! use schemars::JsonSchema;
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Debug, Deserialize, JsonSchema, PartialEq)]
//! struct Request {
//!     message: String,
//! }
//!
//! #[derive(JsonSchema, Serialize)]
//! struct Response {
//!     accepted: bool,
//! }
//!
//! let input = InputModel::<Request>::new()?;
//! let request = input.parse_str(r#"{"message":"hello"}"#)?;
//! assert_eq!(request, Request { message: "hello".into() });
//!
//! let output = OutputModel::<Response>::new()?;
//! assert_eq!(
//!     output.encode_value(&Response { accepted: true })?,
//!     serde_json::json!({"accepted": true}),
//! );
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "typed"))]
//! # fn main() {}
//! ```
//!
//! ## Role Separation Compile-Time Assertions
//!
//! ```compile_fail
//! use adk_schema::{InputSchema, OutputSchema};
//!
//! fn requires_output(_: OutputSchema) {}
//!
//! # fn example(input: InputSchema) {
//! requires_output(input);
//! # }
//! ```

#![deny(missing_docs)]

mod canonical;
mod digest;
mod document;
mod error;
mod ingest;
#[cfg(feature = "typed")]
mod model;
mod policy;
mod references;
mod role;
#[cfg(feature = "schemars")]
mod static_schema;
#[cfg(feature = "runtime-validation")]
mod validation;

pub use digest::SchemaDigest;
pub use document::{JsonSchemaDialect, SchemaDirection, SchemaDocument, SchemaMetrics};
pub use error::{LimitKind, ReferenceRejection, Result, SchemaError, ValidationIssue};
pub use policy::{IngestionPolicy, ReferencePolicy};
pub use role::{Input, InputSchema, Output, OutputSchema, SchemaRole};

#[cfg(feature = "typed")]
pub use model::{InputModel, Model, ModelError, ModelResult, OutputModel};

#[cfg(feature = "runtime-validation")]
pub use validation::ValidatedSchemaDocument;
