//! # adk-graph
//!
//! Graph-based workflow orchestration for ADK-Rust agents, inspired by LangGraph.
//!
//! ## Overview
//!
//! `adk-graph` builds stateful agent workflows as graphs of nodes and edges. A
//! [`GraphAgent`] implements the ADK `Agent` trait, so a graph runs under a `Runner`
//! and streams through it like any other agent.
//!
//! ## Features
//!
//! - **Graph-Based Workflows**: Define agent workflows as directed graphs
//! - **Cyclic Support**: Native support for loops and iterative reasoning
//! - **Conditional Routing**: Dynamic edge routing based on state, or a goto from inside a node
//! - **Fan-out / fan-in**: Parallel branches in one super-step; a node with several
//!   incoming edges waits for the predecessors that can still reach it
//! - **State Management**: Typed state with reducers (overwrite, append, sum, custom)
//! - **Checkpointing**: Persistent, versioned state after each step, with memory, SQLite,
//!   PostgreSQL, Redis, MongoDB, Neo4j, and Firestore backends
//! - **Human-in-the-Loop**: Interrupt before/after nodes, dynamic interrupts, and
//!   [`NodeContext::resume_value`] for the answer to a pause
//! - **Streaming**: Multiple stream modes (values, updates, messages, custom, debug)
//! - **ADK Integration**: [`AgentNode`] runs any ADK agent as a node, and an agent's failure is
//!   [`GraphError::AgentFailed`] with the agent's own `AdkError`
//! - **Functional API** (`functional` feature): Write workflows as async functions
//!   with `#[entrypoint]`/`#[task]` macros, automatic checkpointing, typed state
//!   reducers (ReducedValue, UntrackedValue, MessagesValue), state schema
//!   validation, interrupt/resume, and loop iteration checkpoint keying
//!
//! ## Quick Start
//!
//! ```rust
//! use adk_graph::prelude::*;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> adk_graph::Result<()> {
//! let agent = GraphAgent::builder("processor")
//!     .description("Process data through multiple steps")
//!     .channels(&["data", "result"])
//!     .node_fn("fetch", |_ctx| async move {
//!         Ok(NodeOutput::new().with_update("data", json!({"items": [1, 2, 3]})))
//!     })
//!     .node_fn("transform", |ctx| async move {
//!         let data = ctx.get("data").cloned().unwrap_or_default();
//!         Ok(NodeOutput::new().with_update("result", data))
//!     })
//!     .edge(START, "fetch")
//!     .edge("fetch", "transform")
//!     .edge("transform", END)
//!     .build()?;
//!
//! // Execute
//! let result = agent.invoke(State::new(), ExecutionConfig::new("thread_1")).await?;
//! assert_eq!(result["result"], json!({"items": [1, 2, 3]}));
//! # Ok(())
//! # }
//! ```
//!
//! ## Loops and Conditional Edges
//!
//! ```rust
//! use adk_graph::prelude::*;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> adk_graph::Result<()> {
//! let agent = GraphAgent::builder("counter")
//!     .channels(&["count"])
//!     .node_fn("step", |ctx| async move {
//!         let count = ctx.get("count").and_then(Value::as_i64).unwrap_or(0);
//!         Ok(NodeOutput::new().with_update("count", json!(count + 1)))
//!     })
//!     .edge(START, "step")
//!     .conditional_edge(
//!         "step",
//!         |state: &State| {
//!             let count = state.get("count").and_then(Value::as_i64).unwrap_or(0);
//!             if count < 3 { "step".to_string() } else { END.to_string() }
//!         },
//!         [("step", "step"), (END, END)], // Cycle back until the count reaches 3
//!     )
//!     .build()?;
//!
//! let result = agent.invoke(State::new(), ExecutionConfig::new("thread_2")).await?;
//! assert_eq!(result["count"], json!(3));
//! # Ok(())
//! # }
//! ```

#![allow(clippy::result_large_err)]

pub mod agent;
pub mod checkpoint;
pub mod checkpointers;
pub mod child;
#[cfg(feature = "conformance")]
pub mod conformance;
pub mod deferred;
pub mod edge;
pub mod error;
pub mod executor;
pub mod graph;
pub mod interrupt;
pub mod node;
pub mod retry;
pub mod state;
pub mod stream;
pub mod subgraph;
pub mod timeout;
pub mod tool;

