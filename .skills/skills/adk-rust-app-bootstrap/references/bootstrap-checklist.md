# Bootstrap Checklist

## Cargo setup
- Add `adk-rust = "0.3.0"` for broad usage.
- Or add targeted crates (`adk-agent`, `adk-model`, `adk-runner`, etc.).

## Provider selection
- Gemini default: no additional feature on `adk-model` when using umbrella defaults.
- OpenAI/Anthropic/DeepSeek/Groq/Ollama: enable corresponding feature flags.

## First validation
```bash
cargo check --workspace --all-features
cargo run -p adk-examples --example quickstart
```

## Expansion order
1. Agent behavior
2. Tools
3. Sessions/state
4. Server/deployment
