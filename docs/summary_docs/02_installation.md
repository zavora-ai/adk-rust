# Installation & Setup

This guide will help you set up ADK-Rust in your development environment.

## Prerequisites

### Rust Toolchain

ADK-Rust requires **Rust 1.75 or higher**.

```bash
# Check your Rust version
rustc --version

# Update Rust if needed
rustup update
```

If you don't have Rust installed:

```bash
# Install Rust (Unix/Linux/macOS)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Windows: Download from https://rustup.rs/
```

### Google API Key

To use Gemini models, you need a Google AI API key:

1. Visit [Google AI Studio](https://aistudio.google.com/app/apikey)
2. Create or select a project
3. Generate an API key
4. Set the environment variable:

```bash
# Unix/Linux/macOS
export GOOGLE_API_KEY="your-api-key-here"

# Or add to your shell profile (~/.bashrc, ~/.zshrc, etc.)
echo 'export GOOGLE_API_KEY="your-api-key-here"' >> ~/.bashrc

# Windows (PowerShell)
$env:GOOGLE_API_KEY="your-api-key-here"

# Windows (Command Prompt)
set GOOGLE_API_KEY=your-api-key-here
```

> [!TIP]
> You can also use `GEMINI_API_KEY` as an alternative environment variable name.

## Installation Options

### Option 1: Use as Library (Recommended)

Add ADK crates to your `Cargo.toml`:

```toml
[dependencies]
# Core dependencies (always needed)
adk-core = "0.1"
adk-agent = "0.1"
adk-runner = "0.1"

# Model integration (for LLM agents)
adk-model = "0.1"

# Tool system (for agent capabilities)
adk-tool = "0.1"

# Optional: Session management
adk-session = "0.1"

# Optional: Artifact storage
adk-artifact = "0.1"

# Optional: Memory system
adk-memory = "0.1"

# Optional: HTTP server
adk-server = "0.1"

# Async runtime (required)
tokio = { version = "1.40", features = ["full"] }
```

Minimal setup:
```toml
[dependencies]
adk-core = "0.1"
adk-agent = "0.1"
adk-model = "0.1"
adk-tool = "0.1"
adk-runner = "0.1"
tokio = { version = "1.40", features = ["full"] }
```

### Option 2: Install CLI

Install the `adk-cli` command-line tool globally:

```bash
cargo install adk-cli
```

Verify installation:
```bash
adk --version
```

### Option 3: Clone and Build from Source

For development or contributing:

```bash
# Clone the repository
git clone https://github.com/your-org/adk-rust.git
cd adk-rust

# Build all crates
cargo build --release

# Run tests
cargo test

# Run examples
cargo run --example quickstart
```

## Verify Installation

### Test Library Installation

Create a new project and test the installation:

```bash
# Create new project
cargo new my-adk-agent
cd my-adk-agent

# Add dependencies to Cargo.toml (see Option 1 above)
```

Add this code to `src/main.rs`:

```rust
use adk_agent::LlmAgentBuilder;
use adk_model::gemini::GeminiModel;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;
    
    let agent = LlmAgentBuilder::new("test-agent")
        .model(Arc::new(model))
        .build()?;
    
    println!("✅ ADK-Rust is working! Agent: {}", agent.name());
    Ok(())
}
```

Run it:
```bash
cargo run
```

Expected output:
```
✅ ADK-Rust is working! Agent: test-agent
```

### Test CLI Installation

If you installed the CLI:

```bash
# Start interactive console
adk console --agent-name quickstart

# Start HTTP server
adk serve --port 8080
```

## Configuration

### Environment Variables

ADK-Rust respects these environment variables:

| Variable | Description | Default | Required |
|----------|-------------|---------|----------|
| `GOOGLE_API_KEY` | Google AI API key | - | Yes (for Gemini) |
| `GEMINI_API_KEY` | Alternative API key name | - | No |
| `PORT` | Server port (CLI) | 8080 | No |
| `RUST_LOG` | Logging level | info | No |
| `DATABASE_URL` | SQLite database path | - | No |

### Logging

ADK-Rust uses the `tracing` ecosystem for logging. Configure logging levels:

```bash
# Set log level
export RUST_LOG=info           # Default
export RUST_LOG=debug          # Verbose
export RUST_LOG=adk_runner=debug,adk_agent=trace  # Per-crate

# In code
use tracing_subscriber;

tracing_subscriber::fmt::init();
```

### Database (Optional)

For persistent sessions and artifacts:

```bash
# Set database path (SQLite)
export DATABASE_URL="sqlite://adk.db"

# Or use in code
use adk_session::DatabaseSessionService;

let session_service = DatabaseSessionService::new("sqlite://adk.db").await?;
```

## Troubleshooting

### Common Issues

#### "API key not found"

```
Error: environment variable not found
```

**Solution**: Set `GOOGLE_API_KEY` or `GEMINI_API_KEY` environment variable.

```bash
export GOOGLE_API_KEY="your-key-here"
```

#### "Rust version too old"

```
error: package requires rustc 1.75 or newer
```

**Solution**: Update Rust toolchain.

```bash
rustup update
```

#### "Cannot find dependency"

```
error: no matching package named `adk-core`
```

**Solution**: Currently, ADK-Rust crates are not published to crates.io. Use path dependencies or Git dependencies:

```toml
[dependencies]
adk-core = { path = "../adk-rust/adk-core" }
# or
adk-core = { git = "https://github.com/your-org/adk-rust", branch = "main" }
```

#### Compilation Errors

If you encounter compilation errors after updating:

```bash
# Clean build artifacts
cargo clean

# Update dependencies
cargo update

# Rebuild
cargo build
```

### Getting Help

If you're stuck:

1. Check the [Examples](../examples/README.md) for working code
2. Review the [Architecture Guide](ARCHITECTURE.md) for design patterns
3. Search or create an issue on GitHub
4. Check the [Troubleshooting Guide](09_troubleshooting.md)

## Platform-Specific Notes

### Linux

No special requirements. ADK-Rust works on all major Linux distributions.

```bash
# Install dependencies (if needed)
# Ubuntu/Debian
sudo apt-get install build-essential pkg-config libssl-dev

# Fedora/RHEL
sudo dnf install gcc pkg-config openssl-devel
```

### macOS

Works on both Intel and Apple Silicon (M1/M2/M3).

```bash
# Install Xcode Command Line Tools (if needed)
xcode-select --install
```

### Windows

Supported on Windows 10+ with Visual Studio Build Tools.

```powershell
# Install Visual Studio Build Tools 2019 or later
# Download from: https://visualstudio.microsoft.com/downloads/

# Or use rustup-init.exe from https://rustup.rs/
```

## Next Steps

Now that you have ADK-Rust installed:

- **[Quick Start →](03_quickstart.md)**: Build your first agent
- **[Core Concepts →](04_concepts.md)**: Learn the fundamentals
- **[Examples →](../examples/README.md)**: Explore working code

---

**Previous**: [Introduction](01_introduction.md) | **Next**: [Quick Start](03_quickstart.md)
