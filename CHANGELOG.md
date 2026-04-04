# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.5.0] - 2026-03-26

### Added

#### Realtime — LiveKit Typestate Builder, OpenAI Protocol Centralization ([@mikefaille](https://github.com/mikefaille))

- **`LiveKitConfig`** (`adk-realtime`): Secure LiveKit configuration with `secrecy::SecretString` for API keys. URL validation and empty-credential rejection at construction time.
- **`LiveKitRoomBuilder`** (`adk-realtime`): Typestate builder for LiveKit room connections. `identity` is required at compile time. Supports optional audio track setup, room name, and custom video grants.
- **`LiveKitError`** (`adk-realtime`): Dedicated error type for LiveKit operations (config, token generation, connection).
- **`OpenAIProtocolHandler<T>`** (`adk-realtime`): Generic protocol handler wrapping any `OpenAITransportLink` transport. Implements `RealtimeSession` for both WebSocket and WebRTC.
- **`OpenAITransportLink` trait** (`adk-realtime`): Transport abstraction for OpenAI Realtime API. Default implementations for audio encoding and session configuration. WebRTC overrides for direct media track access.
- **Centralized OpenAI protocol** (`adk-realtime`): Shared `convert_config_to_openai()` and `translate_client_message()` functions used by both WebSocket and WebRTC transports.
- **New examples**: `debug_gemini`, `debug_livekit_auth`, `livekit_gemini_bridge`.

#### Developer Ergonomics — Parallel Dispatch, Builder, Tool Metadata, Macro Attributes

- **`ToolExecutionStrategy`** (`adk-core`): New enum with `Sequential` (default), `Parallel`, and `Auto` variants controlling how multiple tool calls from a single LLM response are dispatched.
- **Tool metadata** (`adk-core`): `is_read_only()` and `is_concurrency_safe()` default methods on the `Tool` trait. Both return `false` by default. Used by `Auto` strategy to partition tools for concurrent execution.
- **`FunctionTool` extensions** (`adk-tool`): `with_read_only(bool)` and `with_concurrency_safe(bool)` builder methods.
- **`SimpleToolContext`** (`adk-tool`): Lightweight `ToolContext` implementation for non-agent callers (testing, MCP servers, sub-agent delegation). Construct with `SimpleToolContext::new("caller-name")`.
- **`StatefulTool<S>`** (`adk-tool`): Generic wrapper managing `Arc<S>` lifetime for stateful tool closures. Clones the `Arc` per invocation. Mirrors all `FunctionTool` builder methods.
- **`RunnerConfigBuilder`** (`adk-runner`): Typestate builder for `Runner` construction. Enforces required fields (`app_name`, `agent`, `session_service`) at compile time. Access via `Runner::builder()`.
- **`Runner::run_str()`** (`adk-runner`): Convenience method accepting `&str` for `user_id` and `session_id`. Validates and converts internally; returns error before agent loop on invalid input.
- **`LlmAgentBuilder::tool_execution_strategy()`** (`adk-agent`): Per-agent strategy override. Defaults to `Sequential` when not set.
- **Parallel tool dispatch** (`adk-agent`): Refactored `LlmAgent` dispatch loop supporting `Sequential`, `Parallel`, and `Auto` modes. Error isolation — failed tools produce JSON error responses without aborting the batch.
- **`#[tool]` macro attributes** (`adk-rust-macros`): `#[tool(read_only)]`, `#[tool(concurrency_safe)]`, `#[tool(long_running)]` — set tool metadata directly in the macro. Plain `#[tool]` unchanged.
- **Non-breaking field addition policy** (`STABILITY.md`): Documented policy requiring `Option<T>` with defaults for new fields on public structs in Stable-tier crates.

#### Competitive Improvements — Stability, Ergonomics, Encryption, Graph Resume, Tool Search

- **STABILITY.md**: New stability roadmap at the repository root defining three tiers (Stable, Beta, Experimental) with contracts, a crate-tier mapping table for every public `adk-*` crate, deprecation lifecycle policy (N+2 minor releases with `#[deprecated(since, note)]`), and 1.0 milestone criteria with GitHub milestone link.
- **Semver CI enforcement**: New `.github/workflows/semver.yml` runs `cargo semver-checks check-release` on every PR — fails for Stable-tier crates, warns for Beta/Experimental.
- **`provider_from_env()`** (`adk-rust`): Auto-detect LLM provider from environment variables. Checks `ANTHROPIC_API_KEY` → `OPENAI_API_KEY` → `GOOGLE_API_KEY` in precedence order, returns `Arc<dyn Llm>`. Feature-gated per provider.
- **`adk::run()`** (`adk-rust`): Single-function agent invocation — `run("instructions", "input").await` handles provider detection, session creation, agent building, and execution. Returns `Result<String>`.
- **MCP Resource API** (`adk-tool`): `McpToolset::list_resources()`, `list_resource_templates()`, and `read_resource(uri)` methods delegating to rmcp's `resources/list`, `resourceTemplates/list`, and `resources/read` protocol methods. Returns empty vec when server doesn't support resources. Re-exports `Resource`, `ResourceTemplate`, `ResourceContents` from `rmcp::model`.
- **Graph durable resume** (`adk-graph`): `PregelExecutor` now checks for existing checkpoints before starting execution. If a checkpoint exists for the thread ID, state, pending nodes, and step are restored from it — skipping already-completed nodes. Both `run()` and `run_stream()` support resume. New `StreamEvent::Resumed` variant emitted when execution resumes from a checkpoint.
- **Deepgram streaming STT** (`adk-audio`): Full WebSocket streaming implementation for `DeepgramStt::transcribe_stream()` — connects to `wss://api.deepgram.com/v1/listen`, forwards audio frames as binary messages, yields interim and final `Transcript` values. Supports diarization, language detection, and model selection.
- **Structured tool output fix** (`adk-model`): Shared `serialize_tool_result()` helper prevents double-encoding of JSON objects in tool results across all 7 provider convert modules (OpenAI, Anthropic, Groq, DeepSeek, Azure AI, Bedrock, Ollama).
- **`InterruptionDetection` enum** (`adk-realtime`): `Manual` (default) and `Automatic` variants controlling how voice activity detection handles user interruptions. Added to `RealtimeConfig` with `with_interruption_detection()` builder method.
- **`EncryptionKey`** (`adk-session`): AES-256-GCM key management behind `encrypted-session` feature flag. `generate()`, `from_env(var_name)`, `from_bytes(&[u8])` constructors. Debug impl redacts key bytes.
- **`EncryptedSession<S>`** (`adk-session`): Transparent encryption wrapper for any `SessionService`. Encrypts state with AES-256-GCM (random 96-bit nonce, stored as `[nonce || ciphertext]`). Supports key rotation — tries current key first, falls back to previous keys, re-encrypts with current key on successful fallback.
- **`ToolSearchConfig`** (`adk-anthropic`): Regex-based tool name filtering. `matches(tool_name)` method compiles pattern and checks match.
- **`AnthropicConfig::with_tool_search()`** (`adk-model`): Optional `ToolSearchConfig` on the Anthropic provider — when set, only tools matching the regex pattern are sent to the API.
- **Validation examples**: Three standalone example crates (`competitive_ergonomics`, `competitive_graph_resume`, `competitive_tool_search`) exercising all new APIs with 37 runtime assertions.

#### Realtime Context Mutation & LiveKit Performance ([@mikefaille](https://github.com/mikefaille))

