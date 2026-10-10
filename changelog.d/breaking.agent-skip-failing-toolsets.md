- **A failing toolset no longer fails the agent turn** (`adk-agent`): when a toolset's
  `tools()` call returns an error, `LlmAgent` skips that toolset for the resolution, logs
  a `warn` naming the toolset and the error, and runs the model with the tools that did
  resolve. Previously one unreachable MCP server ended the whole turn. Call
  `LlmAgentBuilder::strict_toolsets(true)` to keep failing the turn. Duplicate tool
  names still fail it in both modes.
