# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.2] - 2026-02-17

### ⭐ Highlights
- **adk-rag**: New RAG crate with modular pipeline, 6 vector store backends (InMemory, Qdrant, LanceDB, pgvector, SurrealDB), 3 chunking strategies, and agentic retrieval via `RagTool`
- **Generation Config on Agents**: `LlmAgentBuilder` now supports `temperature()`, `top_p()`, `top_k()`, `max_output_tokens()` convenience methods and full `generate_content_config()` for agent-level LLM tuning
- **Gemini Model URL Fix**: `Model::Custom` variant now correctly prefixes `models/` in API URLs, fixing `PerformRequestNew` errors for all Gemini tool-calling examples
- **Gemini Models Discovery API**: New `list_models()` and `get_model()` methods on `Gemini` client for runtime model discovery
- **Expanded Model Enum**: `Model` enum expanded from 5 to 22 variants covering Gemini 3, 2.5, 2.0, and embedding models

### Added

#### adk-rag (NEW CRATE)
- New `adk-rag` crate: modular Retrieval-Augmented Generation for ADK-Rust agents
- Core traits: `EmbeddingProvider`, `VectorStore`, `Chunker`, `Reranker`
- `InMemoryVectorStore` with cosine similarity search (no external deps)
- Three chunking strategies: `FixedSizeChunker`, `RecursiveChunker`, `MarkdownChunker`
- `RagPipeline` orchestrator for ingest (chunk → embed → store) and query (embed → search → rerank → filter) workflows
- `RagPipelineBuilder` with builder-pattern configuration
- `RagTool` implementing `adk_core::Tool` for agentic retrieval — agents call `rag_search` on demand
- Feature-gated embedding providers: `GeminiEmbeddingProvider` (`gemini`), `OpenAIEmbeddingProvider` (`openai`)
- Feature-gated vector stores: `QdrantVectorStore` (`qdrant`), `LanceDBVectorStore` (`lancedb`), `PgVectorStore` (`pgvector`)
- `SurrealVectorStore` (`surrealdb`) with HNSW cosine indexing — supports in-memory, RocksDB, and remote server modes
- `rag` feature flag added to `adk-rust` umbrella crate (included in `full`)
- 7 examples: `rag_basic`, `rag_markdown`, `rag_agent`, `rag_recursive`, `rag_reranker`, `rag_multi_collection`, `rag_surrealdb`
- Official documentation page at `docs/official_docs/tools/rag.md` with validated code samples

#### adk-agent
- `LlmAgentBuilder::generate_content_config()` — set full `GenerateContentConfig` at the agent level
- `LlmAgentBuilder::temperature()` — convenience method for setting default temperature
- `LlmAgentBuilder::top_p()` — convenience method for setting default top-p
- `LlmAgentBuilder::top_k()` — convenience method for setting default top-k
- `LlmAgentBuilder::max_output_tokens()` — convenience method for setting default max output tokens
- Agent-level generation config is merged with `output_schema` in the LLM request loop

#### adk-core
- `GenerateContentConfig` now derives `Default`

#### adk-gemini
- `Model` enum expanded with 17 new variants:
  - Gemini 3: `Gemini3ProPreview`, `Gemini3ProImagePreview`, `Gemini3FlashPreview`
  - Gemini 2.5: `Gemini25Pro`, `Gemini25ProPreviewTts`, `Gemini25FlashPreview092025`, `Gemini25FlashImage`, `Gemini25FlashLive122025`, `Gemini25FlashLive092025`, `Gemini25FlashPreviewTts`, `Gemini25FlashLite`, `Gemini25FlashLitePreview092025`
  - Gemini 2.0 (deprecated): `Gemini20Flash`, `Gemini20Flash001`, `Gemini20FlashExp`, `Gemini20FlashLite`, `Gemini20FlashLite001`
