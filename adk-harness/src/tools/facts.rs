//! `memory_search` and `memory_remember` (spec 3.1 §7.15.2).

use std::sync::Arc;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Tool, ToolContext, ToolEffect};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::audit::HarnessAudit;
use crate::facts::runtime::FactsRuntime;
use crate::facts::writer::{AdmitContext, Admitted, FactCandidate};
use crate::facts::{FactKind, FactStatus, MemoryFact};
use crate::mode::HarnessMode;
use crate::policy::has_harness_layer;

/// Confidence of a fact the model proposes with `memory_remember`.
pub(crate) const REMEMBER_CONFIDENCE: f32 = 0.5;
/// Most facts one `memory_search` returns.
pub(crate) const SEARCH_MAX: usize = 20;

fn unavailable(message: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::Forbidden,
        "harness.memory.unavailable",
        message,
    )
}

fn invalid(message: impl Into<String>) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::InvalidInput,
        "harness.memory.invalid_args",
        message,
    )
}

pub(crate) fn describe(fact: &MemoryFact) -> Value {
    json!({
        "id": fact.id,
        "status": fact.status.label(),
        "kind": fact.kind.as_str(),
        "text": fact.text,
        "source": fact.provenance.source.as_str(),
        "since": fact.valid_from.format("%Y-%m-%d").to_string(),
        "scope": if fact.scope.project.is_some() { "project" } else { "user" },
    })
}

/// `memory_search`: long-term facts matching a query (spec 3.1 §7.15.2; class `Read`).
///
/// Searches the user's facts in this project and their preferences, `Known` first. Team
/// members get nothing (spec 3.1 §9.2 I42).
///
/// # Example
///
/// ```text
/// memory_search { "query": "how to run the tests", "kind": "procedure", "limit": 5 }
/// ```
pub struct MemorySearchTool {
    facts: Arc<FactsRuntime>,
}

impl MemorySearchTool {
    pub(crate) fn new(facts: Arc<FactsRuntime>) -> Self {
        Self { facts }
    }
}

#[async_trait]
impl Tool for MemorySearchTool {
    fn name(&self) -> &str {
        "memory_search"
    }

    /// Reaches only the facts store the harness was built with.
    fn egress(&self, _args: &serde_json::Value) -> adk_core::egress::ToolEgress {
        adk_core::egress::ToolEgress::Configured
    }

    fn description(&self) -> &str {
        "Search what you remember about this user and project from earlier sessions: \
         preferences, how the project builds and tests, what fixed past failures, decisions. \
         known facts were stated by the user; assumed facts are inferences to check."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "What to look for." },
                "kind": { "type": "string", "enum": ["preference", "project_fact", "procedure", "outcome", "decision", "correction"], "description": "Only facts of this kind." },
                "limit": { "type": "integer", "minimum": 1, "maximum": SEARCH_MAX, "description": "Most facts to return (default 8)." }
            },
            "required": ["query"]
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        if !self.facts.serves(ctx.as_ref()) {
            return Err(unavailable(
                "long-term memory serves the harness's own agent for an authenticated user; \
                 ask the coordinator for what you need",
            ));
        }
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("memory_search needs a string 'query'"))?;
        let kind = match args.get("kind").and_then(Value::as_str) {
            Some(kind) => Some(kind.parse::<FactKind>()?),
            None => None,
        };
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map_or(self.facts.config.recall_limit, |limit| limit as usize)
            .clamp(1, SEARCH_MAX);
        let scope = self.facts.scope(ctx.app_name(), ctx.user_id());
        let recalled = self.facts.recall(&scope, query, kind, limit, None).await?;
        let audit = HarnessAudit::for_tool(ctx.as_ref());
        self.facts.record_recalled(audit.as_ref(), &recalled, query).await?;
        Ok(json!({
            "facts": recalled.facts.iter().map(|(fact, _)| describe(fact)).collect::<Vec<_>>(),
        }))
    }
}

/// `memory_remember`: proposes a long-term fact (spec 3.1 §7.15.2; class `MemoryPropose`:
/// denied in ReadOnly and Plan, allowed in Ask and Auto).
///
/// The fact is `Known`, source `UserStated` — "You said this" (spec 3.3 §8.1) — only when
/// `quote` is found verbatim in one of the person's own messages in this session (spec 3.1
/// §9.2 I41); the model's word alone, or a quote from anywhere else, stores it `Assumed`,
/// "A guess", and the answer says so.
/// A preference applies in every project of the user; every other kind in this project.
/// The fact is redacted, and refused when it holds a secret or is longer than the limit;
/// a refusal is answered `{ "error": reason }`.
/// `supersedes` may name an assumed fact; a known fact changes only by the user's own
/// words or a person's correction.
///
/// # Example
///
/// ```text
/// memory_remember { "text": "The tests need `cargo test --features sqlite`", "kind": "procedure" }
/// memory_remember { "text": "Use pnpm, not npm", "kind": "preference", "quote": "use pnpm, not npm" }
/// ```
pub struct MemoryRememberTool {
    facts: Arc<FactsRuntime>,
}

