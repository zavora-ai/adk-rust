//! `adk-rust sessions`, `rules`, `memory` and `approvals` (spec 3.1 §8.1, §7.15.2).

use std::sync::Arc;

use adk_core::{ApprovalScope, ApprovalStore, Part, ToolApproval, ToolOrigin};
use adk_harness::{
    HarnessAgent, HarnessMode, OriginPin, RulePredicate, RuleScope,
    SHELL_PREFIX_SUGGESTION_DENYLIST, ShellIsolation, StandingDecision, StandingRule,
    StandingRuleStore,
};
use adk_session::{DeleteRequest, GetRequest, ListRequest, SessionService};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::cli::{ApprovalsCommands, MemoryCommands, RulesCommands, SessionsCommands};

const APP_NAME: &str = "adk-rust";

/// Escapes control characters in text read back from the store.
fn safe(text: &str) -> String {
    adk_harness::escape_control(text)
}

fn one_line(text: &str, max: usize) -> String {
    let text = safe(text);
    let cut: String = text.chars().take(max).collect();
    if cut.len() < text.len() { format!("{cut}…") } else { cut }
}

/// `adk-rust sessions …`.
pub async fn sessions(command: SessionsCommands) -> Result<()> {
    let db = crate::sessions::open_harness_db().await?;
    let user = crate::setup::os_user();
    let store = db.sessions.as_ref();
    match command {
        SessionsCommands::List => {
            let mut listed = store
                .list(ListRequest {
                    app_name: APP_NAME.into(),
                    user_id: user.clone(),
                    limit: None,
                    offset: None,
                })
                .await?;
            listed.sort_by_key(|session| std::cmp::Reverse(session.last_update_time()));
            if listed.is_empty() {
                println!("No sessions yet. Start one with `adk-rust`.");
            }
            for session in listed {
                let full = get(store, &user, session.id()).await?;
                let workspace = full
                    .state()
                    .get("harness:workspace")
                    .and_then(|value| value.as_str().map(str::to_string))
                    .unwrap_or_else(|| "-".into());
                println!(
                    "{}  {}  {:>4} events  {}",
                    safe(full.id()),
                    full.last_update_time().format("%Y-%m-%d %H:%M"),
                    full.events().len(),
                    safe(&workspace)
                );
            }
        }
        SessionsCommands::Show { id } => {
            let session = get(store, &user, &id).await?;
            for event in session.events().all() {
                let Some(content) = &event.llm_response.content else { continue };
                if event.llm_response.partial {
                    continue;
                }
                for part in &content.parts {
                    match part {
                        Part::Text { text } if !text.trim().is_empty() => {
                            println!("[{}] {}", safe(&event.author), safe(text.trim()));
                        }
                        Part::FunctionCall { name, args, .. } => {
                            println!(
                                "[{}] call {} {}",
                                safe(&event.author),
                                safe(name),
                                one_line(&args.to_string(), 160)
                            );
                        }
                        Part::FunctionResponse { function_response, .. } => {
                            println!(
                                "[{}] result {} {}",
                                safe(&event.author),
                                safe(&function_response.name),
                                one_line(&function_response.response.to_string(), 160)
                            );
                        }
                        _ => {}
                    }
                }
            }
        }
        SessionsCommands::Rm { id } => {
            get(store, &user, &id).await?;
            store
                .delete(DeleteRequest {
                    app_name: APP_NAME.into(),
                    user_id: user,
                    session_id: id.clone(),
                })
                .await?;
            println!("deleted session {}", safe(&id));
        }
    }
    Ok(())
}

async fn get(
    store: &dyn SessionService,
    user: &str,
    id: &str,
) -> Result<Box<dyn adk_session::Session>> {
    store
        .get(GetRequest {
            app_name: APP_NAME.into(),
            user_id: user.into(),
            session_id: id.into(),
            num_recent_events: None,
            after: None,
        })
        .await
        .with_context(|| format!("no session {id}; `adk-rust sessions list` shows yours"))
}

