//! The harness REPL's slash commands (spec 3.1 §8.1), and the same commands for the
//! full-screen terminal and a kernel's control routes (spec 3.3 §5.2).

use std::fmt::Write as _;
use std::sync::Arc;

use adk_core::{Effort, Result, SessionId, UserId};
use adk_harness::hooks::HookSource;
use adk_harness::{
    FactStatus, FileMemory, HarnessMode, MemoryConfig, MemoryFact, MemoryLimits, ModeChange,
    OriginPin, PLAN_STATE_KEY, RuleScope, SkillsConfig, StandingDecision, StandingRule,
    TODOS_STATE_KEY, escape_control,
};
use adk_session::{CreateRequest, GetRequest};
use async_trait::async_trait;
use serde_json::Value;

use super::launcher::{HarnessCommands, Shared, WORKSPACE_STATE_KEY};
use crate::console::{ConsoleCommand, ConsoleCommandContext};

/// The commands the harness REPL offers, in `/help` order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Mode,
    Sandbox,
    Todos,
    Rules,
    Approvals,
    Plan,
    Cost,
    Team,
    Session,
    Skills,
    Memory,
    Web,
    Mcp,
    Effort,
    Hooks,
    Agent,
    Rewind,
    Redo,
    Shells,
}

const COMMANDS: &[(Kind, &str, &str)] = &[
    (Kind::Mode, "mode", "Show or set the mode: /mode read-only|plan|ask|auto"),
    (Kind::Sandbox, "sandbox", "Shell isolation: /sandbox on|off|status"),
    (Kind::Todos, "todos", "Show the session's todo list"),
    (Kind::Rules, "rules", "List your standing rules; /rules rm N revokes one"),
    (Kind::Approvals, "approvals", "Calls in this session waiting for a decision"),
    (Kind::Plan, "plan", "Show the last approved plan"),
    (Kind::Cost, "cost", "Your spend: model calls, tokens and cost per agent"),
    (Kind::Team, "team", "Team members the harness can delegate to"),
    (Kind::Session, "session", "This session: id, user, workspace, events"),
    (Kind::Skills, "skills", "Skills the agent can load"),
    (Kind::Memory, "memory", "Working notes and long-term facts; /memory forget|accept ID"),
    (Kind::Web, "web", "The web allowlist; /web allow HOST adds a standing rule for web_fetch"),
    (Kind::Mcp, "mcp", "MCP servers; /mcp resources SERVER lists what a server publishes"),
    (Kind::Effort, "effort", "Reasoning effort; /effort low|medium|high sets every mode"),
    (Kind::Hooks, "hooks", "Hooks on tool events: source, isolation and whether each runs"),
    (Kind::Agent, "agent", super::agents::DESCRIPTION),
    (
        Kind::Rewind,
        "rewind",
        "Turns to go back to; /rewind N restores files, transcript and plan from before turn N",
    ),
    (Kind::Redo, "redo", "Undo the last /rewind"),
    (Kind::Shells, "shells", super::shells::DESCRIPTION),
];

/// The REPL's built-in commands, which take precedence over a registered one of the same
/// name.
const BUILT_INS: &[(&str, &str)] = &[
    ("help", "List the commands"),
    ("clear", "Clear display (session state is kept)"),
    ("exit", "Leave the REPL; /quit does the same"),
];

/// The harness REPL's slash commands, built-in and registered, as the harness manifest
/// lists them (spec 3.1 §7.25).
///
/// # Example
///
/// ```rust
/// let entries = adk_cli::harness::slash_command_entries();
/// assert!(entries.iter().any(|entry| entry.name == "approvals"));
/// assert!(entries.iter().any(|entry| entry.name == "help"));
/// ```
pub fn slash_command_entries() -> Vec<adk_harness::CommandEntry> {
    BUILT_INS
        .iter()
        .map(|(name, summary)| adk_harness::CommandEntry::slash(*name, *summary))
        .chain(
            COMMANDS
                .iter()
                .map(|(_, name, summary)| adk_harness::CommandEntry::slash(*name, *summary)),
        )
        .collect()
}

/// One data-driven slash command over the REPL's shared state.
struct HarnessCommand {
    kind: Kind,
    name: &'static str,
    description: &'static str,
    shared: Arc<Shared>,
}

/// Every harness slash command, bound to the REPL's session.
pub(crate) fn harness_commands(shared: Arc<Shared>) -> Vec<Arc<dyn ConsoleCommand>> {
    COMMANDS
        .iter()
        .map(|(kind, name, description)| {
            Arc::new(HarnessCommand { kind: *kind, name, description, shared: shared.clone() })
                as Arc<dyn ConsoleCommand>
        })
        .collect()
}

