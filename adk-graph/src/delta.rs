//! Delta checkpoint compression for graph state.
//!
//! This module provides incremental (delta-based) checkpoint storage, reducing
//! storage growth from quadratic (full snapshots every step) to linear (only
//! differences between consecutive states are persisted).
//!
//! # Architecture
//!
//! The core abstraction is the [`Diff`] trait, which defines how to compute and
//! apply incremental deltas for a given state type. [`DeltaConfig`] controls how
//! often full snapshots are stored (bounding reconstruction cost), and
//! [`CheckpointType`] distinguishes between full snapshots and delta records.
//!
//! # Example
//!
//! ```rust
//! use adk_graph::delta::{Diff, DeltaConfig, CheckpointType};
//!
//! // DeltaConfig with default full_snapshot_interval of 10
//! let config = DeltaConfig::default();
//! assert_eq!(config.full_snapshot_interval, 10);
//!
//! // CheckpointType distinguishes full vs delta records
//! let full = CheckpointType::Full;
//! let delta = CheckpointType::Delta {
//!     base_checkpoint_id: "ckpt-001".to_string(),
//! };
//! ```

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::checkpoint::Checkpointer;
use crate::error::GraphError;
use crate::state::{Checkpoint, State};

/// Trait for types that can compute and apply incremental diffs.
///
/// Implementors define an associated `Delta` type representing the difference
/// between two instances. The trait guarantees a round-trip property:
///
/// > For any states `s1` and `s2`, `Diff::apply(&s1, &Diff::diff(&s1, &s2)) == s2`.
///
/// # Example
///
/// ```rust,ignore
/// use adk_graph::delta::Diff;
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Clone, Debug, PartialEq)]
/// struct Counter(u64);
///
/// #[derive(Clone, Debug, Serialize, Deserialize)]
/// struct CounterDelta(i64);
///
/// impl Diff for Counter {
///     type Delta = CounterDelta;
///
///     fn diff(old: &Self, new: &Self) -> Self::Delta {
///         CounterDelta(new.0 as i64 - old.0 as i64)
///     }
///
///     fn apply(base: &Self, delta: &Self::Delta) -> Self {
///         Counter((base.0 as i64 + delta.0) as u64)
///     }
/// }
///
/// let s1 = Counter(5);
/// let s2 = Counter(12);
/// let delta = Counter::diff(&s1, &s2);
/// assert_eq!(Counter::apply(&s1, &delta), s2);
/// ```
pub trait Diff: Sized {
    /// The delta representation capturing the difference between two states.
    ///
    /// Must be serializable for checkpoint storage and sendable across async
    /// boundaries.
    type Delta: Clone + Serialize + DeserializeOwned + Send + Sync;

    /// Compute the delta that transforms `old` into `new`.
    ///
    /// The returned delta, when applied to `old` via [`Diff::apply`], must
    /// reproduce `new` exactly.
    fn diff(old: &Self, new: &Self) -> Self::Delta;

    /// Apply a delta to a base state, producing the resulting state.
    ///
    /// This is the inverse of [`Diff::diff`]: applying the delta produced by
    /// `diff(old, new)` to `old` yields `new`.
    fn apply(base: &Self, delta: &Self::Delta) -> Self;
}

/// Configuration for delta-based checkpoint compression.
///
/// Controls how frequently full snapshots are stored among the stream of delta
/// records. A full snapshot is stored every `full_snapshot_interval` steps,
/// bounding the number of deltas that must be replayed during reconstruction.
///
/// # Example
///
/// ```rust
/// use adk_graph::delta::DeltaConfig;
///
/// // Default: full snapshot every 10 steps
/// let config = DeltaConfig::default();
/// assert_eq!(config.full_snapshot_interval, 10);
///
/// // Custom interval
/// let config = DeltaConfig { full_snapshot_interval: 5 };
/// assert_eq!(config.full_snapshot_interval, 5);
/// ```
#[derive(Debug, Clone)]
pub struct DeltaConfig {
    /// Store a full snapshot every N steps.
    ///
    /// At step boundaries divisible by this value, a full state snapshot is
    /// persisted instead of a delta. This bounds reconstruction cost to at most
    /// `full_snapshot_interval - 1` delta applications.
    ///
    /// Default: 10.
    pub full_snapshot_interval: u32,
}

impl Default for DeltaConfig {
    fn default() -> Self {
        Self { full_snapshot_interval: 10 }
    }
}

/// Discriminates between full state snapshots and incremental delta records.
///
/// Each persisted checkpoint is tagged with its type so the loader knows whether
/// to use the payload directly (full) or apply it as a delta on top of a base
/// checkpoint.
///
/// # Example
///
/// ```rust
/// use adk_graph::delta::CheckpointType;
///
/// let full = CheckpointType::Full;
/// assert!(matches!(full, CheckpointType::Full));
///
/// let delta = CheckpointType::Delta {
///     base_checkpoint_id: "ckpt-042".to_string(),
/// };
/// if let CheckpointType::Delta { base_checkpoint_id } = &delta {
///     assert_eq!(base_checkpoint_id, "ckpt-042");
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointType {
    /// A complete state snapshot — no base required for reconstruction.
    Full,
    /// An incremental delta relative to a base checkpoint.
    Delta {
        /// The identifier of the checkpoint this delta is relative to.
        base_checkpoint_id: String,
    },
}

/// Delta representation for `Vec<Value>`.
///
/// Captures either appended items (when the new vec extends the old one) or a
/// full replacement (when items were removed or modified in the middle).
///
/// # Example
///
/// ```rust
/// use adk_graph::delta::{Diff, VecDelta};
/// use serde_json::{Value, json};
///
/// let old = vec![json!(1), json!(2), json!(3)];
/// let new = vec![json!(1), json!(2), json!(3), json!(4), json!(5)];
///
/// let delta = <Vec<Value> as Diff>::diff(&old, &new);
/// assert!(!delta.full_replacement);
/// assert_eq!(delta.start_index, 3);
/// assert_eq!(delta.items, vec![json!(4), json!(5)]);
///
/// let reconstructed = <Vec<Value> as Diff>::apply(&old, &delta);
/// assert_eq!(reconstructed, new);
/// ```
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VecDelta {
    /// If true, the delta is a full replacement (for non-append-only changes).
    pub full_replacement: bool,
    /// Start index for appended items (only used when `full_replacement` is false).
    pub start_index: usize,
    /// The items (appended items or full replacement).
    pub items: Vec<Value>,
}

