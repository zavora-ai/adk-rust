//! `HarnessToolset`: the harness tools and per-request reminders (spec 3.1 §7.6, §7.7).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use adk_core::{Content, LlmRequest, ReadonlyContext, Result, Tool, ToolOrigin, Toolset};
use async_trait::async_trait;

use crate::approvals::matcher::glob_matches;
use crate::approvals::{StandingDecision, StandingRule, StandingRuleStore};
use crate::mode::{HarnessMode, ModeStore};
use crate::tools::TodoList;

/// The most rules the reminder's digest lists (spec 3.1 §7.25).
const DIGEST_MAX_RULES: usize = 20;
/// The most bytes of the digest's rule lines (spec 3.1 §7.25).
const DIGEST_MAX_BYTES: usize = 1024;
/// The most characters of a deny reason the digest quotes.
const DIGEST_REASON_CHARS: usize = 80;

/// The standing rules that apply to a session, for the per-request reminder
/// (spec 3.1 §7.25): rules a call of this harness's declared tools can match.
struct RulesDigest {
    store: Arc<dyn StandingRuleStore>,
    /// Every declared tool with its origin, so a rule pinned to another declaration is left
    /// out.
    origins: Vec<(String, ToolOrigin)>,
}

impl RulesDigest {
    /// The digest for (`app`, `user`, `session`), or `None` when no rule applies. Rules
    /// are numbered as `/rules` numbers them: every rule of the user, oldest first.
    async fn render(&self, app: &str, user: &str, session: &str) -> Option<String> {
        let mut rules = self.store.list(app, user).await.ok()?;
        rules.sort_by_key(|rule| rule.created_at);
        let now = chrono::Utc::now();
        let mut applicable: Vec<(usize, &StandingRule)> = rules
            .iter()
            .enumerate()
            .map(|(index, rule)| (index + 1, rule))
            .filter(|(_, rule)| {
                !rule.is_expired(now)
                    && rule.scope.session_id().is_none_or(|id| id == session)
                    && self.origins.iter().any(|(name, origin)| {
                        glob_matches(&rule.tool, name) && rule.origin.admits(origin)
                    })
            })
            .collect();
        if applicable.is_empty() {
            return None;
        }
        // Deny before allow, then by number.
        applicable.sort_by_key(|(number, rule)| {
            (!matches!(rule.decision, StandingDecision::Deny { .. }), *number)
        });
        let mut lines = vec![
            "Standing rules for you here; a call one matches runs or is refused without asking:"
                .to_string(),
        ];
        let mut used = 0;
        let mut shown = 0;
        for (number, rule) in &applicable {
            let line = digest_line(*number, rule);
            if shown == DIGEST_MAX_RULES || used + line.len() > DIGEST_MAX_BYTES {
                break;
            }
            used += line.len();
            shown += 1;
            lines.push(line);
        }
        if shown < applicable.len() {
            lines.push(format!("(+{} more; /rules lists them)", applicable.len() - shown));
        }
        Some(lines.join("\n"))
    }
}

/// One rule of the digest: number, decision, tool, what it matches and its scope; the
/// label passes the secret redaction and a deny reason is quoted and cut.
fn digest_line(number: usize, rule: &StandingRule) -> String {
    let scope = match rule.scope.session_id() {
        Some(_) => "this session",
        None => "every session",
    };
    let label = redact(&rule.label);
    match &rule.decision {
        StandingDecision::Allow => format!("- [{number}] allow {label} ({scope})"),
        StandingDecision::Deny { reason } => {
            let mut reason: String = redact(reason).chars().take(DIGEST_REASON_CHARS).collect();
            if reason.chars().count() == DIGEST_REASON_CHARS {
                reason.push('…');
            }
            format!("- [{number}] deny {label} ({scope}): \"{reason}\"")
        }
    }
}

