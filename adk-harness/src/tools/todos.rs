//! `write_todos` and `update_todo` (spec 3.1 §7.6).
//!
//! Todos are display state, so they live in session state under [`TODOS_STATE_KEY`]:
//! each session has its own list, unlike the 3.0 `TodoTool`, whose list was shared by
//! every session of the process.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use adk_core::{
    AdkError, ErrorCategory, ErrorComponent, Result, State, Tool, ToolContext, ToolEffect,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Session-state key of the todo list (spec 3.1 §7.6).
///
/// UIs read `event.actions.state_delta["harness:todos"]`.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::TODOS_STATE_KEY, "harness:todos");
/// ```
pub const TODOS_STATE_KEY: &str = "harness:todos";
/// Most items a todo list holds (spec 3.1 §7.6).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::MAX_TODO_ITEMS, 50);
/// ```
pub const MAX_TODO_ITEMS: usize = 50;
/// Most characters of one todo (spec 3.1 §7.6).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::MAX_TODO_CHARS, 500);
/// ```
pub const MAX_TODO_CHARS: usize = 500;

/// Where a todo stands (spec 3.1 §7.6).
///
/// # Example
///
/// ```rust
/// assert_eq!(serde_json::to_value(adk_harness::TodoStatus::InProgress).unwrap(), "in_progress");
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    /// Not started.
    #[default]
    Pending,
    /// Being worked on.
    InProgress,
    /// Done.
    Completed,
    /// Dropped.
    Cancelled,
}

impl TodoStatus {
    /// Whether the item still needs work.
    pub fn is_open(self) -> bool {
        matches!(self, Self::Pending | Self::InProgress)
    }
}

/// One todo (spec 3.1 §7.6).
///
/// # Example
///
/// ```rust
/// use adk_harness::{TodoItem, TodoStatus};
///
/// let item = TodoItem { id: "1".into(), content: "Run the tests".into(), active_form: None, status: TodoStatus::Pending };
/// assert_eq!(serde_json::to_value(&item).unwrap()["status"], "pending");
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoItem {
    /// Stable id within the list.
    pub id: String,
    /// What to do.
    pub content: String,
    /// Present-continuous form shown while the item is in progress.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
    /// Where it stands.
    pub status: TodoStatus,
}

/// A session's todo list (spec 3.1 §7.6).
///
/// # Example
///
/// ```rust
/// use adk_harness::TodoList;
/// use serde_json::json;
///
/// let list = TodoList::from_value(&json!({ "items": [], "revision": 3 }));
/// assert_eq!(list.revision, 3);
/// assert_eq!(TodoList::from_value(&json!("garbage")), TodoList::default());
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoList {
    /// The items, in order.
    pub items: Vec<TodoItem>,
    /// Incremented on every write.
    pub revision: u64,
}

impl TodoList {
    /// Reads a stored list; anything unparseable is an empty list.
    pub fn from_value(value: &Value) -> Self {
        serde_json::from_value(value.clone()).unwrap_or_default()
    }

    /// The list stored in `state`, or an empty one.
    pub fn from_state(state: &dyn State) -> Self {
        state.get(TODOS_STATE_KEY).map(|value| Self::from_value(&value)).unwrap_or_default()
    }

    /// The items still open.
    pub fn open_items(&self) -> impl Iterator<Item = &TodoItem> {
        self.items.iter().filter(|item| item.status.is_open())
    }
}

fn invalid(message: impl Into<String>) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::InvalidInput,
        "harness.todos.invalid",
        message,
    )
}

/// Most invocations whose written lists are kept; the oldest is dropped first.
const WRITTEN_CAP: usize = 1024;

/// Lists the todo tools wrote, by invocation. Session state shows a call's write only once
/// its batch ends, so a later call in the same batch reads the list from here.
#[derive(Default)]
struct Written {
    lists: HashMap<String, TodoList>,
    order: VecDeque<String>,
}

static WRITTEN: Mutex<Option<Written>> = Mutex::new(None);

/// Reads the session's latest list, applies `change`, and writes the result, as one step
/// per invocation, so no call in a batch overwrites another's update (spec 3.1 §7.6).
fn update(
    ctx: &dyn ToolContext,
    last: &Option<Arc<Mutex<TodoList>>>,
    change: impl FnOnce(TodoList) -> Result<TodoList>,
) -> Result<TodoList> {
    let key = format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}",
        ctx.app_name(),
        ctx.user_id(),
        ctx.session_id(),
        ctx.invocation_id()
    );
    let mut written = WRITTEN.lock().unwrap_or_else(|e| e.into_inner());
    let written = written.get_or_insert_with(Written::default);
    let stored =
        ctx.session().map(|session| TodoList::from_state(session.state())).unwrap_or_default();
    let current = match written.lists.get(&key) {
        Some(list) if list.revision > stored.revision => list.clone(),
        _ => stored,
    };
    let list = change(current)?;
    let mut actions = ctx.actions();
    actions.state_delta.insert(
        TODOS_STATE_KEY.to_string(),
        serde_json::to_value(&list)
            .map_err(|error| invalid(format!("serialize todos: {error}")))?,
    );
    ctx.set_actions(actions);
    if written.lists.insert(key.clone(), list.clone()).is_none() {
        written.order.push_back(key);
        while written.order.len() > WRITTEN_CAP {
            if let Some(oldest) = written.order.pop_front() {
                written.lists.remove(&oldest);
            }
        }
    }
    if let Some(last) = last {
        *last.lock().unwrap_or_else(|e| e.into_inner()) = list.clone();
    }
    Ok(list)
}

