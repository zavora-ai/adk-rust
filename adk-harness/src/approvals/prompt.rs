//! Interactive approval prompts (spec 3.1 §7.5).

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use adk_core::principal::{SubjectKind, SubjectRef};
use adk_core::{
    PolicyCall, PolicyDecision, Result, ToolApproval, ToolConfirmationDecision,
    ToolConfirmationHandler, ToolConfirmationRequest, async_trait,
};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use super::matcher::glob_matches;
use super::rule::{OriginPin, RuleOwner, RuleSuggestion, StandingDecision, StandingRule};
use super::store::StandingRuleStore;
use crate::mode::HarnessMode;
use crate::policy::{HarnessPolicy, PromptContext, ToolKind};
use crate::shell::IsolationStatus;

/// Asks a person to decide one held call (spec 3.1 §7.5).
///
/// Implemented by interactive surfaces: a terminal, an editor, a chat channel.
///
/// # Example
///
/// ```rust
/// use adk_harness::{ApprovalPrompt, ApprovalPrompter, PromptAnswer};
///
/// /// Approves everything once; for tests.
/// #[derive(Debug)]
/// struct Yes;
///
/// #[async_trait::async_trait]
/// impl ApprovalPrompter for Yes {
///     async fn prompt(&self, _prompt: ApprovalPrompt) -> adk_core::Result<PromptAnswer> {
///         Ok(PromptAnswer::ApproveOnce)
///     }
/// }
/// ```
#[async_trait]
pub trait ApprovalPrompter: fmt::Debug + Send + Sync {
    /// Shows `prompt` and returns the person's answer.
    async fn prompt(&self, prompt: ApprovalPrompt) -> Result<PromptAnswer>;
}

/// What a prompt shows (spec 3.1 §7.5, §9.2 I21).
///
/// # Example
///
/// ```rust,ignore
/// println!("{}\n{}", prompt.rendered.title, prompt.rendered.body);
/// ```
#[derive(Clone, Debug)]
pub struct ApprovalPrompt {
    /// The held call, with its final arguments, acting agent and origin.
    pub request: ToolConfirmationRequest,
    /// The call's class.
    pub kind: ToolKind,
    /// The session's mode.
    pub mode: HarnessMode,
    /// Where `bash` runs.
    pub isolation: IsolationStatus,
    /// Rules the person may choose to remember.
    pub suggestions: Vec<RuleSuggestion>,
    /// The call rendered for display.
    pub rendered: RenderedCall,
}

/// A person's answer to an [`ApprovalPrompt`] (spec 3.1 §7.5).
///
/// # Example
///
/// ```rust
/// use adk_harness::PromptAnswer;
///
/// let answer = PromptAnswer::Deny { message: Some("use the staging database".into()) };
/// assert!(matches!(answer, PromptAnswer::Deny { .. }));
/// ```
#[derive(Clone, Debug, PartialEq)]
pub enum PromptAnswer {
    /// Run this call once.
    ApproveOnce,
    /// Run this call once with these arguments; the policy checks them again.
    ApproveWithArgs(Value),
    /// Run this call and remember the rule.
    ApproveAndRemember(RuleSuggestion),
    /// Do not run this call.
    Deny {
        /// Reported to the model.
        message: Option<String>,
    },
    /// Do not run this call and remember a Deny rule.
    DenyAndRemember(RuleSuggestion),
    /// Do not run this call or any other call of this turn.
    DenyAllRemaining,
}

/// Default byte budget of [`RenderedCall::body`] (spec 3.1 §7.5).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::RENDERED_CALL_MAX_BYTES, 4096);
/// ```
pub const RENDERED_CALL_MAX_BYTES: usize = 4096;
/// Most characters of a model-supplied retry reason a prompt shows.
const REASON_MAX_CHARS: usize = 300;

