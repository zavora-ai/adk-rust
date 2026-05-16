//! Type-safe shared state container for plugins.
//!
//! [`PluginContext`] uses the TypeMap pattern where each type serves as its own key,
//! providing a type-safe, concurrent key-value store for plugin shared state.
//!
//! # Overview
//!
//! Plugins often need to share state across hook invocations within a single agent run.
//! For example, a rate-limiting plugin tracks request counts, a caching plugin stores
//! cached responses, and a metrics plugin accumulates statistics.
//!
//! `PluginContext` enables this by allowing plugins to insert and retrieve values
//! keyed by their Rust type. Each unique type can have exactly one value stored.
//!
//! # Examples
//!
//! ```rust
//! use adk_plugin::PluginContext;
//!
//! #[derive(Clone, Debug, PartialEq)]
//! struct RequestCount(u32);
//!
//! #[derive(Clone, Debug, PartialEq)]
//! struct CacheHits(u64);
//!
//! # #[tokio::main]
//! # async fn main() {
//! let ctx = PluginContext::new();
//!
//! // Insert typed state
//! ctx.insert(RequestCount(0)).await;
//! ctx.insert(CacheHits(42)).await;
//!
//! // Retrieve typed state
//! let count = ctx.get::<RequestCount>().await;
//! assert_eq!(count, Some(RequestCount(0)));
//!
//! // Update state (last write wins)
//! ctx.insert(RequestCount(5)).await;
//! let count = ctx.get::<RequestCount>().await;
//! assert_eq!(count, Some(RequestCount(5)));
//!
//! // Remove state
//! let removed = ctx.remove::<CacheHits>().await;
//! assert_eq!(removed, Some(CacheHits(42)));
//! assert_eq!(ctx.get::<CacheHits>().await, None);
//! # }
//! ```

use std::any::{Any, TypeId};
use std::collections::HashMap;

use tokio::sync::RwLock;

/// A type-safe, concurrent key-value store for plugin shared state.
///
/// Uses the TypeMap pattern where each type serves as its own key.
/// Thread-safe via [`tokio::sync::RwLock`] for concurrent async access.
///
/// # Concurrency
///
/// - Multiple readers can access state concurrently via [`get`](Self::get) and
///   [`contains`](Self::contains).
/// - Writers acquire exclusive access via [`insert`](Self::insert) and
///   [`remove`](Self::remove).
/// - No locks are held across await points — each method acquires and releases
///   the lock within a single operation.
///
/// # Examples
///
/// ```rust
/// use adk_plugin::PluginContext;
///
/// #[derive(Clone, Debug)]
/// struct RateLimitState {
///     requests_this_minute: u32,
/// }
///
/// # #[tokio::main]
/// # async fn main() {
/// let ctx = PluginContext::new();
///
/// // A rate-limiting plugin writes state
/// ctx.insert(RateLimitState { requests_this_minute: 1 }).await;
///
/// // A metrics plugin reads it
/// if let Some(state) = ctx.get::<RateLimitState>().await {
///     println!("Requests: {}", state.requests_this_minute);
/// }
/// # }
/// ```
pub struct PluginContext {
    state: RwLock<HashMap<TypeId, Box<dyn Any + Send + Sync>>>,
}

impl PluginContext {
    /// Creates a new empty `PluginContext`.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use adk_plugin::PluginContext;
    ///
    /// let ctx = PluginContext::new();
    /// ```
    pub fn new() -> Self {
        Self {
            state: RwLock::new(HashMap::new()),
        }
    }