/// `adk-rust memory …` (spec 3.1 §7.15.2, §10 L2).
pub async fn memory(command: MemoryCommands) -> Result<()> {
    use adk_harness::{
        FactFilter, FactScope, FactSource, FactStatus, FactStore, MemoryFact, fact_digest,
    };
    let db = crate::sessions::open_harness_db().await?;
    let user = crate::setup::os_user();
    let store: Arc<dyn FactStore> = db.facts.clone();
    let mine = || FactFilter::user(APP_NAME, user.clone());
    let find = |facts: Vec<MemoryFact>, prefix: &str| -> Result<MemoryFact> {
        let prefix = prefix.trim().to_ascii_uppercase();
        let mut matches = facts.into_iter().filter(|fact| fact.id.starts_with(&prefix));
        match (matches.next(), matches.next()) {
            (Some(fact), None) => Ok(fact),
            (None, _) => bail!("no fact {prefix}; `adk-rust memory list --all` shows yours"),
            (Some(_), Some(_)) => bail!("{prefix} matches more than one fact; give more of the id"),
        }
    };
    let by = adk_core::principal::SubjectRef::human(user.clone());
    match command {
        MemoryCommands::List { dir, all } => {
            let filter = if all {
                mine()
            } else {
                let dir = match dir {
                    Some(dir) => dir,
                    None => std::env::current_dir().context("no current directory")?,
                };
                let project = adk_harness::project_slug(&dir);
                FactFilter {
                    live_only: true,
                    ..FactFilter::recall(&FactScope::new(APP_NAME, user.clone(), Some(project)))
                }
            };
            let mut facts = store.list(filter).await?;
            facts.sort_by_key(|fact| fact.status != FactStatus::Known);
            if facts.is_empty() {
                println!(
                    "No facts yet. The harness learns them when a session ends; --all lists \
                     every project."
                );
            }
            for fact in &facts {
                println!("{}", fact_line(fact));
            }
        }
        MemoryCommands::Show { id } => {
            let fact = find(store.list(mine()).await?, &id)?;
            println!("{}", serde_json::to_string_pretty(&fact)?);
        }
        MemoryCommands::Forget { id } => {
            let fact = find(store.list(mine()).await?, &id)?;
            store.set_status(&fact.id, FactStatus::Forgotten, &by).await?;
            println!("forgotten: {}", one_line(&fact.text, 200));
        }
        MemoryCommands::Accept { id } => {
            let fact = find(store.list(mine()).await?, &id)?;
            if fact.status != FactStatus::Known {
                store.set_status(&fact.id, FactStatus::Known, &by).await?;
            }
            println!("known: {}", one_line(&fact.text, 200));
        }
        MemoryCommands::Export { out } => {
            let mut text = String::new();
            for fact in store.list(mine()).await? {
                text.push_str(&serde_json::to_string(&fact)?);
                text.push('\n');
            }
            match out {
                Some(path) => {
                    let mut options = std::fs::OpenOptions::new();
                    options.write(true).create(true).truncate(true);
                    #[cfg(unix)]
                    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
                    std::io::Write::write_all(
                        &mut options
                            .open(&path)
                            .with_context(|| format!("failed to create {}", path.display()))?,
                        text.as_bytes(),
                    )?;
                    println!("{} facts written to {}", text.lines().count(), path.display());
                }
                None => print!("{text}"),
            }
        }
        MemoryCommands::Import { file } => {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("failed to read {}", file.display()))?;
            let (mut imported, mut skipped) = (0, 0);
            for line in text.lines().filter(|line| !line.trim().is_empty()) {
                let Ok(mut fact) = serde_json::from_str::<MemoryFact>(line) else {
                    skipped += 1;
                    continue;
                };
                // Imported facts are assumptions until a person confirms them (I41), and
                // pass the same length, secret and redaction checks as any other (I43).
                let text =
                    adk_harness::redact_fact_text(&fact.text, adk_harness::FACT_TEXT_MAX_CHARS);
                let (true, Ok(text)) = (fact.status.is_live(), text) else {
                    skipped += 1;
                    continue;
                };
                let original = fact.clone();
                fact = MemoryFact::new(
                    FactScope::new(APP_NAME, user.clone(), original.scope.project.clone()),
                    original.kind,
                    text,
                    FactStatus::Assumed,
                    adk_harness::Provenance { source: FactSource::Imported, ..original.provenance },
                );
                fact.valid_from = original.valid_from;
                fact.digest = fact_digest(&fact.text);
                store.put(fact).await?;
                imported += 1;
            }
            println!(
                "{imported} facts imported as assumed, {skipped} skipped; `adk-rust memory accept \
                 ID` confirms one"
            );
        }
    }
    Ok(())
}

