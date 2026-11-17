//! Advisor consultation: a tool an agent calls to ask a stronger or different model for advice
//! mid-task.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use adk_core::{
    AdkError, Content, Effort, ErrorComponent, FinishReason, GenerateContentConfig, Llm,
    LlmRequest, ModelCallScope, Part, Result, Tool, ToolContext, UsageMetadata,
};
use async_trait::async_trait;
use futures::StreamExt;
use futures::future::Either;
use serde_json::{Value, json};

/// The name an [`AdvisorTool`] is declared under unless configured otherwise.
pub const DEFAULT_ADVISOR_TOOL_NAME: &str = "consult_advisor";

/// How many consultations one run may make unless configured otherwise.
pub const DEFAULT_MAX_CONSULTATIONS_PER_RUN: u32 = 3;

/// How many characters of the agent's conversation the advisor sees unless configured
/// otherwise, the most recent kept.
pub const DEFAULT_ADVISOR_HISTORY_CHARS: usize = 24_000;

/// The spend-ledger agent an advisor's calls are attributed to: `advisor:` and the name of
/// the consulting agent, unless configured otherwise.
pub const ADVISOR_SPEND_AGENT_PREFIX: &str = "advisor:";

const DEFAULT_DESCRIPTION: &str = "Consult a stronger model for advice. Call it before you commit \
to a hard or irreversible decision, when you are stuck or your last attempts failed, or before you \
report a complex task as done. Ask one specific question and pass the facts the advisor needs in \
`context`. The advisor sees the conversation so far but cannot call tools; you act on its advice. \
Consultations per run are limited.";

const DEFAULT_INSTRUCTION: &str = "You are an expert advisor consulted by another AI agent in the \
middle of a task. The agent describes its question and context, and you see the conversation so \
far. Give concise, concrete guidance the agent can act on now: the decision you recommend and why, \
the next steps in order (naming the tools to call and with what arguments, when the conversation \
shows them), and the risks or checks it should not skip. Say so plainly when the agent's plan is \
wrong. You cannot call tools or see anything beyond what is shown here.";

/// Runs whose consultation counts are kept; the oldest is forgotten past this.
const TRACKED_RUNS: usize = 1_024;

/// Longest rendering of one tool call or tool result in the advisor's transcript.
const MAX_PART_CHARS: usize = 2_000;

/// A tool that lets an agent consult another model for advice mid-task.
///
/// The provider-neutral counterpart of Google ADK's `ModelConsultTool` and of Claude's advisor
/// tool. The agent calls it with a `question` and optional `context`; the tool sends them,
/// with the most recent part of the agent's conversation, to the advisor model, which runs
/// with no tools, and returns the advice as the tool's result.
///
/// | Guarantee | How |
/// |-----------|-----|
/// | Governed | The call goes through the agent's governed tool path like any tool: policy, approvals, audit and the kill switch apply before it runs |
/// | Kill switch | A frozen run is refused before the advisor is called, and a freeze during the consultation ends it with `governance.frozen` |
/// | Budgeted and ledgered | The advisor's model call runs under the run's [`ModelCallScope`]: one model call in the run budget, and a spend-ledger hold and commit under the advisor's provider |
/// | Own attribution | The spend is attributed to `advisor:{agent}` ([`ADVISOR_SPEND_AGENT_PREFIX`]), or to [`AdvisorToolBuilder::spend_agent`]; org and vendor caps and the run budget cover it |
/// | Per-run cap | At most [`DEFAULT_MAX_CONSULTATIONS_PER_RUN`] consultations per run, counted per root invocation; a call past it returns `limit_reached` without calling the advisor |
///
/// The result is a JSON object whose `status` is one of:
///
/// | `status` | Fields | When |
/// |----------|--------|------|
/// | `ok` | `advice`, `advisorModel`, `consultations`, `usage` | The advisor answered |
/// | `limit_reached` | `message`, `consultations` | The run has used its consultations |
/// | `error` | `error`, `message`, `advisorModel`, `consultations` | The advisor failed, refused or returned no advice; the consultation is not counted unless the advisor responded |
/// | `invalid_request` | `message` | `question` is missing or empty |
///
/// A refusal by the run's budget or spend ledger, and the kill switch, fail the tool call
/// with that error instead, as they fail any other call the run makes.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::Tool;
/// use adk_tool::AdvisorTool;
/// # use adk_core::{Llm, LlmRequest, LlmResponseStream};
/// # struct Advisor;
/// # #[async_trait::async_trait]
/// # impl Llm for Advisor {
/// #     fn name(&self) -> &str { "claude-sonnet-5-5" }
/// #     async fn generate_content(&self, _: LlmRequest, _: bool) -> adk_core::Result<LlmResponseStream> {
/// #         unimplemented!()
/// #     }
/// # }
///
/// let advisor: Arc<dyn Llm> = Arc::new(Advisor);
/// let tool = AdvisorTool::builder(advisor).max_consultations_per_run(2).max_output_tokens(2_048).build();
/// assert_eq!(tool.name(), "consult_advisor");
/// assert_eq!(tool.max_consultations_per_run(), 2);
/// ```
pub struct AdvisorTool {
    model: Arc<dyn Llm>,
    name: String,
    description: String,
    instruction: String,
    max_per_run: u32,
    max_output_tokens: Option<i32>,
    effort: Option<Effort>,
    history_chars: usize,
    spend_agent: Option<String>,
    runs: Mutex<RunCounts>,
}

