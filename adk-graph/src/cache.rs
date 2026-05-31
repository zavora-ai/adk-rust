//! Node-level caching for graph execution.
//!
//! This module provides a caching layer that stores node execution results keyed
//! by a blake3 hash of the node name and its input state. Cached results are
//! returned on subsequent executions with identical inputs, avoiding redundant
//! computation.
//!
//! # Backends
//!
//! Two cache backends are supported:
//!
//! - **InMemory** (default) — an LRU cache with configurable maximum entries.
//! - **Redis** (behind the `redis-cache` feature) — a Redis-backed cache using
//!   the `fred` client.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_graph::cache::{CacheBackend, NodeCachePolicy, NodeCache, compute_cache_key};
//! use std::time::Duration;
//! use serde_json::json;
//! use std::collections::HashMap;
//!
//! // Define a cache policy with in-memory backend
//! let policy = NodeCachePolicy {
//!     backend: CacheBackend::InMemory { max_entries: 128 },
//!     ttl: Some(Duration::from_secs(300)),
//! };
//!
//! // Create a cache from the policy
//! let cache = NodeCache::from_policy(&policy);
//!
//! // Compute a cache key from node name and input state
//! let mut state = HashMap::new();
//! state.insert("input".to_string(), json!("hello"));
//! let key = compute_cache_key("my_node", &state);
//!
//! // Use the cache
//! # tokio_test::block_on(async {
//! assert!(cache.get(&key).await.is_none());
//! cache.set(&key, json!({"result": 42}), policy.ttl).await;
//! assert_eq!(cache.get(&key).await, Some(json!({"result": 42})));
//! # });
//! ```

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::Mutex;

use crate::state::State;

/// Internal type alias for the in-memory LRU cache storage.
type MemoryStore = Arc<Mutex<(LruCache, Option<Duration>)>>;

/// Cache backend selection.
///
/// Determines where cached node results are stored.
///
/// # Example
///
/// ```rust,ignore
/// use adk_graph::cache::CacheBackend;
///
/// // In-memory LRU cache with 256 max entries
/// let backend = CacheBackend::InMemory { max_entries: 256 };
///
/// // Redis backend (requires `redis-cache` feature)
/// #[cfg(feature = "redis-cache")]
/// let backend = CacheBackend::Redis { url: "redis://localhost:6379".to_string() };
/// ```
#[derive(Debug, Clone)]
pub enum CacheBackend {
    /// In-memory LRU cache with a configurable maximum number of entries.
    ///
    /// When the cache is full, the least recently used entry is evicted.
    InMemory {
        /// Maximum number of entries to store. When exceeded, the least
        /// recently used entry is evicted.
        max_entries: usize,
    },
    /// Redis-backed cache (requires the `redis-cache` feature).
    #[cfg(feature = "redis-cache")]
    Redis {
        /// Redis connection URL (e.g. `redis://localhost:6379`).
        url: String,
    },
}

impl Default for CacheBackend {
    fn default() -> Self {
        Self::InMemory { max_entries: 128 }
    }
}

/// Cache policy for a graph node.
///
/// Configures which backend to use and an optional time-to-live for cached entries.
///
/// # Example
///
/// ```rust,ignore
/// use adk_graph::cache::{CacheBackend, NodeCachePolicy};
/// use std::time::Duration;
///
/// let policy = NodeCachePolicy {
///     backend: CacheBackend::InMemory { max_entries: 64 },
///     ttl: Some(Duration::from_secs(60)),
/// };
/// ```
#[derive(Debug, Clone)]
pub struct NodeCachePolicy {
    /// The storage backend for cached results.
    pub backend: CacheBackend,
    /// Optional time-to-live for cached entries. Entries older than this
    /// duration are treated as cache misses. `None` means entries never expire.
    pub ttl: Option<Duration>,
}

