//! Session backends and context compaction, end to end.
//!
//! Every scenario drives the real stack — `LlmAgentBuilder`, `Runner`, a session service, and
//! `OpenAIClient` — and asserts on what the session service stored and what reached the model.
//! Deterministic scenarios point the real client at a scripted OpenAI-compatible server on
//! 127.0.0.1, which also records every request; the live scenario runs when `OPENAI_API_KEY` is
//! set.
//!
//! | Variable | Used by | Example |
//! |----------|---------|---------|
//! | `REDIS_CLUSTER_NODES` | cluster scenarios, identifier checks | `127.0.0.1:7001,127.0.0.1:7002,127.0.0.1:7003` |
//! | `REDIS_URL` | standalone scenarios, identifier checks | `redis://127.0.0.1:6380` |
//! | `POSTGRES_URL` | identifier checks | `postgres://postgres@127.0.0.1:5432/adk_sessions` |
//! | `SESSION_SEED_LABEL` | labels a standalone session this run seeds | `main` |
//! | `EXPECT_SEED_FROM` | requires the standalone session to have been seeded by that label | `main` |
//!
//! ```bash
//! cargo run --manifest-path examples/session_backends/Cargo.toml
//! cargo run --manifest-path examples/session_backends/Cargo.toml -- --scenario invalid_identifiers
//! ```

mod scripted;

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use adk_agent::{LlmAgentBuilder, LlmEventSummarizer};
use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_core::{
    AdkError, Content, ErrorCategory, Event, EventsCompactionConfig, Llm, Part, RunConfig,
    StreamingMode, Tool, ToolContext,
};
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use adk_runner::Runner;
use adk_session::{
    AppendEventRequest, CreateRequest, DeleteRequest, EncryptedSession, EncryptionKey, GetRequest,
    InMemorySessionService, ListRequest, PostgresSessionService, RedisSessionConfig,
    RedisSessionService, SessionService, SqliteSessionService,
};
use adk_tool::FunctionTool;
use futures::StreamExt;
use scripted::{Reply, ScriptedServer, message_text, messages};
use serde_json::{Value, json};

const APP: &str = "session-backends-example";
const LIVE_MODEL: &str = "gpt-5.6-terra";
const SUMMARY_PROMPT_MARKER: &str = "The following is a conversation history";
const TOOL_FACT: &str = "FACT-TOOL-7731";
const LEGACY_USER: &str = "legacy:{user}";
const LEGACY_SESSION: &str = "legacy-seed";

enum Verdict {
    Pass(String),
    Fail(String),
    Skip(String),
}

/// Each check is `(holds, what it asserts or observed)`; the evidence marks the ones that fail.
fn verdict(checks: Vec<(bool, String)>) -> Verdict {
    let all_hold = checks.iter().all(|(holds, _)| *holds);
    let evidence = checks
        .into_iter()
        .map(|(holds, message)| if holds { message } else { format!("[FAILED] {message}") })
        .collect::<Vec<_>>()
        .join("\n      ");
    if all_hold { Verdict::Pass(evidence) } else { Verdict::Fail(evidence) }
}

fn cluster_nodes() -> Option<Vec<String>> {
    let nodes = std::env::var("REDIS_CLUSTER_NODES").ok()?;
    let nodes: Vec<String> =
        nodes.split(',').map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).collect();
    (!nodes.is_empty()).then_some(nodes)
}

async fn cluster_service() -> anyhow::Result<Option<RedisSessionService>> {
    let Some(nodes) = cluster_nodes() else { return Ok(None) };
    let config = RedisSessionConfig {
        url: format!("redis-cluster://{}", nodes[0]),
        ttl: None,
        cluster_nodes: Some(nodes),
    };
    Ok(Some(RedisSessionService::new(config).await?))
}

