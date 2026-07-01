//! The [`CodeActAgent`] and its streaming loop.
//!
//! A peer to [`LlmAgent`](crate::LlmAgent), stateless across invocations: the
//! Runner calls [`Agent::run`], durable state lives in the **Session**.
//!
//! Durability mirrors LlmAgent's per-tool event persistence. The loop is a
//! stream that yields a checkpoint event **before** and **after** every inline
//! tool call (a write-ahead log written to session state via `state_delta`):
//!
//! - **before** ([`Disposition::PendingResult`]): if the process crashes during
//!   the tool, recovery re-runs it (only pure script execution preceded it, so
//!   that is safe);
//! - **after** ([`Disposition::Resolved`]): once the tool has returned, recovery
//!   resumes with the stored result and never re-runs the tool.
//!
//! Deferred calls suspend to session state too:
//! [`Disposition::AwaitingConfirmation`] (decision via
//! `RunConfig::tool_confirmation_decisions`) and
//! [`Disposition::AwaitingCompletion`] (result via a `FunctionResponse` in the
//! next message). On the next invocation `run()` reads the checkpoint back and
//! resumes. This needs a runtime that can snapshot/resume; one that can't runs
//! long-running tools inline and persists nothing.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::{
    AdkError, AfterAgentCallback, AfterModelCallback, AfterToolCallback, AfterToolCallbackFull,
    Agent, Artifacts, BeforeAgentCallback, BeforeModelCallback, BeforeModelResult,
    BeforeToolCallback, CallbackContext, Content, ErrorCategory, ErrorComponent, Event,
    EventActions, EventStream, GenerateContentConfig, GlobalInstructionProvider, IncludeContents,
    InstructionProvider, InvocationContext, Llm, LlmRequest, LlmResponse, MemoryEntry,
    OnToolErrorCallback, Part, ReadonlyContext, RetryBudget, SharedState, Tool,
    ToolCallbackContext, ToolConfirmationDecision, ToolConfirmationPolicy, ToolConfirmationRequest,
    ToolContext, ToolOutcome, Toolset,
};
use async_stream::stream;
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use serde_json::Value;

use crate::codeact::checkpoint::{
    CodeActCheckpoint, Disposition, PENDING_STATE_KEY, PendingToolCall, ResolutionRecord,
};
use crate::codeact::error_map::{
    denied_message, render_value, tool_error_message, unknown_tool_message,
};
use crate::codeact::output::ScriptOutput;
use crate::codeact::runtime::{CodeRuntime, ResumeWith, RunStep, RuntimeError, bind_call_args};
use crate::guardrails::{GuardrailSet, enforce_guardrails};
use crate::skill_shim::{SelectionPolicy, SkillIndex, select_skill_prompt_block};
#[cfg(feature = "enhanced-plugins")]
use adk_plugin::{
    AfterModelCallResult, AfterToolCallResult, BeforeModelCallResult, BeforeToolCallResult,
    EnhancedPlugin, EnhancedPluginManager,
};

/// Default number of model turns before the loop gives up.
pub const DEFAULT_MAX_ITERATIONS: u32 = 12;

/// Default cap (in chars) on an error message fed back to the model.
pub const DEFAULT_MAX_ERROR_CHARS: usize = 4000;

/// Default per-tool execution timeout (5 minutes), matching `LlmAgent`.
pub const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(300);

/// Default number of correction retries for output-schema validation.
pub const DEFAULT_OUTPUT_MAX_RETRIES: usize = 3;

/// Default cap (in chars) on injected skill content.
pub const DEFAULT_MAX_SKILL_CHARS: usize = 2000;

/// The default, language-agnostic CodeAct system prompt.
pub const CODEACT_SYSTEM_PROMPT: &str = "\
You act by writing code. Each turn, emit exactly ONE code block in the language
described below. Tools are available as functions you can call and compose.

Return your result by returning a value tagged with a \"type\":
- {\"type\": \"observation\", \"value\": ...}: something to inspect; you continue.
- {\"type\": \"final_result\", \"value\": ...}: the task is done; this ends it.
- {\"type\": \"error\", \"message\": \"...\"}: a problem to record; you continue.

If your code errors, you will see the error and can try again. Use final_result
only when the task is genuinely done.";

/// A tool lookup table keyed by tool name.
pub type ToolMap = HashMap<String, Arc<dyn Tool>>;

/// Build a [`ToolMap`] from a tool list, excluding built-in (server-side) tools
/// which cannot be invoked from a script.
pub fn build_tool_map(tools: &[Arc<dyn Tool>]) -> ToolMap {
    tools.iter().filter(|t| !t.is_builtin()).map(|t| (t.name().to_string(), t.clone())).collect()
}

// ===========================================================================
// The streaming loop
// ===========================================================================

/// All the (owned) inputs the loop needs.
struct LoopInputs {
    model: Arc<dyn Llm>,
    runtime: Arc<dyn CodeRuntime>,
    tools: Vec<Arc<dyn Tool>>,
    policy: ToolConfirmationPolicy,
    decisions: HashMap<String, ToolConfirmationDecision>,
    /// The live invocation context; a fresh [`CodeToolContext`] is built from it
    /// per tool call so each call carries its own function-call id and actions.
    invocation_ctx: Arc<dyn InvocationContext>,
    incoming: Content,
    /// The fully-assembled system prompt (instructions + contract + runtime +
    /// tools + transfer section), built once per invocation in `run`.
    system_prompt: String,
    /// The seeded conversation (history + current turn) per `include_contents`.
    conversation: Vec<Content>,
    pending: Option<CodeActCheckpoint>,
    invocation_id: String,
    agent_name: String,
    max_iterations: u32,
    max_error_chars: usize,
    supports_suspension: bool,
    /// Agent names this script may transfer to (sub-agents + runner targets).
    /// Empty means transfer is not offered to the model.
    transfer_targets: Vec<String>,
    /// Generation config (temperature, etc.) applied to every model request.
    generate_content_config: Option<GenerateContentConfig>,
    /// Per-tool execution timeout.
    tool_timeout: Duration,
    /// State key to store the final result under, if any.
    output_key: Option<String>,
    /// Retry budget applied to tools without a per-tool override.
    default_retry_budget: Option<RetryBudget>,
    /// Per-tool retry budget overrides, keyed by tool name.
    tool_retry_budgets: HashMap<String, RetryBudget>,
    /// Consecutive-failure threshold before a tool is short-circuited.
    circuit_breaker_threshold: Option<u32>,
    /// Fallback callbacks tried when a tool ultimately fails.
    on_tool_error: Arc<Vec<OnToolErrorCallback>>,
    /// Callbacks invoked before each model request (may rewrite or short-circuit).
    before_model_callbacks: Arc<Vec<BeforeModelCallback>>,
    /// Callbacks invoked after each model response (may rewrite it).
    after_model_callbacks: Arc<Vec<AfterModelCallback>>,
    /// Callbacks invoked before each tool call (may short-circuit with content).
    before_tool_callbacks: Arc<Vec<BeforeToolCallback>>,
    /// Callbacks invoked after each tool attempt resolves (may rewrite the result).
    after_tool_callbacks: Arc<Vec<AfterToolCallback>>,
    /// Rich after-tool callbacks receiving the tool, args, and response value.
    after_tool_callbacks_full: Arc<Vec<AfterToolCallbackFull>>,
    /// JSON schema the final result must validate against, if any.
    output_schema: Option<Value>,
    /// Max correction retries when the final result fails schema validation.
    output_max_retries: usize,
    /// Guardrails applied to the final result content before it is emitted.
    output_guardrails: Arc<GuardrailSet>,
    /// Plugin pipeline intercepting tool and model calls.
    #[cfg(feature = "enhanced-plugins")]
    enhanced_plugin_manager: Option<Arc<EnhancedPluginManager>>,
}

/// Per-invocation consecutive-failure tracker. When a tool's failure count
/// reaches `threshold` it is "open" and short-circuited until the next run.
struct CircuitBreaker {
    threshold: u32,
    failures: HashMap<String, u32>,
}

impl CircuitBreaker {
    fn new(threshold: u32) -> Self {
        Self { threshold, failures: HashMap::new() }
    }

    fn is_open(&self, tool: &str) -> bool {
        self.failures.get(tool).copied().unwrap_or(0) >= self.threshold
    }

    fn record(&mut self, tool: &str, success: bool) {
        if success {
            self.failures.remove(tool);
        } else {
            *self.failures.entry(tool.to_string()).or_insert(0) += 1;
        }
    }
}

/// The tool-execution policy shared across every call in one invocation.
struct ToolPolicy<'a> {
    tool_timeout: Duration,
    default_budget: Option<&'a RetryBudget>,
    tool_budgets: &'a HashMap<String, RetryBudget>,
    on_tool_error: &'a [OnToolErrorCallback],
    before_tool: &'a [BeforeToolCallback],
    after_tool: &'a [AfterToolCallback],
    after_tool_full: &'a [AfterToolCallbackFull],
    #[cfg(feature = "enhanced-plugins")]
    plugins: Option<&'a EnhancedPluginManager>,
}

impl ToolPolicy<'_> {
    /// The retry budget for `tool`: a per-tool override, else the default.
    fn budget_for(&self, tool: &str) -> Option<&RetryBudget> {
        self.tool_budgets.get(tool).or(self.default_budget)
    }
}

/// Drive CodeAct to completion as a stream of events.
///
/// Yields durability/suspend events as it goes (persisted by the Runner) and a
/// final event at the end. This is the whole loop; [`CodeActAgent::run`] just
/// gathers [`LoopInputs`] from `self` + the context and forwards.
fn run_codeact(input: LoopInputs) -> impl Stream<Item = adk_core::Result<Event>> {
    stream! {
        let LoopInputs {
            model, runtime, tools, policy, decisions, invocation_ctx, incoming, system_prompt, conversation,
            pending, invocation_id, agent_name, max_iterations, max_error_chars, supports_suspension,
            transfer_targets, generate_content_config, tool_timeout, output_key,
            default_retry_budget, tool_retry_budgets, circuit_breaker_threshold, on_tool_error,
            before_model_callbacks, after_model_callbacks, before_tool_callbacks,
            after_tool_callbacks, after_tool_callbacks_full,
            output_schema, output_max_retries, output_guardrails,
            #[cfg(feature = "enhanced-plugins")]
            enhanced_plugin_manager,
        } = input;

        let tool_map = build_tool_map(&tools);
        let roster = roster(&tool_map);

        // Tool-robustness state for this invocation.
        let tool_policy = ToolPolicy {
            tool_timeout,
            default_budget: default_retry_budget.as_ref(),
            tool_budgets: &tool_retry_budgets,
            on_tool_error: on_tool_error.as_slice(),
            before_tool: before_tool_callbacks.as_slice(),
            after_tool: after_tool_callbacks.as_slice(),
            after_tool_full: after_tool_callbacks_full.as_slice(),
            #[cfg(feature = "enhanced-plugins")]
            plugins: enhanced_plugin_manager.as_deref(),
        };
        let mut circuit = circuit_breaker_threshold.map(CircuitBreaker::new);

        // Callback context for model-level hooks (the invocation context is also a
        // callback context). Tool-level callbacks get a fresh per-call context.
        let model_ctx: Arc<dyn CallbackContext> = invocation_ctx.clone();
        let model_hooks = ModelHooks {
            before: before_model_callbacks.as_slice(),
            after: after_model_callbacks.as_slice(),
            ctx: &model_ctx,
            #[cfg(feature = "enhanced-plugins")]
            plugins: enhanced_plugin_manager.as_deref(),
        };

        let mut transcript: Vec<Content>;
        let mut iteration: u32;
        let mut pending_step: Option<RunStep>;

        // Tool-produced state/artifact deltas not yet persisted. Drained into
        // the next yielded event so they reach the session, mirroring how
        // LlmAgent puts each tool's actions on its function-response event.
        let mut pending_actions = EventActions::default();

        // Count of output-schema correction retries used so far.
        let mut schema_retries: usize = 0;

        // ----- self-route: resume a pending checkpoint, or start fresh -----
        match pending {
            None => {
                // The system prompt (including any transfer section) is assembled
                // in `run`; seed it followed by the resolved conversation.
                transcript = std::iter::once(Content::new("user").with_text(system_prompt))
                    .chain(conversation)
                    .collect();
                iteration = 0;
                pending_step = None;
            }
            Some(cp) => {
                if cp.tool_roster != roster {
                    yield Err(AdkError::new(
                        ErrorComponent::Agent,
                        ErrorCategory::InvalidInput,
                        "codeact.resume.roster_mismatch",
                        "tool roster changed since suspend; cannot resume continuation",
                    ));
                    return;
                }
                // Resolve the value/error to feed back. For the dispositions
                // that execute (or decide) a tool now, capture a
                // [`ResolutionRecord`] so we can persist a SAVE-AFTER checkpoint
                // *before* resuming the interpreter — recovery then never re-runs
                // the tool, even if the process crashes mid-resume.
                // A terminal signal raised by a freshly-executed tool on this
                // resume (escalate / skip-summarization / transfer). Checked
                // before resuming the interpreter, mirroring the inline path.
                let mut resume_control = ToolControl::default();
                let resolution: Option<ResolutionRecord> = match &cp.disposition {
                    Disposition::PendingResult => {
                        let (with, control) = execute_for_resume(
                            &tool_map, &invocation_ctx, &cp.call, &mut pending_actions,
                            &tool_policy, &mut circuit,
                        )
                        .await;
                        resume_control = control;
                        Some(resume_to_record(with))
                    }
                    Disposition::Resolved(rec) => Some(rec.clone()),
                    Disposition::AwaitingConfirmation => match decisions.get(&cp.call.tool).copied() {
                        Some(ToolConfirmationDecision::Approve) => {
                            let (with, control) = execute_for_resume(
                                &tool_map, &invocation_ctx, &cp.call, &mut pending_actions,
                                &tool_policy, &mut circuit,
                            )
                            .await;
                            resume_control = control;
                            Some(resume_to_record(with))
                        }
                        Some(ToolConfirmationDecision::Deny) => {
                            Some(ResolutionRecord::Raise(denied_message(&cp.call.tool)))
                        }
                        None => None,
                    },
                    Disposition::AwaitingCompletion { .. } => {
                        completion_for(&incoming, &cp.call.tool, cp.call.call_id)
                            .map(ResolutionRecord::Value)
                    }
                };
                match resolution {
                    // No resolution available yet — keep waiting (re-persist).
                    None => {
                        let mut event = suspend_event(&invocation_id, &agent_name, &cp);
                        flush_actions(&mut event, &mut pending_actions);
                        yield Ok(event);
                        return;
                    }
                    Some(rec) => {
                        // A tool that escalated / requested skip-summarization /
                        // a transfer on this resume ends the run now, before the
                        // result is fed back to the script (matches the inline
                        // path and LlmAgent's terminal actions).
                        if resume_control.is_terminal() {
                            let mut event = control_terminal_event(
                                &invocation_id, &agent_name, &resume_control,
                            );
                            flush_actions(&mut event, &mut pending_actions);
                            yield Ok(event);
                            return;
                        }
                        // SAVE-AFTER: persist the resolution unless this checkpoint
                        // was already `Resolved` (in which case it is on disk).
                        if !matches!(cp.disposition, Disposition::Resolved(_)) {
                            let after = mk_checkpoint(
                                cp.iteration,
                                cp.transcript.clone(),
                                cp.snapshot.clone(),
                                cp.call.clone(),
                                Disposition::Resolved(rec.clone()),
                                &roster,
                            );
                            let mut after_ev =
                                checkpoint_event(&invocation_id, &agent_name, &after);
                            flush_actions(&mut after_ev, &mut pending_actions);
                            yield Ok(after_ev);
                        }
                        match runtime.resume(&cp.snapshot, resolution_to_resume(rec)) {
                            Ok(step) => {
                                transcript = cp.transcript;
                                iteration = cp.iteration;
                                pending_step = Some(step);
                            }
                            Err(e) => {
                                yield Err(runtime_err(e));
                                return;
                            }
                        }
                    }
                }
            }
        }

        // ----- main loop -----
        loop {
            let mut step = match pending_step.take() {
                Some(step) => step,
                None => {
                    iteration += 1;
                    if iteration > max_iterations {
                        // Mirror LlmAgent: exceeding the turn budget is an error,
                        // not a silent terminal message.
                        yield Err(AdkError::agent(format!(
                            "max iterations ({max_iterations}) exceeded without a final result"
                        )));
                        return;
                    }
                    let script = match next_script(
                        model.as_ref(),
                        &transcript,
                        &generate_content_config,
                        &model_hooks,
                    )
                    .await
                    {
                        Ok(s) => s,
                        Err(e) => {
                            yield Err(e);
                            return;
                        }
                    };
                    transcript.push(Content::new("model").with_text(script.clone()));
                    // A parse/compile failure arrives as `RunStep::Raised`, not a
                    // `RuntimeError` — it is handled in the loop like any other
                    // script error and fed back to the model.
                    match runtime.start(&script, "agent") {
                        Ok(step) => step,
                        Err(e) => {
                            yield Err(runtime_err(e));
                            return;
                        }
                    }
                }
            };

            // stdout (`print`) the script emits across this turn, accumulated
            // step by step and surfaced to the model when the turn hands control
            // back so it can see what its code printed. It is also baked into any
            // checkpoint written mid-turn (see `transcript_with_stdout`) so it
            // survives suspend/resume and crash recovery. On a *terminal* outcome
            // (`final_result`, transfer, or a terminal tool signal) the run ends
            // and there is no next turn, so the accumulated output is
            // intentionally not surfaced — the final result is the answer.
            let mut script_output = String::new();

            'script: loop {
                match step {
                    RunStep::Call { call, stdout } => {
                        script_output.push_str(&stdout);
                        let name = call.function_name().to_string();
                        let call_id = call.call_id();

                        let Some(tool) = tool_map.get(&name).cloned() else {
                            match call.resume(ResumeWith::Raise(unknown_tool_message(&name))) {
                                Ok(s) => step = s,
                                Err(e) => {
                                    yield Err(runtime_err(e));
                                    return;
                                }
                            }
                            continue 'script;
                        };

                        // Bind the call's positional + keyword arguments onto the
                        // tool's parameters centrally, so runtimes never need a
                        // schema at the call boundary.
                        let args = bind_call_args(
                            tool.as_ref(),
                            call.positional_args(),
                            call.keyword_args(),
                        );
                        let pcall = PendingToolCall {
                            call_id,
                            tool: name.clone(),
                            args: args.clone(),
                        };

                        // Confirmation gate.
                        if policy.requires_confirmation(&name) {
                            match decisions.get(&name).copied() {
                                Some(ToolConfirmationDecision::Approve) => {}
                                Some(ToolConfirmationDecision::Deny) => {
                                    match call.resume(ResumeWith::Raise(denied_message(&name))) {
                                        Ok(s) => step = s,
                                        Err(e) => {
                                            yield Err(runtime_err(e));
                                            return;
                                        }
                                    }
                                    continue 'script;
                                }
                                None if supports_suspension => {
                                    let snapshot = match call.dump() {
                                        Ok(s) => s,
                                        Err(e) => {
                                            yield Err(runtime_err(e));
                                            return;
                                        }
                                    };
                                    let cp = mk_checkpoint(
                                        iteration,
                                        transcript_with_stdout(
                                            &transcript,
                                            &script_output,
                                            max_error_chars,
                                        ),
                                        snapshot,
                                        pcall,
                                        Disposition::AwaitingConfirmation,
                                        &roster,
                                    );
                                    let mut event = suspend_event(&invocation_id, &agent_name, &cp);
                                    flush_actions(&mut event, &mut pending_actions);
                                    yield Ok(event);
                                    return;
                                }
                                None => {
                                    let msg = format!(
                                        "tool '{name}' requires confirmation but this runtime cannot pause"
                                    );
                                    match call.resume(ResumeWith::Raise(msg)) {
                                        Ok(s) => step = s,
                                        Err(e) => {
                                            yield Err(runtime_err(e));
                                            return;
                                        }
                                    }
                                    continue 'script;
                                }
                            }
                        }

                        // Long-running deferral (needs suspension).
                        if supports_suspension && tool.is_long_running() {
                            let snapshot = match call.dump() {
                                Ok(s) => s,
                                Err(e) => {
                                    yield Err(runtime_err(e));
                                    return;
                                }
                            };
                            let tool_ctx: Arc<dyn ToolContext> =
                                Arc::new(CodeToolContext::new(invocation_ctx.clone(), call_id));
                            let exec =
                                run_tool(&tool, &tool_ctx, args, &tool_policy, &mut circuit).await;
                            let control = capture_actions(&mut pending_actions, tool_ctx.actions());
                            // A terminal signal (escalate / skip-summarization /
                            // transfer) ends the run before suspending, matching
                            // the inline path and LlmAgent's terminal actions.
                            if control.is_terminal() {
                                let mut event =
                                    control_terminal_event(&invocation_id, &agent_name, &control);
                                flush_actions(&mut event, &mut pending_actions);
                                yield Ok(event);
                                return;
                            }
                            match exec {
                                Ok(handle) => {
                                    let cp = mk_checkpoint(
                                        iteration,
                                        transcript_with_stdout(
                                            &transcript,
                                            &script_output,
                                            max_error_chars,
                                        ),
                                        snapshot,
                                        pcall,
                                        Disposition::AwaitingCompletion { pending_handle: Some(handle) },
                                        &roster,
                                    );
                                    let mut event = suspend_event(&invocation_id, &agent_name, &cp);
                                    flush_actions(&mut event, &mut pending_actions);
                                    yield Ok(event);
                                    return;
                                }
                                Err(msg) => {
                                    match call.resume(ResumeWith::Raise(msg)) {
                                        Ok(s) => step = s,
                                        Err(e) => {
                                            yield Err(runtime_err(e));
                                            return;
                                        }
                                    }
                                    continue 'script;
                                }
                            }
                        }

                        // Inline tool: bracket with before/after checkpoints when
                        // the runtime can snapshot.
                        let resume = if supports_suspension {
                            let snapshot = match call.dump() {
                                Ok(s) => s,
                                Err(e) => {
                                    yield Err(runtime_err(e));
                                    return;
                                }
                            };
                            // SAVE-BEFORE (carries any deltas captured on resume).
                            let before = mk_checkpoint(
                                iteration,
                                transcript_with_stdout(
                                    &transcript,
                                    &script_output,
                                    max_error_chars,
                                ),
                                snapshot.clone(),
                                pcall.clone(),
                                Disposition::PendingResult,
                                &roster,
                            );
                            let mut before_ev = checkpoint_event(&invocation_id, &agent_name, &before);
                            flush_actions(&mut before_ev, &mut pending_actions);
                            yield Ok(before_ev);

                            let tool_ctx: Arc<dyn ToolContext> =
                                Arc::new(CodeToolContext::new(invocation_ctx.clone(), call_id));
                            let exec =
                                run_tool(&tool, &tool_ctx, args, &tool_policy, &mut circuit).await;
                            let control = capture_actions(&mut pending_actions, tool_ctx.actions());
                            let resolution = match exec {
                                Ok(v) => ResolutionRecord::Value(v),
                                Err(msg) => ResolutionRecord::Raise(msg),
                            };

                            // A tool that escalated / requested skip-summarization /
                            // a transfer ends the run before the result is fed back
                            // to the script (matches LlmAgent's terminal actions).
                            if control.is_terminal() {
                                let mut event =
                                    control_terminal_event(&invocation_id, &agent_name, &control);
                                flush_actions(&mut event, &mut pending_actions);
                                yield Ok(event);
                                return;
                            }

                            // SAVE-AFTER
                            let after = mk_checkpoint(
                                iteration,
                                transcript_with_stdout(
                                    &transcript,
                                    &script_output,
                                    max_error_chars,
                                ),
                                snapshot,
                                pcall,
                                Disposition::Resolved(resolution.clone()),
                                &roster,
                            );
                            let mut after_ev = checkpoint_event(&invocation_id, &agent_name, &after);
                            flush_actions(&mut after_ev, &mut pending_actions);
                            yield Ok(after_ev);
                            resolution_to_resume(resolution)
                        } else {
                            let tool_ctx: Arc<dyn ToolContext> =
                                Arc::new(CodeToolContext::new(invocation_ctx.clone(), call_id));
                            let exec =
                                run_tool(&tool, &tool_ctx, args, &tool_policy, &mut circuit).await;
                            let control = capture_actions(&mut pending_actions, tool_ctx.actions());
                            if control.is_terminal() {
                                let mut event =
                                    control_terminal_event(&invocation_id, &agent_name, &control);
                                flush_actions(&mut event, &mut pending_actions);
                                yield Ok(event);
                                return;
                            }
                            match exec {
                                Ok(v) => ResumeWith::Value(v),
                                Err(msg) => ResumeWith::Raise(msg),
                            }
                        };

                        match call.resume(resume) {
                            Ok(s) => step = s,
                            Err(e) => {
                                yield Err(runtime_err(e));
                                return;
                            }
                        }
                    }
                    RunStep::Complete { value, stdout } => {
                        script_output.push_str(&stdout);
                        match ScriptOutput::decode(value) {
                        ScriptOutput::Observation { value } => {
                            transcript.push(observation_content(&value));
                            break 'script;
                        }
                        ScriptOutput::Error { message } => {
                            transcript.push(error_content(&truncate_middle(&message, max_error_chars)));
                            break 'script;
                        }
                        ScriptOutput::FinalResult { value } => {
                            // Validate against the output schema, if any. On
                            // failure, feed a correction back to the model and
                            // let it retry, up to `output_max_retries`.
                            if let Some(schema) = &output_schema
                                && let Err(err) = validate_against_schema(&value, schema)
                            {
                                if schema_retries >= output_max_retries {
                                    yield Err(AdkError::agent(format!(
                                        "final result failed schema validation after {output_max_retries} attempts: {err}"
                                    )));
                                    return;
                                }
                                schema_retries += 1;
                                transcript.push(error_content(&format!(
                                    "final_result did not match the required schema: {err}. Return a conforming final_result."
                                )));
                                break 'script;
                            }
                            let mut event = final_event(&invocation_id, &agent_name, &value);
                            // Apply output guardrails to the rendered result
                            // (block or transform, e.g. PII redaction). When a
                            // guardrail transforms the content, the redacted text
                            // — not the raw value — is what gets stored under
                            // `output_key`, so nothing unredacted leaks into state.
                            let mut stored_value = value.clone();
                            if !output_guardrails.is_empty() {
                                let original_text = render_final(&value);
                                let content =
                                    Content::new("model").with_text(original_text.clone());
                                match enforce_guardrails(&output_guardrails, &content, "output")
                                    .await
                                {
                                    Ok(c) => {
                                        let guarded_text: String =
                                            c.parts.iter().filter_map(|p| p.text()).collect();
                                        if guarded_text != original_text {
                                            stored_value = Value::String(guarded_text);
                                        }
                                        event.llm_response.content = Some(c);
                                    }
                                    Err(e) => {
                                        yield Err(e);
                                        return;
                                    }
                                }
                            }
                            if let Some(key) = &output_key {
                                event.actions.state_delta.insert(key.clone(), stored_value);
                            }
                            flush_actions(&mut event, &mut pending_actions);
                            yield Ok(event);
                            return;
                        }
                        ScriptOutput::TransferToAgent { agent_name: target } => {
                            if transfer_targets.iter().any(|n| n == &target) {
                                let mut event = transfer_event(&invocation_id, &agent_name, &target);
                                flush_actions(&mut event, &mut pending_actions);
                                yield Ok(event);
                                return;
                            }
                            // Unknown target: tell the model and let it retry.
                            transcript.push(error_content(&format!(
                                "cannot transfer to '{target}'; valid agents: {}",
                                roster_list(&transfer_targets)
                            )));
                            break 'script;
                        }
                        }
                    }
                    RunStep::Raised { message, stdout } => {
                        script_output.push_str(&stdout);
                        transcript.push(error_content(&truncate_middle(&message, max_error_chars)));
                        break 'script;
                    }
                }
            }

            // The turn handed control back to the model: surface any stdout the
            // script printed so the model can see its own output next turn.
            if !script_output.is_empty() {
                transcript.push(stdout_content(&truncate_middle(&script_output, max_error_chars)));
            }
        }
    }
}

