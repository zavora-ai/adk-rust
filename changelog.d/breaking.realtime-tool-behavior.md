- **`ToolDefinition` gains `behavior`** (`adk-realtime`): `Option<ToolBehavior>` sets whether
  the model waits for a tool's result. A struct literal adds `behavior: None`;
  `ToolDefinition::new` and the builder methods are unchanged.
