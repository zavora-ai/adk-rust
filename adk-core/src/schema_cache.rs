//! Schema normalization cache for LLM provider adapters.
//!
//! Binds one adapter to each cache and stores normalized schemas by content hash,
//! avoiding redundant normalization without allowing one adapter's result to be
//! returned for another adapter. The hash ignores the order object keys were
//! written in, so when `serde_json/preserve_order` is enabled — where insertion
//! order would otherwise change the key — one schema built two different ways is
//! one entry.
//!
//! # Example
//!
//! ```rust
//! use adk_core::{GenericSchemaAdapter, SchemaCache};
//! use serde_json::json;
//! use std::sync::Arc;
//!
//! let cache = SchemaCache::for_adapter(Arc::new(GenericSchemaAdapter));
//! let schema = json!({"type": "object", "properties": {"name": {"type": "string"}}});
//!
//! // First call normalizes and caches
//! let result1 = cache.normalize(&schema);
//!
//! // Second call returns cached value without re-normalizing
//! let result2 = cache.normalize(&schema);
//! assert_eq!(result1, result2);
//! ```

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::{GenericSchemaAdapter, SchemaAdapter};

/// A thread-safe cache for normalized JSON Schemas.
///
/// An adapter-bound cache stores normalized schemas keyed by a 64-bit hash of the
/// input schema. Binding the adapter at construction prevents results produced by
/// different adapter instances from sharing an entry.
///
/// # Thread Safety
///
/// Uses [`std::sync::Mutex`] internally, making it safe to share across threads.
/// The lock is held only for the lookup and the insertion, never while the adapter
/// normalizes, so a slow schema delays only the request that carries it.
///
/// # Placement
///
/// Intended to live on model instances so each provider adapter maintains its own
/// cache of normalized schemas. Use [`SchemaCache::new`] for the generic adapter
/// or [`SchemaCache::for_adapter`] for a provider-specific adapter.
#[derive(Debug)]
pub struct SchemaCache {
    adapter: Arc<dyn SchemaAdapter>,
    entries: Mutex<HashMap<CacheKey, Value>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum CacheKey {
    Bound(u64),
    Legacy { schema: u64, normalized: u64 },
}

/// Adds a value to the hash in a fixed order.
///
/// - **Object keys are sorted first.** The order they were written in cannot
///   change the hash.
/// - **No JSON values are cloned.** Object members are collected into a
///   temporary `Vec` for sorting; this runs on every lookup, including hits.
/// - **Each kind of value gets its own marker.** The text `"1"` and the number
///   `1` hash differently.
fn hash_canonical(value: &Value, hasher: &mut DefaultHasher) {
    match value {
        Value::Null => 0u8.hash(hasher),
        Value::Bool(flag) => {
            1u8.hash(hasher);
            flag.hash(hasher);
        }
        Value::Number(number) => {
            2u8.hash(hasher);
            // The textual form, because it is exact in every build. Going
            // through `as_f64` loses precision when `serde_json` is built with
            // `arbitrary_precision`, where a literal wider than f64 is kept
            // verbatim: distinct numbers would round together and share a cache
            // entry, and a literal outside f64 entirely would hash to nothing.
            number.to_string().hash(hasher);
        }
        Value::String(text) => {
            3u8.hash(hasher);
            text.hash(hasher);
        }
        Value::Array(items) => {
            4u8.hash(hasher);
            items.len().hash(hasher);
            for item in items {
                hash_canonical(item, hasher);
            }
        }
        Value::Object(members) => {
            5u8.hash(hasher);
            members.len().hash(hasher);
            let mut entries: Vec<(&String, &Value)> = members.iter().collect();
            entries.sort_unstable_by_key(|(key, _)| *key);
            for (key, member) in entries {
                key.hash(hasher);
                hash_canonical(member, hasher);
            }
        }
    }
}

impl SchemaCache {
    /// Creates an empty cache bound to [`GenericSchemaAdapter`].
    ///
    /// Use [`SchemaCache::for_adapter`] when normalization requires a
    /// provider-specific adapter.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::SchemaCache;
    /// use serde_json::json;
    ///
    /// let cache = SchemaCache::new();
    /// let normalized = cache.normalize(&json!({"type": "string"}));
    /// ```
    pub fn new() -> Self {
        Self::for_adapter(Arc::new(GenericSchemaAdapter))
    }