impl Diff for Vec<Value> {
    type Delta = VecDelta;

    /// Compute the delta between two `Vec<Value>` instances.
    ///
    /// If `new` is a strict extension of `old` (i.e., `old` is a prefix of `new`),
    /// the delta captures only the appended items and the start index. Otherwise,
    /// the delta stores the full new vec as a replacement.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_graph::delta::Diff;
    /// use serde_json::{Value, json};
    ///
    /// // Append case
    /// let old = vec![json!("a"), json!("b")];
    /// let new = vec![json!("a"), json!("b"), json!("c")];
    /// let delta = <Vec<Value> as Diff>::diff(&old, &new);
    /// assert!(!delta.full_replacement);
    ///
    /// // Modification case (full replacement)
    /// let old = vec![json!(1), json!(2)];
    /// let new = vec![json!(1), json!(99)];
    /// let delta = <Vec<Value> as Diff>::diff(&old, &new);
    /// assert!(delta.full_replacement);
    /// ```
    fn diff(old: &Self, new: &Self) -> Self::Delta {
        // Check if new is a strict extension of old (old is a prefix of new)
        if new.len() >= old.len() && old.iter().zip(new.iter()).all(|(a, b)| a == b) {
            VecDelta {
                full_replacement: false,
                start_index: old.len(),
                items: new[old.len()..].to_vec(),
            }
        } else {
            // Items were removed or modified — store full replacement
            VecDelta { full_replacement: true, start_index: 0, items: new.clone() }
        }
    }

    /// Apply a delta to a base `Vec<Value>`, producing the resulting vec.
    ///
    /// If the delta is a full replacement, the base is ignored and the delta's
    /// items are returned directly. Otherwise, the base is truncated to
    /// `start_index` and the delta's items are appended.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_graph::delta::Diff;
    /// use serde_json::{Value, json};
    ///
    /// let old = vec![json!(1), json!(2)];
    /// let new = vec![json!(1), json!(2), json!(3)];
    /// let delta = <Vec<Value> as Diff>::diff(&old, &new);
    /// assert_eq!(<Vec<Value> as Diff>::apply(&old, &delta), new);
    /// ```
    fn apply(base: &Self, delta: &Self::Delta) -> Self {
        if delta.full_replacement {
            delta.items.clone()
        } else {
            let mut result = base[..delta.start_index].to_vec();
            result.extend_from_slice(&delta.items);
            result
        }
    }
}

/// Delta representation for `HashMap<String, Value>`.
///
/// Captures the difference between two maps as three disjoint sets:
/// - `added`: keys present in the new map but not in the old map
/// - `removed`: keys present in the old map but not in the new map
/// - `modified`: keys present in both maps but with different values
///
/// # Example
///
/// ```rust
/// use adk_graph::delta::{Diff, MapDelta};
/// use serde_json::{Value, json};
/// use std::collections::HashMap;
///
/// let old: HashMap<String, Value> = [
///     ("a".to_string(), json!(1)),
///     ("b".to_string(), json!(2)),
/// ].into_iter().collect();
///
/// let new: HashMap<String, Value> = [
///     ("a".to_string(), json!(1)),
///     ("b".to_string(), json!(99)),
///     ("c".to_string(), json!(3)),
/// ].into_iter().collect();
///
/// let delta = <HashMap<String, Value> as Diff>::diff(&old, &new);
/// assert_eq!(delta.added.get("c"), Some(&json!(3)));
/// assert_eq!(delta.removed, Vec::<String>::new());
/// assert_eq!(delta.modified.get("b"), Some(&json!(99)));
///
/// let reconstructed = <HashMap<String, Value> as Diff>::apply(&old, &delta);
/// assert_eq!(reconstructed, new);
/// ```
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MapDelta {
    /// Keys present in the new map but not in the old map, with their values.
    pub added: HashMap<String, Value>,
    /// Keys present in the old map but not in the new map.
    pub removed: Vec<String>,
    /// Keys present in both maps but with different values (new values stored).
    pub modified: HashMap<String, Value>,
}

impl Diff for HashMap<String, Value> {
    type Delta = MapDelta;

    /// Compute the delta between two `HashMap<String, Value>` instances.
    ///
    /// Classifies each key into one of three categories:
    /// - **added**: present in `new` but absent from `old`
    /// - **removed**: present in `old` but absent from `new`
    /// - **modified**: present in both but with different values
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_graph::delta::Diff;
    /// use serde_json::{Value, json};
    /// use std::collections::HashMap;
    ///
    /// let old: HashMap<String, Value> = [
    ///     ("x".to_string(), json!("hello")),
    ///     ("y".to_string(), json!(42)),
    /// ].into_iter().collect();
    ///
    /// let new: HashMap<String, Value> = [
    ///     ("x".to_string(), json!("world")),
    ///     ("z".to_string(), json!(true)),
    /// ].into_iter().collect();
    ///
    /// let delta = <HashMap<String, Value> as Diff>::diff(&old, &new);
    /// assert!(delta.added.contains_key("z"));
    /// assert!(delta.removed.contains(&"y".to_string()));
    /// assert!(delta.modified.contains_key("x"));
    /// ```
    fn diff(old: &Self, new: &Self) -> Self::Delta {
        let mut added = HashMap::new();
        let mut removed = Vec::new();
        let mut modified = HashMap::new();

        // Find removed and modified keys
        for (key, old_value) in old {
            match new.get(key) {
                None => removed.push(key.clone()),
                Some(new_value) if new_value != old_value => {
                    modified.insert(key.clone(), new_value.clone());
                }
                _ => {} // unchanged
            }
        }

        // Find added keys
        for (key, new_value) in new {
            if !old.contains_key(key) {
                added.insert(key.clone(), new_value.clone());
            }
        }

        MapDelta { added, removed, modified }
    }