/// Execute a tool to produce the value/error fed back into a resumed
/// continuation (used on recovery for `PendingResult`/confirmed calls).
///
/// Any state/artifact deltas the tool sets are captured into `pending` so they
/// ride on the next persisted event, just like inline execution. The returned
/// [`ToolControl`] carries any terminal signal (escalate / skip-summarization /
/// transfer) the tool raised, so the caller can end the run before resuming the
/// interpreter, exactly as the inline path does.
async fn execute_for_resume(
    tools: &ToolMap,
    invocation_ctx: &Arc<dyn InvocationContext>,
    call: &PendingToolCall,
    pending: &mut EventActions,
    policy: &ToolPolicy<'_>,
    circuit: &mut Option<CircuitBreaker>,
) -> (ResumeWith, ToolControl) {
    match tools.get(&call.tool) {
        Some(tool) => {
            let tool_ctx: Arc<dyn ToolContext> =
                Arc::new(CodeToolContext::new(invocation_ctx.clone(), call.call_id));
            let result = match run_tool(tool, &tool_ctx, call.args.clone(), policy, circuit).await {
                Ok(v) => ResumeWith::Value(v),
                Err(msg) => ResumeWith::Raise(msg),
            };
            let control = capture_actions(pending, tool_ctx.actions());
            (result, control)
        }
        None => (ResumeWith::Raise(unknown_tool_message(&call.tool)), ToolControl::default()),
    }
}

/// Execute a tool with the full robustness policy and report a single outcome.
///
/// In order: a tripped circuit breaker short-circuits with an error; otherwise
/// the tool runs under a timeout, retrying per its [`RetryBudget`]; the circuit
/// breaker records success/failure; and on ultimate failure the `on_tool_error`
/// callbacks are tried for a fallback value. `Ok` is a value to feed back into
/// the script (a real result or a fallback); `Err` is a message to raise.
async fn run_tool(
    tool: &Arc<dyn Tool>,
    tool_ctx: &Arc<dyn ToolContext>,
    args: Value,
    policy: &ToolPolicy<'_>,
    circuit: &mut Option<CircuitBreaker>,
) -> Result<Value, String> {
    let name = tool.name().to_string();

    if let Some(cb) = circuit.as_ref()
        && cb.is_open(&name)
    {
        return Err(format!(
            "tool '{name}' is temporarily disabled after {} consecutive failures",
            cb.threshold
        ));
    }

    // before-tool plugins: rewrite args or short-circuit with a synthetic result.
    #[cfg(feature = "enhanced-plugins")]
    let args = match policy.plugins {
        Some(epm) => match epm
            .run_before_tool_call(tool.clone(), args, tool_ctx.clone() as Arc<dyn CallbackContext>)
            .await
        {
            Ok(BeforeToolCallResult::Continue(modified)) => modified,
            Ok(BeforeToolCallResult::ShortCircuit(result)) => return Ok(result),
            Err(e) => return Err(e.to_string()),
        },
        None => args,
    };

    // before-tool callbacks: the first to return content short-circuits the call
    // (its value is fed back into the script as if the tool had returned it).
    if !policy.before_tool.is_empty() {
        let cb_ctx: Arc<dyn CallbackContext> = Arc::new(ToolCallbackContext::new(
            tool_ctx.clone() as Arc<dyn CallbackContext>,
            name.clone(),
            args.clone(),
        ));
        for callback in policy.before_tool {
            match callback(cb_ctx.clone()).await {
                Ok(Some(content)) => return Ok(content_to_tool_value(&content)),
                Ok(None) => continue,
                Err(e) => return Err(e.to_string()),
            }
        }
    }

    let budget = policy.budget_for(&name);
    let max_attempts = budget.map(|b| b.max_retries + 1).unwrap_or(1);
    let delay = budget.map(|b| b.delay).unwrap_or_default();

    let started = std::time::Instant::now();
    let mut last_error = String::new();
    let mut value = None;
    // 0-based index of the attempt that produced the outcome (the successful
    // attempt, or the last failed one). Surfaced via `ToolOutcome::attempt`.
    let mut final_attempt: u32 = 0;
    for attempt in 0..max_attempts {
        final_attempt = attempt;
        if attempt > 0 {
            tokio::time::sleep(delay).await;
        }
        // Catch panics so a misbehaving tool surfaces as a script-level error
        // rather than aborting the whole loop (matches LlmAgent).
        let exec = std::panic::AssertUnwindSafe(tokio::time::timeout(
            policy.tool_timeout,
            tool.execute(tool_ctx.clone(), args.clone()),
        ));
        match futures::FutureExt::catch_unwind(exec).await {
            Ok(Ok(Ok(v))) => {
                value = Some(v);
                break;
            }
            Ok(Ok(Err(err))) => last_error = tool_error_message(&err),
            Ok(Err(_)) => {
                last_error = format!("tool timed out after {}s", policy.tool_timeout.as_secs())
            }
            Err(_) => last_error = format!("tool '{name}' panicked during execution"),
        }
    }

    let tool_succeeded = value.is_some();
    if let Some(cb) = circuit.as_mut() {
        cb.record(&name, tool_succeeded);
    }

    let outcome = ToolOutcome {
        tool_name: name.clone(),
        tool_args: args.clone(),
        success: tool_succeeded,
        duration: started.elapsed(),
        error_message: (!tool_succeeded).then(|| last_error.clone()),
        attempt: final_attempt,
    };

    let mut result = match value {
        Some(v) => Ok(v),
        None => {
            // Try fallbacks; the first to return a value wins. As in LlmAgent,
            // after-tool callbacks still see the original failed outcome even
            // when an on-tool-error fallback supplies the response value.
            let mut fallback = None;
            for callback in policy.on_tool_error {
                match callback(
                    tool_ctx.clone() as Arc<dyn CallbackContext>,
                    tool.clone(),
                    args.clone(),
                    last_error.clone(),
                )
                .await
                {
                    Ok(Some(result)) => {
                        fallback = Some(result);
                        break;
                    }
                    Ok(None) => continue,
                    Err(err) => {
                        tracing::warn!(error = %err, "on_tool_error callback failed");
                        break;
                    }
                }
            }
            fallback.ok_or_else(|| last_error.clone())
        }
    };

    // Legacy after-tool callbacks (content-returning), then the rich
    // after-tool-full callbacks (value-returning); the first of each to return
    // Some replaces the tool result fed back into the script. They run for both
    // success and failure, with `ToolOutcome` carrying success/error metadata,
    // matching LlmAgent.
    if !policy.after_tool.is_empty() || !policy.after_tool_full.is_empty() {
        let outcome_ctx: Arc<dyn CallbackContext> = Arc::new(ToolOutcomeContext::new(
            tool_ctx.clone() as Arc<dyn CallbackContext>,
            outcome,
        ));
        let cb_ctx: Arc<dyn CallbackContext> =
            Arc::new(ToolCallbackContext::new(outcome_ctx, name.clone(), args.clone()));
        for callback in policy.after_tool {
            match callback(cb_ctx.clone()).await {
                Ok(Some(content)) => {
                    result = Ok(content_to_tool_value(&content));
                    break;
                }
                Ok(None) => continue,
                Err(e) => return Err(e.to_string()),
            }
        }
        let callback_value =
            result.clone().unwrap_or_else(|message| serde_json::json!({ "error": message }));
        for callback in policy.after_tool_full {
            match callback(cb_ctx.clone(), tool.clone(), args.clone(), callback_value.clone()).await
            {
                Ok(Some(modified)) => {
                    result = Ok(modified);
                    break;
                }
                Ok(None) => continue,
                Err(e) => return Err(e.to_string()),
            }
        }
    }

    let result = result?;

    // after-tool plugins may transform the successful result last.
    #[cfg(feature = "enhanced-plugins")]
    if let Some(epm) = policy.plugins {
        return match epm
            .run_after_tool_call(
                tool.clone(),
                &args,
                result,
                tool_ctx.clone() as Arc<dyn CallbackContext>,
            )
            .await
        {
            Ok(AfterToolCallResult::Continue(modified)) => Ok(modified),
            Err(e) => Err(e.to_string()),
        };
    }
    Ok(result)
}

/// Run-control signals a tool may raise via its [`EventActions`].
///
/// Any of these ends the run before the result is fed back into the script,
/// mirroring how `LlmAgent` treats `escalate`/`skip_summarization` as terminal
/// and forwards a tool-set `transfer_to_agent`.
#[derive(Default)]
struct ToolControl {
    escalate: bool,
    skip_summarization: bool,
    transfer_to_agent: Option<String>,
}

impl ToolControl {
    /// Whether any signal requires ending the run now.
    fn is_terminal(&self) -> bool {
        self.escalate || self.skip_summarization || self.transfer_to_agent.is_some()
    }
}

/// Merge a tool's resulting data deltas (state, artifacts, route) into the
/// pending accumulator and return the run-control signals it raised. Mirrors
/// LlmAgent, which forwards the full tool `EventActions` onto the tool event.
fn capture_actions(pending: &mut EventActions, actions: EventActions) -> ToolControl {
    for (key, value) in actions.state_delta {
        pending.state_delta.insert(key, value);
    }
    for (key, version) in actions.artifact_delta {
        pending.artifact_delta.insert(key, version);
    }
    if let Some(route) = actions.route {
        pending.route = Some(route);
    }
    ToolControl {
        escalate: actions.escalate,
        skip_summarization: actions.skip_summarization,
        transfer_to_agent: actions.transfer_to_agent,
    }
}

/// Drain accumulated tool deltas into an event's actions so the Runner persists
/// them, leaving the accumulator empty. Tool keys are merged alongside any
/// pending-checkpoint key already on the event.
fn flush_actions(event: &mut Event, pending: &mut EventActions) {
    for (key, value) in std::mem::take(&mut pending.state_delta) {
        event.actions.state_delta.insert(key, value);
    }
    for (key, version) in std::mem::take(&mut pending.artifact_delta) {
        event.actions.artifact_delta.insert(key, version);
    }
    if let Some(route) = pending.route.take() {
        event.actions.route = Some(route);
    }
}

/// A terminal event raised by a tool's control signals (escalation,
/// skip-summarization, or transfer). Clears the pending checkpoint and forwards
/// the signals to the Runner, mirroring LlmAgent's terminal tool actions.
fn control_terminal_event(invocation_id: &str, agent_name: &str, control: &ToolControl) -> Event {
    let mut event = Event::new(invocation_id);
    event.author = agent_name.to_string();
    event.actions.escalate = control.escalate;
    event.actions.skip_summarization = control.skip_summarization;
    event.actions.transfer_to_agent = control.transfer_to_agent.clone();
    event.actions.state_delta.insert(PENDING_STATE_KEY.to_string(), Value::Null);
    event
}

fn mk_checkpoint(
    iteration: u32,
    transcript: Vec<Content>,
    snapshot: Vec<u8>,
    call: PendingToolCall,
    disposition: Disposition,
    roster: &[String],
) -> CodeActCheckpoint {
    CodeActCheckpoint {
        iteration,
        transcript,
        snapshot,
        call,
        disposition,
        tool_roster: roster.to_vec(),
    }
}

fn resolution_to_resume(resolution: ResolutionRecord) -> ResumeWith {
    match resolution {
        ResolutionRecord::Value(v) => ResumeWith::Value(v),
        ResolutionRecord::Raise(m) => ResumeWith::Raise(m),
    }
}

/// Convert a freshly-produced [`ResumeWith`] into a persistable
/// [`ResolutionRecord`] for a SAVE-AFTER checkpoint.
fn resume_to_record(with: ResumeWith) -> ResolutionRecord {
    match with {
        ResumeWith::Value(v) => ResolutionRecord::Value(v),
        ResumeWith::Raise(m) => ResolutionRecord::Raise(m),
    }
}

