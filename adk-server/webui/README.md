# ADK Runtime UI

The built-in ADK-Rust agent interface is a small React/Vite application focused
on execution rather than agent authoring. It exposes:

- portable team topology with distinct delegation and handoff edges;
- streaming conversation events, tool calls, transfers, and failures;
- session timelines, shared state, artifacts, and prior sessions;
- runtime capability and UI-protocol discovery;
- responsive light, dark, and system themes.

The visual system follows ADK Studio Next: compact 46–48px chrome, teal accent
tokens, low-noise panels, topology cards, and dotted delegation edges. It uses
system fonts and local assets only.

## Development

```bash
npm ci
npm run dev
```

Vite proxies no API calls. Run an ADK server on the same origin or use the
embedded build for end-to-end testing.

## Verification and production build

```bash
npm run verify
```

`verify` runs strict TypeScript checking, provider-free tests, and writes the
production assets to `../assets/webui`.
