//! # adk-guardrail
//!
//! Guardrails framework for validating agent inputs and outputs.
//!
//! ## Overview
//!
//! Input guardrails validate the user's message before the agent runs, and output guardrails
//! validate the final response; the guardrails in one set run concurrently. They can:
//! - Block harmful or off-topic content
//! - Enforce output schemas
//! - Redact PII (emails, phones, SSNs)
//! - Allow, deny, or narrow tool-call arguments before a tool runs ([`ToolGuardrail`])
//!
//! No guardrail tracks cost or token usage.
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use adk_guardrail::{GuardrailSet, ContentFilter, PiiRedactor};
//!
//! let input_guardrails = GuardrailSet::new()
//!     .with(ContentFilter::harmful_content())
//!     .with(PiiRedactor::new());
//!
//! let agent = LlmAgentBuilder::new("assistant")
//!     .input_guardrails(input_guardrails)
//!     .build()?;
//! ```

//! ## Tool Guardrails
//!
//! [`Guardrail`] validates [`Content`](adk_core::Content) — a user message or a model response —
//! and never sees a tool call. [`ToolConfirmationPolicy`](adk_core::ToolConfirmationPolicy)
//! decides per tool *name*. Neither can express "this tool may run, but not with these
//! arguments".
//!
//! [`ToolGuardrail`] receives the tool name and the arguments before the tool executes, and may
//! allow, deny, or narrow them:
//!
//! ```rust,ignore
//! use adk_guardrail::{PathAllowList, ToolGuardrailSet};
//!
//! let tool_guardrails = ToolGuardrailSet::new().with(
//!     PathAllowList::new("agents-only", ["path"], ["/Users/me/Library/LaunchAgents"])
//!         .on_tools(["plist_write"]),
//! );
//!
//! let agent = LlmAgentBuilder::new("ops")
//!     .tool_guardrails(tool_guardrails)
//!     .build()?;
//! ```
//!
//! Guardrails run in order and revisions compose, so a later guardrail sees what an earlier one
//! produced. The first denial stops evaluation, and a denial is reported to the model as the
//! tool's result so it can correct the call rather than the run stalling.
//!
//! Two implementations ship: [`DeniedArgumentPattern`] refuses calls whose serialized arguments
//! match a regular expression, and [`PathAllowList`] confines path-valued arguments to a set of
//! roots — comparing by path component, resolving each existing component to reject symlink
//! escapes, and refusing any path with a `..` component or a missing path argument. It is a
//! preflight check; filesystem tools exposed across a hostile local trust boundary still need
//! platform secure-open primitives to eliminate time-of-check/time-of-use races.
//!
//! `on_tools` limits a guardrail to the named tools, and a tool it does not name is not checked
//! at all, so name every tool that takes a path.

pub mod content;
pub mod error;
pub mod executor;
pub mod pii;
#[cfg(feature = "schema")]
pub mod schema;
pub mod tool;
pub mod traits;

pub use content::{ContentFilter, ContentFilterConfig};
pub use error::{GuardrailError, Result};
pub use executor::{GuardrailExecutor, GuardrailSet};
pub use pii::{PiiRedactor, PiiType};
#[cfg(feature = "schema")]
pub use schema::SchemaValidator;
pub use tool::{
    DeniedArgumentPattern, PathAllowList, ToolCallDecision, ToolGuardrail, ToolGuardrailResult,
    ToolGuardrailSet,
};
pub use traits::{Guardrail, GuardrailResult, Severity};
