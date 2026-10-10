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

The built-in human-in-the-loop mechanism. When a tool requiring confirmation is called, the agent asks the run's `ToolConfirmationHandler` and waits for its `Approve` or `Deny`. Without a handler, it pauses, emits a `ToolConfirmationRequest` event, and ends the run.

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
5. On the next `Runner::run()`, pass the decision **keyed by the function call ID**
   from the request:

```rust
use adk_core::{Content, RunConfig, ToolConfirmationDecision};
use std::collections::HashMap;

let mut decisions = HashMap::new();
decisions.insert(
    "call_abc123".to_string(), // functionCallId from the request, not the tool name
    ToolConfirmationDecision::Approve, // or Deny
);

let config = RunConfig::builder().tool_confirmation_decisions(decisions).build();
let stream = runner
    .run_with_config(user_id, session_id, Content::new("user").with_text("approved"), Some(config))
    .await?;
```

If denied, the tool is skipped and the LLM receives an error result —
"Tool 'delete_file' execution denied by confirmation policy" — so it can adjust its approach.

> **Note:** A static decision applies only to the call ID it names. A run that asks the
> model again receives a new function call with a new ID, so a decision carried into
> the next run applies only when that run dispatches the same call. For interactive
> approval, configure a [`ToolConfirmationHandler`](#cli-example) instead: the agent
> asks it about each call while the run that made the call waits, so no second run is
> needed.

### Decisions Authorize One Exact Call

A decision applies to the single call it was requested for. Keying by tool name
would make one approval authorize every call of that tool, so an approval for
`delete_file` on a scratch path would also authorize a call targeting something
else. Two calls to the same tool in one turn therefore need two decisions.

An unknown call ID means "no decision", which leaves the call awaiting
confirmation. The failure direction is always toward asking again rather than
executing.

### Binding a Decision to Its Arguments

When a decision travels through something you do not control — a browser, a queue,
an external approval service — the call ID could be replayed with different
arguments. Bind the decision to the arguments it was granted for:

```rust
use adk_core::{RunConfig, ToolConfirmationDecision, tool_call_fingerprint};
use serde_json::json;
use std::collections::HashMap;

let approved_args = json!({ "path": "/data/report.csv" });

let mut decisions = HashMap::new();
decisions.insert("call_abc123".to_string(), ToolConfirmationDecision::Approve);

let mut fingerprints = HashMap::new();
fingerprints.insert(
    "call_abc123".to_string(),
    tool_call_fingerprint("delete_file", &approved_args),
);

let config = RunConfig::builder()
    .tool_confirmation_decisions(decisions)
    .tool_confirmation_fingerprints(fingerprints)
    .build();
```

If the call that arrives does not match the fingerprint, the decision is ignored
and the call is treated as unconfirmed. `tool_call_fingerprint` is canonical over
key order, so a re-serialized argument object still matches.

For decisions that should apply by policy rather than per call, implement a
`ToolConfirmationHandler` instead of widening the static map.

### CLI Example

A terminal agent that asks for confirmation before running a tool. A
`ToolConfirmationHandler` decides each call inside the run, so the approval
applies to the exact call the model made:

```rust
use adk_agent::LlmAgentBuilder;
use adk_core::{
    Content, RunConfig, SessionId, ToolConfirmationDecision, ToolConfirmationHandler,
    ToolConfirmationRequest, UserId, async_trait,
};
use adk_model::GeminiModel;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
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

/// Asks on the terminal before each call that requires confirmation.
#[derive(Debug)]
struct TerminalApprover;

#[async_trait]
impl ToolConfirmationHandler for TerminalApprover {
    async fn decide(
        &self,
        request: &ToolConfirmationRequest,
    ) -> adk_core::Result<ToolConfirmationDecision> {
        let prompt = format!(
            "\nThe agent wants to run '{}' with args: {}\nAllow? [y/n]: ",
            request.tool_name, request.args
        );
        // Reading stdin blocks, so it runs off the async executor.
        let answer = tokio::task::spawn_blocking(move || {
            print!("{prompt}");
            io::stdout().flush()?;
            let mut answer = String::new();
            io::stdin().read_line(&mut answer)?;
            Ok::<_, io::Error>(answer)
        })
        .await
        .map_err(|e| adk_core::AdkError::tool(e.to_string()))?
        .map_err(|e| adk_core::AdkError::tool(e.to_string()))?;

        Ok(if answer.trim().eq_ignore_ascii_case("y") {
            ToolConfirmationDecision::Approve
        } else {
            ToolConfirmationDecision::Deny
        })
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-3.7-flash")?;

    let agent = LlmAgentBuilder::new("file-manager")
        .model(Arc::new(model))
        .instruction("You help manage files. Use delete_file when asked to remove files.")
        .tool(Arc::new(DeleteFile))
        .require_tool_confirmation("delete_file")
        .build()?;

    let session_service = Arc::new(InMemorySessionService::new());
    session_service
        .create(CreateRequest {
            app_name: "file-manager".to_string(),
            user_id: "user-1".to_string(),
            session_id: Some("session-1".to_string()),
            state: HashMap::new(),
        })
        .await?;

    let runner = Runner::builder()
        .app_name("file-manager")
        .agent(Arc::new(agent))
        .session_service(session_service as Arc<dyn SessionService>)
        .build()?;

    // Every run consults the approver for calls to `delete_file`.
    let config = RunConfig::builder()
        .tool_confirmation_handler(Arc::new(TerminalApprover))
        .build();

    println!("File Manager (type 'quit' to exit)");
    loop {
        print!("> ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();
        if input == "quit" {
            break;
        }

        let mut stream = runner
            .run_with_config(
                UserId::new("user-1")?,
                SessionId::new("session-1")?,
                Content::new("user").with_text(input),
                Some(config.clone()),
            )
            .await?;

        while let Some(event) = stream.next().await {
            if let Some(content) = event?.llm_response.content {
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

An SSE endpoint streams events to the frontend. When the agent reaches a call that
requires confirmation, a `ToolConfirmationHandler` sends the request down the same
stream and waits; the frontend renders an approval dialog and posts the decision to a
second endpoint, which completes the waiting call:

```rust
use adk_core::{
    AdkError, Content, RunConfig, SessionId, ToolConfirmationDecision,
    ToolConfirmationHandler, ToolConfirmationRequest, UserId, async_trait,
};
use adk_runner::Runner;
use axum::response::sse::{Event, Sse};
use axum::{Json, extract::State, http::StatusCode};
use futures::StreamExt;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc, oneshot};

