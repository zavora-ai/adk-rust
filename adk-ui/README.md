# adk-ui

Dynamic UI generation for AI agents. Enables agents to render rich user interfaces through tool calls.

[![Crates.io](https://img.shields.io/crates/v/adk-ui.svg)](https://crates.io/crates/adk-ui)
[![Documentation](https://docs.rs/adk-ui/badge.svg)](https://docs.rs/adk-ui)
[![License](https://img.shields.io/crates/l/adk-ui.svg)](LICENSE)

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
adk-ui = "0.3.1"
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

All 13 tools support protocol-aware output selection through the `protocol` argument.

Supported protocol values:

- `a2ui`
- `ag_ui`
- `mcp_apps`

### Output Matrix

| Tool | Default (`protocol` omitted) | `protocol="a2ui"` | `protocol="ag_ui"` | `protocol="mcp_apps"` |
|------|-------------------------------|-------------------|--------------------|-----------------------|
| `render_screen` | A2UI surface payload object (`jsonl`, `components`, `data_model`) | Same A2UI payload object | AG-UI adapter payload (`events`) | MCP Apps adapter payload (`resource`, `_meta.ui.resourceUri`) |
| `render_page` | A2UI JSONL string | A2UI JSONL string | AG-UI adapter payload (`events`) | MCP Apps adapter payload (`resource`, `_meta.ui.resourceUri`) |
| `render_kit` | Kit artifact JSON (`catalog`, `tokens`, `templates`, `theme_css`) | Wrapped payload `{ protocol, surface_id, payload }` | Wrapped payload `{ protocol, surface_id, payload }` | Wrapped payload `{ protocol, surface_id, payload }` |
| `render_form` | Legacy `UiResponse` JSON (`components`) | Protocol envelope (`protocol`, `version`, `surface_id`, `jsonl`) | Protocol envelope (`payload.events`) | Protocol envelope (`payload.payload.resource`) |
| `render_card` | Legacy `UiResponse` JSON (`components`) | Protocol envelope (`protocol`, `version`, `surface_id`, `jsonl`) | Protocol envelope (`payload.events`) | Protocol envelope (`payload.payload.resource`) |
| `render_alert` | Legacy `UiResponse` JSON (`components`) | Protocol envelope (`protocol`, `version`, `surface_id`, `jsonl`) | Protocol envelope (`payload.events`) | Protocol envelope (`payload.payload.resource`) |
| `render_confirm` | Legacy `UiResponse` JSON (`components`) | Protocol envelope (`protocol`, `version`, `surface_id`, `jsonl`) | Protocol envelope (`payload.events`) | Protocol envelope (`payload.payload.resource`) |
| `render_table` | Legacy `UiResponse` JSON (`components`) | Protocol envelope (`protocol`, `version`, `surface_id`, `jsonl`) | Protocol envelope (`payload.events`) | Protocol envelope (`payload.payload.resource`) |
| `render_chart` | Legacy `UiResponse` JSON (`components`) | Protocol envelope (`protocol`, `version`, `surface_id`, `jsonl`) | Protocol envelope (`payload.events`) | Protocol envelope (`payload.payload.resource`) |
| `render_layout` | Legacy `UiResponse` JSON (`components`) | Protocol envelope (`protocol`, `version`, `surface_id`, `jsonl`) | Protocol envelope (`payload.events`) | Protocol envelope (`payload.payload.resource`) |
| `render_progress` | Legacy `UiResponse` JSON (`components`) | Protocol envelope (`protocol`, `version`, `surface_id`, `jsonl`) | Protocol envelope (`payload.events`) | Protocol envelope (`payload.payload.resource`) |
| `render_modal` | Legacy `UiResponse` JSON (`components`) | Protocol envelope (`protocol`, `version`, `surface_id`, `jsonl`) | Protocol envelope (`payload.events`) | Protocol envelope (`payload.payload.resource`) |
| `render_toast` | Legacy `UiResponse` JSON (`components`) | Protocol envelope (`protocol`, `version`, `surface_id`, `jsonl`) | Protocol envelope (`payload.events`) | Protocol envelope (`payload.payload.resource`) |

Example args:

```json
{
  "protocol": "mcp_apps",
  "mcp_apps": {
    "resource_uri": "ui://demo/surface"
  }
}
```

`adk-ui` now includes matrix coverage tests for all `13 x 3` tool/protocol combinations in `adk-ui/tests/tool_protocol_matrix_tests.rs`.

Migration guidance for legacy/default outputs is documented in `adk-ui/docs/PROTOCOL_MIGRATION.md`.

### Deprecation Timeline

Legacy runtime profile `adk_ui` now carries explicit deprecation metadata:

- announced: `2026-02-07`
- sunset target: `2026-12-31`
- replacements: `a2ui`, `ag_ui`, `mcp_apps`

This metadata is exposed through shared capability constants (`UI_PROTOCOL_CAPABILITIES`) and surfaced by `adk-server` in `/api/ui/capabilities`.

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
| `ui_protocol_profiles` | 13x3 protocol output coverage demo | `cargo run --example ui_protocol_profiles` |
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