/// Escapes control characters, including ANSI escapes, carriage returns and bidi
/// overrides, so a prompt shows exactly what runs (spec 3.1 §9.1 "Approval UX").
///
/// # Example
///
/// ```rust
/// use adk_harness::escape_control;
///
/// assert_eq!(escape_control("ls\r; curl evil\x1b[2K"), "ls\\r; curl evil\\x1b[2K");
/// assert_eq!(escape_control("a\u{202e}b"), "a\\u{202e}b");
/// ```
pub fn escape_control(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 || (0x7f..0xa0).contains(&(c as u32)) => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
            | '\u{feff}' => {
                out.push_str(&format!("\\u{{{:04x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

fn cut(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }
    let end = text.floor_char_boundary(max_bytes);
    (format!("{}… [truncated: {} of {} bytes shown]", &text[..end], end, text.len()), true)
}

/// A held call rendered for a person (spec 3.1 §7.5, §9.2 I21).
///
/// Control characters are escaped, the origin, acting agent and isolation are shown, and a
/// body longer than the budget is truncated with a marker. A truncated call cannot be
/// approved until [`expand`](Self::expand) has shown the whole body.
///
/// # Example
///
/// ```rust
/// use adk_core::ToolConfirmationRequest;
/// use adk_harness::{IsolationStatus, RenderedCall};
/// use serde_json::json;
///
/// let long = "x".repeat(10_000);
/// let request = ToolConfirmationRequest::new("bash", json!({ "command": long }));
/// let rendered = RenderedCall::render(&request, &IsolationStatus::Host, 100);
/// assert!(rendered.truncated);
/// assert!(!rendered.can_approve());
/// assert_eq!(rendered.expand().len(), "$ ".len() + 10_000);
/// assert!(rendered.can_approve());
/// ```
#[derive(Clone, Debug)]
pub struct RenderedCall {
    /// One line naming the tool and the isolation; never contains model-written text.
    pub title: String,
    /// Who publishes the tool: source and declaration digest.
    pub origin: String,
    /// The acting agent and the subject it acts for.
    pub agent: String,
    /// The isolation the call runs under.
    pub isolation: String,
    /// The escaped arguments, cut to the budget.
    pub body: String,
    /// Whether [`body`](Self::body) was cut.
    pub truncated: bool,
    full_body: String,
    expanded: Arc<AtomicBool>,
}

impl RenderedCall {
    /// Renders `request` with a body of at most `max_bytes` bytes.
    pub fn render(
        request: &ToolConfirmationRequest,
        isolation: &IsolationStatus,
        max_bytes: usize,
    ) -> Self {
        let tool = escape_control(&request.tool_name);
        let shell = request.tool_name == "bash";
        let title = if shell {
            if request.args.get("unsandboxed").and_then(Value::as_bool) == Some(true) {
                format!("{tool}: unsandboxed retry on the host (full user permissions, network)")
            } else {
                format!("{tool} on {isolation}")
            }
        } else {
            tool.to_string()
        };
        let mut full_body = if shell {
            let command = request.args.get("command").and_then(Value::as_str).unwrap_or_default();
            format!("$ {}", escape_control(command))
        } else {
            escape_control(&request.args.to_string())
        };
        if shell && let Some(reason) = request.args.get("reason").and_then(Value::as_str) {
            let reason: String = reason.chars().take(REASON_MAX_CHARS).collect();
            full_body.push_str(&format!("\nmodel's reason: \"{}\"", escape_control(&reason)));
        }
        let (body, truncated) = cut(&full_body, max_bytes);
        let origin = request.origin.as_ref().map_or_else(
            || "unknown origin".to_string(),
            |origin| format!("{} ({})", escape_control(&origin.source), origin.declaration_digest),
        );
        let agent = request.acting.as_deref().map_or_else(
            || "unknown agent".to_string(),
            |acting| match &acting.on_behalf_of {
                Some(subject) => format!("{} for {}", acting.agent.id, escape_control(&subject.id)),
                None => acting.agent.id.to_string(),
            },
        );
        Self {
            title,
            origin,
            agent,
            isolation: isolation.to_string(),
            body,
            truncated,
            full_body,
            expanded: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The whole escaped body. Showing it lets a truncated call be approved.
    pub fn expand(&self) -> &str {
        self.expanded.store(true, Ordering::SeqCst);
        &self.full_body
    }

    /// Whether [`expand`](Self::expand) was called on this rendering or a clone of it.
    pub fn was_expanded(&self) -> bool {
        self.expanded.load(Ordering::SeqCst)
    }

    /// Whether an approval of this rendering may stand: the body was shown in full.
    pub fn can_approve(&self) -> bool {
        !self.truncated || self.was_expanded()
    }
}

/// Most prompts one turn may show before the remaining calls are denied (spec 3.1 §7.5).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::DEFAULT_PROMPTS_PER_TURN, 20);
/// ```
pub const DEFAULT_PROMPTS_PER_TURN: usize = 20;

#[derive(Debug, Default)]
struct TurnState {
    invocation_id: String,
    prompts: usize,
    deny_all: bool,
}

/// Most sessions whose turn state is kept; the oldest is dropped first.
const TURN_STATES_CAP: usize = 1024;

/// Turn state per session, so one session's "deny all remaining" and prompt cap never
/// reset or spend another's (threat I-3).
#[derive(Debug, Default)]
struct Turns {
    states: std::collections::HashMap<String, TurnState>,
    order: std::collections::VecDeque<String>,
}

impl Turns {
    fn of(&mut self, key: String) -> &mut TurnState {
        if !self.states.contains_key(&key) {
            self.order.push_back(key.clone());
            while self.order.len() > TURN_STATES_CAP {
                if let Some(oldest) = self.order.pop_front() {
                    self.states.remove(&oldest);
                }
            }
        }
        self.states.entry(key).or_default()
    }
}

/// A [`ToolConfirmationHandler`] that asks an [`ApprovalPrompter`] (spec 3.1 §7.5).
///
/// Prompts are serialized, because calls in a batch are decided concurrently, and capped
/// per turn of each session. Before prompting, the call is decided again by the run's whole policy — the
/// harness layer with the call's origin, made stricter by the run's and the operator's
/// layers — so a rule granted for an earlier call in the same batch applies without a
/// prompt when only the harness asked. A call another layer requires approval for, or the
/// agent's confirmation policy holds, is always put to the person. An approval with edited arguments returns them through `decide_approval`, and
/// the governed path checks them against the policy again. After a person denies a call,
/// the rest of the turn's calls are denied (I19).
///
/// # Example
///
/// ```rust,ignore
/// let harness = HarnessAgent::builder("harness")
///     .model(model)
///     .workspace(workspace)
///     .prompter(Arc::new(ConsolePrompter::new()))
///     .build()?;
/// ```
pub struct PromptingConfirmationHandler {
    prompter: Arc<dyn ApprovalPrompter>,
    standing: Arc<dyn StandingRuleStore>,
    policy: HarnessPolicy,
    serial: Mutex<Turns>,
    per_turn_cap: usize,
}

impl fmt::Debug for PromptingConfirmationHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PromptingConfirmationHandler")
            .field("prompter", &self.prompter)
            .field("per_turn_cap", &self.per_turn_cap)
            .finish()
    }
}

fn refuse(message: impl Into<String>) -> ToolApproval {
    ToolApproval::deny().with_message(message)
}

impl PromptingConfirmationHandler {
    /// A handler asking `prompter`, granting remembered rules in `standing`, and
    /// re-checking each call against `policy`.
    pub fn new(
        prompter: Arc<dyn ApprovalPrompter>,
        standing: Arc<dyn StandingRuleStore>,
        policy: HarnessPolicy,
    ) -> Self {
        Self {
            prompter,
            standing,
            policy,
            serial: Mutex::new(Turns::default()),
            per_turn_cap: DEFAULT_PROMPTS_PER_TURN,
        }
    }

    /// Sets the most prompts one turn may show (default [`DEFAULT_PROMPTS_PER_TURN`]).
    #[must_use]
    pub fn with_per_turn_cap(mut self, cap: usize) -> Self {
        self.per_turn_cap = cap;
        self
    }

    /// Decides `request`, with the arguments a person substituted and their message
    /// (spec 3.1 §7.5, §10 C3).
    ///
    /// # Errors
    ///
    /// Returns the prompter's error, or the rule store's when a remembered rule cannot be
    /// stored.
    async fn approval_for(&self, request: &ToolConfirmationRequest) -> Result<ToolApproval> {
        let mut turns = self.serial.lock().await;
        let Some(context) = self.policy.prompts().take(request) else {
            tracing::warn!(tool.name = %request.tool_name, "held call was not evaluated by the harness policy; not run");
            return Ok(refuse("the harness policy did not evaluate this call, so it is not run"));
        };
        let turn = turns.of(format!(
            "{}\u{1f}{}\u{1f}{}",
            context.app_name, context.user_id, context.session_id
        ));
        if turn.invocation_id != context.invocation_id {
            *turn =
                TurnState { invocation_id: context.invocation_id.clone(), ..TurnState::default() };
        }
        if turn.deny_all {
            return Ok(refuse("the person denied the remaining calls of this turn"));
        }

        let policy = self.policy.with_audit(context.audit.clone());
        let policy_request = context.policy_request(&request.tool_name, &request.args);
        let evaluated = policy
            .evaluate_full(
                &policy_request,
                request.origin.as_ref(),
                request.function_call_id.as_deref(),
                false,
            )
            .await;
        // The re-check covers the run's whole policy: a rule granted for an earlier call
        // in the batch answers the harness's own approval, never another layer's.
        let peer_decision = match &context.peers {
            Some(peers) => {
                let mut call = PolicyCall::new();
                if let Some(origin) = request.origin.as_ref() {
                    call = call.with_origin(origin);
                }
                if let Some(call_id) = request.function_call_id.as_deref() {
                    call = call.with_function_call_id(call_id);
                }
                Some(peers.evaluate_call(&policy_request, &call).await)
            }
            None => None,
        };
        let composed = match peer_decision.clone() {
            Some(peers) => evaluated.decision.clone().stricter(peers),
            None => evaluated.decision.clone(),
        };
        match &composed {
            // Held by the harness alone, and the harness now allows it.
            PolicyDecision::Allow if context.harness_held => return Ok(ToolApproval::approve()),
            // Held by another layer or the agent's confirmation policy: the person decides.
            PolicyDecision::Allow => {}
            PolicyDecision::Deny { reason } => return Ok(refuse(reason.clone())),
            PolicyDecision::RequireApproval { .. } => {}
        }
        if turn.prompts >= self.per_turn_cap {
            return Ok(refuse(format!(
                "this turn reached its limit of {} approval prompts",
                self.per_turn_cap
            )));
        }
        turn.prompts += 1;

        let isolation = policy.isolation_status().clone();
        // A remembered rule answers only the harness's own approval, so one is offered only
        // when no other layer asks too.
        let only_the_harness_asks =
            matches!(evaluated.decision, PolicyDecision::RequireApproval { .. })
                && peer_decision.as_ref().is_none_or(|peers| *peers == PolicyDecision::Allow);
        let suggestions = if only_the_harness_asks
            && evaluated.kind.rule_eligible()
            && !evaluated.base.is_non_waivable()
        {
            StandingRule::suggestions(request)
        } else {
            Vec::new()
        };
        let rendered = RenderedCall::render(request, &isolation, RENDERED_CALL_MAX_BYTES);
        let prompt = ApprovalPrompt {
            request: request.clone(),
            kind: evaluated.kind,
            mode: evaluated.mode,
            isolation,
            suggestions: suggestions.clone(),
            rendered: rendered.clone(),
        };
        // A freeze resolves an open prompt: the call is not run and the turn ends with the
        // freeze, as the governed path ends it when a freeze lands after an approval (I20).
        let frozen = async {
            if context.governance.is_empty() {
                return std::future::pending::<adk_core::AdkError>().await;
            }
            loop {
                if let Some(error) =
                    context.governance.iter().find_map(|control| control.check().err())
                {
                    return error;
                }
                tokio::time::sleep(crate::tools::GOVERNANCE_POLL_INTERVAL).await;
            }
        };
        let answer = tokio::select! {
            answer = self.prompter.prompt(prompt) => answer?,
            error = frozen => {
                tracing::warn!(tool.name = %request.tool_name, "execution was frozen while a person was deciding; the call is not run");
                return Err(error);
            }
        };
        let unexpanded = || refuse("the call was approved without showing its full arguments");
        Ok(match answer {
            PromptAnswer::ApproveOnce => {
                if !rendered.can_approve() {
                    unexpanded()
                } else {
                    ToolApproval::approve()
                }
            }
            PromptAnswer::ApproveWithArgs(args) => {
                if !rendered.can_approve() {
                    unexpanded()
                } else {
                    ToolApproval::approve().with_edited_args(args)
                }
            }
            PromptAnswer::ApproveAndRemember(suggestion) => {
                if !rendered.can_approve() {
                    unexpanded()
                } else {
                    self.remember(
                        &context,
                        request,
                        &suggestions,
                        suggestion,
                        StandingDecision::Allow,
                    )
                    .await?;
                    ToolApproval::approve()
                }
            }
            PromptAnswer::Deny { message } => {
                turn.deny_all = true;
                match message {
                    Some(message) => refuse(message),
                    None => ToolApproval::deny(),
                }
            }
            PromptAnswer::DenyAndRemember(suggestion) => {
                turn.deny_all = true;
                let reason = format!("denied by {}", context.user_id);
                self.remember(
                    &context,
                    request,
                    &suggestions,
                    suggestion,
                    StandingDecision::Deny { reason: reason.clone() },
                )
                .await?;
                refuse(reason)
            }
            PromptAnswer::DenyAllRemaining => {
                turn.deny_all = true;
                refuse("the person denied the remaining calls of this turn")
            }
        })
    }

    /// Stores the rule a person chose at a prompt and records `harness.rule.created`.
    async fn remember(
        &self,
        context: &PromptContext,
        request: &ToolConfirmationRequest,
        offered: &[RuleSuggestion],
        chosen: RuleSuggestion,
        decision: StandingDecision,
    ) -> Result<()> {
        // Only a rule the prompt offered, or one at least as narrow, is accepted: same tool,
        // exact pin to this call's declaration.
        let pinned = match (&chosen.origin, request.origin.as_ref()) {
            (OriginPin::Exact { .. }, Some(origin)) => chosen.origin.admits(origin),
            _ => false,
        };
        if !pinned || !glob_matches(&chosen.tool, &request.tool_name) || chosen.tool.contains('*') {
            tracing::warn!(tool.name = %request.tool_name, "ignoring a remembered rule that does not pin this call's tool");
            return Ok(());
        }
        if decision == StandingDecision::Allow && !offered.contains(&chosen) {
            tracing::warn!(tool.name = %request.tool_name, "ignoring an allow rule the prompt did not offer");
            return Ok(());
        }
        let subject = context
            .acting
            .as_ref()
            .and_then(|acting| acting.on_behalf_of.clone())
            .unwrap_or_else(|| SubjectRef::new(SubjectKind::Human, context.user_id.clone()));
        let owner = RuleOwner {
            app_name: context.app_name.clone(),
            user_id: context.user_id.clone(),
            subject_kind: subject.kind,
            session_id: context.session_id.clone(),
            created_by: subject,
        };
        let rule = chosen.into_rule(&owner, decision, Some(request.fingerprint()));
        if let Some(audit) = context.audit() {
            audit.record("harness.rule.created", rule_audit_fields(&rule, request)).await?;
        }
        self.standing.grant(rule).await?;
        Ok(())
    }
}

/// The `ext` fields of a `harness.rule.*` record (spec 3.1 §9.3).
pub(crate) fn rule_audit_fields(rule: &StandingRule, request: &ToolConfirmationRequest) -> Value {
    let digest = |value: &Value| crate::fsutil::hex_digest(value.to_string().as_bytes());
    let (source, declaration_digest) = match &rule.origin {
        OriginPin::Exact { source, declaration_digest } => {
            (source.clone(), Some(declaration_digest.clone()))
        }
        OriginPin::AnyDeclaration { source } => (source.clone(), None),
    };
    json!({
        "ruleId": rule.id,
        "ruleDigest": digest(&serde_json::to_value(rule).unwrap_or(Value::Null)),
        "subject": rule.scope.user_id(),
        "app": rule.scope.app_name(),
        "tool": rule.tool,
        "source": source,
        "declarationDigest": declaration_digest,
        "predicatesDigest": digest(&serde_json::to_value(&rule.predicates).unwrap_or(Value::Null)),
        "decision": serde_json::to_value(&rule.decision).unwrap_or(Value::Null),
        "expiresAt": rule.expires_at,
        "createdFromCallId": request.function_call_id,
        "by": rule.created_by.id,
    })
}

#[async_trait]
impl ToolConfirmationHandler for PromptingConfirmationHandler {
    async fn decide(&self, request: &ToolConfirmationRequest) -> Result<ToolConfirmationDecision> {
        let approval = self.approval_for(request).await?;
        // `decide` cannot carry edited arguments; the governed path asks
        // `decide_approval`, and a direct caller of `decide` gets the safe direction.
        if approval.edited_args.is_some() {
            return Ok(ToolConfirmationDecision::Deny);
        }
        Ok(approval.decision)
    }

    async fn decide_approval(&self, request: &ToolConfirmationRequest) -> Result<ToolApproval> {
        self.approval_for(request).await
    }
}
