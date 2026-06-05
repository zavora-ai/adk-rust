//! Typed reducer trait and built-in reducer implementations.
//!
//! This module provides the [`TypedReducer`] trait for defining custom merge
//! strategies on typed state values, along with built-in implementations:
//!
//! - [`ReplaceReducer`]: Last-write-wins (returns incoming value)
//! - [`AppendReducer`]: Vec concatenation (extends current with incoming)
//! - [`MergeReducer`]: Deep merge for JSON-like structures
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_graph::functional::{TypedReducer, ReplaceReducer, AppendReducer, MergeReducer};
//!
//! // ReplaceReducer always returns the incoming value
//! let reducer = ReplaceReducer;
//! let result: i32 = reducer.reduce(10, 20);
//! assert_eq!(result, 20);
//!
//! // AppendReducer concatenates two Vecs
//! let reducer = AppendReducer;
//! let result = reducer.reduce(vec![1, 2], vec![3, 4]);
//! assert_eq!(result, vec![1, 2, 3, 4]);
//!
//! // MergeReducer deep merges JSON objects
//! let reducer = MergeReducer;
//! let current = serde_json::json!({"a": 1, "b": {"x": 10}});
//! let incoming = serde_json::json!({"b": {"y": 20}, "c": 3});
//! let result = reducer.reduce(current, incoming);
//! // result == {"a": 1, "b": {"x": 10, "y": 20}, "c": 3}
//! ```

use std::marker::PhantomData;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Trait for defining custom merge strategies for state values.
///
/// Implementations define how two values of the same type should be combined
/// when parallel tasks produce outputs for the same state key, or when
/// state updates need to be merged.
///
/// # Type Bounds
///
/// The associated `Value` type must be serializable, deserializable, and
/// thread-safe (`Send + Sync`).
pub trait TypedReducer: Send + Sync {
    /// The value type this reducer operates on.
    type Value: Serialize + DeserializeOwned + Send + Sync;

    /// Merge two values. Called when parallel tasks produce outputs
    /// for the same state key.
    ///
    /// # Arguments
    ///
    /// * `current` - The existing value in state
    /// * `incoming` - The new value to merge in
    ///
    /// # Returns
    ///
    /// The merged result of the two values.
    fn reduce(&self, current: Self::Value, incoming: Self::Value) -> Self::Value;
}

/// Built-in reducer: replace with incoming value (last-write-wins).
///
/// For any two values, `ReplaceReducer` always returns the incoming value,
/// discarding the current value. This is the simplest merge strategy and
/// is useful when only the latest value matters.
///
/// # Example
///
/// ```rust,ignore
/// use adk_graph::functional::{TypedReducer, ReplaceReducer};
///
/// let reducer = ReplaceReducer::<String>::new();
/// let result = reducer.reduce("old".to_string(), "new".to_string());
/// assert_eq!(result, "new");
/// ```
pub struct ReplaceReducer<T> {
    _marker: PhantomData<T>,
}

impl<T> ReplaceReducer<T> {
    /// Create a new `ReplaceReducer`.
    pub fn new() -> Self {
        Self { _marker: PhantomData }
    }
}

impl<T> Default for ReplaceReducer<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> TypedReducer for ReplaceReducer<T>
where
    T: Serialize + DeserializeOwned + Send + Sync,
{
    type Value = T;

    fn reduce(&self, _current: T, incoming: T) -> T {
        incoming
    }
}

// SAFETY: ReplaceReducer holds no data, just a PhantomData marker.
unsafe impl<T> Send for ReplaceReducer<T> {}
unsafe impl<T> Sync for ReplaceReducer<T> {}

/// Built-in reducer: append to a Vec (list concatenation).
///
/// For two `Vec<T>` values, `AppendReducer` extends the current vector
/// with all elements from the incoming vector. The result preserves
/// insertion order: current items first, then incoming items.
///
/// # Example
///
/// ```rust,ignore
/// use adk_graph::functional::{TypedReducer, AppendReducer};
///
/// let reducer = AppendReducer::<i32>::new();
/// let result = reducer.reduce(vec![1, 2, 3], vec![4, 5]);
/// assert_eq!(result, vec![1, 2, 3, 4, 5]);
/// ```
pub struct AppendReducer<T> {
    _marker: PhantomData<T>,
}

impl<T> AppendReducer<T> {
    /// Create a new `AppendReducer`.
    pub fn new() -> Self {
        Self { _marker: PhantomData }
    }
}

impl<T> Default for AppendReducer<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> TypedReducer for AppendReducer<T>
where
    T: Serialize + DeserializeOwned + Send + Sync,
{
    type Value = Vec<T>;

    fn reduce(&self, mut current: Vec<T>, incoming: Vec<T>) -> Vec<T> {
        current.extend(incoming);
        current
    }
}

// SAFETY: AppendReducer holds no data, just a PhantomData marker.
unsafe impl<T> Send for AppendReducer<T> {}
unsafe impl<T> Sync for AppendReducer<T> {}

/// Built-in reducer: deep merge for JSON-like structures.
///
/// For two `serde_json::Value` objects, `MergeReducer` performs a recursive
/// deep merge:
///
/// - **Objects**: Keys from incoming overwrite keys in current. Keys only in
///   current are preserved. Nested objects are merged recursively.
/// - **Non-objects**: The incoming value replaces the current value entirely.
///
/// # Example
///
/// ```rust,ignore
/// use adk_graph::functional::{TypedReducer, MergeReducer};
/// use serde_json::json;
///
/// let reducer = MergeReducer;
/// let current = json!({"a": 1, "nested": {"x": 10}});
/// let incoming = json!({"b": 2, "nested": {"y": 20}});
/// let result = reducer.reduce(current, incoming);
/// assert_eq!(result, json!({"a": 1, "b": 2, "nested": {"x": 10, "y": 20}}));
/// ```
pub struct MergeReducer;