/// Extract a tool result value from callback-supplied [`Content`].
///
/// A `FunctionResponse` part's payload is used directly; otherwise the
/// concatenated text is wrapped as a JSON string so the script always receives
/// a value to continue with.
fn content_to_tool_value(content: &Content) -> Value {
    for part in &content.parts {
        if let Part::FunctionResponse { function_response, .. } = part {
            return function_response.response.clone();
        }
    }
    let text: String = content.parts.iter().filter_map(|p| p.text()).collect();
    Value::String(text)
}

/// A durability event: persists the checkpoint to session state, no content.
///
/// Serializing a [`CodeActCheckpoint`] cannot fail (it is plain data), so a
/// serialization error here is a programming bug, not a recoverable condition.
fn checkpoint_event(invocation_id: &str, agent_name: &str, cp: &CodeActCheckpoint) -> Event {
    let mut event = Event::new(invocation_id);
    event.author = agent_name.to_string();
    let value = serde_json::to_value(cp).expect("CodeActCheckpoint serialization cannot fail");
    event.actions.state_delta.insert(PENDING_STATE_KEY.to_string(), value);
    event
}

/// A suspend event: persists the checkpoint and carries the HITL / long-running
/// signal so the caller knows what's awaited.
fn suspend_event(invocation_id: &str, agent_name: &str, cp: &CodeActCheckpoint) -> Event {
    let mut event = checkpoint_event(invocation_id, agent_name, cp);
    match &cp.disposition {
        Disposition::AwaitingConfirmation => {
            event.actions.tool_confirmation = Some(ToolConfirmationRequest {
                tool_name: cp.call.tool.clone(),
                function_call_id: Some(cp.call.call_id.to_string()),
                args: cp.call.args.clone(),
            });
            // Mark the turn as interrupted and carry a human-readable prompt,
            // mirroring LlmAgent's confirmation interrupt event.
            event.llm_response.interrupted = true;
            event.llm_response.turn_complete = true;
            event.llm_response.content = Some(Content::new("model").with_text(format!(
                "Tool confirmation required for '{}'. Provide approve/deny decision to continue.",
                cp.call.tool
            )));
        }
        Disposition::AwaitingCompletion { .. } => {
            event.long_running_tool_ids = vec![cp.call.tool.clone()];
            let mut content = Content::new("model");
            content.parts.push(Part::FunctionCall {
                name: cp.call.tool.clone(),
                args: cp.call.args.clone(),
                id: Some(cp.call.call_id.to_string()),
                thought_signature: None,
            });
            event.llm_response.content = Some(content);
        }
        Disposition::PendingResult | Disposition::Resolved(_) => {}
    }
    event
}

/// A final result event: clears the pending checkpoint from session state.
fn final_event(invocation_id: &str, agent_name: &str, value: &Value) -> Event {
    terminal_event(invocation_id, agent_name, &render_final(value))
}

/// A terminal event with the given text that clears the pending checkpoint.
fn terminal_event(invocation_id: &str, agent_name: &str, text: &str) -> Event {
    let mut event = Event::new(invocation_id);
    event.author = agent_name.to_string();
    event.llm_response.content = Some(Content::new("model").with_text(text));
    event.actions.state_delta.insert(PENDING_STATE_KEY.to_string(), Value::Null);
    event
}

/// A transfer event: hands control to `target` and clears the pending
/// checkpoint. Mirrors how [`LlmAgent`](crate::LlmAgent) signals a transfer via
/// `EventActions::transfer_to_agent`.
fn transfer_event(invocation_id: &str, agent_name: &str, target: &str) -> Event {
    let mut event = Event::new(invocation_id);
    event.author = agent_name.to_string();
    event.actions.transfer_to_agent = Some(target.to_string());
    event.actions.state_delta.insert(PENDING_STATE_KEY.to_string(), Value::Null);
    event
}

/// The prompt section that reveals the `transfer_to_agent` output and lists the
/// valid targets. Appended to the system prompt only when targets exist.
fn transfer_prompt(targets: &[String]) -> String {
    format!(
        "You may also hand the task to another agent by returning a value tagged \
\"transfer_to_agent\":\n- {{\"type\": \"transfer_to_agent\", \"agent_name\": \"...\"}}: \
transfer control to that agent; this ends your turn.\nValid agents: {}.",
        roster_list(targets)
    )
}

/// Render a list of agent names for a prompt or error message.
fn roster_list(targets: &[String]) -> String {
    targets.join(", ")
}

/// The model-call hooks applied around each [`next_script`] request: legacy
/// before/after-model callbacks plus the optional enhanced-plugin pipeline.
struct ModelHooks<'a> {
    before: &'a [BeforeModelCallback],
    after: &'a [AfterModelCallback],
    ctx: &'a Arc<dyn CallbackContext>,
    #[cfg(feature = "enhanced-plugins")]
    plugins: Option<&'a EnhancedPluginManager>,
}

/// Pull one script out of the model and extract the fenced code block.
///
/// The request passes through before-model hooks (enhanced plugins, then legacy
/// `before_model_callback`s), either of which may rewrite it or short-circuit
/// with a synthetic response; the response then passes through after-model hooks
/// (legacy `after_model_callback`s, then enhanced plugins).
async fn next_script(
    model: &dyn Llm,
    transcript: &[Content],
    config: &Option<GenerateContentConfig>,
    hooks: &ModelHooks<'_>,
) -> Result<String, AdkError> {
    let mut request = LlmRequest::new(model.name().to_string(), transcript.to_vec());
    if let Some(config) = config {
        request = request.with_config(config.clone());
    }

    let mut content: Option<Content> = None;

    // before-model plugins: modify the request or short-circuit.
    #[cfg(feature = "enhanced-plugins")]
    if let Some(epm) = hooks.plugins {
        match epm.run_before_model_call(request.clone(), hooks.ctx.clone()).await? {
            BeforeModelCallResult::Continue(modified) => request = modified,
            BeforeModelCallResult::ShortCircuit(response) => content = response.content,
        }
    }

    // before-model callbacks: rewrite the request or skip the call with a response.
    if content.is_none() {
        for callback in hooks.before {
            match callback(hooks.ctx.clone(), request.clone()).await? {
                BeforeModelResult::Continue(modified) => request = modified,
                BeforeModelResult::Skip(response) => {
                    content = response.content;
                    break;
                }
            }
        }
    }

    if content.is_none() {
        let mut stream = model.generate_content(request, false).await?;
        let mut text = String::new();
        while let Some(chunk) = stream.next().await {
            if let Some(c) = chunk?.content {
                for part in &c.parts {
                    if let Part::Text { text: t } = part {
                        text.push_str(t);
                    }
                }
            }
        }
        content = Some(Content::new("model").with_text(text));
    }

    // after-model callbacks: rewrite the accumulated response.
    for callback in hooks.after {
        let response = LlmResponse { content: content.clone(), ..Default::default() };
        if let Some(modified) = callback(hooks.ctx.clone(), response).await? {
            content = modified.content;
        }
    }

    // after-model plugins: modify the response.
    #[cfg(feature = "enhanced-plugins")]
    if let Some(epm) = hooks.plugins {
        let response = LlmResponse { content: content.clone(), ..Default::default() };
        let AfterModelCallResult::Continue(modified) =
            epm.run_after_model_call(response, hooks.ctx.clone()).await?;
        content = modified.content;
    }

    let text = content
        .map(|c| c.parts.iter().filter_map(|p| p.text()).collect::<Vec<_>>().join(""))
        .unwrap_or_default();
    Ok(extract_code_block(&text))
}

/// Extract the body of the first fenced code block, skipping any language tag on
/// the fence line. Falls back to the trimmed input when no fence is present.
pub fn extract_code_block(text: &str) -> String {
    if let Some(start) = text.find("```") {
        let after = &text[start + 3..];
        if let Some(newline) = after.find('\n') {
            let body = &after[newline + 1..];
            if let Some(end) = body.find("```") {
                return body[..end].trim().to_string();
            }
        }
    }
    text.trim().to_string()
}

/// Find the `FunctionResponse` carrying a long-running tool's result in an
/// incoming message (how the result is delivered on the next invocation).
///
/// To avoid resuming the wrong continuation when there are stale responses or
/// repeated calls to the same tool, a response whose `id` matches the awaited
/// `call_id` is preferred. A response that carries no id falls back to matching
/// on the tool name alone.
fn completion_for(content: &Content, tool: &str, call_id: u64) -> Option<Value> {
    let want_id = call_id.to_string();
    let by_id = content.parts.iter().find_map(|part| match part {
        Part::FunctionResponse { function_response, id }
            if function_response.name == tool && id.as_deref() == Some(want_id.as_str()) =>
        {
            Some(function_response.response.clone())
        }
        _ => None,
    });
    if by_id.is_some() {
        return by_id;
    }
    content.parts.iter().find_map(|part| match part {
        Part::FunctionResponse { function_response, id }
            if function_response.name == tool && id.is_none() =>
        {
            Some(function_response.response.clone())
        }
        _ => None,
    })
}

fn roster(tools: &ToolMap) -> Vec<String> {
    let mut names: Vec<String> = tools.keys().cloned().collect();
    names.sort();
    names
}

fn observation_content(value: &Value) -> Content {
    Content::new("user").with_text(format!("Observation:\n{}", render_value(value)))
}

fn error_content(message: &str) -> Content {
    Content::new("user").with_text(format!("Error during execution:\n{message}"))
}

/// Surface captured stdout (`print` output) back to the model.
fn stdout_content(output: &str) -> Content {
    Content::new("user").with_text(format!("Output (stdout):\n{output}"))
}

/// A *copy* of `transcript` with any stdout printed this turn appended, for
/// baking into a checkpoint.
///
/// The live transcript is left untouched — stdout is flushed onto it at the end
/// of the turn instead. Persisting the same output into the checkpoint ensures
/// anything the script printed *before* a suspend (HITL confirmation, a
/// long-running deferral) or an inline write-ahead checkpoint survives the
/// suspend/resume boundary and crash recovery, without being counted twice on
/// the in-process path (a resumed continuation only re-emits output produced
/// after the call boundary).
fn transcript_with_stdout(transcript: &[Content], stdout: &str, max: usize) -> Vec<Content> {
    let mut out = transcript.to_vec();
    if !stdout.is_empty() {
        out.push(stdout_content(&truncate_middle(stdout, max)));
    }
    out
}

fn truncate_middle(message: &str, max: usize) -> String {
    if message.chars().count() <= max {
        return message.to_string();
    }
    let head: String = message.chars().take(max / 2).collect();
    let tail: String =
        message.chars().rev().take(max / 2).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{head}\n...[truncated]...\n{tail}")
}

/// Validate a JSON value against an output schema, collecting all errors.
fn validate_against_schema(value: &Value, schema: &Value) -> Result<(), String> {
    let validator =
        jsonschema::validator_for(schema).map_err(|e| format!("invalid output schema: {e}"))?;
    let errors: Vec<String> = validator.iter_errors(value).map(|e| e.to_string()).collect();
    if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) }
}

fn render_final(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    }
}

fn runtime_err(err: RuntimeError) -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::Internal,
        "codeact.runtime",
        err.to_string(),
    )
}

// ===========================================================================
// Agent
// ===========================================================================

/// A configured CodeAct agent.
///
/// Build one with [`CodeActAgent::builder`], supplying a [`Llm`] and a
/// [`CodeRuntime`]. The agent then drives the CodeAct loop: each turn the model
/// writes one script, tools are exposed as callable functions, and the script
/// returns a tagged [`ScriptOutput`](crate::codeact::ScriptOutput).
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use adk_agent::codeact::CodeActAgent;
///
/// // `model` implements `adk_core::Llm`; `runtime` implements `CodeRuntime`
/// // (e.g. a Monty-backed Python interpreter).
/// let agent = CodeActAgent::builder()
///     .name("analyst")
///     .model(model)
///     .runtime(runtime)
///     .instruction("Prefer pandas-style transformations.")
///     .tool(Arc::new(load_csv_tool))
///     .output_key("report")
///     .build()?;
/// # Ok::<(), adk_core::AdkError>(())
/// ```
pub struct CodeActAgent {
    name: String,
    description: String,
    model: Arc<dyn Llm>,
    tools: Vec<Arc<dyn Tool>>,
    toolsets: Vec<Arc<dyn Toolset>>,
    runtime: Arc<dyn CodeRuntime>,
    policy: ToolConfirmationPolicy,
    skills_index: Option<Arc<SkillIndex>>,
    skill_policy: SelectionPolicy,
    max_skill_chars: usize,
    instruction: Option<String>,
    instruction_provider: Option<Arc<InstructionProvider>>,
    global_instruction: Option<String>,
    global_instruction_provider: Option<Arc<GlobalInstructionProvider>>,
    include_contents: IncludeContents,
    max_iterations: u32,
    max_error_chars: usize,
    supports_suspension: bool,
    sub_agents: Vec<Arc<dyn Agent>>,
    disallow_transfer_to_parent: bool,
    disallow_transfer_to_peers: bool,
    generate_content_config: Option<GenerateContentConfig>,
    tool_timeout: Duration,
    output_key: Option<String>,
    before_callbacks: Arc<Vec<BeforeAgentCallback>>,
    after_callbacks: Arc<Vec<AfterAgentCallback>>,
    before_model_callbacks: Arc<Vec<BeforeModelCallback>>,
    after_model_callbacks: Arc<Vec<AfterModelCallback>>,
    before_tool_callbacks: Arc<Vec<BeforeToolCallback>>,
    after_tool_callbacks: Arc<Vec<AfterToolCallback>>,
    after_tool_callbacks_full: Arc<Vec<AfterToolCallbackFull>>,
    default_retry_budget: Option<RetryBudget>,
    tool_retry_budgets: HashMap<String, RetryBudget>,
    circuit_breaker_threshold: Option<u32>,
    on_tool_error: Arc<Vec<OnToolErrorCallback>>,
    input_schema: Option<Value>,
    output_schema: Option<Value>,
    output_max_retries: usize,
    input_guardrails: Arc<GuardrailSet>,
    output_guardrails: Arc<GuardrailSet>,
    #[cfg(feature = "enhanced-plugins")]
    enhanced_plugin_manager: Option<Arc<EnhancedPluginManager>>,
}

impl CodeActAgent {
    /// Start building a [`CodeActAgent`].
    pub fn builder() -> CodeActAgentBuilder {
        CodeActAgentBuilder::new()
    }

    /// The JSON schema describing this agent's input, if set.
    ///
    /// Like `LlmAgent`, this is descriptive metadata (e.g. for exposing the
    /// agent as a tool); it is not enforced on the incoming user message.
    pub fn input_schema(&self) -> Option<&Value> {
        self.input_schema.as_ref()
    }

    /// Resolve the global instruction for this invocation (dynamic provider or
    /// static `{state.key}` template), or `None` when unset/empty.
    async fn resolve_global_instruction(
        &self,
        ctx: &Arc<dyn InvocationContext>,
    ) -> adk_core::Result<Option<String>> {
        if let Some(provider) = &self.global_instruction_provider {
            let text = provider(ctx.clone() as Arc<dyn ReadonlyContext>).await?;
            Ok((!text.is_empty()).then_some(text))
        } else if let Some(template) = &self.global_instruction {
            let text = adk_core::inject_session_state(ctx.as_ref(), template).await?;
            Ok((!text.is_empty()).then_some(text))
        } else {
            Ok(None)
        }
    }

    /// Resolve the agent instruction for this invocation.
    async fn resolve_instruction(
        &self,
        ctx: &Arc<dyn InvocationContext>,
    ) -> adk_core::Result<Option<String>> {
        if let Some(provider) = &self.instruction_provider {
            let text = provider(ctx.clone() as Arc<dyn ReadonlyContext>).await?;
            Ok((!text.is_empty()).then_some(text))
        } else if let Some(template) = &self.instruction {
            let text = adk_core::inject_session_state(ctx.as_ref(), template).await?;
            Ok((!text.is_empty()).then_some(text))
        } else {
            Ok(None)
        }
    }

    /// Resolve the callable tool set for this invocation: static tools plus the
    /// tools each [`Toolset`] yields for `ctx`. Rejects duplicate names
    /// (static-vs-toolset and toolset-vs-toolset), mirroring `LlmAgent`.
    async fn resolve_tools(
        &self,
        ctx: &Arc<dyn InvocationContext>,
    ) -> adk_core::Result<Vec<Arc<dyn Tool>>> {
        if self.toolsets.is_empty() {
            return Ok(self.tools.clone());
        }
        let mut resolved = self.tools.clone();
        let static_names: std::collections::HashSet<String> =
            self.tools.iter().map(|t| t.name().to_string()).collect();
        let mut source: HashMap<String, String> = HashMap::new();
        for toolset in &self.toolsets {
            let provided = toolset.tools(ctx.clone() as Arc<dyn ReadonlyContext>).await?;
            for tool in provided {
                let name = tool.name().to_string();
                if static_names.contains(&name) {
                    return Err(AdkError::agent(format!(
                        "duplicate tool name '{name}': conflict between a static tool and toolset '{}'",
                        toolset.name()
                    )));
                }
                if let Some(other) = source.get(&name) {
                    return Err(AdkError::agent(format!(
                        "duplicate tool name '{name}': conflict between toolset '{other}' and toolset '{}'",
                        toolset.name()
                    )));
                }
                source.insert(name, toolset.name().to_string());
                resolved.push(tool);
            }
        }
        Ok(resolved)
    }

    /// Apply input guardrails to the incoming user content. Returns a context
    /// whose user content is the (possibly transformed) result, or an error if
    /// the content is blocked. A no-op when no input guardrails are set.
    async fn apply_input_guardrails(
        &self,
        ctx: Arc<dyn InvocationContext>,
    ) -> adk_core::Result<Arc<dyn InvocationContext>> {
        if self.input_guardrails.is_empty() {
            return Ok(ctx);
        }
        let content =
            enforce_guardrails(self.input_guardrails.as_ref(), ctx.user_content(), "input").await?;
        if content.role != ctx.user_content().role || content.parts != ctx.user_content().parts {
            Ok(crate::workflow::with_user_content_override(ctx, content))
        } else {
            Ok(ctx)
        }
    }

    /// Select the most relevant skill's prompt block for this invocation's
    /// user query, if a skills index is configured. Returns `None` when skills
    /// are unset, disabled, or nothing meets the policy threshold.
    fn select_skill_block(&self, ctx: &Arc<dyn InvocationContext>) -> Option<String> {
        let index = self.skills_index.as_ref()?;
        let query =
            ctx.user_content().parts.iter().filter_map(|p| p.text()).collect::<Vec<_>>().join("\n");
        select_skill_prompt_block(index.as_ref(), &query, &self.skill_policy, self.max_skill_chars)
            .map(|(_, block)| block)
    }

    /// Resolve the conversation that seeds the transcript, honoring
    /// [`IncludeContents`] and transfer history filtering. Always ends with the
    /// current user turn.
    fn resolve_conversation(&self, ctx: &Arc<dyn InvocationContext>) -> Vec<Content> {
        let current = ctx.user_content().clone();
        match self.include_contents {
            IncludeContents::None => vec![current],
            IncludeContents::Default => {
                let mut history = if ctx.run_config().transfer_targets.is_empty() {
                    ctx.session().conversation_history()
                } else {
                    ctx.session().conversation_history_for_agent(&self.name)
                };
                // Session history already contains the current user message;
                // ensure the latest user turn reflects the current content.
                if let Some(idx) = history.iter().rposition(|c| c.role == "user") {
                    history[idx] = current;
                } else {
                    history.push(current);
                }
                history
            }
        }
    }
}

impl std::fmt::Debug for CodeActAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeActAgent")
            .field("name", &self.name)
            .field("tools", &self.tools.len())
            .field("supports_suspension", &self.supports_suspension)
            .finish()
    }
}