async fn standalone_service() -> anyhow::Result<Option<RedisSessionService>> {
    let Ok(url) = std::env::var("REDIS_URL") else { return Ok(None) };
    let config = RedisSessionConfig { url, ttl: None, cluster_nodes: None };
    Ok(Some(RedisSessionService::new(config).await?))
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

fn get_request(user: &str, session: &str) -> GetRequest {
    GetRequest {
        app_name: APP.to_string(),
        user_id: user.to_string(),
        session_id: session.to_string(),
        num_recent_events: None,
        after: None,
    }
}

/// Runs one user turn through the runner and returns the final text, or the error that ended it.
async fn turn(runner: &Runner, user: &str, session: &str, text: &str) -> Result<String, String> {
    let mut stream = runner
        .run_str(user, session, Content::new("user").with_text(text))
        .await
        .map_err(|error| error.to_string())?;
    let mut reply = String::new();
    while let Some(item) = stream.next().await {
        let event = item.map_err(|error| error.to_string())?;
        if event.llm_response.partial {
            continue;
        }
        if let Some(content) = &event.llm_response.content {
            let text: String = content.parts.iter().filter_map(Part::text).collect();
            if !text.is_empty() {
                reply = text;
            }
        }
    }
    Ok(reply)
}

/// An order lookup whose result exists only in the tool response; it also writes one state key
/// per tier (app, user, session), so the runner's append carries all three.
fn lookup_tool() -> Arc<dyn Tool> {
    Arc::new(FunctionTool::new(
        "lookup_fact",
        "Looks up the stored fact for an order. Arguments: order_id (string).",
        |ctx: Arc<dyn ToolContext>, _args: Value| async move {
            let mut actions = ctx.actions();
            actions.state_delta.insert("app:lookups".to_string(), json!(1));
            actions.state_delta.insert("user:last_lookup".to_string(), json!(TOOL_FACT));
            actions.state_delta.insert("last_order".to_string(), json!("7"));
            ctx.set_actions(actions);
            Ok(json!({"fact": TOOL_FACT, "status": "shipped"}))
        },
    ))
}

/// The scripted agent: calls `lookup_fact` when the user asks to look something up, answers
/// "noted" after a tool result, and "ok" otherwise. Summarization requests get a summary that
/// lists every `FACT-…` token present in the summarizer's input, as a faithful summarizer would.
fn agent_script() -> scripted::Script {
    Box::new(|request: &Value| {
        let messages = messages(request);
        let last = messages.last().cloned().unwrap_or(Value::Null);
        let last_text = message_text(&last);
        if last_text.contains(SUMMARY_PROMPT_MARKER) {
            let mut facts: Vec<String> = Vec::new();
            for word in last_text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) {
                if word.starts_with("FACT-") && !facts.iter().any(|fact| fact == word) {
                    facts.push(word.to_string());
                }
            }
            return Reply::Text(format!(
                "Summary of earlier turns. Facts: [{}].",
                facts.join(", ")
            ));
        }
        match last.get("role").and_then(Value::as_str) {
            Some("tool") => Reply::Text("noted".to_string()),
            Some("user") if last_text.to_lowercase().contains("look up") => Reply::Call(
                format!("call_{}", messages.len()),
                "lookup_fact".to_string(),
                r#"{"order_id":"7"}"#.to_string(),
            ),
            _ => Reply::Text("ok".to_string()),
        }
    })
}

fn scripted_model(server: &ScriptedServer) -> anyhow::Result<Arc<dyn Llm>> {
    Ok(Arc::new(OpenAIClient::compatible("scripted-key", &server.base_url, "scripted-model")?))
}

fn runner_for(
    model: Arc<dyn Llm>,
    sessions: Arc<dyn SessionService>,
    run_config: RunConfig,
    compaction: Option<EventsCompactionConfig>,
) -> anyhow::Result<Runner> {
    let agent = LlmAgentBuilder::new("assistant")
        .description("Answers questions and looks up orders.")
        .instruction("You are a concise assistant. Use lookup_fact when asked to look up an order.")
        .model(model)
        .tool(lookup_tool())
        .build()?;
    let mut builder = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(sessions)
        .run_config(run_config);
    if let Some(compaction) = compaction {
        builder = builder.compaction_config(compaction);
    }
    Ok(builder.build()?)
}

fn identity(user: &str, session: &str) -> anyhow::Result<AdkIdentity> {
    Ok(AdkIdentity::new(
        AppName::try_from(APP)?,
        UserId::try_from(user)?,
        SessionId::try_from(session)?,
    ))
}

fn state_event(delta: &[(&str, Value)]) -> Event {
    let mut event = Event::new(uuid::Uuid::new_v4().to_string());
    event.author = "system".to_string();
    for (key, value) in delta {
        event.actions.state_delta.insert((*key).to_string(), value.clone());
    }
    event
}

// ---------------------------------------------------------------------------
// (a) Redis Cluster
// ---------------------------------------------------------------------------