/// Calls waiting for a decision, keyed by session ID and function call ID.
type Pending = Arc<Mutex<HashMap<(String, String), oneshot::Sender<ToolConfirmationDecision>>>>;

#[derive(Clone)]
struct AppState {
    runner: Arc<Runner>,
    pending: Pending,
}

/// Forwards each confirmation request to the browser and waits for its answer.
#[derive(Debug)]
struct BrowserApprover {
    session_id: String,
    pending: Pending,
    requests: mpsc::UnboundedSender<ToolConfirmationRequest>,
}

#[async_trait]
impl ToolConfirmationHandler for BrowserApprover {
    async fn decide(
        &self,
        request: &ToolConfirmationRequest,
    ) -> adk_core::Result<ToolConfirmationDecision> {
        let call_id = request
            .function_call_id
            .clone()
            .ok_or_else(|| AdkError::tool("confirmation request has no call ID"))?;
        let (answer, decision) = oneshot::channel();
        self.pending.lock().await.insert((self.session_id.clone(), call_id), answer);
        self.requests
            .send(request.clone())
            .map_err(|_| AdkError::tool("the client disconnected before approving"))?;
        // An abandoned request — the stream closed without an answer — denies the call.
        Ok(decision.await.unwrap_or(ToolConfirmationDecision::Deny))
    }
}