/// The tool response: the stored list's shape (spec 3.1 §7.6).
fn summary(list: &TodoList) -> Value {
    json!({ "revision": list.revision, "todos": list.items })
}

#[derive(Deserialize)]
struct TodoArg {
    #[serde(default)]
    id: Option<String>,
    content: String,
    #[serde(default, rename = "activeForm", alias = "active_form")]
    active_form: Option<String>,
    #[serde(default)]
    status: TodoStatus,
}

/// `write_todos`: replaces the session's todo list (spec 3.1 §7.6).
///
/// # Example
///
/// ```rust
/// use adk_core::Tool;
///
/// let tool = adk_harness::WriteTodosTool::new();
/// assert_eq!(tool.name(), "write_todos");
/// assert_eq!(tool.effect(), adk_core::ToolEffect::Idempotent);
/// ```
#[derive(Clone, Default)]
pub struct WriteTodosTool {
    last: Option<Arc<Mutex<TodoList>>>,
}

impl WriteTodosTool {
    /// A `write_todos` tool.
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn with_last(last: Arc<Mutex<TodoList>>) -> Self {
        Self { last: Some(last) }
    }
}

#[async_trait]
impl Tool for WriteTodosTool {
    fn name(&self) -> &str {
        "write_todos"
    }

    fn description(&self) -> &str {
        "Replace this session's todo list with a short plan of the task. Use it for any \
         task with more than two steps, mark exactly one item in_progress while you work \
         on it, and keep it current. At most 50 items of 500 characters."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "The complete list, in order.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string", "description": "Stable id; defaults to the position." },
                            "content": { "type": "string", "description": "What to do." },
                            "activeForm": { "type": "string", "description": "Shown while in progress, e.g. 'Running the tests'." },
                            "status": { "type": "string", "enum": ["pending", "in_progress", "completed", "cancelled"] }
                        },
                        "required": ["content"]
                    }
                }
            },
            "required": ["todos"]
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Idempotent
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let todos = args.get("todos").cloned().unwrap_or(Value::Null);
        let parsed: Vec<TodoArg> = serde_json::from_value(todos).map_err(|error| {
            invalid(format!("todos must be a list of {{content, status}}: {error}"))
        })?;
        if parsed.len() > MAX_TODO_ITEMS {
            return Err(invalid(format!("at most {MAX_TODO_ITEMS} todos; got {}", parsed.len())));
        }
        let mut seen = HashSet::new();
        let mut items = Vec::with_capacity(parsed.len());
        for (index, todo) in parsed.into_iter().enumerate() {
            let content = todo.content.trim().to_string();
            if content.is_empty() || content.chars().count() > MAX_TODO_CHARS {
                return Err(invalid(format!(
                    "todo {} must have 1 to {MAX_TODO_CHARS} characters",
                    index + 1
                )));
            }
            if todo.active_form.as_ref().is_some_and(|form| form.chars().count() > MAX_TODO_CHARS) {
                return Err(invalid(format!("todo {} activeForm is too long", index + 1)));
            }
            let id = todo
                .id
                .filter(|id| !id.trim().is_empty())
                .unwrap_or_else(|| (index + 1).to_string());
            if !seen.insert(id.clone()) {
                return Err(invalid(format!("duplicate todo id '{id}'")));
            }
            items.push(TodoItem {
                id,
                content,
                active_form: todo.active_form,
                status: todo.status,
            });
        }
        let list = update(ctx.as_ref(), &self.last, |current| {
            Ok(TodoList { items, revision: current.revision + 1 })
        })?;
        Ok(summary(&list))
    }
}

/// `update_todo`: sets one todo's status (spec 3.1 §7.6).
///
/// # Example
///
/// ```rust
/// use adk_core::Tool;
///
/// assert_eq!(adk_harness::UpdateTodoTool::new().name(), "update_todo");
/// ```
#[derive(Clone, Default)]
pub struct UpdateTodoTool {
    last: Option<Arc<Mutex<TodoList>>>,
}

impl UpdateTodoTool {
    /// An `update_todo` tool.
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn with_last(last: Arc<Mutex<TodoList>>) -> Self {
        Self { last: Some(last) }
    }
}

#[async_trait]
impl Tool for UpdateTodoTool {
    fn name(&self) -> &str {
        "update_todo"
    }

    fn description(&self) -> &str {
        "Set the status of one todo in this session's list by id."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "id": { "type": "string" },
                "status": { "type": "string", "enum": ["pending", "in_progress", "completed", "cancelled"] }
            },
            "required": ["id", "status"]
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Idempotent
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let id = args.get("id").and_then(Value::as_str).ok_or_else(|| invalid("id is required"))?;
        let status: TodoStatus = serde_json::from_value(
            args.get("status").cloned().unwrap_or(Value::Null),
        )
        .map_err(|_| invalid("status must be pending, in_progress, completed or cancelled"))?;
        let list = update(ctx.as_ref(), &self.last, |mut list| {
            let item = list.items.iter_mut().find(|item| item.id == id).ok_or_else(|| {
                invalid(format!("no todo with id '{id}'; call write_todos first"))
            })?;
            item.status = status;
            list.revision += 1;
            Ok(list)
        })?;
        Ok(summary(&list))
    }
}
