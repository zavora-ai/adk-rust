# Serve an agent over MCP

`adk_server::mcp::McpAgentHost` (feature `mcp`, experimental) publishes ADK-Rust agents as MCP
tools. Another agent or MCP client — an ADK-Rust `McpToolset`, Claude, a LangChain agent, the
MCP Inspector — calls the tool, and the call becomes a durable run of the agent:

| The client | The server |
|---|---|
| Calls the agent's tool with a `message` | Enqueues a durable run of the agent for the caller |
| Declared the tasks extension (SEP-2663) | Answers at once with a task whose id is the run id |
| Polls `tasks/get` | Reads the run from the run store |
| Is asked a form elicitation | The run holds a tool call for approval |
| Answers it with `tasks/update` | Decides the held call through the approval inbox and wakes the run |
| Cancels with `tasks/cancel` | Cancels the run |
| Restarts, or the server does | Nothing is lost: the task is the run, in the run store |

The agent runs through the same governed path as anywhere else: its tool policy, approvals,
run budget, spend ledger, action ledger and kill switch apply to every call it makes.

## Setup

```toml
[dependencies]
adk-agent = "3.0.0"
adk-core = "3.0.0"
adk-model = { version = "3.0.0", features = ["anthropic"] }
adk-runner = "3.0.0"
adk-server = { version = "3.0.0", features = ["mcp"] }
adk-session = { version = "3.0.0", features = ["sqlite"] }
adk-tool = { version = "3.0.0", features = ["mcp-server"] }
async-trait = "0.1"
axum = "0.8"
schemars = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
```

## Serve over Streamable HTTP

The agent below moves money with a `transfer` tool that needs approval. Its runs, held calls and
sessions live in one SQLite file, so a restarted server continues every task.

```rust
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{AdkError, ErrorComponent, RunConfig};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use adk_runner::Runner;
use adk_server::background::DurableWorker;
use adk_server::mcp::{McpAgent, McpAgentHost};
use adk_session::{SqliteApprovalStore, SqliteRunStore, SqliteSessionService};
use adk_tool::FunctionTool;
use adk_tool::mcp::server::{McpCaller, McpCallerResolver, McpHttpRequest, McpRouterBuilder};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::json;

/// The arguments of `transfer`.
#[derive(Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct TransferArgs {
    /// The amount, in cents.
    amount_cents: u64,
}

/// Bearer tokens the operator issued, by holder.
struct IssuedTokens;

#[async_trait::async_trait]
impl McpCallerResolver for IssuedTokens {
    async fn resolve(&self, request: &McpHttpRequest<'_>) -> adk_core::Result<McpCaller> {
        match request.bearer_token() {
            Some("ana-token") => Ok(McpCaller::authenticated("ana")),
            _ => Err(AdkError::unauthorized(
                ErrorComponent::Auth,
                "ops.token",
                "a bearer token the operator issued is required",
            )),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = "sqlite://ops.db?mode=rwc";
    let runs = Arc::new(SqliteRunStore::new(db).await?);
    runs.migrate().await?;
    let approvals = Arc::new(SqliteApprovalStore::new(db).await?);
    approvals.migrate().await?;
    let sessions = Arc::new(SqliteSessionService::new(db).await?);
    sessions.migrate().await?;

    let transfer = FunctionTool::new(
        "transfer",
        "Moves money from the operating account",
        |_ctx, args| async move { Ok(json!({ "status": "sent", "amountCents": args["amountCents"] })) },
    )
    .with_parameters_schema::<TransferArgs>();
    let model = AnthropicClient::new(AnthropicConfig::new(
        std::env::var("ANTHROPIC_API_KEY")?,
        "claude-sonnet-5-5",
    ))?;
    let agent = LlmAgentBuilder::new("ops")
        .description("Moves money for the operations team")
        .instruction("Move money with the transfer tool when asked, then report the outcome.")
        .model(Arc::new(model))
        .tool(Arc::new(transfer))
        .require_tool_confirmation("transfer")
        .build()?;
    let runner = Runner::builder()
        .app_name("ops")
        .agent(Arc::new(agent))
        .session_service(sessions)
        .run_config(RunConfig::builder().approval_store(approvals.clone()).build())
        .build()?;

    let host = McpAgentHost::builder("ops", runs.clone())
        .agent(McpAgent::new("ops", "Moves money for the operations team"))
        .approval_store(approvals)
        .build()?;
    let worker = DurableWorker::new(runs)
        .with_agent("ops", Arc::new(runner))
        .with_hooks(host.worker_hooks())
        .spawn();
    let server = host.server("com.example/ops", "1.0.0", "The operations team's agents").build()?;
    let router = McpRouterBuilder::new(server).caller_resolver(Arc::new(IssuedTokens)).build()?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
    axum::serve(listener, router).await?;
    worker.shutdown().await;
    Ok(())
}
```