/// One line describing `fact`: id, status, kind, date, source and session, then its text.
fn fact_line(fact: &adk_harness::MemoryFact) -> String {
    let scope = if fact.scope.project.is_some() { "project" } else { "all projects" };
    let session = &fact.provenance.session_id;
    format!(
        "{} {} · {} · {} · {} in session {} · {scope}\n    {}",
        fact.id,
        fact.status.label(),
        fact.kind,
        fact.valid_from.format("%Y-%m-%d"),
        fact.provenance.source.as_str(),
        if session.is_empty() { "-" } else { session.as_str() },
        one_line(&fact.text, 500)
    )
}

/// `adk-rust rules …`.
pub async fn rules(command: RulesCommands) -> Result<()> {
    let db = crate::sessions::open_harness_db().await?;
    let user = crate::setup::os_user();
    let store: Arc<dyn StandingRuleStore> = db.rules.clone();
    let mut listed = store.list(APP_NAME, &user).await?;
    listed.sort_by_key(|rule| rule.created_at);
    match command {
        RulesCommands::List => {
            if listed.is_empty() {
                println!(
                    "No standing rules. Answer a prompt with [a], or use `adk-rust rules add`."
                );
            }
            for (index, rule) in listed.iter().enumerate() {
                println!("{}", safe(&describe(index + 1, rule)));
            }
        }
        RulesCommands::Rm { rule } => {
            let found = match rule.parse::<usize>() {
                Ok(n) if (1..=listed.len()).contains(&n) => listed.get(n - 1),
                _ => listed.iter().find(|candidate| candidate.id == rule),
            };
            let Some(found) = found else {
                bail!("no rule {rule}; `adk-rust rules list` shows yours");
            };
            store.revoke(APP_NAME, &found.id).await?;
            println!("revoked: {}", safe(&found.label));
        }
        RulesCommands::Add {
            tool,
            prefix,
            path_within,
            deny,
            session,
            hours,
            sandbox,
            operator,
            source,
        } => {
            let rule = new_rule(
                &user,
                NewRule {
                    tool,
                    prefix,
                    path_within,
                    deny,
                    session,
                    hours,
                    sandbox,
                    operator,
                    source,
                },
            )?;
            let granted = store.grant(rule).await?;
            println!("{}", safe(&describe(listed.len() + 1, &granted)));
        }
    }
    Ok(())
}

fn describe(n: usize, rule: &StandingRule) -> String {
    let decision = match &rule.decision {
        StandingDecision::Allow => "allow".to_string(),
        StandingDecision::Deny { reason } => format!("deny ({reason})"),
    };
    let scope = match &rule.scope {
        RuleScope::User { .. } => "every session".to_string(),
        RuleScope::Session { session_id, .. } => format!("session {session_id}"),
    };
    let pin = match &rule.origin {
        OriginPin::Exact { source, declaration_digest } => {
            format!("{source} {}", &declaration_digest[..declaration_digest.len().min(19)])
        }
        OriginPin::AnyDeclaration { source } => format!("{source}, any declaration"),
    };
    let expires = rule
        .expires_at
        .map(|at| format!(", until {}", at.format("%Y-%m-%d %H:%M")))
        .unwrap_or_default();
    format!("{n}. {decision} {} — {scope}; {pin}{expires}  [{}]", rule.label, rule.id)
}