#[async_trait]
impl Agent for CodeActAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &self.sub_agents
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> adk_core::Result<EventStream> {
        // Input guardrails run first; a block aborts the run, a transform
        // (e.g. PII redaction) rewrites the user content downstream.
        let ctx = self.apply_input_guardrails(ctx).await?;

        let pending = ctx
            .session()
            .state()
            .get(PENDING_STATE_KEY)
            .and_then(|v| serde_json::from_value(v).ok());

        // Valid transfer targets: sub-agents (always) plus runner-provided
        // targets (parent/peers) from RunConfig, de-duplicated and filtered by
        // the `disallow_transfer_to_parent`/`disallow_transfer_to_peers` flags
        // (which apply only to the runner-provided parent/peers, never to
        // sub-agents). Mirrors LlmAgent.
        let mut transfer_targets: Vec<String> =
            self.sub_agents.iter().map(|a| a.name().to_string()).collect();
        let parent = ctx.run_config().parent_agent.as_deref();
        for target in &ctx.run_config().transfer_targets {
            if transfer_targets.contains(target) {
                continue;
            }
            let is_parent = parent == Some(target.as_str());
            if is_parent && self.disallow_transfer_to_parent {
                continue;
            }
            if !is_parent && self.disallow_transfer_to_peers {
                continue;
            }
            transfer_targets.push(target.clone());
        }

        let invocation_id = ctx.invocation_id().to_string();
        let agent_name = self.name.clone();

        // Resolve toolsets for this invocation and merge with static tools,
        // rejecting name conflicts (mirrors LlmAgent).
        let resolved_tools = self.resolve_tools(&ctx).await?;

        // Resolve instructions per-invocation (providers are async, statics get
        // `{state.key}` template injection), then assemble the full prompt.
        let global = self.resolve_global_instruction(&ctx).await?;
        let instruction = self.resolve_instruction(&ctx).await?;
        let transfer_section =
            (!transfer_targets.is_empty()).then(|| transfer_prompt(&transfer_targets));
        let skill_block = self.select_skill_block(&ctx);
        let mut system_prompt = assemble_system_prompt(
            skill_block.as_deref(),
            global.as_deref(),
            instruction.as_deref(),
            &self.runtime.capabilities().prompt,
            &self.runtime.render_tools(&resolved_tools),
            transfer_section.as_deref(),
        );
        // Tell the model the final result must conform to the output schema.
        if let Some(schema) = &self.output_schema {
            system_prompt.push_str(&format!(
                "\n\nYour final_result value MUST be valid JSON conforming to this schema:\n{schema}"
            ));
        }

        let conversation = self.resolve_conversation(&ctx);

        let input = LoopInputs {
            model: self.model.clone(),
            runtime: self.runtime.clone(),
            tools: resolved_tools,
            policy: self.policy.clone(),
            decisions: ctx.run_config().tool_confirmation_decisions.clone(),
            invocation_ctx: ctx.clone(),
            incoming: ctx.user_content().clone(),
            system_prompt,
            conversation,
            pending,
            invocation_id: invocation_id.clone(),
            agent_name: agent_name.clone(),
            max_iterations: self.max_iterations,
            max_error_chars: self.max_error_chars,
            supports_suspension: self.supports_suspension,
            transfer_targets,
            generate_content_config: self.generate_content_config.clone(),
            tool_timeout: self.tool_timeout,
            output_key: self.output_key.clone(),
            default_retry_budget: self.default_retry_budget.clone(),
            tool_retry_budgets: self.tool_retry_budgets.clone(),
            circuit_breaker_threshold: self.circuit_breaker_threshold,
            on_tool_error: self.on_tool_error.clone(),
            before_model_callbacks: self.before_model_callbacks.clone(),
            after_model_callbacks: self.after_model_callbacks.clone(),
            before_tool_callbacks: self.before_tool_callbacks.clone(),
            after_tool_callbacks: self.after_tool_callbacks.clone(),
            after_tool_callbacks_full: self.after_tool_callbacks_full.clone(),
            output_schema: self.output_schema.clone(),
            output_max_retries: self.output_max_retries,
            output_guardrails: self.output_guardrails.clone(),
            #[cfg(feature = "enhanced-plugins")]
            enhanced_plugin_manager: self.enhanced_plugin_manager.clone(),
        };

        let before = self.before_callbacks.clone();
        let after = self.after_callbacks.clone();
        // Agent-level callbacks receive the invocation context as a callback
        // context (upcast), exactly like LlmAgent.
        let cb_ctx = ctx as Arc<dyn CallbackContext>;

        Ok(Box::pin(run_with_agent_callbacks(
            input,
            before,
            after,
            cb_ctx,
            invocation_id,
            agent_name,
        )))
    }
}

/// Wrap the CodeAct loop with before/after-agent callbacks, mirroring LlmAgent:
///
/// - Before-agent callbacks run first; the first one returning `Some(content)`
///   short-circuits the loop (its content is emitted, then after-agent
///   callbacks run, then the run ends).
/// - Otherwise the loop runs; after-agent callbacks run only when the loop
///   *completes normally* (a final result) — not when it suspends, errors, or
///   ends via a transfer/escalation. The first after-callback returning `Some`
///   wins.
fn run_with_agent_callbacks(
    input: LoopInputs,
    before: Arc<Vec<BeforeAgentCallback>>,
    after: Arc<Vec<AfterAgentCallback>>,
    cb_ctx: Arc<dyn CallbackContext>,
    invocation_id: String,
    agent_name: String,
) -> impl Stream<Item = adk_core::Result<Event>> {
    stream! {
        // ----- before-agent callbacks -----
        for callback in before.iter() {
            match callback(cb_ctx.clone()).await {
                Ok(Some(content)) => {
                    yield Ok(callback_event(&invocation_id, &agent_name, content));
                    // Short-circuit: run after-agent callbacks, then stop.
                    for after_cb in after.iter() {
                        match after_cb(cb_ctx.clone()).await {
                            Ok(Some(c)) => {
                                yield Ok(callback_event(&invocation_id, &agent_name, c));
                                return;
                            }
                            Ok(None) => continue,
                            Err(e) => {
                                yield Err(e);
                                return;
                            }
                        }
                    }
                    return;
                }
                Ok(None) => continue,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            }
        }

        // ----- main loop -----
        // Track how the loop ended. The last event that touched the pending
        // checkpoint cleared it (Null) on a terminal turn, but set it on suspend.
        // A transfer/escalation/skip-summarization also clears it but must NOT
        // run after-agent callbacks (control is handed off / the run is told to
        // stop), matching LlmAgent's early return on those terminal actions.
        let mut completed = false;
        let mut errored = false;
        let mut handed_off = false;
        let mut inner = Box::pin(run_codeact(input));
        while let Some(item) = inner.next().await {
            match &item {
                Ok(event) => {
                    if let Some(value) = event.actions.state_delta.get(PENDING_STATE_KEY) {
                        completed = value.is_null();
                    }
                    if event.actions.escalate
                        || event.actions.skip_summarization
                        || event.actions.transfer_to_agent.is_some()
                    {
                        handed_off = true;
                    }
                }
                Err(_) => errored = true,
            }
            yield item;
        }

        // ----- after-agent callbacks -----
        if !errored && completed && !handed_off {
            for after_cb in after.iter() {
                match after_cb(cb_ctx.clone()).await {
                    Ok(Some(c)) => {
                        yield Ok(callback_event(&invocation_id, &agent_name, c));
                        break;
                    }
                    Ok(None) => continue,
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
            }
        }
    }
}

/// An event carrying content produced by an agent-level callback.
fn callback_event(invocation_id: &str, agent_name: &str, content: Content) -> Event {
    let mut event = Event::new(invocation_id);
    event.author = agent_name.to_string();
    event.llm_response.content = Some(content);
    event
}

/// Builder for [`CodeActAgent`].
///
/// [`model`](Self::model) and [`runtime`](Self::runtime) are required; every
/// other setting has a default. The configuration surface mirrors
/// [`LlmAgentBuilder`](crate::LlmAgentBuilder) (instructions, tools, toolsets,
/// retries, guardrails, callbacks, transfer, output schema, ...).
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use std::time::Duration;
/// use adk_agent::codeact::CodeActAgent;
///
/// let agent = CodeActAgent::builder()
///     .name("assistant")
///     .model(model)
///     .runtime(runtime)
///     .temperature(0.2)
///     .tool_timeout(Duration::from_secs(30))
///     .require_tool_confirmation("delete_file")
///     .build()?;
/// # Ok::<(), adk_core::AdkError>(())
/// ```
pub struct CodeActAgentBuilder {
    name: String,
    description: String,
    model: Option<Arc<dyn Llm>>,
    tools: Vec<Arc<dyn Tool>>,
    toolsets: Vec<Arc<dyn Toolset>>,
    runtime: Option<Arc<dyn CodeRuntime>>,
    policy: ToolConfirmationPolicy,
    skills_index: Option<Arc<SkillIndex>>,
    skill_policy: SelectionPolicy,
    max_skill_chars: usize,
    instruction: Option<String>,
    instruction_provider: Option<Arc<InstructionProvider>>,
    global_instruction: Option<String>,
    global_instruction_provider: Option<Arc<GlobalInstructionProvider>>,
    include_contents: IncludeContents,
    max_iterations: u32,
    max_error_chars: usize,
    sub_agents: Vec<Arc<dyn Agent>>,
    disallow_transfer_to_parent: bool,
    disallow_transfer_to_peers: bool,
    generate_content_config: Option<GenerateContentConfig>,
    tool_timeout: Duration,
    output_key: Option<String>,
    before_callbacks: Vec<BeforeAgentCallback>,
    after_callbacks: Vec<AfterAgentCallback>,
    before_model_callbacks: Vec<BeforeModelCallback>,
    after_model_callbacks: Vec<AfterModelCallback>,
    before_tool_callbacks: Vec<BeforeToolCallback>,
    after_tool_callbacks: Vec<AfterToolCallback>,
    after_tool_callbacks_full: Vec<AfterToolCallbackFull>,
    default_retry_budget: Option<RetryBudget>,
    tool_retry_budgets: HashMap<String, RetryBudget>,
    circuit_breaker_threshold: Option<u32>,
    on_tool_error: Vec<OnToolErrorCallback>,
    input_schema: Option<Value>,
    output_schema: Option<Value>,
    output_max_retries: usize,
    input_guardrails: GuardrailSet,
    output_guardrails: GuardrailSet,
    #[cfg(feature = "enhanced-plugins")]
    enhanced_plugins: Vec<Arc<dyn EnhancedPlugin>>,
}

impl CodeActAgentBuilder {
    /// Create a builder with default settings.
    pub fn new() -> Self {
        Self {
            name: "code-agent".to_string(),
            description: "A CodeAct agent that acts by writing and running code.".to_string(),
            model: None,
            tools: Vec::new(),
            toolsets: Vec::new(),
            runtime: None,
            policy: ToolConfirmationPolicy::Never,
            skills_index: None,
            skill_policy: SelectionPolicy::default(),
            max_skill_chars: DEFAULT_MAX_SKILL_CHARS,
            instruction: None,
            instruction_provider: None,
            global_instruction: None,
            global_instruction_provider: None,
            include_contents: IncludeContents::default(),
            max_iterations: DEFAULT_MAX_ITERATIONS,
            max_error_chars: DEFAULT_MAX_ERROR_CHARS,
            sub_agents: Vec::new(),
            disallow_transfer_to_parent: false,
            disallow_transfer_to_peers: false,
            generate_content_config: None,
            tool_timeout: DEFAULT_TOOL_TIMEOUT,
            output_key: None,
            before_callbacks: Vec::new(),
            after_callbacks: Vec::new(),
            before_model_callbacks: Vec::new(),
            after_model_callbacks: Vec::new(),
            before_tool_callbacks: Vec::new(),
            after_tool_callbacks: Vec::new(),
            after_tool_callbacks_full: Vec::new(),
            default_retry_budget: None,
            tool_retry_budgets: HashMap::new(),
            circuit_breaker_threshold: None,
            on_tool_error: Vec::new(),
            input_schema: None,
            output_schema: None,
            output_max_retries: DEFAULT_OUTPUT_MAX_RETRIES,
            input_guardrails: GuardrailSet::new(),
            output_guardrails: GuardrailSet::new(),
            #[cfg(feature = "enhanced-plugins")]
            enhanced_plugins: Vec::new(),
        }
    }

    /// Set the agent name.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Set the agent description.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Set the model (required).
    pub fn model(mut self, model: Arc<dyn Llm>) -> Self {
        self.model = Some(model);
        self
    }

    /// Set the code runtime (required).
    pub fn runtime(mut self, runtime: Arc<dyn CodeRuntime>) -> Self {
        self.runtime = Some(runtime);
        self
    }

    /// Register a tool callable from scripts.
    pub fn tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// Add a sub-agent this agent may transfer control to.
    ///
    /// When any sub-agent is present (or the Runner supplies transfer targets),
    /// the [`transfer_to_agent`](crate::codeact::ScriptOutput::TransferToAgent)
    /// output is described to the model.
    pub fn sub_agent(mut self, agent: Arc<dyn Agent>) -> Self {
        self.sub_agents.push(agent);
        self
    }

    /// Add a toolset whose tools are resolved fresh on each invocation from the
    /// run's [`ReadonlyContext`] (e.g. per-user or pooled tools). Resolved tools
    /// are merged with static tools; duplicate names are rejected at run time.
    pub fn toolset(mut self, toolset: Arc<dyn Toolset>) -> Self {
        self.toolsets.push(toolset);
        self
    }

    /// Set a preloaded skills index. The most relevant skill for each user
    /// query is injected into the system prompt.
    #[cfg(feature = "skills")]
    pub fn with_skills(mut self, index: SkillIndex) -> Self {
        self.skills_index = Some(Arc::new(index));
        self
    }

    /// Auto-load skills from `.skills/` in the current working directory.
    ///
    /// # Errors
    /// Returns an error if the skills directory cannot be loaded.
    #[cfg(feature = "skills")]
    pub fn with_auto_skills(self) -> adk_core::Result<Self> {
        self.with_skills_from_root(".")
    }

    /// Load skills from `.skills/` under a custom root directory.
    ///
    /// # Errors
    /// Returns an error if the skills directory cannot be loaded.
    #[cfg(feature = "skills")]
    pub fn with_skills_from_root(
        mut self,
        root: impl AsRef<std::path::Path>,
    ) -> adk_core::Result<Self> {
        let index = crate::skill_shim::load_skill_index(root)
            .map_err(|e| AdkError::agent(e.to_string()))?;
        self.skills_index = Some(Arc::new(index));
        Ok(self)
    }

    /// Customize skill selection behavior.
    #[cfg(feature = "skills")]
    pub fn with_skill_policy(mut self, policy: SelectionPolicy) -> Self {
        self.skill_policy = policy;
        self
    }

    /// Limit injected skill content length (default 2000 chars).
    #[cfg(feature = "skills")]
    pub fn with_skill_budget(mut self, max_chars: usize) -> Self {
        self.max_skill_chars = max_chars;
        self
    }

    /// Prevent this agent from transferring control back to its parent.
    ///
    /// Applies only to the parent the Runner supplies via
    /// `RunConfig::parent_agent`; sub-agents are unaffected.
    pub fn disallow_transfer_to_parent(mut self, disallow: bool) -> Self {
        self.disallow_transfer_to_parent = disallow;
        self
    }

    /// Prevent this agent from transferring control to peer agents.
    ///
    /// Applies only to the runner-provided peers in `RunConfig::transfer_targets`;
    /// sub-agents are unaffected.
    pub fn disallow_transfer_to_peers(mut self, disallow: bool) -> Self {
        self.disallow_transfer_to_peers = disallow;
        self
    }

    /// Set the tool confirmation policy.
    pub fn tool_confirmation_policy(mut self, policy: ToolConfirmationPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Require human confirmation for a specific tool.
    pub fn require_tool_confirmation(mut self, tool_name: impl Into<String>) -> Self {
        self.policy = self.policy.with_tool(tool_name);
        self
    }

    /// Require human confirmation for every tool call.
    pub fn require_tool_confirmation_for_all(mut self) -> Self {
        self.policy = ToolConfirmationPolicy::Always;
        self
    }

    /// Set the generation config (temperature, tokens, etc.) applied to every
    /// model request.
    pub fn generate_content_config(mut self, config: GenerateContentConfig) -> Self {
        self.generate_content_config = Some(config);
        self
    }

    /// Set the default sampling temperature for model requests.
    pub fn temperature(mut self, temperature: f32) -> Self {
        self.generate_content_config
            .get_or_insert_with(GenerateContentConfig::default)
            .temperature = Some(temperature);
        self
    }

    /// Set the default top-p for model requests.
    pub fn top_p(mut self, top_p: f32) -> Self {
        self.generate_content_config.get_or_insert_with(GenerateContentConfig::default).top_p =
            Some(top_p);
        self
    }

    /// Set the default top-k for model requests.
    pub fn top_k(mut self, top_k: i32) -> Self {
        self.generate_content_config.get_or_insert_with(GenerateContentConfig::default).top_k =
            Some(top_k);
        self
    }

    /// Set the default maximum output tokens for model requests.
    pub fn max_output_tokens(mut self, max_tokens: i32) -> Self {
        self.generate_content_config
            .get_or_insert_with(GenerateContentConfig::default)
            .max_output_tokens = Some(max_tokens);
        self
    }

    /// Set the per-tool execution timeout (default: 5 minutes).
    pub fn tool_timeout(mut self, timeout: Duration) -> Self {
        self.tool_timeout = timeout;
        self
    }

    /// Set a session state key under which the final result value is stored.
    pub fn output_key(mut self, key: impl Into<String>) -> Self {
        self.output_key = Some(key.into());
        self
    }

    /// Add a before-agent callback.
    ///
    /// Callbacks run in registration order before the loop starts; the first to
    /// return `Some(content)` short-circuits the run with that content.
    pub fn before_callback(mut self, callback: BeforeAgentCallback) -> Self {
        self.before_callbacks.push(callback);
        self
    }

    /// Add an after-agent callback.
    ///
    /// Callbacks run after the loop completes (not on suspension); the first to
    /// return `Some(content)` emits that content and wins.
    pub fn after_callback(mut self, callback: AfterAgentCallback) -> Self {
        self.after_callbacks.push(callback);
        self
    }

    /// Add a before-model callback, invoked before each model request.
    ///
    /// A callback may rewrite the request
    /// ([`BeforeModelResult::Continue`](adk_core::BeforeModelResult::Continue))
    /// or skip the model call entirely with a synthetic response
    /// ([`BeforeModelResult::Skip`](adk_core::BeforeModelResult::Skip)).
    pub fn before_model_callback(mut self, callback: BeforeModelCallback) -> Self {
        self.before_model_callbacks.push(callback);
        self
    }

    /// Add an after-model callback, invoked after each model response is
    /// assembled. Returning `Some(response)` replaces the response the loop uses
    /// to extract the next script.
    pub fn after_model_callback(mut self, callback: AfterModelCallback) -> Self {
        self.after_model_callbacks.push(callback);
        self
    }

    /// Add a before-tool callback, invoked before each tool call. The first to
    /// return `Some(content)` short-circuits the call; that content is converted
    /// to a value and fed back into the script as the tool's result.
    pub fn before_tool_callback(mut self, callback: BeforeToolCallback) -> Self {
        self.before_tool_callbacks.push(callback);
        self
    }

    /// Add an after-tool callback, invoked after each tool attempt resolves. The
    /// first to return `Some(content)` replaces the result fed back to the script.
    pub fn after_tool_callback(mut self, callback: AfterToolCallback) -> Self {
        self.after_tool_callbacks.push(callback);
        self
    }

    /// Add a rich after-tool callback receiving the tool, arguments, and response
    /// value. Returning `Some(value)` replaces the result fed back to the script.
    /// These run after the plain [`after_tool_callback`](Self::after_tool_callback)
    /// chain.
    pub fn after_tool_callback_full(mut self, callback: AfterToolCallbackFull) -> Self {
        self.after_tool_callbacks_full.push(callback);
        self
    }

    /// Add an on-tool-error callback. When a tool ultimately fails, callbacks
    /// run in order; the first to return `Some(value)` supplies a fallback
    /// result (fed back as if the tool succeeded). If none do, the error is
    /// raised into the script.
    pub fn on_tool_error(mut self, callback: OnToolErrorCallback) -> Self {
        self.on_tool_error.push(callback);
        self
    }

    /// Set the default retry budget applied to tools without a per-tool override.
    pub fn default_retry_budget(mut self, budget: RetryBudget) -> Self {
        self.default_retry_budget = Some(budget);
        self
    }

    /// Set a per-tool retry budget override (takes precedence over the default).
    pub fn tool_retry_budget(mut self, tool_name: impl Into<String>, budget: RetryBudget) -> Self {
        self.tool_retry_budgets.insert(tool_name.into(), budget);
        self
    }

    /// Set the circuit-breaker threshold: after this many consecutive failures
    /// in one invocation, a tool is short-circuited with an immediate error
    /// until the next run.
    pub fn circuit_breaker_threshold(mut self, threshold: u32) -> Self {
        self.circuit_breaker_threshold = Some(threshold);
        self
    }

    /// Set a JSON schema describing this agent's input (descriptive metadata,
    /// e.g. for exposing the agent as a tool; not enforced at runtime).
    pub fn input_schema(mut self, schema: Value) -> Self {
        self.input_schema = Some(schema);
        self
    }

    /// Require the final result to validate against this JSON schema. On a
    /// mismatch the model is asked to correct it, up to
    /// [`output_max_retries`](Self::output_max_retries) times.
    pub fn output_schema(mut self, schema: Value) -> Self {
        self.output_schema = Some(schema);
        self
    }

    /// Require the final result to match `T`'s JSON schema. Convenience over
    /// [`output_schema`](Self::output_schema) using `schemars`.
    pub fn output_type<T: schemars::JsonSchema>(mut self) -> Self {
        let schema = schemars::schema_for!(T);
        self.output_schema =
            Some(serde_json::to_value(schema).expect("schema serialization cannot fail"));
        self
    }

    /// Set the maximum number of output-schema correction retries (default 3).
    pub fn output_max_retries(mut self, retries: usize) -> Self {
        self.output_max_retries = retries;
        self
    }

    /// Set guardrails applied to the incoming user content before the run. A
    /// block aborts the run; a transform (e.g. PII redaction) rewrites it.
    /// No-op unless the `guardrails` feature is enabled.
    pub fn input_guardrails(mut self, guardrails: GuardrailSet) -> Self {
        self.input_guardrails = guardrails;
        self
    }

    /// Set guardrails applied to the final result before it is emitted.
    /// No-op unless the `guardrails` feature is enabled.
    pub fn output_guardrails(mut self, guardrails: GuardrailSet) -> Self {
        self.output_guardrails = guardrails;
        self
    }

    /// Register an enhanced plugin. Plugins intercept tool and model calls
    /// (modify args/results/requests/responses or short-circuit them), running
    /// in priority order. Requires the `enhanced-plugins` feature.
    #[cfg(feature = "enhanced-plugins")]
    pub fn enhanced_plugin(mut self, plugin: Arc<dyn EnhancedPlugin>) -> Self {
        self.enhanced_plugins.push(plugin);
        self
    }

    /// Register multiple enhanced plugins at once.
    #[cfg(feature = "enhanced-plugins")]
    pub fn enhanced_plugins(mut self, plugins: Vec<Arc<dyn EnhancedPlugin>>) -> Self {
        self.enhanced_plugins.extend(plugins);
        self
    }

    /// Set the agent instruction, placed first in the system prompt. Supports
    /// `{state.key}` template injection from session state.
    pub fn instruction(mut self, instruction: impl Into<String>) -> Self {
        self.instruction = Some(instruction.into());
        self
    }

    /// Set a dynamic instruction provider evaluated per invocation (takes
    /// precedence over the static [`instruction`](Self::instruction)).
    pub fn instruction_provider(mut self, provider: InstructionProvider) -> Self {
        self.instruction_provider = Some(Arc::new(provider));
        self
    }

    /// Set a global instruction placed before the agent instruction. Supports
    /// `{state.key}` template injection.
    pub fn global_instruction(mut self, instruction: impl Into<String>) -> Self {
        self.global_instruction = Some(instruction.into());
        self
    }

    /// Set a dynamic global instruction provider evaluated per invocation (takes
    /// precedence over the static [`global_instruction`](Self::global_instruction)).
    pub fn global_instruction_provider(mut self, provider: GlobalInstructionProvider) -> Self {
        self.global_instruction_provider = Some(Arc::new(provider));
        self
    }

    /// Control which conversation history the agent sees (default: full).
    pub fn include_contents(mut self, include: IncludeContents) -> Self {
        self.include_contents = include;
        self
    }

    /// Set the maximum number of model turns.
    pub fn max_iterations(mut self, max: u32) -> Self {
        self.max_iterations = max;
        self
    }

    /// Set the maximum error-message length fed back to the model.
    pub fn max_error_chars(mut self, max: usize) -> Self {
        self.max_error_chars = max;
        self
    }

    /// Build the [`CodeActAgent`].
    ///
    /// # Errors
    /// Returns a config error if the model or runtime was not set.
    pub fn build(self) -> adk_core::Result<CodeActAgent> {
        let model = self.model.ok_or_else(|| AdkError::config("CodeActAgent requires a model"))?;
        let runtime =
            self.runtime.ok_or_else(|| AdkError::config("CodeActAgent requires a code runtime"))?;

        // Reject duplicate sub-agent names: transfer targets are addressed by
        // name, so duplicates would make routing ambiguous (mirrors LlmAgent).
        let mut seen_names = std::collections::HashSet::new();
        for agent in &self.sub_agents {
            if !seen_names.insert(agent.name()) {
                return Err(AdkError::agent(format!("duplicate sub-agent name: {}", agent.name())));
            }
        }

        let capabilities = runtime.capabilities();
        if !capabilities.supports_suspension {
            let needs_suspension = !matches!(self.policy, ToolConfirmationPolicy::Never)
                || self.tools.iter().any(|t| t.is_long_running());
            if needs_suspension {
                tracing::warn!(
                    agent.name = %self.name,
                    "runtime cannot suspend; confirmations are rejected, long-running tools run inline, and nothing is checkpointed"
                );
            }
        }

        Ok(CodeActAgent {
            name: self.name,
            description: self.description,
            model,
            tools: self.tools,
            toolsets: self.toolsets,
            runtime,
            policy: self.policy,
            skills_index: self.skills_index,
            skill_policy: self.skill_policy,
            max_skill_chars: self.max_skill_chars,
            instruction: self.instruction,
            instruction_provider: self.instruction_provider,
            global_instruction: self.global_instruction,
            global_instruction_provider: self.global_instruction_provider,
            include_contents: self.include_contents,
            max_iterations: self.max_iterations,
            max_error_chars: self.max_error_chars,
            supports_suspension: capabilities.supports_suspension,
            sub_agents: self.sub_agents,
            disallow_transfer_to_parent: self.disallow_transfer_to_parent,
            disallow_transfer_to_peers: self.disallow_transfer_to_peers,
            generate_content_config: self.generate_content_config,
            tool_timeout: self.tool_timeout,
            output_key: self.output_key,
            before_callbacks: Arc::new(self.before_callbacks),
            after_callbacks: Arc::new(self.after_callbacks),
            before_model_callbacks: Arc::new(self.before_model_callbacks),
            after_model_callbacks: Arc::new(self.after_model_callbacks),
            before_tool_callbacks: Arc::new(self.before_tool_callbacks),
            after_tool_callbacks: Arc::new(self.after_tool_callbacks),
            after_tool_callbacks_full: Arc::new(self.after_tool_callbacks_full),
            default_retry_budget: self.default_retry_budget,
            tool_retry_budgets: self.tool_retry_budgets,
            circuit_breaker_threshold: self.circuit_breaker_threshold,
            on_tool_error: Arc::new(self.on_tool_error),
            input_schema: self.input_schema,
            output_schema: self.output_schema,
            output_max_retries: self.output_max_retries,
            input_guardrails: Arc::new(self.input_guardrails),
            output_guardrails: Arc::new(self.output_guardrails),
            // Build the plugin manager only when plugins are registered
            // (zero overhead otherwise).
            #[cfg(feature = "enhanced-plugins")]
            enhanced_plugin_manager: (!self.enhanced_plugins.is_empty())
                .then(|| Arc::new(EnhancedPluginManager::new(self.enhanced_plugins))),
        })
    }
}

impl Default for CodeActAgentBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Assemble the system prompt in priority order, omitting empty sections:
/// selected skill block, global instruction, agent instruction, base CodeAct
/// contract, runtime environment, runtime-rendered tools, and the transfer
/// section.
fn assemble_system_prompt(
    skill: Option<&str>,
    global: Option<&str>,
    instruction: Option<&str>,
    runtime_prompt: &str,
    tools_section: &str,
    transfer_section: Option<&str>,
) -> String {
    [
        skill.unwrap_or_default(),
        global.unwrap_or_default(),
        instruction.unwrap_or_default(),
        CODEACT_SYSTEM_PROMPT,
        runtime_prompt,
        tools_section,
        transfer_section.unwrap_or_default(),
    ]
    .into_iter()
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join("\n\n")
}

/// A per-call [`ToolContext`] for CodeAct tool calls.
///
/// One is built for each tool invocation. It carries the interpreter's call id
/// (so `function_call_id` is meaningful and correlatable) and its own actions
/// buffer, and otherwise delegates every capability — artifacts, memory, shared
/// state, user scopes, and secrets — to the live invocation context, matching
/// `LlmAgent`'s `AgentToolContext` so tools behave identically under either
/// agent.
struct CodeToolContext {
    inner: Arc<dyn InvocationContext>,
    function_call_id: String,
    actions: Mutex<EventActions>,
}

impl CodeToolContext {
    fn new(inner: Arc<dyn InvocationContext>, call_id: u64) -> Self {
        Self {
            inner,
            function_call_id: call_id.to_string(),
            actions: Mutex::new(EventActions::default()),
        }
    }
}

#[async_trait]
impl ReadonlyContext for CodeToolContext {
    fn invocation_id(&self) -> &str {
        self.inner.invocation_id()
    }
    fn agent_name(&self) -> &str {
        self.inner.agent_name()
    }
    fn user_id(&self) -> &str {
        self.inner.user_id()
    }
    fn app_name(&self) -> &str {
        self.inner.app_name()
    }
    fn session_id(&self) -> &str {
        self.inner.session_id()
    }
    fn branch(&self) -> &str {
        self.inner.branch()
    }
    fn user_content(&self) -> &Content {
        self.inner.user_content()
    }
}

#[async_trait]
impl CallbackContext for CodeToolContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        self.inner.artifacts()
    }
    fn shared_state(&self) -> Option<Arc<SharedState>> {
        self.inner.shared_state()
    }
}