/// `text` with secrets replaced: the web screen's patterns where compiled, and any
/// whitespace-free token of 24 or more letters and digits otherwise.
fn redact(text: &str) -> String {
    #[cfg(feature = "web")]
    let text = crate::web::redact_secrets(text).0;
    #[cfg(not(feature = "web"))]
    let text = text.to_string();
    text.split(' ')
        .map(|word| {
            let alnum = word.chars().filter(char::is_ascii_alphanumeric).count();
            if alnum >= 24
                && word.chars().any(|c| c.is_ascii_digit())
                && word.chars().any(|c| c.is_ascii_alphabetic())
            {
                "[redacted]".to_string()
            } else {
                word.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Harness work that runs before each model request, ahead of the reminders: identity
/// observation (spec 3.1 §9.2 I6) and the loop guard (§7.13).
#[async_trait]
pub(crate) trait RequestObserver: Send + Sync {
    /// Observes `ctx` before its next model request; an error ends the turn.
    async fn before_request(
        &self,
        ctx: &Arc<dyn ReadonlyContext>,
        request: &mut LlmRequest,
    ) -> Result<()>;
}

/// The harness tools as one [`Toolset`] (spec 3.1 §7.1).
///
/// The tool list is fixed for the agent's lifetime — `exit_plan_mode` is declared in
/// every mode — so the tool surface and the provider's prompt cache stay stable; the
/// policy decides what each mode allows. [`process_llm_request`](Toolset::process_llm_request)
/// appends a one-line mode banner to every request and a reminder of the open todos when
/// the list changed, at the end of the request, keeping the cached prefix stable
/// (spec 3.1 §7.6, §7.7). The harness's toolset also sets the request's
/// [`GenerateContentConfig::effort`](adk_core::GenerateContentConfig::effort) to the
/// [`EffortPolicy`](crate::EffortPolicy) level of the mode it reads, and a member's to
/// `members` (spec 3.1 §7.23).
///
/// # Example
///
/// ```rust,ignore
/// let names = harness.runtime().toolset().tool_names();
/// assert!(names.contains(&"exit_plan_mode".to_string()));
/// ```
pub struct HarnessToolset {
    tools: Vec<Arc<dyn Tool>>,
    mode_store: Arc<dyn ModeStore>,
    default_mode: HarnessMode,
    reminded: Mutex<HashMap<String, u64>>,
    observers: Vec<Arc<dyn RequestObserver>>,
    /// The standing-rules digest of the reminder (spec 3.1 §7.25).
    rules: Option<RulesDigest>,
    /// The effort each request carries (spec 3.1 §7.23); `None` leaves requests as they are.
    effort: Option<crate::posture::EffortPolicy>,
    /// A team member's toolset, whose requests run at `members` rather than the mode's level.
    member: bool,
    /// The line that tells the model what its mode allows.
    banner: fn(HarnessMode) -> String,
    #[cfg(feature = "memory")]
    facts: Option<Arc<crate::facts::runtime::FactsRuntime>>,
    /// The session containers of the container shell, stopped when the toolset closes
    /// (spec 3.1 §7.9 "Container").
    #[cfg(feature = "docker")]
    containers: Option<Arc<crate::shell::ContainerPool>>,
    /// Background shells: named in the reminder when they end, killed when the toolset
    /// closes (spec 3.1 §7.29).
    shells: Option<Arc<crate::shell::ShellSupervisor>>,
}

impl HarnessToolset {
    pub(crate) fn new(
        tools: Vec<Arc<dyn Tool>>,
        mode_store: Arc<dyn ModeStore>,
        default_mode: HarnessMode,
    ) -> Self {
        Self {
            tools,
            mode_store,
            default_mode,
            reminded: Mutex::new(HashMap::new()),
            observers: Vec::new(),
            rules: None,
            effort: None,
            member: false,
            banner: mode_banner,
            #[cfg(feature = "memory")]
            facts: None,
            #[cfg(feature = "docker")]
            containers: None,
            shells: None,
        }
    }

    /// Names the session's ended background shells in each reminder and kills every shell
    /// when the toolset closes (spec 3.1 §7.29).
    pub(crate) fn with_shells(mut self, shells: Arc<crate::shell::ShellSupervisor>) -> Self {
        self.shells = Some(shells);
        self
    }

    /// The background shells, when the harness has a shell.
    pub fn shells(&self) -> Option<&Arc<crate::shell::ShellSupervisor>> {
        self.shells.as_ref()
    }

    /// Stops the container shell's session containers when the toolset closes.
    #[cfg(feature = "docker")]
    pub(crate) fn with_containers(mut self, containers: Arc<crate::shell::ContainerPool>) -> Self {
        self.containers = Some(containers);
        self
    }

    /// Recalls long-term facts into each user turn (spec 3.1 §7.15.2).
    #[cfg(feature = "memory")]
    pub(crate) fn with_facts(mut self, facts: Arc<crate::facts::runtime::FactsRuntime>) -> Self {
        self.facts = Some(facts);
        self
    }

    /// The harness's long-term memory, when on.
    #[cfg(feature = "memory")]
    pub(crate) fn facts(&self) -> Option<&Arc<crate::facts::runtime::FactsRuntime>> {
        self.facts.as_ref()
    }

    /// The origin of the tool named `name`, as a call to it records it.
    pub(crate) fn origin_of(&self, name: &str) -> Option<ToolOrigin> {
        self.tools.iter().find(|tool| tool.name() == name).map(|tool| ToolOrigin::of(tool.as_ref()))
    }

    /// Lists the standing rules that apply to the session in every request's reminder
    /// (spec 3.1 §7.25): rules of `store` that a call of this toolset's tools can match.
    pub(crate) fn with_rules_digest(mut self, store: Arc<dyn StandingRuleStore>) -> Self {
        let origins = self
            .tools
            .iter()
            .map(|tool| (tool.name().to_string(), ToolOrigin::of(tool.as_ref())))
            .collect();
        self.rules = Some(RulesDigest { store, origins });
        self
    }

    /// Sets each request's effort from `policy` (spec 3.1 §7.23): the level of the mode the
    /// request reads, or `members` on a team member's toolset.
    pub(crate) fn with_effort(mut self, policy: crate::posture::EffortPolicy) -> Self {
        self.effort = Some(policy);
        self
    }

    /// Adds work to run before each model request, in order; a team member gets its
    /// coordinator's (spec 3.1 §7.10).
    pub(crate) fn with_observers(mut self, observers: &[Arc<dyn RequestObserver>]) -> Self {
        self.observers.extend(observers.iter().cloned());
        self
    }

    /// The toolset of a team member (spec 3.1 §7.10): `tools` without `exit_plan_mode`,
    /// `memory_write`, `memory_delete`, `memory_search`, `memory_remember` or any agent
    /// delegation, so a member can neither change the mode nor delegate further, and
    /// neither recalls nor writes long-term facts (§9.2 I42). The member reads the parent's mode, because it
    /// shares the parent's (app, user, session).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::{HarnessMode, HarnessToolset, InMemoryModeStore};
    /// use std::sync::Arc;
    ///
    /// let plan_exit = Arc::new(adk_harness::ExitPlanModeTool::new(
    ///     Arc::new(InMemoryModeStore::new()),
    ///     HarnessMode::Ask,
    /// ));
    /// let toolset = HarnessToolset::member(
    ///     vec![plan_exit],
    ///     Arc::new(InMemoryModeStore::new()),
    ///     HarnessMode::Ask,
    /// );
    /// assert!(toolset.tool_names().is_empty());
    /// ```
    pub fn member(
        tools: Vec<Arc<dyn Tool>>,
        mode_store: Arc<dyn ModeStore>,
        default_mode: HarnessMode,
    ) -> Self {
        let tools = tools
            .into_iter()
            .filter(|tool| {
                !matches!(
                    tool.name(),
                    "exit_plan_mode"
                        | "memory_write"
                        | "memory_delete"
                        | "memory_search"
                        | "memory_remember"
                ) && !tool.is_agent_delegation()
            })
            .collect();
        Self { member: true, ..Self::new(tools, mode_store, default_mode) }
    }

    /// Tells the model what its mode allows with `banner` instead of the harness's own
    /// (the website preset's, spec 3.5 §7.11).
    #[must_use]
    pub(crate) fn with_banner(mut self, banner: fn(HarnessMode) -> String) -> Self {
        self.banner = banner;
        self
    }

    /// Names of the tools, in declaration order.
    pub fn tool_names(&self) -> Vec<String> {
        self.tools.iter().map(|tool| tool.name().to_string()).collect()
    }

    /// The tools, in declaration order, for a surface that registers them with a runtime
    /// of its own (the voice surface, spec 3.1 §7.17).
    pub fn all_tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }
}

/// The one-line banner describing what `mode` allows.
pub(crate) fn mode_banner(mode: HarnessMode) -> String {
    let rule = match mode {
        HarnessMode::ReadOnly => "read-only: read and keep todos",
        HarnessMode::Plan => {
            "plan: investigate, then present a plan with exit_plan_mode and end your turn"
        }
        HarnessMode::Ask => {
            "ask: call the tools you need directly; when a call needs approval the harness asks \
             the user, so do not ask for permission in your reply or wait for a go-ahead, and \
             do not call exit_plan_mode"
        }
        HarnessMode::Auto => "auto: call the tools you need directly",
    };
    format!(
        "Harness mode is {rule}. The <harness> block's class lines say which calls run, ask or \
         are refused in this mode."
    )
}

/// The website preset's banner (spec 3.5 §7.11): the mode is the agent's jobs' autonomy
/// level, set by the charter and raised only by the owner, so the agent never asks to leave
/// it; at Propose it drafts, and drafts run in plan mode.
pub(crate) fn website_banner(mode: HarnessMode) -> String {
    let rule = match mode {
        HarnessMode::ReadOnly => "read-only: read and report; nothing changes",
        HarnessMode::Plan => {
            "propose: draft every change with the drafting tools (content_draft, content_edit, \
             translation_draft, media_intake, charter_draft) and raise decision cards; drafts \
             and cards run in this mode, and nothing goes live until the owner approves it"
        }
        HarnessMode::Ask => {
            "act with approval: call the tools you need directly; a call that needs the owner is \
             held as a decision card, so do not ask for permission in your reply"
        }
        HarnessMode::Auto => {
            "act: call the tools you need directly; a call beyond your authority is held for \
             the owner"
        }
    };
    format!(
        "Your mode is {rule}. Your jobs' autonomy levels set the mode and only the owner raises \
         them, so never ask to leave it. The <harness> block's class lines say which calls run, \
         ask or are refused in this mode."
    )
}

fn todo_reminder(list: &TodoList) -> String {
    let open: Vec<String> = list
        .open_items()
        .map(|item| {
            format!(
                "- [{}] {}: {}",
                serde_json::to_value(item.status)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default(),
                item.id,
                item.content
            )
        })
        .collect();
    if open.is_empty() {
        "All todos are complete.".to_string()
    } else {
        format!("Open todos (revision {}):\n{}", list.revision, open.join("\n"))
    }
}

#[async_trait]
impl Toolset for HarnessToolset {
    fn name(&self) -> &str {
        "harness"
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        Ok(self.tools.clone())
    }

    /// Kills every background shell (spec 3.1 §7.29) and stops every session container of
    /// the container shell (`Harness::close` reaches it through `Runner::close`, spec 3.1
    /// §7.9).
    async fn close(&self) {
        if let Some(shells) = &self.shells {
            shells.close().await;
        }
        #[cfg(feature = "docker")]
        if let Some(containers) = &self.containers {
            containers.close().await;
        }
    }

    async fn process_llm_request(
        &self,
        ctx: Arc<dyn ReadonlyContext>,
        request: &mut LlmRequest,
    ) -> Result<()> {
        for observer in &self.observers {
            observer.before_request(&ctx, request).await?;
        }
        #[cfg(feature = "memory")]
        if let Some(facts) = &self.facts {
            facts.before_request(&ctx, request).await?;
        }
        let mode = match self.mode_store.get(ctx.app_name(), ctx.user_id(), ctx.session_id()).await
        {
            Ok(Some(record)) => record.mode_for_invocation(ctx.invocation_id()),
            Ok(None) => self.default_mode,
            Err(_) => HarnessMode::ReadOnly,
        };
        // The effort travels in the request's config, never in its contents, so the prefix
        // stays byte-stable when the mode changes (spec 3.1 §7.23, I56).
        if let Some(policy) = &self.effort {
            let level = if self.member { policy.members } else { policy.for_mode(mode) };
            request.config.get_or_insert_with(Default::default).effort = Some(level);
        }
        let mut lines = vec![(self.banner)(mode)];
        // Rules change mid-session: the digest is rebuilt for every request and never
        // saved, so it stays out of the cached prefix (spec 3.1 §7.25, I56).
        if let Some(digest) = &self.rules
            && let Some(text) = digest.render(ctx.app_name(), ctx.user_id(), ctx.session_id()).await
        {
            lines.push(text);
        }
        if let Some(shells) = &self.shells
            && let Some(text) =
                shells.notices(ctx.app_name(), ctx.user_id(), ctx.session_id()).await
        {
            lines.push(text);
        }
        if let Some(list) = ctx.state().map(TodoList::from_state)
            && list.revision > 0
        {
            let key = format!("{}\u{1f}{}", ctx.session_id(), ctx.agent_name());
            let mut reminded = self.reminded.lock().unwrap_or_else(|e| e.into_inner());
            if reminded.len() > 4096 {
                reminded.clear();
            }
            if reminded.get(&key) != Some(&list.revision) {
                reminded.insert(key, list.revision);
                lines.push(todo_reminder(&list));
            }
        }
        request.contents.push(
            Content::new("user")
                .with_text(format!("<system-reminder>\n{}\n</system-reminder>", lines.join("\n"))),
        );
        Ok(())
    }
}