impl Kind {
    /// Whether the command with `args` changes something — the mode, a rule, the memory, the
    /// shell, the effort — rather than reads (spec 3.3 §5.2).
    fn changes(self, args: &str) -> bool {
        let first = args.split_whitespace().next();
        match self {
            Self::Mode | Self::Effort => first.is_some(),
            Self::Sandbox => matches!(first, Some("on" | "off")),
            Self::Rules => first == Some("rm"),
            Self::Rewind => first.is_some(),
            Self::Redo => true,
            Self::Memory => matches!(first, Some("forget" | "accept")),
            Self::Web => first == Some("allow"),
            Self::Shells => first == Some("kill"),
            Self::Todos
            | Self::Approvals
            | Self::Plan
            | Self::Cost
            | Self::Team
            | Self::Session
            | Self::Skills
            | Self::Mcp
            | Self::Hooks => false,
            // Pausing and removing make agents do less; adding and changing start nothing
            // until Resume all: each works while everything is stopped (spec 3.3 §5.7).
            Self::Agent => false,
        }
    }
}

impl HarnessCommands {
    /// The REPL's commands, in `/help` order: each name and what it does (spec 3.3 §5.2).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_cli::harness::HarnessCommands;
    ///
    /// assert!(HarnessCommands::list().iter().any(|(name, _)| *name == "todos"));
    /// ```
    pub fn list() -> Vec<(&'static str, &'static str)> {
        COMMANDS.iter().map(|(_, name, description)| (*name, *description)).collect()
    }

    /// Whether `/name args` changes something — the mode, a rule, the memory, the shell, the
    /// effort — rather than reads: while everything is stopped, a kernel refuses it unless it
    /// [`tightens`](Self::tightens).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_cli::harness::HarnessCommands;
    ///
    /// assert!(HarnessCommands::changes("rules", "rm 2"));
    /// assert!(!HarnessCommands::changes("rules", ""));
    /// ```
    pub fn changes(name: &str, args: &str) -> bool {
        COMMANDS
            .iter()
            .find(|(_, known, _)| *known == name)
            .is_some_and(|(kind, _, _)| kind.changes(args))
    }

    /// Whether `/name args`, run for `user` in `session`, only makes the agents do less:
    /// `/rules rm`, `/memory forget`, `/sandbox on`, or a `/mode` stricter than the session's.
    /// A kernel runs it while everything is stopped, so a person can clean up before Resume
    /// all (spec 3.3 §5.2, S-I10).
    ///
    /// # Errors
    ///
    /// Returns an invalid-input error for an empty user or session, or the mode store's error.
    pub async fn tightens(
        &self,
        name: &str,
        args: &str,
        user: &str,
        session: &str,
    ) -> Result<bool> {
        let Some((kind, _, _)) = COMMANDS.iter().find(|(_, known, _)| *known == name) else {
            return Ok(false);
        };
        let first = args.split_whitespace().next();
        Ok(match kind {
            Kind::Rules => first == Some("rm"),
            Kind::Memory => first == Some("forget"),
            Kind::Sandbox => first == Some("on"),
            // Stopping a shell only removes an effect (spec 3.1 §7.29).
            Kind::Shells => first == Some("kill"),
            Kind::Mode => match requested_mode(args) {
                Some(requested) => {
                    let current =
                        self.harness.mode(&UserId::new(user)?, &SessionId::new(session)?).await?;
                    ModeChange::between(current, requested) == ModeChange::Tighten
                }
                None => false,
            },
            Kind::Effort | Kind::Web | Kind::Todos | Kind::Approvals | Kind::Plan => false,
            Kind::Cost | Kind::Team | Kind::Session | Kind::Skills | Kind::Mcp => false,
            Kind::Hooks | Kind::Agent | Kind::Rewind | Kind::Redo => false,
        })
    }

    /// The mode of `user`'s `session`, as the harness names it: `read_only`, `plan`, `ask` or
    /// `auto`, the default for a session with no mode of its own (spec 3.3 §5.2).
    ///
    /// # Errors
    ///
    /// Returns an invalid-input error for an empty user or session, or the mode store's error.
    pub async fn session_mode(&self, user: &str, session: &str) -> Result<String> {
        let mode = self.harness.mode(&UserId::new(user)?, &SessionId::new(session)?).await?;
        Ok(mode.as_str().to_string())
    }

