//! Interactive mode for Ralph - conversational interface with LLM-powered orchestration.
//!
//! This module provides:
//! - `Session` - Manages conversation state and persistence
//! - `InteractiveRepl` - REPL interface for user interaction
//! - `OrchestratorAgent` - LLM-powered agent that routes requests to appropriate tools
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use adk_ralph::interactive::{InteractiveRepl, Session, OrchestratorAgent};
//! use adk_ralph::RalphConfig;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let config = RalphConfig::from_env()?;
//!     let session = Session::new("./my-project");
//!     
//!     // Create orchestrator agent
//!     let orchestrator = OrchestratorAgent::builder()
//!         .project_path("./my-project")
//!         .config(config)
//!         .build()
//!         .await?;
//!     
//!     // Use the orchestrator agent...
//!     Ok(())
//! }
//! ```

pub mod orchestrator;
pub mod repl;
pub mod session;

// Re-export public API
pub use orchestrator::{OrchestratorAgent, OrchestratorAgentBuilder, REQUIRED_TOOLS};
pub use repl::{InteractiveRepl, InteractiveReplBuilder};
pub use session::{Message, ProjectContext, Session};
