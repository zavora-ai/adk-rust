# ADK-Rust Official Documentation

Welcome to the official documentation for ADK-Rust (Agent Development Kit for Rust). This documentation provides comprehensive guides and references for building AI agents using the Rust implementation of the ADK framework.

## Getting Started

- [Introduction](introduction.md) - Overview of ADK-Rust, its architecture, and key concepts
- [Quickstart](quickstart.md) - Build your first agent in under 10 minutes

## Core

- [Core Types](core/core.md) - Fundamental types: Content, Part, Agent trait, Tool trait, contexts
- [Runner](core/runner.md) - Agent execution runtime and configuration

## Models

- [Model Providers](models/providers.md) - LLM integrations: Gemini, OpenAI, Anthropic, DeepSeek, Groq, Ollama
- [Ollama](models/ollama.md) - Local inference with Ollama
- [mistral.rs Integration](models/mistralrs.md) - Native mistral.rs for high-performance local inference

## Agents

- [LlmAgent](agents/llm-agent.md) - The core agent type using Large Language Models
- [Workflow Agents](agents/workflow-agents.md) - Deterministic agents: Sequential, Parallel, Loop
- [Multi-Agent Systems](agents/multi-agent.md) - Building agent hierarchies with sub-agents
- [Graph Agents](agents/graph-agents.md) - LangGraph-style workflow orchestration
- [Realtime Agents](agents/realtime-agents.md) - Voice-enabled agents with OpenAI/Gemini

## Tools

- [Function Tools](tools/function-tools.md) - Create custom tools with async Rust functions
- [Built-in Tools](tools/built-in-tools.md) - Pre-built tools like GoogleSearchTool
- [MCP Tools](tools/mcp-tools.md) - Model Context Protocol integration
- [Browser Tools](tools/browser-tools.md) - 46 WebDriver tools for web automation
- [UI Tools](tools/ui-tools.md) - Dynamic UI generation with forms, cards, charts

## Sessions & State

- [Sessions](sessions/sessions.md) - Session management and lifecycle
- [State Management](sessions/state.md) - Managing conversation state with prefixes

## Callbacks & Events

- [Callbacks](callbacks/callbacks.md) - Intercept and customize agent behavior
- [Events](events/events.md) - Understanding the event system and conversation history

## Artifacts

- [Artifacts](artifacts/artifacts.md) - Binary data storage and retrieval

## Observability

- [Telemetry](observability/telemetry.md) - Logging, tracing, and monitoring

## Deployment

- [Launcher](deployment/launcher.md) - Running agents in console or server mode
- [Server](deployment/server.md) - REST API and web UI integration
- [A2A Protocol](deployment/a2a.md) - Agent-to-Agent communication

## Evaluation

- [Agent Evaluation](evaluation/evaluation.md) - Testing and validating agent behavior

## Security

- [Access Control](security/access-control.md) - Role-based permissions and audit logging
- [Guardrails](security/guardrails.md) - PII redaction, content filtering, schema validation
- [Memory](security/memory.md) - Long-term semantic memory for agents

## Studio

- [ADK Studio](studio/studio.md) - Visual development environment for building agents

## Development

- [Development Guidelines](development/development-guidelines.md) - Contributing guide and best practices

---

## Validation Status

All code samples in this documentation are validated through working examples in the `doc-test/` packages. Each documentation page has corresponding examples that compile and execute successfully.
