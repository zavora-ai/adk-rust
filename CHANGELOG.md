# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Complete Rust implementation of Google's ADK
- Core traits: Agent, Llm, Tool, Toolset, SessionService
- Agent types: LlmAgent, CustomAgent, SequentialAgent, ParallelAgent, LoopAgent
- Gemini 2.0 Flash model integration with streaming
- Tool system with GoogleSearch, ExitLoop, LoadArtifacts
- MCP (Model Context Protocol) integration with rmcp SDK
- Session management (in-memory and SQLite)
- Artifact storage (in-memory and SQLite)
- Memory system with semantic search
- Runner for agent execution with context management
- REST API server with Axum
- A2A (Agent-to-Agent) protocol support
- CLI with console mode (rustyline) and server mode
- 8 working examples demonstrating all features

### Documentation
- Comprehensive README with quick start
- Architecture guide
- Implementation plan with 10 phases
- MCP implementation guide
- 8 example READMEs

## [0.1.0] - TBD

Initial release with complete ADK implementation.

### Features
- ✅ All core ADK features from Go implementation
- ✅ MCP integration (gold standard for 2025)
- ✅ Production-ready architecture
- ✅ 8 working examples
- ✅ REST and A2A server support

### Crates
- `adk-core` - Core traits and types
- `adk-agent` - Agent implementations
- `adk-model` - Model integrations (Gemini)
- `adk-tool` - Tool system with MCP
- `adk-session` - Session management
- `adk-artifact` - Artifact storage
- `adk-memory` - Memory system
- `adk-runner` - Execution runtime
- `adk-server` - REST + A2A servers
- `adk-cli` - CLI application

### Requirements
- Rust 1.75+
- Tokio async runtime
- Google API key for Gemini

[Unreleased]: https://github.com/yourusername/adk-rust/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/yourusername/adk-rust/releases/tag/v0.1.0