impl std::fmt::Debug for AdvisorTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdvisorTool")
            .field("name", &self.name)
            .field("model", &self.model.name())
            .field("max_per_run", &self.max_per_run)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("history_chars", &self.history_chars)
            .finish_non_exhaustive()
    }
}

/// Consultations made per run, keyed by the run's root invocation id.
#[derive(Default)]
struct RunCounts {
    used: HashMap<String, u32>,
    order: VecDeque<String>,
}

/// Builds an [`AdvisorTool`].
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::{Effort, Tool};
/// use adk_tool::AdvisorTool;
/// # use adk_core::{Llm, LlmRequest, LlmResponseStream};
/// # struct Advisor;
/// # #[async_trait::async_trait]
/// # impl Llm for Advisor {
/// #     fn name(&self) -> &str { "claude-sonnet-5-5" }
/// #     async fn generate_content(&self, _: LlmRequest, _: bool) -> adk_core::Result<LlmResponseStream> {
/// #         unimplemented!()
/// #     }
/// # }
///
/// let tool = AdvisorTool::builder(Arc::new(Advisor))
///     .name("ask_senior")
///     .effort(Effort::High)
///     .history_chars(0)
///     .spend_agent("advisor:support")
///     .build();
/// assert_eq!(tool.name(), "ask_senior");
/// ```
pub struct AdvisorToolBuilder {
    model: Arc<dyn Llm>,
    name: String,
    description: String,
    instruction: String,
    max_per_run: u32,
    max_output_tokens: Option<i32>,
    effort: Option<Effort>,
    history_chars: usize,
    spend_agent: Option<String>,
}

impl AdvisorToolBuilder {
    /// Declares the tool under `name` instead of [`DEFAULT_ADVISOR_TOOL_NAME`].
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Replaces the description the agent's model reads, which says when to consult.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Replaces the advisor's system instruction.
    pub fn instruction(mut self, instruction: impl Into<String>) -> Self {
        self.instruction = instruction.into();
        self
    }

    /// Allows at most `max` consultations per run. `0` refuses every consultation.
    pub fn max_consultations_per_run(mut self, max: u32) -> Self {
        self.max_per_run = max;
        self
    }

    /// Caps the advisor's output tokens per consultation.
    pub fn max_output_tokens(mut self, max: i32) -> Self {
        self.max_output_tokens = Some(max);
        self
    }

    /// Sets the advisor's reasoning effort.
    pub fn effort(mut self, effort: Effort) -> Self {
        self.effort = Some(effort);
        self
    }

    /// Shows the advisor at most `chars` characters of the agent's conversation, the most
    /// recent kept; `0` sends only the question and context.
    pub fn history_chars(mut self, chars: usize) -> Self {
        self.history_chars = chars;
        self
    }

    /// Attributes the advisor's spend to `agent` instead of `advisor:{agent name}`.
    pub fn spend_agent(mut self, agent: impl Into<String>) -> Self {
        self.spend_agent = Some(agent.into());
        self
    }

