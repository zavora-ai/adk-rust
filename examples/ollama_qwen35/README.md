# Qwen 3.5 on Ollama — Thinking + Tool Calling

Tests ADK-Rust compatibility with Qwen 3.5 running locally on Ollama.

## Scenarios

1. **Thinking / Reasoning** — Qwen 3.5 emits thinking traces that ADK-Rust captures as `Part::Thinking`
2. **Tool Calling (Ollama native)** — Ollama parses tool calls server-side, ADK-Rust receives structured `Part::FunctionCall`
3. **Tool Calling (OpenAI-compat endpoint)** — Uses Ollama's `/v1` OpenAI-compatible endpoint. When the model emits `<tool_call>` XML tags in text, ADK-Rust's text-based tool call parser automatically detects and converts them to `Part::FunctionCall`

## Text-Based Tool Call Parser

ADK-Rust includes a built-in parser that detects tool calls embedded in text responses from 7 model families:

| Format | Models |
|--------|--------|
| `<tool_call>JSON</tool_call>` | Qwen, Hermes |
| `<tool_call><function=NAME>ARGS</function></tool_call>` | Qwen-Coder |
| `` <\|python_tag\|>JSON `` | Llama 3/4 |
| `[TOOL_CALLS][JSON]` | Mistral Nemo |
| `` ```json\nJSON\n``` `` | DeepSeek |
| `<\|tool_call>call:NAME{...}<tool_call\|>` | Gemma 4 |
| `<\|action_start\|>JSON<\|action_end\|>` | InternLM, ChatGLM |

This works automatically — no user code changes needed. The parser runs as a fallback when the serving backend doesn't return structured `tool_calls` JSON.

## Prerequisites

```bash
ollama pull qwen3.5
```

## Usage

```bash
cargo run --manifest-path examples/ollama_qwen35/Cargo.toml
```
