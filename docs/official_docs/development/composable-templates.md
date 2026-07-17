# Composable project templates

`cargo adk new` creates a working Rust project from three choices:

1. a **template** defines the agent or workflow shape;
2. **add-ons** add capabilities such as sessions, MCP, or telemetry; and
3. an **enterprise pattern** selects a template and a reviewed group of add-ons
   for a common product shape.

The installed CLI is authoritative because custom template directories can
extend or replace the built-in registry.

```bash
cargo adk templates
cargo adk addons
cargo adk new --help
```

## Create a project

```bash
# One LLM agent
cargo adk new support-agent --template llm

# A tool-using agent with operating capabilities
cargo adk new support-agent \
  --template tools \
  --addon sessions \
  --addon telemetry \
  --addon guardrails

# Preview generated files without writing them
cargo adk new support-agent --template graph --dry-run
```

`basic` remains an alias for `llm`. Use the explicit `llm` name in new
documentation and automation.

## Built-in templates

The built-in registry currently contains 12 templates.

| Template | What it creates | Choose it when |
|---|---|---|
| `llm` | One conversational `LlmAgent` | One agent can own the request and call tools as needed |
| `tools` | An LLM agent plus typed `#[tool]` examples | The first useful milestone is a visible Rust tool call |
| `rag` | An agent with a vector-search knowledge path | Answers must use a private document or knowledge collection |
| `api` | An agent exposed through an HTTP API | Another application will call the agent over the network |
| `openai` | An LLM agent configured for OpenAI | OpenAI is the intended starting provider |
| `sequential` | Multiple agents executed in a fixed order | Each stage depends on the previous stage's work |
| `parallel` | Concurrent specialists with aggregated results | Independent work can run at the same time |
| `loop` | Repeated execution until a condition is met | A reviewer, repair, or refinement cycle needs a bounded loop |
| `conditional` | Decision-based routing between agents | Different requests need different specialists or paths |
| `graph` | A branching workflow with checkpoints | Work must resume, branch, join, or survive process failure |
| `realtime` | Bidirectional audio and video handling | The product needs a live voice or multimodal conversation |
| `custom` | A manual implementation of the `Agent` trait | The execution contract cannot be expressed by a built-in agent |

Choose the execution shape first. Add sessions, MCP, telemetry, or other
product capabilities afterward instead of using them to decide the workflow.

## Capability add-ons

Repeat `--addon` to compose capabilities. The generator resolves their feature
flags, imports, initialization fragments, environment examples, and generated
files in a stable priority order.

| Add-on | What it adds | Typical reason to use it |
|---|---|---|
| `telemetry` | OpenTelemetry tracing setup | Follow requests across model, agent, and tool boundaries |
| `auth` | API-key and JWT authentication scaffolding | Protect a deployed agent endpoint |
| `sessions` | Session state service setup | Continue a conversation or workflow with stored state |
| `memory` | Semantic memory and RAG integration | Retrieve relevant knowledge beyond the current conversation |
| `mcp` | MCP feature wiring and client starting point | Connect capabilities owned by another process or service |
| `guardrails` | Input and output validation hooks | Enforce product rules before and after agent execution |
| `eval` | Evaluation harness scaffolding | Measure behavior against repeatable cases before release |
| `browser` | Browser automation integration | Let an approved agent operate a web interface |
| `server` | Axum HTTP and A2A server setup | Publish the agent for remote callers |

Example:

```bash
cargo adk new order-agent \
  --template tools \
  --addon mcp \
  --addon sessions \
  --addon server \
  --addon auth \
  --addon telemetry
```

Generated capability code is a starting point. Replace placeholder endpoints,
credentials, policies, and in-memory services with deployment-owned choices
before release.

## Composed enterprise patterns

Patterns appear in the same `--template` namespace as ordinary templates.
There is no separate `--pattern` flag.

| Pattern | Composition | Product starting point |
|---|---|---|
| `multi-agent` | `sequential` + telemetry | A visible multi-stage workflow |
| `production` | `llm` + server + auth + sessions + telemetry | An authenticated, observable agent service |
| `pipeline` | `sequential` + sessions + telemetry | A stateful processing pipeline |
| `chatbot` | `llm` + sessions + memory + server | A conversational HTTP product with recall |
| `a2a-server` | `llm` + server + sessions | An independently deployed A2A agent |

`a2a` remains an alias for `a2a-server`.

```bash
cargo adk new operations-agent --template production
cargo adk new research-team --template multi-agent --addon eval
cargo adk new public-specialist --template a2a-server --addon auth
```

## Provider and model selection

Templates have a default provider, but the CLI can override it without changing
the execution shape.

```bash
cargo adk new support-agent \
  --template tools \
  --provider openai \
  --model company-approved-model
```

Use `--non-interactive` in CI so missing choices fail instead of opening a
prompt. Use `--json-output` when another program needs the generation result.

## Custom templates

Pass a directory of TOML manifests when an organization needs a reusable
starting point beyond the built-in registry.

```bash
cargo adk new finance-agent \
  --template company-finance \
  --template-dir ./agent-templates
```

A custom template with the same name as a built-in replaces it for that CLI
invocation. Keep manifests in version control and test generated projects in CI.

```toml
name = "company-finance"
description = "Company finance agent with approved defaults"
provider = "openai"
features = ["minimal", "tools"]
imports = ["use std::sync::Arc;"]
```

## Verify the generated project

```bash
cd support-agent
cargo adk build
cargo run
```

`cargo adk build` compiles and validates the generated project without
deploying it. Commit the generated source, review its enabled features and
environment requirements, and keep the same command as a CI gate.

## Keep automation current

Registry contents can change between releases. Before upgrading automated
project generation:

```bash
cargo adk templates
cargo adk addons
cargo adk new smoke-agent --template tools --addon mcp --dry-run
```

This verifies names and compatibility against the exact `cargo-adk` binary
installed in the build environment.
