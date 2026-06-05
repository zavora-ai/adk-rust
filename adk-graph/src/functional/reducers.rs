//! Typed state reducers for the Functional API.
//!
//! This module provides state containers with different persistence semantics:
//!
//! - [`ReducedValue<T>`]: Append-only state container persisted across checkpoints.
//! - [`UntrackedValue<T>`]: Transient runtime value excluded from checkpoint serialization.
//!   Resets to `T::default()` on workflow resume.

use std::ops::Deref;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

// ---------------------------------------------------------------------------
// ReducedValue<T>
// ---------------------------------------------------------------------------

/// Append-only state container. Accumulates values across tasks.
/// Persisted to checkpoints on each task completion.
///
/// # Example
///
/// ```rust,ignore
/// use adk_graph::functional::ReducedValue;
///
/// let mut rv = ReducedValue::new();
/// rv.push(1);
/// rv.push(2);
/// assert_eq!(rv.len(), 2);
/// assert_eq!(&*rv, &[1, 2]);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound = "T: Serialize + DeserializeOwned")]
pub struct ReducedValue<T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync,
{
    items: Vec<T>,
}

impl<T> ReducedValue<T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync,
{
    /// Create an empty `ReducedValue`.
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    /// Append a value to the collection.
    pub fn push(&mut self, value: T) {
        self.items.push(value);
    }

    /// Extend with multiple values.
    pub fn extend(&mut self, values: impl IntoIterator<Item = T>) {
        self.items.extend(values);
    }

    /// Get the number of accumulated items.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

impl<T> Deref for ReducedValue<T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync,
{
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.items
    }
}

impl<T> Default for ReducedValue<T>
where
    T: Serialize + DeserializeOwned + Clone + Send + Sync,
{
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// UntrackedValue<T>
// ---------------------------------------------------------------------------

/// Transient runtime value excluded from checkpoints.
///
/// `UntrackedValue<T>` stores a value that is intentionally omitted from checkpoint
/// persistence. When a workflow resumes from a checkpoint, any `UntrackedValue` field
/// will be initialized to `T::default()` regardless of what value it held before
/// the checkpoint was created.
///
/// This is useful for caching intermediate computation results, holding open file
/// handles or connections, or any runtime-only state that should not inflate
/// checkpoint size.
///
/// # Custom Serialization
///
/// `UntrackedValue` implements `Serialize` by always serializing as `null`, and
/// `Deserialize` by always producing `T::default()`. This ensures checkpoint
/// round-trips correctly reset the value.
///
/// # Example
///
/// ```rust,ignore
/// use adk_graph::functional::UntrackedValue;
///
/// let mut cache: UntrackedValue<Vec<String>> = UntrackedValue::new();
/// cache.set(vec!["cached_result".to_string()]);
/// assert_eq!(cache.get().len(), 1);
///
/// // After checkpoint round-trip, value resets to default
/// let serialized = serde_json::to_string(&cache).unwrap();
/// let restored: UntrackedValue<Vec<String>> = serde_json::from_str(&serialized).unwrap();
/// assert!(restored.get().is_empty());
/// ```
#[derive(Debug, Clone)]
pub struct UntrackedValue<T: Default + Send + Sync> {
    value: T,
}

impl<T: Default + Send + Sync> UntrackedValue<T> {
    /// Create a new `UntrackedValue` initialized to `T::default()`.
    pub fn new() -> Self {
        Self { value: T::default() }
    }

    /// Create a new `UntrackedValue` with an initial value.
    pub fn with_value(value: T) -> Self {
        Self { value }
    }

    /// Get a reference to the contained value.
    pub fn get(&self) -> &T {
        &self.value
    }

    /// Set the contained value.
    pub fn set(&mut self, value: T) {
        self.value = value;
    }

    /// Take the value, replacing it with `T::default()`.
    pub fn take(&mut self) -> T {
        std::mem::take(&mut self.value)
    }
}

impl<T: Default + Send + Sync> Default for UntrackedValue<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Custom serialization: always serializes as `null` to exclude from checkpoints.
impl<T: Default + Send + Sync> Serialize for UntrackedValue<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_none()
    }
}

