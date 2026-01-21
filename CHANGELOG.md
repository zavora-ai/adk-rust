# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.1] - 2026-01-21

### ⭐ Highlights
- **OpenAI Structured Output**: `output_schema` now works with OpenAI/Azure via `response_format` API
- **Ralph Autonomous Agent**: New example showcasing spec-driven development with loop agents
- **Local Model Support**: New examples for Ollama and OpenAI-compatible local APIs
- **Improved Error Handling**: Replaced `unwrap()` calls with proper error handling across crates

### Added
- **adk-model**: OpenAI/Azure clients now wire `output_schema` to `response_format` with `json_schema` type
  - Auto-injects `additionalProperties: false` at root level for strict mode compliance
  - Uses sanitized model name for schema name
- **adk-core**: `LlmRequest::with_response_schema()` and `with_config()` builder methods for structured output
- **adk-agent**: `LlmAgentBuilder::max_iterations()` to configure maximum LLM round-trips (default: 100)
- **adk-server**: `TaskStore` for in-memory A2A task persistence and retrieval
- **adk-tool**: `AgentTool` now forwards `state_delta` and `artifact_delta` to parent context
- **examples/ralph**: Autonomous agent example with loop workflow, PRD management, and file/git/test tools
- **examples/ollama_structured**: Structured JSON output with local Ollama models
- **examples/openai_local**: OpenAI client with local models via `OpenAIConfig::compatible()`
- **examples/openai_structured_basic**: Basic structured output example with OpenAI
- **examples/openai_structured_strict**: Strict schema example with nested objects

### Fixed
- **adk-model**: `output_schema` was ignored by OpenAI client - now properly sent as `response_format`
- **adk-session**: Replaced all `unwrap()` calls with proper error handling in `DatabaseSessionService`
- **adk-model**: Fixed rustdoc bare URL warning in `AzureConfig` documentation
- **adk-server**: A2A `tasks/get` endpoint now returns stored tasks instead of empty response

### Changed
- **adk-agent**: Default max iterations increased from 10 to 100 for `LlmAgent`

## [0.2.0] - 2026-01-06

### ⭐ Highlights
- **Documentation Overhaul**: All crate READMEs validated against actual implementations
- **API Consistency**: Fixed incorrect API examples across documentation

### Fixed
- Fixed `LlmAgentBuilder` API: use `.tool()` in loop instead of non-existent `.tools(vec![...])`
- Fixed `Runner::new()` examples: use `Launcher` for simple cases, `RunnerConfig` for advanced
- Fixed `SessionService::create()` API: use `CreateRequest` struct
- Fixed `BrowserConfig` API: use builder pattern instead of `::new(url)`
- Fixed `LoopAgent` API: use `vec![]` and `with_max_iterations()`
- Fixed dotenv → dotenvy in examples
- Removed non-existent `Launcher` methods from docs (`with_server_mode`, `with_user_id`, `with_session_id`)

### Changed
- All ADK crates bumped to version 0.2.0
- Rust edition updated to 2024, requires Rust 1.85+

## [0.1.9] - 2026-01-03

### ⭐ Highlights
- **mistral.rs Integration**: Complete native local LLM inference via `adk-mistralrs` crate
- **Production-Ready Error Handling**: Comprehensive error types with actionable suggestions
- **Diagnostic Logging**: Structured tracing with timing spans for model loading and inference
- **Performance Benchmarks**: Criterion benchmarks for configuration and conversion operations

### Added
- **adk-mistralrs** (`adk-mistralrs`): Native mistral.rs integration for local LLM inference
  - `MistralRsModel`: Basic text generation implementing ADK `Llm` trait
  - `MistralRsAdapterModel`: LoRA/X-LoRA adapter support with hot-swapping
  - `MistralRsVisionModel`: Vision-language model support for image understanding
  - `MistralRsEmbeddingModel`: Semantic embeddings for RAG and search
  - `MistralRsSpeechModel`: Text-to-speech synthesis with multi-speaker support
  - `MistralRsDiffusionModel`: Image generation with FLUX models
  - `MistralRsMultiModel`: Multi-model serving with routing
  - ISQ (In-Situ Quantization) support for memory-efficient inference
  - PagedAttention for longer context windows
  - UQFF pre-quantized model loading for faster startup
  - MCP client integration for external tools
  - MatFormer support for Gemma 3n models
  - Multi-GPU model splitting across devices
- **Error handling improvements**:
  - Structured error types with contextual fields (model_id, reason, suggestion)
  - Convenience constructors for common error patterns
  - Error classification methods (`is_recoverable()`, `is_config_error()`, `is_resource_error()`)
  - Actionable suggestions based on error content
- **Diagnostic logging**:
  - `tracing_utils` module with timing utilities
  - `TimingGuard` for automatic operation timing
  - Logging functions for model loading, inference, embeddings, image/speech generation
  - Token throughput metrics in inference logs
- **CI integration**:
  - `.github/workflows/mistralrs-tests.yml` for mistral.rs-specific testing
  - Separate jobs for unit tests, property tests, doc tests, and clippy
  - Optional integration tests with manual trigger