    /// Apply a delta to a base `HashMap<String, Value>`, producing the resulting map.
    ///
    /// Operations are applied in order: remove keys, insert added keys, update
    /// modified keys.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_graph::delta::Diff;
    /// use serde_json::{Value, json};
    /// use std::collections::HashMap;
    ///
    /// let old: HashMap<String, Value> = [
    ///     ("a".to_string(), json!(1)),
    ///     ("b".to_string(), json!(2)),
    /// ].into_iter().collect();
    ///
    /// let new: HashMap<String, Value> = [
    ///     ("a".to_string(), json!(1)),
    ///     ("c".to_string(), json!(3)),
    /// ].into_iter().collect();
    ///
    /// let delta = <HashMap<String, Value> as Diff>::diff(&old, &new);
    /// assert_eq!(<HashMap<String, Value> as Diff>::apply(&old, &delta), new);
    /// ```
    fn apply(base: &Self, delta: &Self::Delta) -> Self {
        let mut result = base.clone();

        // Remove keys
        for key in &delta.removed {
            result.remove(key);
        }

        // Insert added keys
        for (key, value) in &delta.added {
            result.insert(key.clone(), value.clone());
        }

        // Update modified keys
        for (key, value) in &delta.modified {
            result.insert(key.clone(), value.clone());
        }

        result
    }
}

/// An individual edit operation for string diffs.
///
/// Represents one segment of a string diff: text that is unchanged, text that
/// was inserted, or a deletion of characters from the original string.
///
/// # Example
///
/// ```rust
/// use adk_graph::delta::StringOp;
///
/// let ops = vec![
///     StringOp::Equal("hello ".to_string()),
///     StringOp::Delete(5),   // "world" deleted
///     StringOp::Insert("rust".to_string()),
/// ];
/// ```
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum StringOp {
    /// A segment of text that is identical in both old and new strings.
    Equal(String),
    /// Text that was inserted in the new string (not present in old).
    Insert(String),
    /// Number of characters deleted from the old string (not present in new).
    Delete(usize),
}

/// Delta representation for `String`.
///
/// Captures the difference between two strings as a sequence of edit operations.
/// When applied to the old string, these operations reconstruct the new string.
///
/// Uses the `similar` crate (when the `delta-checkpoint` feature is enabled) to
/// compute character-level diffs.
///
/// # Example
///
/// ```rust
/// use adk_graph::delta::{Diff, StringDelta, StringOp};
///
/// let old = "hello world".to_string();
/// let new = "hello rust".to_string();
///
/// let delta = <String as Diff>::diff(&old, &new);
/// let reconstructed = <String as Diff>::apply(&old, &delta);
/// assert_eq!(reconstructed, new);
/// ```
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StringDelta {
    /// The sequence of edit operations that transform the old string into the new.
    pub ops: Vec<StringOp>,
}

#[cfg(feature = "delta-checkpoint")]
impl Diff for String {
    type Delta = StringDelta;

    /// Compute the delta between two strings using character-level diffing.
    ///
    /// Uses `similar::TextDiff::from_chars()` to compute the minimal set of
    /// edit operations that transform `old` into `new`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_graph::delta::{Diff, StringOp};
    ///
    /// let old = "abcdef".to_string();
    /// let new = "abXYef".to_string();
    ///
    /// let delta = <String as Diff>::diff(&old, &new);
    /// // The delta contains: Equal("ab"), Delete(2), Insert("XY"), Equal("ef")
    /// assert_eq!(<String as Diff>::apply(&old, &delta), new);
    /// ```
    fn diff(old: &Self, new: &Self) -> Self::Delta {
        use similar::{ChangeTag, TextDiff};

        let text_diff = TextDiff::from_chars(old.as_str(), new.as_str());
        let mut ops = Vec::new();

        for change in text_diff.iter_all_changes() {
            let value = change.value();
            match change.tag() {
                ChangeTag::Equal => {
                    // Merge consecutive Equal ops
                    if let Some(StringOp::Equal(s)) = ops.last_mut() {
                        s.push_str(value);
                    } else {
                        ops.push(StringOp::Equal(value.to_string()));
                    }
                }
                ChangeTag::Insert => {
                    // Merge consecutive Insert ops
                    if let Some(StringOp::Insert(s)) = ops.last_mut() {
                        s.push_str(value);
                    } else {
                        ops.push(StringOp::Insert(value.to_string()));
                    }
                }
                ChangeTag::Delete => {
                    // Merge consecutive Delete ops
                    if let Some(StringOp::Delete(count)) = ops.last_mut() {
                        *count += value.chars().count();
                    } else {
                        ops.push(StringOp::Delete(value.chars().count()));
                    }
                }
            }
        }

        StringDelta { ops }
    }

    /// Apply a delta to a base string, producing the resulting string.
    ///
    /// Replays the edit operations in order:
    /// - `Equal(s)`: advance past `s.len()` characters in the base (copy them)
    /// - `Delete(n)`: skip `n` characters in the base
    /// - `Insert(s)`: append `s` to the result
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_graph::delta::Diff;
    ///
    /// let old = "hello world".to_string();
    /// let new = "hello rust".to_string();
    /// let delta = <String as Diff>::diff(&old, &new);
    /// assert_eq!(<String as Diff>::apply(&old, &delta), new);
    /// ```
    fn apply(base: &Self, delta: &Self::Delta) -> Self {
        let mut result = String::new();
        let mut chars = base.chars();

        for op in &delta.ops {
            match op {
                StringOp::Equal(s) => {
                    // Advance past the equal characters in base
                    for _ in 0..s.chars().count() {
                        if let Some(c) = chars.next() {
                            result.push(c);
                        }
                    }
                }
                StringOp::Delete(count) => {
                    // Skip characters in base
                    for _ in 0..*count {
                        chars.next();
                    }
                }
                StringOp::Insert(s) => {
                    // Append inserted text
                    result.push_str(s);
                }
            }
        }

        result
    }
}

// --- Metadata keys used to tag checkpoint records ---

/// Metadata key indicating the checkpoint type (full or delta).
const META_CHECKPOINT_TYPE: &str = "__delta_ckpt_type";
/// Metadata key storing the serialized delta payload for delta checkpoints.
const META_DELTA_PAYLOAD: &str = "__delta_ckpt_payload";
/// Metadata key storing the base checkpoint ID for delta checkpoints.
const META_BASE_CHECKPOINT_ID: &str = "__delta_ckpt_base_id";

/// Metadata value indicating a full snapshot.
const TYPE_FULL: &str = "full";
/// Metadata value indicating a delta record.
const TYPE_DELTA: &str = "delta";