| Part | Role |
|---|---|
| `McpAgentHost` | Starts runs for calls, answers `tasks/*`, and turns held calls into elicitations |
| `McpAgent::new(target, description)` | One published agent: the worker target its runs name, published as a tool of the same name (`tool_name` renames it) |
| `approval_store` | The store the agent's runner records held calls in; required |
| `DurableWorker` | Claims and runs the runs; `host.worker_hooks()` streams their progress to waiting calls |
| `host.server(..)` | An `McpServer` builder with the agents' tools, the host as executor and task host, and no call timeout |
| `McpRouterBuilder` | Streamable HTTP, the Server Card and the caller resolver ([Publish an MCP server](server.md)) |

To use the identity layer of an `adk-server` deployment, resolve callers with
`RequestContextCallers::new(extractor)`: the caller is the `RequestContext`'s `user_id`, and the
context rides on the caller as an extension.

## Serve over stdio

A stdio server serves the one client that started it, so the host names that client with
`local_caller`:

```rust
use adk_server::mcp::McpAgentHost;
use adk_tool::mcp::rmcp::{ServiceExt, transport::stdio};
use adk_tool::mcp::server::McpCaller;

async fn serve_stdio(host: McpAgentHost) -> Result<(), Box<dyn std::error::Error>> {
    let server = host
        .server("com.example/ops", "1.0.0", "The operations team's agents")
        .local_caller(McpCaller::authenticated(format!("local:{}", whoami())))
        .build()?;
    server.serve(stdio()).await?.waiting().await?;
    Ok(())
}

fn whoami() -> String {
    std::env::var("USER").unwrap_or_else(|_| "operator".to_string())
}
```

Nothing may write to stdout but the transport, so send `tracing` output to stderr.

## What a client sees

Each agent is one tool:

| Field | Value |
|---|---|
| `inputSchema` | `message` (required): what to ask; `sessionId`: continue the conversation of an earlier answer |
| `outputSchema` | `text`, `sessionId`, `runId` |
| Annotations | `readOnlyHint: false`, `idempotentHint: false`, `destructiveHint: true` |

A call without `sessionId` starts a new session, `mcp-{uuid}`. Calls in one session run one at a
time, in the order they arrive. A completed call or task carries the agent's final text as text
content and the three fields as `structuredContent`.

## Tasks

A client that declares the tasks extension receives a task for every call, at once. Its id is
the run id, and `pollIntervalMs` is the host's poll interval (one second by default).

| Run | `tasks/get` |
|---|---|
| `Queued` | `working`, `statusMessage: "queued"` |
| `Running` | `working`, `statusMessage` the run's latest step — `calling transfer`, `ops: I sent…` — while this process runs it |
| `Waiting` on held calls still pending | `input_required`, one form elicitation per held call |
| `Waiting` otherwise: suspended, frozen, or decided and about to resume | `working` |
| `Completed` | `completed`, with the call's result |
| `Failed` | `failed`, `-32603` "the agent run failed"; the run's error stays in the server log |
| `Cancelled` | `cancelled` |

`tasks/cancel` cancels the run: a queued or waiting run at once, a running one at its worker's
next heartbeat. A finished task answers `-32602`.