    /// Runs `/name args` for `user` in `session` by the REPL's code; returns what it shows,
    /// or `None` when no command has that name. `reach` is how the caller reached a kernel
    /// serving the command, which `/session` names; `None` in process.
    ///
    /// # Errors
    ///
    /// Returns an invalid-input error for an empty user or session, or the command's error.
    pub async fn run(
        &self,
        name: &str,
        args: &str,
        user: &str,
        session: &str,
        reach: Option<adk_kernel::control::CommandReach>,
    ) -> Result<Option<String>> {
        let Some((kind, _, _)) = COMMANDS.iter().find(|(_, known, _)| *known == name) else {
            return Ok(None);
        };
        let shared = self.shared(UserId::new(user)?, SessionId::new(session)?, reach);
        execute(*kind, &shared, args.trim()).await.map(Some)
    }

    /// Every command, in `/help` order, for the full-screen terminal of `user`: each runs in
    /// the session the terminal shows (spec 3.3 §5.2).
    #[cfg(feature = "tui")]
    pub fn terminal(&self, user: UserId) -> Vec<Arc<dyn adk_surface::tui::TerminalCommand>> {
        let commands = Arc::new(self.clone());
        COMMANDS
            .iter()
            .map(|(kind, name, description)| {
                Arc::new(TerminalHarnessCommand {
                    kind: *kind,
                    name,
                    description,
                    commands: commands.clone(),
                    user: user.clone(),
                }) as Arc<dyn adk_surface::tui::TerminalCommand>
            })
            .collect()
    }
}

#[async_trait]
impl ConsoleCommand for HarnessCommand {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        self.description
    }

    async fn run(&self, args: &str, _ctx: &ConsoleCommandContext) -> Result<String> {
        execute(self.kind, &self.shared, args).await
    }
}

/// A REPL command in the full-screen terminal: run in the session the terminal shows.
#[cfg(feature = "tui")]
struct TerminalHarnessCommand {
    kind: Kind,
    name: &'static str,
    description: &'static str,
    commands: Arc<HarnessCommands>,
    user: UserId,
}

#[cfg(feature = "tui")]
#[async_trait]
impl adk_surface::tui::TerminalCommand for TerminalHarnessCommand {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        self.description
    }

    async fn run(&self, args: &str, context: &adk_surface::tui::CommandContext) -> Result<String> {
        let session = SessionId::new(context.session_id.clone())?;
        let shared = self.commands.shared(self.user.clone(), session, None);
        execute(self.kind, &shared, args).await
    }
}

/// Runs the command `kind` with `args` over `shared`: the one code path of the REPL, the
/// full-screen terminal and a kernel's control routes.
async fn execute(kind: Kind, shared: &Shared, args: &str) -> Result<String> {
    match kind {
        Kind::Mode => mode(shared, args).await,
        Kind::Sandbox => sandbox(shared, args),
        Kind::Todos => Ok(render_todos(shared.state(TODOS_STATE_KEY).await?.as_ref())),
        Kind::Rules => rules(shared, args).await,
        Kind::Approvals => approvals(shared).await,
        Kind::Plan => Ok(match shared.state(PLAN_STATE_KEY).await? {
            Some(Value::String(plan)) => plan,
            Some(Value::Object(plan)) => plan
                .get("plan")
                .and_then(Value::as_str)
                .map_or_else(|| Value::Object(plan.clone()).to_string(), str::to_string),
            Some(other) => other.to_string(),
            None => "No plan approved in this session. /mode plan, then ask for one.".into(),
        }),
        // Spend per acting agent and vendor for this user (spec 3.1 §7.13).
        Kind::Cost => Ok(shared.harness().spend(Some(&shared.user)).render()),
        Kind::Team => {
            let harness = shared.harness();
            let members = &harness.agent().info().members;
            Ok(if members.is_empty() {
                "No team members. Add one with --member NAME=explore|general.".into()
            } else {
                members.iter().map(|name| format!("  {name}")).collect::<Vec<_>>().join("\n")
            })
        }
        Kind::Session => session(shared).await,
        Kind::Skills => skills(shared),
        Kind::Memory => memory(shared, args).await,
        Kind::Web => web(shared, args).await,
        Kind::Mcp => mcp(shared, args).await,
        Kind::Effort => effort(shared, args).await,
        Kind::Hooks => Ok(hooks(shared)),
        Kind::Shells => super::shells::run(shared, args).await,
        Kind::Agent => {
            super::agents::run(
                shared.agents.as_ref(),
                shared.user.as_ref(),
                shared.reach.as_ref(),
                args,
            )
            .await
        }
        Kind::Rewind => super::rewind::rewind(shared, args).await,
        Kind::Redo => super::rewind::redo(shared).await,
    }
}

