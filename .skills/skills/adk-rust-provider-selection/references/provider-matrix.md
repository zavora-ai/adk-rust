# Provider Matrix

## Common env vars
- Gemini: `GOOGLE_API_KEY` or Vertex settings
- OpenAI: `OPENAI_API_KEY`
- Anthropic: `ANTHROPIC_API_KEY`
- DeepSeek: `DEEPSEEK_API_KEY`
- Groq: `GROQ_API_KEY`
- Ollama: local `ollama serve`
- OpenRouter: `OPENROUTER_API_KEY`
- Bedrock: AWS IAM credentials (standard AWS credential chain)
- Azure AI: `AZURE_AI_API_KEY`

## Feature flags
- `openai`
- `anthropic`
- `deepseek`
- `groq`
- `ollama`
- `mistralrs` (separate crate flow)
- `openrouter`
- `bedrock`
- `azure-ai`
- Fireworks, Together, Mistral, Perplexity, Cerebras, SambaNova, xAI — the `openai` feature with an `OpenAICompatibleConfig` preset (`OpenAICompatibleConfig::fireworks(...)`); there is no feature per preset.

## Convenience APIs
- `provider_from_env()` — auto-detect provider from env vars (precedence: Anthropic > OpenAI > Gemini)
- `adk::run(instructions, input)` — one-liner agent invocation with auto provider detection

## Prompt caching
- Enabled by default for Anthropic and Bedrock
- Gemini explicit caching activates when `cache_capable` is set on the runner

## Pricing modules
- `adk_gemini::pricing`
- `adk_model::openai::pricing`
- `adk_anthropic::pricing`

## Verification
```bash
cargo check --workspace --all-features
cargo run -p adk-examples --example verify_backend_selection
cargo run -p adk-examples --example verify_vertex_streaming
```