    /// Creates an empty cache bound to one schema adapter.
    ///
    /// The cache owns the adapter, so every entry is guaranteed to have been
    /// produced by that adapter instance.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{GenericSchemaAdapter, SchemaCache};
    /// use std::sync::Arc;
    ///
    /// let cache = SchemaCache::for_adapter(Arc::new(GenericSchemaAdapter));
    /// assert!(cache.is_empty());
    /// ```
    pub fn for_adapter(adapter: Arc<dyn SchemaAdapter>) -> Self {
        Self { adapter, entries: Mutex::new(HashMap::new()) }
    }

    /// Returns the schema normalized by this cache's adapter.
    ///
    /// If the same input schema has been normalized before, the cached result is
    /// returned. Otherwise, the bound adapter normalizes the schema and the
    /// result is stored.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{GenericSchemaAdapter, SchemaCache};
    /// use serde_json::json;
    /// use std::sync::Arc;
    ///
    /// let cache = SchemaCache::for_adapter(Arc::new(GenericSchemaAdapter));
    /// let schema = json!({"$schema": "draft-07", "type": "string"});
    ///
    /// let normalized = cache.normalize(&schema);
    /// assert!(normalized.get("$schema").is_none());
    /// ```
    pub fn normalize(&self, schema: &Value) -> Value {
        let key = CacheKey::Bound(Self::hash_schema(schema));
        if let Some(cached) = self.lock_entries().get(&key) {
            return cached.clone();
        }
        // Normalized without the lock, so one slow schema does not stall every other
        // request sharing this cache; two threads may then both normalize it once.
        let normalized = self.adapter.normalize_schema(schema.clone());
        self.lock_entries().entry(key).or_insert(normalized).clone()
    }

    fn lock_entries(&self) -> std::sync::MutexGuard<'_, HashMap<CacheKey, Value>> {
        self.entries.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Returns a schema normalized by the supplied adapter.
    ///
    /// This compatibility path normalizes before looking up the result because a
    /// borrowed trait object has no stable identity that the cache can safely
    /// retain. Use [`SchemaCache::for_adapter`] and [`SchemaCache::normalize`] to
    /// avoid repeated normalization.
    #[deprecated(
        note = "bind the adapter with SchemaCache::for_adapter and call SchemaCache::normalize"
    )]
    pub fn get_or_normalize(&self, schema: &Value, adapter: &dyn SchemaAdapter) -> Value {
        let schema_hash = Self::hash_schema(schema);
        let normalized = adapter.normalize_schema(schema.clone());
        let key =
            CacheKey::Legacy { schema: schema_hash, normalized: Self::hash_schema(&normalized) };
        self.lock_entries().entry(key).or_insert(normalized).clone()
    }

    /// Clears all cached entries.
    ///
    /// Call this when the set of tools changes (e.g., MCP server advertises
    /// updated schemas) to force re-normalization on the next request.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{GenericSchemaAdapter, SchemaCache};
    /// use serde_json::json;
    /// use std::sync::Arc;
    ///
    /// let cache = SchemaCache::for_adapter(Arc::new(GenericSchemaAdapter));
    /// let schema = json!({"type": "string"});
    ///
    /// // Populate cache
    /// cache.normalize(&schema);
    ///
    /// // Invalidate all entries
    /// cache.clear();
    /// ```
    pub fn clear(&self) {
        self.lock_entries().clear();
    }

    /// Returns the number of cached entries.
    pub fn len(&self) -> usize {
        self.lock_entries().len()
    }

    /// Returns `true` if the cache contains no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Computes a 64-bit hash of the schema's contents.
    ///
    /// Object keys are sorted before hashing, so the order they were written in
    /// does not change the result. When `serde_json/preserve_order` is enabled,
    /// keys stay in insertion order, and without sorting the same schema built
    /// two different ways would occupy two entries and be normalized twice.
    ///
    /// Numbers are hashed as written: `5` and `5.0` are separate entries. That
    /// costs an extra normalization and never returns the wrong schema.
    fn hash_schema(schema: &Value) -> u64 {
        let mut hasher = DefaultHasher::new();
        hash_canonical(schema, &mut hasher);
        hasher.finish()
    }
}