/// `/hooks`: the effective hook set — each hook with its source, isolation, timeout and
/// whether it runs, since a workspace hook runs only under the workspace's trust — and the
/// bounds of one turn (spec 3.1 §7.22, §8.1).
fn hooks(shared: &Shared) -> String {
    if shared.hooks_disabled {
        return "Hooks are off (--no-hooks).".into();
    }
    let harness = shared.harness();
    let agent = harness.agent();
    let Some(set) = agent.runtime().hooks() else {
        return format!(
            "No hooks. Add [[hooks]] tables to {} (they run once the workspace is trusted) \
             or to your user config.",
            adk_cli_project_file()
        );
    };
    let trusted = agent.runtime().workspace_trusted();
    let mut out = String::new();
    for hook in &set.hooks {
        let runs = match hook.source {
            HookSource::Workspace if !trusted => "does not run: the workspace is not trusted",
            HookSource::Workspace | HookSource::User | HookSource::Operator => "runs",
        };
        let _ = writeln!(
            out,
            "  {} — {} · {} · {} s{} · {runs}",
            hook.describe(),
            hook.source.as_str(),
            hook.isolation.as_str(),
            hook.timeout.as_secs(),
            if hook.fail_closed { " · fail-closed" } else { "" }
        );
    }
    let _ = write!(
        out,
        "  bounds: {} per event, {} s per turn",
        set.max_per_event,
        set.max_time_per_turn.as_secs()
    );
    out
}

/// The project file, as `/hooks` names it.
fn adk_cli_project_file() -> &'static str {
    super::args::PROJECT_CONFIG_FILE
}

/// `/effort`: the session's reasoning effort; `/effort low|medium|high` sets every mode and
/// role to that level from the next model call (spec 3.1 §7.23).
async fn effort(shared: &Shared, args: &str) -> Result<String> {
    let policy = *shared.harness().agent().effort();
    let current = shared.current_effort().map_or_else(|| "per mode".to_string(), |e| e.to_string());
    if args.trim().is_empty() {
        return Ok(format!(
            "effort: {current}\n  policy: {}\n  /effort low|medium|high sets every mode and role \
             to that level from the next model call",
            policy.describe()
        ));
    }
    let level: Effort = match args.trim().parse() {
        Ok(level) => level,
        Err(error) => return Ok(error),
    };
    match shared.rebuild_effort(level).await? {
        Some(_) => Ok(format!("effort: {level} for every mode and role from the next model call")),
        None => Ok("this console cannot change the effort; restart with --effort".into()),
    }
}

async fn mode(shared: &Shared, args: &str) -> Result<String> {
    let harness = shared.harness();
    let current = harness.mode(&shared.user, &shared.session).await?;
    if args.is_empty() {
        return Ok(format!(
            "mode: {current}\n  /mode read-only|plan|ask|auto changes it; a looser mode cancels \
             calls held in this session"
        ));
    }
    let Some(requested) = requested_mode(args) else {
        let other = args.trim();
        return Ok(format!("unknown mode '{other}'; use read-only, plan, ask or auto"));
    };
    if requested == current {
        return Ok(format!("mode: {current} (unchanged)"));
    }
    // A thread with no message yet takes a mode too: its session is opened for the caller,
    // as its first message would (spec 3.3 §5.2).
    let opened = shared
        .sessions
        .get(GetRequest {
            app_name: shared.app_name.clone(),
            user_id: shared.user.to_string(),
            session_id: shared.session.to_string(),
            num_recent_events: Some(1),
            after: None,
        })
        .await;
    match opened {
        Ok(_) => {}
        Err(error) if error.is_not_found() => {
            let created = shared
                .sessions
                .create(CreateRequest {
                    app_name: shared.app_name.clone(),
                    user_id: shared.user.to_string(),
                    session_id: Some(shared.session.to_string()),
                    state: Default::default(),
                })
                .await;
            match created {
                Ok(_) => {}
                Err(error) if error.category == adk_core::ErrorCategory::Conflict => {}
                Err(error) => return Err(error),
            }
        }
        Err(error) => return Err(error),
    }
    harness.set_mode(&shared.user, &shared.session, requested).await?;
    Ok(match ModeChange::between(current, requested) {
        ModeChange::Loosen => {
            format!("mode: {requested} (was {current}); calls held in this session were cancelled")
        }
        _ => format!("mode: {requested} (was {current})"),
    })
}

/// The mode `/mode` names: the harness's words.
fn requested_mode(args: &str) -> Option<HarnessMode> {
    match args.trim() {
        "read-only" | "readonly" => Some(HarnessMode::ReadOnly),
        "plan" => Some(HarnessMode::Plan),
        "ask" => Some(HarnessMode::Ask),
        "auto" => Some(HarnessMode::Auto),
        _ => None,
    }
}

