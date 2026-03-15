# ADK Rust Examples

100+ example applications demonstrating how to use the ADK Rust framework.

## Structure

```
examples/
│
├── 🚀 Getting Started
│   ├── quickstart/                  # Simple weather agent
│   ├── function_tool/               # Custom function tool
│   ├── multiple_tools/              # Agent composition
│   ├── template/                    # Starter template
│   ├── structured_output/           # Structured JSON output
│   ├── translator/                  # Translation agent
│   ├── streaming_demo/              # Streaming responses
│   └── agent_tool/                  # Agent-as-tool pattern
│
├── 🔧 Servers & Protocols
│   ├── server/                      # REST API server
│   ├── a2a/                         # A2A protocol
│   ├── web/                         # Multi-agent server
│   ├── mcp/                         # MCP integration (stdio)
│   ├── mcp_http/                    # MCP over HTTP transport
│   └── mcp_oauth/                   # MCP with OAuth
│
├── 🔄 Workflows
│   ├── sequential/                  # Sequential workflow
│   ├── sequential_code/             # Code generation workflow
│   ├── parallel/                    # Parallel workflow
│   ├── loop_workflow/               # Iterative loop
│   └── load_artifacts/              # Artifact loading
│
├── 🛡️ Guardrails
│   ├── guardrail_basic/             # Basic input/output guardrails
│   ├── guardrail_schema/            # Schema-based validation
│   └── guardrail_agent/             # Agent with guardrails
│
├── 📚 Skills
│   ├── skills_llm_minimal/          # Basic LlmAgent + local skills
│   ├── skills_auto_discovery/       # Auto-discover .skills
│   ├── skills_conventions_index/    # AGENTS/CLAUDE/GEMINI/COPILOT/SKILLS files
│   ├── skills_conventions_llm/      # Live Gemini + convention-file injection
│   ├── skills_policy_filters/       # Tag include/exclude + skill budget
│   ├── skills_runner_injector/      # Runner-level skill injection
│   └── skills_workflow_minimal/     # Workflow agent + skills
│
├── 🗺️ Roadmap Features
│   ├── roadmap_gemini_compat/       # Sync Gemini constructor + additive retry
│   ├── roadmap_vertex_auth/         # Vertex auth modes (API key / ADC / SA / WIF)
│   ├── roadmap_gemini_sdk/          # adk-gemini v1 + Vertex SDK surface
│   └── roadmap_retry_matrix/        # Standardized retry across providers
│
├── 📊 Graph Workflows
│   ├── graph_workflow/              # Basic graph workflow
│   ├── graph_conditional/           # Conditional routing
│   ├── graph_llm/                   # LLM-powered graph nodes
│   ├── graph_react/                 # ReAct pattern with tool loop
│   ├── graph_supervisor/            # Multi-agent supervisor
│   ├── graph_hitl/                  # Human-in-the-loop
│   ├── graph_checkpoint/            # State persistence
│   ├── graph_streaming/             # Streaming graph execution
│   ├── graph_agent/                 # Graph-based agent
│   ├── graph_gemini/                # Graph with Gemini
│   └── graph_openai/                # Graph with OpenAI
│
├── 🤖 Gemini (Default Provider)
│   ├── quickstart/                  # (see Getting Started)
│   ├── research_paper/              # Full-stack research paper generator
│   └── docs_translator/            # Document translation
│
├── 🔵 OpenAI
│   ├── openai_basic/                # Basic chat
│   ├── openai_tools/                # Function calling
│   ├── openai_workflow/             # Workflow orchestration
│   ├── openai_template/             # Template pattern
│   ├── openai_parallel/             # Parallel execution
│   ├── openai_loop/                 # Loop workflow
│   ├── openai_agent_tool/           # Agent-as-tool
│   ├── openai_structured/           # Structured output
│   ├── openai_structured_basic/     # Basic structured output
│   ├── openai_structured_strict/    # Strict schema output
│   ├── openai_local/                # Local OpenAI-compatible
│   ├── openai_artifacts/            # Artifact management
│   ├── openai_mcp/                  # MCP integration
│   ├── openai_a2a/                  # A2A protocol
│   ├── openai_server/               # Server mode
│   ├── openai_web/                  # Web server
│   ├── openai_sequential_code/      # Code generation
│   ├── openai_research_paper/       # Research paper
│   └── debug_openai_error/          # Error debugging
│
├── 🟣 Anthropic
│   ├── anthropic_basic/             # Basic chat
│   └── anthropic_tools/             # Function calling
│
├── 🔷 DeepSeek
│   ├── deepseek_basic/              # Basic chat
│   ├── deepseek_reasoner/           # Thinking mode with reasoning
│   ├── deepseek_tools/              # Function calling
│   ├── deepseek_thinking_tools/     # Reasoning + tools
│   ├── deepseek_caching/            # Context caching
│   ├── deepseek_sequential/         # Multi-agent pipeline
│   ├── deepseek_supervisor/         # Supervisor pattern
│   └── deepseek_structured/         # Structured JSON output
│
├── 🟢 Ollama (Local)
│   ├── ollama_basic/                # Basic chat
│   ├── ollama_tools/                # Function calling
│   ├── ollama_mcp/                  # MCP integration
│   ├── ollama_sequential/           # Sequential workflow
│   ├── ollama_parallel/             # Parallel workflow
│   ├── ollama_supervisor/           # Supervisor pattern
│   └── ollama_structured/           # Structured output
│
├── ⚡ Groq
│   ├── groq_basic/                  # Basic chat
│   └── groq_tools/                  # Function calling
│
├── 🧠 mistral.rs (Local Inference)
│   ├── mistralrs_basic/             # Basic text generation
│   ├── mistralrs_tools/             # Function calling
│   ├── mistralrs_vision/            # Image understanding
│   ├── mistralrs_isq/               # In-situ quantization
│   ├── mistralrs_lora/              # LoRA adapter usage
│   ├── mistralrs_multimodel/        # Multi-model serving
│   ├── mistralrs_mcp/               # MCP client integration
│   ├── mistralrs_speech/            # Text-to-speech synthesis
│   └── mistralrs_diffusion/         # Image generation with FLUX
│
├── 🎙️ Realtime Voice
│   ├── realtime_basic/              # Basic voice agent
│   ├── realtime_vad/                # Voice activity detection
│   ├── realtime_tools/              # Voice + tool calling
│   └── realtime_handoff/            # Voice agent handoff
│
├── 🌐 Browser Automation
│   ├── browser_basic/               # Basic session and tools
│   ├── browser_agent/               # AI agent with browser
│   ├── browser_interactive/         # Full 46-tool example
│   ├── browser_openai/              # OpenAI browser agent
│   └── browser_test/                # Integration tests
│
├── 🖥️ UI & Visualization
│   ├── ui_agent/                    # UI-enabled agent
│   ├── ui_server/                   # UI server
│   ├── ui_protocol_profiles/        # Tri-protocol UI outputs
│   ├── ui_working/                  # Working UI demos (support, appointment, etc.)
│   ├── ui_react_client/             # React client
│   ├── ui_react_client_mui/         # React + MUI client
│   ├── a2ui_demo/                   # A2UI protocol demo
│
├── 🔐 Auth & Security
│   ├── auth_basic/                  # Basic RBAC
│   ├── auth_audit/                  # Audit logging
│   ├── auth_sso/                    # SSO integration
│   ├── auth_jwt/                    # JWT authentication
│   ├── auth_oidc/                   # OpenID Connect
│   └── auth_google/                 # Google OAuth
│
├── 📏 Evaluation
│   ├── eval_basic/                  # Basic evaluation setup
│   ├── eval_trajectory/             # Tool trajectory validation
│   ├── eval_semantic/               # LLM-judged matching
│   ├── eval_rubric/                 # Rubric-based scoring
│   ├── eval_similarity/             # Response similarity
│   ├── eval_report/                 # Report generation
│   ├── eval_llm_gemini/             # Gemini evaluation
│   ├── eval_llm_openai/             # OpenAI evaluation
│   ├── eval_agent/                  # Agent evaluation
│   ├── eval_graph/                  # Graph evaluation
│   └── eval_realtime/               # Realtime evaluation
│
├── 🎨 ADK Studio Templates (JSON)
│   ├── customer_onboarding.json     # Welcome email, enrichment, CRM
│   ├── content_moderation.json      # Classify, flag, auto-respond
│   ├── daily_standup_digest.json    # Jira + Slack → LLM summary
│   ├── lead_scoring.json            # Score leads, route to sales
│   ├── incident_response.json       # Severity triage, page on-call
│   ├── invoice_processing.json      # Extract, validate, approve
│   ├── employee_offboarding.json    # Revoke access, notify teams
│   ├── bug_triage.json              # Classify, assign, ticket
│   ├── newsletter_generator.json    # Multi-source → curate → send
│   ├── data_pipeline_monitor.json   # Diagnose failures, auto-retry
│   ├── contract_reviewer.json       # Clause extraction, risk scoring
│   ├── social_media_scheduler.json  # Multi-platform publishing
│   ├── expense_report.json          # Receipt → policy check → approve
│   ├── churn_predictor.json         # Usage analysis, retention
│   └── api_health_dashboard.json    # Endpoint monitoring, alerting
│
└── 🤖 Standalone Crates
    └── ralph/                       # Autonomous coding agent (cargo run -p ralph)
```


