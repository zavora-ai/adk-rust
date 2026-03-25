//! Anthropic native tools — live agentic example matrix.
//!
//! Demonstrates every Anthropic native tool wrapper currently exposed by
//! ADK-Rust through the pinned `claudius` surface. The examples are agentic:
//! web search runs alongside a function tool, bash is exercised through the
//! tool loop, and text-editor scenarios are multi-turn file editing workflows.
//!
//! Scenarios:
//!   1. Web search + function tool coexistence
//!   2. Bash 20241022 + function tool coexistence
//!   3. Bash 20250124 + function tool coexistence
//!   4. Text editor 20250124 multi-turn workflow
//!   5. Text editor 20250429 multi-turn workflow
//!   6. Text editor 20250728 multi-turn workflow
//!
//! # Usage
//!
//! ```bash
//! export ANTHROPIC_API_KEY=sk-ant-your-key-here
//! cargo run --manifest-path examples/anthropic_server_tools/Cargo.toml
//! ```

use adk_core::{Part, SessionId, UserId};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_rust::futures::StreamExt;
use adk_rust::prelude::*;
use adk_rust::session::{CreateRequest, SessionService};
use adk_tool::{
    AnthropicBashTool20241022, AnthropicBashTool20250124, AnthropicTextEditorTool20250124,
    AnthropicTextEditorTool20250429, AnthropicTextEditorTool20250728, WebSearchTool,
    WebSearchUserLocation,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const APP_NAME: &str = "anthropic-server-tools-example";
const MODEL_NAME: &str = "claude-sonnet-4-20250514";

#[derive(Debug)]
enum ScenarioOutcome {
    Passed,
    Skipped(String),
}

#[derive(Debug, Default)]
struct StreamSummary {
    saw_text: bool,
    function_calls: Vec<String>,
    function_responses: usize,
    server_tool_calls: Vec<String>,
    server_tool_responses: Vec<String>,
    text: String,
}

#[derive(schemars::JsonSchema, serde::Serialize)]
struct ToolStatusArgs {
    tool_name: String,
    note: String,
}

type ScenarioFuture = Pin<Box<dyn Future<Output = anyhow::Result<ScenarioOutcome>> + Send>>;
type ScenarioFn = fn() -> ScenarioFuture;

fn load_dotenv() {
    let mut dir = std::env::current_dir().ok();
    while let Some(d) = dir {
        let path = d.join(".env");
        if path.is_file() {
            let _ = dotenvy::from_path(path);
            return;
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
}

fn separator(title: &str) {
    println!("\n{}", "=".repeat(60));
    println!("  {title}");
    println!("{}\n", "=".repeat(60));
}

fn api_key() -> String {
    std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY must be set")
}

fn model_name() -> String {
    std::env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| MODEL_NAME.to_string())
}

fn skip_known_anthropic_incompatibility(
    error: &anyhow::Error,
    tool_name: &str,
) -> Option<String> {
    let message = format!("{error:#}");
    if tool_name == "bash_20241022"
        && (message.contains("bash_20241022")
            || message.contains("expected one of")
            || message.contains("invalid value"))
    {
        return Some(
            "bash_20241022 is modeled by the pinned SDK but no longer accepted by the live Anthropic API"
                .to_string(),
        );
    }
    None
}

fn skipped(message: impl Into<String>) -> anyhow::Result<ScenarioOutcome> {
    let message = message.into();
    println!("↷ skipped: {message}");
    Ok(ScenarioOutcome::Skipped(message))
}

fn saw_function(summary: &StreamSummary, name: &str) -> bool {
    summary.function_calls.iter().any(|call| call == name)
}

fn server_item_kind(value: &Value) -> String {
    if value.get("query").is_some() || value.get("input").is_some() {
        value.get("name").and_then(Value::as_str).unwrap_or("server_tool_use").to_string()
    } else {
        value.get("type").and_then(Value::as_str).unwrap_or("server_tool_result").to_string()
    }
}

async fn record_tool_status(
    _ctx: Arc<dyn ToolContext>,
    args: serde_json::Value,
) -> Result<serde_json::Value> {
    Ok(json!({
        "acknowledged": true,
        "tool_name": args["tool_name"].as_str().unwrap_or("unknown"),
        "note": args["note"].as_str().unwrap_or(""),
    }))
}

fn note_tool() -> Arc<dyn Tool> {
    Arc::new(
        FunctionTool::new(
            "record_tool_status",
            "Records which native tool the agent used and why.",
            record_tool_status,
        )
        .with_parameters_schema::<ToolStatusArgs>(),
    )
}

async fn make_runner(agent: Arc<dyn Agent>, session_id: &str) -> anyhow::Result<Runner> {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP_NAME.into(),
            user_id: "user".into(),
            session_id: Some(session_id.into()),
            state: HashMap::new(),
        })
        .await?;

    Ok(Runner::new(RunnerConfig {
        app_name: APP_NAME.into(),
        agent,
        session_service: sessions,
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
        request_context: None,
        cancellation_token: None,
    })?)
}

