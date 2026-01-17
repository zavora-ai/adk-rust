//! # adk-ralph
//!
//! Ralph is a multi-agent autonomous development system that transforms a user's idea
//! into a fully implemented project. It uses three specialized agents working in sequence:
//!
//! 1. **PRD Agent** - Creates structured requirements from user prompts
//! 2. **Architect Agent** - Creates system design and task breakdown from PRD
//! 3. **Ralph Loop Agent** - Iteratively implements tasks until completion
//!
//! ## Features
//!
//! - **Multi-Agent Pipeline**: Three specialized agents for requirements, design, and implementation
//! - **Priority-Based Task Selection**: Implements highest priority tasks first with dependency checking
//! - **Progress Tracking**: Append-only progress log captures learnings and gotchas
//! - **Test-Before-Commit**: Only commits code that passes tests
//! - **Multi-Language Support**: Rust, Python, TypeScript, Go, Java
//! - **Telemetry Integration**: Full observability with OpenTelemetry
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use adk_ralph::{RalphConfig, RalphOrchestrator};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let config = RalphConfig::from_env()?;
//!     let orchestrator = RalphOrchestrator::new(config)?;
//!     
//!     orchestrator.run("Create a CLI calculator in Rust").await?;
//!     Ok(())
//! }
//! ```
//!
//! ## Architecture
//!
//! ```text
//! User Prompt → PRD Agent → prd.md
//!                 ↓
//!            Architect Agent → design.md + tasks.json
//!                 ↓
//!            Ralph Loop Agent → Implementation + Tests + Commits
//!                 ↓
//!            Completion Promise
//! ```

pub mod agents;
pub mod error;
pub mod interactive;
pub mod models;
pub mod orchestrator;
pub mod output;
pub mod telemetry;
pub mod tools;

// Re-export main types for convenience
pub use error::{RalphError, Result};
pub use models::{
    // Config types
    AgentModelConfig,
    DebugLevel,
    ModelConfig,
    RalphConfig,
    RalphConfigBuilder,
    TelemetryConfig,
    ValidationError,
    MAX_ITERATIONS_LIMIT,
    MAX_RETRIES_LIMIT,
    MAX_TOKENS_LIMIT,
    SUPPORTED_PROVIDERS,
    // PRD types
    AcceptanceCriterion,
    PrdDocument,
    PrdStats,
    UserStory,
    // Design types
    Component,
    DesignDocument,
    FileStructure,
    TechnologyStack,
    // Task types
    Phase,
    Sprint,
    Task,
    TaskList,
    TaskStatus,
    // Progress types
    ProgressEntry,
    ProgressLog,
    ProgressSummary,
    TestResults,
};

// Re-export tools
pub use tools::{
    // Core tools
    FileTool, GitTool, ProgressTool, TaskTool, TestTool,
    // Interactive mode tools
    AddFeatureMode, AddFeatureTool, GetTimeTool, Language, RunPipelineTool, RunProjectTool,
    SearchResult, WebSearchTool,
};

// Re-export agents
pub use agents::{ArchitectAgent, CompletionStatus, PrdAgent, PrdAgentBuilder, RalphLoopAgent, RalphLoopAgentBuilder};

// Re-export orchestrator
pub use orchestrator::{OrchestratorBuilder, OrchestratorState, PipelinePhase, RalphOrchestrator};

// Re-export interactive mode
pub use interactive::{InteractiveRepl, InteractiveReplBuilder, Message, OrchestratorAgent, OrchestratorAgentBuilder, ProjectContext, Session, REQUIRED_TOOLS};

// Re-export output
pub use output::{RalphOutput, process_event_part};
