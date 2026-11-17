//! The tools that let a model run background sub-agents through a
//! [`SubagentHost`](adk_core::SubagentHost).
//!
//! | Tool | Effect | Does |
//! |---|---|---|
//! | `start_subagent` | non-idempotent | Starts a sub-agent and returns at once; the parent keeps working |
//! | `subagent_status` | read-only | One sub-agent's record with its full answer, or every sub-agent this session started |
//! | `message_subagent` | non-idempotent | Sends a follow-up: steers its running turn, or queues its next run |
//! | `cancel_subagent` | idempotent | Cancels it |
//!
//! Each call goes through the governed tool path like any other: the run's tool policy,
//! confirmation and audit apply, and a policy can require approval for `start_subagent`.
//! The start carves the sub-agent's budget from the calling run's
//! ([`BudgetTracker::carve`](adk_core::BudgetTracker::carve)), limits it to the caller's
//! tools, and is keyed by the call's idempotency key, so a call replayed after a crash
//! starts nothing twice. Every tool answers only for sub-agents of the caller's session.
//!
//! # Example
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use adk_agent::LlmAgentBuilder;
//! use adk_agent::subagents::SubagentTools;
//! use adk_core::SubagentHost;
//!
//! # fn example(model: Arc<dyn adk_core::Llm>, host: Arc<dyn SubagentHost>) -> adk_core::Result<()> {
//! let mut builder = LlmAgentBuilder::new("lead")
//!     .instruction("Delegate long research to background sub-agents and keep working.")
//!     .model(model);
//! for tool in SubagentTools::new(host).tools() {
//!     builder = builder.tool(tool);
//! }
//! let lead = builder.build()?;
//! # let _ = lead;
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;

use adk_core::subagent::{SubagentRecord, SubagentScope, SubagentStart};
use adk_core::{
    AdkError, ErrorCategory, ErrorComponent, Result, RunBudget, SubagentHost, SubagentMode,
    SubagentRequest, Tool, ToolContext, ToolEffect,
};
use async_trait::async_trait;
use serde_json::{Value, json};

/// The most characters of each answer `subagent_status` lists for several sub-agents; one
/// sub-agent's record carries its whole answer.
const LIST_RESULT_CHARS: usize = 600;

/// The four sub-agent tools over one host.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_agent::subagents::SubagentTools;
///
/// # fn example(host: Arc<dyn adk_core::SubagentHost>) {
/// let tools = SubagentTools::new(host).tools();
/// let names: Vec<&str> = tools.iter().map(|tool| tool.name()).collect();
/// assert_eq!(names, ["start_subagent", "subagent_status", "message_subagent", "cancel_subagent"]);
/// # }
/// ```
#[derive(Clone)]
pub struct SubagentTools {
    host: Arc<dyn SubagentHost>,
}

impl std::fmt::Debug for SubagentTools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubagentTools").finish_non_exhaustive()
    }
}

impl SubagentTools {
    /// The tools over `host`.
    pub fn new(host: Arc<dyn SubagentHost>) -> Self {
        Self { host }
    }

    /// `start_subagent`, `subagent_status`, `message_subagent` and `cancel_subagent`.
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(StartSubagentTool::new(Arc::clone(&self.host))),
            Arc::new(SubagentStatusTool { host: Arc::clone(&self.host) }),
            Arc::new(MessageSubagentTool { host: Arc::clone(&self.host) }),
            Arc::new(CancelSubagentTool { host: Arc::clone(&self.host) }),
        ]
    }
}

fn invalid(message: impl Into<String>) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::InvalidInput,
        "subagent.invalid_args",
        message,
    )
}

fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid(format!("`{key}` is required and must be a non-empty string")))
}

fn record_json(record: &SubagentRecord, result_chars: Option<usize>) -> Value {
    let mut value = json!({
        "id": record.id,
        "name": record.name,
        "agent": record.target,
        "status": record.status.as_str(),
        "mode": record.mode.as_str(),
        "runs": record.runs,
        "task": record.task,
        "startedAt": record.created_at,
        "updatedAt": record.updated_at,
    });
    if let Some(result) = &record.result {
        value["result"] = match result_chars {
            Some(max) if result.chars().count() > max => {
                Value::String(format!("{}…", result.chars().take(max).collect::<String>()))
            }
            _ => Value::String(result.clone()),
        };
    }
    if let Some(error) = &record.error {
        value["error"] = Value::String(error.clone());
    }
    value
}

/// `start_subagent`: starts a sub-agent in the background.
pub struct StartSubagentTool {
    host: Arc<dyn SubagentHost>,
    description: String,
}

impl StartSubagentTool {
    /// The tool over `host`, describing the host's targets to the model.
    pub fn new(host: Arc<dyn SubagentHost>) -> Self {
        let mut description = "Starts a sub-agent that works on a task in the background and \
            returns at once with its id, so you keep working. You are told when it finishes — \
            in this turn if it is still running, otherwise in a new turn — and you can check it \
            with subagent_status, send it a follow-up with message_subagent, or stop it with \
            cancel_subagent. Give it a complete, self-contained task. Use mode `fork` to give it \
            a copy of this conversation, `fresh` (the default) to give it only the task."
            .to_string();
        let targets = host.targets();
        if !targets.is_empty() {
            description.push_str(" Agents you can start (`agent`):");
            for target in targets {
                description.push_str(&format!(" `{}` — {};", target.name, target.description));
            }
        }
        Self { host, description }
    }
}