/// Wraps any [`Checkpointer`] with delta compression.
///
/// On save, the `DeltaCheckpointer` stores either a full state snapshot or an
/// incremental delta depending on the step number and the configured
/// [`DeltaConfig::full_snapshot_interval`]. On load, it reconstructs the full
/// state by finding the nearest full snapshot and replaying deltas forward.
///
/// The `DeltaCheckpointer` exposes the same [`Checkpointer`] trait interface,
/// making it a drop-in replacement for any existing checkpointer.
///
/// # Example
///
/// ```rust,ignore
/// use adk_graph::delta::{DeltaCheckpointer, DeltaConfig};
/// use adk_graph::checkpoint::MemoryCheckpointer;
///
/// let inner = MemoryCheckpointer::new();
/// let config = DeltaConfig { full_snapshot_interval: 5 };
/// let checkpointer = DeltaCheckpointer::new(inner, config);
///
/// // Use checkpointer as any other Checkpointer implementation
/// ```
pub struct DeltaCheckpointer<C: Checkpointer> {
    inner: C,
    config: DeltaConfig,
}

impl<C: Checkpointer> DeltaCheckpointer<C> {
    /// Create a new `DeltaCheckpointer` wrapping the given inner checkpointer.
    ///
    /// # Arguments
    ///
    /// * `inner` - The underlying checkpointer used for actual storage.
    /// * `config` - Configuration controlling full snapshot frequency.
    pub fn new(inner: C, config: DeltaConfig) -> Self {
        Self { inner, config }
    }

    /// Returns a reference to the inner checkpointer.
    pub fn inner(&self) -> &C {
        &self.inner
    }

    /// Returns the delta configuration.
    pub fn config(&self) -> &DeltaConfig {
        &self.config
    }

    /// Determine whether a given step should be a full snapshot.
    fn is_full_snapshot_step(&self, step: usize) -> bool {
        step == 0 || (step as u32).is_multiple_of(self.config.full_snapshot_interval)
    }

    /// Reconstruct full state from a list of checkpoints starting from a full
    /// snapshot and replaying deltas forward up to (and including) the target step.
    fn reconstruct_state(
        checkpoints: &[Checkpoint],
        target_step: usize,
    ) -> crate::error::Result<State> {
        // Find the nearest full snapshot at or before target_step
        let full_snapshot_idx = checkpoints
            .iter()
            .enumerate()
            .rev()
            .find(|(_, cp)| {
                cp.step <= target_step
                    && cp.metadata.get(META_CHECKPOINT_TYPE).and_then(|v| v.as_str())
                        == Some(TYPE_FULL)
            })
            .map(|(idx, _)| idx);

        let full_idx = full_snapshot_idx.ok_or_else(|| {
            GraphError::CheckpointError(
                "No full snapshot found for state reconstruction".to_string(),
            )
        })?;

        let base_checkpoint = &checkpoints[full_idx];
        let mut state = base_checkpoint.state.clone();

        // Replay deltas forward from the full snapshot to the target step
        for cp in &checkpoints[full_idx + 1..] {
            if cp.step > target_step {
                break;
            }

            let cp_type =
                cp.metadata.get(META_CHECKPOINT_TYPE).and_then(|v| v.as_str()).unwrap_or(TYPE_FULL);

            if cp_type == TYPE_DELTA {
                // Extract and apply the delta
                let delta_json = cp.metadata.get(META_DELTA_PAYLOAD).ok_or_else(|| {
                    GraphError::CheckpointError(format!(
                        "Delta checkpoint at step {} missing payload",
                        cp.step
                    ))
                })?;

                let delta: MapDelta = serde_json::from_value(delta_json.clone()).map_err(|e| {
                    GraphError::CheckpointError(format!(
                        "Failed to deserialize delta at step {}: {e}",
                        cp.step
                    ))
                })?;

                state = <HashMap<String, Value> as Diff>::apply(&state, &delta);
            } else {
                // It's a full snapshot — use its state directly
                state = cp.state.clone();
            }
        }

        Ok(state)
    }
}

#[async_trait]
impl<C: Checkpointer> Checkpointer for DeltaCheckpointer<C> {
    /// Save a checkpoint with delta compression.
    ///
    /// If the step is a full snapshot boundary (step == 0 or step %
    /// full_snapshot_interval == 0), the full state is stored. Otherwise, the
    /// delta from the previous checkpoint is computed and stored, with the
    /// checkpoint's `state` field set to an empty map to save space.
    async fn save(&self, checkpoint: &Checkpoint) -> crate::error::Result<String> {
        if self.is_full_snapshot_step(checkpoint.step) {
            // Store full snapshot with type metadata
            let mut full_checkpoint = checkpoint.clone();
            full_checkpoint
                .metadata
                .insert(META_CHECKPOINT_TYPE.to_string(), Value::String(TYPE_FULL.to_string()));
            self.inner.save(&full_checkpoint).await
        } else {
            // Load the previous checkpoint to compute delta
            let all_checkpoints = self.inner.list(&checkpoint.thread_id).await?;

            // Find the previous checkpoint (the one with the highest step < current step)
            let previous = all_checkpoints
                .iter()
                .filter(|cp| cp.step < checkpoint.step)
                .max_by_key(|cp| cp.step);

            match previous {
                Some(prev_cp) => {
                    // Reconstruct the previous state (it might itself be a delta)
                    let prev_state = Self::reconstruct_state(&all_checkpoints, prev_cp.step)?;

                    // Compute delta from previous state to current state
                    let delta =
                        <HashMap<String, Value> as Diff>::diff(&prev_state, &checkpoint.state);

                    // Store the delta checkpoint with minimal state (empty map)
                    let mut delta_checkpoint = checkpoint.clone();
                    delta_checkpoint.state = HashMap::new(); // Don't store full state
                    delta_checkpoint.metadata.insert(
                        META_CHECKPOINT_TYPE.to_string(),
                        Value::String(TYPE_DELTA.to_string()),
                    );
                    delta_checkpoint.metadata.insert(
                        META_BASE_CHECKPOINT_ID.to_string(),
                        Value::String(prev_cp.checkpoint_id.clone()),
                    );
                    delta_checkpoint.metadata.insert(
                        META_DELTA_PAYLOAD.to_string(),
                        serde_json::to_value(&delta).map_err(|e| {
                            GraphError::CheckpointError(format!("Failed to serialize delta: {e}"))
                        })?,
                    );

                    self.inner.save(&delta_checkpoint).await
                }
                None => {
                    // No previous checkpoint found — store as full snapshot
                    let mut full_checkpoint = checkpoint.clone();
                    full_checkpoint.metadata.insert(
                        META_CHECKPOINT_TYPE.to_string(),
                        Value::String(TYPE_FULL.to_string()),
                    );
                    self.inner.save(&full_checkpoint).await
                }
            }
        }
    }