async fn build_runner(
    agent_name: &str,
    session_id: &str,
    instruction: &str,
    tools: Vec<Arc<dyn Tool>>,
) -> anyhow::Result<Runner> {
    let model = Arc::new(AnthropicClient::new(AnthropicConfig::new(api_key(), model_name()))?);

    let mut builder = LlmAgentBuilder::new(agent_name).instruction(instruction).model(model);
    for tool in tools {
        builder = builder.tool(tool);
    }
    let agent = Arc::new(builder.build()?);
    make_runner(agent, session_id).await
}

async fn run_turn(
    runner: &Runner,
    session_id: &str,
    prompt: &str,
) -> anyhow::Result<StreamSummary> {
    let mut stream = runner
        .run(
            UserId::new("user")?,
            SessionId::new(session_id)?,
            Content::new("user").with_text(prompt),
        )
        .await?;

    let mut summary = StreamSummary::default();
    while let Some(event) = stream.next().await {
        let event = event?;
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::ServerToolCall { server_tool_call } => {
                        let kind = server_item_kind(server_tool_call);
                        println!("  → ServerToolCall: {kind}");
                        summary.server_tool_calls.push(kind);
                    }
                    Part::ServerToolResponse { server_tool_response } => {
                        let kind = server_item_kind(server_tool_response);
                        println!(
                            "  ← ServerToolResponse: {kind} [{}B]",
                            server_tool_response.to_string().len()
                        );
                        summary.server_tool_responses.push(kind);
                    }
                    Part::FunctionCall { name, args, .. } => {
                        println!("  → FunctionCall: {name}({args})");
                        summary.function_calls.push(name.clone());
                    }
                    Part::FunctionResponse { function_response, .. } => {
                        println!("  ← FunctionResponse: {}", function_response.response);
                        summary.function_responses += 1;
                    }
                    Part::Text { text } if !text.trim().is_empty() => {
                        print!("{text}");
                        summary.text.push_str(text);
                        summary.saw_text = true;
                    }
                    Part::Thinking { .. } => println!("  💭 reasoning"),
                    _ => {}
                }
            }
        }
    }
    if summary.saw_text {
        println!();
    }

    Ok(summary)
}

fn temp_fixture_path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let pid = std::process::id();
    std::env::temp_dir().join(format!("adk_rust_{label}_{pid}_{nanos}"))
}

fn prepare_editor_fixture(label: &str) -> anyhow::Result<PathBuf> {
    let dir = temp_fixture_path(label);
    std::fs::create_dir_all(&dir)?;
    let file = dir.join("notes.txt");
    std::fs::write(&file, "TODO: inspect this file\nTODO: replace this line\nstatus: pending\n")?;
    Ok(file)
}

