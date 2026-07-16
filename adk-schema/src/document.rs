use crate::digest::{SchemaDigest, calculate_digest};
use crate::error::{LimitKind, Result, SchemaError};
use crate::policy::IngestionPolicy;
use crate::role::SchemaRole;
use serde_json::Value;
use std::marker::PhantomData;
use std::sync::Arc;

/// Runtime representation of a schema's input/output direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SchemaDirection {
    /// Schema is intended for inputs.
    Input,
    /// Schema is intended for outputs.
    Output,
}

/// JSON Schema dialects supported by ADK.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum JsonSchemaDialect {
    /// Draft 2020-12
    #[default]
    Draft202012,
}

impl JsonSchemaDialect {
    pub(crate) const fn digest_tag(self) -> u8 {
        match self {
            Self::Draft202012 => 1,
        }
    }
}

/// Measured metrics for the ingested schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemaMetrics {
    /// Maximum nesting depth.
    pub depth: usize,
    /// Total parsed node count.
    pub node_count: usize,
    /// Total resolved local references.
    pub reference_count: usize,
}

/// A canonicalized, type-safe schema document.
pub struct SchemaDocument<R: SchemaRole> {
    inner: Arc<SchemaInner>,
    _role: PhantomData<R>,
}

#[derive(Debug)]
pub(crate) struct SchemaInner {
    pub(crate) dialect: JsonSchemaDialect,
    pub(crate) canonical_value: Value,
    pub(crate) canonical_bytes: Arc<[u8]>,
    pub(crate) digest: SchemaDigest,
    pub(crate) metrics: SchemaMetrics,
}

impl<R: SchemaRole> Clone for SchemaDocument<R> {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone(), _role: PhantomData }
    }
}

impl<R: SchemaRole> std::fmt::Debug for SchemaDocument<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SchemaDocument").field("inner", &self.inner).finish()
    }
}

impl<R: SchemaRole> PartialEq for SchemaDocument<R> {
    fn eq(&self, other: &Self) -> bool {
        self.inner.dialect == other.inner.dialect
            && self.inner.digest == other.inner.digest
            && self.inner.canonical_bytes == other.inner.canonical_bytes
    }
}

impl<R: SchemaRole> Eq for SchemaDocument<R> {}

impl<R: SchemaRole> SchemaDocument<R> {
    pub(crate) fn from_parts(
        canonical_value: Value,
        canonical_bytes: Vec<u8>,
        digest: SchemaDigest,
        metrics: SchemaMetrics,
        dialect: JsonSchemaDialect,
    ) -> Self {
        Self {
            inner: Arc::new(SchemaInner {
                dialect,
                canonical_value,
                canonical_bytes: canonical_bytes.into(),
                digest,
                metrics,
            }),
            _role: PhantomData,
        }
    }

    /// Parse a JSON schema document from raw bytes under the specified ingestion policy.
    pub fn from_json_slice(source: &[u8], policy: &IngestionPolicy) -> Result<Self> {
        if source.len() > policy.max_source_bytes {
            return Err(SchemaError::LimitExceeded {
                kind: LimitKind::SourceBytes,
                limit: policy.max_source_bytes,
                observed: source.len(),
                pointer: String::new(),
            });
        }
        let value: Value = serde_json::from_slice(source)
            .map_err(|e| SchemaError::Parse { message: e.to_string() })?;
        Self::from_value(value, policy)
    }

    /// Ingest and validate a pre-parsed JSON schema document under the specified policy.
    pub fn from_value(value: Value, policy: &IngestionPolicy) -> Result<Self> {
        let structural_metrics = crate::ingest::scan_all_nodes_iteratively(&value, policy)?;
        let scan = crate::ingest::scan_schema_locations_iteratively(&value, policy)?;
        match policy.references {
            crate::policy::ReferencePolicy::LocalJsonPointerAcyclic => {
                crate::ingest::validate_reference_graph(
                    &value,
                    &scan.references,
                    &scan.schema_locations,
                    policy,
                )?;
            }
        }

        let canonical_value = crate::canonical::canonicalize(value, policy.dialect)?;
        let canonical_bytes =
            crate::canonical::serialize_bounded(&canonical_value, policy.max_canonical_bytes)?;
        let digest = calculate_digest::<R>(policy.dialect, &canonical_bytes);

        let metrics = SchemaMetrics {
            depth: structural_metrics.depth,
            node_count: structural_metrics.node_count,
            reference_count: scan.references.len(),
        };

        Ok(Self::from_parts(canonical_value, canonical_bytes, digest, metrics, policy.dialect))
    }

    /// Borrow the canonical JSON Value.
    pub fn as_value(&self) -> &Value {
        &self.inner.canonical_value
    }
    /// Access the raw canonical bytes representation.
    pub fn canonical_bytes(&self) -> &[u8] {
        &self.inner.canonical_bytes
    }
    /// Expose the unique identity digest.
    pub fn digest(&self) -> SchemaDigest {
        self.inner.digest
    }
    /// Access structural metrics.
    pub fn metrics(&self) -> SchemaMetrics {
        self.inner.metrics
    }
    /// Access the JSON Schema dialect.
    pub fn dialect(&self) -> JsonSchemaDialect {
        self.inner.dialect
    }
}
