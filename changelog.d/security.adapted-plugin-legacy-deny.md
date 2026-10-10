- **A legacy plugin's `before_tool` refusal stops the tool** (`adk-plugin`):
  `AdaptedPlugin` discarded the `Some(content)` a legacy `before_tool` callback returns to
  skip a tool, so the tool ran while the plugin reported a refusal. It now short-circuits
  with that content as the call's result, and the callback's context carries the tool name
  and arguments.