MCP `2026-07-28` routes no task-status notifications through `subscriptions/listen`, so clients
follow a task by polling `tasks/get`.

## Held calls become elicitations

When the agent holds a call for approval, its run parks on it. Each held call still pending in
the approval store is asked as a form elicitation:

```json
{
  "method": "elicitation/create",
  "params": {
    "mode": "form",
    "message": "The agent `ops` asks to run `transfer` with {\"amountCents\":2500}. Approve this call?",
    "requestedSchema": {
      "type": "object",
      "properties": {
        "approve": {
          "type": "boolean",
          "title": "Approve",
          "description": "Run the call as shown",
          "default": true
        },
        "reason": {
          "type": "string",
          "title": "Reason",
          "description": "Why not, told to the agent when you decline"
        }
      }
    }
  }
}
```

| Answer | Decision |
|---|---|
| `accept`, unless `approve` is `false` | Approve |
| `accept` with `approve: false`, or `decline` | Reject, with `reason`; the agent hears "Tool 'transfer' was not run; the approver declined it: …" |
| `cancel` | Reject, as dismissed |

Each question's key is a digest of the run, the held call's fingerprint, when it was held, and
the approval request's version. An answer therefore decides exactly the call it was asked about:

| The answer names | Result |
|---|---|
| A question the task asks now | Decided; the run wakes and resumes the held call through the governed path |
| A question already decided — by this client, in the approval inbox, or by another approver | Refused as stale (`-32603`, `mcp.agent.stale_answer`); nothing is decided |
| A call that was held again since | Refused as stale; the new hold is a new question |

When the host has an [approval service](../security/approval-service.md) holding an open
request for the call, the answer is the caller's decision on that request, at the version the
question was asked at, under the request's policy: a quorum, a group of approvers, or a refusal
of self-approval apply. Otherwise the decision is recorded in the approval store under the call's
fingerprint — the run's own user decides, as on the A2A and ACP servers.

## Clients without tasks

A client that did not declare the tasks extension waits for the answer on its call:

| Client | The run parks on a held call |
|---|---|
| MCP `2026-07-28` | The call answers `input_required` with the elicitations and a `requestState` naming the run; the retried call carries the answers, which are decided as above, and waits again |
| Earlier revisions, with form elicitation | The server sends `elicitation/create` on the live connection and decides the answer |
| Neither | The call is refused, `mcp.agent.needs_input`; the held call waits in the approval inbox |

A call waits up to `wait_limit` (ten minutes by default); the run goes on after that. A client
that cancels its call cancels the run.

## Progress

While a call without a task waits for its run, the server sends `notifications/progress` for
each step the run takes — each tool call, each tool result, a call held for approval, and the
start of each model answer — when the request carried a `progressToken`. Partial streaming
chunks are not reported. A task's call returns at once, so its latest step is the task's
`statusMessage` instead. The steps come from `host.worker_hooks()` on a worker in the same
process; without the hooks, calls read the run every poll interval and report no steps.

## Identity and ownership

| Rule | Effect |
|---|---|
| A run belongs to its caller | Its user is the `McpCaller`'s id; another caller's `tasks/get`, `tasks/update` and `tasks/cancel` answer `-32602`, unknown task |
| An authenticated caller's run acts as that user | The worker sets the run's `RequestContext`, so tools see `is_authenticated()` |
| Anonymous callers start nothing | Refused with `mcp.agent.unauthenticated` unless `allow_anonymous(true)`, under which every anonymous call belongs to the one user `anonymous` |
| The agent acts as itself | The worker's principal for the target, `urn:adk:agent:{app}/{agent}`, on behalf of the caller |

## Budgets, governance and audit

The runner's `RunConfig` applies to every run: its `RunBudget`, `ToolPolicy`, approvals and
spend ledger. `governance(control)` on the builder refuses new calls while the kill switch is
frozen; the worker's own control parks runs already in flight.