## Prerequisites

Set your API keys as needed:
```bash
# Google Gemini (default provider)
export GOOGLE_API_KEY="your-key"    # or GEMINI_API_KEY

# Other providers
export OPENAI_API_KEY="your-key"
export ANTHROPIC_API_KEY="your-key"
export DEEPSEEK_API_KEY="your-key"
export GROQ_API_KEY="your-key"
```

## Running Examples

```bash
# Default (Gemini) examples
cargo run --example quickstart

# Provider-specific examples (require feature flags)
cargo run --example openai_basic --features openai
cargo run --example anthropic_basic --features anthropic
cargo run --example deepseek_basic --features deepseek
cargo run --example ollama_basic --features ollama
cargo run --example groq_basic --features groq

# Local inference (no API key needed)
cargo run --example mistralrs_basic --features mistralrs

# Browser automation (requires WebDriver)
docker run -d -p 4444:4444 selenium/standalone-chrome
cargo run --example browser_agent --features browser

# Guardrails
cargo run --example guardrail_basic --features guardrails

# Realtime voice
cargo run --example realtime_basic --features realtime-openai

# Auth/SSO examples
cargo run --example auth_basic
cargo run --example auth_sso --features sso

# Standalone crate
cargo run -p ralph

```

## Example Categories

| Category | Count | Feature Flag |
|----------|-------|-------------|
| Getting Started | 8 | — |
| Servers & Protocols | 6 | `http-transport` for MCP HTTP |
| Workflows | 5 | — |
| Guardrails | 3 | `guardrails` |
| Skills | 7 | — |
| Roadmap Features | 4 | — |
| Graph Workflows | 11 | `openai` for graph_openai |
| Gemini | 3 | — |
| OpenAI | 19 | `openai` |
| Anthropic | 2 | `anthropic` |
| DeepSeek | 8 | `deepseek` |
| Ollama | 7 | `ollama` |
| Groq | 2 | `groq` |
| mistral.rs | 9 | `mistralrs` |
| Realtime Voice | 4 | `realtime-openai` |
| Browser | 5 | `browser` |
| UI & Visualization | 9 | — |
| Auth & Security | 6 | `sso` for SSO examples |
| Evaluation | 11 | `openai` for eval_llm_openai |
| Studio Templates | 15 | — (JSON files) |
| Standalone Crates | 1 | — (`cargo run -p ralph`) |
| **Total** | **145+** | |