struct NewRule {
    tool: String,
    prefix: Option<String>,
    path_within: Option<String>,
    deny: Option<String>,
    session: Option<String>,
    hours: Option<u32>,
    sandbox: bool,
    operator: bool,
    source: Option<String>,
}

/// A rule from `rules add`, pinned to the harness tool's current declaration
/// (spec 3.1 §7.5, §9.2 I8–I10).
fn new_rule(user: &str, new: NewRule) -> Result<StandingRule> {
    if ["exit_plan_mode", "memory_write", "memory_delete"].contains(&new.tool.as_str()) {
        bail!("{} is never covered by a standing rule; it is decided each time", new.tool);
    }
    let mut predicates = Vec::new();
    let mut label = new.tool.clone();
    if let Some(prefix) = &new.prefix {
        let tokens: Vec<String> = prefix.split_whitespace().map(str::to_string).collect();
        let plain = |token: &String| {
            !token.is_empty()
                && !token.chars().any(|c| ";&|`$<>(){}\n\\'\"*?[]~!".contains(c))
                && !token.contains('=')
        };
        if tokens.is_empty() || !tokens.iter().all(plain) {
            bail!("--prefix takes plain command words, such as \"cargo test\"");
        }
        if SHELL_PREFIX_SUGGESTION_DENYLIST.contains(&tokens[0].as_str()) {
            bail!("a rule cannot start with `{}`: it runs any command", tokens[0]);
        }
        label = format!("{}: {} …", new.tool, tokens.join(" "));
        predicates.push(RulePredicate::ShellPrefix { pointer: "/command".into(), tokens });
    }
    if let Some(dir) = &new.path_within {
        if dir.split(['/', '\\']).any(|part| part == "..") || dir.starts_with('/') {
            bail!("--path-within takes a directory inside the workspace, such as src");
        }
        label = format!("{}: within {dir}/", new.tool);
        predicates.push(RulePredicate::PathWithin { pointer: "/path".into(), dir: dir.clone() });
    }
    // Bash rules match only command prefixes (spec 3.1 §9.2 I10).
    if new.tool == "bash" && new.prefix.is_none() && new.deny.is_none() {
        bail!("a bash allow rule needs --prefix, such as --prefix \"cargo test\"");
    }

    let origin = if new.operator {
        OriginPin::AnyDeclaration { source: new.source.unwrap_or_else(|| "static".into()) }
    } else {
        OriginPin::exact(&harness_tool_origin(&new.tool, new.sandbox)?)
    };
    let scope = match new.session {
        Some(session_id) => RuleScope::Session {
            app_name: APP_NAME.into(),
            user_id: user.into(),
            subject_kind: adk_core::principal::SubjectKind::Human,
            session_id,
        },
        None => RuleScope::User {
            app_name: APP_NAME.into(),
            user_id: user.into(),
            subject_kind: adk_core::principal::SubjectKind::Human,
        },
    };
    let decision = match new.deny {
        Some(reason) => StandingDecision::Deny { reason },
        None => StandingDecision::Allow,
    };
    let now = chrono::Utc::now();
    Ok(StandingRule {
        id: uuid::Uuid::new_v4().to_string(),
        scope,
        tool: new.tool,
        origin,
        agent: None,
        predicates,
        decision,
        created_by: adk_core::principal::SubjectRef::human(user),
        created_at: now,
        expires_at: new.hours.map(|hours| now + chrono::Duration::hours(i64::from(hours))),
        label,
        source_fingerprint: None,
    })
}

