# YAML Agent Definition Example

Demonstrates the YAML agent definition loading feature from ADK-Rust v0.7.0.

## What This Shows

- Loading a single agent from a YAML file with `AgentConfigLoader::load_file()`
- Loading a directory of agents with sub-agent cross-references via `AgentConfigLoader::load_directory()`
- Validation error handling for malformed YAML definitions
- Creating a custom `ModelFactory` and `ToolRegistry` for the loader

## Prerequisites

- Rust 1.85+
- A Google API key for Gemini

## Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `GOOGLE_API_KEY` | Yes | Gemini API key |
| `RUST_LOG` | No | Logging level (default: `info`) |

## YAML Agent Definition Format

Agent definitions are YAML files with the following structure:

```yaml
name: researcher
description: "A research assistant"
model:
  provider: gemini
  model_id: gemini-2.0-flash
instructions: |
  You are a research assistant.
config:
  temperature: 0.3
  max_tokens: 1024
```

Sub-agent cross-references use the `ref` keyword:

```yaml
name: assistant
sub_agents:
  - ref: researcher
```

## Run

```bash
# Copy and fill in your API key
cp .env.example .env

# Run the example
cargo run --manifest-path examples/yaml_agent/Cargo.toml
```

## Expected Output

```
╔══════════════════════════════════════════╗
║  YAML Agent Definition — ADK-Rust v0.7.0 ║
╚══════════════════════════════════════════╝

── Section 1: Load a single YAML file ─────────────────────

  Loaded agent: researcher
  Description:  A research assistant that answers questions
  Sub-agents:   0

── Section 2: Load directory with cross-references ────────

  Loaded 2 agents from directory:

    • researcher — A research assistant that answers questions
    • assistant — A helpful assistant that delegates research tasks
      sub-agents: researcher

── Section 3: Validation error handling ───────────────────

  Expected validation error:
  <descriptive error about missing model field>

  Expected temperature validation error:
  <descriptive error about invalid temperature>

✅ YAML Agent Definition example completed successfully.
```