    /// Builds the tool.
    pub fn build(self) -> AdvisorTool {
        AdvisorTool {
            model: self.model,
            name: self.name,
            description: self.description,
            instruction: self.instruction,
            max_per_run: self.max_per_run,
            max_output_tokens: self.max_output_tokens,
            effort: self.effort,
            history_chars: self.history_chars,
            spend_agent: self.spend_agent,
            runs: Mutex::new(RunCounts::default()),
        }
    }
}

impl AdvisorTool {
    /// Starts a tool that consults `model`, with the defaults.
    pub fn builder(model: Arc<dyn Llm>) -> AdvisorToolBuilder {
        AdvisorToolBuilder {
            model,
            name: DEFAULT_ADVISOR_TOOL_NAME.to_string(),
            description: DEFAULT_DESCRIPTION.to_string(),
            instruction: DEFAULT_INSTRUCTION.to_string(),
            max_per_run: DEFAULT_MAX_CONSULTATIONS_PER_RUN,
            max_output_tokens: None,
            effort: None,
            history_chars: DEFAULT_ADVISOR_HISTORY_CHARS,
            spend_agent: None,
        }
    }

    /// A tool that consults `model`, with the defaults.
    pub fn new(model: Arc<dyn Llm>) -> Self {
        Self::builder(model).build()
    }

    /// The advisor model.
    pub fn model(&self) -> &Arc<dyn Llm> {
        &self.model
    }

    /// How many consultations one run may make.
    pub fn max_consultations_per_run(&self) -> u32 {
        self.max_per_run
    }