async fn test_web_search_plus_function_tool() -> anyhow::Result<ScenarioOutcome> {
    separator("1. Web search + function tool coexistence");

    let runner = build_runner(
        "anthropic-web-search",
        "anthropic-web-search",
        "You are a research agent. Use Anthropic web search for current information. After you finish searching, call record_tool_status with the tool name and a short note.",
        vec![
            Arc::new(
                WebSearchTool::new()
                    .with_max_uses(2)
                    .with_user_location(
                        WebSearchUserLocation::new()
                            .with_city("San Francisco")
                            .with_country("US")
                            .with_timezone("America/Los_Angeles"),
                    ),
            ),
            note_tool(),
        ],
    )
    .await?;

    let summary = run_turn(
        &runner,
        "anthropic-web-search",
        "Use web search to find the latest stable Rust release and summarize it. After that, call record_tool_status with tool_name 'web_search'.",
    )
    .await?;

    assert!(
        summary.server_tool_calls.iter().any(|kind| kind.contains("web_search")),
        "expected a web search server tool call"
    );
    Ok(ScenarioOutcome::Passed)
}

async fn run_bash_scenario(
    title: &str,
    session_id: &str,
    bash_tool: Arc<dyn Tool>,
    expected_marker: &str,
) -> anyhow::Result<ScenarioOutcome> {
    separator(title);

    let runner = build_runner(
        session_id,
        session_id,
        "You can use bash and a record_tool_status function. Use bash when asked to inspect the environment, then call record_tool_status.",
        vec![bash_tool, note_tool()],
    )
    .await?;

    let prompt = format!(
        "Use the bash tool to run `printf '{expected_marker}'`. After that, call record_tool_status with tool_name 'bash' and note '{expected_marker}'."
    );
    let summary = run_turn(&runner, session_id, &prompt).await?;

    assert!(saw_function(&summary, "bash"), "expected the bash tool to be called");
    assert!(
        summary.function_responses > 0 || summary.saw_text,
        "expected bash output or final text"
    );
    Ok(ScenarioOutcome::Passed)
}

async fn run_text_editor_scenario(
    title: &str,
    session_id: &str,
    editor_tool: Arc<dyn Tool>,
    editor_name: &str,
) -> anyhow::Result<ScenarioOutcome> {
    separator(title);

    let file = prepare_editor_fixture(session_id)?;
    let file_display = file.display().to_string();

    let runner = build_runner(
        session_id,
        session_id,
        "You are a coding agent with access to a text editor tool and a record_tool_status helper. Follow the user's editing instructions exactly and keep state across turns.",
        vec![editor_tool, note_tool()],
    )
    .await?;

    let prompt_one = format!(
        "Use the text editor tool to view the file at {file_display}, replace every occurrence of TODO with DONE, and then call record_tool_status with tool_name 'text_editor' and the first line of the edited file."
    );
    let prompt_two = format!(
        "Now use the text editor tool to insert the line `summary: reviewed by anthropic agent` at line 1 in {file_display}. Then call record_tool_status with tool_name 'text_editor' and note 'inserted summary line'."
    );

    let first = run_turn(&runner, session_id, &prompt_one).await?;
    let second = run_turn(&runner, session_id, &prompt_two).await?;

    assert!(
        saw_function(&first, editor_name) || saw_function(&second, editor_name),
        "expected the text editor tool to be called"
    );

    let content = std::fs::read_to_string(&file)?;
    assert!(
        content.contains("DONE: inspect this file") && content.contains("DONE: replace this line"),
        "expected TODO markers to be replaced in the file"
    );
    assert!(
        content.lines().next() == Some("summary: reviewed by anthropic agent"),
        "expected the inserted summary line at the top of the file"
    );
    Ok(ScenarioOutcome::Passed)
}

