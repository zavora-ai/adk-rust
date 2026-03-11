# adk-agent Example Validation

Date: 2026-03-08

## Scope

Runtime validation for sensitive `adk-agent` changes across:

- `adk-agent` crate-local examples
- workflow-heavy workspace examples using `adk_agent`
- provider-backed examples for providers configured in this shell:
  - Gemini / Google
  - OpenAI
  - DeepSeek
  - Groq

## Environment

Available credentials detected:

- `GOOGLE_API_KEY`
- `OPENAI_API_KEY`
- `DEEPSEEK_API_KEY`
- `GROQ_API_KEY`
- `AWS_ACCESS_KEY_ID`
- `AWS_SECRET_ACCESS_KEY`
- `AWS_DEFAULT_REGION`

Not currently available in this shell:

- `ANTHROPIC_API_KEY`
- Azure/provider-specific keys for Mistral/Together/Fireworks/Perplexity/Cerebras/SambaNova

## Run Matrix

### adk-agent examples

| Example | Result | Notes |
|---|---|---|
| `agent_control_demo` | PASS | Runs cleanly with Gemini feature enabled |
| `state_management_demo` | PASS | Fixed direct sequential state propagation; rerun completed successfully |

### Additional `adk-agent` consumers

| Example | Result | Notes |
|---|---|---|
| `function_tool` | PASS | Calculator tool call completed with real prompt input |
| `multiple_tools` | PASS | Transfer-to-agent flow completed; model asked a follow-up instead of directly emitting a poem for the chosen prompt |
| `agent_tool` | PASS | Coordinator delegated to `math_expert` and returned the correct final answer |
| `structured_output` | PASS | Returned valid JSON for a real weather prompt |
| `template` | PASS | Session-templated multilingual prompt flow completed |
| `load_artifacts` | PASS | Agent/tool wiring example completed; constructs agent successfully without requiring an artifact backend |
| `eval_agent` | PASS | End-to-end evaluation flow completed; one built-in eval case failed similarity threshold, which the example reports rather than treating as a runtime error |

### Browser

| Example | Result | Notes |
|---|---|---|
| `browser_test` | BLOCKED | WebDriver not available at `http://localhost:4444`; example exits cleanly with startup instructions for `selenium/standalone-chrome` |

### UI

| Example | Result | Notes |
|---|---|---|
| `ui_agent` | PASS | Gemini-backed UI tool flow completed and invoked `render_form` successfully |
| `a2ui_demo` | PASS | A2UI component generation completed and invoked `render_screen` successfully |
| `ui_server` | PASS | Aggregated UI server started and served `/api/health` and `/ui/` successfully |

### Official Docs Examples

| Example | Result | Notes |
|---|---|---|
| `llm_agent_test` | PASS | Workspace doc-example crate builds/tests cleanly after agent changes |
| `callbacks_test` | PASS | Workspace doc-example crate builds/tests cleanly after callback fixes |
| `guardrails_test` | PASS | Workspace doc-example crate builds/tests cleanly after guardrail runtime integration |
| `guardrail_readme_test` | PASS | Workspace doc-example crate builds/tests cleanly after guardrail doc updates |
| `workflow_test` | BLOCKED | Not currently wired as a workspace member; Cargo rejects standalone testing from its manifest under the current workspace layout |

### Skills

| Example | Result | Notes |
|---|---|---|
| `skills_basic` | PASS | Fixed `app_name` / session mismatch; runner-level auto-skill injection now completes |
| `skills_policy` | PASS | Fixed `app_name` / session mismatch; policy-filtered run now completes |
| `skills_conventions` | PASS | Convention-file selection produced the expected Gemini setup guidance |
| `skills_workflow` | PASS | Sequential skill-backed workflow completed |
| `skills_discovery` | PASS | Pure lexical discovery and scoring completed without provider calls |
| `skills_coordinator` | PASS | Tier-3 coordination demo completed, including strict-mode rejection path |

### Graph

