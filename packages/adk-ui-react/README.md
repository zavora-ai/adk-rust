# @zavora-ai/adk-ui-react

<p align="center">
  <strong>React components for rendering dynamic AI agent interfaces</strong>
</p>

<p align="center">
  <a href="https://adk-rust.com">Documentation</a> •
  <a href="https://github.com/zavora-ai/adk-rust">GitHub</a> •
  <a href="https://www.npmjs.com/package/@zavora-ai/adk-ui-react">npm</a>
</p>

---

**@zavora-ai/adk-ui-react** is the official React renderer for [ADK-Rust](https://adk-rust.com) — the high-performance Agent Development Kit for building AI agents in Rust.

Render rich, interactive user interfaces from your AI agents instead of plain text. Forms, tables, charts, modals, and more — all driven by your agent's output.

## Features

- 28 component types — text, buttons, forms, tables, charts, modals, toasts, and more
- Dark mode — built-in light/dark/system theme support
- Bidirectional events — forms and buttons emit events back to your agent
- Streaming updates — replace, patch, append, or remove components in real time
- A2UI protocol — surface-based rendering with data bindings and dynamic expressions
- Tri-protocol support — A2UI, AG-UI, and MCP Apps through a unified client
- TypeScript first — full type definitions included
- Lightweight — ~34KB packaged

## Installation

```bash
npm install @zavora-ai/adk-ui-react
```

Peer dependencies: `react >= 17.0.0` and `react-dom >= 17.0.0`.

## Quick Start

### Basic Renderer

Render a `UiResponse` payload from your agent:

```tsx
import { Renderer } from '@zavora-ai/adk-ui-react';
import type { UiResponse, UiEvent } from '@zavora-ai/adk-ui-react';

function AgentUI({ response }: { response: UiResponse }) {
  const handleAction = (event: UiEvent) => {
    // Send event back to your agent/server
    console.log('User action:', event);
  };

  return (
    <div>
      {response.components.map((component, i) => (
        <Renderer
          key={i}
          component={component}
          onAction={handleAction}
          theme={response.theme}
        />
      ))}
    </div>
  );
}
```

### Streaming Renderer

Apply incremental updates to components without re-rendering the full tree:

```tsx
import { StreamingRenderer } from '@zavora-ai/adk-ui-react';
import type { Component, UiUpdate } from '@zavora-ai/adk-ui-react';

function LiveUI({ component, updates }: { component: Component; updates: UiUpdate[] }) {
  return (
    <StreamingRenderer
      component={component}
      updates={updates}
      onAction={(event) => console.log(event)}
      theme="dark"
    />
  );
}
```

Updates support four operations: `replace`, `patch`, `append`, and `remove` — each targeting a component by `target_id`.

## A2UI Surface Renderer

For surface-based rendering with data bindings, dynamic expressions, and action events:

```tsx
import { A2uiSurfaceRenderer, A2uiStore, parseJsonl, applyParsedMessages } from '@zavora-ai/adk-ui-react';

const store = new A2uiStore();

// Apply A2UI JSONL messages from your agent
const messages = parseJsonl(jsonlPayload);
applyParsedMessages(store, messages);

function SurfaceUI() {
  return (
    <A2uiSurfaceRenderer
      store={store}
      surfaceId="main"
      onAction={(payload) => console.log(payload)}
      theme="light"
    />
  );
}
```

A2UI surfaces support:

- Data bindings — `{ path: "/users/0/name" }` resolves values from the surface data model
- Dynamic expressions — `${/path}` interpolation in strings, `${concat(...)}` function calls
- Built-in functions — `now()`, `concat()`, `add()`, `formatString()`
- Custom function registries — pass your own functions via the `functions` prop
- Action events — buttons and interactions emit structured `A2uiActionEventPayload` objects

## Tri-Protocol Client

Use the unified protocol client when your backend may return different UI protocols:

```tsx
import {
  A2uiSurfaceRenderer,
  UnifiedRenderStore,
  createProtocolClient,
} from '@zavora-ai/adk-ui-react';

const store = new UnifiedRenderStore();
const client = createProtocolClient({ protocol: 'a2ui', store });

// Feed any supported payload format
client.applyPayload(payload);

// Render from the unified store
const surface = store.getA2uiStore().getSurface('main');
```

### Supported inbound payload formats

| Protocol | Payload shape |
|----------|--------------|
| A2UI | JSONL string or `{ protocol: "a2ui", jsonl, ... }` |
| AG-UI | `{ protocol: "ag_ui", events: [...] }` with `adk.ui.surface` custom events |
| MCP Apps | `{ protocol: "mcp_apps", payload: { resourceReadResponse: { contents: [...] } } }` |
| Legacy ADK-UI | `{ components: [...] }` — auto-detected, stored as legacy response |

### Outbound events

Generate protocol-appropriate outbound events:

```ts
import { buildOutboundEvent } from '@zavora-ai/adk-ui-react';

const event = buildOutboundEvent('ag_ui', {
  action: 'button_click',
  action_id: 'approve',
});
// => { protocol: "ag_ui", event: { type: "CUSTOM", name: "adk.ui.event", ... } }
```

### Runtime negotiation pattern

1. Set the UI protocol on requests via `uiProtocol` header or `x-adk-ui-protocol`.
2. Feed response payloads into `client.applyPayload(...)`.
3. Render with `A2uiSurfaceRenderer` from the unified store.
4. Use `client.buildOutboundEvent(event)` for user interactions.

## Available Components

### Atoms

Text, Button, Icon, Image, Badge

### Inputs

TextInput, NumberInput, Select, MultiSelect, Switch, DateInput, Slider, Textarea

### Layouts

Stack, Grid, Card, Container, Divider, Tabs

### Data Display

Table (sortable, paginated), List, KeyValue, CodeBlock

### Visualization

Chart (bar, line, area, pie via Recharts)

### Feedback

Alert, Progress, Toast, Modal, Spinner, Skeleton

### A2UI Components

Text (with Markdown), Image, Icon, Row, Column, List, Card, Divider, Tabs, Modal, Button, CheckBox, TextField, ChoicePicker, Slider, DateTimeInput, Video, AudioPlayer

## API Reference

### Exports

```ts
// Renderers
export { Renderer, StreamingRenderer } from './Renderer';
export { A2uiSurfaceRenderer } from './a2ui/renderer';

// Types
export type { Component, UiResponse, UiEvent, TableColumn } from './types';
export { uiEventToMessage } from './types';

// A2UI Store & Parser
export { A2uiStore } from './a2ui/store';
export { applyParsedMessages, parseJsonl } from './a2ui/parser';
export type {
  A2uiMessage, CreateSurfaceMessage, DeleteSurfaceMessage,
  ParsedA2uiMessage, UpdateComponentsMessage, UpdateDataModelMessage,
} from './a2ui/parser';

// A2UI Bindings
export {
  isDataBinding, isFunctionCall, resolveDynamicString,
  resolveDynamicValue, resolvePath,
} from './a2ui/bindings';
export type { DataBinding, FunctionCall, FunctionRegistry, ResolveContext } from './a2ui/bindings';

// A2UI Events
export { buildActionEvent } from './a2ui/events';
export type {
  A2uiActionDefinition, A2uiActionEventDefinition,
  A2uiActionEventPayload, ActionEventOptions,
} from './a2ui/events';

// Streaming Updates
export { applyUiUpdate, applyUiUpdates } from './updates';

// Protocol Adapters
export { applyProtocolPayload, parseProtocolPayload } from './protocols';

// Unified Store & Client
export { UnifiedRenderStore } from './store';
export { ProtocolClient, buildOutboundEvent, createProtocolClient } from './client';
export type { OutboundEventOptions, ProtocolClientOptions, UiProtocol } from './client';
```

## Integration with ADK-Rust

This package renders UI generated by the `adk-ui` Rust crate:

```rust
use adk_ui::UiToolset;

let tools = UiToolset::all_tools();
let mut builder = LlmAgentBuilder::new("assistant");
for tool in tools {
    builder = builder.tool(tool);
}
let agent = builder.build()?;
```

Your agent calls `render_form`, `render_table`, `render_chart`, and other tools to produce UI payloads that this package renders on the client.

## Requirements

- React 17.0.0+
- react-dom 17.0.0+

## License

Apache-2.0 — See [LICENSE](https://github.com/zavora-ai/adk-rust/blob/main/LICENSE) for details.

---

<p align="center">
  Built with ❤️ by <a href="https://zavora.ai">Zavora AI</a>
</p>
