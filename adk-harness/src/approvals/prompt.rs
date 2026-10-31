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

use super::changeset::{ChangeSetAnswer, ChangeSetPrompt};
use super::matcher::glob_matches;
use super::rule::{
    OriginPin, RuleOwner, RulePredicate, RuleSuggestion, StandingDecision, StandingRule,
};
use super::store::StandingRuleStore;
use crate::mode::HarnessMode;
use crate::policy::{HarnessPolicy, PromptContext, ToolKind};
use crate::shell::IsolationStatus;

/// A standing rule that allowed a call without asking (spec 3.1 §7.33 fix 3).
///
/// # Example
///
/// ```rust
/// let hit = adk_harness::RuleHit::new("r1", Some(3), "bash", "bash: cargo test …");
/// assert_eq!(hit.to_string(), "[allowed by rule 3] bash: cargo test …");
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct RuleHit {
    /// The rule's id.
    pub rule_id: String,
    /// The rule's number as `/rules` lists it: every rule of the user, oldest first.
    pub number: Option<usize>,
    /// The tool the call was made to.
    pub tool: String,
    /// What the rule matches.
    pub label: String,
    /// The function call id of the call the rule allowed, so a surface can show the hit
    /// after the call's own line.
    pub function_call_id: Option<String>,
}

impl RuleHit {
    /// A rule hit.
    pub fn new(
        rule_id: impl Into<String>,
        number: Option<usize>,
        tool: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        Self {
            rule_id: rule_id.into(),
            number,
            tool: tool.into(),
            label: label.into(),
            function_call_id: None,
        }
    }

    /// The hit for the call with function call id `id`.
    ///
    /// # Example
    ///
    /// ```rust
    /// let hit = adk_harness::RuleHit::new("r1", Some(3), "bash", "bash: cargo test …")
    ///     .with_function_call_id("call-1");
    /// assert_eq!(hit.function_call_id.as_deref(), Some("call-1"));
    /// ```
    pub fn with_function_call_id(mut self, id: impl Into<String>) -> Self {
        self.function_call_id = Some(id.into());
        self
    }
}

/// `[allowed by rule N] <tool>: <label>`, the label alone when it already names the tool.
impl fmt::Display for RuleHit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.number {
            Some(number) => write!(f, "[allowed by rule {number}] ")?,
            None => write!(f, "[allowed by a standing rule] ")?,
        }
        if self.label.starts_with(&format!("{}:", self.tool)) {
            write!(f, "{}", self.label)
        } else {
            write!(f, "{}: {}", self.tool, self.label)
        }
    }
}

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

    /// Tells the person a standing rule allowed a call the mode would have asked about, so
    /// the call does not run unannounced (spec 3.1 §7.33 fix 3). The policy calls it for a
    /// call a rule allows when it is evaluated, and the handler when a rule granted for an
    /// earlier call of the batch answers a held one. The default shows nothing.
    fn rule_applied(&self, hit: &RuleHit) {
        let _ = hit;
    }

    /// Shows a change set — two or more held edits of one model turn as one diff — and
    /// returns the person's one answer (spec 3.1 §7.26). `None`, the default, has the
    /// handler ask about each call on its own, as a prompter that shows one call at a time
    /// needs.
    ///
    /// # Errors
    ///
    /// Returns the prompter's error.
    async fn prompt_change_set(&self, prompt: ChangeSetPrompt) -> Result<Option<ChangeSetAnswer>> {
        let _ = prompt;
        Ok(None)
    }

    /// Shows a partition crossing (spec 3.2 §7.8) and returns the person's answer: "allow
    /// this read of *X* for this run" (once, for a change), "deny", or "deny and block
    /// crossings to *X* from this partition".
    ///
    /// The default shows the call through [`prompt`](Self::prompt) with a title naming the
    /// crossing and maps the answer with
    /// [`CrossingAnswer::from_prompt_answer`](crate::CrossingAnswer::from_prompt_answer).
    ///
    /// # Errors
    ///
    /// Returns the prompter's error.
    #[cfg(feature = "kernel")]
    async fn prompt_crossing(
        &self,
        prompt: crate::partition::CrossingPrompt,
    ) -> Result<crate::partition::CrossingAnswer> {
        let mut call = prompt.call;
        call.rendered.title =
            format!("Partition crossing into {}: {}", prompt.crossing.label(), call.rendered.title);
        Ok(crate::partition::CrossingAnswer::from_prompt_answer(self.prompt(call).await?))
    }
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
            } else if request.args.get("run_in_background").and_then(Value::as_bool) == Some(true) {
                format!("{tool} on {isolation}, in the background")
            } else {
                format!("{tool} on {isolation}")
            }
        } else {
            tool.to_string()
        };
        let web_url = match request.tool_name.as_str() {
            // The browser pack's navigation is a web call too (spec 3.1 §7.24).
            "web_fetch" | "browser_navigate" | "browser_new_tab" | "browser_new_window" => {
                request.args.get("url").and_then(Value::as_str)
            }
            "web_search" => request.args.get("site").and_then(Value::as_str),
            _ => None,
        };
        let mut full_body = if shell {
            let command = request.args.get("command").and_then(Value::as_str).unwrap_or_default();
            format!("$ {}", escape_control(command))
        } else if let Some(url) = web_url {
            // Host, path and query shown separately, so a credential or a look-alike host
            // in the URL is visible as such (spec 3.1 §7.20 "Prompt").
            let parsed = crate::web::ParsedUrl::parse(url)
                .or_else(|_| crate::web::ParsedUrl::parse(&format!("https://{url}/")));
            let mut body = match parsed {
                Ok(parsed) => format!(
                    "host: {}\npath: {}\nquery: {}",
                    escape_control(&parsed.host),
                    escape_control(&parsed.path),
                    parsed.query.as_deref().map_or_else(|| "(none)".to_string(), escape_control),
                ),
                Err(refusal) => format!("url: {} (refused: {refusal})", escape_control(url)),
            };
            if let Some(query) = request.args.get("query").and_then(Value::as_str) {
                body.push_str(&format!("\nsearch: {}", escape_control(query)));
            }
            body
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
    pub(super) prompter: Arc<dyn ApprovalPrompter>,
    standing: Arc<dyn StandingRuleStore>,
    policy: HarnessPolicy,
    serial: Mutex<Turns>,
    pub(super) per_turn_cap: usize,
    /// The batches of recent model turns, for change sets (spec 3.1 §7.26).
    pub(super) batches: std::sync::Mutex<super::batch::BatchBook>,
    /// The partition whose crossings are put to the person with their three answers.
    #[cfg(feature = "kernel")]
    partition: Option<crate::partition::PartitionWiring>,
}

