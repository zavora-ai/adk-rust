//! # adk-graph
#![allow(clippy::result_large_err)]
//!
//! Graph-based workflow orchestration for ADK-Rust agents, inspired by LangGraph.
//!
//! ## Overview
//!
//! `adk-graph` provides a powerful way to build complex, stateful agent workflows
//! using a graph-based approach. It brings LangGraph-style capabilities to the Rust
//! ADK ecosystem while maintaining full compatibility with ADK's agent system,
//! callbacks, and streaming infrastructure.
//!
//! ## Features
//!
//! - **Graph-Based Workflows**: Define agent workflows as directed graphs
//! - **Cyclic Support**: Native support for loops and iterative reasoning
//! - **Conditional Routing**: Dynamic edge routing based on state
//! - **State Management**: Typed state with reducers (overwrite, append, sum, custom)
//! - **Checkpointing**: Persistent state after each step
//! - **Human-in-the-Loop**: Interrupt before/after nodes, dynamic interrupts
//! - **Streaming**: Multiple stream modes (values, updates, messages, debug)
//! - **ADK Integration**: Full callback support, works with existing runners
//! - **Functional API** (`functional` feature): Write workflows as async functions
//!   with `#[entrypoint]`/`#[task]` macros, automatic checkpointing, typed state
//!   reducers ([`ReducedValue`], [`UntrackedValue`], [`MessagesValue`]), state schema
//!   validation, interrupt/resume, and loop iteration checkpoint keying
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use adk_graph::prelude::*;
//!
//! let agent = GraphAgent::builder("processor")
//!     .description("Process data through multiple steps")
//!     .node_fn("fetch", |ctx| async move {
//!         Ok(NodeOutput::new().with_update("data", json!({"items": [1, 2, 3]})))
//!     })
//!     .node_fn("transform", |ctx| async move {
//!         let data = ctx.state.get("data").unwrap();
//!         Ok(NodeOutput::new().with_update("result", data.clone()))
//!     })
//!     .edge(START, "fetch")
//!     .edge("fetch", "transform")
//!     .edge("transform", END)
//!     .build()?;
//!
//! // Execute
//! let result = agent.invoke(State::new(), ExecutionConfig::new("thread_1")).await?;
//! ```
//!
//! ## ReAct Pattern
//!
//! ```rust,ignore
//! use adk_graph::prelude::*;
//!
//! let react_agent = GraphAgent::builder("react")
//!     .node(llm_agent_node)
//!     .node_fn("tools", execute_tools)
//!     .edge(START, "llm")
//!     .conditional_edge(
//!         "llm",
//!         |state| {
//!             if has_tool_calls(state) { "tools" } else { END }
//!         },
//!         [("tools", "tools"), (END, END)],
//!     )
//!     .edge("tools", "llm")  // Cycle back
//!     .recursion_limit(25)
//!     .build()?;
//! ```

pub mod agent;
pub mod checkpoint;
pub mod deferred;
pub mod edge;
pub mod error;
pub mod executor;
pub mod graph;
pub mod interrupt;
pub mod node;
pub mod state;
pub mod stream;
pub mod timeout;

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
pub use checkpoint::{Checkpointer, MemoryCheckpointer};
pub use deferred::{DeferredNodeConfig, FanInTracker, MergeStrategy};
pub use edge::{END, Edge, EdgeTarget, Router, START};
pub use error::{GraphError, InterruptedExecution, Result};
pub use executor::PregelExecutor;
pub use graph::{CompiledGraph, StateGraph};
pub use interrupt::{Interrupt, interrupt, interrupt_with_data};
pub use node::{AgentNode, ExecutionConfig, FunctionNode, Node, NodeContext, NodeOutput};
pub use state::{Channel, Checkpoint, Reducer, State, StateSchema, StateSchemaBuilder};
pub use stream::{StreamEvent, StreamMode};
pub use timeout::{OnTimeout, ProgressHandle, TimeoutPolicy, execute_with_timeout};

#[cfg(feature = "sqlite")]
pub use checkpoint::SqliteCheckpointer;

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
    pub use crate::checkpoint::{Checkpointer, MemoryCheckpointer};
    pub use crate::deferred::{DeferredNodeConfig, FanInTracker, MergeStrategy};
    pub use crate::edge::{END, Edge, EdgeTarget, Router, START};
    pub use crate::error::{GraphError, InterruptedExecution, Result};
    pub use crate::graph::{CompiledGraph, StateGraph};
    pub use crate::interrupt::{Interrupt, interrupt, interrupt_with_data};
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