/// The origin the harness declares `tool` with, on the host or in the OS sandbox.
fn harness_tool_origin(tool: &str, sandbox: bool) -> Result<ToolOrigin> {
    let workspace = std::env::current_dir().context("no current directory")?;
    let model =
        Arc::new(adk_model::ScriptedLlm::new("rules", Vec::<adk_model::ScriptedResponse>::new()));
    let mut builder = HarnessAgent::builder(APP_NAME)
        .model(model)
        .workspace(adk_devtools::Workspace::new(&workspace).allow_bash(true))
        .mode(HarnessMode::Ask);
    if sandbox {
        builder = builder.shell(ShellIsolation::OsSandbox(Default::default()));
    }
    let agent = builder.build()?;
    let tools = futures::executor::block_on(adk_core::Toolset::tools(
        agent.runtime().toolset().as_ref(),
        Arc::new(adk_tool::SimpleToolContext::new("adk-rust")),
    ))?;
    let found = tools.iter().find(|candidate| candidate.name() == tool).with_context(|| {
        format!(
            "{tool} is not a harness tool ({}); pin another publisher's tool with --operator --source",
            tools.iter().map(|tool| tool.name().to_string()).collect::<Vec<_>>().join(", ")
        )
    })?;
    Ok(ToolOrigin::of(found.as_ref()))
}

