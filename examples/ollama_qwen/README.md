# Qwen on Ollama — Thinking + Tool Calling

Tests ADK-Rust compatibility with Qwen models running locally on Ollama.

## Supported Models

| Model | Ollama Command | Size | Notes |
|-------|---------------|------|-------|
| **Qwen 3.6** | `ollama pull qwen3.6:35b-a3b` | 24 GB | Latest — 73.4% SWE-bench, MoE 35B/3B active, multimodal |
| Qwen 3.5 | `ollama pull qwen3.5` | varies | Previous generation |
| Qwen3-Coder | `ollama pull qwen3-coder:30b` | 19 GB | Code-focused, uses `<function=NAME>` format |

Set the model via environment variable:

```bash
OLLAMA_MODEL=qwen3.6:35b-a3b cargo run --manifest-path examples/ollama_qwen/Cargo.toml
```

Default: `qwen3.5` (smallest download for quick testing).

## Scenarios

1. **Thinking / Reasoning** — Qwen emits `<think>` blocks that ADK-Rust captures as `Part::Thinking`
2. **Tool Calling (Ollama native)** — Ollama parses tool calls server-side, ADK-Rust receives structured `Part::FunctionCall`
3. **Tool Calling (OpenAI-compat endpoint)** — Uses Ollama's `/v1` OpenAI-compatible endpoint. When the model emits `<tool_call>` XML tags in text, ADK-Rust's text-based tool call parser automatically detects and converts them to `Part::FunctionCall`

## Qwen 3.6 Highlights

Qwen 3.6-35B-A3B (released April 16, 2026) is a sparse MoE model with 35B total parameters but only 3B active — making it fast and memory-efficient while scoring 73.4% on SWE-bench Verified.

Key features relevant to ADK-Rust:
- **Tool calling** — same `<tool_call>` format as Qwen 3.5, fully supported by our parser
- **Thinking preservation** — new `preserve_thinking` option retains reasoning across turns
- **262K native context** — extensible to 1M tokens with YaRN
- **Multimodal** — vision encoder for images and video (text-only mode also available)
- **Apache 2.0** — fully open-source

## Text-Based Tool Call Parser

ADK-Rust includes a built-in parser that detects tool calls embedded in text responses from 7 model families:

| Format | Models |
|--------|--------|
| `<tool_call>JSON</tool_call>` | Qwen 3.5, Qwen 3.6, Hermes |
| `<tool_call><function=NAME>ARGS</function></tool_call>` | Qwen-Coder |
| `` <\|python_tag\|>JSON `` | Llama 3/4 |
| `[TOOL_CALLS][JSON]` | Mistral Nemo |
| `` ```json\nJSON\n``` `` | DeepSeek |
| `<\|tool_call>call:NAME{...}<tool_call\|>` | Gemma 4 |
| `<\|action_start\|>JSON<\|action_end\|>` | InternLM, ChatGLM |

This works automatically — no user code changes needed. The parser runs as a fallback when the serving backend doesn't return structured `tool_calls` JSON.

## Prerequisites

```bash
# Pick one:
ollama pull qwen3.6:35b-a3b   # Latest, 24 GB (recommended)
ollama pull qwen3.5            # Previous gen, smaller
ollama pull qwen3-coder:30b    # Code-focused
```

## Usage

```bash
# Default model (qwen3.5)
cargo run --manifest-path examples/ollama_qwen/Cargo.toml

# Qwen 3.6
OLLAMA_MODEL=qwen3.6:35b-a3b cargo run --manifest-path examples/ollama_qwen/Cargo.toml

# Qwen3-Coder
OLLAMA_MODEL=qwen3-coder:30b cargo run --manifest-path examples/ollama_qwen/Cargo.toml
```