| Decision made through | Audit record |
|---|---|
| The approval service | The service's own trail (`StoreApprovalService::with_audit`): an `approval` record naming the caller as approver |
| The approval store | The host's trail (`audit(trail)`): an `approval` record with the caller as actor, the arguments' digest, `source: "mcp_elicitation"`, the run and the session, written before the decision |

Under `AuditFailureMode::Block` a decision whose record cannot be written is refused. The
governed path then records the resumed call's own decision and outcome.

## Restarts

The task is the run, so it survives anything the run store survives:

| What restarts | What happens |
|---|---|
| The server, while a task waits on a held call | A server on the same stores answers `tasks/get` with the same questions; the answer resumes the run |
| The server, while a run is running | The new worker recovers the run once its lease expires and continues the session ([Durable runtime](../core/durable-runtime.md#crash-resume)) |
| The client | `McpToolset::call_tool_resumable` returns a `ParkedMcpCall` to persist; `reissue` on a new toolset answers it and follows the task to the end |

## Kernel-hosted agents and the harness

The [agent kernel](../kernel/index.md) runs its processes on a durable run store, with one
`DurableWorker` per queue class serving every target the kernel hosts — the harness agent as
`adk-rust` among them. A host built on the kernel's run store, with the queue of a class
(`interactive`, `background`), the app name of the hosted runner and its approval store,
enqueues runs the kernel's workers claim:

```rust,ignore
let host = McpAgentHost::builder(harness_app_name, kernel_run_store)
    .agent(McpAgent::new("adk-rust", "The operations harness"))
    .queue("interactive")
    .approval_store(harness_approval_store)
    .build()?;
```

The kernel's workers take no extra hooks, so these tasks report each state change on the next
poll, without step lines.

## Call an agent from ADK-Rust

```rust
use std::sync::Arc;

use adk_tool::mcp::rmcp::model::{ElicitResult, ElicitationAction, InputResponses};
use adk_tool::mcp::{
    AutoDeclineElicitationHandler, McpAuth, McpCallOutcome, McpHttpClientBuilder, McpTaskConfig,
};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let toolset = McpHttpClientBuilder::new("http://127.0.0.1:8080/mcp")
        .with_auth(McpAuth::bearer("ana-token"))
        .with_elicitation_handler(Arc::new(AutoDeclineElicitationHandler))
        .with_task_support(McpTaskConfig::enabled())
        .connect_with_elicitation()
        .await?
        .with_name("ops");
    let message = json!({ "message": "Send 25 USD to the supplier" });
    let outcome = toolset
        .call_tool_resumable(Some("call-1"), "ops", message.as_object().unwrap().clone())
        .await?;
    let McpCallOutcome::Parked(parked) = outcome else {
        return Ok(());
    };
    // Show `parked.input_requests` to the person; here, approve each one.
    let approve = serde_json::to_value(ElicitResult::new(ElicitationAction::Accept))?;
    let answers: InputResponses =
        parked.input_requests.keys().map(|key| (key.clone(), approve.clone())).collect();
    if let McpCallOutcome::Completed(answer) = toolset.reissue(&parked, answers).await? {
        println!("{}", answer["text"]);
    }
    Ok(())
}
```

`call_tool_resumable` returns `Parked` as soon as the task asks a question; the `ParkedMcpCall`
serializes, so a person can answer it later from another process. A toolset used as an agent's
tool instead answers through its `ElicitationHandler` in process.

## Stability

The `mcp` feature is experimental, as `mcp-server` is: the tasks extension (SEP-2663) and the
`2026-07-28` input-required flow are new upstream, and `rmcp` routes no task-status
notifications yet. Both may change in a 3.x release.

## Related

- [Publish an MCP server](server.md): the `McpServer` the host builds on
- [MCP client](client.md): tasks, elicitation and parked calls on the client
- [Durable runtime](../core/durable-runtime.md): runs, workers and crash resume
- [Tool authorization](../security/tool-authorization.md): held calls and how a decision resumes them
- [Approval service](../security/approval-service.md): the approval inbox