    /// Load the latest checkpoint for a thread, reconstructing full state.
    ///
    /// Finds the latest checkpoint, then reconstructs the full state by
    /// replaying deltas from the nearest full snapshot forward.
    async fn load(&self, thread_id: &str) -> crate::error::Result<Option<Checkpoint>> {
        let all_checkpoints = self.inner.list(thread_id).await?;
        if all_checkpoints.is_empty() {
            return Ok(None);
        }

        // Get the latest checkpoint
        let latest = all_checkpoints.last().unwrap();
        let target_step = latest.step;

        // Reconstruct the full state
        let full_state = Self::reconstruct_state(&all_checkpoints, target_step)?;

        // Return the latest checkpoint with reconstructed state
        let mut result = latest.clone();
        result.state = full_state;
        Ok(Some(result))
    }

    /// Load a specific checkpoint by ID, reconstructing full state.
    ///
    /// Finds the checkpoint by ID, determines its thread, then reconstructs
    /// the full state by replaying deltas from the nearest full snapshot.
    async fn load_by_id(&self, checkpoint_id: &str) -> crate::error::Result<Option<Checkpoint>> {
        let checkpoint = self.inner.load_by_id(checkpoint_id).await?;
        match checkpoint {
            Some(cp) => {
                let all_checkpoints = self.inner.list(&cp.thread_id).await?;
                let full_state = Self::reconstruct_state(&all_checkpoints, cp.step)?;

                let mut result = cp;
                result.state = full_state;
                Ok(Some(result))
            }
            None => Ok(None),
        }
    }

    /// List all checkpoints for a thread.
    ///
    /// Delegates directly to the inner checkpointer. Note that delta
    /// checkpoints in the list will have empty state fields — use
    /// [`load`](Self::load) or [`load_by_id`](Self::load_by_id) to get
    /// reconstructed state.
    async fn list(&self, thread_id: &str) -> crate::error::Result<Vec<Checkpoint>> {
        self.inner.list(thread_id).await
    }

