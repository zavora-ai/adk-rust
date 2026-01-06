# Model Providers Test Examples

This project validates the code examples from the providers.md documentation.

## Structure

```
doc-test/
├── agents/
│   ├── llm_agent_test/
│   ├── multi_agent_test/
│   ├── workflow_test/
│   ├── graph_agent_test/
│   └── realtime_agent_test/
├── models/
│   └── providers_test/     ← You are here
└── quickstart_test/
```

## Examples

| Example | Provider | API Key Required |
|---------|----------|------------------|
| `gemini_example` | Google Gemini | `GOOGLE_API_KEY` |
| `openai_example` | OpenAI | `OPENAI_API_KEY` |
| `anthropic_example` | Anthropic | `ANTHROPIC_API_KEY` |
| `deepseek_example` | DeepSeek | `DEEPSEEK_API_KEY` |
| `groq_example` | Groq | `GROQ_API_KEY` |
| `ollama_example` | Ollama | None (local) |

## Running Examples

```bash
# Set your API keys
export GOOGLE_API_KEY="your-key"
export OPENAI_API_KEY="your-key"
# ... etc

# Run individual examples
cargo run --bin gemini_example
cargo run --bin openai_example
cargo run --bin anthropic_example
cargo run --bin deepseek_example
cargo run --bin groq_example
cargo run --bin ollama_example  # Requires: ollama serve
```

## Notes

- Each example sends a simple test prompt and displays the response
- Examples use the Launcher for interactive mode (type 'exit' to quit)
- Ollama requires the server running locally: `ollama serve`
