//! State management for graph execution
//!
//! Provides typed state with reducers for controlling how updates are merged.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// Graph state - a map of channel names to values
pub type State = HashMap<String, Value>;

/// Reducer determines how state updates are merged
#[derive(Clone)]
pub enum Reducer {
    /// Replace the value entirely (default)
    Overwrite,
    /// Append to a list
    Append,
    /// Sum numeric values
    Sum,
    /// Custom merge function
    Custom(Arc<dyn Fn(Value, Value) -> Value + Send + Sync>),
}

// Cannot derive Default because of the Custom variant with Arc<dyn Fn>
#[allow(clippy::derivable_impls)]
impl Default for Reducer {
    fn default() -> Self {
        Self::Overwrite
    }
}

impl std::fmt::Debug for Reducer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Overwrite => write!(f, "Overwrite"),
            Self::Append => write!(f, "Append"),
            Self::Sum => write!(f, "Sum"),
            Self::Custom(_) => write!(f, "Custom"),
        }
    }
}

/// Channel definition for a state field
#[derive(Clone)]
pub struct Channel {
    /// Channel name
    pub name: String,
    /// Reducer for merging updates
    pub reducer: Reducer,
    /// Default value
    pub default: Option<Value>,
}

impl Channel {
    /// Create a new channel with overwrite semantics
    pub fn new(name: &str) -> Self {
        Self { name: name.to_string(), reducer: Reducer::Overwrite, default: None }
    }

    /// Create a list channel with append semantics
    pub fn list(name: &str) -> Self {
        Self { name: name.to_string(), reducer: Reducer::Append, default: Some(json!([])) }
    }

    /// Create a counter channel with sum semantics
    pub fn counter(name: &str) -> Self {
        Self { name: name.to_string(), reducer: Reducer::Sum, default: Some(json!(0)) }
    }

    /// Set the reducer
    pub fn with_reducer(mut self, reducer: Reducer) -> Self {
        self.reducer = reducer;
        self
    }

    /// Set the default value
    pub fn with_default(mut self, default: Value) -> Self {
        self.default = Some(default);
        self
    }
}

/// State schema defines channels and their reducers
#[derive(Clone, Default)]
pub struct StateSchema {
    /// Channel definitions
    pub channels: HashMap<String, Channel>,
}

impl StateSchema {
    /// Create a new empty schema
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a schema builder
    pub fn builder() -> StateSchemaBuilder {
        StateSchemaBuilder::default()
    }

    /// Create a simple schema with just channel names (all overwrite)
    pub fn simple(channels: &[&str]) -> Self {
        let mut schema = Self::new();
        for name in channels {
            schema.channels.insert((*name).to_string(), Channel::new(name));
        }
        schema
    }

    /// Get the reducer for a channel
    pub fn get_reducer(&self, channel: &str) -> &Reducer {
        self.channels.get(channel).map(|c| &c.reducer).unwrap_or(&Reducer::Overwrite)
    }

    /// Get the default value for a channel
    pub fn get_default(&self, channel: &str) -> Option<&Value> {
        self.channels.get(channel).and_then(|c| c.default.as_ref())
    }

    /// Apply an update to state using the appropriate reducer
    pub fn apply_update(&self, state: &mut State, key: &str, value: Value) {
        let reducer = self.get_reducer(key);
        let current = state.get(key).cloned().unwrap_or(Value::Null);

        let new_value = match reducer {
            Reducer::Overwrite => value,
            Reducer::Append => {
                let mut arr = match current {
                    Value::Array(a) => a,
                    Value::Null => vec![],
                    _ => vec![current],
                };
                match value {
                    Value::Array(items) => arr.extend(items),
                    _ => arr.push(value),
                }
                Value::Array(arr)
            }
            Reducer::Sum => {
                let current_num = current.as_f64().unwrap_or(0.0);
                let add_num = value.as_f64().unwrap_or(0.0);
                json!(current_num + add_num)
            }
            Reducer::Custom(f) => f(current, value),
        };

        state.insert(key.to_string(), new_value);
    }

    /// Initialize state with default values
    pub fn initialize_state(&self) -> State {
        let mut state = State::new();
        for (name, channel) in &self.channels {
            if let Some(default) = &channel.default {
                state.insert(name.clone(), default.clone());
            }
        }
        state
    }
}

