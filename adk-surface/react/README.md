# @zavora-ai/adk-surface-react

The renderer of [adk-surface](../README.md)'s web home for agent-drawn UI (spec 3.3 §6.1 `genui`), as a React package. It replaces `@zavora-ai/adk-ui-react`.

| Export | What it draws |
|---|---|
| `DrawnView` | A form, card, table or screen an agent drew (A2UI v0.9.1 on the `forms` 1.0 catalog), on the card background, never a decision's colours; a form's button says "Sending runs `send_invoice` and asks you first" |
| `RefusedNotice` | A drawing the server refused, as a plain notice |
| `AppFrame`, `AppBridge` | An app from an MCP server (MCP Apps 2026-01-26), framed sandboxed with scripts only, under "App from Weather · it can't approve anything"; what the app asks for shows outside its frame and only a person's press does it |
| `GenuiProtocols` | The developer panel's claims of A2UI, AG-UI and MCP Apps, and the drawings refused in a session |
| `parseJsonl`, `applyMessages`, `SurfaceStore`, `resolve`, `holds`, `sanitizeUrl` | The A2UI parser, surface store, bindings and link check, ported from adk-ui |

The wire types (`DrawnSurface`, `AppView`, `AppOpen`, `AppRequest`, `AppAnswer`, …) are exported as types; they are generated from the Rust types of `adk-surface`.

## Install

```bash
npm install @zavora-ai/adk-surface-react
```

React 18.3 or 19 is a peer dependency. The package has no other dependency.

## Use

```tsx
import { DrawnView } from "@zavora-ai/adk-surface-react";
import type { DrawnSurface } from "@zavora-ai/adk-surface-react";
import "@zavora-ai/adk-surface-react/tokens.css";
import "@zavora-ai/adk-surface-react/style.css";

export function Drawing({ surface, send }: { surface: DrawnSurface; send: (action: string, values: Record<string, unknown>) => Promise<void> }) {
  return <DrawnView surface={surface} onSend={send} />;
}
```

`tokens.css` defines the §9 colour tokens in light and dark; leave it out when the host page defines `--card`, `--border`, `--text` and the rest itself.

A drawn form's **Send** never runs a tool: the host posts a `submitDrawn` command to `POST /api/surface/commands`, the drawing agent holds the call, and the person decides on its card. An app's `tools/call` is held the same way. Nothing a drawing or an app shows can approve anything (spec 3.3 §7.4 rule 5).

## Build

The package is built from `adk-surface/web/src/genui` with the web home's toolchain, so it has no `package-lock.json` of its own:

```bash
cd adk-surface/web && npm ci
cd ../react && npm pack --dry-run
```

CI runs `npm pack --dry-run`; the package is published at release.

## License

Apache-2.0