#[async_trait]
impl ToolContext for CodeToolContext {
    fn function_call_id(&self) -> &str {
        &self.function_call_id
    }
    fn actions(&self) -> EventActions {
        self.actions.lock().unwrap().clone()
    }
    fn set_actions(&self, actions: EventActions) {
        *self.actions.lock().unwrap() = actions;
    }
    async fn search_memory(&self, query: &str) -> adk_core::Result<Vec<MemoryEntry>> {
        match self.inner.memory() {
            Some(memory) => memory.search(query).await,
            None => Ok(vec![]),
        }
    }
    fn user_scopes(&self) -> Vec<String> {
        self.inner.user_scopes()
    }
    async fn get_secret(&self, name: &str) -> adk_core::Result<Option<String>> {
        self.inner.get_secret(name).await
    }
}

/// Wraps a [`CallbackContext`] to expose a [`ToolOutcome`] to after-tool
/// callbacks via [`CallbackContext::tool_outcome`], matching `LlmAgent`'s
/// after-tool callback surface. Every other capability delegates to `inner`.
struct ToolOutcomeContext {
    inner: Arc<dyn CallbackContext>,
    outcome: ToolOutcome,
}

impl ToolOutcomeContext {
    fn new(inner: Arc<dyn CallbackContext>, outcome: ToolOutcome) -> Self {
        Self { inner, outcome }
    }
}

#[async_trait]
impl ReadonlyContext for ToolOutcomeContext {
    fn invocation_id(&self) -> &str {
        self.inner.invocation_id()
    }
    fn agent_name(&self) -> &str {
        self.inner.agent_name()
    }
    fn user_id(&self) -> &str {
        self.inner.user_id()
    }
    fn app_name(&self) -> &str {
        self.inner.app_name()
    }
    fn session_id(&self) -> &str {
        self.inner.session_id()
    }
    fn branch(&self) -> &str {
        self.inner.branch()
    }
    fn user_content(&self) -> &Content {
        self.inner.user_content()
    }
}