/// Builder for StateSchema
#[derive(Default)]
pub struct StateSchemaBuilder {
    channels: HashMap<String, Channel>,
}

impl StateSchemaBuilder {
    /// Add a channel with overwrite semantics
    pub fn channel(mut self, name: &str) -> Self {
        self.channels.insert(name.to_string(), Channel::new(name));
        self
    }

    /// Add a channel with append semantics (for lists)
    pub fn list_channel(mut self, name: &str) -> Self {
        self.channels.insert(name.to_string(), Channel::list(name));
        self
    }

    /// Add a counter channel with sum semantics
    pub fn counter_channel(mut self, name: &str) -> Self {
        self.channels.insert(name.to_string(), Channel::counter(name));
        self
    }

    /// Add a channel with custom reducer
    pub fn channel_with_reducer(mut self, name: &str, reducer: Reducer) -> Self {
        self.channels.insert(name.to_string(), Channel::new(name).with_reducer(reducer));
        self
    }

    /// Add a channel with default value
    pub fn channel_with_default(mut self, name: &str, default: Value) -> Self {
        self.channels.insert(name.to_string(), Channel::new(name).with_default(default));
        self
    }

    /// Build the schema
    pub fn build(self) -> StateSchema {
        StateSchema { channels: self.channels }
    }
}

/// Checkpoint data structure for persistence
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Thread identifier
    pub thread_id: String,
    /// Unique checkpoint ID
    pub checkpoint_id: String,
    /// State at this checkpoint
    pub state: State,
    /// Step number
    pub step: usize,
    /// Nodes pending execution
    pub pending_nodes: Vec<String>,
    /// Additional metadata
    pub metadata: HashMap<String, Value>,
    /// Creation timestamp
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl Checkpoint {
    /// Create a new checkpoint
    pub fn new(thread_id: &str, state: State, step: usize, pending_nodes: Vec<String>) -> Self {
        Self {
            thread_id: thread_id.to_string(),
            checkpoint_id: uuid::Uuid::new_v4().to_string(),
            state,
            step,
            pending_nodes,
            metadata: HashMap::new(),
            created_at: chrono::Utc::now(),
        }
    }

    /// Add metadata to the checkpoint
    pub fn with_metadata(mut self, key: &str, value: Value) -> Self {
        self.metadata.insert(key.to_string(), value);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_overwrite_reducer() {
        let schema = StateSchema::simple(&["value"]);
        let mut state = State::new();

        schema.apply_update(&mut state, "value", json!(1));
        assert_eq!(state.get("value"), Some(&json!(1)));

        schema.apply_update(&mut state, "value", json!(2));
        assert_eq!(state.get("value"), Some(&json!(2)));
    }

    #[test]
    fn test_append_reducer() {
        let schema = StateSchema::builder().list_channel("messages").build();
        let mut state = schema.initialize_state();

        schema.apply_update(&mut state, "messages", json!({"role": "user", "content": "hi"}));
        assert_eq!(state.get("messages"), Some(&json!([{"role": "user", "content": "hi"}])));

        schema.apply_update(
            &mut state,
            "messages",
            json!([{"role": "assistant", "content": "hello"}]),
        );
        assert_eq!(
            state.get("messages"),
            Some(&json!([
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello"}
            ]))
        );
    }

    #[test]
    fn test_sum_reducer() {
        let schema = StateSchema::builder().counter_channel("count").build();
        let mut state = schema.initialize_state();

        assert_eq!(state.get("count"), Some(&json!(0)));

        schema.apply_update(&mut state, "count", json!(5));
        assert_eq!(state.get("count"), Some(&json!(5.0)));

        schema.apply_update(&mut state, "count", json!(3));
        assert_eq!(state.get("count"), Some(&json!(8.0)));
    }

    #[test]
    fn test_custom_reducer() {
        let schema = StateSchema::builder()
            .channel_with_reducer(
                "max",
                Reducer::Custom(Arc::new(|a, b| {
                    let a_num = a.as_f64().unwrap_or(f64::MIN);
                    let b_num = b.as_f64().unwrap_or(f64::MIN);
                    json!(a_num.max(b_num))
                })),
            )
            .build();
        let mut state = State::new();

        schema.apply_update(&mut state, "max", json!(5));
        schema.apply_update(&mut state, "max", json!(3));
        schema.apply_update(&mut state, "max", json!(8));
        assert_eq!(state.get("max"), Some(&json!(8.0)));
    }
}
