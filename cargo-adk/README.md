# cargo-adk

[![crates.io](https://img.shields.io/crates/v/cargo-adk.svg)](https://crates.io/crates/cargo-adk)

Scaffolding CLI for [ADK-Rust](https://github.com/zavora-ai/adk-rust) — generate agent projects from templates in seconds.

## Install

```bash
cargo install cargo-adk
```

## Usage

```bash
# Create a basic Gemini agent
cargo adk new my-agent

# Create with a specific template
cargo adk new my-agent --template tools    # custom tools with #[tool] macro
cargo adk new my-agent --template rag      # RAG with vector search
cargo adk new my-agent --template api      # REST API server
cargo adk new my-agent --template openai   # OpenAI-powered agent

# Use a different provider
cargo adk new my-agent --provider anthropic
cargo adk new my-agent --provider openai

# List available templates
cargo adk templates
```

## Templates

| Template | What you get |
|----------|-------------|
| `basic` | Gemini agent with interactive console (default) |
| `tools` | Agent with `#[tool]` macro custom tools |
| `rag` | RAG pipeline with Gemini embeddings + in-memory vector store |
| `api` | REST server with health check, ready for deployment |
| `openai` | OpenAI GPT-5-mini agent with console |

Each template generates:
- `Cargo.toml` with the right dependencies and feature flags
- `src/main.rs` that compiles and runs immediately
- `.env.example` with the required API key variables
- `.gitignore`

## Generated Project

```
my-agent/
├── Cargo.toml
├── src/
│   └── main.rs
├── .env.example
└── .gitignore
```

```bash
cd my-agent
cp .env.example .env    # add your API key
cargo run
```

## Part of ADK-Rust

This tool is part of the [ADK-Rust](https://github.com/zavora-ai/adk-rust) framework for building AI agents in Rust.

## License

Apache-2.0
