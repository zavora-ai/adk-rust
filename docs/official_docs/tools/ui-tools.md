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

## Related

- [A2UI quickstart](../quickstart-a2ui.md)
- [Function Tools](function-tools.md) — Creating custom tools

---

**Previous**: [← Browser Tools](browser-tools.md) | **Next**: [MCP Tools →](mcp-tools.md)