/// Computes a deterministic cache key from a node name and its input state.
///
/// The key is the hex-encoded blake3 hash of the node name concatenated with
/// the canonical (sorted-key) JSON serialization of the input state.
///
/// # Arguments
///
/// * `node_name` — the name of the graph node
/// * `input_state` — the state map provided as input to the node
///
/// # Example
///
/// ```rust,ignore
/// use adk_graph::cache::compute_cache_key;
/// use serde_json::json;
/// use std::collections::HashMap;
///
/// let mut state = HashMap::new();
/// state.insert("x".to_string(), json!(1));
/// state.insert("y".to_string(), json!(2));
///
/// let key = compute_cache_key("add", &state);
/// // key is a 64-character hex string (blake3 digest)
/// assert_eq!(key.len(), 64);
///
/// // Same inputs always produce the same key
/// let key2 = compute_cache_key("add", &state);
/// assert_eq!(key, key2);
/// ```
pub fn compute_cache_key(node_name: &str, input_state: &State) -> String {
    // Sort keys for deterministic serialization
    let mut sorted_keys: Vec<&String> = input_state.keys().collect();
    sorted_keys.sort();

    let mut canonical = serde_json::Map::new();
    for key in sorted_keys {
        if let Some(value) = input_state.get(key) {
            canonical.insert(key.clone(), value.clone());
        }
    }

    let state_json = serde_json::to_string(&canonical).unwrap_or_default();
    let input = format!("{node_name}{state_json}");
    let hash = blake3::hash(input.as_bytes());
    hash.to_hex().to_string()
}

/// A simple LRU (Least Recently Used) cache.
///
/// Uses a `HashMap` for O(1) lookups and a `VecDeque` to track access order.
/// When the cache exceeds `max_entries`, the least recently used entry is evicted.
#[derive(Debug)]
struct LruCache {
    map: HashMap<String, (Value, Instant)>,
    order: VecDeque<String>,
    max_entries: usize,
}

impl LruCache {
    fn new(max_entries: usize) -> Self {
        Self {
            map: HashMap::with_capacity(max_entries),
            order: VecDeque::with_capacity(max_entries),
            max_entries,
        }
    }

    fn get(&mut self, key: &str, ttl: Option<Duration>) -> Option<Value> {
        if let Some((value, inserted_at)) = self.map.get(key) {
            // Check TTL expiration
            if let Some(ttl) = ttl {
                if inserted_at.elapsed() > ttl {
                    // Entry expired — remove it
                    self.map.remove(key);
                    self.order.retain(|k| k != key);
                    return None;
                }
            }

            let value = value.clone();

            // Move to back (most recently used)
            self.order.retain(|k| k != key);
            self.order.push_back(key.to_string());

            Some(value)
        } else {
            None
        }
    }

    fn insert(&mut self, key: String, value: Value) {
        if self.map.contains_key(&key) {
            // Update existing entry
            self.map.insert(key.clone(), (value, Instant::now()));
            self.order.retain(|k| k != &key);
            self.order.push_back(key);
        } else {
            // Evict if at capacity
            if self.map.len() >= self.max_entries {
                if let Some(evicted) = self.order.pop_front() {
                    self.map.remove(&evicted);
                }
            }
            self.order.push_back(key.clone());
            self.map.insert(key, (value, Instant::now()));
        }
    }
}

/// Node cache that stores and retrieves execution results.
///
/// Supports in-memory LRU caching and optionally Redis-backed caching
/// (behind the `redis-cache` feature flag).
///
/// # Example
///
/// ```rust,ignore
/// use adk_graph::cache::{CacheBackend, NodeCache, NodeCachePolicy};
/// use serde_json::json;
/// use std::time::Duration;
///
/// let policy = NodeCachePolicy {
///     backend: CacheBackend::InMemory { max_entries: 100 },
///     ttl: Some(Duration::from_secs(60)),
/// };
///
/// let cache = NodeCache::from_policy(&policy);
///
/// # tokio_test::block_on(async {
/// // Cache miss
/// assert!(cache.get("key1").await.is_none());
///
/// // Store a value
/// cache.set("key1", json!({"answer": 42}), Some(Duration::from_secs(60))).await;
///
/// // Cache hit
/// assert_eq!(cache.get("key1").await, Some(json!({"answer": 42})));
/// # });
/// ```
pub struct NodeCache {
    memory: Option<MemoryStore>,
    #[cfg(feature = "redis-cache")]
    redis: Option<fred::clients::Client>,
}