impl fmt::Debug for PromptingConfirmationHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PromptingConfirmationHandler")
            .field("prompter", &self.prompter)
            .field("per_turn_cap", &self.per_turn_cap)
            .finish()
    }
}

pub(super) fn refuse(message: impl Into<String>) -> ToolApproval {
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
            batches: std::sync::Mutex::default(),
            #[cfg(feature = "kernel")]
            partition: None,
        }
    }

    /// Puts `wiring`'s crossings to the person with their three answers (spec 3.2 §7.8).
    #[cfg(feature = "kernel")]
    pub(crate) fn with_partition(mut self, wiring: crate::partition::PartitionWiring) -> Self {
        self.partition = Some(wiring);
        self
    }

    /// Shows `prompt` under this handler's serial mutex and per-turn cap, for a request
    /// that is not a held tool call: an MCP server's elicitation (spec 3.1 §7.21).
    ///
    /// `turn` is the (app, user, session, invocation) the request belongs to. Returns
    /// `Ok(None)` when the turn reached its prompt cap or a person denied its remaining
    /// calls, so the caller declines.
    ///
    /// # Errors
    ///
    /// Returns the prompter's error.
    pub async fn prompt_elicitation(
        &self,
        turn: (&str, &str, &str, &str),
        prompt: ApprovalPrompt,
    ) -> Result<Option<PromptAnswer>> {
        let (app, user, session, invocation) = turn;
        let mut turns = self.serial.lock().await;
        let state = turns.of(format!("{app}\u{1f}{user}\u{1f}{session}"));
        if state.invocation_id != invocation {
            *state = TurnState { invocation_id: invocation.to_string(), ..TurnState::default() };
        }
        if state.deny_all || state.prompts >= self.per_turn_cap {
            return Ok(None);
        }
        state.prompts += 1;
        let answer = self.prompter.prompt(prompt).await?;
        if matches!(answer, PromptAnswer::DenyAllRemaining) {
            state.deny_all = true;
        }
        Ok(Some(answer))
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
        // A call a change set decided is answered from that decision (spec 3.1 §7.26).
        if let Some(approval) =
            self.batches.lock().unwrap_or_else(|e| e.into_inner()).decided(request)
        {
            return Ok(approval);
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
            // Held by the harness alone, and the harness now allows it: a rule granted for an
            // earlier call of the batch answers it, and the person is told so.
            PolicyDecision::Allow if context.harness_held => {
                if let Some(hit) = &evaluated.rule {
                    self.prompter.rule_applied(hit);
                }
                return Ok(ToolApproval::approve());
            }
            // Held by another layer or the agent's confirmation policy: the person decides.
            PolicyDecision::Allow => {}
            PolicyDecision::Deny { reason } => return Ok(refuse(reason.clone())),
            PolicyDecision::RequireApproval { .. } => {}
        }
        // Two or more held edits of the turn are reviewed as one change set (spec 3.1 §7.26).
        if evaluated.kind == ToolKind::Edit
            && super::rule::EDIT_TOOLS.contains(&request.tool_name.as_str())
            && let Some(approval) = self
                .change_set(&context, request, &policy, &mut turn.prompts, &mut turn.deny_all)
                .await?
        {
            return Ok(approval);
        }
        if turn.deny_all {
            return Ok(refuse("the person denied the remaining calls of this turn"));
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
        // A crossing: allow (for the run, when it reads), deny, or deny and block the
        // partition; never a remembered rule (spec 3.2 §7.8, K-I15).
        #[cfg(feature = "kernel")]
        if let Some((wiring, crossing)) = self
            .partition
            .as_ref()
            .and_then(|wiring| wiring.crossing_of(request).map(|crossing| (wiring, crossing)))
        {
            let crossing_prompt = crate::partition::CrossingPrompt::new(prompt, crossing.clone());
            let answer = tokio::select! {
                answer = self.prompter.prompt_crossing(crossing_prompt) => answer?,
                error = frozen => {
                    tracing::warn!(tool.name = %request.tool_name, "execution was frozen while a person was deciding; the crossing does not run");
                    return Err(error);
                }
            };
            return Ok(match answer {
                crate::partition::CrossingAnswer::Allow if !rendered.can_approve() => {
                    refuse("the call was approved without showing its full arguments")
                }
                crate::partition::CrossingAnswer::Allow => ToolApproval::approve(),
                crate::partition::CrossingAnswer::Deny { message } => {
                    turn.deny_all = true;
                    match message {
                        Some(message) => refuse(message),
                        None => ToolApproval::deny(),
                    }
                }
                crate::partition::CrossingAnswer::DenyAndBlock => {
                    turn.deny_all = true;
                    let by = match &context.acting {
                        Some(acting) => adk_core::audit_log::AuditActor::new(
                            acting,
                            context.invocation_id.clone(),
                        ),
                        None => adk_core::audit_log::AuditActor::default(),
                    };
                    wiring
                        .policy
                        .partitions()
                        .block(crossing.from.clone(), crossing.to.clone(), by)
                        .await
                        .map_err(adk_core::AdkError::from)?;
                    refuse(format!(
                        "denied, and crossings to `{}` from partition `{}` are blocked",
                        crossing.to, crossing.from
                    ))
                }
            });
        }
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
            let fields = rule_audit_fields(&rule, request.function_call_id.as_deref());
            audit.record("harness.rule.created", fields).await?;
        }
        self.standing.grant(rule).await?;
        Ok(())
    }
}

