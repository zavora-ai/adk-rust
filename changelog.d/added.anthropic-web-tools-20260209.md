- **Anthropic web search and web fetch with dynamic filtering** (`adk-anthropic`, `adk-tool`,
  `adk-model`): `WebSearchTool20260209` and `WebFetchTool20260209` declare the
  `web_search_20260209` and `web_fetch_20260209` tool versions through
  `ToolUnionParam::WebSearch20260209` and `ToolUnionParam::WebFetch20260209`.
  `WebSearchTool::with_dynamic_filtering()` selects the new search version; the default stays
  `web_search_20250305`. `extensions["anthropic"]["built_in_tools"]` accepts both new type
  strings. Responses parse the `code_execution_tool_result`,
  `bash_code_execution_tool_result` and `text_editor_code_execution_tool_result` blocks these
  tools and the code execution tool return (`CodeExecutionToolResultBlock`), and the Anthropic
  provider surfaces them, and web fetch results, as `Part::ServerToolResponse` with their
  `type`.