fn sandbox(shared: &Shared, args: &str) -> Result<String> {
    let status = |harness: &adk_harness::Harness| {
        let info = harness.agent().info();
        // The personal preset offers no shell outside the sandbox (spec 3.3 §10 H1).
        let shell = super::launcher::shell_label(info);
        match &info.degraded_reason {
            Some(reason) => format!("shell: {shell} — {reason}"),
            None => format!("shell: {shell}"),
        }
    };
    match args.trim() {
        "" | "status" => Ok(status(&shared.harness())),
        "on" | "off" => {
            let on = args.trim() == "on";
            match shared.rebuild(on)? {
                Some(harness) => Ok(format!(
                    "{}\n  rules and approvals granted for the other shell do not apply to this one",
                    status(&harness)
                )),
                None => Ok(format!(
                    "this console cannot switch the shell; restart {} --sandbox",
                    if on { "with" } else { "without" }
                )),
            }
        }
        other => Ok(format!("unknown argument '{other}'; use /sandbox on|off|status")),
    }
}

/// Renders `harness:todos` in the harness format (`id`, `activeForm`, `cancelled`) or the
/// `adk_agent::coding` format (`content`, `status`).
pub(crate) fn render_todos(value: Option<&Value>) -> String {
    let items = value.and_then(|value| value.get("items")).and_then(Value::as_array);
    let Some(items) = items.filter(|items| !items.is_empty()) else {
        return "No todos in this session.".into();
    };
    let mut out = String::new();
    for item in items {
        let status = item.get("status").and_then(Value::as_str).unwrap_or("pending");
        let mark = match status {
            "completed" => "✓",
            "in_progress" => "▶",
            "cancelled" => "✗",
            _ => "·",
        };
        let content = item.get("content").and_then(Value::as_str).unwrap_or_default();
        let text = match (status, item.get("activeForm").and_then(Value::as_str)) {
            ("in_progress", Some(active)) => active,
            _ => content,
        };
        let id =
            item.get("id").and_then(Value::as_str).map(|id| format!("{id}. ")).unwrap_or_default();
        let _ = writeln!(out, "  {mark} {id}{text}");
    }
    out
}

async fn rules(shared: &Shared, args: &str) -> Result<String> {
    let harness = shared.harness();
    let store = harness.standing_rules();
    let mut rules = store.list(&shared.app_name, shared.user.as_ref()).await?;
    rules.sort_by_key(|rule| rule.created_at);
    let mut words = args.split_whitespace();
    match (words.next(), words.next()) {
        (None, _) => Ok(if rules.is_empty() {
            "No standing rules. Answer a prompt with [a] to remember one.".into()
        } else {
            rules
                .iter()
                .enumerate()
                .map(|(i, rule)| describe_rule(i + 1, rule))
                .collect::<Vec<_>>()
                .join("\n")
        }),
        (Some("rm"), Some(which)) => {
            let rule = match which.parse::<usize>() {
                Ok(n) if (1..=rules.len()).contains(&n) => &rules[n - 1],
                _ => match rules.iter().find(|rule| rule.id == which) {
                    Some(rule) => rule,
                    None => return Ok(format!("no rule {which}; /rules lists them")),
                },
            };
            store.revoke(&shared.app_name, &rule.id).await?;
            Ok(format!("revoked: {}", rule.label))
        }
        _ => Ok("use /rules, or /rules rm N".into()),
    }
}

/// One line describing `rule`, numbered `n`.
pub(crate) fn describe_rule(n: usize, rule: &StandingRule) -> String {
    let decision = match &rule.decision {
        StandingDecision::Allow => "allow".to_string(),
        StandingDecision::Deny { reason } => format!("deny ({reason})"),
    };
    let scope = match &rule.scope {
        RuleScope::User { .. } => "every session".to_string(),
        RuleScope::Session { session_id, .. } => format!("session {session_id}"),
    };
    let pin = match &rule.origin {
        OriginPin::Exact { source, .. } => format!("{source}, this declaration"),
        OriginPin::AnyDeclaration { source } => format!("{source}, any declaration"),
        OriginPin::ExactEach { source, .. } => format!("{source}, each covered tool's declaration"),
    };
    let expires = rule
        .expires_at
        .map(|at| format!(", until {}", at.format("%Y-%m-%d %H:%M")))
        .unwrap_or_default();
    format!("  {n}. {decision} {} — {scope}; {pin}{expires} [{}]", rule.label, rule.id)
}

