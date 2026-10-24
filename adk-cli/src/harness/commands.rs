//! The harness REPL's slash commands (spec 3.1 §8.1).

use std::fmt::Write as _;
use std::sync::Arc;

use adk_core::{Effort, Result};
use adk_harness::hooks::HookSource;
use adk_harness::{
    FactStatus, FileMemory, HarnessMode, MemoryConfig, MemoryFact, MemoryLimits, ModeChange,
    OriginPin, PLAN_STATE_KEY, RuleScope, SkillsConfig, StandingDecision, StandingRule,
    TODOS_STATE_KEY, escape_control,
};
use async_trait::async_trait;
use serde_json::Value;

use super::launcher::{Shared, WORKSPACE_STATE_KEY};
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
];

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

#[async_trait]
impl ConsoleCommand for HarnessCommand {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        self.description
    }

    async fn run(&self, args: &str, _ctx: &ConsoleCommandContext) -> Result<String> {
        let shared = &self.shared;
        match self.kind {
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
        }
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
    let requested: HarnessMode = match args.trim() {
        "read-only" | "readonly" => HarnessMode::ReadOnly,
        "plan" => HarnessMode::Plan,
        "ask" => HarnessMode::Ask,
        "auto" => HarnessMode::Auto,
        other => return Ok(format!("unknown mode '{other}'; use read-only, plan, ask or auto")),
    };
    if requested == current {
        return Ok(format!("mode: {current} (unchanged)"));
    }
    harness.set_mode(&shared.user, &shared.session, requested).await?;
    Ok(match ModeChange::between(current, requested) {
        ModeChange::Loosen => {
            format!("mode: {requested} (was {current}); calls held in this session were cancelled")
        }
        _ => format!("mode: {requested} (was {current})"),
    })
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
    Ok(format!(
        "session   {}\nuser      {}\napp       {}\nworkspace {workspace}\nmode      {mode}\nevents    {}",
        shared.session.as_ref(),
        shared.user.as_ref(),
        shared.app_name,
        events.len()
    ))
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

    #[test]
    fn every_command_has_a_distinct_name() {
        let mut names: Vec<_> = COMMANDS.iter().map(|(_, name, _)| *name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), COMMANDS.len());
    }
}