impl NodeCache {
    /// Creates a new `NodeCache` from a [`NodeCachePolicy`].
    ///
    /// For the `InMemory` backend, this initializes an LRU cache with the
    /// configured maximum entries. For the `Redis` backend, this creates a
    /// Redis client (connection is established lazily on first use).
    ///
    /// # Arguments
    ///
    /// * `policy` — the cache policy specifying backend and TTL
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_graph::cache::{CacheBackend, NodeCache, NodeCachePolicy};
    /// use std::time::Duration;
    ///
    /// let policy = NodeCachePolicy {
    ///     backend: CacheBackend::InMemory { max_entries: 256 },
    ///     ttl: Some(Duration::from_secs(120)),
    /// };
    /// let cache = NodeCache::from_policy(&policy);
    /// ```
    pub fn from_policy(policy: &NodeCachePolicy) -> Self {
        match &policy.backend {
            CacheBackend::InMemory { max_entries } => Self {
                memory: Some(Arc::new(Mutex::new((LruCache::new(*max_entries), policy.ttl)))),
                #[cfg(feature = "redis-cache")]
                redis: None,
            },
            #[cfg(feature = "redis-cache")]
            CacheBackend::Redis { url: _url } => Self {
                memory: None,
                redis: {
                    // Create a Redis client config from the URL.
                    // Connection is established on first command.
                    let config = fred::prelude::Config::from_url(_url).unwrap_or_default();
                    Some(fred::clients::Client::new(config, None, None, None))
                },
            },
        }
    }

    /// Retrieves a cached value by key.
    ///
    /// Returns `Some(value)` on a cache hit, or `None` on a miss (including
    /// expired entries).
    ///
    /// # Arguments
    ///
    /// * `key` — the cache key (typically produced by [`compute_cache_key`])
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// # tokio_test::block_on(async {
    /// let value = cache.get("some_key").await;
    /// if let Some(cached) = value {
    ///     println!("Cache hit: {cached}");
    /// }
    /// # });
    /// ```
    pub async fn get(&self, key: &str) -> Option<Value> {
        if let Some(memory) = &self.memory {
            let mut guard = memory.lock().await;
            let (lru, ttl) = &mut *guard;
            return lru.get(key, *ttl);
        }

        #[cfg(feature = "redis-cache")]
        if let Some(redis) = &self.redis {
            use fred::interfaces::KeysInterface;
            let result: Option<String> = redis.get(key).await.ok()?;
            return result.and_then(|s| serde_json::from_str(&s).ok());
        }

        None
    }

    /// Stores a value in the cache.
    ///
    /// For the in-memory backend, the value is stored with the current
    /// timestamp for TTL tracking. For the Redis backend, the TTL is applied
    /// as a Redis key expiration.
    ///
    /// # Arguments
    ///
    /// * `key` — the cache key
    /// * `value` — the JSON value to cache
    /// * `ttl` — optional time-to-live for this entry
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use serde_json::json;
    /// use std::time::Duration;
    ///
    /// # tokio_test::block_on(async {
    /// cache.set("key", json!({"result": "ok"}), Some(Duration::from_secs(300))).await;
    /// # });
    /// ```
    pub async fn set(&self, key: &str, value: Value, _ttl: Option<Duration>) {
        if let Some(memory) = &self.memory {
            let mut guard = memory.lock().await;
            let (lru, _) = &mut *guard;
            lru.insert(key.to_string(), value);
        } else {
            #[cfg(feature = "redis-cache")]
            if let Some(redis) = &self.redis {
                use fred::interfaces::KeysInterface;
                let serialized = serde_json::to_string(&value).unwrap_or_default();
                let expiration = _ttl.map(|d| fred::types::Expiration::EX(d.as_secs() as i64));
                let _: std::result::Result<(), _> =
                    redis.set(key, serialized, expiration, None, false).await;
            }
        }
    }
}

