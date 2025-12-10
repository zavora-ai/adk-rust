# Contributing to ADK-Rust

Thank you for your interest in contributing to ADK-Rust! This document provides guidelines and instructions for contributing.

## Quick Start

```bash
# Clone and build
git clone https://github.com/zavora-ai/adk-rust.git
cd adk-rust
cargo build

# Run tests
cargo test --all

# Check lints and formatting
cargo clippy --all-targets --all-features
cargo fmt --all
```

## Development Guidelines

For comprehensive development guidelines including:

- Code style and conventions
- Error handling patterns
- Async patterns and thread safety
- Testing best practices
- Documentation standards
- Common development tasks (adding tools, models, agents)

Please see the **[Development Guidelines](docs/official_docs/development/development-guidelines.md)**.

## Project Structure

```
adk-rust/
├── adk-core/       # Foundational traits (Agent, Tool, Llm)
├── adk-agent/      # Agent implementations
├── adk-model/      # LLM providers (Gemini, OpenAI, Anthropic)
├── adk-tool/       # Tool system
├── adk-session/    # Session management
├── adk-artifact/   # Artifact storage
├── adk-memory/     # Long-term memory
├── adk-runner/     # Execution runtime
├── adk-server/     # REST API and A2A protocol
├── adk-cli/        # Command-line launcher
├── adk-telemetry/  # OpenTelemetry integration
├── adk-realtime/   # Voice/audio agents
├── adk-graph/      # Graph-based workflows
├── adk-browser/    # Browser automation
├── adk-eval/       # Agent evaluation
├── adk-rust/       # Umbrella crate
└── examples/       # Working examples
```

## Pull Request Checklist

Before submitting a PR:

- [ ] All tests pass: `cargo test --all`
- [ ] No clippy warnings: `cargo clippy --all-targets --all-features`
- [ ] Code is formatted: `cargo fmt --all`
- [ ] Documentation is updated for API changes
- [ ] New functionality includes tests

## Commit Messages

Use conventional commits:

```
feat: add new feature
fix: correct a bug
docs: update documentation
refactor: code improvement without behavior change
test: add or update tests
```

## Getting Help

- **Issues**: [GitHub Issues](https://github.com/zavora-ai/adk-rust/issues)
- **Discussions**: [GitHub Discussions](https://github.com/zavora-ai/adk-rust/discussions)

## License

By contributing, you agree that your contributions will be licensed under the Apache 2.0 License.
