# ADK-Rust Official Documentation

Welcome to the official documentation for ADK-Rust (Agent Development Kit for Rust). This documentation provides comprehensive guides and references for building AI agents using the Rust implementation of the ADK framework.

## Getting Started

- [Introduction](introduction.md) - Overview of ADK-Rust, its architecture, and key concepts
- [Quickstart](quickstart.md) - Build your first agent in under 10 minutes
- [A2UI Quickstart](quickstart-a2ui.md) - Emit A2UI JSONL and render it in React

## Core

- [Core Types](core/core.md) - Fundamental types: Content, Part, Agent trait, Tool trait, contexts
- [Runner](core/runner.md) - Agent execution runtime and configuration
- [Plugins](core/plugins.md) - Lifecycle hooks for tool/model interception and middleware

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
- [RAG](tools/rag.md) - Retrieval-Augmented Generation for knowledge base search
- [ACP Tools](tools/acp-tools.md) - Agent Client Protocol (Claude Code, Codex, Kiro CLI)
- [Retry & Reflect](tools/retry-reflect.md) - Tool failure recovery with reflection prompts
- [Action Nodes](tools/action-nodes.md) - 14 deterministic node types for workflow graphs
- [Benchmarking](tools/benchmarking.md) - Performance measurement with `cargo adk bench`

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
- [Agentic Web Protocol](deployment/awp.md) - AWP protocol for agent-native web services

## Evaluation

- [Agent Evaluation](evaluation/evaluation.md) - Testing and validating agent behavior
- [Benchmark Results](evaluation/benchmarks.md) - Published performance comparison data

## Managed Agents

- [Managed Agent Runtime](managed-agents/runtime.md) - Provider-neutral, durable, resumable agent execution engine (Experimental)

## Security

- [Access Control](security/access-control.md) - Role-based permissions and audit logging
- [Guardrails](security/guardrails.md) - PII redaction, content filtering, schema validation
- [Memory](security/memory.md) - Long-term semantic memory for agents
- [Payments and Commerce](security/payments.md) - Agentic commerce journeys, protocol support, and validation paths

## Studio

- [ADK Studio](studio/studio.md) - Visual development environment for building agents
- [Action Nodes](studio/action-nodes.md) - Non-LLM programmatic nodes for automation workflows
- [Triggers](studio/triggers.md) - Webhook, schedule, and event triggers for workflows

## Development

- [Development Guidelines](development/development-guidelines.md) - Contributing guide and best practices
- [Performance 0.8](development/performance-0-8.md) - Optimization release examples and adoption-focused validation

---

## Validation Status

Copy-paste Cargo commands and dependency snippets in the README and official docs are validated by `scripts/check-doc-examples.sh`. CI also checks cargo-adk scaffolds with `scripts/check-cargo-adk-templates.sh`, rejects duplicate example target names with `scripts/check-example-name-collisions.sh`, and compiles workspace examples with `cargo check --workspace --examples`.
