# Serving the harness

`adk-rust serve` puts the harness behind the ADK HTTP server: the same agent, policy and
standing rules as the REPL, with held calls decided through the approval inbox instead of a
terminal prompt (spec 3.1 §8.2).

```bash
adk-rust serve                                  # 127.0.0.1:8080, plan mode
adk-rust serve --mode ask --dir ./project       # edits and commands wait for approval
adk-rust serve --host 0.0.0.0 --auth-token "$TOKEN"
adk-rust serve --agent chat                     # the plain chat agent, as before the harness
```

| Default | Value |
|---|---|
| Agent and app name | `adk-rust` |
| Mode | `plan`; the model reads the workspace and proposes plans, nothing changes |
| Approval inbox | `/api/approvals`, expiry ticker every 30 s |
| Background runs | `/api/runs`, target `adk-rust` |
| Storage | `<config dir>/adk-rust/harness.db`, shared with the REPL |
| Request timeout | 15 minutes, since a turn runs model calls and commands (`with_security_config` sets another) |

> **Important:** a served harness in plan mode still lets the model read the whole workspace.
> Serve only workspaces whose contents the model may see.

## Who may call it

The server's routes need an authenticated caller; the harness mount always has one.

| Listening on | Caller | Identity |
|---|---|---|
| `127.0.0.1` / `::1` (default) | Any loopback peer | `LoopbackOperatorExtractor`: the OS user, scope `operator` |
| Any other address | `Authorization: Bearer <token>` | `BearerTokenExtractor`: `--auth-token` or `ADK_SERVE_TOKEN`, compared in constant time |

Without a token, `serve` refuses a non-loopback `--host`. `--mode auto` beyond loopback also
needs `--sandbox`. The served harness applies standing rules and auto mode only to runs whose
caller was authenticated (spec 3.1 §9.2 I6); every caller the mount admits is. The caller's identity replaces the `userId` a request body names, and
held calls are routed to the `operator` group, so the operator approves the calls of their
own runs.

The bundled web UI sends no `Authorization` header and never calls `/api/approvals`: use it
on loopback, and decide held calls with `adk-rust approvals` or `curl`.

## Deciding a held call over HTTP

With `--mode ask`, a call the policy holds ends the run with a `tool_confirmation` event and
an approval request in the inbox. Deciding it and sending an empty message resumes the
session; the approved call runs once.

```bash
# 1. A run holds its command.
curl -s localhost:8080/api/run -H 'content-type: application/json' -d '{
  "appName": "adk-rust", "userId": "me", "sessionId": "s1",
  "newMessage": {"role": "user", "parts": [{"text": "Run: echo hi >> greeting.txt"}]}}'

# 2. The inbox lists it with its id and version.
curl -s localhost:8080/api/approvals

# 3. Approve it.
curl -s localhost:8080/api/approvals/apr_…/decide -H 'content-type: application/json' \
  -d '{"expectedVersion": 1, "action": "approve"}'

# 4. An empty message resumes the session; the command runs once.
curl -s localhost:8080/api/run -H 'content-type: application/json' -d '{
  "appName": "adk-rust", "userId": "me", "sessionId": "s1",
  "newMessage": {"role": "user", "parts": []}}'
```

`"action": "reject"` with a `"reason"` denies the call instead. The same steps from the
command line:

```bash
adk-rust approvals list --server http://127.0.0.1:8080
adk-rust approvals approve apr_… --server http://127.0.0.1:8080
adk-rust approvals deny apr_… --reason "not on main" --server https://agents.example.com --token "$TOKEN"
```

## Serving your own harness

`HarnessLauncher::build_app` mounts any `HarnessAgent` the same way; build the agent with
the approval service the server mounts, so held calls reach the inbox.

```rust
use std::sync::Arc;

use adk_cli::harness::{HarnessLauncher, OPERATOR_SCOPE};
use adk_core::{
    ApprovalPolicy, ApprovalService, ApproverSelector, InMemoryApprovalStore, StoreApprovalService,
};
use adk_harness::{HarnessAgent, HarnessMode};

async fn serve(model: Arc<dyn adk_core::Llm>) -> adk_core::Result<()> {
    let routing =
        ApprovalPolicy::new([ApproverSelector::group(OPERATOR_SCOPE)]).allow_self_approval();
    let approvals: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(routing),
    );
    let harness = HarnessAgent::builder("my-harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(".").allow_bash(true))
        .mode(HarnessMode::Plan)
        .approval_service(approvals.clone())
        .build()?;
    HarnessLauncher::new(harness)
        .with_user_id("ana")
        .with_approval_service(approvals)
        .run_serve_directly(8080)
        .await
}
```

The server serves the harness under its agent name, which is the `appName` of requests.
Without an extractor, `build_app` admits loopback peers as the operator and refuses to build
for a non-loopback bind address; serve the router with
`into_make_service_with_connect_info::<SocketAddr>()` (as `run_serve_directly` does) so the
peer address is known.

## Related

- [The harness in your terminal](cli.md)
- [Launcher](../deployment/launcher.md)
- Spec: `docs/design/spec-3.1-harness-agent.md` §8.2