## Parity with Go ADK

| Go Example | Rust Example | Status |
|------------|--------------|--------|
| quickstart | quickstart | ✅ |
| rest | server | ✅ |
| a2a | a2a | ✅ |
| mcp | mcp | ✅ |
| web | web | ✅ |
| tools/multipletools | multiple_tools | ✅ |
| tools/loadartifacts | load_artifacts | ✅ |
| workflowagents/sequential | sequential | ✅ |
| workflowagents/sequentialCode | sequential_code | ✅ |
| workflowagents/parallel | parallel | ✅ |
| workflowagents/loop | loop_workflow | ✅ |

## Beyond Go ADK

| Feature | Examples |
|---------|----------|
| OpenAI Integration | 19 examples covering tools, workflows, structured output, MCP, A2A |
| Anthropic Integration | anthropic_basic, anthropic_tools |
| DeepSeek Integration | 8 examples including reasoning, caching, supervisor |
| Ollama (Local) | 7 examples covering tools, MCP, workflows |
| Groq Integration | groq_basic, groq_tools |
| mistral.rs Local Inference | 9 examples: text, vision, speech, diffusion, LoRA |
| Realtime Voice | 4 examples: basic, VAD, tools, handoff |
| Graph Workflows | 11 examples: ReAct, supervisor, HITL, checkpoint |
| Browser Automation | 5 examples: basic, agent, interactive, OpenAI |
| Agent Evaluation | 11 examples: trajectory, semantic, rubric, report |
| Guardrails | 3 examples: basic, schema, agent |
| Auth & Security | 6 examples: RBAC, audit, SSO, JWT, OIDC, Google |
| UI & Visualization | 9 examples: React, 3D, spatial OS, A2UI |
| Studio Templates | 15 visual workflow templates for ADK Studio |

## ADK Studio Templates

15 ready-to-use JSON workflow templates combining LLM agents with action nodes (HTTP, Switch, Merge, Database, Transform, Set).

Import by copying any `.json` file to `~/Library/Application Support/adk-studio/projects/`.

See [studio_templates/README.md](studio_templates/README.md) for the full list and details.

## Tips

- Use `Ctrl+C` to exit console mode
- Server mode runs on port 8080 by default (override with `PORT` env var)
- Console mode includes readline history and editing
- Models are downloaded on first run for mistral.rs examples
- Diffusion models require significant GPU memory (~12-24GB VRAM)