- **Performance benchmarks**:
  - Criterion benchmarks for configuration, error creation, type conversions
  - MCP configuration benchmarks
  - Optional inference benchmarks behind `bench-inference` feature flag
- **Property tests**:
  - 21 error message quality tests validating contextual information and suggestions
  - Tests for error classification consistency
  - Tests for all error types (model load, inference, adapters, media processing, etc.)
- **FileData Part support**: Added `Part::FileData` variant handling in `adk-server` and `adk-cli`
- **New examples**: `mistralrs_speech` (TTS) and `mistralrs_diffusion` (image generation)

### Changed
- All ADK crates bumped to version 0.1.9
- `adk-mistralrs` version updated to 0.1.9
- Updated README with benchmark documentation and performance tips
- Enhanced error messages with platform-specific suggestions (CUDA, Metal)

### Fixed
- Non-exhaustive pattern match for `Part::FileData` in `adk-server/src/a2a/parts.rs`
- Non-exhaustive pattern match for `Part::FileData` in `adk-cli/src/console.rs`

## [0.1.9] - 2025-12-28

### ⭐ Highlights
- **ADK Studio**: Complete visual agent builder with drag-and-drop workflow design
- **Real-Time Streaming**: Live SSE streaming with agent animations and trace events
- **Code Generation**: Compile visual workflows to production Rust code
- **Rust 2024 Edition**: Migrated to Rust 2024 edition for latest language features

### Added
- **ADK Studio** (`adk-studio`): Visual agent development environment
  - Drag-and-drop agent creation with ReactFlow-based canvas
  - Full agent palette: LLM Agent, Sequential, Loop, Parallel, Router agents
  - Tools support: Function, MCP, Browser, Google Search, Load Artifact, Exit Loop
  - Real-time SSE streaming with chat interface and session management
  - **Code generation**: Compile visual designs to Rust code with one click
  - **Build system**: Compile and run generated Rust executables from Studio
  - Monaco Editor integration for viewing/editing generated code
  - MenuBar with File, Templates, Help menus and 7 agent templates
  - Sub-agent support in container nodes with proper event ordering
  - MCP server templates with friendly display names and timeout handling
  - Function tool templates with description editing
  - Session memory persistence across chat interactions
  - Agent rename and enhanced LLM property configuration
- **Studio UI architecture** (`studio-ui`):
  - Component extraction: Canvas reduced by 83% via modular architecture
  - Custom node components: `LlmAgentNode`, `RouterNode`, `ThoughtBubble`
  - Layout system with auto-layout, horizontal/vertical toggle
  - Node activity animations during execution
  - State management with Zustand store
  - Real-time trace events in Events tab
- **Real-time streaming** (`StreamMode::Messages`):
  - Live agent execution with proper event accumulation
  - Trace events for tool calls/results in SSE stream
  - Agent start and model call events for detailed debugging
  - Node start/end trace events for sub-agent tracking
- **Router Agent**: Conditional routing based on LLM decisions
- **Codegen example**: `codegen_demo` showing code generation from all templates
- **Host flag**: `--host` flag for backend and studio management scripts

### 🔥 Breaking Changes
- **Rust 2024 Edition**: All crates now use `edition = "2024"` (requires Rust 1.85+)
- **Workspace Restructure**: `vendor/gemini-rust` → `adk-gemini`
  - Import paths change from `gemini_rust::*` to `adk_gemini::*`
  - Standardized workspace dependencies for consistency

### Changed
- All ADK crates bumped to version 0.1.9
- Generated `Cargo.toml` now uses ADK version 0.1.9
- Improved sub-agent display in containers (robot icon, LLM Agent label, tool descriptions)
- Sequential agent now properly passes conversation history between sub-agents
- Output mapper now accumulates text correctly across agent events
- Auto-detect reqwest dependency in codegen, add User-Agent header
- Build cache invalidation on project changes

### Fixed
- **adk-studio**: Real-time streaming now works correctly
- **adk-studio**: Drag-drop fixed for both agents and tools
- **adk-studio**: Keyboard delete properly handles agent/tool deletion
- **adk-studio**: Agents sorted by workflow order, positioned at top-left
- **adk-studio**: Save on agent delete, handle keyboard delete properly
- **adk-studio**: MCP codegen only generates tool loop if config exists
- **adk-studio**: Sub-agent tools properly added to builders in containers
- **adk-studio**: Tool clicks open config panel, entire tool item clickable
- **studio-ui**: Prevent layout rearrangement during chat execution
- **studio-ui**: Thought bubble moved inside node to prevent overlap
- **adk-agent**: Sequential agent properly passes conversation history between sub-agents
- **adk-agent**: Output mapper accumulates text correctly across agent events
- **adk-graph**: Sub-agent events include agent name in completion log
- **adk-graph**: Proper node_start/node_end trace events emitted

