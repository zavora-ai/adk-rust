# UI Tools

The `adk-ui` crate enables AI agents to dynamically generate rich user interfaces through tool calls. Agents can render forms, cards, alerts, tables, charts, and more - all through a type-safe Rust API that serializes to JSON for frontend consumption.

## What You'll Build

![ADK UI Agent](images/adk-ui.jpg)

**Key Concepts:**
- **Forms** - Collect user input with various field types
- **Cards** - Display information with action buttons
- **Tables** - Present structured data in rows/columns
- **Charts** - Visualize data with bar, line, area, pie charts
- **Alerts** - Show notifications and status messages
- **Modals** - Confirmation dialogs and focused interactions
- **Toasts** - Brief status notifications

### Example: Analytics Dashboard

![ADK UI Analytics](images/adk-ui-agent-analytics.jpg)

### Example: Registration Form

![ADK UI Registration](images/adk-ui-register.jpg)

---

## How It Works

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ STEP 1: User requests something                                             │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│   User: "I want to register for an account"                                 │
│                                                                             │
│                              ↓                                              │
│                                                                             │
│   ┌──────────────────────────────────────┐                                  │
│   │         AI AGENT (LLM)              │                                  │
│   │  "I should show a registration form" │                                  │
│   └──────────────────────────────────────┘                                  │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────────────────┐
│ STEP 2: Agent calls render_form tool                                        │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│   📞 Tool Call: render_form({                                               │
│     title: "Registration",                                                  │
│     fields: [                                                               │
│       {name: "email", type: "email"},                                       │
│       {name: "password", type: "password"}                                  │
│     ]                                                                       │
│   })                                                                        │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────────────────┐
│ STEP 3: Frontend renders the form                                           │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│   ┌─────────────────────────────────────────────────┐                       │
│   │  📋 Registration                               │                       │
│   │  ────────────────────────────────────────────  │                       │
│   │  Email:    [________________________]          │                       │
│   │  Password: [________________________]          │                       │
│   │                                                │                       │
│   │  [           Register           ]              │                       │
│   └─────────────────────────────────────────────────┘                       │
│                                                                             │
│   ✅ User sees an interactive form, fills it out, clicks Register           │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────────────────┐
│ STEP 4: Form submission sent back to agent                                  │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│   📩 Event: {                                                               │
│     type: "form_submit",                                                    │
│     data: { email: "user@example.com", password: "***" }                    │
│   }                                                                         │
│                                                                             │
│   Agent: "Great! I'll process your registration and show a success alert"  │
│                                                                             │
│   📞 Tool Call: render_alert({                                              │
│     title: "Registration Complete!",                                        │
│     variant: "success"                                                      │
│   })                                                                        │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Overview

UI tools allow agents to:

- Collect user input through dynamic forms with textarea support
- Display information with cards, alerts, and notifications
- Present data in tables and interactive charts (Recharts)
- Show progress and loading states (spinner, skeleton)
- Create dashboard layouts with multiple components
- Request user confirmation via modals
- Display toast notifications for status updates

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
adk-rust = { version = "0.2.0", features = ["ui"] }
# Or use individual crates:
adk-ui = "0.2.0"
adk-agent = "0.2.0"
adk-model = "0.2.0"
```

### Basic Usage

```rust
use adk_rust::prelude::*;
use adk_rust::ui::UiToolset;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = Arc::new(GeminiModel::from_env("gemini-2.0-flash")?);

    // Get all 10 UI tools
    let ui_tools = UiToolset::all_tools();

    // Create AI agent with UI tools
    let mut builder = LlmAgentBuilder::new("ui_agent")
        .model(model)
        .instruction(r#"
            You are a helpful assistant that uses UI components to interact with users.
            Use render_form for collecting information.
            Use render_card for displaying results.
            Use render_alert for notifications.
            Use render_modal for confirmation dialogs.
            Use render_toast for brief status messages.
        "#);

    for tool in ui_tools {
        builder = builder.tool(tool);
    }

    let agent = builder.build()?;
    Ok(())
}
```

## Available Tools

### render_form

Render interactive forms to collect user input.

```json
{
  "title": "Registration Form",
  "description": "Create your account",
  "fields": [
    {"name": "username", "label": "Username", "type": "text", "required": true},
    {"name": "email", "label": "Email", "type": "email", "required": true},
    {"name": "password", "label": "Password", "type": "password", "required": true},
    {"name": "newsletter", "label": "Subscribe to newsletter", "type": "switch"}
  ],
  "submit_label": "Register"
}
```

**Renders as:**
```
┌─────────────────────────────────────────────────┐
│  📋 Registration Form                          │
│  Create your account                           │
│  ─────────────────────────────────────────────  │
│                                                │
│  Username *                                    │
│  ┌─────────────────────────────────────────┐   │
│  │                                         │   │
│  └─────────────────────────────────────────┘   │
│                                                │
│  Email *                                       │
│  ┌─────────────────────────────────────────┐   │
│  │                                         │   │
│  └─────────────────────────────────────────┘   │
│                                                │
│  Password *                                    │
│  ┌─────────────────────────────────────────┐   │
│  │ ••••••••                                │   │
│  └─────────────────────────────────────────┘   │
│                                                │
│  Subscribe to newsletter  [○]                  │
│                                                │
│  ┌─────────────────────────────────────────┐   │
│  │             Register                    │   │
│  └─────────────────────────────────────────┘   │
│                                                │
└─────────────────────────────────────────────────┘
```

**Field types**: `text`, `email`, `password`, `number`, `date`, `select`, `multiselect`, `switch`, `slider`, `textarea`

### render_card

Display information cards with optional action buttons.

```json
{
  "title": "Order Confirmed",
  "description": "Order #12345",
  "content": "Your order has been placed successfully. Expected delivery: Dec 15, 2025.",
  "actions": [
    {"label": "Track Order", "action_id": "track", "variant": "primary"},
    {"label": "Cancel", "action_id": "cancel", "variant": "danger"}
  ]
}
```

**Button variants**: `primary`, `secondary`, `danger`, `ghost`, `outline`

### render_alert

Show notifications and status messages.

```json
{
  "title": "Payment Successful",
  "description": "Your payment of $99.00 has been processed.",
  "variant": "success"
}
```

**Variants**: `info`, `success`, `warning`, `error`

### render_confirm

Request user confirmation before actions.

```json
{
  "title": "Delete Account",
  "message": "Are you sure you want to delete your account? This action cannot be undone.",
  "confirm_label": "Delete",
  "cancel_label": "Keep Account",
  "variant": "danger"
}
```

### render_table

Display tabular data.

```json
{
  "title": "Recent Orders",
  "columns": [
    {"header": "Order ID", "accessor_key": "id"},
    {"header": "Date", "accessor_key": "date"},
    {"header": "Amount", "accessor_key": "amount"},
    {"header": "Status", "accessor_key": "status"}
  ],
  "data": [
    {"id": "#12345", "date": "2025-12-10", "amount": "$99.00", "status": "Delivered"},
    {"id": "#12346", "date": "2025-12-11", "amount": "$149.00", "status": "Shipped"}
  ]
}
```

### render_chart

Create data visualizations.

```json
{
  "title": "Monthly Sales",
  "chart_type": "bar",
  "x_key": "month",
  "y_keys": ["revenue", "profit"],
  "data": [
    {"month": "Jan", "revenue": 4000, "profit": 2400},
    {"month": "Feb", "revenue": 3000, "profit": 1398},
    {"month": "Mar", "revenue": 5000, "profit": 3800}
  ]
}
```

**Chart types**: `bar`, `line`, `area`, `pie`

### render_progress

Show task progress with optional steps.

```json
{
  "title": "Installing Dependencies",
  "value": 65,
  "description": "Installing package 13 of 20...",
  "steps": [
    {"label": "Download", "completed": true},
    {"label": "Extract", "completed": true},
    {"label": "Install", "current": true},
    {"label": "Configure", "completed": false}
  ]
}
```

### render_layout

Create dashboard layouts with multiple sections.

```json
{
  "title": "System Status",
  "description": "Current system health overview",
  "sections": [
    {
      "title": "Services",
      "type": "stats",
      "stats": [
        {"label": "API Server", "value": "Healthy", "status": "operational"},
        {"label": "Database", "value": "Degraded", "status": "warning"},
        {"label": "Cache", "value": "Down", "status": "error"}
      ]
    },
    {
      "title": "Recent Errors",
      "type": "table",
      "columns": [{"header": "Time", "key": "time"}, {"header": "Error", "key": "error"}],
      "rows": [{"time": "10:30", "error": "Connection timeout"}]
    }
  ]
}
```

**Section types**: `stats`, `table`, `chart`, `alert`, `text`

### render_modal

Display modal dialogs for confirmations or focused interactions.

```json
{
  "title": "Confirm Deletion",
  "message": "Are you sure you want to delete this item? This action cannot be undone.",
  "size": "medium",
  "closable": true,
  "confirm_label": "Delete",
  "cancel_label": "Cancel",
  "confirm_action": "delete_confirmed"
}
```

**Sizes**: `small`, `medium`, `large`, `full`

### render_toast

Show brief toast notifications for status updates.

```json
{
  "message": "Settings saved successfully",
  "variant": "success",
  "duration": 5000,
  "dismissible": true
}
```

**Variants**: `info`, `success`, `warning`, `error`

## Filtered Tools

Select only the tools your agent needs:

```rust
let toolset = UiToolset::new()
    .without_chart()      // Disable charts
    .without_table()      // Disable tables
    .without_progress()   // Disable progress
    .without_modal()      // Disable modals
    .without_toast();     // Disable toasts

