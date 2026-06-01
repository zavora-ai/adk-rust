# Managed Agents: Custom Tool Flow

Demonstrates defining a custom tool on a managed agent, handling `AgentCustomToolUse` events when the agent invokes the tool, executing the tool locally, and sending results back.

## Prerequisites

- Rust 1.85.0+
- An `ANTHROPIC_API_KEY` with Managed Agents beta access

## Setup

```bash
cp ../../.env.example .env
# Add your ANTHROPIC_API_KEY to .env
```

## Running

```bash
cargo run -p managed-agents-custom-tools
```

## What it demonstrates

1. Define a custom tool with a JSON schema (`get_weather`)
2. Create an agent with both the standard toolset and the custom tool
3. Send a message that triggers the custom tool
4. Detect `AgentCustomToolUse` events in the SSE stream
5. Execute the tool locally (simulated weather lookup)
6. Send the result back via `UserEvent::custom_tool_result`
7. Continue streaming until the agent produces a final response
