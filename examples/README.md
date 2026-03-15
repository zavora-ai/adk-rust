# ADK Rust Examples

Essential examples demonstrating core ADK-Rust framework capabilities.

For the full 120+ example collection, see the [adk-playground](https://github.com/zavora-ai/adk-playground) repo.

## Examples

| Example | Capability | Run Command |
|---------|-----------|-------------|
| `quickstart` | Basic agent with tools | `cargo run --example quickstart` |
| `function_tool` | Custom function tools | `cargo run --example function_tool` |
| `sequential` | Multi-agent workflows | `cargo run --example sequential` |
| `graph_workflow` | Graph orchestration | `cargo run --example graph_workflow` |
| `mcp` | MCP integration | `cargo run --example mcp` |
| `eval_basic` | Evaluation framework | `cargo run --example eval_basic` |
| `template` | Starter template | `cargo run --example template` |
| `ralph` | Autonomous agent (standalone crate) | `cargo run -p ralph` |

## Prerequisites

```bash
# Google Gemini (default provider)
export GOOGLE_API_KEY="your-key"    # or GEMINI_API_KEY
```

## Tips

- Use `Ctrl+C` to exit console mode
- Copy `.env.example` to `.env` for API keys
- Ralph is a standalone crate with its own tests: `cargo test -p ralph`
