# Tool Authorization

Control which tools an agent can execute and when human approval is required. ADK-Rust provides five mechanisms — from a run-wide default-deny policy to full RBAC — that work across the CLI, web server, and A2A protocol, plus an organisation-wide kill switch.

## Quick Comparison

| Mechanism | Use Case | Granularity | Runtime |
|-----------|----------|-------------|---------|
| [Tool Policy](#tool-policy) | Run-wide allow, deny, or require approval | Per tool name and argument | Decides before execution; for [built-in tools](#built-in-tools), before the request |
| [Tool Confirmation Policy](#tool-confirmation-policy) | Interactive approval in CLI/web | Per-tool or all tools | Pauses execution, emits event |
| [BeforeToolCallback](#beforetoolcallback) | Programmatic gate / audit | Custom logic per call | Sync decision, no pause |
| [Access Control (RBAC)](#access-control) | Role-based enterprise security | Per-user, per-tool | Deny before execution |
| [Graph Interrupts](#graph-interrupts) | Complex approval workflows | Per-node checkpoint | Persists state, resumes later |

## Tool Policy

A `ToolPolicy` decides every tool call the run makes — in every agent, including transfer
targets and agents behind an `AgentTool` — on the call's final arguments, after plugin and
callback rewrites. It returns one of:

| Decision | Effect |
|----------|--------|
| `PolicyDecision::Allow` | The call proceeds to guardrails and execution |
| `PolicyDecision::Deny { reason }` | The tool does not run; the model receives `{"error": "Tool '<name>' denied by policy: <reason>"}` |
| `PolicyDecision::RequireApproval { reason }` | The call goes through confirmation, as if the agent required it |

`DeclarativePolicy` matches rules in order. A rule names a tool glob (`*` and `?`), may require
the effect the tool declares (`read_only_tools()`, or `with_effect(ToolEffect::Idempotent)` — see
[Tool Effects](../tools/tool-effects.md)), and may add argument predicates addressed by JSON
pointer. The first matching rule decides; a call no rule matches is **denied by default**.

| Predicate | Holds when |
|-----------|------------|
| `ArgPredicate::equals(pointer, value)` | The argument equals `value` |
| `ArgPredicate::in_set(pointer, values)` | The argument equals one of `values` |
| `ArgPredicate::at_most(pointer, max)` | The argument is a JSON number no greater than `max` |
| `ArgPredicate::starts_with(pointer, prefix)` | The argument is a string with that prefix (lexical — screen paths with a guardrail) |
| `ArgPredicate::domain_in(pointer, domains)` | The argument is an `http(s)` URL whose host is listed; `*.example.com` matches subdomains. URLs with user information, backslashes, or percent-encoded hosts never match |

A predicate whose pointer is missing or whose value has the wrong type does not hold.

```rust
use adk_core::{ArgPredicate, DeclarativePolicy, PolicyRule};
use adk_runner::Runner;
use std::sync::Arc;

let policy = DeclarativePolicy::builder()
    // Rules added earlier win, so this deny applies even though the tool is read-only.
    .deny("read_secret", "secrets are never exposed to the model")
    .allow_read_only()
    .rule(PolicyRule::allow("transfer").when(ArgPredicate::at_most("/amount", 100.0)))
    .require_approval("transfer", "transfers over 100 need a person")
    .rule(PolicyRule::allow("fetch_url").when(ArgPredicate::domain_in("/url", ["docs.rs"])))
    .build();

let runner = Runner::builder()
    .app_name("treasury")
    .agent(agent)
    .session_service(session_service)
    .tool_policy(Arc::new(policy))
    .build()?;
```

The runner installs its policy on every run that does not carry one in its `RunConfig`. Implement
`ToolPolicy` directly for decisions that need outside data; `ToolPolicyRequest` carries the tool
name, final arguments, declared `ToolEffect` (`Tool::effect()`), agent name, app, user, session,
and invocation ID.

## Built-in Tools

Provider-executed built-in tools — Gemini Google Search, OpenAI hosted web search, Anthropic
server web search and the rest — run inside the model provider, so no call reaches the governed
call path. `LlmAgent` governs them before each model request instead, after every callback and
plugin rewrite, with `adk_core::BuiltinToolGovernor`:

1. **Kill switch** — a frozen run ends with `governance.frozen` before the request is sent.
2. **Tool-call budget** — with no tool calls left in the run's `RunBudget`, every built-in tool
   is withheld.
3. **Tool policy** — `ToolPolicyRequest` carries the tool name, the tool's declared
   `ToolEffect`, and its provider-native configuration as the arguments.

| Decision | Result |
|----------|--------|
| `Allow` | The tool is attached to the request |
| `Deny` | The tool is withheld; the provider never sees it |
| `RequireApproval` | Attached only when a decision already exists — a live fingerprint approval in `RunConfig::tool_approvals` or a decision in the run's `ApprovalStore`. Otherwise withheld, and recorded as pending in the store so an approver can decide before the next run |

The provider executes a built-in tool mid-response, so the confirmation handler and call-ID
decisions do not apply. The fingerprint is `tool_call_fingerprint(tool_name, configuration)`
(`ToolConfirmationRequest::fingerprint()` with no call ID). A decision taken from the store
applies to every model request of that invocation.

A run without a policy and without a tool-call limit attaches every built-in tool. Under a
`DeclarativePolicy`, which denies by default, a built-in tool needs a rule that allows it:

```rust
use adk_core::{ArgPredicate, DeclarativePolicy, PolicyRule, ToolEffect};

let policy = DeclarativePolicy::builder()
    // Search and fetch tools declare `ToolEffect::ReadOnly`.
    .allow_read_only()
    // Anthropic web search only with a bounded number of searches per request.
    .rule(PolicyRule::allow("web_search").when(ArgPredicate::at_most("/max_uses", 5.0)))
    .deny("gemini_code_execution", "no server-side code execution")
    .build();
```

| Tool | Provider | Name | Declared effect | Policy arguments |
|------|----------|------|-----------------|------------------|
| `GoogleSearchTool` | Gemini | `google_search` | `ReadOnly` | `{"google_search": {}}` |
| `UrlContextTool` | Gemini | `url_context` | `ReadOnly` | `{"url_context": {}}` |
| `GoogleMapsTool` | Gemini | `google_maps` | `ReadOnly` | `{"google_maps": {...}}` |
| `GeminiFileSearchTool` | Gemini | `gemini_file_search` | `ReadOnly` | `{"file_search": {"file_search_store_names": [...]}}` |
| `GeminiCodeExecutionTool` | Gemini | `gemini_code_execution` | `NonIdempotent` | `{"code_execution": {}}` |
| `GeminiComputerUseTool` | Gemini | `gemini_computer_use` | `NonIdempotent` | `{"computer_use": {...}}` |
| `OpenAIWebSearchTool` | OpenAI Responses | `openai_web_search` | `ReadOnly` | `{"type": "web_search", "filters": {"allowed_domains": [...]}, ...}` |
| `OpenAIFileSearchTool` | OpenAI Responses | `openai_file_search` | `ReadOnly` | `{"type": "file_search", "vector_store_ids": [...], ...}` |
| `OpenAICodeInterpreterTool` | OpenAI Responses | `openai_code_interpreter` | `NonIdempotent` | `{"type": "code_interpreter", "container": ...}` |
| `OpenAIImageGenerationTool` | OpenAI Responses | `openai_image_generation` | `NonIdempotent` | `{"type": "image_generation", ...}` |
| `OpenAIComputerUseTool`, `OpenAIShellTool`, `OpenAILocalShellTool`, `OpenAIApplyPatchTool` | OpenAI Responses | `openai_*` | `NonIdempotent` | The tool's `type` and settings |
| `OpenAIMcpTool` | OpenAI Responses | the server label | `NonIdempotent` | `{"type": "mcp", "server_label": ..., ...}` |
| `WebSearchTool` | Anthropic | `web_search` | `ReadOnly` | `{"type": "web_search_20250305", "max_uses": ..., "allowed_domains": [...], ...}` |

Anthropic bash and text editor tools (`AnthropicBashTool*`, `AnthropicTextEditorTool*`) carry a
provider-native declaration but run in the agent, so they are governed when called, like any
function tool. A declaration with a provider-native definition and no matching tool — one a
`BeforeModelCallback` added — is governed as a `NonIdempotent` built-in tool.

Every model event of a request that withheld a tool carries the list under the
`adk.builtin_tools.withheld` event metadata key (`BUILTIN_TOOLS_WITHHELD_KEY`), a JSON array of
`WithheldBuiltinTool` (`toolName`, `cause` — `policyDenied`, `approvalRequired`, `approvalDenied`
or `budgetExhausted` — and `reason`). Each withheld tool is also logged once per invocation at
`info`.

After the response, the uses the provider reports are counted in
`UsageMetadata::builtin_tool_uses`, against the run's tool-call budget, and in
`UsageMetadata::cost` — see [Run Budgets](../core/runner.md#run-budgets) and
[Usage Attribution and Cost](../models/providers.md#usage-attribution-and-cost).

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
3. The held call is answered with a confirmation-pending response —
   `{"error": "Tool 'delete_file' requires confirmation", "status": "confirmation_pending"}` —
   calls in the same turn that need no approval still run, and the agent stream ends
4. Your UI shows the user: "The agent wants to delete `/data/report.csv`. Allow?"
5. Run again with a decision for the call. Any decision source works: an approval keyed by
   the request's fingerprint (its tool name and canonical arguments), a decision keyed by its
   call ID, or the `ApprovalStore`. An empty user turn resumes without adding a message:

```rust
use adk_core::{Content, RunConfig, ToolApproval};
use std::time::Duration;

// `request` is the ToolConfirmationRequest from the paused run's event.
let config = RunConfig::builder()
    .tool_approval(
        request.fingerprint(),
        ToolApproval::approve().expires_in(Duration::from_secs(600)), // or ToolApproval::deny()
    )
    .build();
let stream = runner
    .run_with_config(user_id, session_id, Content::new("user").with_text(""), Some(config))
    .await?;
```

If denied, the tool is skipped and the LLM receives an error result —
"Tool 'delete_file' execution denied by confirmation policy" — so it can adjust its approach.

The request's arguments are the final ones: plugin rewrites and guardrail revisions have
already been applied, so the approver sees exactly what will execute. A rewrite that changes
the arguments after an approval changes the fingerprint, and the call is held again.

> **Note:** A fingerprint approval in `RunConfig::tool_approvals` applies to every matching
> call in the run it is passed to. For an approval that authorizes exactly one execution,
> record it in an [`ApprovalStore`](#approval-store) instead.

### Resuming a Held Call

At the start of every run, before its first model call, `LlmAgent` finds the calls it holds
for confirmation (`adk_core::held_tool_calls` over the session's events) and resumes each one
that has a decision. The decision does not depend on the model issuing the call again.

| Decision for the held call | What the resumed run does |
|----------------------------|---------------------------|
| Approve | Dispatches the original call — same tool, same model arguments, same call ID — through the full tool path in the [evaluation order](#combining-mechanisms); the governed path finds the decision, and an `ApprovalStore` entry is consumed |
| Deny | Answers the call with "Tool 'delete_file' execution denied by confirmation policy"; the tool does not run |
| None yet | Leaves the call held; the run continues to the model, which sees the pending response |

- **One response per call.** The resumed call's response supersedes its confirmation-pending
  response, both in the running turn and in the history projected for later runs, so providers
  that reject duplicate responses accept the history.
- **Events.** The resumed call's response event streams to the caller and persists in the
  session like any other tool result.
- **Model context.** The resume run's empty user turn and the "Tool confirmation required"
  notice are not sent to the model; they carry nothing the model said or needs.
- **Run budget.** The resumed calls are reserved as one batch. A batch that does not fit ends
  the run with `ResourceExhausted` and leaves the calls held.
- **Still held.** A held call stays held across runs until a real response answers it, or the
  same call, re-issued, is executed or refused. A newer hold of the same call replaces the
  older one, so one decision resumes one call.
- **A repeat after resume.** If the model issues the same call again after it was resumed,
  the call takes the normal path as a new call: a consumed `ApprovalStore` decision does not
  authorize it, and a call-ID decision names a different ID. A fingerprint approval in
  `RunConfig::tool_approvals` still covers it, as noted above.
- **Scope.** Held calls before a compaction, on another branch, or of another agent are not
  resumed. A `Session` that keeps no event log returns no held calls.

`CodeActAgent` resumes a held call from its checkpoint (`Disposition::AwaitingConfirmation`),
which already re-dispatches the call through the governed path. Realtime agents answer a held
call with the reason inside the live session and have no later run to resume it in.

### Decisions Authorize One Exact Call

A decision applies to one exact call: the same tool with the same canonical
arguments. Keying by tool name would make one approval authorize every call of
that tool, so an approval for `delete_file` on a scratch path would also authorize
a call targeting something else. Two calls to the same tool with different
arguments therefore need two decisions.

An unknown fingerprint or call ID means "no decision", which leaves the call
awaiting confirmation. An expired approval is ignored. The failure direction is
always toward asking again rather than executing.

### Approval Store

An `ApprovalStore` keeps held requests and decisions between runs, scoped to the app,
user, and session. When a run holds a call, the agent records the request; an approver
lists them, decides, and the next run resumes the held call with the decision.
Taking it consumes it, so one approval authorizes one execution.
`ApprovalStore::has_decision` reports a waiting decision without consuming it; the agent
asks it before resuming a call.

```rust
use adk_core::{ApprovalScope, ApprovalStore, InMemoryApprovalStore, RunConfig, ToolApproval};
use std::sync::Arc;

let store = Arc::new(InMemoryApprovalStore::new());
let runner = Runner::builder()
    .app_name("ops")
    .agent(agent)
    .session_service(session_service)
    .run_config(RunConfig::builder().approval_store(store.clone()).build())
    .build()?;

// ... a run holds a call ...

let scope = ApprovalScope::new("ops", "user-1", "session-1");
for pending in store.pending(&scope).await? {
    // Show pending.request to the approver, then:
    store.decide(&scope, &pending.fingerprint, ToolApproval::approve()).await?;
}
```

`InMemoryApprovalStore` lives for the process. `adk-session` provides durable stores that
keep requests across restarts and share them between processes — `SqliteApprovalStore`,
`PostgresApprovalStore`, `RedisApprovalStore`, `MongoApprovalStore`, `Neo4jApprovalStore`,
and `FirestoreApprovalStore` — each passing the same conformance suite. Install one with
`Runner::builder().approval_store(...)`. See [Durable Stores](durable-stores.md).

### Approval Service

For approvals that need an inbox — several approvers, a quorum, delegation, escalation,
expiry, notifications, or HTTP endpoints — set `RunConfig::approval_service` to a
`StoreApprovalService` over the store. A held call is then also submitted as an
`ApprovalRequest` under the service's routing policy, and the decision that resolves it is
written to the store, where the next run takes it. An approver may edit the arguments; the
edited call is a new call and passes the `ToolPolicy` and the tool screen again before it
executes. A rejection or response is reported to the model as the call's refusal. See
[Approval Service](approval-service.md).

### Decisions Keyed by Call ID

`RunConfig::tool_confirmation_decisions` keys a decision by function call ID. It applies only
when a run dispatches that exact call ID again — an agent resuming its held call, or a graph
resuming its checkpointed frontier — and not to a call the model re-issues, which receives a
new ID.

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

For decisions that should apply by policy rather than per call, use a
[`ToolPolicy`](#tool-policy) or implement a `ToolConfirmationHandler` instead of widening the
static map.

### Handler Timeout

A `ToolConfirmationHandler` is asked about each call with its final arguments while the run
waits. Calls in one batch are decided concurrently, so a handler that prompts on a shared
device serializes its own prompts. A `decide` that takes longer than
`RunConfig::tool_confirmation_timeout` (default five minutes) denies the call:

```rust
use adk_core::RunConfig;
use std::sync::Arc;
use std::time::Duration;

let config = RunConfig::builder()
    .tool_confirmation_handler(Arc::new(approver))
    .tool_confirmation_timeout(Duration::from_secs(120))
    .build();
```

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
#[derive(Debug, Default)]
struct TerminalApprover {
    /// Calls in one batch are decided concurrently; one prompt at a time on the terminal.
    terminal: tokio::sync::Mutex<()>,
}

#[async_trait]
impl ToolConfirmationHandler for TerminalApprover {
    async fn decide(
        &self,
        request: &ToolConfirmationRequest,
    ) -> adk_core::Result<ToolConfirmationDecision> {
        let _terminal = self.terminal.lock().await;
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
        .tool_confirmation_handler(Arc::new(TerminalApprover::default()))
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
- `Ok(Some(content))` — skip the tool and answer the call with this content instead. A
  function response part in it is sent with the call's id and tool name; text-only content
  is sent as the tool's error result, `{"error": "<text>"}`
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

Order of evaluation in `LlmAgent` and `CodeActAgent`:

1. Run budget (`RunConfig::budget`) — `LlmAgent` reserves the model turn's whole batch of tool
   calls before any of them starts (`CodeActAgent` reserves each call); a batch that does not fit
   is answered with `not run` responses and the run ends with a `ResourceExhausted` error
2. Enhanced plugins (`before_tool_call`) — may rewrite the arguments
3. The runner's plugin `before_tool` callbacks, then the agent's `BeforeToolCallback`s — the
   first to return content skips the tool
4. Circuit breaker
5. The governed path (`adk_core::authorize_tool_call`) on the final arguments:
   1. Kill switch — a frozen run ends with a `governance.frozen` error
   2. `ToolPolicy` — a denial becomes the call's result
   3. Tool guardrails (`ToolGuardrailSet`) — a denial becomes the call's result; a revision is
      checked against the policy again
   4. Confirmation, when the agent or the policy requires it — a call-ID decision, a fingerprint
      approval, the `ApprovalStore`, the `ToolConfirmationHandler`, or a hold
6. Action ledger (`ToolEffect::NonIdempotent` tools, `RunConfig::action_ledger`) — a recorded
   call is answered from its record without executing (`CodeActAgent` raises the answer into
   the script); otherwise the call is begun under its `ToolContext::idempotency_key()`
7. Tool executes under its timeout — the RBAC (`ProtectedTool`) and scope (`ScopeGuard`) wrappers
   check here, inside `execute()`, so a denial is a tool error. Only `ReadOnly` and `Idempotent`
   tools are retried, after a retryable error or a timeout, with backoff
8. Action ledger completion — the outcome is recorded; a non-idempotent call that timed out or
   panicked keeps its begun record and is answered with an `outcome_unknown` response
9. `on_tool_error` callbacks (plugins first) — when the tool failed, including an RBAC denial
10. The runner's plugin `after_tool` callbacks, then `AfterToolCallback`,
    `AfterToolCallbackFull`, and enhanced plugins (`after_tool_call`)

> **Note:** An `on_tool_error` callback that returns a fallback value replaces the error,
> including an access denial. Keep fallbacks to tools whose failures are safe to paper over.

## Kill Switch

`GovernanceControl` stops agent execution everywhere it is attached. Every runner has one,
reachable through `Runner::governance()`; share one control across runners with
`Runner::builder().governance(control)`. While it is frozen:

| Point | Effect |
|-------|--------|
| Run start | The run's stream yields a `governance.frozen` error before the session is read |
| Before each model call | The agent ends the run with the error; no request, and so no built-in tool, reaches the provider |
| Before each tool call | The tool does not execute and the run ends with the error |

```rust
use adk_core::GovernanceControl;

let control = GovernanceControl::new();
let runner = Runner::builder()
    .app_name("ops")
    .agent(agent)
    .session_service(session_service)
    .governance(control.clone())
    .build()?;

control.freeze("incident 4012: unexpected payouts");
// ... investigate ...
control.unfreeze();
```

The error is `ErrorCategory::Forbidden` and not retryable, so a caller's retry loop does not
spin against a frozen runner.

`adk-server` shares one control across every runner it builds (`ServerConfig::governance`) and
exposes `POST /api/admin/freeze` and `POST /api/admin/unfreeze` through
`ServerBuilder::enable_governance_endpoints`; freezing also pauses background-run and cron
scheduling. See [Server](../deployment/server.md#kill-switch).

Realtime tool calls go through the same governed path — see
[Realtime Tools](../realtime/tools.md#the-governed-path).

## Audit Records

`Runner::builder().audit_log(recorder)` makes the governed path record each decision before
it takes effect: a call allowed, denied (by the policy, a guardrail, a confirmation, or the
kill switch), or held for approval, and each approval decision with its source. A call whose
record cannot be written is not executed under the default `AuditFailureMode::Block`.
`freeze_recorded` and `unfreeze_recorded` record kill-switch changes with the acting
administrator. See [Audit Log](audit-log.md#what-the-runtime-records).

## Related

- [Audit Log](audit-log.md) — Signed records of every governed decision

- [Access Control](access-control.md) — RBAC, SSO, audit logging
- [Callbacks](../callbacks/callbacks.md) — All callback types and lifecycle
- [Graph Agents](../agents/graph-agents.md) — Checkpoint-based interrupts
- [Guardrails](guardrails.md) — Input/output validation

---

**Previous**: [← Access Control](access-control.md) | **Next**: [Guardrails →](guardrails.md)