| Example | Result | Notes |
|---|---|---|
| `graph_agent` | PASS | Fixed `AgentNode` output aggregation in example mapper; rerun now produces full translation and summary |
| `graph_workflow` | PASS | Fixed output aggregation in example mappers; rerun now preserves full entities, analysis, and summary |
| `graph_checkpoint` | PASS | Fixed output aggregation in example mappers; checkpointed workflow now preserves full step outputs |
| `graph_conditional` | PASS | Fixed output aggregation in example mappers; rerun now returns full positive/negative/neutral responses |
| `graph_hitl` | PASS | Fixed output aggregation in example mappers; rerun now avoids repeated planner-side logging spam and completes interrupt/resume flow |
| `graph_react` | PASS | ReAct tool/reasoning loop completed across both sample prompts |
| `graph_streaming` | PASS | Fixed output aggregation in example mapper; non-streaming rerun now preserves the full generated story |
| `graph_supervisor` | PASS | Fixed output aggregation in example mappers; rerun now preserves full research, content, and code sections |

### Guardrails

| Example | Result | Notes |
|---|---|---|
| `guardrail_basic` | PASS | PII redaction, keyword filtering, and combined transformation flow completed |
| `guardrail_schema` | PASS | Schema validation and markdown-embedded JSON validation completed |

### Additional Gemini-backed examples

| Example | Result | Notes |
|---|---|---|
| `translator` | PASS | Interactive translation pipeline completed with real prompt input |
| `sequential_code` | PASS | Example is intentionally scaffold-only; startup output completed and documented pending full runner integration |
| `server` | PASS | Server started and served `/api/health` and `/ui/` successfully |
| `web` | PASS | Multi-agent web server started and served `/api/health` and `/ui/` successfully |
| `research_paper` | PASS | Research-paper server mode started and served `/api/health` and `/ui/` successfully |
| `auth_bridge` | PASS | Verified full auth flow with real requests: unauthenticated `401`, admin access allowed, read-only user denied at tool level |

### RAG

| Example | Result | Notes |
|---|---|---|
| `rag_basic` | PASS | Ingest/search demo completed across three queries |
| `rag_markdown` | PASS | Markdown ingestion and search demo completed across three queries |
| `rag_agent` | PASS | Fixed Gemini function-response serialization for array-valued tool outputs; agent now completes multi-tool RAG answer flow |
| `rag_multi_collection` | PASS | Fixed shared Gemini tool-response serialization path; collection-aware support agent now completes successfully |
| `rag_reranker` | PASS | Fixed shared Gemini tool-response serialization path; reranker-backed HR agent now completes successfully |
| `rag_recursive` | PASS | Fixed shared Gemini tool-response serialization path; recursive codebase Q&A agent now completes successfully |

### Gemini / Google

| Example | Result | Notes |
|---|---|---|
| `guardrail_agent` | PASS | Builds and runs; example itself does not execute a guarded prompt |
| `quickstart` | PASS | Tool-backed query succeeded |
| `sequential` | PASS | Interactive console flow succeeded with piped input |
| `parallel` | PASS | Interactive console flow succeeded with piped input |
| `loop_workflow` | PASS | Loop workflow returned refined content and exited on EOF |
| `multi_turn_tool` | PASS | Multi-turn tool history preserved; tool call and order flow completed |
| `gemini_thinking` | PASS | Thinking + thought-signature demo completed |
| `gemini_prompt_caching` | PASS | Completed, but reported `0` cache read/create tokens in this run |
| `gemini_token_usage` | PASS | Usage metadata demo completed |
| `graph_gemini` | PASS | Multi-step graph workflow completed |
| `roadmap_gemini_compat` | PASS | Retry/backward-compat demo completed |
| `roadmap_gemini_sdk` | PASS | Generate call succeeded; embed probe returned provider `404` but example handled it and exited successfully |
| `verify_backend_selection` | PASS | Studio backend constructor/stream/embed checks completed |
| `eval_llm_gemini` | PASS | LLM-judge evaluation demo completed |
| `gemini_multimodal` | PASS | Fixed invalid embedded PNG payloads and reran successfully |
| `verify_vertex_streaming` | SKIP | Missing `GOOGLE_CLOUD_PROJECT` / Vertex auth prerequisites in current shell |

### OpenAI

