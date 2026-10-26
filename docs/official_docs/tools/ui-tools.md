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
        "answerable": true,
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
| `id` | The inbox entry's ID; for a call with no inbox entry, the call ID |
| `toolCallId` | The held call, whose `TOOL_CALL_START`, `TOOL_CALL_ARGS` and `TOOL_CALL_END` the run already sent |
| `metadata.summary`, `risk`, `version` | Drawn from the inbox entry, never from agent output |
| `metadata.agent` | The acting agent's name, from the identity the inbox entry records (`finance` for `urn:adk:agent:finance/finance`) |
| `metadata.answerable` | Whether a `resume` from this caller can decide it: `true` when the entry offers it at least one answer |
| `metadata.arguments` | The arguments the call runs with — another approver's edit in place of the held ones — only when the caller authenticated through the server's `RequestContextExtractor`, so it is the session's owner |
| `responseSchema` | The answers this caller may give the entry: `approved` carries a `const` when only a yes or only a no is allowed, `editedArgs` appears only when Edit is allowed and `reason` only when Deny is; absent when no answer is |
| `expiresAt` | The inbox entry's expiry |

An interrupt offers no answer — `metadata.answerable: false` and no `responseSchema` — when
no resume from this caller can decide it: a call with no inbox entry (no approval service,
or a service that leaves the call held in its store only, such as `StoreApprovalService`
without a router), a server without a `RequestContextExtractor`, or an entry whose policy
does not let the session's owner decide it, such as the default policy, which forbids
approving one's own call. It is decided where it is held or by its approvers. A server
with an approval service accepts it answered `cancelled`; one without accepts no `resume`
at all. The held call's placeholder result and the hold's placeholder text are not sent.

An AG-UI front end cannot confirm that it is the person, so it answers as the surfaces of
spec 3.3 §7.2 that cannot: a call whose inbox entry is high or critical risk, or one that
spends money (a `payments_*` tool), is offered and accepted only as a denial. It is
approved where the person can confirm it is them, such as the web home after a passkey
check. An approval sent anyway is refused with `surface.answer_not_allowed`.

If the inbox cannot be read when the run ends, the stream ends with `RUN_ERROR` carrying
the store's error code instead of an interrupt that would say the call cannot be decided
here; the call keeps waiting in the inbox.

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

A resume is all or nothing. Every entry is checked against its inbox entry first, as
`ApprovalService::decide` would check it, by the authenticated caller as
`human(<user id>)` at the version the client echoes in `metadata.version`, or else the
version read then — unless another approver's edit has replaced the arguments since,
which is a version conflict, so a yes never approves arguments the person was not shown.
The answers are recorded only once the rest of the request is valid, the session and the
agent are loaded, and the server is not frozen, just before the run starts. The run is a
blank turn that resumes the calls the decisions allow; their results arrive as
`TOOL_CALL_RESULT` against the original `toolCallId`. One resume of a thread runs at a
time: a resume sent while an earlier one's run is still going is refused with
`approval.resume_in_progress`, so two runs never resume the same call.

| Entry | Effect |
|-------|--------|
| `resolved`, the inbox entry pending | Decided when the run starts |
| `resolved`, the inbox entry already resolved and its call not yet run — decided on another surface or by this resume sent before, expired, or withdrawn | Nothing is recorded; the run starts and the call follows that outcome, so an expired or withdrawn call is answered as denied. Another answer — another verdict, or a yes with other `editedArgs` — is reported with `code` and `message` |
| `resolved`, the inbox entry's call already run, the entry past its expiry but not yet expired by the approval service, or changed since it was shown | The whole resume is refused and nothing is decided |
| `cancelled` | Nothing is decided: the call keeps waiting in the inbox |
| Naming no inbox entry of the run's session | Reported as `unknown` and skipped |

Each answered entry is reported after `RUN_STARTED` as a `CUSTOM` event named
`adk.decision`, with `interruptId`, `state` (`approved`, `rejected`, `responded`,
`pending`, `expired`, `cancelled`, or `unknown`), `tool`, `version` — the version to
answer at — `editedArgs` when another approver's edit replaced the call's arguments, and,
once decided, `decidedBy` and `decidedAt`; an answer not recorded also carries `code` and
`message`. A refused resume is reported, never retried: the stream is
`RUN_STARTED`, the `adk.decision` updates, and `RUN_ERROR` with the refusal's code, such
as `approval.already_resolved` with the message "Already decided by alice" or
`approval.version_conflict` after an escalation changed the entry, and the run does not
start. Answering again at the reported `version` decides the entry.

| Request | Answer |
|---------|--------|
| `resume` without an approval service, or outside the `ag_ui` profile's `protocol_native` transport | `400` |
| `resume` without an authenticated caller | `401` |
| A `resolved` entry whose `payload` is not one of the answers above, or one interrupt answered twice | `400`, and nothing is decided |
| Any other invalid request, such as a `stateDelta` that is not an object | Its error, and nothing is decided |
| A frozen server | `403` (`governance.frozen`), and nothing is decided |
| A resume of a thread whose earlier resume's run is still going | `RUN_ERROR` (`approval.resume_in_progress`), and nothing is decided |

Every answer a resume gives is written to the audit trail attached to the server's
`GovernanceControl` (`GovernanceControl::attach_audit`), as the surfaces of spec 3.3 record
theirs: a `surface.decide` record with the caller, the inbox entry, the answer and its
`outcome` (`decided`, `already_decided`, `changed`, or `refused` with its `code`),
`surface: "ag_ui"`, and `origin: {"kind": "app", "channel": "another app"}`, so "What your
agents did" says "You said yes to send_invoice, from another app".

`GET /api/ui/capabilities` lists `interrupt_resume` for `ag_ui` only on a server with an
approval service and a `RequestContextExtractor`; `tool_call_interrupts` is listed on
every server.

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