#[async_trait]
impl Tool for StartSubagentTool {
    fn name(&self) -> &str {
        "start_subagent"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "task": { "type": "string", "description": "What the sub-agent is to do, complete and self-contained." },
                "name": { "type": "string", "description": "A short name to refer to it by." },
                "agent": { "type": "string", "description": "The agent to start; this host's default when omitted." },
                "mode": { "type": "string", "enum": ["fresh", "fork"], "description": "`fork` copies this conversation into its session; `fresh` gives it only the task." },
                "maxModelCalls": { "type": "integer", "minimum": 1, "description": "Most model calls it may make; taken out of your own budget." },
                "maxToolCalls": { "type": "integer", "minimum": 1, "description": "Most tool calls it may make." },
                "maxCostUsd": { "type": "number", "exclusiveMinimum": 0, "description": "Most it may spend, in US dollars." },
                "notify": { "type": "boolean", "description": "Whether you are told when it finishes. Defaults to true." }
            },
            "required": ["task"]
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let mut request = SubagentRequest::new(required(&args, "task")?);
        if let Some(name) = args.get("name").and_then(Value::as_str) {
            request = request.with_name(name);
        }
        if let Some(agent) =
            args.get("agent").and_then(Value::as_str).filter(|a| !a.trim().is_empty())
        {
            request = request.with_target(agent.trim());
        }
        request = request.with_mode(match args.get("mode").and_then(Value::as_str) {
            None | Some("fresh") => SubagentMode::Fresh,
            Some("fork") => SubagentMode::Fork,
            Some(other) => {
                return Err(invalid(format!("`mode` is `fresh` or `fork`, not `{other}`")));
            }
        });
        let mut budget = None;
        if let Some(calls) = args.get("maxModelCalls").and_then(Value::as_u64) {
            budget = Some(budget.unwrap_or_else(RunBudget::new).max_model_calls(calls));
        }
        if let Some(calls) = args.get("maxToolCalls").and_then(Value::as_u64) {
            budget = Some(budget.unwrap_or_else(RunBudget::new).max_tool_calls(calls));
        }
        if let Some(usd) = args.get("maxCostUsd").and_then(Value::as_f64) {
            budget = Some(budget.unwrap_or_else(RunBudget::new).max_cost_usd(usd));
        }
        if let Some(budget) = budget {
            request = request.with_budget(budget);
        }
        if args.get("notify").and_then(Value::as_bool) == Some(false) {
            request = request.without_notification();
        }
        let start = SubagentStart::from_tool_context(ctx.as_ref(), request);
        let record = self.host.start(start).await?;
        let mut value = record_json(&record, None);
        value["budget"] = serde_json::to_value(&record.budget).unwrap_or(Value::Null);
        Ok(value)
    }
}

/// `subagent_status`: one sub-agent's record, or every sub-agent of this session.
pub struct SubagentStatusTool {
    host: Arc<dyn SubagentHost>,
}

#[async_trait]
impl Tool for SubagentStatusTool {
    fn name(&self) -> &str {
        "subagent_status"
    }

    fn description(&self) -> &str {
        "Reports on the sub-agents this conversation started: with `id`, one sub-agent and its \
         full answer; without, every one with its status (queued, running, waitingForHuman, \
         parked, completed, failed, cancelled) and the start of its answer."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": { "id": { "type": "string", "description": "The sub-agent's id." } }
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let scope = SubagentScope::from_tool_context(ctx.as_ref());
        match args.get("id").and_then(Value::as_str).filter(|id| !id.trim().is_empty()) {
            Some(id) => match self.host.get(&scope, id.trim()).await? {
                Some(record) => Ok(record_json(&record, None)),
                None => Err(adk_core::subagent::subagent_not_found(id)),
            },
            None => {
                let records = self.host.list(&scope).await?;
                Ok(json!({
                    "subagents": records
                        .iter()
                        .map(|record| record_json(record, Some(LIST_RESULT_CHARS)))
                        .collect::<Vec<_>>()
                }))
            }
        }
    }
}

/// `message_subagent`: a follow-up message to a sub-agent.
pub struct MessageSubagentTool {
    host: Arc<dyn SubagentHost>,
}

#[async_trait]
impl Tool for MessageSubagentTool {
    fn name(&self) -> &str {
        "message_subagent"
    }

    fn description(&self) -> &str {
        "Sends a follow-up message to a sub-agent this conversation started. A running \
         sub-agent reads it at its next step; otherwise it is queued as its next turn, which \
         also continues a finished sub-agent with its earlier work in context."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "The sub-agent's id." },
                "message": { "type": "string", "description": "What to tell it." }
            },
            "required": ["id", "message"]
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let scope = SubagentScope::from_tool_context(ctx.as_ref());
        let id = required(&args, "id")?;
        let message = required(&args, "message")?;
        let follow_up = self.host.send(&scope, id, message).await?;
        let mut value = serde_json::to_value(&follow_up).unwrap_or(Value::Null);
        value["id"] = Value::String(id.to_string());
        Ok(value)
    }
}

/// `cancel_subagent`: cancels a sub-agent.
pub struct CancelSubagentTool {
    host: Arc<dyn SubagentHost>,
}

#[async_trait]
impl Tool for CancelSubagentTool {
    fn name(&self) -> &str {
        "cancel_subagent"
    }

    fn description(&self) -> &str {
        "Cancels a sub-agent this conversation started. A waiting one stops at once, a running \
         one at its next step. You are not told about a cancellation you asked for."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": { "id": { "type": "string", "description": "The sub-agent's id." } },
            "required": ["id"]
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Idempotent
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let scope = SubagentScope::from_tool_context(ctx.as_ref());
        let record = self.host.cancel(&scope, required(&args, "id")?).await?;
        Ok(record_json(&record, Some(LIST_RESULT_CHARS)))
    }
}
