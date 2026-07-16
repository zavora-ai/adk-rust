use crate::document::JsonSchemaDialect;

/// Limits for resource consumption during schema ingestion.
#[derive(Debug, Clone)]
pub struct IngestionPolicy {
    /// Dialect target.
    pub dialect: JsonSchemaDialect,
    /// Maximum allowed input size in bytes before parsing.
    pub max_source_bytes: usize,
    /// Maximum allowed canonical size in bytes.
    pub max_canonical_bytes: usize,
    /// Maximum allowed nesting depth.
    pub max_depth: usize,
    /// Maximum total node count (properties, array elements, primitives).
    pub max_nodes: usize,
    /// Maximum allowed reference resolutions.
    pub max_references: usize,
    /// Reference resolution and cycle rejection policy.
    pub references: ReferencePolicy,
}

impl Default for IngestionPolicy {
    fn default() -> Self {
        Self {
            dialect: JsonSchemaDialect::Draft202012,
            max_source_bytes: 1024 * 1024,
            max_canonical_bytes: 1024 * 1024,
            max_depth: 32,
            max_nodes: 5000,
            max_references: 500,
            references: ReferencePolicy::LocalJsonPointerAcyclic,
        }
    }
}

/// Supported reference resolution policies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferencePolicy {
    /// Strict local JSON Pointer resolution with iterative cycle checks.
    LocalJsonPointerAcyclic,
}