#[cfg(feature = "node-cache")]
pub mod cache;

#[cfg(feature = "delta-checkpoint")]
pub mod delta;

#[cfg(feature = "time-travel")]
pub mod time_travel;

#[cfg(feature = "action")]
pub mod action;
#[cfg(feature = "action")]
pub mod workflow;

#[cfg(feature = "functional")]
pub mod functional;

// Functional API re-exports for convenient access
#[cfg(feature = "functional")]
pub use functional::schema::{ExpectedType, StateSchemaValidator};
#[cfg(feature = "functional")]
pub use functional::{
    AppendReducer, ExecutionLog, FunctionalError, MergeReducer, MessagesValue, ReducedValue,
    ReplaceReducer, TaskContext, TypedReducer, UntrackedValue,
};

// Re-exports
pub use agent::{GraphAgent, GraphAgentBuilder};
pub use checkpoint::{Checkpointer, Durability, MemoryCheckpointer, RetentionPolicy};
pub use deferred::{DeferredNodeConfig, FanInTracker, MergeStrategy};
pub use edge::{END, Edge, EdgeTarget, Router, START};
pub use error::{GraphError, InterruptedExecution, Result};
pub use executor::PregelExecutor;
pub use graph::{CompiledGraph, StateGraph};
pub use interrupt::{GraphToolConfirmationPause, Interrupt, interrupt, interrupt_with_data};
pub use node::{AgentNode, ExecutionConfig, FunctionNode, Node, NodeContext, NodeOutput};
pub use state::{
    Channel, Checkpoint, PendingWrite, Reducer, State, StateSchema, StateSchemaBuilder,
};
pub use stream::{StreamEvent, StreamMode};
pub use timeout::{OnTimeout, ProgressHandle, TimeoutPolicy, execute_with_timeout};

#[cfg(feature = "sqlite")]
pub use checkpoint::SqliteCheckpointer;
#[cfg(feature = "firestore")]
pub use checkpointers::firestore::FirestoreCheckpointer;
#[cfg(feature = "mongodb")]
pub use checkpointers::mongodb::MongoCheckpointer;
#[cfg(feature = "neo4j")]
pub use checkpointers::neo4j::Neo4jCheckpointer;
#[cfg(feature = "postgres")]
pub use checkpointers::postgres::PostgresCheckpointer;
#[cfg(feature = "redis")]
pub use checkpointers::redis::RedisCheckpointer;

#[cfg(feature = "node-cache")]
pub use cache::{CacheBackend, NodeCache, NodeCachePolicy, compute_cache_key};

#[cfg(feature = "delta-checkpoint")]
pub use delta::{
    CheckpointType, DeltaCheckpointer, DeltaConfig, Diff, MapDelta, StringDelta, StringOp, VecDelta,
};

#[cfg(feature = "time-travel")]
pub use time_travel::{StepInfo, TimeTravelHandle};

/// Prelude module for convenient imports
pub mod prelude {
    pub use crate::agent::{GraphAgent, GraphAgentBuilder};
    pub use crate::checkpoint::{Checkpointer, Durability, MemoryCheckpointer};
    pub use crate::deferred::{DeferredNodeConfig, FanInTracker, MergeStrategy};
    pub use crate::edge::{END, Edge, EdgeTarget, Router, START};
    pub use crate::error::{GraphError, InterruptedExecution, Result};
    pub use crate::graph::{CompiledGraph, StateGraph};
    pub use crate::interrupt::{
        GraphToolConfirmationPause, Interrupt, interrupt, interrupt_with_data,
    };
    pub use crate::node::{
        AgentNode, ExecutionConfig, FunctionNode, Node, NodeContext, NodeOutput,
    };
    pub use crate::state::{Channel, Checkpoint, Reducer, State, StateSchema, StateSchemaBuilder};
    pub use crate::stream::{StreamEvent, StreamMode};

    #[cfg(feature = "sqlite")]
    pub use crate::checkpoint::SqliteCheckpointer;

    #[cfg(feature = "action")]
    pub use crate::action::ActionNodeExecutor;

    // Re-export commonly used serde_json
    pub use serde_json::{Value, json};
}
