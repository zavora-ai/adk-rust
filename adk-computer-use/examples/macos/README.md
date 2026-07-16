# macOS native-UI showcases

These examples build a **real** native window (AppKit) or drive a **real** file
manager (Finder), so they are inherently macOS-specific. The ADK graph they run
is identical on every OS — only this demo scaffolding is native.

If you just want a live end-to-end run on any platform, use the cross-platform
clipboard demo instead (it works on macOS, Linux, and Windows):

```bash
cargo run -p adk-computer-use --example live_clipboard
```

And for a portable, dependency-free tour of the graph with no server at all:

```bash
cargo run -p adk-computer-use --example minimal_graph
```

## Prerequisites

Both macOS examples require:

- **macOS** — each `main` exits early on other platforms.
- **Node.js** — runs the `computer-use-mcp` server. Override the binary with
  `NODE`.
- A **`computer-use-mcp` build**. Point `COMPUTER_USE_MCP_ENTRYPOINT` at a local
  `dist/server.js`, or set `COMPUTER_USE_MCP_PACKAGE` to an npm specifier.
- `COMPUTER_USE_PRINCIPAL_ID` — the authenticated operator identity (defaults to
  `adk-local-operator`).

Examples that require picture-in-picture (PiP) approval additionally need:

- **Electron** — launched via `npx --package=electron@43.1.0`, or override with
  `COMPUTER_USE_ELECTRON`.
- `COMPUTER_USE_SUPERVISOR_DIR` — optional override for the supervisor package
  directory (otherwise derived from the entrypoint).

`live_form` also requires:

- **`swiftc`** and the AppKit framework, to compile
  [`macos_form_showcase.swift`](./macos_form_showcase.swift) into a temporary
  demo app.
- A Gemini API key (`GOOGLE_API_KEY` or `GEMINI_API_KEY`) for the
  schema-constrained planner.

## Examples

| Example | What it demonstrates |
| --- | --- |
| `live_form` | Native AppKit form, PiP approval, one executor, and independent value read-back. |
| `live_background_finder` | Background Finder comment update with certified capability, proving focus/pointer stay undisturbed, then rollback. |

Shared helpers live in [`../support/mod.rs`](../support/mod.rs); each example
includes it with `#[path = "../support/mod.rs"] mod support;` and it is not built
as its own example target.