/// Create, two runner turns (one with a tool call writing all three state tiers), list, a direct
/// append, delete, and delete-all on a Redis Cluster, with identifiers containing `:`, `{`, `}`.
async fn cluster_lifecycle() -> anyhow::Result<Verdict> {
    let Some(service) = cluster_service().await? else {
        return Ok(Verdict::Skip("REDIS_CLUSTER_NODES is not set".into()));
    };
    let sessions: Arc<dyn SessionService> = Arc::new(service);
    let server = ScriptedServer::start(agent_script()).await?;
    let user = unique("user:{a}");
    let first = unique("s{1}:x");
    let second = unique("s2");

    let created = sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: user.clone(),
            session_id: Some(first.clone()),
            state: HashMap::from([
                ("app:theme".to_string(), json!("dark")),
                ("user:lang".to_string(), json!("sw")),
                ("topic".to_string(), json!("orders")),
            ]),
        })
        .await;
    if let Err(error) = created {
        return Ok(verdict(vec![(false, format!("create on the cluster failed: {error}"))]));
    }

    let runner =
        runner_for(scripted_model(&server)?, sessions.clone(), RunConfig::default(), None)?;
    let turn_one = turn(&runner, &user, &first, "Please look up order 7.").await;
    let turn_two = turn(&runner, &user, &first, "Thanks.").await;

    let mut checks = vec![
        (
            turn_one.as_deref() == Ok("noted"),
            format!("turn 1 (tool call) on the cluster: {turn_one:?}"),
        ),
        (turn_two.as_deref() == Ok("ok"), format!("turn 2 on the cluster: {turn_two:?}")),
    ];

    let appended = sessions
        .append_event_for_identity(AppendEventRequest {
            identity: identity(&user, &first)?,
            event: state_event(&[
                ("app:direct", json!(true)),
                ("user:direct", json!(2)),
                ("note", json!("n")),
            ]),
        })
        .await;
    checks.push((
        appended.is_ok(),
        format!("append_event_for_identity with all three tiers: {appended:?}"),
    ));
    let by_id = sessions.append_event(&first, state_event(&[("by_id", json!(true))])).await;
    checks.push((by_id.is_ok(), format!("append_event by session id: {by_id:?}")));

    match sessions.get(get_request(&user, &first)).await {
        Ok(session) => {
            let events = session.events().all();
            let state = session.state().all();
            let has_tool_result = events.iter().any(|event| {
                event.llm_response.content.as_ref().is_some_and(|content| {
                    content.parts.iter().any(|part| matches!(part, Part::FunctionResponse { .. }))
                })
            });
            let expected = [
                ("app:theme", json!("dark")),
                ("user:lang", json!("sw")),
                ("topic", json!("orders")),
                ("app:lookups", json!(1)),
                ("user:last_lookup", json!(TOOL_FACT)),
                ("last_order", json!("7")),
                ("app:direct", json!(true)),
                ("user:direct", json!(2)),
                ("note", json!("n")),
                ("by_id", json!(true)),
            ];
            let missing: Vec<&str> = expected
                .iter()
                .filter(|(key, value)| state.get(*key) != Some(value))
                .map(|(key, _)| *key)
                .collect();
            let users = events.iter().filter(|event| event.author == "user").count();
            checks
                .push((users == 2, format!("{} events stored, {users} user turns", events.len())));
            checks.push((has_tool_result, "the tool response event is stored".into()));
            checks.push((
                missing.is_empty(),
                format!("app, user and session state read back; missing: {missing:?}"),
            ));
        }
        Err(error) => checks.push((false, format!("get after the turns failed: {error}"))),
    }

    let second_created = sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: user.clone(),
            session_id: Some(second.clone()),
            state: HashMap::new(),
        })
        .await;
    checks.push((
        second_created.is_ok(),
        format!("second session created: {:?}", second_created.err()),
    ));
    let listed = sessions
        .list(ListRequest {
            app_name: APP.to_string(),
            user_id: user.clone(),
            limit: None,
            offset: None,
        })
        .await
        .map(|sessions| sessions.len());
    checks.push((
        listed.as_ref().ok() == Some(&2),
        format!("list returns both sessions: {listed:?}"),
    ));

    let deleted = sessions
        .delete(DeleteRequest {
            app_name: APP.to_string(),
            user_id: user.clone(),
            session_id: first.clone(),
        })
        .await;
    let gone = sessions.get(get_request(&user, &first)).await.err().map(|error| error.category);
    checks.push((
        deleted.is_ok() && gone == Some(ErrorCategory::NotFound),
        format!("delete: {deleted:?}, then get: {gone:?}"),
    ));
    let cleared = sessions.delete_all_sessions(APP, &user).await;
    let remaining = sessions
        .list(ListRequest {
            app_name: APP.to_string(),
            user_id: user.clone(),
            limit: None,
            offset: None,
        })
        .await
        .map(|sessions| sessions.len());
    checks.push((
        cleared.is_ok() && remaining.as_ref().ok() == Some(&0),
        format!("delete_all_sessions: {cleared:?}, then list: {remaining:?}"),
    ));
    Ok(verdict(checks))
}