#[async_trait]
impl CallbackContext for ToolOutcomeContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        self.inner.artifacts()
    }
    fn shared_state(&self) -> Option<Arc<SharedState>> {
        self.inner.shared_state()
    }
    fn tool_outcome(&self) -> Option<ToolOutcome> {
        Some(self.outcome.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codeact::test_support::{
        MockInvocationContext, Planned, ScriptedRuntime, call_id_tool, echo_tool,
        escalating_long_running_tool, escalating_tool, failing_tool, fake_agent, fake_toolset,
        flaky_tool, long_running_tool, route_tool, skip_summarization_tool, sleeping_tool,
        state_tool, text_agent,
    };
    use adk_core::FunctionResponseData;
    use serde_json::json;
    use std::sync::Mutex as StdMutex;

    struct FakeLlm {
        script: String,
        calls: StdMutex<u32>,
        seen: StdMutex<Vec<Vec<Content>>>,
        seen_config: StdMutex<Vec<Option<GenerateContentConfig>>>,
    }

    impl FakeLlm {
        fn new(script: &str) -> Arc<Self> {
            Arc::new(Self {
                script: script.to_string(),
                calls: StdMutex::new(0),
                seen: StdMutex::new(vec![]),
                seen_config: StdMutex::new(vec![]),
            })
        }
        fn calls(&self) -> u32 {
            *self.calls.lock().unwrap()
        }
        fn last_config(&self) -> Option<GenerateContentConfig> {
            self.seen_config.lock().unwrap().last().cloned().flatten()
        }
    }

    #[async_trait]
    impl Llm for FakeLlm {
        fn name(&self) -> &str {
            "fake"
        }
        async fn generate_content(
            &self,
            req: LlmRequest,
            _stream: bool,
        ) -> adk_core::Result<adk_core::LlmResponseStream> {
            *self.calls.lock().unwrap() += 1;
            self.seen.lock().unwrap().push(req.contents.clone());
            self.seen_config.lock().unwrap().push(req.config.clone());
            let response = adk_core::model::LlmResponse::new(
                Content::new("model").with_text(self.script.clone()),
            );
            Ok(Box::pin(futures::stream::once(async move { Ok(response) })))
        }
    }

    /// A `LoopInputs` with test defaults: empty tools, no policy, suspension on,
    /// system prompt `"sys"`. Tests override individual fields as needed.
    fn base_inputs(
        model: Arc<dyn Llm>,
        runtime: Arc<dyn CodeRuntime>,
        incoming: Content,
    ) -> LoopInputs {
        LoopInputs {
            model,
            runtime,
            tools: vec![],
            policy: ToolConfirmationPolicy::Never,
            decisions: HashMap::new(),
            invocation_ctx: Arc::new(MockInvocationContext::new(incoming.clone())),
            conversation: vec![incoming.clone()],
            incoming,
            system_prompt: "sys".to_string(),
            pending: None,
            invocation_id: "inv-1".to_string(),
            agent_name: "code-agent".to_string(),
            max_iterations: DEFAULT_MAX_ITERATIONS,
            max_error_chars: DEFAULT_MAX_ERROR_CHARS,
            supports_suspension: true,
            transfer_targets: Vec::new(),
            generate_content_config: None,
            tool_timeout: DEFAULT_TOOL_TIMEOUT,
            output_key: None,
            default_retry_budget: None,
            tool_retry_budgets: HashMap::new(),
            circuit_breaker_threshold: None,
            on_tool_error: Arc::new(Vec::new()),
            before_model_callbacks: Arc::new(Vec::new()),
            after_model_callbacks: Arc::new(Vec::new()),
            before_tool_callbacks: Arc::new(Vec::new()),
            after_tool_callbacks: Arc::new(Vec::new()),
            after_tool_callbacks_full: Arc::new(Vec::new()),
            output_schema: None,
            output_max_retries: DEFAULT_OUTPUT_MAX_RETRIES,
            output_guardrails: Arc::new(GuardrailSet::new()),
            #[cfg(feature = "enhanced-plugins")]
            enhanced_plugin_manager: None,
        }
    }

    /// Drive `run_codeact` with fakes and collect all emitted events.
    #[allow(clippy::too_many_arguments)]
    async fn drive(
        model: Arc<dyn Llm>,
        runtime: Arc<dyn CodeRuntime>,
        tools: Vec<Arc<dyn Tool>>,
        policy: ToolConfirmationPolicy,
        decisions: HashMap<String, ToolConfirmationDecision>,
        incoming: Content,
        pending: Option<CodeActCheckpoint>,
        supports_suspension: bool,
    ) -> Vec<Event> {
        let mut input = base_inputs(model, runtime, incoming);
        input.tools = tools;
        input.policy = policy;
        input.decisions = decisions;
        input.pending = pending;
        input.supports_suspension = supports_suspension;
        collect(input).await
    }

    /// Drive a fresh loop whose pre-assembled system prompt includes a transfer
    /// section for `targets` (mirroring what `run` builds).
    async fn drive_targets(
        model: Arc<dyn Llm>,
        runtime: Arc<dyn CodeRuntime>,
        targets: Vec<String>,
        incoming: Content,
    ) -> Vec<Event> {
        let mut input = base_inputs(model, runtime, incoming);
        input.system_prompt = format!("sys\n\n{}", transfer_prompt(&targets));
        input.transfer_targets = targets;
        collect(input).await
    }

    /// Run a loop to exhaustion and collect every emitted event.
    async fn collect(input: LoopInputs) -> Vec<Event> {
        let mut events = Vec::new();
        let mut stream = Box::pin(run_codeact(input));
        while let Some(item) = stream.next().await {
            events.push(item.expect("ok event"));
        }
        events
    }

    fn user(text: &str) -> Content {
        Content::new("user").with_text(text)
    }

    fn final_text(event: &Event) -> Option<String> {
        event.llm_response.content.as_ref()?.parts.first()?.text().map(str::to_string)
    }

    fn pending_in(event: &Event) -> Option<CodeActCheckpoint> {
        event
            .actions
            .state_delta
            .get(PENDING_STATE_KEY)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    #[tokio::test]
    async fn produces_final_result() {
        let events = drive(
            FakeLlm::new("noop"),
            Arc::new(ScriptedRuntime::with_suspension(vec![vec![Planned::Complete(
                json!({"type": "final_result", "value": "answer"}),
            )]])),
            vec![],
            ToolConfirmationPolicy::Never,
            HashMap::new(),
            user("go"),
            None,
            true,
        )
        .await;
        assert_eq!(final_text(events.last().unwrap()).as_deref(), Some("answer"));
    }

    #[tokio::test]
    async fn inline_tool_brackets_with_before_and_after_checkpoints() {
        let events = drive(
            FakeLlm::new("noop"),
            Arc::new(ScriptedRuntime::with_suspension(vec![vec![
                Planned::call("echo", json!({"msg": "x"}), 1),
                Planned::Complete(json!({"type": "final_result", "value": "done"})),
            ]])),
            vec![echo_tool()],
            ToolConfirmationPolicy::Never,
            HashMap::new(),
            user("go"),
            None,
            true,
        )
        .await;

        // before (PendingResult), after (Resolved), final (cleared).
        let dispositions: Vec<_> =
            events.iter().filter_map(pending_in).map(|c| c.disposition).collect();
        assert!(matches!(dispositions.first(), Some(Disposition::PendingResult)));
        assert!(matches!(dispositions.get(1), Some(Disposition::Resolved(_))));
        assert_eq!(final_text(events.last().unwrap()).as_deref(), Some("done"));
        // final event clears the pending state
        assert_eq!(
            events.last().unwrap().actions.state_delta.get(PENDING_STATE_KEY),
            Some(&Value::Null)
        );
    }

    #[tokio::test]
    async fn no_checkpoints_without_suspension() {
        let events = drive(
            FakeLlm::new("noop"),
            Arc::new(ScriptedRuntime::new(vec![vec![
                Planned::call("echo", json!({"msg": "x"}), 1),
                Planned::Complete(json!({"type": "final_result", "value": "inline"})),
            ]])),
            vec![echo_tool()],
            ToolConfirmationPolicy::Never,
            HashMap::new(),
            user("go"),
            None,
            false,
        )
        .await;
        // Only the final event; no durability checkpoints were written.
        assert_eq!(events.len(), 1);
        assert_eq!(final_text(&events[0]).as_deref(), Some("inline"));
    }

    #[tokio::test]
    async fn long_running_suspends_then_resumes_from_checkpoint() {
        let runtime = Arc::new(ScriptedRuntime::with_suspension(vec![vec![
            Planned::call("slow", json!({}), 1),
            Planned::Complete(json!({"type": "final_result", "value": "completed"})),
        ]]));

        // First invocation: suspends to a checkpoint.
        let events = drive(
            FakeLlm::new("noop"),
            runtime.clone(),
            vec![long_running_tool()],
            ToolConfirmationPolicy::Never,
            HashMap::new(),
            user("go"),
            None,
            true,
        )
        .await;
        let suspend = events.last().unwrap();
        assert_eq!(suspend.long_running_tool_ids, vec!["slow".to_string()]);
        let cp = pending_in(suspend).expect("checkpoint persisted");
        assert!(matches!(cp.disposition, Disposition::AwaitingCompletion { .. }));

        // Second invocation: the result arrives as a FunctionResponse.
        let completion = Content {
            role: "user".to_string(),
            parts: vec![Part::FunctionResponse {
                function_response: adk_core::FunctionResponseData::new("slow", json!({"ok": true})),
                id: None,
            }],
        };
        let events = drive(
            FakeLlm::new("noop"),
            runtime,
            vec![long_running_tool()],
            ToolConfirmationPolicy::Never,
            HashMap::new(),
            completion,
            Some(cp),
            true,
        )
        .await;
        assert_eq!(final_text(events.last().unwrap()).as_deref(), Some("completed"));
    }

    #[tokio::test]
    async fn stdout_before_a_suspend_is_persisted_in_the_checkpoint() {
        // The script prints before calling a long-running tool; that output must
        // ride along in the checkpoint transcript so it is not lost across the
        // suspend/resume boundary.
        let runtime = Arc::new(ScriptedRuntime::with_suspension(vec![vec![
            Planned::call_with_stdout("slow", json!({}), 1, "working...\n"),
            Planned::Complete(json!({"type": "final_result", "value": "completed"})),
        ]]));

        let events = drive(
            FakeLlm::new("noop"),
            runtime,
            vec![long_running_tool()],
            ToolConfirmationPolicy::Never,
            HashMap::new(),
            user("go"),
            None,
            true,
        )
        .await;

        let cp = pending_in(events.last().unwrap()).expect("checkpoint persisted");
        let printed = cp
            .transcript
            .iter()
            .filter_map(|c| c.parts.first().and_then(Part::text))
            .any(|t| t.contains("Output (stdout):") && t.contains("working..."));
        assert!(printed, "stdout was not persisted in the checkpoint transcript: {cp:?}");
    }

    #[test]
    fn transcript_with_stdout_appends_only_when_non_empty() {
        let base = vec![Content::new("user").with_text("hi")];
        assert_eq!(transcript_with_stdout(&base, "", 100).len(), 1);
        let with = transcript_with_stdout(&base, "printed", 100);
        assert_eq!(with.len(), 2);
        assert!(
            with[1].parts.first().and_then(Part::text).unwrap().contains("printed"),
            "appended entry should carry the stdout"
        );
    }

    #[tokio::test]
    async fn recovers_from_resolved_checkpoint_without_rerunning_tool() {
        // A crafted "after" checkpoint: resume with the stored result.
        let snapshot = serde_json::to_vec(&vec![Planned::Complete(
            json!({"type": "final_result", "value": "recovered"}),
        )])
        .unwrap();
        let cp = CodeActCheckpoint {
            iteration: 1,
            transcript: vec![],
            snapshot,
            call: PendingToolCall { call_id: 1, tool: "echo".into(), args: json!({}) },
            disposition: Disposition::Resolved(ResolutionRecord::Value(json!("stored"))),
            tool_roster: vec!["echo".to_string()],
        };
        let events = drive(
            FakeLlm::new("noop"),
            Arc::new(ScriptedRuntime::with_suspension(vec![])),
            vec![echo_tool()],
            ToolConfirmationPolicy::Never,
            HashMap::new(),
            user("ignored"),
            Some(cp),
            true,
        )
        .await;
        assert_eq!(final_text(events.last().unwrap()).as_deref(), Some("recovered"));
    }

    #[tokio::test]
    async fn tool_failure_raises_into_script() {
        let rt = Arc::new(ScriptedRuntime::with_suspension(vec![vec![
            Planned::call("boom", json!({}), 1),
            Planned::Complete(json!({"type": "final_result", "value": "caught"})),
        ]]));
        let events = drive(
            FakeLlm::new("noop"),
            rt.clone(),
            vec![failing_tool()],
            ToolConfirmationPolicy::Never,
            HashMap::new(),
            user("go"),
            None,
            true,
        )
        .await;
        assert_eq!(final_text(events.last().unwrap()).as_deref(), Some("caught"));
        assert!(rt.last_raise().unwrap().contains("not_found"));
    }

    /// The full Agent path: `run()` reads session state, suspends, and on a
    /// second `run()` (with the result in the message) resumes to a final.
    #[tokio::test]
    async fn run_round_trips_through_session_state() {
        let agent = CodeActAgent::builder()
            .model(FakeLlm::new("noop"))
            .runtime(Arc::new(ScriptedRuntime::with_suspension(vec![vec![
                Planned::call("slow", json!({}), 1),
                Planned::Complete(json!({"type": "final_result", "value": "completed"})),
            ]])))
            .tool(long_running_tool())
            .build()
            .unwrap();

        // First run: suspends, persisting the checkpoint to session state.
        let ctx1 = Arc::new(MockInvocationContext::new(user("go")));
        let mut state = HashMap::new();
        let mut stream = agent.run(ctx1).await.unwrap();
        while let Some(item) = stream.next().await {
            let event = item.unwrap();
            for (k, v) in &event.actions.state_delta {
                state.insert(k.clone(), v.clone());
            }
        }
        assert!(state.contains_key(PENDING_STATE_KEY));

        // Second run: the result arrives as a FunctionResponse; the seeded
        // session state carries the checkpoint.
        let completion = Content {
            role: "user".to_string(),
            parts: vec![Part::FunctionResponse {
                function_response: FunctionResponseData::new("slow", json!({"ok": true})),
                id: None,
            }],
        };
        let ctx2 = Arc::new(MockInvocationContext::new(completion).with_state(state));
        let mut last = None;
        let mut stream = agent.run(ctx2).await.unwrap();
        while let Some(item) = stream.next().await {
            last = Some(item.unwrap());
        }
        let last = last.unwrap();
        assert_eq!(final_text(&last).as_deref(), Some("completed"));
        assert_eq!(last.actions.state_delta.get(PENDING_STATE_KEY), Some(&Value::Null));
    }

    #[tokio::test]
    async fn observation_then_final_records_model_turns() {
        let model = FakeLlm::new("the script");
        let events = drive(
            model.clone(),
            Arc::new(ScriptedRuntime::with_suspension(vec![
                vec![Planned::Complete(json!({"type": "observation", "value": "look"}))],
                vec![Planned::Complete(json!({"type": "final_result", "value": "done"}))],
            ])),
            vec![],
            ToolConfirmationPolicy::Never,
            HashMap::new(),
            user("go"),
            None,
            true,
        )
        .await;
        assert_eq!(final_text(events.last().unwrap()).as_deref(), Some("done"));
        assert_eq!(model.calls(), 2);
        let second = &model.seen.lock().unwrap()[1];
        assert!(second.iter().any(|c| c.role == "model"));
    }

    #[test]
    fn extract_code_handles_language_tag_and_fallback() {
        assert_eq!(extract_code_block("```python\nreturn 42\n```"), "return 42");
        assert_eq!(extract_code_block("```js\nx;\n```"), "x;");
        assert_eq!(extract_code_block("return 1"), "return 1");
    }

    #[test]
    fn assemble_orders_sections() {
        let prompt = assemble_system_prompt(
            Some("SKILL"),
            Some("GLOBAL"),
            Some("HOUSE"),
            "RT-ENV",
            "TOOLS",
            Some("XFER"),
        );
        let skill = prompt.find("SKILL").unwrap();
        let global = prompt.find("GLOBAL").unwrap();
        let user = prompt.find("HOUSE").unwrap();
        let base = prompt.find("You act by writing code").unwrap();
        let rt = prompt.find("RT-ENV").unwrap();
        let tools = prompt.find("TOOLS").unwrap();
        let xfer = prompt.find("XFER").unwrap();
        assert!(
            skill < global
                && global < user
                && user < base
                && base < rt
                && rt < tools
                && tools < xfer
        );
    }

    #[test]
    fn builder_requires_model_and_runtime() {
        assert!(CodeActAgent::builder().build().unwrap_err().message.contains("model"));
    }

    #[tokio::test]
    async fn generation_config_is_sent_to_the_model() {
        let model = FakeLlm::new("noop");
        let mut input = base_inputs(
            model.clone(),
            Arc::new(ScriptedRuntime::with_suspension(vec![vec![Planned::Complete(
                json!({"type": "final_result", "value": "done"}),
            )]])),
            user("go"),
        );
        input.generate_content_config =
            Some(GenerateContentConfig { temperature: Some(0.5), ..Default::default() });
        collect(input).await;
        assert_eq!(model.last_config().and_then(|c| c.temperature), Some(0.5));
    }

    #[tokio::test]
    async fn output_key_stores_final_result_in_state() {
        let mut input = base_inputs(
            FakeLlm::new("noop"),
            Arc::new(ScriptedRuntime::with_suspension(vec![vec![Planned::Complete(
                json!({"type": "final_result", "value": {"answer": 42}}),
            )]])),
            user("go"),
        );
        input.output_key = Some("result".to_string());
        let events = collect(input).await;
        let last = events.last().unwrap();
        assert_eq!(last.actions.state_delta.get("result"), Some(&json!({"answer": 42})));
    }

    #[tokio::test]
    async fn tool_timeout_raises_into_the_script() {
        let rt = Arc::new(ScriptedRuntime::with_suspension(vec![vec![
            Planned::call("sleeper", json!({}), 1),
            Planned::Complete(json!({"type": "final_result", "value": "recovered"})),
        ]]));
        let mut input = base_inputs(FakeLlm::new("noop"), rt.clone(), user("go"));
        input.tools = vec![sleeping_tool()];
        input.tool_timeout = Duration::from_millis(20);
        let events = collect(input).await;
        assert_eq!(final_text(events.last().unwrap()).as_deref(), Some("recovered"));
        assert!(rt.last_raise().unwrap().contains("timed out"));
    }

    #[tokio::test]
    async fn before_agent_callback_short_circuits_the_loop() {
        let model = FakeLlm::new("noop");
        let before: BeforeAgentCallback = Box::new(|_ctx| {
            Box::pin(async { Ok(Some(Content::new("model").with_text("intercepted"))) })
        });
        let agent = CodeActAgent::builder()
            .model(model.clone())
            .runtime(Arc::new(ScriptedRuntime::with_suspension(vec![])))
            .before_callback(before)
            .build()
            .unwrap();
        let events = run_collect(&agent, Arc::new(MockInvocationContext::new(user("go")))).await;
        assert_eq!(final_text(events.last().unwrap()).as_deref(), Some("intercepted"));
        // The model was never consulted because the loop was short-circuited.
        assert_eq!(model.calls(), 0);
    }

    #[tokio::test]
    async fn after_agent_callback_runs_on_completion() {
        let after: AfterAgentCallback = Box::new(|_ctx| {
            Box::pin(async { Ok(Some(Content::new("model").with_text("after-done"))) })
        });
        let agent = CodeActAgent::builder()
            .model(FakeLlm::new("noop"))
            .runtime(Arc::new(ScriptedRuntime::with_suspension(vec![vec![Planned::Complete(
                json!({"type": "final_result", "value": "answer"}),
            )]])))
            .after_callback(after)
            .build()
            .unwrap();
        let events = run_collect(&agent, Arc::new(MockInvocationContext::new(user("go")))).await;
        // The final result is emitted, then the after-agent callback content.
        assert!(events.iter().any(|e| final_text(e).as_deref() == Some("answer")));
        assert_eq!(final_text(events.last().unwrap()).as_deref(), Some("after-done"));
    }

    #[tokio::test]
    async fn after_agent_callback_skipped_on_suspend() {
        let after: AfterAgentCallback = Box::new(|_ctx| {
            Box::pin(async { Ok(Some(Content::new("model").with_text("after-done"))) })
        });
        let agent = CodeActAgent::builder()
            .model(FakeLlm::new("noop"))
            .runtime(Arc::new(ScriptedRuntime::with_suspension(vec![vec![
                Planned::call("slow", json!({}), 1),
                Planned::Complete(json!({"type": "final_result", "value": "unreached"})),
            ]])))
            .tool(long_running_tool())
            .after_callback(after)
            .build()
            .unwrap();
        let events = run_collect(&agent, Arc::new(MockInvocationContext::new(user("go")))).await;
        // The run suspended, so the after-agent callback must not have fired.
        assert!(events.iter().all(|e| final_text(e).as_deref() != Some("after-done")));
    }

    /// Concatenated text of the transcript the model saw on its first call.
    fn first_transcript_text(model: &FakeLlm) -> String {
        model
            .seen
            .lock()
            .unwrap()
            .first()
            .map(|contents| {
                contents
                    .iter()
                    .flat_map(|c| c.parts.iter())
                    .filter_map(|p| p.text())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }

    fn final_runtime() -> Arc<ScriptedRuntime> {
        Arc::new(ScriptedRuntime::with_suspension(vec![vec![Planned::Complete(
            json!({"type": "final_result", "value": "x"}),
        )]]))
    }

    #[tokio::test]
    async fn agent_instruction_precedes_the_contract() {
        let model = FakeLlm::new("noop");
        let agent = CodeActAgent::builder()
            .model(model.clone())
            .runtime(final_runtime())
            .instruction("HOUSE RULES")
            .build()
            .unwrap();
        run_collect(&agent, Arc::new(MockInvocationContext::new(user("go")))).await;
        let sys = first_transcript_text(&model);
        assert!(sys.find("HOUSE RULES").unwrap() < sys.find("You act by writing code").unwrap());
    }

    #[tokio::test]
    async fn instruction_provider_takes_precedence_over_static() {
        let model = FakeLlm::new("noop");
        let provider: InstructionProvider =
            Box::new(|_ctx| Box::pin(async { Ok("DYNAMIC RULES".to_string()) }));
        let agent = CodeActAgent::builder()
            .model(model.clone())
            .runtime(final_runtime())
            .instruction("STATIC RULES")
            .instruction_provider(provider)
            .build()
            .unwrap();
        run_collect(&agent, Arc::new(MockInvocationContext::new(user("go")))).await;
        let sys = first_transcript_text(&model);
        assert!(sys.contains("DYNAMIC RULES"));
        assert!(!sys.contains("STATIC RULES"));
    }

    #[tokio::test]
    async fn global_instruction_precedes_agent_instruction() {
        let model = FakeLlm::new("noop");
        let agent = CodeActAgent::builder()
            .model(model.clone())
            .runtime(final_runtime())
            .global_instruction("GLOBAL IDENTITY")
            .instruction("AGENT RULES")
            .build()
            .unwrap();
        run_collect(&agent, Arc::new(MockInvocationContext::new(user("go")))).await;
        let sys = first_transcript_text(&model);
        assert!(sys.find("GLOBAL IDENTITY").unwrap() < sys.find("AGENT RULES").unwrap());
    }

    #[tokio::test]
    async fn default_include_contents_seeds_conversation_history() {
        let model = FakeLlm::new("noop");
        let agent =
            CodeActAgent::builder().model(model.clone()).runtime(final_runtime()).build().unwrap();
        let ctx = MockInvocationContext::new(user("q2")).with_history(vec![
            user("q1"),
            Content::new("model").with_text("a1"),
            user("q2"),
        ]);
        run_collect(&agent, Arc::new(ctx)).await;
        let transcript = first_transcript_text(&model);
        assert!(transcript.contains("q1"));
        assert!(transcript.contains("a1"));
    }

    #[tokio::test]
    async fn include_contents_none_excludes_history() {
        let model = FakeLlm::new("noop");
        let agent = CodeActAgent::builder()
            .model(model.clone())
            .runtime(final_runtime())
            .include_contents(IncludeContents::None)
            .build()
            .unwrap();
        let ctx = MockInvocationContext::new(user("q2")).with_history(vec![
            user("q1"),
            Content::new("model").with_text("a1"),
            user("q2"),
        ]);
        run_collect(&agent, Arc::new(ctx)).await;
        let transcript = first_transcript_text(&model);
        assert!(!transcript.contains("a1"));
        assert!(transcript.contains("q2"));
    }

    #[tokio::test]
    async fn retry_budget_recovers_a_flaky_tool() {
        // `flaky` fails once; a budget of one retry lets the call succeed.
        let rt = Arc::new(ScriptedRuntime::with_suspension(vec![vec![
            Planned::call("flaky", json!({}), 1),
            Planned::Complete(json!({"type": "final_result", "value": "done"})),
        ]]));
        let mut input = base_inputs(FakeLlm::new("noop"), rt.clone(), user("go"));
        input.tools = vec![flaky_tool(1)];
        input.default_retry_budget = Some(RetryBudget { max_retries: 1, delay: Duration::ZERO });
        let events = collect(input).await;
        assert_eq!(final_text(events.last().unwrap()).as_deref(), Some("done"));
        // The retried call ultimately returned a value (not a raised error).
        assert_eq!(rt.last_value(), Some(json!({"ok": true})));
        assert!(rt.last_raise().is_none());
    }

    #[tokio::test]
    async fn on_tool_error_supplies_a_fallback_value() {
        let rt = Arc::new(ScriptedRuntime::with_suspension(vec![vec![
            Planned::call("boom", json!({}), 1),
            Planned::Complete(json!({"type": "final_result", "value": "done"})),
        ]]));
        let mut input = base_inputs(FakeLlm::new("noop"), rt.clone(), user("go"));
        input.tools = vec![failing_tool()];
        let cb: OnToolErrorCallback = Box::new(|_ctx, _tool, _args, _err| {
            Box::pin(async { Ok(Some(json!({"fallback": "used"}))) })
        });
        input.on_tool_error = Arc::new(vec![cb]);
        let events = collect(input).await;
        assert_eq!(final_text(events.last().unwrap()).as_deref(), Some("done"));
        // The failure was replaced by the fallback value, not raised.
        assert_eq!(rt.last_value(), Some(json!({"fallback": "used"})));
        assert!(rt.last_raise().is_none());
    }

    #[tokio::test]
    async fn circuit_breaker_opens_after_threshold_failures() {
        // Threshold 1: the first failure trips the breaker; the second call is
        // short-circuited without executing the tool.
        let rt = Arc::new(ScriptedRuntime::with_suspension(vec![vec![
            Planned::call("boom", json!({}), 1),
            Planned::call("boom", json!({}), 2),
            Planned::Complete(json!({"type": "final_result", "value": "done"})),
        ]]));
        let mut input = base_inputs(FakeLlm::new("noop"), rt.clone(), user("go"));
        input.tools = vec![failing_tool()];
        input.circuit_breaker_threshold = Some(1);
        let events = collect(input).await;
        assert_eq!(final_text(events.last().unwrap()).as_deref(), Some("done"));
        assert!(rt.last_raise().unwrap().contains("temporarily disabled"));
    }

    fn object_schema() -> Value {
        json!({
            "type": "object",
            "required": ["answer"],
            "properties": { "answer": { "type": "string" } },
            "additionalProperties": false
        })
    }

    #[tokio::test]
    async fn output_schema_accepts_a_conforming_final_result() {
        let mut input = base_inputs(
            FakeLlm::new("noop"),
            Arc::new(ScriptedRuntime::with_suspension(vec![vec![Planned::Complete(
                json!({"type": "final_result", "value": {"answer": "hi"}}),
            )]])),
            user("go"),
        );
        input.output_schema = Some(object_schema());
        let events = collect(input).await;
        // A single final event, carrying the conforming value.
        assert_eq!(events.len(), 1);
        assert!(final_text(events.last().unwrap()).unwrap().contains("hi"));
    }

    #[tokio::test]
    async fn output_schema_correction_retries_then_succeeds() {
        let model = FakeLlm::new("noop");
        let mut input = base_inputs(
            model.clone(),
            Arc::new(ScriptedRuntime::with_suspension(vec![
                vec![Planned::Complete(json!({"type": "final_result", "value": {"wrong": 1}}))],
                vec![Planned::Complete(json!({"type": "final_result", "value": {"answer": "ok"}}))],
            ])),
            user("go"),
        );
        input.output_schema = Some(object_schema());
        let events = collect(input).await;
        assert!(final_text(events.last().unwrap()).unwrap().contains("ok"));
        // Two model turns: the first was corrected.
        assert_eq!(model.calls(), 2);
        let second = &model.seen.lock().unwrap()[1];
        assert!(second.iter().any(|c| {
            c.parts
                .iter()
                .any(|p| p.text().is_some_and(|t| t.contains("did not match the required schema")))
        }));
    }

    #[tokio::test]
    async fn output_schema_errors_after_max_retries() {
        let mut input = base_inputs(
            FakeLlm::new("noop"),
            Arc::new(ScriptedRuntime::with_suspension(vec![
                vec![Planned::Complete(json!({"type": "final_result", "value": {"wrong": 1}}))],
                vec![Planned::Complete(json!({"type": "final_result", "value": {"wrong": 2}}))],
            ])),
            user("go"),
        );
        input.output_schema = Some(object_schema());
        input.output_max_retries = 1;
        let mut stream = Box::pin(run_codeact(input));
        let mut last_err = None;
        while let Some(item) = stream.next().await {
            if let Err(e) = item {
                last_err = Some(e);
            }
        }
        assert!(last_err.unwrap().message.contains("schema validation"));
    }

    #[derive(schemars::JsonSchema)]
    #[allow(dead_code)]
    struct Answer {
        answer: String,
    }

    #[tokio::test]
    async fn output_type_enforces_a_derived_schema() {
        let agent = CodeActAgent::builder()
            .model(FakeLlm::new("noop"))
            .runtime(Arc::new(ScriptedRuntime::with_suspension(vec![vec![Planned::Complete(
                json!({"type": "final_result", "value": {"answer": "hi"}}),
            )]])))
            .output_type::<Answer>()
            .build()
            .unwrap();
        let events = run_collect(&agent, Arc::new(MockInvocationContext::new(user("go")))).await;
        assert!(final_text(events.last().unwrap()).unwrap().contains("hi"));
    }

    #[tokio::test]
    async fn toolset_tools_are_callable_and_listed_in_the_prompt() {
        let model = FakeLlm::new("noop");
        let agent = CodeActAgent::builder()
            .model(model.clone())
            .runtime(Arc::new(ScriptedRuntime::with_suspension(vec![vec![
                Planned::call("echo", json!({"msg": "hi"}), 1),
                Planned::Complete(json!({"type": "final_result", "value": "done"})),
            ]])))
            .toolset(fake_toolset("dynamic", vec![echo_tool()]))
            .build()
            .unwrap();
        let events = run_collect(&agent, Arc::new(MockInvocationContext::new(user("go")))).await;
        assert_eq!(final_text(events.last().unwrap()).as_deref(), Some("done"));
        // The toolset-provided tool was both callable and described to the model.
        assert!(first_transcript_text(&model).contains("echo"));
    }

    #[cfg(feature = "skills")]
    #[tokio::test]
    async fn skill_block_is_injected_for_a_matching_query() {
        use std::fs;
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join(".skills")).unwrap();
        fs::write(
            temp.path().join(".skills/searcher.md"),
            "---\nname: searcher\ndescription: Search rust code with rg\ntags: [search]\n---\nUse rg to grep the codebase.",
        )
        .unwrap();

        let model = FakeLlm::new("noop");
        let agent = CodeActAgent::builder()
            .model(model.clone())
            .runtime(final_runtime())
            .with_skills_from_root(temp.path())
            .unwrap()
            .with_skill_policy(SelectionPolicy { min_score: 0.1, ..SelectionPolicy::default() })
            .build()
            .unwrap();
        run_collect(&agent, Arc::new(MockInvocationContext::new(user("help me search rust code"))))
            .await;
        // The matched skill's prompt block is present in the system prompt.
        assert!(first_transcript_text(&model).contains("rg"));
    }

    #[cfg(feature = "guardrails")]
    #[tokio::test]
    async fn input_guardrail_blocks_harmful_content() {
        use crate::guardrails::{ContentFilter, GuardrailSet};
        let agent = CodeActAgent::builder()
            .model(FakeLlm::new("noop"))
            .runtime(final_runtime())
            .input_guardrails(GuardrailSet::new().with(ContentFilter::harmful_content()))
            .build()
            .unwrap();
        let ctx = MockInvocationContext::new(user("how to deploy malware on a server"));
        match agent.run(Arc::new(ctx)).await {
            Ok(_) => panic!("harmful input should be blocked"),
            Err(e) => assert!(e.message.contains("guardrail")),
        }
    }

    #[cfg(feature = "guardrails")]
    #[tokio::test]
    async fn output_guardrail_redacts_pii_in_final_result() {
        use crate::guardrails::{GuardrailSet, PiiRedactor};
        let agent = CodeActAgent::builder()
            .model(FakeLlm::new("noop"))
            .runtime(Arc::new(ScriptedRuntime::with_suspension(vec![vec![Planned::Complete(
                json!({"type": "final_result", "value": "reach me at alice@example.com"}),
            )]])))
            .output_guardrails(GuardrailSet::new().with(PiiRedactor::new()))
            .build()
            .unwrap();
        let events = run_collect(&agent, Arc::new(MockInvocationContext::new(user("go")))).await;
        let text = final_text(events.last().unwrap()).unwrap();
        assert!(!text.contains("alice@example.com"));
        assert!(text.contains("[EMAIL REDACTED]"));
    }

    #[cfg(feature = "enhanced-plugins")]
    struct ToolArgRewriter;
    #[cfg(feature = "enhanced-plugins")]
    #[async_trait]
    impl EnhancedPlugin for ToolArgRewriter {
        fn name(&self) -> &str {
            "arg-rewriter"
        }
        async fn before_tool_call(
            &self,
            _tool: Arc<dyn Tool>,
            _args: Value,
            _ctx: Arc<dyn CallbackContext>,
            _pctx: &adk_plugin::PluginContext,
        ) -> adk_core::Result<BeforeToolCallResult> {
            Ok(BeforeToolCallResult::Continue(json!({"rewritten": true})))
        }
    }

    #[cfg(feature = "enhanced-plugins")]
    struct ToolShortCircuit;
    #[cfg(feature = "enhanced-plugins")]
    #[async_trait]
    impl EnhancedPlugin for ToolShortCircuit {
        fn name(&self) -> &str {
            "short-circuit"
        }
        async fn before_tool_call(
            &self,
            _tool: Arc<dyn Tool>,
            _args: Value,
            _ctx: Arc<dyn CallbackContext>,
            _pctx: &adk_plugin::PluginContext,
        ) -> adk_core::Result<BeforeToolCallResult> {
            Ok(BeforeToolCallResult::ShortCircuit(json!({"synthetic": true})))
        }
    }

    #[cfg(feature = "enhanced-plugins")]
    struct ModelShortCircuit;
    #[cfg(feature = "enhanced-plugins")]
    #[async_trait]
    impl EnhancedPlugin for ModelShortCircuit {
        fn name(&self) -> &str {
            "model-short-circuit"
        }
        async fn before_model_call(
            &self,
            _request: LlmRequest,
            _ctx: Arc<dyn CallbackContext>,
            _pctx: &adk_plugin::PluginContext,
        ) -> adk_core::Result<BeforeModelCallResult> {
            let response = LlmResponse {
                content: Some(Content::new("model").with_text("noop")),
                ..Default::default()
            };
            Ok(BeforeModelCallResult::ShortCircuit(response))
        }
    }

    #[cfg(feature = "enhanced-plugins")]
    #[tokio::test]
    async fn enhanced_plugin_rewrites_tool_args() {
        let rt = Arc::new(ScriptedRuntime::with_suspension(vec![vec![
            Planned::call("echo", json!({"orig": true}), 1),
            Planned::Complete(json!({"type": "final_result", "value": "done"})),
        ]]));
        let agent = CodeActAgent::builder()
            .model(FakeLlm::new("noop"))
            .runtime(rt.clone())
            .tool(echo_tool())
            .enhanced_plugin(Arc::new(ToolArgRewriter))
            .build()
            .unwrap();
        run_collect(&agent, Arc::new(MockInvocationContext::new(user("go")))).await;
        // echo received the plugin-rewritten args and returned them.
        assert_eq!(rt.last_value(), Some(json!({"rewritten": true})));
    }

    #[cfg(feature = "enhanced-plugins")]
    #[tokio::test]
    async fn enhanced_plugin_short_circuits_tool() {
        let rt = Arc::new(ScriptedRuntime::with_suspension(vec![vec![
            Planned::call("boom", json!({}), 1),
            Planned::Complete(json!({"type": "final_result", "value": "done"})),
        ]]));
        let agent = CodeActAgent::builder()
            .model(FakeLlm::new("noop"))
            .runtime(rt.clone())
            .tool(failing_tool())
            .enhanced_plugin(Arc::new(ToolShortCircuit))
            .build()
            .unwrap();
        run_collect(&agent, Arc::new(MockInvocationContext::new(user("go")))).await;
        // The failing tool never ran; the synthetic result was fed back.
        assert_eq!(rt.last_value(), Some(json!({"synthetic": true})));
        assert!(rt.last_raise().is_none());
    }

    #[cfg(feature = "enhanced-plugins")]
    #[tokio::test]
    async fn enhanced_plugin_short_circuits_model() {
        let model = FakeLlm::new("noop");
        let agent = CodeActAgent::builder()
            .model(model.clone())
            .runtime(final_runtime())
            .enhanced_plugin(Arc::new(ModelShortCircuit))
            .build()
            .unwrap();
        run_collect(&agent, Arc::new(MockInvocationContext::new(user("go")))).await;
        // The model was short-circuited by the plugin, never invoked.
        assert_eq!(model.calls(), 0);
    }

    #[tokio::test]
    async fn toolset_name_conflict_is_rejected() {
        let agent = CodeActAgent::builder()
            .model(FakeLlm::new("noop"))
            .runtime(Arc::new(ScriptedRuntime::new(vec![])))
            .tool(echo_tool())
            .toolset(fake_toolset("dup", vec![echo_tool()]))
            .build()
            .unwrap();
        let err = match agent.run(Arc::new(MockInvocationContext::new(user("go")))).await {
            Ok(_) => panic!("toolset name conflict should error"),
            Err(e) => e,
        };
        assert!(err.message.contains("duplicate tool name 'echo'"));
    }

    #[test]
    fn input_schema_is_exposed_for_introspection() {
        let schema = json!({"type": "object"});
        let agent = CodeActAgent::builder()
            .model(FakeLlm::new("noop"))
            .runtime(Arc::new(ScriptedRuntime::new(vec![])))
            .input_schema(schema.clone())
            .build()
            .unwrap();
        assert_eq!(agent.input_schema(), Some(&schema));
    }

    #[test]
    fn require_confirmation_for_all_sets_always_policy() {
        let agent = CodeActAgent::builder()
            .model(FakeLlm::new("noop"))
            .runtime(Arc::new(ScriptedRuntime::new(vec![])))
            .require_tool_confirmation_for_all()
            .build()
            .unwrap();
        assert!(agent.policy.requires_confirmation("anything"));
    }

    fn transfer_target(event: &Event) -> Option<&str> {
        event.actions.transfer_to_agent.as_deref()
    }

    #[tokio::test]
    async fn transfer_to_valid_target_ends_loop_with_transfer_event() {
        let events = drive_targets(
            FakeLlm::new("noop"),
            Arc::new(ScriptedRuntime::with_suspension(vec![vec![Planned::Complete(
                json!({"type": "transfer_to_agent", "agent_name": "billing"}),
            )]])),
            vec!["billing".to_string(), "support".to_string()],
            user("go"),
        )
        .await;
        let last = events.last().unwrap();
        assert_eq!(transfer_target(last), Some("billing"));
        // The pending checkpoint is cleared on transfer, like a final result.
        assert_eq!(last.actions.state_delta.get(PENDING_STATE_KEY), Some(&Value::Null));
    }

    #[tokio::test]
    async fn unknown_transfer_target_feeds_error_then_model_retries() {
        // First script transfers to an unknown agent; the model then finishes.
        let model = FakeLlm::new("noop");
        let events = drive_targets(
            model.clone(),
            Arc::new(ScriptedRuntime::with_suspension(vec![
                vec![Planned::Complete(
                    json!({"type": "transfer_to_agent", "agent_name": "ghost"}),
                )],
                vec![Planned::Complete(json!({"type": "final_result", "value": "done"}))],
            ])),
            vec!["billing".to_string()],
            user("go"),
        )
        .await;
        // No transfer happened; the loop reached a final result instead.
        assert!(events.iter().all(|e| transfer_target(e).is_none()));
        assert_eq!(final_text(events.last().unwrap()).as_deref(), Some("done"));
        // The model got a second turn whose transcript carries the error.
        assert_eq!(model.calls(), 2);
        let second = &model.seen.lock().unwrap()[1];
        assert!(second.iter().any(|c| {
            c.parts
                .iter()
                .any(|p| p.text().is_some_and(|t| t.contains("cannot transfer to 'ghost'")))
        }));
    }

    #[tokio::test]
    async fn transfer_section_revealed_only_when_targets_present() {
        // With targets: the first prompt the model sees lists the transfer option.
        let with_model = FakeLlm::new("noop");
        drive_targets(
            with_model.clone(),
            Arc::new(ScriptedRuntime::with_suspension(vec![vec![Planned::Complete(
                json!({"type": "final_result", "value": "x"}),
            )]])),
            vec!["billing".to_string()],
            user("go"),
        )
        .await;
        let with_sys = with_model.seen.lock().unwrap()[0][0].parts[0].text().unwrap().to_string();
        assert!(with_sys.contains("transfer_to_agent"));
        assert!(with_sys.contains("billing"));

        // Without targets: no transfer section is injected.
        let without_model = FakeLlm::new("noop");
        drive(
            without_model.clone(),
            Arc::new(ScriptedRuntime::with_suspension(vec![vec![Planned::Complete(
                json!({"type": "final_result", "value": "x"}),
            )]])),
            vec![],
            ToolConfirmationPolicy::Never,
            HashMap::new(),
            user("go"),
            None,
            true,
        )
        .await;
        let without_sys =
            without_model.seen.lock().unwrap()[0][0].parts[0].text().unwrap().to_string();
        assert!(!without_sys.contains("transfer_to_agent"));
    }

    #[tokio::test]
    async fn run_merges_sub_agents_and_runner_targets() {
        // A sub-agent (build time) plus a runner-provided peer (run time) are
        // both offered; the script transfers to the runner-provided one.
        let agent = CodeActAgent::builder()
            .model(FakeLlm::new("noop"))
            .runtime(Arc::new(ScriptedRuntime::with_suspension(vec![vec![Planned::Complete(
                json!({"type": "transfer_to_agent", "agent_name": "peer"}),
            )]])))
            .sub_agent(fake_agent("child"))
            .build()
            .unwrap();

        let ctx = Arc::new(
            MockInvocationContext::new(user("go")).with_transfer_targets(vec!["peer".to_string()]),
        );
        let mut last = None;
        let mut stream = agent.run(ctx).await.unwrap();
        while let Some(item) = stream.next().await {
            last = Some(item.unwrap());
        }
        assert_eq!(transfer_target(last.as_ref().unwrap()), Some("peer"));
    }

    /// Build an agent that, across two turns, first tries `first` then `second`
    /// as transfer targets, configured with the given disallow flags.
    fn two_transfer_agent(
        first: &str,
        second: &str,
        disallow_parent: bool,
        disallow_peers: bool,
        sub_agents: Vec<Arc<dyn Agent>>,
    ) -> CodeActAgent {
        let mut builder = CodeActAgent::builder()
            .model(FakeLlm::new("noop"))
            .runtime(Arc::new(ScriptedRuntime::with_suspension(vec![
                vec![Planned::Complete(json!({"type": "transfer_to_agent", "agent_name": first}))],
                vec![Planned::Complete(json!({"type": "transfer_to_agent", "agent_name": second}))],
            ])))
            .disallow_transfer_to_parent(disallow_parent)
            .disallow_transfer_to_peers(disallow_peers);
        for agent in sub_agents {
            builder = builder.sub_agent(agent);
        }
        builder.build().unwrap()
    }

    async fn run_collect(agent: &CodeActAgent, ctx: Arc<dyn InvocationContext>) -> Vec<Event> {
        let mut events = Vec::new();
        let mut stream = agent.run(ctx).await.unwrap();
        while let Some(item) = stream.next().await {
            events.push(item.unwrap());
        }
        events
    }

    #[tokio::test]
    async fn disallow_transfer_to_parent_filters_only_the_parent() {
        // First turn transfers to the parent (filtered out) -> error; second
        // turn transfers to a peer, which is still allowed.
        let agent = two_transfer_agent("boss", "peer", true, false, vec![]);
        let ctx = Arc::new(
            MockInvocationContext::new(user("go"))
                .with_parent_agent("boss")
                .with_transfer_targets(vec!["boss".to_string(), "peer".to_string()]),
        );
        let events = run_collect(&agent, ctx).await;
        assert_eq!(transfer_target(events.last().unwrap()), Some("peer"));
    }

    #[tokio::test]
    async fn disallow_transfer_to_peers_filters_peers_but_keeps_parent() {
        // First turn transfers to a peer (filtered out) -> error; second turn
        // transfers to the parent, which is still allowed.
        let agent = two_transfer_agent("peer", "boss", false, true, vec![]);
        let ctx = Arc::new(
            MockInvocationContext::new(user("go"))
                .with_parent_agent("boss")
                .with_transfer_targets(vec!["boss".to_string(), "peer".to_string()]),
        );
        let events = run_collect(&agent, ctx).await;
        assert_eq!(transfer_target(events.last().unwrap()), Some("boss"));
    }

    fn state_delta_in(event: &Event, key: &str) -> Option<Value> {
        event.actions.state_delta.get(key).cloned()
    }

    #[tokio::test]
    async fn inline_tool_state_delta_is_persisted_on_a_checkpoint() {
        // A tool that writes session state has its delta merged onto the
        // after-checkpoint event, so the Runner persists it — like LlmAgent.
        let events = drive(
            FakeLlm::new("noop"),
            Arc::new(ScriptedRuntime::with_suspension(vec![vec![
                Planned::call("remember", json!({}), 1),
                Planned::Complete(json!({"type": "final_result", "value": "done"})),
            ]])),
            vec![state_tool()],
            ToolConfirmationPolicy::Never,
            HashMap::new(),
            user("go"),
            None,
            true,
        )
        .await;
        // Some persisted event carries the tool's state delta.
        assert!(events.iter().any(|e| state_delta_in(e, "note") == Some(json!("kept"))));
        assert_eq!(final_text(events.last().unwrap()).as_deref(), Some("done"));
    }

    #[tokio::test]
    async fn non_suspending_tool_state_delta_persists_on_final() {
        // Without suspension there are no checkpoints, so the delta rides on the
        // final event instead.
        let events = drive(
            FakeLlm::new("noop"),
            Arc::new(ScriptedRuntime::new(vec![vec![
                Planned::call("remember", json!({}), 1),
                Planned::Complete(json!({"type": "final_result", "value": "done"})),
            ]])),
            vec![state_tool()],
            ToolConfirmationPolicy::Never,
            HashMap::new(),
            user("go"),
            None,
            false,
        )
        .await;
        let last = events.last().unwrap();
        assert_eq!(state_delta_in(last, "note"), Some(json!("kept")));
        assert_eq!(final_text(last).as_deref(), Some("done"));
    }

    #[tokio::test]
    async fn escalating_tool_ends_the_run() {
        // A tool that escalates stops the loop before the script continues.
        let events = drive(
            FakeLlm::new("noop"),
            Arc::new(ScriptedRuntime::with_suspension(vec![vec![
                Planned::call("panic_button", json!({}), 1),
                Planned::Complete(json!({"type": "final_result", "value": "unreached"})),
            ]])),
            vec![escalating_tool()],
            ToolConfirmationPolicy::Never,
            HashMap::new(),
            user("go"),
            None,
            true,
        )
        .await;
        let last = events.last().unwrap();
        assert!(last.actions.escalate);
        // The pending checkpoint is cleared and no final result was produced.
        assert_eq!(last.actions.state_delta.get(PENDING_STATE_KEY), Some(&Value::Null));
        assert!(events.iter().all(|e| final_text(e).as_deref() != Some("unreached")));
    }

    #[tokio::test]
    async fn agent_tool_runs_as_a_regular_callable_tool() {
        // An AgentTool wrapping a sub-agent is just a Tool: it is not built-in
        // and not long-running, so the loop calls it inline and hands its
        // output back into the script as the call's return value.
        use adk_tool::AgentTool;
        let tool: Arc<dyn Tool> = Arc::new(AgentTool::new(text_agent("researcher", "the answer")));
        let rt = Arc::new(ScriptedRuntime::with_suspension(vec![vec![
            Planned::call("researcher", json!({"request": "find it"}), 1),
            Planned::Complete(json!({"type": "final_result", "value": "done"})),
        ]]));
        let events = drive(
            FakeLlm::new("noop"),
            rt.clone(),
            vec![tool],
            ToolConfirmationPolicy::Never,
            HashMap::new(),
            user("go"),
            None,
            true,
        )
        .await;
        assert_eq!(final_text(events.last().unwrap()).as_deref(), Some("done"));
        // The sub-agent ran and its response was the value returned into the script.
        assert_eq!(rt.last_value(), Some(json!({"response": "the answer"})));
    }

    #[tokio::test]
    async fn disallow_flags_do_not_filter_sub_agents() {
        // A child sub-agent stays available even with peers disallowed.
        let agent = two_transfer_agent("child", "child", false, true, vec![fake_agent("child")]);
        let ctx = Arc::new(MockInvocationContext::new(user("go")));
        let events = run_collect(&agent, ctx).await;
        assert_eq!(transfer_target(events.last().unwrap()), Some("child"));
    }

    #[tokio::test]
    async fn before_model_callback_can_skip_the_model() {
        // A before-model callback returns a synthetic response, so the model is
        // never called; its content drives the loop to a final result.
        let model = FakeLlm::new("noop");
        let before: BeforeModelCallback = Box::new(|_ctx, _req| {
            Box::pin(async {
                Ok(BeforeModelResult::Skip(LlmResponse::new(
                    Content::new("model").with_text("```\nreturn final_result\n```"),
                )))
            })
        });
        let agent = CodeActAgent::builder()
            .model(model.clone())
            .runtime(Arc::new(ScriptedRuntime::with_suspension(vec![vec![Planned::Complete(
                json!({"type": "final_result", "value": "done"}),
            )]])))
            .before_model_callback(before)
            .build()
            .unwrap();
        let events = run_collect(&agent, Arc::new(MockInvocationContext::new(user("go")))).await;
        assert_eq!(final_text(events.last().unwrap()).as_deref(), Some("done"));
        assert_eq!(model.calls(), 0);
    }

    #[tokio::test]
    async fn after_model_callback_rewrites_the_script() {
        // The model emits one script; the after-model callback swaps the response
        // for a different one. The swapped script is what the runtime executes —
        // here the scripted runtime ignores the text, so we just confirm the
        // callback ran and the loop completed.
        let model = FakeLlm::new("noop");
        let seen = Arc::new(StdMutex::new(false));
        let seen_cb = seen.clone();
        let after: AfterModelCallback = Box::new(move |_ctx, _resp| {
            let seen = seen_cb.clone();
            Box::pin(async move {
                *seen.lock().unwrap() = true;
                Ok(Some(LlmResponse::new(Content::new("model").with_text("```\nx\n```"))))
            })
        });
        let agent = CodeActAgent::builder()
            .model(model)
            .runtime(Arc::new(ScriptedRuntime::with_suspension(vec![vec![Planned::Complete(
                json!({"type": "final_result", "value": "done"}),
            )]])))
            .after_model_callback(after)
            .build()
            .unwrap();
        let events = run_collect(&agent, Arc::new(MockInvocationContext::new(user("go")))).await;
        assert_eq!(final_text(events.last().unwrap()).as_deref(), Some("done"));
        assert!(*seen.lock().unwrap());
    }

    #[tokio::test]
    async fn before_tool_callback_short_circuits_the_tool() {
        // The before-tool callback returns content, so the tool never runs; the
        // content's text becomes the value fed back into the script.
        let rt = Arc::new(ScriptedRuntime::with_suspension(vec![vec![
            Planned::call("boom", json!({}), 1),
            Planned::Complete(json!({"type": "final_result", "value": "done"})),
        ]]));
        let mut input = base_inputs(FakeLlm::new("noop"), rt.clone(), user("go"));
        input.tools = vec![failing_tool()];
        let cb: BeforeToolCallback = Box::new(|_ctx| {
            Box::pin(async { Ok(Some(Content::new("function").with_text("intercepted"))) })
        });
        input.before_tool_callbacks = Arc::new(vec![cb]);
        let events = collect(input).await;
        assert_eq!(final_text(events.last().unwrap()).as_deref(), Some("done"));
        // The failing tool never ran; the callback's text was fed back.
        assert_eq!(rt.last_value(), Some(json!("intercepted")));
        assert!(rt.last_raise().is_none());
    }

    #[tokio::test]
    async fn after_tool_callback_full_rewrites_the_result() {
        let rt = Arc::new(ScriptedRuntime::with_suspension(vec![vec![
            Planned::call("echo", json!({"orig": true}), 1),
            Planned::Complete(json!({"type": "final_result", "value": "done"})),
        ]]));
        let mut input = base_inputs(FakeLlm::new("noop"), rt.clone(), user("go"));
        input.tools = vec![echo_tool()];
        let cb: AfterToolCallbackFull = Box::new(|_ctx, _tool, _args, _resp| {
            Box::pin(async { Ok(Some(json!({"rewritten": true}))) })
        });
        input.after_tool_callbacks_full = Arc::new(vec![cb]);
        let events = collect(input).await;
        assert_eq!(final_text(events.last().unwrap()).as_deref(), Some("done"));
        assert_eq!(rt.last_value(), Some(json!({"rewritten": true})));
    }

    #[tokio::test]
    async fn tool_call_id_is_visible_to_the_tool() {
        // The per-call tool context carries the interpreter call id, not a fixed
        // placeholder, so tools can correlate their invocation.
        let rt = Arc::new(ScriptedRuntime::with_suspension(vec![vec![
            Planned::call("whoami", json!({}), 42),
            Planned::Complete(json!({"type": "final_result", "value": "done"})),
        ]]));
        let mut input = base_inputs(FakeLlm::new("noop"), rt.clone(), user("go"));
        input.tools = vec![call_id_tool()];
        collect(input).await;
        assert_eq!(rt.last_value(), Some(json!({"call_id": "42"})));
    }

    #[tokio::test]
    async fn skip_summarization_tool_ends_the_run() {
        // A tool that sets skip_summarization ends the loop, like escalate.
        let events = drive(
            FakeLlm::new("noop"),
            Arc::new(ScriptedRuntime::with_suspension(vec![vec![
                Planned::call("wrap_up", json!({}), 1),
                Planned::Complete(json!({"type": "final_result", "value": "unreached"})),
            ]])),
            vec![skip_summarization_tool()],
            ToolConfirmationPolicy::Never,
            HashMap::new(),
            user("go"),
            None,
            true,
        )
        .await;
        let last = events.last().unwrap();
        assert!(last.actions.skip_summarization);
        assert_eq!(last.actions.state_delta.get(PENDING_STATE_KEY), Some(&Value::Null));
        assert!(events.iter().all(|e| final_text(e).as_deref() != Some("unreached")));
    }

    #[tokio::test]
    async fn max_iterations_exceeded_is_an_error() {
        // The model keeps emitting observations; with a 1-turn budget the loop
        // errors instead of silently terminating.
        let mut input = base_inputs(
            FakeLlm::new("noop"),
            Arc::new(ScriptedRuntime::with_suspension(vec![
                vec![Planned::Complete(json!({"type": "observation", "value": "again"}))],
                vec![Planned::Complete(json!({"type": "observation", "value": "again"}))],
            ])),
            user("go"),
        );
        input.max_iterations = 1;
        let mut stream = Box::pin(run_codeact(input));
        let mut last_err = None;
        while let Some(item) = stream.next().await {
            if let Err(e) = item {
                last_err = Some(e);
            }
        }
        assert!(last_err.unwrap().message.contains("max iterations"));
    }

    #[tokio::test]
    async fn confirmation_suspend_marks_the_turn_interrupted() {
        let rt = Arc::new(ScriptedRuntime::with_suspension(vec![vec![
            Planned::call("echo", json!({}), 1),
            Planned::Complete(json!({"type": "final_result", "value": "unreached"})),
        ]]));
        let mut input = base_inputs(FakeLlm::new("noop"), rt, user("go"));
        input.tools = vec![echo_tool()];
        input.policy = ToolConfirmationPolicy::Always;
        let events = collect(input).await;
        let last = events.last().unwrap();
        assert!(last.actions.tool_confirmation.is_some());
        assert!(last.llm_response.interrupted);
        assert!(last.llm_response.turn_complete);
    }

    #[tokio::test]
    async fn transfer_skips_after_agent_callbacks() {
        // A transfer hands control elsewhere, so after-agent callbacks must not
        // run (mirrors LlmAgent's early return on transfer).
        let after: AfterAgentCallback = Box::new(|_ctx| {
            Box::pin(async { Ok(Some(Content::new("model").with_text("after-done"))) })
        });
        let agent = CodeActAgent::builder()
            .model(FakeLlm::new("noop"))
            .runtime(Arc::new(ScriptedRuntime::with_suspension(vec![vec![Planned::Complete(
                json!({"type": "transfer_to_agent", "agent_name": "child"}),
            )]])))
            .sub_agent(fake_agent("child"))
            .after_callback(after)
            .build()
            .unwrap();
        let events = run_collect(&agent, Arc::new(MockInvocationContext::new(user("go")))).await;
        assert_eq!(transfer_target(events.last().unwrap()), Some("child"));
        assert!(events.iter().all(|e| final_text(e).as_deref() != Some("after-done")));
    }

    #[tokio::test]
    async fn resume_from_pending_result_persists_a_resolved_checkpoint() {
        // Recovering from a SAVE-BEFORE (PendingResult) checkpoint re-runs the
        // tool, but must persist a SAVE-AFTER (Resolved) checkpoint before
        // resuming so a second crash never re-runs the tool again.
        let snapshot = serde_json::to_vec(&vec![Planned::Complete(
            json!({"type": "final_result", "value": "recovered"}),
        )])
        .unwrap();
        let cp = CodeActCheckpoint {
            iteration: 1,
            transcript: vec![],
            snapshot,
            call: PendingToolCall { call_id: 1, tool: "echo".into(), args: json!({"v": 1}) },
            disposition: Disposition::PendingResult,
            tool_roster: vec!["echo".to_string()],
        };
        let events = drive(
            FakeLlm::new("noop"),
            Arc::new(ScriptedRuntime::with_suspension(vec![])),
            vec![echo_tool()],
            ToolConfirmationPolicy::Never,
            HashMap::new(),
            user("ignored"),
            Some(cp),
            true,
        )
        .await;
        // A Resolved checkpoint is persisted before the final result.
        let dispositions: Vec<_> =
            events.iter().filter_map(pending_in).map(|c| c.disposition).collect();
        assert!(dispositions.iter().any(|d| matches!(d, Disposition::Resolved(_))));
        assert_eq!(final_text(events.last().unwrap()).as_deref(), Some("recovered"));
    }

    #[tokio::test]
    async fn tool_route_is_propagated_onto_a_persisted_event() {
        // A tool that sets `actions.route` has it merged onto a persisted event,
        // so the Runner sees it — like `state_delta`/`artifact_delta`.
        let events = drive(
            FakeLlm::new("noop"),
            Arc::new(ScriptedRuntime::with_suspension(vec![vec![
                Planned::call("set_route", json!({}), 1),
                Planned::Complete(json!({"type": "final_result", "value": "done"})),
            ]])),
            vec![route_tool()],
            ToolConfirmationPolicy::Never,
            HashMap::new(),
            user("go"),
            None,
            true,
        )
        .await;
        assert!(
            events.iter().any(|e| e.actions.route.as_deref() == Some(&["next".to_string()][..]))
        );
        assert_eq!(final_text(events.last().unwrap()).as_deref(), Some("done"));
    }

    #[tokio::test]
    async fn escalation_on_resume_ends_the_run_without_resuming() {
        // Recovering a SAVE-BEFORE (PendingResult) checkpoint re-runs the tool;
        // if that tool escalates, the run ends now (no SAVE-AFTER, no resume),
        // matching the inline path.
        let snapshot = serde_json::to_vec(&vec![Planned::Complete(
            json!({"type": "final_result", "value": "unreached"}),
        )])
        .unwrap();
        let cp = CodeActCheckpoint {
            iteration: 1,
            transcript: vec![],
            snapshot,
            call: PendingToolCall { call_id: 1, tool: "panic_button".into(), args: json!({}) },
            disposition: Disposition::PendingResult,
            tool_roster: vec!["panic_button".to_string()],
        };
        let events = drive(
            FakeLlm::new("noop"),
            Arc::new(ScriptedRuntime::with_suspension(vec![])),
            vec![escalating_tool()],
            ToolConfirmationPolicy::Never,
            HashMap::new(),
            user("ignored"),
            Some(cp),
            true,
        )
        .await;
        let last = events.last().unwrap();
        assert!(last.actions.escalate);
        assert_eq!(last.actions.state_delta.get(PENDING_STATE_KEY), Some(&Value::Null));
        // The run ended before resuming, so no Resolved checkpoint was persisted
        // and the script's final result was never reached.
        let dispositions: Vec<_> =
            events.iter().filter_map(pending_in).map(|c| c.disposition).collect();
        assert!(!dispositions.iter().any(|d| matches!(d, Disposition::Resolved(_))));
        assert!(events.iter().all(|e| final_text(e).as_deref() != Some("unreached")));
    }

    #[tokio::test]
    async fn escalation_on_long_running_ends_the_run_before_suspending() {
        // A long-running tool that escalates ends the run instead of suspending
        // to an AwaitingCompletion checkpoint.
        let events = drive(
            FakeLlm::new("noop"),
            Arc::new(ScriptedRuntime::with_suspension(vec![vec![
                Planned::call("slow_escalate", json!({}), 1),
                Planned::Complete(json!({"type": "final_result", "value": "unreached"})),
            ]])),
            vec![escalating_long_running_tool()],
            ToolConfirmationPolicy::Never,
            HashMap::new(),
            user("go"),
            None,
            true,
        )
        .await;
        let last = events.last().unwrap();
        assert!(last.actions.escalate);
        assert!(last.long_running_tool_ids.is_empty());
        assert_eq!(last.actions.state_delta.get(PENDING_STATE_KEY), Some(&Value::Null));
        assert!(events.iter().all(|e| final_text(e).as_deref() != Some("unreached")));
    }

    #[tokio::test]
    async fn after_tool_callback_sees_the_tool_outcome() {
        // After-tool callbacks receive structured `ToolOutcome` metadata via
        // `CallbackContext::tool_outcome()`, matching LlmAgent.
        let rt = Arc::new(ScriptedRuntime::with_suspension(vec![vec![
            Planned::call("echo", json!({"v": 1}), 1),
            Planned::Complete(json!({"type": "final_result", "value": "done"})),
        ]]));
        let mut input = base_inputs(FakeLlm::new("noop"), rt, user("go"));
        input.tools = vec![echo_tool()];
        let seen: Arc<StdMutex<Option<adk_core::ToolOutcome>>> = Arc::new(StdMutex::new(None));
        let seen_cb = seen.clone();
        let cb: AfterToolCallback = Box::new(move |ctx| {
            let seen = seen_cb.clone();
            Box::pin(async move {
                *seen.lock().unwrap() = ctx.tool_outcome();
                Ok(None)
            })
        });
        input.after_tool_callbacks = Arc::new(vec![cb]);
        let events = collect(input).await;
        assert_eq!(final_text(events.last().unwrap()).as_deref(), Some("done"));
        let outcome = seen.lock().unwrap().clone().expect("tool outcome available");
        assert_eq!(outcome.tool_name, "echo");
        assert!(outcome.success);
        assert_eq!(outcome.attempt, 0);
        assert!(outcome.error_message.is_none());
    }

    #[tokio::test]
    async fn after_tool_callback_sees_failed_tool_outcome() {
        // Match LlmAgent: after-tool callbacks still run for failed tools and see
        // `success = false` with the final error message.
        let rt = Arc::new(ScriptedRuntime::with_suspension(vec![vec![
            Planned::call("boom", json!({}), 1),
            Planned::Complete(json!({"type": "final_result", "value": "done"})),
        ]]));
        let mut input = base_inputs(FakeLlm::new("noop"), rt.clone(), user("go"));
        input.tools = vec![failing_tool()];
        let seen: Arc<StdMutex<Option<adk_core::ToolOutcome>>> = Arc::new(StdMutex::new(None));
        let seen_cb = seen.clone();
        let cb: AfterToolCallback = Box::new(move |ctx| {
            let seen = seen_cb.clone();
            Box::pin(async move {
                *seen.lock().unwrap() = ctx.tool_outcome();
                Ok(None)
            })
        });
        input.after_tool_callbacks = Arc::new(vec![cb]);
        let events = collect(input).await;
        assert_eq!(final_text(events.last().unwrap()).as_deref(), Some("done"));
        assert!(rt.last_raise().unwrap().contains("not_found"));
        let outcome = seen.lock().unwrap().clone().expect("tool outcome available");
        assert_eq!(outcome.tool_name, "boom");
        assert!(!outcome.success);
        assert_eq!(outcome.attempt, 0);
        assert!(outcome.error_message.unwrap().contains("not_found"));
    }
}
