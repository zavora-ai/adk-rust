//! A CodeAct agent: a peer to [`LlmAgent`](crate::LlmAgent) that acts by writing
//! and executing code instead of emitting one tool call at a time.
//!
//! The framework is language-agnostic — the [`CodeRuntime`] defines the language
//! (Python via Monty, JavaScript, a shell, ...) and reports it to the agent.
//!
//! # The loop
//!
//! Each turn, the model produces one code script. Tools are exposed as functions
//! the model can call and compose. The script communicates its result by
//! *returning a tagged value* — a [`ScriptOutput`] variant — which the host
//! classifies:
//!
//! - [`ScriptOutput::Observation`] is fed back to the model for the next turn,
//! - [`ScriptOutput::Error`] is fed back as an opaque message string,
//! - [`ScriptOutput::FinalResult`] ends the loop and is returned to the caller,
//!   and
//! - [`ScriptOutput::TransferToAgent`] ends the loop and hands control to
//!   another agent.
//!
//! Errors are just strings produced by the runtime in whatever form the model
//! expects (a traceback, a stack, ...); any error consumes a model turn. When a
//! tool fails, that surfaces as an error raised into the script.
//!
//! # Transfer to another agent
//!
//! Like [`LlmAgent`](crate::LlmAgent), a `CodeActAgent` can hand control to a
//! sub-agent or to a peer/parent the Runner supplies via
//! `RunConfig::transfer_targets`. The transfer output is only described to the
//! model when at least one target exists; a transfer emits an event carrying
//! [`EventActions::transfer_to_agent`](adk_core::EventActions::transfer_to_agent)
//! and ends the run, exactly as the LlmAgent's `transfer_to_agent` tool does. An
//! unknown target is fed back to the model as an error instead of transferring.
//!
//! # Deferred tool calls (HITL and long-running)
//!
//! The script never decides to suspend. The *host* defers a tool call when it
//! cannot resolve inline:
//!
//! - a confirmation-gated tool with no decision yet, or
//! - a [long-running](adk_core::Tool::is_long_running) tool whose result arrives
//!   out-of-band.
//!
//! In both cases the agent serializes the live interpreter continuation into a
//! [`CodeActCheckpoint`] and writes it to **session state** (via an event's
//! `state_delta`), then ends the run — exactly the "save to session, rebuild,
//! continue" model of [`LlmAgent`](crate::LlmAgent). On the next invocation
//! [`Agent::run`](adk_core::Agent::run) reads the checkpoint back and resumes:
//! the confirmation decision arrives via `RunConfig::tool_confirmation_decisions`,
//! and a long-running result arrives as a `FunctionResponse` in the new message.
//! There is no out-of-band resume API and no side store — the Runner re-invokes
//! `run()` and the agent self-routes.
//!
//! This requires a runtime that can snapshot/resume. A runtime that cannot runs
//! long-running tools inline and rejects confirmation pauses.
//!
//! # Tool side effects
//!
//! Like [`LlmAgent`](crate::LlmAgent), tool-produced session changes are
//! propagated: any `state_delta`/`artifact_delta`/`route` a tool sets on its
//! [`ToolContext`](adk_core::ToolContext) is merged onto the next persisted
//! event (a checkpoint, or the final event when the runtime cannot checkpoint),
//! and a tool that sets `escalate`, `skip_summarization`, or `transfer_to_agent`
//! ends the run immediately, forwarding that signal to the Runner. This is what
//! lets an [`AgentTool`](https://docs.rs/adk-tool) wrapping a sub-agent forward
//! that sub-agent's state back to the session.
//!
//! Each tool call runs against a fresh per-call
//! [`ToolContext`](adk_core::ToolContext) that carries the interpreter's call id
//! and otherwise delegates artifacts, memory, shared state, user scopes, and
//! secrets to the live invocation — so a tool behaves identically whether it is
//! driven by a `CodeActAgent` or an `LlmAgent`.
//!
//! # Capabilities (parity with [`LlmAgent`](crate::LlmAgent))
//!
//! A `CodeActAgent` mirrors `LlmAgent`'s configuration surface, differing only
//! where the CodeAct loop demands it:
//!
//! - **Model**: `generate_content_config` plus `temperature`/`top_p`/`top_k`/
//!   `max_output_tokens` shorthands.
//! - **Instructions** (assembled per invocation): `instruction` and
//!   `instruction_provider`, `global_instruction` (+ provider), with
//!   `{state.key}` template injection; the selected skill block, when a skills
//!   index is configured (`skills` feature).
//! - **History**: `include_contents` controls how much session history seeds
//!   the transcript.
//! - **Tools**: static tools plus per-invocation `toolset`s; `tool_timeout`,
//!   `default_retry_budget`/`tool_retry_budget`, `circuit_breaker_threshold`,
//!   and `on_tool_error` fallbacks.
//! - **Confirmation & transfer**: `ToolConfirmationPolicy`, sub-agents and the
//!   `disallow_transfer_to_parent`/`disallow_transfer_to_peers` flags.
//! - **Output**: `output_key`, and `output_schema`/`output_type` validated with
//!   a correction-retry loop (`output_max_retries`).
//! - **Lifecycle & interception hooks**: before/after-agent callbacks
//!   (after-agent runs on normal completion, not on suspension, transfer, or
//!   escalation), before/after-model callbacks (rewrite or short-circuit the
//!   model call), and before/after-tool callbacks plus the rich
//!   `after_tool_callback_full` (rewrite or short-circuit a tool call).
//! - **Feature-gated**: input/output guardrails (`guardrails`), skills
//!   (`skills`), and the `EnhancedPlugin` pipeline intercepting tool and model
//!   calls (`enhanced-plugins`).
//!
//! Deliberate non-matches: code-execution sandboxing is the [`CodeRuntime`]'s
//! responsibility (not a bolt-on); tool dispatch is sequential by design (see
//! [`runtime`]), so there is no `tool_execution_strategy`/concurrency knob; and
//! the agent has no `skip_summarization` *builder* option — the model ends the
//! loop itself via [`ScriptOutput::FinalResult`] — though a tool that sets
//! `skip_summarization` on its actions still ends the run.
//!
//! # Runtime
//!
//! Execution runs on a [`CodeRuntime`], the step-wise interpreter seam. The
//! production adapter wraps [Monty](https://github.com/pydantic/monty), a
//! Rust-native Python interpreter whose snapshot-at-call-boundary model makes
//! suspend/resume a true continuation rather than a replay. It lives in the
//! `adk-codeact-monty` crate (kept outside the workspace because Monty is a git
//! dependency, not yet on crates.io).

pub mod agent;
pub mod checkpoint;
pub mod error_map;
pub mod output;
pub mod runtime;

#[cfg(test)]
pub(crate) mod test_support;

pub use agent::{
    CODEACT_SYSTEM_PROMPT, CodeActAgent, CodeActAgentBuilder, DEFAULT_MAX_ERROR_CHARS,
    DEFAULT_MAX_ITERATIONS, ToolMap, build_tool_map, extract_code_block,
};
pub use checkpoint::{CodeActCheckpoint, Disposition, PENDING_STATE_KEY, PendingToolCall};
pub use error_map::{denied_message, tool_error_message, unknown_tool_message};
pub use output::ScriptOutput;
pub use runtime::{
    CodeRuntime, PendingCall, ResumeWith, RunStep, RuntimeCapabilities, RuntimeError,
    bind_call_args, default_tool_catalog,
};