async fn approvals(shared: &Shared) -> Result<String> {
    let pending = shared.harness().pending_approvals(&shared.user, &shared.session).await?;
    if pending.is_empty() {
        return Ok("No calls are waiting for a decision in this session.".into());
    }
    let mut out = String::new();
    for entry in pending {
        let _ = writeln!(
            out,
            "  {} {} (held {})",
            entry.request.tool_name,
            entry.request.args,
            entry.requested_at.format("%Y-%m-%d %H:%M")
        );
    }
    out.push_str("  decide with `adk-rust approvals approve|deny ID` in another terminal");
    Ok(out)
}

/// `/memory`: working notes and long-term facts; `/memory forget|accept ID` (spec 3.1
/// §7.15, §10 L2).
async fn memory(shared: &Shared, args: &str) -> Result<String> {
    let harness = shared.harness();
    let mut words = args.split_whitespace();
    let (action, id) = (words.next(), words.next());
    if action.is_none() {
        return list_memory(shared, &harness).await;
    }
    if !super::launcher::memory_on(&harness) {
        return Ok("Long-term memory is off (--no-memory or --ephemeral).".into());
    }
    let (Some(action @ ("forget" | "accept")), Some(prefix)) = (action, id) else {
        return Ok("use /memory, /memory forget ID or /memory accept ID".into());
    };
    let facts = harness.facts(&shared.user).await?;
    let matches: Vec<&MemoryFact> = facts
        .iter()
        .filter(|fact| fact.id.to_ascii_uppercase().starts_with(&prefix.to_ascii_uppercase()))
        .collect();
    let fact = match matches.as_slice() {
        [fact] => *fact,
        [] => return Ok(format!("no fact {prefix}; /memory lists them")),
        _ => return Ok(format!("{prefix} matches {} facts; give more of the id", matches.len())),
    };
    Ok(if action == "forget" {
        harness.forget_fact(&shared.user, &fact.id).await?;
        format!("forgotten: {}", escape_control(&fact.text))
    } else {
        let accepted = harness.accept_fact(&shared.user, &fact.id).await?;
        format!("known: {}", escape_control(&accepted.text))
    })
}

async fn list_memory(shared: &Shared, harness: &adk_harness::Harness) -> Result<String> {
    let mut out = String::from("Working notes:\n");
    let root =
        MemoryConfig::default().root_for(&shared.app_name, shared.user.as_ref(), &shared.workspace);
    let topics = if root.is_dir() {
        FileMemory::open(&root, MemoryLimits::default())?.topics().await?
    } else {
        Vec::new()
    };
    if topics.is_empty() {
        out.push_str("  (none)\n");
    }
    for topic in topics {
        let _ =
            writeln!(out, "  {} — {}", escape_control(&topic.path), escape_control(&topic.summary));
    }
    out.push_str("Long-term facts (this project and your preferences):\n");
    if !super::launcher::memory_on(harness) {
        out.push_str("  off (--no-memory or --ephemeral)\n");
        return Ok(out);
    }
    let mut facts = harness.facts(&shared.user).await?;
    let history = facts.iter().filter(|fact| !fact.status.is_live()).count();
    facts.retain(|fact| fact.status.is_live());
    facts.sort_by_key(|fact| fact.status != FactStatus::Known);
    if facts.is_empty() {
        out.push_str("  (none yet; facts are learned when a session ends)\n");
    }
    for fact in &facts {
        let _ = writeln!(out, "  {}", describe_fact(fact));
    }
    if history > 0 {
        let _ = writeln!(out, "  ({history} superseded or forgotten, kept as history)");
    }
    out.push_str("  /memory forget ID removes one; /memory accept ID confirms an assumed one");
    Ok(out)
}

/// One line describing `fact`: id, status, kind, date, source and session, text.
pub(crate) fn describe_fact(fact: &MemoryFact) -> String {
    let scope = if fact.scope.project.is_some() { "project" } else { "all projects" };
    format!(
        "{} {} · {} · {} · {} in session {} · {scope}\n      {}",
        fact.id,
        fact.status.label(),
        fact.kind,
        fact.valid_from.format("%Y-%m-%d"),
        fact.provenance.source.as_str(),
        if fact.provenance.session_id.is_empty() {
            "-"
        } else {
            fact.provenance.session_id.as_str()
        },
        escape_control(&fact.text)
    )
}

