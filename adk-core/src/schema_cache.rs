//! Schema normalization cache for LLM provider adapters.
//!
//! Caches normalized schemas keyed by a content hash of the serialized JSON,
//! avoiding redundant normalization when the same tool schema is encountered
//! across multiple requests.
//!
//! # Example
//!
//! ```rust
//! use adk_core::{SchemaCache, SchemaAdapter, GenericSchemaAdapter};
//! use serde_json::json;
//!
//! let cache = SchemaCache::new();
//! let adapter = GenericSchemaAdapter;
//! let schema = json!({"type": "object", "properties": {"name": {"type": "string"}}});
//!
//! // First call normalizes and caches
//! let result1 = cache.get_or_normalize(&schema, &adapter);
//!
//! // Second call returns cached value without re-normalizing
//! let result2 = cache.get_or_normalize(&schema, &adapter);
//! assert_eq!(result1, result2);
//! ```

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Mutex;

use serde_json::Value;

use crate::SchemaAdapter;

/// A thread-safe cache for normalized JSON Schemas.
///
/// Stores normalized schemas keyed by a 64-bit hash of the serialized input schema.
/// This avoids re-running normalization transforms on unchanged schemas across
/// repeated `generate_content()` calls.
///
/// # Thread Safety
///
/// Uses [`std::sync::Mutex`] internally, making it safe to share across threads.
/// The lock is held only briefly during hash lookup and insertion.
///
/// # Placement
///
/// Intended to live on model instances so each provider adapter maintains its own
/// cache of normalized schemas.
#[derive(Debug, Default)]
pub struct SchemaCache {
    entries: Mutex<HashMap<u64, Value>>,
}

impl SchemaCache {
    /// Creates a new empty schema cache.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::SchemaCache;
    ///
    /// let cache = SchemaCache::new();
    /// ```
    pub fn new() -> Self {
        Self { entries: Mutex::new(HashMap::new()) }
    }

    /// Returns the normalized schema for the given input, using the cache if available.
    ///
    /// If the schema has been normalized before (based on content hash), the cached
    /// result is returned. Otherwise, `adapter.normalize_schema()` is called and the
    /// result is stored in the cache.
    ///
    /// # Arguments
    ///
    /// * `schema` - The raw JSON Schema to normalize.
    /// * `adapter` - The provider-specific schema adapter to use for normalization.
    ///
    /// # Returns
    ///
    /// The normalized JSON Schema value.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{SchemaCache, SchemaAdapter, GenericSchemaAdapter};
    /// use serde_json::json;
    ///
    /// let cache = SchemaCache::new();
    /// let adapter = GenericSchemaAdapter;
    /// let schema = json!({"$schema": "draft-07", "type": "string"});
    ///
    /// let normalized = cache.get_or_normalize(&schema, &adapter);
    /// assert!(normalized.get("$schema").is_none());
    /// ```
    pub fn get_or_normalize(&self, schema: &Value, adapter: &dyn SchemaAdapter) -> Value {
        let hash = Self::hash_schema(schema);
        let mut cache = self.entries.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.entry(hash).or_insert_with(|| adapter.normalize_schema(schema.clone())).clone()
    }

    /// Clears all cached entries.
    ///
    /// Call this when the set of tools changes (e.g., MCP server advertises
    /// updated schemas) to force re-normalization on the next request.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{SchemaCache, GenericSchemaAdapter};
    /// use serde_json::json;
    ///
    /// let cache = SchemaCache::new();
    /// let adapter = GenericSchemaAdapter;
    /// let schema = json!({"type": "string"});
    ///
    /// // Populate cache
    /// cache.get_or_normalize(&schema, &adapter);
    ///
    /// // Invalidate all entries
    /// cache.clear();
    /// ```
    pub fn clear(&self) {
        let mut cache = self.entries.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.clear();
    }

    /// Returns the number of cached entries.
    pub fn len(&self) -> usize {
        let cache = self.entries.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.len()
    }

    /// Returns `true` if the cache contains no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Computes a 64-bit hash of the serialized schema bytes.
    ///
    /// Uses `serde_json::to_vec` for deterministic serialization and
    /// `DefaultHasher` (SipHash) for the hash function.
    fn hash_schema(schema: &Value) -> u64 {
        let bytes = serde_json::to_vec(schema).unwrap_or_default();
        let mut hasher = DefaultHasher::new();
        bytes.hash(&mut hasher);
        hasher.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::GenericSchemaAdapter;

    #[test]
    fn test_cache_returns_normalized_schema() {
        let cache = SchemaCache::new();
        let adapter = GenericSchemaAdapter;
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": { "name": { "type": "string" } }
        });

        let result = cache.get_or_normalize(&schema, &adapter);
        assert!(result.get("$schema").is_none());
        assert_eq!(result["type"], "object");
    }

    #[test]
    fn test_cache_returns_same_result_on_repeated_calls() {
        let cache = SchemaCache::new();
        let adapter = GenericSchemaAdapter;
        let schema = json!({
            "type": "object",
            "properties": { "x": { "type": "integer", "const": 42 } }
        });

        let first = cache.get_or_normalize(&schema, &adapter);
        let second = cache.get_or_normalize(&schema, &adapter);
        assert_eq!(first, second);
    }

    #[test]
    fn test_cache_stores_entries() {
        let cache = SchemaCache::new();
        let adapter = GenericSchemaAdapter;

        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);

        let schema1 = json!({"type": "string"});
        let schema2 = json!({"type": "number"});

        cache.get_or_normalize(&schema1, &adapter);
        assert_eq!(cache.len(), 1);

        cache.get_or_normalize(&schema2, &adapter);
        assert_eq!(cache.len(), 2);

        // Same schema doesn't add a new entry
        cache.get_or_normalize(&schema1, &adapter);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn test_cache_clear_removes_all_entries() {
        let cache = SchemaCache::new();
        let adapter = GenericSchemaAdapter;

        cache.get_or_normalize(&json!({"type": "string"}), &adapter);
        cache.get_or_normalize(&json!({"type": "number"}), &adapter);
        assert_eq!(cache.len(), 2);

        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn test_cache_different_schemas_produce_different_entries() {
        let cache = SchemaCache::new();
        let adapter = GenericSchemaAdapter;

        let schema_a = json!({"type": "string", "format": "hostname"});
        let schema_b = json!({"type": "string", "format": "email"});

        let result_a = cache.get_or_normalize(&schema_a, &adapter);
        let result_b = cache.get_or_normalize(&schema_b, &adapter);

        // "hostname" is stripped, "email" is preserved
        assert!(result_a.get("format").is_none());
        assert_eq!(result_b["format"], "email");
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn test_cache_new_is_empty() {
        let cache = SchemaCache::new();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn test_cache_default_is_empty() {
        let cache = SchemaCache::default();
        assert!(cache.is_empty());
    }

    #[test]
    fn test_cache_handles_empty_schema() {
        let cache = SchemaCache::new();
        let adapter = GenericSchemaAdapter;
        let schema = json!({});

        let result = cache.get_or_normalize(&schema, &adapter);
        assert_eq!(result, json!({}));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn test_cache_handles_null_schema() {
        let cache = SchemaCache::new();
        let adapter = GenericSchemaAdapter;
        let schema = Value::Null;

        let result = cache.get_or_normalize(&schema, &adapter);
        // GenericSchemaAdapter passes through non-object values
        assert_eq!(result, Value::Null);
        assert_eq!(cache.len(), 1);
    }
}