- `Model::Gemini25FlashImagePreview` marked `#[deprecated]` (use `Gemini25FlashImage`)
- `Model::Gemini20Flash*` variants marked `#[deprecated]` (shutting down March 31, 2026)
- `model_info` module with `ModelInfo` and `ListModelsResponse` types for the Models API
- `Gemini::list_models(page_size)` — paginated stream of available model metadata
- `Gemini::get_model(name)` — fetch metadata for a specific model (token limits, supported methods, etc.)
- `GeminiBackend::list_models()` and `GeminiBackend::get_model()` trait methods with default unsupported impls
- `StudioBackend` implementation of `list_models` and `get_model` via REST
- `ModelInfo` and `ListModelsResponse` re-exported from `prelude`

#### adk-studio
- Generation config parameters (`temperature`, `top_p`, `top_k`, `max_output_tokens`) added to `AgentSchema`
- Advanced Settings section in LlmProperties panel for configuring generation parameters
- Code generation emits `.temperature()`, `.top_p()`, `.top_k()`, `.max_output_tokens()` builder calls

#### Examples
- `gemini_multimodal` — inline image analysis, multi-image comparison, and vision agent pattern using `Part::InlineData` with Gemini
- `anthropic_multimodal` — image analysis with Claude using `Part::InlineData` (requires `--features anthropic`)
- `multi_turn_tool` — inventory management scenario demonstrating multi-turn tool conversations with both Gemini (default) and OpenAI (`--features openai`)
- `rag_surrealdb` — SurrealDB vector store with embedded in-memory mode