    /// How many consultations the run `root_invocation_id` has made.
    pub fn consultations_used(&self, root_invocation_id: &str) -> u32 {
        self.lock().used.get(root_invocation_id).copied().unwrap_or(0)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RunCounts> {
        self.runs.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Takes one consultation of `run`'s allowance, returning how many are used with it, or
    /// `None` when none is left.
    fn reserve(&self, run: &str) -> Option<u32> {
        let mut runs = self.lock();
        let used = runs.used.get(run).copied().unwrap_or(0);
        if used >= self.max_per_run {
            return None;
        }
        if used == 0 {
            runs.order.push_back(run.to_string());
            while runs.order.len() > TRACKED_RUNS {
                if let Some(oldest) = runs.order.pop_front() {
                    runs.used.remove(&oldest);
                }
            }
        }
        runs.used.insert(run.to_string(), used + 1);
        Some(used + 1)
    }

    /// Gives back a consultation that never reached the advisor.
    fn release(&self, run: &str) {
        let mut runs = self.lock();
        if let Some(used) = runs.used.get_mut(run) {
            *used = used.saturating_sub(1);
        }
    }

    fn consultations(&self, used: u32) -> Value {
        json!({
            "used": used,
            "max": self.max_per_run,
            "remaining": self.max_per_run.saturating_sub(used),
        })
    }

    /// The advisor's request: its instruction, then the question, the agent's context and
    /// the recent conversation.
    fn request(&self, ctx: &dyn ToolContext, question: &str, context: Option<&str>) -> LlmRequest {
        let mut handover = format!(
            "The agent `{agent}` asks for your advice.\n\nQuestion:\n{question}\n",
            agent = ctx.agent_name()
        );
        if let Some(context) = context {
            handover.push_str(&format!("\nContext from the agent:\n{context}\n"));
        }
        if self.history_chars > 0
            && let Some(session) = ctx.session()
        {
            let transcript = transcript(
                &session.conversation_history_for_agent(ctx.agent_name()),
                self.history_chars,
            );
            if !transcript.is_empty() {
                handover.push_str(&format!("\nThe conversation so far:\n{transcript}\n"));
            }
        }
        let mut request = LlmRequest::new(
            self.model.name(),
            vec![
                Content::new("system").with_text(self.instruction.clone()),
                Content::new("user").with_text(handover),
            ],
        );
        if self.max_output_tokens.is_some() || self.effort.is_some() {
            request.config = Some(GenerateContentConfig {
                max_output_tokens: self.max_output_tokens,
                effort: self.effort,
                ..Default::default()
            });
        }
        request
    }

    /// Sends `request` to the advisor under `scope` and gathers its answer.
    async fn consult(
        &self,
        scope: Option<ModelCallScope>,
        request: LlmRequest,
    ) -> Result<Consultation> {
        let mut responses = match &scope {
            Some(scope) => scope.generate(self.model.as_ref(), request, false).await?,
            None => self.model.generate_content(request, false).await?,
        };
        let mut consultation = Consultation::default();
        while let Some(response) = responses.next().await {
            let response = response?;
            if let Some(code) = &response.error_code {
                return Err(AdkError::new(
                    ErrorComponent::Model,
                    adk_core::ErrorCategory::Internal,
                    "tool.advisor.provider_error",
                    format!(
                        "{code}: {}",
                        response
                            .error_message
                            .as_deref()
                            .unwrap_or("the advisor's provider reported an error")
                    ),
                ));
            }
            let text: String = response
                .content
                .iter()
                .flat_map(|content| content.parts.iter())
                .filter_map(Part::text)
                .collect();
            if response.partial {
                consultation.advice.push_str(&text);
            } else if !text.is_empty() {
                consultation.advice = text;
            }
            consultation.finish_reason = response.finish_reason.or(consultation.finish_reason);
            consultation.usage = response.usage_metadata.or(consultation.usage.take());
            consultation.model = response.model.or(consultation.model.take());
            consultation.provider = response.provider.or(consultation.provider.take());
        }
        Ok(consultation)
    }
}

/// What one consultation returned.
#[derive(Default)]
struct Consultation {
    advice: String,
    finish_reason: Option<FinishReason>,
    usage: Option<UsageMetadata>,
    model: Option<String>,
    provider: Option<String>,
}

/// Renders `history` as a labelled transcript of at most about `max_chars` characters, the
/// most recent contents kept whole and the oldest one kept shortened at its start.
fn transcript(history: &[Content], max_chars: usize) -> String {
    let mut kept: Vec<String> = Vec::new();
    let mut total = 0usize;
    for content in history.iter().rev() {
        let rendered = render(content);
        if rendered.is_empty() {
            continue;
        }
        let length = rendered.chars().count();
        if total + length > max_chars {
            let room = max_chars.saturating_sub(total);
            if room > 0 {
                let tail: String = rendered.chars().skip(length - room).collect::<String>();
                kept.push(format!("[earlier conversation omitted] …{tail}"));
            } else {
                kept.push("[earlier conversation omitted]".to_string());
            }
            break;
        }
        total += length;
        kept.push(rendered);
    }
    kept.reverse();
    kept.join("\n")
}

fn render(content: &Content) -> String {
    let speaker = match content.role.as_str() {
        "user" => "User",
        "model" | "assistant" => "Agent",
        "system" => return String::new(),
        _ => "Tool",
    };
    let parts: Vec<String> = content
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } if !text.trim().is_empty() => Some(text.clone()),
            Part::Text { .. } | Part::Thinking { .. } => None,
            Part::FunctionCall { name, args, .. } => {
                Some(format!("[called {name} with {}]", clip(&args.to_string())))
            }
            Part::FunctionResponse { function_response, .. } => Some(format!(
                "[{} returned {}]",
                function_response.name,
                clip(&function_response.response.to_string())
            )),
            Part::InlineData { mime_type, .. } => Some(format!("[{mime_type} attachment]")),
            Part::FileData { mime_type, file_uri, .. } => {
                Some(format!("[{mime_type} file {file_uri}]"))
            }
            Part::ServerToolCall { .. } | Part::ServerToolResponse { .. } => {
                Some("[provider tool activity]".to_string())
            }
            Part::EmbeddedResource { .. } => Some("[embedded resource]".to_string()),
        })
        .collect();
    if parts.is_empty() {
        return String::new();
    }
    format!("{speaker}: {}", parts.join("\n"))
}

fn clip(text: &str) -> String {
    if text.chars().count() <= MAX_PART_CHARS {
        return text.to_string();
    }
    let clipped: String = text.chars().take(MAX_PART_CHARS).collect();
    format!("{clipped}… [truncated]")
}

fn usage_json(usage: Option<&UsageMetadata>) -> Value {
    match usage {
        Some(usage) => json!({
            "inputTokens": usage.prompt_token_count,
            "outputTokens": usage.candidates_token_count,
            "thinkingTokens": usage.thinking_token_count,
            "totalTokens": usage.total_token_count,
            "costUsd": usage.cost,
        }),
        None => Value::Null,
    }
}