/// Three live turns on a Redis Cluster: a fact, a tool call, and a question needing both.
async fn cluster_multi_turn_live() -> anyhow::Result<Verdict> {
    let Ok(key) = std::env::var("OPENAI_API_KEY") else {
        return Ok(Verdict::Skip("OPENAI_API_KEY is not set".into()));
    };
    let Some(service) = cluster_service().await? else {
        return Ok(Verdict::Skip("REDIS_CLUSTER_NODES is not set".into()));
    };
    let sessions: Arc<dyn SessionService> = Arc::new(service);
    let user = unique("live:{user}");
    let session = unique("live");
    if let Err(error) = sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: user.clone(),
            session_id: Some(session.clone()),
            state: HashMap::new(),
        })
        .await
    {
        return Ok(verdict(vec![(false, format!("create on the cluster failed: {error}"))]));
    }
    let model: Arc<dyn Llm> = Arc::new(OpenAIClient::new(OpenAIConfig::new(key, LIVE_MODEL))?);
    let runner = runner_for(model, sessions.clone(), RunConfig::default(), None)?;

    let one =
        turn(&runner, &user, &session, "My locker code is 4417. Reply with just: noted.").await;
    let two = turn(
        &runner,
        &user,
        &session,
        "Look up order 7 with the lookup_fact tool and tell me its fact.",
    )
    .await;
    let three = turn(
        &runner,
        &user,
        &session,
        "What is my locker code, and what fact did the tool return for order 7? Answer in one line.",
    )
    .await;
    let mut checks = vec![
        (one.is_ok(), format!("turn 1: {one:?}")),
        (
            two.as_ref().is_ok_and(|text| text.contains(TOOL_FACT)),
            format!("turn 2 (tool): {two:?}"),
        ),
        (
            three.as_ref().is_ok_and(|text| text.contains("4417") && text.contains(TOOL_FACT)),
            format!("turn 3 recalls turn 1 and the tool result: {three:?}"),
        ),
    ];
    match sessions.get(get_request(&user, &session)).await {
        Ok(stored) => {
            let events = stored.events().all();
            let users = events.iter().filter(|event| event.author == "user").count();
            checks
                .push((users == 3, format!("{} events stored, {users} user turns", events.len())));
        }
        Err(error) => checks.push((false, format!("get failed: {error}"))),
    }
    let _ = sessions.delete_all_sessions(APP, &user).await;
    Ok(verdict(checks))
}

// ---------------------------------------------------------------------------
// (b) Existing standalone data
// ---------------------------------------------------------------------------

fn user_turns(session: &dyn adk_session::Session) -> usize {
    session.events().all().iter().filter(|event| event.author == "user").count()
}

