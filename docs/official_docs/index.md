# ADK-Rust Official Documentation

Welcome to the official documentation for ADK-Rust (Agent Development Kit for Rust). This documentation provides comprehensive guides and references for building AI agents using the Rust implementation of the ADK framework.

## Getting Started

- [Introduction](introduction.md) - Overview of ADK-Rust, its architecture, and key concepts
- [Quickstart](quickstart.md) - Build your first agent in under 10 minutes

## Core Concepts

### Agents

- [LlmAgent](agents/llm-agent.md) - The core agent type using Large Language Models for reasoning
- [Workflow Agents](agents/workflow-agents.md) - Deterministic agents: Sequential, Parallel, and Loop
- [Multi-Agent Systems](agents/multi-agent.md) - Building agent hierarchies with sub-agents

### Tools

- [Function Tools](tools/function-tools.md) - Create custom tools with async Rust functions
- [Built-in Tools](tools/built-in-tools.md) - Pre-built tools like GoogleSearchTool
- [MCP Tools](tools/mcp-tools.md) - Model Context Protocol integration

### Sessions & State

- [Sessions](sessions/sessions.md) - Session management and lifecycle
- [State Management](sessions/state.md) - Managing conversation state with prefixes

### Callbacks

- [Callbacks](callbacks/callbacks.md) - Intercept and customize agent behavior at execution points

### Artifacts

- [Artifacts](artifacts/artifacts.md) - Binary data storage and retrieval

### Events

- [Events](events/events.md) - Understanding the event system and conversation history

## Operations

### Observability

- [Telemetry](observability/telemetry.md) - Logging, tracing, and monitoring

### Deployment

- [Launcher](deployment/launcher.md) - Running agents in console or server mode
- [Server](deployment/server.md) - REST API and web UI integration
- [A2A Protocol](deployment/a2a.md) - Agent-to-Agent communication

## Roadmap

Features planned but not yet implemented in ADK-Rust:

- [Long Running Tools](../roadmap/long-running-tools.md) - Async tool execution with progress tracking
- [VertexAI Session](../roadmap/vertex-ai-session.md) - Cloud-based session persistence
- [GCS Artifacts](../roadmap/gcs-artifacts.md) - Google Cloud Storage for artifacts
- [Agent Tool](../roadmap/agent-tool.md) - Using agents as tools
- [Evaluation Framework](../roadmap/evaluation.md) - Testing and evaluating agent performance

## Validation Status

All code samples in this documentation are validated through working examples in the `adk-rust-guide` package. Each documentation page has a corresponding example that compiles and executes successfully.

To run validation examples:

```bash
# Compile all examples
cargo build --examples -p adk-rust-guide

# Run a specific example
cargo run --example quickstart -p adk-rust-guide
```
