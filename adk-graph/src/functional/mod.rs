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
//! ```rust,ignore
//! use adk_graph::functional::{TaskContext, ReducedValue, MessagesValue};
//! use adk_rust_macros::{entrypoint, task};
//!
//! #[task]
//! async fn fetch_data(ctx: &mut TaskContext) -> Result<String, FunctionalError> {
//!     ctx.emit(serde_json::json!({"status": "fetching"})).await;
//!     Ok("data result".to_string())
//! }
//!
//! #[task(retry(max_attempts = 3, backoff = "1s"))]
//! async fn process(ctx: &mut TaskContext, data: &str) -> Result<String, FunctionalError> {
//!     Ok(format!("processed: {data}"))
//! }
//!
//! #[entrypoint]
//! async fn my_workflow(ctx: &mut TaskContext) -> Result<(), FunctionalError> {
//!     let data = fetch_data(ctx).await?;
//!     let result = process(ctx, &data).await?;
//!     ctx.set("output", serde_json::json!(result));
//!     Ok(())
//! }
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
pub mod schema;
mod task_id;
pub mod typed_reducer;

pub use context::TaskContext;
pub use error::FunctionalError;
pub use execution_log::{ExecutionLog, TaskRecord, TaskStatus};
pub use messages::{ChatMessage, MessageRole, MessagesValue};
pub use reducers::{ReducedValue, UntrackedValue};
pub use schema::{ExpectedType, StateSchemaValidator};
#[doc(hidden)]
pub use task_id::__private;
pub use task_id::task_call_id;
pub use typed_reducer::{AppendReducer, MergeReducer, ReplaceReducer, TypedReducer};