async fn session(shared: &Shared) -> Result<String> {
    let events = shared.events().await?;
    let workspace = shared
        .state(WORKSPACE_STATE_KEY)
        .await?
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| shared.workspace.display().to_string());
    let harness = shared.harness();
    let mode = harness.mode(&shared.user, &shared.session).await?;
    let mut out = format!(
        "session   {}\nuser      {}\napp       {}\nworkspace {workspace}\nmode      {mode}\nevents    {}",
        shared.session.as_ref(),
        shared.user.as_ref(),
        shared.app_name,
        events.len()
    );
    // On a kernel, how this terminal reached it (spec 3.3 §5.2).
    if let Some(reach) = &shared.reach {
        let _ = write!(out, "\nreached   {}", describe_reach(reach));
    }
    Ok(out)
}

/// How a caller reached the kernel, in the words `/session` shows.
fn describe_reach(reach: &adk_kernel::control::CommandReach) -> String {
    use adk_kernel::control::ControlTransport;
    let how = match (reach.transport, &reach.device) {
        (ControlTransport::Socket, _) => "the kernel's socket".to_string(),
        (ControlTransport::Pipe, _) => "the kernel's named pipe".to_string(),
        (_, Some(device)) => format!("HTTP, as the paired device {device}"),
        _ => "HTTP, as the operator".to_string(),
    };
    match reach.peer_pid {
        Some(pid) => format!("{how}, from process {pid}"),
        None => how,
    }
}

fn skills(shared: &Shared) -> Result<String> {
    let config = SkillsConfig::default();
    let extra: Vec<_> = config
        .extra_roots
        .iter()
        .map(|dir| if dir.is_absolute() { dir.clone() } else { shared.workspace.join(dir) })
        .collect();
    let index = match adk_skill::load_skill_index_with_extras(&shared.workspace, &extra) {
        Ok(index) => index,
        Err(error) => return Ok(format!("skills could not be indexed: {error}")),
    };
    let skills: Vec<_> = index
        .skills()
        .iter()
        .filter(|skill| !skill.tags.iter().any(|tag| tag == "convention"))
        .map(|skill| format!("  {} — {}", skill.name, skill.description))
        .collect();
    Ok(if skills.is_empty() {
        "No skills found in the workspace's skill directories or ~/.adk/skills/.".into()
    } else {
        skills.join("\n")
    })
}

/// `/web`: the allowlist; `/web allow HOST` grants a `DomainIn` rule for `web_fetch`
/// to this user, as a human action outside any prompt (spec 3.1 §7.5, §7.20).
async fn web(shared: &Shared, args: &str) -> Result<String> {
    let harness = shared.harness();
    let runtime = harness.agent().runtime();
    let declared = harness.agent().info().tools.iter().any(|tool| tool == "web_fetch");
    let allowlist = runtime.policy().registry().web_allowlist().to_vec();
    let mut words = args.split_whitespace();
    match (words.next(), words.next()) {
        (None, _) => {
            let mut out = String::new();
            if !declared {
                out.push_str("web_fetch is off (start with --web to declare it).\n");
            }
            if allowlist.is_empty() {
                out.push_str("Allowlist: none; every fetch asks in Ask mode. /web allow HOST remembers one host.");
            } else {
                out.push_str(&format!("Allowlist (no prompt): {}", allowlist.join(", ")));
            }
            out.push_str(&format!("\nSearch: {}", harness.agent().info().web_search));
            Ok(out)
        }
        (Some("allow"), Some(host)) => {
            let Some(host) = adk_harness::web::url_host(&format!("https://{host}/")) else {
                return Ok(format!(
                    "'{host}' is not a host name the harness fetches; use a public host such as docs.rs"
                ));
            };
            if adk_harness::web::is_private_host(&host) {
                return Ok(format!(
                    "'{host}' is a private or local target the harness never fetches"
                ));
            }
            let now = chrono::Utc::now();
            let created_by = adk_core::principal::SubjectRef::human(shared.user.to_string());
            let rule = StandingRule {
                id: uuid::Uuid::new_v4().to_string(),
                scope: RuleScope::User {
                    app_name: shared.app_name.clone(),
                    user_id: shared.user.to_string(),
                    subject_kind: adk_core::principal::SubjectKind::Human,
                },
                tool: "web_fetch".into(),
                // A console command is the operator's own action, so the rule survives a
                // redeclaration of web_fetch; a prompt's suggestion pins the exact declaration.
                origin: OriginPin::AnyDeclaration {
                    source: adk_core::STATIC_TOOL_ORIGIN.to_string(),
                },
                agent: None,
                predicates: vec![adk_harness::RulePredicate::Arg(adk_harness::ArgSpec::DomainIn {
                    pointer: "/url".into(),
                    domains: vec![host.clone()],
                })],
                decision: StandingDecision::Allow,
                created_by,
                created_at: now,
                expires_at: None,
                label: format!("web_fetch: any page on {host}"),
                source_fingerprint: None,
            };
            harness.standing_rules().grant(rule).await?;
            Ok(format!("web_fetch on {host} no longer asks; /rules lists the rule"))
        }
        _ => Ok("use /web, or /web allow HOST".into()),
    }
}