/// A short id for a pending call: the start of its fingerprint's digest.
fn short_id(fingerprint: &str) -> String {
    Sha256::digest(fingerprint.as_bytes())
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `adk-rust approvals …`.
pub async fn approvals(
    command: ApprovalsCommands,
    server: Option<String>,
    token: Option<String>,
) -> Result<()> {
    match server {
        Some(server) => remote(command, server.trim_end_matches('/'), token).await,
        None => local(command).await,
    }
}

async fn local(command: ApprovalsCommands) -> Result<()> {
    let db = crate::sessions::open_harness_db().await?;
    let user = crate::setup::os_user();
    let mut sessions = db
        .sessions
        .list(ListRequest {
            app_name: APP_NAME.into(),
            user_id: user.clone(),
            limit: None,
            offset: None,
        })
        .await?;
    sessions.sort_by_key(|session| std::cmp::Reverse(session.last_update_time()));
    let mut pending = Vec::new();
    for session in &sessions {
        let scope = ApprovalScope::new(APP_NAME, user.clone(), session.id());
        for entry in db.approvals.pending(&scope).await? {
            pending.push((scope.clone(), entry));
        }
    }
    let find = |id: &str| {
        pending
            .iter()
            .find(|(_, entry)| short_id(&entry.fingerprint) == id || entry.fingerprint == id)
            .cloned()
            .with_context(|| format!("no pending call {id}; `adk-rust approvals list` shows them"))
    };
    match command {
        ApprovalsCommands::List => {
            if pending.is_empty() {
                println!("No calls are waiting for a decision.");
            }
            for (scope, entry) in &pending {
                println!(
                    "{}  session {}  {} {}  (held {})",
                    short_id(&entry.fingerprint),
                    safe(&scope.session_id),
                    safe(&entry.request.tool_name),
                    one_line(&entry.request.args.to_string(), 120),
                    entry.requested_at.format("%Y-%m-%d %H:%M")
                );
            }
        }
        ApprovalsCommands::Approve { id } => {
            let (scope, entry) = find(&id)?;
            db.approvals.decide(&scope, &entry.fingerprint, ToolApproval::approve()).await?;
            println!(
                "approved {} once; it runs when session {} continues: `adk-rust --session {}`",
                safe(&entry.request.tool_name),
                safe(&scope.session_id),
                safe(&scope.session_id)
            );
        }
        ApprovalsCommands::Deny { id, reason } => {
            let (scope, entry) = find(&id)?;
            let approval = match reason {
                Some(reason) => ToolApproval::deny().with_message(reason),
                None => ToolApproval::deny(),
            };
            db.approvals.decide(&scope, &entry.fingerprint, approval).await?;
            println!(
                "denied {}; the model hears it when the session continues",
                safe(&entry.request.tool_name)
            );
        }
    }
    Ok(())
}

async fn remote(command: ApprovalsCommands, server: &str, token: Option<String>) -> Result<()> {
    let client = reqwest::Client::new();
    let with_auth = |request: reqwest::RequestBuilder| match &token {
        Some(token) => request.bearer_auth(token),
        None => request,
    };
    let read = |response: reqwest::Response| async move {
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            bail!("the server answered {status}: {}", one_line(&body.to_string(), 300));
        }
        Ok(body)
    };
    match command {
        ApprovalsCommands::List => {
            let body = read(with_auth(client.get(format!("{server}/api/approvals"))).send().await?)
                .await?;
            let items = body.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
            if items.is_empty() {
                println!("No calls are waiting for a decision.");
            }
            for item in items {
                let text =
                    |key: &str| item.get(key).and_then(Value::as_str).unwrap_or("-").to_string();
                println!(
                    "{}  {}  {} {}  ({})",
                    safe(&text("id")),
                    safe(&text("state")),
                    safe(&text("toolName")),
                    one_line(
                        &item.get("argsRedacted").cloned().unwrap_or(Value::Null).to_string(),
                        120
                    ),
                    one_line(&text("summary"), 80)
                );
            }
        }
        ApprovalsCommands::Approve { ref id } | ApprovalsCommands::Deny { ref id, .. } => {
            let request =
                read(with_auth(client.get(format!("{server}/api/approvals/{id}"))).send().await?)
                    .await?;
            let version = request
                .get("version")
                .and_then(Value::as_u64)
                .context("the request has no version")?;
            let body = match &command {
                ApprovalsCommands::Deny { reason, .. } => {
                    json!({ "expectedVersion": version, "action": "reject", "reason": reason })
                }
                _ => json!({ "expectedVersion": version, "action": "approve" }),
            };
            let decided = read(
                with_auth(client.post(format!("{server}/api/approvals/{id}/decide")))
                    .json(&body)
                    .send()
                    .await?,
            )
            .await?;
            println!(
                "{}: {}",
                safe(id),
                safe(decided.get("state").and_then(Value::as_str).unwrap_or("decided"))
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add(tool: &str) -> NewRule {
        NewRule {
            tool: tool.into(),
            prefix: None,
            path_within: None,
            deny: None,
            session: None,
            hours: None,
            sandbox: false,
            operator: false,
            source: None,
        }
    }

    #[test]
    fn a_bash_rule_pins_the_declaration_and_needs_a_plain_prefix() {
        let rule =
            new_rule("ana", NewRule { prefix: Some("cargo test".into()), ..add("bash") }).unwrap();
        assert!(matches!(rule.origin, OriginPin::Exact { ref source, .. } if source == "static"));
        assert_eq!(
            rule.predicates,
            vec![RulePredicate::ShellPrefix {
                pointer: "/command".into(),
                tokens: vec!["cargo".into(), "test".into()]
            }]
        );
        assert_eq!(rule.scope.user_id(), "ana");
        for bad in ["sudo ls", "cargo test; rm -rf ~", "$(evil)", "FOO=1 make"] {
            assert!(
                new_rule("ana", NewRule { prefix: Some(bad.into()), ..add("bash") }).is_err(),
                "{bad}"
            );
        }
        assert!(new_rule("ana", add("bash")).is_err(), "bash allow needs a prefix");
        assert!(new_rule("ana", add("exit_plan_mode")).is_err());
        assert!(new_rule("ana", add("no_such_tool")).is_err());
    }

    #[test]
    fn operator_rules_pin_any_declaration_of_a_publisher() {
        let rule = new_rule(
            "ana",
            NewRule { operator: true, source: Some("mcp:github".into()), ..add("list_issues") },
        )
        .unwrap();
        assert_eq!(rule.origin, OriginPin::AnyDeclaration { source: "mcp:github".into() });
    }

    #[test]
    fn short_ids_are_stable_and_short() {
        assert_eq!(short_id("a"), short_id("a"));
        assert_ne!(short_id("a"), short_id("b"));
        assert_eq!(short_id("a").len(), 12);
    }
}
