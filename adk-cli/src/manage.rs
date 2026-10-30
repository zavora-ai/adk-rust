//! `adk-rust sessions`, `rules`, `memory` and `approvals` (spec 3.1 §8.1, §7.15.2).

use std::sync::Arc;

use adk_core::{ApprovalScope, Part, ToolApproval, ToolOrigin};
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
    let db = crate::sessions::open_default_db().await?;
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
            let session = get(store, &user, &id).await?;
            // A session's snapshots go with it (spec 3.1 §7.27).
            if let Some(harness) = session_harness(&db, session.as_ref())? {
                let session_id = adk_core::SessionId::new(id.clone())?;
                if let Some(snapshots) = harness.snapshots() {
                    let removed =
                        snapshots.forget_session(APP_NAME, &user, session_id.as_ref()).await?;
                    if removed > 0 {
                        println!("deleted {removed} snapshots of session {}", safe(&id));
                    }
                }
            }
            store
                .delete(DeleteRequest {
                    app_name: APP_NAME.into(),
                    user_id: user,
                    session_id: id.clone(),
                })
                .await?;
            println!("deleted session {}", safe(&id));
        }
        SessionsCommands::Rewind { id, turn, redo, yes } => {
            let session = get(store, &user, &id).await?;
            let harness = session_harness(&db, session.as_ref())?.with_context(|| {
                format!(
                    "session {} records no workspace, so it has no snapshots to rewind to",
                    safe(&id)
                )
            })?;
            let user_id = adk_core::UserId::new(user.clone())?;
            let session_id = adk_core::SessionId::new(id.clone())?;
            let text =
                adk_cli::harness::rewind_session(&harness, &user_id, &session_id, turn, redo, yes)
                    .await?;
            println!("{text}");
        }
    }
    Ok(())
}

