- **Self-contained wire-format docs** (`adk-enterprise`, `adk-managed`): item docs cited
  sections of an internal specification ("CANON §3.4", "CANON §5") that readers cannot open.
  They now state the wire shape or error envelope they describe, and the
  `adk_enterprise::ToolConfig` docs show the built-in tool shape it serializes,
  `{"type": "builtin", "name": "bash"}`.
