# A2UI Quickstart (ADK-UI)

This guide shows how to emit A2UI JSONL from an ADK agent and render it with the React renderer.

## 1) Add dependencies

```toml
[dependencies]
adk-ui = { git = "https://github.com/zavora-ai/adk-ui" }
adk-agent = "0.8.2"
adk-model = "0.8.2"
```

React renderer:

```bash
npm install @zavora-ai/adk-ui-react
```

## 2) Enable UI tools

```rust
use adk_agent::LlmAgentBuilder;
use adk_ui::UiToolset;

let tools = UiToolset::all_tools();
let mut builder = LlmAgentBuilder::new("a2ui_agent")
    .instruction(
        "You output A2UI via render_screen (screens), render_page (pages), and render_kit (kits).",
    );

for tool in tools {
    builder = builder.tool(tool);
}

let _agent = builder.build()?;
```

## 3) Prompt → A2UI JSONL

Prompt your agent:

```
Create a login screen with email, password, and a primary Sign In button.
```

The model will call `render_screen` and return JSONL:

```
{"createSurface":{"surfaceId":"main","catalogId":"zavora.ai:adk-ui/default@0.1.0","sendDataModel":true}}
{"updateComponents":{"surfaceId":"main","components":[{"id":"root","component":"Column",...}]}}
```

## 4) Render JSONL in React

```tsx
import {
  A2uiStore,
  A2uiSurfaceRenderer,
  applyParsedMessages,
  parseJsonl,
} from "@zavora-ai/adk-ui-react";

const store = new A2uiStore();

export function App({ jsonl }: { jsonl: string }) {
  const parsed = parseJsonl(jsonl);
  applyParsedMessages(store, parsed);

  return (
    <A2uiSurfaceRenderer
      store={store}
      surfaceId="main"
      onAction={(payload) => {
        console.log("A2UI action:", payload);
      }}
    />
  );
}
```

## 5) Next steps

- Use `render_page` to generate multi-section pages.
- Use `render_kit` to produce catalogs + tokens + templates.
- See the working UI examples under `examples/ui_working/`.