// Or use forms only
let forms_only = UiToolset::forms_only();
```

## Handling UI Events

When users interact with rendered UI (submit forms, click buttons), events are sent back to the agent:

```rust
use adk_ui::{UiEvent, UiEventType};

// UiEvent structure
pub struct UiEvent {
    pub event_type: UiEventType,  // FormSubmit, ButtonClick, InputChange
    pub action_id: Option<String>,
    pub data: Option<HashMap<String, Value>>,
}

// Convert to message for agent
let message = ui_event.to_message();
```

## Streaming UI Updates

For real-time UI updates, use `UiUpdate` to patch components by ID:

```rust
use adk_ui::{UiUpdate, UiOperation};

let update = UiUpdate {
    target_id: "progress-bar".to_string(),
    operation: UiOperation::Patch,
    payload: Some(Component::Progress(Progress {
        id: Some("progress-bar".to_string()),
        value: 75,
        label: Some("75%".to_string()),
    })),
};
```

**Operations**: `Replace`, `Patch`, `Append`, `Remove`

## Component Schema

All 28 component types support optional `id` fields for streaming updates:

**Atoms**: Text, Button, Icon, Image, Badge
**Inputs**: TextInput, NumberInput, Select, MultiSelect, Switch, DateInput, Slider, Textarea
**Layouts**: Stack, Grid, Card, Container, Divider, Tabs
**Data**: Table, List, KeyValue, CodeBlock
**Visualization**: Chart (bar, line, area, pie via Recharts)
**Feedback**: Alert, Progress, Toast, Modal, Spinner, Skeleton

## React Client

A reference React implementation is provided:

```bash
cd official_docs_examples/tools/ui_test

# Start the UI server
cargo run --bin ui_server

# In another terminal, start the React client
cd ui_react_client
npm install && npm run dev -- --host
```

The React client includes:
- TypeScript types matching the Rust schema
- Component renderer for all 28 types
- Recharts integration for interactive charts
- Markdown rendering support
- Dark mode support
- Form submission handling
- Modal and toast components

## Architecture

```
┌─────────────┐                    ┌─────────────┐
│   Agent     │ ──[render_* tool]──│ UiResponse  │
│  (LLM)      │                    │   (JSON)    │
└─────────────┘                    └──────┬──────┘
       ▲                                  │
       │                                  │ SSE
       │                                  ▼
       │                           ┌─────────────┐
       └────── UiEvent ◄───────────│   Client    │
              (user action)        │  (React)    │
                                   └─────────────┘
```

## Examples

Three examples demonstrate UI tools:

| Example | Description | Run Command |
|---------|-------------|-------------|
| `ui_agent` | Console demo | `cargo run --bin ui_agent` |
| `ui_server` | HTTP server with SSE | `cargo run --bin ui_server` |
| `ui_react_client` | React frontend | `cd ui_react_client && npm run dev` |

Run from `official_docs_examples/tools/ui_test/`.

## Sample Prompts

Test the UI tools with these prompts:

```
# Forms
"I want to register for an account"
"Create a contact form"
"Create a feedback form with a comments textarea"

# Cards
"Show me my profile"
"Display a product card for a laptop"

# Alerts
"Show a success message"
"Display a warning about expiring session"

# Modals
"I want to delete my account" (shows confirmation modal)
"Show a confirmation dialog before submitting"

# Toasts
"Show a success toast notification"
"Display an error toast"

# Tables
"Show my recent orders"
"List all users"

# Charts
"Show monthly sales chart"
"Display traffic trends as a line chart"
"Show revenue breakdown as a pie chart"

# Progress & Loading
"Show upload progress at 75%"
"Display a loading spinner"
"Show skeleton loading state"

# Dashboards
"Show system status dashboard"
```


---

**Previous**: [← Browser Tools](browser-tools.md) | **Next**: [MCP Tools →](mcp-tools.md)