impl MemoryRememberTool {
    pub(crate) fn new(facts: Arc<FactsRuntime>) -> Self {
        Self { facts }
    }
}

#[async_trait]
impl Tool for MemoryRememberTool {
    fn name(&self) -> &str {
        "memory_remember"
    }

    /// Reaches only the facts store the harness was built with.
    fn egress(&self, _args: &serde_json::Value) -> adk_core::egress::ToolEgress {
        adk_core::egress::ToolEgress::Configured
    }

    fn description(&self) -> &str {
        "Remember a fact for later sessions with this user: a preference, how this project \
         builds or tests, what fixed a failure, a decision. When the user stated it outright \
         in this conversation, copy their exact words into 'quote': the fact is then kept as \
         the user's own words. Anything else is kept as an assumption until the user \
         confirms it. Keep it to one short, self-contained fact; never secrets."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "One fact, at most 500 characters." },
                "kind": { "type": "string", "enum": ["preference", "project_fact", "procedure", "outcome", "decision", "correction"] },
                "quote": { "type": "string", "description": "The user's own words, copied verbatim from one of their messages in this conversation, when they stated the fact outright. Leave it out for anything inferred or read elsewhere." },
                "supersedes": { "type": "string", "description": "Id of an assumed fact this one replaces." }
            },
            "required": ["text", "kind"]
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Idempotent
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let governed = ctx
            .run_config()
            .and_then(|config| config.tool_policy.as_deref())
            .is_some_and(has_harness_layer);
        if !governed {
            return Err(AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Forbidden,
                "harness.memory.ungoverned",
                "memory_remember runs only inside a harness run, under the harness policy",
            ));
        }
        if !self.facts.serves(ctx.as_ref()) {
            return Err(unavailable(
                "long-term memory serves the harness's own agent for an authenticated user; \
                 tell the coordinator what is worth remembering",
            ));
        }
        // The policy denies the class in these modes; this keeps I45 if it is ever bypassed.
        let mode = self.facts.mode(ctx.app_name(), ctx.user_id(), ctx.session_id()).await;
        if matches!(mode, HarnessMode::ReadOnly | HarnessMode::Plan) {
            return Err(unavailable("read-only and plan modes write no facts"));
        }
        let text = args
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("memory_remember needs a string 'text'"))?;
        let kind = args
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("memory_remember needs a string 'kind'"))?;
        let quote =
            args.get("quote").and_then(Value::as_str).filter(|quote| !quote.trim().is_empty());
        let candidate = FactCandidate {
            text: text.to_string(),
            kind: kind.to_string(),
            source: Some("agent_inferred".to_string()),
            quote: quote.map(str::to_string),
            confidence: Some(REMEMBER_CONFIDENCE),
            supersedes: args.get("supersedes").and_then(Value::as_str).map(str::to_string),
        };
        // The quote is checked against what the person typed in this session, read from the
        // session store, never against anything the model passes (spec 3.1 §9.2 I41).
        #[cfg(feature = "runner")]
        let said = match quote {
            Some(_) => {
                self.facts.person_messages(ctx.app_name(), ctx.user_id(), ctx.session_id()).await
            }
            None => Vec::new(),
        };
        #[cfg(not(feature = "runner"))]
        let said: Vec<crate::facts::writer::UserMessage> = Vec::new();
        let run_id = ctx.run_config().and_then(|config| config.durable_run_id.clone());
        let context = AdmitContext {
            app: ctx.app_name(),
            user: ctx.user_id(),
            session_id: ctx.session_id(),
            invocation_id: ctx.invocation_id(),
            run_id,
            user_messages: &said,
            audit: HarnessAudit::for_tool(ctx.as_ref()),
            tick: None,
        };
        let answer = |fact: &MemoryFact, already: bool| {
            let mut answer = json!({ "remembered": describe(fact) });
            if already {
                answer["alreadyKnown"] = json!(true);
            }
            if quote.is_some() && fact.status != FactStatus::Known {
                answer["note"] = json!(
                    "the quote was not found verbatim in the user's own messages in this \
                     conversation, or shares no word with the fact, so the fact is kept as an \
                     assumption"
                );
            }
            answer
        };
        match self.facts.admit(&candidate, &context).await? {
            Admitted::Stored(fact) => Ok(answer(&fact, false)),
            Admitted::Duplicate(fact) => Ok(answer(&fact, true)),
            // The reason reaches the model, which a tool error's code alone would not carry.
            Admitted::Rejected { reason, .. } => {
                Ok(json!({ "error": format!("the fact was not remembered: {reason}") }))
            }
        }
    }
}
