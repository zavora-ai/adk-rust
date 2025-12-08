//! Edge types for graph control flow
//!
//! Edges define how execution flows between nodes.

use crate::state::State;
use std::collections::HashMap;
use std::sync::Arc;

/// Special node identifiers
pub const START: &str = "__start__";
pub const END: &str = "__end__";

/// Target of an edge
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EdgeTarget {
    /// Specific node
    Node(String),
    /// End of graph
    End,
}

impl EdgeTarget {
    /// Check if this is the END target
    pub fn is_end(&self) -> bool {
        matches!(self, Self::End)
    }

    /// Get the node name if this is a Node target
    pub fn node_name(&self) -> Option<&str> {
        match self {
            Self::Node(name) => Some(name),
            Self::End => None,
        }
    }
}

impl From<&str> for EdgeTarget {
    fn from(s: &str) -> Self {
        if s == END {
            Self::End
        } else {
            Self::Node(s.to_string())
        }
    }
}

/// Router function type
pub type RouterFn = Arc<dyn Fn(&State) -> String + Send + Sync>;

/// Edge type
#[derive(Clone)]
pub enum Edge {
    /// Direct edge: always go from source to target
    Direct { source: String, target: EdgeTarget },

    /// Conditional edge: route based on state
    Conditional {
        source: String,
        /// Router function returns target node name or END
        router: RouterFn,
        /// Map of route names to targets (for validation and documentation)
        targets: HashMap<String, EdgeTarget>,
    },

    /// Entry edge: from START to first node(s)
    Entry { targets: Vec<String> },
}

impl std::fmt::Debug for Edge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Direct { source, target } => {
                f.debug_struct("Direct").field("source", source).field("target", target).finish()
            }
            Self::Conditional { source, targets, .. } => f
                .debug_struct("Conditional")
                .field("source", source)
                .field("targets", targets)
                .finish(),
            Self::Entry { targets } => f.debug_struct("Entry").field("targets", targets).finish(),
        }
    }
}

/// Router helper functions for common patterns
pub struct Router;

impl Router {
    /// Route based on a state field value
    ///
    /// # Example
    /// ```ignore
    /// .conditional_edge("supervisor", Router::by_field("next_agent"), targets)
    /// ```
    pub fn by_field(field: &str) -> impl Fn(&State) -> String + Send + Sync + Clone {
        let field = field.to_string();
        move |state: &State| state.get(&field).and_then(|v| v.as_str()).unwrap_or(END).to_string()
    }

