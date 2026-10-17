//! # adk-guardrail
//!
//! Guardrails framework for validating agent inputs and outputs.
//!
//! ## Overview
//!
//! Input guardrails validate the user's message before the agent runs, and output guardrails
//! validate the final response and any text the model writes beside a tool call
//! ([`Guardrail::validate_preamble`]); the guardrails in one set run concurrently. They can:
//! - Block harmful or off-topic content
//! - Enforce output schemas
//! - Redact PII (emails, phones, SSNs)
//! - Allow, deny, or narrow tool-call arguments before a tool runs ([`ToolGuardrail`])
//! - Deliver, redact, or withhold a tool's result before the model sees it
//!   ([`ToolGuardrail::validate_result`], [`ResultTextGuardrail`])
//!
//! A block is an error with category `InvalidInput`, code [`INPUT_BLOCKED_CODE`] or
//! [`OUTPUT_BLOCKED_CODE`], and the blocking guardrail's name and severity in its details
//! ([`GuardrailError::Blocked`]). A [`Severity::Low`] failure does not block: the content passes,
//! the failure is logged, and an agent records it on the output event under
//! [`GUARDRAIL_WARNINGS_KEY`].
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
//! After the tool runs, [`ToolGuardrail::validate_result`] sees its result, before the model and
//! the session do, and can revise or withhold it; a withheld result is answered with an error
//! that tells the model the tool ran. [`ResultTextGuardrail`] applies any content guardrail, such
//! as [`PiiRedactor`], to every text value of a result. A [`ToolGuardrailSet`] is also an
//! [`adk_core::ToolResultScreen`], which is how the realtime agents take it.
//!
//! Two call guardrails ship: [`DeniedArgumentPattern`] refuses calls whose serialized arguments
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
pub use error::{
    GUARDRAIL_WARNINGS_KEY, GuardrailError, GuardrailFailure, GuardrailPhase, INPUT_BLOCKED_CODE,
    OUTPUT_BLOCKED_CODE, Result,
};
pub use executor::{GuardrailExecutor, GuardrailSet, Screening};
pub use pii::{PiiRedactor, PiiType};
#[cfg(feature = "schema")]
pub use schema::SchemaValidator;
pub use tool::{
    DeniedArgumentPattern, PathAllowList, ResultTextGuardrail, ToolCallDecision, ToolGuardrail,
    ToolGuardrailResult, ToolGuardrailSet, ToolResultDecision, ToolResultGuardrailResult,
};
pub use traits::{Guardrail, GuardrailResult, Severity};