| Example | Result | Notes |
|---|---|---|
| `openai_basic` | PASS | Runner-backed single turn completed |
| `openai_tools` | PASS | Repeated tool calls succeeded |
| `openai_workflow` | PASS | Sequential workflow completed via console input |
| `openai_template` | PASS | Session-templated prompt flow completed |
| `openai_parallel` | PASS | Parallel workflow completed via console input |
| `openai_loop` | PASS | Loop agent called `exit_loop` |
| `openai_agent_tool` | PASS | Agent-tool delegation flow completed |
| `openai_structured` | PASS | Fixed strict-schema compatibility and reran successfully |
| `openai_structured_basic` | PASS | Fixed strict-schema compatibility and reran successfully |
| `openai_structured_strict` | PASS | Strict structured-output example completed |
| `openai_sequential_code` | PASS | Multi-stage code workflow completed |
| `openai_research_paper` | PASS | Research-paper workflow completed |
| `openai_artifacts` | PASS | Builder/demo example completed |
| `openai_mcp` | PASS | Fallback MCP demo completed after fixing mock `RunConfig` |
| `openai_server` | PASS | Server started and served `/api/health` and `/ui/` successfully |
| `openai_web` | PASS | Multi-agent web server started and served `/api/health` and `/ui/` successfully |
| `openai_a2a` | PASS | A2A server started and served `/api/health`, `/ui/`, and `/.well-known/agent.json` |
| `openai_local` | BLOCKED | Local OpenAI-compatible server is reachable, but `/v1/models` and `/api/tags` returned no installed models |
| `openai_token_usage` | PASS | Token usage demo completed |

### DeepSeek

| Example | Result | Notes |
|---|---|---|
| `deepseek_basic` | PASS | Runner-backed single turn completed |
| `deepseek_reasoner` | PASS | Reasoner demo completed |
| `deepseek_tools` | PASS | Tool-backed query completed |
| `deepseek_thinking_tools` | PASS | Fixed DeepSeek reasoning serialization; multi-tool reasoning flow now completes successfully |
| `deepseek_caching` | PASS | Caching demo completed after disk-space issue was cleared |
| `deepseek_sequential` | PASS | Sequential workflow completed via console input |
| `deepseek_supervisor` | PASS | Supervisor workflow completed via console input |
| `deepseek_structured` | PASS | Structured output demo completed with real input |
| `deepseek_token_usage` | PASS | Token usage demo completed |

### Groq

| Example | Result | Notes |
|---|---|---|
| `groq_basic` | PASS | Interactive console flow succeeded with piped input |
| `groq_tools` | PASS | Tool-backed arithmetic flow completed |

### Anthropic

| Example | Result | Notes |
|---|---|---|
| `anthropic_basic` | PASS | Console flow succeeded with piped input |
| `anthropic_tools` | PASS | Tool-backed weather + arithmetic flow completed |
| `anthropic_thinking` | PASS | Thinking demo completed |
| `anthropic_models` | PASS | Model listing/token counting demo completed |
| `anthropic_streaming` | PASS | Streaming demo completed |
| `anthropic_quickstart` | PASS | Started successfully; run used immediate EOF so no model turn was exercised |
| `anthropic_multimodal` | PASS | Fixed stale unsupported-MIME expectation and invalid inline PNG payload; rerun completed successfully |
| `anthropic_retry` | PASS | Retry/error-handling demo completed |
| `anthropic_token_usage` | PASS | Token usage + prompt caching demo completed |

## Changes made during validation

- Fixed an immediate panic in `state_management_demo` by replacing `run_config()`'s `unimplemented!()` with a real `RunConfig::default()` stored on the context.
- Fixed direct workflow state propagation in `LoopAgent` / `SequentialAgent` local execution by tracking local state and applying emitted `state_delta` between sub-agents.
- Added a regression test covering state propagation without `adk-runner`.
- Fixed `skills_basic` and `skills_policy` example session setup by using the same `app_name` for both `RunnerConfig` and `CreateRequest`.
- Patched `openai_local` to default to `qwen3.5` and to make strict structured output opt-in for better Ollama compatibility.
- Fixed multiple `graph_*` example `AgentNode` output mappers to aggregate text across all node events before writing state, preventing truncated outputs and repeated side-effect logging.
- Fixed Gemini tool-response serialization for non-object values by wrapping array/scalar tool results into a valid object payload before sending `functionResponse.response`, which unblocked the RAG agent examples.

## New runtime findings from Gemini sweep

### `gemini_multimodal`

Resolved:

- The example's embedded 1x1 PNG payloads had invalid `IDAT` CRC values.
- Replaced them with valid PNG byte sequences.
- Rerun succeeded:
  - Example 1 identified the red image correctly.
  - Example 2 identified red and blue images correctly.
  - Example 3 console flow still worked.

## Next steps

1. Extend runtime coverage to additional recent `adk_agent` examples if needed:
   - `ollama_*` examples when local Ollama is available
   - AWS/Bedrock-backed examples if a simple agent example exists in the workspace