/// Reads a standalone session seeded earlier (by the build named in `SESSION_SEED_LABEL`), then
/// appends to it. Seeds it first when it is absent.
async fn standalone_existing_data() -> anyhow::Result<Verdict> {
    let Some(service) = standalone_service().await? else {
        return Ok(Verdict::Skip("REDIS_URL is not set".into()));
    };
    let sessions: Arc<dyn SessionService> = Arc::new(service);
    let server = ScriptedServer::start(agent_script()).await?;
    let runner =
        runner_for(scripted_model(&server)?, sessions.clone(), RunConfig::default(), None)?;
    let label = std::env::var("SESSION_SEED_LABEL").unwrap_or_else(|_| "this build".to_string());
    let expect_from = std::env::var("EXPECT_SEED_FROM").ok();

    let existed = match sessions.get(get_request(LEGACY_USER, LEGACY_SESSION)).await {
        Ok(_) => true,
        Err(error) if error.is_not_found() => false,
        Err(error) => return Err(error.into()),
    };
    if !existed {
        sessions
            .create(CreateRequest {
                app_name: APP.to_string(),
                user_id: LEGACY_USER.to_string(),
                session_id: Some(LEGACY_SESSION.to_string()),
                state: HashMap::from([
                    ("seeded_by".to_string(), json!(label)),
                    ("app:legacy_flag".to_string(), json!(true)),
                    ("user:legacy_pref".to_string(), json!("metric")),
                ]),
            })
            .await?;
        turn(&runner, LEGACY_USER, LEGACY_SESSION, "Please look up order 7.")
            .await
            .map_err(anyhow::Error::msg)?;
    }

    let mut checks = Vec::new();
    let before = match sessions.get(get_request(LEGACY_USER, LEGACY_SESSION)).await {
        Ok(session) => {
            let state = session.state().all();
            let seeded_by =
                state.get("seeded_by").and_then(Value::as_str).unwrap_or("").to_string();
            let tiers = state.get("app:legacy_flag") == Some(&json!(true))
                && state.get("user:legacy_pref") == Some(&json!("metric"))
                && state.get("user:last_lookup") == Some(&json!(TOOL_FACT))
                && state.get("last_order") == Some(&json!("7"));
            checks.push((
                true,
                format!(
                    "{} session seeded by {seeded_by:?}",
                    if existed { "read an existing" } else { "seeded a new" }
                ),
            ));
            if let Some(expected) = &expect_from {
                checks.push((
                    existed && seeded_by == *expected,
                    format!("the session was written by {expected:?} before this run"),
                ));
            }
            checks
                .push((tiers, "app, user and session tiers written at seed time read back".into()));
            user_turns(session.as_ref())
        }
        Err(error) => {
            checks.push((false, format!("get of the existing session failed: {error}")));
            return Ok(verdict(checks));
        }
    };
    checks.push((before >= 1, format!("{before} stored user turns read back")));

    let listed = sessions
        .list(ListRequest {
            app_name: APP.to_string(),
            user_id: LEGACY_USER.to_string(),
            limit: None,
            offset: None,
        })
        .await
        .map(|sessions| sessions.iter().any(|session| session.id() == LEGACY_SESSION));
    checks.push((listed.as_ref().ok() == Some(&true), format!("list includes it: {listed:?}")));

    let appended = turn(&runner, LEGACY_USER, LEGACY_SESSION, "Thanks.").await;
    let after = sessions
        .get(get_request(LEGACY_USER, LEGACY_SESSION))
        .await
        .map(|session| user_turns(session.as_ref()));
    checks.push((
        appended.as_deref() == Ok("ok") && after.as_ref().ok() == Some(&(before + 1)),
        format!("a new turn appends to it: {appended:?}, user turns {before} -> {after:?}"),
    ));
    Ok(verdict(checks))
}

// ---------------------------------------------------------------------------
// (c) Identifier validation
// ---------------------------------------------------------------------------

/// Each case is `(operation, outcome)`; an outcome is rejected when it is an `InvalidInput` error.
async fn identifier_cases(
    service: &dyn SessionService,
) -> Vec<(&'static str, Result<(), AdkError>)> {
    let too_long = "x".repeat(adk_core::identity::MAX_ID_LEN + 1);
    let mut cases = Vec::new();
    for (operation, app, user, session) in [
        ("create: empty app_name", "", "u", "s"),
        ("create: NUL in user_id", APP, "u\0x", "s"),
        ("create: oversized session_id", APP, "u", too_long.as_str()),
    ] {
        let outcome = service
            .create(CreateRequest {
                app_name: app.to_string(),
                user_id: user.to_string(),
                session_id: Some(session.to_string()),
                state: HashMap::new(),
            })
            .await
            .map(|_| ());
        cases.push((operation, outcome));
    }
    let outcome = service
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: String::new(),
            session_id: None,
            state: HashMap::new(),
        })
        .await
        .map(|_| ());
    cases.push(("create: empty user_id, generated session_id", outcome));
    for (operation, app, user, session) in
        [("delete: empty app_name", "", "u", "s"), ("delete: NUL in session_id", APP, "u", "s\0")]
    {
        let outcome = service
            .delete(DeleteRequest {
                app_name: app.to_string(),
                user_id: user.to_string(),
                session_id: session.to_string(),
            })
            .await;
        cases.push((operation, outcome));
    }
    for (operation, session) in [
        ("append: empty session_id", ""),
        ("append: NUL in session_id", "s\0"),
        ("append: oversized session_id", too_long.as_str()),
    ] {
        cases.push((operation, service.append_event(session, Event::new("inv")).await));
    }
    cases.push(("delete_all: empty app_name", service.delete_all_sessions("", "u").await));
    let outcome = service
        .list(ListRequest {
            app_name: APP.to_string(),
            user_id: String::new(),
            limit: None,
            offset: None,
        })
        .await
        .map(|_| ());
    cases.push(("list: empty user_id", outcome));
    cases
}

