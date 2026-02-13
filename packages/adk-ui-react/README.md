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

**@zavora-ai/adk-ui-react** is the official React renderer for [ADK-Rust](https://adk-rust.com) - the high-performance Agent Development Kit for building AI agents in Rust.

Enable your AI agents to render rich, interactive user interfaces instead of plain text responses. Forms, tables, charts, modals, and more - all controlled by your agent.

## ✨ Features

- 🎨 **28 Component Types** - Text, buttons, forms, tables, charts, modals, toasts, and more
- 🌙 **Dark Mode** - Built-in dark theme support
- 📤 **Bidirectional Events** - Forms and buttons emit events back to your agent
- 📦 **TypeScript First** - Full type definitions included
- ⚡ **Lightweight** - Only 14KB gzipped

## 📦 Installation

```bash
npm install @zavora-ai/adk-ui-react
```

## 🚀 Quick Start

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
          key={component.id || i}
          component={component} 
          onAction={handleAction}
          theme={response.theme}  // 'light' | 'dark' | 'system'
        />
      ))}
    </div>
  );
}
```

## Tri-Protocol Client (A2UI / AG-UI / MCP Apps)

Use the protocol client when your backend may return different UI protocols.

```tsx
import {
  A2uiSurfaceRenderer,
  UnifiedRenderStore,
  createProtocolClient,
} from '@zavora-ai/adk-ui-react';

const store = new UnifiedRenderStore();
const client = createProtocolClient({ protocol: 'adk_ui', store });

// payload can be:
// - A2UI JSONL string
// - AG-UI envelope { protocol: "ag_ui", events: [...] }
// - MCP Apps envelope { protocol: "mcp_apps", payload: {...} }
// - legacy ADK-UI payload with `components`
client.applyPayload(payload);

const surface = store.getA2uiStore().getSurface('main');
```

Supported inbound payload patterns:

- A2UI: JSONL string or `{ protocol: "a2ui", jsonl, components, ... }`
- AG-UI: `{ protocol: "ag_ui", events: [...] }` or envelope payload with `payload.events`
- MCP Apps: `{ protocol: "mcp_apps", payload: { resource, ... } }`
- Legacy ADK-UI: `{ components: [...] }`

Outbound events can also be generated per protocol:

```ts
import { buildOutboundEvent } from '@zavora-ai/adk-ui-react';

const event = buildOutboundEvent('ag_ui', {
  action: 'button_click',
  action_id: 'approve',
});
```

Common pattern with server runtime negotiation:

1. Set runtime profile on requests with `uiProtocol` or `x-adk-ui-protocol`.
2. Feed response payloads into `client.applyPayload(...)`.
3. Render with `A2uiSurfaceRenderer` from the unified store.

Migration note:

- `adk_ui` is treated as legacy profile compatibility mode.
- Prefer `a2ui`, `ag_ui`, or `mcp_apps` for new integrations.
- Read `/api/ui/capabilities` and surface server-provided deprecation metadata in client UX.

## 🧩 Available Components

| Category | Components |
|----------|------------|
| **Atoms** | Text, Button, Icon, Image, Badge |
| **Inputs** | TextInput, NumberInput, Select, MultiSelect, Switch, DateInput, Slider, Textarea |
| **Layouts** | Stack, Grid, Card, Container, Divider, Tabs |
| **Data Display** | Table (sortable, paginated), List, KeyValue, CodeBlock |
| **Visualization** | Chart (bar, line, area, pie) |
| **Feedback** | Alert, Progress, Toast, Modal, Spinner, Skeleton |

## 🔗 Integration with ADK-Rust

This package is designed to work with [ADK-Rust](https://adk-rust.com), the Agent Development Kit for building AI agents in Rust.

```rust
use adk_ui::UiToolset;

// Add UI rendering tools to your agent
let tools = UiToolset::all_tools();
let mut builder = LlmAgentBuilder::new("assistant");
for tool in tools {
    builder = builder.tool(tool);
}
let agent = builder.build()?;
```

Your agent can then call `render_form`, `render_table`, `render_chart`, and other tools to generate UI that this package renders.

## 📚 Learn More

- 🌐 **Website**: [adk-rust.com](https://adk-rust.com)
- 📖 **Documentation**: [adk-rust.com/docs](https://adk-rust.com/docs)
- 💻 **GitHub**: [github.com/zavora-ai/adk-rust](https://github.com/zavora-ai/adk-rust)
- 📦 **Rust Crate**: [crates.io/crates/adk-ui](https://crates.io/crates/adk-ui)

## 📋 Requirements

- React 17.0.0 or higher
- react-dom 17.0.0 or higher

## 📄 License

Apache-2.0 - See [LICENSE](https://github.com/zavora-ai/adk-rust/blob/main/LICENSE) for details.

---

<p align="center">
  Built with ❤️ by <a href="https://zavora.ai">Zavora AI</a>
</p>
