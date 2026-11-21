- **A2UI display-only extension** (`adk-surface`, feature `genui`): every A2UI 1.0
  `createSurface` genui writes claims `metadata.extensions.adkrust_display_only` 1.0
  (`DISPLAY_ONLY_EXTENSION`), and the developer panel lists the claim. Reading 1.0 accepts that
  claim and still refuses any other surface metadata. The extension's rules, the approval
  card's minimum content model and a watch list (WebMCP, A2A agent cards, MCP Apps permissions,
  XR, splats) are in `docs/design/a2ui-extension-display-only.md`.