impl std::fmt::Debug for NodeCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeCache").field("has_memory", &self.memory.is_some()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_compute_cache_key_deterministic() {
        let mut state = State::new();
        state.insert("a".to_string(), json!(1));
        state.insert("b".to_string(), json!("hello"));

        let key1 = compute_cache_key("node1", &state);
        let key2 = compute_cache_key("node1", &state);
        assert_eq!(key1, key2);
        assert_eq!(key1.len(), 64); // blake3 hex digest
    }

    #[test]
    fn test_compute_cache_key_different_nodes() {
        let state = State::new();
        let key1 = compute_cache_key("node_a", &state);
        let key2 = compute_cache_key("node_b", &state);
        assert_ne!(key1, key2);
    }

    #[test]
    fn test_compute_cache_key_different_state() {
        let mut state1 = State::new();
        state1.insert("x".to_string(), json!(1));

        let mut state2 = State::new();
        state2.insert("x".to_string(), json!(2));

        let key1 = compute_cache_key("node", &state1);
        let key2 = compute_cache_key("node", &state2);
        assert_ne!(key1, key2);
    }

    #[test]
    fn test_compute_cache_key_order_independent() {
        let mut state1 = State::new();
        state1.insert("a".to_string(), json!(1));
        state1.insert("b".to_string(), json!(2));

        let mut state2 = State::new();
        state2.insert("b".to_string(), json!(2));
        state2.insert("a".to_string(), json!(1));

        let key1 = compute_cache_key("node", &state1);
        let key2 = compute_cache_key("node", &state2);
        assert_eq!(key1, key2);
    }

    #[tokio::test]
    async fn test_node_cache_in_memory_basic() {
        let policy =
            NodeCachePolicy { backend: CacheBackend::InMemory { max_entries: 10 }, ttl: None };
        let cache = NodeCache::from_policy(&policy);

        // Miss
        assert!(cache.get("key1").await.is_none());

        // Set and hit
        cache.set("key1", json!({"result": 42}), None).await;
        assert_eq!(cache.get("key1").await, Some(json!({"result": 42})));
    }

    #[tokio::test]
    async fn test_node_cache_lru_eviction() {
        let policy =
            NodeCachePolicy { backend: CacheBackend::InMemory { max_entries: 3 }, ttl: None };
        let cache = NodeCache::from_policy(&policy);

        cache.set("a", json!(1), None).await;
        cache.set("b", json!(2), None).await;
        cache.set("c", json!(3), None).await;

        // Access "b" and "c" to make "a" the LRU
        assert_eq!(cache.get("b").await, Some(json!(2)));
        assert_eq!(cache.get("c").await, Some(json!(3)));

        // Order is now: a, b, c (a is LRU)
        // Adding "d" should evict "a"
        cache.set("d", json!(4), None).await;

        // "a" should be evicted (it was LRU)
        assert!(cache.get("a").await.is_none());
        assert_eq!(cache.get("b").await, Some(json!(2)));
        assert_eq!(cache.get("c").await, Some(json!(3)));
        assert_eq!(cache.get("d").await, Some(json!(4)));
    }

    #[tokio::test]
    async fn test_node_cache_ttl_expiration() {
        let policy = NodeCachePolicy {
            backend: CacheBackend::InMemory { max_entries: 10 },
            ttl: Some(Duration::from_millis(50)),
        };
        let cache = NodeCache::from_policy(&policy);

        cache.set("key", json!("value"), Some(Duration::from_millis(50))).await;
        assert_eq!(cache.get("key").await, Some(json!("value")));

        // Wait for TTL to expire
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(cache.get("key").await.is_none());
    }
}