/// The `ext` fields of a `harness.rule.*` record (spec 3.1 §9.3); `created_from_call_id`
/// names the held call the rule was created from.
pub(crate) fn rule_audit_fields(rule: &StandingRule, created_from_call_id: Option<&str>) -> Value {
    let digest = |value: &Value| crate::fsutil::hex_digest(value.to_string().as_bytes());
    let (source, declaration_digest) = match &rule.origin {
        OriginPin::Exact { source, declaration_digest } => {
            (source.clone(), Some(declaration_digest.clone()))
        }
        OriginPin::AnyDeclaration { source } => (source.clone(), None),
        OriginPin::ExactEach { source, .. } => (source.clone(), None),
    };
    let mut fields = json!({
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
        "createdFromCallId": created_from_call_id,
        "by": rule.created_by.id,
    });
    // A folder rule's tool pattern matches any name, so its record keeps each tool and the
    // declaration it is pinned to as data, and says what it covers in the words a person
    // reads (spec 3.3 §8.2).
    if let OriginPin::ExactEach { declarations, .. } = &rule.origin {
        fields["tools"] = json!(declarations.keys().collect::<Vec<_>>());
        fields["declarationDigests"] = json!(declarations);
    }
    let folder = rule.predicates.iter().find_map(|predicate| {
        if let RulePredicate::WritesWithin { dir } = predicate { Some(dir) } else { None }
    });
    if let Some(dir) = folder {
        fields["covers"] = json!(if dir == "." {
            "file writes anywhere in the workspace".to_string()
        } else {
            format!("file writes in {dir}/")
        });
    }
    fields
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

    /// Keeps the turn's batch, so its held edits are reviewed as one change set
    /// (spec 3.1 §7.26, §10 C6).
    async fn batch_started(&self, calls: &[adk_core::BatchCall<'_>]) {
        self.batches.lock().unwrap_or_else(|e| e.into_inner()).started(calls);
    }
}