    /// Delete all checkpoints for a thread.
    ///
    /// Delegates directly to the inner checkpointer.
    async fn delete(&self, thread_id: &str) -> crate::error::Result<()> {
        self.inner.delete(thread_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_config_default_interval_is_10() {
        let config = DeltaConfig::default();
        assert_eq!(config.full_snapshot_interval, 10);
    }

    #[test]
    fn delta_config_custom_interval() {
        let config = DeltaConfig { full_snapshot_interval: 25 };
        assert_eq!(config.full_snapshot_interval, 25);
    }

    #[test]
    fn checkpoint_type_full_variant() {
        let ct = CheckpointType::Full;
        assert!(matches!(ct, CheckpointType::Full));
    }

    #[test]
    fn checkpoint_type_delta_variant() {
        let ct = CheckpointType::Delta { base_checkpoint_id: "abc-123".to_string() };
        assert!(matches!(ct, CheckpointType::Delta { .. }));
        if let CheckpointType::Delta { base_checkpoint_id } = ct {
            assert_eq!(base_checkpoint_id, "abc-123");
        }
    }

    #[test]
    fn vec_value_diff_append() {
        use serde_json::json;
        let old = vec![json!(1), json!(2), json!(3)];
        let new = vec![json!(1), json!(2), json!(3), json!(4), json!(5)];
        let delta = <Vec<Value> as Diff>::diff(&old, &new);
        assert!(!delta.full_replacement);
        assert_eq!(delta.start_index, 3);
        assert_eq!(delta.items, vec![json!(4), json!(5)]);
        assert_eq!(<Vec<Value> as Diff>::apply(&old, &delta), new);
    }

    #[test]
    fn vec_value_diff_identical() {
        use serde_json::json;
        let old = vec![json!("a"), json!("b")];
        let new = vec![json!("a"), json!("b")];
        let delta = <Vec<Value> as Diff>::diff(&old, &new);
        assert!(!delta.full_replacement);
        assert_eq!(delta.start_index, 2);
        assert!(delta.items.is_empty());
        assert_eq!(<Vec<Value> as Diff>::apply(&old, &delta), new);
    }

    #[test]
    fn vec_value_diff_shorter_new() {
        use serde_json::json;
        let old = vec![json!(1), json!(2), json!(3)];
        let new = vec![json!(1)];
        let delta = <Vec<Value> as Diff>::diff(&old, &new);
        assert!(delta.full_replacement);
        assert_eq!(delta.items, vec![json!(1)]);
        assert_eq!(<Vec<Value> as Diff>::apply(&old, &delta), new);
    }

    #[test]
    fn vec_value_diff_modified_element() {
        use serde_json::json;
        let old = vec![json!(1), json!(2), json!(3)];
        let new = vec![json!(1), json!(99), json!(3)];
        let delta = <Vec<Value> as Diff>::diff(&old, &new);
        assert!(delta.full_replacement);
        assert_eq!(<Vec<Value> as Diff>::apply(&old, &delta), new);
    }

    #[test]
    fn vec_value_diff_empty_to_items() {
        use serde_json::json;
        let old: Vec<Value> = vec![];
        let new = vec![json!(1), json!(2)];
        let delta = <Vec<Value> as Diff>::diff(&old, &new);
        assert!(!delta.full_replacement);
        assert_eq!(delta.start_index, 0);
        assert_eq!(delta.items, vec![json!(1), json!(2)]);
        assert_eq!(<Vec<Value> as Diff>::apply(&old, &delta), new);
    }

    #[test]
    fn vec_value_diff_items_to_empty() {
        use serde_json::json;
        let old = vec![json!(1), json!(2)];
        let new: Vec<Value> = vec![];
        let delta = <Vec<Value> as Diff>::diff(&old, &new);
        assert!(delta.full_replacement);
        assert!(delta.items.is_empty());
        assert_eq!(<Vec<Value> as Diff>::apply(&old, &delta), new);
    }

    #[test]
    fn map_value_diff_added_keys() {
        use serde_json::json;
        let old: HashMap<String, Value> = [("a".to_string(), json!(1))].into_iter().collect();
        let new: HashMap<String, Value> =
            [("a".to_string(), json!(1)), ("b".to_string(), json!(2)), ("c".to_string(), json!(3))]
                .into_iter()
                .collect();
        let delta = <HashMap<String, Value> as Diff>::diff(&old, &new);
        assert!(delta.removed.is_empty());
        assert!(delta.modified.is_empty());
        assert_eq!(delta.added.len(), 2);
        assert_eq!(delta.added.get("b"), Some(&json!(2)));
        assert_eq!(delta.added.get("c"), Some(&json!(3)));
        assert_eq!(<HashMap<String, Value> as Diff>::apply(&old, &delta), new);
    }

    #[test]
    fn map_value_diff_removed_keys() {
        use serde_json::json;
        let old: HashMap<String, Value> =
            [("a".to_string(), json!(1)), ("b".to_string(), json!(2)), ("c".to_string(), json!(3))]
                .into_iter()
                .collect();
        let new: HashMap<String, Value> = [("a".to_string(), json!(1))].into_iter().collect();
        let delta = <HashMap<String, Value> as Diff>::diff(&old, &new);
        assert!(delta.added.is_empty());
        assert!(delta.modified.is_empty());
        assert_eq!(delta.removed.len(), 2);
        assert!(delta.removed.contains(&"b".to_string()));
        assert!(delta.removed.contains(&"c".to_string()));
        assert_eq!(<HashMap<String, Value> as Diff>::apply(&old, &delta), new);
    }

    #[test]
    fn map_value_diff_modified_keys() {
        use serde_json::json;
        let old: HashMap<String, Value> =
            [("a".to_string(), json!(1)), ("b".to_string(), json!(2))].into_iter().collect();
        let new: HashMap<String, Value> =
            [("a".to_string(), json!(1)), ("b".to_string(), json!(99))].into_iter().collect();
        let delta = <HashMap<String, Value> as Diff>::diff(&old, &new);
        assert!(delta.added.is_empty());
        assert!(delta.removed.is_empty());
        assert_eq!(delta.modified.len(), 1);
        assert_eq!(delta.modified.get("b"), Some(&json!(99)));
        assert_eq!(<HashMap<String, Value> as Diff>::apply(&old, &delta), new);
    }

    #[test]
    fn map_value_diff_mixed_changes() {
        use serde_json::json;
        let old: HashMap<String, Value> =
            [("a".to_string(), json!(1)), ("b".to_string(), json!(2)), ("c".to_string(), json!(3))]
                .into_iter()
                .collect();
        let new: HashMap<String, Value> = [
            ("a".to_string(), json!(1)),
            ("b".to_string(), json!(99)),
            ("d".to_string(), json!(4)),
        ]
        .into_iter()
        .collect();
        let delta = <HashMap<String, Value> as Diff>::diff(&old, &new);
        assert_eq!(delta.added.len(), 1);
        assert_eq!(delta.added.get("d"), Some(&json!(4)));
        assert_eq!(delta.removed, vec!["c".to_string()]);
        assert_eq!(delta.modified.len(), 1);
        assert_eq!(delta.modified.get("b"), Some(&json!(99)));
        assert_eq!(<HashMap<String, Value> as Diff>::apply(&old, &delta), new);
    }

    #[test]
    fn map_value_diff_identical() {
        use serde_json::json;
        let old: HashMap<String, Value> =
            [("a".to_string(), json!(1)), ("b".to_string(), json!(2))].into_iter().collect();
        let new = old.clone();
        let delta = <HashMap<String, Value> as Diff>::diff(&old, &new);
        assert!(delta.added.is_empty());
        assert!(delta.removed.is_empty());
        assert!(delta.modified.is_empty());
        assert_eq!(<HashMap<String, Value> as Diff>::apply(&old, &delta), new);
    }

    #[test]
    fn map_value_diff_empty_to_populated() {
        use serde_json::json;
        let old: HashMap<String, Value> = HashMap::new();
        let new: HashMap<String, Value> =
            [("a".to_string(), json!(1)), ("b".to_string(), json!(2))].into_iter().collect();
        let delta = <HashMap<String, Value> as Diff>::diff(&old, &new);
        assert_eq!(delta.added.len(), 2);
        assert!(delta.removed.is_empty());
        assert!(delta.modified.is_empty());
        assert_eq!(<HashMap<String, Value> as Diff>::apply(&old, &delta), new);
    }

    #[test]
    fn map_value_diff_populated_to_empty() {
        use serde_json::json;
        let old: HashMap<String, Value> =
            [("a".to_string(), json!(1)), ("b".to_string(), json!(2))].into_iter().collect();
        let new: HashMap<String, Value> = HashMap::new();
        let delta = <HashMap<String, Value> as Diff>::diff(&old, &new);
        assert!(delta.added.is_empty());
        assert_eq!(delta.removed.len(), 2);
        assert!(delta.modified.is_empty());
        assert_eq!(<HashMap<String, Value> as Diff>::apply(&old, &delta), new);
    }

    #[test]
    fn map_value_diff_nested_values() {
        use serde_json::json;
        let old: HashMap<String, Value> = [
            ("config".to_string(), json!({"host": "localhost", "port": 8080})),
            ("name".to_string(), json!("app")),
        ]
        .into_iter()
        .collect();
        let new: HashMap<String, Value> = [
            ("config".to_string(), json!({"host": "prod.example.com", "port": 443})),
            ("name".to_string(), json!("app")),
            ("version".to_string(), json!("1.0.0")),
        ]
        .into_iter()
        .collect();
        let delta = <HashMap<String, Value> as Diff>::diff(&old, &new);
        assert_eq!(delta.added.get("version"), Some(&json!("1.0.0")));
        assert!(delta.removed.is_empty());
        assert_eq!(
            delta.modified.get("config"),
            Some(&json!({"host": "prod.example.com", "port": 443}))
        );
        assert_eq!(<HashMap<String, Value> as Diff>::apply(&old, &delta), new);
    }

    #[cfg(feature = "delta-checkpoint")]
    #[test]
    fn string_diff_identical() {
        let old = "hello world".to_string();
        let new = "hello world".to_string();
        let delta = <String as Diff>::diff(&old, &new);
        assert_eq!(delta.ops.len(), 1);
        assert_eq!(delta.ops[0], StringOp::Equal("hello world".to_string()));
        assert_eq!(<String as Diff>::apply(&old, &delta), new);
    }

    #[cfg(feature = "delta-checkpoint")]
    #[test]
    fn string_diff_completely_different() {
        let old = "abc".to_string();
        let new = "xyz".to_string();
        let delta = <String as Diff>::diff(&old, &new);
        let reconstructed = <String as Diff>::apply(&old, &delta);
        assert_eq!(reconstructed, new);
    }

    #[cfg(feature = "delta-checkpoint")]
    #[test]
    fn string_diff_suffix_change() {
        let old = "hello world".to_string();
        let new = "hello rust".to_string();
        let delta = <String as Diff>::diff(&old, &new);
        let reconstructed = <String as Diff>::apply(&old, &delta);
        assert_eq!(reconstructed, new);
    }

    #[cfg(feature = "delta-checkpoint")]
    #[test]
    fn string_diff_prefix_change() {
        let old = "hello world".to_string();
        let new = "goodbye world".to_string();
        let delta = <String as Diff>::diff(&old, &new);
        let reconstructed = <String as Diff>::apply(&old, &delta);
        assert_eq!(reconstructed, new);
    }

    #[cfg(feature = "delta-checkpoint")]
    #[test]
    fn string_diff_middle_insertion() {
        let old = "abcdef".to_string();
        let new = "abcXYZdef".to_string();
        let delta = <String as Diff>::diff(&old, &new);
        let reconstructed = <String as Diff>::apply(&old, &delta);
        assert_eq!(reconstructed, new);
    }

    #[cfg(feature = "delta-checkpoint")]
    #[test]
    fn string_diff_middle_deletion() {
        let old = "abcXYZdef".to_string();
        let new = "abcdef".to_string();
        let delta = <String as Diff>::diff(&old, &new);
        let reconstructed = <String as Diff>::apply(&old, &delta);
        assert_eq!(reconstructed, new);
    }

    #[cfg(feature = "delta-checkpoint")]
    #[test]
    fn string_diff_empty_to_content() {
        let old = String::new();
        let new = "hello".to_string();
        let delta = <String as Diff>::diff(&old, &new);
        assert_eq!(delta.ops.len(), 1);
        assert_eq!(delta.ops[0], StringOp::Insert("hello".to_string()));
        assert_eq!(<String as Diff>::apply(&old, &delta), new);
    }

    #[cfg(feature = "delta-checkpoint")]
    #[test]
    fn string_diff_content_to_empty() {
        let old = "hello".to_string();
        let new = String::new();
        let delta = <String as Diff>::diff(&old, &new);
        assert_eq!(delta.ops.len(), 1);
        assert_eq!(delta.ops[0], StringOp::Delete(5));
        assert_eq!(<String as Diff>::apply(&old, &delta), new);
    }

    #[cfg(feature = "delta-checkpoint")]
    #[test]
    fn string_diff_both_empty() {
        let old = String::new();
        let new = String::new();
        let delta = <String as Diff>::diff(&old, &new);
        assert!(delta.ops.is_empty());
        assert_eq!(<String as Diff>::apply(&old, &delta), new);
    }

    #[cfg(feature = "delta-checkpoint")]
    #[test]
    fn string_diff_unicode() {
        let old = "héllo wörld 🌍".to_string();
        let new = "héllo rüst 🦀".to_string();
        let delta = <String as Diff>::diff(&old, &new);
        let reconstructed = <String as Diff>::apply(&old, &delta);
        assert_eq!(reconstructed, new);
    }

    #[cfg(feature = "delta-checkpoint")]
    #[test]
    fn string_diff_multiline() {
        let old = "line1\nline2\nline3".to_string();
        let new = "line1\nmodified\nline3\nline4".to_string();
        let delta = <String as Diff>::diff(&old, &new);
        let reconstructed = <String as Diff>::apply(&old, &delta);
        assert_eq!(reconstructed, new);
    }

    #[cfg(feature = "delta-checkpoint")]
    #[test]
    fn string_delta_serialization_round_trip() {
        let old = "hello world".to_string();
        let new = "hello rust".to_string();
        let delta = <String as Diff>::diff(&old, &new);

        // Serialize and deserialize the delta
        let json = serde_json::to_string(&delta).unwrap();
        let deserialized: StringDelta = serde_json::from_str(&json).unwrap();

        assert_eq!(<String as Diff>::apply(&old, &deserialized), new);
    }

    /// Verify the Diff trait can be implemented and round-trips correctly.
    #[test]
    fn diff_trait_round_trip() {
        #[derive(Clone, Debug, PartialEq)]
        struct TestState(Vec<i32>);

        #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
        struct TestDelta {
            added: Vec<i32>,
            removed_count: usize,
        }

        impl Diff for TestState {
            type Delta = TestDelta;

            fn diff(old: &Self, new: &Self) -> Self::Delta {
                // Simple: assume new extends or truncates old
                if new.0.len() >= old.0.len() {
                    TestDelta { added: new.0[old.0.len()..].to_vec(), removed_count: 0 }
                } else {
                    TestDelta { added: vec![], removed_count: old.0.len() - new.0.len() }
                }
            }

            fn apply(base: &Self, delta: &Self::Delta) -> Self {
                let mut result = base.0.clone();
                if delta.removed_count > 0 {
                    result.truncate(result.len() - delta.removed_count);
                }
                result.extend_from_slice(&delta.added);
                TestState(result)
            }
        }

        let s1 = TestState(vec![1, 2, 3]);
        let s2 = TestState(vec![1, 2, 3, 4, 5]);
        let delta = TestState::diff(&s1, &s2);
        let reconstructed = TestState::apply(&s1, &delta);
        assert_eq!(reconstructed, s2);
    }

    // --- DeltaCheckpointer tests ---

    use crate::checkpoint::MemoryCheckpointer;

    fn make_state(pairs: &[(&str, i64)]) -> State {
        pairs.iter().map(|(k, v)| (k.to_string(), serde_json::json!(v))).collect()
    }

    #[tokio::test]
    async fn delta_checkpointer_first_save_is_full() {
        let inner = MemoryCheckpointer::new();
        let dc = DeltaCheckpointer::new(inner, DeltaConfig { full_snapshot_interval: 5 });

        let state = make_state(&[("x", 1)]);
        let cp = Checkpoint::new("t1", state.clone(), 0, vec![]);
        dc.save(&cp).await.unwrap();

        // Load should return the full state
        let loaded = dc.load("t1").await.unwrap().unwrap();
        assert_eq!(loaded.state, state);
        assert_eq!(
            loaded.metadata.get(META_CHECKPOINT_TYPE).and_then(|v| v.as_str()),
            Some(TYPE_FULL)
        );
    }

    #[tokio::test]
    async fn delta_checkpointer_stores_delta_between_full_snapshots() {
        let inner = MemoryCheckpointer::new();
        let dc = DeltaCheckpointer::new(inner, DeltaConfig { full_snapshot_interval: 5 });

        // Step 0: full snapshot
        let state0 = make_state(&[("x", 1), ("y", 2)]);
        let cp0 = Checkpoint::new("t1", state0, 0, vec![]);
        dc.save(&cp0).await.unwrap();

        // Step 1: should be delta
        let state1 = make_state(&[("x", 1), ("y", 3), ("z", 4)]);
        let cp1 = Checkpoint::new("t1", state1.clone(), 1, vec![]);
        dc.save(&cp1).await.unwrap();

        // Load should reconstruct the full state at step 1
        let loaded = dc.load("t1").await.unwrap().unwrap();
        assert_eq!(loaded.state, state1);
        assert_eq!(loaded.step, 1);
    }

    #[tokio::test]
    async fn delta_checkpointer_full_snapshot_at_interval() {
        let inner = MemoryCheckpointer::new();
        let dc = DeltaCheckpointer::new(inner, DeltaConfig { full_snapshot_interval: 3 });

        // Steps 0, 1, 2, 3
        for step in 0..=3 {
            let state = make_state(&[("counter", step as i64)]);
            let cp = Checkpoint::new("t1", state, step, vec![]);
            dc.save(&cp).await.unwrap();
        }

        // Step 3 should be a full snapshot (3 % 3 == 0)
        let all = dc.list("t1").await.unwrap();
        let step3 = all.iter().find(|cp| cp.step == 3).unwrap();
        assert_eq!(
            step3.metadata.get(META_CHECKPOINT_TYPE).and_then(|v| v.as_str()),
            Some(TYPE_FULL)
        );

        // Step 1 should be a delta
        let step1 = all.iter().find(|cp| cp.step == 1).unwrap();
        assert_eq!(
            step1.metadata.get(META_CHECKPOINT_TYPE).and_then(|v| v.as_str()),
            Some(TYPE_DELTA)
        );
    }

    #[tokio::test]
    async fn delta_checkpointer_load_by_id_reconstructs_state() {
        let inner = MemoryCheckpointer::new();
        let dc = DeltaCheckpointer::new(inner, DeltaConfig { full_snapshot_interval: 10 });

        // Step 0: full
        let state0 = make_state(&[("a", 1)]);
        let cp0 = Checkpoint::new("t1", state0, 0, vec![]);
        dc.save(&cp0).await.unwrap();

        // Step 1: delta
        let state1 = make_state(&[("a", 1), ("b", 2)]);
        let cp1 = Checkpoint::new("t1", state1.clone(), 1, vec![]);
        let id1 = dc.save(&cp1).await.unwrap();

        // Step 2: delta
        let state2 = make_state(&[("a", 1), ("b", 2), ("c", 3)]);
        let cp2 = Checkpoint::new("t1", state2.clone(), 2, vec![]);
        let id2 = dc.save(&cp2).await.unwrap();

        // Load by ID for step 1
        let loaded1 = dc.load_by_id(&id1).await.unwrap().unwrap();
        assert_eq!(loaded1.state, state1);

        // Load by ID for step 2
        let loaded2 = dc.load_by_id(&id2).await.unwrap().unwrap();
        assert_eq!(loaded2.state, state2);
    }

    #[tokio::test]
    async fn delta_checkpointer_multiple_deltas_between_snapshots() {
        let inner = MemoryCheckpointer::new();
        let dc = DeltaCheckpointer::new(inner, DeltaConfig { full_snapshot_interval: 5 });

        // Create 5 checkpoints (step 0 = full, steps 1-4 = delta)
        let states: Vec<State> =
            (0..5).map(|i| make_state(&[("step", i), ("data", i * 10)])).collect();

        for (step, state) in states.iter().enumerate() {
            let cp = Checkpoint::new("t1", state.clone(), step, vec![]);
            dc.save(&cp).await.unwrap();
        }

        // Load latest (step 4) — should reconstruct through all deltas
        let loaded = dc.load("t1").await.unwrap().unwrap();
        assert_eq!(loaded.state, states[4]);
        assert_eq!(loaded.step, 4);
    }

    #[tokio::test]
    async fn delta_checkpointer_delete_delegates() {
        let inner = MemoryCheckpointer::new();
        let dc = DeltaCheckpointer::new(inner, DeltaConfig::default());

        let state = make_state(&[("x", 1)]);
        let cp = Checkpoint::new("t1", state, 0, vec![]);
        dc.save(&cp).await.unwrap();

        dc.delete("t1").await.unwrap();
        let loaded = dc.load("t1").await.unwrap();
        assert!(loaded.is_none());
    }

    #[tokio::test]
    async fn delta_checkpointer_handles_key_removal() {
        let inner = MemoryCheckpointer::new();
        let dc = DeltaCheckpointer::new(inner, DeltaConfig { full_snapshot_interval: 10 });

        // Step 0: full with keys a, b, c
        let state0 = make_state(&[("a", 1), ("b", 2), ("c", 3)]);
        let cp0 = Checkpoint::new("t1", state0, 0, vec![]);
        dc.save(&cp0).await.unwrap();

        // Step 1: remove key b, modify c
        let state1 = make_state(&[("a", 1), ("c", 99)]);
        let cp1 = Checkpoint::new("t1", state1.clone(), 1, vec![]);
        dc.save(&cp1).await.unwrap();

        // Load should reconstruct correctly
        let loaded = dc.load("t1").await.unwrap().unwrap();
        assert_eq!(loaded.state, state1);
        assert!(!loaded.state.contains_key("b"));
    }

    #[tokio::test]
    async fn delta_checkpointer_reconstruction_across_full_snapshot_boundary() {
        let inner = MemoryCheckpointer::new();
        let dc = DeltaCheckpointer::new(inner, DeltaConfig { full_snapshot_interval: 3 });

        // Steps 0-5: full at 0, 3; deltas at 1, 2, 4, 5
        let states: Vec<State> = (0..6).map(|i| make_state(&[("val", i * 100)])).collect();

        for (step, state) in states.iter().enumerate() {
            let cp = Checkpoint::new("t1", state.clone(), step, vec![]);
            dc.save(&cp).await.unwrap();
        }

        // Load latest (step 5) — should reconstruct from full at step 3 + deltas 4, 5
        let loaded = dc.load("t1").await.unwrap().unwrap();
        assert_eq!(loaded.state, states[5]);
    }
}
