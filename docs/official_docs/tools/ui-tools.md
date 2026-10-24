# UI Tools

The `adk-ui` crate lets an agent render user interfaces through tool calls — forms, cards,
alerts, confirmations, tables, charts, progress, layouts, modals, and toasts — that a client
renders and answers. It is maintained in its own repository,
[zavora-ai/adk-ui](https://github.com/zavora-ai/adk-ui), with a React renderer published as
`@zavora-ai/adk-ui-react`.

![ADK UI Agent](images/adk-ui.jpg)

> **Important:** `adk-ui` 2.2.0, the current release, is built on `adk-core` 2. Its tools
> implement the ADK-Rust 2 `Tool` trait, which is a different trait from ADK-Rust 3's, so an
> ADK-Rust 3.0 agent cannot register them. Use `adk-ui` with ADK-Rust 2 until a release built
> on `adk-core` 3 is published; the [adk-ui README](https://github.com/zavora-ai/adk-ui#readme)
> documents its API.

## What adk-ui provides

The tools on the `adk-ui` main branch:

| Tools | Purpose |
|-------|---------|
| `render_form`, `render_confirm`, `render_modal` | Collect input or confirmation; the client sends the submission back to the agent |
| `render_card`, `render_alert`, `render_toast`, `render_progress` | Show results and status |
| `render_table`, `render_chart`, `render_layout` | Present data and dashboards |
| `render_screen`, `render_page`, `render_kit` | Emit A2UI JSONL surfaces |
| `render_app` | Render a multi-page application with navigation and shared state |
| `save_surface`, `load_surface`, `list_surfaces`, `patch_surface`, `delete_surface` | Persist versioned UI surfaces |

## UI protocols in adk-server

The REST server in ADK-Rust 3.0 negotiates the UI protocol a client renders, without a
dependency on `adk-ui`; the constants and capability specs live in
`adk_server::ui_protocol`.

| Profile | Value |
|---------|-------|
| ADK UI (default) | `adk_ui` |
| A2UI | `a2ui` |
| AG-UI | `ag_ui` |
| MCP Apps | `mcp_apps` |

A run request selects a profile with the `x-adk-ui-protocol` header or its `uiProtocol` /
`ui_protocol` field; an unknown profile is rejected with `400 Bad Request` naming the
supported ones. `GET /api/ui/capabilities` returns each profile's versions, implementation
tier, features, and limitations, and the `/api/ui/initialize`, `/api/ui/message`, and
`/api/ui/notifications/*` routes serve MCP Apps hosts.

## AG-UI decisions

A protocol-native AG-UI run (`uiProtocol: "ag_ui"`, `uiTransport: "protocol_native"`)
carries calls held for approval as AG-UI interrupts (spec 3.3 §10 S2), following the
interrupt-aware run lifecycle of the AG-UI 1.1 draft
([Interrupts](https://docs.ag-ui.com/concepts/interrupts)). With an approval service
(`ServerBuilder::with_approval_service`), every call the run holds is submitted to the
inbox, and the run ends with one interrupt per held call instead of a success:

```json
{
  "type": "RUN_FINISHED",
  "threadId": "thread-1",
  "runId": "run-1",
  "outcome": {
    "type": "interrupt",
    "interrupts": [{
      "id": "apr_5f0c…",
      "reason": "tool_call",
      "message": "Send the October invoice",
      "toolCallId": "call-1",
      "responseSchema": {
        "type": "object",
        "properties": {
          "approved": { "type": "boolean" },
          "editedArgs": { "type": "object" },
          "reason": { "type": "string" }
        },
        "required": ["approved"]
      },
      "metadata": {
        "summary": "Send the October invoice",
        "agent": "finance",
        "tool": "send_invoice",
        "risk": "medium",
        "version": 1,
        "arguments": { "invoice": "INV-10" }
      }
    }]
  }
}
```

| Field | Value |
|-------|-------|
| `id` | The inbox entry's ID; without an approval service, the call ID |
| `toolCallId` | The held call, whose `TOOL_CALL_START`, `TOOL_CALL_ARGS` and `TOOL_CALL_END` the run already sent |
| `metadata.summary`, `risk`, `agent` | Drawn from the inbox entry, never from agent output |
| `metadata.arguments` | The arguments as shown to approvers, only when the caller authenticated through the server's `RequestContextExtractor`, so it is the session's owner |
| `expiresAt` | The inbox entry's expiry, when it has one |

The held call's placeholder result and the hold's placeholder text are not sent.

### Resume

The next run on the thread answers the interrupts in `resume`:

```json
{
  "appName": "finance",
  "userId": "alice",
  "sessionId": "thread-1",
  "uiProtocol": "ag_ui",
  "uiTransport": "protocol_native",
  "input": {
    "threadId": "thread-1",
    "runId": "run-2",
    "messages": [],
    "resume": [
      { "interruptId": "apr_5f0c…", "status": "resolved", "payload": { "approved": true } }
    ]
  }
}
```

| `payload` | Decision |
|-----------|----------|
| `{"approved": true}` | Allow once |
| `{"approved": true, "editedArgs": {...}}` | Edit: the call runs with the full replacement arguments after the policy checks them again |
| `{"approved": false, "reason": "..."}` | Deny, with the reason reported to the model |

Each `resolved` entry becomes `ApprovalService::decide` on its inbox entry, before the
run starts, by the authenticated caller as `human(<user id>)` at the version the client
echoes in `metadata.version`, or else the version read then. The run is a blank turn
that resumes the calls the decisions allow; their results arrive as `TOOL_CALL_RESULT`
against the original `toolCallId`. A `cancelled` entry decides nothing: the call keeps
waiting in the inbox. An entry naming no inbox entry of the run's session is reported as
`unknown` and skipped.

Each answered entry is reported after `RUN_STARTED` as a `CUSTOM` event named
`adk.decision`, with `interruptId`, `state` (`approved`, `rejected`, `responded`,
`pending`, `expired`, `cancelled`, or `unknown`), `tool`, and, once decided, `decidedBy`
and `decidedAt`. An answer the service refuses — already decided, expired, withdrawn, or
changed since it was shown — is reported, never retried: the stream is `RUN_STARTED`, the
entry's `adk.decision`, and `RUN_ERROR` with the service's code, such as
`approval.already_resolved` with the message "Already decided by alice", and the run does
not start.

| Request | Answer |
|---------|--------|
| `resume` without an approval service, or outside the `ag_ui` profile's `protocol_native` transport | `400` |
| `resume` without an authenticated caller | `401` |
| A `resolved` entry whose `payload` is not one of the answers above | `400`, and nothing is decided |

The inbox's approval policy decides who may answer. For a personal agent whose person
approves their own agent's calls, route held calls with
`ApprovalPolicy::allow_self_approval`:

```rust,ignore
use adk_core::{ApprovalPolicy, ApproverSelector, InMemoryApprovalStore, StoreApprovalService};
use adk_server::{ServerBuilder, ServerConfig};
use std::sync::Arc;

let approvals = StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
    .with_router(ApprovalPolicy::new([ApproverSelector::human("alice")]).allow_self_approval());
let config = ServerConfig::new(agent_loader, session_service)
    .with_request_context(Arc::new(extractor));
let app = ServerBuilder::new(config).with_approval_service(Arc::new(approvals)).build();
```

`with_approval_service` needs `adk-server`'s `approvals` feature.

## Related

- [A2UI quickstart](../quickstart-a2ui.md)
- [Function Tools](function-tools.md) — Creating custom tools

---

**Previous**: [← Browser Tools](browser-tools.md) | **Next**: [MCP Tools →](mcp-tools.md)
