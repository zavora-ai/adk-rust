//! Fail-closed governance: runner plugin denial (#761), path guardrails (#762), and failing
//! toolsets (#760).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_agent::LlmAgentBuilder;
use adk_agent::guardrails::{PathAllowList, ToolGuardrailSet};
use adk_core::{
    AdkError, Agent, CallbackContext, Content, Llm, ReadonlyContext, Result as AdkResult, Tool,
    Toolset,
};
use adk_plugin::{Plugin, PluginConfig, PluginManager};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::common::{
    Config, CountingLlm, CountingTool, Provider, Verdict, brief, fail, pair_calls, run_turn,
    session_events,
};

const USER: &str = "user-governance";

async fn runner_for(
    app: &str,
    session_id: &str,
    agent: Arc<dyn Agent>,
    plugins: Option<PluginManager>,
) -> anyhow::Result<(Runner, Arc<InMemorySessionService>)> {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: app.to_string(),
            user_id: USER.to_string(),
            session_id: Some(session_id.to_string()),
            state: HashMap::new(),
        })
        .await?;
    let builder = Runner::builder()
        .app_name(app)
        .agent(agent)
        .session_service(Arc::clone(&sessions) as Arc<dyn SessionService>);
    let runner = match plugins {
        Some(plugins) => builder.plugin_manager(Arc::new(plugins)).build()?,
        None => builder.build()?,
    };
    Ok((runner, sessions))
}

// ─── runner_plugin_denies_tool ───────────────────────────────────────────────

const DENIAL: &str = "DENIED_BY_POLICY: delete_records is disabled for autonomous agents";

/// How the runner plugin's `before_tool` callback refuses the call.
#[derive(Clone, Copy)]
pub enum DenialForm {
    /// `Ok(Some(Content))`, the form `tool-authorization.md` and `AGENTS.md` document.
    Content,
    /// `Err(..)`, documented as reported to the model as the tool's result.
    Error,
}

pub async fn runner_plugin_denies_tool(
    cfg: &Config,
    provider: Provider,
    form: DenialForm,
) -> Verdict {
    let model = match cfg.model(provider) {
        Ok(model) => model,
        Err(error) => return fail("model setup", error),
    };
    let deletes = CountingTool::new(
        "delete_records",
        "Permanently deletes every record of a customer.",
        json!({
            "type": "object",
            "properties": { "customer_id": { "type": "string" } },
            "required": ["customer_id"]
        }),
        |_, _| Ok(json!({ "deleted": 17 })),
    );
    let agent = match LlmAgentBuilder::new("records_admin")
        .model(model as Arc<dyn Llm>)
        .instruction(
            "You manage customer records. When asked to delete records, call delete_records exactly \
             once with the customer id. If the tool result reports an error or a denial, do not \
             retry; tell the user exactly what the tool said.",
        )
        .tool(deletes.clone() as Arc<dyn Tool>)
        .build()
    {
        Ok(agent) => agent,
        Err(error) => return fail("agent setup", error),
    };

    let hook_calls = Arc::new(AtomicUsize::new(0));
    let hook_seen = Arc::clone(&hook_calls);
    let plugin = Plugin::new(PluginConfig {
        name: "deny-deletes".to_string(),
        before_tool: Some(Box::new(move |ctx: Arc<dyn CallbackContext>| {
            let hook_seen = Arc::clone(&hook_seen);
            Box::pin(async move {
                if ctx.tool_name() != Some("delete_records") {
                    return Ok(None);
                }
                hook_seen.fetch_add(1, Ordering::SeqCst);
                match form {
                    DenialForm::Content => Ok(Some(Content::new("tool").with_text(DENIAL))),
                    DenialForm::Error => Err(AdkError::tool(DENIAL)),
                }
            })
        })),
        ..Default::default()
    });

    let app = "autonomy-plugin";
    let session_id = "plugin-deny";
    let (runner, sessions) =
        match runner_for(app, session_id, Arc::new(agent), Some(PluginManager::new(vec![plugin])))
            .await
        {
            Ok(pair) => pair,
            Err(error) => return fail("runner setup", error),
        };

    let turn = run_turn(&runner, USER, session_id, "Delete all records for customer C-17.").await;
    let executions = deletes.executions();
    if executions != 0 {
        return Verdict::Fail(format!("delete_records executed {executions}x despite the denial"));
    }
    if hook_calls.load(Ordering::SeqCst) == 0 {
        if let Some(error) = &turn.error {
            return Verdict::Fail(format!("turn failed before any tool call: {}", brief(error)));
        }
        return Verdict::Retry("model never called delete_records".to_string());
    }

    let events = match session_events(sessions.as_ref(), app, USER, session_id).await {
        Ok(events) => events,
        Err(error) => return fail("session read", error),
    };
    let (pairings, problems) = pair_calls(&events);
    let delete_calls: Vec<_> =
        pairings.iter().filter(|pairing| pairing.name == "delete_records").collect();
    let denial_responses = delete_calls
        .iter()
        .flat_map(|pairing| pairing.responses.iter())
        .filter(|response| response.to_string().contains("DENIED_BY_POLICY"))
        .count();

    let mut issues = Vec::new();
    if let Some(error) = &turn.error {
        issues.push(format!("turn error: {}", brief(error)));
    }
    if denial_responses == 0 {
        issues.push(format!(
            "no function_response carries the denial ({} delete_records call(s), responses {:?})",
            delete_calls.len(),
            delete_calls.iter().map(|pairing| pairing.responses.len()).collect::<Vec<_>>()
        ));
    }
    if !problems.is_empty() {
        issues.push(format!("history pairing: {}", problems.join("; ")));
    }
    if issues.is_empty() {
        Verdict::Pass(format!(
            "delete_records executed 0x; hook ran {}x; denial returned as the call's function_response and the turn completed",
            hook_calls.load(Ordering::SeqCst)
        ))
    } else {
        Verdict::Fail(format!("tool executed 0x (fail-closed) but {}", issues.join("; ")))
    }
}