### Internal
- Tracing subscriber with JSON output for telemetry
- Grounding metadata display with markdown rendering
- Screenshot display in console
- Build output now streams in real-time
- Graph-based workflow design document added
- ADK Studio roadmap and UI requirements updated

## [0.1.7] - 2025-12-14

### Added
- **adk-guardrail**: New crate for agent safety and validation
  - `Guardrail` trait with async `validate()` returning `Pass`, `Fail`, or `Transform`
  - `GuardrailSet` and `GuardrailExecutor` for parallel execution with early exit
  - `Severity` levels: `Low`, `Medium`, `High`, `Critical`
  - Built-in guardrails:
    - `PiiRedactor` - Detects and redacts Email, Phone, SSN, CreditCard, IpAddress
    - `ContentFilter` - Blocks harmful content, off-topic responses, keywords, max length
    - `SchemaValidator` - JSON schema validation with markdown code block extraction
- **adk-agent**: Guardrails integration (feature-gated)
  - `LlmAgentBuilder::input_guardrails()` - Validate/transform user input
  - `LlmAgentBuilder::output_guardrails()` - Validate/transform model output
  - Enable with `adk-agent = { features = ["guardrails"] }`
- 3 new guardrail examples:
  - `guardrail_basic` - PII redaction and content filtering
  - `guardrail_schema` - JSON schema validation
  - `guardrail_agent` - Full agent integration
- **translator example**: Refactored with adk-rust best practices

### Changed
- Roadmap documents added for guardrails, cloud integrations, enterprise, adk-studio
- Updated adk-ui roadmap to implemented status

## [0.1.6] - 2025-12-12

### Added
- **adk-ui**: New modules for improved LLM reliability and developer experience:
  - `prompts.rs` - Tested system prompts (`UI_AGENT_PROMPT`) with few-shot examples
  - `templates.rs` - 10 pre-built UI templates (Registration, Login, Dashboard, etc.)
  - `validation.rs` - Server-side validation with `validate_ui_response()`
- **adk-ui**: Component enhancements:
  - `Button`: Added `icon` field for icon buttons
  - `TextInput`: Added `min_length`, `max_length` validation
  - `NumberInput`: Added `default_value` field
  - `Table`: Added `sortable`, `striped`, `page_size` fields
  - `Chart`: Added `x_label`, `y_label`, `show_legend`, `colors` fields
  - `render_layout`: Added `key_value`, `list`, `code_block` section types
- **npm package**: Published `@zavora-ai/adk-ui-react@0.1.6` to npm
- **streaming_demo**: New example showing `UiUpdate` for real-time progress bar updates
- React client improvements:
  - Clickable example prompts table with instant send
  - Dark mode and theme support
  - Table sorting and pagination
  - Chart colors and axis labels

### Fixed
- All 10 render tools now use proper error handling (replaced `unwrap()`)
- TypeScript types updated for all new Rust schema fields

### Changed
- All crates now use workspace version inheritance (`version.workspace = true`)

## [0.1.5] - 2025-12-10

### Added
- **DeepSeek provider support**: Native integration with DeepSeek's LLM models
  - `DeepSeekClient` and `DeepSeekConfig` for easy configuration
  - Support for `deepseek-chat` (standard) and `deepseek-reasoner` (thinking mode)
  - Thinking mode with chain-of-thought reasoning (`<thinking>` tags in output)
  - Context caching for 10x cost reduction on repeated prefixes
  - Full function calling/tool support
  - Streaming support with proper response accumulation
  - Feature flag: `adk-model = { features = ["deepseek"] }`
- 8 new DeepSeek examples:
  - `deepseek_basic` - Basic chat completion
  - `deepseek_reasoner` - Thinking mode with chain-of-thought
  - `deepseek_tools` - Function calling with weather/calculator tools
  - `deepseek_thinking_tools` - Combined reasoning and tool use
  - `deepseek_caching` - Context caching demonstration
  - `deepseek_sequential` - Multi-agent pipeline (Researcher → Analyst → Writer)
  - `deepseek_supervisor` - Supervisor pattern with specialist agents
  - `deepseek_structured` - Structured JSON output
- DeepSeek documentation in official docs and all READMEs

### Fixed
- CI linker OOM crashes: Now using `mold` linker with reduced debug info
- Function response role mapping for DeepSeek API (uses "tool" not "function")
- Placeholder GitHub URLs updated to `zavora-ai/adk-rust`

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

[Unreleased]: https://github.com/zavora-ai/adk-rust/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/zavora-ai/adk-rust/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/zavora-ai/adk-rust/compare/v0.1.9...v0.2.0
[0.1.9]: https://github.com/zavora-ai/adk-rust/compare/v0.1.7...v0.1.9
[0.1.7]: https://github.com/zavora-ai/adk-rust/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/zavora-ai/adk-rust/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/zavora-ai/adk-rust/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/zavora-ai/adk-rust/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/zavora-ai/adk-rust/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/zavora-ai/adk-rust/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/zavora-ai/adk-rust/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/zavora-ai/adk-rust/releases/tag/v0.1.0
