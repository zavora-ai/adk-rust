# A2UI Quickstart (ADK-UI)

This guide shows how to emit A2UI JSONL from an ADK agent and render it with the React renderer.
The UI tools come from `adk-ui`, which lives in its own repository,
[zavora-ai/adk-ui](https://github.com/zavora-ai/adk-ui).

> **Important:** `adk-ui` 2.2.0, the current release, is built on `adk-core` 2. Its tools
> implement the ADK-Rust 2 `Tool` trait, so an ADK-Rust 3.0 agent cannot register them:
> `LlmAgentBuilder::tool` fails to compile with "multiple different versions of crate
> `adk_core`". Steps 1–3 show the setup for an `adk-ui` release built on `adk-core` 3, which
> is not yet published; until then they work only with ADK-Rust 2. Step 4, the React renderer,
> does not depend on the Rust crates.

## What works with ADK-Rust 3.0

The `adk-server` REST runtime negotiates the UI protocol a client renders, without a
dependency on `adk-ui`:

- A run request selects a profile — `adk_ui` (default), `a2ui`, `ag_ui` or `mcp_apps` — with
  the `x-adk-ui-protocol` header or its `uiProtocol` field.
- `GET /api/ui/capabilities` reports each profile's versions, implementation tier, features and
  limitations. The `a2ui` profile is draft-compatible with A2UI v0.9 (`createSurface`,
  `updateComponents`, `updateDataModel` as JSONL).

See [UI Tools](tools/ui-tools.md) for the profiles.

## 1) Add dependencies

```toml
[dependencies]
# The `adk-core` feature makes the UI tools implement `adk_core::Tool`
adk-ui = { git = "https://github.com/zavora-ai/adk-ui", features = ["adk-core"] }
adk-agent = "3.0.0"
adk-model = "3.0.0"
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

The model calls `render_screen`, which returns A2UI JSONL. Each line is one message, such as
`createSurface` followed by `updateComponents`, and the default catalog is
`zavora.ai:adk-ui/extended@0.2.0`:

```
{"version":"v0.9.1","createSurface":{"surfaceId":"main","catalogId":"zavora.ai:adk-ui/extended@0.2.0",...}}
{"version":"v0.9.1","updateComponents":{"surfaceId":"main","components":[{"id":"root","component":"Column",...}]}}
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
- See the `ui_server` and `ui_react_client` examples in the `adk-ui` repository.