/// Custom deserialization: always produces `T::default()` regardless of input.
impl<'de, T: Default + Send + Sync> Deserialize<'de> for UntrackedValue<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Consume and discard whatever value is in the serialized data
        let _ = serde::de::IgnoredAny::deserialize(deserializer)?;
        Ok(Self::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // ReducedValue tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_reduced_value_new_is_empty() {
        let rv: ReducedValue<i32> = ReducedValue::new();
        assert!(rv.is_empty());
        assert_eq!(rv.len(), 0);
    }

    #[test]
    fn test_reduced_value_push_and_deref() {
        let mut rv = ReducedValue::new();
        rv.push(1);
        rv.push(2);
        rv.push(3);
        assert_eq!(rv.len(), 3);
        assert_eq!(&*rv, &[1, 2, 3]);
    }

    #[test]
    fn test_reduced_value_extend() {
        let mut rv = ReducedValue::new();
        rv.extend(vec![10, 20, 30]);
        assert_eq!(rv.len(), 3);
        assert_eq!(&*rv, &[10, 20, 30]);
    }

    #[test]
    fn test_reduced_value_serde_round_trip() {
        let mut rv = ReducedValue::new();
        rv.push("hello".to_string());
        rv.push("world".to_string());
        let json = serde_json::to_string(&rv).unwrap();
        let restored: ReducedValue<String> = serde_json::from_str(&json).unwrap();
        assert_eq!(&*restored, &*rv);
    }

    // -----------------------------------------------------------------------
    // UntrackedValue tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_new_creates_default_value() {
        let uv: UntrackedValue<i32> = UntrackedValue::new();
        assert_eq!(*uv.get(), 0);
    }

    #[test]
    fn test_with_value_stores_value() {
        let uv = UntrackedValue::with_value(42);
        assert_eq!(*uv.get(), 42);
    }

    #[test]
    fn test_set_and_get() {
        let mut uv: UntrackedValue<String> = UntrackedValue::new();
        assert_eq!(uv.get(), "");
        uv.set("hello".to_string());
        assert_eq!(uv.get(), "hello");
    }

    #[test]
    fn test_take_returns_value_and_resets() {
        let mut uv = UntrackedValue::with_value(vec![1, 2, 3]);
        let taken = uv.take();
        assert_eq!(taken, vec![1, 2, 3]);
        assert!(uv.get().is_empty());
    }

    #[test]
    fn test_default_trait() {
        let uv: UntrackedValue<bool> = UntrackedValue::default();
        assert!(!*uv.get());
    }

    #[test]
    fn test_serialize_produces_null() {
        let uv = UntrackedValue::with_value(999);
        let serialized = serde_json::to_string(&uv).unwrap();
        assert_eq!(serialized, "null");
    }

    #[test]
    fn test_deserialize_always_produces_default() {
        // Deserializing from null
        let restored: UntrackedValue<i32> = serde_json::from_str("null").unwrap();
        assert_eq!(*restored.get(), 0);

        // Deserializing from an arbitrary value
        let restored: UntrackedValue<i32> = serde_json::from_str("42").unwrap();
        assert_eq!(*restored.get(), 0);

        // Deserializing from an array
        let restored: UntrackedValue<Vec<i32>> = serde_json::from_str("[1,2,3]").unwrap();
        assert!(restored.get().is_empty());
    }

    #[test]
    fn test_round_trip_resets_value() {
        let uv = UntrackedValue::with_value("important data".to_string());
        let serialized = serde_json::to_string(&uv).unwrap();
        let restored: UntrackedValue<String> = serde_json::from_str(&serialized).unwrap();
        assert_eq!(restored.get(), "");
    }
}
