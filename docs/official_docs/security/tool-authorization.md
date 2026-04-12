# Tool Authorization

Control which tools an agent can execute and when human approval is required. ADK-Rust provides four mechanisms — from simple per-tool confirmation to full RBAC — that work across the CLI, web server, and A2A protocol.

## Quick Comparison

| Mechanism | Use Case | Granularity | Runtime |
|-----------|----------|-------------|---------|
| [Tool Confirmation Policy](#tool-confirmation-policy) | Interactive approval in CLI/web | Per-tool or all tools | Pauses execution, emits event |
| [BeforeToolCallback](#beforetoolcallback) | Programmatic gate / audit | Custom logic per call | Sync decision, no pause |
| [Access Control (RBAC)](#access-control) | Role-based enterprise security | Per-user, per-tool | Deny before execution |
| [Graph Interrupts](#graph-interrupts) | Complex approval workflows | Per-node checkpoint | Persists state, resumes later |

## Tool Confirmation Policy

The built-in human-in-the-loop mechanism. When a tool requiring confirmation is called, the agent pauses, emits a `ToolConfirmationRequest` event, and waits for an `Approve` or `Deny` decision on the next run.

### Setup

```rust
use adk_agent::LlmAgentBuilder;
use std::sync::Arc;

let agent = LlmAgentBuilder::new("assistant")
    .model(model)
    .instruction("You are a helpful assistant with file and email tools.")
    .tool(Arc::new(search_tool))
    .tool(Arc::new(delete_file_tool))
    .tool(Arc::new(send_email_tool))
    // Require confirmation for dangerous tools
    .require_tool_confirmation("delete_file")
    .require_tool_confirmation("send_email")
    .build()?;

// Or require confirmation for ALL tool calls:
// .require_tool_confirmation_for_all()
```

### How It Works

1. The LLM decides to call `delete_file` with args `{"path": "/data/report.csv"}`
2. The agent emits an `Event` with:
   ```json
   {
     "actions": {
       "toolConfirmation": {
         "toolName": "delete_file",
         "functionCallId": "call_abc123",
         "args": {"path": "/data/report.csv"}
       }
     }
   }
   ```
3. The agent stream ends — execution is paused
4. Your UI shows the user: "The agent wants to delete `/data/report.csv`. Allow?"
5. On the next `Runner::run()`, pass the decision:

```rust
use adk_core::{RunConfig, ToolConfirmationDecision};
use std::collections::HashMap;

let mut decisions = HashMap::new();
decisions.insert(
    "delete_file".to_string(),
    ToolConfirmationDecision::Approve, // or Deny
);

// The runner picks up the decision and continues
```

If denied, the tool is skipped and the LLM receives a message like "Tool execution was denied by the user" so it can adjust its approach.

### CLI Example

A terminal agent that asks for confirmation before running tools:

```rust
use adk_agent::LlmAgentBuilder;
use adk_core::{
    Content, Event, RunConfig, ToolConfirmationDecision,
    SessionId, UserId,
};
use adk_runner::Runner;
use adk_session::InMemorySessionService;
use adk_model::GeminiModel;
use adk_tool::tool;
use futures::StreamExt;
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::Arc;

#[derive(Deserialize, JsonSchema)]
struct DeleteArgs {
    /// File path to delete
    path: String,
}

/// Delete a file from the filesystem.
#[tool]
async fn delete_file(args: DeleteArgs) -> Result<serde_json::Value, adk_core::AdkError> {
    // In production, actually delete the file
    Ok(serde_json::json!({"deleted": args.path}))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let agent = LlmAgentBuilder::new("file-manager")
        .model(Arc::new(model))
        .instruction("You help manage files. Use delete_file when asked to remove files.")
        .tool(Arc::new(DeleteFile))
        .require_tool_confirmation("delete_file")
        .build()?;

    let session_service = Arc::new(InMemorySessionService::new());
    let runner = Runner::new(adk_runner::RunnerConfig {
        app_name: "file-manager".to_string(),
        agent: Arc::new(agent),
        session_service: session_service.clone(),
        ..Default::default()
    })?;

    let user_id = UserId::new("user-1")?;
    let session_id = SessionId::new("session-1")?;

    // Create session
    session_service.create(adk_session::CreateRequest {
        app_name: "file-manager".to_string(),
        user_id: "user-1".to_string(),
        session_id: Some("session-1".to_string()),
        state: HashMap::new(),
    }).await?;

    println!("File Manager (type 'quit' to exit)");
    loop {
        print!("> ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();
        if input == "quit" { break; }

        let content = Content::new("user").with_text(input);
        let mut stream = runner.run(
            user_id.clone(), session_id.clone(), content,
        ).await?;

        while let Some(result) = stream.next().await {
            let event = result?;

            // Check if the agent is requesting tool confirmation
            if let Some(ref confirmation) = event.actions.tool_confirmation {
                println!(
                    "\n⚠️  The agent wants to run '{}' with args: {}",
                    confirmation.tool_name,
                    serde_json::to_string_pretty(&confirmation.args)?
                );
                print!("Allow? [y/n]: ");
                io::stdout().flush()?;

                let mut answer = String::new();
                io::stdin().read_line(&mut answer)?;

                let decision = if answer.trim().eq_ignore_ascii_case("y") {
                    ToolConfirmationDecision::Approve
                } else {
                    ToolConfirmationDecision::Deny
                };

                // Re-run with the decision
                let mut decisions = HashMap::new();
                decisions.insert(confirmation.tool_name.clone(), decision);

                let content = Content::new("user").with_text("");
                let mut resume_stream = runner.run(
                    user_id.clone(), session_id.clone(), content,
                ).await?;

                while let Some(result) = resume_stream.next().await {
                    let event = result?;
                    if let Some(ref content) = event.llm_response.content {
                        for part in &content.parts {
                            if let Some(text) = part.text() {
                                print!("{text}");
                            }
                        }
                    }
                }
                println!();
            } else if let Some(ref content) = event.llm_response.content {
                for part in &content.parts {
                    if let Some(text) = part.text() {
                        print!("{text}");
                    }
                }
            }
        }
        println!();
    }
    Ok(())
}
```

### Web Server Example

An SSE endpoint that streams events to the frontend. When a `toolConfirmation` event arrives, the frontend renders an approval dialog and sends the decision back:

```rust
use adk_agent::LlmAgentBuilder;
use adk_core::{
    Content, RunConfig, ToolConfirmationDecision, SessionId, UserId,
};
use adk_runner::Runner;
use adk_session::InMemorySessionService;
use axum::{Json, Router, extract::State, response::sse::{Event, Sse}};
use axum::routing::post;
use futures::StreamExt;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
struct AppState {
    runner: Arc<Runner>,
}

#[derive(Deserialize)]
struct ChatRequest {
    message: String,
    user_id: String,
    session_id: String,
    /// Tool confirmation decisions from the previous turn
    #[serde(default)]
    tool_decisions: HashMap<String, String>, // "tool_name" -> "approve"|"deny"
}

async fn chat_handler(
    State(state): State<AppState>,
    Json(req): Json<ChatRequest>,
) -> Sse<impl futures::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let runner = state.runner.clone();
    let user_id = UserId::new(&req.user_id).unwrap();
    let session_id = SessionId::new(&req.session_id).unwrap();
    let content = Content::new("user").with_text(&req.message);

    let stream = async_stream::stream! {
        let mut event_stream = match runner.run(user_id, session_id, content).await {
            Ok(s) => s,
            Err(e) => {
                yield Ok(Event::default().data(
                    serde_json::json!({"error": e.to_string()}).to_string()
                ));
                return;
            }
        };

        while let Some(result) = event_stream.next().await {
            match result {
                Ok(event) => {
                    // Emit tool confirmation request to frontend
                    if let Some(ref confirmation) = event.actions.tool_confirmation {
                        yield Ok(Event::default()
                            .event("tool_confirmation")
                            .data(serde_json::json!({
                                "toolName": confirmation.tool_name,
                                "args": confirmation.args,
                                "functionCallId": confirmation.function_call_id,
                            }).to_string()));
                    }

                    // Emit text content
                    if let Some(ref content) = event.llm_response.content {
                        for part in &content.parts {
                            if let Some(text) = part.text() {
                                yield Ok(Event::default()
                                    .event("text")
                                    .data(serde_json::json!({"text": text}).to_string()));
                            }
                        }
                    }
                }
                Err(e) => {
                    yield Ok(Event::default().data(
                        serde_json::json!({"error": e.to_string()}).to_string()
                    ));
                }
            }
        }

        yield Ok(Event::default().event("done").data("{}".to_string()));
    };

    Sse::new(stream)
}

// Frontend JavaScript (conceptual):
//
// const source = new EventSource('/api/chat');
// source.addEventListener('tool_confirmation', (e) => {
//   const data = JSON.parse(e.data);
//   showConfirmDialog(data.toolName, data.args, (approved) => {
//     fetch('/api/chat', {
//       method: 'POST',
//       body: JSON.stringify({
//         message: '',
//         tool_decisions: { [data.toolName]: approved ? 'approve' : 'deny' }
//       })
//     });
//   });
// });
```

## BeforeToolCallback

For programmatic authorization — check permissions, call an external auth service, or log for audit. No user interaction needed.

```rust
use adk_agent::LlmAgentBuilder;
use adk_core::{BeforeToolCallback, CallbackContext, Content};
use std::sync::Arc;

let agent = LlmAgentBuilder::new("assistant")
    .model(model)
    .tool(Arc::new(my_tool))
    .before_tool_callback(Box::new(|ctx: Arc<dyn CallbackContext>| {
        Box::pin(async move {
            let tool_name = ctx.tool_name().unwrap_or("unknown");
            let tool_input = ctx.tool_input();

            // Log for audit
            tracing::info!(tool = tool_name, "tool execution requested");

            // Custom authorization logic
            let user_scopes = ctx.user_scopes();
            if tool_name == "admin_action" && !user_scopes.contains(&"admin".to_string()) {
                // Return Some(Content) to skip the tool
                return Ok(Some(
                    Content::new("tool")
                        .with_text("Permission denied: admin scope required")
                ));
            }

            Ok(None) // Allow execution
        })
    }))
    .build()?;
```

Return values:
- `Ok(None)` — allow the tool to execute
- `Ok(Some(content))` — skip the tool, send this content to the LLM instead
- `Err(e)` — abort the entire agent execution

## Access Control

For enterprise RBAC with role-based permissions. See [Access Control](access-control.md) for full documentation.

```rust
use adk_auth::{AccessControl, Role, Permission, ToolExt};

let ac = AccessControl::builder()
    .role(Role::new("analyst")
        .allow(Permission::Tool("search".into()))
        .allow(Permission::Tool("summarize".into()))
        .deny(Permission::Tool("delete_file".into())))
    .role(Role::new("admin")
        .allow(Permission::AllTools))
    .assign("alice@co.com", "admin")
    .assign("bob@co.com", "analyst")
    .build()?;

// Wrap tools with automatic permission checking
let protected_tool = my_tool.with_access_control(Arc::new(ac));
```

## Graph Interrupts

For complex approval workflows where execution needs to persist state and resume later. See [Graph Agents](../agents/graph-agents.md) for full documentation.

Graph agents support checkpoint-based interrupts where execution pauses at a node, persists state to a checkpoint store, and resumes after human input — even across server restarts.

## Combining Mechanisms

These mechanisms compose naturally:

```rust
let agent = LlmAgentBuilder::new("secure-assistant")
    .model(model)
    // RBAC: deny unauthorized users entirely
    .tool(Arc::new(search_tool.with_access_control(Arc::new(ac))))
    // Callback: audit all tool calls
    .before_tool_callback(audit_callback())
    // Confirmation: require human approval for destructive ops
    .require_tool_confirmation("delete_file")
    .require_tool_confirmation("send_email")
    .build()?;
```

Order of evaluation:
1. RBAC check (if `ProtectedTool` wrapper is used) — denies unauthorized users
2. `BeforeToolCallback` — programmatic gate, can skip or abort
3. `ToolConfirmationPolicy` — pauses for human approval if required
4. Tool executes
5. `AfterToolCallback` / `AfterToolCallbackFull` — post-execution inspection

## Related

- [Access Control](access-control.md) — RBAC, SSO, audit logging
- [Callbacks](../callbacks/callbacks.md) — All callback types and lifecycle
- [Graph Agents](../agents/graph-agents.md) — Checkpoint-based interrupts
- [Guardrails](guardrails.md) — Input/output validation

---

**Previous**: [← Access Control](access-control.md) | **Next**: [Guardrails →](guardrails.md)