// ─── path_guardrail_fail_closed ─────────────────────────────────────────────

pub async fn path_guardrail_fail_closed(cfg: &Config, provider: Provider) -> Verdict {
    let model = match cfg.model(provider) {
        Ok(model) => model,
        Err(error) => return fail("model setup", error),
    };
    let workspace = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(error) => return fail("tempdir", error),
    };
    let root = match std::fs::canonicalize(workspace.path()) {
        Ok(root) => root,
        Err(error) => return fail("canonicalize", error),
    };
    let notes = root.join("notes.txt");
    if let Err(error) = std::fs::write(&notes, "release checklist: ship on friday") {
        return fail("write notes", error);
    }

    let read_file_contents = |path: &str| -> AdkResult<Value> {
        std::fs::read_to_string(path)
            .map(|text| json!({ "content": text }))
            .map_err(|error| AdkError::tool(format!("read failed: {error}")))
    };
    let read_file = CountingTool::new(
        "read_file",
        "Reads a text file and returns its content.",
        json!({
            "type": "object",
            "properties": { "path": { "type": "string", "description": "Absolute file path" } },
            "required": ["path"]
        }),
        move |_, args| read_file_contents(args["path"].as_str().unwrap_or_default()),
    );
    let open_document = CountingTool::new(
        "open_document",
        "Opens a document and returns its content.",
        json!({
            "type": "object",
            "properties": { "document": { "type": "string", "description": "Absolute file path" } },
            "required": ["document"]
        }),
        move |_, args| read_file_contents(args["document"].as_str().unwrap_or_default()),
    );
    let guardrails = ToolGuardrailSet::new().with(
        PathAllowList::new("workspace-only", ["path"], [root.clone()])
            .on_tools(["read_file", "open_document"]),
    );
    let agent = match LlmAgentBuilder::new("file_reader")
        .model(model as Arc<dyn Llm>)
        .instruction(
            "You are a file assistant. Make exactly the tool calls the user lists, one at a time, \
             in order, even if an earlier call is denied or fails. Then report each tool result \
             verbatim.",
        )
        .tool(read_file.clone() as Arc<dyn Tool>)
        .tool(open_document.clone() as Arc<dyn Tool>)
        .tool_guardrails(guardrails)
        .build()
    {
        Ok(agent) => agent,
        Err(error) => return fail("agent setup", error),
    };
    let app = "autonomy-guardrail";
    let session_id = "guardrail";
    let (runner, sessions) = match runner_for(app, session_id, Arc::new(agent), None).await {
        Ok(pair) => pair,
        Err(error) => return fail("runner setup", error),
    };

    let prompt = format!(
        "Make these three tool calls one at a time, in this order:\n\
         1. open_document with document \"/etc/hosts\"\n\
         2. read_file with path \"/etc/hosts\"\n\
         3. read_file with path \"{}\"\n\
         Then report each tool result verbatim.",
        notes.display()
    );
    let turn = run_turn(&runner, USER, session_id, &prompt).await;
    if let Some(error) = &turn.error {
        return Verdict::Fail(format!("turn failed: {}", brief(error)));
    }

    // The tool counters are the ground truth: nothing outside the root may execute.
    let outside: Vec<Value> = read_file
        .seen()
        .into_iter()
        .filter(|args| {
            !args["path"].as_str().unwrap_or_default().starts_with(&*root.to_string_lossy())
        })
        .collect();
    if open_document.executions() != 0 || !outside.is_empty() {
        return Verdict::Fail(format!(
            "executed outside the allow list: open_document={}x, read_file outside root {:?}",
            open_document.executions(),
            outside
        ));
    }

    let events = match session_events(sessions.as_ref(), app, USER, session_id).await {
        Ok(events) => events,
        Err(error) => return fail("session read", error),
    };
    let (pairings, problems) = pair_calls(&events);
    if !problems.is_empty() {
        return Verdict::Fail(format!("history pairing: {}", problems.join("; ")));
    }
    let denials = |name: &str, needle: &str| {
        pairings
            .iter()
            .filter(|pairing| pairing.name == name)
            .flat_map(|pairing| pairing.responses.iter())
            .filter(|response| response.to_string().contains(needle))
            .count()
    };
    let missing_denials = denials("open_document", "is missing");
    let outside_denials = denials("read_file", "not an absolute path inside an allowed root");
    let attempted_open = pairings.iter().any(|pairing| pairing.name == "open_document");
    if !attempted_open {
        return Verdict::Retry("model never called open_document".to_string());
    }
    if missing_denials == 0 {
        return Verdict::Fail(
            "open_document call was not denied for its missing `path`".to_string(),
        );
    }
    if read_file.executions() != 1 {
        return Verdict::Retry(format!(
            "expected one permitted read_file execution, got {} (args {:?})",
            read_file.executions(),
            read_file.seen()
        ));
    }
    Verdict::Pass(format!(
        "open_document executed 0x (denied {missing_denials}x: missing `path`); /etc/hosts read denied {outside_denials}x; permitted read executed 1x"
    ))
}