### Fixed
- **adk-server**: Runtime endpoints (`run_sse`, `run_sse_compat`) now process attachments and `inlineData` instead of silently dropping them — base64 validation, size limits, and per-provider content conversion (#142, #143)
- **adk-model**: All providers now handle `InlineData` and `FileData` parts — native image/audio/PDF blocks for Anthropic and OpenAI, text fallback for DeepSeek/Groq/Ollama, Gemini response `InlineData` no longer silently dropped (#142, #143)
- **adk-runner**: `conversation_history()` now preserves `function`/`tool` content roles instead of overwriting them to `model`, fixing multi-turn tool conversations (#139)
- **adk-gemini**: `PerformRequestNew` error variant now displays the underlying reqwest error instead of swallowing it
- **adk-gemini**: `From<String> for Model` now correctly maps known model names (e.g. `"gemini-2.5-flash"`) to proper enum variants instead of always creating `Custom`
- **adk-gemini**: `Model::Custom` `Display` impl now adds `models/` prefix when missing, fixing broken API URLs like `gemini-2.5-flash:streamGenerateContent` → `models/gemini-2.5-flash:streamGenerateContent`

### Changed
- CI: sccache stats, test results, and clippy summary now appear in GitHub Actions step summary
- CI: devenv scripts renamed to `ws-*` prefix to avoid collisions with Cargo binaries
- `AGENTS.md` consolidated with crates.io publishing guide and PR template improvements
- Removed broken `.pre-commit-config.yaml` symlink

### Contributors
Thanks to the following people for their contributions to this release:
- **@mikefaille** — major contributions to `adk-realtime` (tokio-tungstenite upgrade, rustls migration), LiveKit WebRTC bridge groundwork, CI improvements (sccache summaries, devenv script fixes), environment sync, documentation consolidation, and PR template (#134, #136, #137)
- **@rohan-panickar** — attachment support for runtime endpoints and multi-provider content conversion (#142, #143), fix for tool context role preservation (#139)
- **@dhruv-pant** — Gemini service account auth and configurable retry logic

## [0.3.1] - 2026-02-14

### ⭐ Highlights
- **Vertex AI Streaming**: `adk-gemini` refactored with `GeminiBackend` trait — pluggable `StudioBackend` (REST) and `VertexBackend` (REST SSE streaming + gRPC fallback)
- **Realtime Stabilization**: `adk-realtime` audio transport rewritten with raw bytes, Gemini Live session corrected, event types renamed for OpenAI SDK alignment
- **Multi-Provider Codegen**: ADK Studio code generation now supports Gemini, OpenAI, Anthropic, DeepSeek, Groq, and Ollama (was hardcoded to Gemini)
- **2026 Model Names**: All docs, examples, and source defaults updated to current model names (gemini-2.5-flash, gpt-5-mini, claude-sonnet-4-5-20250929, etc.)
- **Response Parsing Tests**: 25 rigorous tests covering Gemini response edge cases (safety ratings, streaming chunks, function calls, grounding metadata, citations)
- **Code Health**: Span-based line numbers in doc-audit analyzer, validation refactor in adk-ui, dead code cleanup, CONTRIBUTING.md rewrite

### Added

#### adk-gemini
- `GeminiBackend` trait with `send_request()` and `send_streaming_request()` methods
- `StudioBackend` — AI Studio REST implementation (default)
- `VertexBackend` — Vertex AI REST SSE streaming with gRPC fallback, ADC/service account/WIF auth
- `GeminiBuilder` for constructing clients with explicit backend selection
- `Model::GeminiEmbedding001` variant for `gemini-embedding-001` (3072 dimensions, replaces `text-embedding-004`)
- `Model::TextEmbedding004` marked `#[deprecated]` with compiler warning
- 25 response parsing tests: basic text, multi-candidate, safety ratings (string + numeric), blocked prompts, streaming chunks, function calls, inline data, grounding metadata, citations, usage metadata with thinking tokens, all FinishReason variants, unknown enum graceful degradation, round-trip serialization

#### adk-realtime
- Audio transport changed from `String` (base64) to `Vec<u8>` (raw bytes) with custom serde for base64 wire format
- `BoxedModel` changed from `Box<dyn RealtimeModel>` to `Arc<dyn RealtimeModel>` for thread-safe sharing
- ClientEvent renames: `AudioInput`→`AudioDelta`, `AudioCommit`→`InputAudioBufferCommit`, `AudioClear`→`InputAudioBufferClear`, `ItemCreate`→`ConversationItemCreate`, `CreateResponse`→`ResponseCreate`, `CancelResponse`→`ResponseCancel`
- `EventHandler::on_audio` and `AudioCallback` changed from `&str` (base64) to `&[u8]` (raw bytes)
- Gemini Live session rewrite: `send_text` uses `client_content` (correct Gemini API), handles binary WebSocket messages, `GeminiLiveBackend` enum for backend selection
- `GeminiRealtimeModel` now accepts `GeminiLiveBackend` instead of raw API key string
- `RealtimeError::audio()` convenience constructor
- Added `bytes`, `bytemuck` dependencies; optional `adk-gemini` dep behind `gemini` feature flag
- Feature flags: `openai`, `gemini`, `full`

#### adk-rag (NEW CRATE)
- New `adk-rag` crate: modular Retrieval-Augmented Generation for ADK-Rust agents
- Core traits: `EmbeddingProvider`, `VectorStore`, `Chunker`, `Reranker`
- `InMemoryVectorStore` with cosine similarity search (no external deps)
- Three chunking strategies: `FixedSizeChunker`, `RecursiveChunker`, `MarkdownChunker`
- `RagPipeline` orchestrator for ingest (chunk → embed → store) and query (embed → search → rerank → filter) workflows
- `RagTool` implementing `adk_core::Tool` for agentic retrieval — agents call `rag_search` on demand
- Feature-gated embedding providers: `GeminiEmbeddingProvider` (`gemini`), `OpenAIEmbeddingProvider` (`openai`)
- Feature-gated vector stores: `QdrantVectorStore` (`qdrant`), `LanceDBVectorStore` (`lancedb`), `PgVectorStore` (`pgvector`)
- `rag` feature flag added to `adk-rust` umbrella crate (included in `full`)
- 6 examples: `rag_basic`, `rag_markdown`, `rag_agent`, `rag_recursive`, `rag_reranker`, `rag_multi_collection`
- Official documentation page at `docs/official_docs/tools/rag.md` with validated code samples
- Published to crates.io as `adk-rag v0.3.1`

#### adk-studio
- Multi-provider LLM support in code generation (Gemini, OpenAI, Anthropic, DeepSeek, Groq, Ollama)
- Provider-specific environment variable detection and validation
- Ollama local model support with configurable base URL

#### Examples
- `verify_backend_selection` — validates Studio backend (default, with_model, builder, streaming, embedding, v1 API)
- `verify_vertex_streaming` — validates Vertex AI backend (non-streaming, REST SSE streaming, embedding)

### Fixed
- **adk-model**: `GeminiModel::new()` now uses `Gemini::with_model(api_key, model_name)` instead of ignoring the provided model name (bug #77)
- **adk-studio**: CORS restricted to localhost origins only (was allowing all origins)
- **adk-ui**: `NumberInput` validation no longer false-fails when only `min` is set (`Some(min) > None` was always true)
- **adk-graph**: Replaced `eprintln!("DEBUG: ...")` with `tracing::debug!()` in `AgentNode::execute_stream` and `CompiledGraph::stream` (stderr leakage in library code)
- **adk-doc-audit**: Line numbers now use `syn::Span::start().line` instead of hardcoded `0`
- **adk-doc-audit**: `suggest_similar_crate_names` and `suggest_similar_api_names` made static (removed dead `_static` variants)
- **adk-doc-audit**: Deleted stale `test.md` artifact
- **adk-ui**: Validation refactored from monolithic match into per-type `Validate` trait impls (Text, Button, TextInput, NumberInput, Select, Table, Chart, Card, Modal, Stack, Grid, Tabs)

### Changed
- All model name defaults updated to 2026 versions across 95+ files:
  - `gemini-2.0-flash` → `gemini-2.5-flash`
  - `gpt-4o` / `gpt-4o-mini` → `gpt-5-mini`
  - `claude-sonnet-4-20250514` → `claude-sonnet-4-5-20250929`
  - `gemini-2.0-flash-live-preview-04-09` → `gemini-live-2.5-flash-native-audio`
- `adk-doc-audit` now depends on `proc-macro2` with `span-locations` feature for accurate line numbers
- `CONTRIBUTING.md` rewritten with full 25+ crate inventory, build commands, architecture notes
- `.kiro/` and `.vite/` excluded from git tracking
- `.gitignore` cleaned up (removed absolute paths, duplicate entries)
- Added `.skills/` with Kiro skill definitions for agent workflows

### Documentation
- Updated all example model names to 2026 versions (PRs #79-#82)
- Updated source code default model names across all provider crates

## [0.3.0] - 2026-02-08

### ⭐ Highlights
- **Context Compaction**: Sliding-window summarization of older events to reduce LLM context size (ADK Python parity)
- **Workflow Agent Hardening**: ConditionalAgent, LlmConditionalAgent, and ParallelAgent production fixes
- **adk-core Production Hardening**: Security limits, validation, provider-agnostic Event, hand-written template parser
- **Action Node Code Generation**: Full Rust codegen for HTTP, Database, Email, and Code action nodes
- **Workflow Triggers**: Complete trigger system with webhook, schedule, and event triggers
- **rmcp 0.14 Upgrade**: Updated MCP integration with HTTP transport, authentication, and auto-reconnect
- **Plugin System**: Extensible callback architecture for agent lifecycle hooks (adk-go parity)
- **OpenAI Structured Output**: `output_schema` now works with OpenAI/Azure via `response_format` API

### Added

#### adk-core
- `EventCompaction` struct for compacted event metadata (start/end timestamps, summary content)
- `EventActions.compaction` field for marking events as compaction summaries
- `BaseEventsSummarizer` trait for custom summarization strategies
- `EventsCompactionConfig` struct (compaction_interval, overlap_size, summarizer)
- `validate_state_key()` and `MAX_STATE_KEY_LEN` (256 bytes) for state key validation
- `MAX_INLINE_DATA_SIZE` (10MB) limit on `Part::InlineData`
- `provider_metadata: HashMap<String, String>` on `Event` — provider-agnostic replacement for GCP-specific fields
- `has_trailing_code_execution_result()` on `Event` for detecting pending code execution results
- Hand-written placeholder parser for instruction templates (replaces regex dependency)
- `LlmRequest::with_response_schema()` and `with_config()` builder methods for structured output

#### adk-agent
- `LlmEventSummarizer` — LLM-based event summarizer with configurable prompt template
- `LlmAgentBuilder::max_iterations()` to configure maximum LLM round-trips (default: 100)

#### adk-runner
- `compaction_config` field on `RunnerConfig` for enabling automatic context compaction
- Re-exports `BaseEventsSummarizer` and `EventsCompactionConfig` from `adk-core`
- Compaction triggers after invocation when user-event count reaches interval
- `MutableSession::conversation_history()` respects compaction events — replaces old events with summary

#### adk-model
- OpenAI/Azure clients now wire `output_schema` to `response_format` with `json_schema` type
  - Auto-injects `additionalProperties: false` at root level for strict mode compliance
  - Uses sanitized model name for schema name

#### adk-tool
- `ConnectionRefresher` for automatic MCP reconnection
  - `ConnectionFactory` trait for creating new connections
  - `RefreshConfig` for retry settings (max_attempts, retry_delay_ms)
  - `RetryResult<T>` to indicate if reconnection occurred
  - `should_refresh_connection()` to detect refreshable errors
  - `SimpleClient` wrapper for servers without reconnect support
  - Handles: connection closed, EOF, broken pipe, session not found, transport errors
- `McpHttpClientBuilder` for remote MCP server connections
  - Streamable HTTP transport (SEP-1686 compliant)
  - `with_auth()` for authentication configuration
  - `timeout()` for request timeout configuration
  - `header()` for custom headers
- `McpAuth` enum for MCP authentication
  - `McpAuth::bearer(token)` - Bearer token authentication
  - `McpAuth::api_key(header, key)` - API key in custom header
  - `McpAuth::oauth2(config)` - OAuth2 client credentials flow
- `OAuth2Config` for OAuth2 authentication (client credentials flow, token caching)
- `McpTaskConfig` for long-running operations (polling, timeout, max attempts)
- New feature flag `http-transport` for remote MCP servers
- `AgentTool` now forwards `state_delta` and `artifact_delta` to parent context
- Upgraded rmcp from 0.9 to 0.14

#### adk-plugin
- New plugin system crate (adk-go feature parity)
  - `Plugin` and `PluginConfig` for bundling related callbacks
  - `PluginBuilder` for fluent plugin construction
  - `PluginManager` for coordinating callback execution across plugins
  - Run lifecycle callbacks: `on_user_message`, `on_event`, `before_run`, `after_run`
  - Agent callbacks: `before_agent`, `after_agent`
  - Model callbacks: `before_model`, `after_model`, `on_model_error`
  - Tool callbacks: `before_tool`, `after_tool`, `on_tool_error`
  - Helper functions: `log_user_messages()`, `log_events()`, `collect_metrics()`

#### adk-server
- `TaskStore` for in-memory A2A task persistence and retrieval

#### adk-studio
- HTTP action node code generation (all methods, auth, body types, response handling)
- Database action node code generation (PostgreSQL, MySQL, SQLite via sqlx; MongoDB; Redis)
- Email action node code generation (SMTP send via lettre; IMAP monitor via imap + native-tls)
- Code action node code generation (JavaScript via boa_engine with sandboxing)
- Predecessor output injection for all action node types
- Smart Build button (detects when recompilation is needed)
- Webhook trigger endpoints (async, sync, GET)
- Schedule trigger service (cron-based with `last_executed` tracking)
- Event trigger endpoints (source/eventType matching, JSONPath filters)
- Trigger-aware Run button with type-specific default prompts
- Webhook event SSE notifications to UI

#### Examples
- `examples/ralph`: Autonomous agent with loop workflow, PRD management, and file/git/test tools
- `examples/ollama_structured`: Structured JSON output with local Ollama models
- `examples/openai_local`: OpenAI client with local models via `OpenAIConfig::compatible()`
- `examples/openai_structured_basic`: Basic structured output example with OpenAI
- `examples/openai_structured_strict`: Strict schema example with nested objects
- `examples/mcp_http`: Remote MCP server example (Fetch, Sequential Thinking)
- `examples/mcp_oauth`: GitHub Copilot MCP authentication example

#### Dependencies (Generated Projects)
- `reqwest` — auto-detected for HTTP action nodes
- `sqlx` — auto-detected per database type (postgres/mysql/sqlite features)
- `mongodb` — auto-detected for MongoDB action nodes
- `redis` — auto-detected for Redis action nodes
- `lettre` — auto-detected for Email send nodes
- `imap` + `native-tls` — auto-detected for Email monitor nodes
- `boa_engine` — auto-detected for Code action nodes

### Fixed
- **adk-agent**: `ConditionalAgent::sub_agents()` now returns branch agents (was returning empty slice)
- **adk-agent**: `LlmConditionalAgent::sub_agents()` now returns route + default agents (was returning empty slice)
- **adk-agent**: `ParallelAgent` now drains all futures before propagating first error (prevents resource leaks)
- **adk-agent**: Default max iterations increased from 10 to 100 for `LlmAgent`
- **adk-core**: `function_call_ids()` now falls back to function name when call ID is `None` (Gemini compatibility)
- **adk-core**: Removed GCP-specific fields from `Event` (replaced with `provider_metadata`)
- **adk-core**: Removed phantom `adk-3d-ui` workspace member
- **adk-model**: `output_schema` was ignored by OpenAI client — now properly sent as `response_format`
- **adk-model**: Fixed rustdoc bare URL warning in `AzureConfig` documentation
- **adk-session**: Replaced all `unwrap()` calls with proper error handling in `DatabaseSessionService`
- **adk-server**: A2A `tasks/get` endpoint now returns stored tasks instead of empty response
- **adk-studio**: Replaced non-existent `NodeError::Other` with `GraphError::NodeExecutionFailed` in all generated code
- **adk-studio**: Fixed sqlx type inference in database codegen by splitting fetch and map operations
- **adk-studio**: Added missing `sqlx::Row` and `sqlx::Column` imports in database codegen
- **adk-studio**: Fixed moved value error when capturing row count before consuming rows in JSON macro
- **adk-studio**: Run button now correctly uses trigger-specific default prompts
- **adk-studio**: `sendingRef` now properly resets on cancel, allowing re-runs
- **adk-studio**: Cron parsing now uses 6-field format (with seconds) for `cron` crate compatibility
- **adk-tool**: Bearer auth now passes raw token (rmcp adds "Bearer " prefix automatically)
- **Security**: Updated lodash to fix prototype pollution vulnerability (CVE-2020-8203)
- **Security**: Updated vite/esbuild to fix server.fs.deny bypass (CVE-2025-0291)
- **Security**: Updated rsa crate to fix Marvin Attack vulnerability (RUSTSEC-2023-0071)

### Documentation
- Added context compaction guide: `docs/official_docs/sessions/context-compaction.md`
- Updated all crate READMEs with v0.3.0 version references
- Updated all official docs with v0.3.0 version references
- Updated adk-core, adk-agent, adk-runner READMEs with compaction, security, and production hardening details
- Updated events and runner official docs with new EventActions fields and compaction config

### Migration Guide

**From 0.2.x to 0.3.0:**

- All crate versions bumped to `0.3.0`. Update your `Cargo.toml` dependencies.
- `Event` no longer has GCP-specific fields — use `provider_metadata` HashMap instead.
- rmcp 0.14 breaking changes were handled internally in `adk-tool`. Your existing MCP code using `McpToolset::new(client)` continues to work unchanged.

**New features available:**

```rust
// Context compaction for long-running sessions
use adk_runner::{Runner, RunnerConfig, EventsCompactionConfig};
use adk_agent::LlmEventSummarizer;

let config = RunnerConfig {
    compaction_config: Some(EventsCompactionConfig {
        compaction_interval: 3,
        overlap_size: 1,
        summarizer: Arc::new(LlmEventSummarizer::new(model.clone())),
    }),
    ..
};

// HTTP transport for remote MCP servers (requires http-transport feature)
use adk_tool::McpHttpClientBuilder;

let toolset = McpHttpClientBuilder::new("https://remote.mcpservers.org/fetch/mcp")
    .timeout(Duration::from_secs(30))
    .connect()
    .await?;

// Authentication for protected MCP servers
use adk_tool::{McpHttpClientBuilder, McpAuth};

let toolset = McpHttpClientBuilder::new("https://api.githubcopilot.com/mcp/")
    .with_auth(McpAuth::bearer(std::env::var("GITHUB_TOKEN")?))
    .connect()
    .await?;
```

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

[Unreleased]: https://github.com/zavora-ai/adk-rust/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/zavora-ai/adk-rust/compare/v0.2.0...v0.3.0
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
