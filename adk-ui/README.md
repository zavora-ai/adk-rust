# adk-ui

Dynamic UI generation for AI agents. Enables agents to render rich user interfaces through tool calls.

## Features

- **30 Component Types**: Text, buttons, forms, tables, charts, modals, toasts, and more
- **13 Render Tools**: High-level tools for common UI patterns, including protocol-aware screen/page emitters
- **10 Pre-built Templates**: Registration, login, dashboard, settings, and more
- **Bidirectional Data Flow**: Forms submit data back to agents via `UiEvent`
- **Streaming Updates**: Patch components by ID with `UiUpdate`
- **Server-side Validation**: Catch malformed responses before they reach the client
- **Type-Safe**: Full Rust schema with TypeScript types for React client
- **Protocol Interop**: Emit UI payloads as `a2ui`, `ag_ui`, or MCP Apps (`mcp_apps`)

## Quick Start

```toml
[dependencies]
adk-ui = "0.2.0"
```

```rust
use adk_ui::{UiToolset, UI_AGENT_PROMPT};
use adk_agent::LlmAgentBuilder;

// Add all 13 UI tools to an agent with the tested system prompt
let tools = UiToolset::all_tools();
let mut builder = LlmAgentBuilder::new("assistant")
    .model(model)
    .instruction(UI_AGENT_PROMPT);  // Tested prompt for reliable tool usage

for tool in tools {
    builder = builder.tool(tool);
}

let agent = builder.build()?;
```

## Modules

### Prompts (`prompts.rs`)

Tested system prompts for reliable LLM tool usage:

```rust
use adk_ui::{UI_AGENT_PROMPT, UI_AGENT_PROMPT_SHORT};

// UI_AGENT_PROMPT includes:
// - Critical rules for tool usage
// - Tool selection guide
// - Few-shot examples with JSON parameters
```

### Templates (`templates.rs`)

Pre-built UI patterns:

```rust
use adk_ui::{render_template, UiTemplate, TemplateData};

let response = render_template(UiTemplate::Registration, TemplateData::default());
```

Templates: `Registration`, `Login`, `UserProfile`, `Settings`, `ConfirmDelete`, `StatusDashboard`, `DataTable`, `SuccessMessage`, `ErrorMessage`, `Loading`

### Validation (`validation.rs`)

Server-side validation:

```rust
use adk_ui::{validate_ui_response, UiResponse};

let result = validate_ui_response(&ui_response);
if let Err(errors) = result {
    eprintln!("Validation errors: {:?}", errors);
}
```

## Available Tools

| Tool | Description |
|------|-------------|
| `render_screen` | Emit protocol-aware surface payloads (`a2ui`, `ag_ui`, `mcp_apps`) from component definitions |
| `render_page` | Build section-based pages and emit protocol-aware payloads |
| `render_kit` | Generate A2UI kit/catalog payload artifacts |
| `render_form` | Collect user input with forms (text, email, password, textarea, select, etc.) |
| `render_card` | Display information cards with actions |
| `render_alert` | Show notifications and status messages |
| `render_confirm` | Request user confirmation |
| `render_table` | Display tabular data with sorting and pagination |
| `render_chart` | Create bar, line, area, and pie charts with legend/axis labels |
| `render_layout` | Build dashboard layouts with 8 section types |
| `render_progress` | Show progress indicators |
| `render_modal` | Display modal dialogs |
| `render_toast` | Show temporary toast notifications |

## Streaming Updates

Update specific components by ID without re-rendering:

```rust
use adk_ui::{UiUpdate, Component, Progress};

let update = UiUpdate::replace(
    "progress-bar",
    Component::Progress(Progress {
        id: Some("progress-bar".to_string()),
        value: 75,
        label: Some("75%".to_string()),
    }),
);
```

## Protocol Outputs

All render tools support protocol-aware output selection.

Protocol behavior:

- `render_screen` / `render_page`: default to `a2ui`
- legacy render tools (`render_form`, `render_card`, `render_alert`, etc.): default to legacy `adk_ui` payloads unless `protocol` is set

Supported protocol values:

- `a2ui` (default): A2UI JSONL payloads
- `ag_ui`: AG-UI event stream (wrapped as `RUN_STARTED` -> `CUSTOM` -> `RUN_FINISHED`)
- `mcp_apps`: MCP Apps payload with `ui://` resource + `_meta.ui.resourceUri` linkage

Example tool args:

```json
{
  "protocol": "mcp_apps",
  "mcp_apps": {
    "resource_uri": "ui://demo/surface"
  }
}
```

Protocol responses are emitted with a standard envelope shape:

```json
{
  "protocol": "a2ui",
  "version": "1.0",
  "surface_id": "main",
  "components": [],
  "data_model": {},
  "jsonl": "..."
}
```

For `ag_ui` the payload includes `events`; for `mcp_apps` it includes `payload`.

## Interop Adapters

`adk-ui` includes adapter primitives for protocol conversion from canonical surfaces:

- `A2uiAdapter`
- `AgUiAdapter`
- `McpAppsAdapter`

These adapters implement a shared `UiProtocolAdapter` trait and are used by render tools to avoid per-tool protocol conversion drift.

## React Client

Install the npm package:

```bash
npm install @zavora-ai/adk-ui-react
```

```tsx
import { Renderer } from '@zavora-ai/adk-ui-react';
import type { UiResponse, UiEvent } from '@zavora-ai/adk-ui-react';
```

Or use the reference implementation in `examples/ui_react_client/`.

## Examples

| Example | Description | Command |
|---------|-------------|---------|
| `ui_agent` | Console demo | `cargo run --example ui_agent` |
| `ui_server` | HTTP server with SSE | `cargo run --example ui_server` |
| `streaming_demo` | Real-time progress updates | `cargo run --example streaming_demo` |
| `ui_react_client` | React frontend | `cd examples/ui_react_client && npm run dev` |

## Architecture

```
Agent ──[render_screen/render_page]──> protocol payload (`a2ui` | `ag_ui` | `mcp_apps`)
                 ↑
                 └────────── UiEvent / action feedback loop
```

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
