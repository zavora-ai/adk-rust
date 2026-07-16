use crate::document::JsonSchemaDialect;
use thiserror::Error;

/// Result type for schema operations.
pub type Result<T> = std::result::Result<T, SchemaError>;

/// Categories of resource limit exhaustion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    /// Input raw source bytes exceeded the limit.
    SourceBytes,
    /// Canonical serialized bytes exceeded the limit.
    CanonicalBytes,
    /// Structural nesting depth exceeded the limit.
    NestingDepth,
    /// Node count exceeded the limit.
    NodeCount,
    /// Reference count exceeded the limit.
    ReferenceCount,
}

/// Specific reasons for rejecting a schema reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceRejection {
    /// Non-local references (such as http or file URIs).
    NonLocalReference,
    /// Named anchors (such as #node).
    UnsupportedAnchor,
    /// The `$dynamicRef` keyword.
    UnsupportedDynamicRef,
    /// Malformed JSON pointer format or escape sequences.
    MalformedPointer,
    /// Nested `$id` declarations below the root.
    NestedId,
    /// Nested `$schema` declarations below the root.
    NestedSchema,
    /// The resolved reference target is not a valid schema (must be object or boolean).
    InvalidSchemaTarget,
}

/// Description of an invalid field location and message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationIssue {
    /// JSON pointer path to the invalid field.
    pub pointer: String,
    /// Validation message.
    pub message: String,
}

/// Error type returned by schema parsing and validation functions.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SchemaError {
    /// Failed to parse raw JSON.
    #[error("parse error: {message}")]
    Parse {
        /// Parse error message.
        message: String,
    },
    /// A configured resource limit was exceeded.
    #[error("limit exceeded: kind={kind:?}, limit={limit}, observed={observed}, pointer={pointer}")]
    LimitExceeded {
        /// The limit category.
        kind: LimitKind,
        /// Configured limit threshold.
        limit: usize,
        /// Observed count.
        observed: usize,
        /// JSON Pointer path where the limit was crossed.
        pointer: String,
    },
    /// An unsupported reference type or syntax was found.
    #[error("unsupported reference at {pointer}: {reference} ({reason:?})")]
    UnsupportedReference {
        /// JSON Pointer path to the `$ref` key.
        pointer: String,
        /// Raw reference target string.
        reference: String,
        /// The rejection cause.
        reason: ReferenceRejection,
    },
    /// A local reference target was not found in the document.
    #[error("missing reference at {pointer}: {reference}")]
    MissingReference {
        /// JSON Pointer path to the `$ref` key.
        pointer: String,
        /// Target reference string.
        reference: String,
    },
    /// A cyclic loop was detected in the local reference graph.
    #[error("reference cycle: {cycle:?}")]
    ReferenceCycle {
        /// Path representing the circular loop.
        cycle: Vec<String>,
    },
    /// The `$schema` tag in the document does not match the expected dialect.
    #[error("dialect mismatch: declared={declared}, expected={expected:?}")]
    DialectMismatch {
        /// Declared schema URI.
        declared: String,
        /// Expected dialect.
        expected: JsonSchemaDialect,
    },
    /// The schema document itself is invalid.
    #[error("invalid schema: {issues:?}")]
    InvalidSchema {
        /// Structural validation issues.
        issues: Vec<ValidationIssue>,
    },
    /// The instance data does not match the schema.
    #[error("invalid instance: {issues:?}")]
    InvalidInstance {
        /// Instance validation issues.
        issues: Vec<ValidationIssue>,
    },
    /// Canonicalization encoding failed.
    #[error("canonicalization error: {message}")]
    Canonicalization {
        /// Error message.
        message: String,
    },
    /// Serialization failed.
    #[error("serialization error: {0}")]
    Serialization(String),
}