    /// Inserts a value into the context. The type itself is the key.
    ///
    /// If a value of the same type already exists, it is replaced.
    /// The previous value is discarded.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use adk_plugin::PluginContext;
    ///
    /// #[derive(Clone, Debug, PartialEq)]
    /// struct Counter(u32);
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let ctx = PluginContext::new();
    /// ctx.insert(Counter(1)).await;
    /// ctx.insert(Counter(2)).await; // Replaces the previous value
    ///
    /// assert_eq!(ctx.get::<Counter>().await, Some(Counter(2)));
    /// # }
    /// ```
    pub async fn insert<T: Send + Sync + 'static>(&self, value: T) {
        self.state
            .write()
            .await
            .insert(TypeId::of::<T>(), Box::new(value));
    }

    /// Gets a clone of the stored value for type `T`.
    ///
    /// Returns `None` if no value of type `T` has been inserted.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use adk_plugin::PluginContext;
    ///
    /// #[derive(Clone, Debug, PartialEq)]
    /// struct Name(String);
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let ctx = PluginContext::new();
    ///
    /// assert_eq!(ctx.get::<Name>().await, None);
    ///
    /// ctx.insert(Name("alice".to_string())).await;
    /// assert_eq!(ctx.get::<Name>().await, Some(Name("alice".to_string())));
    /// # }
    /// ```
    pub async fn get<T: Clone + Send + Sync + 'static>(&self) -> Option<T> {
        self.state
            .read()
            .await
            .get(&TypeId::of::<T>())
            .and_then(|v| v.downcast_ref::<T>())
            .cloned()
    }

    /// Checks if a value of type `T` exists in the context.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use adk_plugin::PluginContext;
    ///
    /// #[derive(Clone, Debug)]
    /// struct Marker;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let ctx = PluginContext::new();
    ///
    /// assert!(!ctx.contains::<Marker>().await);
    /// ctx.insert(Marker).await;
    /// assert!(ctx.contains::<Marker>().await);
    /// # }
    /// ```
    pub async fn contains<T: Send + Sync + 'static>(&self) -> bool {
        self.state.read().await.contains_key(&TypeId::of::<T>())
    }

    /// Removes a value of type `T`, returning it if present.
    ///
    /// After removal, [`get`](Self::get) and [`contains`](Self::contains) for
    /// type `T` will return `None` and `false` respectively.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use adk_plugin::PluginContext;
    ///
    /// #[derive(Clone, Debug, PartialEq)]
    /// struct Token(String);
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let ctx = PluginContext::new();
    /// ctx.insert(Token("abc".to_string())).await;
    ///
    /// let removed = ctx.remove::<Token>().await;
    /// assert_eq!(removed, Some(Token("abc".to_string())));
    /// assert_eq!(ctx.get::<Token>().await, None);
    /// # }
    /// ```
    pub async fn remove<T: Send + Sync + 'static>(&self) -> Option<T> {
        self.state
            .write()
            .await
            .remove(&TypeId::of::<T>())
            .and_then(|v| v.downcast::<T>().ok())
            .map(|b| *b)
    }
}

impl Default for PluginContext {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct Counter(u32);

    #[derive(Clone, Debug, PartialEq)]
    struct Name(String);

    #[tokio::test]
    async fn test_new_context_is_empty() {
        let ctx = PluginContext::new();
        assert!(!ctx.contains::<Counter>().await);
        assert_eq!(ctx.get::<Counter>().await, None);
    }

    #[tokio::test]
    async fn test_insert_and_get() {
        let ctx = PluginContext::new();
        ctx.insert(Counter(42)).await;

        let value = ctx.get::<Counter>().await;
        assert_eq!(value, Some(Counter(42)));
    }

    #[tokio::test]
    async fn test_insert_overwrites_previous() {
        let ctx = PluginContext::new();
        ctx.insert(Counter(1)).await;
        ctx.insert(Counter(99)).await;

        assert_eq!(ctx.get::<Counter>().await, Some(Counter(99)));
    }

    #[tokio::test]
    async fn test_multiple_types() {
        let ctx = PluginContext::new();
        ctx.insert(Counter(10)).await;
        ctx.insert(Name("hello".to_string())).await;

        assert_eq!(ctx.get::<Counter>().await, Some(Counter(10)));
        assert_eq!(ctx.get::<Name>().await, Some(Name("hello".to_string())));
    }

    #[tokio::test]
    async fn test_contains() {
        let ctx = PluginContext::new();
        assert!(!ctx.contains::<Counter>().await);

        ctx.insert(Counter(0)).await;
        assert!(ctx.contains::<Counter>().await);
    }

    #[tokio::test]
    async fn test_remove_returns_value() {
        let ctx = PluginContext::new();
        ctx.insert(Counter(7)).await;

        let removed = ctx.remove::<Counter>().await;
        assert_eq!(removed, Some(Counter(7)));
    }

    #[tokio::test]
    async fn test_remove_makes_get_return_none() {
        let ctx = PluginContext::new();
        ctx.insert(Counter(7)).await;
        ctx.remove::<Counter>().await;

        assert_eq!(ctx.get::<Counter>().await, None);
        assert!(!ctx.contains::<Counter>().await);
    }

    #[tokio::test]
    async fn test_remove_nonexistent_returns_none() {
        let ctx = PluginContext::new();
        let removed = ctx.remove::<Counter>().await;
        assert_eq!(removed, None);
    }

    #[tokio::test]
    async fn test_default_creates_empty_context() {
        let ctx = PluginContext::default();
        assert!(!ctx.contains::<Counter>().await);
    }

    #[tokio::test]
    async fn test_concurrent_access() {
        use std::sync::Arc;

        let ctx = Arc::new(PluginContext::new());
        ctx.insert(Counter(0)).await;

        let ctx_clone = Arc::clone(&ctx);
        let writer = tokio::spawn(async move {
            for i in 1..=100 {
                ctx_clone.insert(Counter(i)).await;
            }
        });

        let ctx_clone2 = Arc::clone(&ctx);
        let reader = tokio::spawn(async move {
            for _ in 0..100 {
                // Should never panic — reads are always valid
                let _ = ctx_clone2.get::<Counter>().await;
            }
        });

        writer.await.unwrap();
        reader.await.unwrap();

        // Final value should be 100 (last write)
        assert_eq!(ctx.get::<Counter>().await, Some(Counter(100)));
    }
}
