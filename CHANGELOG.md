# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.4] - 2025-12-09

### Added
- **adk-graph crate**: LangGraph-style workflow orchestration
  - `StateGraph` for building complex agent workflows with state channels
  - `AgentNode` for wrapping LLM agents as graph nodes with input/output mappers
  - Conditional routing with `Router::by_field` and custom predicates
  - Human-in-the-loop (HITL) interrupts with `Interrupt::dynamic`
  - State checkpointing with `MemoryCheckpointer` for persistence and replay
  - Full `GraphInvocationContext` implementation for proper agent execution
- **adk-browser crate**: Browser automation with 46 WebDriver tools
  - `BrowserSession` wrapping thirtyfour WebDriver
  - Navigation, element interaction, screenshots, cookies, frames
  - Window/tab management, drag-and-drop, file uploads
  - PDF printing, JavaScript execution
- **adk-eval crate**: Agent evaluation framework
  - `TrajectoryEvaluator` for comparing tool call sequences
  - `SemanticEvaluator` for response similarity scoring
  - `RubricEvaluator` for LLM-based rubric assessment
  - Full `EvalInvocationContext` implementation for agent execution during evaluation
- 7 new graph examples:
  - `graph_agent` - Basic AgentNode usage
  - `graph_workflow` - Multi-agent pipeline (extractor → analyzer → formatter)
  - `graph_conditional` - Dynamic routing based on LLM decisions
  - `graph_react` - ReAct pattern with cyclic tool usage
  - `graph_supervisor` - Supervisor pattern with worker agents
  - `graph_hitl` - Human-in-the-loop interrupts
  - `graph_checkpoint` - State persistence and replay
- `eval_agent` example demonstrating evaluation framework
- Official documentation for graph agents, browser tools, and evaluation

### Fixed
- **AgentNode execution**: Now properly executes wrapped agents instead of returning empty events
- **after_agent_callback**: Now correctly stores and invokes the callback
- Clippy warning in adk-browser for field assignment style
- Documentation warnings for unresolved links in adk-model

### Changed
- All graph examples now use real LLM integration via `AgentNode` (no mock/placeholder code)
- Updated all crate versions to 0.1.4 with standardized workspace inheritance
- Improved documentation with complete AgentNode usage examples

## [0.1.3] - 2025-12-08

### Added
- **adk-realtime crate**: New crate for real-time voice-enabled AI agents
  - `RealtimeAgent` implementing `adk_core::Agent` trait with full callback/tool/instruction support
  - OpenAI Realtime API support (`gpt-4o-realtime-preview-2024-12-17`, `gpt-realtime`)
  - Gemini Live API support (`gemini-2.0-flash-live-preview-04-09`)
  - Bidirectional audio streaming (PCM16, G711 formats)
  - Server-side Voice Activity Detection (VAD)
  - Real-time tool calling during voice conversations
  - Multi-agent handoffs via `transfer_to_agent`
- 4 new realtime examples:
  - `realtime_basic` - Simple text-based realtime session
  - `realtime_vad` - Voice assistant with VAD
  - `realtime_tools` - Tool calling during voice conversations
  - `realtime_handoff` - Multi-agent routing system

### Changed
- Updated default Gemini model from `gemini-2.0-flash-exp` to `gemini-2.5-flash`
- Updated OpenAI model references to use `gpt-4.1` (latest)
- Updated Anthropic model references to use `claude-sonnet-4` (latest)
- Updated all documentation and examples with current model names

## [0.1.2] - 2025-12-07

### Added
- **OpenAI provider support**: Full integration with OpenAI's GPT models
  - `OpenAIClient` and `OpenAIConfig` for easy configuration
  - Streaming support with proper tool call accumulation
  - Compatible with GPT-4o, GPT-4o-mini, GPT-4-turbo, GPT-3.5-turbo
  - Feature flag: `adk-model = { features = ["openai"] }`
- **Anthropic provider support**: Full integration with Anthropic's Claude models
  - `AnthropicClient` and `AnthropicConfig` using the `claudius` crate
  - Streaming support with tool call support
  - Compatible with Claude Opus 4.5, Claude Sonnet 4.5, Claude 3.5 Sonnet, Claude 3 Opus
  - Feature flag: `adk-model = { features = ["anthropic"] }`
- New feature flag `all-providers` to enable Gemini, OpenAI, and Anthropic together
- 16 new OpenAI examples covering all ADK features:
  - `openai_basic`, `openai_tools`, `openai_workflow`, `openai_template`
  - `openai_parallel`, `openai_loop`, `openai_agent_tool`, `openai_structured`
  - `openai_artifacts`, `openai_mcp`, `openai_a2a`, `openai_server`, `openai_web`
  - `openai_sequential_code`, `openai_research_paper`, `debug_openai_error`