async fn invalid_identifiers() -> anyhow::Result<Verdict> {
    let dir = tempfile::tempdir()?;
    let mut backends: Vec<(String, Arc<dyn SessionService>)> = vec![
        ("in-memory".into(), Arc::new(InMemorySessionService::new())),
        (
            "encrypted(in-memory)".into(),
            Arc::new(EncryptedSession::new(
                InMemorySessionService::new(),
                EncryptionKey::generate(),
                vec![],
            )),
        ),
    ];
    let sqlite_url = format!("sqlite:{}?mode=rwc", dir.path().join("sessions.db").display());
    let sqlite = SqliteSessionService::new(&sqlite_url).await?;
    sqlite.migrate().await?;
    backends.push(("sqlite".into(), Arc::new(sqlite)));
    let mut skipped = Vec::new();
    match std::env::var("POSTGRES_URL") {
        Ok(url) => {
            let postgres = PostgresSessionService::new(&url).await?;
            postgres.migrate().await?;
            backends.push(("postgres".into(), Arc::new(postgres)));
        }
        Err(_) => skipped.push("postgres (POSTGRES_URL unset)"),
    }
    match standalone_service().await? {
        Some(service) => backends.push(("redis".into(), Arc::new(service))),
        None => skipped.push("redis (REDIS_URL unset)"),
    }
    match cluster_service().await? {
        Some(service) => backends.push(("redis-cluster".into(), Arc::new(service))),
        None => skipped.push("redis-cluster (REDIS_CLUSTER_NODES unset)"),
    }

    let mut checks = Vec::new();
    for (name, service) in &backends {
        let cases = identifier_cases(service.as_ref()).await;
        let accepted: Vec<String> = cases
            .iter()
            .filter(|(_, outcome)| {
                !matches!(outcome, Err(error) if error.category == ErrorCategory::InvalidInput)
            })
            .map(|(operation, outcome)| match outcome {
                Ok(()) => format!("{operation} -> Ok"),
                Err(error) => format!("{operation} -> {} {:?}", error.code, error.category),
            })
            .collect();
        checks.push((
            accepted.is_empty(),
            if accepted.is_empty() {
                format!("{name}: all {} invalid identifiers rejected as InvalidInput", cases.len())
            } else {
                format!("{name}: not rejected: {}", accepted.join("; "))
            },
        ));
    }
    if !skipped.is_empty() {
        checks.push((true, format!("not run: {}", skipped.join(", "))));
    }
    Ok(verdict(checks))
}

// ---------------------------------------------------------------------------
// (d) Compaction keeps history
// ---------------------------------------------------------------------------