impl Default for SchemaCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::GenericSchemaAdapter;

    fn generic_cache() -> SchemaCache {
        SchemaCache::for_adapter(Arc::new(GenericSchemaAdapter))
    }

    #[derive(Debug)]
    struct TaggedAdapter(&'static str);

    impl SchemaAdapter for TaggedAdapter {
        fn normalize_schema(&self, mut schema: Value) -> Value {
            schema
                .as_object_mut()
                .expect("test schema should be an object")
                .insert("normalized_by".to_string(), Value::String(self.0.to_string()));
            schema
        }
    }

    #[derive(Debug)]
    struct CountingAdapter(Arc<AtomicUsize>);

    impl SchemaAdapter for CountingAdapter {
        fn normalize_schema(&self, schema: Value) -> Value {
            self.0.fetch_add(1, Ordering::Relaxed);
            schema
        }
    }

    #[test]
    fn test_cache_returns_normalized_schema() {
        let cache = generic_cache();
        let schema = json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": { "name": { "type": "string" } }
        });

        let result = cache.normalize(&schema);
        assert!(result.get("$schema").is_none());
        assert_eq!(result["type"], "object");
    }

    #[test]
    fn test_cache_returns_same_result_on_repeated_calls() {
        let cache = generic_cache();
        let schema = json!({
            "type": "object",
            "properties": { "x": { "type": "integer", "const": 42 } }
        });

        let first = cache.normalize(&schema);
        let second = cache.normalize(&schema);
        assert_eq!(first, second);
    }

    #[test]
    fn repeated_calls_only_invoke_the_bound_adapter_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let cache = SchemaCache::for_adapter(Arc::new(CountingAdapter(Arc::clone(&calls))));
        let schema = json!({"type": "string"});

        cache.normalize(&schema);
        cache.normalize(&schema);

        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn adapter_instances_cannot_share_entries() {
        let schema = json!({"type": "object"});
        let alpha = SchemaCache::for_adapter(Arc::new(TaggedAdapter("alpha")));
        let beta = SchemaCache::for_adapter(Arc::new(TaggedAdapter("beta")));

        assert_eq!(alpha.normalize(&schema)["normalized_by"], "alpha");
        assert_eq!(beta.normalize(&schema)["normalized_by"], "beta");
        assert_eq!(alpha.len(), 1);
        assert_eq!(beta.len(), 1);
    }

    #[test]
    #[allow(deprecated)]
    fn deprecated_api_keeps_adapter_results_separate() {
        let cache = SchemaCache::new();
        let schema = json!({"type": "object"});

        let alpha = cache.get_or_normalize(&schema, &TaggedAdapter("alpha"));
        let beta = cache.get_or_normalize(&schema, &TaggedAdapter("beta"));

        assert_eq!(alpha["normalized_by"], "alpha");
        assert_eq!(beta["normalized_by"], "beta");
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn test_cache_stores_entries() {
        let cache = generic_cache();

        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);

        let schema1 = json!({"type": "string"});
        let schema2 = json!({"type": "number"});

        cache.normalize(&schema1);
        assert_eq!(cache.len(), 1);

        cache.normalize(&schema2);
        assert_eq!(cache.len(), 2);

        // Same schema doesn't add a new entry
        cache.normalize(&schema1);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn test_cache_clear_removes_all_entries() {
        let cache = generic_cache();

        cache.normalize(&json!({"type": "string"}));
        cache.normalize(&json!({"type": "number"}));
        assert_eq!(cache.len(), 2);

        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn test_cache_different_schemas_produce_different_entries() {
        let cache = generic_cache();

        let schema_a = json!({"type": "string", "format": "hostname"});
        let schema_b = json!({"type": "string", "format": "email"});

        let result_a = cache.normalize(&schema_a);
        let result_b = cache.normalize(&schema_b);

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
        let cache = generic_cache();
        let schema = json!({});

        let result = cache.normalize(&schema);
        assert_eq!(result, json!({}));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn test_cache_handles_null_schema() {
        let cache = generic_cache();
        let schema = Value::Null;

        let result = cache.normalize(&schema);
        // GenericSchemaAdapter passes through non-object values
        assert_eq!(result, Value::Null);
        assert_eq!(cache.len(), 1);
    }

    /// Two schemas differing only in key order are one schema, so they share a
    /// cache entry rather than each paying for normalization.
    #[test]
    fn key_order_does_not_create_a_second_entry() {
        let cache = generic_cache();

        cache.normalize(&json!({ "type": "object", "properties": { "a": {}, "b": {} } }));
        cache.normalize(&json!({ "properties": { "b": {}, "a": {} }, "type": "object" }));

        assert_eq!(cache.len(), 1, "key order must not change a schema's identity");
    }

    #[test]
    fn genuinely_different_schemas_keep_separate_entries() {
        let cache = generic_cache();

        cache.normalize(&json!({ "type": "string" }));
        cache.normalize(&json!({ "type": "integer" }));

        assert_eq!(cache.len(), 2);
    }

    /// A property name containing pointer or escape syntax must not collapse
    /// two schemas onto one key.
    #[test]
    fn unusual_property_names_stay_distinct() {
        let cache = generic_cache();

        cache.normalize(&json!({ "properties": { "a/b": {} } }));
        cache.normalize(&json!({ "properties": { "a~b": {} } }));

        assert_eq!(cache.len(), 2);
    }

    /// Text and a number that render alike must not collide.
    #[test]
    fn a_string_and_a_number_hash_differently() {
        let cache = generic_cache();

        cache.normalize(&json!({ "const": "1" }));
        cache.normalize(&json!({ "const": 1 }));

        assert_eq!(cache.len(), 2);
    }

    /// Numbers are identified by their written form, so `5` and `5.0` are
    /// different schemas.
    ///
    /// This also closes an `arbitrary_precision` hazard that cannot be
    /// exercised here: with that feature a literal wider than f64 is kept
    /// verbatim, and identifying numbers by `as_f64` would round distinct
    /// values onto one cache entry. Without the feature `serde_json` already
    /// collapses such literals during parsing, so reproducing it would mean
    /// enabling the feature for every crate in the build.
    #[test]
    fn numbers_are_identified_by_their_written_form() {
        let cache = generic_cache();

        cache.normalize(&json!({ "const": 5 }));
        cache.normalize(&json!({ "const": 5.0 }));

        assert_eq!(cache.len(), 2);
    }

    /// Blocks normalizing any schema carrying `"slow": true` until released.
    #[derive(Debug)]
    struct GatedAdapter(std::sync::Mutex<std::sync::mpsc::Receiver<()>>);

    impl SchemaAdapter for GatedAdapter {
        fn normalize_schema(&self, schema: Value) -> Value {
            if schema.get("slow").is_some() {
                let _ = self.0.lock().map(|release| release.recv());
            }
            schema
        }
    }

    #[test]
    fn a_slow_normalization_does_not_block_other_schemas() {
        let (release, gate) = std::sync::mpsc::channel();
        let cache =
            Arc::new(SchemaCache::for_adapter(Arc::new(GatedAdapter(std::sync::Mutex::new(gate)))));
        let slow = {
            let cache = cache.clone();
            std::thread::spawn(move || cache.normalize(&json!({"slow": true})))
        };
        std::thread::sleep(std::time::Duration::from_millis(50));

        let (done, finished) = std::sync::mpsc::channel();
        let fast = {
            let cache = cache.clone();
            std::thread::spawn(move || {
                let _ = done.send(cache.normalize(&json!({"type": "string"})));
            })
        };
        let result = finished.recv_timeout(std::time::Duration::from_secs(5));
        release.send(()).expect("release the slow normalization");
        slow.join().expect("slow normalization thread");
        fast.join().expect("fast normalization thread");

        assert_eq!(result, Ok(json!({"type": "string"})));
        assert_eq!(cache.len(), 2);
    }
}