- 2 new Anthropic examples: `anthropic_basic`, `anthropic_tools`
- `MutableSession` struct in `adk-runner` for shared mutable session state
- `InvocationContext::with_mutable_session()` constructor for sharing sessions across contexts
- `InvocationContext::mutable_session()` accessor for the underlying mutable session
- New tests for `MutableSession` state propagation behavior
- New example: `structured_output` demonstrating JSON schema output constraints

### Fixed
- **Critical bug**: SequentialAgent now correctly propagates state between agents via `output_key`
  - Root cause: InvocationContext held an immutable snapshot of session state
  - Solution: Implemented `MutableSession` wrapper (matching ADK-Go's pattern) that allows
    state changes from `state_delta` to be immediately visible to downstream agents
  - This fix enables proper use of `output_key` in sequential/parallel agent workflows
- OpenAI 400 Bad Request errors caused by empty assistant messages (added placeholder content)
- OpenAI streaming empty Content accumulation issue

### Changed
- `InvocationContext` now internally uses `MutableSession` instead of immutable `SessionAdapter`
- Runner applies `state_delta` from events to the mutable session immediately after each event
- Agent transfers now share the same `MutableSession` to preserve state
- Updated README documentation with multi-provider examples

## [0.1.1] - 2025-11-30

### Fixed
- Clippy `redundant_pattern_matching` warning in test files
- Doc test for `ScopedArtifacts` using incorrect `Part` constructor
- Code formatting issues caught by `cargo fmt`
- Multiple doc tests in `adk-rust/src/lib.rs` with incorrect API usage:
  - `LoopAgent::new` signature (takes `Vec<Arc<dyn Agent>>`, use `.with_max_iterations()`)
  - `FunctionTool::new` handler signature (takes `Arc<dyn ToolContext>, Value`)
  - `McpToolset` API (uses `rmcp` crate, `McpToolset::new(client)`)
  - `SessionService::create` takes `CreateRequest` struct
  - Callback methods renamed to `after_model_callback`, `before_tool_callback`
  - `ArtifactService` trait and request/response structs
  - Server API uses `create_app_with_a2a`, `ServerConfig`, `AgentLoader`
  - Telemetry uses `init_telemetry` and `init_with_otlp` functions
- All clippy warnings for `--all-targets --all-features`:
  - Unused imports in test files and examples
  - Unused variables in example code (prefixed with underscore)
  - `unnecessary_literal_unwrap` in test assertions

### Changed
- Integration tests requiring `GEMINI_API_KEY` now marked with `#[ignore]` for CI compatibility

## [0.1.0] - 2025-11-30

Initial release - Published to crates.io.

### Features
- Complete Rust implementation of Google's ADK
- Core traits: Agent, Llm, Tool, Toolset, SessionService
- Agent types: LlmAgent, CustomAgent, SequentialAgent, ParallelAgent, LoopAgent, ConditionalAgent
- Gemini model integration with streaming support
- MCP (Model Context Protocol) integration via rmcp SDK
- Session management (in-memory and database backends)
- Artifact storage (in-memory and database backends)
- Memory system with semantic search
- Runner for agent execution with context management
- REST API server with Axum
- A2A (Agent-to-Agent) protocol support
- CLI with console mode and server mode
- Security configuration (CORS, timeouts, request limits)
- OpenTelemetry integration for observability

### Crates
- `adk-core` - Core traits and types
- `adk-agent` - Agent implementations
- `adk-model` - LLM integrations (Gemini)
- `adk-tool` - Tool system (FunctionTool, MCP, Google Search)
- `adk-session` - Session management
- `adk-artifact` - Binary artifact storage
- `adk-memory` - Semantic memory
- `adk-runner` - Agent execution runtime
- `adk-server` - HTTP server and A2A protocol
- `adk-cli` - Command-line launcher
- `adk-telemetry` - OpenTelemetry integration
- `adk-rust` - Umbrella crate

### Requirements
- Rust 1.75+
- Tokio async runtime
- Google API key for Gemini

[Unreleased]: https://github.com/zavora-ai/adk-rust/compare/v0.1.4...HEAD
[0.1.4]: https://github.com/zavora-ai/adk-rust/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/zavora-ai/adk-rust/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/zavora-ai/adk-rust/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/zavora-ai/adk-rust/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/zavora-ai/adk-rust/releases/tag/v0.1.0
