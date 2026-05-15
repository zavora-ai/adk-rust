# Feature Tier Examples

Validates that all README and quickstart code examples work across every feature tier.

## Examples by Tier

### Minimal (default — no features needed)

These examples use `adk-rust = "0.8.2"` with **no explicit features**. The `minimal` default includes Gemini, agents, runner, sessions, and the lightweight launcher.

| # | Example | What it validates | Source |
|---|---------|-------------------|--------|
| 01 | `01-minimal-hello` | `adk::run()` one-liner | README "Fastest Start" |
| 02 | `02-minimal-launcher` | `Launcher::new(agent).run()` REPL | README "Basic Example (Gemini)" |
| 03 | `03-minimal-openai` | OpenAI provider with opt-in `openai` feature | README "OpenAI Example" |
| 04 | `04-minimal-anthropic` | Anthropic provider with opt-in `anthropic` feature | README "Anthropic Example" |
| 05 | `05-minimal-tools` | `#[tool]` macro via explicit `adk-tool` dependency | README "Tool System" |
| 06 | `06-minimal-multi-provider` | `provider_from_env()` with compiled providers | README provider detection |
| 07 | `07-minimal-memory` | Multi-turn session history | README sessions |

### Quickstart (minimal tier)

These match the `docs/official_docs/quickstart.md` examples verbatim.

| # | Example | What it validates | Source |
|---|---------|-------------------|--------|
| 08 | `08-quickstart-scaffold` | Scaffolded project code | quickstart.md "Generated Code" |
| 09 | `09-quickstart-tools` | Adding custom tools | quickstart.md "Adding Custom Tools" |
| 10 | `10-quickstart-zero-config` | Zero-config `adk::run()` | quickstart.md "Zero-Config Alternative" |

### Standard (`features = ["standard"]`)

Adds: tools, memory, telemetry, server, auth, graph, eval, guardrail, plugin, artifacts, and skills. CLI providers remain opt-in.

| # | Example | What it validates |
|---|---------|-------------------|
| 11 | `11-standard-graph` | Graph workflow with `GraphAgent::builder()` |
| 12 | `12-standard-sequential` | Sequential multi-agent pipeline |
| 13 | `13-standard-cli-launcher` | Lightweight launcher in the standard tier |

### Enterprise (`features = ["enterprise"]`)

Adds: realtime, browser, RAG, payments, AWP.

| # | Example | What it validates |
|---|---------|-------------------|
| 14 | `14-enterprise-multi-agent` | Multi-agent pipeline with artifact storage |
| 15 | `15-enterprise-parallel` | Parallel agent execution (3 concurrent analysts) |

### Full (`features = ["full"]`)

Adds: audio, code execution, sandbox.

| # | Example | What it validates |
|---|---------|-------------------|
| 16 | `16-full-sandbox` | Sandboxed Python and Rust code execution |

## Run

```bash
cd examples/tier_examples
cp .env.example .env   # add your API key(s)

# ── Minimal tier ──
cargo run --bin 01-minimal-hello
cargo run --bin 02-minimal-launcher
cargo run --features openai --bin 03-minimal-openai          # needs OPENAI_API_KEY
cargo run --features anthropic --bin 04-minimal-anthropic    # needs ANTHROPIC_API_KEY
cargo run --bin 05-minimal-tools
cargo run --bin 06-minimal-multi-provider
cargo run --bin 07-minimal-memory

# ── Quickstart ──
cargo run --bin 08-quickstart-scaffold
cargo run --bin 09-quickstart-tools
cargo run --bin 10-quickstart-zero-config

# ── Standard tier ──
cargo run --bin 11-standard-graph
cargo run --bin 12-standard-sequential
cargo run --bin 13-standard-cli-launcher

# ── Enterprise tier ──
cargo run --bin 14-enterprise-multi-agent
cargo run --bin 15-enterprise-parallel

# ── Full tier ──
cargo run --bin 16-full-sandbox            # needs python3 and rustc
```

## Key Point

All minimal/quickstart examples use `adk-rust = "0.8.2"` with **no explicit features** unless the example name is a provider opt-in. The `minimal` default includes the smallest useful agent stack: Gemini, agents, runner, sessions, and the lightweight launcher.

Higher tiers add production and specialist capabilities:
- **standard** → tools, memory, telemetry, server, auth, graph workflows, eval
- **enterprise** → realtime voice, browser, RAG, payments, AWP
- **full** → audio processing, code execution, sandbox
