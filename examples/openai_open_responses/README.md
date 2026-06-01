# OpenAI Open Responses Example

Demonstrates the **Open Responses mode** for provider-agnostic usage of the Responses API. Open Responses mode relaxes strict OpenAI field validation, allowing you to connect to any compatible third-party endpoint without code changes. Switch between OpenAI, LM Studio, Ollama, vLLM, or any other Open Responses-compliant provider by changing environment variables alone.

## Prerequisites

- Rust 1.85.0+
- One of the following:
  - `OPENAI_API_KEY` environment variable set (for OpenAI)
  - A running Open Responses-compatible local server (LM Studio, Ollama, vLLM)

## Running

```bash
cargo run --manifest-path examples/openai_open_responses/Cargo.toml
```

### With a local provider

```bash
export OPEN_RESPONSES_BASE_URL=http://localhost:1234/v1
export OPEN_RESPONSES_MODEL=my-local-model
cargo run --manifest-path examples/openai_open_responses/Cargo.toml
```

## What It Does

1. **Configure endpoint** — Reads `OPEN_RESPONSES_BASE_URL` (defaults to OpenAI API) and `OPEN_RESPONSES_MODEL` (defaults to `gpt-4.1-nano`)
2. **Enable Open Responses mode** — Configures the client with `open_responses_mode` enabled and the custom base URL
3. **Send prompt** — Sends a simple prompt and streams the response from the configured endpoint
4. **Graceful handling** — Handles missing OpenAI-specific fields without errors when connected to third-party providers

## Configuration

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `OPENAI_API_KEY` | No | `""` (empty) | API key; not required for third-party providers |
| `OPEN_RESPONSES_BASE_URL` | No | `https://api.openai.com/v1` | Base URL for the endpoint |
| `OPEN_RESPONSES_MODEL` | No | `gpt-4.1-nano` | Model name to use |

## Supported Providers

Open Responses mode works with any endpoint implementing the Open Responses specification:

- **[LM Studio](https://lmstudio.ai/)** — Local model server with OpenAI-compatible API (`http://localhost:1234/v1`)
- **[Ollama](https://ollama.com/)** — Run LLMs locally with OpenAI compatibility (`http://localhost:11434/v1`)
- **[vLLM](https://docs.vllm.ai/)** — High-throughput serving engine (`http://localhost:8000/v1`)
- **OpenAI** — The original Responses API (default)

## Related

- [`adk-model/src/openai/config.rs`](../../adk-model/src/openai/config.rs) — `OpenAIResponsesConfig` with `open_responses_mode` field
- [`adk-model/src/openai/responses_client.rs`](../../adk-model/src/openai/responses_client.rs) — OpenAI Responses API client
- [OpenAI Responses API](https://platform.openai.com/docs/api-reference/responses) — Official documentation
- [Open Responses Specification](https://github.com/open-responses/open-responses) — Open standard for Responses API compatibility
