# CLI Usage Guide

The `adk-cli` command-line tool provides an easy way to run agents interactively or as a server.

## Installation

```bash
# Install from source
cargo install --path ./adk-cli

# Or build and run directly
cargo build --release
./target/release/adk-cli --help
```

## Commands

### console

Start an interactive console session with an agent.

```bash
adk-cli console [OPTIONS]
```

**Options**:
- `--agent-name <NAME>`: Agent name (default: "quickstart")
- `--model <MODEL>`: Gemini model (default: "gemini-2.0-flash-exp")
- `--description <DESC>`: Agent description
- `--instruction <TEXT>`: System instruction for agent

**Example**:
```bash
# Basic usage
adk-cli console

# Custom agent
adk-cli console --agent-name research-assistant \
  --description "Research assistant with web search" \
  --instruction "Always cite your sources"

# Different model
adk-cli console --model gemini-1.5-pro
```

**Interactive Commands**:
- Type your message and press Enter
- `exit` or `quit` to exit
- Ctrl+C to interrupt
- Up/Down arrows for history

**Example Session**:
```
🤖 Starting ADK Console...
Agent: quickstart
To exit, type 'exit' or press Ctrl+C

You: What is the weather in Tokyo?
Assistant: Let me search for current weather in Tokyo...
[Searching...]
According to recent reports, Tokyo is experiencing partly cloudy skies...

You: exit
👋 Goodbye!
```

### serve

Start an HTTP server with REST and A2A APIs.

```bash
adk-cli serve [OPTIONS]
```

**Options**:
- `--port <PORT>`: Server port (default: 8080, or from `PORT` env var)
- `--host <HOST>`: Bind address (default: "0.0.0.0")
- `--agent-name <NAME>`: Agent name
- `--database-url <URL>`: Database connection string (optional)

**Example**:
```bash
# Basic server
adk-cli serve

# Custom port
adk-cli serve --port 3000

# With database
adk-cli serve --database-url sqlite://sessions.db

# Custom agent
adk-cli serve --agent-name my-agent
```

**Endpoints**:
- `POST /api/run`: Run agent with input
- `GET /health`: Health check
- `GET /api/agents` (future): List available agents

### version

Show version information.

```bash
adk-cli --version
# or
adk-cli -V
```

### help

Show help information.

```bash
adk-cli --help
# or
adk-cli -h

# Command-specific help
adk-cli console --help
adk-cli serve --help
```

## Configuration

### Environment Variables

The CLI respects these environment variables:

| Variable | Description | Default |
|----------|-------------|---------|
| `GOOGLE_API_KEY` | Google AI API key | **Required** |
| `GEMINI_API_KEY` | Alternative API key | - |
| `PORT` | Server port | 8080 |
| `RUST_LOG` | Logging level | info |
| `DATABASE_URL` | Database connection | In-memory |

### Configuration File (Future)

Support for `~/.adk/config.toml` planned:

```toml
[default]
model = "gemini-2.0-flash-exp"
agent_name = "my-assistant"

[server]
port = 8080
host = "0.0.0.0"

[database]
url = "sqlite://~/.adk/sessions.db"
```

## Console Mode Features

### History

Console mode saves command history:

```bash
# Location: ~/.adk_history
# Navigate with Up/Down arrows
```

### Multiline Input (Future)

Enter multiline text:

```
You: This is line 1\
... This is line 2\
... This is line 3
```

### Autocomplete (Future)

Tab completion for commands:

```
You: /he<TAB>
You: /help
```

## Server Mode Details

### REST API

#### POST /api/run

Execute agent with user input.

**Request**:
```bash
curl -X POST http://localhost:8080/api/run \
  -H "Content-Type: application/json" \
  -d '{
    "userId": "user123",
    "sessionId": "session456",
    "message": "Hello, world!"
  }'
```

**Response** (Server-Sent Events):
```
data: {"invocationId":"inv-1","agentName":"assistant","content":{"role":"model","parts":[{"text":"Hello!"}]},"actions":{}}

data: {"invocationId":"inv-1","agentName":"assistant","content":{"role":"model","parts":[{"text":" How"}]},"actions":{}}

data: {"invocationId":"inv-1","agentName":"assistant","content":{"role":"model","parts":[{"text":" can I help you?"}]},"actions":{}}
```

#### GET /health

Check server health.

**Request**:
```bash
curl http://localhost:8080/health
```

**Response**:
```json
{
  "status": "ok",
  "version": "0.1.0"
}
```

### A2A Protocol (Future)

Agent-to-Agent communication:

```bash
# Get agent card
curl http://localhost:8080/.well-known/agent.json

# Response:
{
  "name": "my-agent",
  "description": "...",
  "capabilities": ["chat", "tools"],
  "endpoints": {
    "chat": "/api/run"
  }
}
```

## Examples

### Example 1: Quick Q&A

```bash
export GOOGLE_API_KEY="your-key"
adk-cli console

You: What is Rust?
Assistant: Rust is a systems programming language...
```

### Example 2: Research Assistant

```bash
adk-cli console \
  --agent-name researcher \
  --instruction "You are a research assistant. Always search for current information and cite sources."

You: What are the latest developments in AI?
Assistant: [Searching for latest AI developments...]
According to recent articles...
```

### Example 3: Code Helper

```bash
adk-cli console \
  --agent-name code-helper \
  --instruction "You are an expert Rust programmer. Provide code examples and explanations."

You: How do I create a HashMap in Rust?
Assistant: Here's how to create a HashMap in Rust...
```

### Example 4: HTTP Server

```bash
# Terminal 1: Start server
adk-cli serve --port 8080

# Terminal 2: Test with curl
curl -X POST http://localhost:8080/api/run \
  -H "Content-Type: application/json" \
  -d '{
    "userId": "test-user",
    "sessionId": "test-session",
    "message": "Hello!"
  }'
```

### Example 5: With Database

```bash
# Use SQLite for persistence
export DATABASE_URL="sqlite://sessions.db"
adk-cli serve

# Sessions are now persisted across restarts
```

## Advanced Usage

### Custom Agent Loader

For complex setups, modify the CLI source or create your own:

```rust
use adk_agent::LlmAgentBuilder;
use adk_cli::{Config, start_console};

#[tokio::main]
async fn main() -> Result<()> {
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);
    
    let agent = LlmAgentBuilder::new("custom")
        .model(model)
        .tool(Arc::new(my_custom_tool))
        .build()?;
    
    let config = Config {
        agent_name: "custom".to_string(),
        // ...
    };
    
    start_console(Arc::new(agent), config).await?;
    Ok(())
}
```

### Scripting

Use the CLI in scripts:

```bash
#!/bin/bash
export GOOGLE_API_KEY="..."

# Non-interactive mode (future)
echo "What is 2+2?" | adk-cli console --non-interactive

# Or use server mode
adk-cli serve --port 8080 &
SERVER_PID=$!

# Make requests
curl -X POST http://localhost:8080/api/run -d '...'

# Cleanup
kill $SERVER_PID
```

## Troubleshooting

### "GOOGLE_API_KEY not set"

```bash
export GOOGLE_API_KEY="your-key-here"
```

### "Address already in use"

Port is already in use:

```bash
# Use different port
adk-cli serve --port 8081

# Or find and kill the process
lsof -i :8080
kill <PID>
```

### "Permission denied"

Can't bind to privileged port (< 1024):

```bash
# Use non-privileged port
adk-cli serve --port 8080

# Or run with sudo (not recommended)
sudo adk-cli serve --port 80
```

### Slow Responses

- Check internet connection
- Verify API key is valid
- Try a different model (e.g., `gemini-2.0-flash-exp` is faster than `gemini-1.5-pro`)

## Development

### Building from Source

```bash
git clone https://github.com/your-org/adk-rust.git
cd adk-rust

# Build CLI
cargo build --release --package adk-cli

# Run
./target/release/adk-cli --version
```

### Running Tests

```bash
cargo test --package adk-cli
```

### Adding Custom Commands

Modify `adk-cli/src/cli.rs`:

```rust
use clap::{Parser, Subcommand};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Console { /* ... */ },
    Serve { /* ... */ },
    MyCustomCommand {
        #[arg(long)]
        my_option: String,
    },
}
```

## Comparison with Other ADK CLIs

### vs. adk-go CLI

| Feature | adk-rust (CLI) | adk-go (CLI) |
|---------|----------------|---------------|
| Interactive console | ✅ | ✅ |
| HTTP server | ✅ | ✅ |
| A2A protocol | 🚧 Partial | ✅ |
| Multi-agent | 🚧 Planned | ✅ |
| Config file | 🚧 Planned | ✅ |
| Performance | ⚡ Faster | Standard |

### vs. adk-python CLI

| Feature | adk-rust (CLI) | adk-python (CLI) |
|---------|----------------|-------------------|
| Interactive console | ✅ | ✅ |
| HTTP server | ✅ | ✅ |
| Deployment | 🏗️ Binary | 🐍 Requires Python |
| Startup time | ⚡ Instant | Slower |
| Memory usage | 💾 Low | Higher |

---

**Previous**: [MCP Integration](06_mcp.md) | **Next**: [Troubleshooting](10_troubleshooting.md)
