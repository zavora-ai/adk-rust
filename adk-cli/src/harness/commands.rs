//! The harness REPL's slash commands (spec 3.1 §8.1).

use std::fmt::Write as _;
use std::sync::Arc;

use adk_core::Result;
use adk_harness::{
    HarnessMode, ModeChange, OriginPin, PLAN_STATE_KEY, RuleScope, SkillsConfig, StandingDecision,
    StandingRule, TODOS_STATE_KEY,
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
}

const COMMANDS: &[(Kind, &str, &str)] = &[
    (Kind::Mode, "mode", "Show or set the mode: /mode read-only|plan|ask|auto"),
    (Kind::Sandbox, "sandbox", "Shell isolation: /sandbox on|off|status"),
    (Kind::Todos, "todos", "Show the session's todo list"),
    (Kind::Rules, "rules", "List your standing rules; /rules rm N revokes one"),
    (Kind::Approvals, "approvals", "Calls in this session waiting for a decision"),
    (Kind::Plan, "plan", "Show the last approved plan"),
    (Kind::Cost, "cost", "Spend of this session"),
    (Kind::Team, "team", "Team members the harness can delegate to"),
    (Kind::Session, "session", "This session: id, user, workspace, events"),
    (Kind::Skills, "skills", "Skills the agent can load"),
    (Kind::Memory, "memory", "The harness's file memory"),
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
            // wave B: observe — Harness::spend (spec 3.1 §7.13) reports this.
            Kind::Cost => Ok("Spend reporting arrives with the harness's observe layer \
                              (spec 3.1 §7.13); this build does not meter the session."
                .into()),
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
            // wave B: observe — file memory (spec 3.1 §7.15) shows its index here.
            Kind::Memory => Ok("File memory is off in this build (spec 3.1 §7.15).".into()),
        }
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
        match &info.degraded_reason {
            Some(reason) => format!("shell: {} — {reason}", info.isolation),
            None => format!("shell: {}", info.isolation),
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