/// A harness over the workspace `session` recorded, with its snapshots, plan checkpoints and
/// stores in `harness.db`, for `sessions rewind` and `sessions rm` (spec 3.1 §7.27). No model
/// is called; `None` when the session records no workspace.
fn session_harness(
    db: &crate::sessions::HarnessDb,
    session: &dyn adk_session::Session,
) -> Result<Option<adk_harness::Harness>> {
    let Some(workspace) = session
        .state()
        .get("harness:workspace")
        .and_then(|value| value.as_str().map(str::to_string))
    else {
        return Ok(None);
    };
    let root = std::path::PathBuf::from(&workspace)
        .canonicalize()
        .with_context(|| format!("the session's workspace {} is gone", safe(&workspace)))?;
    let model =
        Arc::new(adk_model::ScriptedLlm::new("rewind", Vec::<adk_model::ScriptedResponse>::new()));
    let agent = HarnessAgent::builder(APP_NAME)
        .model(model)
        .workspace(adk_devtools::Workspace::new(&root))
        .mode_store(db.modes.clone())
        .standing_rules(db.rules.clone())
        .shell_ledger(db.ledger.clone())
        .action_ledger(db.actions.clone())
        .approval_store(db.approvals.clone())
        .plan_checkpointer(db.plans.clone())
        .snapshots(adk_harness::SnapshotConfig::default())
        .snapshot_store(db.snapshots.clone())
        .snapshot_objects(Arc::new(db.snapshot_objects(&root)))
        .build()?;
    Ok(Some(agent.into_harness(APP_NAME, db.sessions.clone())?))
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
    let db = crate::sessions::open_default_db().await?;
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
        MemoryCommands::List { source: Some(source), .. } if source == "consolidation" => {
            // The ticks, newest first, then the facts they wrote (spec 3.2 §7.5).
            let ticks = store.consolidations(APP_NAME, &user).await?;
            if ticks.is_empty() {
                println!(
                    "No consolidation tick has run. `adk-rust up` runs one in ask and auto mode \
                     once there is new material."
                );
            }
            for tick in &ticks {
                println!("{}", tick_line(tick));
            }
            let mut facts: Vec<MemoryFact> = store
                .list(mine())
                .await?
                .into_iter()
                .filter(|fact| fact.provenance.tick.is_some())
                .collect();
            facts.sort_by_key(|fact| !fact.status.is_live());
            if !facts.is_empty() {
                println!();
            }
            for fact in &facts {
                println!("{}", fact_line(fact));
            }
        }
        MemoryCommands::List { dir, all, source } => {
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
            if let Some(source) = &source {
                facts.retain(|fact| fact.provenance.source.as_str() == source);
            }
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
        MemoryCommands::Undo { tick } => {
            let undone =
                adk_harness::undo_consolidation(store.as_ref(), APP_NAME, &user, tick.trim(), &by)
                    .await?;
            println!(
                "undid tick {}: {} fact(s) assumed again, {} it wrote forgotten, {} kept (changed since)",
                undone.tick_id,
                undone.restored.len(),
                undone.forgotten.len(),
                undone.kept.len()
            );
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
                    #[cfg(not(windows))]
                    let created = options.open(&path);
                    // A DACL granting the owner alone access stands in for mode 0600
                    // (spec 3.2 §10 W4).
                    #[cfg(windows)]
                    let created = crate::owner_only::create_or_truncate(&path);
                    std::io::Write::write_all(
                        &mut created
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

/// One consolidation tick as `memory list --source consolidation` shows it: ids and counts.
fn tick_line(tick: &adk_harness::ConsolidationRecord) -> String {
    let mut line = format!(
        "tick {} {} · {} superseded · {} forgotten · {} merged · {} distilled",
        tick.tick_id,
        tick.at.format("%Y-%m-%d %H:%M"),
        tick.superseded().count(),
        tick.forgotten().count(),
        tick.merged.len(),
        tick.distilled.len()
    );
    if let Some(refused) = &tick.refused {
        line.push_str(&format!(" · refused: {}", one_line(refused, 120)));
    }
    if tick.finished_at.is_none() {
        line.push_str(" · unfinished");
    }
    if tick.undone_at.is_some() {
        line.push_str(" · undone");
    }
    line
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

/// The surface `rules add` and `rules rm` are audited with (spec 3.1 §9.3).
const RULES_SURFACE: &str = "cli";

/// `adk-rust rules …`, on `harness.db` or, with `--store`, on the store served replicas
/// share (spec 3.1 §13 Q2).
pub async fn rules(command: RulesCommands, shared: Option<String>) -> Result<()> {
    let store: Arc<dyn StandingRuleStore> = match &shared {
        Some(url) => crate::shared_store::open_rules(url).await?,
        None => crate::sessions::open_default_db().await?.rules.clone(),
    };
    rules_in(command, store, &crate::setup::os_user(), None).await
}

/// `rules` on `store` for `user`. A grant or revocation goes through the harness, which
/// records `harness.rule.created` or `harness.rule.revoked` with `surface: cli` in `audit`
/// (the harness's own log when `None`) before the store write, as the REST routes record
/// theirs with `surface: rest` (spec 3.1 §7.5, §9.2 I4, §9.3).
async fn rules_in(
    command: RulesCommands,
    store: Arc<dyn StandingRuleStore>,
    user: &str,
    audit: Option<Arc<dyn adk_core::audit_log::AuditRecorder>>,
) -> Result<()> {
    let user = user.to_string();
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
            rules_harness(store, audit)?
                .revoke_rule(APP_NAME, &user, &found.id, RULES_SURFACE)
                .await?
                .with_context(|| format!("rule {} was revoked meanwhile", found.id))?;
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
            let granted =
                rules_harness(store, audit)?.grant_rule(rule, RULES_SURFACE, None).await?;
            println!("{}", safe(&describe(listed.len() + 1, &granted)));
        }
    }
    Ok(())
}

/// The harness `rules add` and `rules rm` grant and revoke through: `store` and `audit`.
fn rules_harness(
    store: Arc<dyn StandingRuleStore>,
    audit: Option<Arc<dyn adk_core::audit_log::AuditRecorder>>,
) -> Result<HarnessAgent> {
    let workspace = std::env::current_dir().context("no current directory")?;
    let model =
        Arc::new(adk_model::ScriptedLlm::new("rules", Vec::<adk_model::ScriptedResponse>::new()));
    let mut builder = HarnessAgent::builder(APP_NAME)
        .model(model)
        .workspace(adk_devtools::Workspace::new(&workspace))
        .standing_rules(store);
    if let Some(audit) = audit {
        builder = builder.audit_log(audit);
    }
    Ok(builder.build()?)
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
        OriginPin::ExactEach { source, .. } => format!("{source}, each covered tool's declaration"),
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
    let db = crate::sessions::open_default_db().await?;
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
            match parked_goal(&db, &scope, &entry.fingerprint).await? {
                Some(goal) => println!(
                    "approved {} once; the goal in session {} waits for it. Continue it in its \
                     workspace: `adk-rust goal {} --until {} --resume`",
                    safe(&entry.request.tool_name),
                    safe(&scope.session_id),
                    safe(&shell_quote(&goal.goal)),
                    safe(&shell_quote(&goal.until))
                ),
                None => println!(
                    "approved {} once; it runs when session {} continues: `adk-rust --session {}`",
                    safe(&entry.request.tool_name),
                    safe(&scope.session_id),
                    safe(&scope.session_id)
                ),
            }
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

/// The goal of a durable goal run parked on the call with `fingerprint`, if one is
/// (spec 3.1 §7.11): such a call runs when `adk-rust goal --resume` continues the goal.
async fn parked_goal(
    db: &crate::sessions::HarnessDb,
    scope: &ApprovalScope,
    fingerprint: &str,
) -> Result<Option<adk_harness::GoalSpec>> {
    use adk_core::{DurableRunStore, RunCheckpointRef, RunFilter, RunState};
    use adk_harness::GoalStore;

    let parked = db
        .runs
        .list(
            RunFilter::default()
                .app_name(scope.app_name.clone())
                .user_id(scope.user_id.clone())
                .session_id(scope.session_id.clone())
                .states([RunState::Waiting]),
        )
        .await?
        .into_iter()
        .any(|run| {
            run.target == adk_harness::GOAL_TARGET
                && matches!(&run.checkpoint, Some(RunCheckpointRef::AwaitingApproval { fingerprints, .. })
                    if fingerprints.iter().any(|parked| parked == fingerprint))
        });
    if !parked {
        return Ok(None);
    }
    let record = db.goals.get(&scope.app_name, &scope.user_id, &scope.session_id).await?;
    Ok(record.map(|record| record.spec))
}

/// `text` as one single-quoted shell word.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
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

    fn add_bash() -> RulesCommands {
        RulesCommands::Add {
            tool: "bash".into(),
            prefix: Some("cargo test".into()),
            path_within: None,
            deny: None,
            session: None,
            hours: None,
            sandbox: false,
            operator: false,
            source: None,
        }
    }

    fn rule_records(records: &[adk_core::audit_log::AuditRecord], action: &str) -> Vec<Value> {
        records
            .iter()
            .filter(|record| record.action == action)
            .map(|record| serde_json::to_value(&record.ext).unwrap())
            .collect()
    }

    /// `rules add` and `rules rm` record `harness.rule.created` and `harness.rule.revoked`
    /// with `surface: cli`, as the REST routes record theirs (spec 3.1 §9.3).
    #[tokio::test]
    async fn rules_add_and_rm_are_audited_with_the_cli_surface() {
        let store: Arc<dyn StandingRuleStore> =
            Arc::new(adk_harness::InMemoryStandingRuleStore::new());
        let log = Arc::new(adk_harness::EphemeralAuditLog::new().unwrap());

        rules_in(add_bash(), store.clone(), "ana", Some(log.clone())).await.unwrap();
        let id = store.list(APP_NAME, "ana").await.unwrap()[0].id.clone();
        rules_in(RulesCommands::Rm { rule: id.clone() }, store.clone(), "ana", Some(log.clone()))
            .await
            .unwrap();
        assert!(store.list(APP_NAME, "ana").await.unwrap().is_empty());

        let records = log.records().await.unwrap();
        let created = rule_records(&records, "harness.rule.created");
        let revoked = rule_records(&records, "harness.rule.revoked");
        assert_eq!((created.len(), revoked.len()), (1, 1), "{records:?}");
        for ext in [&created[0], &revoked[0]] {
            assert_eq!(ext["ruleId"], id.as_str());
            assert_eq!(ext["surface"], "cli");
            assert_eq!(ext["subject"], "ana");
            assert_eq!(ext["by"], "ana");
        }
        assert!(log.verify().await.unwrap());
    }

    /// The record precedes the store write: a grant or a revocation whose record cannot be
    /// written changes nothing.
    #[tokio::test]
    async fn a_rule_change_whose_record_fails_is_not_made() {
        struct Offline;
        #[async_trait::async_trait]
        impl adk_core::audit_log::AuditRecorder for Offline {
            fn digest(&self, _value: &Value) -> String {
                "sha256:0".into()
            }
            async fn record(
                &self,
                _draft: adk_core::audit_log::AuditRecordDraft,
            ) -> adk_core::Result<()> {
                Err(adk_core::AdkError::internal(
                    adk_core::ErrorComponent::Server,
                    "audit.offline",
                    "the audit store is offline",
                ))
            }
        }
        let store: Arc<dyn StandingRuleStore> =
            Arc::new(adk_harness::InMemoryStandingRuleStore::new());

        assert!(rules_in(add_bash(), store.clone(), "ana", Some(Arc::new(Offline))).await.is_err());
        assert!(store.list(APP_NAME, "ana").await.unwrap().is_empty(), "nothing was granted");

        let log = Arc::new(adk_harness::EphemeralAuditLog::new().unwrap());
        rules_in(add_bash(), store.clone(), "ana", Some(log)).await.unwrap();
        let id = store.list(APP_NAME, "ana").await.unwrap()[0].id.clone();
        let rm = RulesCommands::Rm { rule: id };
        assert!(rules_in(rm, store.clone(), "ana", Some(Arc::new(Offline))).await.is_err());
        assert_eq!(store.list(APP_NAME, "ana").await.unwrap().len(), 1, "nothing was revoked");
    }

    #[test]
    fn short_ids_are_stable_and_short() {
        assert_eq!(short_id("a"), short_id("a"));
        assert_ne!(short_id("a"), short_id("b"));
        assert_eq!(short_id("a").len(), 12);
    }
}
