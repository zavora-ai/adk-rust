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
          key={i} 
          component={component} 
          onAction={handleAction}
          theme={response.theme}  // 'light' | 'dark' | 'system'
        />
      ))}
    </div>
  );
}
```

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