impl TypedReducer for MergeReducer {
    type Value = Value;

    fn reduce(&self, current: Value, incoming: Value) -> Value {
        deep_merge(current, incoming)
    }
}

/// Recursively deep merge two JSON values.
///
/// When both values are objects, keys are merged recursively. For all other
/// combinations, the incoming value replaces the current value.
fn deep_merge(current: Value, incoming: Value) -> Value {
    match (current, incoming) {
        (Value::Object(mut current_map), Value::Object(incoming_map)) => {
            for (key, incoming_val) in incoming_map {
                let merged = if let Some(current_val) = current_map.remove(&key) {
                    deep_merge(current_val, incoming_val)
                } else {
                    incoming_val
                };
                current_map.insert(key, merged);
            }
            Value::Object(current_map)
        }
        // For non-object types, incoming replaces current
        (_, incoming) => incoming,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_replace_reducer_returns_incoming() {
        let reducer = ReplaceReducer::<i32>::new();
        assert_eq!(reducer.reduce(10, 20), 20);
        assert_eq!(reducer.reduce(0, 42), 42);
    }

    #[test]
    fn test_replace_reducer_with_strings() {
        let reducer = ReplaceReducer::<String>::new();
        let result = reducer.reduce("old".to_string(), "new".to_string());
        assert_eq!(result, "new");
    }

    #[test]
    fn test_append_reducer_concatenates_vecs() {
        let reducer = AppendReducer::<i32>::new();
        let result = reducer.reduce(vec![1, 2, 3], vec![4, 5]);
        assert_eq!(result, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_append_reducer_empty_current() {
        let reducer = AppendReducer::<i32>::new();
        let result = reducer.reduce(vec![], vec![1, 2, 3]);
        assert_eq!(result, vec![1, 2, 3]);
    }

    #[test]
    fn test_append_reducer_empty_incoming() {
        let reducer = AppendReducer::<i32>::new();
        let result = reducer.reduce(vec![1, 2, 3], vec![]);
        assert_eq!(result, vec![1, 2, 3]);
    }

    #[test]
    fn test_append_reducer_both_empty() {
        let reducer = AppendReducer::<i32>::new();
        let result = reducer.reduce(vec![], vec![]);
        assert_eq!(result, Vec::<i32>::new());
    }

    #[test]
    fn test_merge_reducer_objects() {
        let reducer = MergeReducer;
        let current = json!({"a": 1, "b": 2});
        let incoming = json!({"b": 3, "c": 4});
        let result = reducer.reduce(current, incoming);
        assert_eq!(result, json!({"a": 1, "b": 3, "c": 4}));
    }

    #[test]
    fn test_merge_reducer_nested_objects() {
        let reducer = MergeReducer;
        let current = json!({"nested": {"x": 10, "y": 20}});
        let incoming = json!({"nested": {"y": 30, "z": 40}});
        let result = reducer.reduce(current, incoming);
        assert_eq!(result, json!({"nested": {"x": 10, "y": 30, "z": 40}}));
    }

    #[test]
    fn test_merge_reducer_deeply_nested() {
        let reducer = MergeReducer;
        let current = json!({"a": {"b": {"c": 1, "d": 2}}});
        let incoming = json!({"a": {"b": {"d": 3, "e": 4}}});
        let result = reducer.reduce(current, incoming);
        assert_eq!(result, json!({"a": {"b": {"c": 1, "d": 3, "e": 4}}}));
    }

    #[test]
    fn test_merge_reducer_non_object_incoming_replaces() {
        let reducer = MergeReducer;
        let current = json!({"a": 1});
        let incoming = json!(42);
        let result = reducer.reduce(current, incoming);
        assert_eq!(result, json!(42));
    }

    #[test]
    fn test_merge_reducer_non_object_current_replaced() {
        let reducer = MergeReducer;
        let current = json!(42);
        let incoming = json!({"a": 1});
        let result = reducer.reduce(current, incoming);
        assert_eq!(result, json!({"a": 1}));
    }

    #[test]
    fn test_merge_reducer_null_values() {
        let reducer = MergeReducer;
        let current = json!({"a": 1});
        let incoming = json!({"a": null});
        let result = reducer.reduce(current, incoming);
        assert_eq!(result, json!({"a": null}));
    }

    #[test]
    fn test_merge_reducer_array_replaced() {
        let reducer = MergeReducer;
        let current = json!({"items": [1, 2, 3]});
        let incoming = json!({"items": [4, 5]});
        let result = reducer.reduce(current, incoming);
        // Arrays are not merged, incoming replaces current
        assert_eq!(result, json!({"items": [4, 5]}));
    }

    #[test]
    fn test_merge_reducer_empty_objects() {
        let reducer = MergeReducer;
        let current = json!({});
        let incoming = json!({"a": 1});
        let result = reducer.reduce(current, incoming);
        assert_eq!(result, json!({"a": 1}));
    }

    #[test]
    fn test_merge_reducer_both_empty_objects() {
        let reducer = MergeReducer;
        let current = json!({});
        let incoming = json!({});
        let result = reducer.reduce(current, incoming);
        assert_eq!(result, json!({}));
    }
}