#[async_trait]
impl Tool for AdvisorTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "What you need advice on: the decision, the problem, or the plan to check."
                },
                "context": {
                    "type": "string",
                    "description": "Facts the advisor needs that the conversation may not show: findings, constraints, the options you are weighing."
                }
            },
            "required": ["question"]
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let question = args.get("question").and_then(Value::as_str).map(str::trim).unwrap_or("");
        if question.is_empty() {
            return Ok(json!({
                "status": "invalid_request",
                "message": "Pass `question`: say what you need advice on.",
            }));
        }
        let context =
            args.get("context").and_then(Value::as_str).map(str::trim).filter(|c| !c.is_empty());
        if let Some(config) = ctx.run_config() {
            config.check_governance()?;
        }

        let run = ctx.orchestration_root_invocation_id().to_string();
        let Some(used) = self.reserve(&run) else {
            return Ok(json!({
                "status": "limit_reached",
                "message": "This run has used all its advisor consultations. Continue with what you know.",
                "consultations": self.consultations(self.max_per_run),
            }));
        };

        let request = self.request(ctx.as_ref(), question, context);
        let spend_agent = self
            .spend_agent
            .clone()
            .unwrap_or_else(|| format!("{ADVISOR_SPEND_AGENT_PREFIX}{}", ctx.agent_name()));
        let scope =
            ModelCallScope::for_tool(ctx.as_ref()).map(|scope| scope.with_agent(spend_agent));
        let started = std::time::Instant::now();
        let consultation = self.consult(scope, request);
        let outcome = match ctx.run_config().filter(|config| !config.governance.is_empty()) {
            // A freeze during the consultation ends it; dropping the call settles its hold.
            Some(config) => match futures::future::select(
                Box::pin(consultation),
                Box::pin(config.governance_refused()),
            )
            .await
            {
                Either::Left((outcome, _)) => outcome,
                Either::Right((frozen, _)) => Err(frozen),
            },
            None => consultation.await,
        };
        let advisor_model = self.model.name().to_string();

        let consultation = match outcome {
            Ok(consultation) => consultation,
            Err(error) => {
                self.release(&run);
                if error.component != ErrorComponent::Model {
                    // The run's budget, spend ledger or kill switch: not the advisor's failure.
                    return Err(error);
                }
                tracing::warn!(
                    advisor.model = %advisor_model,
                    error.code = error.code,
                    "advisor consultation failed"
                );
                return Ok(json!({
                    "status": "error",
                    "error": error.code,
                    "message": "The advisor could not be reached. Continue with what you know.",
                    "advisorModel": advisor_model,
                    "consultations": self.consultations(used.saturating_sub(1)),
                }));
            }
        };
        let answered_by = consultation.model.clone().unwrap_or(advisor_model);
        let refused = consultation.finish_reason == Some(FinishReason::Refusal);
        if refused || consultation.advice.trim().is_empty() {
            tracing::warn!(
                advisor.model = %answered_by,
                advisor.refused = refused,
                "advisor returned no advice"
            );
            return Ok(json!({
                "status": "error",
                "error": if refused { "tool.advisor.refused" } else { "tool.advisor.empty" },
                "message": if refused {
                    "The advisor declined this question. Continue with what you know, or rephrase it."
                } else {
                    "The advisor returned no advice. Continue with what you know."
                },
                "advisorModel": answered_by,
                "consultations": self.consultations(used),
                "usage": usage_json(consultation.usage.as_ref()),
            }));
        }
        tracing::info!(
            advisor.model = %answered_by,
            advisor.provider = consultation.provider.as_deref().unwrap_or(""),
            advisor.consultation = used,
            advisor.latency_ms = started.elapsed().as_millis() as u64,
            "advisor consulted"
        );
        let mut advice = consultation.advice;
        if consultation.finish_reason == Some(FinishReason::MaxTokens) {
            advice.push_str("\n\n[The advice was cut off at the advisor's output limit.]");
        }
        Ok(json!({
            "status": "ok",
            "advice": advice,
            "advisorModel": answered_by,
            "consultations": self.consultations(used),
            "usage": usage_json(consultation.usage.as_ref()),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{
        BudgetTracker, CallbackContext, ErrorCategory, EventActions, GovernanceControl,
        InMemorySpendLedger, LlmResponse, LlmResponseStream, MemoryEntry, ReadonlyContext,
        RunBudget, RunConfig, SpendKey, SpendLedger, SpendPeriod,
    };

    /// Answers each call with the next scripted reply and records the requests.
    struct Advisor {
        name: &'static str,
        replies: Mutex<VecDeque<Result<LlmResponse>>>,
        requests: Mutex<Vec<LlmRequest>>,
        freeze: Option<GovernanceControl>,
    }

    impl Advisor {
        fn new(replies: impl IntoIterator<Item = Result<LlmResponse>>) -> Arc<Self> {
            Arc::new(Self {
                name: "claude-sonnet-5-5",
                replies: Mutex::new(replies.into_iter().collect()),
                requests: Mutex::new(Vec::new()),
                freeze: None,
            })
        }

        fn requests(&self) -> Vec<LlmRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Llm for Advisor {
        fn name(&self) -> &str {
            self.name
        }

        async fn generate_content(&self, req: LlmRequest, _: bool) -> Result<LlmResponseStream> {
            self.requests.lock().unwrap().push(req);
            if let Some(control) = &self.freeze {
                control.freeze("incident");
                // Never answers: the freeze has to end the consultation.
                return Ok(Box::pin(futures::stream::pending()));
            }
            let reply = self.replies.lock().unwrap().pop_front().expect("a scripted reply");
            Ok(Box::pin(futures::stream::iter([reply])))
        }
    }

    fn advice(text: &str, cost: f64) -> Result<LlmResponse> {
        let mut response = LlmResponse::new(Content::new("model").with_text(text));
        response.model = Some("claude-sonnet-5-5".to_string());
        response.provider = Some("anthropic".to_string());
        response.usage_metadata = Some(UsageMetadata {
            prompt_token_count: 100,
            candidates_token_count: 20,
            total_token_count: 120,
            cost: Some(cost),
            ..Default::default()
        });
        Ok(response)
    }

    struct Ctx {
        invocation: String,
        content: Content,
        config: Option<RunConfig>,
        actions: Mutex<EventActions>,
    }

    impl Ctx {
        fn new(invocation: &str, config: Option<RunConfig>) -> Arc<Self> {
            Arc::new(Self {
                invocation: invocation.to_string(),
                content: Content::new("user").with_text("go"),
                config,
                actions: Mutex::new(EventActions::default()),
            })
        }
    }

    impl ReadonlyContext for Ctx {
        fn invocation_id(&self) -> &str {
            &self.invocation
        }
        fn agent_name(&self) -> &str {
            "worker"
        }
        fn user_id(&self) -> &str {
            "user"
        }
        fn app_name(&self) -> &str {
            "acme"
        }
        fn session_id(&self) -> &str {
            "session"
        }
        fn branch(&self) -> &str {
            ""
        }
        fn user_content(&self) -> &Content {
            &self.content
        }
    }

    #[async_trait]
    impl CallbackContext for Ctx {
        fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
            None
        }
    }

    #[async_trait]
    impl ToolContext for Ctx {
        fn function_call_id(&self) -> &str {
            "call-1"
        }
        fn actions(&self) -> EventActions {
            self.actions.lock().unwrap().clone()
        }
        fn set_actions(&self, actions: EventActions) {
            *self.actions.lock().unwrap() = actions;
        }
        async fn search_memory(&self, _query: &str) -> Result<Vec<MemoryEntry>> {
            Ok(Vec::new())
        }
        fn run_config(&self) -> Option<&RunConfig> {
            self.config.as_ref()
        }
    }

    fn metered() -> (RunConfig, Arc<InMemorySpendLedger>, Arc<BudgetTracker>) {
        let ledger = Arc::new(InMemorySpendLedger::default());
        let tracker = Arc::new(BudgetTracker::new(RunBudget::new()));
        let mut config = RunConfig::builder().spend_ledger(ledger.clone()).build();
        config.budget_tracker = Some(tracker.clone());
        (config, ledger, tracker)
    }

    #[tokio::test]
    async fn a_consultation_returns_the_advice_and_is_metered_under_its_own_attribution() {
        let (config, ledger, tracker) = metered();
        let advisor = Advisor::new([advice("Release the order after the fraud check.", 0.003)]);
        let tool = AdvisorTool::builder(advisor.clone()).max_output_tokens(1_024).build();

        let result = tool
            .execute(
                Ctx::new("run-1", Some(config)),
                json!({ "question": "Release order 42?", "context": "Fraud check passed." }),
            )
            .await
            .unwrap();

        assert_eq!(
            result,
            json!({
                "status": "ok",
                "advice": "Release the order after the fraud check.",
                "advisorModel": "claude-sonnet-5-5",
                "consultations": { "used": 1, "max": 3, "remaining": 2 },
                "usage": {
                    "inputTokens": 100,
                    "outputTokens": 20,
                    "thinkingTokens": null,
                    "totalTokens": 120,
                    "costUsd": 0.003,
                },
            })
        );
        let request = &advisor.requests()[0];
        assert_eq!(request.model, "claude-sonnet-5-5");
        assert!(request.tools.is_empty());
        assert_eq!(request.contents[0].role, "system");
        let handover = request.contents[1].parts[0].text().unwrap();
        assert!(handover.contains("Release order 42?"), "{handover}");
        assert!(handover.contains("Fraud check passed."), "{handover}");
        assert_eq!(request.config.as_ref().unwrap().max_output_tokens, Some(1_024));

        let advisor_key = SpendKey::org("acme").with_agent("advisor:worker");
        assert_eq!(ledger.spent(&advisor_key, SpendPeriod::Day).await.unwrap(), 3_000);
        let vendor_key = SpendKey::org("acme").with_vendor("anthropic");
        assert_eq!(ledger.spent(&vendor_key, SpendPeriod::Day).await.unwrap(), 3_000);
        assert_eq!(tracker.usage().model_calls, 1);
        assert_eq!(tracker.usage().cost_micro_usd, 3_000);
    }

    #[tokio::test]
    async fn the_per_run_cap_refuses_without_calling_the_advisor() {
        let advisor = Advisor::new([advice("a", 0.0), advice("b", 0.0), advice("c", 0.0)]);
        let tool = AdvisorTool::builder(advisor.clone()).max_consultations_per_run(2).build();
        let ctx = Ctx::new("run-1", None);

        for expected in ["a", "b"] {
            let result = tool.execute(ctx.clone(), json!({ "question": "q" })).await.unwrap();
            assert_eq!(result["advice"], expected);
        }
        let refused = tool.execute(ctx.clone(), json!({ "question": "q" })).await.unwrap();
        assert_eq!(refused["status"], "limit_reached");
        assert_eq!(refused["consultations"], json!({ "used": 2, "max": 2, "remaining": 0 }));
        assert_eq!(advisor.requests().len(), 2);
        assert_eq!(tool.consultations_used("run-1"), 2);

        // Another run has its own allowance.
        let result =
            tool.execute(Ctx::new("run-2", None), json!({ "question": "q" })).await.unwrap();
        assert_eq!(result["advice"], "c");
    }

    #[tokio::test]
    async fn a_frozen_run_is_refused_before_the_advisor_is_called() {
        let control = GovernanceControl::new();
        let config = RunConfig::builder().governance(control.clone()).build();
        control.freeze("incident");
        let advisor = Advisor::new([advice("a", 0.0)]);
        let tool = AdvisorTool::new(advisor.clone());

        let refused =
            tool.execute(Ctx::new("run-1", Some(config)), json!({ "question": "q" })).await;
        assert_eq!(refused.unwrap_err().code, adk_core::GOVERNANCE_FROZEN_CODE);
        assert!(advisor.requests().is_empty());
        assert_eq!(tool.consultations_used("run-1"), 0);
    }

    #[tokio::test]
    async fn a_freeze_during_the_consultation_ends_it() {
        let control = GovernanceControl::new();
        let config = RunConfig::builder().governance(control.clone()).build();
        let advisor = Arc::new(Advisor {
            name: "claude-sonnet-5-5",
            replies: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
            freeze: Some(control),
        });
        let tool = AdvisorTool::new(advisor.clone());

        let refused =
            tool.execute(Ctx::new("run-1", Some(config)), json!({ "question": "q" })).await;
        assert_eq!(refused.unwrap_err().code, adk_core::GOVERNANCE_FROZEN_CODE);
        assert_eq!(advisor.requests().len(), 1);
        assert_eq!(tool.consultations_used("run-1"), 0);
    }

    #[tokio::test]
    async fn a_budget_refusal_fails_the_call_and_gives_the_consultation_back() {
        let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_model_calls(0)));
        let mut config = RunConfig::builder().build();
        config.budget_tracker = Some(tracker);
        let advisor = Advisor::new([advice("a", 0.0)]);
        let tool = AdvisorTool::new(advisor.clone());

        let refused =
            tool.execute(Ctx::new("run-1", Some(config)), json!({ "question": "q" })).await;
        assert_eq!(refused.unwrap_err().category, ErrorCategory::ResourceExhausted);
        assert!(advisor.requests().is_empty());
        assert_eq!(tool.consultations_used("run-1"), 0);
    }

    #[tokio::test]
    async fn an_advisor_failure_is_reported_to_the_agent_and_not_counted() {
        let overloaded = AdkError::new(
            ErrorComponent::Model,
            ErrorCategory::Unavailable,
            "model.anthropic.api_error",
            "overloaded",
        );
        let advisor = Advisor::new([Err(overloaded)]);
        let tool = AdvisorTool::new(advisor);

        let result =
            tool.execute(Ctx::new("run-1", None), json!({ "question": "q" })).await.unwrap();
        assert_eq!(result["status"], "error");
        assert_eq!(result["error"], "model.anthropic.api_error");
        assert_eq!(result["consultations"]["used"], 0);
        assert_eq!(tool.consultations_used("run-1"), 0);
    }

    #[tokio::test]
    async fn a_refusal_or_empty_answer_is_an_error_status() {
        let refused = LlmResponse {
            finish_reason: Some(FinishReason::Refusal),
            turn_complete: true,
            ..Default::default()
        };
        let advisor = Advisor::new([Ok(refused), advice("   ", 0.0)]);
        let tool = AdvisorTool::new(advisor);
        let ctx = Ctx::new("run-1", None);

        let result = tool.execute(ctx.clone(), json!({ "question": "q" })).await.unwrap();
        assert_eq!(result["error"], "tool.advisor.refused");
        let result = tool.execute(ctx, json!({ "question": "q" })).await.unwrap();
        assert_eq!(result["error"], "tool.advisor.empty");
        assert_eq!(tool.consultations_used("run-1"), 2);
    }

    #[tokio::test]
    async fn advice_cut_off_at_the_output_limit_says_so() {
        let mut cut = advice("Batch the delete and", 0.0).unwrap();
        cut.finish_reason = Some(FinishReason::MaxTokens);
        let tool = AdvisorTool::new(Advisor::new([Ok(cut)]));
        let result =
            tool.execute(Ctx::new("run-1", None), json!({ "question": "q" })).await.unwrap();
        assert_eq!(
            result["advice"],
            "Batch the delete and\n\n[The advice was cut off at the advisor's output limit.]"
        );
    }

    #[tokio::test]
    async fn an_empty_question_is_invalid_and_not_counted() {
        let advisor = Advisor::new([]);
        let tool = AdvisorTool::new(advisor.clone());
        let result =
            tool.execute(Ctx::new("run-1", None), json!({ "question": "  " })).await.unwrap();
        assert_eq!(result["status"], "invalid_request");
        assert!(advisor.requests().is_empty());
    }

    #[test]
    fn the_transcript_keeps_the_most_recent_conversation() {
        let history = vec![
            Content::new("user").with_text("first question"),
            Content {
                role: "model".to_string(),
                parts: vec![Part::FunctionCall {
                    name: "lookup".to_string(),
                    args: json!({ "id": 42 }),
                    id: None,
                    thought_signature: None,
                }],
            },
            Content {
                role: "function".to_string(),
                parts: vec![Part::FunctionResponse {
                    function_response: adk_core::FunctionResponseData::new(
                        "lookup",
                        json!({ "status": "held" }),
                    ),
                    id: None,
                    annotations: None,
                }],
            },
            Content::new("model").with_text("It is held."),
        ];
        assert_eq!(
            transcript(&history, 10_000),
            "User: first question\nAgent: [called lookup with {\"id\":42}]\n\
             Tool: [lookup returned {\"status\":\"held\"}]\nAgent: It is held."
        );
        assert_eq!(transcript(&history, 18), "[earlier conversation omitted]\nAgent: It is held.");
        let shortened = transcript(&history, 30);
        assert!(shortened.starts_with("[earlier conversation omitted] …"), "{shortened}");
        assert!(shortened.ends_with("\nAgent: It is held."), "{shortened}");
    }
}
