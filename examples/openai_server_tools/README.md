# OpenAI Native Tools Example Matrix

This crate exercises every OpenAI native tool wrapper currently exposed by `adk-tool`, using agentic scenarios instead of raw provider calls.

## Run

```bash
export OPENAI_API_KEY=sk-your-key-here
cargo run --manifest-path examples/openai_server_tools/Cargo.toml
```

Optional:

```bash
export OPENAI_MODEL=gpt-5
export OPENAI_VECTOR_STORE_ID=vs_...
export OPENAI_MCP_SERVER_URL=https://your-mcp-server.example/sse
```

## Coverage

| Scenario | Wrapper(s) | Notes |
| --- | --- | --- |
| Web search + function tool | `OpenAIWebSearchTool` | Hosted web retrieval plus local function tool |
| File search + function tool | `OpenAIFileSearchTool` | Requires `OPENAI_VECTOR_STORE_ID` or `OPENAI_VECTOR_STORE_IDS` |
| Code interpreter + function tool | `OpenAICodeInterpreterTool` | Hosted Python execution plus local function tool |
| Image generation invocation | `OpenAIImageGenerationTool` | Surfaces image-generation native protocol items |
| Computer use invocation | `OpenAIComputerUseTool` | Surfaces computer-use protocol items |
| MCP invocation | `OpenAIMcpTool` | Requires `OPENAI_MCP_SERVER_URL` or `OPENAI_MCP_CONNECTOR_ID` |
| Local shell invocation | `OpenAILocalShellTool` | Surfaces local-shell protocol items |
| Managed shell invocation | `OpenAIShellTool` | Surfaces shell protocol items and outputs |
| Apply-patch invocation | `OpenAIApplyPatchTool` | Surfaces apply-patch protocol items |
| Multi-turn hosted tool then function tool | `OpenAIWebSearchTool` | Verifies native tool history coexists with a later function-tool turn |

## Notes

- Scenarios that require provider-side resources are skipped automatically when the relevant env vars are missing.
- Hosted tools and protocol tools are both represented through the same `Tool` API in the agent builder.
- Protocol-oriented tools such as computer use, local shell, shell, and apply-patch are intentionally demonstrated by inspecting `Part::ServerToolCall` / `Part::ServerToolResponse` items, because that is the framework contract they currently expose.