async fn test_bash_20241022_plus_function_tool() -> anyhow::Result<ScenarioOutcome> {
    if std::env::var("ANTHROPIC_RUN_LEGACY_BASH_20241022").ok().as_deref() != Some("1") {
        return skipped(
            "bash_20241022 is legacy; set ANTHROPIC_RUN_LEGACY_BASH_20241022=1 to probe live compatibility",
        );
    }

    run_bash_scenario(
        "2. Bash 20241022 + function tool coexistence",
        "anthropic-bash-20241022",
        Arc::new(AnthropicBashTool20241022::new()),
        "anthropic-bash-20241022",
    )
    .await
}

async fn test_bash_20250124_plus_function_tool() -> anyhow::Result<ScenarioOutcome> {
    run_bash_scenario(
        "3. Bash 20250124 + function tool coexistence",
        "anthropic-bash-20250124",
        Arc::new(AnthropicBashTool20250124::new()),
        "anthropic-bash-20250124",
    )
    .await
}

async fn test_text_editor_20250124_multi_turn() -> anyhow::Result<ScenarioOutcome> {
    run_text_editor_scenario(
        "4. Text editor 20250124 multi-turn workflow",
        "anthropic-text-editor-20250124",
        Arc::new(AnthropicTextEditorTool20250124::new()),
        "str_replace_editor",
    )
    .await
}

async fn test_text_editor_20250429_multi_turn() -> anyhow::Result<ScenarioOutcome> {
    run_text_editor_scenario(
        "5. Text editor 20250429 multi-turn workflow",
        "anthropic-text-editor-20250429",
        Arc::new(AnthropicTextEditorTool20250429::new().with_max_characters(4096)),
        "str_replace_based_edit_tool",
    )
    .await
}

async fn test_text_editor_20250728_multi_turn() -> anyhow::Result<ScenarioOutcome> {
    run_text_editor_scenario(
        "6. Text editor 20250728 multi-turn workflow",
        "anthropic-text-editor-20250728",
        Arc::new(AnthropicTextEditorTool20250728::new().with_max_characters(4096)),
        "str_replace_based_edit_tool",
    )
    .await
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_dotenv();

    println!("Anthropic Native Tools — Live Agentic Example Matrix");
    println!("====================================================");
    println!("Model: {}\n", model_name());

    let scenarios: Vec<(&str, ScenarioFn)> = vec![
        ("Web search + function tool", || Box::pin(test_web_search_plus_function_tool())),
        ("Bash 20241022 + function tool", || Box::pin(test_bash_20241022_plus_function_tool())),
        ("Bash 20250124 + function tool", || Box::pin(test_bash_20250124_plus_function_tool())),
        ("Text editor 20250124 multi-turn", || Box::pin(test_text_editor_20250124_multi_turn())),
        ("Text editor 20250429 multi-turn", || Box::pin(test_text_editor_20250429_multi_turn())),
        ("Text editor 20250728 multi-turn", || Box::pin(test_text_editor_20250728_multi_turn())),
    ];

    let mut passed_count = 0usize;
    let mut skipped_count = 0usize;
    let mut failed_count = 0usize;

    for (name, run) in scenarios {
        match run().await {
            Ok(ScenarioOutcome::Passed) => passed_count += 1,
            Ok(ScenarioOutcome::Skipped(reason)) => {
                println!("↷ {name}: {reason}");
                skipped_count += 1;
            }
            Err(error) => {
                if let Some(reason) = skip_known_anthropic_incompatibility(&error, name) {
                    println!("↷ {name}: {reason}");
                    skipped_count += 1;
                    continue;
                }
                eprintln!("\n✗ {name} FAILED: {error:#}");
                failed_count += 1;
            }
        }
    }

    separator("Summary");
    println!("  passed: {passed_count}");
    println!("  skipped: {skipped_count}");
    println!("  failed: {failed_count}");
    if failed_count > 0 {
        std::process::exit(1);
    }
    Ok(())
}
