# Node Caching Example

Demonstrates ADK-Rust's blake3-keyed LRU caching of graph node results, showing how to avoid redundant computations in graph workflows by caching node outputs based on input state hashing.

## What This Shows

- **`NodeCachePolicy`** — configuring a cache policy with TTL and an in-memory LRU backend (`CacheBackend::InMemory`) to control how node results are stored and evicted
- **blake3 hashing** — computing deterministic cache keys from node name + input state using the blake3 cryptographic hash, ensuring identical inputs always map to the same cache entry
- **Cache hit/miss** — executing the same node with identical input returns the cached result instantly (hit), while different input triggers a full re-execution (miss)
- **TTL expiration** — cached entries automatically expire after the configured time-to-live, forcing re-execution to prevent stale results
- **LRU eviction** — when the cache reaches `max_entries`, the least recently used entry is evicted to keep memory bounded

## Prerequisites

- **Rust 1.95+** (edition 2024)
- **`GOOGLE_API_KEY`** environment variable set with a valid Gemini API key

Set up your environment:

```bash
cp examples/node_caching/.env.example examples/node_caching/.env
# Edit .env and add your GOOGLE_API_KEY
```

## Run

```bash
cargo run --manifest-path examples/node_caching/Cargo.toml
```

To enable debug logging:

```bash
RUST_LOG=debug cargo run --manifest-path examples/node_caching/Cargo.toml
```

## Expected Output

```
╔════════════════════════════════════════════╗
║  Node Caching — ADK-Rust v1.0           ║
╚════════════════════════════════════════════╝

  ✓ GOOGLE_API_KEY loaded (39 chars)

--- Step 1: Configure Node Cache Policy ---

  ✓ NodeCachePolicy created:
      backend: InMemory { max_entries: 64 }
      ttl: 5s (short for demonstration)

--- Step 2: Execute Node — Cache Miss (First Run) ---

  → Cache key (blake3): a7f3b2c1e9d04f8a
  → Looking up cache...
  → Cache MISS — executing node...
  ✓ Result: Analysis of 'rust concurrency': This is a comprehensive analysis result.
  ✓ Elapsed: 0.502s (full execution)

--- Step 3: Execute Node — Cache Hit (Same Input) ---

  → Cache key (blake3): a7f3b2c1e9d04f8a
  → Looking up cache...
  ✓ Cache HIT — returning cached result
  ✓ Result: Analysis of 'rust concurrency': This is a comprehensive analysis result.
  ✓ Elapsed: 0.000001s (instant from cache!)
  ✓ Speedup: 502000x faster than first execution

--- Step 4: Execute Node — Cache Miss (Different Input) ---

  → Cache key (blake3): 5e8c1d9a3b7f2046
  → Different input → different cache key
  → Looking up cache...
  → Cache MISS — executing node...
  ✓ Result: Analysis of 'async runtime design': This is a comprehensive analysis result.
  ✓ Elapsed: 0.501s (full execution, new input)

--- Step 5: Demonstrate TTL Expiration ---

  → Waiting 6 seconds for TTL (5s) to expire...
  ✓ TTL elapsed. Re-executing with original input...
  → Cache key (blake3): a7f3b2c1e9d04f8a
  → Same key as Step 2, but entry should be expired
  → Cache MISS (TTL expired) — re-executing node...
  ✓ Result: Analysis of 'rust concurrency': This is a comprehensive analysis result.
  ✓ Elapsed: 0.500s (full execution after TTL expiry)

--- Summary ---

  Cache hits: 1
  Cache misses: 3
  First execution: 0.502s (cache miss, full LLM call)
  Cached execution: 0.000001s (cache hit, instant)
  Different input: 0.501s (cache miss, new key)
  After TTL expiry: 0.500s (cache miss, expired entry)
  blake3 hashing ensures deterministic cache keys from input state.
  TTL prevents stale results from persisting indefinitely.
  LRU eviction keeps memory bounded at max_entries.

✅ Example completed successfully.
```

## Key APIs Demonstrated

### NodeCachePolicy

Configure the caching behavior for graph nodes, specifying the storage backend and expiration policy:

```rust
use std::time::Duration;

struct NodeCachePolicy {
    /// The storage backend for cached results.
    backend: CacheBackend,
    /// Time-to-live for cached entries. `None` means entries never expire.
    ttl: Option<Duration>,
}

let policy = NodeCachePolicy {
    backend: CacheBackend::InMemory { max_entries: 64 },
    ttl: Some(Duration::from_secs(5)),
};

let cache = NodeCache::new(&policy);
```

### compute_cache_key

Compute a deterministic cache key from the node name and input state using blake3 hashing. Identical inputs always produce the same key, while any change in node name or state values produces a different key:

```rust
use std::collections::HashMap;

fn compute_cache_key(node_name: &str, input_state: &HashMap<String, String>) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(node_name.as_bytes());
    hasher.update(b":");

    // Sort keys for deterministic hashing
    let mut keys: Vec<&String> = input_state.keys().collect();
    keys.sort();
    for key in keys {
        hasher.update(key.as_bytes());
        hasher.update(b"=");
        hasher.update(input_state[key].as_bytes());
        hasher.update(b";");
    }

    hasher.finalize().to_hex().to_string()
}

// Usage
let input_state = HashMap::from([
    ("topic".to_string(), "rust concurrency".to_string()),
    ("depth".to_string(), "detailed".to_string()),
]);

let key = compute_cache_key("analysis_node", &input_state);
// key: "a7f3b2c1e9d04f8a..." (64-char hex blake3 hash)
```

### CacheBackend

Define where cached results are stored. The in-memory LRU backend evicts the least recently used entry when `max_entries` is reached:

```rust
enum CacheBackend {
    /// In-memory LRU cache with a maximum number of entries.
    /// When full, the least recently used entry is evicted.
    InMemory { max_entries: usize },
}

// 64-entry LRU cache — suitable for most graph workflows
let backend = CacheBackend::InMemory { max_entries: 64 };

// Small cache for testing eviction behavior
let small_backend = CacheBackend::InMemory { max_entries: 4 };
```

### Cache Lookup Pattern

The typical usage pattern checks the cache before executing expensive work:

```rust
let cache_key = compute_cache_key("analysis_node", &input_state);

let result = match cache.get(&cache_key) {
    Some(cached) => {
        // Cache HIT — return instantly
        cached.to_string()
    }
    None => {
        // Cache MISS — execute the expensive operation
        let result = expensive_analysis_node(&input).await;
        cache.put(cache_key, result.clone());
        result
    }
};
```