// ─── failing_toolset_skipped ────────────────────────────────────────────────

struct UnreachableToolset;

#[async_trait]
impl Toolset for UnreachableToolset {
    fn name(&self) -> &str {
        "unreachable_mcp"
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> AdkResult<Vec<Arc<dyn Tool>>> {
        Err(AdkError::tool("mcp server unreachable: connection refused (simulated)"))
    }
}

async fn weather_turn(
    cfg: &Config,
    provider: Provider,
    strict: bool,
) -> anyhow::Result<(crate::common::Turn, Arc<CountingTool>, Arc<CountingLlm>)> {
    let model = cfg.model(provider)?;
    let weather = CountingTool::new(
        "get_weather",
        "Returns the current weather for a city.",
        json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"]
        }),
        |_, args| Ok(json!({ "city": args["city"], "conditions": "sunny", "temperature_c": 24 })),
    );
    let agent = LlmAgentBuilder::new("weather_agent")
        .model(Arc::clone(&model) as Arc<dyn Llm>)
        .instruction("Answer weather questions by calling get_weather, then reply in one sentence.")
        .tool(weather.clone() as Arc<dyn Tool>)
        .toolset(Arc::new(UnreachableToolset))
        .strict_toolsets(strict)
        .build()?;
    let app = "autonomy-toolset";
    let session_id = if strict { "toolset-strict" } else { "toolset-lenient" };
    let (runner, _) = runner_for(app, session_id, Arc::new(agent), None).await?;
    let turn =
        run_turn(&runner, USER, session_id, "What is the weather in Nairobi right now?").await;
    Ok((turn, weather, model))
}

pub async fn failing_toolset_skipped(cfg: &Config, provider: Provider) -> Verdict {
    let (lenient, weather, _) = match weather_turn(cfg, provider, false).await {
        Ok(result) => result,
        Err(error) => return fail("lenient setup", error),
    };
    if let Some(error) = &lenient.error {
        return Verdict::Fail(format!("default mode failed the turn: {}", brief(error)));
    }
    if weather.executions() == 0 {
        return Verdict::Retry("model never called get_weather".to_string());
    }

    let (strict, strict_weather, strict_model) = match weather_turn(cfg, provider, true).await {
        Ok(result) => result,
        Err(error) => return fail("strict setup", error),
    };
    let Some(error) = &strict.error else {
        return Verdict::Fail(
            "strict_toolsets(true) completed the turn instead of failing".to_string(),
        );
    };
    if !error.contains("unreachable") {
        return Verdict::Fail(format!(
            "strict mode failed with an unrelated error: {}",
            brief(error)
        ));
    }
    if strict_weather.executions() != 0 || strict_model.calls() != 0 {
        return Verdict::Fail(format!(
            "strict mode still ran: get_weather={}x model calls={}",
            strict_weather.executions(),
            strict_model.calls()
        ));
    }
    Verdict::Pass(format!(
        "default: turn ok, get_weather={}x; strict: turn failed before any model call ({})",
        weather.executions(),
        brief(error).chars().take(70).collect::<String>()
    ))
}
