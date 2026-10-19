//! # Functional API for Graph Workflows
//!
//! The Functional API provides a higher-level programming model for `adk-graph`
//! that allows developers to write agent workflows as normal async Rust functions
//! with automatic checkpointing, typed state reducers, and interrupt/resume support.
//!
//! ## Overview
//!
//! Rather than manually constructing nodes, edges, and routers, developers annotate
//! functions with `#[entrypoint]` and `#[task]` macros and use standard Rust control
//! flow (`if`, `for`, `match`, `loop`) to express workflow logic.
//!
//! This module is gated behind the `functional` feature flag.
//!
//! ## Features
//!
//! - **[`TaskContext`]**: Runtime context for tasks providing state, checkpointing, and streaming
//! - **[`ReducedValue<T>`](ReducedValue)**: Append-only state container persisted across checkpoints
//! - **[`UntrackedValue<T>`](UntrackedValue)**: Transient state container excluded from checkpoints
//! - **[`MessagesValue`]**: Chat message container with ID-based deduplication
//! - **[`TypedReducer`]**: Custom merge strategies for typed state values
//! - **[`ExecutionLog`]**: Task completion tracking for resume-skip behavior
//! - **[`StateSchemaValidator`]**: Schema validation for initial state and task output
//!
//! ## Quick Start
//!
//! `#[task]` generates a `__task_<name>` wrapper next to the function. Call the wrapper: it
//! records the call in the run's execution log and checkpoints its result, so a resumed run
//! returns the recorded result instead of running the task again. Calling the function
//! itself runs it with neither. `#[entrypoint]` generates `<Name>Agent`, which runs the
//! workflow against a checkpointer.
//!
//! ```rust
//! use std::sync::Arc;
//!
//! use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer};
//! use adk_graph::error::Result;
//! use adk_graph::functional::TaskContext;
//! use adk_graph::node::ExecutionConfig;
//! use adk_graph::state::State;
//! use adk_rust_macros::{entrypoint, task};
//! use serde_json::{Value, json};
//!
//! #[task]
//! async fn fetch_data(_ctx: &mut TaskContext) -> Result<Value> {
//!     Ok(json!("data result"))
//! }
//!
//! #[task(retry(max_attempts = 3, backoff = "1s"))]
//! async fn process(_ctx: &mut TaskContext, data: &str) -> Result<Value> {
//!     Ok(json!(format!("processed: {data}")))
//! }
//!
//! #[entrypoint]
//! async fn my_workflow(ctx: &mut TaskContext) -> Result<Value> {
//!     let data = __task_fetch_data(ctx).await?;
//!     let result = __task_process(ctx, data.as_str().unwrap_or_default()).await?;
//!     ctx.set("output", result);
//!     Ok(Value::Null)
//! }
//!
//! # #[tokio::main]
//! # async fn main() -> Result<()> {
//! let checkpointer = Arc::new(MemoryCheckpointer::new());
//! let agent = MyWorkflowAgent::new(checkpointer.clone());
//! let state = agent.invoke(State::new(), ExecutionConfig::new("thread-1")).await?;
//! assert_eq!(state["output"], json!("processed: data result"));
//! // The run checkpointed the thread; a resume replays the recorded task results.
//! assert!(checkpointer.load("thread-1").await?.is_some());
//! # Ok(())
//! # }
//! ```
//!
//! ## Typed State Reducers
//!
//! ```rust,ignore
//! use adk_graph::functional::{ReducedValue, UntrackedValue, MessagesValue, ChatMessage, MessageRole};
//!
//! // Append-only accumulator — persisted across checkpoints
//! let mut results: ReducedValue<String> = ReducedValue::default();
//! results.push("step 1 output".to_string());
//! results.push("step 2 output".to_string());
//! assert_eq!(results.len(), 2);
//!
//! // Transient value — excluded from checkpoints
//! let mut temp: UntrackedValue<Vec<u8>> = UntrackedValue::default();
//! temp.set(vec![1, 2, 3]);
//!
//! // Chat messages with deduplication
//! let mut messages = MessagesValue::default();
//! messages.push(ChatMessage {
//!     id: "msg-1".to_string(),
//!     role: MessageRole::User,
//!     content: "Hello".to_string(),
//!     metadata: Default::default(),
//! });
//! ```

mod context;
mod error;
pub mod execution_log;
pub mod messages;
pub mod reducers;
mod resume;
pub mod schema;
mod task_id;
pub mod typed_reducer;

pub use context::TaskContext;
pub use error::FunctionalError;
pub use execution_log::{ExecutionLog, TaskRecord, TaskStatus};
pub use messages::{ChatMessage, MessageRole, MessagesValue};
pub use reducers::{ReducedValue, UntrackedValue};
pub use schema::{ExpectedType, StateSchemaValidator};
/// Items the `#[entrypoint]` and `#[task]` expansions call; not public API.
#[doc(hidden)]
pub mod __private {
    pub use super::resume::load_resume_point;
    pub use super::task_id::__private::*;

    /// Runs `workflow` until it finishes or `token` is cancelled.
    ///
    /// Called by the `#[entrypoint]` expansion, so the generated code needs no
    /// `tokio` macros of its own.
    pub async fn until_cancelled<T>(
        token: &tokio_util::sync::CancellationToken,
        workflow: impl std::future::Future<Output = crate::error::Result<T>>,
    ) -> crate::error::Result<T> {
        tokio::select! {
            biased;
            () = token.cancelled() => Err(crate::error::GraphError::Cancelled),
            result = workflow => result,
        }
    }
}
pub use task_id::task_call_id;
pub use typed_reducer::{AppendReducer, MergeReducer, ReplaceReducer, TypedReducer};