/// `/mcp`: the declared servers; `/mcp resources SERVER` lists what one publishes, the
/// names a `@SERVER:URI` mention takes (spec 3.1 §7.21).
async fn mcp(shared: &Shared, args: &str) -> Result<String> {
    let harness = shared.harness();
    let runtime = harness.agent().runtime();
    let Some(mcp) = runtime.mcp() else {
        return Ok("No MCP servers: declare them in .mcp.json in the workspace.".into());
    };
    let mut words = args.split_whitespace();
    match (words.next(), words.next()) {
        (None, _) => {
            let manager = mcp.manager();
            let mut out = String::new();
            for (id, trusted) in mcp.servers() {
                let status =
                    manager.server_status(&id).await.map(|s| format!("{s:?}")).unwrap_or_default();
                let _ = writeln!(
                    out,
                    "{id}: {status}{}",
                    if trusted {
                        ""
                    } else {
                        " (workspace-declared; starts under workspace trust)"
                    }
                );
            }
            out.push_str("Mention a resource as @SERVER:URI to hand it to the model; /mcp resources SERVER lists them.");
            Ok(out)
        }
        (Some("resources"), Some(server)) => {
            let listed = mcp.list_resources(server).await?;
            let mut out = String::new();
            for resource in listed["resources"].as_array().into_iter().flatten() {
                let _ = writeln!(
                    out,
                    "@{server}:{}  {}",
                    resource["uri"].as_str().unwrap_or_default(),
                    resource["name"].as_str().unwrap_or_default()
                );
            }
            for template in listed["templates"].as_array().into_iter().flatten() {
                let _ = writeln!(
                    out,
                    "@{server}:{}  (template)",
                    template["uriTemplate"].as_str().unwrap_or_default()
                );
            }
            if out.is_empty() {
                out.push_str("no resources");
            }
            // The names just listed are the ones Tab completes.
            shared.refresh_mentions().await;
            Ok(out)
        }
        _ => Ok("use /mcp, or /mcp resources SERVER".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn todos_render_in_both_formats() {
        let harness = json!({ "items": [
            { "id": "1", "content": "Write the test", "status": "completed" },
            { "id": "2", "content": "Fix the bug", "activeForm": "Fixing the bug", "status": "in_progress" },
            { "id": "3", "content": "Drop the hack", "status": "cancelled" }
        ], "revision": 4 });
        assert_eq!(
            render_todos(Some(&harness)),
            "  ✓ 1. Write the test\n  ▶ 2. Fixing the bug\n  ✗ 3. Drop the hack\n"
        );
        let coding = json!({ "items": [
            { "content": "Read the code", "status": "completed" },
            { "content": "Change it", "status": "pending" }
        ] });
        assert_eq!(render_todos(Some(&coding)), "  ✓ Read the code\n  · Change it\n");
        assert_eq!(render_todos(None), "No todos in this session.");
    }

    /// A kernel refuses a command that changes something while everything is stopped, so each
    /// one says which of its forms change and which read (spec 3.3 §5.2).
    #[test]
    fn each_command_says_whether_it_changes_something() {
        for (name, args) in [
            ("mode", "plan"),
            ("sandbox", "on"),
            ("rules", "rm 2"),
            ("memory", "forget 01AB"),
            ("memory", "accept 01AB"),
            ("web", "allow docs.rs"),
            ("effort", "high"),
        ] {
            assert!(HarnessCommands::changes(name, args), "/{name} {args}");
        }
        for (name, args) in [
            ("mode", ""),
            ("sandbox", "status"),
            ("rules", ""),
            ("memory", ""),
            ("web", ""),
            ("mcp", "resources docs"),
            ("todos", ""),
            ("session", ""),
            ("nonesuch", "rm 2"),
        ] {
            assert!(!HarnessCommands::changes(name, args), "/{name} {args}");
        }
        let listed: Vec<&str> = HarnessCommands::list().into_iter().map(|(name, _)| name).collect();
        assert_eq!(listed.len(), COMMANDS.len());
        assert_eq!(listed.first(), Some(&"mode"));
    }

    #[test]
    fn every_command_has_a_distinct_name() {
        let mut names: Vec<_> = COMMANDS.iter().map(|(_, name, _)| *name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), COMMANDS.len());
    }
}
