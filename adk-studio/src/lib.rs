//! ADK Studio - Visual development environment for ADK-Rust agents
//!
//! Build-only architecture: Users create agents in UI, build to binary, run compiled code.

pub mod codegen;
pub mod embedded;
pub mod schema;
pub mod server;
pub mod storage;

pub use schema::{AgentSchema, ProjectSchema, ToolSchema, WorkflowSchema};
pub use server::{AppState, api_routes};
pub use storage::FileStorage;
