//! The harness's own tools (spec 3.1 §7.4, §7.6, §7.9).

#[cfg(feature = "memory")]
mod facts;
#[cfg(feature = "mcp")]
mod mcp;
#[cfg(feature = "memory")]
mod memory;
mod plan;
mod shell;
mod todos;
#[cfg(feature = "web")]
mod web;

#[cfg(feature = "memory")]
pub use facts::{MemoryRememberTool, MemorySearchTool};
#[cfg(feature = "mcp")]
pub use mcp::{McpListResourcesTool, McpReadResourceTool};
#[cfg(feature = "memory")]
pub(crate) use memory::MemoryResolver;
#[cfg(feature = "memory")]
pub use memory::{MemoryDeleteTool, MemoryReadTool, MemoryWriteTool};
pub use plan::{ExitPlanModeTool, PLAN_STATE_KEY, PlanGate, PlanStep};
pub use shell::{GOVERNANCE_POLL_INTERVAL, HarnessBashTool};
pub use todos::{
    MAX_TODO_CHARS, MAX_TODO_ITEMS, TODOS_STATE_KEY, TodoItem, TodoList, TodoStatus,
    UpdateTodoTool, WriteTodosTool,
};
#[cfg(feature = "web")]
pub use web::{WEB_TURN_CAP_CODE, WebFetchTool, WebSearchTool};
