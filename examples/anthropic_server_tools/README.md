# Anthropic Native Tools Example Matrix

This crate exercises every Anthropic native tool wrapper currently exposed by `adk-tool` and supported by the pinned `claudius` SDK surface in this repo.

## Run

```bash
export ANTHROPIC_API_KEY=sk-ant-your-key-here
cargo run --manifest-path examples/anthropic_server_tools/Cargo.toml
```

Optional:

```bash
export ANTHROPIC_MODEL=claude-sonnet-4-20250514
```

## Coverage

| Scenario | Wrapper(s) | Notes |
| --- | --- | --- |
| Web search + function tool | `WebSearchTool` | Preserves Anthropic server-tool parts alongside a local function tool |
| Bash 20241022 + function tool | `AnthropicBashTool20241022` | End-to-end agent loop with native bash execution |
| Bash 20250124 + function tool | `AnthropicBashTool20250124` | End-to-end agent loop with native bash execution |
| Text editor 20250124 multi-turn | `AnthropicTextEditorTool20250124` | Real file edit workflow across two turns |
| Text editor 20250429 multi-turn | `AnthropicTextEditorTool20250429` | Real file edit workflow across two turns |
| Text editor 20250728 multi-turn | `AnthropicTextEditorTool20250728` | Real file edit workflow across two turns |

## Notes

- The text-editor scenarios create a temporary file, ask the agent to edit it across multiple turns, and then verify the resulting file contents.
- Anthropic web search is demonstrated as a true server-side native tool.
- Anthropic bash and text-editor tools are demonstrated through the standard ADK agent tool loop, because that is how the current implementation executes them.
