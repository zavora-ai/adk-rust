# Agent Development Kit

Agent Development Kit (ADK) is a flexible and modular framework for developing and deploying AI agents. While optimized for Gemini and the Google ecosystem, ADK is model-agnostic, deployment-agnostic, and is built for compatibility with other frameworks. ADK was designed to make agent development feel more like software development, to make it easier for developers to create, deploy, and orchestrate agentic architectures that range from simple tasks to complex workflows.

> [!IMPORTANT]
> ADK-Rust v0.1.0 requires Rust 1.75 or higher

**News**: ADK-Rust v0.1.0 released!

## Get Started

### Rust

```bash
cargo add adk-rust
```

```rust
use adk_rust::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;
    
    let agent = LlmAgentBuilder::new("assistant")
        .model(Arc::new(model))
        .build()?;
    
    // Use your agent...
    Ok(())
}
```

### Other Languages

- **Python**: `pip install google-adk` - [Documentation](https://github.com/google/adk-python)
- **Go**: `go get google.golang.org/adk` - [Documentation](https://github.com/google/adk-go)
- **Java**: Maven/Gradle - [Documentation](https://github.com/google/adk-java)

---

**Next**: [Installation →](installation.md)