    /// Route based on whether the last message has tool calls
    ///
    /// # Example
    /// ```ignore
    /// .conditional_edge("agent", Router::has_tool_calls("messages", "tools", END), targets)
    /// ```
    pub fn has_tool_calls(
        messages_field: &str,
        if_true: &str,
        if_false: &str,
    ) -> impl Fn(&State) -> String + Send + Sync + Clone {
        let messages_field = messages_field.to_string();
        let if_true = if_true.to_string();
        let if_false = if_false.to_string();

        move |state: &State| {
            let has_calls = state
                .get(&messages_field)
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.last())
                .and_then(|msg| msg.get("tool_calls"))
                .map(|tc| !tc.as_array().map(|a| a.is_empty()).unwrap_or(true))
                .unwrap_or(false);

            if has_calls {
                if_true.clone()
            } else {
                if_false.clone()
            }
        }
    }

    /// Route based on a boolean state field
    ///
    /// # Example
    /// ```ignore
    /// .conditional_edge("check", Router::by_bool("should_continue", "process", END), targets)
    /// ```
    pub fn by_bool(
        field: &str,
        if_true: &str,
        if_false: &str,
    ) -> impl Fn(&State) -> String + Send + Sync + Clone {
        let field = field.to_string();
        let if_true = if_true.to_string();
        let if_false = if_false.to_string();

        move |state: &State| {
            let is_true = state.get(&field).and_then(|v| v.as_bool()).unwrap_or(false);

            if is_true {
                if_true.clone()
            } else {
                if_false.clone()
            }
        }
    }

    /// Route based on iteration count
    ///
    /// # Example
    /// ```ignore
    /// .conditional_edge("loop", Router::max_iterations("iteration", 5, "continue", "done"), targets)
    /// ```
    pub fn max_iterations(
        counter_field: &str,
        max: usize,
        continue_target: &str,
        done_target: &str,
    ) -> impl Fn(&State) -> String + Send + Sync + Clone {
        let counter_field = counter_field.to_string();
        let continue_target = continue_target.to_string();
        let done_target = done_target.to_string();

        move |state: &State| {
            let count = state.get(&counter_field).and_then(|v| v.as_u64()).unwrap_or(0) as usize;

            if count < max {
                continue_target.clone()
            } else {
                done_target.clone()
            }
        }
    }

    /// Route based on the presence of an error
    ///
    /// # Example
    /// ```ignore
    /// .conditional_edge("process", Router::on_error("error", "error_handler", "success"), targets)
    /// ```
    pub fn on_error(
        error_field: &str,
        error_target: &str,
        success_target: &str,
    ) -> impl Fn(&State) -> String + Send + Sync + Clone {
        let error_field = error_field.to_string();
        let error_target = error_target.to_string();
        let success_target = success_target.to_string();

        move |state: &State| {
            let has_error = state.get(&error_field).map(|v| !v.is_null()).unwrap_or(false);

            if has_error {
                error_target.clone()
            } else {
                success_target.clone()
            }
        }
    }

    /// Create a custom router from a closure
    ///
    /// # Example
    /// ```ignore
    /// .conditional_edge("agent", Router::custom(|state| {
    ///     if state.get("done").and_then(|v| v.as_bool()).unwrap_or(false) {
    ///         END.to_string()
    ///     } else {
    ///         "continue".to_string()
    ///     }
    /// }), targets)
    /// ```
    pub fn custom<F>(f: F) -> impl Fn(&State) -> String + Send + Sync + Clone
    where
        F: Fn(&State) -> String + Send + Sync + Clone + 'static,
    {
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_by_field_router() {
        let router = Router::by_field("next");

        let mut state = State::new();
        state.insert("next".to_string(), json!("agent_a"));
        assert_eq!(router(&state), "agent_a");

        state.insert("next".to_string(), json!("agent_b"));
        assert_eq!(router(&state), "agent_b");

        // Missing field returns END
        let empty_state = State::new();
        assert_eq!(router(&empty_state), END);
    }

    #[test]
    fn test_has_tool_calls_router() {
        let router = Router::has_tool_calls("messages", "tools", END);

        // No messages
        let state = State::new();
        assert_eq!(router(&state), END);

        // Messages without tool calls
        let mut state = State::new();
        state.insert("messages".to_string(), json!([{"role": "assistant", "content": "Hello"}]));
        assert_eq!(router(&state), END);

        // Messages with tool calls
        let mut state = State::new();
        state.insert(
            "messages".to_string(),
            json!([{"role": "assistant", "tool_calls": [{"name": "search"}]}]),
        );
        assert_eq!(router(&state), "tools");
    }

    #[test]
    fn test_by_bool_router() {
        let router = Router::by_bool("should_continue", "continue", "stop");

        let mut state = State::new();
        state.insert("should_continue".to_string(), json!(true));
        assert_eq!(router(&state), "continue");

        state.insert("should_continue".to_string(), json!(false));
        assert_eq!(router(&state), "stop");
    }

    #[test]
    fn test_max_iterations_router() {
        let router = Router::max_iterations("count", 3, "loop", "done");

        let mut state = State::new();
        state.insert("count".to_string(), json!(0));
        assert_eq!(router(&state), "loop");

        state.insert("count".to_string(), json!(2));
        assert_eq!(router(&state), "loop");

        state.insert("count".to_string(), json!(3));
        assert_eq!(router(&state), "done");
    }

    #[test]
    fn test_edge_target_from_str() {
        assert_eq!(EdgeTarget::from("node_a"), EdgeTarget::Node("node_a".to_string()));
        assert_eq!(EdgeTarget::from(END), EdgeTarget::End);
    }
}
