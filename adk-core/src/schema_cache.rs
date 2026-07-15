//! Schema normalization cache for LLM provider adapters.
use crate::SchemaAdapter;
use crate::schema_adapter::SchemaCompileError;
use serde_json::Value;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Mutex;

/// A thread-safe cache for normalized and compiled JSON Schemas.
#[derive(Debug, Default)]
pub struct SchemaCache {
    entries: Mutex<HashMap<u64, Value>>,
}

impl SchemaCache {
    /// Creates a new empty schema cache.
    pub fn new() -> Self {
        Self { entries: Mutex::new(HashMap::new()) }
    }

    /// Returns the normalized schema for the given input, using the cache if available.
    pub fn get_or_normalize(&self, schema: &Value, adapter: &dyn SchemaAdapter) -> Value {
        let hash = Self::hash_schema_with_adapter(schema, adapter, "normalize");
        let mut cache = self.entries.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.entry(hash).or_insert_with(|| adapter.normalize_schema(schema.clone())).clone()
    }

    /// Returns the compiled schema for the given input, using the cache if available.
    pub fn get_or_compile(
        &self,
        schema: &Value,
        adapter: &dyn SchemaAdapter,
    ) -> Result<Value, SchemaCompileError> {
        let hash = Self::hash_schema_with_adapter(schema, adapter, "compile");
        let mut cache = self.entries.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

        if let Some(cached) = cache.get(&hash) {
            return Ok(cached.clone());
        }

        let compiled = adapter.compile_schema(schema)?;
        cache.insert(hash, compiled.clone());
        Ok(compiled)
    }

    /// Clears all cached entries.
    pub fn clear(&self) {
        let mut cache = self.entries.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.clear();
    }

    /// Returns the number of cached entries.
    pub fn len(&self) -> usize {
        let cache = self.entries.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.len()
    }

    /// Returns true if the cache contains no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn hash_schema_with_adapter(
        schema: &Value,
        adapter: &dyn SchemaAdapter,
        operation: &str,
    ) -> u64 {
        // Use a robust, collision-resistant identity for the cache key.
        let mut hasher = DefaultHasher::new();

        // 1. Identity of the schema content itself.
        // We use the JSON string representation as a stable, canonical identity.
        // If serialization fails (pathological), we hash a fallback sentinel.
        match serde_json::to_vec(schema) {
            Ok(bytes) => bytes.hash(&mut hasher),
            Err(_) => "serialization-failure-sentinel".hash(&mut hasher),
        }

        // 2. Identity of the compiler/adapter.
        adapter.identifier().hash(&mut hasher);
        adapter.version().hash(&mut hasher);
        adapter.surface().hash(&mut hasher);

        // 3. Identity of the operation type.
        operation.hash(&mut hasher);

        hasher.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GenericSchemaAdapter;
    use serde_json::json;

    #[derive(Debug)]
    struct MockAdapter(&'static str);
    impl crate::SchemaAdapter for MockAdapter {
        fn identifier(&self) -> &str {
            self.0
        }
        fn normalize_schema(&self, schema: Value) -> Value {
            schema
        }
    }

    #[test]
    fn test_cache_separation_by_adapter() {
        let cache = SchemaCache::new();
        let schema = json!({"type": "string"});

        let adapter1 = MockAdapter("a1");
        let adapter2 = MockAdapter("a2");

        // Use get_or_normalize to insert into cache
        cache.get_or_normalize(&schema, &adapter1);
        assert_eq!(cache.len(), 1);

        cache.get_or_normalize(&schema, &adapter2);
        assert_eq!(cache.len(), 2, "Cache should have separate entries for different adapters");
    }

    #[test]
    fn test_cache_separation_by_operation() {
        let cache = SchemaCache::new();
        let schema = json!({"type": "string"});
        let adapter = GenericSchemaAdapter;

        cache.get_or_normalize(&schema, &adapter);
        assert_eq!(cache.len(), 1);

        cache.get_or_compile(&schema, &adapter).unwrap();
        assert_eq!(cache.len(), 2, "Cache should have separate entries for normalize vs compile");
    }

    #[test]
    fn test_cache_hit() {
        let cache = SchemaCache::new();
        let schema = json!({"type": "string"});
        let adapter = GenericSchemaAdapter;

        cache.get_or_normalize(&schema, &adapter);
        assert_eq!(cache.len(), 1);

        cache.get_or_normalize(&schema, &adapter);
        assert_eq!(cache.len(), 1, "Cache should hit for same schema and adapter");
    }
}