/// What the chat stream does next.
enum Step {
    Confirm(ToolConfirmationRequest),
    Agent(Option<adk_core::Result<adk_core::Event>>),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatRequest {
    message: String,
    user_id: String,
    session_id: String,
}

async fn chat_handler(
    State(state): State<AppState>,
    Json(req): Json<ChatRequest>,
) -> Sse<impl futures::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let (requests, mut confirmations) = mpsc::unbounded_channel();
    let config = RunConfig::builder()
        .tool_confirmation_handler(Arc::new(BrowserApprover {
            session_id: req.session_id.clone(),
            pending: state.pending.clone(),
            requests,
        }))
        .build();

    let stream = async_stream::stream! {
        let (Ok(user_id), Ok(session_id)) =
            (UserId::new(&req.user_id), SessionId::new(&req.session_id))
        else {
            yield Ok(Event::default().event("error").data("invalid user or session ID"));
            return;
        };
        let content = Content::new("user").with_text(&req.message);
        let mut events = match state
            .runner
            .run_with_config(user_id, session_id, content, Some(config))
            .await
        {
            Ok(events) => events,
            Err(e) => {
                yield Ok(Event::default().event("error").data(e.to_string()));
                return;
            }
        };

        loop {
            let step = tokio::select! {
                Some(request) = confirmations.recv() => Step::Confirm(request),
                next = events.next() => Step::Agent(next),
            };
            match step {
                // A call is waiting for approval: show the dialog.
                Step::Confirm(request) => {
                    yield Ok(Event::default()
                        .event("tool_confirmation")
                        .data(serde_json::to_string(&request).unwrap_or_default()));
                }
                Step::Agent(Some(Ok(event))) => {
                    for part in event.llm_response.content.iter().flat_map(|c| &c.parts) {
                        if let Some(text) = part.text() {
                            yield Ok(Event::default()
                                .event("text")
                                .data(serde_json::json!({ "text": text }).to_string()));
                        }
                    }
                }
                Step::Agent(Some(Err(e))) => {
                    yield Ok(Event::default().event("error").data(e.to_string()));
                }
                Step::Agent(None) => break,
            }
        }

        yield Ok(Event::default().event("done").data("{}"));
    };

