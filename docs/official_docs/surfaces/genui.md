# Agent Surfaces: Agent-Drawn UI

An agent draws forms, cards, tables and screens in a person's thread with the `genui` feature
of `adk-surface` (spec 3.3 §6.1, §7.4 rule 5). A drawing never decides anything: it carries no
approval controls and none of a decision's words, and a form's **Send** never runs a tool. It
asks for the call, the drawing agent holds it for the person, and the person decides on the
home's own card, as for any held call.

| A form Finance drew (light) | Sent and allowed (dark) |
|---|---|
| ![A form Finance drew](../images/surfaces/genui-form-light.png) | ![The form sent, its decision answered, dark](../images/surfaces/genui-done-dark.png) |

> **Note:** `genui` replaces the `adk-ui` crate, which is retired (spec 3.3 decision 11). See
> [Moving from adk-ui](#moving-from-adk-ui).

```toml
[dependencies]
adk-surface = { version = "3.0.0", features = ["web", "genui"] }
```

| Part | Feature | What it is |
|---|---|---|
| `DrawToolset` | `genui` | The render tools: `render_form`, `render_card`, `render_table`, `render_screen`, `patch_surface`, `delete_surface` |
| `Validator`, `Catalog::forms` | `genui` | Rule 5, and the `forms` 1.0 catalog the tools draw in |
| The A2UI v0.9.1 model (`A2uiMessage`, `encode_jsonl`, `decode_jsonl`) | `genui` | The wire format a drawing is stored in |
| The thread items `drawn`, `drawnRefused` and `drawnSent`, and the `submitDrawn` command | every `web` build | The page and the OpenAPI document are the same on every server; one built without `genui` never projects a drawing and answers `submitDrawn` with `surface.unsupported` |
| `GET /api/surface/genui/capabilities`, `GET /api/surface/genui/sessions/{session}/forms` | `web` and `genui` | What the home claims of A2UI, AG-UI and MCP Apps, and each sent form's state |
| The terminal's box and fill-in pane | `tui` and `genui` | See [In the terminal](#in-the-terminal) |

`genui` pulls in no further `adk-*` crate.

## In `adk-rust`

`adk-cli`'s `genui` feature turns it on for `adk-rust serve` and `adk-rust up`:

```bash
cargo install adk-cli --features genui
```

| Command | What `genui` adds |
|---|---|
| `adk-rust serve` | The render tools on the harness, the routes, and "Show forms and tables in your threads — sending asks you first" under what the agent can do |
| `adk-rust up` | The same for every harness the kernel hosts: the primary agent in every area and every agent a person adds (wave 4 decision 14) |

## Drawing

Give an agent the render tools as a toolset:

```rust,ignore
use std::sync::Arc;

use adk_harness::{HarnessAgent, HarnessMode};
use adk_surface::genui::DrawToolset;

let finance = HarnessAgent::builder("finance")
    .model(model)
    .mode(HarnessMode::Ask)
    .tool(Arc::new(send_invoice))
    .toolset(Arc::new(DrawToolset::forms()))
    .build()?;
```

`LlmAgentBuilder::toolset` takes the same toolset. The render tools are read-only and safe to
run side by side, so they need no approval in any mode.

| Tool | Draws |
|---|---|
| `render_form` | A title, fields (`text`, `longText`, `number`, `email`, `date`, `checkbox`, `choice`) and actions |
| `render_card` | A title and label and value rows |
| `render_table` | A title, column headings and rows of text or numbers |
| `render_screen` | A2UI v0.9.1 components of the `forms` catalog, one with id `root`, a data model and actions |
| `patch_surface` | The next version of a surface: data model changes, components added or replaced by id, components removed |
| `delete_surface` | Removes a surface from the thread |

A form's action names one of the agent's own tools and its arguments, each a constant or a
field's value:

```json
{
  "surfaceId": "invoice-10",
  "title": "Invoice INV-10",
  "fields": [
    { "name": "to", "label": "To", "type": "email", "required": true },
    { "name": "amount", "label": "Amount", "type": "number", "value": 4200 }
  ],
  "actions": [{
    "id": "send",
    "label": "Send",
    "tool": "send_invoice",
    "args": { "to": { "path": "/to" }, "amount": { "path": "/amount" }, "invoice": "INV-10" }
  }]
}
```

Drawing a surface again under its id makes its next version. The thread collapses an earlier
version to "Earlier version · 10:02" and says "Finance changed this form at 10:05" over the
new one.

### What a drawing may not contain

Every drawing is checked on the server before it is shown, on the strings a person reads: its
bindings, `formatString` and `pluralize` are resolved first, and a later `patch_surface` is
checked as a whole surface again.

| Refused | Code | The thread says |
|---|---|---|
| A decision's phrases anywhere — "Allow once", "Always allow", "Waiting for you", "You said yes", "You said no", "Confirm it's you", "Stop all", "Resume all", "Take over" — after folding case, look-alike and zero-width characters | `genui.refused.looks_like_decision` | "Finance tried to show something that looks like a decision. It wasn't shown." |
| A decision's single words — Edit, Deny, Done, Approve, Approved, Decided — as a whole label or heading; "Edit address" is shown (wave 4 decision 20) | `genui.refused.looks_like_decision` | The same |
| A password or other secret field | `genui.refused.asks_for_secret` | "Finance asked for a password in a form. Use Take over for passwords." |
| A component outside the catalog, a link, an undeclared action, an action naming a render tool or a kernel tool, more than the limits (`MAX_COMPONENTS`, `MAX_DEPTH`, `MAX_TEXT_CHARS`, `MAX_DRAWING_BYTES`, `MAX_ACTIONS`) | `genui.refused.cannot_show` | "Finance tried to show something this home can't show." |

A refused drawing is a tool error: the agent reads why and what to draw instead. The thread
shows the plain notice above, and the technical reason — "catalog forms 1.0 · component c7
Button · label matches reserved word "Allow once" after normalisation" — stays in the
developer panel's **Protocols** tab.

The `forms` 1.0 catalog is the A2UI v0.9.1 basic catalog without `Image`, `Icon`, `Modal`,
`Video` and `AudioPlayer`: `Text`, `Row`, `Column`, `List`, `Card`, `Tabs`, `Divider`, `Button`,
`TextField`, `CheckBox`, `ChoicePicker`, `Slider` and `DateTimeInput`.

### Only a render tool draws

A render tool records each drawing it makes in the event's state, under
`adk.surface.drawn` (`surfaceId`, `version` and the digest of the drawing). The thread shows
a drawing only when its event carries that record and the digest matches; a tool of the same
name without it, such as an MCP server's `render_form`, is shown as an ordinary tool result.
The web home and the full-screen terminal show a drawing in place of its render call, with no
"Render form" step.

## Sending a form

**Send** checks each field as the server will, then posts the version the person saw:

```json
{
  "surface": "web",
  "origin": { "kind": "pointer" },
  "command": {
    "type": "submitDrawn",
    "sessionId": "s1",
    "surfaceId": "invoice-10",
    "version": 1,
    "actionId": "send",
    "values": { "/to": "ana@example.com", "/amount": 4200 }
  }
}
```

The engine checks, in order:

1. The session is the caller's; another person's answers `404` (S-I8).
2. The caller is a person at a screen: origin `keyboard`, `pointer` or `touch`. An agent, chat
   or voice answers `403` with `surface.not_here`.
3. The version is the surface's latest: an older one answers `409` with
   `surface.drawn_changed`, and the form reads "Couldn't send: this form changed — check it and
   send again".
4. The action is declared on that version, and the form was not sent already (`surface.invalid`).
5. Each value fits its field's type and checks (`surface.invalid`).

The arguments come from the stored drawing: a constant such as `"invoice": "INV-10"` is
never read from the request. The engine writes a `surface.drawn.submitted` audit record —
`sessionId`, `surfaceId`, `version`, `actionId`, `tool`, the call's `fingerprint`,
`valuesDigest`, `callId` and `drawnBy`, never the values — and then asks for the call
(`Source::request_call`). The answer is `formSent` with the call's ID; nothing has run.

### Held as the drawing agent

The call is an `adk_core::RequestedCall`, held for the person as the agent that drew the form,
inside that agent's own runtime:

| Source | Where the call is held |
|---|---|
| `LocalSource` over a kernel | A durable run in the target of the session's latest run — an area's or an added agent's — parked until the decision wakes it on any surface |
| `LocalSource` over a harness | `Harness::send_requested` |
| `ControlSource` | A run spawned over the kernel's control plane, so the terminal attached to `adk-rust up` sends forms too |

The agent authorizes the call under its own policy, standing rules, acting context and
approval store, as `GovernedCall::requested`, and the run ends without a model call. The
model reads the call as the person's request and its answer as the person's line, never as a
call it made itself. See
[Calls a Person Asks For](../security/tool-authorization.md#calls-a-person-asks-for).

| Situation | What happens |
|---|---|
| Go ahead, a standing rule allowing the tool, an earlier approval of the identical call, or a confirmation handler | It still waits for the person: pressing **Send** is never the approval (wave 4 decision 12) |
| The agent's policy denies the tool | Nothing is held and no inbox entry is made; the form reads "Finance can't use send_invoice here" and can be changed and sent again |
| A rule tightened before the person answers | Refused when it resumes: "Not run: Finance can't use send_invoice any more" |
| **Allow once** | The parked run wakes in its own target and the tool runs once, with the arguments the card showed |

The decision's card is the ordinary card, with "How it got here" starting "You pressed Send
on a form Finance drew · 10:03" and the line "From a form Finance drew". It offers **Allow
once**, **Edit** and **Deny**, never **Always allow**: a standing rule would let the model call
the tool unasked, which is not what the person decided on a form. A high-risk call still needs
"Confirm it's you".

| Waiting (light) | Waiting (dark) |
|---|---|
| ![The sent form and its card](../images/surfaces/genui-card-light.png) | ![The sent form and its card, dark](../images/surfaces/genui-card-dark.png) |

### The form after it is sent

The form turns read-only, its decision is drawn under it, and it says where the call is.
`GET /api/surface/genui/sessions/{session}/forms` returns the same, and
`adk_surface::genui::form_states` computes it from a thread.

| Sent call | The form says |
|---|---|
| Held, its decision open | "Sent at 10:03 · waiting for you" |
| Answered yes, still running | "Sent at 10:03" |
| Ran | "Sent · done at 10:04" |
| Ran and failed | "Sent · failed" |
| Answered no | "Sent · you said no" |
| Refused by the agent's rules before it was held | "Finance can't use send_invoice here" |
| Refused when it resumed | "Not run: Finance can't use send_invoice any more" |
| Ended without an answer | "Ended without an answer" |

## In the web home

A drawing sits on the card background with the agent and time over it — "Drawn by Finance ·
10:02" — and never in a decision's colours or pills. Its strings are text: no markup, no
control characters, and a link is never followed. A table shows ten rows and **Show all 42**.
Under a form's buttons, "Sending runs `send_invoice` and asks you first".

| Cards and tables (light) | Refused (dark) |
|---|---|
| ![A card and a table Finance drew](../images/surfaces/genui-display-light.png) | ![A refused drawing's notice, dark](../images/surfaces/genui-refused-dark.png) |

The developer panel's **Protocols** tab shows `GET /api/surface/genui/capabilities` — each
claim with its status and scope, the catalog and what it leaves out — and the session's refused
drawings with their technical reasons.

![The Protocols tab](../images/surfaces/genui-protocols-light.png)

| Protocol | Claim | Status |
|---|---|---|
| A2UI (`a2ui`) | Version 0.9.1 | Supported |
| A2UI | Version 1.0 | Candidate |
| A2UI | Trusted component catalog; no approval controls in drawn UI | Supported (security) |
| AG-UI (`ag_ui`) | a2ui-surface activity; interrupts for a drawn submit | Planned |
| MCP Apps (`mcp_apps`) | Version 2026-01-26, sandbox and CSP, governed app tool calls | Planned |

The phone app shows the decision a form asks for, as any decision; it has no thread view, so the drawing itself is shown in the web home and the terminal. Chat
cards and push notifications never carry a drawing.

## In the terminal

The full-screen terminal draws a drawing as a bordered box in the normal border colour, with
"drawn by Finance · forms · rev 1" and the fields as aligned pairs. **ctrl-f** opens the
thread's latest form in the right pane (wave 4 decision 19):

| Key | In the fill-in pane |
|---|---|
| Tab, ↑, ↓ | Move between fields |
| Any character, including `y`, `n`, `e` and `a` | Types into the field; no key answers a decision here |
| Space | Toggles a checkbox, moves to the next choice |
| Enter | Checks the fields and shows the call: `send_invoice(to: "ana@example.com", amount: 4200, invoice: "INV-10") · asks you first` |
| Enter again | Sends it |
| Esc | Back to the fields, then out of the pane |

`--ui inline` prints a drawing into the scrollback as a plain block. A refused drawing is its
notice, with the technical reason on a muted second line. `--output json` adds three line types:

| `type` | Fields |
|---|---|
| `drawn` | `session`, `surface`, `version`, `by`, `catalog`, `kind`, `title`, `deleted` |
| `drawn.submitted` | `session`, `surface`, `version`, `action`, `tool`, `held`, `decision` |
| `drawn.refused` | `session`, `by`, `reason`, `component`, `detail` |

Every string passes the terminal's escape filter.

## Mounting it on your own server

```rust,ignore
use std::sync::Arc;

use adk_server::ServerBuilder;
use adk_surface::{GrantedUse, SurfaceEngine, words};

let engine = SurfaceEngine::builder(Arc::new(source))
    .grant(GrantedUse::new("genui", words::SHOW_FORMS_AND_TABLES))
    .build();
let app = ServerBuilder::new(config)
    .add_api_routes(adk_kernel::control::router(kernel))
    .add_api_routes(adk_surface::web::router(engine.clone()))
    .add_api_routes(adk_surface::web::genui::router(engine))
    .add_root_routes(adk_surface::web::home())
    .build();
```

`examples/scripted_genui.rs` is a complete home over a kernel and a scripted model: Finance
draws the invoice form, then a screen with an "Allow once" button, which is refused.

```bash
cargo run -p adk-surface --features web,genui --example scripted_genui -- 8805 /tmp/genui-home
```

## Moving from adk-ui

`adk-ui` 2.2.0 is built on `adk-core` 2, so an ADK-Rust 3 agent cannot register its tools.
The render tools of `genui` take their place:

| `adk-ui` | `genui` |
|---|---|
| `render_form` | `render_form`; an action names one of the agent's tools and asks the person before it runs |
| `render_card` | `render_card` |
| `render_table` | `render_table` |
| `render_screen`, `render_page`, `render_layout` | `render_screen`, with the `forms` catalog's components |
| `render_confirm` | None: a drawing never asks for a yes. A call that needs one is held by the agent's policy or `require_tool_confirmation`, and decided on the home's card |
| `render_alert`, `render_toast` | The agent's reply, or `render_card` with a note |
| `render_progress` | `render_card`, then `patch_surface` as the work moves |
| `render_chart` | `render_table`; charts are not in the catalog |
| `render_modal` | None: `Modal` is not in the catalog |
| `render_kit` | None: the home's catalog and tokens are fixed |
| `render_app` | None in this release; MCP Apps are planned |
| `save_surface`, `load_surface`, `list_surfaces` | None needed: every drawing is an event of the session, versioned by its surface id |
| `patch_surface`, `delete_surface` | `patch_surface`, `delete_surface` |
| `@zavora-ai/adk-ui-react` | The web home renders drawings; the terminal draws them as boxes |
| The `adk_ui` profile of `adk-server` | Still served for existing clients, deprecated, with a sunset target of 2026-12-31 (`GET /api/ui/capabilities`) |

The catalog id is `https://adk-rust.com/a2ui/catalogs/forms/1.0/catalog.json`, in place of
`zavora.ai:adk-ui/extended@0.2.0`.

## Related

- [The Engine](index.md) — the `submitDrawn` command, its error codes and its audit record
- [The Web Home](web-home.md) — threads, cards and the developer panel
- [The Terminal](terminal.md) — full screen, inline and JSON lines
- [Tool Authorization](../security/tool-authorization.md#calls-a-person-asks-for) — requested calls and how they are held
- [Scenarios](scenarios.md#governed-form) — the governed form, end to end
- [UI Tools](../tools/ui-tools.md) — the render tools and the server's UI protocol profiles
