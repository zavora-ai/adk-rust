//! `adk-rust sessions`, `rules`, `memory` and `approvals` (spec 3.1 §8.1, §7.15.2).

use std::sync::Arc;

use adk_core::{
    ApprovalAction, ApprovalDecision, ApprovalRequest, ApprovalRequestQuery, ApprovalScope,
    ApprovalService, ApprovalState, Part, StoreApprovalService, ToolApproval, ToolOrigin,
};
use adk_harness::{
    HarnessAgent, HarnessMode, OriginPin, RulePredicate, RuleScope,
    SHELL_PREFIX_SUGGESTION_DENYLIST, ShellIsolation, StandingDecision, StandingRule,
    StandingRuleStore,
};
use adk_kernel::control::ControlClient;
use adk_kernel::control::api::ApprovalItem;
use adk_session::{DeleteRequest, GetRequest, ListRequest, SessionService};
use anyhow::{Context, Result, bail};
use serde_json::Value;
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
        SessionsCommands::List { archived } => {
            let request = ListRequest {
                app_name: APP_NAME.into(),
                user_id: user.clone(),
                limit: None,
                offset: None,
            };
            let mut listed = if archived {
                store.list_archived(request).await?
            } else {
                store.list_active(request).await?
            };
            listed.sort_by_key(|session| std::cmp::Reverse(session.last_update_time()));
            if listed.is_empty() {
                if archived {
                    println!("No archived sessions.");
                } else {
                    println!("No sessions yet. Start one with `adk-rust`.");
                }
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
        SessionsCommands::Fork { id, turns, at, new_id } => {
            let identity = adk_core::AdkIdentity::new(
                adk_core::AppName::try_from(APP_NAME)?,
                adk_core::UserId::try_from(user.as_str())?,
                adk_core::SessionId::try_from(id.as_str())?,
            );
            let at = match (at, turns) {
                (Some(event_id), _) => Some(event_id),
                (None, Some(turns)) => {
                    let history = store.history(&identity).await?;
                    turn_end(&history.events, turns)
                }
                (None, None) => None,
            };
            let mut request = adk_session::ForkRequest::new(identity);
            if let Some(event_id) = at {
                request = request.at_event(event_id);
            }
            if let Some(new_id) = new_id {
                request = request.with_session_id(new_id);
            }
            let actor = adk_core::audit_log::AuditActor::subject(
                adk_core::principal::SubjectRef::human(user.clone()),
            );
            let fork = adk_runner::HistoryControl::new(db.sessions.clone())
                .with_run_store(db.runs.clone())
                .fork(&actor, request)
                .await?;
            println!(
                "forked session {} into {} with {} events; continue it with `adk-rust --session {}`",
                safe(&id),
                safe(fork.session.id()),
                fork.session.events().len(),
                safe(fork.session.id())
            );
        }
        SessionsCommands::Archive { id, reason } => {
            let session = get(store, &user, &id).await?;
            let mark = store.archive(&session.try_identity()?, reason).await?;
            println!(
                "archived session {} at {}; `adk-rust sessions unarchive {}` brings it back",
                safe(&id),
                mark.archived_at.format("%Y-%m-%d %H:%M"),
                safe(&id)
            );
        }
        SessionsCommands::Unarchive { id } => {
            let session = get(store, &user, &id).await?;
            store.unarchive(&session.try_identity()?).await?;
            println!("session {} is back in `adk-rust sessions list`", safe(&id));
        }
        SessionsCommands::Export { id, out } => {
            let session = get(store, &user, &id).await?;
            let bundle = adk_session::SessionBundle::of(session.as_ref());
            let json = serde_json::to_string_pretty(&bundle)?;
            match out {
                Some(path) => {
                    // The conversation stays readable by its owner only, as harness.db is.
                    write_private(&path, json.as_bytes())
                        .with_context(|| format!("cannot write {}", path.display()))?;
                    println!(
                        "wrote session {} ({} events) to {}",
                        safe(&id),
                        bundle.events.len(),
                        path.display()
                    );
                }
                None => println!("{json}"),
            }
        }
        SessionsCommands::Rewind { id, turn, redo, yes } => {
            let session = get(store, &user, &id).await?;
            adk_session::ensure_writable(session.as_ref())?;
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

/// The last event of the session's first `turns` turns, or `None` when it has no more turns
/// than that. A turn starts at an invocation's first event when the user wrote it.
fn turn_end(events: &[adk_session::Event], turns: u32) -> Option<String> {
    let mut started = 0_u32;
    let mut previous: Option<&str> = None;
    for (index, event) in events.iter().enumerate() {
        let new_invocation = previous != Some(event.invocation_id.as_str());
        previous = Some(&event.invocation_id);
        if new_invocation && event.author == "user" {
            started += 1;
            if started > turns {
                return index.checked_sub(1).map(|last| events[last].id.clone());
            }
        }
    }
    None
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

/// Writes `bytes` to `path`, creating it readable and writable by its owner only on Unix.
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)
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

/// `adk-rust approvals …`: the approvals inbox of the kernel running here (or at `home`), or
/// of the served harness or kernel `server` names, so the ids and decisions are the ones every
/// surface sees. Calls an in-process session holds in this machine's harness.db, which no
/// inbox files, are listed and decided beside them, and on their own when no kernel runs.
pub async fn approvals(
    command: ApprovalsCommands,
    server: Option<String>,
    token: Option<String>,
    home: Option<std::path::PathBuf>,
) -> Result<()> {
    if let Some(server) = server {
        let mut client = ControlClient::http(server.trim_end_matches('/'));
        if let Some(token) = token.filter(|token| !token.trim().is_empty()) {
            client = client.with_bearer_token(token);
        }
        return decide_or_list(command, Some(&client), false).await;
    }
    let kernel = match home {
        Some(home) => {
            let target = crate::cli::KernelTarget { home: Some(home), server: None, token: None };
            Some(crate::kernel_cli::client(&target).await?)
        }
        None => crate::kernel_cli::local().await?,
    };
    decide_or_list(command, kernel.as_ref(), true).await
}

/// One row of `approvals list`.
fn print_row(id: &str, session: &str, tool: &str, args: &Value, note: &str) {
    println!(
        "{}  session {}  {} {}  ({})",
        safe(id),
        safe(session),
        safe(tool),
        one_line(&args.to_string(), 120),
        one_line(note, 80)
    );
}

/// Lists or decides the pending calls of the inbox `kernel` reaches, and with `here` those
/// harness.db holds that no inbox filed; a decision in the inbox wakes the process parked on
/// the call.
async fn decide_or_list(
    command: ApprovalsCommands,
    kernel: Option<&ControlClient>,
    here: bool,
) -> Result<()> {
    let filed: Vec<ApprovalItem> = match kernel {
        Some(client) => client
            .approvals()
            .await?
            .into_iter()
            .filter(|item| item.state == ApprovalState::Pending)
            .collect(),
        None => Vec::new(),
    };
    // With a kernel up, harness.db's calls it filed are in its inbox under the same id.
    let held = match (here, kernel) {
        (false, _) => Vec::new(),
        (true, None) => held_here(false).await?,
        (true, Some(_)) => held_here(true).await?,
    };
    let (id, action, reason) = match command {
        ApprovalsCommands::List => {
            if filed.is_empty() && held.is_empty() {
                println!("No calls are waiting for a decision.");
            }
            for item in &filed {
                print_row(
                    &item.id,
                    &item.scope.session_id,
                    &item.tool_name,
                    &item.args_redacted,
                    &item.summary,
                );
            }
            for held in &held {
                let at = held.entry.requested_at.format("held %Y-%m-%d %H:%M").to_string();
                let request = &held.entry.request;
                print_row(
                    &held.id(),
                    &held.scope.session_id,
                    &request.tool_name,
                    &request.args,
                    &at,
                );
            }
            return Ok(());
        }
        ApprovalsCommands::Approve { id } => (id, ApprovalAction::Approve, None),
        ApprovalsCommands::Deny { id, reason } => (id, ApprovalAction::Reject, reason),
    };
    // The full id, or the start of exactly one.
    let mut starting = filed.iter().filter(|item| item.id.starts_with(&id));
    let item = match (filed.iter().find(|item| item.id == id), starting.next(), starting.next()) {
        (Some(item), _, _) | (None, Some(item), None) => Some(item),
        (None, Some(_), Some(_)) => {
            bail!("{id} starts more than one waiting call; give more of it")
        }
        (None, None, _) => None,
    };
    let Some(item) = item else {
        let Some(held) = held.iter().find(|held| held.id() == id || held.entry.fingerprint == id)
        else {
            let place = kernel.map_or_else(|| "here".to_string(), |client| client.endpoint());
            bail!("no pending call {id} at {place}; `adk-rust approvals list` shows them");
        };
        return decide_here(held, action, reason).await;
    };
    let kernel = kernel.context("a call in an inbox has a kernel")?;
    let decided = kernel.decide(&item.id, item.version, action, reason.as_deref()).await?;
    let tool = safe(&item.tool_name);
    let process = match &item.run_id {
        Some(run) => format!("process {}", safe(run)),
        None => "the turn that holds it".to_string(),
    };
    match (action, decided.state) {
        (ApprovalAction::Approve, ApprovalState::Pending) => {
            println!("approval of {tool} recorded; {} waits for more approvers", safe(&item.id));
        }
        (ApprovalAction::Approve, _) => println!("approved {tool} once; {process} continues now"),
        _ => println!("denied {tool}; {process} hears it now"),
    }
    Ok(())
}

/// A call held in harness.db, with the inbox request a kernel filed for it, if one did.
#[derive(Clone)]
struct Held {
    scope: ApprovalScope,
    entry: adk_core::PendingApproval,
    request: Option<ApprovalRequest>,
}

impl Held {
    /// The id `approvals list` shows: the inbox request's, as the kernel shows it, else the
    /// start of the call's fingerprint digest.
    fn id(&self) -> String {
        match &self.request {
            Some(request) => request.id.clone(),
            None => short_id(&self.entry.fingerprint),
        }
    }
}

/// The calls harness.db holds for this user, newest session first; with `unfiled`, only those
/// no inbox filed, which an in-process session decides.
async fn held_here(unfiled: bool) -> Result<Vec<Held>> {
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
    let mut held = Vec::new();
    for session in &sessions {
        let scope = ApprovalScope::new(APP_NAME, user.clone(), session.id());
        let entries = db.approvals.pending(&scope).await?;
        if entries.is_empty() {
            continue;
        }
        let open = db
            .approvals
            .list_requests(
                &ApprovalRequestQuery::default()
                    .with_session(&scope)
                    .with_states([ApprovalState::Pending]),
            )
            .await?;
        for entry in entries {
            let request = open.iter().find(|request| request.fingerprint == entry.fingerprint);
            if unfiled && request.is_some() {
                continue;
            }
            held.push(Held { scope: scope.clone(), request: request.cloned(), entry });
        }
    }
    Ok(held)
}

/// Decides a call harness.db holds. A request a kernel filed is decided as the operator its
/// socket would name, so it resolves and its parked process is queued for the next
/// `adk-rust up`; any other call is decided in the store for its session to take.
async fn decide_here(held: &Held, action: ApprovalAction, reason: Option<String>) -> Result<()> {
    use adk_core::principal::SubjectRef;

    let db = crate::sessions::open_default_db().await?;
    let tool = safe(&held.entry.request.tool_name);
    let (scope, fingerprint) = (&held.scope, &held.entry.fingerprint);
    if let Some(request) = &held.request {
        let service =
            StoreApprovalService::new(db.approvals.clone()).with_run_store(db.runs.clone());
        let operator = SubjectRef::human(crate::setup::os_user());
        let decision = match action {
            ApprovalAction::Approve => ApprovalDecision::approve(operator),
            _ => ApprovalDecision::reject(operator, reason.unwrap_or_default()),
        }
        .with_groups([adk_cli::harness::OPERATOR_SCOPE]);
        service.decide(&request.id, request.version, decision).await?;
        match action {
            ApprovalAction::Approve => println!(
                "approved {tool} once; the kernel's process continues when `adk-rust up` starts"
            ),
            _ => println!("denied {tool}; the kernel's process hears it when `adk-rust up` starts"),
        }
        return Ok(());
    }
    if action != ApprovalAction::Approve {
        let approval = match reason {
            Some(reason) => ToolApproval::deny().with_message(reason),
            None => ToolApproval::deny(),
        };
        db.approvals.decide(scope, fingerprint, approval).await?;
        println!("denied {tool}; the model hears it when the session continues");
        return Ok(());
    }
    db.approvals.decide(scope, fingerprint, ToolApproval::approve()).await?;
    match parked_goal(&db, scope, fingerprint).await? {
        Some(goal) => println!(
            "approved {tool} once; the goal in session {} waits for it. Continue it in its \
             workspace: `adk-rust goal {}{} --resume`",
            safe(&scope.session_id),
            safe(&shell_quote(&goal.goal)),
            goal.until
                .as_deref()
                .map(|until| format!(" --until {}", safe(&shell_quote(until))))
                .unwrap_or_default()
        ),
        None => println!(
            "approved {tool} once; it runs when session {} continues: `adk-rust --session {}`",
            safe(&scope.session_id),
            safe(&scope.session_id)
        ),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_end_names_the_last_event_of_the_kept_turns() {
        let event = |invocation: &str, author: &str, id: &str| {
            let mut event = adk_session::Event::new(invocation);
            event.author = author.into();
            event.id = id.into();
            event
        };
        let events = vec![
            event("i1", "user", "u1"),
            event("i1", "agent", "a1"),
            event("i2", "user", "u2"),
            event("i2", "agent", "a2"),
            event("i3", "user", "u3"),
        ];
        assert_eq!(turn_end(&events, 1).as_deref(), Some("a1"));
        assert_eq!(turn_end(&events, 2).as_deref(), Some("a2"));
        assert_eq!(turn_end(&events, 3), None, "every turn is kept");
    }

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

    /// K1, K13: `approvals approve` with a kernel up decides on the kernel's inbox, under the
    /// id the kernel shows or a unique start of it, and the request resolves, which wakes the
    /// process parked on it; `deny` takes a reason. Fails on `origin/main`, where the command
    /// wrote to harness.db and the request stayed pending.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn approvals_decide_on_the_kernels_inbox() {
        use adk_core::principal::SubjectRef;
        use adk_core::{
            ApprovalPolicy, ApproverSelector, InMemoryApprovalStore, InMemoryRunStore,
            ToolConfirmationRequest,
        };
        use adk_kernel::control::{ControlPlane, ControlPlaneConfig};

        let service = Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())));
        let kernel = adk_kernel::Kernel::builder()
            .config(
                adk_kernel::KernelConfig::default().with_app_name(APP_NAME).with_control(
                    ControlPlaneConfig::default()
                        .without_socket()
                        .with_http("127.0.0.1:0".parse().unwrap()),
                ),
            )
            .run_store(Arc::new(InMemoryRunStore::new()))
            .approval_service(service.clone())
            .build()
            .unwrap();
        let plane = ControlPlane::start(&kernel).await.unwrap();
        let client = ControlClient::http(format!("http://{}", plane.http_addr().unwrap()));
        let held = |id: &str, command: &str| {
            let call =
                ToolConfirmationRequest::new("bash", serde_json::json!({ "command": command }));
            ApprovalRequest::new(
                id,
                ApprovalScope::new(APP_NAME, "ana", "s-1"),
                &call,
                ApprovalPolicy::new([ApproverSelector::group("operator")]).allow_self_approval(),
            )
            .with_requester(SubjectRef::human("ana"))
        };
        service.submit(held("apr_7f7db6", "date -u >> stamp.txt")).await.unwrap();
        service.submit(held("apr_91c2aa", "rm -rf build")).await.unwrap();

        decide_or_list(ApprovalsCommands::List, Some(&client), false).await.unwrap();
        let unknown = ApprovalsCommands::Approve { id: "apr_0".into() };
        let error = decide_or_list(unknown, Some(&client), false).await.unwrap_err();
        assert!(error.to_string().contains("no pending call apr_0"), "{error}");
        let ambiguous = ApprovalsCommands::Approve { id: "apr_".into() };
        let error = decide_or_list(ambiguous, Some(&client), false).await.unwrap_err();
        assert!(error.to_string().contains("more than one"), "{error}");

        let approve = ApprovalsCommands::Approve { id: "apr_7f".into() };
        decide_or_list(approve, Some(&client), false).await.unwrap();
        let deny = ApprovalsCommands::Deny { id: "apr_91c2aa".into(), reason: Some("no".into()) };
        decide_or_list(deny, Some(&client), false).await.unwrap();
        let state = |id: &'static str| {
            let service = service.clone();
            async move { service.get(id).await.unwrap().unwrap().state }
        };
        assert_eq!(
            (state("apr_7f7db6").await, state("apr_91c2aa").await),
            (ApprovalState::Approved, ApprovalState::Rejected)
        );
        plane.shutdown().await.unwrap();
    }
}
