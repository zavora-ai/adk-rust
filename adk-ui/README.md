# adk-ui

Dynamic UI generation for AI agents. Enables agents to render rich user interfaces through tool calls.

## Features

- **28 Component Types**: Text, buttons, forms, tables, charts, modals, toasts, and more
- **10 Render Tools**: High-level tools for common UI patterns
- **10 Pre-built Templates**: Registration, login, dashboard, settings, and more
- **Bidirectional Data Flow**: Forms submit data back to agents via `UiEvent`
- **Streaming Updates**: Patch components by ID with `UiUpdate`
- **Server-side Validation**: Catch malformed responses before they reach the client
- **Type-Safe**: Full Rust schema with TypeScript types for React client

## Quick Start

```toml
[dependencies]
adk-ui = "{{version}}"
```

```rust
use adk_ui::{UiToolset, UI_AGENT_PROMPT};
use adk_agent::LlmAgentBuilder;

// Add all 10 UI tools to an agent with the tested system prompt
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
Agent ──[render_* tool]──> UiResponse ──[SSE]──> React Client
             ↑                                        │
             └────────── UiEvent <────────────────────┘
```

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
