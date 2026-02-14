# Provider Matrix

## Common env vars
- Gemini: `GOOGLE_API_KEY` or Vertex settings
- OpenAI: `OPENAI_API_KEY`
- Anthropic: `ANTHROPIC_API_KEY`
- DeepSeek: `DEEPSEEK_API_KEY`
- Groq: `GROQ_API_KEY`
- Ollama: local `ollama serve`

## Feature flags
- `openai`
- `anthropic`
- `deepseek`
- `groq`
- `ollama`
- `mistralrs` (separate crate flow)

## Verification
```bash
cargo check --workspace --all-features
cargo run -p adk-examples --example verify_backend_selection
cargo run -p adk-examples --example verify_vertex_streaming
```
