//! The `write_todos` planning tool.

use std::sync::{Arc, Mutex};

use adk_core::{Result, Tool, ToolContext, ToolEffect};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The session state key that holds a session's todo list (spec 3.1 §7.6).
///
/// The value is `{ "items": [{ "id", "content", "status" }], "revision" }`, where `id`
/// is the item's 1-based position as a string and `revision` counts the session's
/// writes. UIs read it from `event.actions.state_delta["harness:todos"]` or the
/// session's state.
///
/// # Example
///
/// ```rust
/// use adk_agent::coding::TODOS_STATE_KEY;
///
/// assert_eq!(TODOS_STATE_KEY, "harness:todos");
/// ```
pub const TODOS_STATE_KEY: &str = "harness:todos";

/// A single task in the agent's plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TodoItem {
    /// What the step does.
    pub content: String,
    /// One of `pending`, `in_progress`, `completed`.
    #[serde(default = "default_status")]
    pub status: String,
}

fn default_status() -> String {
    "pending".to_string()
}

/// A planning tool: the model calls `write_todos` to record and update a short
/// task list.
///
/// Each call replaces the list of the session it runs in: the tool writes it to
/// session state under [`TODOS_STATE_KEY`] through the call's state delta, so
/// sessions sharing one agent keep separate plans and a plan survives with its
/// session. The tool is [`ToolEffect::Idempotent`]: repeating a call writes the
/// same list. Implements spec 3.1 §10 A1.
///
/// [`items`](Self::items) returns the last list any session wrote through this tool
/// in this process, a convenience for single-session programs; read a session's
/// own list from its state. Cloning a `TodoTool` shares that last-seen list.
#[derive(Clone, Default)]
pub struct TodoTool {
    last_seen: Arc<Mutex<Vec<TodoItem>>>,
}

impl TodoTool {
    /// Create a new, empty todo tool.
    pub fn new() -> Self {
        Self::default()
    }

    /// The last list any session wrote through this tool in this process.
    ///
    /// Sessions are not told apart; read [`TODOS_STATE_KEY`] from a session's
    /// state for that session's list.
    pub fn items(&self) -> Vec<TodoItem> {
        self.last_seen.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

#[async_trait]
impl Tool for TodoTool {
    fn name(&self) -> &str {
        "write_todos"
    }

    fn description(&self) -> &str {
        "Record or update your task plan. Pass the full list of todos each time; \
         it replaces the previous list. Each todo has `content` and a `status` of \
         'pending', 'in_progress', or 'completed'. Keep exactly one item \
         'in_progress' while you work."
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Idempotent
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "The full, updated task list.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": { "type": "string", "description": "What this step does." },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed"],
                                "description": "Current status."
                            }
                        },
                        "required": ["content", "status"]
                    }
                }
            },
            "required": ["todos"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let todos = args
            .get("todos")
            .cloned()
            .ok_or_else(|| adk_core::AdkError::tool("missing required argument 'todos'"))?;
        let parsed: Vec<TodoItem> = serde_json::from_value(todos)
            .map_err(|e| adk_core::AdkError::tool(format!("invalid todos: {e}")))?;

        let revision = ctx
            .session()
            .map(|session| session.state())
            .and_then(|state| state.get(TODOS_STATE_KEY))
            .and_then(|stored| stored.get("revision").and_then(Value::as_u64))
            .unwrap_or(0)
            + 1;
        let items: Vec<Value> = parsed
            .iter()
            .enumerate()
            .map(|(index, todo)| {
                json!({
                    "id": (index + 1).to_string(),
                    "content": todo.content,
                    "status": todo.status,
                })
            })
            .collect();
        let mut actions = ctx.actions();
        actions
            .state_delta
            .insert(TODOS_STATE_KEY.to_string(), json!({ "items": items, "revision": revision }));
        ctx.set_actions(actions);

        if let Ok(mut guard) = self.last_seen.lock() {
            *guard = parsed.clone();
        }

        let remaining = parsed.iter().filter(|t| t.status != "completed").count();
        Ok(json!({
            "todos": parsed,
            "total": parsed.len(),
            "remaining": remaining,
            "message": format!("Plan updated: {} item(s), {remaining} remaining.", parsed.len()),
        }))
    }
}
