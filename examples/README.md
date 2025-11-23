# ADK Rust Examples

This directory contains example applications demonstrating how to use the ADK Rust framework.

## Prerequisites

Set your Google API key:
```bash
export GOOGLE_API_KEY="your-api-key-here"
# or
export GEMINI_API_KEY="your-api-key-here"
```

## Running Examples

All examples can be run with:
```bash
cargo run --example <example_name>
```

## Available Examples

### Basic Examples

#### quickstart
A simple weather and time agent using Google Search:
```bash
cargo run --example quickstart
```
Demonstrates: Creating a Gemini model, building an LLM agent with tools, running an interactive console session.

#### server
Starts an HTTP server with REST and A2A endpoints:
```bash
cargo run --example server
# or with custom port
PORT=3000 cargo run --example server
```
Demonstrates: Server mode, agent loader, HTTP endpoints.

### Tool Examples

#### function_tool
Calculator agent with custom function tool:
```bash
cargo run --example function_tool
```
Demonstrates: Creating custom function tools, arithmetic operations.

#### multiple_tools
Agent orchestrating multiple sub-agents with different tool types:
```bash
cargo run --example multiple_tools
```
Demonstrates: Sub-agent pattern, mixing GoogleSearch and custom tools, agent composition.

### Workflow Examples

#### sequential
Sequential workflow processing (analyze → expand → summarize):
```bash
cargo run --example sequential
```
Demonstrates: Sequential agent execution, multi-step processing.

#### parallel
Parallel workflow with multiple perspectives (technical, business, user):
```bash
cargo run --example parallel
```
Demonstrates: Concurrent agent execution, parallel analysis.

#### loop_workflow
Iterative refinement loop with exit condition:
```bash
cargo run --example loop_workflow
```
Demonstrates: Loop agent, iterative processing, exit_loop tool.

## Example Structure

Each example is a standalone Rust file that:
1. Loads API key from environment
2. Creates Gemini model(s)
3. Builds agent(s) with tools/sub-agents
4. Runs console or server mode

## Tips

- Use Ctrl+C to exit console mode
- Server mode runs on port 8080 by default (override with PORT env var)
- All examples use `gemini-2.0-flash-exp` model
- Console mode includes readline history and editing
