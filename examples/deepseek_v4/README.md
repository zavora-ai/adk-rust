# DeepSeek V4 Example

Demonstrates all DeepSeek V4 API features with ADK-Rust.

## What This Shows

| # | Feature | Model | Config |
|---|---------|-------|--------|
| 1 | Fast inference | `deepseek-v4-flash` | No thinking |
| 2 | Standard reasoning | `deepseek-v4-pro` | `ReasoningEffort::High` |
| 3 | Deep reasoning | `deepseek-v4-pro` | `ReasoningEffort::Max` |
| 4 | Thinking + tools | `deepseek-v4-pro` | Tools + thinking |
| 5 | Thinking disabled | `deepseek-v4-pro` | `ThinkingMode::Disabled` |
| 6 | Multi-turn thinking | `deepseek-v4-pro` | Conversation across turns |
| 7 | Legacy compat | `deepseek-chat` | Old constructor still works |

## V4 Best Practices

- **Use `v4_flash` for speed** — no thinking overhead, lowest cost
- **Use `v4_pro` + `High` for standard tasks** — good balance of quality and speed
- **Use `v4_pro` + `Max` for hard problems** — deepest reasoning (math, logic, code)
- **Thinking + tools**: `reasoning_content` is automatically preserved across tool call turns
- **Disable thinking** when you need deterministic, fast responses from V4 Pro
- **temperature/top_p are ignored** in thinking mode — the API silently drops them

## Prerequisites

- `DEEPSEEK_API_KEY` environment variable set

## Run

```bash
cd examples/deepseek_v4
cp .env.example .env   # add your DEEPSEEK_API_KEY
cargo run
```

## V4 Model Comparison

| Model | Thinking | Speed | Best For |
|-------|----------|-------|----------|
| `deepseek-v4-flash` | Off by default | Fastest | Chat, simple tasks, high volume |
| `deepseek-v4-pro` | On by default | Slower | Reasoning, math, code, complex agents |
| `deepseek-chat` | Off | Fast | Legacy compatibility |
| `deepseek-reasoner` | On | Slower | Legacy reasoning |

## API Features Used

- `ThinkingMode::Enabled` / `ThinkingMode::Disabled` — explicit thinking toggle
- `ReasoningEffort::High` / `ReasoningEffort::Max` — reasoning depth control
- `reasoning_content` — chain-of-thought output in streaming and non-streaming
- Tool calling in thinking mode with `reasoning_content` preservation
- Prefix caching metrics (`prompt_cache_hit_tokens`, `prompt_cache_miss_tokens`)
