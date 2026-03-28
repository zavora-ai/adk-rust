//! Reproduction for GitHub Issue #224 — using ADK runner to trace tool calls.
//!
//! Mirrors the gemini3_builtin_tools example pattern: built-in tool + function
//! tool coexistence, with full ServerToolCall/ServerToolResponse tracing.

use adk_core::{Part, SessionId, Tool, ToolContext, UserId};
use adk_model::GeminiModel;
use adk_runner::{Runner, RunnerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::Agent;

const APP_NAME: &str = "gemini-search-bug";
const MODEL_NAME: &str = "gemini-3-pro-preview";
const INSTRUCTIONS: &str =
    "You are a research agent. Use Google Search for current information, then call record_tool_status.";

fn gemini_server_tool_kind(value: &serde_json::Value) -> String {
    if value.get("toolCall").is_some() {
        "tool_call".to_string()
    } else if value.get("toolResponse").is_some() {
        "tool_response".to_string()
    } else if value.get("executableCode").is_some() {
        "executable_code".to_string()
    } else if value.get("codeExecutionResult").is_some() {
        "code_execution_result".to_string()
    } else {
        "unknown".to_string()
    }
}

fn server_tool_sig(val: &serde_json::Value) -> Option<String> {
    val.get("thoughtSignature")
        .and_then(|v| v.as_str())
        .or_else(|| {
            val.get("toolCall")
                .and_then(|tc| tc.get("_thought_signature"))
                .and_then(|v| v.as_str())
        })
        .map(String::from)
}

fn sig_display(sig: &Option<String>) -> String {
    match sig {
        Some(s) if s.len() > 50 => format!("{}…[{}B]", &s[..50], s.len()),
        Some(s) => s.clone(),
        None => "(none)".to_string(),
    }
}

/// Simple function tool that acknowledges it was called — proves coexistence.
fn note_tool() -> Arc<dyn Tool> {
    #[derive(Debug)]
    struct RecordToolStatus;

    #[async_trait::async_trait]
    impl Tool for RecordToolStatus {
        fn name(&self) -> &str {
            "record_tool_status"
        }
        fn description(&self) -> &str {
            "Record that a tool was used. Call this after using any built-in tool."
        }
        fn parameters_schema(&self) -> Option<Value> {
            Some(json!({
                "type": "object",
                "properties": {
                    "tool_name": { "type": "string", "description": "Name of the tool that was used" },
                    "note": { "type": "string", "description": "Short note about what the tool did" }
                },
                "required": ["tool_name", "note"]
            }))
        }
        async fn execute(
            &self,
            _ctx: Arc<dyn ToolContext>,
            args: Value,
        ) -> Result<Value, adk_core::AdkError> {
            let tool_name = args.get("tool_name").and_then(|v| v.as_str()).unwrap_or("unknown");
            let note = args.get("note").and_then(|v| v.as_str()).unwrap_or("");
            Ok(json!({
                "acknowledged": true,
                "tool_name": tool_name,
                "note": note,
            }))
        }
    }

    Arc::new(RecordToolStatus)
}

fn print_grounding(event: &adk_core::Event) {
    let Some(meta) = &event.llm_response.provider_metadata else { return };
    let obj = match meta.as_object() {
        Some(o) if !o.is_empty() => o,
        _ => return,
    };

    println!("\n  🌐 Grounding metadata:");
    if let Some(queries) = obj.get("webSearchQueries").and_then(|v| v.as_array()) {
        let qs: Vec<&str> = queries.iter().filter_map(|q| q.as_str()).collect();
        if !qs.is_empty() {
            println!("  🔍 Search queries: {}", qs.join(", "));
        }
    }
    if let Some(chunks) = obj.get("groundingChunks").and_then(|v| v.as_array()) {
        for (i, chunk) in chunks.iter().enumerate() {
            if let Some(web) = chunk.get("web") {
                let title = web.get("title").and_then(|v| v.as_str()).unwrap_or("?");
                let uri = web.get("uri").and_then(|v| v.as_str()).unwrap_or("?");
                println!("  📚 Source [{i}]: {title}");
                println!("     {uri}");
            }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY").expect("GOOGLE_API_KEY must be set");
    let model = Arc::new(GeminiModel::new(&api_key, MODEL_NAME)?);

    println!("Gemini Search Bug Reproduction");
    println!("==============================");
    println!("Model: {MODEL_NAME}\n");

    // Built-in tools + function tool coexistence (same pattern as gemini3_builtin_tools)
    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("search-bug-agent")
            .instruction(INSTRUCTIONS)
            .model(model)
            .tool(Arc::new(adk_tool::GoogleSearchTool::new()))
            .tool(Arc::new(adk_tool::UrlContextTool::new()))
            .tool(note_tool())
            .build()?,
    );

    // Set up session + runner
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP_NAME.into(),
            user_id: "user".into(),
            session_id: Some("session-1".into()),
            state: HashMap::new(),
        })
        .await?;

    let runner = Runner::new(RunnerConfig {
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
    })?;

    // Run the agent
    let mut stream = runner
        .run(
            UserId::new("user")?,
            SessionId::new("session-1")?,
            adk_core::Content::new("user")
                .with_text("Use Google Search to find the latest technology news today. After that, call record_tool_status with tool_name 'google_search' and a short note about what you found."),
        )
        .await?;

    let mut full_text = String::new();
    while let Some(event) = stream.next().await {
        let event = event?;

        // Print grounding metadata if present
        if event.llm_response.provider_metadata.is_some() {
            print_grounding(&event);
        }

        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                match part {
                    Part::ServerToolCall { server_tool_call } => {
                        let kind = gemini_server_tool_kind(server_tool_call);
                        println!("  → ServerToolCall: {kind}");
                        println!(
                            "    thought_signature: {}",
                            sig_display(&server_tool_sig(server_tool_call))
                        );
                    }
                    Part::ServerToolResponse { server_tool_response } => {
                        let kind = gemini_server_tool_kind(server_tool_response);
                        println!(
                            "  ← ServerToolResponse: {kind} [{}B]",
                            server_tool_response.to_string().len()
                        );
                    }
                    Part::FunctionCall { name, args, thought_signature, .. } => {
                        println!("  → FunctionCall: {name}({args})");
                        println!("    thought_signature: {}", sig_display(thought_signature));
                    }
                    Part::FunctionResponse { function_response, .. } => {
                        println!("  ← FunctionResponse: {}", function_response.response);
                    }
                    Part::Thinking { .. } => {
                        println!("  💭 Thinking...");
                    }
                    Part::Text { text } if !text.trim().is_empty() => {
                        print!("{text}");
                        full_text.push_str(text);
                    }
                    _ => {}
                }
            }
        }
    }

    if !full_text.is_empty() {
        println!();
    }
    println!("\n✅ Done — {} chars of text output", full_text.len());

    Ok(())
}