    Sse::new(stream)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalRequest {
    session_id: String,
    function_call_id: String,
    approved: bool,
}

/// Completes the call the dialog was shown for.
async fn approve_handler(
    State(state): State<AppState>,
    Json(req): Json<ApprovalRequest>,
) -> StatusCode {
    let key = (req.session_id, req.function_call_id);
    let Some(answer) = state.pending.lock().await.remove(&key) else {
        return StatusCode::NOT_FOUND;
    };
    let decision = if req.approved {
        ToolConfirmationDecision::Approve
    } else {
        ToolConfirmationDecision::Deny
    };
    let _ = answer.send(decision);
    StatusCode::NO_CONTENT
}

// Routes:
//   POST /api/chat    -> chat_handler (SSE)
//   POST /api/approve -> approve_handler
//
// Frontend (conceptual):
//
// source.addEventListener('tool_confirmation', (e) => {
//   const request = JSON.parse(e.data);
//   showConfirmDialog(request.toolName, request.args, (approved) => {
//     fetch('/api/approve', {
//       method: 'POST',
//       headers: { 'Content-Type': 'application/json' },
//       body: JSON.stringify({
//         sessionId,
//         functionCallId: request.functionCallId,
//         approved,
//       }),
//     });
//   });
// });
```

Authenticate the approve endpoint as you do the chat endpoint, so only the session's
user can answer for it.

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
            let admins = ["alice@co.com"];
            if tool_name == "admin_action" && !admins.contains(&ctx.user_id()) {
                // Return Some(Content) to skip the tool
                return Ok(Some(
                    Content::new("tool")
                        .with_text("Permission denied: admin_action is limited to admins")
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
- `Err(e)` — skip the tool and report the error to the LLM as the tool's result; the run
  continues and after-tool callbacks do not run for that call

A callback sees the user ID but not the request's scopes. For scope checks, wrap the
tool with `adk_auth::ScopeGuard`, which reads them from the tool context.

To apply the same gate to every agent in an application, register it as a `before_tool`
plugin on the runner's `PluginManager`. The runner's plugin callbacks run ahead of each
agent's own callbacks, in every agent the run reaches.

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

### Graph-Native Tool Confirmation

An `AgentNode` preserves the standard tool-confirmation policy when it runs in
a `CompiledGraph`. Instead of flattening the graph into a `Runner` event stream,
the graph checkpoints its own frontier and emits a structured custom event that
can be read with `GraphToolConfirmationPause::from_stream_event`.

```rust,no_run
use adk_agent::LlmAgentBuilder;
use adk_core::{RunConfig, ToolConfirmationDecision};
use adk_graph::{
    checkpoint::MemoryCheckpointer,
    edge::{END, START},
    graph::StateGraph,
    node::{AgentNode, ExecutionConfig},
    state::State,
    interrupt::GraphToolConfirmationPause,
    stream::StreamMode,
};
use futures::StreamExt;
use std::{collections::HashMap, sync::Arc};

let agent = LlmAgentBuilder::new("file_manager")
    .model(model)
    .tool(delete_file_tool)
    .require_tool_confirmation("delete_file")
    .build()?;

let graph = StateGraph::with_channels(&["messages"])
    .add_node(AgentNode::new(Arc::new(agent)))
    .add_edge(START, "file_manager")
    .add_edge("file_manager", END)
    .compile()?
    .with_checkpointer(MemoryCheckpointer::new());

let mut events = Box::pin(graph.stream(
    State::new(),
    ExecutionConfig::new("delete-report"),
    StreamMode::Debug,
));

let pause = loop {
    match events.next().await.transpose()? {
        Some(event) => {
            if let Some(pause) = GraphToolConfirmationPause::from_stream_event(&event) {
                break pause;
            }
        }
        None => unreachable!("the graph must pause before the tool runs"),
    }
};

// Present `pause.request.tool_name` and `pause.request.args` to the approver. A decision
// is scoped to this exact function call ID; bind its arguments as well when it
// crosses an untrusted boundary.
let call_id = pause.request.function_call_id.expect("LLM tool calls have an ID");
let decisions = HashMap::from([(call_id, ToolConfirmationDecision::Approve)]);

// The checkpoint is selected automatically by thread ID. `pause.checkpoint_id` is
// available for audit records or an explicit `with_resume_from` call.
drop(events);
let final_events = graph.stream_with_run_config(
    State::new(),
    ExecutionConfig::new("delete-report"),
    StreamMode::Debug,
    RunConfig::builder().tool_confirmation_decisions(decisions).build(),
);
# let _ = pause;
# let _ = final_events;
```

The graph retains node lifecycle, intermediate state, nested subgraphs, and
the pending frontier. Nodes that completed alongside the confirmation request
are checkpointed and are not replayed after approval. The agent itself uses the
same `RunConfig` decision semantics as a normal ADK run.

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

Order of evaluation in `LlmAgent`:

1. Tool guardrails (`ToolGuardrailSet`) — screen every call in the model's batch; a denial
   becomes the call's result
2. `ToolConfirmationPolicy` — a static decision, the `ToolConfirmationHandler`, or a pause
3. Enhanced plugins (`before_tool_call`)
4. The runner's plugin `before_tool` callbacks, then the agent's `BeforeToolCallback`s — the
   first to return content skips the tool
5. Circuit breaker
6. Tool executes — the RBAC (`ProtectedTool`) and scope (`ScopeGuard`) wrappers check here,
   inside `execute()`, so a denial is a tool error
7. `on_tool_error` callbacks (plugins first) — when the tool failed, including an RBAC denial
8. The runner's plugin `after_tool` callbacks, then `AfterToolCallback`,
   `AfterToolCallbackFull`, and enhanced plugins (`after_tool_call`)

> **Note:** An `on_tool_error` callback that returns a fallback value replaces the error,
> including an access denial. Keep fallbacks to tools whose failures are safe to paper over.

## Related

- [Access Control](access-control.md) — RBAC, SSO, audit logging
- [Callbacks](../callbacks/callbacks.md) — All callback types and lifecycle
- [Graph Agents](../agents/graph-agents.md) — Checkpoint-based interrupts
- [Guardrails](guardrails.md) — Input/output validation

---

**Previous**: [← Access Control](access-control.md) | **Next**: [Guardrails →](guardrails.md)
