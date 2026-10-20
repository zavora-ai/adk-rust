//! Team members (spec 3.1 §7.10, feature `team`).
//!
//! These types fix the shape of `HarnessAgentBuilder::member`. Lowering members to a
//! `TeamSpec` lands with spec 3.1 §7.10; until then a harness with members refuses to
//! build with `not yet implemented: spec 3.1 §7.10`.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{Agent, Llm, Tool};
use serde_json::Value;

/// One member of the harness team (spec 3.1 §7.10).
///
/// # Example
///
/// ```rust
/// let explore = adk_harness::MemberSpec::explore();
/// assert_eq!(explore.name, "explore");
/// assert_eq!(explore.max_iterations, 50);
/// ```
#[derive(Clone)]
pub struct MemberSpec {
    /// Member name.
    pub name: String,
    /// When to use it (model-facing).
    pub description: String,
    /// What the member is.
    pub kind: MemberKind,
    /// The member's tools.
    pub tools: MemberTools,
    /// Whether the member sees skills.
    pub skills: bool,
    /// Most model turns.
    pub max_iterations: u32,
    /// Delegation timeout; never unlimited.
    pub timeout: Duration,
    /// Structured output schema.
    pub output_schema: Option<Value>,
}

impl fmt::Debug for MemberSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemberSpec")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("skills", &self.skills)
            .field("max_iterations", &self.max_iterations)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

/// What a member is (spec 3.1 §7.10).
///
/// # Example
///
/// ```rust
/// let kind = adk_harness::MemberKind::Local { instruction: "Find the relevant files.".into(), model: None };
/// # let _ = kind;
/// ```
#[derive(Clone)]
#[non_exhaustive]
pub enum MemberKind {
    /// An `LlmAgent` the harness builds.
    Local {
        /// The member's instruction.
        instruction: String,
        /// Its model; `None` uses the member model or the harness model.
        model: Option<Arc<dyn Llm>>,
    },
    /// Claude Code, Codex, Kiro or another harness over ACP.
    #[cfg(feature = "team-acp")]
    Acp(adk_acp::AcpAgentConfig),
    /// A remote agent over A2A.
    #[cfg(feature = "team-a2a")]
    Remote(adk_server::a2a::RemoteA2aConfig),
    /// Any agent the caller supplies.
    Agent(Arc<dyn Agent>),
}

/// The tools a member gets (spec 3.1 §7.10).
///
/// # Example
///
/// ```rust
/// assert!(matches!(adk_harness::MemberTools::default(), adk_harness::MemberTools::ReadOnly));
/// ```
#[derive(Clone, Default)]
pub enum MemberTools {
    /// Read tools only; the default.
    #[default]
    ReadOnly,
    /// Every harness tool a member may have.
    Full,
    /// Exactly these tools.
    Custom(Vec<Arc<dyn Tool>>),
}

impl MemberSpec {
    fn local(name: &str, description: &str, instruction: &str, tools: MemberTools) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            kind: MemberKind::Local { instruction: instruction.to_string(), model: None },
            tools,
            skills: false,
            max_iterations: 50,
            timeout: Duration::from_secs(15 * 60),
            output_schema: None,
        }
    }

    /// A read-only explorer.
    pub fn explore() -> Self {
        Self::local(
            "explore",
            "Explores the codebase read-only and reports what it found, with file paths.",
            "Investigate the question with read-only tools. Report findings with file paths and line numbers.",
            MemberTools::ReadOnly,
        )
    }

    /// A general-purpose member with the full tool set.
    pub fn general() -> Self {
        Self::local(
            "general",
            "Handles a self-contained subtask with the full tool set and reports the result.",
            "Complete the subtask you are given and report what you changed and verified.",
            MemberTools::Full,
        )
    }
}