/// Four turns with compaction after every turn (overlap 1) and a 4-event history window. The
/// turn-1 user message and tool result fall out of the window, so by turn 4 they reach the model
/// only through the summaries.
async fn compaction_keeps_history() -> anyhow::Result<Verdict> {
    let (sessions, backend): (Arc<dyn SessionService>, &str) = match standalone_service().await? {
        Some(service) => (Arc::new(service), "standalone Redis"),
        None => (Arc::new(InMemorySessionService::new()), "in-memory"),
    };
    let server = ScriptedServer::start(agent_script()).await?;
    let model = scripted_model(&server)?;
    let compaction = EventsCompactionConfig {
        compaction_interval: 1,
        overlap_size: 1,
        summarizer: Arc::new(LlmEventSummarizer::new(model.clone())),
    };
    let run_config = RunConfig::builder()
        .streaming_mode(StreamingMode::None)
        .history_max_events(Some(4))
        .build();
    let runner = runner_for(model, sessions.clone(), run_config, Some(compaction))?;
    let user = unique("compaction-user");
    let session = unique("compaction");
    sessions
        .create(CreateRequest {
            app_name: APP.to_string(),
            user_id: user.clone(),
            session_id: Some(session.clone()),
            state: HashMap::new(),
        })
        .await?;

    let first_message = "Remember FACT-A: the vault code is 1234. Then look up order 7.";
    let mut turns = Vec::new();
    for text in [first_message, "Second turn.", "Third turn.", "Fourth turn."] {
        turns.push(turn(&runner, &user, &session, text).await);
    }

    let requests = server.requests();
    let summaries: Vec<String> = requests
        .iter()
        .filter_map(|request| messages(request).last().map(message_text))
        .filter(|text| text.contains(SUMMARY_PROMPT_MARKER))
        .collect();
    let agent_requests: Vec<&Value> = requests
        .iter()
        .filter(|request| {
            !messages(request)
                .last()
                .map(message_text)
                .unwrap_or_default()
                .contains(SUMMARY_PROMPT_MARKER)
        })
        .collect();
    let last_agent_request =
        agent_requests.last().map(|request| request.to_string()).unwrap_or_default();
    let compactions = match sessions.get(get_request(&user, &session)).await {
        Ok(stored) => {
            stored.events().all().iter().filter(|event| event.actions.compaction.is_some()).count()
        }
        Err(_) => 0,
    };
    let second_summary_input = summaries.get(1).cloned().unwrap_or_default();

    Ok(verdict(vec![
        (turns.iter().all(Result::is_ok), format!("four turns on {backend}: {turns:?}")),
        (compactions >= 2, format!("{compactions} compaction events stored")),
        (
            second_summary_input.contains("FACT-A") && second_summary_input.contains(TOOL_FACT),
            "the second summarizer call received the first summary's facts and the tool result"
                .into(),
        ),
        (
            !last_agent_request.contains("vault code is 1234"),
            "the turn-1 message itself is outside the turn-4 history window".into(),
        ),
        (
            last_agent_request.contains("FACT-A"),
            "turn-4 model request carries FACT-A (from summary 1)".into(),
        ),
        (
            last_agent_request.contains(TOOL_FACT),
            format!("turn-4 model request carries the turn-1 tool result {TOOL_FACT}"),
        ),
    ]))
}

const SCENARIOS: &[&str] = &[
    "cluster_lifecycle",
    "cluster_multi_turn_live",
    "standalone_existing_data",
    "invalid_identifiers",
    "compaction_keeps_history",
];

async fn run_scenario(name: &str) -> anyhow::Result<Verdict> {
    match name {
        "cluster_lifecycle" => cluster_lifecycle().await,
        "cluster_multi_turn_live" => cluster_multi_turn_live().await,
        "standalone_existing_data" => standalone_existing_data().await,
        "invalid_identifiers" => invalid_identifiers().await,
        "compaction_keeps_history" => compaction_keeps_history().await,
        other => anyhow::bail!("unknown scenario {other}; known: {}", SCENARIOS.join(", ")),
    }
}

fn load_env() {
    let env_file = std::env::var("ADK_ENV_FILE").unwrap_or_else(|_| ".env".to_string());
    let _ = dotenvy::from_path_override(&env_file);
}

#[tokio::main]
async fn main() -> ExitCode {
    load_env();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "error".into()),
        )
        .try_init();
    adk_core::ensure_crypto_provider();

    let mut selected: Vec<String> = SCENARIOS.iter().map(ToString::to_string).collect();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        if flag == "--scenario"
            && let Some(value) = args.next()
        {
            selected = value.split(',').map(str::to_string).collect();
        }
    }

    println!("session backends — {} scenario(s)\n", selected.len());
    let mut failures = 0;
    for name in &selected {
        let result = tokio::time::timeout(Duration::from_secs(180), run_scenario(name)).await;
        let (status, evidence) = match result {
            Ok(Ok(Verdict::Pass(evidence))) => ("PASS", evidence),
            Ok(Ok(Verdict::Skip(reason))) => ("SKIP", reason),
            Ok(Ok(Verdict::Fail(evidence))) => {
                failures += 1;
                ("FAIL", evidence)
            }
            Ok(Err(error)) => {
                failures += 1;
                ("FAIL", format!("scenario error: {error:#}"))
            }
            Err(_) => {
                failures += 1;
                ("FAIL", "timed out after 180s".into())
            }
        };
        println!("{status}  {name}\n      {evidence}");
    }
    println!("\n{} scenario(s), {failures} failed", selected.len());
    if failures == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