- **Provider-agnostic context mutation** (`adk-realtime`, #232): Mid-session instruction and tool swapping without dropping the call. `SessionUpdateConfig` newtype for safe partial session updates. `ContextMutationOutcome` enum — `Applied` (OpenAI: in-place `session.update`) or `RequiresResumption` (Gemini: session resumption with `SessionResumptionConfig`). `RealtimeRunner::update_session()` and `update_session_with_bridge()` orchestrate the provider-appropriate path. Includes `SESSION_MANAGEMENT.md` architecture documentation.
- **`RealtimeRunner` session management** (`adk-realtime`, #105/#232): `update_session()`, `next_event()`, and `send_tool_response()` methods for dynamic FSM IVR state transitions. `SessionUpdateConfig` uses the Newtype pattern wrapping `RealtimeConfig` with `Deref`/`DerefMut` for ergonomic field access.
- **Gemini session resumption** (`adk-realtime`, #232): `SessionResumptionConfig` with handle-based reconnection. `GeminiLiveSession` enables session resumption in setup, receives `SessionResumptionUpdate` messages, and reconnects with the handle for context changes.
- **Two realtime examples** (`adk-realtime`, #232): `openai_session_update` (mid-session persona switch with tool swap) and `gemini_context_mutation` (session resumption for context changes).
- **Zero-allocation LiveKit audio output** (`adk-realtime`, #236): Replaced manual `Vec::push` loops with `bytemuck::try_cast_slice` for O(0) copy. `Cow::Borrowed` passes aligned slices directly to WebRTC FFI. Vectorized iterator fallback for unaligned WebSocket chunks. Safety guards skip invalid audio frames. Includes `livekit_pcm_bench` benchmark.

#### devenv & CI ([@mikefaille](https://github.com/mikefaille))

- **devenv v2.0.6 upgrade** (#230): Updated `setup.sh` with v2 experimental features. Added `dbus` and `pkgs.dbus.dev` dependencies for `keyring` crate (adk-cli secure credential storage). Conditional `~/.bashrc` modification (CI-only) to avoid duplicate entries for local devs. Fixed `adk-rag` missing `gemini` feature in examples config.

#### adk-anthropic — Dedicated Anthropic API Client (NEW CRATE)
- **Standalone crate** replacing the `claudius` dependency in `adk-model`. Follows the same pattern as `adk-gemini` — a dedicated, publishable client crate.
- **Full Anthropic API parity** (March 2026): Messages, Batches, Files, Skills, Models, Token Counting APIs.
- **Current model support**: Claude Opus 4.6, Sonnet 4.6, Haiku 4.5, plus legacy 4.5/4.0/4.1 models. `KnownModel` enum with `Model::Custom(String)` fallback.
- **Adaptive thinking**: `ThinkingConfig::adaptive()` for 4.6 models. Effort controlled via `OutputConfig::with_effort()` (supports `Low`, `Medium`, `High`, `Max`).
- **Budget-based thinking**: `ThinkingConfig::enabled(budget_tokens)` for older models (deprecated on 4.6).
- **Structured outputs**: `OutputConfig` with `OutputFormat::Json` and `OutputFormat::JsonSchema`.
- **Prompt caching**: Top-level `cache_control: CacheControlEphemeral` for automatic caching, plus block-level `cache_control` on system prompts, tools, and content blocks.
- **Context management** (beta): `ContextManagement` with `ClearToolUses` and `ClearThinking` strategies. Auto-injects `context-management-2025-06-27` beta header.
- **Fast mode** (beta): `SpeedMode::Fast` for Opus 4.6. Auto-injects `fast-mode-2026-02-01` beta header.
- **Citations**: `CitationsConfig` on documents with `TextCitation` variants (char location, page location, content block location, web search result).
- **Vision**: URL and base64 image analysis via `ImageBlock`.
- **PDF processing**: URL, base64, and Files API PDF analysis via `DocumentBlock`.
- **SSE streaming**: Full event set including `ToolInputStart`, `ToolInputDelta`, `CompactionEvent`, `StreamError`.
- **Token counting**: `count_tokens()` method for pre-send estimation.
- **Token pricing**: `pricing` module with `ModelPricing` constants for all current models and `estimate_cost()` / `estimate_cost_1h()` calculators.
- **Stop reasons**: `StopReason` enum with `EndTurn`, `MaxTokens`, `StopSequence`, `ToolUse`, `PauseTurn`, `Refusal`, `PauseRun`, `ModelContextWindowExceeded`.
- **Container support**: `container` field on `MessageCreateParams` and `ContainerInfo` on `Message` response.
- **Service tier**: `service_tier` field for priority capacity.
- **14 examples**: `basic`, `streaming`, `thinking`, `tools`, `structured_output`, `caching`, `context_editing`, `compaction`, `token_counting`, `stop_reasons`, `fast_mode`, `citations`, `pdf_processing`, `vision`.
- **373 unit tests** covering all types, serialization round-trips, client logic, and SSE parsing.

#### adk-model — Anthropic Migration
- Replaced `claudius` dependency with `adk-anthropic` in `adk-model`. Import paths changed from `use claudius::` to `use adk_anthropic::`.
- Renamed `convert_claudius_error` to `convert_anthropic_error` across all Anthropic adapter modules.
- All 72 adk-model lib tests pass with the new dependency.

#### MCP Elicitation Support (adk-tool)
- **`ElicitationHandler` trait**: User-implementable trait for handling MCP elicitation requests from servers. Supports form-based elicitation (structured schemas) and URL-based elicitation. Requires `Send + Sync` for async safety.
- **`AutoDeclineElicitationHandler`**: Built-in zero-size handler that declines all elicitation requests, preserving backward-compatible behavior identical to rmcp's `()` ClientHandler default.
- **`AdkClientHandler`**: Bridge struct implementing rmcp's `ClientHandler` trait, advertising elicitation capabilities and delegating requests to the user's `ElicitationHandler`. Catches panics and errors gracefully, falling back to Decline.
- **`McpToolset::with_elicitation_handler()`**: Async factory method that creates an MCP client connection with elicitation support from any transport and an `Arc<dyn ElicitationHandler>`.
- **`McpToolset::with_client_handler()`**: Factory method for using a custom `ClientHandler` type with `McpToolset`.
- **`McpHttpClientBuilder::with_elicitation_handler()` / `connect_with_elicitation()`**: Builder methods for HTTP-based MCP connections with elicitation support.
- **Capability advertisement**: `AdkClientHandler` advertises form and URL elicitation capabilities to MCP servers during initialization.
- **Elicitation example**: `examples/mcp_elicitation/` — standalone crate with a real MCP server using `peer.elicit::<T>()` and an LLM-powered agent client with interactive stdin-based `ElicitationHandler`.
- Full backward compatibility: `McpToolset::new()` with `()` handler continues to work unchanged.

#### Gemini built-in tool tracing example (examples)
- **`gemini_search_bug`**: Standalone example reproducing GitHub Issue #224 — demonstrates Google Search + URL Context + function tool coexistence through the ADK runner with full `ServerToolCall`/`ServerToolResponse` tracing, thought signature propagation, and grounding metadata display. Uses `gemini-3-pro-preview` with `include_server_side_tool_invocations` to surface the complete tool call chain.

#### Action Node Graph Standardization (adk-action, adk-graph, adk-rust)
- **`adk-action` crate**: New shared crate containing all 14 action node type definitions, `StandardProperties`, `ActionError` enum, and variable interpolation utilities. Zero runtime dependencies beyond `serde`, `serde_json`, `thiserror`, and `regex`.
- **`ActionNodeExecutor`** in `adk-graph`: Implements the `Node` trait for any `ActionNodeConfig`, applying error handling (stop/continue/retry/fallback), timeout enforcement, and skip conditions uniformly across all node types.
- **14 action node executors**: Set, Transform, Switch, Loop, Merge, Wait, File, Code (Rust), Manual Trigger, HTTP, Code (JS/TS), Database (SQL/MongoDB/Redis), Email (IMAP/SMTP), Notification (Slack/Discord/Teams/webhook), RSS/Feed.
- **`TriggerRuntime`**: Background infrastructure for webhook routes (Axum), cron scheduling (`tokio-cron-scheduler`), and event subscriptions (`tokio::sync::mpsc`).
- **`WorkflowSchema`**: Serializable interchange format for graph workflows with `from_json()` and `build_graph()` methods, enabling adk-studio projects to be loaded and executed by adk-graph.
- **`GraphAgentBuilder` extensions**: `action_node()` and `from_workflow_schema()` methods for convenient action node integration.
- **Feature flags**: `action` (core nodes, no extra deps), `action-trigger`, `action-http`, `action-db`, `action-db-mongo`, `action-db-redis`, `action-code`, `action-email`, `action-rss`, `action-full`. Forwarded through `adk-rust` umbrella crate.
- **10 correctness properties**: Property-based tests across both crates covering round-trip serialization, error mode retry counts, switch condition determinism, interpolation idempotence, backward compatibility, and notification payload formats.

### Fixed

#### adk-gemini
- Fixed Gemini 3 built-in tools (Google Search, URL Context) causing truncated responses (#224). `ContentBuilder::build()` now auto-sets `includeServerSideToolInvocations: true` when server-side tools are present, enabling Gemini 3 to return `toolCall`/`toolResponse` parts on AI Studio instead of silently truncating.
- Fixed Vertex AI 400 error when `includeServerSideToolInvocations` was sent. Vertex AI rejects this field — it handles built-in tools natively. Both the Vertex backend and the Studio backend (when `with_base_url` points at `aiplatform.googleapis.com`) now strip the field before sending.

#### adk-model
- Fixed `test_server_tool_response_round_trip_as_openai_items` test — JSON fixture had `outcome` fields flattened instead of nested, causing deserialization mismatch with `async-openai` 0.33 structs.
- Fixed Anthropic system prompt tests (`test_heuristic_skipped_when_explicit_system_exists`, `test_instruction_rerouting_to_system`, `test_multiple_system_entries_concatenated`) that expected `SystemPrompt::String` but received `SystemPrompt::Blocks` after `prompt_caching` default changed to `true`.
- Fixed `prop_default_config_backward_compatible` property test asserting `prompt_caching` should be `false` — updated to match the actual default of `true`.
- Removed unused `OutputStatus` import in `responses_convert.rs`.
- Replaced `drain(..).collect()` with `std::mem::take()` in Anthropic streaming client per clippy `drain_collect` lint.

### Changed

#### Dependency upgrade (adk-gemini)
- **google-cloud-aiplatform-v1 1.8.0 → 1.9.0**: Migrated `EmbedContentRequest` from deprecated top-level `title`, `task_type`, and `output_dimensionality` fields to the new `EmbedContentConfig` struct. Eliminates 3 deprecation warnings on every build.

#### Provider-native built-in tool support (adk-tool, adk-model, adk-gemini, examples)
- Added typed built-in tool wrappers for Gemini (`GoogleMapsTool`, `GeminiCodeExecutionTool`, `GeminiFileSearchTool`, `GeminiComputerUseTool`), OpenAI Responses (`OpenAIWebSearchTool`, `OpenAIFileSearchTool`, `OpenAICodeInterpreterTool`, `OpenAIImageGenerationTool`, `OpenAIComputerUseTool`, `OpenAIMcpTool`, `OpenAILocalShellTool`, `OpenAIShellTool`, `OpenAIApplyPatchTool`), and Anthropic (`WebSearchTool`, native bash, native text editor variants).
- Added a provider-native declaration path to the shared `Tool` API so agents can mix built-in tools with ordinary `FunctionTool`s without relying on opaque `GenerateContentConfig.extensions` blobs.
- Expanded Gemini wire models to understand additional native tool declarations and code-execution parts, and updated the OpenAI/Anthropic examples to use the new typed wrappers directly.

### Changed

#### Built-in tool adapters
- **adk-model (OpenAI Responses)**: Native OpenAI tool declarations now deserialize from tool metadata instead of only `extensions["openai"]["built_in_tools"]`. Server-side tool outputs are preserved as typed Responses `Item` payloads so they survive streaming finalization and stateless round-trips.
- **adk-model (Anthropic)**: Native Anthropic tool declarations now deserialize from tool metadata, streamed `server_tool_use` / `web_search_tool_result` blocks are preserved in final streamed responses, and string tool results are no longer double-JSON-encoded.
- **adk-model (Gemini)**: Gemini native tools are now metadata-driven instead of name-driven, mixed built-in/function tool detection works for the broader Gemini tool surface, and native tool config such as `retrievalConfig` is forwarded correctly.

#### AP2 alpha adapter (adk-payments)
- Added typed AP2 alpha mandate, payment request, payment response, and payment receipt models plus an `Ap2Adapter` that routes human-present and human-not-present flows through the shared checkout, payment, intervention, journal, and evidence services.
- Added `ap2-a2a` AgentCard and A2A container helpers, `ap2-mcp` safe MCP-facing mandate and receipt views, AP2 fixture coverage, and end-to-end AP2 integration tests.

#### Agentic commerce validation and docs (adk-payments)
- Added a shared multi-actor integration harness for shopper, merchant, credentials-provider, payment-processor, and webhook actors, and rewired ACP/AP2 end-to-end tests to use the shared journal, memory, and evidence plumbing.
- Added payments documentation updates in `adk-payments/README.md`, `docs/official_docs/security/payments.md`, and `examples/payments/README.md`, plus a local `examples/payments` scenario index for the supported commerce journeys.

#### OpenAI Responses API client (adk-model)
- **`OpenAIResponsesClient`**: Dedicated client for OpenAI's `/v1/responses` endpoint — the successor to Chat Completions. Implements `adk_core::Llm` with full streaming, tool calling, and multi-turn support.
- **`OpenAIResponsesConfig`**: Configuration type with `with_reasoning_effort()`, `with_reasoning_summary()`, `with_organization()`, `with_project()`, `with_base_url()`.
- **`ReasoningSummary`** enum (`Auto`, `Concise`, `Detailed`): Controls reasoning summary generation for o-series models. Summaries appear as `Part::Thinking` in the response stream.
- **Streaming deduplication**: `ResponseCompleted` events extract only function calls and usage metadata — text/thinking content already streamed via delta events is not re-emitted.
- **Provider metadata**: Every response includes `provider_metadata.openai.response_id` for server-side state and debugging.
- **Documentation**: Full docs page at `docs/official_docs/models/openai-responses.md`, updated `providers.md`, `adk-model/README.md`, and root `README.md`.
- **Example**: `examples/openai_responses/` — standalone crate with 7 scenarios (basic, streaming, reasoning, tools, multi-turn, system instructions, generation config).

#### OpenRouter deep integration (adk-model, adk-rust, examples)
- **`openrouter` feature on `adk-rust`**: The umbrella crate now re-exports `OpenRouterClient`, `OpenRouterConfig`, `OpenRouterApiMode`, `OpenRouterRequestOptions`, and related types behind a dedicated `openrouter` feature.
- **Native OpenRouter examples**: Added `adk-model/examples/openrouter_chat.rs`, `openrouter_responses.rs`, `openrouter_adapter.rs`, and `openrouter_discovery.rs` plus shared support modules for live provider validation.
- **Agentic OpenRouter validation crate**: Added `examples/openrouter` as a standalone example crate mirroring the `examples/openai_responses` style and covering chat, streaming, tools, responses mode, multimodal input, routing, discovery, and sessioned runner flows.
- **Ignored live contracts**: Added `adk-model/tests/openrouter_contract_tests.rs` and wired OpenRouter into the shared provider contract harness for ignored live validation.

#### Config validation (adk-gemini)
- **`ThinkingConfig::validate()`**: Pre-send validation that rejects mutually exclusive `thinking_budget` + `thinking_level` combinations before the request reaches the Gemini API.
- **`GenerationConfig::validate()`**: Pre-send validation for `temperature` (0.0–2.0), `top_p` (0.0–1.0), `top_k` (> 0), `max_output_tokens` (> 0), and delegates to `ThinkingConfig::validate()` when present. Validation is wired at the request boundary — invalid configs return `AdkError` instead of sending malformed requests.

#### Audio codec capability queries (adk-audio)
- **`AudioFormat::supports_encode()`**: Returns `true` for formats with working `encode()` implementations (`Pcm16`, `Wav`), `false` for all others. Uses exhaustive `match` so new variants force a decision.
- **`AudioFormat::supports_decode()`**: Returns `true` for formats with working `decode()` implementations (`Pcm16`, `Wav`), `false` for all others.

#### Feature presets (adk-rust)
- **`labs` feature preset**: New preset for experimental crates (`code`, `sandbox`, `audio`). Use `features = ["labs"]` to opt in to experimental functionality.

### Changed

#### OpenRouter production hardening
- **Streaming finalization**: The OpenRouter chat adapter now emits exactly one final `LlmResponse` chunk even when OpenRouter streams `finish_reason` and usage metadata in separate SSE frames.
- **Tool-call mapping**: Chat-mode tool responses now round-trip as `role="tool"` messages with `tool_call_id`, and streamed tool-call deltas tolerate missing `role`, `type`, and function `name` fields that appear in real OpenRouter streams.
- **Documentation**: Updated `adk-model` crate docs and README to document native OpenRouter APIs, the generic `Llm` adapter boundary, and the local example entry points.

#### Feature presets (adk-rust)
- **`full` feature preset no longer includes experimental crates**: `full` now compiles only stable specialist crates (`graph`, `realtime`, `browser`, `eval`, `rag`). Experimental crates (`code`, `sandbox`, `audio`) moved to the new `labs` preset. Use `features = ["full", "labs"]` to get everything.

#### Debug endpoint honesty (adk-server)
- **`get_graph` returns 501 Not Implemented**: Previously returned HTTP 200 with a hardcoded fake DOT graph string. Now returns HTTP 501 with `{ "error": "graph generation is not yet implemented" }`.
- **`get_eval_sets` returns 501 Not Implemented**: Previously returned HTTP 200 with an empty array stub. Now returns HTTP 501 with `{ "error": "eval sets are not yet implemented" }`.
- **`get_event` returns 404 when event not found**: Previously returned HTTP 200 with a stub JSON body containing an empty `invocationId`. Now returns HTTP 404 Not Found. Existing successful path (event found in span exporter) is unchanged.

#### Production hardening
- **adk-core**: Added validated `new()` constructors for `AppName`, `UserId`, `SessionId`, and `InvocationId` so trust-boundary code can use an explicit safe constructor instead of relying on `TryFrom`.
- **adk-runner**: `Runner::run()` now accepts typed `UserId` and `SessionId` parameters. Migration: `runner.run("user".to_string(), "session".to_string(), content)` becomes `runner.run(UserId::new("user")?, SessionId::new("session")?, content)`.
- **adk-runner**: Added `MutableSession::events_len()` and updated compaction checks to avoid cloning the full event list for count-only access.
- **adk-audio**: AssemblyAI, Deepgram, and MLX `transcribe_stream()` stubs now return explicit `AudioError::Stt` errors instead of silently succeeding with empty streams.
- **adk-audio**: MLX STT placeholder errors now clearly state that local Whisper inference is not yet implemented and recommend using a cloud STT provider.

#### Structured Error Envelope (Breaking)
- **adk-core**: Replaced flat `AdkError` enum with a multi-axis struct separating component (where), category (what kind), code (machine key), message (human text), retry hint, and error details. This is a deliberate breaking change targeting pre-1.0.
- **adk-core**: Added `ErrorComponent` (14 variants) and `ErrorCategory` (10 variants) enums for structured error classification.
- **adk-core**: Added `RetryHint` with `should_retry`, `retry_after_ms`, and `max_attempts` fields for structured retry guidance.
- **adk-core**: Added `http_status_code()` and `to_problem_json()` methods on `AdkError` for HTTP error response generation.
- **adk-core**: Backward-compatible constructors (`agent()`, `model()`, `tool()`, `session()`, etc.) preserved with `.legacy` code suffix.
- **adk-model**: All providers (Gemini, OpenAI, Anthropic, DeepSeek, Groq, Azure AI, Ollama) now emit structured errors with proper `ErrorCategory` based on HTTP status codes (429→RateLimited, 503→Unavailable, etc.).
- **adk-model**: `is_retryable_model_error()` now checks `error.retry.should_retry` as single source of truth, with fallback to message parsing for legacy errors.
- **adk-model**: `execute_with_retry_hint()` extracts `retry_after` from structured `AdkError` fields.
- **adk-server**: Runtime controller uses `AdkError::http_status_code()` and `to_problem_json()` for error responses instead of hardcoded 500s.
- **All crates**: Migrated from `AdkError::Variant("msg".into())` to `AdkError::variant("msg")` method syntax.
- **Boundary crates**: Added `From<CrateLocalError> for AdkError` impls in adk-realtime, adk-graph, adk-guardrail, adk-auth, adk-code, adk-skill, adk-sandbox, adk-eval, adk-rag.

### Changed (from 0.4.1)

#### Examples
- **Moved to adk-playground**: All examples removed from this workspace and consolidated in the [adk-playground](https://github.com/zavora-ai/adk-playground) repo (120+ examples). The `examples/` directory now contains only a README pointing there.

#### Error Handling Hardening
- **adk-runner**: Replaced all `RwLock::unwrap()` calls in `MutableSession` with graceful error handling. Poisoned locks now log via `tracing::error` and return safe defaults (empty `Vec`, empty `HashMap`, `None`) instead of panicking. Affects `apply_state_delta`, `append_event`, `events_snapshot`, `conversation_history`, and `State` trait methods.
- **adk-telemetry**: Replaced `expect()` calls in `init_with_otlp()` with proper error propagation — OTLP exporter build failures now return `TelemetryError::Init` instead of panicking. Replaced `expect()` with `unwrap_or_else` fallback for `EnvFilter` in all init functions. Replaced `expect()` with `let-else` early return in span `Layer` callbacks. Replaced `unwrap()` with `unwrap_or_else(into_inner)` for `RwLock` in `AdkSpanExporter`.
- **adk-code**: `DockerExecutor::new()` now returns `Result<Self, ExecutionError>` instead of panicking when the Docker daemon is unreachable.
- **adk-agent**: Replaced `Arc::get_mut().expect()` with `if let Some` in builder methods for `LoopAgent`, `ConditionalAgent`, and `ParallelAgent`.

#### Dependency Cleanup
- **adk-agent**: Removed unused `adk-model` direct dependency and `gemini` feature forwarding. `adk-agent` source code had zero imports from `adk_model`; the dependency only existed to forward the `gemini` feature flag. No crate in the workspace referenced `adk-agent/gemini`. `adk-model` remains as a dev-dependency for tests.
- **adk-guardrail**: Set `jsonschema = { version = "0.43", optional = true, default-features = false }` to eliminate `reqwest 0.13` from the dependency tree. ADK does not use remote JSON Schema `$ref` resolution, so the network features are unnecessary.
- **adk-model (anthropic)**: Upgraded `claudius` from 0.16 to 0.19, eliminating `reqwest 0.11` from the dependency tree. The claudius 0.19 API takes `&params` instead of `params` in `.stream()`. Note: claudius 0.19 uses `reqwest 0.13` internally, so there is still a reqwest version duplicate with the workspace's `reqwest 0.12`, but the older `reqwest 0.11` is gone.
- **adk-telemetry**: Upgraded OpenTelemetry stack from 0.21 to 0.28 (`opentelemetry 0.28`, `opentelemetry_sdk 0.28`, `opentelemetry-otlp 0.28`, `tracing-opentelemetry 0.29`). This eliminates duplicate `axum`, `hyper`, `http`, `h2`, and `tower` crates — the old OTel stack pulled `tonic 0.9` → `axum 0.6` → `hyper 0.14` → `http 0.2`, while `adk-server` uses `axum 0.8` → `hyper 1.x` → `http 1.x`. Updated `init_with_otlp()` to use new 0.28 builder APIs (`SdkTracerProvider`, `SpanExporter::builder`, `MetricExporter::builder`, `SdkMeterProvider`). Updated `shutdown_telemetry()` to replace the global provider with a no-op (the `shutdown_tracer_provider()` global function was removed in OTel 0.28).

#### Examples
- **telemetry_demo**: Updated to use OTel 0.28 APIs — `.build()` instead of `.init()` for metrics instruments. Replaced mock/simulated LLM calls with real Gemini API calls. The demo now requires `GOOGLE_API_KEY` and demonstrates actual token usage recording via `with_usage_tracking` for both non-streaming and streaming responses.

### Fixed

#### adk-gemini
- **Gemini 3.x thought_signature serialization**: Changed `#[serde(skip_serializing)]` to `#[serde(skip_serializing_if = "Option::is_none")]` on `thought_signature` fields in `Part::Text`, `Part::FunctionCall`, and the tools `FunctionCall` struct. Gemini 3.x models require `thought_signature` to be echoed back in multi-turn function calling; the previous behavior silently dropped it, causing 400 errors on the second LLM call after tool execution. Backward compatible — field is omitted when `None`.

#### adk-tool
- **AgentTool infinite loop on empty sub-agent responses**: `AgentToolInvocationContext::run_config()` now returns `StreamingMode::None` instead of `StreamingMode::SSE`. In SSE mode, the sub-agent's final event often contained empty text (actual content was spread across earlier partial chunks), causing the coordinator to re-call the same tool indefinitely. Non-streaming mode accumulates the full response before yielding a single complete event. Additionally, `extract_response` now skips empty text parts and falls back to collecting text from all events.

#### adk-session
- **MongoDB standalone deployment support**: `MongoSessionService` now auto-detects whether the connected MongoDB instance supports multi-document transactions (replica set / sharded cluster) or is running standalone. On standalone deployments, all write operations execute sequentially without transactions instead of failing with `IllegalOperation: Transaction numbers are only allowed on a replica set member or mongos`. Detection uses the `hello` command at connection time to check for `setName` in the response. New `supports_transactions()` method exposes the detected mode. The `retryWrites=false` connection string workaround is no longer required.
- **PostgreSQL migration INT4/INT8 type mismatch**: Fixed `COALESCE(MAX(version), 0)` in the migration registry query to use `CAST(... AS BIGINT)`. PostgreSQL creates the `version` column as `INTEGER` (INT4) but the Rust code reads it as `i64` (INT8), causing a type mismatch error on migration. The cast ensures the return type matches the expected Rust type.
- **PostgreSQL migration registry DDL**: Parameterized the migration runner macro to use `BIGINT PRIMARY KEY` for PostgreSQL and `INTEGER PRIMARY KEY` for SQLite, matching the Rust `i64` type natively. Removed the `CAST(... AS BIGINT)` workaround from SELECT queries since the column type is now correct. Applied to both `adk-session` and `adk-memory` migration runners.
- **examples**: Added `required-features = ["rag-gemini"]` to the `rag_gemini` example entry, fixing `cargo test --workspace` compilation failure when the optional `adk-rag` dependency is not enabled.


## [0.4.0] - 2026-03-16

### Added

#### `cargo-adk` Scaffolding CLI
- **Project scaffolding**: New `cargo-adk` binary for generating agent projects from templates. `cargo adk new my-agent` scaffolds a working project with the right dependencies, feature flags, and boilerplate. Templates: `basic`, `tools` (#[tool] macro), `rag` (vector search), `api` (REST server), `openai`. Supports `--provider` flag for OpenAI/Anthropic/Gemini.

#### `#[tool]` Proc Macro (adk-rust-macros)
- **Zero-boilerplate tool registration**: New `#[tool]` attribute macro turns an async function into a full `Tool` implementation. Doc comments become the description, argument types derive JSON schemas via schemars, and a PascalCase struct is generated implementing `adk_core::Tool`. Supports both standalone functions and functions with `Arc<dyn ToolContext>` parameter. Schema output is automatically cleaned for LLM API compatibility (strips `$schema`, simplifies nullable types).

#### Development Infrastructure
- **cargo-nextest integration**: Switched from `cargo test` to `cargo nextest run` for workspace test execution. Parallel test binary execution reduces test wall-clock time from ~1m47s to ~9s (~11x speedup). Added `.config/nextest.toml` with default and CI profiles (CI profile includes retry-on-flaky and slow-test warnings). `devenv.nix` updated with `ws-test` (nextest), `ws-test-ci` (nextest CI profile), and `ws-test-slow` (fallback `cargo test` for doctests) scripts.

#### Vision / Multimodal Support (adk-model)
- **Bedrock**: `InlineData` with image MIME types (jpeg/png/gif/webp) now maps to `ContentBlock::Image`; document MIME types (pdf/csv/html/md/txt/doc/docx) map to `ContentBlock::Document`. Response-side `ContentBlock::Image` converts back to `Part::InlineData`. `FileData` with image/document URLs becomes a text reference (Bedrock only supports S3 URIs natively).
- **OpenAI**: `FileData` with `image/*` MIME types now maps to `ImageUrl` content part instead of falling back to text, enabling direct image URL vision.
- **Anthropic**: `FileData` with image MIME types (jpeg/png/gif/webp) now maps to `ImageBlock` with `UrlImageSource` instead of text fallback, enabling direct image URL vision.

#### OpenAI Reasoning Model Support (adk-model)
- **Reasoning content extraction**: OpenAI-compatible client now uses direct reqwest calls instead of async-openai's HTTP client, enabling extraction of `reasoning_content` from reasoning models (o3, o4-mini, gpt-5-mini) that async-openai 0.33 silently drops. Reasoning content maps to `Part::Thinking`.
- **Empty text filtering**: `from_openai_response` and new `from_raw_openai_response` now filter empty text parts produced by reasoning models when all tokens go to internal chain-of-thought.

### Changed

#### adk-rust (umbrella crate)
- **Tiered feature presets**: Default changed from `full` to `standard`. Three presets: `minimal` (agents + Gemini + runner, ~30s build), `standard` (+ tools, sessions, memory, telemetry, guardrail, auth, plugin, ~51s build), `full` (+ server, CLI, graph, browser, eval, realtime, RAG, audio, ~2min build). Users who need server/CLI/specialist crates add `features = ["full"]`.
- **Minimal tokio features**: `adk-rust` umbrella crate now declares explicit tokio features (`rt`, `rt-multi-thread`, `sync`, `time`, `macros`, `net`, `signal`, `fs`, `process`, `io-util`) instead of `"full"`. Binary crates (`adk-cli`, examples) retain `"full"`. This follows the Rust convention that library crates should never use `tokio = { features = ["full"] }`.

#### adk-core
- **AdkError documentation**: All 9 error variants now have doc comments describing their use (Agent, Model, Tool, Session, Artifact, Memory, Config, Io, Serde).

#### Examples
- **openai_basic**: Default model changed from `gpt-5-mini` to `gpt-4o-mini`, `max_output_tokens` increased from 64 to 256 (reasoning models need headroom). Supports `OPENAI_MODEL` env var override.
- **vision_test**: OpenAI model changed from `gpt-5-mini` to `gpt-4o-mini`.
- **Cleanup**: Removed 20 non-essential `openai_*` example directories (full collection in adk-playground repo).

#### adk-model
- **Consolidated OpenAI-compatible providers**: Replaced 7 near-identical provider modules (fireworks, together, mistral, perplexity, cerebras, sambanova, xai) with `OpenAICompatibleConfig` presets. Each was ~150 lines wrapping the same `OpenAICompatible` client — now 7 preset constructors totaling 63 lines. Usage: `OpenAICompatible::new(OpenAICompatibleConfig::fireworks(key, model))`. Feature flags preserved as backward-compatible aliases (`fireworks = ["openai"]`). `all-providers` simplified from 15 to 8 flags.

#### adk-telemetry
- **Standardized LLM token usage telemetry**: New `llm_generate_span(provider, model, stream)` creates spans with pre-declared `gen_ai.usage.*` fields following OpenTelemetry GenAI semantic conventions. New `LlmUsage` struct and `record_llm_usage(&usage)` record token counts (input, output, total, cache read/creation, thinking, audio input/output) on the current span. All 8 fields are optional-aware — only non-None values are recorded.
- **Proper error type**: Replaced `Box<dyn std::error::Error>` with `TelemetryError` (thiserror) in all init functions. Convention-compliant typed errors.

#### adk-model
- **Unified token usage tracking across all providers**: New `usage_tracking::with_usage_tracking(stream, span)` wraps any `LlmResponseStream` to automatically record `gen_ai.usage.*` fields on the tracing span. Applied to all 10 providers: Gemini, OpenAI, OpenAI-compatible (Fireworks, Together, Mistral, Perplexity, Cerebras, SambaNova, xAI), Anthropic, Ollama, Bedrock, DeepSeek, Groq, Azure AI, Azure OpenAI. Previously only Anthropic recorded token counts; now all providers emit standardized telemetry including cache, thinking, and audio token counts.

#### adk-plugin
- **Removed unused dependencies**: `async-trait` and `serde` removed from Cargo.toml (never imported).

#### adk-memory
- **Shared text utilities**: Extracted `extract_text()`, `extract_words()`, and `extract_words_from_content()` into `adk_memory::text` module. Removed duplicate implementations from 5 backends (postgres, sqlite, mongodb, neo4j, redis) and inmemory.

#### Documentation (Tier 2 crates)
- **adk-artifact**: Documented all request/response structs, `ArtifactService` trait methods, `InMemoryArtifactService`.
- **adk-guardrail**: Documented `GuardrailError` variants, `GuardrailSet` methods, `ExecutionResult` fields.
- **adk-skill**: Documented 8 public functions (`select_skills`, `apply_skill_injection`, `discover_skill_files`, `parse_skill_markdown`, `load_skill_index`, etc.).
- **adk-gemini**: Removed `println!` debug statements from tests.
- **README versions**: Bumped 0.3→0.4 in adk-telemetry, adk-memory, adk-artifact, adk-plugin, adk-guardrail, adk-gemini.

### Removed
- **adk-doc-audit**: Removed from workspace (docs.rs provides this functionality). Backed up to standalone directory.

#### adk-mistralrs
- **Minimal tokio features**: Changed from `tokio = { features = ["full"] }` to `tokio = { features = ["rt", "sync", "macros"] }` — the minimal set actually used by the crate.

#### CI
- **nextest in CI**: GitHub Actions workflow now uses `ws-test-ci` (cargo-nextest with CI profile) instead of `cargo test --workspace`. Summary parser updated to handle nextest output format with fallback for `cargo test` format.

#### adk-model (OpenAI / OpenAI-compatible providers)
- **async-openai 0.33**: Upgraded from 0.27 to 0.33. Breaking API changes adapted: types moved to `types::chat::*`, `ChatCompletionToolType` removed, `FunctionObject.parameters` changed to `Option<serde_json::Value>`, `max_tokens` replaced with `max_completion_tokens`.
- **Non-streaming workaround**: OpenAI and Azure OpenAI providers temporarily use non-streaming `create()` instead of `create_stream()` due to a `reqwest-eventsource` compatibility bug in async-openai 0.33 that causes "Invalid header value" errors on SSE connections. Responses arrive as a single chunk. Streaming will be restored when the upstream bug is fixed.
- **reqwest default features restored**: Root workspace `reqwest` dependency no longer sets `default-features = false`, fixing transitive feature resolution issues.

### Added

#### adk-sandbox (NEW CRATE)
- New `adk-sandbox` crate: isolated code execution runtime for ADK agents
- `SandboxBackend` trait with `execute(ExecRequest) -> Result<ExecResult, SandboxError>` and `capabilities()` methods
- `ProcessBackend`: subprocess execution via `tokio::process::Command` with timeout enforcement, environment isolation (`env_clear()`), output truncation (1 MB, UTF-8 safe), and `kill_on_drop(true)`. Supports Rust, Python, JavaScript, TypeScript, and shell commands
- `WasmBackend`: in-process WASM execution via `wasmtime` with epoch-based timeout, memory limits via `StoreLimitsBuilder`, WASI stdin/stdout/stderr capture, and no filesystem or network access (behind `wasm` feature)
- `SandboxTool`: `adk_core::Tool` implementation delegating to any `SandboxBackend`, with error-as-information pattern (errors returned as structured JSON, never `ToolError`)
- `ExecRequest` and `ExecResult` types with explicit timeout (no `Default` impl), `Language` enum, and `SandboxError` enum
- `BackendCapabilities` with honest `EnforcedLimits` reporting what each backend actually enforces
- Feature flags: `process` (default), `wasm` (optional, requires `wasmtime`)

### Changed

#### Repository structure
- `adk-deploy-server` and `adk-deploy-console` have been hard-migrated out of the `adk-rust` workspace into the sibling `adk-platform` repo, while `adk-deploy` remains in `adk-rust` as the shared deployment manifest and bundling utility crate

#### adk-code
- Redesigned with `RustExecutor`: check → build → execute pipeline delegating to `SandboxBackend` from `adk-sandbox`
- New `CodeTool` implementing `adk_core::Tool` with structured diagnostic passthrough (compile errors as JSON, not `ToolError`)
- New `CodeError` enum with `CompileError` (structured `Vec<RustDiagnostic>`), `DependencyNotFound`, `Sandbox`, `InvalidCode` variants
- Extracted `harness.rs` (harness template, source validation) and `diagnostics.rs` (rustc JSON diagnostic parser) as shared modules
- `EmbeddedJsExecutor` capabilities fixed: now honestly reports `true` for network/filesystem/environment enforcement (isolation by omission via `boa_engine`)
- `DockerExecutor` Drop safety fixed: uses `Handle::try_current()` before spawning cleanup, logs warning when no runtime is available
- Migration compatibility layer in `compat` module with deprecated type aliases for one release cycle

### Deprecated

#### adk-tool
- `RustCodeTool` is deprecated in favor of `adk_code::CodeTool`

#### adk-code
- `CodeExecutor`, `ExecutionRequest`, `ExecutionResult`, `RustSandboxExecutor`, `RustSandboxConfig` type aliases deprecated (use `adk-sandbox` and new `adk-code` types instead). Will be removed in v0.6.0

## [0.4.0] - 2026-03-12

### Added

#### adk-code (NEW CRATE)
- New `adk-code` crate: first-class code execution substrate for ADK-Rust
- Core types: `CodeExecutor` trait, `ExecutionRequest`, `ExecutionResult`, `ExecutionLanguage`, `SandboxPolicy`, `BackendCapabilities`, `ExecutionIsolation`
- `CodeExecutor` lifecycle methods: `start()`, `stop()`, `restart()`, `is_running()` for persistent execution environments (default no-ops for simple backends)
- `RustSandboxExecutor`: flagship Rust-authored code execution with host-local process isolation and strict defaults (30s timeout, 1MB output limits)
- `EmbeddedJsExecutor`: secondary in-process JavaScript backend via `boa_engine` for lightweight transforms (behind `embedded-js` feature)
- `DockerExecutor`: persistent Docker container executor using `bollard` SDK (behind `docker` feature) — matches AutoGen's `DockerCommandLineCodeExecutor` lifecycle model (start once, execute many, stop when done)
- `DockerConfig` presets: `python()`, `node()`, `custom(image)` with builder methods `.pip_install()`, `.npm_install()`, `.with_network()`, `.env()`, `.bind_mount()`
- `ContainerCommandExecutor`: CLI-based ephemeral container executor for Python, JavaScript, and command execution
- `WasmGuestExecutor`: guest-module backend for precompiled `.wasm` modules with explicit boundary validation
- `Workspace` and `CollaborationEvent`: shared project context for multi-agent code generation with typed collaboration events (NeedWork, WorkClaimed, WorkPublished, FeedbackRequested, FeedbackProvided, Blocked, Completed)
- A2A-compatible collaboration event mapping for future remote specialist execution
- `ExecutionMetadata` and `ArtifactRef` for telemetry correlation and artifact storage references
- Fail-closed sandbox policy validation: backends reject unsupported controls before executing user code
- 10 correctness properties validated by proptest (100+ iterations each)

#### adk-tool
- `RustCodeTool`: primary Rust-first code execution tool with `code:execute` and `code:execute:rust` scopes
- `JavaScriptCodeTool`: secondary JavaScript execution tool — uses real `EmbeddedJsExecutor` when `code-embedded-js` feature is enabled, returns descriptive error otherwise
- `PythonCodeTool`: container-backed Python execution tool, supports custom executors via `with_executor()` (e.g., `DockerExecutor` for persistent containers)
- `FrontendCodeTool`: container-backed Node.js execution tool for React/frontend code, supports custom executors via `with_executor()`
- New feature flags: `code-embedded-js` (enables boa_engine JS backend), `code-docker` (enables Docker SDK persistent containers)
- Workspace-friendly presets: `RustCodeTool::backend()`, `FrontendCodeTool::react()` for collaborative project builds

#### adk-studio
- Rust-first code execution: Studio live runner executes authored Rust through `adk-code` `RustSandboxExecutor` instead of returning placeholder errors
- Generated Studio projects reuse the same authored Rust body for code nodes
- Rust is the primary code authoring mode; JavaScript/TypeScript available as secondary scripting
- Sandbox settings map to backend-enforceable capabilities with incompatibility surfacing

#### adk-deploy
- `adk-deploy` manifest coverage now includes telemetry, auth, guardrails, realtime, A2A, graph/HITL, plugins, skills, and richer service binding validation for self-hosted deployment workflows
- Bundle creation now rejects asset paths that escape the project root
- `adk-cli` deploy login now validates operator-provided bearer tokens against the external platform API and stores them in the OS credential store instead of plaintext config
- Deployment manifests can now publish operator interaction metadata for manual, webhook, schedule, and event triggers, and Studio carries trigger configuration into that manifest for external platform consumers

### Fixed

#### adk-gemini
- **Citation metadata deserialization**: `CitationMetadata` now deserializes correctly when Gemini returns `citationMetadata` without a `citationSources` field. Previously this caused a deserialization error for grounded responses using Google Search or URL context tools. ([#178](https://github.com/zavora-ai/adk-rust/issues/178))
- **Vertex AI global endpoint**: The Vertex endpoint builder now correctly produces `https://aiplatform.googleapis.com` when `location` is `"global"`, instead of the invalid `https://global-aiplatform.googleapis.com`. No custom base URL workaround is needed for Gemini 3 models on the global endpoint. ([#179](https://github.com/zavora-ai/adk-rust/issues/179))
- **Feature-gated Google Cloud dependencies**: `google-cloud-aiplatform-v1`, `google-cloud-auth`, and `google-cloud-gax` are now optional dependencies behind the `vertex` feature flag. Users who only need the Gemini Developer API (AI Studio) can compile with `--no-default-features --features studio` to avoid pulling in heavy Google Cloud crates. Default features include `vertex` for backward compatibility. ([#181](https://github.com/zavora-ai/adk-rust/issues/181))

### Added

#### adk-gemini
- **Gemini 3 thinking level**: `ThinkingLevel` enum (`Minimal`, `Low`, `Medium`, `High`) and `thinking_level` field on `ThinkingConfig` for native Gemini 3 level-based reasoning control. Builder method `with_thinking_level()` available on both `ThinkingConfig` and `ContentBuilder`. Existing Gemini 2.5 budget-based APIs (`with_thinking_budget`, `with_dynamic_thinking`) are unchanged. ([#177](https://github.com/zavora-ai/adk-rust/issues/177))

#### adk-model
- **OpenAI reasoning effort**: `ReasoningEffort` enum (`Low`, `Medium`, `High`) and `reasoning_effort` field on `OpenAIConfig` for OpenAI reasoning models (o1, o3, etc.). Builder method `with_reasoning_effort()` wires through to the `reasoning_effort` API field. Also available on `OpenAICompatibleConfig` for compatible providers. ([#177](https://github.com/zavora-ai/adk-rust/issues/177))

#### adk-core
- **Typed identity module**: New `adk_core::identity` module with `AppName`, `UserId`, `SessionId`, `InvocationId` newtypes, `AdkIdentity` (session-scoped triple), `ExecutionIdentity` (per-invocation capsule), and `IdentityError`. All leaf types implement `Clone`, `Debug`, `Eq`, `Hash`, `Ord`, `Display`, `AsRef<str>`, `Borrow<str>`, `FromStr`, `TryFrom<&str>`, `TryFrom<String>`, `Serialize`, `Deserialize` with `#[serde(transparent)]`. Validation rejects empty values, null bytes, and strings exceeding 512 bytes. `SessionId::generate()` and `InvocationId::generate()` produce UUID-based identifiers.
- **Typed context helpers on `ReadonlyContext`**: Additive default methods `try_app_name()`, `try_user_id()`, `try_session_id()`, `try_invocation_id()`, `try_identity()`, and `try_execution_identity()` parse existing string fields into typed identifiers, returning `IdentityError` on invalid values instead of panicking.
- **Typed session helpers on `Session`**: Additive default methods `try_app_name()`, `try_user_id()`, `try_session_id()`, and `try_identity()` on the `Session` trait.
- **`ToolOutcome` struct**: Structured metadata for tool execution results — carries tool name, arguments, success/failure, execution duration, optional error message, and retry attempt number. Available via `CallbackContext::tool_outcome()` in after-tool callbacks.
- **`tool_outcome()` default method on `CallbackContext`**: Returns `Option<ToolOutcome>`, defaulting to `None` for full backward compatibility with existing implementors.
- **`RetryBudget` struct**: Configurable retry policy with `max_retries` and `delay` for automatic tool retry on transient failures.
- **`OnToolErrorCallback` type**: Promoted to `adk-core` as the canonical, framework-level tool-error callback type. Previously defined locally in `adk-agent` and `adk-plugin`.
- **`AfterToolCallbackFull` type**: V2 rich after-tool callback aligned with Python/Go ADK model. Receives `(CallbackContext, Tool, args, response)` and can inspect or replace the tool response sent to the LLM.

#### adk-auth
- **Typed auth-boundary user validation**: `JwtRequestContextExtractor` now validates the mapped auth user against `UserId` before returning `RequestContext`. Invalid mapped user IDs now fail with `RequestContextError::ExtractionFailed` instead of slipping deeper into the runtime. `ClaimsMapper` remains responsible only for claim selection.

#### adk-agent
- **`.toolset()` builder method**: `LlmAgentBuilder` now accepts `Arc<dyn Toolset>` for dynamic per-invocation tool resolution. Toolsets are resolved at the start of each `run()` call using the current `ReadonlyContext`, enabling context-dependent tools (e.g., per-user browser sessions). Static `.tool()` and dynamic `.toolset()` can be mixed freely.
- **`.default_retry_budget()` and `.tool_retry_budget()`**: Configure automatic retry for transient tool failures. Per-tool budgets override the default. When retries are exhausted, the final failure is reported to the LLM.
- **`.circuit_breaker_threshold()`**: Tracks consecutive tool failures per tool name within an invocation. After the configured threshold, the tool is temporarily disabled with an immediate error response to the LLM. Resets at the start of each new invocation.
- **`.on_tool_error()` callback**: Register fallback handlers invoked when a tool fails (after retries are exhausted). Callbacks can return a substitute `Value` used as the function response, or `None` to pass through to the next handler. If no handler provides a fallback, the original error is reported to the LLM.
- **`ToolOutcome` in after-tool callbacks**: `CallbackContext::tool_outcome()` returns structured execution metadata (success, duration, error, attempt number) without requiring JSON error parsing.
- **`.after_tool_callback_full()` builder method**: V2 rich after-tool callback that receives the tool, arguments, and response. Runs after the legacy `AfterToolCallback` chain. Aligned with Python/Go ADK model for first-class tool result handling.

#### adk-realtime
- **`.toolset()` builder method on `RealtimeAgentBuilder`**: Dynamic per-invocation tool resolution for realtime voice agents, matching `LlmAgentBuilder` parity. Toolsets are resolved before the realtime session connects, with the same duplicate detection (static-vs-toolset, toolset-vs-toolset) as `LlmAgent`. Static `.tool()` and dynamic `.toolset()` can be mixed freely. Fully backward compatible.

#### adk-tool
- **Toolset composition utilities**: Three reusable toolset wrappers for complex agent configurations:
  - `FilteredToolset` — wraps any toolset and filters tools by predicate (allow-list via `string_predicate()` or custom `ToolPredicate`)
  - `MergedToolset` — combines multiple toolsets into one with first-wins deduplication and `tracing::warn` on name conflicts
  - `PrefixedToolset` — namespaces all tool names with a configurable prefix to avoid collisions across toolsets
  All three implement `Toolset` and compose with any toolset implementation including `McpToolset` and `BrowserToolset`.

#### adk-browser
- **Pool-backed `BrowserToolset`**: `BrowserToolset::with_pool()` and `BrowserToolset::with_pool_and_profile()` constructors resolve per-user browser sessions from `BrowserSessionPool` using the invocation's `user_id`. This is the production path for multi-tenant browser agents. Existing `new()` and `with_profile()` constructors are unchanged.
- **`try_all_tools()`**: Explicit error handling for pool-backed toolsets where `all_tools()` cannot resolve without context.
- **`ensure_started()` auto-recovery**: All public `BrowserSession` methods that access the WebDriver now go through a centralized lifecycle-safe path that auto-starts or reconnects stale sessions. Tools no longer fail with "Browser session not started" errors. Explicit `start()` and `stop()` remain for manual lifecycle control.
- **Navigation tool page context**: `NavigateTool`, `BackTool`, `ForwardTool`, and `RefreshTool` now include a `"page"` field in responses with the current page context (URL, title, truncated text), matching the format used by interaction tools. If page context capture fails, a `"page_context_error"` field is included instead.

#### Examples
- **`browser_pool`**: Multi-tenant pool-backed `BrowserToolset` with per-user session isolation, `.toolset()` API, and `ensure_started()` auto-recovery. Requires `--features browser`.
- **`resilient_agent`**: Retry budgets, circuit breakers, `on_tool_error` fallback callbacks, and `ToolOutcome` metadata in after-tool callbacks. Uses mock flaky/broken/reliable tools.
- **`toolset_composition`**: `FilteredToolset`, `MergedToolset`, `PrefixedToolset`, `BasicToolset`, `string_predicate`, and full composition chains.
- **`server_compaction`**: `ServerConfig::with_compaction()`, `EventsCompactionConfig`, and custom `BaseEventsSummarizer`.

#### adk-session
- **Typed identity session APIs**: `AppendEventRequest` struct and `SessionService::append_event_for_identity()` default method accept `AdkIdentity` for unambiguous session-scoped event appending. Additive `get_for_identity()` and `delete_for_identity()` default methods for typed get/delete. All 8 backends (InMemory, SQLite, PostgreSQL, Redis, MongoDB, Firestore, Neo4j, Vertex) override `append_event_for_identity()`. `InMemorySessionService` uses `AdkIdentity` as its internal HashMap key instead of delimiter-concatenated strings. Typed request helpers on `GetRequest`, `DeleteRequest`, `ListRequest`, and `CreateRequest`.
- **Legacy append guidance**: The typed `AdkIdentity` path is now the preferred session API for new code. The legacy `append_event(&str, ...)` method remains for migration only and is the first legacy identity API slated for deprecation after internal callers complete their move to typed identity.
- **Schema migrations**: Versioned, forward-only migration system for all database backends (SQLite, PostgreSQL, MongoDB, Neo4j). Each backend tracks applied migrations in a `_schema_migrations` registry table with checksums and timestamps. Migrations are idempotent — calling `migrate()` on an already-current database is a no-op.
- **Baseline detection**: `migrate()` detects pre-existing tables created before the migration system and registers them as already applied, avoiding destructive re-creation.
- **`schema_version()` method**: All database backends expose `schema_version()` returning the current migration version (0 if no migrations applied).
- **`from_pool()` / `pool()` methods on `SqliteSessionService`**: Parity with other backends for constructing from an existing connection pool and accessing the inner pool.

#### adk-memory
- **Schema migrations**: Same versioned migration system as `adk-session`, applied to all `adk-memory` database backends (SQLite, PostgreSQL, MongoDB, Neo4j). Each backend has its own migration registry and version tracking.
- **`schema_version()` method**: All database backends expose `schema_version()`.

#### adk-cli / adk-server
- **Production app builder path**: `Launcher` now exposes `build_app()` and `build_app_with_a2a(...)`, making it possible to reuse ADK server wiring while still owning the Axum router, middleware stack, and serve loop in production applications.
- **Launcher A2A and telemetry configuration**: `Launcher` now supports `with_a2a_base_url(...)` and `with_telemetry(...)`, so A2A routes and telemetry initialization are configurable instead of hardcoded in serve mode.
- **Server runtime passthrough**: `ServerConfig` now exposes `with_compaction(...)` and `with_context_cache(...)`, and the SSE + A2A runtime controllers now forward those settings into `RunnerConfig`.

#### adk-runner
- **Typed execution identity**: `InvocationContext` stores `ExecutionIdentity` internally, providing validated typed identity throughout the agent execution lifecycle. Event creation and agent transfers use typed invocation identity after boundary parsing.

#### adk-server / adk-studio
- **Boundary identity parsing**: HTTP and Studio ingress handlers parse user-controlled `app_name`, `user_id`, and `session_id` values into typed identifiers at the boundary, returning `400 Bad Request` on invalid input instead of panicking downstream.

### Changed

#### adk-session
- **`DatabaseSessionService` renamed to `SqliteSessionService`**: The struct, source file (`database.rs` → `sqlite.rs`), and test file (`database_tests.rs` → `sqlite_tests.rs`) have been renamed to accurately reflect the SQLite-only backend. A deprecated type alias `DatabaseSessionService` is provided for backward compatibility. The `database` feature flag remains as an alias for `sqlite`.

#### adk-realtime
- **LiveKit re-exports**: Replaced glob `pub use livekit::prelude::*` with explicit type re-exports in `adk_realtime::livekit` module, eliminating semver hazard from upstream prelude changes
- **Breaking**: Removed crate-level `pub use ::livekit` and `pub use ::livekit_api` re-exports that collided with the `livekit` module namespace — use `adk_realtime::livekit::{AccessToken, VideoGrants}` instead of `adk_realtime::livekit_api::access_token::{AccessToken, VideoGrants}`
- Added `AudioFrame` re-export to `adk_realtime::livekit` for downstream audio processing

#### adk-core
- **`ToolOutcome` struct**: Structured metadata for tool execution results — carries tool name, arguments, success/failure, execution duration, optional error message, and retry attempt number. Available via `CallbackContext::tool_outcome()` in after-tool callbacks.
- **`tool_outcome()` default method on `CallbackContext`**: Returns `Option<ToolOutcome>`, defaulting to `None` for full backward compatibility with existing implementors.
- **`RetryBudget` struct**: Configurable retry policy with `max_retries` and `delay` for automatic tool retry on transient failures.
- **`OnToolErrorCallback` type**: Promoted to `adk-core` as the canonical, framework-level tool-error callback type shared by `adk-agent` and `adk-plugin`.
- **`AfterToolCallbackFull` type**: V2 rich after-tool callback aligned with Python/Go ADK model. Receives `(CallbackContext, Tool, args, response)` and can inspect or replace the tool response sent to the LLM.

#### adk-agent
- **`.toolset()` builder method**: `LlmAgentBuilder` now accepts `Arc<dyn Toolset>` for dynamic per-invocation tool resolution. Toolsets are resolved at the start of each `run()` call using the current `ReadonlyContext`, enabling context-dependent tools (e.g., per-user browser sessions). Static `.tool()` and dynamic `.toolset()` can be mixed freely.
- **`.default_retry_budget()` and `.tool_retry_budget()`**: Configure automatic retry for transient tool failures. Per-tool budgets override the default. When retries are exhausted, the final failure is reported to the LLM.
- **`.circuit_breaker_threshold()`**: Tracks consecutive tool failures per tool name within an invocation. After the configured threshold, the tool is temporarily disabled with an immediate error response to the LLM. Resets at the start of each new invocation.
- **`.on_tool_error()` callback**: Register fallback handlers invoked when a tool fails (after retries are exhausted). Callbacks can return a substitute `Value` used as the function response, or `None` to pass through to the next handler.
- **`.after_tool_callback_full()` builder method**: V2 rich after-tool callback that receives the tool, arguments, and response. Aligned with Python/Go ADK model for first-class tool result handling.

#### adk-browser
- **`BrowserSessionPool`**: Multi-tenant session pool for managing browser sessions across concurrent agent invocations. Supports configurable pool size and session lifecycle management.
- **`BrowserProfile` enum**: Pool-aware toolset creation with `Shared` (pooled) and `Dedicated` (single-session) profiles.
- **JS string escaping**: `escape_js_string()` utility for safe JavaScript injection in evaluate tool.

#### adk-tool
- **Toolset composition**: `adk-tool/src/toolset/` module with composable toolset support for combining multiple tool sources.

#### adk-cli
- **Global provider flags**: `--provider`, `--model`, `--api-key` flags available on all subcommands.
- **First-run setup wizard**: Interactive provider selection and API key configuration on first launch.
- **Default to REPL**: Running `adk-rust` with no subcommand starts an interactive session.

### Fixed

#### adk-auth
- **Cross-role deny precedence**: `AccessControl` and `SsoAccessControl` now evaluate deny rules across all assigned roles before allowing access. Previously, the first allowing role could bypass a deny from another role, making authorization depend on role assignment order.
- **Verified email identity mapping**: `ClaimsMapper::user_id_from_email()` and `TokenClaims::user_id()` now require `email_verified == true` before using an email claim as the effective identity. Unverified emails fall back to `sub`.
- **SSO validation hardening**: OIDC discovery now rejects issuer mismatches, provider validators now enforce `nbf`, JWKS refreshes are single-flight with a cache key cap, and Azure multi-tenant validation can be restricted with `with_allowed_tenants(...)`.
- **Auth bridge implementation**: The `auth-bridge` feature now provides `JwtRequestContextExtractor` for `adk-server`, mapping Bearer tokens into `RequestContext` with validated user IDs and JWT scopes.
- **FileAuditSink mutex poisoning**: `FileAuditSink` now recovers from poisoned mutex instead of panicking, using `unwrap_or_else` to reclaim the lock guard.
- **TokenError placeholder**: `TokenError::placeholder()` now returns a proper error variant instead of a debug-only stub that could mask real token validation failures.
- **ScopedTool/ProtectedTool macro consolidation**: Eliminated duplicated trait implementations between `ScopedTool` and `ProtectedTool` by extracting shared logic into macros, reducing maintenance surface.

#### adk-gemini
- **FunctionCall serialization**: Fixed `thought_signature` leaking inside the `functionCall` JSON object when serializing `Part::FunctionCall`. The Gemini API expects `thoughtSignature` at the Part level only, not inside `functionCall`. The conversion layer in `adk-model` now correctly places the signature at the Part level and omits it from the inner `FunctionCall` struct.
- **Broken serde attributes**: Restored missing `#[serde(skip_serializing_if = "Option::is_none")]` attributes on `FunctionDeclaration`, `FunctionCall`, `FunctionResponse`, and `ToolConfig` fields that had been replaced with invalid placeholder text, causing compilation failures.
- **Non-object tool responses**: Gemini-backed agents now normalize array/scalar tool outputs into a valid object payload before sending `functionResponse.response`. This fixes Gemini tool-calling flows for tools like `RagTool` that naturally return lists of results.

#### adk-agent / adk-runner / adk-core
- **Multi-agent transfer round-trip**: Sub-agents can now transfer back to their parent and peer agents. The runner computes valid transfer targets (parent + peers) and passes them via `RunConfig::transfer_targets`. Previously, sub-agents with no children had an empty valid-agents list, making all transfers fail.
- **Transfer chain support**: The runner now loops on transfers (up to 10 hops) instead of handling only a single transfer. This enables coordinator → sub-agent → coordinator round-trip patterns.
- **Sub-agent conversation history isolation**: When a sub-agent is invoked via transfer, it now receives filtered conversation history that excludes other agents' events. Previously, the sub-agent's LLM saw the parent's tool calls mapped as "model" role, causing it to think work was already done and return immediately.
- **Transfer tool schema**: The `transfer_to_agent` tool declaration now includes valid target names as an `enum` in the JSON schema and lists them in the description, so the LLM knows which agents it can transfer to.
- **`disallow_transfer_to_parent` / `disallow_transfer_to_peers`**: These `LlmAgent` builder flags are now wired up and actively filter the transfer targets list. Previously they were stored but never checked.
- **Agent runtime hardening**: `LlmAgent` now enforces configured input/output guardrails at runtime, normalizes XML tool-call markup before tool dispatch, preserves unique `function_call_id` values per tool invocation, and rejects duplicate sub-agent names during builder validation.
- **Workflow agent contract fixes**: `ParallelAgent` and `ConditionalAgent` now execute their registered before/after callbacks, `IncludeContents::None` now keeps only the current user turn plus injected instructions, and `LoopAgent` maintains local conversation history for direct workflow use outside `adk-runner`.
- **Deterministic LLM routing**: `LlmConditionalAgent` now resolves overlapping route labels deterministically, preferring exact matches and then the longest matching label.

#### adk-browser
- **Centralized `ensure_started()`**: All WebDriver-accessing methods now go through a single session initialization path, eliminating race conditions on first use.
- **Navigation tool response alignment**: `navigate` tool returns consistent structured responses across success and error paths.
- **Tool hardening**: `click`, `evaluate`, `extract`, `type_text`, and `wait` tools handle edge cases (stale elements, timeouts, JS errors) with actionable error messages.

#### adk-model
- **DeepSeek reasoning content**: `Part::Thinking` content is now correctly placed in `reasoning_content` field instead of being mixed into the main `content` field.

#### adk-server
- **Compaction config wiring**: `compaction_config` from server config is now passed through to `RunnerConfig` in both runtime and A2A controllers.

#### adk-agent (Added)
- **Regression test suite**: New `review_regression_tests.rs` with 10 targeted tests covering guardrail runtime enforcement, parallel/conditional agent callbacks, function_call_id uniqueness, `IncludeContents::None` filtering, deterministic LLM routing, sub-agent name uniqueness validation, and tool_call_markup normalization.
- **README accuracy**: Updated README to reflect all current builder methods, correct examples, and accurate feature descriptions.
- **Guardrail example update**: Removed outdated caveat from `guardrail_agent` example that incorrectly stated guardrails were builder-only; example now documents that guardrails are enforced at runtime.

## [0.3.2] - 2026-02-17

### ⭐ Highlights
- **9 New LLM Providers**: xAI, Fireworks AI, Together AI, Mistral AI, Perplexity, Cerebras, SambaNova (OpenAI-compatible), Amazon Bedrock (AWS SDK), and Azure AI Inference (reqwest) — all feature-gated with contract tests
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

#### adk-model — New Providers
- **Fireworks AI** (`fireworks` feature) — OpenAI-compatible provider for fast open-model inference. Default model: `accounts/fireworks/models/llama-v3p1-8b-instruct`. Env: `FIREWORKS_API_KEY`
- **Together AI** (`together` feature) — OpenAI-compatible provider for hosted open models. Default model: `meta-llama/Llama-3.3-70B-Instruct-Turbo`. Env: `TOGETHER_API_KEY`
- **Mistral AI** (`mistral` feature) — OpenAI-compatible provider for Mistral cloud models. Default model: `mistral-small-latest`. Env: `MISTRAL_API_KEY`
- **Perplexity** (`perplexity` feature) — OpenAI-compatible provider for search-augmented LLM. Default model: `sonar`. Env: `PERPLEXITY_API_KEY`
- **Cerebras** (`cerebras` feature) — OpenAI-compatible provider for ultra-fast inference. Default model: `llama-3.3-70b`. Env: `CEREBRAS_API_KEY`
- **SambaNova** (`sambanova` feature) — OpenAI-compatible provider for fast inference. Default model: `Meta-Llama-3.3-70B-Instruct`. Env: `SAMBANOVA_API_KEY`
- **Amazon Bedrock** (`bedrock` feature) — AWS SDK Converse API with IAM/STS authentication, streaming and non-streaming support. Default model: `anthropic.claude-sonnet-4-20250514-v1:0`. Uses AWS credential chain
- **Azure AI Inference** (`azure-ai` feature) — reqwest-based client for Azure AI Inference endpoints with `api-key` header auth, streaming SSE and non-streaming JSON. Env: `AZURE_AI_API_KEY`
- `all-providers` feature now includes all eight new provider feature flags
- Contract tests (`ProviderSpec` + `provider_contract_tests!` macro) for all eight new providers
- Comprehensive rustdoc with quick-start examples for all new provider types

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
